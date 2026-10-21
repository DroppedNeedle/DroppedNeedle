//! Integration tests for acquisition: requests, searches, downloads and imports.

#[path = "../common/mod.rs"]
mod common;

mod acquire_album_removal;
mod acquire_download_tasks;
mod acquire_downloads;
mod acquire_drop_import;
mod acquire_flows;
mod acquire_imports;
mod acquire_journey;
mod acquire_landing;
mod acquire_requests;
mod acquire_search_jobs;
mod acquire_slskd;
mod acquire_sources;
mod acquire_track_album;
mod acquire_usenet;

use droppedneedle_acquire::downloads::DownloadSource;

/// One source as the worker holds it.
fn as_source<S: DownloadSource + 'static>(
    source: std::sync::Arc<S>,
) -> std::sync::Arc<dyn DownloadSource> {
    source
}
