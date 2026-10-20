//! AcoustID fingerprint-lookup client (Tier-3 identification).
//!
//! Ports v2's audio fingerprinter: a lookup call posts the compressed
//! Chromaprint fingerprint to the AcoustID web service (see
//! <https://acoustid.org/webservice>) and the answer is folded into a
//! recording match. A clean "no match" is `Ok(None)`; every failure is a
//! typed [`ProviderError`]. Library identification uses
//! [`AcoustIdClient::lookup_batch`], which sends up to twenty fingerprints
//! per request.
//!
//! Every request goes through the one AcoustID [`Executor`], at the
//! `acoustid` row of the rate table (3 a second): identification, imports
//! and Library Management share it. The API key is passed to every call
//! and never stored on the client, mirroring v2's per-call key provider:
//! a settings change applies without a restart.

use std::time::Duration;

use serde::Deserialize;

use crate::RequestPriority;
use crate::degradation::{IntegrationStatus, record_current};
use crate::error::{MAX_RETRY_AFTER, PayloadKind, ProviderError};
use crate::executor::{Answer, Backoff, Executor, Request, Spec, Url, standard_classify};

/// Default AcoustID host. Tests point the client at a scripted fake instead.
pub const DEFAULT_BASE_URL: &str = "https://api.acoustid.org";
/// A best result under this score is not a confident match.
pub const MIN_SCORE: f64 = 0.70;
/// Metadata the lookup asks AcoustID to attach.
pub const LOOKUP_META: &str = "recordings releasegroups";
/// The provider name errors carry, and the AcoustID rate row.
pub const SOURCE: &str = "acoustid";
/// Largest AcoustID answer read. A batch of [`MAX_BATCH`] fingerprints
/// lists every release of every match, which can pass 4 MiB.
const MAX_BODY_BYTES: usize = 16 * 1024 * 1024;

/// AcoustID on the wire. A 4xx whose body names error code 4 is a bad key
/// ([`ProviderError::Unauthorized`]); any other 4xx stays a rejection. A
/// 200 whose `status` is not `ok` is [`ProviderError::Unavailable`]. A 429
/// that names no wait rests a minute ("the
/// historical 60s window"). The error body was recorded from the live
/// service on 2026-10-15 (`server/tests/fixtures/acoustid/`).
pub static SPEC: Spec = Spec {
    source: SOURCE,
    health_key: SOURCE,
    timeout: Duration::from_secs(15),
    max_body: MAX_BODY_BYTES,
    retry_after_default: Backoff::Fixed(Duration::from_secs(60)),
    retry_after_cap: MAX_RETRY_AFTER,
    classify,
};

/// The AcoustID hook over the standard rules.
pub fn classify(answer: &Answer<'_>) -> Result<(), ProviderError> {
    #[derive(Deserialize)]
    struct Status {
        #[serde(default)]
        status: Option<String>,
        #[serde(default)]
        error: Option<Refusal>,
    }
    #[derive(Deserialize)]
    struct Refusal {
        #[serde(default)]
        code: Option<i64>,
    }
    let status = answer.reply.status();
    let body = || serde_json::from_slice::<Status>(answer.reply.bytes()).ok();
    if (400..500).contains(&status)
        && status != 429
        && body().and_then(|body| body.error?.code) == Some(4)
    {
        return Err(ProviderError::Unauthorized { provider: SOURCE });
    }
    if status == 200
        && let Some(said) = body().and_then(|body| body.status)
        && said != "ok"
    {
        return Err(ProviderError::Unavailable {
            provider: SOURCE,
            retry_after: None,
        });
    }
    standard_classify(answer)
}

