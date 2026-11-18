//! `/api/v3/me/home-layout`: read and save the caller's home layout.

use axum::{Json, extract::State};

use droppedneedle_auth::User;
use droppedneedle_kernel::error::ApiError;
use droppedneedle_kernel::extract::ValidJson;
use droppedneedle_kernel::runtime_config::sections::{Personalization, PersonalizationMode};
use droppedneedle_kernel::runtime_config::settings_error::SettingsError;

use super::models::{HomeLayoutResponse, HomeLayoutSource, HomeLayoutUpdate};
use super::store::{Refused, Stored, check};
use crate::settings::wiring::SettingsSetup;

type JsonResult<T> = Result<Json<T>, ApiError>;

/// Read the layout the caller should see: their own, or the admin's while
/// personalization is forced. `editable` says whether they may save now.
#[utoipa::path(
    get,
    tag = "settings",
    path = "/api/v3/me/home-layout",
    responses((status = 200, description = "Home layout", body = HomeLayoutResponse))
)]
pub async fn get_home_layout(
    State(settings): State<SettingsSetup>,
    User(caller): User,
) -> JsonResult<HomeLayoutResponse> {
    let posture = settings.service().get::<Personalization>()?.mode;
    let (stored, source) = match posture {
        PersonalizationMode::Force => (
            settings.layouts().load_admin().await.map_err(internal)?,
            HomeLayoutSource::Admin,
        ),
        PersonalizationMode::Allow => (
            settings
                .layouts()
                .load(caller.user_id())
                .await
                .map_err(internal)?,
            HomeLayoutSource::User,
        ),
    };
    let editable = posture == PersonalizationMode::Allow || caller.is_admin();
    Ok(Json(response(stored, posture, source, editable)))
}

/// Replace the caller's own layout. Refused while personalization is forced,
/// unless the caller is an admin: theirs is the row everyone is served.
#[utoipa::path(
    put,
    tag = "settings",
    path = "/api/v3/me/home-layout",
    request_body = HomeLayoutUpdate,
    responses(
        (status = 200, description = "Saved layout", body = HomeLayoutResponse),
        (status = 409, description = "Personalization is forced")
    )
)]
pub async fn put_home_layout(
    State(settings): State<SettingsSetup>,
    User(caller): User,
    ValidJson(body): ValidJson<HomeLayoutUpdate>,
) -> JsonResult<HomeLayoutResponse> {
    let posture = settings.service().get::<Personalization>()?.mode;
    if posture == PersonalizationMode::Force && !caller.is_admin() {
        return Err(SettingsError::Conflict {
            message: "Home layouts are set by the admin right now.".to_owned(),
        }
        .into());
    }
    if let Err(Refused(message)) = check(&body) {
        return Err(SettingsError::InvalidInput { message }.into());
    }
    settings
        .layouts()
        .save(caller.user_id(), &body)
        .await
        .map_err(internal)?;
    Ok(Json(response(
        Some(Stored {
            version: body.version,
            widgets: body.widgets,
            hidden: body.hidden,
        }),
        posture,
        HomeLayoutSource::User,
        true,
    )))
}

/// A database failure is a server fault.
fn internal(error: droppedneedle_kernel::db::DbError) -> ApiError {
    SettingsError::internal(&error).into()
}

/// The wire answer for a stored layout, or the built-in order when none.
fn response(
    stored: Option<Stored>,
    posture: PersonalizationMode,
    source: HomeLayoutSource,
    editable: bool,
) -> HomeLayoutResponse {
    match stored {
        Some(layout) => HomeLayoutResponse {
            version: layout.version,
            widgets: layout.widgets,
            hidden: layout.hidden,
            posture,
            source,
            editable,
        },
        None => HomeLayoutResponse {
            version: 0,
            widgets: Vec::new(),
            hidden: Vec::new(),
            posture,
            source: HomeLayoutSource::Default,
            editable,
        },
    }
}
