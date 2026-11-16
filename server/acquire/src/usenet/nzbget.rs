//! NZBGet download client: enqueue NZBs, track jobs, locate output.
//!
//! [`NzbgetClient`] wraps NZBGet's JSON-RPC API: every call is a POST to
//! `{base}/jsonrpc` signed in with HTTP basic auth, so the credentials never
//! ride in the URL. Adds go through `append` with the fetched and checked NZB
//! bytes; `append` with the link itself is the fallback only. [`NzbgetQueue`]
//! is the download-client layer, the same shape as the SABnzbd one: the
//! `job_name` set before the add (NZBGet's job name and dupe key), a
//! queue then history status walk, and the storage remap onto the
//! DroppedNeedle downloads mount that both clients share.

use std::path::{Path, PathBuf};
use std::time::Duration;

use base64::Engine as _;
use reqwest::Client;
use reqwest::header::CONTENT_TYPE;
use thiserror::Error;
use tokio::sync::Mutex;

use super::nzbget_models::{ConfigEntry, Group, HistoryItem, RpcReply};
use super::policy::UsenetPolicy;
use super::sabnzbd::{
    DIAGNOSIS_SAMPLE, Health, MAX_NZB_BYTES, Materialization, MountDiagnosis, NzbFetchError,
    NzbGrab, TaskHandle, TaskStatus, dir_has_file, exact_local_storage, fetch_nzb, grab_nzb,
    local_storage, mount_has_any_file, mount_healthy, non_empty, redact_query_secrets,
    response_snippet, retrying,
};
use crate::client_error::{ClientError, Service, retry_after};
use droppedneedle_kernel::http_client::{BodyError, read_capped, transport_error};
use droppedneedle_providers::executor::decode_failure;

/// The most one NZBGet answer may weigh. The largest is the history list,
/// a few kilobytes a job; NZBGet keeps a week of it by default.
pub const MAX_NZBGET_BODY_BYTES: usize = 16 * 1024 * 1024;

/// How deep `${Option}` references in a config value are expanded.
const EXPANSION_PASSES: usize = 5;

/// An NZBGet call that failed. HTTP, transport, sign-in and decode
/// failures are a [`ClientError`]; the rest are NZBGet's own outcomes.
#[derive(Debug, Clone, Error)]
pub enum NzbgetError {
    /// The call failed the way any client call can. A refused sign-in is
    /// HTTP 401. The transport detail never holds a credential: they ride
    /// in a header.
    #[error(transparent)]
    Client(#[from] ClientError),
    /// NZBGet answered a JSON-RPC error.
    #[error("{0}")]
    Api(String),
    /// The indexer answered the NZB fetch with an HTTP error, so handing
    /// NZBGet the same URL cannot help.
    #[error("the indexer answered HTTP {status} for the NZB")]
    NzbRefused {
        /// The indexer's status code.
        status: u16,
    },
    /// The indexer is at its grab cap or asked us to slow down.
    #[error("the indexer is limiting NZB grabs")]
    NzbPaced {
        /// Seconds until it takes grabs again, when known.
        retry_after: Option<u64>,
    },
    /// The indexer's NZB passed [`MAX_NZB_BYTES`].
    #[error("the NZB is larger than {} MB and was not fetched", MAX_NZB_BYTES / (1024 * 1024))]
    NzbTooLarge,
    /// Two jobs matched one handle; refusing to guess.
    #[error("NZBGet returned an ambiguous job identity")]
    AmbiguousIdentity,
    /// The add answered no job id.
    #[error("NZBGet rejected the NZB (no job id returned)")]
    RejectedNzb,
    /// The add by link answered no job id.
    #[error("NZBGet rejected the NZB URL (no job id returned)")]
    RejectedNzbUrl,
    /// Enqueue without an NZB URL is meaningless for Usenet.
    #[error("enqueue requires an nzb_url for the usenet source")]
    MissingNzbUrl,
}

impl NzbgetError {
    /// Whether this failure means bad credentials.
    pub fn is_auth(&self) -> bool {
        matches!(self, NzbgetError::Client(error) if error.is_auth())
    }
}

/// Raw NZBGet JSON-RPC wrapper. The HTTP clients are injected; the password
/// is never logged, and Debug leaves it out.
pub struct NzbgetClient {
    /// The factory's client that follows a redirect only on the same host,
    /// for NZBGet's API: the sign-in rides in a header.
    http: Client,
    /// The factory's client that follows no redirect, for an NZB fetched
    /// straight from its link.
    nzb_http: Client,
    base_url: String,
    username: String,
    password: String,
    max_attempts: u32,
    retry_backoff: Duration,
}

impl std::fmt::Debug for NzbgetClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NzbgetClient")
            .field("base_url", &self.base_url)
            .field("username", &self.username)
            .field("password", &"<redacted>")
            .field("max_attempts", &self.max_attempts)
            .field("retry_backoff", &self.retry_backoff)
            .finish()
    }
}

