//! Typed runtime config (tier 2 of 2) plus the secrets core.
//!
//! Deployment values live in tier 1 (the environment via `AppConfig::load`;
//! see [`deployment`](crate::config::deployment) for the kept/dropped
//! registry). Everything
//! user-editable at runtime lives here as a typed section with a schema and
//! a getter on [`ConfigStore`](store::ConfigStore). Secrets follow the
//! exact-match mask-sentinel rule ([`mask`]) and are encrypted at rest by
//! [`Crypto`](crypto::Crypto); they are never logged (a test enforces it).
//!
//! No HTTP surface lives here: `settings` owns the settings routes,
//! `import` the v2 importer (which re-encrypts under this key), and
//! `providers::youtube` the client around [`QuotaStore`](quota::QuotaStore).

pub mod crypto;
pub mod error;
pub mod mask;
pub mod quota;
pub mod secret;
pub mod secret_sections;
pub mod sections;
pub mod store;

pub use crypto::Crypto;
pub use error::ConfigError;
pub use mask::Masked;
pub use quota::{QuotaStatus, QuotaStore};
pub use secret::Secret;
pub use secret_sections::SecretSection;
pub use sections::Section;
pub use store::ConfigStore;

/// Every top-level key v3 owns in `config.json`: kept sections plus the
/// instance id. Anything else is reported by
/// [`ConfigStore::unknown_top_level_keys`](store::ConfigStore::unknown_top_level_keys).
pub const KNOWN_TOP_LEVEL_KEYS: &[&str] = &[
    "user_preferences",
    "home_settings",
    "library_scan_schedule",
    "library_scan_filesystem_watcher",
    "advanced_settings",
    "download_client",
    "download_clients",
    "download_policy",
    "wanted",
    "source_priority",
    "usenet_search_backend",
    "indexers",
    "prowlarr",
    "lidarr_import",
    "jellyfin_settings",
    "navidrome_settings",
    "plex_settings",
    "listenbrainz_settings",
    "youtube_settings",
    "lastfm_settings",
    "lyrics_settings",
    "spotify_settings",
    "events",
    "wrapped_settings",
    "oidc_settings",
    "security_settings",
    "connect_apps",
    "library_settings",
    "library_management",
    "musicbrainz_settings",
    "scrobble_settings",
    "primary_music_source",
    "free_music",
    "get_it",
    "plugins",
    "instance_id",
    "_internal",
];

/// One dropped section with the reason it is gone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DroppedSection {
    /// Former config-file key.
    pub key: &'static str,
    /// Why it is gone.
    pub note: &'static str,
}

/// Sections v3 dropped. The import validator rejects these in exports;
/// [`ConfigStore::dropped_sections_present`](store::ConfigStore::dropped_sections_present)
/// reports them when a v2 file is opened directly.
pub const DROPPED_SECTIONS: &[DroppedSection] = &[
    DroppedSection {
        key: "library_sync_settings",
        note: "Legacy catalog; one-shot sync_frequency import only.",
    },
    DroppedSection {
        key: "library_scan_dirty_scopes",
        note: "Transient hints; mechanism stays runtime-only.",
    },
    DroppedSection {
        key: "local_files_settings",
        note: "Vestigial; zero consumers, no routes.",
    },
    DroppedSection {
        key: "_legacy_lidarr",
        note: "One-time backup; its plaintext key never carries forward.",
    },
    DroppedSection {
        key: "jellyfin_url",
        note: "Top-level mirror; the section URL wins.",
    },
];

/// Dropped section keys, for presence checks.
pub const DROPPED_SECTION_KEYS: &[&str] = &[
    "library_sync_settings",
    "library_scan_dirty_scopes",
    "local_files_settings",
    "_legacy_lidarr",
    "jellyfin_url",
];
