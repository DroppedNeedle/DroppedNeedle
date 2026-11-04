//! What tests need from this crate and production never uses: the statement registry, a report queue with no worker and the
//! stream route over any engine.
//!
//! Compiled for this crate's tests and for the `test-support` feature only.

use crate::playback::reports::ReportQueue;
use crate::stream::engine::StreamEngine;
use crate::stream::routes::{StreamState, stream_get, stream_head};
use droppedneedle_kernel::db::statement::StatementFile;

impl ReportQueue {
    /// Build a queue with no worker: every report drops immediately. For
    /// tests that never assert what a server was sent.
    pub fn detached() -> Self {
        let (queue, _) = Self::channel();
        queue
    }
}

/// The stream route at a relative path over any engine, for tests that
/// drive it with fakes.
pub fn stream_routes<E>(state: StreamState<E>) -> axum::Router
where
    E: StreamEngine + Send + Sync + 'static,
{
    axum::Router::new()
        .route(
            "/stream/{source}/{*key}",
            axum::routing::get(stream_get::<E>).head(stream_head::<E>),
        )
        .with_state(state)
}

/// Every `sql.rs` of this crate, one line each, for the schema test.
pub fn statement_files() -> Vec<StatementFile> {
    vec![
        crate::playback::sql::registered(),
        crate::remotes::sql::registered(),
        crate::stream::sql::registered(),
    ]
}
