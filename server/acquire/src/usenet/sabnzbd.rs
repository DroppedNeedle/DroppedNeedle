//! SABnzbd download client: enqueue NZBs, track jobs, locate output.
//!
//! [`SabnzbdClient`] is the raw API wrapper (no business logic),
//! verified against SABnzbd 5.0.4: every call appends `output=json` + `apikey`
//! as suffix query params (never headers; Lidarr `SabnzbdProxy`), adds go
//! through `mode=addfile` (multipart POST of fetched + validated NZB bytes),
//! and `mode=addurl` is the enqueue fallback only. [`SabnzbdQueue`] is the
//! download-client layer: pre-enqueue `job_name` correlation, queue→history
//! status walk, storage remap onto the DroppedNeedle downloads mount, and
//! folder-based import source.

use std::path::{Path, PathBuf};
use std::time::Duration;

use reqwest::Client;
use thiserror::Error;
use tokio::sync::Mutex;

use super::policy::UsenetPolicy;
use crate::client_error::{ClientError, Service, retry_after};
use crate::landing::DownloadsFolder;
use crate::landing::confine::{Place, place};
use droppedneedle_kernel::http_client::{BodyError, read_capped, transport_error};

pub use super::sabnzbd_models::{
    AddResponse, Category, Config, History, HistorySlot, Misc, Queue, QueueSlot,
};
use super::sabnzbd_models::{
    CategoriesReply, ConfigReply, HistoryReply, QueueReply, StatusReply, VersionReply,
};

/// The most an NZB may weigh. An NZB lists one segment per posted article:
/// about 120 bytes of XML for an article of about 750 KB, so roughly
/// 1/6000 of the release. That is 2 MB for a 10 GB box set and 17 MB for
/// 100 GB. 50 MB leaves room for older posts with smaller articles and
/// still bounds what one enqueue holds in memory, since the NZB is handed
/// to SABnzbd from memory. The figures are arithmetic, not a measurement
/// of a live indexer.
pub const MAX_NZB_BYTES: usize = 50 * 1024 * 1024;

/// The most one SABnzbd API answer may weigh. The largest are the queue
/// and a 50-job history page. At a few kilobytes a job, and tens for one
/// with a long unpack log, a real answer is a few megabytes at worst.
/// Those sizes are an estimate, not a measurement.
pub const MAX_SABNZBD_BODY_BYTES: usize = 16 * 1024 * 1024;

// ---------------------------------------------------------------------------
// Errors + credential scrubbing.
// ---------------------------------------------------------------------------

