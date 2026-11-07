//! A person's custom CSS as an admin sees it: a summary that never holds
//! the text, and a switch that turns it off and keeps the text, so the
//! person sees what happened and can switch it back on. An unknown user
//! answers 404, as with the quota pair.

use super::error::AdminError;
use crate::appearance::models::CustomCssSummary;
use crate::appearance::service::summary;
use crate::appearance::store::AppearanceStore;
use droppedneedle_auth::users::UsersDeps;
use droppedneedle_kernel::db::DbError;

/// One user's custom CSS summary.
pub async fn get(
    users: &UsersDeps,
    store: &AppearanceStore,
    user_id: &str,
) -> Result<CustomCssSummary, AdminError> {
    known_user(users, user_id).await?;
    let stored = store.read(user_id).await.map_err(store_error)?;
    Ok(summary(stored))
}

/// Turn one user's custom CSS off. A user with no row gets none.
pub async fn turn_off(
    users: &UsersDeps,
    store: &AppearanceStore,
    user_id: &str,
) -> Result<CustomCssSummary, AdminError> {
    known_user(users, user_id).await?;
    let stored = store.turn_off(user_id).await.map_err(store_error)?;
    Ok(summary(stored))
}

/// A 404 for a user id no account has.
async fn known_user(users: &UsersDeps, user_id: &str) -> Result<(), AdminError> {
    users
        .users
        .get_by_id(user_id)
        .await
        .map_err(|error| {
            AdminError::internal(&format_args!("custom CSS user read failed: {error}"))
        })?
        .map(|_| ())
        .ok_or(AdminError::NotFound)
}

fn store_error(error: DbError) -> AdminError {
    match error {
        DbError::Busy { .. } => AdminError::Busy {
            cause: format!("custom CSS: {error}"),
        },
        other => AdminError::internal(&format_args!("custom CSS store failed: {other}")),
    }
}
