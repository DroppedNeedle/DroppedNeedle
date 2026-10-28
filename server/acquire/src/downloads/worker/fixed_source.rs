//! A scripted download source for tests.

use std::path::PathBuf;

use crate::downloads::source_tag::SourceTag;
use crate::downloads::sources::{DownloadSource, SourceError, SourceHandle};
use futures_util::future::BoxFuture;

/// A test source whose every download is finished at once and landed at
/// fixed paths, so journeys can drive the worker end to end. It answers
/// as the plugin source `fixed`.
pub struct FixedSource {
    mount: PathBuf,
    paths: Vec<PathBuf>,
    discard_fails: std::sync::atomic::AtomicBool,
}

impl FixedSource {
    /// Downloads land at `paths`, inside the downloads folder `mount`.
    pub fn new(mount: PathBuf, paths: Vec<PathBuf>) -> Self {
        Self {
            mount,
            paths,
            discard_fails: std::sync::atomic::AtomicBool::new(false),
        }
    }

    /// Make every later discard fail (or succeed again).
    pub fn fail_discards(&self, fail: bool) {
        self.discard_fails
            .store(fail, std::sync::atomic::Ordering::Relaxed);
    }

    fn names(&self) -> Vec<String> {
        self.paths
            .iter()
            .map(|path| path.to_string_lossy().into_owned())
            .collect()
    }
}

impl DownloadSource for FixedSource {
    fn tag(&self) -> SourceTag {
        SourceTag::Plugin("fixed".to_owned())
    }

    fn search<'a>(
        &'a self,
        _target: &'a crate::target::SearchTarget,
    ) -> BoxFuture<'a, Result<Vec<crate::search_jobs::candidates::Candidate>, SourceError>> {
        Box::pin(async { Ok(Vec::new()) })
    }

    fn enqueue<'a>(
        &'a self,
        task_id: &'a str,
        candidate_index: i64,
    ) -> BoxFuture<'a, Result<SourceHandle, SourceError>> {
        Box::pin(async move {
            Ok(SourceHandle {
                filenames: self.names(),
                job_name: format!("fixed-{task_id}-{candidate_index}"),
                ..SourceHandle::new(self.tag())
            })
        })
    }

    fn poll<'a>(
        &'a self,
        _handle: &'a SourceHandle,
    ) -> BoxFuture<'a, Result<crate::downloads::sources::TransferProgress, SourceError>> {
        Box::pin(async move {
            Ok(crate::downloads::sources::TransferProgress {
                all_terminal: true,
                all_succeeded: true,
                has_active_transfer: false,
                downloaded_bytes: 1,
                succeeded_filenames: self.names(),
                ..Default::default()
            })
        })
    }

    fn landed<'a>(
        &'a self,
        _handle: &'a SourceHandle,
    ) -> BoxFuture<'a, Result<crate::landing::Landed, SourceError>> {
        Box::pin(async move {
            Ok(crate::landing::Landed {
                mounts: vec![self.mount.clone().into()],
                files: self
                    .paths
                    .iter()
                    .cloned()
                    .map(crate::landing::Reported::path)
                    .collect(),
            })
        })
    }

    fn discard<'a>(
        &'a self,
        _handle: &'a SourceHandle,
    ) -> BoxFuture<'a, Result<bool, SourceError>> {
        Box::pin(async move {
            if self
                .discard_fails
                .load(std::sync::atomic::Ordering::Relaxed)
            {
                Err(SourceError::LocalFault("discard refused".to_owned()))
            } else {
                Ok(true)
            }
        })
    }

    fn abort<'a>(&'a self, _handle: &'a SourceHandle) -> BoxFuture<'a, Result<bool, SourceError>> {
        Box::pin(async { Ok(true) })
    }
}
