//! The token step's wire shapes: the OAuth 2.0 access token request and its
//! answers (RFC 6749 sections 4.1.3, 5.1 and 5.2, RFC 7636 section 4.5).
//!
//! This is the one native route that speaks a protocol of its own: app
//! libraries send a form body and read the RFC's error shape, so neither
//! goes through `ValidJson` or the native error envelope.

use axum::http::HeaderValue;
use droppedneedle_kernel::runtime_config::sections::AppConnectCredentialKind;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// The one grant type the token step trades.
const GRANT_TYPE: &str = "authorization_code";
/// The one body type it reads.
const FORM: &str = "application/x-www-form-urlencoded";

/// The app's code trade, sent form-encoded.
#[derive(Clone, PartialEq, Eq, Deserialize, ToSchema)]
pub struct AppConnectTokenRequest {
    /// Must be `authorization_code`.
    pub grant_type: String,
    /// The one-time code the browser handed back.
    pub code: String,
    /// The return address, exactly as in the authorization request.
    pub redirect_uri: String,
    /// The app's id, as in the authorization request.
    pub client_id: String,
    /// The PKCE verifier the challenge was made from.
    pub code_verifier: String,
}

// The code and verifier are a credential: never printed.
impl std::fmt::Debug for AppConnectTokenRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AppConnectTokenRequest")
            .field("grant_type", &self.grant_type)
            .field("redirect_uri", &self.redirect_uri)
            .field("client_id", &self.client_id)
            .finish_non_exhaustive()
    }
}

/// Why a trade was refused, as RFC 6749 section 5.2 names it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum AppConnectTokenErrorCode {
    /// The body is not a form, or a parameter is missing or repeated.
    InvalidRequest,
    /// No app with this `client_id` is listed.
    InvalidClient,
    /// The code is unknown, used or expired, or does not fit the app,
    /// address or verifier. Every such refusal is this one answer.
    InvalidGrant,
    /// The grant type is not `authorization_code`.
    UnsupportedGrantType,
}

/// A refused trade (400).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct AppConnectTokenError {
    /// What was wrong.
    pub error: AppConnectTokenErrorCode,
}

/// The token type returned by the OAuth token endpoint.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub enum AppConnectTokenType {
    /// The only kind.
    Bearer,
}

/// The credential a trade mints.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct AppConnectTokenResponse {
    /// The credential, shown once. A device session authenticates native API requests with Bearer.
    pub access_token: String,
    /// What the credential is: an app password or a native device session.
    pub credential_kind: AppConnectCredentialKind,
    /// Lifetime in seconds for a native device session.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires_in: Option<i64>,
    /// Always `Bearer`.
    pub token_type: AppConnectTokenType,
    /// The login name of the user who allowed it.
    pub username: String,
}

impl std::fmt::Debug for AppConnectTokenResponse {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AppConnectTokenResponse")
            .field("token_type", &self.token_type)
            .field("credential_kind", &self.credential_kind)
            .field("expires_in", &self.expires_in)
            .finish_non_exhaustive()
    }
}

impl AppConnectTokenRequest {
    /// Read a trade from its body. Only a form body is read; a parameter
    /// sent twice is refused and one sent empty counts as missing (RFC 6749
    /// section 3.2). Parameters the step does not use are ignored.
    pub fn from_form(
        content_type: Option<&HeaderValue>,
        body: &[u8],
    ) -> Result<Self, AppConnectTokenErrorCode> {
        use AppConnectTokenErrorCode::{InvalidRequest, UnsupportedGrantType};

        let is_form = content_type
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.split(';').next())
            .is_some_and(|mime| mime.trim().eq_ignore_ascii_case(FORM));
        if !is_form {
            return Err(InvalidRequest);
        }
        let mut grant_type = None;
        let mut code = None;
        let mut redirect_uri = None;
        let mut client_id = None;
        let mut code_verifier = None;
        for (name, value) in url::form_urlencoded::parse(body) {
            let slot = match name.as_ref() {
                "grant_type" => &mut grant_type,
                "code" => &mut code,
                "redirect_uri" => &mut redirect_uri,
                "client_id" => &mut client_id,
                "code_verifier" => &mut code_verifier,
                _ => continue,
            };
            if slot.is_some() {
                return Err(InvalidRequest);
            }
            *slot = Some(value.into_owned());
        }
        let given = |value: Option<String>| value.filter(|value| !value.is_empty());
        let (grant_type, code, redirect_uri, client_id, code_verifier) = (
            given(grant_type),
            given(code),
            given(redirect_uri),
            given(client_id),
            given(code_verifier),
        );
        match grant_type.as_deref() {
            None => return Err(InvalidRequest),
            Some(GRANT_TYPE) => {}
            Some(_) => return Err(UnsupportedGrantType),
        }
        let (Some(code), Some(redirect_uri), Some(client_id), Some(code_verifier)) =
            (code, redirect_uri, client_id, code_verifier)
        else {
            return Err(InvalidRequest);
        };
        Ok(Self {
            grant_type: GRANT_TYPE.to_owned(),
            code,
            redirect_uri,
            client_id,
            code_verifier,
        })
    }
}
