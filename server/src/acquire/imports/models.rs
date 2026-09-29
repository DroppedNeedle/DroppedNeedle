//! Wire shapes for the imports + health slice.
//!
//! One shape per concept, all snake_case, mirroring the v2 DTOs in
//! `backend/api/v1/schemas/` (lidarr_import, me_connections, download,
//! settings). Handlers render these as-is.

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// Masked sentinel for the Lidarr API key on read. A PUT carrying it back
/// keeps the stored key (v2 `LIDARR_IMPORT_API_KEY_MASK`).
pub const LIDARR_API_KEY_MASK: &str = "lidarr****";
/// Masked sentinel for the Spotify client secret on read.
pub const SPOTIFY_SECRET_MASK: &str = "spotify****";

/// Admin-configured read-only Lidarr import connection (v2
/// `LidarrImportConnectionSettings`, D5). NOT a management integration.
// Debug is hand-written below: the API key never appears in debug output.
#[derive(Clone, Default, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct LidarrConnectionSettings {
    /// Lidarr base URL, normalised to a bare origin.
    pub url: String,
    /// API key, Fernet-encrypted at rest, masked on read.
    pub api_key: String,
}

impl std::fmt::Debug for LidarrConnectionSettings {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LidarrConnectionSettings")
            .field("url", &self.url)
            .field("api_key", &"<redacted>")
            .finish()
    }
}

/// Result of testing submitted Lidarr credentials (v2 `LidarrTestResponse`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct LidarrTestResponse {
    /// True when the probe authenticated.
    pub valid: bool,
    /// Lidarr version from `system/status`, when the probe reached it.
    pub version: Option<String>,
    /// User-safe summary. Never echoes the URL or host.
    pub message: String,
}

/// One monitored Lidarr artist annotated for the requesting user (v2
/// `LidarrArtistCandidate`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct LidarrArtistCandidate {
    /// MusicBrainz artist MBID (`foreignArtistId`).
    pub mbid: String,
    /// Artist name as Lidarr reports it.
    pub name: String,
    /// Lidarr `monitorNewItems`: `none` or `all`.
    pub monitor_new_items: String,
    /// True when the user already follows this MBID.
    pub already_following: bool,
    /// True when importing would arm auto-download (`all`).
    pub would_auto_download: bool,
}

/// Candidate list (v2 `LidarrArtistListResponse`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct LidarrArtistListResponse {
    /// Monitored artists with valid MBIDs.
    pub artists: Vec<LidarrArtistCandidate>,
    /// Count of `artists`.
    pub total: i64,
}

/// Import selection (v2 `LidarrImportRequest`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct LidarrImportRequest {
    /// MBIDs to import; unknown or unmonitored entries are ignored.
    pub selected_mbids: Vec<String>,
}

/// Import summary (v2 `LidarrImportResponse`). `imported` counts brand-new
/// follows only; `already_following` is the disjoint pre-existing subset;
/// `auto_download_enabled` counts brand-new auto-download follows only (D9).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct LidarrImportResponse {
    /// Brand-new follows created.
    pub imported: i64,
    /// Selected MBIDs already followed before this import.
    pub already_following: i64,
    /// Selected entries that were not valid MBIDs.
    pub skipped_invalid: i64,
    /// Brand-new follows with auto-download armed.
    pub auto_download_enabled: i64,
    /// Approval batch for a non-admin auto-download mirror; else null.
    pub approval_batch_id: Option<String>,
}

/// Admin Spotify app settings (v2 `SpotifySettings`, GH-298).
// Debug is hand-written below: the client secret never appears in debug output.
#[derive(Clone, Default, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct SpotifySettings {
    /// Spotify app client id, returned unmasked.
    pub client_id: String,
    /// Spotify app secret, encrypted at rest, masked on read.
    pub client_secret: String,
    /// Master switch for Spotify OAuth.
    pub enabled: bool,
    /// Optional absolute origin the OAuth redirect URI is built from.
    pub spotify_redirect_origin: String,
}

impl std::fmt::Debug for SpotifySettings {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SpotifySettings")
            .field("client_id", &self.client_id)
            .field("client_secret", &"<redacted>")
            .field("enabled", &self.enabled)
            .field("spotify_redirect_origin", &self.spotify_redirect_origin)
            .finish()
    }
}