impl NzbgetClient {
    /// Build over the injected clients: `http`, which follows a redirect
    /// only on the same host, for the API and `nzb_http`, which follows no
    /// redirect, for NZB links. `base_url` is NZBGet's
    /// origin (`/jsonrpc` is appended per call).
    pub fn new(
        http: Client,
        nzb_http: Client,
        base_url: &str,
        username: &str,
        password: &str,
        max_attempts: u32,
        retry_backoff: Duration,
    ) -> Self {
        NzbgetClient {
            http,
            nzb_http,
            base_url: base_url.trim_end_matches('/').to_owned(),
            username: username.to_owned(),
            password: password.to_owned(),
            max_attempts: max_attempts.max(1),
            retry_backoff,
        }
    }

    /// Server version string.
    pub async fn version(&self, timeout: Duration) -> Result<String, NzbgetError> {
        self.send("version", &serde_json::json!([]), timeout, true)
            .await
    }

    /// Every config option, values unexpanded.
    pub async fn config(&self, timeout: Duration) -> Result<Vec<ConfigEntry>, NzbgetError> {
        self.send("config", &serde_json::json!([]), timeout, true)
            .await
    }

    /// Jobs still in the queue.
    pub async fn list_groups(&self, timeout: Duration) -> Result<Vec<Group>, NzbgetError> {
        self.send("listgroups", &serde_json::json!([0]), timeout, true)
            .await
    }

    /// Finished jobs, hidden ones left out.
    pub async fn history(&self, timeout: Duration) -> Result<Vec<HistoryItem>, NzbgetError> {
        self.send("history", &serde_json::json!([false]), timeout, true)
            .await
    }

    /// Run one queue or history command on `ids`. Safe to repeat: a
    /// command on a job already gone answers false.
    pub async fn edit_queue(
        &self,
        command: &str,
        ids: &[i64],
        timeout: Duration,
    ) -> Result<bool, NzbgetError> {
        if ids.is_empty() {
            return Ok(true);
        }
        self.send(
            "editqueue",
            &serde_json::json!([command, "", ids]),
            timeout,
            true,
        )
        .await
    }

    /// Add a job from NZB bytes, or from a link NZBGet fetches itself.
    /// `job_name` is both the job's name and its dupe key, so a restart
    /// before the id is stored still finds it. Never retried: a second add
    /// could queue the job twice. Errors from an add by link are scrubbed
    /// of credentials the link may carry.
    pub async fn append(
        &self,
        job_name: &str,
        content: AppendContent<'_>,
        category: Option<&str>,
        priority: Option<i32>,
        timeout: Duration,
    ) -> Result<i64, NzbgetError> {
        let (content, by_link) = match content {
            AppendContent::Nzb(bytes) => (
                base64::engine::general_purpose::STANDARD.encode(bytes),
                false,
            ),
            AppendContent::Link(url) => (url.to_owned(), true),
        };
        let params = serde_json::json!([
            format!("{job_name}.nzb"),
            content,
            category.unwrap_or(""),
            priority.unwrap_or(0),
            false,
            false,
            job_name,
            0,
            "FORCE",
            []
        ]);
        match self.send("append", &params, timeout, false).await {
            Ok(id) => Ok(id),
            Err(error) if by_link => Err(redacted(&error)),
            Err(error) => Err(error),
        }
    }

