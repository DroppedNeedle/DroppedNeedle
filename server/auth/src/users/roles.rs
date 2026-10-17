//! Roles and the request principal.
//!
//! The session gate authenticates every `/api/v3/*` request, reads the
//! token and its user in one query, and puts one [`Principal`] in the
//! request extensions. The extractors here read only that: they make no
//! store call, work in any router whatever its state, and add role
//! gating. The role is read once per request, at the gate, so a role
//! change or an account deletion applies from the next request.

use std::ops::Deref;

use axum::extract::FromRequestParts;
use axum::http::request::Parts;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use super::super::session::extract::Transport;
use super::error::UsersError;
use droppedneedle_kernel::error::ApiError;

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

/// Where a principal came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Origin {
    /// A signed-in request, resolved by the session gate.
    Session,
    /// Work done for a user with no request ([`Principal::acting_for`]).
    ActingFor,
}

/// The caller of one request: the token's user, read with the token by the
/// session gate. Fields are private so no other crate can write or alter
/// one; the gate, [`Principal::acting_for`] and the test-support
/// constructor are the only ways to make one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Principal {
    user_id: String,
    username: Option<String>,
    username_display: Option<String>,
    role: Role,
    session_id: String,
    session_kind: SessionKind,
    transport: Transport,
    origin: Origin,
}

impl Principal {
    /// The principal of a request the session gate admitted. Only the gate
    /// calls this, after every one of its checks passed.
    pub(crate) fn from_session(
        user_id: String,
        username: Option<String>,
        username_display: Option<String>,
        role: Role,
        session_id: String,
        session_kind: SessionKind,
        transport: Transport,
    ) -> Self {
        Self {
            user_id,
            username,
            username_display,
            role,
            session_id,
            session_kind,
            transport,
            origin: Origin::Session,
        }
    }

    /// The principal for work done for a user with no request, such as the
    /// Weekly Mix standing grant or removing a discovery batch as its owner.
    ///
    /// Only the service that performs the work calls this, never a handler,
    /// and `user_id` and `username_display` come from the stored row the work
    /// belongs to, never from request data. The result is never put in a
    /// request. It has no session: it cannot create credentials, and
    /// `User`, `Curator` and `Admin` refuse it whatever its role.
    pub fn acting_for(
        user_id: impl Into<String>,
        role: Role,
        username_display: Option<String>,
    ) -> Self {
        Self {
            user_id: user_id.into(),
            username: None,
            username_display,
            role,
            session_id: String::new(),
            session_kind: SessionKind::Companion,
            transport: Transport::Bearer,
            origin: Origin::ActingFor,
        }
    }

    /// A signed-in principal for the test sign-in layer, which stands in for
    /// the session gate on a slice router.
    #[cfg(any(test, feature = "test-support"))]
    pub fn for_tests(
        user_id: &str,
        role: Role,
        session_id: &str,
        session_kind: SessionKind,
    ) -> Self {
        Self::from_session(
            user_id.to_owned(),
            None,
            None,
            role,
            session_id.to_owned(),
            session_kind,
            Transport::Bearer,
        )
    }

    /// The same test principal shown under `username_display`.
    #[cfg(any(test, feature = "test-support"))]
    pub fn with_username_display(mut self, username_display: &str) -> Self {
        self.username_display = Some(username_display.to_owned());
        self
    }

    /// Owning user id.
    pub fn user_id(&self) -> &str {
        &self.user_id
    }

    /// Login username, when the account has one.
    pub fn username(&self) -> Option<&str> {
        self.username.as_deref()
    }

    /// The name to show for this user, as in "requested by": the login as
    /// typed at sign-up (`auth_users.username_display`), else the login,
    /// else the user id. Not the profile's `display_name`.
    pub fn username_display(&self) -> &str {
        self.username_display.as_deref().unwrap_or(&self.user_id)
    }

    /// Account role, read with the token for this request. Auto-approval
    /// and quota skips read it; access is proven by an extractor.
    pub fn role(&self) -> Role {
        self.role
    }

