//! Connection probes behind the verify endpoints.
//!
//! Every verify endpoint tests the submitted values, not the stored
//! config, so Test works before the first save and reflects edits. A
//! masked secret resolves to the stored one in the handler; these
//! probes only ever see concrete values. Verdicts carry the
//! reachable/bad-credential distinction in the body, never as a leaked
//! 5xx, and never echo the URL or host.
//!
//! Probes reuse the production clients (media adapters, slskd and
//! Usenet clients, the ListenBrainz verifier), so a passing verify
//! means the same code path the app uses can reach the upstream. The
//! [`VerifyProbes`] port keeps tests hermetic: HTTP tests run against
//! scripted fakes while the live probes prove themselves against
//! loopback stubs with explicit base URLs.

use std::time::Duration;

use futures_util::future::BoxFuture;

use super::error::SettingsError;

/// Per-probe HTTP budget, matching the v2 repositories.
const PROBE_TIMEOUT: Duration = Duration::from_secs(10);

/// YouTube Data API root.
pub const YOUTUBE_BASE_URL: &str = "https://www.googleapis.com";
/// Ticketmaster Discovery API root.
pub const TICKETMASTER_BASE_URL: &str = "https://app.ticketmaster.com/discovery/v2";
/// Skiddle events API root.
pub const SKIDDLE_BASE_URL: &str = "https://www.skiddle.com/api/v1";

/// Plain connection-test verdict.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProbeVerdict {
    /// Whether the submitted values checked out.
    pub valid: bool,
    /// Human summary.
    pub message: String,
}

impl ProbeVerdict {
    /// Passing verdict.
    pub fn ok(message: impl Into<String>) -> Self {
        Self {
            valid: true,
            message: message.into(),
        }
    }

    /// Failing verdict.
    pub fn failed(message: impl Into<String>) -> Self {
        Self {
            valid: false,
            message: message.into(),
        }
    }
}

/// Jellyfin verdict, with the user list on success (for the admin
/// user picker).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JellyfinVerdict {
    /// Whether the submitted values checked out.
    pub valid: bool,
    /// Human summary.
    pub message: String,
    /// Server users as `(id, name)` (empty unless the probe passed).
    pub users: Vec<(String, String)>,
}

/// Plex verdict, with music libraries on success.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlexVerdict {
    /// Whether the submitted values checked out.
    pub valid: bool,
    /// Human summary.
    pub message: String,
    /// Music libraries as `(key, title)` (empty unless the probe passed).
    pub libraries: Vec<(String, String)>,
}

/// ListenBrainz verdict. Rate limiting is its own flag because the
/// route answers 429 there (v2 parity), not a body verdict.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListenBrainzVerdict {
    /// Whether the submitted values checked out.
    pub valid: bool,
    /// Human summary.
    pub message: String,
    /// The upstream is rate-limiting; the caller answers 429.
    pub rate_limited: bool,
}

/// slskd verdict, with the reported server version.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VersionVerdict {
    /// Whether the submitted values checked out.
    pub valid: bool,
    /// Server version, when known.
    pub version: Option<String>,
    /// Human summary.
    pub message: String,
}

/// SABnzbd verdict: version plus the category picker, the SABnzbd-side
/// completed dir, and the mount diagnosis over the submitted mount.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SabnzbdVerdict {
    /// Whether the submitted values checked out.
    pub valid: bool,
    /// Server version, when known.
    pub version: Option<String>,
    /// Human summary.
    pub message: String,
    /// Known categories.
    pub categories: Vec<String>,
    /// SABnzbd-side completed dir, when known.
    pub complete_dir: Option<String>,
    /// Mount diagnosis over the submitted mount, when diagnosed.
    pub diagnosis: Option<SabnzbdMountDiagnosis>,
}

/// Mount cross-check over the submitted downloads mount.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SabnzbdMountDiagnosis {
    /// Whether the mount holds any file at all.
    pub mount_has_files: bool,
    /// Sampled completions locatable under the mount.
    pub resolvable_downloads: i64,
    /// Sample size.
    pub sampled_downloads: i64,
    /// Actionable guidance, when the mount looks wrong.
    pub mount_message: Option<String>,
}

