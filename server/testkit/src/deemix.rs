//! Loopback Deemix web API for the client tests.
//!
//! Serves the answers recorded from deemix-webui 4.8.0 in
//! `tests/fixtures/deemix/`. Like Deemix it names each session with a
//! `connect.sid` cookie, set on the first answer to a request that sends
//! none (or one it no longer knows), keeps a Deezer sign-in per session,
//! and answers any path it has no route for with 200 and an HTML page.
//!
//! - `/connect` answers the recorded signed-out answer, or the signed-in
//!   one on a session that signed in. With [`DeemixMock::multi_user`] the
//!   signed-out answer carries no `singleUser`.
//! - `loginArl` signs the session in (the recorded success), then answers
//!   the recorded "already signed in" on the same session. With
//!   [`DeemixState::login_fails`] it answers failed and, as Deemix does in
//!   single-user mode, stops offering the saved sign-in.
//! - `addToQueue` refuses a signed-out session (`NotLoggedIn`) and a
//!   bitrate the account cannot stream (`CantStream`), and otherwise
//!   answers the recorded success naming the item it queued, or the
//!   recorded empty add with [`DeemixState::add_nothing`].
//! - `removeFromQueue` answers the recorded success.
//! - `getQueue` answers the recorded empty queue, or walks the snapshots a
//!   test hands it ([`DeemixMock::walk_queue`]).
//!
//! A test can put any answer on a path in place of the route. Every call
//! is recorded with the cookie it sent. It listens on `127.0.0.1` only.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};

use axum::Router;
use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, Method, StatusCode, Uri, header};
use axum::response::{IntoResponse, Response};
use serde_json::{Value, json};

use crate::scripted::ScriptedStatuses;

const CONNECT: &str = include_str!("../../tests/fixtures/deemix/connect_signed_out.json");
const CONNECT_SAVED_LOGIN: &str =
    include_str!("../../tests/fixtures/deemix/connect_saved_login.json");
const CONNECT_SIGNED_IN: &str = include_str!("../../tests/fixtures/deemix/connect_signed_in.json");
const LOGIN_SUCCESS: &str = include_str!("../../tests/fixtures/deemix/loginArl_success.json");
const LOGIN_ALREADY: &str =
    include_str!("../../tests/fixtures/deemix/loginArl_already_logged.json");
const SEARCH_ALBUM: &str = include_str!("../../tests/fixtures/deemix/search_album.json");
const SEARCH_ALBUM_MULTIDISC: &str =
    include_str!("../../tests/fixtures/deemix/search_album_multidisc.json");
const TRACKLIST_ALBUM: &str = include_str!("../../tests/fixtures/deemix/tracklist_album.json");
const TRACKLIST_ALBUM_MULTIDISC: &str =
    include_str!("../../tests/fixtures/deemix/tracklist_album_multidisc.json");
const TRACKLIST_UNKNOWN: &str = include_str!("../../tests/fixtures/deemix/tracklist_unknown.json");
const ADD_NOT_LOGGED_IN: &str =
    include_str!("../../tests/fixtures/deemix/addToQueue_not_logged_in.json");
const ADD_CANT_STREAM: &str =
    include_str!("../../tests/fixtures/deemix/addToQueue_cant_stream.json");
const ADD_NOTHING: &str = include_str!("../../tests/fixtures/deemix/addToQueue_nothing_added.json");
const ADD_TRACK: &str = include_str!("../../tests/fixtures/deemix/addToQueue_track.json");
const ADD_ALBUM: &str = include_str!("../../tests/fixtures/deemix/addToQueue_album.json");
const ADD_ALBUM_MULTIDISC: &str =
    include_str!("../../tests/fixtures/deemix/addToQueue_album_multidisc.json");
const REMOVE: &str = include_str!("../../tests/fixtures/deemix/removeFromQueue.json");
const QUEUE_EMPTY: &str = include_str!("../../tests/fixtures/deemix/getQueue_empty.json");

