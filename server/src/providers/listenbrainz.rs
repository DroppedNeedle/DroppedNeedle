//! ListenBrainz metadata client.
//!
//! Ports the read paths of v2's ListenBrainz repository and its wire
//! models, including the
//! live-verified metadata surface (`listenbrainz_MANAGEMENT_API_NOTES.md`,
//! verified against production on 2026-07-21; the recording-metadata POST
//! was verified on 2026-07-17).
//!
//! Every call sends the headers ListenBrainz requires (`Accept` and
//! `Content-Type` as JSON, plus `Authorization: Token <token>` whenever a
//! token is configured), and every token is header-safety checked before it
//! goes near the wire (v2 `_is_header_safe_listenbrainz_token`). The source
//! is optional, so failures fail soft into [`Outcome`]: a dead upstream
//! records one degradation note and yields [`Outcome::Unavailable`], while
//! an authoritative "no such thing" yields [`Outcome::Missing`].
//!
//! Pacing and degradation ride the shared core traits
//! ([`Pacer`](super::limiter::Pacer), [`DegradationSink`](super::degradation::DegradationSink)):
//! wire the pacer to a 1/second bucket ([`RATE_PER_SEC`] / [`BURST`]). v2
//! paces at 2.5/second from live edge evidence (30 requests/10 seconds,
//! 2026-08-26); v3 uses the stricter documented 1/second.
//!
//! One gap stays open: v2's response-header window tracking
//! (`X-RateLimit-Remaining` merging, the headerless-429 escalation, the
//! popularity-degraded flag) is retry and limiter state, so it belongs to
//! the provider core, and this client only reads the 429 delay headers on
//! the failure itself and reports the hint. Proactive
//! `X-RateLimit-Remaining` tracking is absent. The core [`RateLimiter`](super::limiter::RateLimiter)
//! exposes no header-feedback hook, so adding it means growing limiter API
//! surface, not client logic. The 1/s bucket plus the 429 delay hint carry
//! the pacing until that hook exists.
//!
//! v2 also borrows a fallback token for anonymous public reads when its own
//! repo has none; that provider callback is wiring, not client logic, so
//! this client simply sends no `Authorization` header when unconfigured.

use std::collections::HashMap;
use std::time::Duration;

use super::{DegradationSink, Pacer};

pub mod playlists;
pub mod stats;

/// Default API host (v2 `LISTENBRAINZ_API_URL`).
pub const DEFAULT_BASE_URL: &str = "https://api.listenbrainz.org";
/// Pacing the wiring must configure: 1 call/second (the documented limit; v2 paced
/// 2.5/second from live edge evidence, 2026-08-26).
pub const RATE_PER_SEC: f64 = 1.0;
/// Bucket burst: none, the baseline stays evenly paced (v2 `capacity=1`).
pub const BURST: u32 = 1;
/// Wait hint when a 429 carries no usable delay header (v2
/// `_RATE_LIMIT_DEFAULT_COOLDOWN_SECONDS`).
pub const DEFAULT_RETRY_AFTER_SECS: f64 = 2.0;
/// Upper clamp for server-supplied retry delays (v2
/// `_RATE_LIMIT_MAX_DELAY_SECONDS`).
pub const MAX_RETRY_AFTER_SECS: f64 = 3600.0;
/// Local ceiling on release-group ids per metadata GET. The notes stress this
/// is a local safety bound, not an upstream claim (v2 batches of 25).
pub const RELEASE_GROUP_BATCH: usize = 25;
/// Local ceiling on recording ids per metadata POST (v2 batches of 50).
pub const RECORDING_BATCH: usize = 50;
/// Local ceiling on ids per genre-batch call (v2 raises past 500).
pub const MAX_GENRE_IDS: usize = 500;
/// Longest token accepted for a header (v2
/// `_MAX_LISTENBRAINZ_TOKEN_LENGTH`).
pub const MAX_TOKEN_LEN: usize = 1024;
/// Per-request wire timeout (v2 request timeout).
pub const REQUEST_TIMEOUT_SECS: u64 = 15;
/// Source name used for degradation records.
pub const SOURCE: &str = "listenbrainz";

/// What a call produced. `Found` carries the payload, `Missing` is an
/// authoritative negative (safe to treat as one), and `Unavailable` covers
/// transport failure, rate limiting, server errors, deterministic upstream
/// policy blocks, and malformed payloads. Credential rejections surface as
/// `Unavailable` without a record, because v2 raises those instead of
/// recording them.
#[derive(Debug, Clone, PartialEq)]
pub enum Outcome<T> {
    /// The upstream answer, decoded.
    Found(T),
    /// Authoritative negative. Never recorded as degradation.
    Missing,
    /// The source could not answer.
    Unavailable {
        /// Seconds the caller should wait before retrying, when known.
        retry_after_secs: Option<f64>,
        /// Short human-readable reason (never carries secrets).
        message: String,
        /// Whether the sink already holds a record for this failure.
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

/// One user's ListenBrainz identity. The username scopes reads; the token
/// authenticates them. Both are optional because the metadata reads need no
/// token (per the management notes), while submissions and validation do.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ListenBrainzCredentials {
    /// ListenBrainz username.
    pub username: Option<String>,
    /// User token, sent as `Authorization: Token <token>`.
    pub user_token: Option<String>,
}

/// A username/token validation answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Validation {
    /// Whether the value checked out.
    pub valid: bool,
    /// Short human-readable detail.
    pub detail: String,
}

/// One listen (v2 `ListenBrainzListen`). Track and artist names are required;
/// a payload missing them fails decode instead of yielding placeholder
/// text. (v2 defaults those to `"Unknown"`; this client treats names as
/// identity and skips nameless items, so placeholder text can never flow
/// into the library as metadata.)
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Listen {
    /// Track title.
    pub track_name: String,
    /// Artist name.
    pub artist_name: String,
    /// Listen time, unix seconds.
    pub listened_at: i64,
    /// Recording MBID, from the MBID mapping or the additional info.
    pub recording_mbid: Option<String>,
    /// Release title.
    pub release_name: Option<String>,
    /// Release MBID, from the MBID mapping or the additional info.
    pub release_mbid: Option<String>,
    /// Artist MBIDs.
    pub artist_mbids: Option<Vec<String>>,
}