/// A confident fingerprint match keyed on a MusicBrainz recording id.
#[derive(Debug, Clone, PartialEq)]
pub struct FingerprintMatch {
    /// First valid recording MBID.
    pub recording_id: String,
    /// Every valid recording MBID, case-insensitively deduped.
    pub recording_ids: Vec<String>,
    /// AcoustID score of the best result (always `>= MIN_SCORE`).
    pub score: f64,
    /// Recording title, when the payload carried a non-blank one.
    pub title: Option<String>,
    /// Recording artists joined with `"; "`, when any were named.
    pub artist: Option<String>,
    /// Recording duration in seconds, when the payload carried a usable one.
    pub duration_secs: Option<i64>,
    /// Release-group MBIDs merged from the recording and result level,
    /// deduped with order preserved.
    pub release_group_ids: Vec<String>,
}

/// AcoustID lookup client over the AcoustID executor. Built once with the
/// providers; cheap to clone.
#[derive(Debug, Clone)]
pub struct AcoustIdClient {
    executor: Executor,
    base: Option<Url>,
}

impl AcoustIdClient {
    /// The client sending through `executor` to the API root at `base_url`.
    #[must_use]
    pub fn new(executor: Executor, base_url: &str) -> Self {
        let base = Url::parse(base_url.trim())
            .ok()
            .filter(|url| matches!(url.scheme(), "http" | "https") && !url.cannot_be_a_base());
        if base.is_none() {
            tracing::error!("the AcoustID address is not an http or https URL");
        }
        Self { executor, base }
    }

    /// Look one fingerprint up. `api_key` arrives per call, read fresh from
    /// settings on every lookup; an empty key means "not configured" and
    /// answers `None` without touching the wire.
    ///
    /// # Errors
    ///
    /// The typed failure of the call, or a payload failure for an answer
    /// that does not decode or a confident match with no recording id to
    /// key on (which must never read as "no match").
    pub async fn lookup(
        &self,
        api_key: &str,
        fingerprint: &str,
        duration_secs: u64,
        priority: RequestPriority,
    ) -> Result<Option<FingerprintMatch>, ProviderError> {
        if api_key.is_empty() {
            return Ok(None);
        }
        let form = vec![
            ("client".to_owned(), api_key.to_owned()),
            ("duration".to_owned(), duration_secs.to_string()),
            ("fingerprint".to_owned(), fingerprint.to_owned()),
            ("meta".to_owned(), LOOKUP_META.to_owned()),
        ];
        let reply = self
            .executor
            .send(self.lookup_request(form)?, priority)
            .await?;
        let payload: serde_json::Value = reply.json().inspect_err(|error| {
            note_shape(&error.to_string());
        })?;
        parse_lookup(&payload)
    }

