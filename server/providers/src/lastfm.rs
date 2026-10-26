//! Last.fm metadata client with per-user credentials.
//!
//! The read paths, the two signed account-linking calls and the signed
//! scrobble posts. Every answer decodes through the wire shapes in
//! `models`, where each endpoint notes what it was verified against.
//!
//! Credentials are per user, always. `LastFmCredentials` mirrors the
//! plaintext projection of the per-user store (`auth::users::models`
//! `LastFmConnection`; only the service layer holds decrypted values, in
//! memory). There is no global key pair, on purpose: every method takes
//! the calling user's credentials, and the client itself stores none.
//!
//! Every request goes through the one Last.fm [`Executor`], which paces at
//! the `lastfm` row of the rate table: reads, account linking and scrobble
//! forwarding share that bucket. Each call names its priority.
//!
//! Last.fm reports its errors as a JSON envelope (`{"error": n,
//! "message"}`), inside a 200 as often as not; the hook turns the code
//! into a typed [`ProviderError`] (see [`classify`]).

use std::time::Duration;

use serde::de::DeserializeOwned;

mod models;

use crate::RequestPriority;
use crate::degradation::{IntegrationStatus, record_current};
use crate::error::{MAX_RETRY_AFTER, PayloadKind, ProviderError};
use crate::executor::{
    Answer, Backoff, Executor, Reply, Request, Spec, Url, short, standard_classify,
};

/// Default API host.
pub const DEFAULT_BASE_URL: &str = "https://ws.audioscrobbler.com/2.0/";
/// Last.fm's documented rate-limit error code.
/// The management notes call out that this code was never provoked live on
/// purpose; it is decoded from the documented envelope and covered by a
/// scripted fake instead.
pub const ERROR_RATE_LIMITED: i64 = 29;
/// The rest after error 29.
pub const RATE_LIMIT_WAIT: Duration = Duration::from_secs(1);
/// The provider name errors carry, and the Last.fm rate row.
pub const SOURCE: &str = "lastfm";
/// Largest Last.fm answer read.
const MAX_BODY_BYTES: usize = 4 * 1024 * 1024;

/// Last.fm on the wire: the envelope codes become their variants, a 429
/// that names no wait rests the bucket a second.
pub static SPEC: Spec = Spec {
    source: SOURCE,
    health_key: SOURCE,
    timeout: Duration::from_secs(15),
    max_body: MAX_BODY_BYTES,
    retry_after_default: Backoff::Fixed(RATE_LIMIT_WAIT),
    retry_after_cap: MAX_RETRY_AFTER,
    classify,
};

/// The Last.fm hook over the standard rules. A 429 or a 5xx is standard.
/// Any other answer carrying the error envelope is read by its code,
/// whatever the status (the live invalid-key probe saw a 403 with error
/// 10; most errors come in a 200), per https://www.last.fm/api/errorcodes:
///
/// - 6, unknown entity: absence ([`ProviderError::NotFound`]);
/// - 29, rate limit: [`ProviderError::RateLimited`] for one second;
/// - 11 and 16, offline or temporary: [`ProviderError::Unavailable`];
/// - 4, 10, 13, 15 and 26 (key, signature, expired token, suspended key):
///   [`ProviderError::Unauthorized`];
/// - 9, 14 and 17 (session, unapproved token, login required):
///   [`ProviderError::Forbidden`];
/// - any other code: [`ProviderError::BadRequest`] with the status;
/// - an envelope with no usable code: [`ProviderError::Server`].
pub fn classify(answer: &Answer<'_>) -> Result<(), ProviderError> {
    let status = answer.reply.status();
    if status == 429 || status >= 500 {
        return standard_classify(answer);
    }
    let Some(code) = envelope_code(answer.reply.bytes()) else {
        return standard_classify(answer);
    };
    let provider = SOURCE;
    Err(match code {
        Some(6) => ProviderError::NotFound { provider },
        Some(ERROR_RATE_LIMITED) => ProviderError::RateLimited {
            provider,
            retry_after: Some(RATE_LIMIT_WAIT),
        },
        Some(11 | 16) => ProviderError::Unavailable {
            provider,
            retry_after: None,
        },
        Some(4 | 10 | 13 | 15 | 26) => ProviderError::Unauthorized { provider },
        Some(9 | 14 | 17) => ProviderError::Forbidden { provider },
        Some(_) => ProviderError::BadRequest { provider, status },
        None => ProviderError::Server { provider, status },
    })
}

/// The code of a Last.fm error envelope: `None` when the body is not one,
/// `Some(None)` when it is one without a usable code.
fn envelope_code(body: &[u8]) -> Option<Option<i64>> {
    if !body.windows(7).any(|window| window == b"\"error\"") {
        return None;
    }
    let envelope: models::WireError = serde_json::from_slice(body).ok()?;
    match envelope.error {
        models::WireErrorCode::Code(code) => Some(Some(code)),
        models::WireErrorCode::Other(_) => Some(None),
    }
}