/// Prowlarr verdict, with the server version and member count.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProwlarrVerdict {
    /// Whether the submitted values checked out.
    pub valid: bool,
    /// Server version, when known.
    pub version: Option<String>,
    /// Human summary.
    pub message: String,
    /// Enabled member indexers, when listed.
    pub indexer_count: Option<i64>,
}

/// Newznab caps verdict, with the audio-search flag and the homepage
/// suggestion when the URL was the site root.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewznabVerdict {
    /// Whether the submitted values checked out.
    pub valid: bool,
    /// Server version, when known.
    pub version: Option<String>,
    /// Human summary.
    pub message: String,
    /// Whether structured music search is advertised.
    pub supports_audio_search: bool,
    /// Advertised category count.
    pub category_count: i64,
    /// One-click `/api` fix, when the submitted URL was the homepage.
    pub suggested_url: Option<String>,
}

/// Connection probes. Every method tests explicitly passed values;
/// fixed-host upstreams take their base URL so tests point them at
/// loopback stubs while production passes the module constants. All
/// failures are verdicts, never errors: only the Plex library listing
/// (a GET over stored config) fails the request.
pub trait VerifyProbes: Send + Sync {
    /// Jellyfin `/System/Info` plus the user list.
    fn jellyfin<'a>(&'a self, url: &'a str, key: &'a str) -> BoxFuture<'a, JellyfinVerdict>;
    /// Navidrome Subsonic `ping`.
    fn navidrome<'a>(
        &'a self,
        url: &'a str,
        username: &'a str,
        password: &'a str,
    ) -> BoxFuture<'a, ProbeVerdict>;
    /// Plex `/` plus the music-library sections.
    fn plex<'a>(&'a self, url: &'a str, token: &'a str) -> BoxFuture<'a, PlexVerdict>;
    /// Plex music-library sections over stored config.
    fn plex_libraries<'a>(
        &'a self,
        url: &'a str,
        token: &'a str,
    ) -> BoxFuture<'a, Result<Vec<(String, String)>, String>>;
    /// ListenBrainz token (or username-only) check.
    fn listenbrainz<'a>(
        &'a self,
        base: &'a str,
        username: &'a str,
        token: &'a str,
    ) -> BoxFuture<'a, ListenBrainzVerdict>;
    /// YouTube Data API key check.
    fn youtube<'a>(&'a self, base: &'a str, key: &'a str) -> BoxFuture<'a, ProbeVerdict>;
    /// Ticketmaster Discovery key check.
    fn ticketmaster<'a>(&'a self, base: &'a str, key: &'a str) -> BoxFuture<'a, ProbeVerdict>;
    /// Skiddle events key check.
    fn skiddle<'a>(&'a self, base: &'a str, key: &'a str) -> BoxFuture<'a, ProbeVerdict>;
    /// slskd session health.
    fn slskd<'a>(&'a self, url: &'a str, key: &'a str) -> BoxFuture<'a, VersionVerdict>;
    /// SABnzbd version plus categories, completed dir, and the mount
    /// diagnosis over the submitted mount.
    fn sabnzbd<'a>(
        &'a self,
        url: &'a str,
        key: &'a str,
        downloads_mount: &'a str,
    ) -> BoxFuture<'a, SabnzbdVerdict>;
    /// Prowlarr system status plus the member-indexer list.
    fn prowlarr<'a>(&'a self, url: &'a str, key: &'a str) -> BoxFuture<'a, ProwlarrVerdict>;
    /// Newznab `t=caps`.
    fn newznab<'a>(&'a self, url: &'a str, key: &'a str) -> BoxFuture<'a, NewznabVerdict>;
    /// MusicBrainz-compatible artist lookup (official, mirror,
    /// community, or the pinned BrainzMash endpoint).
    fn musicbrainz<'a>(&'a self, api_url: &'a str) -> BoxFuture<'a, ProbeVerdict>;
    /// OIDC issuer discovery document.
    fn oidc<'a>(&'a self, issuer: &'a str) -> BoxFuture<'a, ProbeVerdict>;
}

/// Live probes over one shared HTTP client.
pub struct LiveProbes {
    /// Outbound client.
    pub http: reqwest::Client,
}

