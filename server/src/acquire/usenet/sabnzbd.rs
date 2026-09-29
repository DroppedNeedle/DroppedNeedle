//! SABnzbd download client: enqueue NZBs, track jobs, locate output.
//!
//! [`SabnzbdClient`] is the raw API wrapper (no business logic), ported
//! from v2 `sabnzbd_client.py` and verified against the owner's SABnzbd
//! 5.0.4: every call appends `output=json` + `apikey` as suffix query
//! params (never headers; Lidarr `SabnzbdProxy`), adds go through
//! `mode=addfile` (multipart POST of fetched + validated NZB bytes), and
//! `mode=addurl` is the enqueue fallback only. [`SabnzbdQueue`] is the
//! download-client layer (v2 `SabnzbdDownloadClient`): pre-enqueue
//! `job_name` correlation, queue→history status walk, storage remap onto
//! the DroppedNeedle downloads mount, and folder-based import source.

use std::path::{Path, PathBuf};
use std::time::Duration;

use reqwest::Client;
use serde::Deserialize;
use thiserror::Error;
use tokio::sync::Mutex;

use super::policy::UsenetPolicy;

// ---------------------------------------------------------------------------
// Models (v2 `sabnzbd_models.py`: stringly-typed queue numbers, real-number
// history bytes, opaque `nzo_id`).
// ---------------------------------------------------------------------------

/// `mode=addfile` / `addurl` response. Empty `nzo_ids` means rejection.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct AddResponse {
    /// Whether SABnzbd accepted the add.
    #[serde(default)]
    pub status: bool,
    /// Job ids created, usually exactly one.
    #[serde(default)]
    pub nzo_ids: Vec<String>,
}

/// Accept SABnzbd's stringly numbers (`"100.0"`) and real numbers alike.
fn de_stringly<'de, D>(value: D) -> Result<String, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::de::{self, Visitor};

    struct Stringly;

    impl Visitor<'_> for Stringly {
        type Value = String;

        fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("a string or number")
        }

        fn visit_str<E: de::Error>(self, value: &str) -> Result<String, E> {
            Ok(value.to_owned())
        }

        fn visit_string<E: de::Error>(self, value: String) -> Result<String, E> {
            Ok(value)
        }

        fn visit_i64<E: de::Error>(self, value: i64) -> Result<String, E> {
            Ok(value.to_string())
        }

        fn visit_u64<E: de::Error>(self, value: u64) -> Result<String, E> {
            Ok(value.to_string())
        }

        fn visit_f64<E: de::Error>(self, value: f64) -> Result<String, E> {
            Ok(value.to_string())
        }

        fn visit_bool<E: de::Error>(self, value: bool) -> Result<String, E> {
            Ok(value.to_string())
        }
    }

    value.deserialize_any(Stringly)
}

/// One in-progress job. `filename` is the job name; `mb`/`mbleft` are
/// decimal megabytes as strings; `priority` is a name on read.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct QueueSlot {
    /// Opaque job id (a plain UUID on 5.0.4).
    #[serde(default)]
    pub nzo_id: String,
    /// `Downloading`, `Queued`, `Paused`, `Fetching`, and post states.
    #[serde(default)]
    pub status: String,
    /// Job name (`droppedneedle-{task_id}`).
    #[serde(default)]
    pub filename: String,
    /// Category name.
    #[serde(default)]
    pub cat: String,
    /// Total megabytes, stringly-typed.
    #[serde(default, deserialize_with = "de_stringly")]
    pub mb: String,
    /// Remaining megabytes, stringly-typed.
    #[serde(default, deserialize_with = "de_stringly")]
    pub mbleft: String,
    /// Percent complete, int-as-string.
    #[serde(default, deserialize_with = "de_stringly")]
    pub percentage: String,
    /// Human time-left estimate.
    #[serde(default)]
    pub timeleft: String,
    /// Priority name (`Normal`/`High`/…).
    #[serde(default)]
    pub priority: String,
}

