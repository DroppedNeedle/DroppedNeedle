//! Access and inbound apps: security posture and Connect Apps.

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use super::{AudioFormat, Section, check_range};
use crate::runtime_config::Live;
use crate::runtime_config::error::ConfigError;
use crate::runtime_config::origin::Origin;

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

impl DownloadAccess {
    /// Whether `role` may download library files.
    #[must_use]
    pub fn admits(self, role: &str) -> bool {
        match self {
            Self::Everyone => true,
            Self::Trusted => role == "admin" || role == "trusted",
            Self::Admin => role == "admin",
        }
    }
}

/// How the web UI's content-security policy is sent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ContentSecurityPolicyMode {
    /// The browser refuses what the policy does not allow.
    #[default]
    Enforce,
    /// The browser lists in its console what the policy would refuse and
    /// refuses nothing, except that who may frame the web UI and where its
    /// frames may go are still enforced.
    ReportOnly,
    /// No policy is added to the web UI or to other responses. The plugin
    /// panel page and the remote image relay keep their own policies in
    /// every mode.
    Off,
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
    /// How the web UI's content-security policy is sent.
    pub content_security_policy: ContentSecurityPolicyMode,
    /// Up to eight origins that may show DroppedNeedle in a frame, such as
    /// a dashboard at `https://home.example.com`. Empty: nobody may.
    /// Stored the way a browser sends `Origin`.
    pub frame_ancestors: Vec<String>,
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
            content_security_policy: ContentSecurityPolicyMode::default(),
            frame_ancestors: Vec::new(),
        }
    }
}

/// `origin` as a source in a content-security policy, or why it cannot be
/// one. A policy is a `;`-separated list of space-separated words, so a
/// source is built from the parsed origin and only from characters the
/// policy grammar gives a host (CSP Level 3, section 2.3.1, read
/// 2026-10-14: `host-char = ALPHA / DIGIT / "-"`, parts joined by `.`).
/// The URL parser lets `;`, `,` and `'` through in a host name; here they
/// are refused, and so is an IPv6 address, which the grammar has no form
/// for.
fn frame_source(origin: &Origin) -> Result<String, String> {
    let host = origin.host();
    if host.starts_with('[') {
        return Err(
            "is an IPv6 address, which a browser does not accept here: use a host name or \
             an IPv4 address"
                .to_owned(),
        );
    }
    let plain = !host.is_empty()
        && host.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'-' | b'.')
        });
    if !plain {
        return Err(
            "has a character in its host name that a browser does not accept here".to_owned(),
        );
    }
    Ok(origin.serialized())
}

/// The framing addresses an admin typed as policy sources, in the order
/// given and without duplicates. The error names the entry and why.
fn parse_frame_ancestors(entries: &[String]) -> Result<Vec<String>, String> {
    parse_origins(entries)?
        .iter()
        .map(|origin| {
            frame_source(origin).map_err(|reason| format!("{:?} {reason}", origin.serialized()))
        })
        .collect()
}

impl SecuritySettings {
    /// The stored framing addresses as policy sources. An entry that is not
    /// a plain origin is left out, and so is anything past the limit: a bad
    /// entry never widens who may frame the app.
    #[must_use]
    pub fn framing_sources(&self) -> Vec<String> {
        let mut sources: Vec<String> = Vec::new();
        for entry in &self.frame_ancestors {
            let Ok(source) = Origin::from_stated(entry).and_then(|origin| frame_source(&origin))
            else {
                continue;
            };
            if sources.len() < MAX_ORIGINS && !sources.contains(&source) {
                sources.push(source);
            }
        }
        sources
    }

    /// Whether `role` may download library files.
    #[must_use]
    pub fn download_allowed(&self, role: &str) -> bool {
        self.library_download_access.admits(role)
    }
}

/// Who may frame a page the app itself shows in a frame (the plugin panel
/// page), as the sources of that page's `frame-ancestors`: the app, plus
/// the addresses an admin allowed to frame the app. The browser checks
/// every ancestor up to the top, so a dashboard that frames the app has to
/// be named here too or the inner frame stays empty. Unreadable settings
/// leave the app alone.
pub fn framed_page_ancestors(settings: &Live<SecuritySettings>) -> String {
    let mut sources = vec!["'self'".to_owned()];
    match settings.get() {
        Ok(settings) => sources.extend(settings.framing_sources()),
        Err(error) => {
            tracing::debug!(%error, "security settings unreadable; framing by the app only")
        }
    }
    sources.join(" ")
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
        parse_frame_ancestors(&self.frame_ancestors)
            .map(drop)
            .map_err(|reason| ConfigError::Validation {
                section: Self::KEY,
                field: "frame_ancestors",
                reason,
            })
    }

    /// Rewrites each framing address the way a browser sends it and drops
    /// duplicates. In a file edited by hand, an entry that is not a plain
    /// origin and anything past the limit are dropped too.
    fn normalize(&mut self) {
        self.frame_ancestors = self.framing_sources();
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
/// `origins` has no default for the same reason: a save that misspells the
/// field is refused, where a default would store an empty list. A settings
/// file with no such section still reads as the empty list.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, ToSchema)]
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

/// Inbound Connect Apps config. Both protocols default off.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(default)]
pub struct ConnectApps {
    /// Serve the Subsonic API.
    pub subsonic_enabled: bool,
    /// Serve the Jellyfin API.
    pub jellyfin_enabled: bool,
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
}

impl Default for ConnectApps {
    fn default() -> Self {
        Self {
            subsonic_enabled: false,
            jellyfin_enabled: false,
            transcoding_enabled: true,
            transcode_default_format: AudioFormat::Mp3,
            transcode_max_bitrate_kbps: 320,
            advertise_server_name: "DroppedNeedle".to_owned(),
            advertise_server_version: "10.10.6".to_owned(),
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
