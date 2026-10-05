//! Typed config sections holding secrets (tier 2 of 2).
//!
//! Secret fields use [`Secret`](super::secret::Secret), so a derived `Debug`
//! can never leak them. Each section lists its secret fields through
//! [`SecretSection::secret_fields`]; the store drives masking, mask
//! resolution, and encryption generically from that list:
//!
//! - masked read: decrypt every secret, replace non-empty with the mask;
//! - raw read: decrypt every secret;
//! - save: incoming mask keeps the stored ciphertext, anything else is
//!   stripped and re-encrypted (empty stays empty).
//!
//! `validate()` never inspects secret contents (on the save path they may
//! hold the mask) and `normalize()` never touches them.

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

mod accounts;
mod advanced;
mod downloads;
mod library;
mod media_servers;

pub use accounts::*;
pub use advanced::*;
pub use downloads::*;
pub use library::*;
pub use media_servers::*;

use crate::runtime_config::error::ConfigError;
use crate::runtime_config::mask::{
    ACOUSTID_KEY_MASK, AUDIODB_API_KEY_MASK, JELLYFIN_API_KEY_MASK, LIDARR_API_KEY_MASK,
    LISTENBRAINZ_TOKEN_MASK, NAVIDROME_PASSWORD_MASK, OIDC_SECRET_MASK, PLEX_TOKEN_MASK,
    PROWLARR_API_KEY_MASK, SABNZBD_API_KEY_MASK, SKIDDLE_KEY_MASK, SLSKD_API_KEY_MASK,
    SPOTIFY_SECRET_MASK, TICKETMASTER_KEY_MASK, WRAPPED_API_KEY_MASK, YOUTUBE_API_KEY_MASK,
};
use crate::runtime_config::secret::Secret;
use crate::runtime_config::sections::{
    DEFAULT_NAMING_TEMPLATE, Section, check_range, is_valid_hhmm, normalize_http_url,
    sanitize_absolute_mount, sanitize_subpath, strip_api_suffix, tier_rank,
};

/// One mutable secret field plus its mask sentinel.
pub struct SecretField<'a> {
    /// The secret value (ciphertext on load, plaintext after decrypt,
    /// mask or new value on save).
    pub value: &'a mut Secret,
    /// This field's exact-match mask.
    pub mask: &'static str,
    /// Whether v2 strips paste whitespace for this secret. True for API
    /// keys (a stray space earns a 403); false for passwords and
    /// verbatim-saved tokens, where edge whitespace may be meaningful.
    pub strip: bool,
}

/// A [`Section`] with encrypted secret fields.
pub trait SecretSection: Section {
    /// Every secret field in struct order. Order is part of the contract:
    /// the store pairs incoming and stored fields positionally, which is
    /// why variable-length lists (indexers) do not implement this trait.
    fn secret_fields(&mut self) -> Vec<SecretField<'_>>;
}
