//! In-repo mock slskd server (axum).
//!
//! Ported from v2's slskd mock: the executable record
//! of live-verified slskd 0.25.1 behavior. Shapes mirror the verified JSON
//! (camelCase keys, comma-joined `state` flags, plain-array enqueue
//! returning `{Enqueued, Failed}`, no batch GUID: correlation is by
//! `(username, filename)`). No test touches a live slskd instance; every
//! contract test runs against this on loopback.
//!
//! Beyond the v2 mock, this pins three more verified behaviors as
//! scriptable modes so the tests can assert them:
//!
//! - 429 single-op: one-shot flags make the next search or enqueue answer
//!   429 (slskd permits only one concurrent operation).
//! - Specific-query silence: a search text carrying a 4-digit year answers
//!   with zero peers, the live-verified "a specific query sometimes returns
//!   nothing when a broader one returns thousands" that justifies the query
//!   ladder (v2 `search_album`).
//! - Partial rejection: enqueued filenames containing `REJECT-ME` land in
//!   `Failed`, so the accepted-filenames correlation test can fail.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use axum::{
    Json,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{delete, get, post},
};
use serde_json::{Value, json};
use uuid::Uuid;

/// API key the mock accepts when strict mode is armed.
pub const MOCK_API_KEY: &str = "slskd-test-key";

/// Enqueued filenames containing this marker land in `Failed`.
pub const REJECT_MARKER: &str = "REJECT-ME";

#[derive(Debug, Clone)]
struct Transfer {
    id: String,
    username: String,
    filename: String,
    size: i64,
    bytes_transferred: i64,
    state: String,
    requested_at: Option<String>,
    started_at: Option<String>,
}

impl Transfer {
    fn completed(id: String, username: &str, filename: &str, size: i64) -> Self {
        Self {
            id,
            username: username.to_owned(),
            filename: filename.to_owned(),
            size,
            bytes_transferred: size,
            state: "Completed, Succeeded".to_owned(),
            requested_at: None,
            started_at: None,
        }
    }

    fn json(&self) -> Value {
        json!({
            "id": self.id,
            "username": self.username,
            "filename": self.filename,
            "size": self.size,
            "bytesTransferred": self.bytes_transferred,
            "bytesRemaining": (self.size - self.bytes_transferred).max(0),
            "percentComplete": 100.0,
            "averageSpeed": 1_000_000.0,
            "state": self.state,
            "direction": "Download",
            "requestedAt": self.requested_at,
            "startedAt": self.started_at,
        })
    }
}

#[derive(Debug, Default)]
struct MockState {
    /// search_id -> canned peer responses.
    searches: HashMap<String, Vec<Value>>,
    /// username -> transfers.
    transfers: HashMap<String, Vec<Transfer>>,
    /// Every search text received, in order.
    search_texts: Vec<String>,
    /// Every `searchTimeout` body value received, in order.
    search_timeouts: Vec<i64>,
    /// `None` preserves the legacy behavior where any non-empty key passes
    /// (v2 mock); `Some` pins the exact key the mock accepts, and any other
    /// non-empty key 401s like a wrong key or a key-CIDR deny (v2 #193).
    expected_api_key: Option<String>,
    /// One-shot 429s for the rate-limit tests.
    fail_next_search: bool,
    fail_next_enqueue: bool,
    /// Concurrent in-flight searches+enqueues right now, and the observed max.
    in_flight: usize,
    max_in_flight: usize,
    /// Peer responses every search answers instead of the canned three.
    scripted_responses: Option<Vec<Value>>,
    /// Artificial delay (ms) applied inside search/enqueue handlers so the
    /// semaphore test can observe overlap if serialization breaks.
    handler_delay_ms: u64,
}

#[derive(Debug, Clone)]
struct AppState {
    inner: Arc<Mutex<MockState>>,
}

