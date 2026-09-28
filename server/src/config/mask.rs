//! Exact-match mask sentinels (D12).
//!
//! v2 used four different sentinel schemes: exact match (most sections),
//! `startswith` prefix match (Last.fm secrets, wrapped), a `***`-prefix DTO
//! mask with a last-3 leak (AudioDB), and three sections with no mask at all
//! (jellyfin, ListenBrainz, YouTube returned plaintext on GET). v3 uses one
//! rule everywhere: the masked getter returns the section's `MASK` constant
//! when a secret is set (empty string when unset), and a save whose incoming
//! value EQUALS the mask keeps the stored ciphertext. Anything else,
//! including a value that merely starts with the mask, is a new secret.
//!
//! Two consequences worth stating plainly: a real secret that literally
//! equals its mask can never be saved (the save reads as keep-existing),
//! and the v2 `***`-prefix AudioDB limitation is gone (a real key starting
//! with `***` now saves fine, because only the full mask matches).

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
/// Jellyfin key. NEW: v2 had no mask and GET returned plaintext.
pub const JELLYFIN_API_KEY_MASK: &str = "jellyfin****";
/// Navidrome password (v2 literal kept).
pub const NAVIDROME_PASSWORD_MASK: &str = "********";
/// Plex token (v2 literal kept).
pub const PLEX_TOKEN_MASK: &str = "plex****";
/// ListenBrainz user token. NEW: v2 had no mask and GET returned plaintext.
pub const LISTENBRAINZ_TOKEN_MASK: &str = "listenbrainz****";
/// YouTube key. NEW: v2 had no mask and GET returned plaintext.
pub const YOUTUBE_API_KEY_MASK: &str = "youtube****";
/// Per-user Last.fm credentials (R7 deleted the admin-global pair, so this
/// mask serves the per-user store). Normalized from the v2 `••••••••` +
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
/// AudioDB key. NEW literal replacing the v2 `***...last3` DTO mask, which
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

/// The masked display for one decrypted secret: the mask when set, empty
/// when unset (so clients can tell unset apart from set).
#[must_use]
pub fn display_mask(plaintext: &str, mask: &'static str) -> &'static str {
    if plaintext.is_empty() { "" } else { mask }
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
