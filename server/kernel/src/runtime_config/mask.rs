//! The one mask.
//!
//! A masked read returns [`SECRET_MASK`] in place of every secret that is
//! set (empty when unset), and a save whose value equals the mask keeps the
//! stored ciphertext. Anything else, including a value that merely starts
//! with the mask, is a new secret. One consequence: a real secret that is
//! literally the mask can never be saved (the save reads as keep).
//!
//! [`Masked`] carries this rule through the API: settings handlers serve
//! and accept masked sections only.

use serde::{Deserialize, Serialize};

/// What every set secret shows as in the API, and what a save sends back
/// to keep it.
pub const SECRET_MASK: &str = "********";

/// How one incoming secret value resolves against the stored ciphertext.
#[derive(Debug, PartialEq, Eq)]
pub enum SaveResolution {
    /// Incoming equals the mask: keep the stored ciphertext untouched.
    KeepStored,
    /// Anything else (already stripped): encrypt and store. Empty stays
    /// empty, which clears the secret.
    StoreNew(String),
}

/// Apply the exact-match rule to one incoming secret value.
///
/// `strip` follows the v2 per-secret precedent: API keys strip paste
/// whitespace before the comparison (a pasted mask with stray spaces still
/// reads as keep-existing, and a pasted key never 403s on a stray space),
/// while passwords and verbatim-saved tokens compare and store exactly as
/// submitted so meaningful edge whitespace survives.
#[must_use]
pub fn resolve_on_save(incoming: &str, strip: bool) -> SaveResolution {
    let candidate = if strip { incoming.trim() } else { incoming };
    if candidate == SECRET_MASK {
        SaveResolution::KeepStored
    } else {
        SaveResolution::StoreNew(candidate.to_owned())
    }
}

/// The value a connection probe should test for one submitted secret:
/// the stored plaintext when the mask came back, otherwise the submitted
/// value under the same strip rule a save applies. Verify endpoints test
/// what the form holds, so this is the save rule pointed at the stored
/// plaintext instead of the stored ciphertext.
#[must_use]
pub fn resolve_for_probe(incoming: &str, strip: bool, stored_plain: &str) -> String {
    match resolve_on_save(incoming, strip) {
        SaveResolution::KeepStored => stored_plain.to_owned(),
        SaveResolution::StoreNew(value) => value,
    }
}

/// The masked display for one decrypted secret: the mask when set, empty
/// when unset (so clients can tell unset apart from set).
#[must_use]
pub fn display_mask(plaintext: &str) -> &'static str {
    if plaintext.is_empty() {
        ""
    } else {
        SECRET_MASK
    }
}

/// A section (or indexer) as it crosses the API boundary. Every secret
/// field holds the mask, a new value, or "" (clear), never the stored
/// plaintext. There are only three ways to get one:
///
/// - a masked read: [`ConfigStore::get_masked`](super::ConfigStore::get_masked)
///   and the echo of [`ConfigStore::save_secret`](super::ConfigStore::save_secret)
///   swap each set secret for the mask;
/// - a request body, which decodes straight into it;
/// - [`Masked::map`] / [`Masked::try_map`], which derive a part or a
///   normalized copy of a value that was already masked.
///
/// There is no conversion from a bare `T`, so a raw read (decrypted
/// secrets) cannot type-check as `Masked`. Settings handlers serve and
/// accept `Masked<S>` for secret sections; [`ConfigStore::unmask`](super::ConfigStore::unmask)
/// turns submitted values into what a connection probe should test. On
/// the wire the wrapper is invisible: it serializes as `S`, and its
/// OpenAPI schema is `S`'s.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Masked<T>(T);

impl<T> Masked<T> {
    /// Wrap a value whose secrets the store just masked. Only the store
    /// builds masked values from scratch.
    pub(super) fn from_masked(value: T) -> Self {
        Self(value)
    }

    /// The wrapped value, for reading or editing non-secret fields before
    /// handing it to a save as submitted values.
    #[must_use]
    pub fn into_inner(self) -> T {
        self.0
    }

    /// Derive a part or a normalized copy of a masked value. `derive`
    /// must only reshape what it is given, never bring in secrets from
    /// elsewhere.
    #[must_use]
    pub fn map<U>(self, derive: impl FnOnce(T) -> U) -> Masked<U> {
        Masked(derive(self.0))
    }

    /// [`Masked::map`] for a derivation that can fail.
    pub fn try_map<U, E>(self, derive: impl FnOnce(T) -> Result<U, E>) -> Result<Masked<U>, E> {
        derive(self.0).map(Masked)
    }
}

impl<T> std::ops::Deref for Masked<T> {
    type Target = T;

    fn deref(&self) -> &T {
        &self.0
    }
}

impl<T: utoipa::PartialSchema> utoipa::PartialSchema for Masked<T> {
    fn schema() -> utoipa::openapi::RefOr<utoipa::openapi::schema::Schema> {
        T::schema()
    }
}

impl<T: utoipa::ToSchema> utoipa::ToSchema for Masked<T> {
    fn name() -> std::borrow::Cow<'static, str> {
        T::name()
    }

    fn schemas(
        schemas: &mut Vec<(
            String,
            utoipa::openapi::RefOr<utoipa::openapi::schema::Schema>,
        )>,
    ) {
        T::schemas(schemas);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_match_keeps_but_prefix_match_does_not() {
        assert_eq!(
            resolve_on_save(SECRET_MASK, true),
            SaveResolution::KeepStored
        );
        assert_eq!(
            resolve_on_save("********extra", true),
            SaveResolution::StoreNew("********extra".to_owned())
        );
        assert_eq!(
            resolve_on_save("  ********  ", true),
            SaveResolution::KeepStored
        );
    }

    #[test]
    fn verbatim_secrets_compare_without_stripping() {
        assert_eq!(
            resolve_on_save(SECRET_MASK, false),
            SaveResolution::KeepStored
        );
        assert_eq!(
            resolve_on_save("  ********  ", false),
            SaveResolution::StoreNew("  ********  ".to_owned())
        );
    }

    #[test]
    fn unset_secrets_show_empty_not_the_mask() {
        assert_eq!(display_mask(""), "");
        assert_eq!(display_mask("real-key"), SECRET_MASK);
    }
}