fn require_api_key(headers: &HeaderMap, state: &AppState) -> Result<(), Box<Response>> {
    let key = headers
        .get("X-API-Key")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default();
    if key.is_empty() {
        return Err(Box::new(
            (
                StatusCode::UNAUTHORIZED,
                Json(json!({"detail": "X-API-Key header required"})),
            )
                .into_response(),
        ));
    }
    let guard = state
        .inner
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(expected) = &guard.expected_api_key
        && key != *expected
    {
        return Err(Box::new(
            (
                StatusCode::UNAUTHORIZED,
                Json(json!({"detail": "Invalid API key"})),
            )
                .into_response(),
        ));
    }
    Ok(())
}

fn file_entry(
    filename: &str,
    size: i64,
    ext: &str,
    bitrate: Option<i64>,
    bit_depth: Option<i64>,
    sample_rate: Option<i64>,
) -> Value {
    let mut entry = json!({
        "filename": filename,
        "size": size,
        "extension": ext,
        "length": 240.0,
        "code": 1,
        "isLocked": false,
    });
    if let Some(bitrate) = bitrate {
        entry["bitRate"] = json!(bitrate);
    }
    if let Some(depth) = bit_depth {
        entry["bitDepth"] = json!(depth);
    }
    if let Some(rate) = sample_rate {
        entry["sampleRate"] = json!(rate);
    }
    entry
}

/// Three peers: a complete FLAC album, a partial MP3 folder, and a junk
/// folder (v2 mock `_canned_responses`).
fn canned_responses() -> Vec<Value> {
    let alice_files: Vec<Value> = (1..=12)
        .map(|n: u32| {
            file_entry(
                &format!("@@music\\Radiohead - OK Computer (1997)\\{n:02} Track {n}.flac"),
                30_000_000,
                "",   // slskd omits extension for some files (v2 C6a)
                None, // lossless: bitRate absent (v2 C6b)
                Some(16),
                Some(44100),
            )
        })
        .collect();
    let bob_files: Vec<Value> = (1..=5)
        .map(|n: u32| {
            file_entry(
                &format!("/home/bob/Random Rips/{n:02} song.mp3"),
                8_000_000,
                "mp3",
                Some(320),
                None,
                None,
            )
        })
        .collect();
    let charlie_files = vec![file_entry(
        "/downloads/Various Artists - Unknown Album/track.mp3",
        7_000_000,
        "mp3",
        Some(128),
        None,
        None,
    )];
    vec![
        json!({
            "username": "alice",
            "hasFreeUploadSlot": true,
            "uploadSpeed": 2_000_000,
            "queueLength": 0,
            "fileCount": alice_files.len(),
            "lockedFileCount": 0,
            "files": alice_files,
            "lockedFiles": [],
            "token": 1,
        }),
        json!({
            "username": "bob",
            "hasFreeUploadSlot": false,
            "uploadSpeed": 500_000,
            "queueLength": 3,
            "fileCount": bob_files.len(),
            "lockedFileCount": 0,
            "files": bob_files,
            "lockedFiles": [],
            "token": 2,
        }),
        json!({
            "username": "charlie",
            "hasFreeUploadSlot": true,
            "uploadSpeed": 100_000,
            "queueLength": 0,
            "fileCount": charlie_files.len(),
            "lockedFileCount": 0,
            "files": charlie_files,
            "lockedFiles": [],
            "token": 3,
        }),
    ]
}

/// Whether a search text carries a 4-digit year (the specific rung of the
/// ladder). The mock answers those with zero peers.
fn carries_year(search_text: &str) -> bool {
    let chars: Vec<char> = search_text.chars().collect();
    chars
        .windows(4)
        .any(|window| window.iter().all(|ch| ch.is_ascii_digit()) && window[0] != '0')
}

async fn application(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(rejection) = require_api_key(&headers, &state) {
        return *rejection;
    }
    Json(json!({
        "version": {"current": "0.25.1.0", "latest": "0.25.1.0", "isUpdateAvailable": false},
        "server": {"state": "Connected, LoggedIn", "address": "vps.slsknet.org"},
        "shares": {"directories": 277},
    }))
    .into_response()
}

async fn options(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(rejection) = require_api_key(&headers, &state) {
        return *rejection;
    }
    Json(json!({
        "directories": {
            "downloads": "/slskd/downloads",
            "incomplete": "/slskd/incomplete",
        },
    }))
    .into_response()
}