/// A SABnzbd call that failed. HTTP, transport, key and decode failures
/// are a [`ClientError`]; the rest are SABnzbd's own outcomes.
#[derive(Debug, Clone, Error)]
pub enum SabnzbdError {
    /// The call failed the way any client call can. A refused key comes
    /// as HTTP 200 with "API Key Incorrect" or "API Key Required" in
    /// either error form (SABnzbd 5.0.4), so it is matched by text. The
    /// transport detail never holds the request URL, which carries the
    /// API key.
    #[error(transparent)]
    Client(#[from] ClientError),
    /// SABnzbd answered an API-level error other than a refused key (both
    /// wire forms).
    #[error("{0}")]
    Api(String),
    /// The indexer answered the NZB fetch with an HTTP error, so handing
    /// SABnzbd the same URL cannot help.
    #[error("the indexer answered HTTP {status} for the NZB")]
    NzbRefused {
        /// The indexer's status code.
        status: u16,
    },
    /// The indexer's NZB passed [`MAX_NZB_BYTES`].
    #[error("the NZB is larger than {} MB and was not fetched", MAX_NZB_BYTES / (1024 * 1024))]
    NzbTooLarge,
    /// Two jobs matched one handle; refusing to guess.
    #[error("SABnzbd returned an ambiguous job identity")]
    AmbiguousIdentity,
    /// The add succeeded on the wire but carried no `nzo_id`.
    #[error("SABnzbd rejected the NZB (no nzo_id returned)")]
    RejectedNzb,
    /// The addurl fallback got the same empty envelope.
    #[error("SABnzbd rejected the NZB URL (no nzo_id returned)")]
    RejectedNzbUrl,
    /// Enqueue without an NZB URL is meaningless for Usenet.
    #[error("enqueue requires an nzb_url for the usenet source")]
    MissingNzbUrl,
}

impl SabnzbdError {
    /// Whether this failure means bad credentials.
    pub fn is_auth(&self) -> bool {
        matches!(self, SabnzbdError::Client(error) if error.is_auth())
    }
}

/// NZB fetch failure, classified so the enqueue path can decide: only a
/// no-response transport failure falls back to addurl; a definitive indexer
/// HTTP error or a content rejection never does.
#[derive(Debug, Clone, Error)]
pub enum NzbFetchError {
    /// No response at all (DNS/refused/timeout): addurl may help. The
    /// detail never holds the enclosure URL, which carries the indexer key.
    #[error("NZB fetch failed: {0}")]
    Transport(String),
    /// The indexer answered with HTTP 4xx/5xx: handing the same URL to
    /// SABnzbd cannot help.
    #[error("NZB fetch returned HTTP {status}")]
    Definitive {
        /// Status code.
        status: u16,
        /// Body snippet.
        snippet: String,
    },
    /// The indexer answered with a non-NZB body (an error/limit page):
    /// a deterministic rejection, blocklist-worthy, never addurl'd.
    #[error("indexer returned a non-NZB body (likely an error/limit page), not an NZB")]
    ContentRejection {
        /// Status code.
        status: u16,
        /// Response content type, when present.
        content_type: Option<String>,
        /// Body snippet.
        snippet: String,
    },
    /// The NZB passed [`MAX_NZB_BYTES`]. The indexer answered, so SABnzbd
    /// is not handed the URL either.
    #[error("the NZB is larger than {} MB", MAX_NZB_BYTES / (1024 * 1024))]
    TooLarge,
}

/// Query keys whose value is scrubbed, with whether the key must start a
/// word. Each is one a client here sends or a recorded response shows:
/// - `apikey`: the Newznab client's own parameter, SABnzbd's, and the
///   download URL Prowlarr returns (`?apikey=...&link=...`);
/// - `api_key`: the underscore spelling;
/// - `r`: DrunkenSlug's per-user key in the recorded feed's enclosures
///   (`...nzb&i=1&r=KEY`), anchored so `error=` and the like pass;
/// - `link`: the other value in Prowlarr's download URL, an opaque
///   per-download token. What it encodes is not verified, so it is
///   scrubbed with the key. Anchored like `r`;
/// - `nzbkey` and `nzb_key`: SABnzbd's add-only key;
/// - `password` and `pass`: a login carried in a query or a JSON answer.
///   `pass` is anchored so `bypass=` passes.
///
/// `i=` (DrunkenSlug's user id) stays visible. An unanchored key also
/// matches as the end of a longer JSON member name (`"sabnzbd_apikey"`).
const SECRET_QUERY_KEYS: [(&str, bool); 8] = [
    ("api_key", false),
    ("apikey", false),
    ("nzb_key", false),
    ("nzbkey", false),
    ("password", false),
    ("pass", true),
    ("link", true),
    ("r", true),
];

/// Shortest URL path segment read as a key. Indexer keys seen so far are
/// 32 characters; a shorter path part (`getnzb`, `api`, a numeric id) is
/// left alone.
const PATH_KEY_MIN_LEN: usize = 20;

/// Scrub credentials from text SABnzbd or an indexer sent back: the
/// Newznab enclosure URL carries the indexer's key and either may echo it.
/// A [`SECRET_QUERY_KEYS`] value is scrubbed as `key=value`, percent-encoded
/// as `key%3Dvalue` (a URL nested in another's query) and as a JSON member
/// `"key": "value"`, also with its quotes escaped (`\"key\": \"value\"`,
/// JSON inside a JSON string). In an `http(s)` URL (slashes escaped as
/// `\/` or not), a password in the authority (`user:password@host`) is
/// scrubbed, and so is a path segment that looks like a key (long, letters
/// and digits), as an indexer may put its key in the path
/// (`/fetch/<id>/<key>`). Matched without a regex dependency. A body is
/// scrubbed whole before it is cut short, so a cut cannot leave half a key
/// unmatched.
pub fn redact_query_secrets(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let mut index = 0;
    // Inside an `http(s)` URL, before its query or fragment.
    let mut in_url_path = false;
    while index < bytes.len() {
        if let Some((kept, end)) = secret_value_at(bytes, index) {
            out.push_str(&text[index..index + kept]);
            out.push_str("***");
            index = end;
        } else if let Some(scheme_end) = url_scheme_end(bytes, index) {
            out.push_str(&text[index..scheme_end]);
            index = scheme_end;
            if let Some((password, at)) = userinfo_password(bytes, index) {
                out.push_str(&text[index..password]);
                out.push_str("***");
                index = at;
            }
            in_url_path = true;
        } else if let Some(end) = path_key_end(bytes, index).filter(|_| in_url_path) {
            out.push_str("***");
            index = end;
        } else {
            let ch = text[index..].chars().next().unwrap_or('\u{FFFD}');
            if matches!(ch, '?' | '#' | '"' | '\'' | '<' | '>') || ch.is_whitespace() {
                in_url_path = false;
            }
            out.push(ch);
            index += ch.len_utf8();
        }
    }
    out
}

/// A secret key at `index`, case-insensitive: how many bytes of key text
/// to keep and where its value ends. A key that must start a word does
/// not match after an identifier char; a percent-encoded `&`, `?` or `;`
/// before it starts a word too.
fn secret_value_at(bytes: &[u8], index: usize) -> Option<(usize, usize)> {
    let rest = &bytes[index..];
    let encoded_separator = index >= 3
        && [b"%26".as_slice(), b"%3F", b"%3B"]
            .iter()
            .any(|separator| starts_with_ignore_case(&bytes[index - 3..], separator));
    let starts_word = index == 0
        || encoded_separator
        || !matches!(bytes[index - 1], b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'_');
    SECRET_QUERY_KEYS.iter().find_map(|(key, anchored)| {
        let key = key.as_bytes();
        if rest.len() <= key.len() || !rest[..key.len()].eq_ignore_ascii_case(key) {
            return None;
        }
        if *anchored && !starts_word {
            return None;
        }
        let after = index + key.len();
        if bytes[after] == b'=' {
            return Some((key.len() + 1, query_value_end(bytes, after + 1)));
        }
        if starts_with_ignore_case(&bytes[after..], b"%3D") {
            return Some((key.len() + 3, query_value_end(bytes, after + 3)));
        }
        // A JSON member: `"key"` (or `\"key\"`), a colon, then the value.
        if !json_name_opens_before(bytes, index, *anchored) {
            return None;
        }
        let closing = quote_len(bytes, after)?;
        let colon = skip_space(bytes, after + closing);
        if bytes.get(colon) != Some(&b':') {
            return None;
        }
        let start = skip_space(bytes, colon + 1);
        if let Some(quote) = quote_len(bytes, start) {
            let value = start + quote;
            let end = if quote == 2 {
                escaped_string_end(bytes, value)
            } else {
                json_string_end(bytes, value)
            };
            return Some((value - index, end));
        }
        let end = start
            + bytes[start..]
                .iter()
                .take_while(|byte| {
                    !matches!(byte, b',' | b'}' | b']' | b'\\') && !byte.is_ascii_whitespace()
                })
                .count();
        Some((start - index, end))
    })
}

/// End of a query value: `&` (plain or as `%26`), a quote, a backslash
/// (an escaped quote) or whitespace.
fn query_value_end(bytes: &[u8], mut index: usize) -> usize {
    while index < bytes.len()
        && !matches!(bytes[index], b'&' | b'\'' | b'"' | b'\\')
        && !bytes[index].is_ascii_whitespace()
        && !starts_with_ignore_case(&bytes[index..], b"%26")
    {
        index += 1;
    }
    index
}

/// Where the quote closing a JSON string whose text starts at `index` is.
fn json_string_end(bytes: &[u8], mut index: usize) -> usize {
    while index < bytes.len() && bytes[index] != b'"' {
        index += if bytes[index] == b'\\' { 2 } else { 1 };
    }
    index.min(bytes.len())
}

/// Where an escaped JSON string (`\"...\"`) whose text starts at `index`
/// ends: at the next `\"`, or at a bare `"` should the escaping stop.
fn escaped_string_end(bytes: &[u8], mut index: usize) -> usize {
    while index < bytes.len() && bytes[index] != b'"' {
        if bytes[index] == b'\\' && bytes.get(index + 1) == Some(&b'"') {
            return index;
        }
        index += 1;
    }
    index
}

/// The length of a JSON quote at `index`: 1 for `"`, 2 for an escaped `\"`.
fn quote_len(bytes: &[u8], index: usize) -> Option<usize> {
    match bytes.get(index..) {
        Some([b'"', ..]) => Some(1),
        Some([b'\\', b'"', ..]) => Some(2),
        _ => None,
    }
}

/// True when a JSON member name opens right before `index` or, for an
/// unanchored key, before a run of name characters that ends there.
fn json_name_opens_before(bytes: &[u8], index: usize, anchored: bool) -> bool {
    let mut start = index;
    if !anchored {
        while start > 0
            && matches!(bytes[start - 1], b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'_' | b'-')
        {
            start -= 1;
        }
    }
    start > 0 && bytes[start - 1] == b'"'
}

fn skip_space(bytes: &[u8], index: usize) -> usize {
    index
        + bytes
            .get(index..)
            .unwrap_or_default()
            .iter()
            .take_while(|byte| byte.is_ascii_whitespace())
            .count()
}

fn starts_with_ignore_case(bytes: &[u8], prefix: &[u8]) -> bool {
    bytes.len() >= prefix.len() && bytes[..prefix.len()].eq_ignore_ascii_case(prefix)
}

/// When an `http://` or `https://` scheme starts at `index` (its slashes
/// escaped as `\/` or not), where it ends.
fn url_scheme_end(bytes: &[u8], index: usize) -> Option<usize> {
    let rest = &bytes[index..];
    [
        b"https://".as_slice(),
        b"http://".as_slice(),
        br"https:\/\/".as_slice(),
        br"http:\/\/".as_slice(),
    ]
    .into_iter()
    .find(|scheme| starts_with_ignore_case(rest, scheme))
    .map(|scheme| index + scheme.len())
}

/// When the URL authority starting at `index` holds `user:password@`,
/// where the password starts and where its `@` is.
fn userinfo_password(bytes: &[u8], index: usize) -> Option<(usize, usize)> {
    let authority = &bytes[index..];
    let len = authority
        .iter()
        .position(|byte| {
            matches!(
                byte,
                b'/' | b'\\' | b'?' | b'#' | b'"' | b'\'' | b'<' | b'>'
            ) || byte.is_ascii_whitespace()
        })
        .unwrap_or(authority.len());
    let at = authority[..len].iter().rposition(|byte| *byte == b'@')?;
    let colon = authority[..at].iter().position(|byte| *byte == b':')?;
    Some((index + colon + 1, index + at))
}

/// When a URL path segment that looks like a key starts at `index`, where
/// it ends. Only the segment's letters and digits are taken, so whatever
/// follows them (`&r=...`, `;apikey=...`) is still scanned for keys.
fn path_key_end(bytes: &[u8], index: usize) -> Option<usize> {
    if index == 0 || bytes[index - 1] != b'/' {
        return None;
    }
    let end = index
        + bytes[index..]
            .iter()
            .take_while(|byte| byte.is_ascii_alphanumeric())
            .count();
    let whole = bytes
        .get(end)
        .is_none_or(|byte| !matches!(byte, b'.' | b'-' | b'_' | b'%'));
    (whole && looks_like_key(&bytes[index..end])).then_some(end)
}

fn looks_like_key(segment: &[u8]) -> bool {
    segment.len() >= PATH_KEY_MIN_LEN
        && segment.iter().all(u8::is_ascii_alphanumeric)
        && segment.iter().any(u8::is_ascii_digit)
        && segment.iter().any(u8::is_ascii_alphabetic)
}

// ---------------------------------------------------------------------------
// Raw client.
// ---------------------------------------------------------------------------

/// Raw SABnzbd API wrapper. The HTTP client is injected; the
/// full `apikey` is required (the add-only `nzbkey` can't do
/// queue/history/delete) and is never logged. Debug is hand-written: the
/// key never appears in debug output.
pub struct SabnzbdClient {
    http: Client,
    base_url: String,
    api_key: String,
    max_attempts: u32,
    retry_backoff: Duration,
}

impl std::fmt::Debug for SabnzbdClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SabnzbdClient")
            .field("base_url", &self.base_url)
            .field("api_key", &"<redacted>")
            .field("max_attempts", &self.max_attempts)
            .field("retry_backoff", &self.retry_backoff)
            .finish()
    }
}