/// Recorded `getQueue` snapshots, for [`DeemixMock::walk_queue`].
pub mod snapshots {
    /// Track 138546807 downloading twice, then completed, loose in the
    /// download folder.
    pub const TRACK: [&str; 3] = [
        include_str!("../../tests/fixtures/deemix/getQueue_track_1_downloading.json"),
        include_str!("../../tests/fixtures/deemix/getQueue_track_2_downloading.json"),
        include_str!("../../tests/fixtures/deemix/getQueue_track_3_completed.json"),
    ];

    /// Album 14880659 downloading with album 6158273 (two discs) waiting,
    /// then 6158273 downloading, then both completed.
    pub const TWO_ALBUMS: [&str; 7] = [
        include_str!("../../tests/fixtures/deemix/getQueue_two_albums_1.json"),
        include_str!("../../tests/fixtures/deemix/getQueue_two_albums_2.json"),
        include_str!("../../tests/fixtures/deemix/getQueue_two_albums_3.json"),
        include_str!("../../tests/fixtures/deemix/getQueue_two_albums_4.json"),
        include_str!("../../tests/fixtures/deemix/getQueue_two_albums_5.json"),
        include_str!("../../tests/fixtures/deemix/getQueue_two_albums_6.json"),
        include_str!("../../tests/fixtures/deemix/getQueue_two_albums_7.json"),
    ];

    /// After a kill mid-download: both albums waiting, only 14880659 in
    /// `queueOrder`, nothing current.
    pub const AFTER_KILL: &str =
        include_str!("../../tests/fixtures/deemix/getQueue_after_kill.json");

    /// Just after the current item was removed: `current` still names
    /// album 6158273, which has left `queue`.
    pub const AFTER_CANCEL: &str =
        include_str!("../../tests/fixtures/deemix/getQueue_after_cancel.json");
}

/// The page Deemix answers a path it has no route for with.
const WEB_UI: &str = "<!DOCTYPE html><html><head><title>deemix</title></head><body><div id=\"app\"></div></body></html>";

/// The cookie Deemix's sessions are named by.
pub const SESSION_COOKIE: &str = "connect.sid";

fn recorded(answer: &str) -> Value {
    serde_json::from_str(answer).unwrap_or_default()
}

/// One recorded request.
#[derive(Debug, Clone)]
pub struct DeemixCall {
    pub method: String,
    /// The path, `/api/connect` and the like.
    pub path: String,
    /// The query, decoded.
    pub query: HashMap<String, String>,
    /// The body as sent.
    pub body: String,
    /// The `connect.sid` value the request carried.
    pub cookie: Option<String>,
}

/// A whole answer a test put on one path.
#[derive(Debug, Clone)]
pub struct Scripted {
    pub status: u16,
    pub content_type: String,
    pub body: String,
}

/// Mutable Deemix mock state, shared with the tests.
#[derive(Debug)]
pub struct DeemixState {
    /// The `/connect` answer to a signed-out session: the recorded one
    /// with no saved sign-in.
    pub connect: Value,
    /// The `/connect` answer to a signed-in session: the recorded one.
    pub connect_signed_in: Value,
    /// When set, `/connect` carries it as `queue`, as Deemix does while
    /// its queue holds anything.
    pub connect_queue: Option<Value>,
    /// Search answers by term: the two recorded terms. Any other term
    /// answers no hits.
    pub searches: HashMap<String, Value>,
    /// Tracklists by album id: the two recorded albums. Any other id
    /// answers the recorded unknown-id answer.
    pub tracklists: HashMap<String, Value>,
    /// The bitrate codes the account streams: a free account's 1 only.
    pub account_bitrates: Vec<u64>,
    /// Successful adds by link: the three recorded ones. An add of any
    /// other link answers the recorded track or album add, renamed.
    pub adds: HashMap<String, Value>,
    /// A signed-in add answers the recorded empty add.
    pub add_nothing: bool,
    /// `loginArl` on a signed-out session answers failed.
    pub login_fails: bool,
    /// The `getQueue` answer while no walk is set: the recorded empty
    /// queue.
    pub queue: Value,
    /// `getQueue` answers these in turn, staying on the last.
    pub queue_walk: VecDeque<Value>,
    /// Answers that take the place of a path's route, by path.
    pub scripted: HashMap<String, Scripted>,
    /// Every session cookie handed out, in order.
    pub sessions: Vec<String>,
    /// The sessions signed in to Deezer.
    pub signed_in: Vec<String>,
    /// Every request in arrival order.
    pub calls: Vec<DeemixCall>,
}

