//! MusicBrainz source selection, including the BrainzMash binding.

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use super::{Section, validation};
use crate::runtime_config::error::ConfigError;

// --- musicbrainz_settings -------------------------------------------------
// Dropped transients: pending_brainzmash, source_quarantined,
// quarantine_reason. Kept: source_mode, api_url, rate_limit,
// concurrent_searches, community_acknowledged, selected_source_mode,
// source_id, generation, active_brainzmash.

/// Official MusicBrainz API base (v2 `OFFICIAL_MB_API_BASE`).
pub const OFFICIAL_MB_API_BASE: &str = "https://musicbrainz.org/ws/2";
/// Server-owned BrainzMash endpoint. Never accepted from a client.
pub const BRAINZMASH_ENDPOINT: &str = "https://api.brainzmash.cc/ws/2";
/// Forced BrainzMash throughput (v2 `_BRAINZMASH_RATE_LIMIT`).
pub const BRAINZMASH_RATE_LIMIT: f64 = 10.0;
/// Forced BrainzMash concurrency (v2 `_BRAINZMASH_CONCURRENT_SEARCHES`).
pub const BRAINZMASH_CONCURRENT_SEARCHES: i64 = 1;
/// Official-host ceilings (never raised).
pub const OFFICIAL_MB_RATE_LIMIT: f64 = 1.0;
/// Official-host ceilings (never raised).
pub const OFFICIAL_MB_CONCURRENT_SEARCHES: i64 = 6;
/// Widest allowed off-official throughput (0 = Unlimited sentinel).
pub const MAX_MB_RATE_LIMIT: f64 = 500.0;
/// Widest allowed off-official concurrency.
pub const MAX_MB_CONCURRENT_SEARCHES: i64 = 64;

/// MusicBrainz source tier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum MbSourceMode {
    /// musicbrainz.org, hard 1 req/s ceiling.
    Official,
    /// User-owned mirror.
    Mirror,
    /// Community infrastructure.
    Community,
    /// Built-in server-owned source (the v2 effective default).
    #[default]
    Brainzmash,
}

/// Active BrainzMash binding (kept; the pending proposal is transient).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(default)]
pub struct BrainzmashActiveBinding {
    /// Pinned endpoint.
    pub endpoint: String,
    /// Access revision.
    pub access_revision: String,
    /// Source identity.
    pub source_id: String,
    /// Source generation.
    pub generation: i64,
    /// Disclosure version.
    pub disclosure_version: String,
    /// Consent recorded.
    pub consented: bool,
    /// Endpoint verified.
    pub verified: bool,
}

impl Default for BrainzmashActiveBinding {
    fn default() -> Self {
        Self {
            endpoint: BRAINZMASH_ENDPOINT.to_owned(),
            access_revision: String::new(),
            source_id: String::new(),
            generation: 0,
            disclosure_version: "brainzmash-v1".to_owned(),
            consented: false,
            verified: false,
        }
    }
}

/// MusicBrainz connection settings.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(default)]
pub struct MusicBrainzSettings {
    /// Active source tier.
    pub source_mode: MbSourceMode,
    /// API base (canonicalized for official/brainzmash).
    pub api_url: String,
    /// Requests per second (0 = Unlimited, off-official only).
    pub rate_limit: f64,
    /// Concurrent searches.
    pub concurrent_searches: i64,
    /// Community-tier disclosure acknowledged.
    pub community_acknowledged: bool,
    /// Last tier the admin chose.
    pub selected_source_mode: MbSourceMode,
    /// Source identity. A fresh install mints a random id, so the schema
    /// documents no default.
    #[schema(schema_with = source_id_schema)]
    pub source_id: String,
    /// Source generation.
    pub generation: i64,
    /// Active BrainzMash binding, if any.
    pub active_brainzmash: Option<BrainzmashActiveBinding>,
    /// True when the official-host clamp forced values down (or lifted a
    /// 0 sentinel up). Rendered, never refused.
    pub clamped_to_official_limits: bool,
}

impl Default for MusicBrainzSettings {
    fn default() -> Self {
        Self {
            source_mode: MbSourceMode::Brainzmash,
            api_url: BRAINZMASH_ENDPOINT.to_owned(),
            rate_limit: BRAINZMASH_RATE_LIMIT,
            concurrent_searches: BRAINZMASH_CONCURRENT_SEARCHES,
            community_acknowledged: false,
            selected_source_mode: MbSourceMode::Brainzmash,
            source_id: uuid::Uuid::new_v4().to_string(),
            generation: 1,
            active_brainzmash: None,
            clamped_to_official_limits: false,
        }
    }
}

/// Public MusicBrainz origins for transport-rate policy (v2
/// `_MB_RATE_POLICY_PUBLIC_ORIGINS`, both schemes so insecure transport
/// cannot bypass the ceiling). Never identity proof.
const MB_RATE_POLICY_PUBLIC_ORIGINS: [&str; 8] = [
    "http://musicbrainz.org",
    "http://musicbrainz.org:80",
    "http://www.musicbrainz.org",
    "http://www.musicbrainz.org:80",
    "https://musicbrainz.org",
    "https://musicbrainz.org:443",
    "https://www.musicbrainz.org",
    "https://www.musicbrainz.org:443",
];

