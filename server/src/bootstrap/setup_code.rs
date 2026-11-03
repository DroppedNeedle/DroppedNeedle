//! The code first-admin setup asks for on a server started fresh from v2.
//!
//! While a v2 backup is kept and no account exists, setup needs a one-time
//! code from the server log. After a fresh start nothing came across from
//! v2, federated sign-ins cannot be on (their settings start empty), and
//! setup is the only way in: without the code, whoever reached the page
//! first would own the server. The backup folder on disk is the marker; the
//! code is never stored. Each start mints and logs a new one while the rule
//! holds, so a restart never locks the owner out. A start that follows
//! upgrade mode in the same process keeps its code, so the log holds one.

use droppedneedle_auth::{session::one_time_code::OneTimeCode, sqlite::Users};
use droppedneedle_kernel::config::AppConfig;

use super::{BootError, data_dirs::DataDirs};

/// The code setup asks for, logged, or `None` when setup needs none. A
/// users read that fails counts as no account: it asks for the code.
pub(super) async fn setup_code(
    config: &AppConfig,
    users: &Users,
    seed: Option<OneTimeCode>,
) -> Result<Option<OneTimeCode>, BootError> {
    let dirs = DataDirs::from_config(config);
    let backup = tokio::task::spawn_blocking(move || dirs.holds_backup())
        .await
        .unwrap_or(true);
    if !backup {
        return Ok(None);
    }
    let accounts = match users.list(1, 0).await {
        Ok((_, total)) => total,
        Err(error) => {
            tracing::warn!(%error, "cannot tell whether an account exists; setup asks for the code");
            0
        }
    };
    if accounts > 0 {
        return Ok(None);
    }
    let code = match seed {
        Some(code) => code,
        None => OneTimeCode::mint().map_err(|error| BootError::Stage {
            stage: "setup code",
            message: error.to_string(),
        })?,
    };
    tracing::warn!(
        code = %code.spaced(),
        "first-admin setup code: this server was started fresh from v2, so setup asks for this \
         code; a new one is logged at each start until the first account exists"
    );
    Ok(Some(code))
}
