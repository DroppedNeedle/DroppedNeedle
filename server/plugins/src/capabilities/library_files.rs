//! `host.library.write`: a plugin adds one file to the music library
//! (v2's `plugin_write_library_file`).
//!
//! Only plugins that declare the `library_writer` capability may write.
//! The file lands under the first library root, through the library
//! publisher's sandbox: a root-relative path only, no symlinked
//! components, and never over an existing file. The write holds the
//! root's write lease (as every publisher write does, so a scan never
//! walks a half-written folder) and gives up after 30 seconds.
//!
//! Limits: 6 MiB per file and 100 MiB per plugin per rolling day; the
//! allowance is reserved before the write and handed back if it fails.
//! The last refusal is kept per plugin so it can be shown with the
//! plugin's health.
//!
//! Each write's hidden temp file is listed in a small ledger under the
//! plugins folder until the write ends, so a crash mid-write leaves a
//! name the next start removes.
//!
//! Like v2, a plugin may add audio files; the next library scan picks
//! them up like any other file.

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use super::super::host::PluginHost;
use droppedneedle_library::publish::HIDDEN_PREFIX;
use droppedneedle_library::publish::paths::{Root, Sandbox};
use droppedneedle_library::publish::plugin_files::{TEMP_TAG, write_new_file};

/// The capability a plugin declares to write library files.
pub const CAPABILITY: &str = "library_writer";
/// Bytes one plugin may write per rolling day.
pub const QUOTA_BYTES_PER_DAY: usize = 100 * 1024 * 1024;
/// Largest single file.
pub const MAX_FILE_BYTES: usize = 6 * 1024 * 1024;
const QUOTA_WINDOW: Duration = Duration::from_secs(24 * 60 * 60);
/// How long one write may wait for the root and run.
const WRITE_TIMEOUT: Duration = Duration::from_secs(30);
/// Longest relative path accepted.
const MAX_PATH_LEN: usize = 200;
/// Ledger folder under the plugins data folder.
const LEDGER_DIR: &str = ".library-writes";

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

/// Remove temp files left by writes a crash interrupted. Only names the
/// plugin writer creates are touched.
pub fn sweep_ledger(data_dir: &Path) {
    let ledger = data_dir.join(LEDGER_DIR);
    let Ok(entries) = std::fs::read_dir(&ledger) else {
        return;
    };
    for entry in entries.flatten() {
        let note = entry.path();
        if let Ok(text) = std::fs::read_to_string(&note) {
            let temp = PathBuf::from(text.trim());
            let ours = temp
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with(&format!("{HIDDEN_PREFIX}{TEMP_TAG}")));
            if ours {
                match std::fs::remove_file(&temp) {
                    Ok(()) => {
                        tracing::info!(path = %temp.display(), "removed an interrupted plugin library write")
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                    Err(error) => {
                        tracing::warn!(path = %temp.display(), %error, "interrupted plugin library write not removed");
                        continue;
                    }
                }
            }
        }
        if let Err(error) = std::fs::remove_file(&note) {
            tracing::warn!(%error, "plugin library write ledger entry not removed");
        }
    }
}

impl PluginHost {
    /// Add one file to the library for `plugin`. Answers the stored path,
    /// or the reason it was refused.
    pub async fn write_library_file(
        &self,
        plugin: &str,
        rel_path: &str,
        data: Vec<u8>,
    ) -> Result<String, String> {
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
        let library = self.library.get().map_err(|error| {
            writes.refuse(plugin, format!("library settings unreadable: {error}"))
        })?;
        let roots: Vec<Root> = library
            .library_roots
            .iter()
            .filter(|root| !root.path.trim().is_empty() && !root.id.is_empty())
            .map(|root| Root::library(root.id.clone(), PathBuf::from(root.path.trim())))
            .collect();
        let Some(primary) = roots.first().map(|root| root.id.clone()) else {
            return Err(writes.refuse(plugin, "no library folder is set up".to_owned()));
        };
        let sandbox =
            Sandbox::new(roots).map_err(|error| writes.refuse(plugin, error.to_string()))?;
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
        let ledger = self.data_dir("").join(LEDGER_DIR);
        let fs = self.library_fs();
        let relative = rel_path.to_owned();
        let root_id = primary.clone();
        let write = async move {
            // Hold the root like every publisher write; dropped when done.
            let _lease = fs.write(&root_id).await;
            tokio::task::spawn_blocking(move || {
                let note = ledger.join(uuid::Uuid::new_v4().simple().to_string());
                let stored = write_new_file(&sandbox, &root_id, &relative, &data, |temp| {
                    std::fs::create_dir_all(&ledger)?;
                    std::fs::write(&note, temp.to_string_lossy().as_bytes())?;
                    Ok(())
                });
                if let Err(error) = std::fs::remove_file(&note)
                    && error.kind() != std::io::ErrorKind::NotFound
                {
                    tracing::warn!(%error, "plugin library write ledger entry not removed");
                }
                stored
            })
            .await
        };
        let outcome = match tokio::time::timeout(WRITE_TIMEOUT, write).await {
            Err(_) => Err(format!(
                "the library write took longer than {} seconds",
                WRITE_TIMEOUT.as_secs()
            )),
            Ok(Err(error)) => Err(format!("library write stopped: {error}")),
            Ok(Ok(Err(error))) => Err(error.to_string()),
            Ok(Ok(Ok(stored))) => Ok(stored),
        };
        match outcome {
            Ok(stored) => {
                writes.landed(plugin);
                tracing::info!(plugin, rel_path, bytes, "plugin added a library file");
                Ok(stored.to_string_lossy().into_owned())
            }
            Err(reason) => {
                writes.release(plugin, stamp);
                Err(writes.refuse(plugin, reason))
            }
        }
    }
}