async fn start_search(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    if let Err(rejection) = require_api_key(&headers, &state) {
        return *rejection;
    }
    let delay_ms = {
        let mut guard = state
            .inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if guard.fail_next_search {
            guard.fail_next_search = false;
            return (
                StatusCode::TOO_MANY_REQUESTS,
                Json(json!({"detail": "only one concurrent operation is permitted"})),
            )
                .into_response();
        }
        guard.in_flight += 1;
        guard.max_in_flight = guard.max_in_flight.max(guard.in_flight);
        guard.handler_delay_ms
    };
    if delay_ms > 0 {
        tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
    }
    let search_text = body
        .get("searchText")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let timeout_ms = body
        .get("searchTimeout")
        .and_then(Value::as_i64)
        .unwrap_or(-1);
    let search_id = Uuid::new_v4().simple().to_string();
    let (file_count, response_count) = {
        let mut guard = state
            .inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        guard.in_flight -= 1;
        guard.search_texts.push(search_text.to_owned());
        guard.search_timeouts.push(timeout_ms);
        // Specific-query silence: a year-carrying query answers zero peers
        // (v2 `search_album`: a specific query sometimes returns nothing
        // when a broader one returns thousands; verified live).
        let responses = if carries_year(search_text) {
            Vec::new()
        } else {
            guard
                .scripted_responses
                .clone()
                .unwrap_or_else(canned_responses)
        };
        let file_count: usize = responses
            .iter()
            .map(|peer| peer["fileCount"].as_u64().unwrap_or(0) as usize)
            .sum();
        let response_count = responses.len();
        guard.searches.insert(search_id.clone(), responses);
        (file_count, response_count)
    };
    Json(json!({
        "id": search_id,
        "searchText": search_text,
        "state": "Completed, Succeeded",
        "isComplete": true,
        "fileCount": file_count,
        "responseCount": response_count,
        "lockedFileCount": 0,
        "token": 12345,
    }))
    .into_response()
}

async fn search_state(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(search_id): Path<String>,
) -> Response {
    if let Err(rejection) = require_api_key(&headers, &state) {
        return *rejection;
    }
    let guard = state
        .inner
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    match guard.searches.get(&search_id) {
        None => (
            StatusCode::NOT_FOUND,
            Json(json!({"detail": "search not found"})),
        )
            .into_response(),
        Some(responses) => Json(json!({
            "id": search_id,
            "state": "Completed, Succeeded",
            "isComplete": true,
            "responseCount": responses.len(),
        }))
        .into_response(),
    }
}

async fn search_responses(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(search_id): Path<String>,
) -> Response {
    if let Err(rejection) = require_api_key(&headers, &state) {
        return *rejection;
    }
    let guard = state
        .inner
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    match guard.searches.get(&search_id) {
        None => (
            StatusCode::NOT_FOUND,
            Json(json!({"detail": "search not found"})),
        )
            .into_response(),
        Some(responses) => Json(responses.clone()).into_response(),
    }
}

/// Plain array body `[{filename, size}]` -> 201 `{Enqueued, Failed}` (v2).
async fn enqueue(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(username): Path<String>,
    Json(files): Json<Vec<Value>>,
) -> Response {
    if let Err(rejection) = require_api_key(&headers, &state) {
        return *rejection;
    }
    let delay_ms = {
        let mut guard = state
            .inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if guard.fail_next_enqueue {
            guard.fail_next_enqueue = false;
            return (
                StatusCode::TOO_MANY_REQUESTS,
                Json(json!({"detail": "only one concurrent operation is permitted"})),
            )
                .into_response();
        }
        guard.in_flight += 1;
        guard.max_in_flight = guard.max_in_flight.max(guard.in_flight);
        guard.handler_delay_ms
    };
    if delay_ms > 0 {
        tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
    }
    let mut enqueued = Vec::new();
    let mut failed = Vec::new();
    {
        let mut guard = state
            .inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        guard.in_flight -= 1;
        let bucket = guard.transfers.entry(username.clone()).or_default();
        for entry in &files {
            let filename = entry
                .get("filename")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let size = entry.get("size").and_then(Value::as_i64).unwrap_or(0);
            if filename.contains(REJECT_MARKER) {
                failed.push(json!({"filename": filename, "size": size}));
                continue;
            }
            bucket.push(Transfer::completed(
                Uuid::new_v4().simple().to_string(),
                &username,
                filename,
                size,
            ));
            enqueued.push(json!({"filename": filename, "size": size}));
        }
    }
    (
        StatusCode::CREATED,
        Json(json!({"Enqueued": enqueued, "Failed": failed})),
    )
        .into_response()
}

