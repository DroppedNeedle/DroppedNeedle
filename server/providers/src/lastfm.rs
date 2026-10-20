//! Last.fm metadata client with per-user credentials.
//!
//! Ports the read paths of v2's Last.fm repository, its wire models, and
//! the genre surface verified against production on 2026-07-22, plus the
//! two signed account-linking calls and the signed scrobble posts.
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

use crate::RequestPriority;
use crate::degradation::{IntegrationStatus, record_current};
use crate::error::{MAX_RETRY_AFTER, PayloadKind, ProviderError};
use crate::executor::{
    Answer, Backoff, Executor, Reply, Request, Spec, Url, short, standard_classify,
};

/// Default API host (v2 `LASTFM_API_URL`).
pub const DEFAULT_BASE_URL: &str = "https://ws.audioscrobbler.com/2.0/";
/// Last.fm's documented rate-limit error code (v2 `LASTFM_ERROR_MAP[29]`).
/// The management notes call out that this code was never provoked live on
/// purpose; it is decoded from the documented envelope and covered by a
/// scripted fake instead.
pub const ERROR_RATE_LIMITED: i64 = 29;
/// The rest after error 29 (v2 `retry_after_seconds=1.0`).
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
    #[derive(serde::Deserialize)]
    struct Envelope {
        error: serde_json::Value,
    }
    if !body.windows(7).any(|window| window == b"\"error\"") {
        return None;
    }
    let envelope: Envelope = serde_json::from_slice(body).ok()?;
    Some(envelope.error.as_i64())
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

/// A session reply from `auth.getSession`. Name and key are both required
/// (v2 `parse_session` raises when either is absent).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Session {
    /// Linked Last.fm username.
    pub name: String,
    /// Session key for signed calls.
    pub key: String,
    /// Subscriber flag.
    pub subscriber: i64,
}

/// A plain tag (v2 `LastFmTag`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tag {
    /// Tag name.
    pub name: String,
    /// Tag URL.
    pub url: String,
}

/// A weighted tag from the genre surface (v2 `LastFmManagementTag`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WeightedTag {
    /// Tag name (never blank; v2 filters blank names before projecting).
    pub name: String,
    /// Weight, 0 through 100 on the live API.
    pub weight: i64,
}

/// Artist metadata (v2 `LastFmArtistInfo`). The name is required; counts and
/// artwork stay lenient the way v2's `_safe_int` parsing is.
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

/// Album metadata (v2 `LastFmAlbumInfo`). The title is required.
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

/// One album track (v2 `LastFmAlbumTrack`).
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

/// A similar artist (v2 `LastFmSimilarArtist`).
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
/// albums, or the sitewide artist chart (v2 `LastFmTrack` / `LastFmAlbum` /
/// `LastFmArtist`). The name is required; counts are lenient.
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

