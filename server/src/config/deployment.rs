//! Environment knobs v2 had that v3 dropped.
//!
//! Deployment values come from the environment through one accessor
//! ([`AppConfig::load`](super::AppConfig::load)); use sites take the loaded
//! struct by constructor and never read the environment themselves. The
//! migration validator uses this list to reject environment-only values
//! that turn up in a v2 export file.

/// One v2 `Settings` field dropped from the deployment tier.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DroppedEnvVar {
    /// Former environment variable name.
    pub env_name: &'static str,
    /// Where the value lives now, or why it is gone.
    pub note: &'static str,
}

/// Every v2 `Settings` field dropped from the deployment tier.
pub const DROPPED_ENV_VARS: &[DroppedEnvVar] = &[
    DroppedEnvVar {
        env_name: "CACHE_TTL_DEFAULT",
        note: "Vestigial; no consumers. Live TTLs are in advanced_settings.",
    },
    DroppedEnvVar {
        env_name: "CACHE_TTL_ARTIST",
        note: "Vestigial; no consumers.",
    },
    DroppedEnvVar {
        env_name: "CACHE_TTL_ALBUM",
        note: "Vestigial; no consumers.",
    },
    DroppedEnvVar {
        env_name: "CACHE_TTL_COVERS",
        note: "Vestigial; no consumers.",
    },
    DroppedEnvVar {
        env_name: "CACHE_CLEANUP_INTERVAL",
        note: "Vestigial; no consumers.",
    },
    DroppedEnvVar {
        env_name: "AUDIODB_API_KEY",
        note: "Dual source; advanced_settings.audiodb_api_key wins.",
    },
    DroppedEnvVar {
        env_name: "AUDIODB_PREMIUM",
        note: "Dual source; the advanced section owns AudioDB.",
    },
    DroppedEnvVar {
        env_name: "JELLYFIN_URL",
        note: "Top-level mirror; jellyfin_settings.jellyfin_url wins.",
    },
    DroppedEnvVar {
        env_name: "INSTANCE_ID",
        note: "Dual source collapsed; config-file instance_id is the owner.",
    },
];