impl SabnzbdClient {
    /// Build over an injected client. `base_url` is the SABnzbd origin
    /// (`/api` is appended per call).
    pub fn new(
        http: Client,
        base_url: &str,
        api_key: &str,
        max_attempts: u32,
        retry_backoff: Duration,
    ) -> Self {
        SabnzbdClient {
            http,
            base_url: base_url.trim_end_matches('/').to_owned(),
            api_key: api_key.to_owned(),
            max_attempts: max_attempts.max(1),
            retry_backoff,
        }
    }

    /// Server version string (`mode=version`).
    pub async fn version(&self, timeout: Duration) -> Result<String, SabnzbdError> {
        let reply: VersionReply = self.get(&[("mode", "version")], timeout).await?;
        Ok(reply.version)
    }

    /// Category names for the settings picker (`mode=get_cats`).
    pub async fn get_cats(&self, timeout: Duration) -> Result<Vec<String>, SabnzbdError> {
        let reply: CategoriesReply = self.get(&[("mode", "get_cats")], timeout).await?;
        Ok(reply.categories.into_iter().map(|name| name.0).collect())
    }

    /// Full config: `complete_dir` + categories (`mode=get_config`).
    pub async fn get_config(&self, timeout: Duration) -> Result<Config, SabnzbdError> {
        let reply: ConfigReply = self.get(&[("mode", "get_config")], timeout).await?;
        Ok(reply.config)
    }

    /// Current queue (`mode=queue`).
    pub async fn queue(&self, timeout: Duration) -> Result<Queue, SabnzbdError> {
        let reply: QueueReply = self.get(&[("mode", "queue")], timeout).await?;
        Ok(reply.queue)
    }

    /// History window. `nzo_ids` (exact) or `search` (name substring)
    /// narrow the result so a busy server can't bury the job.
    pub async fn history(
        &self,
        limit: u32,
        search: Option<&str>,
        nzo_ids: Option<&str>,
        timeout: Duration,
    ) -> Result<History, SabnzbdError> {
        let limit_text = limit.to_string();
        let mut params: Vec<(&str, &str)> = vec![("mode", "history"), ("limit", &limit_text)];
        if let Some(ids) = nzo_ids {
            params.push(("nzo_ids", ids));
        }
        if let Some(query) = search {
            params.push(("search", query));
        }
        let reply: HistoryReply = self.get(&params, timeout).await?;
        Ok(reply.history)
    }

    /// `mode=addfile` multipart POST. `cat`, not `category`, on add (the
    /// Lidarr quirk); the file field is `name` =
    /// (`{job_name}.nzb`, bytes, `application/x-nzb`). Never retried: a
    /// retried add could double-add the job and re-create the `.1` orphan.
    pub async fn add_file(
        &self,
        job_name: &str,
        nzb_bytes: &[u8],
        category: Option<&str>,
        priority: Option<i32>,
        post_processing: Option<i32>,
        timeout: Duration,
    ) -> Result<AddResponse, SabnzbdError> {
        let priority_text;
        let pp_text;
        let mut params: Vec<(&str, &str)> = vec![("mode", "addfile"), ("nzbname", job_name)];
        if let Some(cat) = category {
            params.push(("cat", cat));
        }
        if let Some(value) = priority {
            priority_text = value.to_string();
            params.push(("priority", &priority_text));
        }
        if let Some(value) = post_processing {
            pp_text = value.to_string();
            params.push(("pp", &pp_text));
        }
        let (body, content_type) = multipart_nzb(job_name, nzb_bytes);
        let url = self.api_url();
        let query = self.suffixed(&params);
        let response = self
            .http
            .post(&url)
            .query(&query)
            .header("content-type", content_type)
            .body(body)
            .timeout(timeout)
            .send()
            .await
            .map_err(|err| ClientError::transport(Service::Sabnzbd, transport_error(err)))?;
        self.parse_add(response).await
    }

    /// `mode=addurl` GET: hand SABnzbd the Newznab enclosure URL and let it
    /// fetch the NZB itself. Enqueue fallback only, for indexers reachable
    /// solely from the SABnzbd host; like `add_file` it mutates the queue
    /// and is never retried. Errors are scrubbed of echoed enclosure
    /// credentials before they surface.
    pub async fn add_url(
        &self,
        job_name: &str,
        nzb_url: &str,
        category: Option<&str>,
        priority: Option<i32>,
        post_processing: Option<i32>,
        timeout: Duration,
    ) -> Result<AddResponse, SabnzbdError> {
        let priority_text;
        let pp_text;
        let mut params: Vec<(&str, &str)> =
            vec![("mode", "addurl"), ("name", nzb_url), ("nzbname", job_name)];
        if let Some(cat) = category {
            params.push(("cat", cat));
        }
        if let Some(value) = priority {
            priority_text = value.to_string();
            params.push(("priority", &priority_text));
        }
        if let Some(value) = post_processing {
            pp_text = value.to_string();
            params.push(("pp", &pp_text));
        }
        let url = self.api_url();
        let query = self.suffixed(&params);
        let response = self
            .http
            .get(&url)
            .query(&query)
            .timeout(timeout)
            .send()
            .await
            .map_err(|err| ClientError::transport(Service::Sabnzbd, transport_error(err)))?;
        match self.parse_add(response).await {
            Ok(added) => Ok(added),
            Err(err) => Err(redacted_addurl_error(&err)),
        }
    }

    /// Delete one queue job (`del_files` removes its incomplete bytes).
    pub async fn delete_queue(
        &self,
        nzo_id: &str,
        del_files: bool,
        timeout: Duration,
    ) -> Result<bool, SabnzbdError> {
        let flag = if del_files { "1" } else { "0" };
        let reply: StatusReply = self
            .get(
                &[
                    ("mode", "queue"),
                    ("name", "delete"),
                    ("value", nzo_id),
                    ("del_files", flag),
                ],
                timeout,
            )
            .await?;
        Ok(reply.ok())
    }

    /// Delete one history row. SABnzbd 5.0.4 applies `del_files` to a
    /// failed job's incomplete path only; completed output must use `0`.
    pub async fn delete_history(
        &self,
        nzo_id: &str,
        del_files: bool,
        archive: bool,
        timeout: Duration,
    ) -> Result<bool, SabnzbdError> {
        let files_flag = if del_files { "1" } else { "0" };
        let archive_flag = if archive { "1" } else { "0" };
        let reply: StatusReply = self
            .get(
                &[
                    ("mode", "history"),
                    ("name", "delete"),
                    ("value", nzo_id),
                    ("del_files", files_flag),
                    ("archive", archive_flag),
                ],
                timeout,
            )
            .await?;
        Ok(reply.ok())
    }

