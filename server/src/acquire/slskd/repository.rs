//! `SlskdRepository`: search and acquisition over one slskd instance.
//!
//! Ported from v2's slskd repository. Owns the search and enqueue
//! semaphores (both 1; slskd permits only one concurrent search and one
//! concurrent enqueue) and translates slskd JSON shapes to/from the
//! protocol types. slskd has no batch id: a task is correlated to its
//! transfers by `TaskHandle(source="soulseek", username, filenames)`.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;

use tokio::sync::Semaphore;
use unicode_normalization::UnicodeNormalization;

use super::client::SlskdClient;
use super::error::SlskdError;
use super::http::SlskdHttp;
use super::models::{SlskdEnqueueResponse, SlskdTransfer, SlskdUserSearchResponse};
use super::policy::DownloadPolicy;
use super::query::{album_query_ladder, track_query_ladder};

/// How many finished transfers the mount diagnosis tries to locate under
/// the mount (v2 `_DIAGNOSIS_SAMPLE`). Small: it is a settings-page check
/// and a wrong mount makes each a full walk.
const DIAGNOSIS_SAMPLE: usize = 3;

/// Correlation key for one enqueue: slskd returns no batch GUID, so the
/// `(username, filenames)` pair is the task identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskHandle {
    pub source: String,
    pub username: String,
    pub filenames: Vec<String>,
}

impl TaskHandle {
    #[must_use]
    pub fn new(username: &str, filenames: Vec<String>) -> Self {
        Self {
            source: "soulseek".to_owned(),
            username: username.to_owned(),
            filenames,
        }
    }
}

/// One file to enqueue: which peer holds it and its advertised size.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnqueueFile {
    pub username: String,
    pub filename: String,
    pub size: i64,
}

/// One per-file search hit, translated from slskd's shapes (v2
/// `DownloadSearchResult` fields the repository fills).
#[derive(Debug, Clone)]
pub struct SearchResult {
    pub username: String,
    pub filename: String,
    pub parent_directory: String,
    pub size: i64,
    /// Lowercase extension parsed from the filename (v2 C6a).
    pub extension: String,
    /// `None` for lossless (bitRate absent on the wire, never coerced).
    pub bitrate: Option<i32>,
    pub bit_depth: Option<i32>,
    pub sample_rate: Option<i32>,
    pub duration: Option<f64>,
    pub has_free_slot: bool,
    pub upload_speed: i64,
    pub queue_length: i64,
}

/// Aggregate per-file status for one task (v2 `DownloadTaskStatus` fields
/// the repository fills).
#[derive(Debug, Clone, PartialEq)]
pub struct TaskStatus {
    /// `completed` | `partial` | `failed` | `downloading` | `queued`.
    pub status: String,
    pub files_total: usize,
    pub files_completed: usize,
    pub files_failed: usize,
    pub bytes_total: i64,
    pub bytes_downloaded: i64,
    pub progress_percent: f64,
    pub succeeded_filenames: Vec<String>,
    pub has_active_transfer: bool,
    pub matched_transfers: usize,
    pub queue_position_start: Option<i64>,
    pub queue_position_end: Option<i64>,
}

/// Health outcome. Auth failures carry the uniform message (never the
/// URL/host/key/headers); the slskd body (usually empty) is appended only
/// as a stripped single-line snippet (v2 `health_check`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceStatus {
    pub ok: bool,
    pub version: Option<String>,
    pub message: String,
}

/// Mount cross-check outcome (v2 `MountDiagnosis` fields the repository
/// fills). Best-effort: diagnosing never raises.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MountDiagnosis {
    pub supported: bool,
    pub completed_downloads: usize,
    pub mount_has_files: bool,
    pub resolvable_downloads: usize,
    pub sampled_downloads: usize,
    pub client_downloads_dir: Option<String>,
}

/// Search and acquisition over one slskd instance.
pub struct SlskdRepository<T: SlskdHttp> {
    client: SlskdClient<T>,
    // The key itself lives only in the HTTP layer; the repository keeps
    // just the configured bit for `is_configured`.
    configured: bool,
    downloads_mount: PathBuf,
    incomplete_mount: Option<PathBuf>,
    search_permits: Arc<Semaphore>,
    enqueue_permits: Arc<Semaphore>,
    policy: DownloadPolicy,
}

