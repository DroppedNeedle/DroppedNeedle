//! In-repo mock Jellyfin, Navidrome, and Plex servers.
//!
//! These axum apps are the executable record of the live-verified quirks
//! the adapters port: the Navidrome mock pins the 0.62.0 single-folder
//! probe (same `musicFolderId` twice accepted, unknown folders ignored for
//! catalog endpoints, two-folder behavior not modeled), the
//! Jellyfin mock pins the 10.11 auth-header rule (the `MediaBrowser`
//! header passes, the legacy Emby headers 401), and the Plex mock pins the
//! container-paging and composite-fallback shapes. No test touches a live
//! media server; every parity test runs against these on
//! loopback.

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

/// Shared canned catalog behind all three mocks.
const ARTIST_ONE: &str = "Aurora Current";
const ARTIST_TWO: &str = "Boreal Static";
const ALBUM_ONE: &str = "Neon Meridian";
const ALBUM_TWO: &str = "Glass Tides";
const ALBUM_THREE: &str = "Paper Satellites";
const GENRE: &str = "Synthwave";
/// Release MBID for "Neon Meridian", used by every match test.
pub const MATCH_MBID: &str = "11111111-1111-1111-1111-111111111111";

/// Credentials the mocks accept.
pub const JELLYFIN_KEY: &str = "jf-test-key";
/// Navidrome login the mock accepts (any token salt passes).
pub const NAVIDROME_USER: &str = "nd-user";
/// Plex token the mock accepts.
pub const PLEX_TOKEN: &str = "plex-token";
/// Plex account token the mock's plex.tv `/api/v2/resources` accepts; it
/// resolves to [`PLEX_TOKEN`] for the mock server.
pub const PLEX_ACCOUNT_TOKEN: &str = "plex-account-token";
/// The mock server's machine id.
pub const PLEX_MACHINE_ID: &str = "mock-machine-1";

/// Deterministic canned audio body served by every mock stream endpoint.
/// 1024 bytes; the first byte tags the source (`J`/`N`/`P`) and the rest
/// walk a counter, so E2E seeks can assert exact slices.
pub fn canned_stream_bytes(source_tag: u8) -> Vec<u8> {
    (0..1024)
        .map(|index| {
            if index == 0 {
                source_tag
            } else {
                (index % 251) as u8
            }
        })
        .collect()
}

/// Recorded upstream calls for test assertions.
#[derive(Debug, Clone, Default)]
pub struct RecordedCalls {
    /// `Authorization` header values seen by the Jellyfin mock.
    pub jellyfin_auth: Vec<String>,
    /// Query pairs seen by `GET /Items`, per call.
    pub jellyfin_items_queries: Vec<Vec<(String, String)>>,
    /// `musicFolderId` values per Navidrome endpoint call.
    pub navidrome_folder_params: HashMap<String, Vec<Vec<String>>>,
    /// Full query pairs per Navidrome endpoint call.
    pub navidrome_queries: Vec<(String, Vec<(String, String)>)>,
    /// `X-Plex-Token` values seen by the Plex mock.
    pub plex_tokens: Vec<String>,
    /// Query pairs per Plex section call, keyed by endpoint path.
    pub plex_section_queries: Vec<(String, Vec<(String, String)>)>,
}

/// Cloneable recorder handle shared by a mock app and its test.
#[derive(Debug, Clone, Default)]
pub struct MockRecorder {
    inner: Arc<Mutex<RecordedCalls>>,
}

impl MockRecorder {
    /// Snapshot the calls recorded so far.
    pub fn snapshot(&self) -> RecordedCalls {
        self.inner
            .lock()
            .map(|guard| guard.clone())
            .unwrap_or_default()
    }

    fn record(&self, update: impl FnOnce(&mut RecordedCalls)) {
        if let Ok(mut guard) = self.inner.lock() {
            update(&mut guard);
        }
    }
}

/// A running mock server on loopback. Dropping aborts the listener.
pub struct MockServer {
    /// Base URL, e.g. `http://127.0.0.1:41231`.
    pub base_url: String,
    /// Recorder handle for assertions.
    pub recorder: MockRecorder,
    handle: Option<tokio::task::JoinHandle<()>>,
}

impl MockServer {
    async fn serve(router: axum::Router, recorder: MockRecorder) -> Result<Self, std::io::Error> {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let address: SocketAddr = listener.local_addr()?;
        let handle = tokio::spawn(async move {
            let _ = axum::serve(listener, router).await;
        });
        Ok(Self {
            base_url: format!("http://{address}"),
            recorder,
            handle: Some(handle),
        })
    }
}

impl Drop for MockServer {
    fn drop(&mut self) {
        if let Some(handle) = self.handle.take() {
            handle.abort();
        }
    }
}

/// Start the mock Jellyfin 10.11 server.
pub async fn serve_jellyfin() -> Result<MockServer, std::io::Error> {
    let recorder = MockRecorder::default();
    let router = jellyfin_router(recorder.clone());
    MockServer::serve(router, recorder).await
}

/// Start the mock Navidrome 0.62.0 server.
pub async fn serve_navidrome() -> Result<MockServer, std::io::Error> {
    let recorder = MockRecorder::default();
    let router = navidrome_router(recorder.clone());
    MockServer::serve(router, recorder).await
}

/// Start the mock Plex server.
pub async fn serve_plex() -> Result<MockServer, std::io::Error> {
    let recorder = MockRecorder::default();
    let router = plex_router(recorder.clone());
    MockServer::serve(router, recorder).await
}

// ---------------------------------------------------------------------------
// Jellyfin 10.11 mock
// ---------------------------------------------------------------------------

fn jellyfin_router(recorder: MockRecorder) -> axum::Router {
    axum::Router::new()
        .route("/System/Info", get(jf_system_info))
        .route("/Users", get(jf_users))
        .route("/Items", get(jf_items))
        .route("/Artists", get(jf_artists))
        .route("/Items/Latest", get(jf_latest))
        .route("/MusicGenres", get(jf_genres))
        .route("/Search/Hints", get(jf_search_hints))
        .route("/Items/{id}", get(jf_item))
        .route("/Items/{id}/Similar", get(jf_similar))
        .route("/Items/{id}/InstantMix", get(jf_mix))
        .route("/Artists/{id}/InstantMix", get(jf_mix))
        .route("/MusicGenres/{genre}/InstantMix", get(jf_mix))
        .route("/Playlists/{id}/Items", get(jf_playlist_items))
        .route("/Audio/{id}/Lyrics", get(jf_lyrics))
        .route("/Audio/{id}/stream", get(jf_stream))
        .route("/Sessions", get(jf_sessions))
        .route("/Items/{id}/Images/Primary", get(jf_image))
        .with_state(recorder)
}

/// The 10.11 auth rule: only the MediaBrowser header passes. Legacy Emby
/// headers and query keys 401, exactly like the live 10.11.11 server.
fn jf_authorized(headers: &HeaderMap, recorder: &MockRecorder) -> bool {
    let seen = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("")
        .to_owned();
    recorder.record(|calls| calls.jellyfin_auth.push(seen.clone()));
    seen == format!("MediaBrowser Token=\"{JELLYFIN_KEY}\"")
}

