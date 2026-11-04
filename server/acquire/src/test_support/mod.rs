//! What tests need from acquisition and production never uses: the
//! statement registry, a scratch database and settings store, a user row
//! for foreign keys, and the imports and queue routers with no layers.
//!
//! Compiled for this crate's tests and for the `test-support` feature only.

pub(crate) mod sql;

use std::sync::Arc;

use droppedneedle_kernel::db::Db;
use droppedneedle_kernel::db::statement::StatementFile;
use droppedneedle_kernel::runtime_config::ConfigStore;

use crate::db::AcquireDb;
use crate::downloads::http::{DownloadsRoutes, routes};
use crate::downloads::upgrades::Upgrades;
use crate::downloads::worker::DownloadWorker;
use crate::imports::handlers::ImportsDeps;

/// Every `sql.rs` of this crate, one line each, for the schema test.
pub fn statement_files() -> Vec<StatementFile> {
    vec![
        crate::downloads::sql::registered(),
        crate::drop_import::sql::registered(),
        crate::flows::sql::registered(),
        crate::follows::sql::registered(),
        crate::held::sql::registered(),
        crate::imports::sql::registered(),
        crate::landing::sql::registered(),
        crate::requests::sql::registered(),
        crate::search_jobs::sql::registered(),
        crate::test_support::sql::registered(),
    ]
}

/// A settings store for tests over `settings.db` in `dir`, with a fixed
/// test key. Opening the same `dir` again reopens the same rows.
pub fn scratch_settings(dir: &std::path::Path) -> Result<ConfigStore, String> {
    let crypto = droppedneedle_kernel::runtime_config::Crypto::from_key_bytes(&[7u8; 32])
        .map_err(|error| error.to_string())?;
    ConfigStore::scratch(dir, crypto)
}

impl AcquireDb {
    /// Scratch database for tests: a private copy of the migrated template
    /// in the temp dir, removed once the last handle on it has closed.
    pub fn scratch() -> Result<Self, String> {
        Ok(Self::new(
            Db::scratch("acquire-db")?,
            Arc::new(droppedneedle_kernel::clock::SystemClock),
        ))
    }

    /// Insert one user row so foreign keys to `auth_users` hold in tests.
    pub async fn add_user(&self, id: &str, display_name: &str, role: &str) -> Result<(), String> {
        let (id, display_name, role) = (id.to_owned(), display_name.to_owned(), role.to_owned());
        self.write("test.add_user", move |tx| {
            tx.execute(
                sql::INSERT_TEST_USER.text(),
                rusqlite::params![id, display_name, role],
            )?;
            Ok(())
        })
        .await
        .map_err(|error| error.to_string())
    }
}

/// Every imports route in one router with no layers, for tests.
pub fn imports_router(deps: ImportsDeps) -> axum::Router {
    crate::imports::handlers::routes()
        .with_state(deps)
        .into_router()
}

/// Queue routes over default settings (upgrades off), for tests. The
/// settings sit beside the worker's scratch database.
pub fn downloads_router(worker: Arc<DownloadWorker>) -> Result<axum::Router, String> {
    let db = worker.journal().db().clone();
    let dir = db
        .path()
        .parent()
        .map(|dir| dir.join("settings"))
        .ok_or_else(|| "scratch database has no directory".to_owned())?;
    let settings = scratch_settings(&dir)?;
    let dispatch = crate::dispatch::DownloadDispatch::new(
        worker.journal().clone(),
        Arc::new(droppedneedle_kernel::ids::UuidGenerator),
        settings.live(),
    );
    let upgrades = Upgrades::new(
        crate::flows::stores::UpgradeWorklist::new(db),
        settings.live(),
        Arc::new(dispatch),
    );
    Ok(routes()
        .with_state(DownloadsRoutes::new(
            worker.journal().clone(),
            worker,
            upgrades,
        ))
        .into_router())
}