/// Per-user transfers grouped as directories -> files (slskd shape, v2).
async fn user_transfers(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(username): Path<String>,
) -> Response {
    if let Err(rejection) = require_api_key(&headers, &state) {
        return *rejection;
    }
    let guard = state
        .inner
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    // Unknown users 404 like a missing bucket would; known-but-empty
    // buckets answer the empty grouped shape.
    if !guard.transfers.contains_key(&username) {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({"detail": "user not found"})),
        )
            .into_response();
    }
    let files = guard.transfers.get(&username).cloned().unwrap_or_default();
    let mut directories: HashMap<String, Vec<Value>> = HashMap::new();
    for transfer in &files {
        let parent = transfer
            .filename
            .replace('\\', "/")
            .rsplit_once('/')
            .map(|(parent, _)| parent.to_owned())
            .unwrap_or_default();
        directories.entry(parent).or_default().push(transfer.json());
    }
    let mut grouped: Vec<Value> = directories
        .into_iter()
        .map(|(directory, items)| {
            json!({"directory": directory, "fileCount": items.len(), "files": items})
        })
        .collect();
    grouped.sort_by(|left, right| left["directory"].as_str().cmp(&right["directory"].as_str()));
    Json(json!({"username": username, "directories": grouped})).into_response()
}

async fn all_transfers(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(rejection) = require_api_key(&headers, &state) {
        return *rejection;
    }
    let guard = state
        .inner
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let blocks: Vec<Value> = guard
        .transfers
        .iter()
        .map(|(username, files)| {
            json!({
                "username": username,
                "directories": [{
                    "directory": "",
                    "fileCount": files.len(),
                    "files": files.iter().map(Transfer::json).collect::<Vec<_>>(),
                }],
            })
        })
        .collect();
    Json(blocks).into_response()
}

async fn remove_transfer(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((username, transfer_id)): Path<(String, String)>,
    Query(_query): Query<HashMap<String, String>>,
) -> Response {
    if let Err(rejection) = require_api_key(&headers, &state) {
        return *rejection;
    }
    let mut guard = state
        .inner
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let before = guard.transfers.get(&username).map(Vec::len).unwrap_or(0);
    if let Some(bucket) = guard.transfers.get_mut(&username) {
        bucket.retain(|transfer| transfer.id != transfer_id);
    }
    let after = guard.transfers.get(&username).map(Vec::len).unwrap_or(0);
    if before == after {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({"detail": "transfer not found"})),
        )
            .into_response();
    }
    Json(json!({"removed": transfer_id})).into_response()
}

fn router(state: AppState) -> axum::Router {
    axum::Router::new()
        .route("/api/v0/application", get(application))
        .route("/api/v0/options", get(options))
        .route("/api/v0/searches", post(start_search))
        .route("/api/v0/searches/{id}", get(search_state))
        .route("/api/v0/searches/{id}/responses", get(search_responses))
        .route("/api/v0/transfers/downloads", get(all_transfers))
        .route(
            "/api/v0/transfers/downloads/{username}",
            post(enqueue).get(user_transfers),
        )
        .route(
            "/api/v0/transfers/downloads/{username}/{id}",
            delete(remove_transfer),
        )
        .with_state(state)
}

/// A running mock slskd server on loopback, with scriptable fault modes.
pub struct MockSlskd {
    state: AppState,
    addr: SocketAddr,
}

