//! In-repo mock Lidarr and Spotify servers.
//!
//! These axum apps are the executable record of the live-verified quirks
//! the imports code ports: the Lidarr mock pins the `/api/v1` + `X-Api-Key`
//! contract (key in query is ignored, wrong key 401s) and the tolerant
//! decode shape (extra fields, `mbId` present-but-ignored); the Spotify
//! mock pins the `/me/playlists` current shape (`items: {total}`, `tracks`
//! null), the `/items` tracks endpoint (the legacy `/tracks` path 403s, as
//! it does for dev-mode apps after the March 2026 migration), and the
//! `track`/`item` entry alias. No test touches a live Lidarr or Spotify.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use axum::{
    Json,
    body::Body,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
    routing::get,
};
use serde_json::{Value, json};

/// API key the Lidarr mock accepts.
pub const LIDARR_KEY: &str = "lidarr-test-key";
/// Version the Lidarr mock reports.
pub const LIDARR_VERSION: &str = "3.1.3.4968";
/// Bearer token the Spotify mock accepts.
pub const SPOTIFY_TOKEN: &str = "spotify-test-token";
/// Refresh token the Spotify mock accepts.
pub const SPOTIFY_REFRESH: &str = "spotify-refresh-token";
/// Spotify user id the mock owns.
pub const SPOTIFY_USER_ID: &str = "spotify-user-1";
/// MBIDs the Lidarr mock serves.
pub const LIDARR_MBID_ALL: &str = "11111111-1111-1111-1111-111111111111";
/// Second mock MBID (monitor `none`).
pub const LIDARR_MBID_NONE: &str = "22222222-2222-2222-2222-222222222222";
/// Unmonitored mock MBID (never imported).
pub const LIDARR_MBID_UNMONITORED: &str = "33333333-3333-3333-3333-333333333333";

/// Loopback mock server handle.
pub struct MockServer {
    /// Base URL (scheme + host + port, no trailing slash).
    pub base_url: String,
    _guard: tokio::task::JoinHandle<()>,
}

/// Serve `app` on a loopback port.
async fn serve(app: axum::Router) -> Result<MockServer, std::io::Error> {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let addr: SocketAddr = listener.local_addr()?;
    let guard = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap_or(());
    });
    Ok(MockServer {
        base_url: format!("http://{addr}"),
        _guard: guard,
    })
}

/// Recorded upstream calls for test assertions.
#[derive(Debug, Clone, Default)]
pub struct RecordedCalls {
    /// Paths the Lidarr mock served (management paths would show here).
    pub lidarr_paths: Vec<String>,
    /// `X-Api-Key` values the Lidarr mock saw.
    pub lidarr_keys: Vec<String>,
    /// Token-endpoint grant types the accounts mock saw.
    pub spotify_grants: Vec<String>,
    /// API paths the Spotify mock served.
    pub spotify_paths: Vec<String>,
}

/// The redirect URI the mock Spotify app lists by default: the v3 callback
/// as the test routers derive it (host `app.test`, no origin, no base
/// path).
pub const SPOTIFY_REGISTERED_REDIRECT: &str =
    "http://app.test/api/v3/acquire/spotify/auth/callback";

/// Cloneable recorder handle shared by a mock app and its test.
#[derive(Debug, Clone, Default)]
pub struct MockRecorder {
    inner: Arc<Mutex<RecordedCalls>>,
    registered_redirect: Arc<Mutex<Option<String>>>,
}

impl MockRecorder {
    /// Set the one redirect URI the mock Spotify app lists, as its
    /// dashboard would. Code exchanges naming any other URI fail, like
    /// Spotify's `invalid_grant`.
    pub fn register_redirect_uri(&self, uri: &str) {
        if let Ok(mut guard) = self.registered_redirect.lock() {
            *guard = Some(uri.to_owned());
        }
    }

    /// The redirect URI the mock app lists.
    pub fn registered_redirect_uri(&self) -> String {
        self.registered_redirect
            .lock()
            .ok()
            .and_then(|guard| guard.clone())
            .unwrap_or_else(|| SPOTIFY_REGISTERED_REDIRECT.to_owned())
    }

    /// Snapshot the calls recorded so far.
    pub fn snapshot(&self) -> RecordedCalls {
        self.inner
            .lock()
            .map(|guard| guard.clone())
            .unwrap_or_default()
    }

    fn record(&self, visit: impl FnOnce(&mut RecordedCalls)) {
        if let Ok(mut guard) = self.inner.lock() {
            visit(&mut guard);
        }
    }
}