fn jf_unauthorized() -> Response {
    (StatusCode::UNAUTHORIZED, Json(json!({}))).into_response()
}

async fn jf_system_info(State(recorder): State<MockRecorder>, headers: HeaderMap) -> Response {
    if !jf_authorized(&headers, &recorder) {
        return jf_unauthorized();
    }
    Json(json!({"ServerName": "Mock Jellyfin", "Version": "10.11.11"})).into_response()
}

async fn jf_users(State(recorder): State<MockRecorder>, headers: HeaderMap) -> Response {
    if !jf_authorized(&headers, &recorder) {
        return jf_unauthorized();
    }
    Json(json!([{"Id": "jf-user-1", "Name": "Listener"}])).into_response()
}

fn jf_albums() -> Vec<Value> {
    vec![
        jf_album(
            "jf-al-1",
            ALBUM_ONE,
            ARTIST_ONE,
            "jf-ar-1",
            2024,
            Some(MATCH_MBID),
            2,
        ),
        jf_album("jf-al-2", ALBUM_TWO, ARTIST_ONE, "jf-ar-1", 2022, None, 1),
        jf_album("jf-al-3", ALBUM_THREE, ARTIST_TWO, "jf-ar-2", 2020, None, 1),
    ]
}

fn jf_album(
    id: &str,
    name: &str,
    artist: &str,
    artist_id: &str,
    year: i64,
    mbid: Option<&str>,
    tracks: i64,
) -> Value {
    let mut providers = json!({});
    if let Some(mbid) = mbid {
        providers = json!({"MusicBrainzAlbum": mbid, "MusicBrainzReleaseGroup": mbid});
    }
    json!({
        "Id": id, "Name": name, "Type": "MusicAlbum",
        "AlbumArtist": artist,
        "ArtistItems": [{"Id": artist_id, "Name": artist}],
        "ProductionYear": year, "ChildCount": tracks,
        "ProviderIds": providers,
        "ImageTags": {"Primary": format!("{id}-tag")},
        "UserData": {"PlayCount": 5, "IsFavorite": id == "jf-al-1"},
    })
}

fn jf_artists_list() -> Vec<Value> {
    vec![
        json!({
            "Id": "jf-ar-1", "Name": ARTIST_ONE, "Type": "MusicArtist",
            "AlbumCount": 2, "ProviderIds": {},
            "ImageTags": {"Primary": "jf-ar-1-tag"},
            "UserData": {"PlayCount": 14, "IsFavorite": true},
        }),
        json!({
            "Id": "jf-ar-2", "Name": ARTIST_TWO, "Type": "MusicArtist",
            "AlbumCount": 1, "ProviderIds": {},
            "ImageTags": {"Primary": "jf-ar-2-tag"},
            "UserData": {"PlayCount": 0, "IsFavorite": false},
        }),
    ]
}

fn jf_track(id: &str, title: &str, album: &str, album_id: &str, artist: &str, index: i64) -> Value {
    json!({
        "Id": id, "Name": title, "Type": "Audio",
        "Album": album, "AlbumId": album_id, "ParentId": album_id,
        "ArtistItems": [{"Id": "jf-ar-1", "Name": artist}],
        "IndexNumber": index, "ParentIndexNumber": 1,
        "RunTimeTicks": 1_800_000_000i64,
        "ProductionYear": 2024,
        "ProviderIds": {},
        "ImageTags": {"Primary": format!("{id}-tag")},
        "UserData": {"PlayCount": 3, "IsFavorite": id == "jf-t-1"},
    })
}

fn jf_tracks() -> Vec<Value> {
    vec![
        jf_track(
            "jf-t-1",
            "Meridian Dawn",
            ALBUM_ONE,
            "jf-al-1",
            ARTIST_ONE,
            1,
        ),
        jf_track(
            "jf-t-2",
            "Meridian Dusk",
            ALBUM_ONE,
            "jf-al-1",
            ARTIST_ONE,
            2,
        ),
        jf_track("jf-t-3", "Tide Glass", ALBUM_TWO, "jf-al-2", ARTIST_ONE, 1),
        jf_track(
            "jf-t-4",
            "Satellite Fold",
            ALBUM_THREE,
            "jf-al-3",
            ARTIST_TWO,
            1,
        ),
    ]
}

async fn jf_items(
    State(recorder): State<MockRecorder>,
    headers: HeaderMap,
    Query(params): Query<Vec<(String, String)>>,
) -> Response {
    if !jf_authorized(&headers, &recorder) {
        return jf_unauthorized();
    }
    recorder.record(|calls| calls.jellyfin_items_queries.push(params.clone()));
    let query: HashMap<&str, &str> = params
        .iter()
        .map(|(key, value)| (key.as_str(), value.as_str()))
        .collect();
    let limit = query
        .get("limit")
        .or(query.get("Limit"))
        .and_then(|value| value.parse::<usize>().ok());
    let start = query
        .get("startIndex")
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(0);
    if query.get("IncludeItemTypes") == Some(&"Playlist") {
        return Json(json!({"Items": [jf_playlist_meta()], "TotalRecordCount": 1})).into_response();
    }
    let mut items = match query.get("includeItemTypes") {
        Some(&"MusicAlbum") => {
            let mut albums = jf_albums();
            if query.get("isFavorite") == Some(&"true") {
                albums.retain(|album| {
                    album
                        .get("UserData")
                        .and_then(|data| data.get("IsFavorite"))
                        .and_then(Value::as_bool)
                        == Some(true)
                });
            }
            albums
        }
        Some(&"Audio") => {
            let mut tracks = jf_tracks();
            if let Some(album) = query.get("albumIds") {
                tracks.retain(|track| track.get("AlbumId").and_then(Value::as_str) == Some(album));
            }
            if query.get("isFavorite") == Some(&"true") {
                tracks.retain(|track| {
                    track
                        .get("UserData")
                        .and_then(|data| data.get("IsFavorite"))
                        .and_then(Value::as_bool)
                        == Some(true)
                });
            }
            if let Some(term) = query.get("searchTerm") {
                tracks.retain(|track| {
                    track
                        .get("Name")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .contains(term)
                });
            }
            tracks
        }
        _ => jf_albums(),
    };
    if query.get("sortBy") == Some(&"DatePlayed") {
        items = vec![jf_tracks()[0].clone(), jf_tracks()[2].clone()];
    }
    let total = items.len();
    if limit == Some(0) {
        return Json(json!({"Items": [], "TotalRecordCount": total})).into_response();
    }
    let page: Vec<Value> = items
        .into_iter()
        .skip(start)
        .take(limit.unwrap_or(total))
        .collect();
    Json(json!({"Items": page, "TotalRecordCount": total})).into_response()
}