    /// Whether this is a signed-in admin, the test [`Admin`] makes, without
    /// taking the principal. Work done for a user never is.
    pub fn is_admin(&self) -> bool {
        self.origin == Origin::Session && self.role.is_admin()
    }

    /// Whether this is a signed-in curator (admin or trusted), the test
    /// [`Curator`] makes. Work done for a user never is.
    fn is_curator(&self) -> bool {
        self.origin == Origin::Session && self.role.is_curator()
    }

    /// Authenticating session id (for the `current` marker in listings).
    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    /// Kind of the authenticating session.
    pub fn session_kind(&self) -> SessionKind {
        self.session_kind
    }

    /// Transport that carried the credential.
    pub fn transport(&self) -> Transport {
        self.transport
    }

    /// Reject companion (device) tokens from creating credentials: further
    /// tokens, app passwords, or a first local password. A stolen device
    /// token must not turn into a permanent credential.
    pub fn require_standard_session(&self) -> Result<(), UsersError> {
        match self.session_kind {
            SessionKind::Standard => Ok(()),
            SessionKind::Companion => Err(UsersError::Forbidden {
                message: "Device tokens cannot create credentials; sign in to do this".to_owned(),
            }),
        }
    }
}

/// A principal lacked the role a [`Curator`] or an [`Admin`] needs. Answers
/// 403 `FORBIDDEN` with its message.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum RoleRequired {
    /// Not an admin or a trusted user.
    #[error("Curator role required")]
    Curator,
    /// Not an admin.
    #[error("Admin role required")]
    Admin,
}

impl From<RoleRequired> for ApiError {
    fn from(error: RoleRequired) -> Self {
        ApiError::forbidden(error.to_string())
    }
}

/// Any signed-in caller. No principal is 401.
#[derive(Debug, Clone)]
pub struct User(pub Principal);

/// A curator: an admin or a trusted user. Anyone else is 403. Made only
/// by the extractor or by `TryFrom<Principal>`.
#[derive(Debug, Clone)]
pub struct Curator(Principal);

/// An admin. Anyone else is 403. Made only by the extractor or by
/// `TryFrom<Principal>`.
#[derive(Debug, Clone)]
pub struct Admin(Principal);

impl TryFrom<Principal> for Curator {
    type Error = RoleRequired;

    fn try_from(principal: Principal) -> Result<Self, Self::Error> {
        if principal.is_curator() {
            Ok(Self(principal))
        } else {
            Err(RoleRequired::Curator)
        }
    }
}

impl TryFrom<Principal> for Admin {
    type Error = RoleRequired;

    fn try_from(principal: Principal) -> Result<Self, Self::Error> {
        if principal.is_admin() {
            Ok(Self(principal))
        } else {
            Err(RoleRequired::Admin)
        }
    }
}

impl Curator {
    /// The curator's principal.
    pub fn principal(&self) -> &Principal {
        &self.0
    }

    /// Give the principal back.
    pub fn into_principal(self) -> Principal {
        self.0
    }
}

impl Admin {
    /// The admin's principal.
    pub fn principal(&self) -> &Principal {
        &self.0
    }

    /// Give the principal back.
    pub fn into_principal(self) -> Principal {
        self.0
    }
}

impl Deref for Curator {
    type Target = Principal;

    fn deref(&self) -> &Principal {
        &self.0
    }
}

impl Deref for Admin {
    type Target = Principal;

    fn deref(&self) -> &Principal {
        &self.0
    }
}

/// The principal the session gate (or the test sign-in layer) put in the
/// request. None means the request never passed the gate: 401. So is a
/// principal for work done for a user, which never belongs in a request.
fn request_principal(parts: &Parts) -> Result<Principal, ApiError> {
    parts
        .extensions
        .get::<Principal>()
        .filter(|principal| principal.origin == Origin::Session)
        .cloned()
        .ok_or_else(|| ApiError::unauthorized("Authentication required"))
}

impl<S: Send + Sync> FromRequestParts<S> for User {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, _: &S) -> Result<Self, Self::Rejection> {
        request_principal(parts).map(User)
    }
}

