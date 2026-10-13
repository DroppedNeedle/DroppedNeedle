//! Access and inbound apps: security posture and Connect Apps.

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use super::{AudioFormat, Section, check_range};
use crate::auth::session::origin::Origin;
use crate::runtime_config::error::ConfigError;

// --- security_settings ----------------------------------------------------

/// Who may download library files. `trusted` admits trusted and admin.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum DownloadAccess {
    /// Everyone.
    #[default]
    Everyone,
    /// Trusted and admin roles.
    Trusted,
    /// Admins only.
    Admin,
}

/// Security posture settings (no secrets; the HIBP path is a local file).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(default)]
pub struct SecuritySettings {
    /// Check passwords against Have-I-Been-Pwned.
    pub hibp_check: bool,
    /// Local HIBP file; used instead of the API when present.
    pub hibp_local_path: String,
    /// HSTS max-age in seconds; 0 disables.
    pub hsts_max_age: i64,
    /// HSTS include-subdomains flag.
    pub hsts_include_subdomains: bool,
    /// HSTS preload flag.
    pub hsts_preload: bool,
    /// Who may download library files.
    pub library_download_access: DownloadAccess,
}

impl Default for SecuritySettings {
    fn default() -> Self {
        Self {
            hibp_check: true,
            hibp_local_path: String::new(),
            hsts_max_age: 0,
            hsts_include_subdomains: false,
            hsts_preload: false,
            library_download_access: DownloadAccess::Everyone,
        }
    }
}

impl SecuritySettings {
    /// Whether `role` may download library files.
    #[must_use]
    pub fn download_allowed(&self, role: &str) -> bool {
        match self.library_download_access {
            DownloadAccess::Everyone => true,
            DownloadAccess::Trusted => role == "admin" || role == "trusted",
            DownloadAccess::Admin => role == "admin",
        }
    }
}

/// Longest HSTS max-age a save accepts: two years, in seconds. A typo
/// with extra digits would otherwise pin browsers to HTTPS for good.
pub const HSTS_MAX_AGE_LIMIT: i64 = 63_072_000;

impl Section for SecuritySettings {
    const KEY: &'static str = "security_settings";

    fn validate(&self) -> Result<(), ConfigError> {
        check_range(
            Self::KEY,
            "hsts_max_age",
            self.hsts_max_age,
            0,
            HSTS_MAX_AGE_LIMIT,
        )?;
        if self.hsts_preload && !self.hsts_include_subdomains {
            return Err(ConfigError::Validation {
                section: Self::KEY,
                field: "hsts_preload",
                reason: "needs hsts_include_subdomains".to_owned(),
            });
        }
        Ok(())
    }
}

// --- public_address -------------------------------------------------------

/// Most addresses one list of origins may hold.
pub const MAX_ORIGINS: usize = 8;

/// Parse a list of web addresses an admin typed into distinct origins, in
/// the order given. The error names the entry at fault and why.
pub fn parse_origins(entries: &[String]) -> Result<Vec<Origin>, String> {
    let mut origins = Vec::new();
    for entry in entries {
        let origin = Origin::from_stated(entry).map_err(|reason| format!("{entry:?} {reason}"))?;
        if origins.contains(&origin) {
            continue;
        }
        if origins.len() == MAX_ORIGINS {
            return Err(format!("holds more than {MAX_ORIGINS} addresses"));
        }
        origins.push(origin);
    }
    Ok(origins)
}

/// The addresses at which this DroppedNeedle is served, for a deployment
/// whose reverse proxy leaves the server with the wrong idea of its own
/// address. The session origin check accepts a change from a page at one of
/// them, on top of the address it works out for itself.
///
/// A section of its own, so that saving another one can never empty it.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, ToSchema)]
#[serde(default)]
pub struct PublicAddress {
    /// Up to eight origins such as `https://music.example.com` or
    /// `http://192.168.1.5:8688`: scheme, host and optional port, no path.
    /// Stored the way a browser sends `Origin`.
    pub origins: Vec<String>,
}

impl PublicAddress {
    /// The stored addresses as origins. An entry that does not parse is
    /// left out: a bad entry never widens the check.
    #[must_use]
    pub fn stated(&self) -> Vec<Origin> {
        self.origins
            .iter()
            .filter_map(|entry| Origin::from_stated(entry).ok())
            .collect()
    }
}

impl Section for PublicAddress {
    const KEY: &'static str = "public_address";

    fn validate(&self) -> Result<(), ConfigError> {
        parse_origins(&self.origins)
            .map(drop)
            .map_err(|reason| ConfigError::Validation {
                section: Self::KEY,
                field: "origins",
                reason,
            })
    }

    /// Rewrites each address the way a browser sends it and drops
    /// duplicates. In a file edited by hand, an entry that is not an
    /// address and anything past the limit are dropped too.
    fn normalize(&mut self) {
        let mut origins: Vec<Origin> = Vec::new();
        for origin in self.stated() {
            if origins.len() < MAX_ORIGINS && !origins.contains(&origin) {
                origins.push(origin);
            }
        }
        self.origins = origins.iter().map(Origin::serialized).collect();
    }
}