async fn jf_artists(
    State(recorder): State<MockRecorder>,
    headers: HeaderMap,
    Query(params): Query<Vec<(String, String)>>,
) -> Response {
    if !jf_authorized(&headers, &recorder) {
        return jf_unauthorized();
    }
    let query: HashMap<&str, &str> = params
        .iter()
        .map(|(key, value)| (key.as_str(), value.as_str()))
        .collect();
    let mut artists = jf_artists_list();
    if query.get("isFavorite") == Some(&"true") {
        artists.retain(|artist| {
            artist
                .get("UserData")
                .and_then(|data| data.get("IsFavorite"))
                .and_then(Value::as_bool)
                == Some(true)
        });
    }
    if let Some(term) = query.get("searchTerm") {
        artists.retain(|artist| {
            artist
                .get("Name")
                .and_then(Value::as_str)
                .unwrap_or("")
                .contains(term)
        });
    }
    let total = artists.len();
    let start = query
        .get("startIndex")
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(0);
    let limit = query
        .get("limit")
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(total);
    let page: Vec<Value> = artists.into_iter().skip(start).take(limit).collect();
    Json(json!({"Items": page, "TotalRecordCount": total})).into_response()
}

async fn jf_latest(State(recorder): State<MockRecorder>, headers: HeaderMap) -> Response {
    if !jf_authorized(&headers, &recorder) {
        return jf_unauthorized();
    }
    Json(json!([jf_albums()[0].clone()])).into_response()
}

async fn jf_genres(State(recorder): State<MockRecorder>, headers: HeaderMap) -> Response {
    if !jf_authorized(&headers, &recorder) {
        return jf_unauthorized();
    }
    Json(json!({"Items": [{"Name": GENRE}]})).into_response()
}

async fn jf_search_hints(
    State(recorder): State<MockRecorder>,
    headers: HeaderMap,
    Query(params): Query<Vec<(String, String)>>,
) -> Response {
    if !jf_authorized(&headers, &recorder) {
        return jf_unauthorized();
    }
    let query: HashMap<&str, &str> = params
        .iter()
        .map(|(key, value)| (key.as_str(), value.as_str()))
        .collect();
    let term = query.get("searchTerm").copied().unwrap_or("");
    let mut hints = Vec::new();
    for album in jf_albums() {
        let name = album.get("Name").and_then(Value::as_str).unwrap_or("");
        let providers = album.get("ProviderIds").cloned().unwrap_or(Value::Null);
        let mbid_hit = providers
            .as_object()
            .map(|map| map.values().any(|value| value.as_str() == Some(term)))
            .unwrap_or(false);
        if name.contains(term) || mbid_hit {
            hints.push(album);
        }
    }
    for track in jf_tracks() {
        if track
            .get("Name")
            .and_then(Value::as_str)
            .unwrap_or("")
            .contains(term)
        {
            hints.push(track);
        }
    }
    for artist in jf_artists_list() {
        if artist
            .get("Name")
            .and_then(Value::as_str)
            .unwrap_or("")
            .contains(term)
        {
            hints.push(artist);
        }
    }
    Json(json!({"SearchHints": hints})).into_response()
}

fn jf_known_item(id: &str) -> Option<Value> {
    for item in jf_albums()
        .into_iter()
        .chain(jf_tracks())
        .chain(jf_artists_list())
    {
        if item.get("Id").and_then(Value::as_str) == Some(id) {
            return Some(item);
        }
    }
    if id == "jf-pl-1" {
        return Some(jf_playlist_meta());
    }
    None
}

async fn jf_item(
    State(recorder): State<MockRecorder>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    if !jf_authorized(&headers, &recorder) {
        return jf_unauthorized();
    }
    match jf_known_item(&id) {
        Some(item) => Json(item).into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

async fn jf_similar(
    State(recorder): State<MockRecorder>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    if !jf_authorized(&headers, &recorder) {
        return jf_unauthorized();
    }
    let _ = id;
    Json(json!({"Items": [jf_tracks()[1].clone()]})).into_response()
}

async fn jf_mix(State(recorder): State<MockRecorder>, headers: HeaderMap) -> Response {
    if !jf_authorized(&headers, &recorder) {
        return jf_unauthorized();
    }
    Json(json!({"Items": jf_tracks()})).into_response()
}

fn jf_playlist_meta() -> Value {
    json!({
        "Id": "jf-pl-1", "Name": "Road Mix", "Type": "Playlist",
        "ChildCount": 2, "ImageTags": {},
    })
}

async fn jf_playlist_items(
    State(recorder): State<MockRecorder>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    if !jf_authorized(&headers, &recorder) {
        return jf_unauthorized();
    }
    if id != "jf-pl-1" {
        return StatusCode::NOT_FOUND.into_response();
    }
    Json(json!({"Items": [jf_tracks()[0].clone(), jf_tracks()[1].clone()]})).into_response()
}

async fn jf_lyrics(
    State(recorder): State<MockRecorder>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    if !jf_authorized(&headers, &recorder) {
        return jf_unauthorized();
    }
    if id != "jf-t-1" {
        return Json(json!({"Lyrics": []})).into_response();
    }
    Json(json!({"Lyrics": [
        {"Text": "dawn over the meridian", "Start": 0},
        {"Text": "neon on the water", "Start": 30_000_000},
    ]}))
    .into_response()
}

/// Direct audio bytes for any item id. The adapter under test never runs
/// the playback-info dance, so the mock answers the stream URL directly.
async fn jf_stream(
    State(recorder): State<MockRecorder>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    if !jf_authorized(&headers, &recorder) {
        return jf_unauthorized();
    }
    if id.trim().is_empty() {
        return (StatusCode::NOT_FOUND, Json(json!({}))).into_response();
    }
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "audio/mpeg")],
        Body::from(canned_stream_bytes(b'J')),
    )
        .into_response()
}

async fn jf_sessions(State(recorder): State<MockRecorder>, headers: HeaderMap) -> Response {
    if !jf_authorized(&headers, &recorder) {
        return jf_unauthorized();
    }
    Json(json!([{
        "Id": "jf-session-1", "UserName": "Listener",
        "DeviceName": "Kitchen Speaker", "Client": "Finamp",
        "NowPlayingItem": {
            "Id": "jf-t-1", "Name": "Meridian Dawn", "Type": "Audio",
            "Artists": [ARTIST_ONE], "Album": ALBUM_ONE, "AlbumId": "jf-al-1",
            "RunTimeTicks": 1_800_000_000i64,
            "ImageTags": {"Primary": "jf-t-1-tag"},
        },
        "PlayState": {"PositionTicks": 450_000_000i64, "IsPaused": false, "IsMuted": false},
    }]))
    .into_response()
}

async fn jf_image(
    State(recorder): State<MockRecorder>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    if !jf_authorized(&headers, &recorder) {
        return jf_unauthorized();
    }
    if jf_known_item(&id).is_none() {
        return StatusCode::NOT_FOUND.into_response();
    }
    image_bytes()
}

