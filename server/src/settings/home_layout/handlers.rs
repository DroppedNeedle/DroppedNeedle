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
    let (stored, source, first_admin_id) = match posture {
        PersonalizationMode::Force => {
            let admin = settings.layouts().load_admin().await.map_err(internal)?;
            match admin {
                Some((id, layout)) => (layout, HomeLayoutSource::Admin, Some(id)),
                None => (None, HomeLayoutSource::Admin, None),
            }
        }
        PersonalizationMode::Allow => (
            settings
                .layouts()
                .load(caller.user_id())
                .await
                .map_err(internal)?,
            HomeLayoutSource::User,
            None,
        ),
    };
    let editable = match posture {
        PersonalizationMode::Allow => true,
        PersonalizationMode::Force => first_admin_id
            .as_deref()
            .map(|id| id == caller.user_id())
            .unwrap_or(false),
    };
    Ok(Json(response(stored, posture, source, editable)))
}

/// Replace the caller's own layout. Refused while personalization is forced,
/// unless the caller is the first admin: theirs is the row everyone is served.
#[utoipa::path(
    put,
    tag = "settings",
    path = "/api/v3/me/home-layout",
    request_body = HomeLayoutUpdate,
    responses(
        (status = 200, description = "Saved layout", body = HomeLayoutResponse),
        (status = 403, description = "Personalization is forced"),
        (status = 409, description = "Stale revision")
    )
)]
pub async fn put_home_layout(
    State(settings): State<SettingsSetup>,
    User(caller): User,
    ValidJson(body): ValidJson<HomeLayoutUpdate>,
) -> JsonResult<HomeLayoutResponse> {
    let posture = settings.service().get::<Personalization>()?.mode;

    // Force mode: only the first admin may save; everyone else is refused.
    let stored_target = if posture == PersonalizationMode::Force {
        let admin = settings.layouts().load_admin().await.map_err(internal)?;
        match admin {
            Some((id, layout)) if id == caller.user_id() => layout,
            Some(_) => {
                return Err(ApiError::forbidden(
                    "Home layouts are set by the admin right now.",
                ));
            }
            None => {
                return Err(ApiError::forbidden(
                    "Home layouts are set by the admin right now.",
                ));
            }
        }
    } else {
        settings
            .layouts()
            .load(caller.user_id())
            .await
            .map_err(internal)?
    };

    // Optimistic concurrency: a stale revision is refused, unless there is no
    // stored row yet, in which case any revision is accepted.
    if let Some(stored) = &stored_target
        && stored.revision != body.revision
    {
        return Err(SettingsError::StaleRevision {
            message: "The home layout changed since it was read; refresh and retry.".to_owned(),
        }
        .into());
    }

    if let Err(Refused(message)) = check(&body) {
        return Err(SettingsError::InvalidInput { message }.into());
    }

    let new_revision = match settings
        .layouts()
        .save(caller.user_id(), &body)
        .await
        .map_err(internal)?
    {
        Some(revision) => revision,
        None => {
            return Err(SettingsError::StaleRevision {
                message: "The home layout changed since it was read; refresh and retry.".to_owned(),
            }
            .into());
        }
    };

    let source = if posture == PersonalizationMode::Force {
        HomeLayoutSource::Admin
    } else {
        HomeLayoutSource::User
    };
    Ok(Json(HomeLayoutResponse {
        version: body.version,
        revision: new_revision,
        widgets: body.widgets,
        hidden: body.hidden,
        posture,
        source,
        editable: true,
    }))
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
            revision: layout.revision,
            widgets: layout.widgets,
            hidden: layout.hidden,
            posture,
            source,
            editable,
        },
        None => HomeLayoutResponse {
            version: 0,
            revision: 0,
            widgets: Vec::new(),
            hidden: Vec::new(),
            posture,
            source: if posture == PersonalizationMode::Force {
                HomeLayoutSource::Admin
            } else {
                HomeLayoutSource::Default
            },
            editable,
        },
    }
}