impl LiveProbes {
    /// Build over the shared client.
    pub fn new(http: reqwest::Client) -> Self {
        Self { http }
    }
}

/// Require a usable service URL: non-blank, http(s), trailing slash
/// stripped. Anything else is a 400 naming the field.
pub fn require_service_url(url: &str, label: &str) -> Result<String, SettingsError> {
    let trimmed = url.trim();
    if trimmed.is_empty() {
        return Err(SettingsError::InvalidInput {
            message: format!("{label} is required."),
        });
    }
    if !(trimmed.starts_with("http://") || trimmed.starts_with("https://")) {
        return Err(SettingsError::InvalidInput {
            message: format!("{label} must be an http(s) URL."),
        });
    }
    Ok(trimmed.trim_end_matches('/').to_owned())
}

/// Collapse a transport error to one line. URLs and hosts never ride
/// in probe errors: reqwest's display can embed them, so only the
/// error kind is ever rendered.
fn transport_kind(error: &reqwest::Error) -> &'static str {
    if error.is_timeout() {
        "timed out"
    } else if error.is_connect() {
        "connection refused"
    } else if error.is_body() || error.is_decode() {
        "unreadable response"
    } else {
        "request failed"
    }
}

/// Map a media-adapter failure to a v2-shaped verdict message. The URL
/// is never echoed.
fn adapter_message(
    error: &crate::remotes::adapter::AdapterError,
    unconfigured: &str,
    auth: &str,
) -> String {
    use crate::remotes::adapter::AdapterError as Source;
    match error {
        Source::NotConfigured => unconfigured.to_owned(),
        Source::Auth => auth.to_owned(),
        Source::Api(detail) => format!("Connection failed: {detail}"),
        Source::NotFound => "Connection failed: the server has no such item.".to_owned(),
        Source::Unsupported(detail) => format!("Connection failed: {detail}"),
        Source::Transport(detail) => {
            // Classified, never rendered: the adapter detail embeds the
            // request URL, which must not ride in a verdict message.
            let lowered = detail.to_lowercase();
            if lowered.contains("timed out") || lowered.contains("timeout") {
                "Connection timed out - check URL.".to_owned()
            } else {
                "Could not connect - check URL and ensure server is running.".to_owned()
            }
        }
    }
}