/// `mode=queue` payload.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct Queue {
    /// Overall state.
    #[serde(default)]
    pub status: String,
    /// Whether the queue is paused.
    #[serde(default)]
    pub paused: bool,
    /// In-progress jobs.
    #[serde(default)]
    pub slots: Vec<QueueSlot>,
}

/// One finished / post-processing / failed job. `storage` is the final
/// folder in SABnzbd's namespace; `bytes` is already bytes. Debug is
/// hand-written: the NZB password must never appear in debug output.
#[derive(Clone, Default, Deserialize)]
pub struct HistorySlot {
    /// Opaque job id.
    #[serde(default)]
    pub nzo_id: String,
    /// Job name.
    #[serde(default)]
    pub name: String,
    /// Source NZB filename.
    #[serde(default)]
    pub nzb_name: String,
    /// `Completed`, `Failed`, `Verifying`, `Extracting`, ….
    #[serde(default)]
    pub status: String,
    /// Category name.
    #[serde(default)]
    pub category: String,
    /// Final folder (SABnzbd namespace).
    #[serde(default)]
    pub storage: String,
    /// Total bytes (a real number).
    #[serde(default)]
    pub bytes: u64,
    /// Failure detail on `Failed`.
    #[serde(default)]
    pub fail_message: String,
    /// Password used, when any.
    #[serde(default)]
    pub password: Option<String>,
    /// Download duration in seconds.
    #[serde(default)]
    pub download_time: u64,
    /// Completion timestamp.
    #[serde(default)]
    pub completed: u64,
}

impl std::fmt::Debug for HistorySlot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HistorySlot")
            .field("nzo_id", &self.nzo_id)
            .field("name", &self.name)
            .field("nzb_name", &self.nzb_name)
            .field("status", &self.status)
            .field("category", &self.category)
            .field("storage", &self.storage)
            .field("bytes", &self.bytes)
            .field("fail_message", &self.fail_message)
            .field("password", &self.password.as_ref().map(|_| "<redacted>"))
            .field("download_time", &self.download_time)
            .field("completed", &self.completed)
            .finish()
    }
}

/// `mode=history` payload.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct History {
    /// Finished jobs.
    #[serde(default)]
    pub slots: Vec<HistorySlot>,
}

/// One SABnzbd category row.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct Category {
    /// Category name.
    #[serde(default)]
    pub name: String,
    /// Category folder.
    #[serde(default)]
    pub dir: String,
    /// Post-processing setting.
    #[serde(default)]
    pub pp: String,
}

/// `config.misc`: the mount-remap prefix lives here.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct Misc {
    /// Completed-downloads dir (the remap prefix).
    #[serde(default)]
    pub complete_dir: String,
}

/// `mode=get_config` payload.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct Config {
    /// Misc settings incl. `complete_dir`.
    #[serde(default)]
    pub misc: Misc,
    /// Category rows.
    #[serde(default)]
    pub categories: Vec<Category>,
}

// ---------------------------------------------------------------------------
// Errors + credential scrubbing.
// ---------------------------------------------------------------------------

/// Transport/HTTP/SABnzbd failure. Auth failures carry `auth: true`.
#[derive(Debug, Clone, Error)]
pub enum SabnzbdError {
    /// The request never got an answer (DNS/refused/timeout/reset).
    #[error("SABnzbd request failed: {0}")]
    Transport(String),
    /// SABnzbd answered HTTP 4xx/5xx.
    #[error("SABnzbd returned HTTP {status}")]
    Http {
        /// Status code.
        status: u16,
        /// Body snippet (already redacted for addurl).
        snippet: String,
    },
    /// SABnzbd answered an API-level error (both wire forms).
    #[error("{message}")]
    Api {
        /// Error text from SABnzbd.
        message: String,
        /// True for "API key incorrect/required".
        auth: bool,
    },
    /// SABnzbd returned a body that is neither JSON nor its text form.
    #[error("SABnzbd returned non-JSON: {0}")]
    NonJson(String),
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
        matches!(self, SabnzbdError::Api { auth: true, .. })
    }
}