impl<T: SlskdHttp> SlskdRepository<T> {
    #[must_use]
    pub fn new(
        client: SlskdClient<T>,
        url: &str,
        api_key: &str,
        downloads_mount: PathBuf,
        policy: DownloadPolicy,
    ) -> Self {
        Self {
            client,
            configured: !url.is_empty() && !api_key.is_empty(),
            downloads_mount,
            incomplete_mount: None,
            // slskd permits only one concurrent search and one concurrent
            // enqueue.
            search_permits: Arc::new(Semaphore::new(1)),
            enqueue_permits: Arc::new(Semaphore::new(1)),
            policy,
        }
    }

    /// Optional second mount for slskd's incomplete dir (v2 #292). `None`
    /// disables the partial fallback entirely; never consulted by
    /// `get_file_path`, only by `locate_partial`.
    #[must_use]
    pub fn with_incomplete_mount(mut self, mount: PathBuf) -> Self {
        self.incomplete_mount = Some(mount);
        self
    }

    #[must_use]
    pub fn client_name(&self) -> &'static str {
        "slskd"
    }

    #[must_use]
    pub fn is_configured(&self) -> bool {
        self.configured
    }

    #[must_use]
    pub fn policy(&self) -> &DownloadPolicy {
        &self.policy
    }

    pub async fn health_check(&self) -> ServiceStatus {
        match self.client.health_check().await {
            Err(SlskdError::Auth { status, detail }) => {
                // Wrong key and key-CIDR deny are both 401 server-side: one
                // uniform message, never the URL/host/key/headers (v2).
                let code = if status == 401 || status == 403 {
                    status
                } else {
                    401
                };
                let mut message = format!(
                    "Authentication rejected ({code}): check the API key and slskd's CIDR allowlist"
                );
                if !detail.is_empty() {
                    message.push_str(": ");
                    message.push_str(&detail);
                }
                ServiceStatus {
                    ok: false,
                    version: None,
                    message,
                }
            }
            Err(err) => {
                // Plain summary on the surface; technical detail in the log.
                tracing::warn!(%err, "slskd health check failed");
                ServiceStatus {
                    ok: false,
                    version: None,
                    message: "slskd unreachable".to_owned(),
                }
            }
            Ok(info) => {
                let version = info
                    .get("version")
                    .and_then(|block| block.as_object())
                    .and_then(|block| {
                        block
                            .get("current")
                            .or_else(|| block.get("currentVersion"))
                            .and_then(|value| value.as_str())
                            .map(str::to_owned)
                    });
                let message = version
                    .as_ref()
                    .map_or_else(|| "slskd".to_owned(), |v| format!("slskd {v}"));
                ServiceStatus {
                    ok: true,
                    version,
                    message,
                }
            }
        }
    }

    /// Album acquisition search: escalating query breadth (v2
    /// `search_album`). Returns the first rung with results.
    pub async fn search_album(
        &self,
        artist_name: &str,
        album_title: &str,
        year: Option<i32>,
    ) -> Result<Vec<SearchResult>, SlskdError> {
        for query in album_query_ladder(artist_name, album_title, year) {
            let results = self.run_search(&query).await?;
            if !results.is_empty() {
                return Ok(results);
            }
        }
        Ok(Vec::new())
    }

    /// Track acquisition search: like [`Self::search_album`] but every rung
    /// keeps the track title so the track matcher can pick the right
    /// recording (v2 `search_track`).
    pub async fn search_track(
        &self,
        artist_name: &str,
        track_title: &str,
        album_title: Option<&str>,
    ) -> Result<Vec<SearchResult>, SlskdError> {
        for query in track_query_ladder(artist_name, track_title, album_title) {
            let results = self.run_search(&query).await?;
            if !results.is_empty() {
                return Ok(results);
            }
        }
        Ok(Vec::new())
    }

    /// Enqueue files from one peer. The correlation key is
    /// (username, filenames) since slskd returns no batch GUID (v2).
    /// Serialized via the enqueue semaphore; the client retries the 429
    /// "only one concurrent operation" with backoff (v2).
    pub async fn enqueue(&self, files: &[EnqueueFile]) -> Result<TaskHandle, SlskdError> {
        let first = files
            .first()
            .ok_or_else(|| SlskdError::Decode("enqueue requires at least one file".to_owned()))?;
        let username = first.username.clone();
        let requested: Vec<String> = files.iter().map(|file| file.filename.clone()).collect();
        let payload: Vec<(String, i64)> = files
            .iter()
            .map(|file| (file.filename.clone(), file.size))
            .collect();
        let _permit = self
            .enqueue_permits
            .acquire()
            .await
            .map_err(|err| SlskdError::Transport(err.to_string()))?;
        let result = self.client.enqueue(&username, &payload).await?;
        if !result.failed.is_empty() {
            tracing::warn!(
                rejected = result.failed.len(),
                total = files.len(),
                peer = %username,
                "slskd rejected some enqueued files"
            );
        }
        // The correlation key must reflect what slskd accepted, not the
        // input set, or get_status/cancel poll forever on transfers never
        // created for rejected files (v2).
        Ok(TaskHandle::new(
            &username,
            accepted_filenames(&result, &requested),
        ))
    }

    /// Aggregate per-file status for one task.
    pub async fn get_status(&self, handle: &TaskHandle) -> Result<TaskStatus, SlskdError> {
        let transfers = self.client.get_downloads(&handle.username).await?;
        let matched = match_transfers(handle, &transfers);
        Ok(aggregate_status(handle, &matched))
    }

    /// Remove the transfer records for one task (v2 `abort` /
    /// `discard_client_artifacts` / `_remove_transfer_records`).
    pub async fn abort(&self, handle: &TaskHandle) -> Result<bool, SlskdError> {
        self.remove_transfer_records(handle).await
    }

    pub async fn discard_client_artifacts(&self, handle: &TaskHandle) -> Result<bool, SlskdError> {
        self.remove_transfer_records(handle).await
    }

    async fn remove_transfer_records(&self, handle: &TaskHandle) -> Result<bool, SlskdError> {
        let transfers = self.client.get_downloads(&handle.username).await?;
        let matched = match_transfers(handle, &transfers);
        let mut ok = true;
        for transfer in &matched {
            ok = self
                .client
                .cancel_transfer(&handle.username, &transfer.id)
                .await?
                && ok;
        }
        Ok(ok)
    }

    /// Cross-check slskd's completed (not-yet-imported) downloads against
    /// the configured mount (v2 `diagnose_downloads_mount`). Best-effort:
    /// never raises. The real test is whether a sample of those finished
    /// files actually resolves under the mount (`resolvable_downloads`);
    /// `mount_has_files` is a weaker signal that a parent-of-downloads
    /// mount (e.g. the whole library) defeats.
    pub async fn diagnose_downloads_mount(&self) -> MountDiagnosis {
        let client_dir = self.configured_downloads_dir().await;
        let transfers = match self.client.get_all_downloads().await {
            Ok(transfers) => transfers,
            Err(_) => {
                return MountDiagnosis {
                    supported: true,
                    completed_downloads: 0,
                    mount_has_files: false,
                    resolvable_downloads: 0,
                    sampled_downloads: 0,
                    client_downloads_dir: client_dir,
                };
            }
        };
        let completed: Vec<&SlskdTransfer> = transfers
            .iter()
            .filter(|transfer| state_flags(&transfer.state).contains("succeeded"))
            .collect();
        if completed.is_empty() {
            return MountDiagnosis {
                supported: true,
                completed_downloads: 0,
                mount_has_files: true,
                resolvable_downloads: 0,
                sampled_downloads: 0,
                client_downloads_dir: client_dir,
            };
        }
        // Resolve a small sample under the mount: the cheap get_file_path
        // steps hit first for a correct mount, so only a misconfigured one
        // pays the walk cost (v2).
        let mut resolvable = 0;
        let sample: Vec<&&SlskdTransfer> = completed.iter().take(DIAGNOSIS_SAMPLE).collect();
        for transfer in &sample {
            let handle = TaskHandle::new(&transfer.username, Vec::new());
            let size = (transfer.size > 0).then_some(transfer.size);
            let located = self
                .get_file_path(&handle, &transfer.filename, size)
                .await
                .unwrap_or(None);
            if located.is_some() {
                resolvable += 1;
            }
        }
        let mount = self.downloads_mount.clone();
        let has_files = tokio::task::spawn_blocking(move || mount_has_any_file(&mount))
            .await
            .unwrap_or(false);
        MountDiagnosis {
            supported: true,
            completed_downloads: completed.len(),
            mount_has_files: has_files,
            resolvable_downloads: resolvable,
            sampled_downloads: sample.len(),
            client_downloads_dir: client_dir,
        }
    }

    /// slskd's own `directories.downloads` (its in-container path),
    /// best-effort, shown to the user so they can match it to the mount (v2).
    async fn configured_downloads_dir(&self) -> Option<String> {
        match self.client.get_options().await {
            Ok(options) if !options.directories.downloads.is_empty() => {
                Some(options.directories.downloads)
            }
            _ => None,
        }
    }

    /// Resolve a finished transfer to its on-disk path, OFF the async
    /// runtime (v2 runs the lookup in a thread: the bounded but potentially
    /// large walks froze the whole loop (polling, SSE, every request)
    /// whenever the mount was big or misconfigured).
    pub async fn get_file_path(
        &self,
        handle: &TaskHandle,
        remote_filename: &str,
        size: Option<i64>,
    ) -> Result<Option<PathBuf>, SlskdError> {
        let locator = super::locate::Locator::new(
            self.downloads_mount.clone(),
            self.incomplete_mount.clone(),
        );
        let username = handle.username.clone();
        let remote = remote_filename.to_owned();
        tokio::task::spawn_blocking(move || locator.locate_file(&username, &remote, size))
            .await
            .map_err(|err| SlskdError::Transport(err.to_string()))
    }

    /// Basename-keyed partial fallback confined to the incomplete mount
    /// (v2 `locate_partial`). Never called by `get_file_path`; only the
    /// verifier's retry-signal path consults it, and only for subset
    /// imports.
    pub async fn locate_partial(
        &self,
        handle: &TaskHandle,
        remote_filename: &str,
        size: Option<i64>,
    ) -> Result<Option<PathBuf>, SlskdError> {
        let locator = super::locate::Locator::new(
            self.downloads_mount.clone(),
            self.incomplete_mount.clone(),
        );
        let username = handle.username.clone();
        let remote = remote_filename.to_owned();
        tokio::task::spawn_blocking(move || locator.locate_partial(&username, &remote, size))
            .await
            .map_err(|err| SlskdError::Transport(err.to_string()))
    }

    /// Run one query rung: start the search, then poll past the search
    /// window by the completion grace (v2 `_run_search`).
    async fn run_search(&self, query: &str) -> Result<Vec<SearchResult>, SlskdError> {
        let _permit = self
            .search_permits
            .acquire()
            .await
            .map_err(|err| SlskdError::Transport(err.to_string()))?;
        let timeout = self.policy.search_timeout;
        let search = self.client.start_search(query, timeout).await?;
        let deadline = tokio::time::Instant::now() + timeout + self.policy.completion_grace;
        loop {
            if tokio::time::Instant::now() >= deadline {
                tracing::warn!(
                    search_id = %search.id,
                    waited_secs = (timeout + self.policy.completion_grace).as_secs(),
                    "slskd search did not complete in time"
                );
                return Ok(Vec::new());
            }
            let state = self.client.get_search_state(&search.id).await?;
            if state.is_complete {
                let responses = self.client.get_search_responses(&search.id).await?;
                return Ok(parse_search_responses(&responses));
            }
            tokio::time::sleep(self.policy.poll_interval).await;
        }
    }
}