impl<S: Send + Sync> FromRequestParts<S> for Curator {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, _: &S) -> Result<Self, Self::Rejection> {
        Ok(Curator::try_from(request_principal(parts)?)?)
    }
}

impl<S: Send + Sync> FromRequestParts<S> for Admin {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, _: &S) -> Result<Self, Self::Rejection> {
        Ok(Admin::try_from(request_principal(parts)?)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::{Request, StatusCode, header};
    use axum::response::IntoResponse;

    #[test]
    fn stored_roles_degrade_to_least_privilege() {
        assert_eq!(Role::from_stored("owner"), Role::User);
    }

    #[test]
    fn companion_sessions_cannot_mint() {
        let companion = Principal::for_tests("u", Role::Admin, "s", SessionKind::Companion);
        assert!(companion.require_standard_session().is_err());
        let standard = Principal::for_tests("u", Role::Admin, "s", SessionKind::Standard);
        assert!(standard.require_standard_session().is_ok());
        assert!(
            Principal::acting_for("u", Role::Admin, None)
                .require_standard_session()
                .is_err()
        );
    }

    fn parts(principal: Option<Principal>) -> Parts {
        let mut request = Request::new(());
        if let Some(principal) = principal {
            request.extensions_mut().insert(principal);
        }
        request.into_parts().0
    }

    /// Status, envelope message and challenge of an extractor's answer.
    async fn answer<E>(principal: Option<Principal>) -> Option<(StatusCode, String, bool)>
    where
        E: FromRequestParts<(), Rejection = ApiError>,
    {
        let error = E::from_request_parts(&mut parts(principal), &())
            .await
            .err()?;
        let response = error.into_response();
        let status = response.status();
        let challenged = response.headers().get(header::WWW_AUTHENTICATE).is_some();
        let bytes = axum::body::to_bytes(response.into_body(), 4096)
            .await
            .unwrap();
        let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        Some((
            status,
            body["error"]["message"].as_str().unwrap().to_owned(),
            challenged,
        ))
    }

    #[tokio::test]
    async fn extractors_read_only_the_principal_and_judge_its_role() {
        let signed_in = |role| Some(Principal::for_tests("u", role, "s", SessionKind::Standard));
        let unauthorized = Some((
            StatusCode::UNAUTHORIZED,
            "Authentication required".to_owned(),
            true,
        ));
        let refused = |message: &str| Some((StatusCode::FORBIDDEN, message.to_owned(), false));

        assert_eq!(answer::<User>(None).await, unauthorized);
        assert_eq!(answer::<Curator>(None).await, unauthorized);
        assert_eq!(answer::<Admin>(None).await, unauthorized);

        assert_eq!(answer::<User>(signed_in(Role::User)).await, None);
        assert_eq!(
            answer::<Curator>(signed_in(Role::User)).await,
            refused("Curator role required")
        );
        assert_eq!(answer::<Curator>(signed_in(Role::Trusted)).await, None);
        assert_eq!(answer::<Curator>(signed_in(Role::Admin)).await, None);
        assert_eq!(
            answer::<Admin>(signed_in(Role::Trusted)).await,
            refused("Admin role required")
        );
        assert_eq!(answer::<Admin>(signed_in(Role::Admin)).await, None);

        // A principal for work done for a user, put in a request by
        // mistake, signs nobody in.
        let acting = || Some(Principal::acting_for("u", Role::Admin, None));
        assert_eq!(answer::<User>(acting()).await, unauthorized);
        assert_eq!(answer::<Curator>(acting()).await, unauthorized);
        assert_eq!(answer::<Admin>(acting()).await, unauthorized);
    }

    #[test]
    fn work_done_for_a_user_never_proves_a_role() {
        let acting = Principal::acting_for("u", Role::Admin, None);
        assert_eq!(
            Admin::try_from(acting.clone()).err(),
            Some(RoleRequired::Admin)
        );
        assert_eq!(Curator::try_from(acting).err(), Some(RoleRequired::Curator));
    }
}
