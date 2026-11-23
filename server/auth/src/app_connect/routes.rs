//! The app connect routes: what is asked, allow, and the code trade.
//!
//! ```text
//! GET  /api/v3/auth/app-connect/request     (user)
//! POST /api/v3/auth/app-connect/authorize   (user, standard session)
//! POST /api/v3/auth/app-connect/token       (public: the code and verifier are the credential)
//! ```
//!
//! The token step is the OAuth 2.0 token endpoint: a form body, and the
//! answers of RFC 6749 section 5 rather than the native error envelope. Its
//! 5xx still go through the request boundary.

use axum::{
    Json,
    body::Bytes,
    extract::State,
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use utoipa::ToSchema;

use super::request::ConnectRequest;
use super::token::{AppConnectTokenError, AppConnectTokenRequest, AppConnectTokenResponse};
use super::{AppConnectError, AppConnectService, TokenStepError};
use crate::session::login::NO_STORE;
use crate::users::User;
use droppedneedle_kernel::error::ApiError;
use droppedneedle_kernel::extract::{ValidJson, ValidQuery};
use droppedneedle_kernel::runtime_config::sections::AppConnectCredentialKind;

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
    /// The kind of credential the user is being asked to approve.
    pub credential_kind: AppConnectCredentialKind,
    /// The label the new credential will carry.
    pub password_name: String,
    /// Allowing replaces an active credential for this user.
    pub replaces_existing: bool,
    /// The user must revoke another credential of this kind before connecting.
    pub at_limit: bool,
    /// Where "Cancel" sends the browser: the app's address with
    /// `error=access_denied`.
    pub cancel_redirect: String,
}

/// The request and the kind of credential displayed by the consent page.
#[derive(Debug, Deserialize, ToSchema)]
pub struct AppConnectAuthorization {
    /// The checked app request.
    #[serde(flatten)]
    pub request: ConnectRequest,
    /// The kind the consent page showed; it must still be the app's.
    pub confirmed_credential_kind: AppConnectCredentialKind,
}

/// Where "Allow" sends the browser.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
pub struct AppConnectRedirect {
    /// The app's address with the one-time `code` and the `state`.
    pub redirect: String,
}

fn page_error(error: AppConnectError) -> ApiError {
    match error {
        AppConnectError::Off => ApiError::not_found(),
        AppConnectError::UnknownClient => {
            ApiError::invalid_input("This app is not allowed to sign in to this server.")
        }
        AppConnectError::Invalid { reason, redirect } => {
            ApiError::invalid_input(reason).with_details(json!({ "redirect": redirect }))
        }
        AppConnectError::AtLimit(kind) => {
            let credential = match kind {
                AppConnectCredentialKind::AppPassword => "App-password",
                AppConnectCredentialKind::DeviceSession => "Connected device session",
            };
            ApiError::conflict(format!(
                "{credential} limit reached. Revoke one before connecting another app."
            ))
        }
        AppConnectError::ConsentChanged => {
            ApiError::conflict("The app's access changed. Review the sign-in request again.")
        }
        AppConnectError::SettingsUnreadable => {
            ApiError::internal("the app_connect settings section does not read")
        }
        AppConnectError::Users(error) => ApiError::from(error),
    }
}

/// Keep the account-specific consent view, one-time codes and credentials out of caches.
fn no_store(status: StatusCode, body: impl Serialize) -> Response {
    let mut response = (status, Json(body)).into_response();
    let headers = response.headers_mut();
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static(NO_STORE));
    headers.insert(header::PRAGMA, HeaderValue::from_static("no-cache"));
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
    State(service): State<AppConnectService>,
    User(caller): User,
    ValidQuery(request): ValidQuery<ConnectRequest>,
) -> Result<Response, ApiError> {
    let view = service
        .describe(&caller, &request)
        .await
        .map_err(page_error)?;
    Ok(no_store(StatusCode::OK, view))
}

/// Allow an app connect request: issue its one-time code and answer the
/// address that hands it to the app. Needs a standard session, as creating
/// a credential by hand does.
#[utoipa::path(
    post,
    tag = "auth",
    path = "/api/v3/auth/app-connect/authorize",
    request_body = AppConnectAuthorization,
    responses(
        (status = 200, description = "Where to send the browser; never stored", body = AppConnectRedirect),
        (status = 400, description = "The app or its address is not listed, or the request is malformed"),
        (status = 403, description = "A device session cannot allow this"),
        (status = 404, description = "App connect is off: no app is listed"),
        (status = 409, description = "The account has reached the credential limit or the registered access changed")
    )
)]
pub async fn app_connect_authorize_handler(
    State(service): State<AppConnectService>,
    User(caller): User,
    ValidJson(body): ValidJson<AppConnectAuthorization>,
) -> Result<Response, ApiError> {
    let redirect = service
        .allow(&caller, &body.request, body.confirmed_credential_kind)
        .await
        .map_err(page_error)?;
    Ok(no_store(StatusCode::OK, redirect))
}

/// The OAuth 2.0 token endpoint: trade an app connect code and its PKCE
/// verifier for the credential the user approved. The code is
/// gone after this call, whatever it answers.
#[utoipa::path(
    post,
    tag = "auth",
    path = "/api/v3/auth/app-connect/token",
    request_body(
        content = AppConnectTokenRequest,
        content_type = "application/x-www-form-urlencoded"
    ),
    responses(
        (status = 200, description = "The new credential, shown once", body = AppConnectTokenResponse),
        (status = 400, description = "The trade is refused, as RFC 6749 section 5.2 names it", body = AppConnectTokenError),
        (status = 429, description = "Too many trades from this address; see `Retry-After`")
    )
)]
pub async fn app_connect_token_handler(
    State(service): State<AppConnectService>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, ApiError> {
    let answer = match AppConnectTokenRequest::from_form(headers.get(header::CONTENT_TYPE), &body) {
        Ok(trade) => service.exchange(trade).await,
        Err(error) => Err(TokenStepError::Refused(error)),
    };
    match answer {
        Ok(minted) => Ok(no_store(StatusCode::OK, minted)),
        Err(TokenStepError::Refused(error)) => Ok(no_store(
            StatusCode::BAD_REQUEST,
            AppConnectTokenError { error },
        )),
        Err(TokenStepError::SettingsUnreadable) => Err(ApiError::internal(
            "the app_connect settings section does not read",
        )),
        Err(TokenStepError::Users(error)) => Err(ApiError::from(error)),
    }
}