/// Translate peer responses to per-file hits (v2 `_parse_search_responses`).
/// Walks up past disc-pattern directories (`Disc N` / `CD N`, v2 `_DISC_DIR`)
/// so multi-disc albums group by the album-level folder.
pub fn parse_search_responses(responses: &[SlskdUserSearchResponse]) -> Vec<SearchResult> {
    let mut out = Vec::new();
    for response in responses {
        for file in &response.files {
            let parts: Vec<&str> = file.filename.split(['/', '\\']).collect();
            let mut parent = parts.get(parts.len().saturating_sub(2)).unwrap_or(&"");
            if !parent.is_empty() && is_disc_dir(parent) && parts.len() >= 3 {
                parent = &parts[parts.len() - 3];
            }
            out.push(SearchResult {
                username: response.username.clone(),
                filename: file.filename.clone(),
                parent_directory: (*parent).to_owned(),
                size: file.size,
                extension: extension_from_filename(&file.filename),
                bitrate: file.bit_rate,
                bit_depth: file.bit_depth,
                sample_rate: file.sample_rate,
                duration: file.length,
                has_free_slot: response.has_free_upload_slot,
                upload_speed: response.upload_speed.max(0),
                queue_length: response.queue_length,
            });
        }
    }
    out
}

fn is_disc_dir(name: &str) -> bool {
    // `\b(?:Disc|CD)\s*\d+\b`, case-insensitive (v2 `_DISC_DIR`).
    let lower = name.to_lowercase();
    let bytes = lower.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        let rest = &lower[index..];
        let after_keyword = rest.strip_prefix("disc").or_else(|| {
            // `cd` must not match inside a longer word: require a
            // non-alphanumeric boundary before it.
            rest.strip_prefix("cd")
                .filter(|_| index == 0 || !bytes[index - 1].is_ascii_alphanumeric())
        });
        if let Some(after) = after_keyword {
            let digits = after.trim_start_matches(|ch: char| ch.is_whitespace());
            let digit_len = digits
                .chars()
                .take_while(|ch| ch.is_numeric())
                .map(char::len_utf8)
                .sum::<usize>();
            if digit_len > 0 {
                let after_digits = &digits[digit_len..];
                let boundary_after = after_digits
                    .chars()
                    .next()
                    .is_none_or(|ch| !ch.is_alphanumeric() && ch != '_');
                let boundary_before = index == 0 || {
                    let prev = lower[..index].chars().next_back();
                    prev.is_none_or(|ch| !ch.is_alphanumeric() && ch != '_')
                };
                if boundary_before && boundary_after {
                    return true;
                }
            }
        }
        index += lower[index..].chars().next().map_or(1, |ch| ch.len_utf8());
    }
    false
}

