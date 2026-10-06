//! Request and response bodies for the auth HTTP routes.
//!
//! Clean-slate `/api/v3` shapes (snake_case). The session-issuing responses
//! share one rule: cookie mode sets the session cookie and the
//! body carries no token; Bearer mode returns the raw token once in the body and
//! sets no cookie. Every token-mint response carries `no-store`.

use crate::auth::federated::users::StoredUser;
use crate::auth::session::login::TransportParam;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// Credential handoff for session-issuing routes. Mirrors the session login
/// `TransportParam` (which owns the mechanism) so this layer stays the only
/// place that needs `ToSchema`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, Serialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum TransportDto {
    /// httpOnly cookie session; body carries no token.
    #[default]
    Cookie,
    /// Raw token returned once in the body; no cookie set.
    Bearer,
}

impl TransportDto {
    /// The session login mechanism value.
    pub fn as_param(self) -> TransportParam {
        match self {
            Self::Cookie => TransportParam::Cookie,
            Self::Bearer => TransportParam::Bearer,
        }
    }
}

/// Local login body.
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct LoginBody {
    /// Username, matched case-insensitively.
    pub username: String,
    /// Account password.
    pub password: String,
    /// Credential handoff; defaults to cookie.
    #[serde(default)]
    pub transport: TransportDto,
}

/// First-admin setup body. Runs only against an empty user table.
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct SetupBody {
    /// Login username, 3-32 chars.
    pub username: String,
    /// Initial password, at least 12 chars.
    pub password: String,
    /// Display name; defaults to the username casing.
    pub display_name: Option<String>,
    /// Optional email.
    pub email: Option<String>,
    /// Credential handoff; defaults to cookie.
    #[serde(default)]
    pub transport: TransportDto,
}

/// Setup-required probe for the SPA first-run gate.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct SetupStatusBody {
    /// True when no users exist yet and setup must run.
    pub setup_required: bool,
}

/// Authenticated user, federated view. Stored provider rows never surface;
/// only the profile fields the SPA renders.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
pub struct FederatedUserView {
    /// User id.
    pub id: String,
    /// Display name from the provider.
    pub display_name: String,
    /// `user`, `trusted`, or `admin`.
    pub role: String,
    /// Lowercased email, when the provider supplies one.
    pub email: Option<String>,
    /// Avatar URL from the provider.
    pub avatar_url: Option<String>,
    /// Lowercased login identifier.
    pub username: String,
    /// Preferred casing for display.
    pub username_display: String,
}

impl From<&StoredUser> for FederatedUserView {
    fn from(user: &StoredUser) -> Self {
        Self {
            id: user.id.clone(),
            display_name: user.display_name.clone(),
            role: user.role.clone(),
            email: user.email.clone(),
            avatar_url: user.avatar_url.clone(),
            username: user.username.clone(),
            username_display: user.username_display.clone(),
        }
    }
}

/// Jellyfin login body.
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct JellyfinLoginBody {
    /// Jellyfin username.
    pub username: String,
    /// Jellyfin password.
    pub password: String,
    /// Credential handoff; defaults to cookie.
    #[serde(default)]
    pub transport: TransportDto,
}

/// OIDC callback query (IdP redirect target).
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct OidcCallbackQuery {
    /// Authorization code from the IdP.
    pub code: String,
    /// State echoed from the authorize step.
    pub state: String,
}

/// OIDC authorize answer: the browser URL for this login.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct OidcAuthorizeBody {
    /// IdP authorize URL (PKCE + state baked in).
    pub authorize_url: String,
}

/// OIDC exchange body: swap the one-time callback code for a session.
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct OidcExchangeBody {
    /// Single-use code from the callback redirect.
    pub code: String,
    /// Credential handoff; defaults to cookie.
    #[serde(default)]
    pub transport: TransportDto,
}

/// Plex journey start answer.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct PlexStartBody {
    /// PIN id for the poll steps.
    pub pin_id: i64,
    /// `app.plex.tv` URL the user authorizes at.
    pub authorize_url: String,
}

/// One PIN to poll.
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct PlexPinBody {
    /// PIN id from the start step.
    pub pin_id: i64,
}

/// Login-completion poll body.
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct PlexLoginPollBody {
    /// PIN id from the start step.
    pub pin_id: i64,
    /// Credential handoff; defaults to cookie.
    #[serde(default)]
    pub transport: TransportDto,
}

/// Login-completion poll answer. Pending carries only the flag; a completed
/// poll carries the user (plus the token in Bearer mode, or the session cookie
/// in cookie mode, like every login).
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct PlexLoginPollResult {
    /// True once the user authorized the PIN.
    pub completed: bool,
    /// Present only when completed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user: Option<FederatedUserView>,
    /// Present only when completed in Bearer mode.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub token: Option<String>,
}

/// Link-completion poll answer. The link itself is stored server-side; the
/// account tokens never reach the browser.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct PlexLinkPollResult {
    /// True once the user authorized the PIN and the link is stored.
    pub completed: bool,
    /// The linked Plex user name; present only when completed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub username: Option<String>,
}

/// Sign-in methods the login page offers (v2 `AuthProvidersResponse`).
#[derive(Debug, Clone, Copy, Serialize, ToSchema)]
pub struct AuthProvidersBody {
    /// Username and password; always true.
    pub local: bool,
    /// Plex sign-in is switched on.
    pub plex: bool,
    /// Jellyfin sign-in is switched on and the server is set up.
    pub jellyfin: bool,
    /// OIDC sign-in is switched on with an issuer and client id.
    pub oidc: bool,
}

/// Settings-completion poll answer: the raw auth token, untouched.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct PlexConnectPollResult {
    /// True once the user authorized the PIN.
    pub completed: bool,
    /// Present only when completed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auth_token: Option<String>,
}