    /// Look many fingerprints up, up to [`MAX_BATCH`] per request
    /// (`batch=1`, AcoustID's limit). Every recording above
    /// [`BATCH_MIN_SCORE`] is kept, with the releases AcoustID lists for
    /// it, keyed by the query's index. One fingerprint often maps to
    /// several duplicate MusicBrainz recordings (single and album
    /// versions), so keeping only the best would read as "unsupported"
    /// for the others (Lidarr keeps them all the same way). A request
    /// that fails contributes nothing; the other requests still count.
    /// An empty key never touches the wire.
    pub async fn lookup_batch(
        &self,
        api_key: &str,
        queries: &[BatchQuery<'_>],
        priority: RequestPriority,
    ) -> std::collections::HashMap<usize, BatchMatch> {
        let mut found = std::collections::HashMap::new();
        if api_key.is_empty() {
            return found;
        }
        for (chunk_index, chunk) in queries.chunks(MAX_BATCH).enumerate() {
            let offset = chunk_index * MAX_BATCH;
            let BatchOutcome::Answered(entries) =
                self.lookup_batch_request(api_key, chunk, priority).await
            else {
                continue;
            };
            for (index, results) in entries {
                if index < chunk.len() {
                    found.insert(offset + index, fold_batch(&results));
                }
            }
        }
        found
    }

    /// One batch request of at most [`MAX_BATCH`] prints (any past that
    /// are left out). An empty key is rejected without touching the wire.
    pub async fn lookup_batch_request(
        &self,
        api_key: &str,
        queries: &[BatchQuery<'_>],
        priority: RequestPriority,
    ) -> BatchOutcome {
        if api_key.is_empty() {
            return BatchOutcome::Rejected {
                message: "no AcoustID key".to_owned(),
            };
        }
        let queries = &queries[..queries.len().min(MAX_BATCH)];
        let mut form: Vec<(String, String)> = vec![
            ("client".to_owned(), api_key.to_owned()),
            ("batch".to_owned(), "1".to_owned()),
            ("meta".to_owned(), BATCH_META.to_owned()),
        ];
        for (index, query) in queries.iter().enumerate() {
            form.push((format!("duration.{index}"), query.duration_secs.to_string()));
            form.push((format!("fingerprint.{index}"), query.fingerprint.to_owned()));
        }
        let answer = match self.lookup_request(form) {
            Ok(request) => self.executor.send(request, priority).await,
            Err(error) => Err(error),
        };
        let answer = answer.and_then(|reply| {
            reply
                .json::<BatchAnswer>()
                .inspect_err(|error| note_shape(&error.to_string()))
        });
        match answer {
            Ok(answer) => BatchOutcome::Answered(
                answer
                    .fingerprints
                    .into_iter()
                    .filter_map(|entry| {
                        Some((entry.index.value()?, entry.results.unwrap_or_default()))
                    })
                    .collect(),
            ),
            Err(error) => BatchOutcome::from_error(&error),
        }
    }

    /// A lookup POST. It only reads, so sending it twice is safe.
    fn lookup_request(&self, form: Vec<(String, String)>) -> Result<Request, ProviderError> {
        let unusable = || ProviderError::Invalid {
            provider: SOURCE,
            reason: "the AcoustID address is not usable",
        };
        let base = self.base.as_ref().ok_or_else(unusable)?;
        Ok(Request::post_form(base, &["v2", "lookup"], form)
            .map_err(|_| unusable())?
            .idempotent(true))
    }
}

/// An answer without the expected shape: the service answered, so it is
/// noted for the request but never counted as an outage.
fn note_shape(cause: &str) {
    tracing::debug!(
        provider = SOURCE,
        cause,
        "AcoustID answer had no usable shape"
    );
    record_current(SOURCE, IntegrationStatus::Error, true);
}

/// A lookup answer that breaks its documented shape.
fn malformed() -> ProviderError {
    note_shape("malformed AcoustID answer");
    ProviderError::Payload {
        provider: SOURCE,
        kind: PayloadKind::Decode,
        message: "malformed AcoustID answer".to_owned(),
    }
}

/// Fold a lookup payload into a match.
fn parse_lookup(payload: &serde_json::Value) -> Result<Option<FingerprintMatch>, ProviderError> {
    let Some(body) = payload.as_object() else {
        return Err(malformed());
    };
    if body.get("status").and_then(serde_json::Value::as_str) != Some("ok") {
        return Err(malformed());
    }
    // A missing or null `results` reads as "no results" (v2); only a
    // non-list value is malformed.
    let results = match body.get("results") {
        None | Some(serde_json::Value::Null) => return Ok(None),
        Some(serde_json::Value::Array(results)) => results,
        Some(_) => return Err(malformed()),
    };
    let Some(best) = results.first() else {
        return Ok(None);
    };
    let Some(best) = best.as_object() else {
        return Err(malformed());
    };
    // A missing score reads as 0.0 and falls below the threshold (v2);
    // only a present-but-unusable score is malformed. JSON booleans must
    // not pass as numbers.
    let score = match best.get("score") {
        None | Some(serde_json::Value::Null) => 0.0,
        Some(serde_json::Value::Number(number)) => match number.as_f64() {
            Some(score) if score.is_finite() => score,
            _ => return Err(malformed()),
        },
        Some(_) => return Err(malformed()),
    };
    if score < MIN_SCORE {
        return Ok(None);
    }
    let recordings = match best.get("recordings") {
        None | Some(serde_json::Value::Null) => &Vec::new(),
        Some(serde_json::Value::Array(recordings)) => recordings,
        Some(_) => return Err(malformed()),
    };
    let mut recording_ids: Vec<String> = Vec::new();
    let mut seen: Vec<String> = Vec::new();
    let mut selected: Option<&serde_json::Map<String, serde_json::Value>> = None;
    for recording in recordings.iter().filter_map(serde_json::Value::as_object) {
        let Some(id) = recording.get("id").and_then(serde_json::Value::as_str) else {
            continue;
        };
        let id = id.trim().to_owned();
        if id.is_empty() {
            continue;
        }
        // v2 dedupes recording ids case-insensitively but keeps the first
        // spelling it saw.
        let folded = id.to_lowercase();
        if seen.contains(&folded) {
            continue;
        }
        seen.push(folded);
        recording_ids.push(id);
        selected.get_or_insert(recording);
    }
    // A confident audio match with nothing to key the row on (v2 FAIL):
    // never "no match", and v2 queues it for manual review.
    let (Some(recording), Some(recording_id)) = (selected, recording_ids.first().cloned()) else {
        return Err(ProviderError::Payload {
            provider: SOURCE,
            kind: PayloadKind::Contract,
            message: "AcoustID matched audio with no recording id".to_owned(),
        });
    };
    let title = recording
        .get("title")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|title| !title.is_empty())
        .map(str::to_owned);
    Ok(Some(FingerprintMatch {
        recording_id,
        recording_ids,
        score,
        title,
        artist: joined_artist_names(recording),
        duration_secs: recording_duration_secs(recording),
        release_group_ids: release_group_ids(recording, best),
    }))
}

/// Fingerprints per batch request: the AcoustID server's limit.
pub const MAX_BATCH: usize = 20;
/// Batch results keep every recording scoring above this (Lidarr).
pub const BATCH_MIN_SCORE: f64 = 0.5;
/// Batch lookups ask for recording and release ids only: small answers,
/// and the release ids nominate candidates when the tags found none.
pub const BATCH_META: &str = "recordingids releaseids";

/// One fingerprint to look up.
#[derive(Debug, Clone, Copy)]
pub struct BatchQuery<'a> {
    pub fingerprint: &'a str,
    pub duration_secs: u64,
}

