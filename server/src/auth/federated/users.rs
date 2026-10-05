//! Shared federated user import: find-or-create plus username derivation.
//!
//! All three providers (OIDC, Jellyfin, Plex) run the same steps as v2:
//! an existing `(provider, provider_uid)` binding refreshes its tokens and
//! returns the linked user; otherwise a matching email links the provider
//! to that user; otherwise a new user is created (admin when the instance
//! is empty, `user` otherwise) with an auto-derived username so a local
//! password can be set later without a choose-username step.

use super::FederatedError;

/// New-user role when the instance already has users.
pub const ROLE_USER: &str = "user";
/// New-user role for the first user on the instance.
pub const ROLE_ADMIN: &str = "admin";

/// Provider names, v2 `auth_providers.provider` values kept verbatim.
pub const PROVIDER_OIDC: &str = "oidc";
/// Provider names, v2 `auth_providers.provider` values kept verbatim.
pub const PROVIDER_JELLYFIN: &str = "jellyfin";
/// Provider names, v2 `auth_providers.provider` values kept verbatim.
pub const PROVIDER_PLEX: &str = "plex";

/// Insert/create races retry this many times before giving up (v2 parity).
pub const CREATE_RETRIES: u32 = 20;

/// Minimal user view the federated flows need.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredUser {
    /// `auth_users.id`.
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

/// One `(provider, provider_uid)` binding row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderBinding {
    /// `auth_providers.id`.
    pub id: String,
    /// Owning user.
    pub user_id: String,
    /// `oidc`, `jellyfin`, or `plex`.
    pub provider: String,
    /// Provider-side user id (`sub`, Jellyfin id, Plex uuid).
    pub provider_uid: String,
}

/// Fields for creating a user from a federated profile.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewFederatedUser {
    /// Display name from the provider.
    pub display_name: String,
    /// [`ROLE_ADMIN`] for the first user, else [`ROLE_USER`].
    pub role: String,
    /// Lowercased email, when the provider supplies one.
    pub email: Option<String>,
    /// Avatar URL from the provider.
    pub avatar_url: Option<String>,
    /// Derived lowercased login identifier.
    pub username: String,
    /// Derived preferred casing for display.
    pub username_display: String,
}

/// Provider-agnostic profile handed to [`find_or_create_federated_user`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FederatedProfile {
    /// Provider-side user id.
    pub provider_uid: String,
    /// Display name from the provider.
    pub display_name: String,
    /// Lowercased email, when the provider supplies one.
    pub email: Option<String>,
    /// Avatar URL from the provider.
    pub avatar_url: Option<String>,
    /// Plaintext token JSON, e.g. `{"access_token":"..."}`. The production
    /// store must seal this with the deployment key before persisting it
    /// as `provider_data`; fakes keep it in memory only.
    pub token_json: String,
}

/// Persistence port for federated user import. The production adapter is
/// the sibling auth tables adapter; id generation is adapter-owned.
pub trait FederatedUserStore: Clone + Send + Sync + 'static {
    /// Look up a provider binding by `(provider, provider_uid)`.
    fn get_provider(
        &self,
        provider: &str,
        provider_uid: &str,
    ) -> impl Future<Output = Result<Option<ProviderBinding>, FederatedError>> + Send;

    /// Look up a user by id.
    fn get_user_by_id(
        &self,
        user_id: &str,
    ) -> impl Future<Output = Result<Option<StoredUser>, FederatedError>> + Send;

    /// Look up a user by lowercased email.
    fn get_user_by_email(
        &self,
        email: &str,
    ) -> impl Future<Output = Result<Option<StoredUser>, FederatedError>> + Send;

    /// Look up a user by lowercased username (drives derivation).
    fn get_user_by_username(
        &self,
        username: &str,
    ) -> impl Future<Output = Result<Option<StoredUser>, FederatedError>> + Send;

    /// True when at least one user exists.
    fn has_any_users(&self) -> impl Future<Output = Result<bool, FederatedError>> + Send;

    /// Insert a user; maps a unique-username race to
    /// [`FederatedError::UsernameTaken`] so the caller re-derives.
    fn create_user(
        &self,
        user: NewFederatedUser,
    ) -> impl Future<Output = Result<StoredUser, FederatedError>> + Send;

    /// Insert a `(provider, provider_uid)` binding with sealed tokens.
    fn create_provider(
        &self,
        user_id: &str,
        provider: &str,
        provider_uid: &str,
        token_json: &str,
    ) -> impl Future<Output = Result<ProviderBinding, FederatedError>> + Send;

    /// Replace the sealed tokens on a binding (called on every login).
    fn update_provider_tokens(
        &self,
        binding_id: &str,
        token_json: &str,
    ) -> impl Future<Output = Result<(), FederatedError>> + Send;
}

