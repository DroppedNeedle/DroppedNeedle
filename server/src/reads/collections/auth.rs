//! Collections principal and role extractor.
//!
//! `ReadsSetup` mounts the routes inside the session gate with a
//! principal-translation layer that resolves the role fresh from the user
//! store. Tests that mount the routes alone use the header gate below
//! (`x-slice-principal: <user-id>:<role>[:<username>]`), compiled only with
//! the `test-support` feature. Response shapes match the session layer:
//! 401 carries `WWW-Authenticate: Bearer`, role denials are 403.

use axum::{extract::FromRequestParts, http::request::Parts};
#[cfg(any(test, feature = "test-support"))]
use axum::{
    extract::Request,
    middleware::Next,
    response::{IntoResponse, Response},
};

use super::error::CollectionsError;

/// Account role. Meanings match v2: `user` requests await approval;
/// `trusted` and `admin` auto-approve and are quota-exempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    /// Regular account.
    User,
    /// Approved account with moderation-adjacent rights.
    Trusted,
    /// Full administration.
    Admin,
}

impl Role {
    /// Parse a submitted role string. Unknown strings fail closed to `None`.
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "user" => Some(Self::User),
            "trusted" => Some(Self::Trusted),
            "admin" => Some(Self::Admin),
            _ => None,
        }
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

/// The authenticated principal for one request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Principal {
    /// Owning user id.
    pub user_id: String,
    /// Login username, when the account has one.
    pub username: Option<String>,
    /// Account role.
    pub role: Role,
}

impl Principal {
    /// Display name for ownership fields. Falls back to the user id.
    pub fn display_name(&self) -> String {
        self.username
            .clone()
            .unwrap_or_else(|| self.user_id.clone())
    }

    /// Reject non-admins with 403.
    pub fn require_admin(&self) -> Result<(), CollectionsError> {
        if self.role.is_admin() {
            Ok(())
        } else {
            Err(CollectionsError::Forbidden {
                message: "Admin role required".to_owned(),
            })
        }
    }

    /// Reject non-curators with 403.
    pub fn require_curator(&self) -> Result<(), CollectionsError> {
        if self.role.is_curator() {
            Ok(())
        } else {
            Err(CollectionsError::Forbidden {
                message: "Curator role required".to_owned(),
            })
        }
    }
}

impl<S: Send + Sync> FromRequestParts<S> for Principal {
    type Rejection = CollectionsError;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        parts
            .extensions
            .get::<Principal>()
            .cloned()
            .ok_or_else(|| CollectionsError::Unauthorized {
                message: "Authentication required".to_owned(),
            })
    }
}

/// Header carrying the test credential.
#[cfg(any(test, feature = "test-support"))]
pub const PRINCIPAL_HEADER: &str = "x-slice-principal";

/// Parse one header value into a principal. Anything malformed fails closed.
#[cfg(any(test, feature = "test-support"))]
fn parse_header(value: &str) -> Option<Principal> {
    let mut parts = value.splitn(3, ':');
    let user_id = parts.next().unwrap_or_default().trim();
    let role = parts.next().unwrap_or_default().trim();
    if user_id.is_empty() {
        return None;
    }
    let role = Role::parse(role)?;
    let username = parts
        .next()
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(str::to_owned);
    Some(Principal {
        user_id: user_id.to_owned(),
        username,
        role,
    })
}

/// Header auth gate for tests that mount the routes without the session
/// middleware. Missing or malformed credentials are 401; the principal
/// lands in the request extensions for the extractor.
#[cfg(any(test, feature = "test-support"))]
pub async fn gate(mut req: Request, next: Next) -> Response {
    let principal = req
        .headers()
        .get(PRINCIPAL_HEADER)
        .and_then(|value| value.to_str().ok())
        .and_then(parse_header);
    match principal {
        Some(principal) => {
            req.extensions_mut().insert(principal);
            next.run(req).await
        }
        None => CollectionsError::Unauthorized {
            message: "Authentication required".to_owned(),
        }
        .into_response(),
    }
}