impl VerifyProbes for LiveProbes {
    fn jellyfin<'a>(&'a self, url: &'a str, key: &'a str) -> BoxFuture<'a, JellyfinVerdict> {
        Box::pin(async move {
            use crate::remotes::jellyfin::JellyfinAdapter;
            if url.trim().is_empty() || key.is_empty() {
                return JellyfinVerdict {
                    valid: false,
                    message: "Jellyfin URL or API key not configured.".to_owned(),
                    users: Vec::new(),
                };
            }
            let adapter = JellyfinAdapter::new(
                self.http.clone(),
                url.to_owned(),
                key.to_owned(),
                String::new(),
            );
            let message = match adapter.validate_connection().await {
                Ok(message) => message,
                Err(error) => {
                    return JellyfinVerdict {
                        valid: false,
                        message: adapter_message(
                            &error,
                            "Jellyfin URL or API key not configured.",
                            "Authentication failed - check API key.",
                        ),
                        users: Vec::new(),
                    };
                }
            };
            // The user list fails open: the connection verdict stands.
            let users = jellyfin_users(&self.http, url, key).await;
            JellyfinVerdict {
                valid: true,
                message,
                users,
            }
        })
    }

    fn navidrome<'a>(
        &'a self,
        url: &'a str,
        username: &'a str,
        password: &'a str,
    ) -> BoxFuture<'a, ProbeVerdict> {
        Box::pin(async move {
            use crate::remotes::navidrome::NavidromeAdapter;
            let adapter = NavidromeAdapter::new(
                self.http.clone(),
                url.to_owned(),
                username.to_owned(),
                password.to_owned(),
            );
            match adapter.validate_connection().await {
                Ok(message) => ProbeVerdict::ok(message),
                Err(error) => ProbeVerdict::failed(adapter_message(
                    &error,
                    "Navidrome URL, username, or password not configured.",
                    "Authentication failed - check username and password.",
                )),
            }
        })
    }

    fn plex<'a>(&'a self, url: &'a str, token: &'a str) -> BoxFuture<'a, PlexVerdict> {
        Box::pin(async move {
            use crate::remotes::plex::PlexAdapter;
            let adapter = PlexAdapter::new(
                self.http.clone(),
                url.to_owned(),
                token.to_owned(),
                String::new(),
                Vec::new(),
            );
            let message = match adapter.validate_connection().await {
                Ok(message) => message,
                Err(error) => {
                    return PlexVerdict {
                        valid: false,
                        message: adapter_message(
                            &error,
                            "Plex URL or token not configured.",
                            "Authentication failed - check your Plex token.",
                        ),
                        libraries: Vec::new(),
                    };
                }
            };
            // The library list fails open: the connection verdict stands.
            let libraries = adapter.music_libraries().await.unwrap_or_default();
            PlexVerdict {
                valid: true,
                message,
                libraries,
            }
        })
    }

    fn plex_libraries<'a>(
        &'a self,
        url: &'a str,
        token: &'a str,
    ) -> BoxFuture<'a, Result<Vec<(String, String)>, String>> {
        Box::pin(async move {
            use crate::remotes::plex::PlexAdapter;
            if url.trim().is_empty() || token.is_empty() {
                return Err("Plex is not configured.".to_owned());
            }
            let adapter = PlexAdapter::new(
                self.http.clone(),
                url.to_owned(),
                token.to_owned(),
                String::new(),
                Vec::new(),
            );
            adapter
                .music_libraries()
                .await
                .map_err(|_| "Could not fetch libraries from Plex.".to_owned())
        })
    }

    fn listenbrainz<'a>(
        &'a self,
        base: &'a str,
        username: &'a str,
        token: &'a str,
    ) -> BoxFuture<'a, ListenBrainzVerdict> {
        Box::pin(async move {
            use crate::plugins::scrobble::{HttpListenBrainzVerifier, ListenBrainzVerifier as _};
            let verifier = HttpListenBrainzVerifier::new(self.http.clone(), base);
            let outcome = verifier.verify(username, token).await;
            ListenBrainzVerdict {
                valid: outcome.valid,
                message: outcome.message,
                rate_limited: outcome.rate_limited,
            }
        })
    }

    fn youtube<'a>(&'a self, base: &'a str, key: &'a str) -> BoxFuture<'a, ProbeVerdict> {
        Box::pin(async move {
            if key.trim().is_empty() {
                return ProbeVerdict::failed("An API key is required.");
            }
            let url = format!("{}/youtube/v3/videos", base.trim_end_matches('/'));
            let response = self
                .http
                .get(&url)
                .query(&[("part", "id"), ("id", "dQw4w9WgXcQ"), ("key", key.trim())])
                .timeout(PROBE_TIMEOUT)
                .send()
                .await;
            match response {
                Ok(response) if response.status().is_success() => {
                    ProbeVerdict::ok("YouTube API key is valid.")
                }
                Ok(response) if response.status() == reqwest::StatusCode::FORBIDDEN => {
                    ProbeVerdict::failed("API key is invalid or YouTube Data API is not enabled.")
                }
                Ok(response) => ProbeVerdict::failed(format!(
                    "Unexpected response: {}.",
                    response.status().as_u16()
                )),
                Err(error) => {
                    ProbeVerdict::failed(format!("Connection error: {}.", transport_kind(&error)))
                }
            }
        })
    }

    fn ticketmaster<'a>(&'a self, base: &'a str, key: &'a str) -> BoxFuture<'a, ProbeVerdict> {
        Box::pin(async move {
            if key.trim().is_empty() {
                return ProbeVerdict::failed("An API key is required.");
            }
            let url = format!("{}/attractions.json", base.trim_end_matches('/'));
            let reached = self
                .http
                .get(&url)
                .query(&[("keyword", "test"), ("size", "1"), ("apikey", key.trim())])
                .timeout(PROBE_TIMEOUT)
                .send()
                .await
                .is_ok_and(|response| response.status().is_success());
            if reached {
                ProbeVerdict::ok("Connected to Ticketmaster.")
            } else {
                ProbeVerdict::failed("Ticketmaster rejected the key or is unreachable.")
            }
        })
    }

    fn skiddle<'a>(&'a self, base: &'a str, key: &'a str) -> BoxFuture<'a, ProbeVerdict> {
        Box::pin(async move {
            if key.trim().is_empty() {
                return ProbeVerdict::failed("An API key is required.");
            }
            let url = format!("{}/events/search/", base.trim_end_matches('/'));
            let response = self
                .http
                .get(&url)
                .query(&[("limit", "1"), ("api_key", key.trim())])
                .timeout(PROBE_TIMEOUT)
                .send()
                .await;
            let ok = match response {
                Ok(response) => {
                    if !response.status().is_success() {
                        false
                    } else {
                        decode_body(response)
                            .await
                            .and_then(|body| body.get("error").and_then(|error| error.as_i64()))
                            == Some(0)
                    }
                }
                Err(_) => false,
            };
            if ok {
                ProbeVerdict::ok("Connected to Skiddle.")
            } else {
                ProbeVerdict::failed("Skiddle rejected the key or is unreachable.")
            }
        })
    }

    fn slskd<'a>(&'a self, url: &'a str, key: &'a str) -> BoxFuture<'a, VersionVerdict> {
        Box::pin(async move {
            use crate::acquire::slskd::{
                ReqwestSlskdHttp, SlskdClient, policy::DownloadPolicy as SlskdPolicy,
                repository::SlskdRepository,
            };
            // Paste whitespace is stripped before probing (the mask
            // itself is strip-identity) so a pasted key tests as typed.
            let key = key.trim();
            if url.trim().is_empty() || key.is_empty() {
                return VersionVerdict {
                    valid: false,
                    version: None,
                    message: "Download client URL or API key not configured.".to_owned(),
                };
            }
            let repo = SlskdRepository::new(
                SlskdClient::new(ReqwestSlskdHttp::new(self.http.clone(), url, key)),
                url,
                key,
                std::path::PathBuf::new(),
                SlskdPolicy::default(),
            );
            let status = repo.health_check().await;
            VersionVerdict {
                valid: status.ok,
                version: status.version,
                message: status.message,
            }
        })
    }

    fn sabnzbd<'a>(
        &'a self,
        url: &'a str,
        key: &'a str,
        downloads_mount: &'a str,
    ) -> BoxFuture<'a, SabnzbdVerdict> {
        Box::pin(async move {
            use crate::acquire::usenet::{
                policy::UsenetPolicy,
                sabnzbd::{SabnzbdClient, SabnzbdQueue},
            };
            if url.trim().is_empty() || key.is_empty() {
                return SabnzbdVerdict {
                    valid: false,
                    version: None,
                    message: "SABnzbd URL or API key not configured.".to_owned(),
                    categories: Vec::new(),
                    complete_dir: None,
                    diagnosis: None,
                };
            }
            let policy = UsenetPolicy {
                poll_timeout: PROBE_TIMEOUT,
                ..UsenetPolicy::default()
            };
            // Two clients over the one HTTP handle: the queue owns its
            // clone for health + diagnosis, the spare serves the picker.
            let cats_client =
                SabnzbdClient::new(self.http.clone(), url, key, 1, Duration::from_secs(1));
            let queue = SabnzbdQueue::new(
                SabnzbdClient::new(self.http.clone(), url, key, 1, Duration::from_secs(1)),
                url,
                key,
                std::path::PathBuf::from(downloads_mount),
                policy,
            );
            let health = queue.health_check().await;
            if health.status != "ok" {
                return SabnzbdVerdict {
                    valid: false,
                    version: health.version,
                    message: if health.message.is_empty() {
                        "SABnzbd unreachable.".to_owned()
                    } else {
                        health.message
                    },
                    categories: Vec::new(),
                    complete_dir: None,
                    diagnosis: None,
                };
            }
            let categories = cats_client
                .get_cats(PROBE_TIMEOUT)
                .await
                .unwrap_or_default();
            let complete_dir = queue
                .get_complete_dir()
                .await
                .ok()
                .filter(|dir| !dir.is_empty());
            // The submitted mount is diagnosed, not the stored one, so an
            // unsaved correction already shows the fixed verdict.
            let raw = queue.diagnose_downloads_mount().await;
            let mount_message =
                if raw.sampled_downloads > 0 && raw.resolvable_downloads < raw.sampled_downloads {
                    Some(format!(
                        "Only {}/{} sampled SABnzbd download(s) resolve under {} - the mount \
                     likely points at the wrong folder (for example a category subfolder of \
                     SABnzbd's completed dir shown above).",
                        raw.resolvable_downloads, raw.sampled_downloads, downloads_mount
                    ))
                } else {
                    None
                };
            let version = health.version.clone().unwrap_or_default();
            SabnzbdVerdict {
                valid: true,
                version: health.version,
                message: format!("SABnzbd {version}"),
                categories,
                complete_dir,
                diagnosis: Some(SabnzbdMountDiagnosis {
                    mount_has_files: raw.mount_has_files,
                    resolvable_downloads: raw.resolvable_downloads as i64,
                    sampled_downloads: raw.sampled_downloads as i64,
                    mount_message,
                }),
            }
        })
    }

    fn prowlarr<'a>(&'a self, url: &'a str, key: &'a str) -> BoxFuture<'a, ProwlarrVerdict> {
        Box::pin(async move {
            use crate::acquire::usenet::prowlarr::ProwlarrClient;
            if url.trim().is_empty() || key.is_empty() {
                return ProwlarrVerdict {
                    valid: false,
                    version: None,
                    message: "Prowlarr URL or API key not configured.".to_owned(),
                    indexer_count: None,
                };
            }
            let client = ProwlarrClient::new(self.http.clone(), url, key, "prowlarr");
            let status = match client.system_status(PROBE_TIMEOUT).await {
                Ok(status) => status,
                Err(error) => return prowlarr_failure(&error),
            };
            let indexers = match client.list_indexers(PROBE_TIMEOUT).await {
                Ok(indexers) => indexers,
                Err(error) => return prowlarr_failure(&error),
            };
            // Enabled rows only: disabled indexers are never searched, so
            // the raw list length would over-promise.
            let count = indexers.iter().filter(|indexer| indexer.enable).count() as i64;
            let version = status
                .map(|status| status.version)
                .filter(|version| !version.is_empty());
            let message = match &version {
                Some(version) => {
                    format!("Connected - Prowlarr v{version} with {count} enabled indexer(s).")
                }
                None => format!("Connected - {count} enabled indexer(s)."),
            };
            ProwlarrVerdict {
                valid: true,
                version,
                message,
                indexer_count: Some(count),
            }
        })
    }

    fn newznab<'a>(&'a self, url: &'a str, key: &'a str) -> BoxFuture<'a, NewznabVerdict> {
        Box::pin(async move {
            use crate::acquire::usenet::newznab::NewznabClient;
            if url.trim().is_empty() {
                return NewznabVerdict {
                    valid: false,
                    version: None,
                    message: "Indexer URL is required.".to_owned(),
                    supports_audio_search: false,
                    category_count: 0,
                    suggested_url: None,
                };
            }
            let url = url.trim().trim_end_matches('/').to_owned();
            let client = NewznabClient::new(self.http.clone(), &url, key, "", "");
            let caps = match client.caps(PROBE_TIMEOUT).await {
                Ok(caps) => caps,
                Err(error) => {
                    return newznab_failure(&self.http, &url, key, &error.to_string()).await;
                }
            };
            // Well-formed but empty caps mean the URL answered something
            // other than a caps document (typically the site homepage):
            // not an endpoint.
            if caps_are_empty(&caps) {
                return newznab_failure(
                    &self.http,
                    &url,
                    key,
                    "The URL did not answer as a Newznab endpoint.",
                )
                .await;
            }
            let title = caps.server_title.clone().unwrap_or_default();
            let name = if title.is_empty() { "Indexer" } else { &title };
            let message = if caps.supports_audio_search {
                format!("{name} OK - structured music search.")
            } else {
                format!("{name} OK - text search (no audio-search).")
            };
            NewznabVerdict {
                valid: true,
                version: caps.server_version.clone(),
                message,
                supports_audio_search: caps.supports_audio_search,
                category_count: caps.categories.len() as i64,
                suggested_url: None,
            }
        })
    }

    fn musicbrainz<'a>(&'a self, api_url: &'a str) -> BoxFuture<'a, ProbeVerdict> {
        Box::pin(async move {
            let url = format!("{}/artist", api_url.trim_end_matches('/'));
            let response = self
                .http
                .get(&url)
                .query(&[("query", "test"), ("limit", "1"), ("fmt", "json")])
                .timeout(PROBE_TIMEOUT)
                .send()
                .await;
            match response {
                Ok(response) if response.status().is_success() => {
                    ProbeVerdict::ok("Connected to MusicBrainz.")
                }
                Ok(response) => ProbeVerdict::failed(format!(
                    "MusicBrainz verification returned HTTP {}.",
                    response.status().as_u16()
                )),
                Err(_) => ProbeVerdict::failed("Could not connect to MusicBrainz."),
            }
        })
    }

    fn oidc<'a>(&'a self, issuer: &'a str) -> BoxFuture<'a, ProbeVerdict> {
        Box::pin(async move {
            let issuer = issuer.trim();
            if issuer.is_empty() {
                return ProbeVerdict::failed("Issuer URL is required.");
            }
            let url = format!(
                "{}{}",
                issuer.trim_end_matches('/'),
                crate::auth::federated::oidc::DISCOVERY_SUFFIX
            );
            let response = self.http.get(&url).timeout(PROBE_TIMEOUT).send().await;
            match response {
                Ok(response) if response.status().is_success() => {
                    let name = decode_body(response)
                        .await
                        .and_then(|doc| {
                            doc.get("issuer")
                                .and_then(|value| value.as_str())
                                .map(str::to_owned)
                        })
                        .unwrap_or_else(|| issuer.to_owned());
                    ProbeVerdict::ok(format!("Connected to {name}."))
                }
                Ok(_) => ProbeVerdict::failed("Failed to fetch OIDC discovery document."),
                Err(_) => ProbeVerdict::failed("Failed to fetch OIDC discovery document."),
            }
        })
    }
}