// --- connect_apps ---------------------------------------------------------

/// Compat discovery mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, ToSchema)]
pub enum DiscoverMode {
    /// Local library only.
    #[serde(rename = "local-only")]
    #[default]
    LocalOnly,
    /// Lazy MusicBrainz enrichment.
    #[serde(rename = "lazy-mb")]
    LazyMb,
    /// Use scrobble targets.
    #[serde(rename = "use-scrobble-targets")]
    UseScrobbleTargets,
}

/// Inbound Connect Apps config. Both protocols default off.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(default)]
pub struct ConnectApps {
    /// Serve the Subsonic API.
    pub subsonic_enabled: bool,
    /// Serve the Jellyfin API.
    pub jellyfin_enabled: bool,
    /// Capability flag for the approval-safe exact-track endpoint.
    pub exact_track_approval_supported: bool,
    /// Allow transcoding for compat clients.
    pub transcoding_enabled: bool,
    /// Default transcode format.
    pub transcode_default_format: AudioFormat,
    /// Transcode ceiling in kbps (32-1411).
    pub transcode_max_bitrate_kbps: i64,
    /// Advertised server name.
    pub advertise_server_name: String,
    /// Advertised server version.
    pub advertise_server_version: String,
    /// Compat discovery mode.
    pub discover_mode: DiscoverMode,
}

impl Default for ConnectApps {
    fn default() -> Self {
        Self {
            subsonic_enabled: false,
            jellyfin_enabled: false,
            exact_track_approval_supported: true,
            transcoding_enabled: true,
            transcode_default_format: AudioFormat::Mp3,
            transcode_max_bitrate_kbps: 320,
            advertise_server_name: "DroppedNeedle".to_owned(),
            advertise_server_version: "10.10.6".to_owned(),
            discover_mode: DiscoverMode::LocalOnly,
        }
    }
}

impl Section for ConnectApps {
    const KEY: &'static str = "connect_apps";

    fn validate(&self) -> Result<(), ConfigError> {
        check_range(
            Self::KEY,
            "transcode_max_bitrate_kbps",
            self.transcode_max_bitrate_kbps,
            32,
            1411,
        )?;
        if self.transcode_default_format == AudioFormat::Flac {
            return Err(ConfigError::Validation {
                section: Self::KEY,
                field: "transcode_default_format",
                reason: "must be mp3 or opus".to_owned(),
            });
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn section(origins: &[&str]) -> PublicAddress {
        PublicAddress {
            origins: origins.iter().map(|&origin| origin.to_owned()).collect(),
        }
    }

    /// What a save stores: `validate`, then `normalize`.
    fn saved(origins: &[&str]) -> Result<Vec<String>, String> {
        let mut section = section(origins);
        section.validate().map_err(|error| error.to_string())?;
        section.normalize();
        Ok(section.origins)
    }

    #[test]
    fn public_address_keeps_only_origins_as_a_browser_sends_them() {
        assert_eq!(saved(&[]), Ok(vec![]));
        assert_eq!(
            saved(&[
                "HTTPS://Music.Example.com/",
                "http://192.168.1.5:8688",
                "https://music.example.com:443",
                "http://[::1]:80",
            ]),
            Ok(vec![
                "https://music.example.com".to_owned(),
                "http://192.168.1.5:8688".to_owned(),
                "http://[::1]".to_owned(),
            ])
        );

        // One bad entry refuses the whole save and is named with the reason.
        let refused = saved(&["https://music.example.com", "https://example.com/music"])
            .expect_err("a path is refused");
        assert!(
            refused.contains("public_address.origins")
                && refused.contains("\"https://example.com/music\"")
                && refused.contains("base path"),
            "{refused}"
        );
        for bad in ["music.example.com", "https://*.example.com", "ftp://a.test"] {
            assert!(saved(&[bad]).is_err(), "{bad}");
        }

        let ports: Vec<String> = (1..=9).map(|n| format!("http://music.lan:{n}")).collect();
        let ports: Vec<&str> = ports.iter().map(String::as_str).collect();
        assert_eq!(saved(&ports[..8]).map(|kept| kept.len()), Ok(8));
        assert!(saved(&ports).is_err(), "nine addresses");
        // The same address nine times over is one address.
        assert_eq!(
            saved(&["http://music.lan"; 9]).map(|kept| kept.len()),
            Ok(1)
        );

        // A file edited by hand is never trusted further than a save: what
        // is not an address is dropped, and so is anything past the limit.
        let mut edited = section(&["https://*.example.com", "null", "http://music.lan:8080/x"]);
        edited
            .origins
            .extend(ports.iter().map(|&port| port.to_owned()));
        edited.normalize();
        assert_eq!(edited.origins, ports[..8]);
        assert_eq!(edited.stated().len(), 8);
    }
}
