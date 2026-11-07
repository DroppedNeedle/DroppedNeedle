//! Thin appearance handlers. The one `From<AppearanceError> for ApiError`
//! here gives the service's errors a status.

use axum::{Json, extract::State, http::StatusCode};
use droppedneedle_auth::{Admin, User};
use droppedneedle_kernel::error::{ApiError, Unavailable};
use droppedneedle_kernel::extract::ValidJson;
use droppedneedle_kernel::runtime_config::sections::Appearance;

use super::check::Limit;
use super::models::{AppearanceImported, AppearanceUpdate, CssRefusal, CssSave, MyAppearance};
use super::service::{self, AppearanceError};
use super::wiring::AppearanceSetup;
use droppedneedle_kernel::db::DbError;

/// A sheet the check refuses.
pub const CUSTOM_CSS_REFUSED: &str = "CUSTOM_CSS_REFUSED";
/// A sheet past its length limit.
pub const CUSTOM_CSS_TOO_LONG: &str = "CUSTOM_CSS_TOO_LONG";
/// People's own CSS is turned off on this server.
pub const CUSTOM_CSS_TURNED_OFF: &str = "CUSTOM_CSS_TURNED_OFF";

impl From<AppearanceError> for ApiError {
    fn from(error: AppearanceError) -> Self {
        match error {
            AppearanceError::Refused(problems) => ApiError::new(
                StatusCode::BAD_REQUEST,
                CUSTOM_CSS_REFUSED,
                "This CSS uses something that is not allowed.",
            )
            .with_details(serde_json::to_value(CssRefusal { problems }).unwrap_or_default()),
            AppearanceError::TooLong { limit } => ApiError::new(
                StatusCode::BAD_REQUEST,
                CUSTOM_CSS_TOO_LONG,
                format!("This CSS is longer than {limit} characters."),
            )
            .with_details(serde_json::json!({ "limit": limit })),
            AppearanceError::TurnedOff => ApiError::new(
                StatusCode::FORBIDDEN,
                CUSTOM_CSS_TURNED_OFF,
                "Custom CSS is turned off on this server.",
            ),
            AppearanceError::Store(error @ DbError::Busy { .. }) => ApiError::unavailable(
                Unavailable::ServerBusy,
                format!("database busy during an appearance change: {error}"),
            ),
            AppearanceError::Store(error) => {
                ApiError::internal(format!("appearance store failed: {error}"))
            }
        }
    }
}

/// Read your own theme and custom CSS, with the site's CSS.
#[utoipa::path(
    get,
    tag = "users",
    path = "/api/v3/me/appearance",
    responses((status = 200, description = "Own appearance", body = MyAppearance))
)]
pub async fn get_my_appearance(
    State(setup): State<AppearanceSetup>,
    User(caller): User,
) -> Result<Json<MyAppearance>, ApiError> {
    service::read(&setup.store, &setup.section, caller.user_id())
        .await
        .map(Json)
        .map_err(ApiError::from)
}

/// Save your own theme or custom CSS. A field left out keeps its value,
/// and a change any part of which is refused saves nothing.
#[utoipa::path(
    put,
    tag = "users",
    path = "/api/v3/me/appearance",
    request_body = AppearanceUpdate,
    responses(
        (status = 200, description = "Saved appearance", body = MyAppearance),
        (status = 400, description = "The CSS is refused (`CUSTOM_CSS_REFUSED`, its problems in `details`) or too long (`CUSTOM_CSS_TOO_LONG`)"),
        (status = 403, description = "Custom CSS is turned off on this server (`CUSTOM_CSS_TURNED_OFF`)"),
    )
)]
pub async fn put_my_appearance(
    State(setup): State<AppearanceSetup>,
    User(caller): User,
    ValidJson(body): ValidJson<AppearanceUpdate>,
) -> Result<Json<MyAppearance>, ApiError> {
    service::save(&setup.store, &setup.section, caller.user_id(), body)
        .await
        .map(Json)
        .map_err(ApiError::from)
}

/// Move the theme and CSS this browser kept to your account, once. An
/// account that already holds them keeps its own and changes nothing.
#[utoipa::path(
    post,
    tag = "users",
    path = "/api/v3/me/appearance/import",
    request_body = AppearanceUpdate,
    responses(
        (status = 200, description = "The appearance as stored, and whether this call wrote it", body = AppearanceImported),
        (status = 400, description = "The CSS is refused (`CUSTOM_CSS_REFUSED`, its problems in `details`) or too long (`CUSTOM_CSS_TOO_LONG`)"),
        (status = 403, description = "Custom CSS is turned off on this server (`CUSTOM_CSS_TURNED_OFF`)"),
    )
)]
pub async fn import_my_appearance(
    State(setup): State<AppearanceSetup>,
    User(caller): User,
    ValidJson(body): ValidJson<AppearanceUpdate>,
) -> Result<Json<AppearanceImported>, ApiError> {
    service::import(&setup.store, &setup.section, caller.user_id(), body)
        .await
        .map(Json)
        .map_err(ApiError::from)
}

/// List your last saves of your custom CSS, newest first.
#[utoipa::path(
    get,
    tag = "users",
    path = "/api/v3/me/appearance/css-saves",
    responses((status = 200, description = "Own CSS saves, newest first", body = Vec<CssSave>))
)]
pub async fn list_my_css_saves(
    State(setup): State<AppearanceSetup>,
    User(caller): User,
) -> Result<Json<Vec<CssSave>>, ApiError> {
    service::saves(&setup.store, caller.user_id())
        .await
        .map(Json)
        .map_err(ApiError::from)
}

/// Read the site CSS and whether people may add their own.
#[utoipa::path(
    get,
    tag = "settings",
    path = "/api/v3/settings/appearance",
    responses((status = 200, description = "Site appearance", body = Appearance))
)]
pub async fn get_site_appearance(
    State(setup): State<AppearanceSetup>,
    _caller: Admin,
) -> Result<Json<Appearance>, ApiError> {
    setup
        .settings
        .get::<Appearance>()
        .map(Json)
        .map_err(ApiError::from)
}

/// Save the site CSS and whether people may add their own. The site CSS
/// passes the same check as a person's, with a longer limit.
#[utoipa::path(
    put,
    tag = "settings",
    path = "/api/v3/settings/appearance",
    request_body = Appearance,
    responses(
        (status = 200, description = "Saved site appearance", body = Appearance),
        (status = 400, description = "The CSS is refused (`CUSTOM_CSS_REFUSED`, its problems in `details`) or too long (`CUSTOM_CSS_TOO_LONG`)"),
    )
)]
pub async fn put_site_appearance(
    State(setup): State<AppearanceSetup>,
    _caller: Admin,
    ValidJson(body): ValidJson<Appearance>,
) -> Result<Json<Appearance>, ApiError> {
    service::check_sheet(&body.site_css, Limit::Site)?;
    setup
        .settings
        .save(body)
        .await
        .map(Json)
        .map_err(ApiError::from)
}