impl Default for DeemixState {
    fn default() -> Self {
        DeemixState {
            connect: recorded(CONNECT),
            connect_signed_in: recorded(CONNECT_SIGNED_IN),
            connect_queue: None,
            searches: HashMap::from([
                ("Radiohead In Rainbows".to_owned(), recorded(SEARCH_ALBUM)),
                (
                    "Mellon Collie and the Infinite Sadness".to_owned(),
                    recorded(SEARCH_ALBUM_MULTIDISC),
                ),
            ]),
            tracklists: HashMap::from([
                ("14880659".to_owned(), recorded(TRACKLIST_ALBUM)),
                ("6158273".to_owned(), recorded(TRACKLIST_ALBUM_MULTIDISC)),
            ]),
            account_bitrates: vec![1],
            adds: HashMap::from([
                (
                    "https://www.deezer.com/track/138546807".to_owned(),
                    recorded(ADD_TRACK),
                ),
                (
                    "https://www.deezer.com/album/14880659".to_owned(),
                    recorded(ADD_ALBUM),
                ),
                (
                    "https://www.deezer.com/album/6158273".to_owned(),
                    recorded(ADD_ALBUM_MULTIDISC),
                ),
            ]),
            add_nothing: false,
            login_fails: false,
            queue: recorded(QUEUE_EMPTY),
            queue_walk: VecDeque::new(),
            scripted: HashMap::new(),
            sessions: Vec::new(),
            signed_in: Vec::new(),
            calls: Vec::new(),
        }
    }
}

/// Handle to the shared mock state.
#[derive(Debug, Clone, Default)]
pub struct DeemixMock {
    state: Arc<Mutex<DeemixState>>,
    statuses: ScriptedStatuses,
}

impl DeemixMock {
    /// Fresh mock: the recorded answers, no session open.
    pub fn new() -> Self {
        DeemixMock::default()
    }

    /// Error statuses the next requests answer with, and the request count.
    pub fn statuses(&self) -> &ScriptedStatuses {
        &self.statuses
    }

    /// Borrow the state for setup and assertion.
    pub fn state(&self) -> std::sync::MutexGuard<'_, DeemixState> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// The recorded calls to `path`.
    pub fn calls_to(&self, path: &str) -> Vec<DeemixCall> {
        self.state()
            .calls
            .iter()
            .filter(|call| call.path == path)
            .cloned()
            .collect()
    }

    /// Answer every request to `path` with `body` under `content_type`.
    pub fn script(&self, path: &str, status: u16, content_type: &str, body: &str) -> &Self {
        self.state().scripted.insert(
            path.to_owned(),
            Scripted {
                status,
                content_type: content_type.to_owned(),
                body: body.to_owned(),
            },
        );
        self
    }

    /// Offer the saved sign-in to signed-out sessions: the recorded
    /// `/connect` of a fresh session after a sign-in was saved.
    pub fn offer_saved_login(&self) -> &Self {
        self.state().connect = recorded(CONNECT_SAVED_LOGIN);
        self
    }

    /// Answer as in multi-user mode: `/connect` to a signed-out session
    /// carries no `singleUser`, as deemix-webui 4.8.0 sends it only in
    /// single-user mode (`routes/api/get/connect.ts`).
    pub fn multi_user(&self) -> &Self {
        if let Some(connect) = self.state().connect.as_object_mut() {
            connect.remove("singleUser");
        }
        self
    }

    /// Answer `getQueue` with `snapshots` in turn, staying on the last.
    pub fn walk_queue(&self, snapshots: &[&str]) -> &Self {
        self.state().queue_walk = snapshots.iter().map(|text| recorded(text)).collect();
        self
    }