fn image_bytes() -> Response {
    // Minimal valid JPEG: SOI, empty scan, EOI. Decoders accept the shape;
    // tests assert bytes and content type, not pixels.
    let bytes = vec![0xFF, 0xD8, 0xFF, 0xD9];
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "image/jpeg")],
        Body::from(bytes),
    )
        .into_response()
}

// ---------------------------------------------------------------------------
// Navidrome 0.62.0 mock
// ---------------------------------------------------------------------------

fn navidrome_router(recorder: MockRecorder) -> axum::Router {
    axum::Router::new()
        .route("/rest/{endpoint}", get(nd_handle))
        .route("/rest/getCoverArt", get(nd_cover))
        .route("/rest/stream", get(nd_stream))
        .with_state(recorder)
}

fn nd_envelope(payload: Value) -> Response {
    let mut body = json!({
        "subsonic-response": {
            "status": "ok",
            "version": "1.16.1",
            "serverVersion": "0.62.0 (mock)",
        }
    });
    if let (Some(map), Value::Object(extra)) = (body.get_mut("subsonic-response"), payload)
        && let Some(target) = map.as_object_mut()
    {
        target.extend(extra);
    }
    Json(body).into_response()
}

fn nd_failure(code: i64, message: &str) -> Response {
    Json(json!({
        "subsonic-response": {
            "status": "failed",
            "version": "1.16.1",
            "error": {"code": code, "message": message},
        }
    }))
    .into_response()
}

fn nd_albums() -> Vec<Value> {
    vec![
        json!({
            "id": "nd-al-1", "name": ALBUM_ONE, "artist": ARTIST_ONE, "artistId": "nd-ar-1",
            "year": 2024, "genre": GENRE, "songCount": 2, "duration": 360,
            "coverArt": "nd-al-1", "musicBrainzId": MATCH_MBID,
        }),
        json!({
            "id": "nd-al-2", "name": ALBUM_TWO, "artist": ARTIST_ONE, "artistId": "nd-ar-1",
            "year": 2022, "genre": GENRE, "songCount": 1, "duration": 180,
            "coverArt": "nd-al-2", "musicBrainzId": "",
        }),
        json!({
            "id": "nd-al-3", "name": ALBUM_THREE, "artist": ARTIST_TWO, "artistId": "nd-ar-2",
            "year": 2020, "genre": "Ambient", "songCount": 1, "duration": 200,
            "coverArt": "nd-al-3", "musicBrainzId": "",
        }),
    ]
}

fn nd_songs() -> Vec<Value> {
    vec![
        json!({
            "id": "nd-t-1", "title": "Meridian Dawn", "album": ALBUM_ONE, "albumId": "nd-al-1",
            "artist": ARTIST_ONE, "artistId": "nd-ar-1", "track": 1, "discNumber": 1,
            "year": 2024, "duration": 180, "suffix": "flac", "coverArt": "nd-al-1",
            "musicBrainzId": "22222222-2222-2222-2222-222222222222",
        }),
        json!({
            "id": "nd-t-2", "title": "Meridian Dusk", "album": ALBUM_ONE, "albumId": "nd-al-1",
            "artist": ARTIST_ONE, "artistId": "nd-ar-1", "track": 2, "discNumber": 1,
            "year": 2024, "duration": 180, "suffix": "flac", "coverArt": "nd-al-1",
            "musicBrainzId": "",
        }),
        json!({
            "id": "nd-t-3", "title": "Tide Glass", "album": ALBUM_TWO, "albumId": "nd-al-2",
            "artist": ARTIST_ONE, "artistId": "nd-ar-1", "track": 1, "discNumber": 1,
            "year": 2022, "duration": 180, "suffix": "mp3", "coverArt": "nd-al-2",
            "musicBrainzId": "",
        }),
        json!({
            "id": "nd-t-4", "title": "Satellite Fold", "album": ALBUM_THREE, "albumId": "nd-al-3",
            "artist": ARTIST_TWO, "artistId": "nd-ar-2", "track": 1, "discNumber": 1,
            "year": 2020, "duration": 200, "suffix": "mp3", "coverArt": "nd-al-3",
            "musicBrainzId": "",
        }),
    ]
}

fn nd_artists() -> Vec<Value> {
    vec![
        json!({"id": "nd-ar-1", "name": ARTIST_ONE, "albumCount": 2, "coverArt": "nd-ar-1"}),
        json!({"id": "nd-ar-2", "name": ARTIST_TWO, "albumCount": 1, "coverArt": "nd-ar-2"}),
    ]
}