/// A genre tag on a release group (v2 management tag shape). Curated genre
/// entries carry `genre_mbid`; ordinary folksonomy entries omit it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GenreTag {
    /// Tag text (never blank).
    pub tag: String,
    /// Tag count.
    pub count: i64,
    /// Curated genre MBID, when the entry is a curated genre.
    pub genre_mbid: Option<String>,
}

/// Release-group metadata (v2 `LbManagementReleaseGroupMetadata`). Only the
/// tag half is projected here; the artist/release summaries ride along in
/// the tolerant decode so the shape stays faithful to the live response.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ReleaseGroupMetadata {
    /// Artist tags.
    pub artist_tags: Vec<GenreTag>,
    /// Release-group tags.
    pub release_group_tags: Vec<GenreTag>,
}

/// One top release-group row behind an artist popularity sum (v2
/// `ListenBrainzReleaseGroup`, trimmed to the popularity fields). The MBID
/// is identity and required; the count is required too (a zero count is a
/// real answer, a non-integer count is malformed and skipped); the display
/// name tolerates absence rather than placeholder text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TopReleaseGroup {
    /// Release-group MBID.
    pub release_group_mbid: String,
    /// Release-group title, empty when the wire omits it.
    pub name: String,
    /// Credited artist name, empty when the wire omits it.
    pub artist_name: String,
    /// Total listen count.
    pub listen_count: i64,
}

/// One of an artist's most played recordings (v2 `ListenBrainzRecording`).
/// The display fields tolerate absence; a row without a usable title is
/// skipped rather than shown as "Unknown".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TopRecording {
    /// Recording title.
    pub title: String,
    /// Credited artist name.
    pub artist_name: String,
    /// Total listen count.
    pub listen_count: i64,
    /// Recording MBID, when the row has one.
    pub recording_mbid: Option<String>,
    /// Release the listens point at, when known.
    pub release_name: Option<String>,
    /// That release's MBID, when known.
    pub release_mbid: Option<String>,
}

/// One similar artist from LB Radio (v2 `ListenBrainzSimilarArtist`). The
/// MBID keys the row; the listen count sums the sample recordings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SimilarArtist {
    /// Artist MBID.
    pub artist_mbid: String,
    /// Artist name, empty when the sample carried none.
    pub artist_name: String,
    /// Summed listen count of the sampled recordings.
    pub listen_count: i64,
}

/// Too many ids for one genre-batch call (v2 raises past 500).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BatchTooLarge {
    /// How many ids were asked for.
    pub asked: usize,
    /// The local ceiling.
    pub max: usize,
}

impl std::fmt::Display for BatchTooLarge {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "ListenBrainz genre lookup accepts at most {} IDs (asked {})",
            self.max, self.asked
        )
    }
}

impl std::error::Error for BatchTooLarge {}

/// ListenBrainz client. Stateless apart from its ports; cheap to clone.
#[derive(Debug, Clone)]
pub struct ListenBrainzClient<P, S> {
    http: reqwest::Client,
    base_url: String,
    pacer: P,
    sink: S,
}

impl<P: Pacer, S: DegradationSink> ListenBrainzClient<P, S> {
    /// Build a client against `base_url` (the production host or a fake).
    pub fn new(http: reqwest::Client, base_url: &str, pacer: P, sink: S) -> Self {
        Self {
            http,
            base_url: base_url.trim_end_matches('/').to_owned(),
            pacer,
            sink,
        }
    }

    /// Check that a username exists (`GET /1/user/{user}/listen-count`). A
    /// 404 is an ordinary invalid answer (v2 `accepted_statuses=(404,)`).
    pub async fn validate_username(
        &self,
        username: &str,
        creds: &ListenBrainzCredentials,
    ) -> Outcome<Validation> {
        if username.is_empty() {
            return Outcome::Found(Validation {
                valid: false,
                detail: "No username provided".to_owned(),
            });
        }
        let endpoint = format!("/1/user/{}/listen-count", path_segment(username));
        let payload = match self.get(&endpoint, &[], creds, false, &[404]).await {
            Ok(payload) => payload,
            Err(RequestFailure::Accepted(404)) => {
                return Outcome::Found(Validation {
                    valid: false,
                    detail: format!("User '{username}' not found"),
                });
            }
            Err(RequestFailure::Outcome(outcome)) => return outcome,
            Err(RequestFailure::Accepted(_)) => {
                return self.shape_error("ListenBrainz gave an unexpected reply");
            }
        };
        match payload {
            Body::Json(payload) => {
                let count = payload
                    .get("payload")
                    .and_then(|payload| payload.get("count"))
                    .and_then(serde_json::Value::as_i64)
                    .unwrap_or(0);
                Outcome::Found(Validation {
                    valid: true,
                    detail: format!("User found with {count} listens"),
                })
            }
            // A 204 or an undecodable body reads as "not found" (v2 returns
            // `(False, ...)` when the result is None).
            Body::NoContent | Body::InvalidJson => Outcome::Found(Validation {
                valid: false,
                detail: format!("User '{username}' not found"),
            }),
        }
    }

