//! Read-only Lidarr import client and service.
//!
//! Two GET endpoints
//! (`/system/status`, `/artist`) under `{base}/api/v1` with the `X-Api-Key`
//! header (verified against live Lidarr 3.1.3.4968), monitored artists only,
//! the authoritative re-fetch, the pre-read counts, the auto-download rule
//! (only brand-new follows monitored `all`), and the follows written in
//! one transaction. No method here issues any Lidarr management call.

use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};

use super::models::{LidarrArtistCandidate, LidarrArtistListResponse, LidarrImportResponse};
use crate::client_error::{ClientError, Service, retry_after};
use crate::follows::{Follows, ImportArtist};
use crate::requests::error::RequestsError;
use crate::settings::ConfigLidarrSettings;
use droppedneedle_kernel::http_client::{BodyError, read_capped, transport_error};
use droppedneedle_kernel::runtime_config::ConfigError;

/// Largest Lidarr answer read. `/artist` is one unpaged list of every
/// artist with its overview and images, so a large library runs to tens
/// of megabytes.
const MAX_BODY_BYTES: usize = 64 * 1024 * 1024;

/// Largest error answer read, for the short line kept for the log.
const ERROR_BODY_BYTES: usize = 8 * 1024;

/// True for a well-formed MusicBrainz id: `8-4-4-4-12` lowercase-or-upper
/// hex, surrounding whitespace ignored, `unknown_` ids rejected.
pub fn is_valid_mbid(value: &str) -> bool {
    let trimmed = value.trim();
    if trimmed.is_empty() || trimmed.starts_with("unknown_") {
        return false;
    }
    let bytes = trimmed.as_bytes();
    if bytes.len() != 36 {
        return false;
    }
    for (index, byte) in bytes.iter().enumerate() {
        if matches!(index, 8 | 13 | 18 | 23) {
            if *byte != b'-' {
                return false;
            }
        } else if !byte.is_ascii_hexdigit() {
            return false;
        }
    }
    true
}

/// Normalise a Lidarr base URL to a bare origin: trim, default a
/// schemeless host to `http://` (Lidarr is a plain-http LAN service, never
/// the SABnzbd https-forcing), strip one trailing slash, then strip a
/// pasted `/api/v1` or `/api` suffix. Silent, so Test needs no suggestion.
pub fn normalize_lidarr_url(raw: &str) -> String {
    let mut url = raw.trim().to_owned();
    if !url.is_empty() && !url.starts_with("http://") && !url.starts_with("https://") {
        url = format!("http://{url}");
    }
    url = url.trim_end_matches('/').to_owned();
    for suffix in ["/api/v1", "/api"] {
        if let Some(bare) = url.strip_suffix(suffix) {
            url = bare.trim_end_matches('/').to_owned();
            break;
        }
    }
    url
}

/// One Lidarr artist row (camelCase). `foreign_artist_id` is the MBID join
/// key and required, so a row without it fails the decode; the other
/// fields default. Lidarr's `mbId` is never populated live and is ignored
/// entirely.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LidarrArtist {
    /// MusicBrainz artist MBID.
    pub foreign_artist_id: String,
    /// Artist name as Lidarr reports it.
    #[serde(default)]
    pub artist_name: String,
    /// Monitored in Lidarr.
    #[serde(default)]
    pub monitored: bool,
    /// `none` or `all`.
    #[serde(default)]
    pub monitor_new_items: String,
    /// `continuing`, `ended`, ... Imported regardless of status.
    #[serde(default)]
    pub status: String,
}

/// `system/status` probe shape.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct LidarrSystemStatus {
    /// Lidarr version string.
    pub version: String,
}

/// Read-only Lidarr client: exactly the two sanctioned GETs.
#[derive(Debug, Clone)]
pub struct LidarrClient {
    http: reqwest::Client,
}

impl LidarrClient {
    /// Over `same_host`, the factory's client that follows a redirect only
    /// on the same host: the key rides in a header a redirect elsewhere
    /// would carry to another address.
    pub fn new(same_host: reqwest::Client) -> Self {
        Self { http: same_host }
    }

    /// Probe `system/status` with submitted-or-stored credentials.
    pub async fn system_status(
        &self,
        base_url: &str,
        api_key: &str,
    ) -> Result<LidarrSystemStatus, ClientError> {
        let body = self.get(base_url, api_key, "/system/status").await?;
        serde_json::from_slice::<LidarrSystemStatus>(&body)
            .map_err(|cause| ClientError::decode(Service::Lidarr, cause.to_string()))
    }

    /// Fetch the whole artist catalog for the import.
    pub async fn list_artists(
        &self,
        base_url: &str,
        api_key: &str,
    ) -> Result<Vec<LidarrArtist>, ClientError> {
        let body = self.get(base_url, api_key, "/artist").await?;
        serde_json::from_slice::<Vec<LidarrArtist>>(&body)
            .map_err(|cause| ClientError::decode(Service::Lidarr, cause.to_string()))
    }

