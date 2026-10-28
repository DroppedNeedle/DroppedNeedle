//! The retry pass: settled tasks whose backoff elapsed spawn successors.

use super::{DownloadWorker, Pass};
use crate::downloads::recovery::plan_retry;
use crate::downloads::store::{NewTask, TaskRow};

/// The retry successor row for a settled task: same ask, one generation
/// on. `None` for a task that is not settled yet.
pub(super) fn successor_row(task: &TaskRow) -> Option<NewTask> {
    let spawn = plan_retry(&task.id, task.status, &task.origin, task.retry_count)?;
    // Successor ids keep the 32-hex shape the orphan parser recognises;
    // the retry generation rides in `retry_count`.
    Some(NewTask {
        id: hex_task_id(&spawn.task_id),
        user_id: task.user_id.clone(),
        artist_name: task.artist_name.clone(),
        album_title: task.album_title.clone(),
        release_group_mbid: task.release_group_mbid.clone(),
        origin: spawn.origin,
        retry_count: spawn.retry_count,
    })
}

/// Fold a retry successor id into the 32-hex shape: the planned
/// `{task}-r{n}` suffix breaks the orphan parser, so hash it down while
/// the generation rides in `retry_count`. FNV-1a, stable across processes
/// (the std `DefaultHasher` is process-randomised, so a successor id must
/// never depend on it).
fn hex_task_id(planned: &str) -> String {
    fn fnv1a(text: &str, seed: u64) -> u64 {
        let mut hash = seed;
        for byte in text.bytes() {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
        hash
    }
    let first = fnv1a(planned, 0xcbf2_9ce4_8422_2325);
    let second = fnv1a(planned, 0x8422_2325_cbf2_9ce4 ^ first);
    format!("{first:016x}{second:016x}")
}

impl DownloadWorker {
    /// Spawn successors for terminal tasks whose retry backoff elapsed.
    /// The successor id is deterministic per (task, generation), and the
    /// existence check, detail copy and insert share one write, so a crash
    /// or a failed insert simply retries on the next pass.
    pub(super) async fn retry_pass(&self, pass: &Pass) {
        let now = pass.now;
        let max = i64::from(pass.config.retry.auto_retry_max());
        if max == 0 {
            return;
        }
        let retryable = match self.journal.read_retryable(max).await {
            Ok(rows) => rows,
            Err(error) => {
                tracing::warn!(%error, "retryable download read failed");
                return;
            }
        };
        for task in retryable {
            let anchor = task.completed_at.unwrap_or(task.updated_at);
            let due = pass.config.retry.next_retry_at(
                u32::try_from(task.retry_count.max(0)).unwrap_or(u32::MAX),
                anchor,
                task.status.as_str(),
            );
            if due.is_none_or(|at| at > now) {
                continue;
            }
            let Some(row) = successor_row(&task) else {
                continue;
            };
            let successor = row.id.clone();
            let source = task.clone();
            let spawned = self
                .step("downloads.retry", move |store| {
                    // Held tracks gate their task: re-downloading a held
                    // track loops.
                    if crate::held::store::HeldRows::new(store.conn()).task_holds(&source.id)? {
                        return Ok(false);
                    }
                    // The request follows its download onto the retry.
                    store.spawn_successor(&source, &row, now)
                })
                .await;
            if spawned == Some(true) {
                tracing::info!(
                    task_id = %task.id,
                    successor = %successor,
                    "download auto-retry spawned"
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn successor_ids_are_stable_32_hex() {
        let first = hex_task_id("task-r1");
        assert_eq!(first.len(), 32);
        assert!(first.bytes().all(|b| b.is_ascii_hexdigit()));
        assert_eq!(first, hex_task_id("task-r1"));
        assert_ne!(first, hex_task_id("task-r2"));
    }
}
