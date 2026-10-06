//! AcoustID fingerprint-lookup client (Tier-3 identification).
//!
//! Ports v2's audio fingerprinter: a lookup call posts
//! the compressed Chromaprint fingerprint to the AcoustID web service
//! (see <https://acoustid.org/webservice>) and the response is folded into a
//! recording match. Everything here fails soft: a dead or confused upstream
//! records one degradation note and yields [`Outcome::Unavailable`], while a
//! clean "no match" yields [`Outcome::Missing`]. Library identification uses
//! [`AcoustIdClient::lookup_batch`], which sends up to twenty fingerprints
//! per request.
//!
//! Pacing and degradation ride the shared core traits
//! ([`Pacer`](super::limiter::Pacer), [`DegradationSink`](super::degradation::DegradationSink)):
//! wire the pacer to a 3/second bucket ([`RATE_PER_SEC`] / [`BURST`], per v2
//! `service_providers.get_audio_fingerprinter`).
//!
//! The API key is passed to every [`AcoustIdClient::lookup`] call and never
//! stored on the client, mirroring v2's per-call key provider: a settings
//! change applies without a restart.

use std::time::Duration;

use super::{DegradationSink, Pacer};

/// Default AcoustID host. Tests point the client at a scripted fake instead.
pub const DEFAULT_BASE_URL: &str = "https://api.acoustid.org";
/// Lookup path below the base URL (v2 `AudioFingerprinter.ACOUSTID_API`).
pub const LOOKUP_PATH: &str = "/v2/lookup";
/// Pacing the wiring must configure: 3 lookups/second (v2, same source).
pub const RATE_PER_SEC: f64 = 3.0;
/// Bucket burst for the 3/second limiter (v2 `capacity=3`).
pub const BURST: u32 = 3;
/// A best result under this score is not a confident match
/// (v2 `_ACOUSTID_MIN_SCORE`).
pub const MIN_SCORE: f64 = 0.70;
/// Metadata the lookup asks AcoustID to attach
/// (v2 `meta="recordings releasegroups"`).
pub const LOOKUP_META: &str = "recordings releasegroups";
/// Fallback wait when a 429 carries no usable `Retry-After` header
/// (v2 `_retry_after_seconds`, "the historical 60s window").
pub const DEFAULT_RETRY_AFTER_SECS: f64 = 60.0;
/// Per-request wire timeout (v2 `HttpClientFactory` acoustid timeout).
pub const REQUEST_TIMEOUT_SECS: u64 = 15;
/// Source name used for degradation records.
pub const SOURCE: &str = "acoustid";

/// What a lookup produced. `Found` carries a usable match, `Missing` is an
/// authoritative "no match" (safe to treat as a negative), and `Unavailable`
/// covers everything else: transport failure, rate limiting, rejection, a
/// malformed payload, or a confident match with no recording id to key on
/// (v2 `FAIL`, which must never read as a negative).
#[derive(Debug, Clone, PartialEq)]
pub enum Outcome<T> {
    /// A usable match.
    Found(T),
    /// Authoritative no-match. Never recorded as degradation.
    Missing,
    /// The source could not answer.
    Unavailable {
        /// Seconds the caller should wait before retrying, when known.
        retry_after_secs: Option<f64>,
        /// Short human-readable reason (never carries secrets).
        message: String,
        /// Whether the sink already holds a record for this failure.
        /// Transport failure, rate limiting, server errors, and malformed
        /// payloads record; deterministic rejection and the confident-but-
        /// unkeyable match do not, because v2 raises or queues those instead
        /// of recording them.
        recorded: bool,
    },
}

impl<T> Outcome<T> {
    /// Collapse to the fail-soft `Option` shape: only `Found` is `Some`.
    pub fn into_option(self) -> Option<T> {
        match self {
            Outcome::Found(value) => Some(value),
            Outcome::Missing | Outcome::Unavailable { .. } => None,
        }
    }
}

/// A confident fingerprint match keyed on a MusicBrainz recording id.
#[derive(Debug, Clone, PartialEq)]
pub struct FingerprintMatch {
    /// First valid recording MBID (v2 `recording_id`).
    pub recording_id: String,
    /// Every valid recording MBID, case-insensitively deduped (v2
    /// `recording_ids`).
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
    /// deduped with order preserved (v2 `_extract_release_group_ids`).
    pub release_group_ids: Vec<String>,
}

