//! Loopback SABnzbd / Newznab / Prowlarr servers for the contract tests.
//!
//! Ported from v2's SABnzbd, Newznab and Prowlarr test mocks: the same
//! feeds, the same
//! scenario indexers, the same recorded-request discipline, served over
//! real HTTP on `127.0.0.1` so the clients run their production reqwest
//! path end to end. Nothing here is reachable beyond loopback, and the
//! tests never contact the live network.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use axum::Router;
use axum::body::Bytes;
use axum::extract::{Query, RawQuery, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use serde_json::json;

// ---------------------------------------------------------------------------
// Shared plumbing.
// ---------------------------------------------------------------------------

/// Serve `router` on an ephemeral loopback port. Returns the base URL
/// (`http://127.0.0.1:{port}`); dropping the handle stops the server.
pub async fn serve_loopback(
    router: Router,
) -> Result<(String, tokio::task::JoinHandle<()>), String> {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .map_err(|error| format!("loopback bind: {error}"))?;
    let addr = listener
        .local_addr()
        .map_err(|error| format!("local addr: {error}"))?;
    let base = format!("http://{addr}");
    let handle = tokio::task::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });
    Ok((base, handle))
}

fn json_response(body: serde_json::Value) -> Response {
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "application/json")],
        body.to_string(),
    )
        .into_response()
}

fn xml_response(body: &str) -> Response {
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "application/rss+xml")],
        body.to_owned(),
    )
        .into_response()
}

// ---------------------------------------------------------------------------
// SABnzbd mock (v2 `sabnzbd_mock.py`).
// ---------------------------------------------------------------------------

/// One recorded `/api` call: raw query string (order-preserving, so the
/// tests can prove `output`/`apikey` ride last) plus parsed params.
#[derive(Debug, Clone)]
pub struct SabApiCall {
    /// Raw query string in arrival order.
    pub raw_query: String,
    /// Parsed params.
    pub params: Vec<(String, String)>,
    /// Multipart content type, for addfile calls.
    pub content_type: Option<String>,
    /// Raw request body (the multipart envelope for addfile).
    pub body: Vec<u8>,
}

/// Forced `/api` error form (v2's JSON + plain-text shapes).
#[derive(Debug, Clone)]
pub enum SabErrorForm {
    /// `{"status": false, "error": …}`.
    JsonFalse(String),
    /// `{"status": "False", "error": …}` (stringly boolean).
    JsonFalseString(String),
    /// Plain-text `error: …`.
    PlainText(String),
}

/// Mutable SABnzbd mock state, shared with the tests.
#[derive(Debug, Default)]
pub struct SabnzbdState {
    /// Queue slots served by `mode=queue`.
    pub queue_slots: Vec<serde_json::Value>,
    /// History slots served by `mode=history`.
    pub history_slots: Vec<serde_json::Value>,
    /// Categories served by `mode=get_cats`.
    pub categories: Vec<String>,
    /// `complete_dir` served by `mode=get_config`.
    pub complete_dir: String,
    /// `nzo_ids` served by add calls.
    pub add_nzo_ids: Vec<String>,
    /// Recorded addfile calls.
    pub add_file_requests: Vec<SabApiCall>,
    /// Recorded addurl calls.
    pub add_url_requests: Vec<SabApiCall>,
    /// `(mode, value)` pairs removed.
    pub deleted: Vec<(String, String)>,
    /// Recorded delete calls (full params).
    pub delete_requests: Vec<Vec<(String, String)>>,
    /// Failed-job storage removed with `del_files=1`.
    pub deleted_storage: Vec<String>,
    /// Completed storage SABnzbd retained despite `del_files=1` (5.0.4).
    pub retained_completed_storage: Vec<String>,
    /// Recorded non-delete history calls.
    pub history_requests: Vec<Vec<(String, String)>>,
    /// Every `/api` call in arrival order (suffix-order + retry-count tests).
    pub api_calls: Vec<SabApiCall>,
    /// Countdown of HTTP 500s served on `mode=queue` (retry test).
    pub queue_fail_500: usize,
    /// Countdown of HTTP 500s served on `mode=addfile` (no-retry test).
    pub addfile_fail_500: usize,
    /// Forced error form for every `/api` call, until cleared.
    pub api_error: Option<SabErrorForm>,
    /// When true, addurl rejects echoing the submitted URL (redaction test).
    pub addurl_echo_url: bool,
}

