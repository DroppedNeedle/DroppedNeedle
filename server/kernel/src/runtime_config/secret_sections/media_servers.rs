//! Media server connections: Jellyfin, Navidrome, and Plex.

use super::*;

// --- jellyfin_settings (the section owns the URL; mask gap closed) ---------

/// Jellyfin connection. The section URL is the single owner (the top-level
/// mirror and the in-memory `Settings` mutation are gone).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(default)]
pub struct JellyfinConnection {
    /// Jellyfin base URL.
    pub jellyfin_url: String,
    /// API key (encrypted at rest; v2 returned it plaintext on GET).
    #[schema(value_type = String)]
    pub api_key: Secret,
    /// Jellyfin user id.
    pub user_id: String,
    /// Master switch.
    pub enabled: bool,
    /// Jellyfin login switch.
    pub login_enabled: bool,
}

impl Default for JellyfinConnection {
    fn default() -> Self {
        Self {
            jellyfin_url: "http://jellyfin:8096".to_owned(),
            api_key: Secret::default(),
            user_id: String::new(),
            enabled: false,
            login_enabled: false,
        }
    }
}

impl Section for JellyfinConnection {
    const KEY: &'static str = "jellyfin_settings";

    fn validate(&self) -> Result<(), ConfigError> {
        if !self.jellyfin_url.starts_with("http://") && !self.jellyfin_url.starts_with("https://") {
            return Err(ConfigError::Validation {
                section: Self::KEY,
                field: "jellyfin_url",
                reason: "jellyfin_url must start with http:// or https://".to_owned(),
            });
        }
        Ok(())
    }

    fn normalize(&mut self) {
        self.jellyfin_url = self.jellyfin_url.trim().to_owned();
        while self.jellyfin_url.ends_with('/') && self.jellyfin_url.len() > 1 {
            self.jellyfin_url.pop();
        }
    }
}

impl SecretSection for JellyfinConnection {
    fn secret_fields(&mut self) -> Vec<SecretField<'_>> {
        vec![SecretField {
            value: &mut self.api_key,
            mask: JELLYFIN_API_KEY_MASK,
            strip: false,
        }]
    }
}

// --- navidrome_settings -----------------------------------------------------

/// Navidrome connection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(default)]
pub struct NavidromeConnection {
    /// Navidrome base URL.
    pub navidrome_url: String,
    /// Username.
    pub username: String,
    /// Password (encrypted at rest).
    #[schema(value_type = String)]
    pub password: Secret,
    /// Master switch.
    pub enabled: bool,
    /// .m3u8 export switch (off by default).
    pub playlist_sync_enabled: bool,
    /// Export directory Navidrome scans.
    pub playlist_sync_path: String,
    /// Export scope (`public`, or opt-in `all`).
    pub playlist_sync_scope: String,
    /// Remove exports that stop qualifying.
    pub playlist_sync_remove_deleted: bool,
}

impl Default for NavidromeConnection {
    fn default() -> Self {
        Self {
            navidrome_url: String::new(),
            username: String::new(),
            password: Secret::default(),
            enabled: false,
            playlist_sync_enabled: false,
            playlist_sync_path: String::new(),
            playlist_sync_scope: "public".to_owned(),
            playlist_sync_remove_deleted: true,
        }
    }
}

impl Section for NavidromeConnection {
    const KEY: &'static str = "navidrome_settings";

    fn normalize(&mut self) {
        self.navidrome_url = self.navidrome_url.trim().to_owned();
        while self.navidrome_url.ends_with('/') && self.navidrome_url.len() > 1 {
            self.navidrome_url.pop();
        }
        self.playlist_sync_path = self.playlist_sync_path.trim().to_owned();
        if self.playlist_sync_scope != "all" {
            self.playlist_sync_scope = "public".to_owned();
        }
    }
}

impl SecretSection for NavidromeConnection {
    fn secret_fields(&mut self) -> Vec<SecretField<'_>> {
        vec![SecretField {
            value: &mut self.password,
            mask: NAVIDROME_PASSWORD_MASK,
            strip: false,
        }]
    }
}

// --- plex_settings ----------------------------------------------------------

/// Plex connection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(default)]
pub struct PlexConnection {
    /// Plex base URL.
    pub plex_url: String,
    /// Plex token (encrypted at rest).
    #[schema(value_type = String)]
    pub plex_token: Secret,
    /// Master switch.
    pub enabled: bool,
    /// Plex login switch.
    pub login_enabled: bool,
    /// Music library ids.
    pub music_library_ids: Vec<String>,
    /// Scrobble back to Plex.
    pub scrobble_to_plex: bool,
}

impl Default for PlexConnection {
    fn default() -> Self {
        Self {
            plex_url: String::new(),
            plex_token: Secret::default(),
            enabled: false,
            login_enabled: false,
            music_library_ids: Vec::new(),
            scrobble_to_plex: true,
        }
    }
}

impl Section for PlexConnection {
    const KEY: &'static str = "plex_settings";

    fn normalize(&mut self) {
        self.plex_url = self.plex_url.trim().to_owned();
        while self.plex_url.ends_with('/') && self.plex_url.len() > 1 {
            self.plex_url.pop();
        }
    }
}

impl SecretSection for PlexConnection {
    fn secret_fields(&mut self) -> Vec<SecretField<'_>> {
        vec![SecretField {
            value: &mut self.plex_token,
            mask: PLEX_TOKEN_MASK,
            strip: false,
        }]
    }
}
