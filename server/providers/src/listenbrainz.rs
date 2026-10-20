//! ListenBrainz client.
//!
//! Ports the read paths of v2's ListenBrainz repository and its wire
//! models, including the live-verified metadata surface
//! (`listenbrainz_MANAGEMENT_API_NOTES.md`, verified against production on
//! 2026-07-21; the recording-metadata POST was verified on 2026-07-17), the
//! credential check behind Settings and account linking, and listen
//! submission for scrobble forwarding.
//!
//! Every request goes through the one ListenBrainz [`Executor`], which
//! paces at the `listenbrainz` row of the rate table: reads and scrobble
//! forwarding share that bucket, so one install never sends more than one
//! request a second. Each call names its priority.
//!
//! Every call sends `Accept: application/json`, plus
//! `Authorization: Token <token>` whenever a token is given, and every
//! token is header-safety checked before it goes near the wire.
//! A token that fails the check is
//! refused before sending.
//!
//! Failures are typed [`ProviderError`]s. An empty answer (204, or a body
//! that does not decode, which is noted for the request) reads as an empty
//! result, so an outage never reads as "zero listens".

use std::collections::HashMap;
use std::time::Duration;

use crate::RequestPriority;
use crate::degradation::{IntegrationStatus, record_current};
use crate::error::{ProviderError, TransportKind, retry_delay_from_headers};
use crate::executor::{Answer, Backoff, Executor, Reply, Request, Spec, Url, standard_classify};

pub mod playlists;
pub mod stats;

/// Default API host.
pub const DEFAULT_BASE_URL: &str = "https://api.listenbrainz.org";
/// Local ceiling on release-group ids per metadata GET. The notes stress this
/// is a local safety bound, not an upstream claim (v2 batches of 25).
pub const RELEASE_GROUP_BATCH: usize = 25;
/// Local ceiling on recording ids per metadata POST (v2 batches of 50).
pub const RECORDING_BATCH: usize = 50;
/// Local ceiling on ids per genre-batch call (v2 raises past 500).
pub const MAX_GENRE_IDS: usize = 500;
/// Longest token accepted for a header.
pub const MAX_TOKEN_LEN: usize = 1024;
/// The provider name errors carry, and the ListenBrainz rate row.
pub const SOURCE: &str = "listenbrainz";
/// Largest ListenBrainz answer read.
const MAX_BODY_BYTES: usize = 4 * 1024 * 1024;

/// ListenBrainz on the wire. A policy block is [`ProviderError::Disabled`];
/// a 429 waits for `X-RateLimit-Reset-In`, else `Retry-After`, else two
/// seconds, and never more than
/// an hour. The rate-limit headers are
/// described at
/// https://listenbrainz.readthedocs.io/en/latest/users/api/index.html (read
/// 2026-10-18).
pub static SPEC: Spec = Spec {
    source: SOURCE,
    health_key: SOURCE,
    timeout: Duration::from_secs(15),
    max_body: MAX_BODY_BYTES,
    retry_after_default: Backoff::Fixed(Duration::from_secs(2)),
    retry_after_cap: Duration::from_secs(3600),
    classify,
};

/// The ListenBrainz hook over the standard rules.
fn classify(answer: &Answer<'_>) -> Result<(), ProviderError> {
    let reply = answer.reply;
    let status = reply.status();
    if is_policy_block(status, &String::from_utf8_lossy(reply.bytes())) {
        return Err(ProviderError::Disabled {
            provider: SOURCE,
            message: format!("ListenBrainz switched this endpoint off upstream ({status})"),
        });
    }
    if status == 429 {
        return Err(ProviderError::RateLimited {
            provider: SOURCE,
            retry_after: reset_in(reply, answer.now_unix).or_else(|| answer.retry_after()),
        });
    }
    standard_classify(answer)
}

/// Whether this answer is a deterministic upstream policy block rather
/// than an ordinary error: the popularity
/// feature-flag 500, and the anti-scraper 401 added in 2026-07 when
/// ListenBrainz began gating anonymous popularity calls.
fn is_policy_block(status: u16, body: &str) -> bool {
    (status == 500 && body.contains("currently disabled"))
        || (status == 401
            && (body.contains("provide an Auth token") || body.contains("AI scrapers")))
}

