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

use crate::RequestPriority;
use crate::degradation::{IntegrationStatus, record_current};
use crate::error::{MAX_RETRY_AFTER, PayloadKind, ProviderError};
use crate::executor::{Answer, Backoff, Executor, Request, Spec, Url, standard_classify};

mod models;

use models::{ArtistCredit, Envelope, LookupAnswer, LookupRecording, LookupResult};
pub use models::{BatchAnswer, BatchEntry, BatchIndex, BatchRecording, BatchRelease, BatchResult};

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
/// ([`ProviderError::Unauthorized`], and the only way to get one). Code 14,
/// too many requests, is a 429 and rests the client the same way. Any
/// other 4xx that names an AcoustID error code passes through, so the
/// batch request can read the code; one that names none is
/// [`ProviderError::BadRequest`], a bare 401 included. A 200 whose
/// `status` is not `ok` is [`ProviderError::Unavailable`]. A 429 that
/// names no wait rests a minute ("the historical 60s window"). The error
/// body was seen on 2026-10-15 per `server/tests/fixtures/acoustid/README.md`.
pub static SPEC: Spec = Spec {
    source: SOURCE,
    health_key: SOURCE,
    timeout: Duration::from_secs(15),
    max_body: MAX_BODY_BYTES,
    retry_after_default: Backoff::Fixed(Duration::from_secs(60)),
    retry_after_cap: MAX_RETRY_AFTER,
    classify,
};

/// AcoustID's error code for a client over its request rate.
const TOO_MANY_REQUESTS: i64 = 14;

