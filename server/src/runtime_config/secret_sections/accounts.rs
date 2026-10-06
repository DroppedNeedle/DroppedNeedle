//! Third-party accounts and keys: ListenBrainz, YouTube, Spotify, the
//! events sources, Wrapped, OIDC, and the Last.fm app key pair.

use super::*;

// --- listenbrainz_settings (mask gap closed) --------------------------------

/// ListenBrainz connection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default, ToSchema)]
#[serde(default)]
pub struct ListenBrainzConnection {
    /// Username.
    pub username: String,
    /// User token (encrypted at rest; v2 returned it plaintext on GET).
    #[schema(value_type = String)]
    pub user_token: Secret,
    /// Master switch.
    pub enabled: bool,
}

impl Section for ListenBrainzConnection {
    const KEY: &'static str = "listenbrainz_settings";
}

impl SecretSection for ListenBrainzConnection {
    fn secret_fields(&mut self) -> Vec<SecretField<'_>> {
        vec![SecretField {
            value: &mut self.user_token,
            mask: LISTENBRAINZ_TOKEN_MASK,
            strip: false,
        }]
    }
}

// --- youtube_settings (mask gap closed) -------------------------------------

/// YouTube connection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(default)]
pub struct YouTubeConnection {
    /// API key (encrypted at rest; v2 returned it plaintext on GET).
    #[schema(value_type = String)]
    pub api_key: Secret,
    /// Master switch.
    pub enabled: bool,
    /// API search switch.
    pub api_enabled: bool,
    /// Daily quota limit (1-10000).
    pub daily_quota_limit: i64,
}

impl Default for YouTubeConnection {
    fn default() -> Self {
        Self {
            api_key: Secret::default(),
            enabled: false,
            api_enabled: false,
            daily_quota_limit: 80,
        }
    }
}

impl Section for YouTubeConnection {
    const KEY: &'static str = "youtube_settings";

    fn validate(&self) -> Result<(), ConfigError> {
        check_range(
            Self::KEY,
            "daily_quota_limit",
            self.daily_quota_limit,
            1,
            10000,
        )
    }
}

impl SecretSection for YouTubeConnection {
    fn secret_fields(&mut self) -> Vec<SecretField<'_>> {
        vec![SecretField {
            value: &mut self.api_key,
            mask: YOUTUBE_API_KEY_MASK,
            strip: true,
        }]
    }
}

// --- spotify_settings -------------------------------------------------------

/// Spotify settings (OAuth client + import).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct SpotifySettings {
    /// OAuth client id.
    pub client_id: String,
    /// OAuth client secret (encrypted at rest).
    pub client_secret: Secret,
    /// Master switch.
    pub enabled: bool,
    /// Redirect origin for OAuth.
    pub spotify_redirect_origin: String,
}

/// Whether the redirect origin is a bare http(s) origin: absolute URL
/// with a host and no path, query, or fragment (v2 GH-298: anything else
/// silently corrupts the value admins register in the Spotify dashboard).
/// Empty means the dynamic fallback and is always accepted.
#[must_use]
pub fn is_valid_spotify_redirect_origin(origin: &str) -> bool {
    let trimmed = origin.trim();
    if trimmed.is_empty() {
        return true;
    }
    let Some(after_scheme) = trimmed
        .strip_prefix("http://")
        .or_else(|| trimmed.strip_prefix("https://"))
    else {
        return false;
    };
    if after_scheme.is_empty() {
        return false;
    }
    let host_end = after_scheme
        .find(['/', '?', '#'])
        .unwrap_or(after_scheme.len());
    if host_end == 0 {
        return false;
    }
    let rest = &after_scheme[host_end..];
    rest.is_empty() || rest == "/"
}

impl Section for SpotifySettings {
    const KEY: &'static str = "spotify_settings";

    fn validate(&self) -> Result<(), ConfigError> {
        if !is_valid_spotify_redirect_origin(&self.spotify_redirect_origin) {
            return Err(ConfigError::Validation {
                section: Self::KEY,
                field: "spotify_redirect_origin",
                reason: "Spotify redirect origin must be an absolute http(s) URL \
                     with no path, query, or fragment"
                    .to_owned(),
            });
        }
        Ok(())
    }

    fn normalize(&mut self) {
        self.client_id = self.client_id.trim().to_owned();
        self.spotify_redirect_origin = self.spotify_redirect_origin.trim().to_owned();
        while self.spotify_redirect_origin.ends_with('/') && self.spotify_redirect_origin.len() > 1
        {
            self.spotify_redirect_origin.pop();
        }
    }
}

impl SecretSection for SpotifySettings {
    fn secret_fields(&mut self) -> Vec<SecretField<'_>> {
        vec![SecretField {
            value: &mut self.client_secret,
            mask: SPOTIFY_SECRET_MASK,
            strip: false,
        }]
    }
}

// --- events -----------------------------------------------------------------