/// One user's Last.fm credentials, plaintext in memory only. This mirrors the
/// decrypted projection of the `LastFmConnection` store record
/// (`api_key`, `shared_secret`, `username`, `session_key`); the store keeps
/// ciphertext at rest and only the service layer decrypts. Methods borrow
/// this per call so credentials can never leak across users.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct LastFmCredentials {
    /// The user's own Last.fm API key.
    pub api_key: String,
    /// The user's own shared secret (needed for signed calls).
    pub shared_secret: String,
    /// Linked Last.fm username, when a session was exchanged.
    pub username: Option<String>,
    /// Session key from `auth.getSession`, when linked.
    pub session_key: Option<String>,
}

/// An approved-token reply from `auth.getToken`. The token is required: v2
/// `parse_token` raises when it is absent, so a payload missing it fails
/// decode instead of yielding an empty success.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthToken {
    /// The token to approve in the browser.
    pub token: String,
}

/// A session reply from `auth.getSession`. Name and key are both required:
/// an answer missing either is a failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Session {
    /// Linked Last.fm username.
    pub name: String,
    /// Session key for signed calls.
    pub key: String,
    /// Subscriber flag.
    pub subscriber: i64,
}

/// A plain tag.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tag {
    /// Tag name.
    pub name: String,
    /// Tag URL.
    pub url: String,
}

/// A weighted tag from the genre surface.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WeightedTag {
    /// Tag name (never blank; v2 filters blank names before projecting).
    pub name: String,
    /// Weight, 0 through 100 on the live API.
    pub weight: i64,
}

/// Artist metadata. The name is required; counts and artwork stay lenient.
#[derive(Debug, Clone, PartialEq)]
pub struct ArtistInfo {
    /// Artist name.
    pub name: String,
    /// MusicBrainz artist id.
    pub mbid: Option<String>,
    /// Listener count.
    pub listeners: i64,
    /// Play count.
    pub playcount: i64,
    /// Last.fm URL.
    pub url: String,
    /// Biography summary.
    pub bio_summary: String,
    /// Full biography, when the payload carried one.
    pub bio_content: String,
    /// Tags.
    pub tags: Vec<Tag>,
    /// Similar artists.
    pub similar: Vec<SimilarArtist>,
}

/// Album metadata. The title is required.
#[derive(Debug, Clone, PartialEq)]
pub struct AlbumInfo {
    /// Album title.
    pub name: String,
    /// Album artist name.
    pub artist_name: String,
    /// MusicBrainz release id.
    pub mbid: Option<String>,
    /// Listener count.
    pub listeners: i64,
    /// Play count.
    pub playcount: i64,
    /// Last.fm URL.
    pub url: String,
    /// Artwork URL (extralarge preferred, else the last image).
    pub image_url: String,
    /// Wiki summary.
    pub summary: String,
    /// Tags.
    pub tags: Vec<Tag>,
    /// Tracks, when the payload listed any.
    pub tracks: Vec<AlbumTrack>,
}

/// One album track.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AlbumTrack {
    /// Track title.
    pub name: String,
    /// Duration in seconds.
    pub duration_secs: i64,
    /// Track rank.
    pub rank: i64,
    /// Last.fm URL.
    pub url: String,
}

/// A similar artist.
#[derive(Debug, Clone, PartialEq)]
pub struct SimilarArtist {
    /// Artist name.
    pub name: String,
    /// MusicBrainz artist id.
    pub mbid: Option<String>,
    /// Similarity score.
    pub score: f64,
    /// Last.fm URL.
    pub url: String,
}

/// One row of a top list: an artist's top tracks or albums, a user's top
/// albums, or the sitewide artist chart. The name is required; counts are
/// lenient.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TopItem {
    /// Track, album or (on the artist chart) artist name.
    pub name: String,
    /// Credited artist name; empty on the artist chart.
    pub artist_name: String,
    /// MusicBrainz id (recording, release or artist), when Last.fm knows one.
    pub mbid: Option<String>,
    /// Play count.
    pub playcount: i64,
    /// Artwork URL (extralarge preferred), empty when none was sent.
    pub image_url: String,
}

/// One scrobble from a user's recent tracks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecentTrack {
    /// Track title.
    pub name: String,
    /// Credited artist name.
    pub artist_name: String,
    /// Album title, empty when Last.fm has none.
    pub album_name: String,
    /// MusicBrainz release id of the album, when Last.fm knows it.
    pub album_mbid: Option<String>,
    /// Artwork URL (extralarge preferred), empty when none was sent.
    pub image_url: String,
}

/// Last.fm client over the Last.fm executor. Holds no credentials; every
/// method takes the calling user's and names its priority. Built once
/// with the providers; cheap to clone.
#[derive(Debug, Clone)]
pub struct LastFmClient {
    executor: Executor,
    base: Option<Url>,
}

impl LastFmClient {
    /// The client sending through `executor` to the API root at `base_url`.
    #[must_use]
    pub fn new(executor: Executor, base_url: &str) -> Self {
        let base = Url::parse(base_url.trim())
            .ok()
            .filter(|url| matches!(url.scheme(), "http" | "https") && !url.cannot_be_a_base());
        if base.is_none() {
            tracing::error!("the Last.fm address is not an http or https URL");
        }
        Self { executor, base }
    }