/// Handle to the shared mock state.
#[derive(Debug, Clone, Default)]
pub struct SabnzbdMock {
    state: Arc<Mutex<SabnzbdState>>,
}

impl SabnzbdMock {
    /// Fresh mock with v2's defaults (5.0.4-flavored categories + dirs).
    pub fn new() -> Self {
        SabnzbdMock {
            state: Arc::new(Mutex::new(SabnzbdState {
                categories: vec![
                    "*".to_owned(),
                    "movies".to_owned(),
                    "tv".to_owned(),
                    "audio".to_owned(),
                    "software".to_owned(),
                ],
                complete_dir: "/data/Downloads/complete".to_owned(),
                add_nzo_ids: vec!["nzo-test-1".to_owned()],
                ..SabnzbdState::default()
            })),
        }
    }

    /// Borrow the state for setup/assertion.
    pub fn state(&self) -> std::sync::MutexGuard<'_, SabnzbdState> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Add a queue job (stringly numbers, like the live instance).
    pub fn queue_job(
        &self,
        nzo_id: &str,
        name: &str,
        status: &str,
        mb: &str,
        mbleft: &str,
        percentage: &str,
    ) -> &Self {
        self.state().queue_slots.push(json!({
            "nzo_id": nzo_id, "filename": name, "status": status, "cat": "audio",
            "mb": mb, "mbleft": mbleft, "percentage": percentage,
            "timeleft": "0:00:00", "priority": "Normal",
        }));
        self
    }

    /// Add a history job (real-number bytes, like the live instance).
    pub fn history_job(
        &self,
        nzo_id: &str,
        name: &str,
        status: &str,
        storage: &str,
        bytes: u64,
        fail_message: &str,
    ) -> &Self {
        self.state().history_slots.push(json!({
            "nzo_id": nzo_id, "name": name, "nzb_name": format!("{name}.nzb"),
            "status": status, "category": "audio", "storage": storage,
            "bytes": bytes, "fail_message": fail_message,
            "password": null, "download_time": 100, "completed": 1,
        }));
        self
    }

    /// The router: `/api` plus the NZB fixtures `fetch_nzb` reads.
    pub fn router(&self) -> Router {
        Router::new()
            .route("/api", get(sab_api_get).post(sab_api_post))
            .route(
                "/nzb/good",
                get(|| async {
                    (
                        StatusCode::OK,
                        [(header::CONTENT_TYPE, "application/x-nzb")],
                        GOOD_NZB,
                    )
                }),
            )
            .route(
                "/nzb/errorpage",
                get(|| async {
                    (
                        StatusCode::OK,
                        [(header::CONTENT_TYPE, "text/html")],
                        "<html><body>API limit reached</body></html>",
                    )
                }),
            )
            .route(
                "/nzb/denied",
                get(|| async { (StatusCode::FORBIDDEN, "denied") }),
            )
            .with_state(self.clone())
    }
}

const GOOD_NZB: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<nzb xmlns="http://www.newznab.com/DTD/2010/nzb">
 <file poster="poster" date="1760000000" subject="test.part01.rar">
  <segments><segment bytes="100" number="1">seg1</segment></segments>
 </file>
</nzb>"#;

fn parse_query(raw: &str) -> Vec<(String, String)> {
    url_decode_pairs(raw)
}

/// Minimal query decoder (percent + `+`); enough for the mock's needs.
fn url_decode_pairs(raw: &str) -> Vec<(String, String)> {
    raw.split('&')
        .filter(|pair| !pair.is_empty())
        .map(|pair| {
            let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
            (url_decode(key), url_decode(value))
        })
        .collect()
}

fn url_decode(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let bytes = text.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'+' => {
                out.push(' ');
                index += 1;
            }
            b'%' if index + 2 < bytes.len() => {
                let hex = &text[index + 1..index + 3];
                if let Ok(byte) = u8::from_str_radix(hex, 16) {
                    out.push(byte as char);
                } else {
                    out.push_str(&text[index..index + 3]);
                }
                index += 3;
            }
            _ => {
                out.push(bytes[index] as char);
                index += 1;
            }
        }
    }
    out
}

fn param(params: &[(String, String)], key: &str) -> Option<String> {
    params
        .iter()
        .find(|(name, _)| name == key)
        .map(|(_, value)| value.clone())
}

async fn sab_api_get(
    State(mock): State<SabnzbdMock>,
    RawQuery(raw): RawQuery,
    headers: HeaderMap,
) -> Response {
    sab_api(mock, raw.unwrap_or_default(), headers, Vec::new()).await
}