/// Run the shared import: existing binding, email link, or fresh create.
pub async fn find_or_create_federated_user<S: FederatedUserStore>(
    store: &S,
    provider: &str,
    profile: &FederatedProfile,
) -> Result<StoredUser, FederatedError> {
    if let Some(binding) = store.get_provider(provider, &profile.provider_uid).await? {
        store
            .update_provider_tokens(&binding.id, &profile.token_json)
            .await?;
        let user = store.get_user_by_id(&binding.user_id).await?;
        return user
            .ok_or_else(|| FederatedError::Authentication("Linked account not found".to_owned()));
    }

    if let Some(email) = profile.email.as_deref()
        && let Some(user) = store.get_user_by_email(email).await?
    {
        store
            .create_provider(
                &user.id,
                provider,
                &profile.provider_uid,
                &profile.token_json,
            )
            .await?;
        return Ok(user);
    }

    let role = if store.has_any_users().await? {
        ROLE_USER
    } else {
        ROLE_ADMIN
    };
    for _ in 0..CREATE_RETRIES {
        let (username, username_display) =
            derive_username(store, profile.email.as_deref(), &profile.display_name).await?;
        let created = store
            .create_user(NewFederatedUser {
                display_name: profile.display_name.clone(),
                role: role.to_owned(),
                email: profile.email.clone(),
                avatar_url: profile.avatar_url.clone(),
                username,
                username_display,
            })
            .await;
        match created {
            Ok(user) => {
                store
                    .create_provider(
                        &user.id,
                        provider,
                        &profile.provider_uid,
                        &profile.token_json,
                    )
                    .await?;
                return Ok(user);
            }
            Err(FederatedError::UsernameTaken) => continue,
            Err(other) => return Err(other),
        }
    }
    Err(FederatedError::Authentication(
        "Could not create an account from this provider".to_owned(),
    ))
}

/// Derive a unique `(username, username_display)` pair: email local-part,
/// else display name, else `user`, de-duped with a numeric suffix
/// (`jane`, `jane-2`, ...). An intended unification: v2 SSO passed
/// display-name-only while bulk import passed email-first; v3 runs every
/// provider through the email-first rule (fresh assignment, no continuity
/// impact).
pub async fn derive_username<S: FederatedUserStore>(
    store: &S,
    email: Option<&str>,
    display_name: &str,
) -> Result<(String, String), FederatedError> {
    let base = username_base(email, display_name);
    let mut n = 1u32;
    loop {
        let (username, display) = if n == 1 {
            (base.to_lowercase(), base.clone())
        } else {
            (
                format!("{}-{n}", base.to_lowercase()),
                format!("{base}-{n}"),
            )
        };
        if store.get_user_by_username(&username).await?.is_none() {
            return Ok((username, display));
        }
        n += 1;
    }
}

/// Pick the base candidate: email local-part, else display name, else `user`.
pub fn username_base(email: Option<&str>, display_name: &str) -> String {
    let local_part = email
        .and_then(|address| address.split('@').next())
        .filter(|_| email.is_some_and(|address| address.contains('@')))
        .unwrap_or("");
    let slug = slugify(local_part);
    if !slug.is_empty() {
        return slug;
    }
    let slug = slugify(display_name);
    if !slug.is_empty() {
        return slug;
    }
    "user".to_owned()
}

/// Reduce a string to `[a-zA-Z0-9._-]`: disallowed runs become `-`,
/// repeated `-` collapse, leading/trailing `-._` trimmed. v2 `_slugify`
/// parity (case preserved; the caller lowercases for storage).
pub fn slugify(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for byte in raw.trim().bytes() {
        let ok = byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-');
        let c = if ok { byte as char } else { '-' };
        if c != '-' || !out.ends_with('-') {
            out.push(c);
        }
    }
    out.trim_matches(['-', '.', '_']).to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugify_matches_v2_charset_rules() {
        assert_eq!(slugify("Jane Doe"), "Jane-Doe");
        assert_eq!(slugify("  a!!b  "), "a-b");
        assert_eq!(slugify("...x..."), "x");
        assert_eq!(slugify("a---b"), "a-b");
        assert_eq!(slugify("!!!"), "");
        assert_eq!(slugify("under_score.1-x"), "under_score.1-x");
        assert_eq!(slugify("caf\u{e9}"), "caf");
        assert_eq!(slugify("a b\u{3000}c"), "a-b-c");
        assert_eq!(slugify("-._-x-._-"), "x");
        assert_eq!(slugify("a/b\\c@d:e"), "a-b-c-d-e");
    }

    #[test]
    fn username_base_prefers_email_local_part() {
        assert_eq!(
            username_base(Some("Jane.Doe@Example.com"), "ignored"),
            "Jane.Doe"
        );
        assert_eq!(
            username_base(Some("no-at-sign"), "Display Name"),
            "Display-Name"
        );
        assert_eq!(username_base(None, "!!!"), "user");
    }
}