/// Newznab failure mapping. A bare site URL is almost never the
/// endpoint: offer `<url>/api` when it answers as a real endpoint.
/// The URL is never echoed.
async fn newznab_failure(
    http: &reqwest::Client,
    url: &str,
    key: &str,
    message: &str,
) -> NewznabVerdict {
    if let Some(suggestion) = api_path_suggestion(url)
        && reaches_newznab(http, &suggestion, key).await
    {
        return NewznabVerdict {
            valid: false,
            version: None,
            message: "That's the site's homepage, not the API endpoint.".to_owned(),
            supports_audio_search: false,
            category_count: 0,
            suggested_url: Some(suggestion),
        };
    }
    NewznabVerdict {
        valid: false,
        version: None,
        message: message.to_owned(),
        supports_audio_search: false,
        category_count: 0,
        suggested_url: None,
    }
}

/// Prowlarr failure mapping: auth and rate-limit verdicts name the
/// cause; anything else is a reachability verdict. The URL is never
/// echoed.
fn prowlarr_failure(error: &crate::acquire::usenet::prowlarr::ProwlarrError) -> ProwlarrVerdict {
    use crate::acquire::usenet::prowlarr::ProwlarrError as Source;
    let message = match error {
        Source::Auth { .. } => {
            "Prowlarr rejected the API key. Check the key in Prowlarr.".to_owned()
        }
        Source::RateLimited { .. } => {
            "Prowlarr rate-limited the test. Try again shortly.".to_owned()
        }
        _ => "Couldn't reach Prowlarr. Check the URL and that Prowlarr is running.".to_owned(),
    };
    ProwlarrVerdict {
        valid: false,
        version: None,
        message,
        indexer_count: None,
    }
}