/// Privacy-safe origin label: `scheme://host[:port]`, lowercased, no
/// credentials or path (v2 `normalize_mb_source_label`, without a URL
/// crate). Returns "" for non-HTTP(S) input.
#[must_use]
pub fn normalize_mb_source_label(url: &str) -> String {
    let trimmed = url.trim();
    let Some((scheme, rest)) = trimmed.split_once("://") else {
        return String::new();
    };
    let scheme = scheme.to_ascii_lowercase();
    if scheme != "http" && scheme != "https" {
        return String::new();
    }
    let authority = rest.split('/').next().unwrap_or("");
    let host_port = authority.rsplit('@').next().unwrap_or("");
    if host_port.is_empty() {
        return String::new();
    }
    let (host, port) = if host_port.starts_with('[') {
        match host_port.split_once("]:") {
            Some((host, port)) => (format!("{host}]"), Some(port)),
            None => {
                if host_port.ends_with(']') {
                    (host_port.to_owned(), None)
                } else {
                    return String::new();
                }
            }
        }
    } else {
        match host_port.split_once(':') {
            Some((host, port)) => (host.to_owned(), Some(port)),
            None => (host_port.to_owned(), None),
        }
    };
    if host.is_empty() || host.contains(':') && !host.starts_with('[') {
        return String::new();
    }
    let mut label = format!("{scheme}://{}", host.to_ascii_lowercase());
    if let Some(port) = port {
        if port.is_empty() || !port.bytes().all(|byte| byte.is_ascii_digit()) {
            return String::new();
        }
        label.push(':');
        label.push_str(port);
    }
    label
}

/// Whether the URL sits on a public MusicBrainz origin (rate ceiling
/// applies).
#[must_use]
pub fn is_mb_rate_policy_public_host(url: &str) -> bool {
    MB_RATE_POLICY_PUBLIC_ORIGINS.contains(&normalize_mb_source_label(url).as_str())
}

impl MusicBrainzSettings {
    fn canonicalize_urls(&mut self) {
        self.api_url = self.api_url.trim().to_owned();
        match self.source_mode {
            MbSourceMode::Official => self.api_url = OFFICIAL_MB_API_BASE.to_owned(),
            MbSourceMode::Brainzmash => self.api_url = BRAINZMASH_ENDPOINT.to_owned(),
            MbSourceMode::Mirror | MbSourceMode::Community => {}
        }
        while self.api_url.ends_with('/') && self.api_url.len() > 1 {
            self.api_url.pop();
        }
    }
}

impl Section for MusicBrainzSettings {
    const KEY: &'static str = "musicbrainz_settings";

    fn validate(&self) -> Result<(), ConfigError> {
        let key = Self::KEY;
        if !self.rate_limit.is_finite() {
            return Err(validation(
                key,
                "rate_limit",
                "rate_limit must be finite".to_owned(),
            ));
        }
        if self.concurrent_searches < 1 {
            return Err(validation(
                key,
                "concurrent_searches",
                "concurrent_searches must be at least 1".to_owned(),
            ));
        }
        match self.source_mode {
            MbSourceMode::Official | MbSourceMode::Brainzmash => {}
            MbSourceMode::Mirror | MbSourceMode::Community => {
                let trimmed = self.api_url.trim();
                if trimmed.is_empty()
                    || !(trimmed.starts_with("http://") || trimmed.starts_with("https://"))
                {
                    return Err(validation(
                        key,
                        "api_url",
                        "api_url must be an absolute HTTP(S) URL for non-official sources"
                            .to_owned(),
                    ));
                }
            }
        }
        if !is_mb_rate_policy_public_host(&self.api_url) {
            if self.rate_limit < 0.0
                || (self.rate_limit > 0.0 && self.rate_limit < 0.1)
                || self.rate_limit > MAX_MB_RATE_LIMIT
            {
                return Err(validation(
                    key,
                    "rate_limit",
                    format!(
                        "rate_limit must be 0 (unlimited) or between 0.1 and {MAX_MB_RATE_LIMIT}"
                    ),
                ));
            }
            if self.concurrent_searches > MAX_MB_CONCURRENT_SEARCHES {
                return Err(validation(
                    key,
                    "concurrent_searches",
                    format!(
                        "concurrent_searches must be between 1 and {MAX_MB_CONCURRENT_SEARCHES}"
                    ),
                ));
            }
        }
        Ok(())
    }

    fn normalize(&mut self) {
        self.canonicalize_urls();
        self.clamped_to_official_limits = false;
        if self.source_mode == MbSourceMode::Brainzmash {
            self.rate_limit = BRAINZMASH_RATE_LIMIT;
            self.concurrent_searches = BRAINZMASH_CONCURRENT_SEARCHES;
        }
        if is_mb_rate_policy_public_host(&self.api_url) {
            let before = (self.rate_limit, self.concurrent_searches);
            self.rate_limit = self.rate_limit.min(OFFICIAL_MB_RATE_LIMIT);
            self.concurrent_searches = self
                .concurrent_searches
                .min(OFFICIAL_MB_CONCURRENT_SEARCHES);
            if self.rate_limit <= 0.0 {
                self.rate_limit = OFFICIAL_MB_RATE_LIMIT;
            }
            self.clamped_to_official_limits = before != (self.rate_limit, self.concurrent_searches);
        }
    }
}

/// `source_id` as a plain string with no default: the default is a
/// freshly minted id, which would make the published contract change on
/// every build.
fn source_id_schema() -> utoipa::openapi::schema::Object {
    utoipa::openapi::schema::ObjectBuilder::new()
        .schema_type(utoipa::openapi::schema::Type::String)
        .description(Some("Source identity."))
        .build()
}