/// NZB fetch failure, classified so the enqueue path can decide: only a
/// no-response transport failure falls back to addurl (v2
/// `transport_failure` marker); a definitive indexer HTTP error or a
/// content rejection (v2 `content_rejection` marker) never does.
#[derive(Debug, Clone, Error)]
pub enum NzbFetchError {
    /// No response at all (DNS/refused/timeout): addurl may help.
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
}

/// Scrub credential-looking query values from addurl errors: the Newznab
/// enclosure URL carries the indexer's key and SABnzbd may echo it back.
/// Covers `apikey=`/`api_key=` and DrunkenSlug-style `r=` (anchored so
/// innocent words like `error=` don't match); `i=` stays visible.
/// Ported from v2 `_QUERY_SECRET_RE` (no regex crate in this slice).
pub fn redact_query_secrets(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let mut index = 0;
    while index < bytes.len() {
        if let Some(key_len) = match_secret_key(bytes, index) {
            out.push_str(&text[index..index + key_len]);
            index += key_len;
            while index < bytes.len()
                && !matches!(
                    bytes[index],
                    b'&' | b'\'' | b'"' | b' ' | b'\t' | b'\n' | b'\r'
                )
            {
                index += 1;
            }
            out.push_str("***");
        } else {
            let ch = text[index..].chars().next().unwrap_or('\u{FFFD}');
            out.push(ch);
            index += ch.len_utf8();
        }
    }
    out
}

/// Length of `apikey=`/`api_key=`/`r=` at `index`, case-insensitive; `r`
/// must not follow an identifier char (the v2 anchor).
fn match_secret_key(bytes: &[u8], index: usize) -> Option<usize> {
    let rest = &bytes[index..];
    if rest.len() >= 7 && rest[..7].eq_ignore_ascii_case(b"api_key") && rest.get(7) == Some(&b'=') {
        return Some(8);
    }
    if rest.len() >= 6 && rest[..6].eq_ignore_ascii_case(b"apikey") && rest.get(6) == Some(&b'=') {
        return Some(7);
    }
    if rest.len() >= 2
        && (rest[0] == b'r' || rest[0] == b'R')
        && rest[1] == b'='
        && (index == 0
            || !matches!(bytes[index - 1], b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'_'))
    {
        return Some(2);
    }
    None
}

// ---------------------------------------------------------------------------
// Raw client.
// ---------------------------------------------------------------------------