    /// GET the release's NZB URL (the Newznab enclosure, apikey already
    /// embedded) and validate the bytes are a real NZB, not an indexer
    /// error page. Redirects are followed (reqwest's default policy).
    pub async fn fetch_nzb(&self, url: &str, timeout: Duration) -> Result<Vec<u8>, NzbFetchError> {
        let response = self
            .get_with_retry(url, &[], timeout)
            .await
            .map_err(|err| NzbFetchError::Transport(transport_error(err)))?;
        let status = response.status().as_u16();
        if status >= 400 {
            let snippet = response_snippet(response).await;
            return Err(NzbFetchError::Definitive { status, snippet });
        }
        let content_type = response
            .headers()
            .get("content-type")
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned);
        let bytes = read_capped(response, MAX_NZB_BYTES)
            .await
            .map_err(|error| match error {
                BodyError::TooLarge => NzbFetchError::TooLarge,
                BodyError::Transport(detail) => NzbFetchError::Transport(detail),
            })?;
        let head: Vec<u8> = bytes.iter().take(512).copied().collect();
        if !looks_like_nzb(&head) {
            let snippet = snippet_of(&bytes);
            return Err(NzbFetchError::ContentRejection {
                status,
                content_type,
                snippet,
            });
        }
        Ok(bytes)
    }

    fn api_url(&self) -> String {
        format!("{}/api", self.base_url)
    }

    /// Params with `output=json` + `apikey` forced to the end (Lidarr
    /// `SabnzbdProxy.BuildRequest`).
    fn suffixed(&self, params: &[(&str, &str)]) -> Vec<(String, String)> {
        let mut out: Vec<(String, String)> = params
            .iter()
            .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
            .collect();
        out.push(("output".to_owned(), "json".to_owned()));
        out.push(("apikey".to_owned(), self.api_key.clone()));
        out
    }

    async fn get<T>(&self, params: &[(&str, &str)], timeout: Duration) -> Result<T, SabnzbdError>
    where
        T: serde::de::DeserializeOwned,
    {
        let url = self.api_url();
        let query = self.suffixed(params);
        let query_refs: Vec<(&str, &str)> = query
            .iter()
            .map(|(key, value)| (key.as_str(), value.as_str()))
            .collect();
        let response = self
            .get_with_retry(&url, &query_refs, timeout)
            .await
            .map_err(|err| ClientError::transport(Service::Sabnzbd, transport_error(err)))?;
        let data = self.parse_json(response).await?;
        serde_json::from_value(data).map_err(|err| non_json(decode_failure(&err)))
    }

    /// Idempotent GETs only: transient transport errors + 5xx retry with
    /// exponential backoff. 4xx and logical errors return for the caller.
    async fn get_with_retry(
        &self,
        url: &str,
        query: &[(&str, &str)],
        timeout: Duration,
    ) -> Result<reqwest::Response, reqwest::Error> {
        let mut attempt = 0;
        loop {
            let send = self
                .http
                .get(url)
                .query(query)
                .timeout(timeout)
                .send()
                .await;
            match send {
                Ok(response) if response.status().as_u16() < 500 => return Ok(response),
                Ok(response) if attempt + 1 >= self.max_attempts => return Ok(response),
                Ok(_) => {}
                Err(err) if attempt + 1 >= self.max_attempts => return Err(err),
                Err(_) => {}
            }
            attempt += 1;
            let wait = self
                .retry_backoff
                .saturating_mul(2u32.saturating_pow(attempt - 1));
            tokio::time::sleep(wait).await;
        }
    }

    async fn parse_json(
        &self,
        response: reqwest::Response,
    ) -> Result<serde_json::Value, SabnzbdError> {
        let status = response.status().as_u16();
        if status >= 400 {
            let wait = retry_after(response.headers());
            let snippet = response_snippet(response).await;
            return Err(ClientError::for_status(
                Service::Sabnzbd,
                status,
                wait,
                snippet.as_bytes(),
            )
            .into());
        }
        let bytes = read_capped(response, MAX_SABNZBD_BODY_BYTES)
            .await
            .map_err(|error| match error {
                BodyError::TooLarge => non_json("the answer is too large to read".to_owned()),
                BodyError::Transport(detail) => {
                    ClientError::transport(Service::Sabnzbd, detail).into()
                }
            })?;
        parse_body(&String::from_utf8_lossy(&bytes))
    }

    async fn parse_add(&self, response: reqwest::Response) -> Result<AddResponse, SabnzbdError> {
        let data = self.parse_json(response).await?;
        serde_json::from_value(data).map_err(|err| non_json(decode_failure(&err)))
    }
}

/// A SABnzbd answer that is not the documented JSON.
fn non_json(detail: String) -> SabnzbdError {
    ClientError::decode(Service::Sabnzbd, detail).into()
}

/// Where and how an answer failed to decode: the decoder's kind, line and
/// column. Never its text or the body, which can quote values from the
/// answer.
fn decode_failure(error: &serde_json::Error) -> String {
    let kind = match error.classify() {
        serde_json::error::Category::Io => "read error",
        serde_json::error::Category::Syntax => "not JSON",
        serde_json::error::Category::Data => "unexpected shape",
        serde_json::error::Category::Eof => "cut short",
    };
    if error.line() == 0 {
        return kind.to_owned();
    }
    format!("{kind} at line {}, column {}", error.line(), error.column())
}

/// A SABnzbd API-level error: a refused key, or SABnzbd's own message.
fn api_error(message: String) -> SabnzbdError {
    if is_auth_error(&message) {
        return ClientError::Auth {
            service: Service::Sabnzbd,
            status: None,
            detail: message,
        }
        .into();
    }
    SabnzbdError::Api(message)
}

/// Rebuild an addurl failure with echoed enclosure credentials
/// scrubbed. SABnzbd's own answer is what can echo
/// them; a transport failure never holds the URL to begin with.
fn redacted_addurl_error(err: &SabnzbdError) -> SabnzbdError {
    let redacted = |detail: &String| redact_query_secrets(detail);
    match err {
        SabnzbdError::Api(message) => SabnzbdError::Api(redacted(message)),
        SabnzbdError::Client(ClientError::Auth {
            service,
            status,
            detail,
        }) => ClientError::Auth {
            service: *service,
            status: *status,
            detail: redacted(detail),
        }
        .into(),
        SabnzbdError::Client(ClientError::BadRequest { service, detail }) => {
            ClientError::BadRequest {
                service: *service,
                detail: redacted(detail),
            }
            .into()
        }
        SabnzbdError::Client(ClientError::Decode { service, detail }) => ClientError::Decode {
            service: *service,
            detail: redacted(detail),
        }
        .into(),
        other => other.clone(),
    }
}

/// Both SABnzbd error forms: plain-text `error: …` and JSON
/// `{"status": false, "error": …}`.
fn parse_body(text: &str) -> Result<serde_json::Value, SabnzbdError> {
    let trimmed = text.trim();
    if trimmed.to_ascii_lowercase().starts_with("error") {
        let message = trimmed
            .split_once(':')
            .map(|(_, rest)| rest.trim())
            .unwrap_or(trimmed)
            .to_owned();
        return Err(api_error(message));
    }
    let data: serde_json::Value =
        serde_json::from_str(text).map_err(|err| non_json(decode_failure(&err)))?;
    if let Some(error_text) = data.get("error").and_then(serde_json::Value::as_str)
        && is_false(data.get("status"))
    {
        return Err(api_error(error_text.to_owned()));
    }
    Ok(data)
}

/// SABnzbd serialises the status boolean as `false` or `"False"`,
/// matched case-insensitively.
fn is_false(value: Option<&serde_json::Value>) -> bool {
    match value {
        Some(serde_json::Value::Bool(flag)) => !flag,
        Some(serde_json::Value::String(text)) => text.trim().eq_ignore_ascii_case("false"),
        _ => false,
    }
}

fn is_auth_error(message: &str) -> bool {
    let low = message.to_ascii_lowercase();
    low.contains("api key incorrect") || low.contains("api key required")
}

/// The start of an error body, credentials scrubbed. One longer than a
/// few kilobytes reads as empty.
async fn response_snippet(response: reqwest::Response) -> String {
    let bytes = read_capped(response, 8 * 1024).await.unwrap_or_default();
    snippet_of(&bytes)
}