    /// The executor every Last.fm request goes through.
    #[must_use]
    pub fn executor(&self) -> &Executor {
        &self.executor
    }

    /// `auth.getToken` with the user's own API key, signed with the shared
    /// secret; no session yet.
    ///
    /// # Errors
    ///
    /// [`ProviderError::NotConfigured`] without a key or secret, otherwise
    /// the typed failure of the call or of its answer.
    pub async fn request_token(
        &self,
        creds: &LastFmCredentials,
        priority: RequestPriority,
    ) -> Result<AuthToken, ProviderError> {
        let wire: models::WireToken = self
            .read(
                self.call("auth.getToken", creds, &[], Signed::Yes)?,
                priority,
            )
            .await?;
        if wire.token.is_empty() {
            return Err(shape("Last.fm auth.getToken answer has no token"));
        }
        Ok(AuthToken { token: wire.token })
    }

    /// `auth.getSession` with the user's key pair and an approved token.
    ///
    /// # Errors
    ///
    /// [`ProviderError::Forbidden`] while the token is not approved (error
    /// 14), [`ProviderError::NotConfigured`] without a key or secret,
    /// otherwise the typed failure of the call or of its answer.
    pub async fn exchange_session(
        &self,
        creds: &LastFmCredentials,
        token: &str,
        priority: RequestPriority,
    ) -> Result<Session, ProviderError> {
        let wire: models::WireSessionReply = self
            .read(
                self.call("auth.getSession", creds, &[("token", token)], Signed::Yes)?,
                priority,
            )
            .await?;
        let (models::WireSessionReply::Nested { session }
        | models::WireSessionReply::Flat(session)) = wire;
        if session.name.is_empty() || session.key.is_empty() {
            return Err(shape("Last.fm auth.getSession answer has no name or key"));
        }
        Ok(Session {
            name: session.name,
            key: session.key,
            subscriber: count(session.subscriber.as_ref()),
        })
    }

    /// A signed form post for the session in `creds` whose answer matters
    /// only as success or failure (`track.updateNowPlaying`). Sent once: a
    /// post is never repeated here.
    ///
    /// # Errors
    ///
    /// [`ProviderError::NotConfigured`] without a key or secret, otherwise
    /// the typed failure of the call.
    pub async fn submit(
        &self,
        method: &str,
        creds: &LastFmCredentials,
        params: &[(&str, &str)],
        priority: RequestPriority,
    ) -> Result<(), ProviderError> {
        let request = self.call(method, creds, params, Signed::Post)?;
        self.send(request, priority).await.map(|_| ())
    }

    /// One signed `track.scrobble` for the session in `creds`, and what
    /// Last.fm did with it. Sent once: the caller's queue decides about
    /// another try. Last.fm answers a scrobble it turns away with a success
    /// that names why (see the models file), so a delivered request is not
    /// always a kept play.
    ///
    /// # Errors
    ///
    /// [`ProviderError::NotConfigured`] without a key or secret, a
    /// [`ProviderError::Payload`] when a success does not say what became
    /// of the play, otherwise the typed failure of the call.
    pub async fn scrobble(
        &self,
        creds: &LastFmCredentials,
        params: &[(&str, &str)],
        priority: RequestPriority,
    ) -> Result<ScrobbleAnswer, ProviderError> {
        let request = self.call("track.scrobble", creds, params, Signed::Post)?;
        let reply = self.send(request, priority).await?;
        let wire: models::WireScrobbleReply = reply
            .json()
            .inspect_err(|error| note_shape(&error.to_string()))?;
        Ok(scrobble_answer(&wire))
    }

    /// `artist.getInfo` by name or, when given, by MusicBrainz id. `None`
    /// when Last.fm does not know the artist (error 6).
    ///
    /// # Errors
    ///
    /// The typed failure of the call or of its answer.
    pub async fn artist_info(
        &self,
        creds: &LastFmCredentials,
        artist: &str,
        mbid: Option<&str>,
        priority: RequestPriority,
    ) -> Result<Option<ArtistInfo>, ProviderError> {
        let (key, value) = match mbid {
            Some(mbid) => ("mbid", mbid),
            None => ("artist", artist),
        };
        let Some(wire) = self
            .read_found::<models::WireArtistInfoReply>(
                "artist.getInfo",
                creds,
                &[(key, value)],
                priority,
            )
            .await?
        else {
            return Ok(None);
        };
        artist_info(wire.artist)
            .map(Some)
            .ok_or_else(|| shape("Last.fm artist.getInfo answer has no name"))
    }

