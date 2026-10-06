//! TheAudioDB artwork client.
//!
//! Ports v2's AudioDB repository plus its wire models: artist and album lookups by MusicBrainz id or by
//! name, returning the artwork URLs TheAudioDB knows. The source is optional,
//! so every failure fails soft into [`Outcome`]: a dead upstream records one
//! degradation note and yields [`Outcome::Unavailable`], while "no match"
//! yields [`Outcome::Missing`].
//!
//! Pacing and degradation ride the shared core traits
//! ([`Pacer`](super::limiter::Pacer), [`DegradationSink`](super::degradation::DegradationSink)):
//! wire the pacer to the free 30/minute bucket ([`FREE_RATE_PER_SEC`] /
//! [`FREE_BURST`]) or the premium bucket ([`PREMIUM_RATE_PER_SEC`] /
//! [`PREMIUM_BURST`]) per v2 `_make_rate_limiter`.
//!
//! The enabled flag and the settings API key live in user preferences in v2;
//! wiring passes them in here ([`AudioDbClient::with_enabled`],
//! [`AudioDbClient::with_api_key`]) so this client stays a pure HTTP caller.

use std::time::Duration;

use super::{DegradationSink, Pacer};

/// Default API host (v2 `AUDIODB_API_URL`, without the key segment).
pub const DEFAULT_BASE_URL: &str = "https://www.theaudiodb.com/api/v1/json";
/// Free key, used when no settings key is configured (v2 `AUDIODB_FREE_KEY`).
pub const FREE_API_KEY: &str = "123";
/// Free pacing: 30 requests/minute (v2 free bucket).
pub const FREE_RATE_PER_SEC: f64 = 0.5;
/// Free bucket burst (v2 free bucket).
pub const FREE_BURST: u32 = 2;
/// Premium pacing (v2 premium bucket).
pub const PREMIUM_RATE_PER_SEC: f64 = 5.0;
/// Premium bucket burst (v2 premium bucket).
pub const PREMIUM_BURST: u32 = 10;
/// Wait hint surfaced with a 429 (v2 raises with `retry_after_seconds=60`).
pub const RATE_LIMIT_RETRY_SECS: f64 = 60.0;
/// Per-request wire timeout (v2 request timeout).
pub const REQUEST_TIMEOUT_SECS: u64 = 15;
/// Source name used for degradation records.
pub const SOURCE: &str = "audiodb";