/// The first 200 characters of a body, scrubbed whole before the cut.
fn snippet_of(bytes: &[u8]) -> String {
    redact_query_secrets(&String::from_utf8_lossy(bytes))
        .chars()
        .take(200)
        .collect()
}

/// The NZB head check: `<?xml` or `<nzb` up front, or
/// `<nzb` anywhere in the first 512 bytes.
fn looks_like_nzb(head: &[u8]) -> bool {
    let trimmed: Vec<u8> = head
        .iter()
        .skip_while(|byte| byte.is_ascii_whitespace())
        .copied()
        .collect();
    let lower: Vec<u8> = trimmed
        .iter()
        .map(|byte| byte.to_ascii_lowercase())
        .collect();
    lower.starts_with(b"<?xml")
        || lower.starts_with(b"<nzb")
        || lower.windows(4).any(|w| w == b"<nzb")
}

/// Hand-rolled multipart/form-data: the crate has no multipart feature,
/// and the shape is one file field. Field `name`, filename
/// `{job_name}.nzb`, type `application/x-nzb`.
fn multipart_nzb(job_name: &str, nzb_bytes: &[u8]) -> (Vec<u8>, String) {
    let boundary = "droppedneedle-nzb-boundary";
    let mut body = Vec::new();
    body.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
    body.extend_from_slice(
        format!("Content-Disposition: form-data; name=\"name\"; filename=\"{job_name}.nzb\"\r\n")
            .as_bytes(),
    );
    body.extend_from_slice(b"Content-Type: application/x-nzb\r\n\r\n");
    body.extend_from_slice(nzb_bytes);
    body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
    (body, format!("multipart/form-data; boundary={boundary}"))
}

// ---------------------------------------------------------------------------
// Queue layer.
// ---------------------------------------------------------------------------

/// Queue states that move 0 bytes (never active transfers). Anything else
/// in the queue that isn't true `Downloading` is post-processing.
const QUEUE_NOT_ACTIVE: [&str; 4] = ["queued", "grabbing", "propagating", "paused"];
/// History window for job lookups.
const HISTORY_LIMIT: u32 = 50;
/// Failure-path-only bound for the suffix walk.
const REMAP_WALK_BUDGET: usize = 10_000;
/// Bound for the mount-has-files probe.
const MOUNT_PROBE_BUDGET: usize = 5_000;
/// History sample for mount diagnosis.
const DIAGNOSIS_SAMPLE: usize = 3;

/// Client-agnostic task correlation, Usenet half.
#[derive(Debug, Clone, Default)]
pub struct TaskHandle {
    /// Always `usenet` here.
    pub source: String,
    /// `droppedneedle-{task_id}`: the pre-enqueue key.
    pub job_name: String,
    /// SABnzbd's unique key, filled after the add returns.
    pub nzo_id: String,
}

/// One status poll.
#[derive(Debug, Clone, Default)]
pub struct TaskStatus {
    /// `queued` / `downloading` / `processing` / `completed` / `failed`.
    pub status: String,
    /// Usenet jobs are folder-granular: always 1 file.
    pub files_total: u32,
    /// 1 once completed.
    pub files_completed: u32,
    /// Total bytes (queue: from MB; history: `bytes`).
    pub bytes_total: u64,
    /// Bytes in hand.
    pub bytes_downloaded: u64,
    /// 0-100; held at 100 through post-processing.
    pub progress_percent: f64,
    /// Failure detail when `failed`.
    pub error: Option<String>,
    /// True only for the true `Downloading` state.
    pub has_active_transfer: bool,
    /// 1 when the job was found, 0 when it is (not yet) anywhere.
    pub matched_transfers: u32,
}

/// Fresh evidence about bytes and client records.
#[derive(Debug, Clone, Default)]
pub struct Materialization {
    /// `active` / `completed` / `failed` / `missing`.
    pub state: String,
    /// Resolved job id, when known.
    pub nzo_id: String,
    /// SABnzbd-namespace storage path, when reported.
    pub remote_storage: String,
    /// DroppedNeedle mount root.
    pub mount_root: String,
    /// Exact local workspace, when resolvable.
    pub workspace_path: String,
    /// Whether the mount itself is usable.
    pub mount_healthy: bool,
}

/// Mount cross-check.
#[derive(Debug, Clone)]
pub struct MountDiagnosis {
    /// SABnzbd can introspect its downloads: always true here.
    pub supported: bool,
    /// Completed rows seen.
    pub completed_downloads: usize,
    /// Whether the mount holds any file at all.
    pub mount_has_files: bool,
    /// Sampled completions locatable under the mount.
    pub resolvable_downloads: usize,
    /// Sample size.
    pub sampled_downloads: usize,
    /// SABnzbd's own complete dir, in its namespace.
    pub client_downloads_dir: Option<String>,
}

/// Health answer.
#[derive(Debug, Clone)]
pub struct Health {
    /// `ok` or `error`.
    pub status: String,
    /// Server version, when known.
    pub version: Option<String>,
    /// Human summary.
    pub message: String,
}

/// The download side of Usenet: enqueue, poll, abort, locate.
///
/// Enqueue fetches the NZB, validates it, and `addfile`s it; only a
/// no-response transport failure falls back to `addurl`. Status walks
/// queue→history, and only the true `Downloading` state sets
/// `has_active_transfer`, so Grabbing/Queued/Paused/post-processing never
/// trip the stall/queued watchdogs.
pub struct SabnzbdQueue {
    client: SabnzbdClient,
    // The key itself lives only in the raw client; the queue keeps just
    // the configured bit for `is_configured`.
    configured: bool,
    mount: PathBuf,
    policy: UsenetPolicy,
    complete_dir_cache: Mutex<Option<String>>,
}

impl SabnzbdQueue {
    /// Build over the raw client. `downloads_mount` is DroppedNeedle's
    /// mount of SABnzbd's completed output.
    pub fn new(
        client: SabnzbdClient,
        url: &str,
        api_key: &str,
        downloads_mount: PathBuf,
        policy: UsenetPolicy,
    ) -> Self {
        SabnzbdQueue {
            client,
            configured: !url.is_empty() && !api_key.is_empty(),
            // A relative folder means "under the working directory"; made
            // absolute once so every path joined onto it is too.
            mount: std::path::absolute(&downloads_mount).unwrap_or(downloads_mount),
            policy,
            complete_dir_cache: Mutex::new(None),
        }
    }