    /// `album.getInfo` by artist plus title or, when given, by MusicBrainz
    /// id. `None` when Last.fm does not know the album (error 6).
    ///
    /// # Errors
    ///
    /// The typed failure of the call or of its answer.
    pub async fn album_info(
        &self,
        creds: &LastFmCredentials,
        artist: &str,
        album: &str,
        mbid: Option<&str>,
        priority: RequestPriority,
    ) -> Result<Option<AlbumInfo>, ProviderError> {
        let params: Vec<(&str, &str)> = match mbid {
            Some(mbid) => vec![("mbid", mbid)],
            None => vec![("artist", artist), ("album", album)],
        };
        let Some(wire) = self
            .read_found::<models::WireAlbumInfoReply>("album.getInfo", creds, &params, priority)
            .await?
        else {
            return Ok(None);
        };
        album_info(wire.album)
            .map(Some)
            .ok_or_else(|| shape("Last.fm album.getInfo answer has no name"))
    }

    /// `artist.getSimilar`, most similar first; empty for an unknown
    /// artist.
    ///
    /// # Errors
    ///
    /// The typed failure of the call.
    pub async fn similar_artists(
        &self,
        creds: &LastFmCredentials,
        artist: &str,
        mbid: Option<&str>,
        limit: u32,
        priority: RequestPriority,
    ) -> Result<Vec<SimilarArtist>, ProviderError> {
        let limit_text = limit.to_string();
        let params: Vec<(&str, &str)> = match mbid {
            Some(mbid) => vec![("mbid", mbid), ("limit", &limit_text)],
            None => vec![("artist", artist), ("limit", &limit_text)],
        };
        Ok(self
            .read_found::<models::WireSimilarReply>("artist.getSimilar", creds, &params, priority)
            .await?
            .map(|wire| similar_artists(&wire.similarartists))
            .unwrap_or_default())
    }

    /// `artist.getTopTracks`, most played first.
    ///
    /// # Errors
    ///
    /// The typed failure of the call.
    pub async fn artist_top_tracks(
        &self,
        creds: &LastFmCredentials,
        artist: &str,
        mbid: Option<&str>,
        limit: u32,
        priority: RequestPriority,
    ) -> Result<Vec<TopItem>, ProviderError> {
        let limit_text = limit.to_string();
        self.list_items(
            "artist.getTopTracks",
            creds,
            &artist_params(artist, mbid, &limit_text),
            priority,
        )
        .await
    }

    /// `artist.getTopAlbums`, most played first.
    ///
    /// # Errors
    ///
    /// The typed failure of the call.
    pub async fn artist_top_albums(
        &self,
        creds: &LastFmCredentials,
        artist: &str,
        mbid: Option<&str>,
        limit: u32,
        priority: RequestPriority,
    ) -> Result<Vec<TopItem>, ProviderError> {
        let limit_text = limit.to_string();
        self.list_items(
            "artist.getTopAlbums",
            creds,
            &artist_params(artist, mbid, &limit_text),
            priority,
        )
        .await
    }

    /// `chart.getTopArtists`: the sitewide artist chart, most played first.
    /// Each row's `name` is the artist.
    ///
    /// # Errors
    ///
    /// The typed failure of the call.
    pub async fn chart_top_artists(
        &self,
        creds: &LastFmCredentials,
        limit: u32,
        priority: RequestPriority,
    ) -> Result<Vec<TopItem>, ProviderError> {
        let limit_text = limit.to_string();
        self.list_items(
            "chart.getTopArtists",
            creds,
            &[("limit", &limit_text)],
            priority,
        )
        .await
    }

    /// `user.getTopAlbums` for one Last.fm user over `period` (`7day`,
    /// `1month`, `12month` or `overall`), most played first.
    ///
    /// # Errors
    ///
    /// The typed failure of the call.
    pub async fn user_top_albums(
        &self,
        creds: &LastFmCredentials,
        username: &str,
        period: &str,
        limit: u32,
        priority: RequestPriority,
    ) -> Result<Vec<TopItem>, ProviderError> {
        let limit_text = limit.to_string();
        self.list_items(
            "user.getTopAlbums",
            creds,
            &[
                ("user", username),
                ("period", period),
                ("limit", &limit_text),
            ],
            priority,
        )
        .await
    }

    /// `user.getTopArtists` for one Last.fm user over `period`, most played
    /// first. Each row's `name` is the artist.
    ///
    /// # Errors
    ///
    /// The typed failure of the call.
    pub async fn user_top_artists(
        &self,
        creds: &LastFmCredentials,
        username: &str,
        period: &str,
        limit: u32,
        priority: RequestPriority,
    ) -> Result<Vec<TopItem>, ProviderError> {
        let limit_text = limit.to_string();
        self.list_items(
            "user.getTopArtists",
            creds,
            &[
                ("user", username),
                ("period", period),
                ("limit", &limit_text),
            ],
            priority,
        )
        .await
    }

    /// `user.getWeeklyArtistChart`: the user's artists this week, most
    /// played first.
    ///
    /// # Errors
    ///
    /// The typed failure of the call.
    pub async fn user_weekly_artist_chart(
        &self,
        creds: &LastFmCredentials,
        username: &str,
        priority: RequestPriority,
    ) -> Result<Vec<TopItem>, ProviderError> {
        self.list_items(
            "user.getWeeklyArtistChart",
            creds,
            &[("user", username)],
            priority,
        )
        .await
    }

