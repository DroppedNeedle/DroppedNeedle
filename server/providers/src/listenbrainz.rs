//! ListenBrainz client.
//!
//! The core reads (listens, recording and release-group metadata,
//! popularity, LB Radio), the credential check behind Settings and account
//! linking, and listen submission for scrobble forwarding. The wire shapes
//! are in [`models`], each noting what it was checked against.
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
//! Failures are typed [`ProviderError`]s. A 204 (ListenBrainz is still
//! computing) reads as an empty result; a body that does not decode is a
//! [`ProviderError::Payload`], so a changed answer never reads as "zero
//! listens".

use std::collections::HashMap;
use std::time::Duration;

use serde::de::DeserializeOwned;

use crate::RequestPriority;
use crate::cache::{CacheKey, Policy, ProviderCacheStore, Source};
use crate::degradation::{IntegrationStatus, record_current};
use crate::error::{ProviderError, TransportKind, retry_delay_from_headers};
use crate::executor::{Answer, Backoff, Executor, Reply, Request, Spec, Url, standard_classify};

use self::models::{
    ListenCountAnswer, ListenItem, ListensAnswer, RadioArtistAnswer, RecordingMetadataAnswer,
    ReleaseGroupEntry, ReleaseGroupMetadataAnswer, ReleaseGroupPopularityAnswer, TagItem,
    TopRecordingRow, TopRecordingsAnswer, TopReleaseGroupRow, TopReleaseGroupsAnswer,
    ValidateTokenAnswer,
};

pub mod models;
pub mod playlists;
pub mod stats;

/// Default API host.
pub const DEFAULT_BASE_URL: &str = "https://api.listenbrainz.org";
/// Local ceiling on release-group ids per metadata GET: a local safety
/// bound, not an upstream limit.
pub const RELEASE_GROUP_BATCH: usize = 25;
/// Local ceiling on recording ids per metadata POST.
pub const RECORDING_BATCH: usize = 50;
/// Local ceiling on ids per genre-batch call.
pub const MAX_GENRE_IDS: usize = 500;
/// Longest token accepted for a header.
pub const MAX_TOKEN_LEN: usize = 1024;
/// The provider name errors carry, and the ListenBrainz rate row.
pub const SOURCE: &str = "listenbrainz";
/// Largest ListenBrainz answer read.
// The popularity lists take no limit: Radiohead's top recordings are 1.5 MB
// (recorded 2026-11-02). An answer over the cap fails and is asked for again.
const MAX_BODY_BYTES: usize = 16 * 1024 * 1024;
/// The [`ProviderError::Invalid`] reason when the API root is not a usable
/// URL, so a caller can tell it from a token that cannot be sent.
pub const UNUSABLE_ADDRESS: &str = "the ListenBrainz address is not usable";

/// ListenBrainz on the wire. A policy block is [`ProviderError::Disabled`];
/// a 429 waits for `X-RateLimit-Reset-In`, else `Retry-After`, and never
/// more than an hour. The rate-limit headers are described at
/// https://listenbrainz.readthedocs.io/en/latest/users/api/index.html (read
/// 2026-10-18). A 429 with neither comes from the MetaBrainz gateway in
/// front of the service (seen 2026-10-20: an HTML 429 from openresty with
/// `x-mb-gateway`, for every read from one address), which does not clear
/// in seconds: it rests the bucket from a minute, doubling while they keep
/// coming, up to fifteen minutes.
pub static SPEC: Spec = Spec {
    source: SOURCE,
    health_key: SOURCE,
    timeout: Duration::from_secs(15),
    max_body: MAX_BODY_BYTES,
    retry_after_default: Backoff::Exponential {
        base: Duration::from_secs(60),
        max: Duration::from_secs(15 * 60),
    },
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
    // A refusal that also spends the window goes back to `send`, which
    // rests the bucket and then reads it as the refusal it is.
    if (400..500).contains(&status) && window_spent(reply).is_some() {
        return Ok(());
    }
    standard_classify(answer)
}

