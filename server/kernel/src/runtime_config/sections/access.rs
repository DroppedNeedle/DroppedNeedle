//! Access and inbound apps: security posture, Connect Apps and app connect.

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
                reason: "needs Include Subdomains on".to_owned(),
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

// --- app_connect -----------------------------------------------------------

/// Most apps one server lists for app connect.
pub const MAX_APP_CONNECT_CLIENTS: usize = 8;
/// Most return addresses one app lists.
pub const MAX_APP_CONNECT_REDIRECTS: usize = 4;
/// Longest app name, in characters.
pub const MAX_APP_CONNECT_NAME_LEN: usize = 48;

/// One native app that may ask for an app password through the browser.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, ToSchema)]
pub struct AppConnectClient {
    /// What the app sends as `client_id`: 1-64 of `a-z`, `0-9`, `.`, `_`
    /// and `-`, starting with a letter or digit.
    pub client_id: String,
    /// The name the confirmation page shows and the app password is
    /// named after, such as `My Player`. Never taken from the request.
    pub name: String,
    /// The addresses the browser may hand the result to, matched exactly:
    /// a private-use scheme named after a domain the app's maker owns
    /// (`com.example.player:/connect`, RFC 8252 section 7.1) or an `https`
    /// address. Up to four.
    pub redirect_uris: Vec<String>,
}

/// Native app sign-in ("app connect"): the apps that may get an app
/// password for the signed-in user after the user confirms it in the
/// browser (OAuth 2.0 authorization code with PKCE, RFC 6749/7636/8252).
/// Off while the list is empty, which is the default.
///
/// A section of its own, so that saving another one can never empty it.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, ToSchema)]
pub struct AppConnect {
    /// Up to eight apps.
    pub clients: Vec<AppConnectClient>,
}

impl AppConnect {
    /// The app listed as `client_id` when `redirect_uri` is exactly one of
    /// its addresses. No prefix or pattern ever matches.
    #[must_use]
    pub fn client(&self, client_id: &str, redirect_uri: &str) -> Option<&AppConnectClient> {
        self.clients.iter().find(|client| {
            client.client_id == client_id
                && client
                    .redirect_uris
                    .iter()
                    .any(|listed| listed == redirect_uri)
        })
    }

    /// Whether any app is listed.
    #[must_use]
    pub fn enabled(&self) -> bool {
        !self.clients.is_empty()
    }
}

fn app_connect_error(field: &'static str, reason: String) -> ConfigError {
    ConfigError::Validation {
        section: AppConnect::KEY,
        field,
        reason,
    }
}

/// Why `uri` cannot be an app's return address, if it cannot. It must be
/// written the way it is matched (the parser's own spelling), so a request
/// that spells it differently is refused rather than half-matched.
fn redirect_problem(uri: &str) -> Option<&'static str> {
    let Ok(parsed) = reqwest::Url::parse(uri) else {
        return Some("is not an address");
    };
    if parsed.as_str() != uri {
        return Some("is not written the way it is matched; write it in its plain form");
    }
    if parsed.fragment().is_some() || !parsed.username().is_empty() || parsed.password().is_some() {
        return Some("may not carry a fragment or a user name");
    }
    match parsed.scheme() {
        "https" => None,
        "http" => Some("must use https or a private-use scheme"),
        scheme if scheme.contains('.') => None,
        _ => Some(
            "must use https or a private-use scheme named after a domain, such as com.example.app",
        ),
    }
}

impl Section for AppConnect {
    const KEY: &'static str = "app_connect";

    fn validate(&self) -> Result<(), ConfigError> {
        if self.clients.len() > MAX_APP_CONNECT_CLIENTS {
            return Err(app_connect_error(
                "clients",
                format!("holds more than {MAX_APP_CONNECT_CLIENTS} apps"),
            ));
        }
        let mut seen: Vec<&str> = Vec::new();
        for client in &self.clients {
            let id = client.client_id.as_str();
            let id_ok = (1..=64).contains(&id.len())
                && id
                    .bytes()
                    .next()
                    .is_some_and(|first| first.is_ascii_lowercase() || first.is_ascii_digit())
                && id.bytes().all(|byte| {
                    byte.is_ascii_lowercase() || byte.is_ascii_digit() || b"._-".contains(&byte)
                });
            if !id_ok {
                return Err(app_connect_error(
                    "client_id",
                    format!("{id:?} must be 1-64 of a-z, 0-9, '.', '_' and '-'"),
                ));
            }
            if seen.contains(&id) {
                return Err(app_connect_error(
                    "client_id",
                    format!("{id:?} is listed twice"),
                ));
            }
            seen.push(id);
            let name = client.name.trim();
            if name.is_empty()
                || name.chars().count() > MAX_APP_CONNECT_NAME_LEN
                || name.chars().any(char::is_control)
                || name != client.name
            {
                return Err(app_connect_error(
                    "name",
                    format!(
                        "of {id:?} must be 1-{MAX_APP_CONNECT_NAME_LEN} characters with no \
                         control characters and no surrounding spaces"
                    ),
                ));
            }
            if client.redirect_uris.is_empty()
                || client.redirect_uris.len() > MAX_APP_CONNECT_REDIRECTS
            {
                return Err(app_connect_error(
                    "redirect_uris",
                    format!("of {id:?} must hold 1-{MAX_APP_CONNECT_REDIRECTS} addresses"),
                ));
            }
            for uri in &client.redirect_uris {
                if let Some(problem) = redirect_problem(uri) {
                    return Err(app_connect_error(
                        "redirect_uris",
                        format!("{uri:?} of {id:?} {problem}"),
                    ));
                }
            }
        }
        Ok(())
    }