/// Raw SABnzbd API wrapper. The HTTP client is injected (v2 AUD-12); the
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
        let data: serde_json::Value = self.get(&[("mode", "version")], timeout).await?;
        Ok(data
            .get("version")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("")
            .to_owned())
    }

    /// Category names for the settings picker (`mode=get_cats`).
    pub async fn get_cats(&self, timeout: Duration) -> Result<Vec<String>, SabnzbdError> {
        let data: serde_json::Value = self.get(&[("mode", "get_cats")], timeout).await?;
        let cats = data
            .get("categories")
            .and_then(serde_json::Value::as_array)
            .cloned()
            .unwrap_or_default();
        Ok(cats
            .iter()
            .filter_map(|value| {
                value
                    .as_str()
                    .map(str::to_owned)
                    .or_else(|| Some(value.to_string()))
            })
            .collect())
    }

    /// Full config: `complete_dir` + categories (`mode=get_config`).
    pub async fn get_config(&self, timeout: Duration) -> Result<Config, SabnzbdError> {
        let data: serde_json::Value = self.get(&[("mode", "get_config")], timeout).await?;
        let config = data
            .get("config")
            .cloned()
            .unwrap_or(serde_json::Value::Null);
        serde_json::from_value(config).map_err(|err| SabnzbdError::NonJson(err.to_string()))
    }

    /// Current queue (`mode=queue`).
    pub async fn queue(&self, timeout: Duration) -> Result<Queue, SabnzbdError> {
        let data: serde_json::Value = self.get(&[("mode", "queue")], timeout).await?;
        let queue = data
            .get("queue")
            .cloned()
            .unwrap_or(serde_json::Value::Null);
        serde_json::from_value(queue).map_err(|err| SabnzbdError::NonJson(err.to_string()))
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
        let data: serde_json::Value = self.get(&params, timeout).await?;
        let history = data
            .get("history")
            .cloned()
            .unwrap_or(serde_json::Value::Null);
        serde_json::from_value(history).map_err(|err| SabnzbdError::NonJson(err.to_string()))
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
            .map_err(|err| {
                // Transport text can echo the request URL, apikey included.
                SabnzbdError::Transport(redact_query_secrets(&err.to_string()))
            })?;
        self.parse_add(response).await
    }

    /// `mode=addurl` GET: hand SABnzbd the Newznab enclosure URL and let it
    /// fetch the NZB itself. Enqueue FALLBACK only, for indexers reachable
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
            .map_err(|err| redacted_addurl_error(&SabnzbdError::Transport(err.to_string())))?;
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
        let data: serde_json::Value = self
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
        Ok(is_ok(&data))
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
        let data: serde_json::Value = self
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
        Ok(is_ok(&data))
    }

    /// GET the release's NZB URL (the Newznab enclosure, apikey already
    /// embedded) and validate the bytes are a real NZB, not an indexer
    /// error page. Redirects are followed (reqwest's default policy).
    pub async fn fetch_nzb(&self, url: &str, timeout: Duration) -> Result<Vec<u8>, NzbFetchError> {
        let response = self
            .get_with_retry(url, &[], timeout)
            .await
            .map_err(|err| NzbFetchError::Transport(err.to_string()))?;
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
        let bytes = response
            .bytes()
            .await
            .map_err(|err| NzbFetchError::Transport(err.to_string()))?;
        let head: Vec<u8> = bytes.iter().take(512).copied().collect();
        if !looks_like_nzb(&head) {
            let snippet = String::from_utf8_lossy(&bytes).chars().take(200).collect();
            return Err(NzbFetchError::ContentRejection {
                status,
                content_type,
                snippet,
            });
        }
        Ok(bytes.to_vec())
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
            .map_err(|err| SabnzbdError::Transport(err.to_string()))?;
        let data = self.parse_json(response).await?;
        serde_json::from_value(data).map_err(|err| SabnzbdError::NonJson(err.to_string()))
    }

    /// IDEMPOTENT GETs only: transient transport errors + 5xx retry with
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
            return Err(SabnzbdError::Http {
                status,
                snippet: response_snippet(response).await,
            });
        }
        let text = response
            .text()
            .await
            .map_err(|err| SabnzbdError::Transport(err.to_string()))?;
        parse_body(&text)
    }

    async fn parse_add(&self, response: reqwest::Response) -> Result<AddResponse, SabnzbdError> {
        let data = self.parse_json(response).await?;
        serde_json::from_value(data).map_err(|err| SabnzbdError::NonJson(err.to_string()))
    }
}

/// Rebuild an addurl failure with echoed enclosure credentials scrubbed
/// (v2 `_redacted_addurl_error`).
fn redacted_addurl_error(err: &SabnzbdError) -> SabnzbdError {
    match err {
        // Transport text can echo the request URL, enclosure key included.
        SabnzbdError::Transport(detail) => SabnzbdError::Transport(redact_query_secrets(detail)),
        SabnzbdError::Http { status, snippet } => SabnzbdError::Http {
            status: *status,
            snippet: redact_query_secrets(snippet),
        },
        SabnzbdError::Api { message, auth } => SabnzbdError::Api {
            message: redact_query_secrets(message),
            auth: *auth,
        },
        SabnzbdError::NonJson(text) => SabnzbdError::NonJson(redact_query_secrets(text)),
        other => other.clone(),
    }
}

/// Both SABnzbd error forms: plain-text `error: …` and JSON
/// `{"status": false, "error": …}` (v2 `_parse`).
fn parse_body(text: &str) -> Result<serde_json::Value, SabnzbdError> {
    let trimmed = text.trim();
    if trimmed.to_ascii_lowercase().starts_with("error") {
        let message = trimmed
            .split_once(':')
            .map(|(_, rest)| rest.trim())
            .unwrap_or(trimmed)
            .to_owned();
        let auth = is_auth_error(&message);
        return Err(SabnzbdError::Api { message, auth });
    }
    let data: serde_json::Value = serde_json::from_str(text)
        .map_err(|_| SabnzbdError::NonJson(trimmed.chars().take(120).collect()))?;
    if let Some(error_text) = data.get("error").and_then(serde_json::Value::as_str)
        && is_false(data.get("status"))
    {
        let message = error_text.to_owned();
        let auth = is_auth_error(&message);
        return Err(SabnzbdError::Api { message, auth });
    }
    Ok(data)
}