/// Events sweep scope.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum EventsSweepScope {
    /// Followed artists only.
    #[default]
    Followed,
    /// Every artist in the library index.
    Library,
}

/// Upcoming-events sources. The sweep runs daily at `poll_time`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(default)]
pub struct EventsSettings {
    /// Master switch.
    pub enabled: bool,
    /// Ticketmaster switch.
    pub ticketmaster_enabled: bool,
    /// Ticketmaster key (encrypted at rest).
    #[schema(value_type = String)]
    pub ticketmaster_api_key: Secret,
    /// Skiddle switch.
    pub skiddle_enabled: bool,
    /// Skiddle key (encrypted at rest).
    #[schema(value_type = String)]
    pub skiddle_api_key: Secret,
    /// Daily sweep time, server-local `HH:MM`.
    pub poll_time: String,
    /// Sweep scope.
    pub sweep_scope: EventsSweepScope,
}

impl Default for EventsSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            ticketmaster_enabled: false,
            ticketmaster_api_key: Secret::default(),
            skiddle_enabled: false,
            skiddle_api_key: Secret::default(),
            poll_time: "06:00".to_owned(),
            sweep_scope: EventsSweepScope::Followed,
        }
    }
}

impl Section for EventsSettings {
    const KEY: &'static str = "events";

    fn validate(&self) -> Result<(), ConfigError> {
        if !is_valid_hhmm(&self.poll_time) {
            return Err(ConfigError::Validation {
                section: Self::KEY,
                field: "poll_time",
                reason: format!("must be HH:MM (00:00-23:59), got {:?}", self.poll_time),
            });
        }
        Ok(())
    }
}

impl SecretSection for EventsSettings {
    fn secret_fields(&mut self) -> Vec<SecretField<'_>> {
        vec![
            SecretField {
                value: &mut self.ticketmaster_api_key,
                mask: TICKETMASTER_KEY_MASK,
                strip: true,
            },
            SecretField {
                value: &mut self.skiddle_api_key,
                mask: SKIDDLE_KEY_MASK,
                strip: true,
            },
        ]
    }
}

// --- wrapped_settings -------------------------------------------------------

/// Shared secret for the wrapped endpoints (service-to-service).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default, ToSchema)]
#[serde(default)]
pub struct WrappedSettings {
    /// API key (encrypted at rest).
    #[schema(value_type = String)]
    pub api_key: Secret,
}

impl Section for WrappedSettings {
    const KEY: &'static str = "wrapped_settings";
}

impl SecretSection for WrappedSettings {
    fn secret_fields(&mut self) -> Vec<SecretField<'_>> {
        vec![SecretField {
            value: &mut self.api_key,
            mask: WRAPPED_API_KEY_MASK,
            strip: true,
        }]
    }
}

// --- oidc_settings ----------------------------------------------------------

/// OIDC connection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(default)]
pub struct OidcConnection {
    /// Master switch.
    pub enabled: bool,
    /// Issuer URL.
    pub issuer: String,
    /// Client id.
    pub client_id: String,
    /// Client secret (encrypted at rest).
    #[schema(value_type = String)]
    pub client_secret: Secret,
    /// Scopes.
    pub scopes: String,
    /// Redirect URI.
    pub redirect_uri: String,
}

impl Default for OidcConnection {
    fn default() -> Self {
        Self {
            enabled: false,
            issuer: String::new(),
            client_id: String::new(),
            client_secret: Secret::default(),
            scopes: "openid email profile".to_owned(),
            redirect_uri: String::new(),
        }
    }
}

impl Section for OidcConnection {
    const KEY: &'static str = "oidc_settings";
}

impl SecretSection for OidcConnection {
    fn secret_fields(&mut self) -> Vec<SecretField<'_>> {
        vec![SecretField {
            value: &mut self.client_secret,
            mask: OIDC_SECRET_MASK,
            strip: false,
        }]
    }
}

// --- lastfm_settings ----------------------------------------------------------

/// Last.fm: the master switch plus the instance's app key pair (v2
/// parity). Users link their own Last.fm account with this pair; a user
/// may store their own pair instead, which wins for that user.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default, ToSchema)]
#[serde(default)]
pub struct LastFmSettings {
    /// Master switch for Last.fm linking and scrobbling.
    pub enabled: bool,
    /// The instance's Last.fm API key (encrypted at rest).
    #[schema(value_type = String)]
    pub api_key: Secret,
    /// The instance's Last.fm shared secret (encrypted at rest).
    #[schema(value_type = String)]
    pub shared_secret: Secret,
}

impl Section for LastFmSettings {
    const KEY: &'static str = "lastfm_settings";
}

impl SecretSection for LastFmSettings {
    fn secret_fields(&mut self) -> Vec<SecretField<'_>> {
        vec![
            SecretField {
                value: &mut self.api_key,
                mask: LASTFM_SECRET_MASK,
                strip: true,
            },
            SecretField {
                value: &mut self.shared_secret,
                mask: LASTFM_SECRET_MASK,
                strip: true,
            },
        ]
    }
}
