//! Library principal plus role-gating extractors.
//!
//! The wiring mounts these routes inside the session gate with a
//! principal-translation layer that resolves the role fresh from the
//! user store. `RequireAdmin` and `RequireCurator` gate in the
//! extractor (before body parsing) so role denials 403 even on
//! malformed bodies.

use axum::extract::FromRequestParts;
use axum::http::request::Parts;

use super::error::LibraryError;

/// Account role. Meanings match the sibling slices.
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

impl<S: Send + Sync> FromRequestParts<S> for Principal {
    type Rejection = LibraryError;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        parts
            .extensions
            .get::<Principal>()
            .cloned()
            .ok_or(LibraryError::Unauthorized {
                message: "Authentication required".to_owned(),
            })
    }
}

/// Extractor that admits admins and 403s everyone else.
pub struct RequireAdmin(pub Principal);

impl<S: Send + Sync> FromRequestParts<S> for RequireAdmin {
    type Rejection = LibraryError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        let caller = Principal::from_request_parts(parts, state).await?;
        if caller.role.is_admin() {
            Ok(Self(caller))
        } else {
            Err(LibraryError::Forbidden {
                message: "Admin role required".to_owned(),
            })
        }
    }
}

/// Extractor that admits curators (admin or trusted) and 403s the rest.
pub struct RequireCurator(pub Principal);

impl<S: Send + Sync> FromRequestParts<S> for RequireCurator {
    type Rejection = LibraryError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        let caller = Principal::from_request_parts(parts, state).await?;
        if caller.role.is_curator() {
            Ok(Self(caller))
        } else {
            Err(LibraryError::Forbidden {
                message: "Curator role required".to_owned(),
            })
        }
    }
}