/// AcoustID lookup client. Stateless apart from its ports; cheap to clone.
#[derive(Debug, Clone)]
pub struct AcoustIdClient<P, S> {
    http: reqwest::Client,
    base_url: String,
    pacer: P,
    sink: S,
}

impl<P: Pacer, S: DegradationSink> AcoustIdClient<P, S> {
    /// Build a client against `base_url` (the production host or a fake).
    pub fn new(http: reqwest::Client, base_url: &str, pacer: P, sink: S) -> Self {
        Self {
            http,
            base_url: base_url.trim_end_matches('/').to_owned(),
            pacer,
            sink,
        }
    }

    /// Look one fingerprint up. `api_key` arrives per call (v2 reads it fresh
    /// from settings on every lookup); an empty key means "not configured"
    /// and short-circuits to `Missing` without touching the wire, exactly
    /// like v2's `DISABLED` result.
    pub async fn lookup(
        &self,
        api_key: &str,
        fingerprint: &str,
        duration_secs: u64,
    ) -> Outcome<FingerprintMatch> {
        if api_key.is_empty() {
            return Outcome::Missing;
        }
        self.pacer.acquire().await;
        let url = format!("{base}{LOOKUP_PATH}", base = self.base_url);
        let response = match self
            .http
            .post(url)
            .form(&[
                ("client", api_key),
                ("duration", &duration_secs.to_string()),
                ("fingerprint", fingerprint),
                ("meta", LOOKUP_META),
            ])
            .timeout(Duration::from_secs(REQUEST_TIMEOUT_SECS))
            .send()
            .await
        {
            Ok(response) => response,
            Err(error) => {
                return self.recorded(
                    None,
                    format!("AcoustID lookup failed during transport: {error}"),
                );
            }
        };
        let status = response.status().as_u16();
        if status == 429 {
            // v2 honors Retry-After and otherwise falls back to 60s.
            let retry_after = retry_after_secs(response.headers().get("retry-after"));
            return self.recorded(Some(retry_after), "AcoustID rate limit exceeded".to_owned());
        }
        if status >= 500 {
            return self.recorded(None, format!("AcoustID API error ({status})"));
        }
        if status != 200 {
            // v2 `AcoustIDRejectedError`: deterministic 4xx rejection, never
            // retried and never counted toward the breaker.
            return self.unrecorded(format!("AcoustID rejected the lookup ({status})"));
        }
        let payload: serde_json::Value = match response.text().await {
            Ok(text) => match serde_json::from_str(&text) {
                Ok(payload) => payload,
                Err(_) => {
                    return self.recorded(None, "malformed AcoustID response".to_owned());
                }
            },
            Err(error) => {
                return self.recorded(None, format!("AcoustID response body unreadable: {error}"));
            }
        };
        self.parse_lookup(payload)
    }