/// The positive `X-RateLimit-Reset-In` wait in seconds, read by the shared
/// delta-seconds parser.
fn reset_in(reply: &Reply, now_unix: i64) -> Option<Duration> {
    let value = reply.header("x-ratelimit-reset-in")?;
    retry_delay_from_headers([("ratelimit-reset", value)], now_unix).filter(|wait| !wait.is_zero())
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

/// One listen. Track and artist names are required;
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

/// Release-group metadata. Only the
/// tag half is projected here; the artist/release summaries ride along in
/// the tolerant decode so the shape stays faithful to the live response.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ReleaseGroupMetadata {
    /// Artist tags.
    pub artist_tags: Vec<GenreTag>,
    /// Release-group tags.
    pub release_group_tags: Vec<GenreTag>,
}

/// One top release-group row behind an artist popularity sum (trimmed to the
/// popularity fields). The MBID is identity and required; the count is required
/// too (a zero count is a real answer, a non-integer count is malformed and
/// skipped); the display name tolerates absence rather than placeholder text.
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

/// One of an artist's most played recordings.
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

/// One similar artist from LB Radio. The
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

/// ListenBrainz client over the ListenBrainz executor. Built once with the
/// providers; every call names its priority.
#[derive(Debug, Clone)]
pub struct ListenBrainzClient {
    executor: Executor,
    base: Option<Url>,
}

impl ListenBrainzClient {
    /// The client sending through `executor` to the API root at `base_url`.
    #[must_use]
    pub fn new(executor: Executor, base_url: &str) -> Self {
        let base = Url::parse(base_url.trim())
            .ok()
            .filter(|url| matches!(url.scheme(), "http" | "https") && !url.cannot_be_a_base());
        if base.is_none() {
            tracing::error!("the ListenBrainz address is not an http or https URL");
        }
        Self { executor, base }
    }

    /// The executor every ListenBrainz request goes through.
    #[must_use]
    pub fn executor(&self) -> &Executor {
        &self.executor
    }

    /// Recent listens for a user (`GET /1/user/{user}/listens`, at most 100
    /// per call). An empty username answers empty without touching the wire
    /// (v2). Items missing track or artist names are skipped.
    ///
    /// # Errors
    ///
    /// The typed failure of the call.
    pub async fn user_listens(
        &self,
        username: &str,
        count: u32,
        creds: &ListenBrainzCredentials,
        priority: RequestPriority,
    ) -> Result<Vec<Listen>, ProviderError> {
        if username.is_empty() {
            return Ok(Vec::new());
        }
        let count = count.min(100).to_string();
        let request = self.get(
            &["1", "user", username, "listens"],
            &[("count", &count)],
            creds,
        )?;
        let Some(payload) = self.read(request, priority).await? else {
            return Ok(Vec::new());
        };
        let listens = payload
            .get("payload")
            .and_then(|payload| payload.get("listens"))
            .and_then(serde_json::Value::as_array)
            .cloned()
            .unwrap_or_default();
        Ok(listens.iter().filter_map(parse_listen).collect())
    }