/// What AcoustID heard in one fingerprint.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct BatchMatch {
    /// Recording MBIDs, lowercase, deduplicated, best result first.
    pub recording_ids: Vec<String>,
    /// Release MBIDs those recordings appear on, lowercase, deduplicated.
    pub release_ids: Vec<String>,
}

/// One batch request's outcome.
#[derive(Debug, Clone, PartialEq)]
pub enum BatchOutcome {
    /// AcoustID answered: each query index of the request with its
    /// results, in AcoustID's order.
    Answered(Vec<(usize, Vec<BatchResult>)>),
    /// A 4xx other than 429, such as a bad key: asking again gets the
    /// same answer.
    Rejected { message: String },
    /// Transport failure, 429 (with its `Retry-After`), 5xx, or a body
    /// that did not decode.
    Unavailable {
        retry_after_secs: Option<f64>,
        message: String,
    },
}

impl BatchOutcome {
    /// What a failed request means for the batch: a rejection asking
    /// again cannot change, or an outage worth asking again later.
    fn from_error(error: &ProviderError) -> Self {
        let message = error.to_string();
        match error {
            ProviderError::Unauthorized { .. }
            | ProviderError::Forbidden { .. }
            | ProviderError::NotFound { .. }
            | ProviderError::BadRequest { .. }
            | ProviderError::Redirect { .. }
            | ProviderError::NotConfigured { .. }
            | ProviderError::Invalid { .. } => Self::Rejected { message },
            _ => Self::Unavailable {
                retry_after_secs: error.retry_after().map(|wait| wait.as_secs_f64()),
                message,
            },
        }
    }
}

// The batch answer's shape, verified 2026-10-15 against a live
// `/v2/lookup` (format=json, batch=1, meta="recordingids releaseids"),
// recorded in `server/tests/fixtures/acoustid/`: `{"status": "ok",
// "fingerprints": [{"index": 0, "results": [{"id", "score",
// "recordings": [{"id", "releases": [{"id"}]}]}]}]}`. A bad key answers
// HTTP 400 with `{"status": "error", "error": {"code": 4, "message"}}`.