    /// Check that the configured token is valid (`GET /1/validate-token`). A
    /// 401 or 403 is an ordinary invalid answer.
    pub async fn validate_token(&self, creds: &ListenBrainzCredentials) -> Outcome<Validation> {
        if creds
            .user_token
            .as_deref()
            .filter(|token| !token.is_empty())
            .is_none()
        {
            return Outcome::Found(Validation {
                valid: false,
                detail: "No token provided".to_owned(),
            });
        }
        let payload = match self
            .get("/1/validate-token", &[], creds, false, &[401, 403])
            .await
        {
            Ok(payload) => payload,
            Err(RequestFailure::Accepted(_)) => {
                return Outcome::Found(Validation {
                    valid: false,
                    detail: "Token invalid or expired".to_owned(),
                });
            }
            Err(RequestFailure::Outcome(outcome)) => return outcome,
        };
        let valid = match &payload {
            Body::Json(payload) => payload
                .get("valid")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false),
            Body::NoContent | Body::InvalidJson => false,
        };
        if !valid {
            return Outcome::Found(Validation {
                valid: false,
                detail: "Token invalid or expired".to_owned(),
            });
        }
        let username = match &payload {
            Body::Json(payload) => payload
                .get("user_name")
                .and_then(serde_json::Value::as_str)
                .or(creds.username.as_deref())
                .unwrap_or(""),
            Body::NoContent | Body::InvalidJson => creds.username.as_deref().unwrap_or(""),
        };
        Outcome::Found(Validation {
            valid: true,
            detail: format!("Successfully connected as '{username}'"),
        })
    }

    /// Recent listens for a user (`GET /1/user/{user}/listens`, at most 100
    /// per call). An empty username answers empty without touching the wire
    /// (v2). Items missing track or artist names are skipped.
    pub async fn user_listens(
        &self,
        username: &str,
        count: u32,
        creds: &ListenBrainzCredentials,
    ) -> Outcome<Vec<Listen>> {
        if username.is_empty() {
            return Outcome::Found(Vec::new());
        }
        let count_text = count.min(100).to_string();
        let endpoint = format!("/1/user/{}/listens", path_segment(username));
        let payload = match self
            .get(
                &endpoint,
                &[("count", count_text.as_str())],
                creds,
                false,
                &[],
            )
            .await
        {
            Ok(payload) => payload,
            Err(RequestFailure::Outcome(outcome)) => return outcome,
            Err(RequestFailure::Accepted(_)) => {
                return self.shape_error("ListenBrainz gave an unexpected reply");
            }
        };
        // An empty answer reads as no listens (v2 returns `[]` when the
        // result is falsy).
        let listens = match payload {
            Body::Json(payload) => payload
                .get("payload")
                .and_then(|payload| payload.get("listens"))
                .and_then(serde_json::Value::as_array)
                .cloned()
                .unwrap_or_default(),
            Body::NoContent | Body::InvalidJson => Vec::new(),
        };
        Outcome::Found(listens.iter().filter_map(parse_listen).collect())
    }

    /// Resolve recording MBIDs to release-group MBIDs (`POST
    /// /1/metadata/recording/` with `inc=release`; live-verified on
    /// 2026-07-17, returns an object keyed by recording MBID whose release
    /// object carries `release_group_mbid`). Input dedupes, wire calls batch
    /// at 50, and MBIDs absent from the answer are simply left out.
    pub async fn recording_release_groups(
        &self,
        recording_mbids: &[String],
        creds: &ListenBrainzCredentials,
    ) -> Outcome<HashMap<String, String>> {
        let mut unique: Vec<&str> = Vec::new();
        for mbid in recording_mbids {
            if !mbid.is_empty() && !unique.contains(&mbid.as_str()) {
                unique.push(mbid.as_str());
            }
        }
        if unique.is_empty() {
            return Outcome::Found(HashMap::new());
        }
        unique.sort_unstable();
        let mut resolved: HashMap<String, String> = HashMap::new();
        for batch in unique.chunks(RECORDING_BATCH) {
            let body = serde_json::json!({"recording_mbids": batch, "inc": "release"});
            let payload = match self
                .post("/1/metadata/recording/", &body, creds, false)
                .await
            {
                Ok(payload) => payload,
                Err(outcome) => return outcome,
            };
            // A non-object payload records a note and skips the batch (v2);
            // results gathered so far are kept.
            let payload = match payload {
                Body::Json(payload) => match payload.as_object() {
                    Some(payload) => payload.clone(),
                    None => {
                        self.sink.record_quiet(
                            SOURCE,
                            "ListenBrainz returned no recording metadata".to_owned(),
                        );
                        continue;
                    }
                },
                Body::NoContent | Body::InvalidJson => {
                    self.sink.record_quiet(
                        SOURCE,
                        "ListenBrainz returned no recording metadata".to_owned(),
                    );
                    continue;
                }
            };
            for mbid in batch {
                let release_group = payload
                    .get(*mbid)
                    .and_then(|metadata| metadata.get("release"))
                    .and_then(|release| release.get("release_group_mbid"))
                    .and_then(serde_json::Value::as_str);
                if let Some(release_group) = release_group {
                    resolved.insert((*mbid).to_owned(), release_group.to_owned());
                }
            }
        }
        Outcome::Found(resolved)
    }

    /// Genre tags for release-group MBIDs from the live-verified GET-only
    /// endpoint (`GET /1/metadata/release_group/` with comma-separated ids
    /// and `inc=artist tag release`; verified against production on
    /// 2026-07-21). Calls batch at 25 ids; a valid-but-unknown MBID answers
    /// HTTP 200 with `{}` and yields an empty tag list for that id (v2).
    /// POST must never be used here: the live notes record that POST answers
    /// 405, and the recording endpoint's POST support must not be
    /// generalized to release groups.
    pub async fn release_group_genres(
        &self,
        release_group_mbids: &[String],
        creds: &ListenBrainzCredentials,
    ) -> Result<Outcome<HashMap<String, ReleaseGroupMetadata>>, BatchTooLarge> {
        let mut unique: Vec<String> = Vec::new();
        for mbid in release_group_mbids {
            let trimmed = mbid.trim();
            if !trimmed.is_empty() && !unique.iter().any(|known| known == trimmed) {
                unique.push(trimmed.to_owned());
            }
        }
        if unique.len() > MAX_GENRE_IDS {
            return Err(BatchTooLarge {
                asked: unique.len(),
                max: MAX_GENRE_IDS,
            });
        }
        let mut resolved: HashMap<String, ReleaseGroupMetadata> = HashMap::new();
        unique.sort();
        for batch in unique.chunks(RELEASE_GROUP_BATCH) {
            let ids = batch.join(",");
            let payload = match self
                .get(
                    "/1/metadata/release_group/",
                    &[
                        ("release_group_mbids", ids.as_str()),
                        ("inc", "artist tag release"),
                    ],
                    creds,
                    false,
                    &[],
                )
                .await
            {
                Ok(payload) => payload,
                Err(RequestFailure::Outcome(outcome)) => return Ok(outcome),
                Err(RequestFailure::Accepted(_)) => {
                    return Ok(self.shape_error("ListenBrainz gave an unexpected reply"));
                }
            };
            // No payload at all is an upstream failure, not an empty answer
            // (v2 raises `ExternalServiceError` without recording here; an
            // undecodable body already holds its classifier record).
            let payload = match payload {
                Body::Json(payload) => match payload.as_object() {
                    Some(payload) => payload.clone(),
                    None => {
                        return Ok(self
                            .shape_error("ListenBrainz returned invalid release-group metadata."));
                    }
                },
                Body::NoContent => {
                    return Ok(self.unrecorded("ListenBrainz returned no release-group metadata."));
                }
                Body::InvalidJson => {
                    return Ok(Outcome::Unavailable {
                        retry_after_secs: None,
                        message: "ListenBrainz returned no release-group metadata.".to_owned(),
                        recorded: true,
                    });
                }
            };
            for mbid in batch {
                let metadata = payload
                    .get(mbid)
                    .map(parse_release_group_metadata)
                    .unwrap_or_default();
                resolved.insert(mbid.clone(), metadata);
            }
        }
        Ok(Outcome::Found(resolved))
    }

    /// Listen counts for release-group MBIDs (`POST
    /// /1/popularity/release-group`). A malformed or absent answer yields
    /// whatever was gathered so far with nothing written, because an outage
    /// must never read as "zero listens" (v2's poisoning guards). A
    /// well-formed empty list is legitimate and yields an empty map.
    pub async fn release_group_popularity(
        &self,
        release_group_mbids: &[String],
        creds: &ListenBrainzCredentials,
    ) -> Outcome<HashMap<String, i64>> {
        if release_group_mbids.is_empty() {
            return Outcome::Found(HashMap::new());
        }
        let mut unique: Vec<&str> = Vec::new();
        for mbid in release_group_mbids {
            if !unique.contains(&mbid.as_str()) {
                unique.push(mbid.as_str());
            }
        }
        unique.sort_unstable();
        let body = serde_json::json!({"release_group_mbids": unique});
        let payload = match self
            .post("/1/popularity/release-group", &body, creds, false)
            .await
        {
            Ok(payload) => payload,
            Err(outcome) => return outcome,
        };
        // Malformed or absent payloads return whatever was gathered so
        // far (v2 writes nothing and returns `counts`): an outage must never
        // read as "zero listens", and a well-formed empty list is legitimate.
        let mut counts: HashMap<String, i64> = HashMap::new();
        let items = match payload {
            Body::Json(payload) => match payload.as_array() {
                Some(items) => items.clone(),
                None => return Outcome::Found(counts),
            },
            Body::NoContent | Body::InvalidJson => return Outcome::Found(counts),
        };
        for item in &items {
            let mbid = item
                .get("release_group_mbid")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("");
            // v2 keeps a zero count (`count is not None`) but skips a blank
            // id; a non-integer count is malformed and skipped.
            let count = item.get("total_listen_count").and_then(count_int);
            if mbid.is_empty() {
                continue;
            }
            if let Some(count) = count {
                counts.insert(mbid.to_owned(), count);
            }
        }
        Outcome::Found(counts)
    }

    /// Top release groups behind one artist's popularity sum (`GET`
    /// /1/popularity/top-release-groups-for-artist/{mbid}`, v2
    /// `get_artist_top_release_groups`). Anonymous: popularity reads need
    /// no token. Malformed items are skipped and a malformed payload yields
    /// whatever was gathered, mirroring the popularity batch: an outage
    /// must never read as "zero listens", and a well-formed empty list is
    /// a legitimate unknown artist.
    pub async fn artist_top_release_groups(
        &self,
        artist_mbid: &str,
        count: usize,
        creds: &ListenBrainzCredentials,
    ) -> Outcome<Vec<TopReleaseGroup>> {
        let artist_mbid = artist_mbid.trim();
        if artist_mbid.is_empty() || count == 0 {
            return Outcome::Found(Vec::new());
        }
        let endpoint = format!("/1/popularity/top-release-groups-for-artist/{artist_mbid}");
        let payload = match self.get(&endpoint, &[], creds, false, &[]).await {
            Ok(payload) => payload,
            Err(RequestFailure::Outcome(outcome)) => return outcome,
            Err(RequestFailure::Accepted(_)) => {
                return self.shape_error("ListenBrainz gave an unexpected reply");
            }
        };
        let items = match payload {
            Body::Json(payload) => match payload.as_array() {
                Some(items) => items.clone(),
                None => return Outcome::Found(Vec::new()),
            },
            Body::NoContent | Body::InvalidJson => return Outcome::Found(Vec::new()),
        };
        Outcome::Found(
            items
                .iter()
                .take(count)
                .filter_map(parse_top_release_group)
                .collect(),
        )
    }

    /// An artist's most played recordings
    /// (`GET /1/popularity/top-recordings-for-artist/{mbid}`), most played
    /// first. Public, no token. Malformed rows are skipped; a malformed
    /// payload reads as an empty answer, never as "zero listens" rows.
    pub async fn artist_top_recordings(
        &self,
        artist_mbid: &str,
        count: usize,
        creds: &ListenBrainzCredentials,
    ) -> Outcome<Vec<TopRecording>> {
        let artist_mbid = artist_mbid.trim();
        if artist_mbid.is_empty() || count == 0 {
            return Outcome::Found(Vec::new());
        }
        let endpoint = format!("/1/popularity/top-recordings-for-artist/{artist_mbid}");
        let items = match self.get_array(&endpoint, &[], creds).await {
            Ok(items) => items,
            Err(outcome) => return outcome,
        };
        Outcome::Found(
            items
                .iter()
                .filter_map(parse_top_recording)
                .take(count)
                .collect(),
        )
    }

    /// Artists similar to one artist, from LB Radio's artist mode
    /// (`GET /1/lb-radio/artist/{mbid}` with v2's `easy` parameters). The
    /// reply maps each similar artist's MBID to a sample of its recordings;
    /// the seed artist is dropped and the rest sort by summed listens.
    pub async fn similar_artists(
        &self,
        artist_mbid: &str,
        max_similar: usize,
        creds: &ListenBrainzCredentials,
    ) -> Outcome<Vec<SimilarArtist>> {
        let artist_mbid = artist_mbid.trim();
        if artist_mbid.is_empty() || max_similar == 0 {
            return Outcome::Found(Vec::new());
        }
        let endpoint = format!("/1/lb-radio/artist/{artist_mbid}");
        let max = max_similar.to_string();
        let params = [
            ("mode", "easy"),
            ("max_similar_artists", max.as_str()),
            ("max_recordings_per_artist", "5"),
            ("pop_begin", "0"),
            ("pop_end", "100"),
        ];
        let payload = match self.get(&endpoint, &params, creds, false, &[]).await {
            Ok(Body::Json(payload)) => payload,
            Ok(Body::NoContent | Body::InvalidJson) => return Outcome::Found(Vec::new()),
            Err(RequestFailure::Outcome(outcome)) => return outcome,
            Err(RequestFailure::Accepted(_)) => {
                return self.shape_error("ListenBrainz gave an unexpected reply");
            }
        };
        let Some(entries) = payload.as_object() else {
            return Outcome::Found(Vec::new());
        };
        let mut similar: Vec<SimilarArtist> = entries
            .iter()
            .filter(|(mbid, _)| {
                super::musicbrainz::is_valid_mbid(mbid) && !mbid.eq_ignore_ascii_case(artist_mbid)
            })
            .filter_map(|(mbid, recordings)| {
                let recordings = recordings.as_array()?;
                let artist_name = recordings
                    .first()
                    .and_then(|first| first.get("similar_artist_name"))
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("")
                    .to_owned();
                let listen_count = recordings
                    .iter()
                    .filter_map(|recording| recording.get("total_listen_count"))
                    .filter_map(count_int)
                    .sum();
                Some(SimilarArtist {
                    artist_mbid: mbid.clone(),
                    artist_name,
                    listen_count,
                })
            })
            .collect();
        similar.sort_by(|left, right| right.listen_count.cmp(&left.listen_count));
        Outcome::Found(similar)
    }

    /// GET an endpoint whose answer is a JSON array. Anything else reads
    /// as an empty array.
    async fn get_array<T>(
        &self,
        endpoint: &str,
        params: &[(&str, &str)],
        creds: &ListenBrainzCredentials,
    ) -> Result<Vec<serde_json::Value>, Outcome<T>> {
        match self.get(endpoint, params, creds, false, &[]).await {
            Ok(Body::Json(serde_json::Value::Array(items))) => Ok(items),
            Ok(_) => Ok(Vec::new()),
            Err(RequestFailure::Outcome(outcome)) => Err(outcome),
            Err(RequestFailure::Accepted(_)) => {
                Err(self.shape_error("ListenBrainz gave an unexpected reply"))
            }
        }
    }

    /// Run one paced GET. `accepted` statuses are validators' expected
    /// negatives (a 404 on a username check, a 401/403 on a token check);
    /// they come back as `Accepted` and stay neutral to retry and
    /// degradation bookkeeping (v2 `_ListenBrainzValidationOutcome`).
    async fn get<T>(
        &self,
        endpoint: &str,
        params: &[(&str, &str)],
        creds: &ListenBrainzCredentials,
        require_auth: bool,
        accepted: &[u16],
    ) -> Result<Body, RequestFailure<T>> {
        let headers = self.headers(creds, require_auth)?;
        self.pacer.acquire().await;
        let url = format!("{base}{endpoint}", base = self.base_url);
        let response = match self
            .http
            .get(url)
            .headers(headers)
            .query(params)
            .timeout(Duration::from_secs(REQUEST_TIMEOUT_SECS))
            .send()
            .await
        {
            Ok(response) => response,
            Err(error) => {
                return Err(RequestFailure::Outcome(self.recorded(
                    None,
                    &format!("ListenBrainz GET request failed during transport: {error}"),
                )));
            }
        };
        self.classify(response, "GET", endpoint, accepted).await
    }

    /// Run one paced POST with a JSON body.
    async fn post<T>(
        &self,
        endpoint: &str,
        body: &serde_json::Value,
        creds: &ListenBrainzCredentials,
        require_auth: bool,
    ) -> Result<Body, Outcome<T>> {
        let headers = self
            .headers(creds, require_auth)
            .map_err(|failure| match failure {
                RequestFailure::Outcome(outcome) => outcome,
                RequestFailure::Accepted(_) => {
                    self.shape_error("ListenBrainz gave an unexpected reply")
                }
            })?;
        self.pacer.acquire().await;
        let url = format!("{base}{endpoint}", base = self.base_url);
        let body_text = serde_json::to_string(body).unwrap_or_else(|_| "{}".to_owned());
        let response = match self
            .http
            .post(url)
            .headers(headers)
            .body(body_text)
            .timeout(Duration::from_secs(REQUEST_TIMEOUT_SECS))
            .send()
            .await
        {
            Ok(response) => response,
            Err(error) => {
                return Err(self.recorded(
                    None,
                    &format!("ListenBrainz POST request failed during transport: {error}"),
                ));
            }
        };
        match self.classify(response, "POST", endpoint, &[]).await {
            Ok(payload) => Ok(payload),
            Err(RequestFailure::Outcome(outcome)) => Err(outcome),
            Err(RequestFailure::Accepted(_)) => {
                Err(self.shape_error("ListenBrainz gave an unexpected reply"))
            }
        }
    }

    /// Build the required headers: JSON `Accept` and `Content-Type` always,
    /// plus `Authorization: Token <token>` when a token is configured (v2
    /// `_get_headers`). Tokens are header-safety checked first; a rejected
    /// token (or a missing one on an auth-required call) surfaces as an
    /// unrecorded `Unavailable`, because v2 raises those deterministically
    /// without retrying.
    fn headers<T>(
        &self,
        creds: &ListenBrainzCredentials,
        require_auth: bool,
    ) -> Result<reqwest::header::HeaderMap, RequestFailure<T>> {
        if require_auth {
            match creds.user_token.as_deref() {
                Some(token) if header_safe_token(token) => {}
                _ => {
                    return Err(RequestFailure::Outcome(
                        self.unrecorded("ListenBrainz user token required for this request"),
                    ));
                }
            }
        }
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(
            reqwest::header::ACCEPT,
            reqwest::header::HeaderValue::from_static("application/json"),
        );
        headers.insert(
            reqwest::header::CONTENT_TYPE,
            reqwest::header::HeaderValue::from_static("application/json"),
        );
        if let Some(token) = creds
            .user_token
            .as_deref()
            .filter(|token| !token.is_empty())
        {
            if !header_safe_token(token) {
                return Err(RequestFailure::Outcome(
                    self.unrecorded("ListenBrainz credentials rejected"),
                ));
            }
            let value = format!("Token {token}");
            match reqwest::header::HeaderValue::from_str(&value) {
                Ok(value) => {
                    headers.insert(reqwest::header::AUTHORIZATION, value);
                }
                Err(_) => {
                    return Err(RequestFailure::Outcome(
                        self.unrecorded("ListenBrainz credentials rejected"),
                    ));
                }
            }
        }
        Ok(headers)
    }

    /// Classify one wire response, following v2 `_request_attempt` branch
    /// for branch.
    async fn classify<T>(
        &self,
        response: reqwest::Response,
        method: &str,
        endpoint: &str,
        accepted: &[u16],
    ) -> Result<Body, RequestFailure<T>> {
        let status = response.status().as_u16();
        let category = endpoint_category(endpoint);
        if status == 204 {
            self.sink.succeeded(SOURCE);
            return Ok(Body::NoContent);
        }
        if status == 429 {
            // Explicit server delay wins; a headerless 429 falls back to the
            // 2s default here, while the provider core owns the streak escalation.
            let retry_after = retry_after_secs(response.headers());
            return Err(RequestFailure::Outcome(self.recorded(
                Some(retry_after),
                "ListenBrainz is temporarily rate-limiting this server. Try again shortly.",
            )));
        }
        if status != 200 {
            if accepted.contains(&status) {
                return Err(RequestFailure::Accepted(status));
            }
            let body = response.text().await.unwrap_or_default();
            // Deterministic upstream policy blocks fail fast and must not
            // trip any shared breaker (v2 `_is_upstream_policy_block`): the
            // popularity feature-flag 500, and the anti-scraper 401 added in
            // 2026-07 when ListenBrainz began gating anonymous popularity
            // calls.
            if is_policy_block(status, &body) {
                let message = format!(
                    "ListenBrainz {method} {category} endpoint unavailable upstream ({status})"
                );
                self.sink.record_quiet(SOURCE, message.clone());
                return Err(RequestFailure::Outcome(Outcome::Unavailable {
                    retry_after_secs: None,
                    message,
                    recorded: true,
                }));
            }
            if status == 401 || status == 403 {
                return Err(RequestFailure::Outcome(self.unrecorded(&format!(
                    "ListenBrainz credentials rejected ({status})"
                ))));
            }
            return Err(RequestFailure::Outcome(self.recorded(
                None,
                &format!("ListenBrainz {method} {category} request failed ({status})"),
            )));
        }
        match response.text().await {
            Ok(text) => match serde_json::from_str(&text) {
                Ok(payload) => {
                    self.sink.succeeded(SOURCE);
                    Ok(Body::Json(payload))
                }
                // v2 records a note and returns None on invalid JSON rather
                // than raising; each caller below then applies its own
                // empty-answer rule.
                Err(_) => {
                    self.sink.record_quiet(
                        SOURCE,
                        format!("ListenBrainz returned invalid JSON for {method} {category}"),
                    );
                    Ok(Body::InvalidJson)
                }
            },
            Err(error) => Err(RequestFailure::Outcome(self.recorded(
                None,
                &format!("ListenBrainz response body unreadable: {error}"),
            ))),
        }
    }

    /// Build a recorded `Unavailable` outcome.
    fn recorded<T>(&self, retry_after_secs: Option<f64>, message: &str) -> Outcome<T> {
        self.sink.record(SOURCE, message.to_owned());
        Outcome::Unavailable {
            retry_after_secs,
            message: message.to_owned(),
            recorded: true,
        }
    }

    /// A reply whose shape did not decode: recorded for the request, kept
    /// out of service health (the service answered).
    fn shape_error<T>(&self, message: &str) -> Outcome<T> {
        self.sink.record_quiet(SOURCE, message.to_owned());
        Outcome::Unavailable {
            retry_after_secs: None,
            message: message.to_owned(),
            recorded: true,
        }
    }

    /// Build an unrecorded `Unavailable` outcome for credential failures,
    /// which v2 raises rather than records.
    fn unrecorded<T>(&self, message: &str) -> Outcome<T> {
        Outcome::Unavailable {
            retry_after_secs: None,
            message: message.to_owned(),
            recorded: false,
        }
    }
}