    /// `user.getWeeklyAlbumChart`: the user's albums this week, most played
    /// first. Album MBIDs name releases.
    ///
    /// # Errors
    ///
    /// The typed failure of the call.
    pub async fn user_weekly_album_chart(
        &self,
        creds: &LastFmCredentials,
        username: &str,
        priority: RequestPriority,
    ) -> Result<Vec<TopItem>, ProviderError> {
        self.list_items(
            "user.getWeeklyAlbumChart",
            creds,
            &[("user", username)],
            priority,
        )
        .await
    }

    /// `tag.getTopArtists`: the most played artists carrying one tag.
    ///
    /// # Errors
    ///
    /// The typed failure of the call.
    pub async fn tag_top_artists(
        &self,
        creds: &LastFmCredentials,
        tag: &str,
        limit: u32,
        priority: RequestPriority,
    ) -> Result<Vec<TopItem>, ProviderError> {
        let limit_text = limit.to_string();
        self.list_items(
            "tag.getTopArtists",
            creds,
            &[("tag", tag), ("limit", &limit_text)],
            priority,
        )
        .await
    }

    /// `user.getLovedTracks`: the tracks one Last.fm user loved, newest
    /// first. Each row's `name` is the track.
    ///
    /// # Errors
    ///
    /// The typed failure of the call.
    pub async fn user_loved_tracks(
        &self,
        creds: &LastFmCredentials,
        username: &str,
        limit: u32,
        priority: RequestPriority,
    ) -> Result<Vec<TopItem>, ProviderError> {
        let limit_text = limit.to_string();
        self.list_items(
            "user.getLovedTracks",
            creds,
            &[("user", username), ("limit", &limit_text)],
            priority,
        )
        .await
    }

    /// `user.getRecentTracks`: the user's latest scrobbles, newest first.
    ///
    /// # Errors
    ///
    /// The typed failure of the call.
    pub async fn user_recent_tracks(
        &self,
        creds: &LastFmCredentials,
        username: &str,
        limit: u32,
        priority: RequestPriority,
    ) -> Result<Vec<RecentTrack>, ProviderError> {
        let limit_text = limit.to_string();
        let params = [("user", username), ("limit", limit_text.as_str())];
        let Some(wire) = self
            .read_found::<models::WireRecentReply>("user.getRecentTracks", creds, &params, priority)
            .await?
        else {
            return Ok(Vec::new());
        };
        Ok(wire
            .recenttracks
            .track
            .iter()
            .flat_map(models::OneOrMany::iter)
            .filter_map(recent_track)
            .collect())
    }

    /// Weighted artist tags (`artist.getTopTags` with `autocorrect=0`).
    /// Unknown artists (error 6) answer empty: no tags is an answer, not a
    /// fault.
    ///
    /// # Errors
    ///
    /// The typed failure of the call.
    pub async fn artist_top_genres(
        &self,
        creds: &LastFmCredentials,
        artist: &str,
        priority: RequestPriority,
    ) -> Result<Vec<WeightedTag>, ProviderError> {
        let params = [("artist", artist), ("autocorrect", "0")];
        Ok(self
            .read_found::<models::WireTopTagsReply>("artist.getTopTags", creds, &params, priority)
            .await?
            .map(|wire| top_tags(&wire.toptags))
            .unwrap_or_default())
    }

    /// Weighted album tags (`album.getTopTags` with `autocorrect=0`).
    /// Unknown albums answer empty.
    ///
    /// # Errors
    ///
    /// The typed failure of the call.
    pub async fn album_top_genres(
        &self,
        creds: &LastFmCredentials,
        artist: &str,
        album: &str,
        priority: RequestPriority,
    ) -> Result<Vec<WeightedTag>, ProviderError> {
        let params = [("artist", artist), ("album", album), ("autocorrect", "0")];
        Ok(self
            .read_found::<models::WireTopTagsReply>("album.getTopTags", creds, &params, priority)
            .await?
            .map(|wire| top_tags(&wire.toptags))
            .unwrap_or_default())
    }

    /// One unsigned list read: its rows, blank names dropped. An empty
    /// list, or an unknown entity, reads as empty.
    async fn list_items(
        &self,
        method: &str,
        creds: &LastFmCredentials,
        params: &[(&str, &str)],
        priority: RequestPriority,
    ) -> Result<Vec<TopItem>, ProviderError> {
        let Some(wire) = self
            .read_found::<models::WireTopReply>(method, creds, params, priority)
            .await?
        else {
            return Ok(Vec::new());
        };
        Ok(wire
            .list
            .rows
            .iter()
            .flat_map(models::OneOrMany::iter)
            .filter_map(top_item)
            .collect())
    }

    /// One unsigned read; `None` when Last.fm does not know the entity.
    async fn read_found<T: DeserializeOwned>(
        &self,
        method: &str,
        creds: &LastFmCredentials,
        params: &[(&str, &str)],
        priority: RequestPriority,
    ) -> Result<Option<T>, ProviderError> {
        let request = self.call(method, creds, params, Signed::No)?;
        match self.read(request, priority).await {
            Ok(payload) => Ok(Some(payload)),
            Err(ProviderError::NotFound { .. }) => Ok(None),
            Err(error) => Err(error),
        }
    }

