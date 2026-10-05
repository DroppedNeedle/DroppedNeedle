//! The wire protocol between the server and a plugin.
//!
//! JSON-RPC 2.0, one JSON message per line (UTF-8, `\n` terminated). Both
//! sides send requests, responses and notifications over the same pair of
//! streams; for a subprocess that is the child's stdin and stdout. Nothing
//! here knows about processes, so another transport (a WebAssembly
//! sandbox, a socket) can carry the same messages later without changing
//! plugins. PLUGINS.md documents every method below.
//!
//! The protocol is versioned separately from the manifest's
//! `api_version`: `api_version` says which capabilities a manifest may
//! declare, [`PROTOCOL_VERSION`] says how the messages look. The host
//! sends its version in `initialize` and refuses a plugin that answers
//! with one it does not speak.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Wire protocol version this host speaks.
pub const PROTOCOL_VERSION: i64 = 1;

/// Largest single message the host reads from a plugin (8 MiB). A longer
/// line is a protocol error and restarts the plugin.
pub const MAX_MESSAGE_BYTES: usize = 8 * 1024 * 1024;

/// Host-to-plugin methods.
pub mod methods {
    /// First call after start: versions, settings and paths in; the
    /// implemented capabilities out.
    pub const INITIALIZE: &str = "initialize";
    /// Ask the plugin to exit cleanly. The host kills it shortly after.
    pub const SHUTDOWN: &str = "shutdown";
    /// Notification: the admin saved new settings.
    pub const SETTINGS_UPDATE: &str = "settings.update";
    /// Notification: the host gave up waiting on one request.
    pub const CANCEL: &str = "$/cancel";
    /// Health and readiness (`is_configured` plus `health_check` in v2).
    pub const HEALTH: &str = "plugin.health";
    /// `scrobbler`: one accepted play.
    pub const SCROBBLE: &str = "scrobbler.on_scrobble";
    /// `purchase_links`: links for one album.
    pub const PURCHASE_LINKS: &str = "purchase_links.get";
    /// `subscriber`: one engine event.
    pub const EVENT: &str = "subscriber.on_event";
    /// `scheduler`: one tick.
    pub const TICK: &str = "scheduler.on_tick";
    /// `publisher`: one `/ext/` request.
    pub const ROUTE: &str = "routes.handle";
    /// `metadata_provider`: artist enrichment.
    pub const ENRICH_ARTIST: &str = "metadata.enrich_artist";
    /// `metadata_provider`: album enrichment.
    pub const ENRICH_ALBUM: &str = "metadata.enrich_album";
    /// `streaming_source`: resolve one recording to a path or URL.
    pub const RESOLVE_STREAM: &str = "stream.resolve";
    /// `indexer`: album search.
    pub const SEARCH_ALBUM: &str = "indexer.search_album";
    /// `indexer`: track search.
    pub const SEARCH_TRACK: &str = "indexer.search_track";
    /// `download_client`: start one download.
    pub const ENQUEUE: &str = "download_client.enqueue";
    /// `download_client`: progress of one download.
    pub const STATUS: &str = "download_client.status";
    /// `download_client`: files a download produced.
    pub const INSPECT: &str = "download_client.inspect";
    /// `download_client`: forget a finished download.
    pub const DISCARD: &str = "download_client.discard";
    /// `download_client`: stop a running download.
    pub const ABORT: &str = "download_client.abort";

    /// Plugin-to-host: publish one hint (`publisher`).
    pub const HOST_PUBLISH: &str = "host.publish";
    /// Plugin-to-host: read one durable state key.
    pub const HOST_STATE_GET: &str = "host.state.get";
    /// Plugin-to-host: write one durable state key.
    pub const HOST_STATE_SET: &str = "host.state.set";
    /// Plugin-to-host notification: one log line.
    pub const HOST_LOG: &str = "host.log";
}

/// Standard and application error codes.
pub mod codes {
    /// The message was not JSON.
    pub const PARSE_ERROR: i64 = -32700;
    /// The message was not a JSON-RPC request.
    pub const INVALID_REQUEST: i64 = -32600;
    /// The receiver does not implement the method.
    pub const METHOD_NOT_FOUND: i64 = -32601;
    /// The params did not decode.
    pub const INVALID_PARAMS: i64 = -32602;
    /// The receiver failed in an unexpected way.
    pub const INTERNAL_ERROR: i64 = -32603;
    /// The plugin's own code raised or refused.
    pub const PLUGIN_ERROR: i64 = -32000;
    /// The host refused a plugin request (for example a bad state key).
    pub const HOST_REFUSED: i64 = -32001;
}

/// One error object.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RpcError {
    /// Error code from [`codes`].
    pub code: i64,
    /// Human message.
    pub message: String,
    /// Extra detail, when any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

impl RpcError {
    /// Error with no extra detail.
    pub fn new(code: i64, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            data: None,
        }
    }
}

/// Request and response ids. The host only issues integers; a plugin may
/// use strings, which the host echoes back unchanged.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(untagged)]
pub enum RpcId {
    /// Integer id.
    Num(i64),
    /// String id.
    Str(String),
}

/// Any message on the wire, before classification. Unknown fields are
/// ignored so either side can add optional fields later.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct RawMessage {
    /// Must be `"2.0"`.
    #[serde(default)]
    pub jsonrpc: String,
    /// Present on requests and responses.
    #[serde(default)]
    pub id: Option<RpcId>,
    /// Present on requests and notifications.
    #[serde(default)]
    pub method: Option<String>,
    /// Request params.
    #[serde(default)]
    pub params: Option<Value>,
    /// Success payload.
    #[serde(default)]
    pub result: Option<Value>,
    /// Failure payload.
    #[serde(default)]
    pub error: Option<RpcError>,
}