/// Lowercase extension parsed from the filename (v2
/// `_extension_from_filename`; slskd's `extension` field is unreliable, v2
/// C6a).
#[must_use]
pub fn extension_from_filename(filename: &str) -> String {
    let base = filename.rsplit(['/', '\\']).next().unwrap_or(filename);
    match base.rfind('.') {
        Some(dot) if dot > 0 => base[dot + 1..].to_lowercase(),
        _ => String::new(),
    }
}

/// Lowercase comma-joined state flags (v2 `_state_flags`).
#[must_use]
pub fn state_flags(state: &str) -> HashSet<String> {
    state
        .split(',')
        .map(str::trim)
        .filter(|flag| !flag.is_empty())
        .map(|flag| flag.to_lowercase())
        .collect()
}

/// Filenames slskd actually accepted (v2 `_accepted_filenames`).
/// Enqueued/Failed entries are untyped: extract filenames when present,
/// else requested-minus-failed, else the full requested set.
#[must_use]
pub fn accepted_filenames(result: &SlskdEnqueueResponse, requested: &[String]) -> Vec<String> {
    fn names(entries: &[serde_json::Value]) -> Vec<String> {
        let mut out = Vec::new();
        for entry in entries {
            if let Some(filename) = entry.get("filename").and_then(|value| value.as_str()) {
                out.push(filename.to_owned());
            } else if let Some(text) = entry.as_str() {
                out.push(text.to_owned());
            }
        }
        out
    }

    let enqueued = names(&result.enqueued);
    if !enqueued.is_empty() {
        return enqueued;
    }
    let failed: HashSet<String> = names(&result.failed).into_iter().collect();
    if failed.is_empty() {
        return requested.to_vec();
    }
    requested
        .iter()
        .filter(|name| !failed.contains(*name))
        .cloned()
        .collect()
}