    /// Build one call. Signed calls sort their parameters (excluding
    /// `format` and `callback`), concatenate key plus value, append the
    /// shared secret, and MD5 the result; the session
    /// key rides along as `sk` when one is linked. Reads are GETs, posts
    /// are forms; both go to the API root with its trailing slash.
    fn call(
        &self,
        method: &str,
        creds: &LastFmCredentials,
        params: &[(&str, &str)],
        signed: Signed,
    ) -> Result<Request, ProviderError> {
        if creds.api_key.is_empty() {
            // v2 raises ConfigurationError: a missing key is a configuration
            // failure for this optional source, never a silent skip.
            return Err(not_configured("Last.fm API key is not configured"));
        }
        if signed != Signed::No && creds.shared_secret.is_empty() {
            return Err(not_configured(
                "Last.fm shared secret is required for signed requests",
            ));
        }
        let mut call: Vec<(String, String)> = vec![
            ("method".to_owned(), method.to_owned()),
            ("api_key".to_owned(), creds.api_key.clone()),
            ("format".to_owned(), "json".to_owned()),
        ];
        call.extend(
            params
                .iter()
                .map(|(key, value)| ((*key).to_owned(), (*value).to_owned())),
        );
        if signed != Signed::No {
            if let Some(session_key) = creds.session_key.as_deref()
                && !call.iter().any(|(key, _)| key == "sk")
            {
                call.push(("sk".to_owned(), session_key.to_owned()));
            }
            let signature = api_sig(&call, &creds.shared_secret);
            call.push(("api_sig".to_owned(), signature));
        }
        let base = self.base.as_ref().ok_or_else(unusable_base)?;
        let request = if signed == Signed::Post {
            Request::post_form(base, &[], call)
        } else {
            let pairs: Vec<(&str, &str)> = call
                .iter()
                .map(|(key, value)| (key.as_str(), value.as_str()))
                .collect();
            Request::get(base, &[], &pairs)
        };
        Ok(request.map_err(|_| unusable_base())?.trailing_slash())
    }

    /// Send one call and decode its JSON body. An answer that does not
    /// decode is noted for the request.
    async fn read<T: DeserializeOwned>(
        &self,
        request: Request,
        priority: RequestPriority,
    ) -> Result<T, ProviderError> {
        let reply = self.send(request, priority).await?;
        reply
            .json()
            .inspect_err(|error| note_shape(&error.to_string()))
    }

    /// Send one call. An error code Last.fm does not list as an outage is
    /// still noted for the request, as the service answered with one.
    async fn send(
        &self,
        request: Request,
        priority: RequestPriority,
    ) -> Result<Reply, ProviderError> {
        self.executor
            .send(request, priority)
            .await
            .inspect_err(|error| {
                if matches!(error, ProviderError::BadRequest { .. }) {
                    record_current(SOURCE, IntegrationStatus::Error, true);
                }
            })
    }
}

/// What Last.fm did with one scrobble.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScrobbleAnswer {
    /// Kept.
    Accepted,
    /// Turned away, for this reason.
    Ignored(IgnoredReason),
}

/// Why Last.fm turned a scrobble away: its `ignoredMessage` code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IgnoredReason {
    /// 1: the artist is filtered.
    ArtistIgnored,
    /// 2: the track is filtered.
    TrackIgnored,
    /// 3: the play time is too far in the past.
    TooOld,
    /// 4: the play time is too far in the future.
    TooNew,
    /// 5: the account's daily scrobble limit is reached.
    DailyLimit,
    /// Any other code, or none given while nothing was accepted.
    Other(i64),
}

/// Read one decoded `track.scrobble` answer: accepted when Last.fm counts
/// it, else the first scrobble's ignored code.
fn scrobble_answer(wire: &models::WireScrobbleReply) -> ScrobbleAnswer {
    if wire.scrobbles.counts.accepted.value().unwrap_or(0) > 0 {
        return ScrobbleAnswer::Accepted;
    }
    let code = wire
        .scrobbles
        .scrobble
        .as_ref()
        .and_then(models::OneOrMany::first)
        .and_then(|scrobble| scrobble.ignored_message.as_ref())
        .and_then(|message| message.code.as_ref())
        .and_then(models::Lenient::value)
        .unwrap_or(0);
    ScrobbleAnswer::Ignored(match code {
        1 => IgnoredReason::ArtistIgnored,
        2 => IgnoredReason::TrackIgnored,
        3 => IgnoredReason::TooOld,
        4 => IgnoredReason::TooNew,
        5 => IgnoredReason::DailyLimit,
        other => IgnoredReason::Other(other),
    })
}

/// How one call is built.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Signed {
    /// An unsigned GET.
    No,
    /// A signed GET.
    Yes,
    /// A signed form post.
    Post,
}