    /// Look many fingerprints up, up to [`MAX_BATCH`] per request
    /// (`batch=1`, AcoustID's limit). Every recording above
    /// [`BATCH_MIN_SCORE`] is kept, with the releases AcoustID lists for
    /// it, keyed by the query's index. One fingerprint often maps to
    /// several duplicate MusicBrainz recordings (single and album
    /// versions), so keeping only the best would read as "unsupported"
    /// for the others (Lidarr keeps them all the same way). A request
    /// that fails records a degradation and contributes nothing; the
    /// other requests still count. An empty key never touches the wire.
    pub async fn lookup_batch(
        &self,
        api_key: &str,
        queries: &[BatchQuery<'_>],
    ) -> std::collections::HashMap<usize, BatchMatch> {
        let mut found = std::collections::HashMap::new();
        if api_key.is_empty() {
            return found;
        }
        for (chunk_index, chunk) in queries.chunks(MAX_BATCH).enumerate() {
            let offset = chunk_index * MAX_BATCH;
            let mut form: Vec<(String, String)> = vec![
                ("client".to_owned(), api_key.to_owned()),
                ("batch".to_owned(), "1".to_owned()),
                ("meta".to_owned(), BATCH_META.to_owned()),
            ];
            for (index, query) in chunk.iter().enumerate() {
                form.push((format!("duration.{index}"), query.duration_secs.to_string()));
                form.push((format!("fingerprint.{index}"), query.fingerprint.to_owned()));
            }
            self.pacer.acquire().await;
            let url = format!("{base}{LOOKUP_PATH}", base = self.base_url);
            let response = match self
                .http
                .post(url)
                .form(&form)
                .timeout(Duration::from_secs(REQUEST_TIMEOUT_SECS))
                .send()
                .await
            {
                Ok(response) => response,
                Err(error) => {
                    self.sink.record(
                        SOURCE,
                        format!("AcoustID batch lookup failed during transport: {error}"),
                    );
                    continue;
                }
            };
            let status = response.status().as_u16();
            if status != 200 {
                self.sink
                    .record(SOURCE, format!("AcoustID batch lookup answered {status}"));
                continue;
            }
            let body = response.text().await.unwrap_or_default();
            let payload = match serde_json::from_str::<serde_json::Value>(&body) {
                Ok(payload) => payload,
                Err(_) => {
                    self.sink
                        .record(SOURCE, "malformed AcoustID batch response".to_owned());
                    continue;
                }
            };
            if payload.get("status").and_then(serde_json::Value::as_str) != Some("ok") {
                self.sink
                    .record(SOURCE, "AcoustID batch lookup did not succeed".to_owned());
                continue;
            }
            for (index, matched) in parse_batch(&payload) {
                if index < chunk.len() {
                    found.insert(offset + index, matched);
                }
            }
        }
        found
    }

    /// Fold a lookup payload into an outcome, following v2 `_parse_response`
    /// branch for branch.
    fn parse_lookup(&self, payload: serde_json::Value) -> Outcome<FingerprintMatch> {
        let malformed = || "malformed AcoustID response".to_owned();
        let body = match payload.as_object() {
            Some(body) => body,
            None => return self.recorded(None, malformed()),
        };
        match body.get("status").and_then(serde_json::Value::as_str) {
            Some("ok") => {}
            Some(other) => return self.recorded(None, other.to_owned()),
            None => return self.recorded(None, malformed()),
        }
        // A missing or null `results` reads as "no results" (v2); only a
        // non-list value is malformed.
        let results = match body.get("results") {
            None | Some(serde_json::Value::Null) => Vec::new(),
            Some(serde_json::Value::Array(results)) => results.clone(),
            Some(_) => return self.recorded(None, malformed()),
        };
        if results.is_empty() {
            return Outcome::Missing;
        }
        let best = match results.first().and_then(serde_json::Value::as_object) {
            Some(best) => best,
            None => return self.recorded(None, malformed()),
        };
        // A missing score reads as 0.0 and falls below the threshold (v2);
        // only a present-but-unusable score is malformed. JSON booleans must
        // not pass as numbers.
        let score = match best.get("score") {
            None | Some(serde_json::Value::Null) => 0.0,
            Some(serde_json::Value::Number(number)) => match number.as_f64() {
                Some(score) if score.is_finite() => score,
                _ => return self.recorded(None, malformed()),
            },
            Some(_) => return self.recorded(None, malformed()),
        };
        if score < MIN_SCORE {
            return Outcome::Missing;
        }
        let recordings = match best.get("recordings") {
            None | Some(serde_json::Value::Null) => Vec::new(),
            Some(serde_json::Value::Array(recordings)) => recordings.clone(),
            Some(_) => return self.recorded(None, malformed()),
        };
        let mut recording_ids: Vec<String> = Vec::new();
        let mut seen: Vec<String> = Vec::new();
        let mut selected: Option<&serde_json::Map<String, serde_json::Value>> = None;
        for recording in &recordings {
            let recording = match recording.as_object() {
                Some(recording) => recording,
                None => continue,
            };
            let id = match recording.get("id").and_then(serde_json::Value::as_str) {
                Some(id) => id.trim().to_owned(),
                None => continue,
            };
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
            if selected.is_none() {
                selected = Some(recording);
            }
        }
        let recording = match selected {
            Some(recording) => recording,
            // Confident audio match, but nothing to key the row on (v2 FAIL).
            // This is `Unavailable`, never `Missing`: it must not read as a
            // negative, and v2 queues it for manual review.
            None => {
                return self.unrecorded("AcoustID matched audio with no recording id".to_owned());
            }
        };
        let artist = joined_artist_names(recording);
        let title = recording
            .get("title")
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|title| !title.is_empty())
            .map(str::to_owned);
        // `selected` is only set alongside a push, so a first id exists.
        let recording_id = recording_ids.first().cloned().unwrap_or_default();
        Outcome::Found(FingerprintMatch {
            recording_id,
            recording_ids,
            score,
            title,
            artist,
            duration_secs: recording_duration_secs(recording),
            release_group_ids: release_group_ids(recording, best),
        })
    }