/// NFC form used for filename comparisons only (v2 `_normalised_filename`).
fn normalised_filename(value: &str) -> String {
    value.nfc().collect()
}

/// Transfer path key with separators normalised, but not Unicode (v2
/// `_exact_transfer_path`).
fn exact_transfer_path(value: &str) -> String {
    value.replace('\\', "/")
}

/// Canonical comparison key for a path reported by slskd (v2
/// `_normalised_path`).
fn normalised_path(value: &str) -> String {
    normalised_filename(&exact_transfer_path(value))
}

/// Match transfer records to handle filenames without merging spellings
/// (v2 `_match_transfers`). Each handle filename claims all records with
/// its exact transfer path key; if no exact spelling is present, it may
/// claim the NFC-equivalent records only when those records have one
/// distinct exact spelling. A record is assigned only once when multiple
/// handle filenames overlap.
#[must_use]
pub fn match_transfers<'t>(
    handle: &TaskHandle,
    transfers: &'t [SlskdTransfer],
) -> Vec<&'t SlskdTransfer> {
    let mut exact: HashMap<String, Vec<usize>> = HashMap::new();
    let mut nfc: HashMap<String, HashMap<String, Vec<usize>>> = HashMap::new();
    for (index, transfer) in transfers.iter().enumerate() {
        let exact_key = exact_transfer_path(&transfer.filename);
        exact.entry(exact_key.clone()).or_default().push(index);
        nfc.entry(normalised_path(&transfer.filename))
            .or_default()
            .entry(exact_key)
            .or_default()
            .push(index);
    }

    let mut assigned: HashSet<usize> = HashSet::new();
    for filename in &handle.filenames {
        if let Some(matches) = exact.get(&exact_transfer_path(filename)) {
            assigned.extend(matches.iter().copied());
        }
    }
    for filename in &handle.filenames {
        if exact.contains_key(&exact_transfer_path(filename)) {
            continue;
        }
        if let Some(spellings) = nfc.get(&normalised_path(filename))
            && spellings.len() == 1
            && let Some(indices) = spellings.values().next()
        {
            assigned.extend(indices.iter().copied());
        }
    }

    transfers
        .iter()
        .enumerate()
        .filter(|(index, _)| assigned.contains(index))
        .map(|(_, transfer)| transfer)
        .collect()
}

