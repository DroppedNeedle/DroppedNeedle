//! Acquisition's side of an album removal (v2 `purge_album_downloads`
//! plus the wanted follow-up): the album's failed downloads stop
//! retrying, its held files, held rows and blocklist entries go, and its
//! wanted watch stops or rearms. Every step is best effort and logged:
//! the user already confirmed the removal.

use std::sync::Arc;

use futures_util::future::BoxFuture;

use super::dispatch::Journal;
use super::requests::sqlite::WantedStore;
use crate::library::mutations::AlbumRemovalHook;

/// Cleans up download and wanted state for removed albums.
pub struct AcquireAlbumCleanup {
    journal: Arc<Journal>,
    wanted: WantedStore,
}

impl AcquireAlbumCleanup {
    /// Cleanup over the download journal and the wanted watches.
    pub fn new(journal: Arc<Journal>, wanted: WantedStore) -> Self {
        Self { journal, wanted }
    }
}

impl AlbumRemovalHook for AcquireAlbumCleanup {
    fn album_removed<'a>(
        &'a self,
        release_group_mbid: &'a str,
        stop_wanted: bool,
    ) -> BoxFuture<'a, ()> {
        Box::pin(async move {
            let group = release_group_mbid.to_owned();
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|elapsed| elapsed.as_secs_f64())
                .unwrap_or(0.0);
            match self
                .journal
                .run_foreground("downloads.purge_album", move |store| {
                    store.purge_album(&group, now)
                })
                .await
            {
                Ok(purge) => {
                    let held = purge.held_paths.len();
                    let unlinked = tokio::task::spawn_blocking(move || {
                        for path in &purge.held_paths {
                            if let Err(error) = std::fs::remove_file(path)
                                && error.kind() != std::io::ErrorKind::NotFound
                            {
                                tracing::warn!(%error, "held file of a removed album was kept");
                            }
                        }
                    })
                    .await;
                    if let Err(error) = unlinked {
                        tracing::warn!(%error, "held file cleanup did not finish");
                    }
                    tracing::info!(
                        release_group_mbid,
                        retries_cancelled = purge.retries_cancelled,
                        held_files = held,
                        "download state of a removed album cleaned up"
                    );
                }
                Err(error) => {
                    tracing::warn!(%error, release_group_mbid, "download cleanup after album removal failed");
                }
            }
            let wanted = if stop_wanted {
                self.wanted.stop_after_removal(release_group_mbid).await
            } else {
                self.wanted
                    .rearm_after_removal(release_group_mbid, now as u64)
                    .await
            };
            if let Err(error) = wanted {
                tracing::warn!(
                    ?error,
                    release_group_mbid,
                    "wanted follow-up after album removal failed"
                );
            }
        })
    }
}