/// The parameters of an artist list read: by MusicBrainz id when given.
fn artist_params<'a>(
    artist: &'a str,
    mbid: Option<&'a str>,
    limit: &'a str,
) -> Vec<(&'a str, &'a str)> {
    match mbid {
        Some(mbid) => vec![("mbid", mbid), ("limit", limit)],
        None => vec![("artist", artist), ("limit", limit)],
    }
}

/// The weighted tags, blank names dropped.
fn top_tags(wire: &models::WireTopTags) -> Vec<WeightedTag> {
    wire.tag
        .iter()
        .flat_map(models::OneOrMany::iter)
        .filter_map(|tag| {
            let name = tag.name.trim();
            (!name.is_empty()).then(|| WeightedTag {
                name: name.to_owned(),
                weight: count(tag.count.as_ref()),
            })
        })
        .collect()
}

/// An answer that did not have the expected shape: the service answered,
/// so it is noted for the request but never counted as an outage.
fn note_shape(cause: &str) {
    tracing::debug!(
        provider = SOURCE,
        cause,
        "Last.fm answer had no usable shape"
    );
    record_current(SOURCE, IntegrationStatus::Error, true);
}

/// A decoded answer missing what the call promises, noted for the request.
fn shape(message: &str) -> ProviderError {
    note_shape(message);
    ProviderError::Payload {
        provider: SOURCE,
        kind: PayloadKind::Contract,
        message: short(message),
    }
}

fn not_configured(message: &str) -> ProviderError {
    ProviderError::NotConfigured {
        provider: SOURCE,
        message: message.to_owned(),
    }
}

fn unusable_base() -> ProviderError {
    ProviderError::Invalid {
        provider: SOURCE,
        reason: "the Last.fm address is not usable",
    }
}

/// Build the call signature: sort parameters excluding `format` and
/// `callback`, concatenate key plus value, append the shared secret, MD5 the
/// result.
pub fn api_sig(params: &[(String, String)], shared_secret: &str) -> String {
    let mut filtered: Vec<(&str, &str)> = params
        .iter()
        .filter(|(key, _)| key != "format" && key != "callback")
        .map(|(key, value)| (key.as_str(), value.as_str()))
        .collect();
    filtered.sort_by(|left, right| left.0.cmp(right.0));
    let mut signed = String::new();
    for (key, value) in filtered {
        signed.push_str(key);
        signed.push_str(value);
    }
    signed.push_str(shared_secret);
    format!("{:x}", md5::compute(signed.as_bytes()))
}

/// A count, 0 when Last.fm sent none or sent text that is not a number.
fn count(value: Option<&models::Lenient>) -> i64 {
    value.and_then(models::Lenient::value).unwrap_or(0)
}

/// Pick the artwork URL: `extralarge` wins, otherwise the last image wins.
fn pick_image(images: Option<&models::OneOrMany<models::WireImage>>) -> String {
    let Some(images) = images else {
        return String::new();
    };
    images
        .iter()
        .find(|image| image.size.as_deref() == Some("extralarge"))
        .or_else(|| images.iter().last())
        .and_then(|image| image.url.clone())
        .unwrap_or_default()
}

/// A MusicBrainz id from Last.fm, lowercase. Blank, missing or malformed
/// ids read as absent: callers put them into request paths.
fn mbid_or_none(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|id| super::musicbrainz::is_valid_mbid(id))
        .map(str::to_ascii_lowercase)
}

/// The tags of an info answer.
fn tags(wire: Option<&models::Section<models::WireTagList>>) -> Vec<Tag> {
    wire.and_then(models::Section::get)
        .and_then(|list| list.tag.as_ref())
        .iter()
        .flat_map(|tags| tags.iter())
        .map(|tag| Tag {
            name: tag.name.clone().unwrap_or_default(),
            url: tag.url.clone().unwrap_or_default(),
        })
        .collect()
}

/// `artist.getInfo`'s artist; `None` when its name is blank.
fn artist_info(artist: models::WireArtistInfo) -> Option<ArtistInfo> {
    if artist.name.trim().is_empty() {
        return None;
    }
    let stats = artist.stats.as_ref().and_then(models::Section::get);
    let bio = artist.bio.as_ref().and_then(models::Section::get);
    Some(ArtistInfo {
        mbid: mbid_or_none(artist.mbid.as_deref()),
        listeners: count(stats.and_then(|stats| stats.listeners.as_ref())),
        playcount: count(stats.and_then(|stats| stats.playcount.as_ref())),
        url: artist.url.unwrap_or_default(),
        bio_summary: bio.and_then(|bio| bio.summary.clone()).unwrap_or_default(),
        bio_content: bio.and_then(|bio| bio.content.clone()).unwrap_or_default(),
        tags: tags(artist.tags.as_ref()),
        similar: artist
            .similar
            .as_ref()
            .and_then(models::Section::get)
            .map(similar_artists)
            .unwrap_or_default(),
        name: artist.name,
    })
}