/// A classified incoming message.
#[derive(Debug, Clone, PartialEq)]
pub enum Incoming {
    /// The peer asks for something and wants an answer.
    Request {
        /// Echoed back in the response.
        id: RpcId,
        /// Method name.
        method: String,
        /// Params (`null` when absent).
        params: Value,
    },
    /// The peer tells us something and wants no answer.
    Notification {
        /// Method name.
        method: String,
        /// Params (`null` when absent).
        params: Value,
    },
    /// The answer to one of our requests.
    Response {
        /// The id we sent.
        id: RpcId,
        /// Result or error.
        outcome: Result<Value, RpcError>,
    },
}

/// Parse and classify one line. Anything that is not a well-formed
/// JSON-RPC 2.0 message is an error naming the problem.
pub fn parse_line(line: &[u8]) -> Result<Incoming, String> {
    let raw: RawMessage =
        serde_json::from_slice(line).map_err(|error| format!("not a JSON message: {error}"))?;
    if raw.jsonrpc != "2.0" {
        return Err("message is missing jsonrpc \"2.0\"".to_owned());
    }
    match (raw.method, raw.id) {
        (Some(method), Some(id)) => Ok(Incoming::Request {
            id,
            method,
            params: raw.params.unwrap_or(Value::Null),
        }),
        (Some(method), None) => Ok(Incoming::Notification {
            method,
            params: raw.params.unwrap_or(Value::Null),
        }),
        (None, Some(id)) => {
            let outcome = match (raw.result, raw.error) {
                (_, Some(error)) => Err(error),
                (Some(result), None) => Ok(result),
                (None, None) => Ok(Value::Null),
            };
            Ok(Incoming::Response { id, outcome })
        }
        (None, None) => Err("message has neither a method nor an id".to_owned()),
    }
}

/// Encode a request as one line (newline included).
pub fn request_line(id: i64, method: &str, params: &Value) -> String {
    line(&serde_json::json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": method,
        "params": params,
    }))
}

/// Encode a notification as one line (newline included).
pub fn notification_line(method: &str, params: &Value) -> String {
    line(&serde_json::json!({
        "jsonrpc": "2.0",
        "method": method,
        "params": params,
    }))
}

/// Encode a response as one line (newline included).
pub fn response_line(id: &RpcId, outcome: &Result<Value, RpcError>) -> String {
    let body = match outcome {
        Ok(result) => serde_json::json!({"jsonrpc": "2.0", "id": id, "result": result}),
        Err(error) => serde_json::json!({"jsonrpc": "2.0", "id": id, "error": error}),
    };
    line(&body)
}

fn line(value: &Value) -> String {
    // serde_json escapes control characters inside strings, so the encoded
    // message never contains a raw newline and one line is one message.
    let mut text = value.to_string();
    text.push('\n');
    text
}

/// `initialize` params.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InitializeParams {
    /// Host protocol version.
    pub protocol_version: i64,
    /// Host name and version, for logs.
    pub host: HostInfo,
    /// The plugin as the host sees it.
    pub plugin: PluginIdentity,
    /// Current settings, secrets decrypted.
    pub settings: std::collections::HashMap<String, String>,
    /// Plugin code directory.
    pub plugin_dir: String,
    /// Writable directory for the plugin's own files.
    pub data_dir: String,
}

/// Host identity in `initialize`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HostInfo {
    /// Always `droppedneedle`.
    pub name: String,
    /// Server version.
    pub version: String,
}

/// Plugin identity in `initialize`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PluginIdentity {
    /// Manifest name.
    pub name: String,
    /// Manifest version.
    pub version: String,
    /// Manifest `api_version`.
    pub api_version: i64,
    /// Manifest entrypoint (`module:Class`), empty for custom commands.
    pub entrypoint: String,
    /// Declared capabilities.
    pub capabilities: Vec<String>,
}

/// `initialize` result.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InitializeResult {
    /// Protocol version the plugin speaks.
    pub protocol_version: i64,
    /// Capabilities the plugin implements.
    #[serde(default)]
    pub capabilities: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_requests_notifications_and_responses() {
        let request = parse_line(
            br#"{"jsonrpc":"2.0","id":3,"method":"host.publish","params":{"kind":"x"}}"#,
        );
        assert!(matches!(
            request,
            Ok(Incoming::Request {
                id: RpcId::Num(3),
                ..
            })
        ));
        let note = parse_line(br#"{"jsonrpc":"2.0","method":"host.log"}"#);
        assert!(matches!(
            note,
            Ok(Incoming::Notification {
                params: Value::Null,
                ..
            })
        ));
        let failed =
            parse_line(br#"{"jsonrpc":"2.0","id":"a","error":{"code":-32601,"message":"no"}}"#);
        match failed {
            Ok(Incoming::Response {
                outcome: Err(error),
                ..
            }) => {
                assert_eq!(error.code, codes::METHOD_NOT_FOUND)
            }
            other => panic!("unexpected {other:?}"),
        }
        assert!(parse_line(br#"{"id":1,"result":1}"#).is_err());
        assert!(parse_line(b"print('hello')").is_err());
    }

    #[test]
    fn encoded_lines_hold_exactly_one_message() {
        let text = request_line(1, "routes.handle", &serde_json::json!({"body": "a\nb"}));
        assert_eq!(text.matches('\n').count(), 1);
        assert!(text.ends_with('\n'));
    }
}