/// The AcoustID hook over the standard rules.
pub fn classify(answer: &Answer<'_>) -> Result<(), ProviderError> {
    let status = answer.reply.status();
    let decoded = || answer.reply.json::<Envelope>().ok();
    if (400..500).contains(&status) && status != 429 {
        return match decoded().and_then(|envelope| envelope.error?.code) {
            Some(4) => Err(ProviderError::Unauthorized { provider: SOURCE }),
            Some(TOO_MANY_REQUESTS) => Err(ProviderError::RateLimited {
                provider: SOURCE,
                retry_after: answer.retry_after(),
            }),
            Some(_) => Ok(()),
            None => Err(ProviderError::BadRequest {
                provider: SOURCE,
                status,
            }),
        };
    }
    if status == 200
        && let Some(said) = decoded().and_then(|envelope| envelope.status)
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
        if !(200..300).contains(&reply.status()) {
            return Err(ProviderError::BadRequest {
                provider: SOURCE,
                status: reply.status(),
            });
        }
        let answer: LookupAnswer = reply.json().inspect_err(|_| note_shape())?;
        parse_lookup(answer)
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
    /// are left out). An empty key never touches the wire.
    pub async fn lookup_batch_request(
        &self,
        api_key: &str,
        queries: &[BatchQuery<'_>],
        priority: RequestPriority,
    ) -> BatchOutcome {
        if api_key.is_empty() {
            return BatchOutcome::NoKey;
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
        let reply = match self.lookup_request(form) {
            Ok(request) => self.executor.send(request, priority).await,
            Err(error) => Err(error),
        };
        let reply = match reply {
            Ok(reply) => reply,
            Err(error) => return BatchOutcome::from_error(&error),
        };
        // A 4xx naming an AcoustID error code other than a refused key:
        // the request was not served, and asking later may be.
        if !(200..300).contains(&reply.status()) {
            let code = reply
                .json::<Envelope>()
                .ok()
                .and_then(|envelope| envelope.error?.code);
            tracing::info!(
                provider = SOURCE,
                status = reply.status(),
                code,
                "AcoustID did not serve the lookup"
            );
            return BatchOutcome::Unavailable(code.map_or(Unanswered::Failed, Unanswered::Code));
        }
        match reply.json::<BatchAnswer>().inspect_err(|_| note_shape()) {
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
/// noted for the request but never counted as an outage. The decoder's
/// account is not logged: it can quote the body.
fn note_shape() {
    tracing::debug!(provider = SOURCE, "AcoustID answer had no usable shape");
    record_current(SOURCE, IntegrationStatus::Error, true);
}

/// A lookup answer that breaks its documented shape.
fn malformed() -> ProviderError {
    note_shape();
    ProviderError::Payload {
        provider: SOURCE,
        kind: PayloadKind::Decode,
        message: "malformed AcoustID answer".to_owned(),
    }
}

/// Fold a lookup answer into a match.
fn parse_lookup(answer: LookupAnswer) -> Result<Option<FingerprintMatch>, ProviderError> {
    if answer.status != "ok" {
        return Err(malformed());
    }
    // A missing or null `results` reads as no match (v2); any other type
    // fails decoding.
    let Some(best) = answer
        .results
        .and_then(|results| results.into_iter().next())
    else {
        return Ok(None);
    };
    // A missing score reads as 0.0 and falls below the threshold (v2).
    let score = best.score.unwrap_or(0.0);
    if score < MIN_SCORE {
        return Ok(None);
    }
    let recordings = best.recordings.as_deref().unwrap_or_default();
    // v2 dedupes recording ids case-insensitively but keeps the first
    // spelling it saw.
    let mut recording_ids: Vec<String> = Vec::new();
    let mut folded_seen: Vec<String> = Vec::new();
    for recording in recordings {
        let folded = recording.id.to_lowercase();
        if !folded_seen.contains(&folded) {
            folded_seen.push(folded);
            recording_ids.push(recording.id.clone());
        }
    }
    // A confident audio match with nothing to key the row on (v2 FAIL):
    // never "no match", and v2 queues it for manual review.
    let Some(first) = recordings.first() else {
        return Err(ProviderError::Payload {
            provider: SOURCE,
            kind: PayloadKind::Contract,
            message: "AcoustID matched audio with no recording id".to_owned(),
        });
    };
    let title = first
        .title
        .as_deref()
        .map(str::trim)
        .filter(|title| !title.is_empty())
        .map(str::to_owned);
    Ok(Some(FingerprintMatch {
        recording_id: first.id.clone(),
        recording_ids,
        score,
        title,
        artist: first.artists.as_deref().and_then(joined_artist_names),
        duration_secs: first.duration.and_then(whole_seconds),
        release_group_ids: release_group_ids(first, &best),
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
    /// results, in AcoustID's order. An index it left out is not a "no
    /// match".
    Answered(Vec<(usize, Vec<BatchResult>)>),
    /// No key was given, so nothing was sent.
    NoKey,
    /// AcoustID refused the key (its error code 4).
    KeyRefused,
    /// No usable answer now; asking later may get one.
    Unavailable(Unanswered),
}

/// Why a batch request got no usable answer. The executor already rests
/// AcoustID for any wait it asked for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unanswered {
    /// A 429, or the rate limiter held the request back.
    Paused,
    /// AcoustID named an error code other than a refused key.
    Code(i64),
    /// Anything else: no answer, a redirect, a 4xx without a code, a 5xx,
    /// or an answer that did not decode.
    Failed,
}

impl BatchOutcome {
    /// What a failed request means: only a refused key is final; anything
    /// else waits. Logged by HTTP status alone, never the error's text.
    fn from_error(error: &ProviderError) -> Self {
        let status = match error {
            ProviderError::Forbidden { .. } => Some(403),
            ProviderError::NotFound { .. } => Some(404),
            ProviderError::RateLimited { .. } => Some(429),
            ProviderError::BadRequest { status, .. }
            | ProviderError::Redirect { status, .. }
            | ProviderError::Server { status, .. } => Some(*status),
            _ => None,
        };
        let outcome = match error {
            ProviderError::Unauthorized { .. } => return Self::KeyRefused,
            ProviderError::RateLimited { .. } => Self::Unavailable(Unanswered::Paused),
            _ => Self::Unavailable(Unanswered::Failed),
        };
        tracing::info!(provider = SOURCE, status, "AcoustID lookup not answered");
        outcome
    }
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

/// Join the credited names with `"; "`, skipping blanks (v2).
fn joined_artist_names(credits: &[ArtistCredit]) -> Option<String> {
    let names: Vec<&str> = credits
        .iter()
        .filter_map(|credit| credit.name.as_deref())
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .collect();
    (!names.is_empty()).then(|| names.join("; "))
}

/// A recording length as whole seconds. A fraction reads as absent (v2).
fn whole_seconds(seconds: f64) -> Option<i64> {
    (seconds.is_finite() && seconds.fract() == 0.0).then_some(seconds as i64)
}

/// Release-group MBIDs from the recording and from the result, deduped with
/// order preserved (used by the download-verify release-group check). Unlike
/// recording ids, these compare case-sensitively, exactly as v2 does.
fn release_group_ids(recording: &LookupRecording, best: &LookupResult) -> Vec<String> {
    let mut ids: Vec<String> = Vec::new();
    let groups = recording
        .releasegroups
        .iter()
        .chain(best.releasegroups.iter())
        .flatten();
    for group in groups {
        if !ids.contains(&group.id) {
            ids.push(group.id.clone());
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
