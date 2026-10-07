//! Last.fm metadata client with per-user credentials.
//!
//! Ports the read paths of v2's Last.fm repository, its wire models, and
//! the genre surface verified against production on 2026-07-22.
//!
//! Credentials are per user, always. `LastFmCredentials` mirrors the
//! plaintext projection of the per-user store (`auth::users::models`
//! `LastFmConnection`, read through the `LastFmStore` trait; only the
//! service layer holds decrypted values, in memory). There is no global
//! key pair, on purpose:
//! every method takes the calling user's credentials, and the client itself
//! stores none. The one shared thing is pacing: wiring must hand every
//! per-user client the same community limiter ([`RATE_PER_SEC`] / [`BURST`])
//! so the host as a whole stays under 5 calls/second.
//!
//! Pacing and degradation ride the shared core traits
//! ([`Pacer`](super::limiter::Pacer), [`DegradationSink`](super::degradation::DegradationSink)):
//! wiring hands every per-user client the same community pacer so the host
//! as a whole stays under 5 calls/second.
//!
//! Backoff signaling (not sleeping) lives here: error 29 maps to
//! [`Outcome::Unavailable`] with a 1-second retry hint, exactly like v2's
//! `RateLimitedError`. The retry loop and circuit breaker belong to the
//! provider core.
//!
//! The two auth calls (`request_token`, `exchange_session`) are the
//! production implementation the `LastFmAuthClient` seam
//! (`auth::users::stores`) awaits; wiring would adapt their outcomes
//! onto `LastFmError` (`TokenNotAuthorized` maps from the error-14 outcome).

use std::time::Duration;

use super::{DegradationSink, Pacer};

/// Default API host (v2 `LASTFM_API_URL`).
pub const DEFAULT_BASE_URL: &str = "https://ws.audioscrobbler.com/2.0/";
/// Community pacing the wiring must configure: 5 calls/second shared by all
/// users of this host (v2 `_lastfm_rate_limiter`).
pub const RATE_PER_SEC: f64 = 5.0;
/// Community bucket burst (v2 `capacity=10`).
pub const BURST: u32 = 10;
/// Last.fm's documented rate-limit error code (v2 `LASTFM_ERROR_MAP[29]`).
/// The management notes call out that this code was never provoked live on
/// purpose; it is decoded from the documented envelope and covered by a
/// scripted fake instead.
pub const ERROR_RATE_LIMITED: i64 = 29;
/// Retry hint surfaced with error 29 (v2 `retry_after_seconds=1.0`).
pub const RATE_LIMIT_RETRY_SECS: f64 = 1.0;
/// Per-request wire timeout (v2 request timeout).
pub const REQUEST_TIMEOUT_SECS: u64 = 15;
/// Source name used for degradation records.
pub const SOURCE: &str = "lastfm";

/// What a call produced. `Found` carries the payload, `Missing` is an
/// authoritative "unknown entity" (Last.fm error 6; safe to treat as a
/// negative), and `Unavailable` covers everything else. Transport failure,
/// server errors, error 29, and service-offline (error 11) are recorded in
/// the sink; credential failures (errors 4, 9, 10, 17, 26), the
/// token-not-authorized reply (error 14), and the not-configured case surface
/// as `Unavailable` without a record, because v2 raises those instead of
/// recording them.
#[derive(Debug, Clone, PartialEq)]
pub enum Outcome<T> {
    /// The upstream answer, decoded.
    Found(T),
    /// Authoritative unknown-entity. Never recorded as degradation.
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

impl LastFmCredentials {
    /// Whether signed calls that need a session can be made (v2 `_can_sign`).
    /// The user-stats surface signs its reads opportunistically when this
    /// holds; that surface has not been ported yet, so this currently serves
    /// the auth checks and the wiring step.
    pub fn can_sign(&self) -> bool {
        !self.shared_secret.is_empty() && self.session_key.is_some()
    }
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

/// Last.fm client. Holds no credentials; every method takes the calling
/// user's. Stateless apart from its ports; cheap to clone.
#[derive(Debug, Clone)]
pub struct LastFmClient<P, S> {
    http: reqwest::Client,
    base_url: String,
    pacer: P,
    sink: S,
}

impl<P: Pacer, S: DegradationSink> LastFmClient<P, S> {
    /// Build a client against `base_url` (the production host or a fake).
    pub fn new(http: reqwest::Client, base_url: &str, pacer: P, sink: S) -> Self {
        Self {
            http,
            base_url: base_url.to_owned(),
            pacer,
            sink,
        }
    }