/// What a 200 response carried: either a decoded payload, a genuine
/// empty answer (204), or an undecodable body (already recorded in the sink
/// by the classifier). Callers need all three because v2 treats them
/// differently: validators read emptiness as a negative, the genre batch
/// escalates it, and the popularity batch swallows it as partial results.
#[derive(Debug, Clone, PartialEq)]
enum Body {
    /// Decoded JSON payload.
    Json(serde_json::Value),
    /// HTTP 204: genuinely empty.
    NoContent,
    /// Undecodable body. The sink already holds its record.
    InvalidJson,
}

/// What went wrong below the payload level: either a validator's expected
/// status (neutral to retry and degradation) or a finished outcome.
enum RequestFailure<T> {
    /// An accepted deterministic status (v2
    /// `_ListenBrainzValidationOutcome`).
    Accepted(u16),
    /// A finished outcome, already recorded when recording applies.
    Outcome(Outcome<T>),
}

/// Percent-encode one URL path segment (a username can hold `/`, `?`, `#`
/// or spaces).
pub(crate) fn path_segment(value: &str) -> String {
    use std::fmt::Write as _;
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            out.push(byte as char);
        } else {
            let _ = write!(out, "%{byte:02X}");
        }
    }
    out
}

/// Whether a token may ride in a header: non-empty, at most 1024 chars, all
/// printable ASCII (v2 `_is_header_safe_listenbrainz_token`).
fn header_safe_token(token: &str) -> bool {
    if token.is_empty() || token.len() > MAX_TOKEN_LEN {
        return false;
    }
    token
        .chars()
        .all(|char| ('\u{21}'..='\u{7e}').contains(&char))
}

