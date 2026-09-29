//! Slice-local principal and auth gate.
//!
//! The app wiring replaces this seam: `ReadsSetup` mounts the routes inside
//! the session gate with a principal-translation layer that resolves the
//! role fresh from the user store. The gate below reads a
//! `x-slice-principal` header of the form `<user-id>:<role>[:<username>]`
//! so the standalone briefs can still drive the auth matrix.
//! Response shapes mirror the crate session slice: 401 carries
//! `WWW-Authenticate: Bearer`, role denials are 403.

use axum::{
    extract::{FromRequestParts, Request},
    http::request::Parts,
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

    /// Storage/wire form.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Trusted => "trusted",
            Self::Admin => "admin",
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

/// Header carrying the slice-local credential. Wiring deletes this gate.
pub const PRINCIPAL_HEADER: &str = "x-slice-principal";

/// Parse one header value into a principal. Anything malformed fails closed.
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

/// Slice-local auth gate. Missing or malformed credentials are 401; the
/// principal lands in the request extensions for the extractor.
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_parses_user_role_and_optional_name() {
        let full = parse_header("u-1:admin:Ada").expect("valid header parses");
        assert_eq!(full.user_id, "u-1");
        assert_eq!(full.role, Role::Admin);
        assert_eq!(full.username.as_deref(), Some("Ada"));
        let bare = parse_header("u-2:user").expect("name is optional");
        assert_eq!(bare.username, None);
    }

    #[test]
    fn malformed_headers_fail_closed() {
        for bad in ["", ":", "u-1", "u-1:owner", ":admin"] {
            assert!(parse_header(bad).is_none(), "{bad:?} must fail closed");
        }
        assert!(
            parse_header("u-1:admin:extra:bits").is_some(),
            "names may hold colons"
        );
    }

    #[test]
    fn role_parse_round_trip() {
        for role in [Role::User, Role::Trusted, Role::Admin] {
            assert_eq!(Role::parse(role.as_str()), Some(role));
        }
        assert_eq!(Role::parse("owner"), None);
    }

    #[test]
    fn curator_is_trusted_or_admin() {
        assert!(!Role::User.is_curator());
        assert!(Role::Trusted.is_curator());
        assert!(Role::Admin.is_curator());
        assert!(Role::Admin.is_admin());
        assert!(!Role::Trusted.is_admin());
    }
}