    /// `auth.getToken` with the user's own API key. Needs the shared secret
    /// for the signature but no session yet.
    pub async fn request_token(&self, creds: &LastFmCredentials) -> Outcome<AuthToken> {
        if creds.api_key.is_empty() {
            return self.unrecorded("Last.fm API key is not configured");
        }
        if creds.shared_secret.is_empty() {
            return self.unrecorded("Last.fm shared secret is required for signed requests");
        }
        let payload = match self.request("auth.getToken", creds, &[], true, false).await {
            Ok(payload) => payload,
            Err(outcome) => return outcome,
        };
        match payload.get("token").and_then(serde_json::Value::as_str) {
            Some(token) if !token.is_empty() => Outcome::Found(AuthToken {
                token: token.to_owned(),
            }),
            _ => self.shape_error("Last.fm auth.getToken response missing 'token'"),
        }
    }

    /// `auth.getSession` with the user's key pair and an approved token.
    pub async fn exchange_session(
        &self,
        creds: &LastFmCredentials,
        token: &str,
    ) -> Outcome<Session> {
        if creds.api_key.is_empty() {
            return self.unrecorded("Last.fm API key is not configured");
        }
        if creds.shared_secret.is_empty() {
            return self.unrecorded("Last.fm shared secret is required for signed requests");
        }
        let payload = match self
            .request("auth.getSession", creds, &[("token", token)], true, false)
            .await
        {
            Ok(payload) => payload,
            Err(outcome) => return outcome,
        };
        // v2 reads `data["session"]` but falls back to the top level.
        let session = payload
            .get("session")
            .and_then(serde_json::Value::as_object)
            .map_or(payload.as_object(), Some)
            .cloned();
        let session = match session {
            Some(session) => session,
            None => {
                return self
                    .shape_error("Last.fm auth.getSession response missing 'name' or 'key'");
            }
        };
        let name = session
            .get("name")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("");
        let key = session
            .get("key")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("");
        if name.is_empty() || key.is_empty() {
            return self.shape_error("Last.fm auth.getSession response missing 'name' or 'key'");
        }
        Outcome::Found(Session {
            name: name.to_owned(),
            key: key.to_owned(),
            subscriber: lenient_int(session.get("subscriber")),
        })
    }

    /// `artist.getInfo` by name or, when given, by MusicBrainz id. Unknown
    /// artists (error 6) yield `Missing`.
    pub async fn artist_info(
        &self,
        creds: &LastFmCredentials,
        artist: &str,
        mbid: Option<&str>,
    ) -> Outcome<ArtistInfo> {
        let (key, value) = match mbid {
            Some(mbid) => ("mbid", mbid),
            None => ("artist", artist),
        };
        let payload = match self
            .request("artist.getInfo", creds, &[(key, value)], false, false)
            .await
        {
            Ok(payload) => payload,
            Err(outcome) => return outcome,
        };
        match parse_artist_info(&payload) {
            Some(info) => Outcome::Found(info),
            None => self.shape_error("Last.fm artist.getInfo response missing 'name'"),
        }
    }

    /// `album.getInfo` by artist plus title or, when given, by MusicBrainz
    /// id. Unknown albums (error 6) yield `Missing`.
    pub async fn album_info(
        &self,
        creds: &LastFmCredentials,
        artist: &str,
        album: &str,
        mbid: Option<&str>,
    ) -> Outcome<AlbumInfo> {
        let params: Vec<(&str, &str)> = match mbid {
            Some(mbid) => vec![("mbid", mbid)],
            None => vec![("artist", artist), ("album", album)],
        };
        let payload = match self
            .request("album.getInfo", creds, &params, false, false)
            .await
        {
            Ok(payload) => payload,
            Err(outcome) => return outcome,
        };
        match parse_album_info(&payload) {
            Some(info) => Outcome::Found(info),
            None => self.shape_error("Last.fm album.getInfo response missing 'name'"),
        }
    }