/// SABnzbd serialises the status boolean as `false` or `"False"`,
/// matched case-insensitively (v2 `_is_false`).
fn is_false(value: Option<&serde_json::Value>) -> bool {
    match value {
        Some(serde_json::Value::Bool(flag)) => !flag,
        Some(serde_json::Value::String(text)) => text.trim().eq_ignore_ascii_case("false"),
        _ => false,
    }
}

fn is_ok(data: &serde_json::Value) -> bool {
    !is_false(data.get("status"))
}

fn is_auth_error(message: &str) -> bool {
    let low = message.to_ascii_lowercase();
    low.contains("api key incorrect") || low.contains("api key required")
}

async fn response_snippet(response: reqwest::Response) -> String {
    response
        .text()
        .await
        .unwrap_or_default()
        .chars()
        .take(200)
        .collect()
}

/// The NZB head check (v2 `fetch_nzb`): `<?xml` or `<nzb` up front, or
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
/// `{job_name}.nzb`, type `application/x-nzb` (v2 `add_file`).
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
// Queue layer (v2 `SabnzbdDownloadClient`).
// ---------------------------------------------------------------------------

/// Queue states that move 0 bytes (never active transfers). Anything else
/// in the queue that isn't true `Downloading` is post-processing.
const QUEUE_NOT_ACTIVE: [&str; 4] = ["queued", "grabbing", "propagating", "paused"];
/// History window for job lookups.
const HISTORY_LIMIT: u32 = 50;
/// Failure-path-only bound for the suffix walk (v2 `_REMAP_WALK_BUDGET`).
const REMAP_WALK_BUDGET: usize = 10_000;
/// Bound for the finished-folder audio enumeration.
const ENUMERATE_BUDGET: usize = 10_000;
/// Bound for the mount-has-files probe.
const MOUNT_PROBE_BUDGET: usize = 5_000;
/// History sample for mount diagnosis.
const DIAGNOSIS_SAMPLE: usize = 3;

/// Mirror of the importer's accepted audio set (v2 `_AUDIO_SUFFIXES`).
const AUDIO_SUFFIXES: [&str; 9] = [
    ".flac", ".mp3", ".m4a", ".m4b", ".mp4", ".ogg", ".oga", ".opus", ".wav",
];

/// Client-agnostic task correlation (v2 `TaskHandle`, Usenet half).
#[derive(Debug, Clone, Default)]
pub struct TaskHandle {
    /// Always `usenet` here.
    pub source: String,
    /// `droppedneedle-{task_id}`: the pre-enqueue key.
    pub job_name: String,
    /// SABnzbd's unique key, filled after the add returns.
    pub nzo_id: String,
}

/// One status poll, shaped like v2 `DownloadTaskStatus`.
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

/// Fresh evidence about bytes and client records (v2 `DownloadMaterialization`).
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

/// Mount cross-check (v2 `MountDiagnosis`).
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

