//! One person's choices for their music apps (`/me/connect-apps`), kept in
//! `compat_user_settings` and read on each list.

pub mod sql;

use axum::{Json, extract::State};
use droppedneedle_auth::User;
use droppedneedle_kernel::db::{Db, DbError, Lane};
use droppedneedle_kernel::error::{ApiError, Unavailable};
use droppedneedle_kernel::extract::ValidJson;
use droppedneedle_kernel::mount::{Routes, routes};
use rusqlite::{OptionalExtension as _, params};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// The caller's choices for their music apps.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, ToSchema)]
pub struct MyConnectApps {
    /// Apps list other people's public playlists as well as the caller's.
    pub include_public_playlists: bool,
}

/// The store behind the switch.
#[derive(Debug, Clone)]
pub struct UserSettings {
    db: Db,
}

impl UserSettings {
    /// The store over one database.
    pub fn new(db: Db) -> Self {
        Self { db }
    }

    /// Whether the user's apps list other people's public playlists.
    pub async fn include_public_playlists(&self, user_id: &str) -> Result<bool, DbError> {
        let user_id = user_id.to_owned();
        self.db
            .read("compat.user_settings.read", move |conn| {
                let stored = conn
                    .query_row(sql::INCLUDE_PUBLIC_PLAYLISTS.text(), [&user_id], |row| {
                        row.get::<_, bool>(0)
                    })
                    .optional()?;
                Ok::<_, DbError>(stored.unwrap_or(false))
            })
            .await
    }

    /// Save the choice.
    pub async fn set_include_public_playlists(
        &self,
        user_id: &str,
        include: bool,
    ) -> Result<(), DbError> {
        let user_id = user_id.to_owned();
        self.db
            .write(Lane::Foreground, "compat.user_settings.write", move |tx| {
                tx.execute(
                    sql::SET_INCLUDE_PUBLIC_PLAYLISTS.text(),
                    params![user_id, include],
                )?;
                Ok::<_, DbError>(())
            })
            .await
    }
}

fn api_error(error: DbError) -> ApiError {
    match error {
        DbError::Busy { .. } => ApiError::unavailable(
            Unavailable::ServerBusy,
            format!("database busy during connect apps choice: {error}"),
        ),
        other => ApiError::internal(format!("connect apps choice failed: {other}")),
    }
}

/// Read your own Connect Apps choices.
#[utoipa::path(
    get,
    tag = "users",
    path = "/api/v3/me/connect-apps",
    responses((status = 200, description = "Own Connect Apps choices", body = MyConnectApps))
)]
pub async fn get_my_connect_apps(
    State(store): State<UserSettings>,
    User(caller): User,
) -> Result<Json<MyConnectApps>, ApiError> {
    let include_public_playlists = store
        .include_public_playlists(caller.user_id())
        .await
        .map_err(api_error)?;
    Ok(Json(MyConnectApps {
        include_public_playlists,
    }))
}

/// Save your own Connect Apps choices.
#[utoipa::path(
    put,
    tag = "users",
    path = "/api/v3/me/connect-apps",
    request_body = MyConnectApps,
    responses((status = 200, description = "Saved choices", body = MyConnectApps))
)]
pub async fn put_my_connect_apps(
    State(store): State<UserSettings>,
    User(caller): User,
    ValidJson(body): ValidJson<MyConnectApps>,
) -> Result<Json<MyConnectApps>, ApiError> {
    store
        .set_include_public_playlists(caller.user_id(), body.include_public_playlists)
        .await
        .map_err(api_error)?;
    Ok(Json(body))
}

/// The caller's own Connect Apps choices, for any signed-in user.
pub fn routes() -> Routes<UserSettings> {
    Routes::new().user(routes!(get_my_connect_apps, put_my_connect_apps))
}