/// The computed OAuth redirect URI (v2 `GET /spotify/redirect-uri`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct SpotifyRedirectUri {
    /// Byte-exact URI registered in the Spotify dashboard.
    pub redirect_uri: String,
}

/// Authorize URL for the caller's Spotify link flow (v2
/// `SpotifyAuthUrlResponse`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct SpotifyAuthUrlResponse {
    /// `accounts.spotify.com/authorize` URL with state.
    pub auth_url: String,
}

/// One owned Spotify playlist (v2 `SpotifyPlaylistItem`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct SpotifyPlaylistItem {
    /// Spotify playlist id.
    pub id: String,
    /// Playlist name.
    pub name: String,
    /// Playlist description.
    pub description: String,
    /// Track total from the `items`/`tracks` dict quirk.
    pub track_count: i64,
    /// Picked cover URL, smallest image at least 250 wide.
    pub cover_url: Option<String>,
    /// Owner display name.
    pub owner: String,
    /// Internal playlist id when already imported, else null.
    pub imported_playlist_id: Option<String>,
}

/// Owned playlists (v2 `SpotifyPlaylistListResponse`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct SpotifyPlaylistListResponse {
    /// Playlists owned by the caller's Spotify account.
    pub playlists: Vec<SpotifyPlaylistItem>,
}

/// Import start (v2 `SpotifyImportRequest`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct SpotifyImportRequest {
    /// Name for the internal playlist record.
    pub name: String,
}

/// Import acknowledgement (v2 `SpotifyImportResponse`). The populate runs
/// as the `spotify:import` durable job; the id is usable immediately.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct SpotifyImportResponse {
    /// Internal playlist id.
    pub playlist_id: String,
}

/// Terminal-or-live state of one `spotify:import` job.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct SpotifyJobStatus {
    /// `running`, `done`, or `error`.
    pub state: String,
    /// Internal playlist id the job populates.
    pub playlist_id: String,
    /// Tracks written on success.
    pub track_count: Option<i64>,
    /// User-safe failure summary, else null.
    pub message: Option<String>,
}

/// Per-source release gate verdict. Each source gates independently: one
/// red gate never flips another.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct SourceGate {
    /// `slskd`, `sabnzbd`, `newznab`, or `lidarr_import`.
    pub source: String,
    /// Admin master switch for this source.
    pub enabled: bool,
    /// Credentials/URL present.
    pub configured: bool,
    /// Live probe answered.
    pub reachable: bool,
    /// True when releases may flow from this source.
    pub open: bool,
    /// User-safe summary.
    pub message: String,
}

/// Acquisition health smoke (v2 `StatusReport` shape, extended). `ready`
/// is Free OR slskd OR Usenet readiness; the gates unpack per source.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct AcquireHealth {
    /// Overall verdict: `ok`, `degraded`, or `error`.
    pub status: String,
    /// True when at least one acquisition path can serve releases.
    pub ready: bool,
    /// Paths currently ready: any of `free`, `slskd`, `usenet`.
    pub ready_via: Vec<String>,
    /// Independent per-source release gates.
    pub gates: Vec<SourceGate>,
}

/// Live slskd client status (v2 `DownloadClientStatusResponse`, trimmed to
/// the client half; the mount half belongs to the downloads slice).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct SlskdStatusResponse {
    /// Credentials/URL present.
    pub configured: bool,
    /// Live probe answered.
    pub reachable: bool,
    /// slskd version, when the probe reached it.
    pub version: Option<String>,
    /// User-safe summary.
    pub message: String,
}

/// Live SABnzbd status against the saved config (v2 `SabnzbdTestResponse`,
/// status half).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct SabnzbdStatusResponse {
    /// True when the saved config probes clean.
    pub valid: bool,
    /// SABnzbd version, when the probe reached it.
    pub version: Option<String>,
    /// User-safe summary.
    pub message: String,
    /// Category list, when the probe reached it.
    pub categories: Vec<String>,
    /// Completed dir (the mount hint), when the probe reached it.
    pub complete_dir: Option<String>,
}