/// Shared mock state.
#[derive(Debug, Clone)]
struct MockState {
    recorder: MockRecorder,
}

/// Serve the mock Lidarr. Serves exactly the two sanctioned GETs; every
/// other path 404s, so a stray management call fails its test loudly.
pub async fn serve_lidarr() -> Result<(MockServer, MockRecorder), std::io::Error> {
    serve_lidarr_with(&[]).await
}

/// Serve the mock Lidarr with extra artists appended to the catalog.
pub async fn serve_lidarr_with(
    extra: &[Value],
) -> Result<(MockServer, MockRecorder), std::io::Error> {
    let recorder = MockRecorder::default();
    let mut artists = vec![
        json!({
            "foreignArtistId": LIDARR_MBID_ALL,
            "artistName": "Aurora Current",
            "monitored": true,
            "monitorNewItems": "all",
            "status": "continuing",
            "mbId": "not-a-mbid",
            "statistics": {"trackFileCount": 12},
        }),
        json!({
            "foreignArtistId": LIDARR_MBID_NONE,
            "artistName": "Boreal Static",
            "monitored": true,
            "monitorNewItems": "none",
            "status": "ended",
        }),
        json!({
            "foreignArtistId": LIDARR_MBID_UNMONITORED,
            "artistName": "Glass Tides",
            "monitored": false,
            "monitorNewItems": "all",
            "status": "continuing",
        }),
        json!({
            "foreignArtistId": "not-a-mbid",
            "artistName": "Broken Join Key",
            "monitored": true,
            "monitorNewItems": "all",
            "status": "continuing",
        }),
    ];
    artists.extend(extra.iter().cloned());
    let state = LidarrState {
        recorder: recorder.clone(),
        artists,
    };
    let app = axum::Router::new()
        .route("/api/v1/system/status", get(lidarr_status))
        .route("/api/v1/artist", get(lidarr_artists))
        .fallback(lidarr_fallback)
        .with_state(state);
    Ok((serve(app).await?, recorder))
}

/// Lidarr mock state.
#[derive(Debug, Clone)]
struct LidarrState {
    recorder: MockRecorder,
    artists: Vec<Value>,
}

fn lidarr_authed(headers: &HeaderMap) -> bool {
    headers
        .get("X-Api-Key")
        .and_then(|value| value.to_str().ok())
        .is_some_and(|key| key == LIDARR_KEY)
}

async fn lidarr_status(State(state): State<LidarrState>, headers: HeaderMap) -> Response {
    state.recorder.record(|calls| {
        calls.lidarr_paths.push("/api/v1/system/status".to_owned());
        calls.lidarr_keys.push(
            headers
                .get("X-Api-Key")
                .and_then(|value| value.to_str().ok())
                .unwrap_or("")
                .to_owned(),
        );
    });
    if !lidarr_authed(&headers) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    Json(json!({"appName": "Lidarr", "version": LIDARR_VERSION})).into_response()
}

async fn lidarr_artists(State(state): State<LidarrState>, headers: HeaderMap) -> Response {
    state.recorder.record(|calls| {
        calls.lidarr_paths.push("/api/v1/artist".to_owned());
        calls.lidarr_keys.push(
            headers
                .get("X-Api-Key")
                .and_then(|value| value.to_str().ok())
                .unwrap_or("")
                .to_owned(),
        );
    });
    if !lidarr_authed(&headers) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    Json(state.artists.clone()).into_response()
}

async fn lidarr_fallback(
    State(state): State<LidarrState>,
    headers: HeaderMap,
    uri: axum::http::Uri,
) -> Response {
    let path = uri.path().to_owned();
    state.recorder.record(|calls| {
        calls.lidarr_paths.push(path.clone());
        calls.lidarr_keys.push(
            headers
                .get("X-Api-Key")
                .and_then(|value| value.to_str().ok())
                .unwrap_or("")
                .to_owned(),
        );
    });
    StatusCode::NOT_FOUND.into_response()
}

/// Serve the mock Spotify pair. Returns `(api, accounts, covers, recorder)`
/// base URLs: the API mock, the accounts (token) mock, and the cover host
/// the playlist images point at.
pub async fn serve_spotify()
-> Result<(MockServer, MockServer, MockServer, MockRecorder), std::io::Error> {
    let recorder = MockRecorder::default();
    let covers = serve(cover_app()).await?;
    let state = SpotifyState {
        recorder: recorder.clone(),
        cover_base: covers.base_url.clone(),
    };
    let api = axum::Router::new()
        .route("/me", get(spotify_me))
        .route("/me/playlists", get(spotify_playlists))
        .route("/playlists/{id}", get(spotify_playlist))
        .route("/playlists/{id}/items", get(spotify_items))
        .route("/playlists/{id}/tracks", get(spotify_tracks_tombstone))
        .with_state(state);
    let accounts = axum::Router::new()
        .route("/api/token", axum::routing::post(spotify_token))
        .with_state(MockState {
            recorder: recorder.clone(),
        });
    Ok((serve(api).await?, serve(accounts).await?, covers, recorder))
}