    /// Client name for routing.
    pub fn client_name(&self) -> &'static str {
        "sabnzbd"
    }

    /// Configured means URL + key were both present at build time.
    pub fn is_configured(&self) -> bool {
        self.configured
    }

    /// DroppedNeedle's mount of SABnzbd's completed output.
    pub fn downloads_mount(&self) -> &Path {
        &self.mount
    }

    /// Health never raises: failures become the error variant.
    pub async fn health_check(&self) -> Health {
        match self.client.version(self.policy.poll_timeout).await {
            Ok(version) => Health {
                status: "ok".to_owned(),
                version: if version.is_empty() {
                    None
                } else {
                    Some(version.clone())
                },
                message: if version.is_empty() {
                    "SABnzbd".to_owned()
                } else {
                    format!("SABnzbd {version}")
                },
            },
            Err(err) => {
                // Health surfaces a plain summary; the technical detail
                // stays in the log, never on the status surface.
                tracing::warn!(%err, "sabnzbd health check failed");
                Health {
                    status: "error".to_owned(),
                    version: None,
                    message: if err.is_auth() {
                        "SABnzbd rejected the API key".to_owned()
                    } else {
                        "SABnzbd unreachable".to_owned()
                    },
                }
            }
        }
    }

    /// SABnzbd's category names (for the settings picker).
    pub async fn get_categories(&self) -> Result<Vec<String>, SabnzbdError> {
        self.client.get_cats(self.policy.poll_timeout).await
    }

    /// SABnzbd's completed-downloads dir (the settings mount hint).
    pub async fn get_complete_dir(&self) -> Result<String, SabnzbdError> {
        Ok(self.complete_dir().await)
    }

    /// Enqueue one album release: `droppedneedle-{task_id}` job, NZB
    /// fetched + validated, `addfile`'d. `category`/`priority`/`pp` ride
    /// along.
    pub async fn enqueue_album(
        &self,
        task_id: &str,
        nzb_url: Option<&str>,
        category: Option<&str>,
        priority: Option<i32>,
        post_processing: Option<i32>,
    ) -> Result<TaskHandle, SabnzbdError> {
        self.enqueue(task_id, None, nzb_url, category, priority, post_processing)
            .await
    }

    /// Enqueue one album release under a worker-built job name. The worker
    /// passes `droppedneedle-{task_id}-{candidate_index}`:
    /// the counter keeps failover attempts distinct on the client and is
    /// the shape the orphan reconciler recognises.
    pub async fn enqueue_album_as(
        &self,
        task_id: &str,
        job_name: &str,
        nzb_url: Option<&str>,
        category: Option<&str>,
        priority: Option<i32>,
        post_processing: Option<i32>,
    ) -> Result<TaskHandle, SabnzbdError> {
        self.enqueue(
            task_id,
            Some(job_name),
            nzb_url,
            category,
            priority,
            post_processing,
        )
        .await
    }

    /// Enqueue one track. Usenet has no reliable single-track search: the
    /// track resolved to its album upstream, so this is the same
    /// release enqueue under a track-derived job name.
    pub async fn enqueue_track(
        &self,
        task_id: &str,
        track_job: &str,
        nzb_url: Option<&str>,
        category: Option<&str>,
        priority: Option<i32>,
        post_processing: Option<i32>,
    ) -> Result<TaskHandle, SabnzbdError> {
        self.enqueue(
            task_id,
            Some(track_job),
            nzb_url,
            category,
            priority,
            post_processing,
        )
        .await
    }

    async fn enqueue(
        &self,
        task_id: &str,
        job_name: Option<&str>,
        nzb_url: Option<&str>,
        category: Option<&str>,
        priority: Option<i32>,
        post_processing: Option<i32>,
    ) -> Result<TaskHandle, SabnzbdError> {
        let Some(url) = nzb_url else {
            return Err(SabnzbdError::MissingNzbUrl);
        };
        let job_name = job_name.unwrap_or("").to_owned();
        let job_name = if job_name.is_empty() {
            format!("droppedneedle-{task_id}")
        } else {
            job_name
        };
        let nzb_bytes = match self
            .client
            .fetch_nzb(url, self.policy.enqueue_timeout)
            .await
        {
            Ok(bytes) => bytes,
            Err(NzbFetchError::Transport(_)) => {
                // No-response transport failure: this indexer may be
                // reachable only from the SABnzbd host, so hand SABnzbd
                // the enclosure URL. The URL is never logged: enclosures
                // carry the indexer apikey.
                let response = self
                    .client
                    .add_url(
                        &job_name,
                        url,
                        category,
                        priority,
                        post_processing,
                        self.policy.enqueue_timeout,
                    )
                    .await?;
                let Some(nzo_id) = response.nzo_ids.first() else {
                    return Err(SabnzbdError::RejectedNzbUrl);
                };
                return Ok(TaskHandle {
                    source: crate::downloads::source_tag::SourceTag::Usenet.to_string(),
                    job_name,
                    nzo_id: nzo_id.clone(),
                });
            }
            Err(NzbFetchError::Definitive { status, .. }) => {
                return Err(SabnzbdError::NzbRefused { status });
            }
            Err(NzbFetchError::TooLarge) => return Err(SabnzbdError::NzbTooLarge),
            Err(NzbFetchError::ContentRejection { snippet, .. }) => {
                // An error page can echo the URL it was asked for.
                return Err(SabnzbdError::Api(format!(
                    "indexer returned a non-NZB body (likely an error/limit page): {snippet}"
                )));
            }
        };
        let response = self
            .client
            .add_file(
                &job_name,
                &nzb_bytes,
                category,
                priority,
                post_processing,
                self.policy.enqueue_timeout,
            )
            .await?;
        let Some(nzo_id) = response.nzo_ids.first() else {
            return Err(SabnzbdError::RejectedNzb);
        };
        Ok(TaskHandle {
            source: crate::downloads::source_tag::SourceTag::Usenet.to_string(),
            job_name,
            nzo_id: nzo_id.clone(),
        })
    }

    /// Poll queue, then history. A job in neither reads `missing` with no
    /// matched transfers: just added and not listed yet, or gone. Callers
    /// tell the two apart by time (the materialize watchdog) and never
    /// treat a missing job as live.
    pub async fn get_status(&self, handle: &TaskHandle) -> Result<TaskStatus, SabnzbdError> {
        let queue = self.client.queue(self.policy.poll_timeout).await?;
        if let Some(slot) = unique_queue_slot(&queue.slots, handle)? {
            return Ok(queue_status(slot));
        }
        if let Some(slot) = self.find_history_slot(handle).await? {
            return Ok(history_status(&slot));
        }
        Ok(TaskStatus {
            status: "missing".to_owned(),
            ..TaskStatus::default()
        })
    }

    /// Stop an active job and remove only client-owned incomplete data.
    /// Completed output is not client-owned: abort returns false so the
    /// cleanup service validates and removes its exact workspace first.
    pub async fn abort(&self, handle: &TaskHandle) -> Result<bool, SabnzbdError> {
        let nzo_id = self.resolve_nzo_id(handle).await?;
        if nzo_id.is_empty() {
            return Ok(true);
        }
        let queue = self.client.queue(self.policy.poll_timeout).await?;
        if queue.slots.iter().any(|slot| slot.nzo_id == nzo_id) {
            return self
                .client
                .delete_queue(&nzo_id, true, self.policy.poll_timeout)
                .await;
        }
        let slot = self.find_history_slot(handle).await?;
        let Some(slot) = slot else {
            return Ok(true);
        };
        if slot.status.eq_ignore_ascii_case("failed") {
            return self
                .client
                .delete_history(&nzo_id, true, false, self.policy.poll_timeout)
                .await;
        }
        Ok(false)
    }

    /// Fresh evidence about the job's bytes and records, inspected
    /// separately from removal so local cleanup stays durable.
    pub async fn inspect_materialization(
        &self,
        handle: &TaskHandle,
    ) -> Result<Materialization, SabnzbdError> {
        let queue = self.client.queue(self.policy.poll_timeout).await?;
        if let Some(slot) = unique_queue_slot(&queue.slots, handle)? {
            return Ok(Materialization {
                state: "active".to_owned(),
                nzo_id: slot.nzo_id.clone(),
                mount_root: self.mount.display().to_string(),
                mount_healthy: self.downloads_mount_healthy().await,
                ..Materialization::default()
            });
        }
        let slot = self.find_history_slot(handle).await?;
        let mount_healthy = self.downloads_mount_healthy().await;
        let Some(slot) = slot else {
            return Ok(Materialization {
                state: "missing".to_owned(),
                mount_root: self.mount.display().to_string(),
                mount_healthy,
                ..Materialization::default()
            });
        };
        let local = if slot.storage.is_empty() {
            None
        } else {
            self.exact_local_storage(&slot.storage).await
        };
        Ok(Materialization {
            state: if slot.status.eq_ignore_ascii_case("failed") {
                "failed".to_owned()
            } else {
                "completed".to_owned()
            },
            nzo_id: slot.nzo_id.clone(),
            remote_storage: slot.storage.clone(),
            mount_root: self.mount.display().to_string(),
            workspace_path: local
                .map(|path| path.display().to_string())
                .unwrap_or_default(),
            mount_healthy,
        })
    }

    /// Drop history records after local cleanup is durable. Completed
    /// output was removed locally and must use `del_files=false`; only a
    /// failed job's incomplete bytes go with the record.
    pub async fn discard_client_artifacts(
        &self,
        handle: &TaskHandle,
    ) -> Result<bool, SabnzbdError> {
        let slot = self.find_history_slot(handle).await?;
        let Some(slot) = slot else {
            let queue = self.client.queue(self.policy.poll_timeout).await?;
            let still_queued = queue
                .slots
                .iter()
                .any(|item| slot_matches(&item.nzo_id, &item.filename, handle));
            return Ok(!still_queued);
        };
        self.client
            .delete_history(
                &slot.nzo_id,
                slot.status.eq_ignore_ascii_case("failed"),
                false,
                self.policy.poll_timeout,
            )
            .await
    }

    /// Cross-check completions against the mount so a silently-wrong path
    /// is caught proactively. Never raises.
    pub async fn diagnose_downloads_mount(&self) -> MountDiagnosis {
        let client_dir = self.complete_dir().await;
        let history = match self
            .client
            .history(20, None, None, self.policy.poll_timeout)
            .await
        {
            Ok(history) => history,
            Err(_) => {
                return MountDiagnosis {
                    supported: true,
                    completed_downloads: 0,
                    mount_has_files: true,
                    resolvable_downloads: 0,
                    sampled_downloads: 0,
                    client_downloads_dir: non_empty(client_dir),
                };
            }
        };
        let completed: Vec<&HistorySlot> = history
            .slots
            .iter()
            .filter(|slot| {
                slot.status.eq_ignore_ascii_case("completed") && !slot.storage.is_empty()
            })
            .collect();
        if completed.is_empty() {
            return MountDiagnosis {
                supported: true,
                completed_downloads: 0,
                mount_has_files: true,
                resolvable_downloads: 0,
                sampled_downloads: 0,
                client_downloads_dir: non_empty(client_dir),
            };
        }
        let mut resolvable = 0;
        let sample: Vec<&&HistorySlot> = completed.iter().take(DIAGNOSIS_SAMPLE).collect();
        for slot in &sample {
            let Some(local) = self.local_storage(&slot.storage).await else {
                continue;
            };
            let mount = self.mount.clone();
            if tokio::task::spawn_blocking(move || dir_has_file(&mount, &local))
                .await
                .unwrap_or(false)
            {
                resolvable += 1;
            }
        }
        let mount = self.mount.clone();
        let has_files = tokio::task::spawn_blocking(move || mount_has_any_file(&mount))
            .await
            .unwrap_or(false);
        MountDiagnosis {
            supported: true,
            completed_downloads: completed.len(),
            mount_has_files: has_files,
            resolvable_downloads: resolvable,
            sampled_downloads: sample.len(),
            client_downloads_dir: non_empty(client_dir),
        }
    }

    async fn find_history_slot(
        &self,
        handle: &TaskHandle,
    ) -> Result<Option<HistorySlot>, SabnzbdError> {
        // Query by nzo_id alone when we have it: also passing
        // search=job_name risks a logical AND that drops the row when SAB renamed
        // the job (category sorting, `.1` dedup), making a completed job
        // read as never-completed and falsely blocklisting a good release.
        // job_name search is only the pre-enqueue crash-recovery fallback.
        let nzo_ids = non_empty(handle.nzo_id.clone());
        let search = if nzo_ids.is_some() {
            None
        } else {
            non_empty(handle.job_name.clone())
        };
        let history = self
            .client
            .history(
                HISTORY_LIMIT,
                search.as_deref(),
                nzo_ids.as_deref(),
                self.policy.poll_timeout,
            )
            .await?;
        let mut matches = history
            .slots
            .into_iter()
            .filter(|slot| slot_matches(&slot.nzo_id, &slot.name, handle));
        let first = matches.next();
        if matches.next().is_some() {
            return Err(SabnzbdError::AmbiguousIdentity);
        }
        Ok(first)
    }

    async fn resolve_nzo_id(&self, handle: &TaskHandle) -> Result<String, SabnzbdError> {
        if !handle.nzo_id.is_empty() {
            return Ok(handle.nzo_id.clone());
        }
        if let Some(slot) = self.find_history_slot(handle).await? {
            return Ok(slot.nzo_id);
        }
        let queue = self.client.queue(self.policy.poll_timeout).await?;
        Ok(unique_queue_slot(&queue.slots, handle)?
            .map(|slot| slot.nzo_id.clone())
            .unwrap_or_default())
    }

    /// Whether the downloads mount itself is usable. False only when the
    /// mount root is missing or unreadable (a real environment fault). A
    /// healthy mount whose per-job folder is merely empty is a release
    /// problem, not a mount fault.
    async fn downloads_mount_healthy(&self) -> bool {
        let mount = self.mount.clone();
        tokio::task::spawn_blocking(move || {
            if !mount.is_dir() {
                return false;
            }
            // Force a readdir; raises when unreadable.
            std::fs::read_dir(&mount).is_ok()
        })
        .await
        .unwrap_or(false)
    }

    /// Cached `complete_dir`, but only non-empty values: a transient
    /// failure must not poison the cache and force the basename-only
    /// remap forever.
    async fn complete_dir(&self) -> String {
        if let Some(cached) = self.complete_dir_cache.lock().await.clone() {
            return cached;
        }
        let value = self
            .client
            .get_config(self.policy.poll_timeout)
            .await
            .map(|config| config.misc.complete_dir)
            .unwrap_or_default();
        if !value.is_empty() {
            *self.complete_dir_cache.lock().await = Some(value.clone());
        }
        value
    }

    /// Remap SABnzbd's `storage` (its namespace) onto the DroppedNeedle
    /// mount by stripping the `complete_dir` prefix, basename fallback
    /// when the prefix doesn't match. Repairs two deterministic breakages
    /// first (issue #245): backslash paths from a Windows-native SAB, and a
    /// mount pointed at a category subfolder via a bounded suffix walk.
    /// A storage that names no folder below `complete_dir` (the complete
    /// folder itself, or nothing at all) maps to nothing: the whole
    /// downloads folder is never one job's output.
    async fn local_storage(&self, storage: &str) -> Option<PathBuf> {
        let complete_dir = self.complete_dir().await;
        let remote = posix_parts(storage);
        if !complete_dir.is_empty() {
            let complete = posix_parts(&complete_dir);
            if let Some(relative) = strip_prefix(&remote, &complete) {
                if relative.is_empty() {
                    return None;
                }
                let direct: PathBuf = self.mount.join(relative.join("/"));
                if tokio::fs::metadata(&direct)
                    .await
                    .map(|meta| meta.is_dir())
                    .unwrap_or(false)
                {
                    return Some(direct);
                }
                let mount = self.mount.clone();
                let owned: Vec<String> = relative.to_vec();
                let walked = tokio::task::spawn_blocking(move || suffix_walk(&mount, &owned))
                    .await
                    .unwrap_or(None);
                if walked.is_some() {
                    return walked;
                }
            }
            tracing::warn!(
                storage = storage,
                complete_dir = complete_dir.as_str(),
                mount = self.mount.display().to_string().as_str(),
                fallback = remote.last().unwrap_or(&String::new()).as_str(),
                "sabnzbd storage remap failed; falling back to basename"
            );
        }
        remote.last().map(|name| self.mount.join(name))
    }

    /// Cleanup-evidence mapping only when SAB's complete root is known
    /// exactly. The complete folder itself is
    /// never one job's storage, so an empty remainder maps to nothing.
    async fn exact_local_storage(&self, storage: &str) -> Option<PathBuf> {
        let complete_dir = self.complete_dir().await;
        if complete_dir.is_empty() {
            return None;
        }
        let remote = posix_parts(storage);
        let complete = posix_parts(&complete_dir);
        let relative = strip_prefix(&remote, &complete).filter(|rest| !rest.is_empty())?;
        Some(self.mount.join(relative.join("/")))
    }
}

