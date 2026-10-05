//! Exact-match mask sentinels.
//!
//! v2 used four different sentinel schemes: exact match (most sections),
//! `startswith` prefix match (Last.fm secrets, wrapped), a `***`-prefix DTO
//! mask with a last-3 leak (AudioDB), and three sections with no mask at all
//! (jellyfin, ListenBrainz, YouTube returned plaintext on GET). v3 uses one
//! rule everywhere: the masked getter returns the section's `MASK` constant
//! when a secret is set (empty string when unset), and a save whose incoming
//! value equals the mask keeps the stored ciphertext. Anything else,
//! including a value that merely starts with the mask, is a new secret.
//!
//! Two consequences worth stating plainly: a real secret that literally
//! equals its mask can never be saved (the save reads as keep-existing),
//! and the v2 `***`-prefix AudioDB limitation is gone (a real key starting
//! with `***` now saves fine, because only the full mask matches).
//!
//! [`Masked`] carries this rule through the API: settings handlers serve
//! and accept masked sections only.

use serde::{Deserialize, Serialize};

/// slskd download-client key (v2 literal kept).
pub const SLSKD_API_KEY_MASK: &str = "slskd****";
/// SABnzbd key (v2 literal kept).
pub const SABNZBD_API_KEY_MASK: &str = "sabnzbd****";
/// Prowlarr key (v2 literal kept).
pub const PROWLARR_API_KEY_MASK: &str = "prowlarr****";
/// Per-indexer key (v2 literal kept).
pub const INDEXER_API_KEY_MASK: &str = "indexer****";
/// Lidarr import key (v2 literal kept).
pub const LIDARR_API_KEY_MASK: &str = "lidarr****";
/// Jellyfin key. New in v3: v2 had no mask and GET returned plaintext.
pub const JELLYFIN_API_KEY_MASK: &str = "jellyfin****";
/// Navidrome password (v2 literal kept).
pub const NAVIDROME_PASSWORD_MASK: &str = "********";
/// Plex token (v2 literal kept).
pub const PLEX_TOKEN_MASK: &str = "plex****";
/// ListenBrainz user token. New in v3: v2 had no mask and GET returned plaintext.
pub const LISTENBRAINZ_TOKEN_MASK: &str = "listenbrainz****";
/// YouTube key. New in v3: v2 had no mask and GET returned plaintext.
pub const YOUTUBE_API_KEY_MASK: &str = "youtube****";
/// Per-user Last.fm credentials (v3 has no admin-global pair, so this mask
/// serves the per-user store). Normalized from the v2 `••••••••` +
/// last-4 prefix match, which leaked key material into responses.
pub const LASTFM_SECRET_MASK: &str = "lastfm****";
/// Spotify client secret (v2 literal kept).
pub const SPOTIFY_SECRET_MASK: &str = "spotify****";
/// Ticketmaster key (v2 literal kept).
pub const TICKETMASTER_KEY_MASK: &str = "ticketmaster****";
/// Skiddle key (v2 literal kept).
pub const SKIDDLE_KEY_MASK: &str = "skiddle****";
/// Wrapped API key. Normalized from the v2 `••••••••` prefix match.
pub const WRAPPED_API_KEY_MASK: &str = "wrapped****";
/// OIDC client secret (v2 literal kept).
pub const OIDC_SECRET_MASK: &str = "oidc****";
/// AcoustID key (v2 literal kept).
pub const ACOUSTID_KEY_MASK: &str = "acoustid****";
/// AudioDB key. New literal replacing the v2 `***...last3` DTO mask, which
/// leaked the last three characters and was stored plaintext.
pub const AUDIODB_API_KEY_MASK: &str = "audiodb****";
/// Plugin secret-flagged values (v2 literal kept; now encrypted at rest -
/// v2 stored them plaintext).
pub const PLUGIN_SECRET_MASK: &str = "plugin****";

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
pub fn resolve_on_save(incoming: &str, mask: &str, strip: bool) -> SaveResolution {
    let candidate = if strip { incoming.trim() } else { incoming };
    if candidate == mask {
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
pub fn resolve_for_probe(incoming: &str, mask: &str, strip: bool, stored_plain: &str) -> String {
    match resolve_on_save(incoming, mask, strip) {
        SaveResolution::KeepStored => stored_plain.to_owned(),
        SaveResolution::StoreNew(value) => value,
    }
}

/// The masked display for one decrypted secret: the mask when set, empty
/// when unset (so clients can tell unset apart from set).
#[must_use]
pub fn display_mask(plaintext: &str, mask: &'static str) -> &'static str {
    if plaintext.is_empty() { "" } else { mask }
}

/// A section (or indexer) as it crosses the API boundary. Every secret
/// field holds its mask sentinel, a new value, or "" (clear), never the
/// stored plaintext:
///
/// - reads build it through [`ConfigStore::get_masked`](super::ConfigStore::get_masked),
///   which swaps each set secret for its mask;
/// - request bodies decode straight into it, and
///   [`ConfigStore::save_secret`](super::ConfigStore::save_secret) resolves
///   each field (mask keeps the stored secret, anything else is new);
/// - [`ConfigStore::unmask`](super::ConfigStore::unmask) turns submitted
///   values into what a connection probe should test.
///
/// Settings handlers serve and accept `Masked<S>` for secret sections, so
/// a raw read (decrypted secrets) cannot be returned by accident: it has a
/// different type. On the wire the wrapper is invisible: it serializes as
/// `S`, and its OpenAPI schema is `S`'s.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Masked<T>(T);

impl<T> Masked<T> {
    /// The wrapped value, for reading or editing non-secret fields before
    /// handing it back to a save (which re-wraps it through `From`).
    #[must_use]
    pub fn into_inner(self) -> T {
        self.0
    }
}

/// Values built server-side follow the same rule as a request body: each
/// secret field holds its mask (keep), a new value, or "" (clear).
impl<T> From<T> for Masked<T> {
    fn from(value: T) -> Self {
        Self(value)
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
            resolve_on_save(SLSKD_API_KEY_MASK, SLSKD_API_KEY_MASK, true),
            SaveResolution::KeepStored
        );
        assert_eq!(
            resolve_on_save("slskd****extra", SLSKD_API_KEY_MASK, true),
            SaveResolution::StoreNew("slskd****extra".to_owned())
        );
        assert_eq!(
            resolve_on_save("  slskd****  ", SLSKD_API_KEY_MASK, true),
            SaveResolution::KeepStored
        );
    }

    #[test]
    fn verbatim_secrets_compare_without_stripping() {
        assert_eq!(
            resolve_on_save(NAVIDROME_PASSWORD_MASK, NAVIDROME_PASSWORD_MASK, false),
            SaveResolution::KeepStored
        );
        assert_eq!(
            resolve_on_save("  ********  ", NAVIDROME_PASSWORD_MASK, false),
            SaveResolution::StoreNew("  ********  ".to_owned())
        );
    }

    #[test]
    fn legacy_prefix_style_masks_are_new_values_now() {
        assert_eq!(
            resolve_on_save("••••••••abcd", WRAPPED_API_KEY_MASK, true),
            SaveResolution::StoreNew("••••••••abcd".to_owned())
        );
        assert_eq!(
            resolve_on_save("***...xyz", AUDIODB_API_KEY_MASK, false),
            SaveResolution::StoreNew("***...xyz".to_owned())
        );
    }

    #[test]
    fn unset_secrets_show_empty_not_the_mask() {
        assert_eq!(display_mask("", SLSKD_API_KEY_MASK), "");
        assert_eq!(
            display_mask("real-key", SLSKD_API_KEY_MASK),
            SLSKD_API_KEY_MASK
        );
    }
}
