//! Loopback NZBGet JSON-RPC server for the contract tests.
//!
//! Serves `POST /jsonrpc` (`version`, `config`, `listgroups`, `history`,
//! `append`, `editqueue`) from the answers recorded from NZBGet 26.3 in
//! `tests/fixtures/nzbget/`: the version, the config and every job are
//! built on those files, so the mock cannot drift from NZBGet's shapes.
//! It records every call and checks HTTP basic auth when a sign-in is
//! set. Like the other mocks it listens on `127.0.0.1` only.

use std::sync::{Arc, Mutex};

use axum::Router;
use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use serde_json::{Value, json};

use crate::scripted::ScriptedStatuses;
use crate::usenet::GOOD_NZB;

const VERSION: &str = include_str!("../../tests/fixtures/nzbget/version.json");
const CONFIG: &str = include_str!("../../tests/fixtures/nzbget/config.json");
const LISTGROUPS: &str = include_str!("../../tests/fixtures/nzbget/listgroups.json");
const HISTORY: &str = include_str!("../../tests/fixtures/nzbget/history.json");

/// The `result` of a recorded answer.
fn recorded(answer: &str) -> Value {
    serde_json::from_str::<Value>(answer)
        .ok()
        .and_then(|answer| answer.get("result").cloned())
        .unwrap_or_default()
}

/// The first recorded entry of a list answer, as a template for new ones.
fn template(answer: &str) -> Value {
    recorded(answer).get(0).cloned().unwrap_or_default()
}

/// One recorded JSON-RPC call.
#[derive(Debug, Clone)]
pub struct NzbgetCall {
    /// The method name.
    pub method: String,
    /// The params array as sent.
    pub params: Value,
    /// Whether the call carried a basic-auth header.
    pub authorized: bool,
}

/// Mutable NZBGet mock state, shared with the tests.
#[derive(Debug)]
pub struct NzbgetState {
    /// The `version` answer.
    pub version: String,
    /// `config` entries as `(Name, Value)`.
    pub config: Vec<(String, String)>,
    /// Queue entries served by `listgroups`.
    pub groups: Vec<Value>,
    /// History entries served by `history`.
    pub history: Vec<Value>,
    /// The id the next `append` answers with; 0 or less rejects the add.
    pub next_id: i64,
    /// Required sign-in, as `(username, password)`; `None` takes any.
    pub credentials: Option<(String, String)>,
    /// Every call in arrival order.
    pub calls: Vec<NzbgetCall>,
    /// Forced JSON-RPC error message for every call, until cleared.
    pub rpc_error: Option<String>,
}

impl Default for NzbgetState {
    fn default() -> Self {
        let config = recorded(CONFIG)
            .as_array()
            .map(|entries| {
                entries
                    .iter()
                    .map(|entry| {
                        let text = |key: &str| entry[key].as_str().unwrap_or_default().to_owned();
                        (text("Name"), text("Value"))
                    })
                    .collect()
            })
            .unwrap_or_default();
        NzbgetState {
            version: recorded(VERSION).as_str().unwrap_or_default().to_owned(),
            config,
            groups: Vec::new(),
            history: Vec::new(),
            next_id: 1,
            credentials: None,
            calls: Vec::new(),
            rpc_error: None,
        }
    }
}

/// Handle to the shared mock state.
#[derive(Debug, Clone, Default)]
pub struct NzbgetMock {
    state: Arc<Mutex<NzbgetState>>,
    statuses: ScriptedStatuses,
}

impl NzbgetMock {
    /// Fresh mock: the recorded NZBGet 26.3 answers, no sign-in required.
    pub fn new() -> Self {
        NzbgetMock::default()
    }

    /// Error statuses the next requests answer with, and the request count.
    pub fn statuses(&self) -> &ScriptedStatuses {
        &self.statuses
    }

    /// Borrow the state for setup and assertion.
    pub fn state(&self) -> std::sync::MutexGuard<'_, NzbgetState> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// The recorded calls of one method.
    pub fn calls_to(&self, method: &str) -> Vec<NzbgetCall> {
        self.state()
            .calls
            .iter()
            .filter(|call| call.method == method)
            .cloned()
            .collect()
    }

    /// Add a queue job named `name` (also its dupe key).
    pub fn group_job(&self, id: i64, name: &str, status: &str, size: u32, left: u32) -> &Self {
        self.state()
            .groups
            .push(group(id, name, status, size, left));
        self
    }

    /// Add a history job named `name` (also its dupe key), written to
    /// `dest_dir` on NZBGet's side.
    pub fn history_job(
        &self,
        id: i64,
        name: &str,
        status: &str,
        dest_dir: &str,
        size: u32,
    ) -> &Self {
        let mut entry = template(HISTORY);
        set_job(&mut entry, id, name, status, size);
        entry["DestDir"] = json!(dest_dir);
        self.state().history.push(entry);
        self
    }

    /// The router: `/jsonrpc` plus the NZB fixture a direct grab reads.
    pub fn router(&self) -> Router {
        let router = Router::new()
            .route("/jsonrpc", post(rpc))
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
            .with_state(self.clone());
        self.statuses.layer(router)
    }
}

