//! Roles and the request principal.
//!
//! The session middleware authenticates every `/api/v3/*` request
//! and stashes [`CurrentSession`] in the request extensions. The extractors
//! here read it, resolve the account role fresh from the user store (so
//! role changes take effect on the next request), and add role gating.
//! They never verify tokens themselves.
//!
//! [`CurrentSession`]: super::super::session::middleware::CurrentSession

use axum::extract::FromRequestParts;
use axum::http::request::Parts;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use super::super::session::middleware::CurrentSession;
use super::UsersDeps;
use super::error::UsersError;
use super::handlers::UsersHttpError;
use super::stores::StoreError;

/// Session kind, re-exported from the session module so services and
/// tests share one definition.
pub use super::super::session::store::SessionKind;

/// Account role. Meanings are unchanged from v2: `user` requests await
/// approval; `trusted` and `admin` auto-approve and are quota-exempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    /// Regular account.
    User,
    /// Approved account with moderation-adjacent rights.
    Trusted,
    /// Full administration.
    Admin,
}

impl Role {
    /// Parse a stored or submitted role string. Unknown strings fail closed
    /// to `None` at API boundaries; see [`Role::from_stored`] for rows.
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "user" => Some(Self::User),
            "trusted" => Some(Self::Trusted),
            "admin" => Some(Self::Admin),
            _ => None,
        }
    }

    /// Storage form. Matches the federated module's `ROLE_USER`/`ROLE_ADMIN`
    /// strings and the `auth_users.role` column values.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Trusted => "trusted",
            Self::Admin => "admin",
        }
    }

    /// Interpret a role string read from storage. Unknown values degrade to
    /// the least-privilege role rather than failing the request.
    pub fn from_stored(value: &str) -> Self {
        Self::parse(value).unwrap_or(Self::User)
    }

    /// Whether this role passes admin gating.
    pub fn is_admin(self) -> bool {
        matches!(self, Self::Admin)
    }

    /// Whether this role passes curator gating (admin or trusted).
    pub fn is_curator(self) -> bool {
        matches!(self, Self::Admin | Self::Trusted)
    }
}

/// The authenticated principal for one request, built by the extractors
/// from the session middleware's [`CurrentSession`] plus a fresh user-row lookup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthContext {
    /// Owning user id.
    pub user_id: String,
    /// Login username, when the account has one.
    pub username: Option<String>,
    /// Account role, resolved fresh for this request.
    pub role: Role,
    /// Authenticating session id (for the `current` marker in listings).
    pub session_id: String,
    /// Kind of the authenticating session.
    pub session_kind: SessionKind,
    /// True when the session came from the cookie rather than Bearer.
    pub via_cookie: bool,
}

impl AuthContext {
    /// Reject companion tokens from minting further tokens.
    pub fn require_standard_session(&self) -> Result<(), UsersError> {
        match self.session_kind {
            SessionKind::Standard => Ok(()),
            SessionKind::Companion => Err(UsersError::Forbidden {
                message: "Companion tokens cannot mint further tokens".to_owned(),
            }),
        }
    }
}

/// Any authenticated user. Missing session or stale account is 401.
#[derive(Debug, Clone)]
pub struct CurrentUser(pub AuthContext);

/// A curator (admin or trusted). Non-curator is 403.
#[derive(Debug, Clone)]
pub struct CurrentCurator(pub AuthContext);

/// An admin. Non-admin is 403.
#[derive(Debug, Clone)]
pub struct CurrentAdmin(pub AuthContext);

/// No session stashed: the middleware (or test layer) never authenticated.
fn missing_session() -> UsersError {
    UsersError::Unauthorized {
        message: "Authentication required".to_owned(),
    }
}

/// Resolve the principal: the stashed session plus a fresh user-row lookup.
/// A session whose account is gone is stale, hence 401.
async fn resolve(parts: &Parts, deps: &UsersDeps) -> Result<AuthContext, UsersError> {
    let session = parts
        .extensions
        .get::<CurrentSession>()
        .ok_or_else(missing_session)?;
    let user = deps
        .users
        .get_by_id(&session.user_id)
        .await
        .map_err(|error| match error {
            StoreError::Conflict => UsersError::Conflict {
                message: "Conflicting state".to_owned(),
            },
            StoreError::Internal(cause) => UsersError::internal(&cause, deps.ids.as_ref()),
        })?
        .ok_or_else(missing_session)?;
    Ok(AuthContext {
        user_id: user.id,
        username: user.username,
        role: user.role,
        session_id: session.session_id.clone(),
        session_kind: session.kind,
        via_cookie: session.transport == super::super::session::extract::Transport::Cookie,
    })
}

impl FromRequestParts<UsersDeps> for CurrentUser {
    type Rejection = UsersHttpError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &UsersDeps,
    ) -> Result<Self, Self::Rejection> {
        Ok(CurrentUser(resolve(parts, state).await?))
    }
}

impl FromRequestParts<UsersDeps> for CurrentCurator {
    type Rejection = UsersHttpError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &UsersDeps,
    ) -> Result<Self, Self::Rejection> {
        let ctx = resolve(parts, state).await?;
        if ctx.role.is_curator() {
            Ok(CurrentCurator(ctx))
        } else {
            Err(UsersError::Forbidden {
                message: "Curator role required".to_owned(),
            }
            .into())
        }
    }
}

impl FromRequestParts<UsersDeps> for CurrentAdmin {
    type Rejection = UsersHttpError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &UsersDeps,
    ) -> Result<Self, Self::Rejection> {
        let ctx = resolve(parts, state).await?;
        if ctx.role.is_admin() {
            Ok(CurrentAdmin(ctx))
        } else {
            Err(UsersError::Forbidden {
                message: "Admin role required".to_owned(),
            }
            .into())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stored_roles_degrade_to_least_privilege() {
        assert_eq!(Role::from_stored("owner"), Role::User);
    }

    #[test]
    fn companion_sessions_cannot_mint() {
        let companion = AuthContext {
            user_id: "u".to_owned(),
            username: None,
            role: Role::Admin,
            session_id: "s".to_owned(),
            session_kind: SessionKind::Companion,
            via_cookie: false,
        };
        assert!(companion.require_standard_session().is_err());
        let standard = AuthContext {
            session_kind: SessionKind::Standard,
            ..companion
        };
        assert!(standard.require_standard_session().is_ok());
    }
}