async fn sab_api_post(
    State(mock): State<SabnzbdMock>,
    RawQuery(raw): RawQuery,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    sab_api(mock, raw.unwrap_or_default(), headers, body.to_vec()).await
}

async fn sab_api(
    mock: SabnzbdMock,
    raw_query: String,
    headers: HeaderMap,
    body: Vec<u8>,
) -> Response {
    let params = parse_query(&raw_query);
    let mode = param(&params, "mode").unwrap_or_default();
    let mut state = mock.state();
    state.api_calls.push(SabApiCall {
        raw_query: raw_query.clone(),
        params: params.clone(),
        content_type: headers
            .get(header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned),
        body: body.clone(),
    });
    if let Some(form) = state.api_error.clone() {
        return match form {
            SabErrorForm::JsonFalse(message) => {
                json_response(json!({"status": false, "error": message}))
            }
            SabErrorForm::JsonFalseString(message) => {
                json_response(json!({"status": "False", "error": message}))
            }
            SabErrorForm::PlainText(message) => (
                StatusCode::OK,
                [(header::CONTENT_TYPE, "text/plain")],
                format!("error: {message}"),
            )
                .into_response(),
        };
    }
    match mode.as_str() {
        "version" => json_response(json!({"version": "5.0.4"})),
        "get_cats" => json_response(json!({"categories": state.categories})),
        "get_config" => json_response(json!({
            "config": {
                "misc": {"complete_dir": state.complete_dir},
                "categories": state.categories.iter().map(|name| json!({"name": name})).collect::<Vec<_>>(),
            }
        })),
        "queue" => {
            if param(&params, "name").as_deref() == Some("delete") {
                let value = param(&params, "value").unwrap_or_default();
                state.deleted.push(("queue".to_owned(), value));
                state.delete_requests.push(params);
                return json_response(json!({"status": true}));
            }
            if state.queue_fail_500 > 0 {
                state.queue_fail_500 -= 1;
                return (StatusCode::INTERNAL_SERVER_ERROR, "blip").into_response();
            }
            let status = if state.queue_slots.is_empty() {
                "Idle"
            } else {
                "Downloading"
            };
            json_response(
                json!({"queue": {"status": status, "paused": false, "slots": state.queue_slots}}),
            )
        }
        "history" => {
            if param(&params, "name").as_deref() == Some("delete") {
                let value = param(&params, "value").unwrap_or_default();
                let del_files = param(&params, "del_files").as_deref() == Some("1");
                state.deleted.push(("history".to_owned(), value.clone()));
                state.delete_requests.push(params);
                if del_files
                    && let Some(slot) = state
                        .history_slots
                        .iter()
                        .find(|slot| slot["nzo_id"] == value)
                {
                    let storage = slot["storage"].as_str().unwrap_or("").to_owned();
                    if slot["status"]
                        .as_str()
                        .unwrap_or("")
                        .eq_ignore_ascii_case("failed")
                    {
                        state.deleted_storage.push(storage);
                    } else {
                        state.retained_completed_storage.push(storage);
                    }
                }
                state.history_slots.retain(|slot| slot["nzo_id"] != value);
                return json_response(json!({"status": true}));
            }
            state.history_requests.push(params.clone());
            let mut slots = state.history_slots.clone();
            if let Some(nzo) = param(&params, "nzo_ids") {
                let wanted: Vec<&str> = nzo.split(',').collect();
                slots.retain(|slot| {
                    slot["nzo_id"]
                        .as_str()
                        .is_some_and(|id| wanted.contains(&id))
                });
            } else if let Some(search) = param(&params, "search") {
                slots.retain(|slot| {
                    slot["name"]
                        .as_str()
                        .is_some_and(|name| name.contains(&search))
                });
            }
            let count = slots.len();
            json_response(json!({"history": {"slots": slots, "noofslots": count}}))
        }
        "addfile" => {
            if state.addfile_fail_500 > 0 {
                state.addfile_fail_500 -= 1;
                return (StatusCode::INTERNAL_SERVER_ERROR, "blip").into_response();
            }
            state.add_file_requests.push(SabApiCall {
                raw_query,
                params,
                content_type: headers
                    .get(header::CONTENT_TYPE)
                    .and_then(|value| value.to_str().ok())
                    .map(str::to_owned),
                body,
            });
            json_response(json!({"status": true, "nzo_ids": state.add_nzo_ids}))
        }
        "addurl" => {
            if state.addurl_echo_url
                && let Some(url) = param(&params, "name")
            {
                return json_response(
                    json!({"status": false, "error": format!("fetch failed for {url}")}),
                );
            }
            state.add_url_requests.push(SabApiCall {
                raw_query,
                params,
                content_type: None,
                body,
            });
            json_response(json!({"status": true, "nzo_ids": state.add_nzo_ids}))
        }
        _ => json_response(json!({"status": false, "error": format!("unknown mode {mode}")})),
    }
}