async fn nd_handle(
    State(recorder): State<MockRecorder>,
    Path(endpoint): Path<String>,
    Query(params): Query<Vec<(String, String)>>,
) -> Response {
    let folders: Vec<String> = params
        .iter()
        .filter(|(key, _)| key == "musicFolderId")
        .map(|(_, value)| value.clone())
        .collect();
    recorder.record(|calls| {
        calls
            .navidrome_folder_params
            .entry(endpoint.clone())
            .or_default()
            .push(folders);
        calls
            .navidrome_queries
            .push((endpoint.clone(), params.clone()));
    });
    let query: HashMap<&str, &str> = params
        .iter()
        .map(|(key, value)| (key.as_str(), value.as_str()))
        .collect();
    if query.get("u") != Some(&NAVIDROME_USER)
        || !query.contains_key("t")
        || !query.contains_key("s")
        || !query.contains_key("v")
        || !query.contains_key("c")
        || !query.contains_key("f")
    {
        return nd_failure(41, "Wrong username or password");
    }
    // The 0.62.0 probe shape: one folder, repeated ids accepted, unknown
    // ids ignored for catalog endpoints.
    match endpoint.as_str() {
        "ping" => nd_envelope(json!({})),
        "getMusicFolders" => nd_envelope(json!({
            "musicFolders": {"musicFolder": [{"id": "folder-1", "name": "Library"}]}
        })),
        "getArtists" => nd_envelope(json!({
            "artists": {"index": [
                {"name": "A", "artist": [nd_artists()[0].clone()]},
                {"name": "B", "artist": [nd_artists()[1].clone()]},
            ]}
        })),
        "getAlbumList2" => {
            let list_type = query.get("type").unwrap_or(&"alphabeticalByName");
            let mut albums = nd_albums();
            match *list_type {
                "byGenre" => {
                    let genre = query.get("genre").copied().unwrap_or("");
                    albums
                        .retain(|album| album.get("genre").and_then(Value::as_str) == Some(genre));
                }
                "byYear" => {
                    let from = query
                        .get("fromYear")
                        .and_then(|value| value.parse::<i64>().ok())
                        .unwrap_or(0);
                    let to = query
                        .get("toYear")
                        .and_then(|value| value.parse::<i64>().ok())
                        .unwrap_or(9999);
                    let (low, high) = (from.min(to), from.max(to));
                    albums.retain(|album| {
                        album
                            .get("year")
                            .and_then(Value::as_i64)
                            .map(|year| year >= low && year <= high)
                            .unwrap_or(false)
                    });
                }
                "recent" | "newest" => {
                    albums = vec![albums[0].clone()];
                }
                _ => {}
            }
            let size = query
                .get("size")
                .and_then(|value| value.parse::<usize>().ok())
                .unwrap_or(20);
            let offset = query
                .get("offset")
                .and_then(|value| value.parse::<usize>().ok())
                .unwrap_or(0);
            let page: Vec<Value> = albums.into_iter().skip(offset).take(size).collect();
            nd_envelope(json!({"albumList2": {"album": page}}))
        }
        "getAlbum" => {
            let id = query.get("id").copied().unwrap_or("");
            match nd_albums()
                .into_iter()
                .find(|album| album.get("id").and_then(Value::as_str) == Some(id))
            {
                Some(mut album) => {
                    let songs: Vec<Value> = nd_songs()
                        .into_iter()
                        .filter(|song| song.get("albumId").and_then(Value::as_str) == Some(id))
                        .collect();
                    if let Some(map) = album.as_object_mut() {
                        map.insert("song".to_owned(), Value::Array(songs));
                    }
                    nd_envelope(json!({"album": album}))
                }
                None => nd_envelope(json!({})),
            }
        }
        "getArtist" => {
            let id = query.get("id").copied().unwrap_or("");
            match nd_artists()
                .into_iter()
                .find(|artist| artist.get("id").and_then(Value::as_str) == Some(id))
            {
                Some(artist) => nd_envelope(json!({"artist": artist})),
                None => nd_envelope(json!({})),
            }
        }
        "getSong" => {
            let id = query.get("id").copied().unwrap_or("");
            match nd_songs()
                .into_iter()
                .find(|song| song.get("id").and_then(Value::as_str) == Some(id))
            {
                Some(song) => nd_envelope(json!({"song": song})),
                None => nd_envelope(json!({})),
            }
        }
        "search3" => {
            let text = query.get("query").copied().unwrap_or("").to_owned();
            let lowered = text.to_lowercase();
            let artist_count = query
                .get("artistCount")
                .and_then(|value| value.parse::<usize>().ok())
                .unwrap_or(20);
            let album_count = query
                .get("albumCount")
                .and_then(|value| value.parse::<usize>().ok())
                .unwrap_or(20);
            let song_count = query
                .get("songCount")
                .and_then(|value| value.parse::<usize>().ok())
                .unwrap_or(20);
            let song_offset = query
                .get("songOffset")
                .and_then(|value| value.parse::<usize>().ok())
                .unwrap_or(0);
            // The empty-browse spelling '""' matches every song.
            let match_all_songs = text == "\"\"";
            let artists: Vec<Value> = nd_artists()
                .into_iter()
                .filter(|artist| {
                    lowered.is_empty()
                        || artist
                            .get("name")
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .to_lowercase()
                            .contains(&lowered)
                })
                .take(artist_count)
                .collect();
            let albums: Vec<Value> = nd_albums()
                .into_iter()
                .filter(|album| {
                    lowered.is_empty()
                        || album
                            .get("name")
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .to_lowercase()
                            .contains(&lowered)
                        || album.get("musicBrainzId").and_then(Value::as_str) == Some(text.as_str())
                })
                .take(album_count)
                .collect();
            let songs: Vec<Value> = nd_songs()
                .into_iter()
                .filter(|song| {
                    match_all_songs
                        || lowered.is_empty()
                        || song
                            .get("title")
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .to_lowercase()
                            .contains(&lowered)
                })
                .skip(song_offset)
                .take(song_count)
                .collect();
            let mut result = json!({});
            if artist_count > 0 {
                result["artist"] = Value::Array(artists);
            }
            if album_count > 0 {
                result["album"] = Value::Array(albums);
            }
            if song_count > 0 {
                result["song"] = Value::Array(songs);
            }
            nd_envelope(json!({"searchResult3": result}))
        }
        "getStarred2" => nd_envelope(json!({"starred2": {
            "artist": [nd_artists()[0].clone()],
            "album": [nd_albums()[0].clone()],
            "song": [nd_songs()[0].clone()],
        }})),
        "getGenres" => nd_envelope(json!({"genres": {"genre": [
            {"value": GENRE, "songCount": 3, "albumCount": 2},
            {"value": "Ambient", "songCount": 1, "albumCount": 1},
        ]}})),
        "getSongsByGenre" => {
            let genre = query.get("genre").copied().unwrap_or("");
            let count = query
                .get("count")
                .and_then(|value| value.parse::<usize>().ok())
                .unwrap_or(50);
            let offset = query
                .get("offset")
                .and_then(|value| value.parse::<usize>().ok())
                .unwrap_or(0);
            let songs: Vec<Value> = nd_songs()
                .into_iter()
                .filter(|song| {
                    let album_id = song.get("albumId").and_then(Value::as_str).unwrap_or("");
                    nd_albums().into_iter().any(|album| {
                        album.get("id").and_then(Value::as_str) == Some(album_id)
                            && album.get("genre").and_then(Value::as_str) == Some(genre)
                    })
                })
                .skip(offset)
                .take(count)
                .collect();
            nd_envelope(json!({"songsByGenre": {"song": songs}}))
        }
        "getPlaylists" => nd_envelope(json!({"playlists": {"playlist": [nd_playlist_meta()]}})),
        "getPlaylist" => {
            let id = query.get("id").copied().unwrap_or("");
            if id != "nd-pl-1" {
                return nd_envelope(json!({}));
            }
            let mut playlist = nd_playlist_meta();
            if let Some(map) = playlist.as_object_mut() {
                map.insert(
                    "entry".to_owned(),
                    Value::Array(vec![nd_songs()[0].clone(), nd_songs()[1].clone()]),
                );
            }
            nd_envelope(json!({"playlist": playlist}))
        }
        "getRandomSongs" => nd_envelope(json!({"randomSongs": {"song": [nd_songs()[2].clone()]}})),
        "getTopSongs" => nd_envelope(json!({"topSongs": {"song": [nd_songs()[0].clone()]}})),
        "getSimilarSongs2" => {
            nd_envelope(json!({"similarSongs2": {"song": [nd_songs()[1].clone()]}}))
        }
        "getArtistInfo2" => nd_envelope(json!({"artistInfo2": {
            "biography": format!("{ARTIST_ONE} shapes neon-lit instrumentals."),
            "musicBrainzId": "33333333-3333-3333-3333-333333333333",
            "largeImageUrl": "https://example.invalid/ar-1.jpg",
            "similarArtist": [nd_artists()[1].clone()],
        }})),
        "getAlbumInfo2" => nd_envelope(json!({"albumInfo": {
            "notes": format!("{ALBUM_ONE} was recorded at dawn."),
            "musicBrainzId": MATCH_MBID,
            "lastFmUrl": "https://example.invalid/al-1",
            "largeImageUrl": "https://example.invalid/al-1.jpg",
        }})),
        "getLyrics" => nd_envelope(json!({"lyrics": {
            "artist": ARTIST_ONE,
            "title": "Meridian Dawn",
            "value": "dawn over the meridian\nneon on the water",
        }})),
        "getLyricsBySongId" => {
            let id = query.get("id").copied().unwrap_or("");
            if id != "nd-t-1" {
                return nd_envelope(json!({}));
            }
            nd_envelope(json!({"lyricsList": {"structuredLyrics": [{
                "synced": true,
                "line": [
                    {"value": "dawn over the meridian", "start": 0},
                    {"value": "neon on the water", "start": 3000},
                ],
            }]}}))
        }
        "getNowPlaying" => nd_envelope(json!({"nowPlaying": {"entry": [{
            "id": "nd-t-1", "title": "Meridian Dawn", "artist": ARTIST_ONE,
            "album": ALBUM_ONE, "albumId": "nd-al-1", "artistId": "nd-ar-1",
            "coverArt": "nd-al-1", "duration": 180,
            "username": "Listener", "minutesAgo": 1, "playerId": 7, "playerName": "Symfonium",
        }]}})),
        "scrobble" => nd_envelope(json!({})),
        _ => nd_envelope(json!({})),
    }
}

