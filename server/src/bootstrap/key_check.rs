//! The data key at start, and the reset that forgets what a lost key sealed.
//!
//! [`verify`] refuses a start whose key file is not the one the database's
//! secrets were sealed with (see `runtime_config::key_check`). A missing key
//! is refused earlier, by `data_dirs::refuse_lost_key`, before any key is
//! minted. [`forget_secrets`] is the clearing half of the tool's
//! `reset-key`, for an operator whose key is gone for good. It lives here
//! because it reaches the kernel, auth and media in one transaction.

use std::path::Path;
use std::sync::Arc;

use droppedneedle_kernel::clock::Clock;
use droppedneedle_kernel::db::{Db, DbError, DurableLane};
use droppedneedle_kernel::runtime_config::key_check::{
    self, KeyCheck, enforce, forget_sealed_settings,
};
use droppedneedle_kernel::runtime_config::{ConfigError, Crypto};

use super::BootError;
use super::data_dirs::key_refusal;

/// Check the key against the database, recording its check value on the
/// first start that has none. Runs after the settings open, so after the
/// one-time `config.json` copy.
pub(super) async fn verify(
    db: &Db,
    crypto: &Arc<Crypto>,
    clock: &Arc<dyn Clock>,
    config_dir: &Path,
    db_path: &Path,
) -> Result<(), BootError> {
    let outcome = super::wait_out_busy("key check", ConfigError::is_busy, || {
        let (db, crypto, clock) = (db.clone(), crypto.clone(), clock.clone());
        async move {
            tokio::task::spawn_blocking(move || enforce(&db, &crypto, clock.as_ref()))
                .await
                .map_err(|error| ConfigError::Database(error.to_string()))?
        }
    })
    .await?;
    match outcome {
        KeyCheck::Mismatch => Err(BootError::DataDir(key_refusal(config_dir, db_path, false))),
        KeyCheck::Matches | KeyCheck::Recorded => Ok(()),
    }
}

/// What [`forget_secrets`] cleared. Counts and setting paths only.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Forgotten {
    /// Settings values emptied, as `section.field` paths.
    pub settings: Vec<String>,
    /// App passwords revoked.
    pub app_passwords: usize,
    /// Federated sign-in token sets dropped.
    pub sign_in_tokens: usize,
    /// Per-user service links deleted.
    pub connections: usize,
}

/// In one transaction, clear every value sealed under the old key and
/// store the check value of `new_key`, stamped with `clock`'s time. The
/// places cleared are the ones `runtime_config::key_check::assess`
/// probes; keep the two in step.
pub fn forget_secrets(
    db: &Db,
    new_key: &Crypto,
    clock: &dyn Clock,
) -> Result<Forgotten, ConfigError> {
    let now = clock.now_ms();
    db.write_durable(DurableLane::Foreground, "key.forget_secrets", |tx| {
        let settings = forget_sealed_settings(tx, now)?;
        let auth = droppedneedle_auth::sqlite::forget_sealed(tx).map_err(DbError::from)?;
        let connections =
            droppedneedle_media::remotes::connections::forget_sealed(tx).map_err(DbError::from)?;
        key_check::record(tx, new_key, now)?;
        Ok(Forgotten {
            settings,
            app_passwords: auth.app_passwords,
            sign_in_tokens: auth.sign_in_tokens,
            connections,
        })
    })
}