/// Decode a JSON body. The reqwest `json` feature is off, so this
/// reads bytes and decodes with serde directly; anything unreadable
/// is `None`.
async fn decode_body(response: reqwest::Response) -> Option<serde_json::Value> {
    let bytes = response.bytes().await.ok()?;
    serde_json::from_slice(&bytes).ok()
}

/// Jellyfin `/Users` as `(id, name)` pairs. Fails open to empty: the
/// connection verdict stands without the picker.
async fn jellyfin_users(http: &reqwest::Client, url: &str, key: &str) -> Vec<(String, String)> {
    let response = http
        .get(format!("{}/Users", url.trim_end_matches('/')))
        .header(
            reqwest::header::AUTHORIZATION,
            format!("MediaBrowser Token=\"{key}\""),
        )
        .timeout(PROBE_TIMEOUT)
        .send()
        .await;
    let body: serde_json::Value = match response {
        Ok(response) if response.status().is_success() => match decode_body(response).await {
            Some(body) => body,
            None => return Vec::new(),
        },
        _ => return Vec::new(),
    };
    body.as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter_map(|user| {
            let id = user.get("Id")?.as_str()?;
            let name = user.get("Name")?.as_str().unwrap_or("");
            if id.is_empty() {
                return None;
            }
            Some((id.to_owned(), name.to_owned()))
        })
        .collect()
}