/// Collapse records to the latest attempt per unique file (v2
/// `_latest_transfer_per_file`, #131/#253): slskd appends one record per
/// retry attempt, so raw counts double-count retried files and let a stale
/// Succeeded row shadow a newer TimedOut/Errored one (and vice versa).
/// Highest recency key wins; exact ties (including two
/// untimestamped/garbage-stamped records) fall through to list order, where
/// the later record wins. Winners keep their original input order.
#[must_use]
pub fn latest_transfer_per_file<'t>(transfers: &[&'t SlskdTransfer]) -> Vec<&'t SlskdTransfer> {
    let mut best: HashMap<String, (i64, usize, &SlskdTransfer)> = HashMap::new();
    for (index, transfer) in transfers.iter().enumerate() {
        let key = exact_transfer_path(&transfer.filename);
        let recency = transfer_recency(transfer);
        let replace = best
            .get(&key)
            .is_none_or(|incumbent| recency >= incumbent.0);
        if replace {
            best.insert(key, (recency, index, transfer));
        }
    }
    let mut winners: Vec<(usize, &SlskdTransfer)> = best
        .into_values()
        .map(|(_, index, transfer)| (index, transfer))
        .collect();
    winners.sort_by_key(|(index, _)| *index);
    winners.into_iter().map(|(_, transfer)| transfer).collect()
}

