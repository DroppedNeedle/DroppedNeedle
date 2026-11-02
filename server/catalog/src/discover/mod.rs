//! Discover: discover + queue + radio, and home
//! shelves with the range-pair redesign.
//!
//! The handlers call [`services`], which hold each feature's concrete
//! service: the live providers, the database, and honest "not configured"
//! answers where a feature needs setup first. [`wiring`] builds them.
//!
//! Wiring: the app mounts [`routes`], which also registers the paths in
//! the API document. The now-playing route is served by `playback`.

pub mod cache;
pub mod charts;
pub mod content;
pub mod demand;
pub mod error;
pub mod genre;
pub mod handlers;
pub mod home;
pub mod listening;
pub mod models;
pub mod ownership;
pub mod page;
pub mod ports;
pub mod previews;
pub mod queue;
pub mod radio;
pub mod scheduler;
pub mod sections;
pub mod services;
pub(crate) mod sql;
pub mod wiring;
pub mod youtube;

pub use handlers::routes;
pub use services::DiscoverDeps;

/// Every row of one statement, on a pooled reader connection.
pub(crate) async fn read_all<T, P, F>(
    db: &droppedneedle_kernel::db::Db,
    name: &'static str,
    statement: impl AsRef<str> + Send + 'static,
    params: P,
    map: F,
) -> Result<Vec<T>, droppedneedle_kernel::db::DbError>
where
    T: Send + 'static,
    P: rusqlite::Params + Send + 'static,
    F: FnMut(&rusqlite::Row<'_>) -> rusqlite::Result<T> + Send + 'static,
{
    db.read(name, move |conn| {
        let rows = conn
            .prepare(statement.as_ref())?
            .query_map(params, map)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    })
    .await
}