    /// `artist.getSimilar`, most similar first.
    pub async fn similar_artists(
        &self,
        creds: &LastFmCredentials,
        artist: &str,
        mbid: Option<&str>,
        limit: u32,
    ) -> Outcome<Vec<SimilarArtist>> {
        let limit_text = limit.to_string();
        let params: Vec<(&str, &str)> = match mbid {
            Some(mbid) => vec![("mbid", mbid), ("limit", &limit_text)],
            None => vec![("artist", artist), ("limit", &limit_text)],
        };
        let payload = match self
            .request("artist.getSimilar", creds, &params, false, false)
            .await
        {
            Ok(payload) => payload,
            Err(outcome) => return outcome,
        };
        let artists = payload
            .get("similarartists")
            .and_then(|similar| similar.get("artist"))
            .and_then(serde_json::Value::as_array)
            .cloned()
            .unwrap_or_default();
        Outcome::Found(artists.iter().map(parse_similar_artist).collect())
    }

    /// `artist.getTopTracks`, most played first.
    pub async fn artist_top_tracks(
        &self,
        creds: &LastFmCredentials,
        artist: &str,
        mbid: Option<&str>,
        limit: u32,
    ) -> Outcome<Vec<TopItem>> {
        self.top_items(
            "artist.getTopTracks",
            "toptracks",
            "track",
            creds,
            artist,
            mbid,
            limit,
        )
        .await
    }

    /// `artist.getTopAlbums`, most played first.
    pub async fn artist_top_albums(
        &self,
        creds: &LastFmCredentials,
        artist: &str,
        mbid: Option<&str>,
        limit: u32,
    ) -> Outcome<Vec<TopItem>> {
        self.top_items(
            "artist.getTopAlbums",
            "topalbums",
            "album",
            creds,
            artist,
            mbid,
            limit,
        )
        .await
    }

    /// `chart.getTopArtists`: the sitewide artist chart, most played first
    /// (v2 `get_global_top_artists`). Each row's `name` is the artist.
    pub async fn chart_top_artists(
        &self,
        creds: &LastFmCredentials,
        limit: u32,
    ) -> Outcome<Vec<TopItem>> {
        let limit_text = limit.to_string();
        self.list_items(
            "chart.getTopArtists",
            "artists",
            "artist",
            creds,
            &[("limit", &limit_text)],
        )
        .await
    }

    /// `user.getTopAlbums` for one Last.fm user over `period` (`7day`,
    /// `1month`, `12month` or `overall`), most played first (v2
    /// `get_user_top_albums`).
    pub async fn user_top_albums(
        &self,
        creds: &LastFmCredentials,
        username: &str,
        period: &str,
        limit: u32,
    ) -> Outcome<Vec<TopItem>> {
        let limit_text = limit.to_string();
        self.list_items(
            "user.getTopAlbums",
            "topalbums",
            "album",
            creds,
            &[
                ("user", username),
                ("period", period),
                ("limit", &limit_text),
            ],
        )
        .await
    }

    /// `user.getTopArtists` for one Last.fm user over `period`, most played
    /// first (v2 `get_user_top_artists`). Each row's `name` is the artist.
    pub async fn user_top_artists(
        &self,
        creds: &LastFmCredentials,
        username: &str,
        period: &str,
        limit: u32,
    ) -> Outcome<Vec<TopItem>> {
        let limit_text = limit.to_string();
        self.list_items(
            "user.getTopArtists",
            "topartists",
            "artist",
            creds,
            &[
                ("user", username),
                ("period", period),
                ("limit", &limit_text),
            ],
        )
        .await
    }

    /// `user.getWeeklyArtistChart`: the user's artists this week, most
    /// played first (v2 `get_user_weekly_artist_chart`).
    pub async fn user_weekly_artist_chart(
        &self,
        creds: &LastFmCredentials,
        username: &str,
    ) -> Outcome<Vec<TopItem>> {
        self.list_items(
            "user.getWeeklyArtistChart",
            "weeklyartistchart",
            "artist",
            creds,
            &[("user", username)],
        )
        .await
    }

    /// `user.getWeeklyAlbumChart`: the user's albums this week, most played
    /// first (v2 `get_user_weekly_album_chart`). Album MBIDs name releases.
    pub async fn user_weekly_album_chart(
        &self,
        creds: &LastFmCredentials,
        username: &str,
    ) -> Outcome<Vec<TopItem>> {
        self.list_items(
            "user.getWeeklyAlbumChart",
            "weeklyalbumchart",
            "album",
            creds,
            &[("user", username)],
        )
        .await
    }