fn non_empty(value: String) -> Option<String> {
    if value.is_empty() { None } else { Some(value) }
}

fn slot_matches(slot_id: &str, slot_name: &str, handle: &TaskHandle) -> bool {
    if !handle.nzo_id.is_empty() {
        return slot_id == handle.nzo_id;
    }
    !handle.job_name.is_empty() && slot_name == handle.job_name
}

fn unique_queue_slot<'a>(
    slots: &'a [QueueSlot],
    handle: &TaskHandle,
) -> Result<Option<&'a QueueSlot>, SabnzbdError> {
    let mut matches = slots.iter().filter(|slot| {
        !slot.status.eq_ignore_ascii_case("deleted")
            && slot_matches(&slot.nzo_id, &slot.filename, handle)
    });
    let first = matches.next();
    if matches.next().is_some() {
        return Err(SabnzbdError::AmbiguousIdentity);
    }
    Ok(first)
}

/// Queue mapping: only true `Downloading` is active;
/// post-download phases hold the bar at 100% with `processing` (SABnzbd
/// zeroes the queue percentage during unpack, so surfacing it would show
/// a 0% regression).
fn queue_status(slot: &QueueSlot) -> TaskStatus {
    let state = slot.status.to_ascii_lowercase();
    let mb = to_float(&slot.mb);
    let mbleft = to_float(&slot.mbleft);
    let bytes_total = (mb * 1024.0 * 1024.0) as u64;
    let mut bytes_downloaded = ((mb - mbleft).max(0.0) * 1024.0 * 1024.0) as u64;
    let active = state == "downloading";
    let mut percent = to_int(&slot.percentage) as f64;
    let status = if QUEUE_NOT_ACTIVE.contains(&state.as_str()) {
        "queued"
    } else if active {
        "downloading"
    } else {
        percent = 100.0;
        bytes_downloaded = bytes_total;
        "processing"
    };
    TaskStatus {
        status: status.to_owned(),
        files_total: 1,
        bytes_total,
        bytes_downloaded,
        progress_percent: percent,
        has_active_transfer: active,
        matched_transfers: 1,
        ..TaskStatus::default()
    }
}