// ---------------------------------------------------------------------------
// Newznab mock (v2 `newznab_mock.py`).
// ---------------------------------------------------------------------------

/// One recorded search call: the assertable wire record for the ladder.
#[derive(Debug, Clone, Default)]
pub struct NewznabSearchCall {
    /// `search` or `music`.
    pub kind: String,
    /// Free-text query.
    pub q: String,
    /// Structured artist param.
    pub artist: String,
    /// Structured album param.
    pub album: String,
    /// Structured year param.
    pub year: String,
    /// Category filter.
    pub cat: String,
    /// Whether `extended=1` was sent.
    pub extended: bool,
    /// Whether an `apikey` param was present (never the value).
    pub apikey_present: bool,
}

/// Scenario indexers, mirroring the v2 handlers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NewznabScenario {
    /// DrunkenSlug (nZEDb): audio-search=no, `t=music` 202s.
    DrunkenSlug,
    /// Audionix: audio-search=yes, `t=music` works.
    Audionix,
    /// Caps advertise audio-search but `t=music` 202s: must fall back.
    BrokenMusic,
    /// `t=caps` 500s but search works: permissive defaults apply.
    CapsDead,
    /// Every call answers the auth error.
    AuthError,
    /// Every search answers the limit error.
    RateLimitError,
    /// Every search answers HTTP 429 + `Retry-After: 5`.
    Http429,
    /// Search answers torrent/magnet enclosures (Torznab mix-up).
    TorznabFeed,
    /// Search answers malformed XML (bare `&`, `&nbsp;`, control chars).
    MalformedXml,
}

/// Handle to the shared Newznab call log.
#[derive(Debug, Clone, Default)]
pub struct NewznabMock {
    calls: Arc<Mutex<Vec<NewznabSearchCall>>>,
}