/// Spotify API mock state.
#[derive(Debug, Clone)]
struct SpotifyState {
    recorder: MockRecorder,
    cover_base: String,
}

fn spotify_authed(headers: &HeaderMap) -> bool {
    headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value == format!("Bearer {SPOTIFY_TOKEN}"))
}

async fn spotify_me(State(state): State<SpotifyState>, headers: HeaderMap) -> Response {
    state
        .recorder
        .record(|calls| calls.spotify_paths.push("/me".to_owned()));
    if !spotify_authed(&headers) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    Json(json!({"id": SPOTIFY_USER_ID, "display_name": "Mock Listener"})).into_response()
}

async fn spotify_playlists(
    State(state): State<SpotifyState>,
    headers: HeaderMap,
    Query(params): Query<HashMap<String, String>>,
) -> Response {
    state
        .recorder
        .record(|calls| calls.spotify_paths.push("/me/playlists".to_owned()));
    if !spotify_authed(&headers) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    // Current simplified-playlist shape: the count under `items` as a dict,
    // `tracks` null. One foreign-owned playlist must be filtered out.
    let all = vec![
        json!({
            "id": "sp-playlist-1",
            "name": "Neon Nights",
            "description": "Synthwave staples",
            "images": [
                {"url": format!("{}/covers/neon-small.jpg", state.cover_base), "width": 60},
                {"url": format!("{}/covers/neon.jpg", state.cover_base), "width": 640},
            ],
            "owner": {"id": SPOTIFY_USER_ID, "display_name": "Mock Listener"},
            "items": {"href": "https://api.spotify.com/v1/x", "total": 2},
            "tracks": null,
        }),
        json!({
            "id": "sp-playlist-2",
            "name": "Paper Satellites",
            "description": "",
            "images": [],
            "owner": {"id": SPOTIFY_USER_ID, "display_name": "Mock Listener"},
            "tracks": {"href": "https://api.spotify.com/v1/y", "total": 0},
        }),
        json!({
            "id": "sp-foreign",
            "name": "Someone Else's",
            "description": "",
            "images": [],
            "owner": {"id": "another-user", "display_name": "Stranger"},
            "items": {"href": "https://api.spotify.com/v1/z", "total": 9},
            "tracks": null,
        }),
    ];
    let offset: usize = params
        .get("offset")
        .and_then(|value| value.parse().ok())
        .unwrap_or(0);
    let limit: usize = params
        .get("limit")
        .and_then(|value| value.parse().ok())
        .unwrap_or(50);
    let total = all.len();
    let items: Vec<Value> = all.into_iter().skip(offset).take(limit).collect();
    Json(json!({"items": items, "total": total, "limit": limit, "offset": offset})).into_response()
}

async fn spotify_playlist(
    State(state): State<SpotifyState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    state
        .recorder
        .record(|calls| calls.spotify_paths.push(format!("/playlists/{id}")));
    if !spotify_authed(&headers) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    if id != "sp-playlist-1" {
        return StatusCode::NOT_FOUND.into_response();
    }
    Json(json!({
        "id": "sp-playlist-1",
        "name": "Neon Nights",
        "images": [
            {"url": format!("{}/covers/neon-small.jpg", state.cover_base), "width": 60},
            {"url": format!("{}/covers/neon.jpg", state.cover_base), "width": 640},
        ],
        "tracks": {"total": 2},
    }))
    .into_response()
}