/// A recorded job with its id, names, status and size replaced.
fn set_job(entry: &mut Value, id: i64, name: &str, status: &str, size: u32) {
    for (key, value) in [
        ("NZBID", json!(id)),
        ("ID", json!(id)),
        ("Name", json!(name)),
        ("NZBName", json!(name)),
        ("NZBNicename", json!(name)),
        ("NZBFilename", json!(format!("{name}.nzb"))),
        ("DupeKey", json!(name)),
        ("Status", json!(status)),
        ("FileSizeLo", json!(size)),
        (
            "DestDir",
            json!(format!("/downloads/intermediate/{name}.#{id}")),
        ),
    ] {
        if entry.get(key).is_some() {
            entry[key] = value;
        }
    }
}

fn group(id: i64, name: &str, status: &str, size: u32, left: u32) -> Value {
    let mut entry = template(LISTGROUPS);
    set_job(&mut entry, id, name, status, size);
    entry["RemainingSizeLo"] = json!(left);
    entry
}

fn reply(result: Value) -> Response {
    json_reply(json!({"version": "1.1", "id": 1, "result": result}))
}

fn json_reply(body: Value) -> Response {
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "application/json")],
        body.to_string(),
    )
        .into_response()
}

fn rpc_failure(message: &str) -> Response {
    json_reply(json!({
        "version": "1.1", "id": 1,
        "error": {"name": "JSONRPCError", "code": 1, "message": message},
    }))
}

/// `Basic base64(user:pass)` for the required sign-in.
fn basic(username: &str, password: &str) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let input = format!("{username}:{password}").into_bytes();
    let mut out = String::new();
    for chunk in input.chunks(3) {
        let b = [
            chunk[0],
            chunk.get(1).copied().unwrap_or(0),
            chunk.get(2).copied().unwrap_or(0),
        ];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        for (index, shift) in [18, 12, 6, 0].into_iter().enumerate() {
            if index <= chunk.len() {
                out.push(TABLE[((n >> shift) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    format!("Basic {out}")
}

fn ids(params: &Value) -> Vec<i64> {
    params
        .get(2)
        .and_then(Value::as_array)
        .map(|ids| ids.iter().filter_map(Value::as_i64).collect())
        .unwrap_or_default()
}

fn take(entries: &mut Vec<Value>, ids: &[i64]) -> Vec<Value> {
    let (taken, kept) = std::mem::take(entries)
        .into_iter()
        .partition(|entry| ids.contains(&entry["NZBID"].as_i64().unwrap_or(-1)));
    *entries = kept;
    taken
}

async fn rpc(State(mock): State<NzbgetMock>, headers: HeaderMap, body: Bytes) -> Response {
    let request: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    let method = request["method"].as_str().unwrap_or_default().to_owned();
    let params = request["params"].clone();
    let auth = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
    let mut state = mock.state();
    state.calls.push(NzbgetCall {
        method: method.clone(),
        params: params.clone(),
        authorized: auth.is_some(),
    });
    if let Some((username, password)) = state.credentials.clone()
        && auth.as_deref() != Some(basic(&username, &password).as_str())
    {
        return (
            StatusCode::UNAUTHORIZED,
            [
                (header::WWW_AUTHENTICATE, "Basic realm=\"NZBGet\""),
                (header::CONTENT_TYPE, "text/plain"),
            ],
        )
            .into_response();
    }
    if let Some(message) = state.rpc_error.clone() {
        return rpc_failure(&message);
    }
    match method.as_str() {
        "version" => reply(json!(state.version)),
        "config" => reply(Value::Array(
            state
                .config
                .iter()
                .map(|(name, value)| json!({"Name": name, "Value": value}))
                .collect(),
        )),
        "listgroups" => reply(Value::Array(state.groups.clone())),
        "history" => reply(Value::Array(state.history.clone())),
        "append" => {
            let id = state.next_id;
            if id > 0 {
                state.next_id += 1;
                let file = params[0].as_str().unwrap_or_default();
                let name = file.strip_suffix(".nzb").unwrap_or(file);
                let mut entry = group(id, name, "QUEUED", 100, 100);
                entry["DupeKey"] = params[6].clone();
                entry["Category"] = params[2].clone();
                state.groups.push(entry);
            }
            reply(json!(id))
        }
        "editqueue" => {
            let command = params[0].as_str().unwrap_or_default().to_owned();
            let ids = ids(&params);
            let found = match command.as_str() {
                "GroupDelete" | "GroupFinalDelete" => {
                    let taken = take(&mut state.groups, &ids);
                    if command == "GroupDelete" {
                        for group in &taken {
                            let mut entry = template(HISTORY);
                            let text = |key: &str| group[key].as_str().unwrap_or_default();
                            let id = group["NZBID"].as_i64().unwrap_or_default();
                            let size = group["FileSizeLo"].as_u64().unwrap_or_default() as u32;
                            set_job(&mut entry, id, text("NZBName"), "DELETED/MANUAL", size);
                            entry["DupeKey"] = group["DupeKey"].clone();
                            entry["Category"] = group["Category"].clone();
                            state.history.push(entry);
                        }
                    }
                    !taken.is_empty()
                }
                "HistoryDelete" | "HistoryFinalDelete" => {
                    !take(&mut state.history, &ids).is_empty()
                }
                _ => return rpc_failure("Invalid command"),
            };
            reply(json!(found))
        }
        _ => rpc_failure("Invalid procedure"),
    }
}

#[cfg(test)]
mod tests {
    use super::basic;

    #[test]
    fn basic_auth_header_encodes_like_rfc_7617() {
        assert_eq!(
            basic("nzbget", "tegbzn6789"),
            "Basic bnpiZ2V0OnRlZ2J6bjY3ODk="
        );
        assert_eq!(basic("a", "b"), "Basic YTpi");
    }
}