    /// `tag.getTopArtists`: the most played artists carrying one tag (v2
    /// `get_tag_top_artists`).
    pub async fn tag_top_artists(
        &self,
        creds: &LastFmCredentials,
        tag: &str,
        limit: u32,
    ) -> Outcome<Vec<TopItem>> {
        let limit_text = limit.to_string();
        self.list_items(
            "tag.getTopArtists",
            "topartists",
            "artist",
            creds,
            &[("tag", tag), ("limit", &limit_text)],
        )
        .await
    }

    /// `user.getRecentTracks`: the user's latest scrobbles, newest first
    /// (v2 `get_user_recent_tracks`).
    pub async fn user_recent_tracks(
        &self,
        creds: &LastFmCredentials,
        username: &str,
        limit: u32,
    ) -> Outcome<Vec<RecentTrack>> {
        let limit_text = limit.to_string();
        let params = [("user", username), ("limit", limit_text.as_str())];
        let payload = match self
            .request("user.getRecentTracks", creds, &params, false, false)
            .await
        {
            Ok(payload) => payload,
            Err(outcome) => return outcome,
        };
        let items = payload
            .get("recenttracks")
            .and_then(|list| list.get("track"))
            .and_then(serde_json::Value::as_array)
            .cloned()
            .unwrap_or_default();
        Outcome::Found(items.iter().filter_map(parse_recent_track).collect())
    }

    #[allow(clippy::too_many_arguments)]
    async fn top_items(
        &self,
        method: &str,
        envelope: &str,
        key: &str,
        creds: &LastFmCredentials,
        artist: &str,
        mbid: Option<&str>,
        limit: u32,
    ) -> Outcome<Vec<TopItem>> {
        let limit_text = limit.to_string();
        let params: Vec<(&str, &str)> = match mbid {
            Some(mbid) => vec![("mbid", mbid), ("limit", &limit_text)],
            None => vec![("artist", artist), ("limit", &limit_text)],
        };
        self.list_items(method, envelope, key, creds, &params).await
    }

    /// One unsigned list read: the rows under `envelope.key`, blank names
    /// dropped. A missing list reads as empty.
    async fn list_items(
        &self,
        method: &str,
        envelope: &str,
        key: &str,
        creds: &LastFmCredentials,
        params: &[(&str, &str)],
    ) -> Outcome<Vec<TopItem>> {
        let payload = match self.request(method, creds, params, false, false).await {
            Ok(payload) => payload,
            Err(outcome) => return outcome,
        };
        let items = payload
            .get(envelope)
            .and_then(|list| list.get(key))
            .and_then(serde_json::Value::as_array)
            .cloned()
            .unwrap_or_default();
        Outcome::Found(items.iter().filter_map(parse_top_item).collect())
    }

    /// Weighted artist tags from the live-verified genre surface
    /// (`artist.getTopTags` with `autocorrect=0`; verified against production
    /// on 2026-07-22, see `lastfm_MANAGEMENT_API_NOTES.md`). Unknown artists
    /// (error 6) yield an empty `Found`, exactly like v2's empty candidate
    /// tuple: no tags is an answer, not a fault.
    pub async fn artist_top_genres(
        &self,
        creds: &LastFmCredentials,
        artist: &str,
    ) -> Outcome<Vec<WeightedTag>> {
        let payload = match self
            .request(
                "artist.getTopTags",
                creds,
                &[("artist", artist), ("autocorrect", "0")],
                false,
                false,
            )
            .await
        {
            Ok(payload) => payload,
            Err(Outcome::Missing) => return Outcome::Found(Vec::new()),
            Err(outcome) => return outcome,
        };
        self.top_tags(&payload)
    }

    /// Weighted album tags from the live-verified genre surface
    /// (`album.getTopTags` with `autocorrect=0`; same verification). Unknown
    /// albums yield an empty `Found`.
    pub async fn album_top_genres(
        &self,
        creds: &LastFmCredentials,
        artist: &str,
        album: &str,
    ) -> Outcome<Vec<WeightedTag>> {
        let payload = match self
            .request(
                "album.getTopTags",
                creds,
                &[("artist", artist), ("album", album), ("autocorrect", "0")],
                false,
                false,
            )
            .await
        {
            Ok(payload) => payload,
            Err(Outcome::Missing) => return Outcome::Found(Vec::new()),
            Err(outcome) => return outcome,
        };
        self.top_tags(&payload)
    }