    /// Fetch an NZB straight from its link (a Prowlarr or plugin release),
    /// under the hop rules an indexer's grab follows.
    pub async fn fetch_nzb(&self, url: &str, timeout: Duration) -> Result<Vec<u8>, NzbFetchError> {
        fetch_nzb(
            &self.nzb_http,
            url,
            timeout,
            self.max_attempts,
            self.retry_backoff,
        )
        .await
    }

    /// One JSON-RPC call. `retry` is set only for idempotent methods, which
    /// are retried on a transport error or a 5xx.
    async fn send<T>(
        &self,
        method: &str,
        params: &serde_json::Value,
        timeout: Duration,
        retry: bool,
    ) -> Result<T, NzbgetError>
    where
        T: serde::de::DeserializeOwned,
    {
        let body = serde_json::json!({
            "version": "1.1",
            "id": 1,
            "method": method,
            "params": params,
        })
        .to_string();
        let url = format!("{}/jsonrpc", self.base_url);
        let request = || {
            let mut builder = self
                .http
                .post(&url)
                .header(CONTENT_TYPE, "application/json")
                .body(body.clone())
                .timeout(timeout);
            if !self.username.is_empty() || !self.password.is_empty() {
                builder = builder.basic_auth(&self.username, Some(&self.password));
            }
            builder.send()
        };
        let attempts = if retry { self.max_attempts } else { 1 };
        let response = retrying(request, attempts, self.retry_backoff)
            .await
            .map_err(|err| ClientError::transport(Service::Nzbget, transport_error(err)))?;
        let status = response.status().as_u16();
        // A 3xx here is a redirect to another host or to plain http, which
        // the client does not follow: not an answer.
        if status >= 300 {
            let wait = retry_after(response.headers());
            let snippet = response_snippet(response).await;
            return Err(
                ClientError::for_status(Service::Nzbget, status, wait, snippet.as_bytes()).into(),
            );
        }
        let bytes =
            read_capped(response, MAX_NZBGET_BODY_BYTES)
                .await
                .map_err(|error| match error {
                    BodyError::TooLarge => decode("the answer is too large to read".to_owned()),
                    BodyError::Transport(detail) => {
                        ClientError::transport(Service::Nzbget, detail).into()
                    }
                })?;
        // A page that is not JSON-RPC is almost always the web UI: the URL
        // points somewhere other than NZBGet's root.
        let reply: RpcReply = serde_json::from_slice(&bytes).map_err(|_| {
            decode("the answer is not JSON-RPC; check the URL points at NZBGet".to_owned())
        })?;
        if let Some(error) = reply.error {
            return Err(NzbgetError::Api(format!(
                "NZBGet {method} failed: {}",
                error.message
            )));
        }
        let Some(result) = reply.result else {
            return Err(decode(format!("NZBGet {method} answered no result")));
        };
        serde_json::from_value(result).map_err(|err| decode(decode_failure(&err)))
    }
}