/// A bare site URL (no path) is almost never the Newznab endpoint: the
/// API lives at `/api`. Offer `<url>/api` only then.
fn api_path_suggestion(url: &str) -> Option<String> {
    let rest = url
        .trim_start_matches("https://")
        .trim_start_matches("http://");
    let path = rest.split('/').skip(1).collect::<Vec<_>>().join("/");
    if path.trim_matches('/').is_empty() {
        Some(format!("{}/api", url.trim_end_matches('/')))
    } else {
        None
    }
}

/// Did `url` answer as a real Newznab endpoint? Non-empty caps, an
/// auth error, or a rate limit all mean the API was reached; anything
/// else means the suggestion is wrong too.
async fn reaches_newznab(http: &reqwest::Client, url: &str, key: &str) -> bool {
    use crate::acquire::usenet::newznab::{NewznabClient, NewznabError};
    let client = NewznabClient::new(http.clone(), url, key, "", "");
    match client.caps(PROBE_TIMEOUT).await {
        Ok(caps) => !caps_are_empty(&caps),
        Err(NewznabError::Auth { .. } | NewznabError::RateLimited { .. }) => true,
        Err(_) => false,
    }
}

/// Empty caps (no server, no searching, no categories) mean the URL
/// answered something other than a caps document.
fn caps_are_empty(caps: &crate::acquire::usenet::newznab::NewznabCaps) -> bool {
    caps.server_title.is_none()
        && caps.server_version.is_none()
        && !caps.supports_audio_search
        && caps.categories.is_empty()
}