fn nd_playlist_meta() -> Value {
    json!({
        "id": "nd-pl-1", "name": "Road Mix", "songCount": 2, "duration": 360,
        "owner": "Listener", "public": false,
        "created": "2026-01-01", "changed": "2026-02-01", "coverArt": "nd-pl-1",
    })
}

async fn nd_cover(
    State(recorder): State<MockRecorder>,
    Query(params): Query<Vec<(String, String)>>,
) -> Response {
    let query: HashMap<&str, &str> = params
        .iter()
        .map(|(key, value)| (key.as_str(), value.as_str()))
        .collect();
    if query.get("u") != Some(&NAVIDROME_USER) {
        return nd_failure(41, "Wrong username or password");
    }
    let _ = recorder;
    image_bytes()
}

/// Direct audio bytes for one song id. Auth mirrors `nd_handle`; an
/// unknown id 404s so gateway NotFound paths stay testable.
async fn nd_stream(Query(params): Query<Vec<(String, String)>>) -> Response {
    let query: HashMap<&str, &str> = params
        .iter()
        .map(|(key, value)| (key.as_str(), value.as_str()))
        .collect();
    if query.get("u") != Some(&NAVIDROME_USER)
        || !query.contains_key("t")
        || !query.contains_key("s")
        || !query.contains_key("v")
        || !query.contains_key("c")
        || !query.contains_key("f")
    {
        return nd_failure(41, "Wrong username or password");
    }
    let known = nd_songs()
        .iter()
        .any(|song| song.get("id").and_then(Value::as_str) == query.get("id").copied());
    if !known {
        return (StatusCode::NOT_FOUND, Json(json!({}))).into_response();
    }
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "audio/mpeg")],
        Body::from(canned_stream_bytes(b'N')),
    )
        .into_response()
}

// ---------------------------------------------------------------------------
// Plex mock
// ---------------------------------------------------------------------------

fn plex_router(recorder: MockRecorder) -> axum::Router {
    axum::Router::new()
        .route("/", get(px_root))
        .route("/identity", get(px_identity))
        .route("/api/v2/resources", get(px_resources))
        .route("/library/sections", get(px_sections))
        .route("/library/sections/{id}/all", get(px_section_all))
        .route("/library/sections/{id}/recentlyAdded", get(px_recently))
        .route("/library/sections/{id}/recentlyViewed", get(px_recently))
        .route("/library/sections/{id}/genre", get(px_taxonomy))
        .route("/library/sections/{id}/mood", get(px_taxonomy))
        .route("/library/metadata/{id}", get(px_metadata))
        .route("/library/metadata/{id}/children", get(px_children))
        .route("/library/metadata/{id}/thumb", get(px_thumb))
        .route("/playlists", get(px_playlists))
        .route("/playlists/{id}/items", get(px_playlist_items))
        .route("/playlists/{id}/composite", get(px_composite))
        .route(
            "/playlists/px-pl-1/composite/custom",
            get(px_custom_composite),
        )
        .route("/hubs/search", get(px_search))
        .route("/hubs/sections/{id}", get(px_hubs))
        .route("/status/sessions", get(px_sessions))
        .route("/status/sessions/history/all", get(px_history))
        .route("/library/parts/{*key}", get(px_part))
        .with_state(recorder)
}

fn px_authorized(headers: &HeaderMap, recorder: &MockRecorder) -> bool {
    let seen = headers
        .get("X-Plex-Token")
        .and_then(|value| value.to_str().ok())
        .unwrap_or("")
        .to_owned();
    recorder.record(|calls| calls.plex_tokens.push(seen.clone()));
    seen == PLEX_TOKEN
}

fn px_unauthorized() -> Response {
    (StatusCode::UNAUTHORIZED, Json(json!({}))).into_response()
}

fn px_container(payload: Value) -> Response {
    Json(json!({"MediaContainer": payload})).into_response()
}

#[allow(clippy::too_many_arguments)]
fn px_album(
    key: &str,
    title: &str,
    artist: &str,
    artist_key: &str,
    year: i64,
    leaf: i64,
    mbid: Option<&str>,
    added: i64,
    viewed: i64,
) -> Value {
    let guids: Vec<Value> = mbid
        .map(|mbid| vec![json!({"id": format!("mbid://{mbid}")})])
        .unwrap_or_default();
    json!({
        "ratingKey": key, "title": title, "type": "album",
        "parentTitle": artist, "parentRatingKey": artist_key,
        "year": year, "leafCount": leaf, "Guid": guids,
        "Genre": [{"tag": GENRE}],
        "thumb": format!("/library/metadata/{key}/thumb"),
        "addedAt": added, "lastViewedAt": viewed,
    })
}

fn px_albums(section: &str) -> Vec<Value> {
    match section {
        "2" => vec![px_album(
            "px-al-3",
            ALBUM_THREE,
            ARTIST_TWO,
            "px-ar-2",
            2020,
            1,
            None,
            300,
            30,
        )],
        _ => vec![
            px_album(
                "px-al-1",
                ALBUM_ONE,
                ARTIST_ONE,
                "px-ar-1",
                2024,
                2,
                Some(MATCH_MBID),
                100,
                90,
            ),
            px_album(
                "px-al-2", ALBUM_TWO, ARTIST_ONE, "px-ar-1", 2022, 1, None, 200, 10,
            ),
        ],
    }
}

fn px_artists(section: &str) -> Vec<Value> {
    match section {
        "2" => vec![json!({
            "ratingKey": "px-ar-2", "title": ARTIST_TWO, "type": "artist",
            "thumb": "/library/metadata/px-ar-2/thumb", "addedAt": 300, "Guid": [],
        })],
        _ => vec![json!({
            "ratingKey": "px-ar-1", "title": ARTIST_ONE, "type": "artist",
            "thumb": "/library/metadata/px-ar-1/thumb", "addedAt": 100, "Guid": [],
        })],
    }
}

