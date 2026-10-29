//! The app connect routes: what is asked, allow, and the code trade.
//!
//! ```text
//! GET  /api/v3/auth/app-connect/request     (user)
//! POST /api/v3/auth/app-connect/authorize   (user, standard session)
//! POST /api/v3/auth/app-connect/token       (public: the code and verifier are the credential)
//! ```
//!
//! The token step answers in the OAuth 2.0 shape (RFC 6749 section 5) that
//! native apps parse, not the native error envelope: 200 with the app
//! password, or 400 `{"error": "invalid_grant" | "invalid_request"}`.
//! Every refusal of a well-formed trade is the same `invalid_grant`, so the
//! answer tells nothing about which check failed.

use axum::{
    Json,
    body::Bytes,
    extract::State,
    http::{HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use utoipa::ToSchema;

use super::AppConnectState;
use super::codes::Grant;
use super::request::{ConnectRequest, Rejection, check, verifier_matches};
use crate::session::login::NO_STORE;
use crate::session::tokens::secret_eq;
use crate::users::User;
use crate::users::UsersError;
use crate::users::services::{self, MAX_ACTIVE_APP_PASSWORDS};
use droppedneedle_kernel::error::ApiError;
use droppedneedle_kernel::extract::{ValidJson, ValidQuery};

/// What the confirmation page shows.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
pub struct AppConnectRequestView {
    /// The app's id.
    pub client_id: String,
    /// The app's name from the settings.
    pub client_name: String,
    /// The device name the app reported, cleaned up; absent when it sent
    /// none.
    pub device_name: Option<String>,
    /// The name the new app password will carry.
    pub password_name: String,
    /// The user already has a live app password of that name, which
    /// allowing replaces.
    pub replaces_existing: bool,
    /// The user holds the most app passwords allowed under other names, so
    /// allowing would be refused until one is revoked.
    pub at_limit: bool,
    /// Where "Cancel" sends the browser: the app's address with
    /// `error=access_denied`.
    pub cancel_redirect: String,
}

/// Where "Allow" sends the browser.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
pub struct AppConnectRedirect {
    /// The app's address with the one-time `code` and the `state`.
    pub redirect: String,
}

/// The app's code trade.
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct AppConnectTokenRequest {
    /// The app's id, as in the request.
    pub client_id: String,
    /// The return address, as in the request.
    pub redirect_uri: String,
    /// The one-time code the browser handed back.
    pub code: String,
    /// The PKCE verifier the challenge was made from.
    pub code_verifier: String,
}

/// The app password the trade mints.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
pub struct AppConnectTokenResponse {
    /// The app password, shown this once. Subsonic takes it as `apiKey`.
    pub app_password: String,
    /// Its id, as listed under the user's app passwords.
    pub id: String,
    /// Its name, as listed.
    pub name: String,
    /// The signed-in user's login name.
    pub username: String,
}

/// A refused trade.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
pub struct AppConnectTokenError {
    /// `invalid_grant` (the code is unknown, used or expired, or does not
    /// fit the app, address or verifier) or `invalid_request` (the body is
    /// not a trade).
    pub error: String,
}

fn rejection_error(rejection: Rejection) -> ApiError {
    match rejection {
        Rejection::Disabled => ApiError::not_found(),
        Rejection::UnknownClient => {
            ApiError::invalid_input("This app is not allowed to sign in to this server.")
        }
        Rejection::Invalid { reason, redirect } => {
            ApiError::invalid_input(reason).with_details(json!({ "redirect": redirect }))
        }
    }
}

fn no_store(mut response: Response) -> Response {
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static(NO_STORE));
    response
}

/// What an app connect request asks for, for the confirmation page.
#[utoipa::path(
    get,
    tag = "auth",
    path = "/api/v3/auth/app-connect/request",
    params(ConnectRequest),
    responses(
        (status = 200, description = "The request, checked", body = AppConnectRequestView),
        (status = 400, description = "The app or its address is not listed, or the request is malformed (then `details.redirect` hands the error back to the app)"),
        (status = 404, description = "App connect is off: no app is listed")
    )
)]
pub async fn app_connect_request_handler(
    State(state): State<AppConnectState>,
    User(caller): User,
    ValidQuery(request): ValidQuery<ConnectRequest>,
) -> Result<Json<AppConnectRequestView>, ApiError> {
    let checked = check(&state.apps(), &request).map_err(rejection_error)?;
    let own = services::list_app_passwords(&state.users, caller.user_id())
        .await
        .map_err(ApiError::from)?
        .app_passwords;
    let replaces_existing = own.iter().any(|row| row.name == checked.password_name);
    let others = own
        .iter()
        .filter(|row| row.name != checked.password_name)
        .count();
    Ok(Json(AppConnectRequestView {
        client_id: checked.client.client_id.clone(),
        client_name: checked.client.name.clone(),
        device_name: checked.device_name.clone(),
        password_name: checked.password_name.clone(),
        replaces_existing,
        at_limit: u64::try_from(others).unwrap_or(u64::MAX) >= MAX_ACTIVE_APP_PASSWORDS,
        cancel_redirect: checked.cancel_redirect(),
    }))
}