/// Health answer (v2 `ServiceStatus` shape).
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
    #[allow(dead_code)]
    url: String,
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
            url: url.to_owned(),
            configured: !url.is_empty() && !api_key.is_empty(),
            mount: downloads_mount,
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
    /// along like v2's `EnqueueRequest`.
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
    /// passes `droppedneedle-{task_id}-{candidate_index}` (v2 strategy):
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

    /// Enqueue one track. Usenet has no reliable single-track search (v2
    /// D4): the track resolved to its album upstream, so this is the same
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
                // carry the indexer apikey (v2 `enqueue`).
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
                    source: "usenet".to_owned(),
                    job_name,
                    nzo_id: nzo_id.clone(),
                });
            }
            Err(NzbFetchError::Definitive { status, .. }) => {
                return Err(SabnzbdError::Http {
                    status,
                    snippet: "indexer answered; addurl cannot help".to_owned(),
                });
            }
            Err(NzbFetchError::ContentRejection { snippet, .. }) => {
                return Err(SabnzbdError::Api {
                    message: format!(
                        "indexer returned a non-NZB body (likely an error/limit page): {snippet}"
                    ),
                    auth: false,
                });
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
            source: "usenet".to_owned(),
            job_name,
            nzo_id: nzo_id.clone(),
        })
    }

    /// Poll queue, then history; missing in both means just-added (or
    /// gone), reported as `queued` with no matched transfers.
    pub async fn get_status(&self, handle: &TaskHandle) -> Result<TaskStatus, SabnzbdError> {
        let queue = self.client.queue(self.policy.poll_timeout).await?;
        if let Some(slot) = unique_queue_slot(&queue.slots, handle)? {
            return Ok(queue_status(slot));
        }
        if let Some(slot) = self.find_history_slot(handle).await? {
            return Ok(history_status(&slot));
        }
        Ok(TaskStatus {
            status: "queued".to_owned(),
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
    /// output was removed locally and MUST use `del_files=false`; only a
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

    /// Audio files of the finished job, on disk (the folder-based import
    /// source, v2 D18).
    pub async fn list_completed_files(
        &self,
        handle: &TaskHandle,
    ) -> Result<Vec<PathBuf>, SabnzbdError> {
        let slot = self.find_history_slot(handle).await?;
        let Some(slot) = slot else {
            return Ok(Vec::new());
        };
        if slot.storage.is_empty() {
            return Ok(Vec::new());
        }
        let local = self.local_storage(&slot.storage).await;
        let mount = self.mount.clone();
        Ok(
            tokio::task::spawn_blocking(move || enumerate_audio(&mount, &local))
                .await
                .unwrap_or_default(),
        )
    }

    /// Local path of one completed file: exact basename match first, then
    /// a size match (the client may have sanitised the name).
    pub async fn get_file_path(
        &self,
        handle: &TaskHandle,
        remote_filename: &str,
        size: Option<u64>,
    ) -> Result<Option<PathBuf>, SabnzbdError> {
        let files = self.list_completed_files(handle).await?;
        let basename = remote_filename
            .rsplit(['/', '\\'])
            .next()
            .unwrap_or(remote_filename);
        for path in &files {
            if path.file_name().and_then(|name| name.to_str()) == Some(basename) {
                return Ok(Some(path.clone()));
            }
        }
        if let Some(expected) = size {
            let files = files.clone();
            let matched = tokio::task::spawn_blocking(move || {
                files.into_iter().find(|path| {
                    std::fs::metadata(path)
                        .map(|meta| meta.len())
                        .unwrap_or(u64::MAX)
                        == expected
                })
            })
            .await
            .unwrap_or(None);
            if matched.is_some() {
                return Ok(matched);
            }
        }
        Ok(None)
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
            let local = self.local_storage(&slot.storage).await;
            if tokio::task::spawn_blocking(move || dir_has_file(&local))
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
        // Query by nzo_id ALONE when we have it: also passing
        // search=job_name risks an AND that drops the row when SAB renamed
        // the job (category sorting, `.1` dedup), making a completed job
        // read as never-completed and falsely blocklisting a good release
        // (v2 `_find_history_slot`). job_name search is only the
        // pre-enqueue crash-recovery fallback.
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

    /// Whether the downloads MOUNT itself is usable. False ONLY when the
    /// mount root is missing or unreadable (a real environment fault). A
    /// healthy mount whose per-job folder is merely empty is a RELEASE
    /// problem, not a mount fault (v2 `downloads_mount_healthy`).
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
    /// remap forever (v2 `_complete_dir`).
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
    /// first (v2 #245): backslash paths from a Windows-native SAB, and a
    /// mount pointed at a category subfolder via a bounded suffix walk.
    async fn local_storage(&self, storage: &str) -> PathBuf {
        let complete_dir = self.complete_dir().await;
        let remote = posix_parts(storage);
        if !complete_dir.is_empty() {
            let complete = posix_parts(&complete_dir);
            if let Some(relative) = strip_prefix(&remote, &complete) {
                if relative.is_empty() {
                    return self.mount.clone();
                }
                let direct: PathBuf = self.mount.join(relative.join("/"));
                if tokio::fs::metadata(&direct)
                    .await
                    .map(|meta| meta.is_dir())
                    .unwrap_or(false)
                {
                    return direct;
                }
                let mount = self.mount.clone();
                let owned: Vec<String> = relative.to_vec();
                let walked = tokio::task::spawn_blocking(move || suffix_walk(&mount, &owned))
                    .await
                    .unwrap_or(None);
                if let Some(found) = walked {
                    return found;
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
        match remote.last() {
            Some(name) => self.mount.join(name),
            None => self.mount.clone(),
        }
    }

    /// Cleanup-evidence mapping only when SAB's complete root is known
    /// exactly (v2 `_exact_local_storage`).
    async fn exact_local_storage(&self, storage: &str) -> Option<PathBuf> {
        let complete_dir = self.complete_dir().await;
        if complete_dir.is_empty() {
            return None;
        }
        let remote = posix_parts(storage);
        let complete = posix_parts(&complete_dir);
        let relative = strip_prefix(&remote, &complete)?;
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

/// Queue mapping (v2 `_queue_status`): only true `Downloading` is active;
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

/// History mapping (v2 `_history_status`): `Deleted` is a terminal failure
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
            error: Some(if slot.fail_message.is_empty() {
                "download failed".to_owned()
            } else {
                slot.fail_message.clone()
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

/// Fold separators to posix before any remap (v2 `_posix_norm`): a
/// Windows-native SAB reports backslash paths.
fn posix_parts(value: &str) -> Vec<String> {
    value
        .replace('\\', "/")
        .split('/')
        .filter(|part| !part.is_empty())
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
/// the caller offloads (v2 `_suffix_walk`).
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

/// Audio files under the finished job folder (bounded DFS), confined to
/// the mount (v2 `_enumerate_audio`). Sync I/O the caller offloads.
fn enumerate_audio(mount: &Path, folder: &Path) -> Vec<PathBuf> {
    let mount = mount.canonicalize().unwrap_or_else(|_| mount.to_path_buf());
    let root = match folder.canonicalize() {
        Ok(root) => root,
        Err(_) => return Vec::new(),
    };
    if !root.starts_with(&mount) || !root.is_dir() {
        return Vec::new();
    }
    let mut out = Vec::new();
    let mut stack = vec![root];
    let mut seen = 0usize;
    while let Some(current) = stack.pop() {
        let entries = match std::fs::read_dir(&current) {
            Ok(entries) => entries,
            Err(_) => continue,
        };
        for entry in entries {
            seen += 1;
            if seen > ENUMERATE_BUDGET {
                return out;
            }
            let Ok(entry) = entry else { continue };
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.is_file()
                && path
                    .extension()
                    .and_then(|ext| ext.to_str())
                    .is_some_and(|ext| {
                        AUDIO_SUFFIXES.contains(&format!(".{}", ext.to_ascii_lowercase()).as_str())
                    })
            {
                out.push(path);
            }
        }
    }
    out
}

fn dir_has_file(folder: &Path) -> bool {
    if !folder.is_dir() {
        return false;
    }
    std::fs::read_dir(folder)
        .map(|mut entries| {
            entries.any(|entry| entry.map(|entry| entry.path().is_file()).unwrap_or(false))
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
            let path = entry.path();
            if path.is_file() {
                return true;
            }
            if path.is_dir() {
                stack.push(path);
            }
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn addurl_transport_errors_scrub_echoed_keys() {
        let err = redacted_addurl_error(&SabnzbdError::Transport(
            "error sending request for url (https://sab/addurl?name=https://idx/getnzb?apikey=SUPERSECRET)".to_owned(),
        ));
        assert!(!err.to_string().contains("SUPERSECRET"), "{err}");
    }
}