/// A batch answer.
#[derive(Debug, Clone, Deserialize)]
pub struct BatchAnswer {
    pub status: String,
    #[serde(default)]
    pub fingerprints: Vec<BatchEntry>,
}

/// The results for one query of a batch.
#[derive(Debug, Clone, Deserialize)]
pub struct BatchEntry {
    pub index: BatchIndex,
    #[serde(default)]
    pub results: Option<Vec<BatchResult>>,
}

/// The query index, which AcoustID echoes as a number or a numeric
/// string (seen in v2's answers; the 2026-10-15 probe had a number).
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum BatchIndex {
    Number(u64),
    Text(String),
}

impl BatchIndex {
    fn value(&self) -> Option<usize> {
        match self {
            BatchIndex::Number(number) => usize::try_from(*number).ok(),
            BatchIndex::Text(text) => text.trim().parse().ok(),
        }
    }
}

/// One AcoustID track that matched a print.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct BatchResult {
    /// AcoustID's own id for the track.
    pub id: String,
    #[serde(default)]
    pub score: Option<f64>,
    #[serde(default)]
    pub recordings: Option<Vec<BatchRecording>>,
}

impl BatchResult {
    /// The score, 0 when AcoustID gave none.
    pub fn score(&self) -> f64 {
        self.score.unwrap_or(0.0)
    }

    /// The MusicBrainz recordings AcoustID links to the track.
    pub fn recordings(&self) -> &[BatchRecording] {
        self.recordings.as_deref().unwrap_or_default()
    }
}

/// A MusicBrainz recording linked to an AcoustID track.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct BatchRecording {
    pub id: String,
    #[serde(default)]
    pub releases: Option<Vec<BatchRelease>>,
}

/// A MusicBrainz release a recording appears on.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct BatchRelease {
    pub id: String,
}

/// Every recording and release of the results above [`BATCH_MIN_SCORE`].
fn fold_batch(results: &[BatchResult]) -> BatchMatch {
    let mut matched = BatchMatch::default();
    for result in results
        .iter()
        .filter(|result| result.score() > BATCH_MIN_SCORE)
    {
        for recording in result.recordings() {
            push_id(&mut matched.recording_ids, &recording.id);
            for release in recording.releases.as_deref().unwrap_or_default() {
                push_id(&mut matched.release_ids, &release.id);
            }
        }
    }
    matched
}

fn push_id(ids: &mut Vec<String>, id: &str) {
    let id = id.trim().to_ascii_lowercase();
    if !id.is_empty() && !ids.contains(&id) {
        ids.push(id);
    }
}

/// Join the recording's artist names with `"; "`, skipping blanks (v2).
fn joined_artist_names(recording: &serde_json::Map<String, serde_json::Value>) -> Option<String> {
    let artists = recording.get("artists")?.as_array()?;
    let mut names: Vec<&str> = Vec::new();
    for entry in artists {
        let name = entry
            .as_object()
            .and_then(|artist| artist.get("name"))
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .unwrap_or("");
        if !name.is_empty() {
            names.push(name);
        }
    }
    if names.is_empty() {
        None
    } else {
        Some(names.join("; "))
    }
}

/// Read the recording duration: integers pass through, finite integral
/// floats truncate, booleans and everything else read as absent (v2).
fn recording_duration_secs(recording: &serde_json::Map<String, serde_json::Value>) -> Option<i64> {
    match recording.get("duration") {
        Some(serde_json::Value::Number(number)) => {
            if let Some(int) = number.as_i64() {
                Some(int)
            } else if let Some(float) = number.as_f64() {
                if float.is_finite() && float.fract() == 0.0 {
                    Some(float as i64)
                } else {
                    None
                }
            } else {
                None
            }
        }
        _ => None,
    }
}