/// Read the retry delay from a 429's headers: `X-RateLimit-Reset-In` first,
/// then `Retry-After`, requiring a positive finite value and clamping to an
/// hour; anything else falls back to the 2s default (v2
/// `_parse_retry_after_info`).
fn retry_after_secs(headers: &reqwest::header::HeaderMap) -> f64 {
    for name in ["x-ratelimit-reset-in", "retry-after"] {
        let delay = headers
            .get(name)
            .and_then(|value| value.to_str().ok())
            .and_then(|text| text.trim().parse::<f64>().ok())
            .filter(|seconds| seconds.is_finite() && *seconds >= 0.0);
        if let Some(delay) = delay.filter(|delay| *delay > 0.0) {
            return delay.min(MAX_RETRY_AFTER_SECS);
        }
    }
    DEFAULT_RETRY_AFTER_SECS
}

/// Whether this response is a deterministic upstream policy block rather
/// than an ordinary error (v2 `_is_upstream_policy_block`).
fn is_policy_block(status: u16, body: &str) -> bool {
    if status == 500 && body.contains("currently disabled") {
        return true;
    }
    if status == 401 && (body.contains("provide an Auth token") || body.contains("AI scrapers")) {
        return true;
    }
    false
}

/// Short category for log and degradation messages (v2
/// `_listenbrainz_endpoint_category`).
fn endpoint_category(endpoint: &str) -> &'static str {
    let path = endpoint.split('?').next().unwrap_or(endpoint);
    for (marker, category) in [
        ("/validate-token", "token validation"),
        ("/popularity/", "popularity"),
        ("/metadata/", "metadata"),
        ("/feedback/", "feedback"),
        ("/submit-listens", "listen submission"),
        ("/playing-now", "now-playing"),
        ("/stats/", "statistics"),
        ("/user/", "user data"),
    ] {
        if path.contains(marker) {
            return category;
        }
    }
    "request"
}