fn px_track(
    key: &str,
    title: &str,
    album: &str,
    album_key: &str,
    artist: &str,
    index: i64,
) -> Value {
    json!({
        "ratingKey": key, "title": title, "type": "track",
        "parentTitle": album, "parentRatingKey": album_key,
        "grandparentTitle": artist, "index": index, "parentIndex": 1,
        "duration": 180_000, "addedAt": 100, "Guid": [],
        "Media": [{"id": 1, "duration": 180_000, "bitrate": 800,
                   "audioCodec": "flac", "audioChannels": 2, "container": "flac",
                   "Part": [{"id": 11, "key": format!("/library/parts/{key}/file.flac"),
                              "duration": 180_000, "container": "flac"}]}],
    })
}

fn px_tracks(section: &str) -> Vec<Value> {
    match section {
        "2" => vec![px_track(
            "px-t-4",
            "Satellite Fold",
            ALBUM_THREE,
            "px-al-3",
            ARTIST_TWO,
            1,
        )],
        _ => vec![
            px_track(
                "px-t-1",
                "Meridian Dawn",
                ALBUM_ONE,
                "px-al-1",
                ARTIST_ONE,
                1,
            ),
            px_track(
                "px-t-2",
                "Meridian Dusk",
                ALBUM_ONE,
                "px-al-1",
                ARTIST_ONE,
                2,
            ),
            px_track("px-t-3", "Tide Glass", ALBUM_TWO, "px-al-2", ARTIST_ONE, 1),
        ],
    }
}

async fn px_root(State(recorder): State<MockRecorder>, headers: HeaderMap) -> Response {
    if !px_authorized(&headers, &recorder) {
        return px_unauthorized();
    }
    px_container(json!({"friendlyName": "Mock Plex", "version": "1.41.0", "size": 0}))
}

/// A real server answers `/identity` without a token.
async fn px_identity() -> Response {
    px_container(json!({"machineIdentifier": PLEX_MACHINE_ID, "version": "1.41.0"}))
}

/// plex.tv `/resources` for [`PLEX_ACCOUNT_TOKEN`]: a player without a
/// token and the mock server with [`PLEX_TOKEN`].
async fn px_resources(headers: HeaderMap) -> Response {
    let token = headers
        .get("X-Plex-Token")
        .and_then(|value| value.to_str().ok());
    if token != Some(PLEX_ACCOUNT_TOKEN) {
        return px_unauthorized();
    }
    Json(json!([
        {"clientIdentifier": "phone-1", "provides": "player"},
        {"clientIdentifier": PLEX_MACHINE_ID, "provides": "server", "accessToken": PLEX_TOKEN}
    ]))
    .into_response()
}

async fn px_sections(State(recorder): State<MockRecorder>, headers: HeaderMap) -> Response {
    if !px_authorized(&headers, &recorder) {
        return px_unauthorized();
    }
    px_container(json!({"Directory": [
        {"key": "1", "title": "Music", "type": "artist", "uuid": "uuid-1"},
        {"key": "2", "title": "More Music", "type": "artist", "uuid": "uuid-2"},
        {"key": "3", "title": "Movies", "type": "movie", "uuid": "uuid-3"},
    ]}))
}

async fn px_section_all(
    State(recorder): State<MockRecorder>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Query(params): Query<Vec<(String, String)>>,
) -> Response {
    if !px_authorized(&headers, &recorder) {
        return px_unauthorized();
    }
    recorder.record(|calls| {
        calls
            .plex_section_queries
            .push((format!("/library/sections/{id}/all"), params.clone()));
    });
    let query: HashMap<&str, &str> = params
        .iter()
        .map(|(key, value)| (key.as_str(), value.as_str()))
        .collect();
    let mut items = match query.get("type") {
        Some(&"8") => px_artists(&id),
        Some(&"10") => px_tracks(&id),
        _ => px_albums(&id),
    };
    if let Some(title) = query.get("title") {
        items.retain(|item| {
            item.get("title")
                .and_then(Value::as_str)
                .unwrap_or("")
                .contains(title)
        });
    }
    if let Some(genre) = query.get("genre") {
        items.retain(|item| {
            item.get("Genre")
                .and_then(Value::as_array)
                .map(|tags| {
                    tags.iter()
                        .any(|tag| tag.get("tag").and_then(Value::as_str) == Some(genre))
                })
                .unwrap_or(false)
        });
    }
    if let Some(years) = query.get("year") {
        let wanted: Vec<i64> = years
            .split(',')
            .filter_map(|year| year.parse().ok())
            .collect();
        items.retain(|item| {
            item.get("year")
                .and_then(Value::as_i64)
                .map(|year| wanted.contains(&year))
                .unwrap_or(false)
        });
    }
    let total = items.len();
    let start = query
        .get("X-Plex-Container-Start")
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(0);
    let size = query
        .get("X-Plex-Container-Size")
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(total);
    let page: Vec<Value> = items.into_iter().skip(start).take(size).collect();
    let size = page.len();
    px_container(json!({"Metadata": page, "size": size, "totalSize": total}))
}

async fn px_recently(
    State(recorder): State<MockRecorder>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    if !px_authorized(&headers, &recorder) {
        return px_unauthorized();
    }
    px_container(json!({"Metadata": px_albums(&id), "totalSize": px_albums(&id).len()}))
}

async fn px_taxonomy(
    State(recorder): State<MockRecorder>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    if !px_authorized(&headers, &recorder) {
        return px_unauthorized();
    }
    let _ = id;
    px_container(json!({"Directory": [{"title": GENRE}, {"title": "Ambient"}]}))
}

fn px_known(id: &str) -> Option<Value> {
    for section in ["1", "2"] {
        for item in px_albums(section)
            .into_iter()
            .chain(px_artists(section))
            .chain(px_tracks(section))
        {
            if item.get("ratingKey").and_then(Value::as_str) == Some(id) {
                return Some(item);
            }
        }
    }
    None
}

async fn px_metadata(
    State(recorder): State<MockRecorder>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    if !px_authorized(&headers, &recorder) {
        return px_unauthorized();
    }
    match px_known(&id) {
        Some(item) => px_container(json!({"Metadata": [item]})),
        None => px_container(json!({"Metadata": []})),
    }
}

async fn px_children(
    State(recorder): State<MockRecorder>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    if !px_authorized(&headers, &recorder) {
        return px_unauthorized();
    }
    let tracks: Vec<Value> = ["1", "2"]
        .into_iter()
        .flat_map(px_tracks)
        .filter(|track| track.get("parentRatingKey").and_then(Value::as_str) == Some(id.as_str()))
        .collect();
    px_container(json!({"Metadata": tracks}))
}

async fn px_thumb(
    State(recorder): State<MockRecorder>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    if !px_authorized(&headers, &recorder) {
        return px_unauthorized();
    }
    if px_known(&id).is_none() && id != "thumb-check" {
        return StatusCode::NOT_FOUND.into_response();
    }
    image_bytes()
}

