//! Fakes for the federated building blocks: an in-memory user store, a
//! deterministic session issuer and a password hasher. No network, no
//! clock, no real crypto. Clones share state through `Arc`, so a test can
//! hold one handle while the code under test holds another.
//!
//! The identity providers themselves are exercised end to end against
//! local mock servers instead (tests/it/auth_federated.rs).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use super::FederatedError;
use super::SessionIssuer;
use super::password_import::PasswordHasher;
use super::users::{FederatedUserStore, NewFederatedUser, ProviderBinding, StoredUser};

fn locked<T>(mutex: &Mutex<T>) -> Result<std::sync::MutexGuard<'_, T>, FederatedError> {
    mutex
        .lock()
        .map_err(|_| FederatedError::StoreUnavailable("fake lock poisoned".to_owned()))
}

// --- user store ---

#[derive(Debug, Default)]
struct FakeUserRows {
    by_id: HashMap<String, StoredUser>,
    by_email: HashMap<String, String>,
    by_username: HashMap<String, String>,
    providers: HashMap<(String, String), ProviderBinding>,
    provider_tokens: HashMap<String, String>,
    next_user: u64,
    next_provider: u64,
}

/// In-memory [`FederatedUserStore`]. Ids are `user-N` / `provider-N`.
#[derive(Debug, Clone, Default)]
pub struct FakeUserStore {
    rows: Arc<Mutex<FakeUserRows>>,
}

impl FakeUserStore {
    /// Empty store.
    pub fn new() -> Self {
        Self::default()
    }

    /// Sealed tokens currently stored on a binding (plaintext in the fake).
    pub fn provider_tokens(&self, binding_id: &str) -> Option<String> {
        locked(&self.rows)
            .ok()?
            .provider_tokens
            .get(binding_id)
            .cloned()
    }

    /// Binding id for a provider pair, when one exists.
    pub fn binding_id(&self, provider: &str, uid: &str) -> Option<String> {
        locked(&self.rows)
            .ok()?
            .providers
            .get(&(provider.to_owned(), uid.to_owned()))
            .map(|binding| binding.id.clone())
    }
}

impl FederatedUserStore for FakeUserStore {
    async fn get_provider(
        &self,
        provider: &str,
        provider_uid: &str,
    ) -> Result<Option<ProviderBinding>, FederatedError> {
        let rows = locked(&self.rows)?;
        Ok(rows
            .providers
            .get(&(provider.to_owned(), provider_uid.to_owned()))
            .cloned())
    }

    async fn get_user_by_id(&self, user_id: &str) -> Result<Option<StoredUser>, FederatedError> {
        let rows = locked(&self.rows)?;
        Ok(rows.by_id.get(user_id).cloned())
    }

    async fn get_user_by_email(&self, email: &str) -> Result<Option<StoredUser>, FederatedError> {
        let rows = locked(&self.rows)?;
        let id = rows.by_email.get(email).cloned();
        Ok(id.and_then(|id| rows.by_id.get(&id).cloned()))
    }

    async fn get_user_by_username(
        &self,
        username: &str,
    ) -> Result<Option<StoredUser>, FederatedError> {
        let rows = locked(&self.rows)?;
        let id = rows.by_username.get(username).cloned();
        Ok(id.and_then(|id| rows.by_id.get(&id).cloned()))
    }

    async fn create_user(&self, user: NewFederatedUser) -> Result<StoredUser, FederatedError> {
        let mut rows = locked(&self.rows)?;
        if rows.by_username.contains_key(&user.username) {
            return Err(FederatedError::UsernameTaken);
        }
        if let Some(email) = user.email.as_deref()
            && rows.by_email.contains_key(email)
        {
            return Err(FederatedError::UsernameTaken);
        }
        let role = if rows.by_id.is_empty() {
            super::users::ROLE_ADMIN
        } else {
            super::users::ROLE_USER
        };
        rows.next_user += 1;
        let stored = StoredUser {
            id: format!("user-{}", rows.next_user),
            display_name: user.display_name,
            role: role.to_owned(),
            email: user.email.clone(),
            avatar_url: user.avatar_url,
            username: user.username.clone(),
            username_display: user.username_display,
        };
        rows.by_username
            .insert(stored.username.clone(), stored.id.clone());
        if let Some(email) = stored.email.clone() {
            rows.by_email.insert(email, stored.id.clone());
        }
        rows.by_id.insert(stored.id.clone(), stored.clone());
        rows.next_provider += 1;
        let binding = ProviderBinding {
            id: format!("provider-{}", rows.next_provider),
            user_id: stored.id.clone(),
            provider: user.provider.clone(),
            provider_uid: user.provider_uid.clone(),
        };
        rows.provider_tokens
            .insert(binding.id.clone(), user.token_json);
        rows.providers
            .insert((user.provider, user.provider_uid), binding);
        Ok(stored)
    }