/// Read an integer count, tolerating integral floats the way the lenient v2
/// parsers do. Booleans and strings never pass as counts.
fn count_int(value: &serde_json::Value) -> Option<i64> {
    match value {
        serde_json::Value::Number(number) => number.as_i64().or_else(|| {
            number
                .as_f64()
                .filter(|float| float.is_finite() && float.fract() == 0.0)
                .map(|float| float as i64)
        }),
        _ => None,
    }
}

/// Read an optional MBID-ish string: blank reads as absent.
fn optional_text(value: Option<&serde_json::Value>) -> Option<String> {
    value
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(str::to_owned)
}

/// Parse one listen (v2 `parse_listen`). Recording and release MBIDs prefer
/// the MBID mapping and fall back to the additional info.
fn parse_listen(item: &serde_json::Value) -> Option<Listen> {
    let meta = item.get("track_metadata")?.as_object()?;
    let track_name = meta.get("track_name")?.as_str()?;
    let artist_name = meta.get("artist_name")?.as_str()?;
    if track_name.trim().is_empty() || artist_name.trim().is_empty() {
        return None;
    }
    let additional = meta.get("additional_info");
    let mapping = meta.get("mbid_mapping");
    let recording_mbid = mapping
        .and_then(|mapping| mapping.get("recording_mbid"))
        .or_else(|| additional.and_then(|info| info.get("recording_mbid")));
    let release_mbid = mapping
        .and_then(|mapping| mapping.get("release_mbid"))
        .or_else(|| additional.and_then(|info| info.get("release_mbid")));
    let artist_mbids = mapping
        .and_then(|mapping| mapping.get("artist_mbids"))
        .and_then(serde_json::Value::as_array)
        .map(|mbids| {
            mbids
                .iter()
                .filter_map(|mbid| mbid.as_str().map(str::to_owned))
                .collect::<Vec<_>>()
        });
    Some(Listen {
        track_name: track_name.to_owned(),
        artist_name: artist_name.to_owned(),
        listened_at: item.get("listened_at").and_then(count_int).unwrap_or(0),
        recording_mbid: optional_text(recording_mbid),
        release_name: optional_text(meta.get("release_name")),
        release_mbid: optional_text(release_mbid),
        artist_mbids,
    })
}

