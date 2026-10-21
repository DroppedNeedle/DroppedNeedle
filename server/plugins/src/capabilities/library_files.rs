//! `host.library.write`: a plugin adds one file to the music library
//! (v2's `plugin_write_library_file`).
//!
//! Only plugins that declare the `library_writer` capability may write.
//! The file lands under the first library root through the library
//! publisher, as one recorded change under the plugin's name: a
//! root-relative path only, no linked part, and never over an existing
//! file. A crash leaves the file placed whole or not at all, and an
//! administrator can undo the change. The write waits up to 30 seconds
//! for the library to be free, and records nothing once the plugin has
//! stopped waiting.
//!
//! Limits: 6 MiB per file and 100 MiB per plugin per rolling day; the
//! allowance is reserved before the write and handed back if it fails.
//! The last refusal is kept per plugin so it can be shown with the
//! plugin's health.
//!
//! Like v2, a plugin may add audio files; the next library scan picks
//! them up like any other file.

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use super::super::host::PluginHost;
use droppedneedle_library::manage::PluginFileRefusal;

/// The capability a plugin declares to write library files.
pub const CAPABILITY: &str = "library_writer";
/// Bytes one plugin may write per rolling day.
pub const QUOTA_BYTES_PER_DAY: usize = 100 * 1024 * 1024;
/// Largest single file.
pub const MAX_FILE_BYTES: usize = 6 * 1024 * 1024;
const QUOTA_WINDOW: Duration = Duration::from_secs(24 * 60 * 60);
/// How long one write may wait for the library to be free.
const WRITE_TIMEOUT: Duration = Duration::from_secs(30);
/// Longest relative path accepted.
const MAX_PATH_LEN: usize = 200;

/// Per-plugin write usage and the last refusal.
#[derive(Default)]
pub struct LibraryWrites {
    usage: Mutex<HashMap<String, VecDeque<(Instant, usize)>>>,
    refusals: Mutex<HashMap<String, String>>,
}

impl LibraryWrites {
    fn refuse(&self, plugin: &str, reason: String) -> String {
        tracing::warn!(plugin, %reason, "plugin library write refused");
        if let Ok(mut refusals) = self.refusals.lock() {
            refusals.insert(plugin.to_owned(), reason.clone());
        }
        reason
    }

    /// Check and reserve `bytes` of the plugin's allowance in one step.
    /// Answers the reservation's stamp, or false when it does not fit.
    fn reserve(&self, plugin: &str, bytes: usize) -> Option<Instant> {
        let mut usage = self.usage.lock().ok()?;
        let entries = usage.entry(plugin.to_owned()).or_default();
        let now = Instant::now();
        while entries
            .front()
            .is_some_and(|(at, _)| now.duration_since(*at) >= QUOTA_WINDOW)
        {
            entries.pop_front();
        }
        let used: usize = entries.iter().map(|(_, bytes)| bytes).sum();
        if used.saturating_add(bytes) > QUOTA_BYTES_PER_DAY {
            return None;
        }
        entries.push_back((now, bytes));
        Some(now)
    }

    /// Hand back a reservation whose write failed.
    fn release(&self, plugin: &str, stamp: Instant) {
        if let Ok(mut usage) = self.usage.lock()
            && let Some(entries) = usage.get_mut(plugin)
            && let Some(at) = entries.iter().position(|(at, _)| *at == stamp)
        {
            entries.remove(at);
        }
    }

    fn landed(&self, plugin: &str) {
        if let Ok(mut refusals) = self.refusals.lock() {
            refusals.remove(plugin);
        }
    }
}