/// `album.getInfo`'s album; `None` when its title is blank.
fn album_info(album: models::WireAlbumInfo) -> Option<AlbumInfo> {
    if album.name.trim().is_empty() {
        return None;
    }
    let tracks = album
        .tracks
        .as_ref()
        .and_then(models::Section::get)
        .and_then(|list| list.track.as_ref())
        .iter()
        .flat_map(|tracks| tracks.iter())
        .map(|track| AlbumTrack {
            name: track.name.clone().unwrap_or_default(),
            duration_secs: count(track.duration.as_ref()),
            rank: count(track.attr.as_ref().and_then(|attr| attr.rank.as_ref())),
            url: track.url.clone().unwrap_or_default(),
        })
        .collect();
    Some(AlbumInfo {
        artist_name: album
            .artist
            .as_ref()
            .and_then(models::WireNameRef::name)
            .unwrap_or("")
            .to_owned(),
        mbid: mbid_or_none(album.mbid.as_deref()),
        listeners: count(album.listeners.as_ref()),
        playcount: count(album.playcount.as_ref()),
        image_url: pick_image(album.image.as_ref()),
        summary: album
            .wiki
            .as_ref()
            .and_then(models::Section::get)
            .and_then(|wiki| wiki.summary.clone())
            .unwrap_or_default(),
        tags: tags(album.tags.as_ref()),
        tracks,
        url: album.url.unwrap_or_default(),
        name: album.name,
    })
}

/// One top-list row; a blank name skips it.
fn top_item(item: &models::WireTopItem) -> Option<TopItem> {
    let name = item.name.trim();
    if name.is_empty() {
        return None;
    }
    Some(TopItem {
        name: name.to_owned(),
        artist_name: item
            .artist
            .as_ref()
            .and_then(models::WireNameRef::name)
            .unwrap_or("")
            .to_owned(),
        mbid: mbid_or_none(item.mbid.as_deref()),
        playcount: count(item.playcount.as_ref()),
        image_url: pick_image(item.image.as_ref()),
    })
}

/// One scrobble; a blank title skips it.
fn recent_track(item: &models::WireRecentTrack) -> Option<RecentTrack> {
    let name = item.name.trim();
    if name.is_empty() {
        return None;
    }
    let text_of = |value: Option<&models::WireNameRef>| {
        value
            .and_then(models::WireNameRef::text)
            .unwrap_or("")
            .trim()
            .to_owned()
    };
    Some(RecentTrack {
        name: name.to_owned(),
        artist_name: text_of(item.artist.as_ref()),
        album_name: text_of(item.album.as_ref()),
        album_mbid: mbid_or_none(item.album.as_ref().and_then(models::WireNameRef::mbid)),
        image_url: pick_image(item.image.as_ref()),
    })
}

/// A list of similar artists.
fn similar_artists(list: &models::WireSimilarList) -> Vec<SimilarArtist> {
    list.artist
        .iter()
        .flat_map(models::OneOrMany::iter)
        .map(|artist| SimilarArtist {
            name: artist.name.clone(),
            mbid: mbid_or_none(artist.mbid.as_deref()),
            score: artist
                .score
                .as_ref()
                .and_then(models::Lenient::fraction)
                .unwrap_or(0.0),
            url: artist.url.clone().unwrap_or_default(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_scrobble_answer_says_whether_the_play_was_kept() {
        let read = |body: &str| {
            serde_json::from_str::<models::WireScrobbleReply>(body)
                .map(|wire| scrobble_answer(&wire))
                .ok()
        };
        assert_eq!(
            read(r#"{"scrobbles":{"@attr":{"accepted":1,"ignored":0}}}"#),
            Some(ScrobbleAnswer::Accepted)
        );
        assert_eq!(
            read(
                r##"{"scrobbles":{"scrobble":{"ignoredMessage":{"code":"5","#text":"Daily scrobble limit exceeded"}},"@attr":{"accepted":"0","ignored":"1"}}}"##
            ),
            Some(ScrobbleAnswer::Ignored(IgnoredReason::DailyLimit))
        );
        assert_eq!(
            read(
                r#"{"scrobbles":{"scrobble":[{"ignoredMessage":{"code":3}}],"@attr":{"accepted":0,"ignored":1}}}"#
            ),
            Some(ScrobbleAnswer::Ignored(IgnoredReason::TooOld))
        );
        assert_eq!(read(r#"{"scrobbles":{}}"#), None, "no count, no answer");
        assert_eq!(read(r#"{"status":"ok"}"#), None);
    }

    #[test]
    fn malformed_mbids_read_as_absent() {
        let list: models::WireSimilarList = serde_json::from_value(serde_json::json!({"artist": [
            {"name": "A", "mbid": "A74B1B7F-71A5-4011-9441-D0B5E4122711"},
            {"name": "B", "mbid": "../../release/x"},
            {"name": "C", "mbid": " "},
        ]}))
        .unwrap();
        let ids: Vec<Option<String>> = similar_artists(&list)
            .into_iter()
            .map(|artist| artist.mbid)
            .collect();
        assert_eq!(
            ids,
            vec![
                Some("a74b1b7f-71a5-4011-9441-d0b5e4122711".to_owned()),
                None,
                None
            ]
        );
    }
}