/// Parse one artist top-release-group row: the MBID is required identity
/// (blank reads as absent), the count must be an integer (zero kept, like
/// the popularity batch), and the nested display name tolerates absence.
fn parse_top_release_group(item: &serde_json::Value) -> Option<TopReleaseGroup> {
    let mbid = item
        .get("release_group_mbid")?
        .as_str()
        .map(str::trim)
        .filter(|mbid| !mbid.is_empty())?;
    let listen_count = item.get("total_listen_count").and_then(count_int)?;
    let name = item
        .get("release_group")
        .and_then(|group| group.get("name"))
        .and_then(serde_json::Value::as_str)
        .unwrap_or("")
        .to_owned();
    let artist_name = item
        .get("artist")
        .and_then(|artist| artist.get("name"))
        .and_then(serde_json::Value::as_str)
        .unwrap_or("")
        .to_owned();
    Some(TopReleaseGroup {
        release_group_mbid: mbid.to_owned(),
        name,
        artist_name,
        listen_count,
    })
}

/// Parse one top-recording row: a non-blank title and an integer count are
/// required, everything else is optional display data.
fn parse_top_recording(item: &serde_json::Value) -> Option<TopRecording> {
    let text = |key: &str| {
        item.get(key)
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_owned)
    };
    let title = text("recording_name")?;
    let listen_count = item.get("total_listen_count").and_then(count_int)?;
    Some(TopRecording {
        title,
        artist_name: text("artist_name").unwrap_or_default(),
        listen_count,
        recording_mbid: text("recording_mbid"),
        release_name: text("release_name"),
        release_mbid: text("release_mbid"),
    })
}

/// Parse one genre tag: tag text required and non-blank, count lenient,
/// `genre_mbid` optional (v2 management tag shape; the live notes record
/// that curated entries carry it and folksonomy entries may omit it).
fn parse_genre_tag(item: &serde_json::Value) -> Option<GenreTag> {
    let tag = item.as_object()?;
    let text = tag.get("tag")?.as_str()?;
    if text.trim().is_empty() {
        return None;
    }
    Some(GenreTag {
        tag: text.to_owned(),
        count: tag.get("count").and_then(count_int).unwrap_or(0),
        genre_mbid: optional_text(tag.get("genre_mbid")),
    })
}

/// Parse the release-group metadata object (v2
/// `LbManagementReleaseGroupMetadata`). `tag` holds `artist` and
/// `release_group` arrays; `release` is an object with the same summary
/// shape, not a list (live notes). Unknown members are ignored.
fn parse_release_group_metadata(entry: &serde_json::Value) -> ReleaseGroupMetadata {
    let tags = entry.get("tag");
    let collect = |key: &str| {
        tags.and_then(|tags| tags.get(key))
            .and_then(serde_json::Value::as_array)
            .map(|items| items.iter().filter_map(parse_genre_tag).collect())
            .unwrap_or_default()
    };
    ReleaseGroupMetadata {
        artist_tags: collect("artist"),
        release_group_tags: collect("release_group"),
    }
}

// --- credential verification ---------------------------------------------------
//
// The settings Verify button and the per-user link flow both check a
// username/token pair against the live API. Neither goes through the paced
// metadata client: a verify is one admin-initiated call, not a read path.