/// Merge release-group MBIDs from the recording level and the result level,
/// deduped with order preserved (used by
/// the download-verify release-group check). Unlike recording ids, these
/// compare case-sensitively, exactly as v2 does.
fn release_group_ids(
    recording: &serde_json::Map<String, serde_json::Value>,
    best: &serde_json::Map<String, serde_json::Value>,
) -> Vec<String> {
    let mut ids: Vec<String> = Vec::new();
    for source in [recording.get("releasegroups"), best.get("releasegroups")] {
        let groups = match source.and_then(serde_json::Value::as_array) {
            Some(groups) => groups,
            None => continue,
        };
        for group in groups {
            let id = group
                .as_object()
                .and_then(|group| group.get("id"))
                .and_then(serde_json::Value::as_str)
                .map(str::trim)
                .unwrap_or("");
            if id.is_empty() || ids.iter().any(|known| known == id) {
                continue;
            }
            ids.push(id.to_owned());
        }
    }
    ids
}

/// What `fpcalc` produced for one file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FpCalcOutput {
    /// Compressed (base64) Chromaprint fingerprint, ready for `/v2/lookup`.
    pub fingerprint: String,
    /// Audio duration in whole seconds.
    pub duration_secs: i64,
    /// True when fpcalc exited non-zero yet still emitted a fingerprint.
    /// v2 carries this flag so confident matches from a partial
    /// decode can be corroborated downstream.
    pub partial_decode: bool,
    /// fpcalc's stderr, preserved so a changed or unexpected error stays
    /// visible (v2 logs it alongside the tolerated exit).
    pub stderr: String,
}

/// Why `fpcalc` output was unusable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FpCalcError {
    /// No `FINGERPRINT=` line was emitted.
    MissingFingerprint(String),
    /// No usable positive `DURATION=` line was emitted. A zero duration
    /// would make AcoustID return an empty result set, which is
    /// indistinguishable from a genuine no-match, so v2 surfaces it as an
    /// error instead of a silent skip.
    BadDuration(String),
}

/// Parse `fpcalc` output. `exit_success` is fpcalc's exit status and
/// `stderr` its error text.
///
/// Two quirks matter here. First, fpcalc exits non-zero ("Error decoding
/// audio frame (End of file)") on tracks shorter than its 120s window, but
/// still writes a valid `FINGERPRINT=` line; a non-zero exit only fails when
/// no fingerprint was produced, otherwise the fingerprint is used and the
/// result is flagged partial. Second, this parser expects the plain (not
/// `-raw`) compressed fingerprint: AcoustID's `/v2/lookup` rejects the raw
/// comma-separated integers with HTTP 400, so the spawner must never pass
/// `-raw` (v2 calls this out after every lookup silently failed).
pub fn parse_fpcalc_output(
    stdout: &str,
    exit_success: bool,
    stderr: &str,
) -> Result<FpCalcOutput, FpCalcError> {
    let mut duration_secs: Option<i64> = None;
    let mut fingerprint: Option<String> = None;
    for line in stdout.split('\n') {
        if let Some(value) = line.strip_prefix("DURATION=") {
            // v2 parses `int(float(...))`, so fractional input truncates.
            if let Ok(seconds) = value.trim().parse::<f64>() {
                duration_secs = Some(seconds as i64);
            }
        } else if let Some(value) = line.strip_prefix("FINGERPRINT=") {
            fingerprint = Some(value.to_owned());
        }
    }
    let fingerprint = match fingerprint.filter(|print| !print.is_empty()) {
        Some(fingerprint) => fingerprint,
        None => {
            return Err(FpCalcError::MissingFingerprint(stderr.trim().to_owned()));
        }
    };
    match duration_secs.filter(|seconds| *seconds > 0) {
        Some(duration_secs) => Ok(FpCalcOutput {
            fingerprint,
            duration_secs,
            partial_decode: !exit_success,
            stderr: stderr.trim().to_owned(),
        }),
        None => Err(FpCalcError::BadDuration(stderr.trim().to_owned())),
    }
}