/// What a lookup produced. `Found` carries the artwork, `Missing` is an
/// authoritative "no match" (safe to treat as a negative), and `Unavailable`
/// covers transport failure, rate limiting, server errors, and schema
/// failures. Every `Unavailable` is already recorded in the sink, except the
/// disabled case, which v2 also leaves unrecorded.
#[derive(Debug, Clone, PartialEq)]
pub enum Outcome<T> {
    /// Artwork for the requested entity.
    Found(T),
    /// Authoritative no-match. Never recorded as degradation.
    Missing,
    /// The source could not answer. Already recorded in the sink.
    Unavailable {
        /// Seconds the caller should wait before retrying, when known.
        retry_after_secs: Option<f64>,
        /// Short human-readable reason (never carries secrets).
        message: String,
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

/// Artist artwork (v2 `AudioDBArtistResponse`). The two identity fields are
/// required, so a payload missing them fails decode instead of yielding an
/// empty success; every artwork URL stays optional, and unknown fields are
/// ignored exactly like v2's tolerant structs.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct AudioDbArtist {
    /// TheAudioDB artist id.
    #[serde(rename = "idArtist")]
    pub id_artist: String,
    /// Artist name.
    #[serde(rename = "strArtist")]
    pub name: String,
    /// MusicBrainz artist id, when the entry carries one.
    #[serde(rename = "strMusicBrainzID", default)]
    pub mbid: Option<String>,
    /// Artist thumbnail URL.
    #[serde(rename = "strArtistThumb", default)]
    pub thumb: Option<String>,
    /// Artist fanart URLs.
    #[serde(rename = "strArtistFanart", default)]
    pub fanart: Option<String>,
    /// Second fanart URL.
    #[serde(rename = "strArtistFanart2", default)]
    pub fanart_2: Option<String>,
    /// Third fanart URL.
    #[serde(rename = "strArtistFanart3", default)]
    pub fanart_3: Option<String>,
    /// Fourth fanart URL.
    #[serde(rename = "strArtistFanart4", default)]
    pub fanart_4: Option<String>,
    /// Wide thumbnail URL.
    #[serde(rename = "strArtistWideThumb", default)]
    pub wide_thumb: Option<String>,
    /// Banner URL.
    #[serde(rename = "strArtistBanner", default)]
    pub banner: Option<String>,
    /// Logo URL.
    #[serde(rename = "strArtistLogo", default)]
    pub logo: Option<String>,
    /// Cutout URL.
    #[serde(rename = "strArtistCutout", default)]
    pub cutout: Option<String>,
    /// Clearart URL.
    #[serde(rename = "strArtistClearart", default)]
    pub clearart: Option<String>,
}

/// Album artwork (v2 `AudioDBAlbumResponse`). Identity required, artwork
/// optional, unknown fields ignored, just like the artist shape.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct AudioDbAlbum {
    /// TheAudioDB album id.
    #[serde(rename = "idAlbum")]
    pub id_album: String,
    /// Album title.
    #[serde(rename = "strAlbum")]
    pub title: String,
    /// MusicBrainz release id, when the entry carries one.
    #[serde(rename = "strMusicBrainzID", default)]
    pub mbid: Option<String>,
    /// Album thumbnail URL.
    #[serde(rename = "strAlbumThumb", default)]
    pub thumb: Option<String>,
    /// Back cover URL.
    #[serde(rename = "strAlbumBack", default)]
    pub back: Option<String>,
    /// CD art URL.
    #[serde(rename = "strAlbumCDart", default)]
    pub cdart: Option<String>,
    /// Spine URL.
    #[serde(rename = "strAlbumSpine", default)]
    pub spine: Option<String>,
    /// 3D case URL.
    #[serde(rename = "strAlbum3DCase", default)]
    pub case_3d: Option<String>,
    /// 3D flat URL.
    #[serde(rename = "strAlbum3DFlat", default)]
    pub flat_3d: Option<String>,
    /// 3D face URL.
    #[serde(rename = "strAlbum3DFace", default)]
    pub face_3d: Option<String>,
    /// 3D thumbnail URL.
    #[serde(rename = "strAlbum3DThumb", default)]
    pub thumb_3d: Option<String>,
}

/// TheAudioDB client. Stateless apart from its ports; cheap to clone.
#[derive(Debug, Clone)]
pub struct AudioDbClient<P, S> {
    http: reqwest::Client,
    base_url: String,
    api_key: String,
    enabled: bool,
    pacer: P,
    sink: S,
}

impl<P: Pacer, S: DegradationSink> AudioDbClient<P, S> {
    /// Build a client against `base_url` (the production host or a fake)
    /// using the free key. Wiring overrides both via the builders below.
    pub fn new(http: reqwest::Client, base_url: &str, pacer: P, sink: S) -> Self {
        Self {
            http,
            base_url: base_url.trim_end_matches('/').to_owned(),
            api_key: FREE_API_KEY.to_owned(),
            enabled: true,
            pacer,
            sink,
        }
    }

    /// Use `api_key` instead of the free key (v2 reads the settings key when
    /// one is configured and otherwise falls back to `"123"`).
    pub fn with_api_key(mut self, api_key: &str) -> Self {
        if !api_key.trim().is_empty() {
            self.api_key = api_key.to_owned();
        }
        self
    }