    /// Decode the `toptags.tag` list into weighted tags. A missing `toptags`
    /// object reads as empty (v2's tolerant structs default it); blank names
    /// are filtered before projecting (v2 `_management_genre_candidates`).
    fn top_tags(&self, payload: &serde_json::Value) -> Outcome<Vec<WeightedTag>> {
        let tags = match payload.get("toptags") {
            None | Some(serde_json::Value::Null) => return Outcome::Found(Vec::new()),
            Some(toptags) => match toptags.get("tag") {
                None | Some(serde_json::Value::Null) => return Outcome::Found(Vec::new()),
                Some(serde_json::Value::Array(tags)) => tags.clone(),
                // Last.fm sometimes answers a single object where a list
                // belongs; accept it rather than failing the whole read.
                Some(single) => vec![single.clone()],
            },
        };
        let mut weighted = Vec::new();
        for tag in &tags {
            let Some(tag) = tag.as_object() else {
                continue;
            };
            let name = tag
                .get("name")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("")
                .trim();
            if name.is_empty() {
                continue;
            }
            weighted.push(WeightedTag {
                name: name.to_owned(),
                weight: lenient_int(tag.get("count")),
            });
        }
        Outcome::Found(weighted)
    }

    /// Run one paced call. Signed calls sort their parameters (excluding
    /// `format` and `callback`), concatenate key plus value, append the
    /// shared secret, and MD5 the result (v2 `_build_api_sig`); the session
    /// key rides along as `sk` when one is linked.
    async fn request<T>(
        &self,
        method: &str,
        creds: &LastFmCredentials,
        params: &[(&str, &str)],
        signed: bool,
        post: bool,
    ) -> Result<serde_json::Value, Outcome<T>> {
        if creds.api_key.is_empty() {
            // v2 raises ConfigurationError: a missing key is a configuration
            // failure for this optional source, never a silent skip.
            return Err(self.unrecorded("Last.fm API key is not configured"));
        }
        if signed && creds.shared_secret.is_empty() {
            return Err(self.unrecorded("Last.fm shared secret is required for signed requests"));
        }
        self.pacer.acquire().await;
        let mut call: Vec<(String, String)> = Vec::new();
        call.push(("method".to_owned(), method.to_owned()));
        call.push(("api_key".to_owned(), creds.api_key.clone()));
        call.push(("format".to_owned(), "json".to_owned()));
        for (key, value) in params {
            call.push(((*key).to_owned(), (*value).to_owned()));
        }
        if signed {
            if let Some(session_key) = creds.session_key.as_deref()
                && !call.iter().any(|(key, _)| key == "sk")
            {
                call.push(("sk".to_owned(), session_key.to_owned()));
            }
            let signature = api_sig(&call, &creds.shared_secret);
            call.push(("api_sig".to_owned(), signature));
        }
        let call_refs: Vec<(&str, &str)> = call
            .iter()
            .map(|(key, value)| (key.as_str(), value.as_str()))
            .collect();
        let send = if post {
            self.http.post(self.base_url.clone()).form(&call_refs)
        } else {
            self.http.get(self.base_url.clone()).query(&call_refs)
        };
        let response = match send
            .timeout(Duration::from_secs(REQUEST_TIMEOUT_SECS))
            .send()
            .await
        {
            Ok(response) => response,
            Err(error) => {
                return Err(self.recorded(None, &format!("Last.fm request failed: {error}")));
            }
        };
        // Last.fm reports application errors as HTTP 200 bodies; a non-200
        // never carries a decoded envelope (v2 raises before parsing). The
        // live invalid-key probe saw HTTP 403 with error 10, which therefore
        // surfaces here without ever decoding the envelope.
        match response.status().as_u16() {
            200 => {}
            // Credential rejections, like the error-10 envelope in a 200:
            // unrecorded, because v2 raises those instead of recording them.
            401 | 403 => {
                return Err(self.unrecorded(&format!(
                    "Last.fm rejected the credentials (HTTP {})",
                    response.status().as_u16()
                )));
            }
            404 => return Err(Outcome::Missing),
            // HTTP-level rate limiting, distinct from the error-29 envelope:
            // recorded with the honored Retry-After when one parsed.
            429 => {
                let retry_after = response
                    .headers()
                    .get(reqwest::header::RETRY_AFTER)
                    .and_then(|value| value.to_str().ok());
                let retry_after_secs =
                    super::error::parse_retry_after(retry_after).map(|delay| delay.as_secs_f64());
                return Err(self.recorded(retry_after_secs, "Last.fm rate limited (HTTP 429)"));
            }
            status => {
                return Err(self.recorded(None, &format!("Last.fm request failed ({status})")));
            }
        }
        let payload: serde_json::Value = match response.text().await {
            Ok(text) => match serde_json::from_str(&text) {
                Ok(payload) => payload,
                Err(_) => {
                    return Err(self.shape_error("Last.fm returned invalid JSON"));
                }
            },
            Err(error) => {
                return Err(
                    self.recorded(None, &format!("Last.fm response body unreadable: {error}"))
                );
            }
        };
        // The JSON error envelope rides inside HTTP 200 (v2
        // `_handle_error_response`).
        if let Some(code) = payload.get("error") {
            let message = payload
                .get("message")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("Unknown Last.fm error");
            let code = code.as_i64();
            return Err(self.mapped_error(code, message));
        }
        self.sink.succeeded(SOURCE);
        Ok(payload)
    }