/// Check a submitted HIBP hash-list path: it must exist, start with a
/// `40-char-SHA1:count` line, and read cleanly. Pure filesystem work,
/// no network, so it lives outside the probe port.
pub fn check_hibp_file(path: &str) -> ProbeVerdict {
    use std::io::BufRead as _;
    let trimmed = path.trim();
    if trimmed.is_empty() {
        return ProbeVerdict::failed("No path provided.");
    }
    let file = match std::fs::File::open(trimmed) {
        Ok(file) => file,
        Err(_) => return ProbeVerdict::failed(format!("File not found: {trimmed}.")),
    };
    let size = file.metadata().map(|meta| meta.len()).unwrap_or(0);
    let mut lines = std::io::BufReader::new(file).lines();
    let first = lines.next().and_then(|line| line.ok()).unwrap_or_default();
    let first = first.trim();
    let Some((hash, _)) = first.split_once(':') else {
        return ProbeVerdict::failed(
            "File does not appear to be a valid HIBP hash list (unexpected format).",
        );
    };
    if hash.len() != 40 || !hash.bytes().all(|byte| byte.is_ascii_alphanumeric()) {
        return ProbeVerdict::failed(
            "File does not appear to be a valid HIBP hash list (expected 40-char SHA-1 hash).",
        );
    }
    ProbeVerdict::ok(format!("File looks valid. Size: {}.", format_bytes(size)))
}

/// Human byte size, v2's `_fmt_size` units.
fn format_bytes(bytes: u64) -> String {
    let mut size = bytes as f64;
    for unit in ["B", "KB", "MB", "GB"] {
        if size < 1024.0 {
            return format!("{size:.1} {unit}");
        }
        size /= 1024.0;
    }
    format!("{size:.1} TB")
}
