//! `host.library.write`: a plugin adds one file to the music library
//! (v2's `plugin_write_library_file`).
//!
//! The file lands under the first library root, through the library
//! publisher's sandbox: a root-relative path only, no symlinked
//! components, and never over an existing file. Each plugin may write
//! 100 MiB per rolling day; past that, writes are refused until older
//! ones age out. The last refusal is kept per plugin so it can be shown
//! next to the plugin's health.

use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use super::super::host::PluginHost;
use crate::library::publish::paths::{Root, Sandbox};
use crate::library::publish::plugin_files::write_new_file;
use crate::runtime_config::secret_sections::TypedLibrary;

/// Bytes one plugin may write per rolling day.
pub const QUOTA_BYTES_PER_DAY: usize = 100 * 1024 * 1024;
const QUOTA_WINDOW: Duration = Duration::from_secs(24 * 60 * 60);
/// Longest relative path accepted.
const MAX_PATH_LEN: usize = 200;

/// Per-plugin write usage and the last refusal.
#[derive(Default)]
pub struct LibraryWrites {
    usage: Mutex<HashMap<String, VecDeque<(Instant, usize)>>>,
    refusals: Mutex<HashMap<String, String>>,
}

impl LibraryWrites {
    /// The last refused write for this plugin, cleared by the next write
    /// that lands.
    pub fn last_refusal(&self, plugin: &str) -> Option<String> {
        self.refusals.lock().ok()?.get(plugin).cloned()
    }

    fn refuse(&self, plugin: &str, reason: String) -> String {
        tracing::warn!(plugin, %reason, "plugin library write refused");
        if let Ok(mut refusals) = self.refusals.lock() {
            refusals.insert(plugin.to_owned(), reason.clone());
        }
        reason
    }

    /// Bytes written inside the window, dropping older entries.
    fn used(&self, plugin: &str, now: Instant) -> usize {
        let Ok(mut usage) = self.usage.lock() else {
            return usize::MAX;
        };
        let Some(entries) = usage.get_mut(plugin) else {
            return 0;
        };
        while entries
            .front()
            .is_some_and(|(at, _)| now.duration_since(*at) >= QUOTA_WINDOW)
        {
            entries.pop_front();
        }
        entries.iter().map(|(_, bytes)| bytes).sum()
    }

    fn record(&self, plugin: &str, bytes: usize) {
        if let Ok(mut usage) = self.usage.lock() {
            usage
                .entry(plugin.to_owned())
                .or_default()
                .push_back((Instant::now(), bytes));
        }
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
        check_rel_path(rel_path).map_err(|reason| writes.refuse(plugin, reason))?;
        let used = writes.used(plugin, Instant::now());
        if used.saturating_add(data.len()) > QUOTA_BYTES_PER_DAY {
            return Err(writes.refuse(
                plugin,
                format!(
                    "the plugin has used its library write allowance ({} MiB a day); try again later",
                    QUOTA_BYTES_PER_DAY / (1024 * 1024)
                ),
            ));
        }
        let library = self.config.get_raw::<TypedLibrary>().map_err(|error| {
            writes.refuse(plugin, format!("library settings unreadable: {error}"))
        })?;
        let roots: Vec<Root> = library
            .library_roots
            .iter()
            .filter(|root| !root.path.trim().is_empty() && !root.id.is_empty())
            .map(|root| Root {
                id: root.id.clone(),
                dir: std::path::PathBuf::from(root.path.trim()),
            })
            .collect();
        let Some(primary) = roots.first().map(|root| root.id.clone()) else {
            return Err(writes.refuse(plugin, "no library folder is set up".to_owned()));
        };
        let sandbox =
            Sandbox::new(roots).map_err(|error| writes.refuse(plugin, error.to_string()))?;
        let relative = rel_path.to_owned();
        let bytes = data.len();
        let stored = tokio::task::spawn_blocking(move || {
            write_new_file(&sandbox, &primary, &relative, &data)
        })
        .await
        .map_err(|error| writes.refuse(plugin, format!("library write stopped: {error}")))?
        .map_err(|error| writes.refuse(plugin, error.to_string()))?;
        writes.record(plugin, bytes);
        tracing::info!(plugin, rel_path, bytes, "plugin added a library file");
        Ok(stored.to_string_lossy().into_owned())
    }
}