/// Best-effort recency key for one transfer record (v2 `_transfer_recency`):
/// RequestedAt first, falling back to StartedAt (requestedAt is
/// absent/mixed across slskd versions, v2 PR #222). Absent or unparseable
/// values rank as the oldest possible instant; naive timestamps read as
/// UTC. slskd's `id` is a GUID, not monotonic, so it carries no recency
/// signal.
fn transfer_recency(transfer: &SlskdTransfer) -> i64 {
    for text in [&transfer.requested_at, &transfer.started_at]
        .into_iter()
        .flatten()
    {
        if let Some(epoch) = parse_datetime(text) {
            return epoch;
        }
    }
    i64::MIN
}

/// Parse the ISO-8601 timestamps slskd emits into epoch seconds. Naive
/// timestamps read as UTC (v2). Returns `None` for unparseable values.
fn parse_datetime(text: &str) -> Option<i64> {
    let text = text.trim().replace('T', " ");
    let (date_part, time_part) = text.split_once(' ')?;
    let mut date = date_part.split('-');
    let year: i64 = date.next()?.parse().ok()?;
    let month: i64 = date.next()?.parse().ok()?;
    let day: i64 = date.next()?.parse().ok()?;
    // Strip a trailing timezone: `Z` or `+HH:MM` / `+HHMM` / `+HH`.
    let mut time = time_part.trim();
    let mut offset_seconds: i64 = 0;
    if let Some(stripped) = time.strip_suffix(['Z', 'z']) {
        time = stripped;
    } else if let Some(pos) = time.rfind(['+', '-'])
        && pos > 0
    {
        let (head, tail) = time.split_at(pos);
        // Only treat it as a zone when the head still holds a clock.
        if head.contains(':') {
            let sign = if tail.starts_with('-') { -1 } else { 1 };
            let digits: String = tail[1..].chars().filter(|ch| ch.is_numeric()).collect();
            let hours: i64 = digits.get(0..2)?.parse().ok()?;
            let minutes: i64 = digits.get(2..4).unwrap_or("00").parse().ok()?;
            offset_seconds = sign * (hours * 3600 + minutes * 60);
            time = head;
        }
    }
    // Drop fractional seconds.
    let time = time.split(['.', ',']).next()?;
    let mut clock = time.split(':');
    let hour: i64 = clock.next()?.parse().ok()?;
    let minute: i64 = clock.next()?.parse().ok()?;
    let second: i64 = clock.next()?.parse().ok()?;
    if !(1..=12).contains(&month)
        || !(1..=31).contains(&day)
        || hour > 23
        || minute > 59
        || second > 60
    {
        return None;
    }
    // Days-from-civil (Howard Hinnant's algorithm), naive read as UTC.
    let adjusted_year = if month <= 2 { year - 1 } else { year };
    let era = adjusted_year.div_euclid(400);
    let year_of_era = adjusted_year.rem_euclid(400);
    let month_prime = (month + 9).rem_euclid(12);
    let day_of_year = (153 * month_prime + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    let days = era * 146_097 + day_of_era - 719_468;
    Some(days * 86_400 + hour * 3600 + minute * 60 + second - offset_seconds)
}

/// Per-file status from matched transfer records (v2 `_aggregate_status`).
/// File-level verdicts judge each file only by its latest attempt; byte
/// totals stay sum-over-all-records on purpose so cumulative progress
/// keeps counting prior attempts. A "succeeded" flag only counts when the
/// transfer moved at least `size` bytes (size known positive); a short
/// succeeded record is a truncated stub (v2 #122): failed when terminal,
/// non-terminal when still active. Unknown or non-positive sizes fail open
/// to the flag verdict.
#[must_use]
pub fn aggregate_status(handle: &TaskHandle, transfers: &[&SlskdTransfer]) -> TaskStatus {
    let files_total = handle.filenames.len();
    let bytes_total: i64 = transfers.iter().map(|transfer| transfer.size).sum();
    let bytes_downloaded: i64 = transfers
        .iter()
        .map(|transfer| transfer.bytes_transferred)
        .sum();

    let mut completed = 0;
    let mut failed = 0;
    let mut succeeded_filenames = Vec::new();
    let mut has_active_transfer = false;
    let mut queue_positions: Vec<i64> = Vec::new();

    for transfer in latest_transfer_per_file(transfers) {
        let flags = state_flags(&transfer.state);
        if let Some(place) = transfer.place_in_queue
            && place >= 0
        {
            queue_positions.push(place);
        }
        if flags.contains("succeeded") {
            let size_known = transfer.size > 0;
            if size_known && transfer.bytes_transferred < transfer.size {
                // Truncated stub flagged succeeded: never importable. Still
                // active -> stay non-terminal; terminal -> fail over/retries.
                if flags.contains("inprogress") || flags.contains("initializing") {
                    has_active_transfer = true;
                } else {
                    failed += 1;
                }
            } else {
                completed += 1;
                succeeded_filenames.push(transfer.filename.clone());
            }
        } else if flags.contains("errored")
            || flags.contains("cancelled")
            || flags.contains("failed")
            || flags.contains("rejected")
            || flags.contains("timedout")
            || flags.contains("aborted")
        {
            // 'aborted' (a "Completed, Aborted" transfer) is
            // terminal-failed, not active: without it the file never reaches
            // a terminal count and the task waits out the full queued timeout
            // instead of failing over on the next poll (v2).
            failed += 1;
        } else if flags.contains("inprogress") || flags.contains("initializing") {
            has_active_transfer = true;
        }
    }

    let progress = if bytes_total > 0 {
        bytes_downloaded as f64 / bytes_total as f64 * 100.0
    } else {
        0.0
    };

    // Terminal only once every enqueued file has a terminal matched
    // transfer, so a not-yet-materialised record can't trigger a premature
    // terminal state (v2).
    let latest_count = latest_transfer_per_file(transfers).len();
    let all_terminal = !transfers.is_empty()
        && (completed + failed) == latest_count
        && (completed + failed) >= files_total
        && files_total > 0;
    let status = if all_terminal && failed == 0 && completed == files_total {
        "completed"
    } else if all_terminal && completed > 0 {
        "partial"
    } else if all_terminal {
        "failed"
    } else if completed > 0 || bytes_downloaded > 0 {
        "downloading"
    } else {
        "queued"
    }
    .to_owned();

    TaskStatus {
        status,
        files_total,
        files_completed: completed,
        files_failed: failed,
        bytes_total,
        bytes_downloaded,
        progress_percent: progress,
        succeeded_filenames,
        has_active_transfer,
        matched_transfers: transfers.len(),
        queue_position_start: queue_positions.iter().min().copied(),
        queue_position_end: queue_positions.iter().max().copied(),
    }
}

/// Whether the downloads mount holds any file (v2 `_mount_has_any_file`):
/// bounded DFS, stops at the first hit. An unreadable or wrong-path mount
/// returns false; that is the signal. Sync filesystem I/O; the caller
/// offloads it off the async runtime.
fn mount_has_any_file(mount: &std::path::Path) -> bool {
    let mount = match mount.canonicalize() {
        Ok(resolved) => resolved,
        Err(_) => return false,
    };
    if !mount.is_dir() {
        return false;
    }
    let mut stack = vec![mount.clone()];
    let mut seen_dirs: HashSet<PathBuf> = HashSet::new();
    let mut seen = 0;
    while let Some(current) = stack.pop() {
        let current = match current.canonicalize() {
            Ok(resolved) => resolved,
            Err(_) => continue,
        };
        if !current.starts_with(&mount) || !current.is_dir() || !seen_dirs.insert(current.clone()) {
            continue;
        }
        let entries = match std::fs::read_dir(&current) {
            Ok(entries) => entries,
            Err(_) => continue,
        };
        for entry in entries.flatten() {
            seen += 1;
            if seen > 5000 {
                return true; // clearly not empty (v2)
            }
            let resolved = match entry.path().canonicalize() {
                Ok(resolved) => resolved,
                Err(_) => continue,
            };
            if !resolved.starts_with(&mount) {
                continue;
            }
            if resolved.is_file() {
                return true;
            }
            if resolved.is_dir() {
                stack.push(resolved);
            }
        }
    }
    false
}

/// Total poll window: the search timeout plus the completion grace.
#[cfg(any(test, feature = "test-support"))]
#[must_use]
pub fn poll_deadline(policy: &DownloadPolicy) -> std::time::Duration {
    policy.search_timeout + policy.completion_grace
}