/// What an `append` adds.
#[derive(Debug, Clone, Copy)]
pub enum AppendContent<'a> {
    /// The NZB itself, sent base64-encoded.
    Nzb(&'a [u8]),
    /// A link NZBGet fetches itself.
    Link(&'a str),
}

fn decode(detail: String) -> NzbgetError {
    ClientError::decode(Service::Nzbget, detail).into()
}

/// An add-by-link failure with any credentials the link carried, and NZBGet
/// may echo, scrubbed.
fn redacted(err: &NzbgetError) -> NzbgetError {
    let scrub = |detail: &str| redact_query_secrets(detail);
    match err {
        NzbgetError::Api(message) => NzbgetError::Api(scrub(message)),
        NzbgetError::Client(ClientError::Auth {
            service,
            status,
            detail,
        }) => ClientError::Auth {
            service: *service,
            status: *status,
            detail: scrub(detail),
        }
        .into(),
        NzbgetError::Client(ClientError::BadRequest { service, detail }) => {
            ClientError::BadRequest {
                service: *service,
                detail: scrub(detail),
            }
            .into()
        }
        NzbgetError::Client(ClientError::Decode { service, detail }) => ClientError::Decode {
            service: *service,
            detail: scrub(detail),
        }
        .into(),
        other => other.clone(),
    }
}

/// NZBGet as DroppedNeedle's Usenet download client. The same contract as
/// the SABnzbd queue: jobs are found by the id the add answered, or before
/// that by the job name DroppedNeedle chose, and only a job actually
/// downloading counts as an active transfer.
pub struct NzbgetQueue {
    client: NzbgetClient,
    configured: bool,
    mount: PathBuf,
    policy: UsenetPolicy,
    complete_dir_cache: Mutex<Option<String>>,
}

impl NzbgetQueue {
    /// Build over the raw client. `downloads_mount` is DroppedNeedle's
    /// mount of NZBGet's destination folder. NZBGet may run without
    /// a sign-in, so the URL alone makes it configured.
    pub fn new(
        client: NzbgetClient,
        url: &str,
        downloads_mount: PathBuf,
        policy: UsenetPolicy,
    ) -> Self {
        NzbgetQueue {
            client,
            configured: !url.is_empty(),
            mount: std::path::absolute(&downloads_mount).unwrap_or(downloads_mount),
            policy,
            complete_dir_cache: Mutex::new(None),
        }
    }

    /// Client name for routing.
    pub fn client_name(&self) -> &'static str {
        "nzbget"
    }

    /// Configured means the URL was present at build time.
    pub fn is_configured(&self) -> bool {
        self.configured
    }

    /// DroppedNeedle's mount of NZBGet's destination folder.
    pub fn downloads_mount(&self) -> &Path {
        &self.mount
    }

    /// Health never raises: failures become the error variant.
    pub async fn health_check(&self) -> Health {
        match self.client.version(self.policy.poll_timeout).await {
            Ok(version) => Health {
                status: "ok".to_owned(),
                message: if version.is_empty() {
                    "NZBGet".to_owned()
                } else {
                    format!("NZBGet {version}")
                },
                version: non_empty(version),
            },
            Err(err) => {
                tracing::warn!(%err, "nzbget health check failed");
                Health {
                    status: "error".to_owned(),
                    version: None,
                    message: if err.is_auth() {
                        "NZBGet rejected the username or password".to_owned()
                    } else {
                        "NZBGet unreachable".to_owned()
                    },
                }
            }
        }
    }

    /// NZBGet's category names (for the settings picker).
    pub async fn get_categories(&self) -> Result<Vec<String>, NzbgetError> {
        let config = self.client.config(self.policy.poll_timeout).await?;
        Ok(config
            .into_iter()
            .filter(|entry| is_category_name(&entry.name) && !entry.value.trim().is_empty())
            .map(|entry| entry.value.trim().to_owned())
            .collect())
    }

    /// NZBGet's destination folder (the settings mount hint).
    pub async fn get_complete_dir(&self) -> Result<String, NzbgetError> {
        Ok(self.complete_dir().await)
    }

    /// Enqueue one album release under `droppedneedle-{task_id}`.
    /// NZBGet has no post-processing level per job, so `post_processing`
    /// is not used.
    pub async fn enqueue_album(
        &self,
        task_id: &str,
        nzb_url: Option<&str>,
        grab: NzbGrab<'_>,
        category: Option<&str>,
        priority: Option<i32>,
        _post_processing: Option<i32>,
    ) -> Result<TaskHandle, NzbgetError> {
        self.enqueue(task_id, None, nzb_url, grab, category, priority)
            .await
    }

    /// Enqueue one album release under a worker-built job name.
    #[allow(clippy::too_many_arguments)]
    pub async fn enqueue_album_as(
        &self,
        task_id: &str,
        job_name: &str,
        nzb_url: Option<&str>,
        grab: NzbGrab<'_>,
        category: Option<&str>,
        priority: Option<i32>,
        _post_processing: Option<i32>,
    ) -> Result<TaskHandle, NzbgetError> {
        self.enqueue(task_id, Some(job_name), nzb_url, grab, category, priority)
            .await
    }

    /// Enqueue one track: the release of its album, under a track-derived
    /// job name.
    #[allow(clippy::too_many_arguments)]
    pub async fn enqueue_track(
        &self,
        task_id: &str,
        track_job: &str,
        nzb_url: Option<&str>,
        grab: NzbGrab<'_>,
        category: Option<&str>,
        priority: Option<i32>,
        _post_processing: Option<i32>,
    ) -> Result<TaskHandle, NzbgetError> {
        self.enqueue(task_id, Some(track_job), nzb_url, grab, category, priority)
            .await
    }

    async fn enqueue(
        &self,
        task_id: &str,
        job_name: Option<&str>,
        nzb_url: Option<&str>,
        grab: NzbGrab<'_>,
        category: Option<&str>,
        priority: Option<i32>,
    ) -> Result<TaskHandle, NzbgetError> {
        let Some(url) = nzb_url else {
            return Err(NzbgetError::MissingNzbUrl);
        };
        let job_name = match job_name {
            Some(name) if !name.is_empty() => name.to_owned(),
            _ => format!("droppedneedle-{task_id}"),
        };
        let fetched = match grab {
            NzbGrab::Indexer(grab) => grab_nzb(grab, url).await,
            NzbGrab::Direct => {
                self.client
                    .fetch_nzb(url, self.policy.enqueue_timeout)
                    .await
            }
        };
        let (content, rejected) = match &fetched {
            Ok(bytes) => (AppendContent::Nzb(bytes), NzbgetError::RejectedNzb),
            // No response at all: the indexer may be reachable only from
            // the NZBGet host, so hand NZBGet the link. It is never logged:
            // enclosures carry the indexer key.
            Err(NzbFetchError::Transport(_)) => {
                (AppendContent::Link(url), NzbgetError::RejectedNzbUrl)
            }
            Err(NzbFetchError::Definitive { status, .. }) => {
                return Err(NzbgetError::NzbRefused { status: *status });
            }
            Err(NzbFetchError::Paced { retry_after }) => {
                return Err(NzbgetError::NzbPaced {
                    retry_after: *retry_after,
                });
            }
            Err(NzbFetchError::TooLarge) => return Err(NzbgetError::NzbTooLarge),
            Err(NzbFetchError::ContentRejection { snippet, .. }) => {
                return Err(NzbgetError::Api(format!(
                    "indexer returned a non-NZB body (likely an error/limit page): {snippet}"
                )));
            }
        };
        let id = self
            .client
            .append(
                &job_name,
                content,
                category,
                priority,
                self.policy.enqueue_timeout,
            )
            .await?;
        if id <= 0 {
            return Err(rejected);
        }
        Ok(TaskHandle {
            source: crate::downloads::source_tag::SourceTag::Usenet.to_string(),
            job_name,
            nzo_id: id.to_string(),
        })
    }

    /// Poll the queue, then history. A job in neither reads `missing` with
    /// no matched transfers, as the SABnzbd queue does.
    pub async fn get_status(&self, handle: &TaskHandle) -> Result<TaskStatus, NzbgetError> {
        if let Some(group) = self.find_group(handle).await? {
            return Ok(group_status(&group));
        }
        if let Some(item) = self.find_history(handle).await? {
            return Ok(history_status(&item));
        }
        Ok(TaskStatus {
            status: "missing".to_owned(),
            ..TaskStatus::default()
        })
    }

    /// Stop an active job and remove only client-owned incomplete data.
    /// Completed output is not client-owned: abort returns false so the
    /// cleanup service validates and removes its exact workspace first.
    pub async fn abort(&self, handle: &TaskHandle) -> Result<bool, NzbgetError> {
        if let Some(group) = self.find_group(handle).await? {
            return self
                .client
                .edit_queue(
                    "GroupFinalDelete",
                    &[group.nzb_id],
                    self.policy.poll_timeout,
                )
                .await;
        }
        let Some(item) = self.find_history(handle).await? else {
            return Ok(true);
        };
        if outcome(&item) == Outcome::Failed {
            return self
                .client
                .edit_queue(
                    "HistoryFinalDelete",
                    &[item.nzb_id],
                    self.policy.poll_timeout,
                )
                .await;
        }
        Ok(false)
    }

    /// Fresh evidence about the job's bytes and records, inspected
    /// separately from removal so local cleanup stays durable.
    pub async fn inspect_materialization(
        &self,
        handle: &TaskHandle,
    ) -> Result<Materialization, NzbgetError> {
        if let Some(group) = self.find_group(handle).await? {
            return Ok(Materialization {
                state: "active".to_owned(),
                nzo_id: group.nzb_id.to_string(),
                mount_root: self.mount.display().to_string(),
                mount_healthy: mount_healthy(&self.mount).await,
                ..Materialization::default()
            });
        }
        let item = self.find_history(handle).await?;
        let healthy = mount_healthy(&self.mount).await;
        let Some(item) = item else {
            return Ok(Materialization {
                state: "missing".to_owned(),
                mount_root: self.mount.display().to_string(),
                mount_healthy: healthy,
                ..Materialization::default()
            });
        };
        let storage = item.storage().to_owned();
        let local = if storage.is_empty() {
            None
        } else {
            let complete_dir = self.complete_dir().await;
            exact_local_storage(&self.mount, &complete_dir, &storage)
        };
        Ok(Materialization {
            state: match outcome(&item) {
                Outcome::Completed => "completed",
                Outcome::Failed => "failed",
                Outcome::Running => "active",
            }
            .to_owned(),
            nzo_id: item.nzb_id.to_string(),
            remote_storage: storage,
            mount_root: self.mount.display().to_string(),
            workspace_path: local
                .map(|path| path.display().to_string())
                .unwrap_or_default(),
            mount_healthy: healthy,
        })
    }

    /// Drop the history record after local cleanup is durable. A failed
    /// job's record goes for good; a completed one is hidden, as NZBGet's
    /// own history delete does.
    pub async fn discard_client_artifacts(&self, handle: &TaskHandle) -> Result<bool, NzbgetError> {
        let Some(item) = self.find_history(handle).await? else {
            let groups = self.client.list_groups(self.policy.poll_timeout).await?;
            let still_queued = groups.iter().any(|group| group_matches(group, handle));
            return Ok(!still_queued);
        };
        let command = if outcome(&item) == Outcome::Failed {
            "HistoryFinalDelete"
        } else {
            "HistoryDelete"
        };
        self.client
            .edit_queue(command, &[item.nzb_id], self.policy.poll_timeout)
            .await
    }

    /// Cross-check completions against the mount so a silently-wrong path
    /// is caught proactively. Never raises.
    pub async fn diagnose_downloads_mount(&self) -> MountDiagnosis {
        let client_dir = self.complete_dir().await;
        let empty = |client_dir: String| MountDiagnosis {
            supported: true,
            completed_downloads: 0,
            mount_has_files: true,
            resolvable_downloads: 0,
            sampled_downloads: 0,
            client_downloads_dir: non_empty(client_dir),
        };
        let Ok(history) = self.client.history(self.policy.poll_timeout).await else {
            return empty(client_dir);
        };
        let completed: Vec<&HistoryItem> = history
            .iter()
            .filter(|item| outcome(item) == Outcome::Completed && !item.storage().is_empty())
            .collect();
        if completed.is_empty() {
            return empty(client_dir);
        }
        let mut resolvable = 0;
        let sample = &completed[..completed.len().min(DIAGNOSIS_SAMPLE)];
        for item in sample {
            let Some(local) =
                local_storage(&self.mount, &client_dir, item.storage(), "nzbget").await
            else {
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

    async fn find_group(&self, handle: &TaskHandle) -> Result<Option<Group>, NzbgetError> {
        let groups = self.client.list_groups(self.policy.poll_timeout).await?;
        unique(
            groups
                .into_iter()
                .filter(|group| group_matches(group, handle)),
        )
    }

    async fn find_history(&self, handle: &TaskHandle) -> Result<Option<HistoryItem>, NzbgetError> {
        let history = self.client.history(self.policy.poll_timeout).await?;
        unique(
            history
                .into_iter()
                .filter(|item| history_matches(item, handle)),
        )
    }

    /// NZBGet's `DestDir` with its `${Option}` references expanded, cached
    /// only once known: a failed read must not pin the basename-only remap.
    async fn complete_dir(&self) -> String {
        if let Some(cached) = self.complete_dir_cache.lock().await.clone() {
            return cached;
        }
        let value = self
            .client
            .config(self.policy.poll_timeout)
            .await
            .map(|config| dest_dir(&config))
            .unwrap_or_default();
        if !value.is_empty() {
            *self.complete_dir_cache.lock().await = Some(value.clone());
        }
        value
    }
}

fn unique<T>(mut matches: impl Iterator<Item = T>) -> Result<Option<T>, NzbgetError> {
    let first = matches.next();
    if matches.next().is_some() {
        return Err(NzbgetError::AmbiguousIdentity);
    }
    Ok(first)
}

/// The job name must match NZBGet's job name or the dupe key DroppedNeedle
/// set, and a stored id must match too. An id never matches alone: NZBGet
/// numbers jobs from 1 again on a fresh queue, so after a reinstall a
/// stored id can name another app's job.
fn job_matches(id: i64, names: &[&str], handle: &TaskHandle) -> bool {
    if handle.job_name.is_empty() || !names.iter().any(|name| *name == handle.job_name) {
        return false;
    }
    handle.nzo_id.is_empty() || id.to_string() == handle.nzo_id
}

fn group_matches(group: &Group, handle: &TaskHandle) -> bool {
    job_matches(group.nzb_id, &[&group.nzb_name, &group.dupe_key], handle)
}

fn history_matches(item: &HistoryItem, handle: &TaskHandle) -> bool {
    job_matches(
        item.nzb_id,
        &[&item.nzb_name, &item.name, &item.dupe_key],
        handle,
    )
}

/// Queue mapping: only `DOWNLOADING` is active; post-download states hold
/// the bar at 100% with `processing`.
fn group_status(group: &Group) -> TaskStatus {
    let state = group.status.to_ascii_uppercase();
    let bytes_total = group.size();
    let mut bytes_downloaded = bytes_total.saturating_sub(group.remaining());
    let mut percent = if bytes_total == 0 {
        0.0
    } else {
        bytes_downloaded as f64 * 100.0 / bytes_total as f64
    };
    let status = match state.as_str() {
        "QUEUED" | "PAUSED" | "FETCHING" => "queued",
        "DOWNLOADING" => "downloading",
        _ => {
            percent = 100.0;
            bytes_downloaded = bytes_total;
            "processing"
        }
    };
    TaskStatus {
        status: status.to_owned(),
        files_total: 1,
        bytes_total,
        bytes_downloaded,
        progress_percent: percent,
        has_active_transfer: state == "DOWNLOADING",
        matched_transfers: 1,
        ..TaskStatus::default()
    }
}

/// How a finished job ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Outcome {
    /// The files are there to land.
    Completed,
    /// Nothing usable came out, so the worker fails over.
    Failed,
    /// A status NZBGet does not document; polled again.
    Running,
}

/// NZBGet's history `Status`, as its API documents it (`history`, checked
/// 2026-11-16 at nzbget.com/documentation/api/history): every `SUCCESS/*`
/// and `WARNING/SCRIPT` (only a post-processing script failed) complete.
/// Every other `WARNING/*` (`DAMAGED`, `REPAIRABLE`, `PASSWORD`, `SPACE`,
/// `HEALTH`, `SKIPPED`) left the files damaged, packed or absent, and
/// fails like `FAILURE/*` and `DELETED/*`.
fn outcome(item: &HistoryItem) -> Outcome {
    let status = item.status.to_ascii_uppercase();
    let (category, detail) = status.split_once('/').unwrap_or((&status, ""));
    match (category, detail) {
        ("SUCCESS", _) | ("WARNING", "SCRIPT") => Outcome::Completed,
        ("WARNING" | "FAILURE" | "DELETED", _) => Outcome::Failed,
        _ => Outcome::Running,
    }
}

/// History mapping by [`outcome`]. A deleted job is a terminal failure so
/// the worker fails over instead of polling to the deadline.
fn history_status(item: &HistoryItem) -> TaskStatus {
    let bytes = item.size();
    match outcome(item) {
        Outcome::Failed if item.status_category() == "DELETED" => TaskStatus {
            status: "failed".to_owned(),
            error: Some("job removed from NZBGet".to_owned()),
            matched_transfers: 1,
            ..TaskStatus::default()
        },
        Outcome::Completed => TaskStatus {
            status: "completed".to_owned(),
            files_total: 1,
            files_completed: 1,
            bytes_total: bytes,
            bytes_downloaded: bytes,
            progress_percent: 100.0,
            matched_transfers: 1,
            ..TaskStatus::default()
        },
        Outcome::Failed => TaskStatus {
            status: "failed".to_owned(),
            error: Some(failure_reason(item)),
            bytes_total: bytes,
            matched_transfers: 1,
            ..TaskStatus::default()
        },
        Outcome::Running => TaskStatus {
            status: "processing".to_owned(),
            files_total: 1,
            bytes_total: bytes,
            bytes_downloaded: bytes,
            progress_percent: 100.0,
            matched_transfers: 1,
            ..TaskStatus::default()
        },
    }
}

/// The first post-processing step that did not succeed, else the status
/// detail.
fn failure_reason(item: &HistoryItem) -> String {
    let steps = [
        ("repair", &item.par_status),
        ("unpack", &item.unpack_status),
        ("move", &item.move_status),
        ("script", &item.script_status),
    ];
    for (label, value) in steps {
        let value = value.to_ascii_uppercase();
        if !matches!(value.as_str(), "" | "SUCCESS" | "NONE" | "OK" | "SKIPPED") {
            return format!("{label} failed: {value}");
        }
    }
    match item.status.split_once('/') {
        Some((_, detail)) if !detail.is_empty() => format!("download failed: {detail}"),
        _ => "download failed".to_owned(),
    }
}

/// `Category<N>.Name`, any case.
fn is_category_name(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    lower
        .strip_prefix("category")
        .and_then(|rest| rest.strip_suffix(".name"))
        .is_some_and(|digits| !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()))
}

/// `DestDir` with `${Option}` references (usually `${MainDir}`) expanded
/// from the other options, a few levels deep.
fn dest_dir(config: &[ConfigEntry]) -> String {
    let lookup = |key: &str| {
        config
            .iter()
            .find(|entry| entry.name.eq_ignore_ascii_case(key))
            .map(|entry| entry.value.clone())
    };
    let Some(mut value) = lookup("DestDir") else {
        return String::new();
    };
    for _ in 0..EXPANSION_PASSES {
        let Some(start) = value.find("${") else {
            break;
        };
        let Some(len) = value[start + 2..].find('}') else {
            break;
        };
        let replacement = lookup(&value[start + 2..start + 2 + len]).unwrap_or_default();
        value.replace_range(start..start + 3 + len, &replacement);
    }
    value
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(name: &str, value: &str) -> ConfigEntry {
        ConfigEntry {
            name: name.to_owned(),
            value: value.to_owned(),
        }
    }

    #[test]
    fn dest_dir_expands_main_dir() {
        let config = [
            entry("MainDir", "/data"),
            entry("DestDir", "${MainDir}/completed"),
        ];
        assert_eq!(dest_dir(&config), "/data/completed");
    }

    #[test]
    fn category_names_need_digits() {
        assert!(is_category_name("Category1.Name"));
        assert!(is_category_name("category12.name"));
        assert!(!is_category_name("Category.Name"));
        assert!(!is_category_name("Category1.DestDir"));
    }
}