async fn spotify_items(
    State(state): State<SpotifyState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    state
        .recorder
        .record(|calls| calls.spotify_paths.push(format!("/playlists/{id}/items")));
    if !spotify_authed(&headers) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    if id != "sp-playlist-1" {
        return StatusCode::NOT_FOUND.into_response();
    }
    let _ = &state;
    // One entry under the legacy `track` alias, one under the new `item`
    // key, plus an `is_local` row the client must skip.
    Json(json!({
        "items": [
            {"track": {
                "id": "sp-track-1", "type": "track", "name": "Midnight Drive",
                "artists": [{"name": "Aurora Current"}],
                "album": {"id": "sp-album-1", "name": "Neon Meridian", "images": []},
                "external_ids": {"isrc": "USRC17607839"},
                "track_number": 1, "disc_number": 1, "duration_ms": 183000,
            }},
            {"item": {
                "id": "sp-track-2", "type": "track", "name": "Glass Tides",
                "artists": [{"name": "Boreal Static"}],
                "album": {"id": "sp-album-2", "name": "Glass Tides", "images": []},
                "external_ids": {"isrc": "USRC17607840"},
                "track_number": 2, "disc_number": 1, "duration_ms": 201000,
            }},
            {"is_local": true, "track": {
                "id": "sp-local", "type": "track", "name": "Local File",
                "artists": [{"name": "Nobody"}],
                "album": {"id": "sp-album-3", "name": "Local", "images": []},
            }},
        ],
        "next": null,
    }))
    .into_response()
}

/// The deprecated `/tracks` path: 403, as live Spotify answers dev-mode
/// apps after the March 2026 migration. Pins the `/items` choice.
async fn spotify_tracks_tombstone(State(state): State<SpotifyState>) -> Response {
    state
        .recorder
        .record(|calls| calls.spotify_paths.push("LEGACY_TRACKS_PATH".to_owned()));
    StatusCode::FORBIDDEN.into_response()
}

async fn spotify_token(
    State(state): State<MockState>,
    headers: HeaderMap,
    body: String,
) -> Response {
    let params: HashMap<String, String> = serde_urlencoded_parse(&body).into_iter().collect();
    let grant = params.get("grant_type").cloned().unwrap_or_default();
    state
        .recorder
        .record(|calls| calls.spotify_grants.push(grant.clone()));
    // Basic auth required, any test client id accepted.
    let authed = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.starts_with("Basic "));
    if !authed {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    match grant.as_str() {
        "authorization_code"
            if params.get("code").is_some_and(|code| code == "good-code")
                && params
                    .get("redirect_uri")
                    .is_some_and(|uri| *uri == state.recorder.registered_redirect_uri()) =>
        {
            Json(json!({
                "access_token": SPOTIFY_TOKEN,
                "refresh_token": SPOTIFY_REFRESH,
                "expires_in": 3600,
            }))
            .into_response()
        }
        "refresh_token"
            if params
                .get("refresh_token")
                .is_some_and(|token| token == SPOTIFY_REFRESH) =>
        {
            Json(json!({
                "access_token": SPOTIFY_TOKEN,
                "expires_in": 3600,
            }))
            .into_response()
        }
        _ => StatusCode::BAD_REQUEST.into_response(),
    }
}

/// Minimal form decoder (no new dependency).
fn serde_urlencoded_parse(body: &str) -> Vec<(String, String)> {
    body.split('&')
        .filter_map(|pair| {
            let (key, value) = pair.split_once('=')?;
            Some((percent_decode(key), percent_decode(value)))
        })
        .collect()
}

fn percent_decode(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut bytes = value.bytes();
    while let Some(byte) = bytes.next() {
        if byte == b'%' {
            let hi = bytes.next().unwrap_or(b'0');
            let lo = bytes.next().unwrap_or(b'0');
            let hex = |digit: u8| match digit {
                b'0'..=b'9' => digit - b'0',
                b'a'..=b'f' => digit - b'a' + 10,
                b'A'..=b'F' => digit - b'A' + 10,
                _ => 0,
            };
            out.push((hex(hi) * 16 + hex(lo)) as char);
        } else if byte == b'+' {
            out.push(' ');
        } else {
            out.push(byte as char);
        }
    }
    out
}

/// Cover host: one real image, one redirect (must be refused), one
/// non-image (must be refused).
fn cover_app() -> axum::Router {
    axum::Router::new()
        .route("/covers/neon.jpg", get(cover_image))
        .route("/covers/neon-small.jpg", get(cover_image))
        .route("/covers/redirect.jpg", get(cover_redirect))
        .route("/covers/not-an-image.jpg", get(cover_text))
}

async fn cover_image() -> Response {
    (
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, "image/jpeg"),
            (header::CONTENT_LENGTH, "16"),
        ],
        Body::from(vec![
            0xFF, 0xD8, 0xFF, 0xE0, 0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11,
        ]),
    )
        .into_response()
}

async fn cover_redirect() -> Response {
    (StatusCode::FOUND, [(header::LOCATION, "/covers/neon.jpg")]).into_response()
}

async fn cover_text() -> Response {
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "text/html")],
        "nope",
    )
        .into_response()
}