fn px_playlist_list() -> Vec<Value> {
    vec![
        json!({
            "ratingKey": "px-pl-1", "title": "Road Mix", "leafCount": 2,
            "duration": 360_000, "playlistType": "audio", "smart": false,
            "updatedAt": 400, "composite": "/playlists/px-pl-1/composite/custom",
        }),
        json!({
            "ratingKey": "px-pl-2", "title": "Quiet Mix", "leafCount": 1,
            "duration": 180_000, "playlistType": "audio", "smart": false,
            "updatedAt": 100, "composite": "",
        }),
    ]
}

async fn px_playlists(State(recorder): State<MockRecorder>, headers: HeaderMap) -> Response {
    if !px_authorized(&headers, &recorder) {
        return px_unauthorized();
    }
    px_container(json!({"Metadata": px_playlist_list()}))
}

async fn px_playlist_items(
    State(recorder): State<MockRecorder>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    if !px_authorized(&headers, &recorder) {
        return px_unauthorized();
    }
    let tracks = match id.as_str() {
        "px-pl-1" => vec![px_tracks("1")[0].clone(), px_tracks("1")[1].clone()],
        "px-pl-2" => vec![px_tracks("1")[2].clone()],
        _ => return px_container(json!({"Metadata": []})),
    };
    px_container(json!({"Metadata": tracks}))
}

async fn px_composite(State(recorder): State<MockRecorder>, headers: HeaderMap) -> Response {
    if !px_authorized(&headers, &recorder) {
        return px_unauthorized();
    }
    // Fallback composite path: distinct trailing marker so the test can
    // tell it apart from a playlist's own composite path.
    let bytes = vec![0xFF, 0xD8, 0xFF, 0xD9, 0x02];
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "image/jpeg")],
        Body::from(bytes),
    )
        .into_response()
}

async fn px_custom_composite(State(recorder): State<MockRecorder>, headers: HeaderMap) -> Response {
    if !px_authorized(&headers, &recorder) {
        return px_unauthorized();
    }
    let bytes = vec![0xFF, 0xD8, 0xFF, 0xD9, 0x01];
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "image/jpeg")],
        Body::from(bytes),
    )
        .into_response()
}

async fn px_search(
    State(recorder): State<MockRecorder>,
    headers: HeaderMap,
    Query(params): Query<Vec<(String, String)>>,
) -> Response {
    if !px_authorized(&headers, &recorder) {
        return px_unauthorized();
    }
    let query: HashMap<&str, &str> = params
        .iter()
        .map(|(key, value)| (key.as_str(), value.as_str()))
        .collect();
    let text = query.get("query").copied().unwrap_or("").to_owned();
    let lowered = text.to_lowercase();
    let sections: Vec<&str> = match query.get("sectionId") {
        Some(section) => vec![section],
        None => vec!["1", "2"],
    };
    let mut albums = Vec::new();
    let mut tracks = Vec::new();
    let mut artists = Vec::new();
    for section in sections {
        for album in px_albums(section) {
            let title_hit = album
                .get("title")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_lowercase()
                .contains(&lowered);
            let mbid_hit = album
                .get("Guid")
                .and_then(Value::as_array)
                .map(|guids| {
                    guids.iter().any(|guid| {
                        guid.get("id").and_then(Value::as_str)
                            == Some(format!("mbid://{text}").as_str())
                    })
                })
                .unwrap_or(false);
            if title_hit || mbid_hit {
                albums.push(album);
            }
        }
        for track in px_tracks(section) {
            if track
                .get("title")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_lowercase()
                .contains(&lowered)
            {
                tracks.push(track);
            }
        }
        for artist in px_artists(section) {
            if artist
                .get("title")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_lowercase()
                .contains(&lowered)
            {
                artists.push(artist);
            }
        }
    }
    px_container(json!({"Hub": [
        {"type": "album", "Metadata": albums},
        {"type": "track", "Metadata": tracks},
        {"type": "artist", "Metadata": artists},
    ]}))
}

/// Section hubs: one album shelf plus one artist shelf so discovery
/// filtering (album-type only) stays testable.
async fn px_hubs(
    State(recorder): State<MockRecorder>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    if !px_authorized(&headers, &recorder) {
        return px_unauthorized();
    }
    let _ = id;
    px_container(json!({"Hub": [
        {"title": "Recommended for you", "type": "album", "Metadata": px_albums("1")},
        {"title": "Top artists", "type": "artist", "Metadata": px_artists("1")},
    ]}))
}

async fn px_sessions(State(recorder): State<MockRecorder>, headers: HeaderMap) -> Response {
    if !px_authorized(&headers, &recorder) {
        return px_unauthorized();
    }
    let mut track = px_tracks("1")[0].clone();
    if let Some(map) = track.as_object_mut() {
        map.insert("viewOffset".to_owned(), json!(45_000));
        map.insert("User".to_owned(), json!({"title": "Listener"}));
        map.insert(
            "Player".to_owned(),
            json!({"title": "Plexamp", "platform": "iOS", "state": "playing"}),
        );
        map.insert("Session".to_owned(), json!({"id": "px-session-1"}));
    }
    px_container(json!({"Metadata": [track]}))
}

async fn px_history(
    State(recorder): State<MockRecorder>,
    headers: HeaderMap,
    Query(params): Query<Vec<(String, String)>>,
) -> Response {
    if !px_authorized(&headers, &recorder) {
        return px_unauthorized();
    }
    let query: HashMap<&str, &str> = params
        .iter()
        .map(|(key, value)| (key.as_str(), value.as_str()))
        .collect();
    let mut first = px_tracks("1")[0].clone();
    if let Some(map) = first.as_object_mut() {
        map.insert("viewedAt".to_owned(), json!(1_700_000_200));
    }
    let mut second = px_tracks("1")[1].clone();
    if let Some(map) = second.as_object_mut() {
        map.insert("viewedAt".to_owned(), json!(1_700_000_100));
    }
    let entries = vec![first, second];
    let total = entries.len();
    let start = query
        .get("X-Plex-Container-Start")
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(0);
    let size = query
        .get("X-Plex-Container-Size")
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(total);
    let page: Vec<Value> = entries.into_iter().skip(start).take(size).collect();
    px_container(json!({"Metadata": page, "totalSize": total}))
}

/// Direct audio bytes for any part key. The adapter validates the key
/// shape before requesting, so the mock only checks the token.
async fn px_part(
    State(recorder): State<MockRecorder>,
    headers: HeaderMap,
    Path(key): Path<String>,
) -> Response {
    if !px_authorized(&headers, &recorder) {
        return px_unauthorized();
    }
    if key.trim().is_empty() {
        return (StatusCode::NOT_FOUND, Json(json!({}))).into_response();
    }
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "audio/mpeg")],
        Body::from(canned_stream_bytes(b'P')),
    )
        .into_response()
}
