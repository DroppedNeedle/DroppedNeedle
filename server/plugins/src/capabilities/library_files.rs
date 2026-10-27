//! `host.library.write`: a plugin adds one file to the music library
//! (v2's `plugin_write_library_file`).
//!
//! Only plugins that declare the `library_writer` capability may write.
//! The file lands under the first library root through the library
//! publisher, as one recorded change under the plugin's name: a
//! root-relative path only, no linked part, and never over an existing
//! file. A crash leaves the file placed whole or not at all, and an
//! administrator can undo the change.
//!
//! A write has [`WRITE_BUDGET`] from the plugin's call: it waits for the
//! library to be free within it, and a file not placed by its end, or
//! once the server stopped waiting, is never placed. The host answers
//! every plugin request within 10 seconds, so the budget ends before
//! that: a busy library answers busy, and a write the host gave up on
//! never lands later.
//!
//! Limits: [`MAX_FILE_BYTES`] per file, and per plugin per rolling day
//! [`QUOTA_BYTES_PER_DAY`] and [`QUOTA_FILES_PER_DAY`]. The day's writes
//! are rows in `plugin_library_writes`, written in the transaction that
//! records the change, so they survive a restart and a write that
//! stopped before it recorded anything spends nothing. A write whose
//! change was unwound (refused at placement, past its budget, or undone
//! by recovery after a crash) no longer counts. The last refusal is kept
//! per plugin so it can be shown with the plugin's health.
//!
//! Like v2, a plugin may add audio files; the next library scan picks
//! them up like any other file.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use super::super::host::PluginHost;
use super::super::manifest::Capability;
use super::super::protocol::MAX_MESSAGE_BYTES;
use droppedneedle_library::manage::PluginFileRefusal;
use droppedneedle_library::publish::{JournalState, PublishError};

/// Bytes one plugin may write per rolling day.
pub const QUOTA_BYTES_PER_DAY: usize = 100 * 1024 * 1024;
/// Files one plugin may write per rolling day. Each file keeps a recorded
/// change for undo, so this bounds what a plugin adds to the database; at
/// the byte allowance it binds only for files under 100 KiB on average.
pub const QUOTA_FILES_PER_DAY: usize = 1000;
/// Room in one protocol message for all but the file's text: the
/// JSON-RPC envelope, the method, the id and the path.
const ENVELOPE_BYTES: usize = 4096;
/// Largest single file: its base64 text and the envelope fit one
/// protocol message.
pub const MAX_FILE_BYTES: usize = (MAX_MESSAGE_BYTES - ENVELOPE_BYTES) / 4 * 3;
const QUOTA_WINDOW_MS: i64 = 24 * 60 * 60 * 1000;
/// How long one write may take, waiting included: 2 seconds inside the
/// host's 10-second cut on a plugin request, so the plugin hears the
/// answer and a write the server stopped waiting for is never placed.
pub const WRITE_BUDGET: Duration = Duration::from_secs(8);
/// Longest relative path accepted.
const MAX_PATH_LEN: usize = 200;

/// The last refusal per plugin.
#[derive(Default)]
pub struct LibraryWrites {
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

    fn landed(&self, plugin: &str) {
        if let Ok(mut refusals) = self.refusals.lock() {
            refusals.remove(plugin);
        }
    }
}

