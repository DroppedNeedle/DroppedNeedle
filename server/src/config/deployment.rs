//! Deployment-tier registry (tier 1 of 2).
//!
//! Deployment and infrastructure values come from the environment via the
//! ONE accessor (`AppConfig::load` in `config.rs`); use sites take the
//! loaded struct by constructor and never read the environment themselves.
//! This module is the machine-readable list of which v2 `Settings` fields
//! survive as environment knobs in v3 after D-hygiene, and which were
//! dropped with their decision refs. The stage-11 export validator uses the
//! dropped list to reject env-tier values smuggled into the export file.
//! Only PORT is wired into `AppConfig` so far; the rest wires up in
//! consumer stages as they land.

/// One environment knob v3 keeps.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DeploymentVar {
    /// Environment variable name.
    pub env_name: &'static str,
    /// Default when unset.
    pub default: &'static str,
    /// One-line owner note.
    pub note: &'static str,
}

/// One v2 `Settings` field dropped from the deployment tier.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DroppedEnvVar {
    /// Former environment variable name.
    pub env_name: &'static str,
    /// Decision ref (D-list or scout finding).
    pub decision: &'static str,
    /// Where the value lives now, or why it is gone.
    pub note: &'static str,
}

/// Every environment knob v3 keeps. Anything user-editable at runtime is a
/// typed config section instead, never a new entry here.
pub const DEPLOYMENT_VARS: &[DeploymentVar] = &[
    DeploymentVar {
        env_name: "PORT",
        default: "8688",
        note: "HTTP port; already owned by AppConfig.",
    },
    DeploymentVar {
        env_name: "CONFIG_FILE_PATH",
        default: "<root>/config/config.json",
        note: "Config file location.",
    },
    DeploymentVar {
        env_name: "ROOT_APP_DIR",
        default: "/app",
        note: "Derives cache, db, and config paths.",
    },
    DeploymentVar {
        env_name: "CACHE_DIR",
        default: "<root>/cache",
        note: "Covers, quota file, staging, disk caches.",
    },
    DeploymentVar {
        env_name: "LIBRARY_DB_PATH",
        default: "<cache>/library.db",
        note: "The single SQLite WAL file.",
    },
    DeploymentVar {
        env_name: "CONTACT_EMAIL",
        default: "contact@droppedneedle.com",
        note: "MusicBrainz User-Agent contact.",
    },
    DeploymentVar {
        env_name: "LOG_LEVEL",
        default: "INFO",
        note: "DEBUG/INFO/WARNING/ERROR/CRITICAL.",
    },
    DeploymentVar {
        env_name: "DEBUG",
        default: "false",
        note: "App-factory debug branch.",
    },
    DeploymentVar {
        env_name: "BASE_PATH",
        default: "",
        note: "Reverse-proxy mount prefix.",
    },
    DeploymentVar {
        env_name: "TRUSTED_PROXY_IPS",
        default: "127.0.0.1,::1",
        note: "Proxy-headers trust list.",
    },
    DeploymentVar {
        env_name: "SLSKD_DOWNLOADS_PATH",
        default: "/data/downloads/slskd",
        note: "Mounted slskd downloads dir.",
    },
    DeploymentVar {
        env_name: "DOWNLOAD_CLIENT_CONCURRENT_SEARCHES",
        default: "1",
        note: "slskd search limit.",
    },
    DeploymentVar {
        env_name: "DOWNLOAD_CLIENT_CONCURRENT_ENQUEUES",
        default: "1",
        note: "slskd enqueue limit (slskd permits one).",
    },
    DeploymentVar {
        env_name: "SHUTDOWN_GRACE_PERIOD",
        default: "10.0",
        note: "Task-drain seconds on shutdown.",
    },
    DeploymentVar {
        env_name: "HTTP_TIMEOUT",
        default: "10.0",
        note: "Outbound HTTP factory default.",
    },
    DeploymentVar {
        env_name: "HTTP_CONNECT_TIMEOUT",
        default: "5.0",
        note: "Outbound connect default.",
    },
    DeploymentVar {
        env_name: "HTTP_MAX_CONNECTIONS",
        default: "200",
        note: "Outbound pool default.",
    },
    DeploymentVar {
        env_name: "HTTP_MAX_KEEPALIVE",
        default: "50",
        note: "Outbound keepalive default.",
    },
    DeploymentVar {
        env_name: "DISCOVER_WARMER_ENABLED",
        default: "true",
        note: "Discover/Home background warmer kill switch.",
    },
    DeploymentVar {
        env_name: "COVER_CACHE_MAX_SIZE_MB",
        default: "500",
        note: "Cover-art cache ceiling.",
    },
];

/// Every v2 `Settings` field dropped from the deployment tier, with refs.
pub const DROPPED_ENV_VARS: &[DroppedEnvVar] = &[
    DroppedEnvVar {
        env_name: "CACHE_TTL_DEFAULT",
        decision: "D6",
        note: "Vestigial; no consumers. Live TTLs are in advanced_settings.",
    },
    DroppedEnvVar {
        env_name: "CACHE_TTL_ARTIST",
        decision: "D6",
        note: "Vestigial; no consumers.",
    },
    DroppedEnvVar {
        env_name: "CACHE_TTL_ALBUM",
        decision: "D6",
        note: "Vestigial; no consumers.",
    },
    DroppedEnvVar {
        env_name: "CACHE_TTL_COVERS",
        decision: "D6",
        note: "Vestigial; no consumers.",
    },
    DroppedEnvVar {
        env_name: "CACHE_CLEANUP_INTERVAL",
        decision: "D6",
        note: "Vestigial; no consumers.",
    },
    DroppedEnvVar {
        env_name: "AUDIODB_API_KEY",
        decision: "D7",
        note: "Dual source; advanced_settings.audiodb_api_key wins.",
    },
    DroppedEnvVar {
        env_name: "AUDIODB_PREMIUM",
        decision: "D7",
        note: "Dual source; the advanced section owns AudioDB.",
    },
    DroppedEnvVar {
        env_name: "JELLYFIN_URL",
        decision: "D8",
        note: "Top-level mirror; jellyfin_settings.jellyfin_url wins.",
    },
    DroppedEnvVar {
        env_name: "INSTANCE_ID",
        decision: "scout-08 §6",
        note: "Dual source collapsed; config-file instance_id is the owner.",
    },
];

/// Look up a kept deployment knob by environment name.
#[must_use]
pub fn find_deployment_var(env_name: &str) -> Option<&'static DeploymentVar> {
    DEPLOYMENT_VARS.iter().find(|var| var.env_name == env_name)
}