    /// Forget every session and sign-in, as a Deemix restart does.
    pub fn restart(&self) -> &Self {
        let mut state = self.state();
        state.sessions.clear();
        state.signed_in.clear();
        drop(state);
        self
    }

    /// The router: every path goes to the one handler, as on Deemix.
    pub fn router(&self) -> Router {
        let router = Router::new().fallback(answer).with_state(self.clone());
        self.statuses.layer(router)
    }
}

fn json_reply(body: &Value) -> (StatusCode, &'static str, String) {
    (
        StatusCode::OK,
        "application/json; charset=utf-8",
        body.to_string(),
    )
}

/// `%XX` and `+` decoded; a bad escape is kept as it came.
fn decode(component: &str) -> String {
    let bytes = component.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut at = 0;
    while at < bytes.len() {
        let escaped = (bytes[at] == b'%')
            .then(|| bytes.get(at + 1..at + 3))
            .flatten()
            .and_then(|hex| std::str::from_utf8(hex).ok())
            .and_then(|hex| u8::from_str_radix(hex, 16).ok());
        match (escaped, bytes[at]) {
            (Some(byte), _) => {
                out.push(byte);
                at += 3;
            }
            (None, b'+') => {
                out.push(b' ');
                at += 1;
            }
            (None, byte) => {
                out.push(byte);
                at += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn query_of(uri: &Uri) -> HashMap<String, String> {
    uri.query()
        .unwrap_or_default()
        .split('&')
        .filter(|pair| !pair.is_empty())
        .map(|pair| {
            let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
            (decode(key), decode(value))
        })
        .collect()
}

fn session_cookie(headers: &HeaderMap) -> Option<String> {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(';'))
        .filter_map(|pair| pair.trim().split_once('='))
        .find(|(name, _)| *name == SESSION_COOKIE)
        .map(|(_, value)| value.to_owned())
}

/// The links an add sent, split as Deemix splits them.
fn links_of(sent: &Value) -> Vec<String> {
    sent["url"]
        .as_str()
        .unwrap_or_default()
        .split([' ', ';'])
        .filter(|link| !link.is_empty())
        .map(str::to_owned)
        .collect()
}

/// A recorded add answer, naming what was sent.
fn add_answer(recorded_answer: &str, sent: &Value) -> Value {
    let mut answer = recorded(recorded_answer);
    answer["data"]["url"] = json!(links_of(sent));
    answer["data"]["bitrate"] = sent["bitrate"].clone();
    answer
}

/// A recorded success for the one link sent, its item renamed to that
/// link and bitrate.
fn add_queued(state: &DeemixState, sent: &Value) -> Value {
    let link = links_of(sent).into_iter().next().unwrap_or_default();
    let (kind, id) = link
        .trim_start_matches("https://www.deezer.com/")
        .split_once('/')
        .unwrap_or_default();
    let mut answer = state.adds.get(&link).cloned().unwrap_or_else(|| {
        recorded(if kind == "track" {
            ADD_TRACK
        } else {
            ADD_ALBUM
        })
    });
    let bitrate = sent["bitrate"].clone();
    answer["data"]["url"] = json!([link]);
    answer["data"]["bitrate"] = bitrate.clone();
    let item = &mut answer["data"]["obj"][0];
    item["type"] = json!(kind);
    item["id"] = json!(id);
    item["uuid"] = json!(format!("{kind}_{id}_{bitrate}"));
    item["bitrate"] = bitrate;
    answer
}

/// The recorded sign-in answer, echoing the ARL sent as Deemix does.
fn login_answer(recorded_answer: &str, arl: &Value) -> Value {
    let mut answer = recorded(recorded_answer);
    answer["arl"] = arl.clone();
    answer
}

/// Deemix's failed sign-in (not recorded: it erases the saved sign-in).
fn login_failed(arl: &Value) -> Value {
    json!({"status": 0, "arl": arl, "user": {}, "childs": [], "currentChild": 0})
}

async fn answer(
    State(mock): State<DeemixMock>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let path = uri.path().to_owned();
    let query = query_of(&uri);
    let body = String::from_utf8_lossy(&body).into_owned();
    let cookie = session_cookie(&headers);
    let mut state = mock.state();
    state.calls.push(DeemixCall {
        method: method.to_string(),
        path: path.clone(),
        query: query.clone(),
        body: body.clone(),
        cookie: cookie.clone(),
    });
    let (session, set_cookie) = match cookie {
        Some(cookie) if state.sessions.contains(&cookie) => (cookie, None),
        _ => {
            let session = format!("s%3A{}.mock", uuid::Uuid::new_v4().simple());
            state.sessions.push(session.clone());
            let set = format!("{SESSION_COOKIE}={session}; Path=/; HttpOnly");
            (session, Some(set))
        }
    };
    let signed_in = state.signed_in.contains(&session);
    let sent: Value = serde_json::from_str(&body).unwrap_or_default();

    let (status, content_type, text) = if let Some(scripted) = state.scripted.get(&path) {
        (
            StatusCode::from_u16(scripted.status).unwrap_or(StatusCode::OK),
            scripted.content_type.clone(),
            scripted.body.clone(),
        )
    } else {
        let (status, content_type, text) = match (method.as_str(), path.as_str()) {
            ("GET", "/api/connect") => {
                let mut connect = if signed_in {
                    state.connect_signed_in.clone()
                } else {
                    state.connect.clone()
                };
                if let Some(queue) = state.connect_queue.clone() {
                    connect["queue"] = queue;
                }
                json_reply(&connect)
            }
            ("POST", "/api/loginArl") if sent["arl"].as_str().is_none_or(str::is_empty) => {
                (StatusCode::BAD_REQUEST, "text/plain", String::new())
            }
            ("POST", "/api/loginArl") => {
                let arl = &sent["arl"];
                let answer = if signed_in {
                    login_answer(LOGIN_ALREADY, arl)
                } else if state.login_fails {
                    state.connect["singleUser"]["arl"] = Value::Null;
                    login_failed(arl)
                } else {
                    state.signed_in.push(session.clone());
                    login_answer(LOGIN_SUCCESS, arl)
                };
                json_reply(&answer)
            }
            ("GET", "/api/search") => {
                let term = query.get("term").cloned().unwrap_or_default();
                let found =
                    state.searches.get(&term).cloned().unwrap_or_else(
                        || json!({"data": [], "total": 0, "type": query.get("type")}),
                    );
                json_reply(&found)
            }
            ("GET", "/api/getTracklist") => {
                let id = query.get("id").cloned().unwrap_or_default();
                let found = state
                    .tracklists
                    .get(&id)
                    .cloned()
                    .unwrap_or_else(|| recorded(TRACKLIST_UNKNOWN));
                json_reply(&found)
            }
            ("POST", "/api/addToQueue") => {
                let streams = sent["bitrate"]
                    .as_u64()
                    .is_some_and(|code| state.account_bitrates.contains(&code));
                let answer = if !signed_in {
                    add_answer(ADD_NOT_LOGGED_IN, &sent)
                } else if !streams {
                    add_answer(ADD_CANT_STREAM, &sent)
                } else if state.add_nothing {
                    add_answer(ADD_NOTHING, &sent)
                } else {
                    add_queued(&state, &sent)
                };
                json_reply(&answer)
            }
            ("POST", "/api/removeFromQueue") => json_reply(&recorded(REMOVE)),
            ("GET", "/api/getQueue") => {
                let queue = if state.queue_walk.len() > 1 {
                    state.queue_walk.pop_front()
                } else {
                    state.queue_walk.front().cloned()
                };
                json_reply(&queue.unwrap_or_else(|| state.queue.clone()))
            }
            _ => (
                StatusCode::OK,
                "text/html; charset=UTF-8",
                WEB_UI.to_owned(),
            ),
        };
        (status, content_type.to_owned(), text)
    };
    drop(state);

    let mut response = (status, [(header::CONTENT_TYPE, content_type)], text).into_response();
    if let Some(set_cookie) = set_cookie
        && let Ok(value) = set_cookie.parse()
    {
        response.headers_mut().insert(header::SET_COOKIE, value);
    }
    response
}