/// Count the plugin's writes of the last day and, when `bytes` more fit
/// both allowances, add this one as `bundle_id`. Runs in the transaction
/// that records the change. A write whose change was unwound is not
/// counted, and neither is one that never wrote its journal row; rows
/// older than a day are deleted.
fn reserve(
    conn: &rusqlite::Connection,
    plugin: &str,
    bundle_id: &str,
    bytes: usize,
    now_ms: i64,
) -> Result<bool, PublishError> {
    conn.prepare_cached("DELETE FROM plugin_library_writes WHERE written_at <= ?1")?
        .execute([now_ms - QUOTA_WINDOW_MS])?;
    let (files, used): (i64, i64) = conn
        .prepare_cached(
            "SELECT COUNT(*), COALESCE(SUM(w.bytes), 0) FROM plugin_library_writes w
             WHERE w.plugin = ?1 AND EXISTS (
                 SELECT 1 FROM library_publish_journal j
                 WHERE j.bundle_id = w.bundle_id AND j.state <> ?2)",
        )?
        .query_row(
            rusqlite::params![plugin, JournalState::Compensated.as_str()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
    let files = usize::try_from(files).unwrap_or(usize::MAX);
    let used = usize::try_from(used).unwrap_or(usize::MAX);
    if files >= QUOTA_FILES_PER_DAY || used.saturating_add(bytes) > QUOTA_BYTES_PER_DAY {
        return Ok(false);
    }
    conn.prepare_cached(
        "INSERT INTO plugin_library_writes (bundle_id, plugin, bytes, written_at)
         VALUES (?1, ?2, ?3, ?4)",
    )?
    .execute(rusqlite::params![
        bundle_id,
        plugin,
        i64::try_from(bytes).unwrap_or(i64::MAX),
        now_ms
    ])?;
    Ok(true)
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

/// Sets its flag when dropped: a write the server stopped waiting for
/// records nothing and places nothing.
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
            .is_some_and(|loaded| loaded.serves(Capability::LibraryWriter));
        if !declared {
            return Err(writes.refuse(
                plugin,
                "declare the library_writer capability to write library files".to_owned(),
            ));
        }
        check_rel_path(rel_path).map_err(|reason| writes.refuse(plugin, reason))?;
        let bytes = data.len();
        if bytes > MAX_FILE_BYTES {
            return Err(writes.refuse(
                plugin,
                format!("a library file may be at most {MAX_FILE_BYTES} bytes"),
            ));
        }
        let cancelled = Arc::new(AtomicBool::new(false));
        let _cancel = CancelOnDrop(cancelled.clone());
        let deadline = Instant::now() + WRITE_BUDGET;
        let library = self.library.clone();
        let (name, relative) = (plugin.to_owned(), rel_path.to_owned());
        let outcome = tokio::task::spawn_blocking(move || {
            let over_allowance = AtomicBool::new(false);
            let on_record = |conn: &rusqlite::Connection, bundle_id: &str, now_ms: i64| {
                if reserve(conn, &name, bundle_id, bytes, now_ms)? {
                    return Ok(());
                }
                over_allowance.store(true, Ordering::SeqCst);
                Err(PublishError::Validation("over the daily allowance".into()))
            };
            let added =
                library.add_plugin_file(&name, &relative, data, deadline, &cancelled, &on_record);
            (added, over_allowance.load(Ordering::SeqCst))
        })
        .await;
        let refusal = match outcome {
            Ok((Ok(added), _)) => {
                writes.landed(plugin);
                tracing::info!(plugin, rel_path, bytes, bundle_id = %added.bundle_id, "plugin added a library file");
                return Ok((added.path.to_string_lossy().into_owned(), added.bundle_id));
            }
            Ok((Err(_), true)) => {
                return Err(writes.refuse(
                    plugin,
                    format!(
                        "the plugin has used its library write allowance ({} MiB or {} files a \
                         day); try again later",
                        QUOTA_BYTES_PER_DAY / (1024 * 1024),
                        QUOTA_FILES_PER_DAY
                    ),
                ));
            }
            Ok((Err(refusal), false)) => refusal,
            Err(error) => {
                return Err(writes.refuse(plugin, format!("library write stopped: {error}")));
            }
        };
        let reason = match refusal {
            PluginFileRefusal::Busy => format!(
                "the library stayed busy for {} seconds; try again later",
                WRITE_BUDGET.as_secs()
            ),
            PluginFileRefusal::Cancelled => "the server stopped waiting for the write".to_owned(),
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