/// Answer from a ListenBrainz credential check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifyOutcome {
    /// Whether the credential checked out.
    pub valid: bool,
    /// Short human-readable detail.
    pub message: String,
    /// The upstream is rate-limiting; the caller should answer 429.
    pub rate_limited: bool,
}

/// Credential checks against ListenBrainz. With a token this validates the
/// token; without one it validates that the username exists.
pub trait ListenBrainzVerifier: Send + Sync {
    /// Check one username/token pair.
    fn verify<'a>(
        &'a self,
        username: &'a str,
        token: &'a str,
    ) -> futures_util::future::BoxFuture<'a, VerifyOutcome>;
}

/// Percent-encode one path segment.
fn encode_segment(segment: &str) -> String {
    let mut out = String::new();
    for byte in segment.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

/// Render a count with thousands separators, the way v2's `{count:,}` does.
fn grouped_count(count: i64) -> String {
    let digits = count.max(0).to_string();
    let mut out = String::new();
    for (index, digit) in digits.chars().rev().enumerate() {
        if index > 0 && index % 3 == 0 {
            out.push(',');
        }
        out.push(digit);
    }
    out.chars().rev().collect()
}

fn unavailable() -> VerifyOutcome {
    VerifyOutcome {
        valid: false,
        message: "ListenBrainz is temporarily unavailable. Try again shortly.".to_owned(),
        rate_limited: false,
    }
}

fn transport_outcome(error: &reqwest::Error) -> VerifyOutcome {
    if error.is_timeout() {
        return VerifyOutcome {
            valid: false,
            message: "Connection timed out".to_owned(),
            rate_limited: false,
        };
    }
    if error.is_connect() {
        return VerifyOutcome {
            valid: false,
            message: "Could not connect to ListenBrainz".to_owned(),
            rate_limited: false,
        };
    }
    unavailable()
}

/// Credential checks over HTTP. Status mapping ports v2's repository
/// verify methods: 401/403 on the token call means an invalid token, 404
/// on the username call means an unknown user, and 429 means back off.
pub struct HttpListenBrainzVerifier {
    http: reqwest::Client,
    base_url: String,
}

impl HttpListenBrainzVerifier {
    /// Check against one API root (the production host or a fake).
    pub fn new(http: reqwest::Client, base_url: &str) -> Self {
        Self {
            http,
            base_url: base_url.trim_end_matches('/').to_owned(),
        }
    }

    /// Check against the production API.
    pub fn prod(http: reqwest::Client) -> Self {
        Self::new(http, DEFAULT_BASE_URL)
    }

    async fn verify_token(&self, token: &str, username: &str) -> VerifyOutcome {
        if !header_safe_token(token) {
            return VerifyOutcome {
                valid: false,
                message: "Token invalid or expired".to_owned(),
                rate_limited: false,
            };
        }
        let response = self
            .http
            .get(format!("{}/1/validate-token", self.base_url))
            .header("Authorization", format!("Token {token}"))
            .timeout(Duration::from_secs(REQUEST_TIMEOUT_SECS))
            .send()
            .await;
        let response = match response {
            Ok(response) => response,
            Err(error) => return transport_outcome(&error),
        };
        let status = response.status().as_u16();
        if status == 429 {
            return VerifyOutcome {
                valid: false,
                message:
                    "ListenBrainz is temporarily rate-limiting this server. Try again shortly."
                        .to_owned(),
                rate_limited: true,
            };
        }
        if status == 401 || status == 403 {
            return VerifyOutcome {
                valid: false,
                message: "Token invalid or expired".to_owned(),
                rate_limited: false,
            };
        }
        if status != 200 {
            return unavailable();
        }
        let bytes = match response.bytes().await {
            Ok(bytes) => bytes,
            Err(_) => return unavailable(),
        };
        let body: serde_json::Value = match serde_json::from_slice(&bytes) {
            Ok(body) => body,
            Err(_) => return unavailable(),
        };
        if body.get("valid").and_then(|valid| valid.as_bool()) == Some(true) {
            let who = body
                .get("user_name")
                .and_then(|name| name.as_str())
                .unwrap_or(username);
            return VerifyOutcome {
                valid: true,
                message: format!("Successfully connected as '{who}'"),
                rate_limited: false,
            };
        }
        VerifyOutcome {
            valid: false,
            message: "Token invalid or expired".to_owned(),
            rate_limited: false,
        }
    }

    async fn verify_username(&self, username: &str) -> VerifyOutcome {
        if username.is_empty() {
            return VerifyOutcome {
                valid: false,
                message: "No username provided".to_owned(),
                rate_limited: false,
            };
        }
        let response = self
            .http
            .get(format!(
                "{}/1/user/{}/listen-count",
                self.base_url,
                encode_segment(username)
            ))
            .timeout(Duration::from_secs(REQUEST_TIMEOUT_SECS))
            .send()
            .await;
        let response = match response {
            Ok(response) => response,
            Err(error) => return transport_outcome(&error),
        };
        let status = response.status().as_u16();
        if status == 429 {
            return VerifyOutcome {
                valid: false,
                message:
                    "ListenBrainz is temporarily rate-limiting this server. Try again shortly."
                        .to_owned(),
                rate_limited: true,
            };
        }
        if status == 404 {
            return VerifyOutcome {
                valid: false,
                message: format!("User '{username}' not found"),
                rate_limited: false,
            };
        }
        if status != 200 {
            return unavailable();
        }
        let bytes = match response.bytes().await {
            Ok(bytes) => bytes,
            Err(_) => return unavailable(),
        };
        let body: serde_json::Value = match serde_json::from_slice(&bytes) {
            Ok(body) => body,
            Err(_) => return unavailable(),
        };
        let count = body
            .get("payload")
            .and_then(|payload| payload.get("count"))
            .and_then(|count| count.as_i64())
            .unwrap_or(0);
        VerifyOutcome {
            valid: true,
            message: format!("User found with {} listens", grouped_count(count)),
            rate_limited: false,
        }
    }
}

impl ListenBrainzVerifier for HttpListenBrainzVerifier {
    fn verify<'a>(
        &'a self,
        username: &'a str,
        token: &'a str,
    ) -> futures_util::future::BoxFuture<'a, VerifyOutcome> {
        Box::pin(async move {
            if token.is_empty() {
                self.verify_username(username).await
            } else {
                self.verify_token(token, username).await
            }
        })
    }
}