/// One scrobble from a user's recent tracks (v2 `LastFmRecentTrack`).
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
        let payload = self
            .read(
                self.call("auth.getToken", creds, &[], Signed::Yes)?,
                priority,
            )
            .await?;
        match payload.get("token").and_then(serde_json::Value::as_str) {
            Some(token) if !token.is_empty() => Ok(AuthToken {
                token: token.to_owned(),
            }),
            _ => Err(shape("Last.fm auth.getToken answer has no token")),
        }
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
        let payload = self
            .read(
                self.call("auth.getSession", creds, &[("token", token)], Signed::Yes)?,
                priority,
            )
            .await?;
        // v2 reads `data["session"]` but falls back to the top level.
        let session = payload
            .get("session")
            .and_then(serde_json::Value::as_object)
            .or(payload.as_object());
        let text = |key: &str| {
            session
                .and_then(|session| session.get(key))
                .and_then(serde_json::Value::as_str)
                .unwrap_or("")
                .to_owned()
        };
        let (name, key) = (text("name"), text("key"));
        if name.is_empty() || key.is_empty() {
            return Err(shape("Last.fm auth.getSession answer has no name or key"));
        }
        Ok(Session {
            name,
            key,
            subscriber: lenient_int(session.and_then(|session| session.get("subscriber"))),
        })
    }

    /// A signed form post for the session in `creds` (`track.scrobble`,
    /// `track.updateNowPlaying`). Sent once: a post is never repeated
    /// here, the caller's queue decides about another try.
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
        let Some(payload) = self
            .read_found("artist.getInfo", creds, &[(key, value)], priority)
            .await?
        else {
            return Ok(None);
        };
        parse_artist_info(&payload)
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
        let Some(payload) = self
            .read_found("album.getInfo", creds, &params, priority)
            .await?
        else {
            return Ok(None);
        };
        parse_album_info(&payload)
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
        let Some(payload) = self
            .read_found("artist.getSimilar", creds, &params, priority)
            .await?
        else {
            return Ok(Vec::new());
        };
        let artists = payload
            .get("similarartists")
            .and_then(|similar| similar.get("artist"))
            .and_then(serde_json::Value::as_array)
            .cloned()
            .unwrap_or_default();
        Ok(artists.iter().map(parse_similar_artist).collect())
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
            ("artist.getTopTracks", "toptracks", "track"),
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
            ("artist.getTopAlbums", "topalbums", "album"),
            creds,
            &artist_params(artist, mbid, &limit_text),
            priority,
        )
        .await
    }

    /// `chart.getTopArtists`: the sitewide artist chart, most played first
    /// (v2 `get_global_top_artists`). Each row's `name` is the artist.
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
            ("chart.getTopArtists", "artists", "artist"),
            creds,
            &[("limit", &limit_text)],
            priority,
        )
        .await
    }

    /// `user.getTopAlbums` for one Last.fm user over `period` (`7day`,
    /// `1month`, `12month` or `overall`), most played first (v2
    /// `get_user_top_albums`).
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
            ("user.getTopAlbums", "topalbums", "album"),
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
    /// first (v2 `get_user_top_artists`). Each row's `name` is the artist.
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
            ("user.getTopArtists", "topartists", "artist"),
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
    /// played first (v2 `get_user_weekly_artist_chart`).
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
            ("user.getWeeklyArtistChart", "weeklyartistchart", "artist"),
            creds,
            &[("user", username)],
            priority,
        )
        .await
    }

    /// `user.getWeeklyAlbumChart`: the user's albums this week, most played
    /// first (v2 `get_user_weekly_album_chart`). Album MBIDs name releases.
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
            ("user.getWeeklyAlbumChart", "weeklyalbumchart", "album"),
            creds,
            &[("user", username)],
            priority,
        )
        .await
    }

    /// `tag.getTopArtists`: the most played artists carrying one tag (v2
    /// `get_tag_top_artists`).
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
            ("tag.getTopArtists", "topartists", "artist"),
            creds,
            &[("tag", tag), ("limit", &limit_text)],
            priority,
        )
        .await
    }

    /// `user.getLovedTracks`: the tracks one Last.fm user loved, newest
    /// first (v2 `get_user_loved_tracks`). Each row's `name` is the track.
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
            ("user.getLovedTracks", "lovedtracks", "track"),
            creds,
            &[("user", username), ("limit", &limit_text)],
            priority,
        )
        .await
    }

    /// `user.getRecentTracks`: the user's latest scrobbles, newest first
    /// (v2 `get_user_recent_tracks`).
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
        let Some(payload) = self
            .read_found("user.getRecentTracks", creds, &params, priority)
            .await?
        else {
            return Ok(Vec::new());
        };
        let items = payload
            .get("recenttracks")
            .and_then(|list| list.get("track"))
            .and_then(serde_json::Value::as_array)
            .cloned()
            .unwrap_or_default();
        Ok(items.iter().filter_map(parse_recent_track).collect())
    }

    /// Weighted artist tags from the live-verified genre surface
    /// (`artist.getTopTags` with `autocorrect=0`; verified against
    /// production on 2026-07-22, see `lastfm_MANAGEMENT_API_NOTES.md`).
    /// Unknown artists (error 6) answer empty, exactly like v2's empty
    /// candidate tuple: no tags is an answer, not a fault.
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
            .read_found("artist.getTopTags", creds, &params, priority)
            .await?
            .map(|payload| top_tags(&payload))
            .unwrap_or_default())
    }

    /// Weighted album tags from the live-verified genre surface
    /// (`album.getTopTags` with `autocorrect=0`; same verification).
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
            .read_found("album.getTopTags", creds, &params, priority)
            .await?
            .map(|payload| top_tags(&payload))
            .unwrap_or_default())
    }

    /// One unsigned list read: the rows under `envelope.key`, blank names
    /// dropped. A missing list, or an unknown entity, reads as empty.
    async fn list_items(
        &self,
        (method, envelope, key): (&str, &str, &str),
        creds: &LastFmCredentials,
        params: &[(&str, &str)],
        priority: RequestPriority,
    ) -> Result<Vec<TopItem>, ProviderError> {
        let Some(payload) = self.read_found(method, creds, params, priority).await? else {
            return Ok(Vec::new());
        };
        let items = payload
            .get(envelope)
            .and_then(|list| list.get(key))
            .and_then(serde_json::Value::as_array)
            .cloned()
            .unwrap_or_default();
        Ok(items.iter().filter_map(parse_top_item).collect())
    }

    /// One unsigned read; `None` when Last.fm does not know the entity.
    async fn read_found(
        &self,
        method: &str,
        creds: &LastFmCredentials,
        params: &[(&str, &str)],
        priority: RequestPriority,
    ) -> Result<Option<serde_json::Value>, ProviderError> {
        let request = self.call(method, creds, params, Signed::No)?;
        match self.read(request, priority).await {
            Ok(payload) => Ok(Some(payload)),
            Err(ProviderError::NotFound { .. }) => Ok(None),
            Err(error) => Err(error),
        }
    }

    /// Build one call. Signed calls sort their parameters (excluding
    /// `format` and `callback`), concatenate key plus value, append the
    /// shared secret, and MD5 the result (v2 `_build_api_sig`); the session
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
    async fn read(
        &self,
        request: Request,
        priority: RequestPriority,
    ) -> Result<serde_json::Value, ProviderError> {
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

/// Decode the `toptags.tag` list into weighted tags. A missing `toptags`
/// object reads as empty (v2's tolerant structs default it); blank names
/// are filtered before projecting (v2 `_management_genre_candidates`).
fn top_tags(payload: &serde_json::Value) -> Vec<WeightedTag> {
    let tags = match payload
        .get("toptags")
        .and_then(|toptags| toptags.get("tag"))
    {
        None | Some(serde_json::Value::Null) => return Vec::new(),
        Some(serde_json::Value::Array(tags)) => tags.clone(),
        // Last.fm sometimes answers a single object where a list belongs;
        // accept it rather than failing the whole read.
        Some(single) => vec![single.clone()],
    };
    tags.iter()
        .filter_map(serde_json::Value::as_object)
        .filter_map(|tag| {
            let name = tag
                .get("name")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("")
                .trim();
            (!name.is_empty()).then(|| WeightedTag {
                name: name.to_owned(),
                weight: lenient_int(tag.get("count")),
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
/// result (v2 `_build_api_sig`).
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

/// Parse a string-or-number count the way v2 `_safe_int` does: integers pass
/// through, numeric strings parse, floats truncate, and anything else reads
/// as zero. Counts are telemetry, not identity, so leniency here is
/// intended.
fn lenient_int(value: Option<&serde_json::Value>) -> i64 {
    match value {
        None | Some(serde_json::Value::Null) => 0,
        Some(serde_json::Value::Number(number)) => number
            .as_i64()
            .or_else(|| number.as_f64().map(|float| float as i64))
            .unwrap_or(0),
        Some(serde_json::Value::String(text)) => text.trim().parse::<i64>().unwrap_or(0),
        Some(_) => 0,
    }
}

/// Parse a string-or-number score the way v2 `_safe_float` does.
fn lenient_float(value: Option<&serde_json::Value>) -> f64 {
    match value {
        None | Some(serde_json::Value::Null) => 0.0,
        Some(serde_json::Value::Number(number)) => number.as_f64().unwrap_or(0.0),
        Some(serde_json::Value::String(text)) => text.trim().parse::<f64>().unwrap_or(0.0),
        Some(_) => 0.0,
    }
}

/// Pick the artwork URL: `extralarge` wins, otherwise the last image wins
/// (v2 `_extract_image`).
fn pick_image(images: Option<&serde_json::Value>) -> String {
    let images = match images.and_then(serde_json::Value::as_array) {
        Some(images) if !images.is_empty() => images,
        _ => return String::new(),
    };
    for image in images {
        let size = image
            .get("size")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("");
        if size == "extralarge" {
            return image
                .get("#text")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("")
                .to_owned();
        }
    }
    images
        .last()
        .and_then(|image| image.get("#text"))
        .and_then(serde_json::Value::as_str)
        .unwrap_or("")
        .to_owned()
}

/// A MusicBrainz id from Last.fm, lowercase. Blank, missing or malformed
/// ids read as absent: callers put them into request paths.
fn mbid_or_none(value: Option<&serde_json::Value>) -> Option<String> {
    blank_to_none(value)
        .filter(|id| super::musicbrainz::is_valid_mbid(id))
        .map(|id| id.to_ascii_lowercase())
}

/// A blank-or-missing MusicBrainz id reads as absent (v2 `or None`).
fn blank_to_none(value: Option<&serde_json::Value>) -> Option<String> {
    value
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(str::to_owned)
}

/// Parse the tag list shape (`tags.tag`, tolerating a lone object).
fn parse_tags(entity: &serde_json::Map<String, serde_json::Value>) -> Vec<Tag> {
    let tags = match entity.get("tags").and_then(|tags| tags.get("tag")) {
        None | Some(serde_json::Value::Null) => return Vec::new(),
        Some(serde_json::Value::Array(tags)) => tags.clone(),
        Some(single) => vec![single.clone()],
    };
    tags.iter()
        .filter_map(|tag| {
            let tag = tag.as_object()?;
            Some(Tag {
                name: tag
                    .get("name")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("")
                    .to_owned(),
                url: tag
                    .get("url")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("")
                    .to_owned(),
            })
        })
        .collect()
}

/// Parse `artist.getInfo` (v2 `parse_artist_info`). The artist name is
/// required; a payload missing it fails instead of yielding an empty
/// success.
fn parse_artist_info(payload: &serde_json::Value) -> Option<ArtistInfo> {
    let artist = payload.get("artist")?.as_object()?;
    let name = artist.get("name")?.as_str()?;
    if name.trim().is_empty() {
        return None;
    }
    let stats = artist.get("stats");
    let stats = stats.and_then(serde_json::Value::as_object);
    let bio_text = |key: &str| {
        artist
            .get("bio")
            .and_then(|bio| bio.get(key))
            .and_then(serde_json::Value::as_str)
            .unwrap_or("")
            .to_owned()
    };
    let bio_summary = bio_text("summary");
    let bio_content = bio_text("content");
    let similar = artist
        .get("similar")
        .and_then(|similar| similar.get("artist"))
        .and_then(serde_json::Value::as_array)
        .cloned()
        .unwrap_or_default();
    Some(ArtistInfo {
        name: name.to_owned(),
        mbid: mbid_or_none(artist.get("mbid")),
        listeners: lenient_int(stats.and_then(|stats| stats.get("listeners"))),
        playcount: lenient_int(stats.and_then(|stats| stats.get("playcount"))),
        url: artist
            .get("url")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("")
            .to_owned(),
        bio_summary,
        bio_content,
        tags: parse_tags(artist),
        similar: similar.iter().map(parse_similar_artist).collect(),
    })
}

/// Parse `album.getInfo` (v2 `parse_album_info`). The title is required.
fn parse_album_info(payload: &serde_json::Value) -> Option<AlbumInfo> {
    let album = payload.get("album")?.as_object()?;
    let name = album.get("name")?.as_str()?;
    if name.trim().is_empty() {
        return None;
    }
    let tracks = match album.get("tracks").and_then(|tracks| tracks.get("track")) {
        None | Some(serde_json::Value::Null) => Vec::new(),
        Some(serde_json::Value::Array(tracks)) => tracks
            .iter()
            .filter_map(|track| {
                let track = track.as_object()?;
                Some(AlbumTrack {
                    name: track
                        .get("name")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or("")
                        .to_owned(),
                    duration_secs: lenient_int(track.get("duration")),
                    rank: lenient_int(track.get("@attr").and_then(|attr| attr.get("rank"))),
                    url: track
                        .get("url")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or("")
                        .to_owned(),
                })
            })
            .collect(),
        Some(_) => Vec::new(),
    };
    Some(AlbumInfo {
        name: name.to_owned(),
        artist_name: album
            .get("artist")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("")
            .to_owned(),
        mbid: mbid_or_none(album.get("mbid")),
        listeners: lenient_int(album.get("listeners")),
        playcount: lenient_int(album.get("playcount")),
        url: album
            .get("url")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("")
            .to_owned(),
        image_url: pick_image(album.get("image")),
        summary: album
            .get("wiki")
            .and_then(|wiki| wiki.get("summary"))
            .and_then(serde_json::Value::as_str)
            .unwrap_or("")
            .to_owned(),
        tags: parse_tags(album),
        tracks,
    })
}

/// Parse one similar artist (v2 `parse_similar_artist`).
/// Parse one top-tracks or top-albums row (v2 `parse_top_track` /
/// `parse_top_album`). A blank name skips the row.
fn parse_top_item(item: &serde_json::Value) -> Option<TopItem> {
    let item = item.as_object()?;
    let name = item.get("name")?.as_str()?.trim();
    if name.is_empty() {
        return None;
    }
    let artist_name = match item.get("artist") {
        // Charts send `{"#text": ...}` where top lists send `{"name": ...}`.
        Some(serde_json::Value::Object(artist)) => artist
            .get("name")
            .or_else(|| artist.get("#text"))
            .and_then(serde_json::Value::as_str)
            .unwrap_or(""),
        Some(serde_json::Value::String(artist)) => artist.as_str(),
        _ => "",
    };
    Some(TopItem {
        name: name.to_owned(),
        artist_name: artist_name.to_owned(),
        mbid: mbid_or_none(item.get("mbid")),
        playcount: lenient_int(item.get("playcount")),
        image_url: pick_image(item.get("image")),
    })
}

fn parse_recent_track(item: &serde_json::Value) -> Option<RecentTrack> {
    let item = item.as_object()?;
    let name = item.get("name")?.as_str()?.trim();
    if name.is_empty() {
        return None;
    }
    let text_of = |key: &str| {
        item.get(key)
            .and_then(|value| value.get("#text").or_else(|| value.get("name")))
            .and_then(serde_json::Value::as_str)
            .unwrap_or("")
            .trim()
            .to_owned()
    };
    Some(RecentTrack {
        name: name.to_owned(),
        artist_name: text_of("artist"),
        album_name: text_of("album"),
        album_mbid: mbid_or_none(item.get("album").and_then(|album| album.get("mbid"))),
        image_url: pick_image(item.get("image")),
    })
}

fn parse_similar_artist(item: &serde_json::Value) -> SimilarArtist {
    let item = item.as_object();
    SimilarArtist {
        name: item
            .and_then(|artist| artist.get("name"))
            .and_then(serde_json::Value::as_str)
            .unwrap_or("")
            .to_owned(),
        mbid: item.and_then(|artist| mbid_or_none(artist.get("mbid"))),
        score: lenient_float(item.and_then(|artist| artist.get("match"))),
        url: item
            .and_then(|artist| artist.get("url"))
            .and_then(serde_json::Value::as_str)
            .unwrap_or("")
            .to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn malformed_mbids_read_as_absent() {
        let items = serde_json::json!([
            {"name": "A", "mbid": "A74B1B7F-71A5-4011-9441-D0B5E4122711"},
            {"name": "B", "mbid": "../../release/x"},
            {"name": "C", "mbid": " "},
        ]);
        let ids: Vec<Option<String>> = items
            .as_array()
            .unwrap()
            .iter()
            .map(|item| parse_similar_artist(item).mbid)
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