    /// Resolve recording MBIDs to release-group MBIDs (`POST
    /// /1/metadata/recording/` with `inc=release`; live-verified on
    /// 2026-07-17, returns an object keyed by recording MBID whose release
    /// object carries `release_group_mbid`). Input dedupes, wire calls batch
    /// at 50, and MBIDs absent from the answer are simply left out.
    ///
    /// # Errors
    ///
    /// The typed failure of the first batch that failed.
    pub async fn recording_release_groups(
        &self,
        recording_mbids: &[String],
        creds: &ListenBrainzCredentials,
        priority: RequestPriority,
    ) -> Result<HashMap<String, String>, ProviderError> {
        let mut unique: Vec<&str> = Vec::new();
        for mbid in recording_mbids {
            if !mbid.is_empty() && !unique.contains(&mbid.as_str()) {
                unique.push(mbid.as_str());
            }
        }
        unique.sort_unstable();
        let mut resolved: HashMap<String, String> = HashMap::new();
        for batch in unique.chunks(RECORDING_BATCH) {
            let body = serde_json::json!({"recording_mbids": batch, "inc": "release"});
            let request = self.post(&["1", "metadata", "recording"], &body, creds)?;
            // A read sent as POST: sending it twice is safe.
            let request = request.trailing_slash().idempotent(true);
            // A non-object payload is noted and skips the batch (v2);
            // results gathered so far are kept.
            let Some(payload) = self
                .read(request, priority)
                .await?
                .and_then(|payload| payload.as_object().cloned())
            else {
                note_shape("ListenBrainz returned no recording metadata");
                continue;
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
        Ok(resolved)
    }

    /// Genre tags for release-group MBIDs from the live-verified GET-only
    /// endpoint (`GET /1/metadata/release_group/` with comma-separated ids
    /// and `inc=artist tag release`; verified against production on
    /// 2026-07-21). Calls batch at 25 ids; a valid-but-unknown MBID answers
    /// HTTP 200 with `{}` and yields an empty tag list for that id (v2).
    /// POST must never be used here: the live notes record that POST answers
    /// 405, and the recording endpoint's POST support must not be
    /// generalized to release groups.
    ///
    /// # Errors
    ///
    /// [`ProviderError::Invalid`] for more than [`MAX_GENRE_IDS`] ids
    /// (nothing is sent), a payload error when a batch carries no metadata
    /// object, otherwise the typed failure of the call.
    pub async fn release_group_genres(
        &self,
        release_group_mbids: &[String],
        creds: &ListenBrainzCredentials,
        priority: RequestPriority,
    ) -> Result<HashMap<String, ReleaseGroupMetadata>, ProviderError> {
        let mut unique: Vec<String> = Vec::new();
        for mbid in release_group_mbids {
            let trimmed = mbid.trim();
            if !trimmed.is_empty() && !unique.iter().any(|known| known == trimmed) {
                unique.push(trimmed.to_owned());
            }
        }
        if unique.len() > MAX_GENRE_IDS {
            return Err(invalid("too many release groups for one genre lookup"));
        }
        let mut resolved: HashMap<String, ReleaseGroupMetadata> = HashMap::new();
        unique.sort();
        for batch in unique.chunks(RELEASE_GROUP_BATCH) {
            let ids = batch.join(",");
            let request = self
                .get(
                    &["1", "metadata", "release_group"],
                    &[
                        ("release_group_mbids", ids.as_str()),
                        ("inc", "artist tag release"),
                    ],
                    creds,
                )?
                .trailing_slash();
            // No payload at all is an upstream failure, not an empty answer.
            let Some(payload) = self
                .read(request, priority)
                .await?
                .and_then(|payload| payload.as_object().cloned())
            else {
                return Err(ProviderError::Payload {
                    provider: SOURCE,
                    kind: crate::PayloadKind::Contract,
                    message: "ListenBrainz returned no release-group metadata".to_owned(),
                });
            };
            for mbid in batch {
                let metadata = payload
                    .get(mbid)
                    .map(parse_release_group_metadata)
                    .unwrap_or_default();
                resolved.insert(mbid.clone(), metadata);
            }
        }
        Ok(resolved)
    }

    /// Listen counts for release-group MBIDs (`POST
    /// /1/popularity/release-group`). A malformed or absent answer yields
    /// an empty map, because an outage must never read as "zero listens"
    /// (v2's poisoning guards). A well-formed empty list is legitimate.
    ///
    /// # Errors
    ///
    /// The typed failure of the call.
    pub async fn release_group_popularity(
        &self,
        release_group_mbids: &[String],
        creds: &ListenBrainzCredentials,
        priority: RequestPriority,
    ) -> Result<HashMap<String, i64>, ProviderError> {
        if release_group_mbids.is_empty() {
            return Ok(HashMap::new());
        }
        let mut unique: Vec<&str> = Vec::new();
        for mbid in release_group_mbids {
            if !unique.contains(&mbid.as_str()) {
                unique.push(mbid.as_str());
            }
        }
        unique.sort_unstable();
        let body = serde_json::json!({"release_group_mbids": unique});
        // A read sent as POST: sending it twice is safe.
        let request = self
            .post(&["1", "popularity", "release-group"], &body, creds)?
            .idempotent(true);
        let mut counts: HashMap<String, i64> = HashMap::new();
        let Some(items) = self
            .read(request, priority)
            .await?
            .and_then(|payload| payload.as_array().cloned())
        else {
            return Ok(counts);
        };
        for item in &items {
            let mbid = item
                .get("release_group_mbid")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("");
            // v2 keeps a zero count (`count is not None`) but skips a blank
            // id; a non-integer count is malformed and skipped.
            if mbid.is_empty() {
                continue;
            }
            if let Some(count) = item.get("total_listen_count").and_then(count_int) {
                counts.insert(mbid.to_owned(), count);
            }
        }
        Ok(counts)
    }

    /// Top release groups behind one artist's popularity sum (`GET
    /// /1/popularity/top-release-groups-for-artist/{mbid}`, v2
    /// `get_artist_top_release_groups`). Malformed items are skipped and a
    /// malformed payload reads as empty, mirroring the popularity batch.
    ///
    /// # Errors
    ///
    /// The typed failure of the call.
    pub async fn artist_top_release_groups(
        &self,
        artist_mbid: &str,
        count: usize,
        creds: &ListenBrainzCredentials,
        priority: RequestPriority,
    ) -> Result<Vec<TopReleaseGroup>, ProviderError> {
        let artist_mbid = artist_mbid.trim();
        if artist_mbid.is_empty() || count == 0 {
            return Ok(Vec::new());
        }
        let request = self.get(
            &[
                "1",
                "popularity",
                "top-release-groups-for-artist",
                artist_mbid,
            ],
            &[],
            creds,
        )?;
        Ok(self
            .read_array(request, priority)
            .await?
            .iter()
            .take(count)
            .filter_map(parse_top_release_group)
            .collect())
    }

    /// An artist's most played recordings
    /// (`GET /1/popularity/top-recordings-for-artist/{mbid}`), most played
    /// first. Malformed rows are skipped; a malformed payload reads as an
    /// empty answer, never as "zero listens" rows.
    ///
    /// # Errors
    ///
    /// The typed failure of the call.
    pub async fn artist_top_recordings(
        &self,
        artist_mbid: &str,
        count: usize,
        creds: &ListenBrainzCredentials,
        priority: RequestPriority,
    ) -> Result<Vec<TopRecording>, ProviderError> {
        let artist_mbid = artist_mbid.trim();
        if artist_mbid.is_empty() || count == 0 {
            return Ok(Vec::new());
        }
        let request = self.get(
            &["1", "popularity", "top-recordings-for-artist", artist_mbid],
            &[],
            creds,
        )?;
        Ok(self
            .read_array(request, priority)
            .await?
            .iter()
            .filter_map(parse_top_recording)
            .take(count)
            .collect())
    }

    /// Artists similar to one artist, from LB Radio's artist mode
    /// (`GET /1/lb-radio/artist/{mbid}` with v2's `easy` parameters). The
    /// reply maps each similar artist's MBID to a sample of its recordings;
    /// the seed artist is dropped and the rest sort by summed listens.
    ///
    /// # Errors
    ///
    /// The typed failure of the call.
    pub async fn similar_artists(
        &self,
        artist_mbid: &str,
        max_similar: usize,
        creds: &ListenBrainzCredentials,
        priority: RequestPriority,
    ) -> Result<Vec<SimilarArtist>, ProviderError> {
        let artist_mbid = artist_mbid.trim();
        if artist_mbid.is_empty() || max_similar == 0 {
            return Ok(Vec::new());
        }
        let max = max_similar.to_string();
        let request = self.get(
            &["1", "lb-radio", "artist", artist_mbid],
            &[
                ("mode", "easy"),
                ("max_similar_artists", max.as_str()),
                ("max_recordings_per_artist", "5"),
                ("pop_begin", "0"),
                ("pop_end", "100"),
            ],
            creds,
        )?;
        let Some(payload) = self.read(request, priority).await? else {
            return Ok(Vec::new());
        };
        let Some(entries) = payload.as_object() else {
            return Ok(Vec::new());
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
        Ok(similar)
    }

    /// Submit listens for the token's account (`POST /1/submit-listens`).
    /// `listens` is the JSON document. Sent once: a submission is never
    /// repeated here, the caller's queue decides about another try.
    ///
    /// # Errors
    ///
    /// [`ProviderError::Invalid`] for a token that cannot ride in a header
    /// (nothing is sent), otherwise the typed failure of the call.
    pub async fn submit_listens(
        &self,
        token: &str,
        listens: Vec<u8>,
        priority: RequestPriority,
    ) -> Result<(), ProviderError> {
        let request = Request::post_json(self.base()?, &["1", "submit-listens"], listens)
            .map_err(|_| invalid("the ListenBrainz address is not usable"))?;
        if token.is_empty() {
            return Err(invalid("a submission needs a token"));
        }
        let request = with_headers(request.header("accept", "application/json"), Some(token))?;
        self.send(request, priority).await.map(|_| ())
    }

    /// Check one username and token pair: with a token, the token
    /// (`GET /1/validate-token`); without one, that the username exists
    /// (`GET /1/user/{user}/listen-count`). 401 and 403 mean an invalid
    /// token, 404 an unknown user, and a rate limit asks the caller to back
    /// off (v2's repository verify methods).
    pub async fn verify(
        &self,
        username: &str,
        token: &str,
        priority: RequestPriority,
    ) -> VerifyOutcome {
        if token.is_empty() {
            self.verify_username(username, priority).await
        } else {
            self.verify_token(token, username, priority).await
        }
    }

    async fn verify_token(
        &self,
        token: &str,
        username: &str,
        priority: RequestPriority,
    ) -> VerifyOutcome {
        let invalid_token = || VerifyOutcome::invalid("Token invalid or expired".to_owned());
        if !header_safe_token(token) {
            return invalid_token();
        }
        let creds = ListenBrainzCredentials {
            username: None,
            user_token: Some(token.to_owned()),
        };
        let reply = match self.get(&["1", "validate-token"], &[], &creds) {
            Ok(request) => self.send(request, priority).await,
            Err(error) => Err(error),
        };
        let body: serde_json::Value = match reply.and_then(|reply| reply.json()) {
            Ok(body) => body,
            Err(ProviderError::Unauthorized { .. } | ProviderError::Forbidden { .. }) => {
                return invalid_token();
            }
            Err(error) => return VerifyOutcome::failed(&error),
        };
        if body.get("valid").and_then(serde_json::Value::as_bool) != Some(true) {
            return invalid_token();
        }
        let who = body
            .get("user_name")
            .and_then(serde_json::Value::as_str)
            .unwrap_or(username);
        VerifyOutcome {
            valid: true,
            message: format!("Successfully connected as '{who}'"),
            rate_limited: false,
        }
    }

    async fn verify_username(&self, username: &str, priority: RequestPriority) -> VerifyOutcome {
        if username.is_empty() {
            return VerifyOutcome::invalid("No username provided".to_owned());
        }
        let reply = match self.get(
            &["1", "user", username, "listen-count"],
            &[],
            &ListenBrainzCredentials::default(),
        ) {
            Ok(request) => self.send(request, priority).await,
            Err(error) => Err(error),
        };
        let body: serde_json::Value = match reply.and_then(|reply| reply.json()) {
            Ok(body) => body,
            Err(ProviderError::NotFound { .. }) => {
                return VerifyOutcome::invalid(format!("User '{username}' not found"));
            }
            Err(error) => return VerifyOutcome::failed(&error),
        };
        let count = body
            .get("payload")
            .and_then(|payload| payload.get("count"))
            .and_then(serde_json::Value::as_i64)
            .unwrap_or(0);
        VerifyOutcome {
            valid: true,
            message: format!("User found with {} listens", grouped_count(count)),
            rate_limited: false,
        }
    }

    /// A GET of `segments` under the API root, with the headers every call
    /// carries.
    fn get(
        &self,
        segments: &[&str],
        query: &[(&str, &str)],
        creds: &ListenBrainzCredentials,
    ) -> Result<Request, ProviderError> {
        let request = Request::get(self.base()?, segments, query)
            .map_err(|_| invalid("the ListenBrainz address is not usable"))?
            .header("accept", "application/json")
            .header("content-type", "application/json");
        with_headers(request, creds.user_token.as_deref())
    }

    /// A POST of a JSON document under the API root, with the headers
    /// every call carries.
    fn post(
        &self,
        segments: &[&str],
        body: &serde_json::Value,
        creds: &ListenBrainzCredentials,
    ) -> Result<Request, ProviderError> {
        let json = serde_json::to_vec(body).map_err(|_| invalid("the request did not encode"))?;
        let request = Request::post_json(self.base()?, segments, json)
            .map_err(|_| invalid("the ListenBrainz address is not usable"))?
            .header("accept", "application/json");
        with_headers(request, creds.user_token.as_deref())
    }

    /// Send one request. An answer that says the rate window is spent
    /// rests the bucket until the window resets, as the API docs ask.
    async fn send(
        &self,
        request: Request,
        priority: RequestPriority,
    ) -> Result<Reply, ProviderError> {
        let reply = self.executor.send(request, priority).await?;
        if reply.header("x-ratelimit-remaining").map(str::trim) == Some("0")
            && let Some(wait) = reply
                .header("x-ratelimit-reset-in")
                .and_then(|value| value.trim().parse::<f64>().ok())
                .filter(|seconds| seconds.is_finite() && *seconds > 0.0)
                .and_then(|seconds| Duration::try_from_secs_f64(seconds).ok())
        {
            self.executor
                .limiter()
                .cool_down(wait.min(SPEC.retry_after_cap));
        }
        Ok(reply)
    }

    /// Send one read and decode its JSON body. `None` is an empty answer:
    /// a 204 (ListenBrainz is still computing), or a body that does not
    /// decode, which is noted for the request.
    async fn read(
        &self,
        request: Request,
        priority: RequestPriority,
    ) -> Result<Option<serde_json::Value>, ProviderError> {
        let reply = self.send(request, priority).await?;
        if reply.status() == 204 {
            return Ok(None);
        }
        match reply.json::<serde_json::Value>() {
            Ok(payload) => Ok(Some(payload)),
            Err(error) => {
                note_shape(&error.to_string());
                Ok(None)
            }
        }
    }

    /// Send one read whose answer is a JSON array. Anything else reads as
    /// an empty array.
    async fn read_array(
        &self,
        request: Request,
        priority: RequestPriority,
    ) -> Result<Vec<serde_json::Value>, ProviderError> {
        Ok(match self.read(request, priority).await? {
            Some(serde_json::Value::Array(items)) => items,
            _ => Vec::new(),
        })
    }

    fn base(&self) -> Result<&Url, ProviderError> {
        self.base
            .as_ref()
            .ok_or_else(|| invalid("the ListenBrainz address is not usable"))
    }
}

/// `request` with `Authorization: Token <token>` when a token is given.
/// A token that cannot ride in a header is refused before sending.
fn with_headers(request: Request, token: Option<&str>) -> Result<Request, ProviderError> {
    match token.filter(|token| !token.is_empty()) {
        None => Ok(request),
        Some(token) if header_safe_token(token) => {
            Ok(request.header("authorization", format!("Token {token}")))
        }
        Some(_) => Err(invalid("the token cannot be sent in a header")),
    }
}

/// An answer that did not have the expected shape: the service answered,
/// so it is noted for the request but never counted as an outage.
fn note_shape(cause: &str) {
    tracing::debug!(
        provider = SOURCE,
        cause,
        "ListenBrainz answer had no usable shape"
    );
    record_current(SOURCE, IntegrationStatus::Error, true);
}

fn invalid(reason: &'static str) -> ProviderError {
    ProviderError::Invalid {
        provider: SOURCE,
        reason,
    }
}

/// Whether a token may ride in a header: non-empty, at most 1024 chars, all
/// printable ASCII.
fn header_safe_token(token: &str) -> bool {
    if token.is_empty() || token.len() > MAX_TOKEN_LEN {
        return false;
    }
    token
        .chars()
        .all(|char| ('\u{21}'..='\u{7e}').contains(&char))
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

/// Parse one listen. Recording and release MBIDs prefer
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

/// Parse the release-group metadata object.
/// `tag` holds `artist` and
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
// username/token pair against the live API, through the one client.

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

impl VerifyOutcome {
    fn invalid(message: String) -> Self {
        Self {
            valid: false,
            message,
            rate_limited: false,
        }
    }

    /// A check that got no verdict: rate limited, unreachable or failing.
    fn failed(error: &ProviderError) -> Self {
        match error {
            ProviderError::RateLimited { .. } => Self {
                valid: false,
                message:
                    "ListenBrainz is temporarily rate-limiting this server. Try again shortly."
                        .to_owned(),
                rate_limited: true,
            },
            ProviderError::Transport {
                kind: TransportKind::Timeout,
                ..
            } => Self::invalid("Connection timed out".to_owned()),
            ProviderError::Transport {
                kind: TransportKind::Connect,
                ..
            } => Self::invalid("Could not connect to ListenBrainz".to_owned()),
            _ => Self::invalid(
                "ListenBrainz is temporarily unavailable. Try again shortly.".to_owned(),
            ),
        }
    }
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

/// A person waits on every check: linking an account or the Settings
/// Verify button.
impl ListenBrainzVerifier for ListenBrainzClient {
    fn verify<'a>(
        &'a self,
        username: &'a str,
        token: &'a str,
    ) -> futures_util::future::BoxFuture<'a, VerifyOutcome> {
        Box::pin(ListenBrainzClient::verify(
            self,
            username,
            token,
            RequestPriority::UserInitiated,
        ))
    }
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