/// Allow an app connect request: issue its one-time code and answer the
/// address that hands it to the app. Needs a standard session, as creating
/// an app password by hand does.
#[utoipa::path(
    post,
    tag = "auth",
    path = "/api/v3/auth/app-connect/authorize",
    request_body = ConnectRequest,
    responses(
        (status = 200, description = "Where to send the browser; never stored", body = AppConnectRedirect),
        (status = 400, description = "The app or its address is not listed, or the request is malformed"),
        (status = 403, description = "A device session cannot allow this"),
        (status = 404, description = "App connect is off: no app is listed"),
        (status = 409, description = "The user holds the most app passwords allowed")
    )
)]
pub async fn app_connect_authorize_handler(
    State(state): State<AppConnectState>,
    User(caller): User,
    ValidJson(request): ValidJson<ConnectRequest>,
) -> Result<Response, ApiError> {
    caller.require_standard_session().map_err(ApiError::from)?;
    let checked = check(&state.apps(), &request).map_err(rejection_error)?;
    let others = services::list_app_passwords(&state.users, caller.user_id())
        .await
        .map_err(ApiError::from)?
        .app_passwords
        .iter()
        .filter(|row| row.name != checked.password_name)
        .count();
    if u64::try_from(others).unwrap_or(u64::MAX) >= MAX_ACTIVE_APP_PASSWORDS {
        return Err(ApiError::conflict(format!(
            "App-password limit reached ({MAX_ACTIVE_APP_PASSWORDS}). Revoke one before connecting another app."
        )));
    }
    let code = state
        .codes
        .issue(
            Grant {
                user_id: caller.user_id().to_owned(),
                client_id: checked.client.client_id.clone(),
                redirect_uri: checked.redirect_uri.clone(),
                code_challenge: checked.code_challenge.clone(),
                password_name: checked.password_name.clone(),
            },
            state.users.clock.now_unix(),
        )
        .map_err(|cause| ApiError::internal(&cause))?;
    tracing::info!(
        user_id = caller.user_id(),
        client_id = %checked.client.client_id,
        "app connect allowed"
    );
    Ok(no_store(
        Json(AppConnectRedirect {
            redirect: checked.code_redirect(&code),
        })
        .into_response(),
    ))
}

fn token_error(error: &str) -> Response {
    no_store(
        (
            StatusCode::BAD_REQUEST,
            Json(AppConnectTokenError {
                error: error.to_owned(),
            }),
        )
            .into_response(),
    )
}

/// Trade an app connect code and its PKCE verifier for an app password of
/// the user who allowed it. The code is gone after this call, whatever it
/// answers.
#[utoipa::path(
    post,
    tag = "auth",
    path = "/api/v3/auth/app-connect/token",
    request_body = AppConnectTokenRequest,
    responses(
        (status = 200, description = "The new app password, shown once", body = AppConnectTokenResponse),
        (status = 400, description = "`invalid_grant` or `invalid_request`", body = AppConnectTokenError),
        (status = 404, description = "App connect is off: no app is listed"),
        (status = 429, description = "Too many trades from this address; see `Retry-After`")
    )
)]
pub async fn app_connect_token_handler(
    State(state): State<AppConnectState>,
    body: Bytes,
) -> Result<Response, ApiError> {
    let apps = state.apps();
    if !apps.enabled() {
        return Err(ApiError::not_found());
    }
    let Ok(trade) = serde_json::from_slice::<AppConnectTokenRequest>(&body) else {
        return Ok(token_error("invalid_request"));
    };
    let Some(grant) = state.codes.take(&trade.code, state.users.clock.now_unix()) else {
        tracing::debug!("app connect trade refused: unknown, used or expired code");
        return Ok(token_error("invalid_grant"));
    };
    let fits = secret_eq(&grant.client_id, &trade.client_id)
        & secret_eq(&grant.redirect_uri, &trade.redirect_uri)
        & verifier_matches(&trade.code_verifier, &grant.code_challenge);
    // The admin may have taken the app off the list since the user allowed it.
    let listed = apps.client(&grant.client_id, &grant.redirect_uri).is_some();
    if !(fits && listed) {
        tracing::debug!(
            client_id = %grant.client_id,
            listed,
            "app connect trade refused: app, address or verifier does not fit"
        );
        return Ok(token_error("invalid_grant"));
    }
    let user = state
        .users
        .users
        .get_by_id(&grant.user_id)
        .await
        .map_err(|error| ApiError::from(services::store_internal(error)))?;
    let Some(user) = user else {
        tracing::debug!("app connect trade refused: the user is gone");
        return Ok(token_error("invalid_grant"));
    };
    let minted =
        match services::replace_app_password(&state.users, &user.id, &grant.password_name).await {
            Ok(minted) => minted,
            Err(UsersError::Conflict { .. }) => {
                tracing::debug!("app connect trade refused: app-password limit reached");
                return Ok(token_error("invalid_grant"));
            }
            Err(error) => return Err(ApiError::from(error)),
        };
    let username = user
        .username_display
        .or(user.username)
        .unwrap_or_else(|| user.id.clone());
    Ok(no_store(
        Json(AppConnectTokenResponse {
            app_password: minted.secret,
            id: minted.id,
            name: minted.name,
            username,
        })
        .into_response(),
    ))
}