    /// A file edited by hand is never trusted further than a save: an app
    /// that would not pass `validate` is dropped, and so is anything past
    /// the limit, so a bad entry never lets an app in.
    fn normalize(&mut self) {
        let mut kept: Vec<AppConnectClient> = Vec::new();
        for client in std::mem::take(&mut self.clients) {
            if kept.len() == MAX_APP_CONNECT_CLIENTS
                || kept.iter().any(|other| other.client_id == client.client_id)
            {
                continue;
            }
            let alone = AppConnect {
                clients: vec![client],
            };
            if alone.validate().is_ok() {
                kept.extend(alone.clients);
            }
        }
        self.clients = kept;
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

    fn app(client_id: &str, name: &str, redirects: &[&str]) -> AppConnectClient {
        AppConnectClient {
            client_id: client_id.to_owned(),
            name: name.to_owned(),
            redirect_uris: redirects.iter().map(|&uri| uri.to_owned()).collect(),
        }
    }

    #[test]
    fn app_connect_matches_listed_apps_exactly() {
        let section = AppConnect {
            clients: vec![app(
                "my-player",
                "My Player",
                &["com.example.player:/connect"],
            )],
        };
        assert!(section.validate().is_ok());
        assert!(section.enabled());
        assert!(AppConnect::default().validate().is_ok());
        assert!(!AppConnect::default().enabled());
        let found = section.client("my-player", "com.example.player:/connect");
        assert_eq!(found.map(|client| client.name.as_str()), Some("My Player"));
        for (client_id, redirect) in [
            ("my-player", "com.example.player:/connect/"),
            ("my-player", "com.example.player:/connec"),
            ("my-player", "com.example.player:/connect?x=1"),
            ("my-player", "COM.example.player:/connect"),
            ("my-player", ""),
            ("My-player", "com.example.player:/connect"),
            ("other", "com.example.player:/connect"),
        ] {
            assert!(
                section.client(client_id, redirect).is_none(),
                "{client_id} {redirect}"
            );
        }
    }

    #[test]
    fn app_connect_refuses_what_could_widen_it() {
        let refused = |clients: Vec<AppConnectClient>| AppConnect { clients }.validate().is_err();
        let ok = &["com.example.player:/connect"];
        assert!(!refused(vec![app(
            "a",
            "A",
            &["https://app.example.com/back"]
        )]));
        for uri in [
            "http://app.example.com/back",
            "javascript:alert(1)",
            "myapp:/connect",
            "com.example.player:/connect#x",
            "https://user@app.example.com/",
            "HTTPS://app.example.com/back",
            "not an address",
        ] {
            assert!(refused(vec![app("a", "A", &[uri])]), "{uri}");
        }
        assert!(refused(vec![app("a", "A", &[])]));
        assert!(refused(vec![app("a", "A", &[ok[0]; 5])]));
        for id in ["", "-a", "A", "a b", "a/b", &"a".repeat(65)] {
            assert!(refused(vec![app(id, "A", ok)]), "{id:?}");
        }
        for name in ["", " A", "A\u{7}", &"n".repeat(49)] {
            assert!(refused(vec![app("a", name, ok)]), "{name:?}");
        }
        assert!(refused(vec![app("a", "A", ok), app("a", "B", ok)]));
        let nine: Vec<AppConnectClient> = (0..9).map(|n| app(&format!("a{n}"), "A", ok)).collect();
        assert!(refused(nine.clone()));

        // A file edited by hand keeps only the apps a save would accept.
        let mut edited = AppConnect {
            clients: vec![
                app("good", "Good", ok),
                app("bad", "Bad", &["http://evil.example/"]),
                app("good", "Twice", ok),
            ],
        };
        edited.clients.extend(nine);
        edited.normalize();
        let ids: Vec<&str> = edited
            .clients
            .iter()
            .map(|c| c.client_id.as_str())
            .collect();
        assert_eq!(ids, ["good", "a0", "a1", "a2", "a3", "a4", "a5", "a6"]);
    }
}