    /// Enable or disable the source (v2 `audiodb_enabled`). A disabled client
    /// answers `Missing` without touching the wire or the sink.
    pub fn with_enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }

    /// Look an artist up by MusicBrainz id (`artist-mb.php`).
    pub async fn artist_by_mbid(&self, mbid: &str) -> Outcome<AudioDbArtist> {
        if !self.enabled || mbid.is_empty() {
            return Outcome::Missing;
        }
        let payload = match self.request("artist-mb.php", &[("i", mbid)]).await {
            Ok(payload) => payload,
            Err(outcome) => return outcome,
        };
        self.first(payload, "artists", &format!("artist mbid {mbid}"))
    }

    /// Look an album up by MusicBrainz id (`album-mb.php`).
    pub async fn album_by_mbid(&self, mbid: &str) -> Outcome<AudioDbAlbum> {
        if !self.enabled || mbid.is_empty() {
            return Outcome::Missing;
        }
        let payload = match self.request("album-mb.php", &[("i", mbid)]).await {
            Ok(payload) => payload,
            Err(outcome) => return outcome,
        };
        self.first(payload, "album", &format!("album mbid {mbid}"))
    }

    /// Search for an artist by name (`search.php`).
    pub async fn search_artist(&self, name: &str) -> Outcome<AudioDbArtist> {
        if !self.enabled || name.is_empty() {
            return Outcome::Missing;
        }
        let payload = match self.request("search.php", &[("s", name)]).await {
            Ok(payload) => payload,
            Err(outcome) => return outcome,
        };
        self.first(payload, "artists", "artist name search")
    }

    /// Search for an album by artist and title (`searchalbum.php`).
    pub async fn search_album(&self, artist: &str, album: &str) -> Outcome<AudioDbAlbum> {
        if !self.enabled || artist.is_empty() || album.is_empty() {
            return Outcome::Missing;
        }
        let payload = match self
            .request("searchalbum.php", &[("s", artist), ("a", album)])
            .await
        {
            Ok(payload) => payload,
            Err(outcome) => return outcome,
        };
        self.first(payload, "album", "album name search")
    }

    /// Run one paced GET. On success the decoded envelope comes back; every
    /// failure mode already carries its outcome (and its sink record, when
    /// v2 records one).
    async fn request<T>(
        &self,
        endpoint: &str,
        params: &[(&str, &str)],
    ) -> Result<serde_json::Value, Outcome<T>> {
        self.pacer.acquire().await;
        let url = format!(
            "{base}/{key}/{endpoint}",
            base = self.base_url,
            key = self.api_key
        );
        let response = match self
            .http
            .get(url)
            .query(params)
            .timeout(Duration::from_secs(REQUEST_TIMEOUT_SECS))
            .send()
            .await
        {
            Ok(response) => response,
            Err(error) => {
                // v2 wraps transport failure in ExternalServiceError, which
                // the retry wrapper treats as retriable and the optional
                // funnel degrades.
                return Err(self.unavailable(None, format!("AudioDB request failed: {error}")));
            }
        };
        let status = response.status().as_u16();
        if status == 429 {
            return Err(self.unavailable(
                Some(RATE_LIMIT_RETRY_SECS),
                "AudioDB rate limit exceeded".to_owned(),
            ));
        }
        if status == 404 {
            // v2 returns None on 404: "not found" is an answer, not a fault.
            return Err(Outcome::Missing);
        }
        if status != 200 {
            return Err(self.unavailable(None, format!("AudioDB request failed ({status})")));
        }
        match response.text().await {
            Ok(text) => match serde_json::from_str(&text) {
                Ok(payload) => {
                    self.sink.succeeded(SOURCE);
                    Ok(payload)
                }
                Err(_) => {
                    let message = "AudioDB returned invalid JSON".to_owned();
                    self.sink.record_quiet(SOURCE, message.clone());
                    Err(Outcome::Unavailable {
                        retry_after_secs: None,
                        message,
                    })
                }
            },
            Err(error) => {
                Err(self.unavailable(None, format!("AudioDB response body unreadable: {error}")))
            }
        }
    }

    /// Pull the first entry of `key` out of the envelope (v2
    /// `_extract_first`): a missing, null, empty, or non-list member means
    /// "no match", never an error. Schema failure on the first entry records
    /// a degradation note and yields `Unavailable`, exactly like v2's
    /// `msgspec.convert` failure path.
    fn first<T>(&self, payload: serde_json::Value, key: &str, what: &str) -> Outcome<T>
    where
        T: serde::de::DeserializeOwned,
    {
        let item = payload
            .get(key)
            .and_then(serde_json::Value::as_array)
            .and_then(|items| items.first())
            .cloned();
        match item {
            None => Outcome::Missing,
            Some(item) => match serde_json::from_value(item) {
                Ok(decoded) => Outcome::Found(decoded),
                Err(error) => {
                    // A shape problem, not an outage: kept out of service
                    // health.
                    let message = format!("Schema error for {what}: {error}");
                    self.sink.record_quiet(SOURCE, message.clone());
                    Outcome::Unavailable {
                        retry_after_secs: None,
                        message,
                    }
                }
            },
        }
    }

    /// Build an `Unavailable` outcome and record it in the sink.
    fn unavailable<T>(&self, retry_after_secs: Option<f64>, message: String) -> Outcome<T> {
        self.sink.record(SOURCE, message.clone());
        Outcome::Unavailable {
            retry_after_secs,
            message,
        }
    }
}