impl MockSlskd {
    /// Serve the mock on an ephemeral loopback port. No live contact ever.
    pub async fn start() -> Result<Self, String> {
        let state = AppState {
            inner: Arc::new(Mutex::new(MockState::default())),
        };
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .map_err(|error| format!("mock binds loopback: {error}"))?;
        let addr = listener
            .local_addr()
            .map_err(|error| format!("mock has an address: {error}"))?;
        let app = router(state.clone());
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        Ok(Self { state, addr })
    }

    #[must_use]
    pub fn base_url(&self) -> String {
        format!("http://{}", self.addr)
    }

    /// Pin the key the mock accepts; `None` restores accept-any-non-empty
    /// (v2 `set_expected_api_key`).
    pub fn set_expected_api_key(&self, key: Option<&str>) {
        self.state
            .inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .expected_api_key = key.map(str::to_owned);
    }

    /// Answer every search with these peer responses (slskd's
    /// `/searches/{id}/responses` shape) instead of the canned three.
    pub fn script_responses(&self, responses: Vec<Value>) {
        self.state
            .inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .scripted_responses = Some(responses);
    }

    /// The next search answers 429 once (slskd single-op limit).
    pub fn fail_next_search(&self) {
        self.state
            .inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .fail_next_search = true;
    }

    /// The next enqueue answers 429 once (slskd single-op limit).
    pub fn fail_next_enqueue(&self) {
        self.state
            .inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .fail_next_enqueue = true;
    }

    /// Delay (ms) inside search/enqueue handlers for the semaphore test.
    pub fn set_handler_delay_ms(&self, delay_ms: u64) {
        self.state
            .inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .handler_delay_ms = delay_ms;
    }

    /// Highest concurrent in-flight searches+enqueues observed so far.
    #[must_use]
    pub fn max_in_flight(&self) -> usize {
        self.state
            .inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .max_in_flight
    }

    /// Every search text received, in order.
    #[must_use]
    pub fn search_texts(&self) -> Vec<String> {
        self.state
            .inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .search_texts
            .clone()
    }

    /// Every `searchTimeout` body value received, in order.
    #[must_use]
    pub fn search_timeouts(&self) -> Vec<i64> {
        self.state
            .inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .search_timeouts
            .clone()
    }

    /// Inject a transfer record directly (for status-aggregation tests).
    pub fn inject_transfer(
        &self,
        username: &str,
        filename: &str,
        size: i64,
        state: &str,
        requested_at: Option<&str>,
        started_at: Option<&str>,
    ) {
        self.inject_transfer_progress(
            username,
            filename,
            size,
            size,
            state,
            requested_at,
            started_at,
        );
    }

    /// Inject a transfer record with explicit moved bytes (for the
    /// truncated-stub test, v2 #122).
    #[allow(clippy::too_many_arguments)]
    pub fn inject_transfer_progress(
        &self,
        username: &str,
        filename: &str,
        size: i64,
        bytes_transferred: i64,
        state: &str,
        requested_at: Option<&str>,
        started_at: Option<&str>,
    ) {
        let mut guard = self
            .state
            .inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut transfer = Transfer::completed(
            Uuid::new_v4().simple().to_string(),
            username,
            filename,
            size,
        );
        transfer.bytes_transferred = bytes_transferred;
        transfer.state = state.to_owned();
        transfer.requested_at = requested_at.map(str::to_owned);
        transfer.started_at = started_at.map(str::to_owned);
        guard
            .transfers
            .entry(username.to_owned())
            .or_default()
            .push(transfer);
    }

    /// Clear in-memory state between tests (v2 `reset_state`).
    pub fn reset(&self) {
        let mut guard = self
            .state
            .inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        guard.searches.clear();
        guard.transfers.clear();
        guard.search_texts.clear();
        guard.search_timeouts.clear();
        guard.expected_api_key = None;
        guard.fail_next_search = false;
        guard.fail_next_enqueue = false;
        guard.in_flight = 0;
        guard.max_in_flight = 0;
        guard.handler_delay_ms = 0;
    }
}