/// The wait an answer names when it says the rate window is spent
/// (`X-RateLimit-Remaining: 0` with a positive `X-RateLimit-Reset-In`).
fn window_spent(reply: &Reply) -> Option<Duration> {
    if reply.header("x-ratelimit-remaining").map(str::trim) != Some("0") {
        return None;
    }
    reply
        .header("x-ratelimit-reset-in")
        .and_then(|value| value.trim().parse::<f64>().ok())
        .filter(|seconds| seconds.is_finite() && *seconds > 0.0)
        .and_then(|seconds| Duration::try_from_secs_f64(seconds).ok())
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
/// token, while submissions and validation do. Its debug form never shows
/// the token.
#[derive(Clone, PartialEq, Eq, Default)]
pub struct ListenBrainzCredentials {
    /// ListenBrainz username.
    pub username: Option<String>,
    /// User token, sent as `Authorization: Token <token>`.
    pub user_token: Option<String>,
}

impl std::fmt::Debug for ListenBrainzCredentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ListenBrainzCredentials")
            .field("username", &self.username)
            .field(
                "user_token",
                &self.user_token.as_ref().map(|_| "<redacted>"),
            )
            .finish()
    }
}

/// One listen. Track and artist names are identity: a listen without them
/// is skipped, so placeholder text never flows into the library.
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

/// A genre tag on a release group. Curated genre entries carry
/// `genre_mbid`; ordinary folksonomy entries omit it.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct GenreTag {
    /// Tag text (never blank).
    pub tag: String,
    /// Tag count.
    pub count: i64,
    /// Curated genre MBID, when the entry is a curated genre.
    pub genre_mbid: Option<String>,
}

/// Release-group metadata: the tags of the group and of its artists.
#[derive(Debug, Clone, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub struct ReleaseGroupMetadata {
    /// Artist tags.
    pub artist_tags: Vec<GenreTag>,
    /// Release-group tags.
    pub release_group_tags: Vec<GenreTag>,
}

/// One top release-group row behind an artist popularity sum. The MBID is
/// identity; a row without a whole-number count is skipped (zero is a real
/// count); the display names are empty when the wire omits them.
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
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
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

/// The cache key of one recording's release group.
#[must_use]
pub fn recording_group_key(recording_mbid: &str) -> CacheKey {
    CacheKey::new(
        Source::ListenBrainz,
        &["recording_group", &recording_mbid.trim().to_lowercase()],
    )
}

