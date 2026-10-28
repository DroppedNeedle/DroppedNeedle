//! The orphan sweep: complete-dir debris with no owner is removed.

use std::path::Path;
use std::time::{Duration, UNIX_EPOCH};

use super::{DownloadWorker, Pass};
use crate::downloads::orphans::{OrphanDecision, OrphanEvidence, evaluate_orphan, job_name_parts};
use crate::downloads::source_tag::SourceTag;
use crate::downloads::sources::{OrphanOwnership, SourceHandle};

/// Pure removal gate: the folder must sit strictly under the walked
/// root and must not be a symlink.
fn orphan_remove_allowed(root: &Path, path: &Path, is_symlink: bool) -> bool {
    !is_symlink && path != root && path.starts_with(root)
}

impl DownloadWorker {
    /// Walk the complete dirs and remove proven debris. Every ambiguous
    /// answer keeps the folder.
    pub(super) async fn orphan_pass(&self, pass: &Pass) {
        for (source_tag, root) in &pass.config.orphan_roots {
            self.orphan_root(pass, source_tag, root).await;
        }
    }

    /// Reconcile one complete dir.
    async fn orphan_root(&self, pass: &Pass, source_tag: &SourceTag, root: &Path) {
        let mut entries = match tokio::fs::read_dir(root).await {
            Ok(entries) => entries,
            Err(error) => {
                tracing::warn!(root = %root.display(), %error, "orphan sweep cannot read root");
                return;
            }
        };
        loop {
            let entry = match entries.next_entry().await {
                Ok(Some(entry)) => entry,
                Ok(None) => break,
                Err(error) => {
                    tracing::warn!(root = %root.display(), %error, "orphan sweep entry failed");
                    continue;
                }
            };
            let name = entry.file_name().to_string_lossy().into_owned();
            let is_symlink = entry
                .file_type()
                .await
                .map(|kind| kind.is_symlink())
                .unwrap_or(true);
            let Some((task_id, job_name)) = job_name_parts(&name) else {
                continue;
            };
            let evidence = self
                .orphan_evidence(pass, source_tag, &task_id, &job_name, &entry.path())
                .await;
            match evaluate_orphan(&name, is_symlink, evidence) {
                OrphanDecision::Remove => {
                    self.remove_orphan(pass, source_tag, &task_id, &job_name, root, &entry.path())
                        .await;
                }
                OrphanDecision::Keep | OrphanDecision::Ignore => {}
            }
        }
    }

    /// Gather ownership evidence for one candidate folder. `None` fails
    /// closed (the folder stays).
    async fn orphan_evidence(
        &self,
        pass: &Pass,
        source_tag: &SourceTag,
        task_id: &str,
        job_name: &str,
        path: &Path,
    ) -> Option<OrphanEvidence> {
        let has_cleanup_debt = self
            .ownership
            .has_cleanup_debt(&source_tag.to_string(), task_id, job_name)
            .await
            .ok()?;
        let task_status = self.ownership.task_status(task_id).await.ok()?;
        let task_active = task_status.is_some_and(|status| {
            matches!(status.as_str(), "queued" | "downloading" | "processing")
        });
        let bundles_settled = self.ownership.bundles_settled(task_id).await.ok()?;
        let source = pass.source_for(source_tag)?;
        let handle = SourceHandle {
            job_name: job_name.to_owned(),
            ..SourceHandle::new(source_tag.clone())
        };
        // A source that cannot tell whether it still owns the job (slskd:
        // the folder name carries no peer or filenames) answers `None`,
        // and the folder stays.
        let client_job_active = source.job_active(&handle).await?;
        // Real client-side mount truth; a lookup error fails closed.
        let mount_healthy = source.mount_healthy(&handle).await?;
        let now_ms = u64::try_from(self.journal.db().clock().now_ms()).unwrap_or(0);
        let now = UNIX_EPOCH + Duration::from_millis(now_ms);
        let age_seconds = tokio::fs::metadata(path)
            .await
            .ok()
            .and_then(|meta| meta.modified().ok())
            .and_then(|modified| now.duration_since(modified).ok())
            .map(|age| age.as_secs_f64())
            .unwrap_or(0.0);
        Some(OrphanEvidence {
            has_cleanup_debt,
            task_active,
            bundles_settled,
            mount_healthy,
            client_job_active,
            age_seconds,
        })
    }

    /// Discard client records, then remove the folder. A discard failure
    /// keeps the folder for the next sweep. Confinement is re-asserted
    /// immediately before removal: the path must still sit under the
    /// walked root and must not be a symlink.
    async fn remove_orphan(
        &self,
        pass: &Pass,
        source_tag: &SourceTag,
        task_id: &str,
        job_name: &str,
        root: &Path,
        path: &Path,
    ) {
        let is_symlink = tokio::fs::symlink_metadata(path)
            .await
            .map(|meta| meta.file_type().is_symlink())
            .unwrap_or(true);
        if !orphan_remove_allowed(root, path, is_symlink) {
            tracing::warn!(task_id, path = %path.display(), "orphan remove refused");
            return;
        }
        let Some(source) = pass.source_for(source_tag) else {
            return;
        };
        let handle = SourceHandle {
            job_name: job_name.to_owned(),
            ..SourceHandle::new(source_tag.clone())
        };
        if let Err(error) = source.discard(&handle).await {
            tracing::warn!(task_id, %error, "orphan client discard failed; folder kept");
            return;
        }
        if let Err(error) = tokio::fs::remove_dir_all(path).await {
            tracing::warn!(task_id, %error, "orphan folder remove failed");
        } else {
            tracing::info!(task_id, path = %path.display(), "orphan folder removed");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn orphan_removal_stays_confined() {
        let root = Path::new("/data/complete");
        assert!(orphan_remove_allowed(
            root,
            &root.join("droppedneedle-job"),
            false
        ));
        assert!(!orphan_remove_allowed(root, root, false));
        assert!(!orphan_remove_allowed(
            root,
            Path::new("/data/other/job"),
            false
        ));
        assert!(!orphan_remove_allowed(
            root,
            &root.join("droppedneedle-job"),
            true
        ));
    }
}