impl NewznabMock {
    /// Fresh call log.
    pub fn new() -> Self {
        NewznabMock {
            calls: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// Recorded searches, in order.
    pub fn calls(&self) -> Vec<NewznabSearchCall> {
        self.calls
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    /// Router serving one scenario at `/api`.
    pub fn router(&self, scenario: NewznabScenario) -> Router {
        Router::new()
            .route("/api", get(newznab_api))
            .with_state((self.clone(), scenario))
    }

    fn record(&self, call: NewznabSearchCall) {
        self.calls
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .push(call);
    }
}

async fn newznab_api(
    State((mock, scenario)): State<(NewznabMock, NewznabScenario)>,
    Query(params): Query<HashMap<String, String>>,
) -> Response {
    let kind = params.get("t").cloned().unwrap_or_default();
    if kind == "caps" {
        return match scenario {
            NewznabScenario::CapsDead => {
                (StatusCode::INTERNAL_SERVER_ERROR, "caps exploded").into_response()
            }
            NewznabScenario::AuthError => xml_response(AUTH_ERROR),
            NewznabScenario::DrunkenSlug
            | NewznabScenario::RateLimitError
            | NewznabScenario::Http429
            | NewznabScenario::TorznabFeed
            | NewznabScenario::MalformedXml => xml_response(DS_CAPS),
            NewznabScenario::Audionix | NewznabScenario::BrokenMusic => xml_response(AX_CAPS),
        };
    }
    if matches!(scenario, NewznabScenario::AuthError) {
        return xml_response(AUTH_ERROR);
    }
    if kind == "music" {
        mock.record(NewznabSearchCall {
            kind,
            artist: params.get("artist").cloned().unwrap_or_default(),
            album: params.get("album").cloned().unwrap_or_default(),
            year: params.get("year").cloned().unwrap_or_default(),
            cat: params.get("cat").cloned().unwrap_or_default(),
            extended: params.get("extended").is_some_and(|value| value == "1"),
            apikey_present: params.contains_key("apikey"),
            ..NewznabSearchCall::default()
        });
        return match scenario {
            NewznabScenario::Audionix => xml_response(AX_MUSIC),
            _ => xml_response(DS_MUSIC_ERROR),
        };
    }
    if kind != "search" {
        return xml_response(DS_MUSIC_ERROR);
    }
    mock.record(NewznabSearchCall {
        kind,
        q: params.get("q").cloned().unwrap_or_default(),
        artist: params.get("artist").cloned().unwrap_or_default(),
        album: params.get("album").cloned().unwrap_or_default(),
        year: params.get("year").cloned().unwrap_or_default(),
        cat: params.get("cat").cloned().unwrap_or_default(),
        extended: params.get("extended").is_some_and(|value| value == "1"),
        apikey_present: params.contains_key("apikey"),
    });
    match scenario {
        NewznabScenario::RateLimitError => xml_response(RATE_LIMIT_ERROR),
        NewznabScenario::Http429 => (
            StatusCode::TOO_MANY_REQUESTS,
            [(header::RETRY_AFTER, "5")],
            "Rate limited",
        )
            .into_response(),
        NewznabScenario::TorznabFeed => xml_response(TORZNAB_FEED),
        NewznabScenario::MalformedXml => xml_response(MALFORMED_FEED),
        NewznabScenario::Audionix => xml_response(AX_SEARCH_WRONG),
        _ => {
            let q = params.get("q").cloned().unwrap_or_default();
            xml_response(&free_text_feed(&q, DS_SEARCH))
        }
    }
}

/// The punctuation-ladder feed (v2 `_free_text_feed`, #259): the gated
/// release only on the punctuation-free rung, empty on the canonical
/// punctuated rung, the default feed for anything else.
fn free_text_feed(q: &str, default: &str) -> String {
    const GATED: &str = "honestly";
    const NORESULT: &str = "xyzzynomatch";
    const PUNCT: [char; 12] = [',', '\'', '"', '?', '!', ';', ':', '&', '‘', '’', '“', '”'];
    let low = q.to_ascii_lowercase();
    if low.contains(GATED) {
        if q.chars().any(|ch| PUNCT.contains(&ch)) {
            return DS_EMPTY.to_owned();
        }
        return GATED_RELEASE_FEED.to_owned();
    }
    if low.contains(NORESULT) {
        return DS_EMPTY.to_owned();
    }
    default.to_owned()
}

const DS_CAPS: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<caps>
 <server appversion="0.8.21.0" version="0.1" title="DS" strapline="DrunkenSlug"/>
 <limits max="100" default="100"/>
 <searching>
  <search available="yes" supportedParams="q"/>
  <tv-search available="yes" supportedParams="q,season,ep"/>
  <movie-search available="yes" supportedParams="q,imdbid"/>
  <audio-search available="no" supportedParams=""/>
 </searching>
 <categories>
  <category id="3000" name="Audio">
   <subcat id="3030" name="Audiobook"/>
   <subcat id="3060" name="Foreign"/>
   <subcat id="3040" name="Lossless"/>
   <subcat id="3010" name="MP3"/>
   <subcat id="3999" name="Other"/>
   <subcat id="3020" name="Video"/>
  </category>
  <category id="5000" name="TV">
   <subcat id="5040" name="HD"/>
  </category>
 </categories>
</caps>"#;

const DS_SEARCH: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<rss version="2.0" xmlns:atom="http://www.w3.org/2005/Atom" xmlns:newznab="http://www.newznab.com/DTD/2010/feeds/attributes/">
 <channel>
  <title>DS</title>
  <newznab:response offset="0" total="3"/>
  <newznab:apilimits apiCurrent="183" grabCurrent="46"/>
  <item>
   <title>[003/113] &quot;Radiohead-In_Rainbows-LP-24BIT-FLAC-2007-REETKEVER.part002.rar&quot;</title>
   <guid isPermaLink="true">https://drunkenslug.com/details/93bc59792f2ee7af8486c3d278265b1a9964f2e5</guid>
   <link>https://drunkenslug.com/getnzb/93bc.nzb&amp;i=1&amp;r=KEY</link>
   <pubDate>Thu, 23 Oct 2025 19:21:32 +0100</pubDate>
   <category>Audio &gt; Lossless</category>
   <enclosure url="https://drunkenslug.com/getnzb/93bc.nzb&amp;i=1&amp;r=KEY" length="2315726631" type="application/x-nzb"/>
   <newznab:attr name="category" value="3040"/>
   <newznab:attr name="size" value="2315726631"/>
   <newznab:attr name="files" value="113"/>
   <newznab:attr name="grabs" value="205"/>
   <newznab:attr name="password" value="0"/>
   <newznab:attr name="usenetdate" value="Thu, 23 Oct 2025 19:17:23 +0100"/>
  </item>
  <item>
   <title>[4/9] &quot;Radiohead - In Rainbows (For Overhead Play) [CD-R, US promo].zip.vol01+02.par2&quot;</title>
   <guid isPermaLink="true">https://drunkenslug.com/details/58780764e9e6c77177add7bd2cff5894dfc4d787</guid>
   <pubDate>Thu, 19 Feb 2026 18:57:03 +0000</pubDate>
   <category>Audio &gt; Other</category>
   <enclosure url="https://drunkenslug.com/getnzb/5878.nzb&amp;i=1&amp;r=KEY" length="580998479" type="application/x-nzb"/>
   <newznab:attr name="category" value="3999"/>
   <newznab:attr name="size" value="580998479"/>
   <newznab:attr name="files" value="9"/>
   <newznab:attr name="grabs" value="8"/>
   <newznab:attr name="usenetdate" value="Thu, 19 Feb 2026 18:45:35 +0000"/>
  </item>
  <item>
   <title>aHR0cHM6Ly9 obfuscated release name xZQ.part01.rar</title>
   <guid isPermaLink="true">https://drunkenslug.com/details/cafe0000</guid>
   <enclosure url="https://drunkenslug.com/getnzb/cafe.nzb&amp;i=1&amp;r=KEY" length="402653184" type="application/x-nzb"/>
   <newznab:attr name="category" value="3040"/>
   <newznab:attr name="size" value="402653184"/>
   <newznab:attr name="grabs" value="61"/>
  </item>
 </channel>
</rss>"#;

const DS_MUSIC_ERROR: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<error code="202" description="No such function (music)"/>"#;

const AX_CAPS: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<caps>
 <server version="1.2" title="Audionix"/>
 <limits max="100" default="100"/>
 <searching>
  <search available="yes" supportedParams="q"/>
  <audio-search available="yes" supportedParams="q,artist,album"/>
 </searching>
 <categories>
  <category id="3000" name="Audio">
   <subcat id="3010" name="MP3"/>
   <subcat id="3040" name="Lossless"/>
   <subcat id="3050" name="Other"/>
  </category>
 </categories>
</caps>"#;

const AX_MUSIC: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<rss version="2.0" xmlns:newznab="http://www.newznab.com/DTD/2010/feeds/attributes/">
 <channel>
  <newznab:response offset="0" total="2"/>
  <item>
   <title>[003/113] &quot;Radiohead-In_Rainbows-LP-24BIT-FLAC-2007-REETKEVER.part002.rar&quot;</title>
   <guid>audionix-guid-flac</guid>
   <enclosure url="https://audionix.test/nzb/flac" length="2315726631" type="application/x-nzb"/>
   <newznab:attr name="category" value="3040"/>
   <newznab:attr name="size" value="2315726631"/>
   <newznab:attr name="grabs" value="999"/>
  </item>
  <item>
   <title>Radiohead - In Rainbows (2007) [MP3-320]</title>
   <guid>audionix-guid-mp3</guid>
   <enclosure url="https://audionix.test/nzb/mp3" length="115343360" type="application/x-nzb"/>
   <newznab:attr name="category" value="3010"/>
   <newznab:attr name="size" value="115343360"/>
   <newznab:attr name="grabs" value="120"/>
  </item>
 </channel>
</rss>"#;

const AX_SEARCH_WRONG: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<rss version="2.0" xmlns:newznab="http://www.newznab.com/DTD/2010/feeds/attributes/">
 <channel>
  <item>
   <title>WRONG-PATH-used-t=search-instead-of-t=music</title>
   <guid>wrong</guid>
   <enclosure url="https://audionix.test/nzb/wrong" length="1" type="application/x-nzb"/>
   <newznab:attr name="size" value="1"/>
  </item>
 </channel>
</rss>"#;

const AUTH_ERROR: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<error code="100" description="Incorrect user credentials"/>"#;

const RATE_LIMIT_ERROR: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<error code="500" description="Request limit reached"/>"#;

const GATED_RELEASE_FEED: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<rss version="2.0" xmlns:newznab="http://www.newznab.com/DTD/2010/feeds/attributes/">
 <channel>
  <newznab:response offset="0" total="1"/>
  <item>
   <title>Drake-Honestly_Nevermind-WEB-FLAC-2022-GROUP</title>
   <guid isPermaLink="true">https://drunkenslug.com/details/d9aa0f7e5c4b4a3b8f6e1d2c5b7a49301</guid>
   <pubDate>Fri, 17 Jun 2022 12:00:00 +0100</pubDate>
   <enclosure url="https://drunkenslug.com/getnzb/d9aa.nzb&amp;i=1&amp;r=KEY" length="734003200" type="application/x-nzb"/>
   <newznab:attr name="category" value="3040"/>
   <newznab:attr name="size" value="734003200"/>
   <newznab:attr name="files" value="42"/>
   <newznab:attr name="grabs" value="87"/>
   <newznab:attr name="password" value="0"/>
   <newznab:attr name="usenetdate" value="Fri, 17 Jun 2022 11:58:00 +0100"/>
  </item>
 </channel>
</rss>"#;

const DS_EMPTY: &str = r#"<?xml version="1.0"?><rss version="2.0" xmlns:newznab="http://www.newznab.com/DTD/2010/feeds/attributes/"><channel></channel></rss>"#;

const TORZNAB_FEED: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<rss version="2.0" xmlns:newznab="http://www.newznab.com/DTD/2010/feeds/attributes/">
 <channel>
  <item>
   <title>Radiohead - In Rainbows (2007) [FLAC]</title>
   <guid>torznab-1</guid>
   <enclosure url="magnet:?xt=urn:btih:cafe" length="1" type="application/x-bittorrent"/>
   <newznab:attr name="size" value="400000000"/>
  </item>
 </channel>
</rss>"#;

/// Bare `&`, a non-predefined `&nbsp;`, and a control char: the hardening
/// test proves the feed still parses.
const MALFORMED_FEED: &str = concat!(
    "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n",
    "<rss version=\"2.0\" xmlns:newznab=\"http://www.newznab.com/DTD/2010/feeds/attributes/\">\n",
    " <channel>\n",
    "  <item>\n",
    "   <title>Fish & Chips & Trouble\x07 and&nbsp;more</title>\n",
    "   <guid>malformed-1</guid>\n",
    "   <enclosure url=\"https://malformed.test/nzb/1\" length=\"100\" type=\"application/x-nzb\"/>\n",
    "   <newznab:attr name=\"size\" value=\"100\"/>\n",
    "  </item>\n",
    " </channel>\n",
    "</rss>",
);

// ---------------------------------------------------------------------------
// Prowlarr mock (v2 `prowlarr_mock.py`).
// ---------------------------------------------------------------------------

/// One recorded search call (params only; header presence, never the key).
#[derive(Debug, Clone, Default)]
pub struct ProwlarrSearchCall {
    /// Free-text query.
    pub query: String,
    /// Every `categories` value, in order.
    pub categories: Vec<String>,
    /// Every `indexerIds` value, in order.
    pub indexer_ids: Vec<String>,
    /// `limit` value.
    pub limit: String,
    /// Whether the `X-Api-Key` header was present (never its value).
    pub api_key_present: bool,
}

/// Scenario switches, mirroring the v2 hosts.
#[derive(Debug, Clone, Default)]
pub struct ProwlarrScenario {
    /// `system/status` 404s (the unconfirmed endpoint).
    pub status_404: bool,
    /// Every endpoint 401s (wrong API key).
    pub auth_fail: bool,
    /// Search 429s with `Retry-After: 5`.
    pub search_429: bool,
    /// Search 500s.
    pub search_500: bool,
    /// Search 200s with an HTML body (proxy/login page).
    pub search_html: bool,
    /// Search answers torrent-only results.
    pub torrents_only: bool,
}

/// Handle to the shared Prowlarr call log.
#[derive(Debug, Clone, Default)]
pub struct ProwlarrMock {
    calls: Arc<Mutex<Vec<ProwlarrSearchCall>>>,
}

impl ProwlarrMock {
    /// Fresh call log.
    pub fn new() -> Self {
        ProwlarrMock {
            calls: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// Recorded searches, in order.
    pub fn calls(&self) -> Vec<ProwlarrSearchCall> {
        self.calls
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    /// Router serving one scenario under `/api/v1`.
    pub fn router(&self, scenario: ProwlarrScenario) -> Router {
        Router::new()
            .route("/api/v1/system/status", get(prowlarr_status))
            .route("/api/v1/indexer", get(prowlarr_indexers))
            .route("/api/v1/search", get(prowlarr_search))
            .with_state((self.clone(), scenario))
    }

    fn record(&self, call: ProwlarrSearchCall) {
        self.calls
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .push(call);
    }
}

async fn prowlarr_status(
    State((_, scenario)): State<(ProwlarrMock, ProwlarrScenario)>,
) -> Response {
    if scenario.auth_fail {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    if scenario.status_404 {
        return StatusCode::NOT_FOUND.into_response();
    }
    json_response(json!({"version": "1.32.2.4987"}))
}

async fn prowlarr_indexers(
    State((_, scenario)): State<(ProwlarrMock, ProwlarrScenario)>,
) -> Response {
    if scenario.auth_fail {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    json_response(json!([
        {"id": 7, "name": "NZBGeek", "protocol": "usenet", "enable": true},
        {"id": 11, "name": "DrunkenSlug", "protocol": "usenet", "enable": true},
        {"id": 12, "name": "DisabledTracker", "protocol": "torrent", "enable": false},
    ]))
}

async fn prowlarr_search(
    State((mock, scenario)): State<(ProwlarrMock, ProwlarrScenario)>,
    RawQuery(raw): RawQuery,
    headers: HeaderMap,
) -> Response {
    if scenario.auth_fail {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let pairs = url_decode_pairs(&raw.unwrap_or_default());
    mock.record(ProwlarrSearchCall {
        query: param(&pairs, "query").unwrap_or_default(),
        categories: pairs
            .iter()
            .filter(|(key, _)| key == "categories")
            .map(|(_, value)| value.clone())
            .collect(),
        indexer_ids: pairs
            .iter()
            .filter(|(key, _)| key == "indexerIds")
            .map(|(_, value)| value.clone())
            .collect(),
        limit: param(&pairs, "limit").unwrap_or_default(),
        api_key_present: headers.contains_key("x-api-key"),
    });
    if scenario.search_429 {
        return (
            StatusCode::TOO_MANY_REQUESTS,
            [(header::RETRY_AFTER, "5")],
            "Rate limited",
        )
            .into_response();
    }
    if scenario.search_500 {
        return (StatusCode::INTERNAL_SERVER_ERROR, "Server error").into_response();
    }
    if scenario.search_html {
        return (
            StatusCode::OK,
            [(header::CONTENT_TYPE, "text/html")],
            "<html><body>login</body></html>",
        )
            .into_response();
    }
    if scenario.torrents_only {
        return json_response(json!([{
            "guid": "torrent-only-1",
            "title": "Radiohead - In Rainbows (2007) [FLAC] [torrent]",
            "size": 400000000,
            "indexerId": 12,
            "indexer": "SomeTracker",
            "categories": [{"id": 3040, "name": "Audio/Lossless"}],
            "magnetUrl": "magnet:?xt=urn:btih:cafe",
            "protocol": "torrent",
        }]));
    }
    json_response(json!([
        {
            "guid": "usenet-guid-flac-1",
            "title": "Radiohead - In Rainbows (2007) [FLAC]",
            "size": 2315726631_i64,
            "files": 113,
            "grabs": 205,
            "indexerId": 7,
            "indexer": "NZBGeek",
            "categories": [{"id": 3040, "name": "Audio/Lossless"}],
            "downloadUrl": "https://prowlarr.test/9/download?apikey=MOCKKEY&link=ezQxYw",
            "magnetUrl": "",
            "protocol": "usenet",
            "publishDate": "2025-10-23T19:17:23Z",
        },
        {
            "guid": "torrent-guid-1",
            "title": "Radiohead - In Rainbows (2007) [FLAC] [torrent]",
            "size": 400000000,
            "indexerId": 12,
            "indexer": "SomeTracker",
            "categories": [{"id": 3040, "name": "Audio/Lossless"}],
            "downloadUrl": "",
            "magnetUrl": "magnet:?xt=urn:btih:cafe",
            "protocol": "torrent",
            "publishDate": "2025-10-20T10:00:00Z",
            "seeders": 42,
            "leechers": 3,
        },
        {
            "guid": "usenet-guid-nourl",
            "title": "Radiohead - In Rainbows (2007) [MP3]",
            "size": 120000000,
            "indexerId": 11,
            "indexer": "DrunkenSlug",
            "categories": [{"id": 3010, "name": "Audio/MP3"}],
            "downloadUrl": "",
            "protocol": "usenet",
            "publishDate": "",
        },
    ]))
}
