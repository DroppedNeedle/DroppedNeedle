//! The deployment environment, as a list.
//!
//! [`DEPLOYMENT_VARS`] names every variable
//! [`AppConfig::load`](super::AppConfig::load) reads, with its default; a
//! unit test proves the list and the loader agree. Use sites take the
//! loaded struct by constructor and never read the environment themselves.
//!
//! [`DROPPED_ENV_VARS`] names v2 `Settings` fields that v3 no longer reads.
//! The migration validator uses it to reject environment-only values that
//! turn up in a v2 export file.

/// One environment variable the server reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DeploymentVar {
    /// Environment variable name.
    pub env_name: &'static str,
    /// Value used when unset or blank.
    pub default: &'static str,
    /// What it controls.
    pub note: &'static str,
}

/// Every environment variable [`AppConfig::load`](super::AppConfig::load)
/// reads. Anything user-editable at runtime is a typed config section
/// instead, never a new entry here.
pub const DEPLOYMENT_VARS: &[DeploymentVar] = &[
    DeploymentVar {
        env_name: "PORT",
        default: "8688",
        note: "HTTP port.",
    },
    DeploymentVar {
        env_name: "BIND_HOST",
        default: "auto",
        note: "Listener address; auto is dual-stack [::] with an IPv4 fallback.",
    },
    DeploymentVar {
        env_name: "ROOT_APP_DIR",
        default: "/app",
        note: "Derives the cache, database, config, plugin and web UI paths.",
    },
    DeploymentVar {
        env_name: "CACHE_DIR",
        default: "<root>/cache",
        note: "Database, backups, covers, stamped web UI, disk caches.",
    },
    DeploymentVar {
        env_name: "COVER_CACHE_MAX_SIZE_MB",
        default: "500",
        note: "Size bound on the cover image cache under <cache>/covers.",
    },
    DeploymentVar {
        env_name: "LIBRARY_DB_PATH",
        default: "<cache>/library.db",
        note: "The single SQLite WAL file.",
    },
    DeploymentVar {
        env_name: "CONFIG_FILE_PATH",
        default: "<root>/config/config.json",
        note: "Config file; the encryption key sits next to it.",
    },
    DeploymentVar {
        env_name: "BASE_PATH",
        default: "",
        note: "Reverse-proxy mount prefix.",
    },
    DeploymentVar {
        env_name: "DROPPEDNEEDLE_STATIC_DIR",
        default: "<root>/static",
        note: "Pristine web UI build, stamped into <cache>/static at boot.",
    },
    DeploymentVar {
        env_name: "TRUSTED_PROXY_IPS",
        default: "127.0.0.1,::1",
        note: "Peers whose X-Forwarded-* headers are honored.",
    },
    DeploymentVar {
        env_name: "DISCOVER_WARMER_ENABLED",
        default: "true",
        note: "Keeps each user's discover page and queue deck fresh in the background while they use them.",
    },
    DeploymentVar {
        env_name: "SLSKD_DOWNLOADS_PATH",
        default: "/data/downloads/slskd",
        note: "slskd completed-downloads directory inside the container.",
    },
    DeploymentVar {
        env_name: "LOG_LEVEL",
        default: "INFO",
        note: "DEBUG, INFO, WARNING or ERROR.",
    },
    DeploymentVar {
        env_name: "RUST_LOG",
        default: "",
        note: "Full tracing filter; overrides LOG_LEVEL when set.",
    },
    DeploymentVar {
        env_name: "TZ",
        default: "",
        note: "Timezone name shown next to scan schedules.",
    },
    DeploymentVar {
        env_name: "CONTACT_EMAIL",
        default: "contact@droppedneedle.com",
        note: "Contact address in the outbound User-Agent; MusicBrainz asks for one.",
    },
    DeploymentVar {
        env_name: "HTTP_TIMEOUT",
        default: "30",
        note: "Outbound request timeout in seconds.",
    },
    DeploymentVar {
        env_name: "HTTP_CONNECT_TIMEOUT",
        default: "10",
        note: "Outbound connect timeout in seconds.",
    },
    DeploymentVar {
        env_name: "HTTP_MAX_KEEPALIVE",
        default: "50",
        note: "Idle outbound connections kept per host.",
    },
    DeploymentVar {
        env_name: "SHUTDOWN_GRACE_PERIOD",
        default: "10",
        note: "Seconds to drain connections and stop background work.",
    },
];

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