/// Check a plugin-chosen relative path: lowercase letters, digits, `_`,
/// `-`, `.` and `/`, no empty, `.` or `..` parts, and no hidden names
/// (the publisher keeps its temps under a hidden prefix).
pub fn check_rel_path(rel_path: &str) -> Result<(), String> {
    let allowed = |c: char| {
        c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '_' | '-' | '.' | '/')
    };
    let ok = !rel_path.is_empty()
        && rel_path.len() <= MAX_PATH_LEN
        && rel_path.chars().all(allowed)
        && rel_path
            .split('/')
            .all(|part| !part.is_empty() && !part.starts_with('.'));
    if ok {
        Ok(())
    } else {
        Err(format!(
            "{rel_path:?} is not a safe library path: use lowercase letters, digits, _ - . and /, \
             with no empty, hidden or .. parts, up to {MAX_PATH_LEN} characters"
        ))
    }
}

/// Sets its flag when dropped: a write the plugin stopped waiting for
/// records nothing.
struct CancelOnDrop(Arc<AtomicBool>);

impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

impl PluginHost {
    /// Add one file to the library for `plugin`. Answers the stored path
    /// and the id of the recorded change, or the reason it was refused.
    pub async fn write_library_file(
        &self,
        plugin: &str,
        rel_path: &str,
        data: Vec<u8>,
    ) -> Result<(String, String), String> {
        let writes = &self.library_writes;
        let declared = self
            .get(plugin)
            .is_some_and(|loaded| loaded.serves(CAPABILITY));
        if !declared {
            return Err(writes.refuse(
                plugin,
                format!("declare the {CAPABILITY} capability to write library files"),
            ));
        }
        check_rel_path(rel_path).map_err(|reason| writes.refuse(plugin, reason))?;
        if data.len() > MAX_FILE_BYTES {
            return Err(writes.refuse(
                plugin,
                format!(
                    "a library file may be at most {} MiB",
                    MAX_FILE_BYTES / (1024 * 1024)
                ),
            ));
        }
        let bytes = data.len();
        let Some(stamp) = writes.reserve(plugin, bytes) else {
            return Err(writes.refuse(
                plugin,
                format!(
                    "the plugin has used its library write allowance ({} MiB a day); try again later",
                    QUOTA_BYTES_PER_DAY / (1024 * 1024)
                ),
            ));
        };
        let cancelled = Arc::new(AtomicBool::new(false));
        let _cancel = CancelOnDrop(cancelled.clone());
        let deadline = Instant::now() + WRITE_TIMEOUT;
        let library = self.library.clone();
        let (name, relative) = (plugin.to_owned(), rel_path.to_owned());
        let outcome = tokio::task::spawn_blocking(move || {
            library.add_plugin_file(&name, &relative, data, deadline, &cancelled)
        })
        .await;
        let refusal = match outcome {
            Ok(Ok(added)) => {
                writes.landed(plugin);
                tracing::info!(plugin, rel_path, bytes, bundle_id = %added.bundle_id, "plugin added a library file");
                return Ok((added.path.to_string_lossy().into_owned(), added.bundle_id));
            }
            Ok(Err(refusal)) => refusal,
            Err(error) => {
                writes.release(plugin, stamp);
                return Err(writes.refuse(plugin, format!("library write stopped: {error}")));
            }
        };
        // A write that stopped partway may still land: its bytes stay
        // counted.
        if !matches!(refusal, PluginFileRefusal::Pending) {
            writes.release(plugin, stamp);
        }
        let reason = match refusal {
            PluginFileRefusal::Busy => format!(
                "the library stayed busy for {} seconds; try again later",
                WRITE_TIMEOUT.as_secs()
            ),
            PluginFileRefusal::Cancelled => "the plugin stopped waiting for the write".to_owned(),
            PluginFileRefusal::NoFolder => "no library folder is set up".to_owned(),
            PluginFileRefusal::FolderUnusable(why) => why.to_owned(),
            PluginFileRefusal::Taken => {
                format!("{rel_path} already exists; plugins only add new files")
            }
            PluginFileRefusal::Pending => {
                "the library write stopped partway; it finishes or is undone by itself shortly"
                    .to_owned()
            }
            PluginFileRefusal::Publish(error) => error.to_string(),
        };
        Err(writes.refuse(plugin, reason))
    }
}