    async fn get(&self, base_url: &str, api_key: &str, path: &str) -> Result<Vec<u8>, ClientError> {
        let endpoint = format!("{}/api/v1{path}", base_url.trim_end_matches('/'));
        let response = self
            .http
            .get(&endpoint)
            .header("X-Api-Key", api_key)
            .send()
            .await
            .map_err(|cause| ClientError::transport(Service::Lidarr, transport_error(cause)))?;
        let status = response.status();
        if !status.is_success() {
            let wait = retry_after(response.headers());
            let body = read_capped(response, ERROR_BODY_BYTES)
                .await
                .unwrap_or_default();
            let mut text = String::from_utf8_lossy(&body).into_owned();
            if !api_key.is_empty() {
                text = text.replace(api_key, "<redacted>");
            }
            return Err(ClientError::for_status(
                Service::Lidarr,
                status.as_u16(),
                wait,
                text.as_bytes(),
            ));
        }
        read_capped(response, MAX_BODY_BYTES)
            .await
            .map_err(|cause| match cause {
                BodyError::TooLarge => {
                    ClientError::decode(Service::Lidarr, "the answer exceeded the size limit")
                }
                BodyError::Transport(cause) => ClientError::transport(Service::Lidarr, cause),
            })
    }
}

/// Read-only Lidarr-to-follows importer.
pub struct LidarrImportService {
    client: LidarrClient,
    settings: std::sync::Arc<ConfigLidarrSettings>,
    follows: Follows,
}

/// Missing-connection failure. The message reaches the user as a 400.
pub const LIDARR_NOT_CONNECTED: &str =
    "Lidarr is not connected. An admin must configure it in Settings first.";

impl LidarrImportService {
    /// Wire the service from its parts.
    pub fn new(
        client: LidarrClient,
        settings: std::sync::Arc<ConfigLidarrSettings>,
        follows: Follows,
    ) -> Self {
        Self {
            client,
            settings,
            follows,
        }
    }

    /// Re-fetch Lidarr and keep monitored artists with a valid MBID.
    async fn monitored_artists(&self) -> Result<Vec<LidarrArtist>, ServiceError> {
        let raw = self.settings.get_raw().map_err(ServiceError::Settings)?;
        if raw.url.is_empty() || raw.api_key.expose().is_empty() {
            return Err(ServiceError::NotConnected);
        }
        let artists = self
            .client
            .list_artists(&raw.url, raw.api_key.expose())
            .await
            .map_err(ServiceError::Lidarr)?;
        Ok(artists
            .into_iter()
            .filter(|artist| artist.monitored && is_valid_mbid(&artist.foreign_artist_id))
            .collect())
    }

    /// Candidate list annotated for one user.
    pub async fn list_candidates(
        &self,
        user_id: &str,
    ) -> Result<LidarrArtistListResponse, ServiceError> {
        let monitored = self.monitored_artists().await?;
        let lowers: Vec<String> = monitored
            .iter()
            .map(|artist| artist.foreign_artist_id.to_lowercase())
            .collect();
        let existing = self
            .follows
            .followed_among(user_id, &lowers)
            .await
            .map_err(ServiceError::Follows)?;
        let artists: Vec<LidarrArtistCandidate> = monitored
            .iter()
            .map(|artist| {
                let lower = artist.foreign_artist_id.to_lowercase();
                LidarrArtistCandidate {
                    mbid: artist.foreign_artist_id.clone(),
                    name: artist.artist_name.clone(),
                    monitor_new_items: artist.monitor_new_items.clone(),
                    already_following: existing.contains(&lower),
                    would_auto_download: artist.monitor_new_items == "all",
                }
            })
            .collect();
        Ok(LidarrArtistListResponse {
            total: artists.len() as i64,
            artists,
        })
    }

    /// Import a selection into one user's follows.
    /// The follows and the auto-download intents land in one transaction.
    /// Only brand-new follows of artists Lidarr monitors with `all` get
    /// auto-download, so a re-import never re-arms a follow the user turned
    /// off.
    pub async fn import_artists(
        &self,
        user_id: &str,
        selected_mbids: &[String],
    ) -> Result<LidarrImportResponse, ServiceError> {
        let monitored = self.monitored_artists().await?;
        // Authoritative map from the re-fetch: a selected MBID not
        // currently monitored in Lidarr is silently ignored.
        let by_lower: HashMap<String, &LidarrArtist> = monitored
            .iter()
            .map(|artist| (artist.foreign_artist_id.to_lowercase(), artist))
            .collect();

        let mut artists = Vec::new();
        let mut skipped_invalid: i64 = 0;
        let mut seen: HashSet<String> = HashSet::new();
        for mbid in selected_mbids {
            if !is_valid_mbid(mbid) {
                skipped_invalid += 1;
                continue;
            }
            let lower = mbid.trim().to_lowercase();
            if !seen.insert(lower.clone()) {
                continue;
            }
            if let Some(artist) = by_lower.get(&lower) {
                artists.push(ImportArtist {
                    mbid: artist.foreign_artist_id.clone(),
                    name: artist.artist_name.clone(),
                    monitors_new: artist.monitor_new_items == "all",
                });
            }
        }
        let outcome = self
            .follows
            .import(user_id, artists)
            .await
            .map_err(ServiceError::Follows)?;
        Ok(LidarrImportResponse {
            imported: outcome.imported as i64,
            already_following: outcome.already_following as i64,
            skipped_invalid,
            auto_download_enabled: outcome.auto_download_enabled as i64,
        })
    }
}

/// Service-level failure: missing connection or a Lidarr fetch error.
#[derive(Debug)]
pub enum ServiceError {
    /// The Lidarr settings do not read: nothing is fetched until they are
    /// saved again.
    Settings(ConfigError),
    /// No Lidarr connection configured. User-safe 400.
    NotConnected,
    /// Lidarr fetch failed.
    Lidarr(ClientError),
    /// The follow write or read failed.
    Follows(RequestsError),
}