    async fn create_provider(
        &self,
        user_id: &str,
        provider: &str,
        provider_uid: &str,
        token_json: &str,
    ) -> Result<ProviderBinding, FederatedError> {
        let mut rows = locked(&self.rows)?;
        rows.next_provider += 1;
        let binding = ProviderBinding {
            id: format!("provider-{}", rows.next_provider),
            user_id: user_id.to_owned(),
            provider: provider.to_owned(),
            provider_uid: provider_uid.to_owned(),
        };
        rows.provider_tokens
            .insert(binding.id.clone(), token_json.to_owned());
        rows.providers.insert(
            (provider.to_owned(), provider_uid.to_owned()),
            binding.clone(),
        );
        Ok(binding)
    }

    async fn update_provider_tokens(
        &self,
        binding_id: &str,
        token_json: &str,
    ) -> Result<(), FederatedError> {
        locked(&self.rows)?
            .provider_tokens
            .insert(binding_id.to_owned(), token_json.to_owned());
        Ok(())
    }
}

// --- sessions ---

#[derive(Debug, Default)]
struct FakeSessionRows {
    live: HashMap<String, String>,
    next: u64,
}

/// Deterministic [`SessionIssuer`]: tokens are `test-session-N`.
#[derive(Debug, Clone, Default)]
pub struct FakeSessionIssuer {
    rows: Arc<Mutex<FakeSessionRows>>,
}

impl FakeSessionIssuer {
    /// Empty issuer.
    pub fn new() -> Self {
        Self::default()
    }

    /// True while the token is live.
    pub fn is_live(&self, token: &str) -> bool {
        locked(&self.rows)
            .map(|rows| rows.live.contains_key(token))
            .unwrap_or(false)
    }

    /// Sessions issued so far.
    pub fn issued(&self) -> u64 {
        locked(&self.rows).map(|rows| rows.next).unwrap_or(0)
    }

    /// Drop every session (simulates the import wipe).
    pub fn revoke_all(&self) {
        if let Ok(mut rows) = self.rows.lock() {
            rows.live.clear();
        }
    }
}

impl SessionIssuer for FakeSessionIssuer {
    async fn issue_session(
        &self,
        user_id: &str,
        _user_agent: Option<&str>,
    ) -> Result<String, FederatedError> {
        let mut rows = locked(&self.rows)?;
        rows.next += 1;
        let token = format!("test-session-{}", rows.next);
        rows.live.insert(token.clone(), user_id.to_owned());
        Ok(token)
    }
}

// --- password hashing ---

/// Deterministic [`PasswordHasher`]: bcrypt fixtures look like
/// `$2b$12$fake:{password}`, Argon2id hashes like `$argon2id$fake:{password}`.
/// Flow-only; the shapes prove scheme dispatch, never real hashing.
#[derive(Debug, Clone, Default)]
pub struct FakeHasher {
    dummy_calls: Arc<Mutex<u64>>,
}

impl FakeHasher {
    /// Fresh fake.
    pub fn new() -> Self {
        Self::default()
    }

    /// The bcrypt hash the fake accepts for `password`.
    pub fn bcrypt_fixture(password: &str) -> String {
        format!("$2b$12$fake:{password}")
    }

    /// How many dummy verifies ran.
    pub fn dummy_calls(&self) -> u64 {
        self.dummy_calls.lock().map(|count| *count).unwrap_or(0)
    }
}

impl PasswordHasher for FakeHasher {
    fn verify_bcrypt(&self, password: &str, hash: &str) -> bool {
        hash == Self::bcrypt_fixture(password)
    }

    fn verify_argon2id(&self, password: &str, hash: &str) -> bool {
        hash == self.hash_argon2id(password)
    }

    fn hash_argon2id(&self, password: &str) -> String {
        format!("$argon2id$fake:{password}")
    }

    fn dummy_verify(&self) {
        if let Ok(mut count) = self.dummy_calls.lock() {
            *count += 1;
        }
    }
}
