//! A password recovery code from the host, for an admin locked out of the
//! web UI.
//!
//! Whoever can run this can already read and write the data directory, so
//! it asks for no credential of its own. The code is the one the admin
//! route issues: same store, same expiry, single use, only its hash kept.
//! Unlike that route it also issues for an account that only signs in
//! through single sign-on, Plex or Jellyfin, and the reset then gives
//! that account its first password. The exclusive [`DataLock`] is held
//! for the run, so the server has to be stopped first.

use std::path::PathBuf;

use thiserror::Error;

use droppedneedle::bootstrap::data_dirs::{BootRefusal, DataDirs, check_bootable};
use droppedneedle_auth::prod::{AuthDb, RecoveryCodes, Users};
use droppedneedle_auth::users::roles::Role;
use droppedneedle_auth::users::services::{
    IssuedRecoveryCode, RecoveryIssueError, RecoveryReach, issue_recovery_code,
};
use droppedneedle_kernel::clock::{Clock as _, SystemClock};
use droppedneedle_kernel::db::datalock::{DataLock, DataLockError};
use droppedneedle_kernel::db::{DbConfig, DbRuntime, open_runtime};

/// Users read per page when listing admins.
const PAGE: u64 = 200;

/// Why no code was issued. No code row is written in any of these cases.
#[derive(Debug, Error)]
pub enum RecoveryCodeError {
    /// A running server (or another tool run) holds the data directory.
    #[error(
        "The data directory is in use ({} is locked). Stop the server first, then run this \
         again.",
        .0.display()
    )]
    ServerRunning(PathBuf),
    /// The folders do not hold a v3 instance.
    #[error("{0}")]
    NotV3(BootRefusal),
    /// The folders hold no database yet.
    #[error("There is no DroppedNeedle database at {}.", .0.display())]
    NoDatabase(PathBuf),
    /// No account has this username.
    #[error(
        "No user named {0:?}. Give the account's DroppedNeedle username, which is not always \
         the name it has with a single sign-on provider. `droppedneedle-tool recovery-code \
         --list-admins` prints the admin usernames."
    )]
    UnknownUser(String),
    /// The lock, the database or the random generator failed.
    #[error("Could not create a recovery code: {0}")]
    Failed(String),
}

/// Issue a recovery code for `username` in the data directory `dirs`.
pub async fn issue(
    dirs: &DataDirs,
    username: &str,
) -> Result<IssuedRecoveryCode, RecoveryCodeError> {
    let (_lock, runtime) = open_stopped(dirs).await?;
    let outcome = issue_in(&runtime, username).await;
    runtime.shutdown().await;
    outcome
}

/// The usernames of every admin account, oldest account first. An admin
/// without a username is left out: no code can be issued for it.
pub async fn admin_usernames(dirs: &DataDirs) -> Result<Vec<String>, RecoveryCodeError> {
    let (_lock, runtime) = open_stopped(dirs).await?;
    let outcome = admins_in(&runtime).await;
    runtime.shutdown().await;
    outcome
}

/// Check the folders hold a v3 instance, take the lock, open the database.
async fn open_stopped(dirs: &DataDirs) -> Result<(DataLock, DbRuntime), RecoveryCodeError> {
    // The lock creates the folder and a `.lock` file, so a folder with no
    // database, or a v2 one, is refused before it is taken. The same checks
    // run again under the lock.
    check_bootable(dirs).map_err(RecoveryCodeError::NotV3)?;
    if !dirs.db_path.is_file() {
        return Err(RecoveryCodeError::NoDatabase(dirs.db_path.clone()));
    }
    let lock = DataLock::exclusive(&dirs.db_path).map_err(|error| match error {
        DataLockError::Held { path } => RecoveryCodeError::ServerRunning(path),
        other @ DataLockError::Io { .. } => RecoveryCodeError::Failed(other.to_string()),
    })?;
    check_bootable(dirs).map_err(RecoveryCodeError::NotV3)?;
    // Opening creates a missing database, and an empty one has no users.
    if !dirs.db_path.is_file() {
        return Err(RecoveryCodeError::NoDatabase(dirs.db_path.clone()));
    }
    let runtime = open_runtime(&DbConfig::new(&dirs.db_path))
        .await
        .map_err(|error| RecoveryCodeError::Failed(error.to_string()))?;
    Ok((lock, runtime))
}

fn failed(error: &dyn std::fmt::Display) -> RecoveryCodeError {
    RecoveryCodeError::Failed(error.to_string())
}

async fn issue_in(
    runtime: &DbRuntime,
    username: &str,
) -> Result<IssuedRecoveryCode, RecoveryCodeError> {
    let unknown = || RecoveryCodeError::UnknownUser(username.to_owned());
    let db = AuthDb::new(runtime.db(), std::sync::Arc::new(SystemClock));
    let users = Users::new(&db);
    let user = users
        .get_by_username(&username.trim().to_lowercase())
        .await
        .map_err(|error| failed(&error))?
        .ok_or_else(unknown)?;
    issue_recovery_code(
        &users,
        &RecoveryCodes::new(&db),
        &user.id,
        RecoveryReach::AnyAccount,
        SystemClock.now_unix(),
    )
    .await
    .map_err(|error| match error {
        RecoveryIssueError::UnknownUser => unknown(),
        other => failed(&other),
    })
}

async fn admins_in(runtime: &DbRuntime) -> Result<Vec<String>, RecoveryCodeError> {
    let db = AuthDb::new(runtime.db(), std::sync::Arc::new(SystemClock));
    let users = Users::new(&db);
    let mut names = Vec::new();
    let mut offset = 0;
    loop {
        let (page, total) = users
            .list(PAGE, offset)
            .await
            .map_err(|error| failed(&error))?;
        names.extend(
            page.into_iter()
                .filter(|user| user.role == Role::Admin)
                .filter_map(|user| user.username),
        );
        offset += PAGE;
        if offset >= total {
            return Ok(names);
        }
    }
}