    /// Build a recorded `Unavailable` outcome.
    fn recorded(
        &self,
        retry_after_secs: Option<f64>,
        message: String,
    ) -> Outcome<FingerprintMatch> {
        self.sink.record(SOURCE, message.clone());
        Outcome::Unavailable {
            retry_after_secs,
            message,
            recorded: true,
        }
    }

    /// Build an unrecorded `Unavailable` outcome for deterministic
    /// rejections and unkeyable matches, which v2 raises or queues rather
    /// than records.
    fn unrecorded(&self, message: String) -> Outcome<FingerprintMatch> {
        Outcome::Unavailable {
            retry_after_secs: None,
            message,
            recorded: false,
        }
    }
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

/// Read `fingerprints[].{index, results}`; unknown shapes are skipped.
fn parse_batch(payload: &serde_json::Value) -> Vec<(usize, BatchMatch)> {
    let Some(fingerprints) = payload
        .get("fingerprints")
        .and_then(serde_json::Value::as_array)
    else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for entry in fingerprints {
        let Some(index) = entry.get("index").and_then(index_of) else {
            continue;
        };
        let mut matched = BatchMatch::default();
        let results = entry
            .get("results")
            .and_then(serde_json::Value::as_array)
            .map(Vec::as_slice)
            .unwrap_or_default();
        for result in results {
            let score = result
                .get("score")
                .and_then(serde_json::Value::as_f64)
                .unwrap_or(0.0);
            if score <= BATCH_MIN_SCORE {
                continue;
            }
            let recordings = result
                .get("recordings")
                .and_then(serde_json::Value::as_array)
                .map(Vec::as_slice)
                .unwrap_or_default();
            for recording in recordings {
                push_id(&mut matched.recording_ids, recording.get("id"));
                for release in recording
                    .get("releases")
                    .and_then(serde_json::Value::as_array)
                    .map(Vec::as_slice)
                    .unwrap_or_default()
                {
                    push_id(&mut matched.release_ids, release.get("id"));
                }
            }
        }
        out.push((index, matched));
    }
    out
}

/// AcoustID echoes the index as a number or a numeric string.
fn index_of(value: &serde_json::Value) -> Option<usize> {
    match value {
        serde_json::Value::Number(number) => number.as_u64().map(|index| index as usize),
        serde_json::Value::String(text) => text.trim().parse().ok(),
        _ => None,
    }
}

fn push_id(ids: &mut Vec<String>, value: Option<&serde_json::Value>) {
    let Some(id) = value.and_then(serde_json::Value::as_str) else {
        return;
    };
    let id = id.trim().to_ascii_lowercase();
    if !id.is_empty() && !ids.contains(&id) {
        ids.push(id);
    }
}

/// Parse a `Retry-After` header the way v2 `_retry_after_seconds` does:
/// positive seconds win, anything else falls back to 60s.
fn retry_after_secs(header: Option<&reqwest::header::HeaderValue>) -> f64 {
    header
        .and_then(|value| value.to_str().ok())
        .and_then(|text| text.trim().parse::<f64>().ok())
        .filter(|seconds| *seconds > 0.0)
        .unwrap_or(DEFAULT_RETRY_AFTER_SECS)
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
/// deduped with order preserved (v2 `_extract_release_group_ids`, used by
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

/// Parse `fpcalc` output the way v2 `_run_fpcalc` + `_parse_fpcalc_output`
/// do. `exit_success` is fpcalc's exit status and `stderr` its error text.
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

/// Separators v2 `split_artist_credit` splits on.
const ARTIST_SEPARATORS: &[&str] = &[";", ",", "feat.", "ft.", "&", "+", "vs.", " x ", " with "];

/// Split an AcoustID artist-credit string into individual tokens (v2
/// `split_artist_credit`). Aggressive splitting is intentional: a target
/// artist is matched against any token.
pub fn split_artist_credit(credit: &str) -> Vec<String> {
    let mut tokens: Vec<String> = vec![credit.to_owned()];
    for separator in ARTIST_SEPARATORS {
        let mut split: Vec<String> = Vec::new();
        for token in &tokens {
            split.extend(token.split(separator).map(str::to_owned));
        }
        tokens = split;
    }
    tokens
        .into_iter()
        .map(|token| token.trim().to_owned())
        .filter(|token| !token.is_empty())
        .collect()
}