/// History mapping: `Deleted` is a terminal failure
/// so the orchestrator fails over instead of polling to the deadline; the
/// fail message surfaces verbatim (disk-full classification lives in the
/// orchestrator, not here).
fn history_status(slot: &HistorySlot) -> TaskStatus {
    let state = slot.status.to_ascii_lowercase();
    if state == "deleted" {
        return TaskStatus {
            status: "failed".to_owned(),
            error: Some("job removed from SABnzbd".to_owned()),
            matched_transfers: 1,
            ..TaskStatus::default()
        };
    }
    if state == "completed" {
        return TaskStatus {
            status: "completed".to_owned(),
            files_total: 1,
            files_completed: 1,
            bytes_total: slot.bytes,
            bytes_downloaded: slot.bytes,
            progress_percent: 100.0,
            matched_transfers: 1,
            ..TaskStatus::default()
        };
    }
    if state == "failed" {
        return TaskStatus {
            status: "failed".to_owned(),
            // Not verified: whether SABnzbd quotes the URL of a failed
            // addurl here. No recorded history has such a job (the mock
            // is shaped on 5.0.4) and checking needs a live failed add,
            // so the text is scrubbed either way.
            error: Some(if slot.fail_message.is_empty() {
                "download failed".to_owned()
            } else {
                redact_query_secrets(&slot.fail_message)
            }),
            bytes_total: slot.bytes,
            matched_transfers: 1,
            ..TaskStatus::default()
        };
    }
    TaskStatus {
        status: "processing".to_owned(),
        files_total: 1,
        bytes_total: slot.bytes,
        bytes_downloaded: slot.bytes,
        progress_percent: 100.0,
        matched_transfers: 1,
        ..TaskStatus::default()
    }
}

fn to_float(value: &str) -> f64 {
    value.trim().parse::<f64>().unwrap_or(0.0)
}

fn to_int(value: &str) -> i64 {
    value.trim().parse::<f64>().unwrap_or(0.0) as i64
}

/// Fold separators to posix before any remap: a
/// Windows-native SAB reports backslash paths.
/// `.` and `..` segments are dropped, so a storage path joined onto the
/// mount can never climb out of it.
fn posix_parts(value: &str) -> Vec<String> {
    value
        .replace('\\', "/")
        .split('/')
        .filter(|part| !part.is_empty() && *part != "." && *part != "..")
        .map(str::to_owned)
        .collect()
}

fn strip_prefix<'a>(remote: &'a [String], complete: &[String]) -> Option<&'a [String]> {
    if remote.len() < complete.len() {
        return None;
    }
    if remote[..complete.len()] == complete[..] {
        Some(&remote[complete.len()..])
    } else {
        None
    }
}

/// Bounded walk for a directory under the mount whose path ends with the
/// storage-relative suffix (the category-subfolder mount: the mount IS
/// `complete_dir/<cat>`, so the stripped remainder still carries the
/// category component). Longest suffix wins; failure-path only; sync I/O
/// the caller offloads.
fn suffix_walk(mount: &Path, relative: &[String]) -> Option<PathBuf> {
    let mount = mount.canonicalize().unwrap_or_else(|_| mount.to_path_buf());
    let suffixes: Vec<String> = (0..relative.len())
        .map(|index| relative[index..].join("/"))
        .collect();
    let mut best: Option<PathBuf> = None;
    let mut best_len = 0;
    let mut seen_dirs: std::collections::HashSet<PathBuf> = std::collections::HashSet::new();
    let mut stack = vec![mount.clone()];
    let mut seen = 0usize;
    while let Some(current) = stack.pop() {
        let current = current.canonicalize().unwrap_or(current);
        if !current.starts_with(&mount) || !seen_dirs.insert(current.clone()) {
            continue;
        }
        let relative_posix = current
            .strip_prefix(&mount)
            .unwrap_or_else(|_| Path::new(""))
            .to_string_lossy()
            .replace('\\', "/");
        for suffix in &suffixes {
            if relative_posix == *suffix || relative_posix.ends_with(&format!("/{suffix}")) {
                if suffix.len() >= best_len {
                    best_len = suffix.len();
                    best = Some(current.clone());
                }
                break;
            }
        }
        let entries = std::fs::read_dir(&current).map(|entries| entries.collect::<Vec<_>>());
        let Ok(entries) = entries else { continue };
        for entry in entries {
            seen += 1;
            if seen > REMAP_WALK_BUDGET {
                return best;
            }
            let Ok(entry) = entry else { continue };
            if entry.file_type().map(|kind| kind.is_dir()).unwrap_or(false) {
                stack.push(entry.path());
            }
        }
    }
    best
}

/// Whether the job folder holds a file directly. The folder is placed
/// against the mount first, so a job folder that is (or sits under) a link
/// is never read, and a link inside it never counts as a file. Blocking.
fn dir_has_file(mount: &Path, folder: &Path) -> bool {
    let Place::Inside(real) = place(folder, &[DownloadsFolder::new(mount.to_path_buf())]) else {
        return false;
    };
    std::fs::read_dir(real)
        .map(|mut entries| {
            entries.any(|entry| {
                entry
                    .and_then(|entry| entry.file_type())
                    .is_ok_and(|kind| kind.is_file())
            })
        })
        .unwrap_or(false)
}

fn mount_has_any_file(mount: &Path) -> bool {
    let mut stack = vec![mount.to_path_buf()];
    let mut seen = 0usize;
    while let Some(current) = stack.pop() {
        let entries = match std::fs::read_dir(&current) {
            Ok(entries) => entries,
            Err(_) => return false,
        };
        for entry in entries {
            seen += 1;
            if seen > MOUNT_PROBE_BUDGET {
                return true;
            }
            let Ok(entry) = entry else { continue };
            // The entry's own type: a link is neither followed nor counted.
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            if kind.is_file() {
                return true;
            }
            if kind.is_dir() {
                stack.push(entry.path());
            }
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_storage_path_cannot_climb_out_of_the_mount() {
        assert_eq!(
            posix_parts("/complete/../../etc/./job\\..\\x"),
            ["complete", "etc", "job", "x"]
        );
    }

    #[test]
    fn a_body_is_scrubbed_before_it_is_cut() {
        let key = "0123456789abcdef0123456789abcdef";
        let body = format!("{}https://x/fetch/{key}", "-".repeat(180));
        let snippet = snippet_of(body.as_bytes());
        assert!(!snippet.contains("0123"), "{snippet}");
    }
}
