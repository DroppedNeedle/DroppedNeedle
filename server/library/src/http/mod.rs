//! Library HTTP: models, handlers, and routes under `/api/v3/library`.

use droppedneedle_kernel::error::ApiError;

pub mod contrib;
pub mod corrections;
pub mod edition;
pub mod error;
pub mod handlers;
pub mod history;
pub mod management;
pub mod management_tools;
pub mod models;
pub mod mutations;
pub mod operations;
pub mod reconcile;
pub mod recycle_bin;
pub mod repairs;
pub mod reviews;
pub mod routes;
pub mod scan;

/// Run store-backed work on a blocking thread: the library stores are
/// synchronous SQLite and must not stall the async workers. The work's
/// error answers through `ApiError: From<E>`.
pub(crate) async fn blocking<T, E>(
    work: impl FnOnce() -> Result<T, E> + Send + 'static,
) -> Result<T, ApiError>
where
    T: Send + 'static,
    E: Send + 'static,
    ApiError: From<E>,
{
    tokio::task::spawn_blocking(work)
        .await
        .map_err(ApiError::internal)?
        .map_err(ApiError::from)
}