/// The cache key of one release group's genre tags.
#[must_use]
pub fn release_group_genres_key(release_group_mbid: &str) -> CacheKey {
    CacheKey::new(
        Source::ListenBrainz,
        &[
            "release_group_genres",
            &release_group_mbid.trim().to_lowercase(),
        ],
    )
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
    /// per call). An empty username answers empty without touching the
    /// wire. Items missing track or artist names are skipped.
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
        let Some(answer) = self.read::<ListensAnswer>(request, priority).await? else {
            return Ok(Vec::new());
        };
        Ok(answer
            .payload
            .listens
            .into_iter()
            .filter_map(listen)
            .collect())
    }

    /// Resolve recording MBIDs to release-group MBIDs (`POST
    /// /1/metadata/recording/` with `inc=release`). Input dedupes, wire
    /// calls batch at 50, and MBIDs absent from the answer are left out.
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
            let Some(mut answer) = self
                .read_unconfirmed::<RecordingMetadataAnswer>(request, priority)
                .await?
            else {
                continue;
            };
            for mbid in batch {
                if let Some(release) = answer.remove(*mbid).and_then(|metadata| metadata.release) {
                    resolved.insert((*mbid).to_owned(), release.release_group_mbid);
                }
            }
        }
        Ok(resolved)
    }

    /// [`recording_release_groups`](Self::recording_release_groups) through
    /// `cache`, each recording kept on its own under
    /// [`recording_group_key`], so only recordings never seen go out.
    ///
    /// # Errors
    ///
    /// The typed failure of the first batch that failed.
    pub async fn cached_recording_groups(
        &self,
        cache: &ProviderCacheStore,
        recording_mbids: &[String],
        creds: &ListenBrainzCredentials,
        priority: RequestPriority,
    ) -> Result<HashMap<String, String>, ProviderError> {
        cache
            .fetch_each(
                recording_mbids,
                recording_group_key,
                Policy::RECORDING_TO_GROUP,
                |missing| async move {
                    self.recording_release_groups(&missing, creds, priority)
                        .await
                },
            )
            .await
    }

    /// Genre tags for release-group MBIDs (`GET /1/metadata/release_group/`
    /// with comma-separated ids and `inc=artist tag release`). Calls batch
    /// at 25 ids; an MBID ListenBrainz does not know is left out of its
    /// answer and gets an empty tag list. This endpoint is GET only: unlike
    /// the recording endpoint it answered a POST with 405 (seen 2026-07-21).
    ///
    /// # Errors
    ///
    /// [`ProviderError::Invalid`] for more than [`MAX_GENRE_IDS`] ids
    /// (nothing is sent), a payload error when a batch answers 204 or does
    /// not decode, otherwise the typed failure of the call.
    pub async fn release_group_genres(
        &self,
        release_group_mbids: &[String],
        creds: &ListenBrainzCredentials,
        priority: RequestPriority,
    ) -> Result<HashMap<String, ReleaseGroupMetadata>, ProviderError> {
        let unique = unique_trimmed(release_group_mbids);
        let mut known = self.known_genres(&unique, creds, priority).await?;
        Ok(unique
            .into_iter()
            .map(|mbid| {
                let metadata = known.remove(&mbid).unwrap_or_default();
                (mbid, metadata)
            })
            .collect())
    }

    /// [`release_group_genres`](Self::release_group_genres) through
    /// `cache`, each release group kept on its own under
    /// [`release_group_genres_key`], so only groups not held go out. An id
    /// the answer leaves out is kept as "nothing there", for the miss time,
    /// rather than as a group with no tags.
    ///
    /// # Errors
    ///
    /// The typed failure of the first batch that failed.
    pub async fn cached_release_group_genres(
        &self,
        cache: &ProviderCacheStore,
        release_group_mbids: &[String],
        creds: &ListenBrainzCredentials,
        priority: RequestPriority,
    ) -> Result<HashMap<String, ReleaseGroupMetadata>, ProviderError> {
        cache
            .fetch_each(
                release_group_mbids,
                release_group_genres_key,
                Policy::LISTENBRAINZ_RELEASE_GROUP,
                |missing| async move {
                    self.known_genres(&unique_trimmed(&missing), creds, priority)
                        .await
                },
            )
            .await
    }

    /// Genre tags of the release groups ListenBrainz knows among `unique`;
    /// the ones it left out are absent from the map.
    async fn known_genres(
        &self,
        unique: &[String],
        creds: &ListenBrainzCredentials,
        priority: RequestPriority,
    ) -> Result<HashMap<String, ReleaseGroupMetadata>, ProviderError> {
        if unique.len() > MAX_GENRE_IDS {
            return Err(invalid("too many release groups for one genre lookup"));
        }
        let mut resolved: HashMap<String, ReleaseGroupMetadata> = HashMap::new();
        let mut sorted = unique.to_vec();
        sorted.sort();
        for batch in sorted.chunks(RELEASE_GROUP_BATCH) {
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
            let Some(mut answer) = self
                .read::<ReleaseGroupMetadataAnswer>(request, priority)
                .await?
            else {
                return Err(ProviderError::Payload {
                    provider: SOURCE,
                    kind: crate::PayloadKind::Contract,
                    message: "ListenBrainz returned no release-group metadata".to_owned(),
                });
            };
            for mbid in batch {
                if let Some(entry) = answer.remove(mbid) {
                    resolved.insert(mbid.clone(), release_group_metadata(entry));
                }
            }
        }
        Ok(resolved)
    }

    /// Listen counts for release-group MBIDs (`POST
    /// /1/popularity/release-group`). A 204 yields an empty map with a note
    /// and a malformed answer a payload error, never "zero listens"; a row
    /// without a whole-number count is left out. A well-formed empty list
    /// is legitimate.
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
        let rows = self
            .read_unconfirmed::<ReleaseGroupPopularityAnswer>(request, priority)
            .await?
            .unwrap_or_default();
        // A zero count is kept; a blank id is skipped.
        Ok(rows
            .into_iter()
            .filter(|row| !row.release_group_mbid.trim().is_empty())
            .filter_map(|row| Some((row.release_group_mbid, row.total_listen_count?)))
            .collect())
    }

    /// Top release groups behind one artist's popularity sum (`GET
    /// /1/popularity/top-release-groups-for-artist/{mbid}`). Rows whose count
    /// is null are skipped; a malformed answer is a payload error, and a 204
    /// reads as empty with a note, so neither is kept as "nothing there".
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
            .read_unconfirmed::<TopReleaseGroupsAnswer>(request, priority)
            .await?
            .unwrap_or_default()
            .into_iter()
            .take(count)
            .filter_map(top_release_group)
            .collect())
    }

    /// An artist's most played recordings
    /// (`GET /1/popularity/top-recordings-for-artist/{mbid}`), most played
    /// first. Rows whose title or count is null are skipped; a malformed
    /// answer is a payload error, never "zero listens" rows.
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
            .read_unconfirmed::<TopRecordingsAnswer>(request, priority)
            .await?
            .unwrap_or_default()
            .into_iter()
            .filter_map(top_recording)
            .take(count)
            .collect())
    }

    /// Artists similar to one artist, from LB Radio's artist mode
    /// (`GET /1/lb-radio/artist/{mbid}` in `easy` mode). The reply maps
    /// each similar artist's MBID to a sample of its recordings; the seed
    /// artist is dropped and the rest sort by summed listens.
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
        let Some(entries) = self
            .read_unconfirmed::<RadioArtistAnswer>(request, priority)
            .await?
        else {
            return Ok(Vec::new());
        };
        let mut similar: Vec<SimilarArtist> = entries
            .into_iter()
            .filter(|(mbid, _)| {
                super::musicbrainz::is_valid_mbid(mbid) && !mbid.eq_ignore_ascii_case(artist_mbid)
            })
            .map(|(mbid, recordings)| SimilarArtist {
                artist_name: recordings
                    .first()
                    .and_then(|first| first.similar_artist_name.clone())
                    .unwrap_or_default(),
                listen_count: recordings
                    .iter()
                    .filter_map(|recording| recording.total_listen_count)
                    .sum(),
                artist_mbid: mbid,
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
            .map_err(|_| invalid(UNUSABLE_ADDRESS))?;
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
    /// off. An answer that does not decode gives no verdict.
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
        let answer = match reply.and_then(|reply| decode::<ValidateTokenAnswer>(&reply)) {
            Ok(answer) => answer,
            Err(ProviderError::Unauthorized { .. } | ProviderError::Forbidden { .. }) => {
                return invalid_token();
            }
            Err(error) => return VerifyOutcome::failed(&error),
        };
        if !answer.valid {
            return invalid_token();
        }
        let who = answer.user_name.as_deref().unwrap_or(username);
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
        let count = match reply.and_then(|reply| decode::<ListenCountAnswer>(&reply)) {
            Ok(answer) => answer.payload.count,
            Err(ProviderError::NotFound { .. }) => {
                return VerifyOutcome::invalid(format!("User '{username}' not found"));
            }
            Err(error) => return VerifyOutcome::failed(&error),
        };
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
            .map_err(|_| invalid(UNUSABLE_ADDRESS))?
            .header("accept", "application/json")
            .header("content-type", "application/json");
        with_headers(request, creds.user_token.as_deref())
    }

    /// A POST of a JSON document under the API root, with the headers
    /// every call carries.
    fn post(
        &self,
        segments: &[&str],
        body: &impl serde::Serialize,
        creds: &ListenBrainzCredentials,
    ) -> Result<Request, ProviderError> {
        let json = serde_json::to_vec(body).map_err(|_| invalid("the request did not encode"))?;
        let request = Request::post_json(self.base()?, segments, json)
            .map_err(|_| invalid(UNUSABLE_ADDRESS))?
            .header("accept", "application/json");
        with_headers(request, creds.user_token.as_deref())
    }

    /// Send one request. An answer that says the rate window is spent,
    /// a refusal included, rests the bucket until the window resets, as
    /// the API docs ask.
    async fn send(
        &self,
        request: Request,
        priority: RequestPriority,
    ) -> Result<Reply, ProviderError> {
        let reply = self.executor.send(request, priority).await?;
        if let Some(wait) = window_spent(&reply) {
            self.executor
                .limiter()
                .cool_down(wait.min(SPEC.retry_after_cap));
        }
        // A refusal `classify` passed back for its headers (no other 4xx
        // gets here). The standard rules read the time only for 429 and
        // 503, and neither is passed back.
        if (400..500).contains(&reply.status()) {
            standard_classify(&Answer {
                reply: &reply,
                now_unix: 0,
            })?;
        }
        Ok(reply)
    }

    /// Send one read and decode its body as `T`. `None` is a 204:
    /// ListenBrainz is still computing.
    async fn read<T: DeserializeOwned>(
        &self,
        request: Request,
        priority: RequestPriority,
    ) -> Result<Option<T>, ProviderError> {
        let reply = self.send(request, priority).await?;
        if reply.status() == 204 {
            return Ok(None);
        }
        decode(&reply).map(Some)
    }

    /// [`read`](Self::read) for an answer whose emptiness gets kept. A 204
    /// carries a note, so nothing is stored.
    async fn read_unconfirmed<T: DeserializeOwned>(
        &self,
        request: Request,
        priority: RequestPriority,
    ) -> Result<Option<T>, ProviderError> {
        let answer = self.read(request, priority).await?;
        if answer.is_none() {
            record_current(SOURCE, IntegrationStatus::Degraded, false);
        }
        Ok(answer)
    }

    fn base(&self) -> Result<&Url, ProviderError> {
        self.base.as_ref().ok_or_else(|| invalid(UNUSABLE_ADDRESS))
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

/// Decode `reply` as `T`. A body that does not decode is a payload error,
/// noted for the request.
fn decode<T: DeserializeOwned>(reply: &Reply) -> Result<T, ProviderError> {
    reply
        .json::<T>()
        .inspect_err(|error| note_shape(&error.to_string()))
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

/// Non-blank trimmed text, or `None`.
fn non_blank(text: Option<String>) -> Option<String> {
    text.map(|text| text.trim().to_owned())
        .filter(|text| !text.is_empty())
}

/// One listen. Recording and release MBIDs prefer the MBID mapping and fall
/// back to the additional info.
fn listen(item: ListenItem) -> Option<Listen> {
    let meta = item.track_metadata;
    let track_name = meta.track_name.filter(|name| !name.trim().is_empty())?;
    let artist_name = meta.artist_name.filter(|name| !name.trim().is_empty())?;
    let mapping = meta.mbid_mapping.unwrap_or_default();
    let additional = meta.additional_info.unwrap_or_default();
    Some(Listen {
        track_name,
        artist_name,
        listened_at: item.listened_at,
        recording_mbid: non_blank(mapping.recording_mbid).or(additional.recording_mbid),
        release_name: meta.release_name,
        release_mbid: non_blank(mapping.release_mbid).or(additional.release_mbid),
        artist_mbids: mapping.artist_mbids,
    })
}

/// One artist top-release-group row: a blank MBID or a missing count skips
/// it; the display names tolerate absence.
fn top_release_group(row: TopReleaseGroupRow) -> Option<TopReleaseGroup> {
    let mbid = row.release_group_mbid.trim();
    if mbid.is_empty() {
        return None;
    }
    Some(TopReleaseGroup {
        release_group_mbid: mbid.to_owned(),
        name: row
            .release_group
            .and_then(|group| group.name)
            .unwrap_or_default(),
        artist_name: row
            .artist
            .and_then(|artist| artist.name)
            .unwrap_or_default(),
        listen_count: row.total_listen_count?,
    })
}

/// One top-recording row: a non-blank title and a count are needed,
/// everything else is optional display data.
fn top_recording(row: TopRecordingRow) -> Option<TopRecording> {
    Some(TopRecording {
        title: row.recording_name?,
        artist_name: non_blank(row.artist_name).unwrap_or_default(),
        listen_count: row.total_listen_count?,
        recording_mbid: non_blank(row.recording_mbid),
        release_name: non_blank(row.release_name),
        release_mbid: non_blank(row.release_mbid),
    })
}

/// One genre tag; a blank tag is skipped.
fn genre_tag(item: TagItem) -> Option<GenreTag> {
    if item.tag.trim().is_empty() {
        return None;
    }
    Some(GenreTag {
        tag: item.tag,
        count: item.count,
        genre_mbid: non_blank(item.genre_mbid),
    })
}

/// The tags of one release group and its artists.
/// The ids once each, trimmed, blanks dropped, in first-seen order.
fn unique_trimmed(ids: &[String]) -> Vec<String> {
    let mut unique: Vec<String> = Vec::new();
    for mbid in ids {
        let trimmed = mbid.trim();
        if !trimmed.is_empty() && !unique.iter().any(|known| known == trimmed) {
            unique.push(trimmed.to_owned());
        }
    }
    unique
}

fn release_group_metadata(entry: ReleaseGroupEntry) -> ReleaseGroupMetadata {
    let tags = entry.tag.unwrap_or_default();
    ReleaseGroupMetadata {
        artist_tags: tags.artist.into_iter().filter_map(genre_tag).collect(),
        release_group_tags: tags
            .release_group
            .into_iter()
            .filter_map(genre_tag)
            .collect(),
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

/// Render a count with thousands separators.
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