    /// Map a JSON error envelope onto an outcome (v2 `LASTFM_ERROR_MAP`
    /// plus the error-29 branch).
    fn mapped_error<T>(&self, code: Option<i64>, message: &str) -> Outcome<T> {
        match code {
            // Last.fm-29: rate limiting. Surfaces immediately with a 1s hint;
            // v2 never retries this in-repo, the caller backs off.
            Some(ERROR_RATE_LIMITED) => self.recorded(
                Some(RATE_LIMIT_RETRY_SECS),
                &format!("Rate limit exceeded: {message}"),
            ),
            // Unknown entity. v2 raises ResourceNotFoundError, which the info
            // and genre reads translate to None.
            // The service answered; the entity is just not there.
            Some(6) => {
                self.sink.succeeded(SOURCE);
                Outcome::Missing
            }
            // Credential and authorization failures. v2 raises
            // ConfigurationError / TokenNotAuthorizedError, never recorded.
            Some(4) => self.unrecorded(&format!(
                "Authentication failed - invalid API key or shared secret: {message}"
            )),
            Some(9) => self.unrecorded(&format!(
                "Session key expired - please re-authorize with Last.fm: {message}"
            )),
            Some(10) => self.unrecorded(&format!(
                "Invalid API key - check your Last.fm API key: {message}"
            )),
            Some(14) => self.unrecorded(&format!("Token not yet authorized: {message}")),
            Some(17) => self.unrecorded(&format!(
                "Authentication required - re-authorize Last.fm or make your listening history public: {message}"
            )),
            Some(26) => self.unrecorded(&format!(
                "API key has been suspended - contact Last.fm support: {message}"
            )),
            Some(2) => self.recorded(
                None,
                &format!("Invalid service - This service does not exist: {message}"),
            ),
            Some(3) => self.recorded(
                None,
                &format!("Invalid method - No method with that name in this package: {message}"),
            ),
            Some(11) => self.recorded(
                None,
                &format!("Last.fm service is temporarily offline: {message}"),
            ),
            Some(other) => self.recorded(
                None,
                &format!("Last.fm error ({other}): {message}"),
            ),
            // A present-but-unusable code reads as an unknown upstream error,
            // matching v2's fallthrough for unmapped codes.
            None => self.recorded(None, &format!("Last.fm error: {message}")),
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

    /// A reply that did not decode: recorded for the request, kept out of
    /// service health (the service answered).
    fn shape_error<T>(&self, message: &str) -> Outcome<T> {
        self.sink.record_quiet(SOURCE, message.to_owned());
        Outcome::Unavailable {
            retry_after_secs: None,
            message: message.to_owned(),
            recorded: true,
        }
    }

    /// Build an unrecorded `Unavailable` outcome for configuration and
    /// authorization failures, which v2 raises rather than records.
    fn unrecorded<T>(&self, message: &str) -> Outcome<T> {
        Outcome::Unavailable {
            retry_after_secs: None,
            message: message.to_owned(),
            recorded: false,
        }
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
