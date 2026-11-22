//! Deemix's web API, the routes DroppedNeedle uses and no others.
//!
//! Deemix is a service the user runs: one factory client that follows no
//! redirect, never paced. It has no published API and no sign-in of its
//! own, so every answer is held to JSON (its content type and its decode):
//! a wrong base path answers 200 with the web UI's page, which reads as
//! [`DeemixError::NotDeemix`]. Requests are built from typed values only.
//! An add without a valid `url` and `bitrate` crashes Deemix and drops
//! every session, so [`QueueTarget`] and [`Bitrate`] cannot hold a bad one.
//!
//! Every call carries the [`DeemixSession`] cookie and keeps the one an
//! answer sets. Calls are not retried: an add sent twice could queue
//! twice, and the reads are cheap to make again on the next pass.

use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use reqwest::header::{CONTENT_TYPE, COOKIE, HeaderMap};
use reqwest::{Client, Method, Url};
use serde::Deserialize;
use serde::de::DeserializeOwned;
use thiserror::Error;

use super::models::{
    AddAnswer, AddOutcome, AddRefusal, Arl, Connect, LoginAnswer, LoginStatus, Queue, RemoveAnswer,
    SearchAlbum, SearchAnswer, Tracklist, TracklistAnswer,
};
use super::session::{DeemixSession, SESSION_COOKIE};
use droppedneedle_kernel::http_client::{BodyError, read_capped, transport_error};
use droppedneedle_providers::executor::decode_failure;

/// The cap on `/connect` and `getQueue`, which carry the whole queue.
const MAX_QUEUE_BODY_BYTES: usize = 32 * 1024 * 1024;

/// The cap on every other answer.
const MAX_BODY_BYTES: usize = 4 * 1024 * 1024;

/// The longest search term sent.
const MAX_TERM_CHARS: usize = 200;

/// The most search hits asked for.
const MAX_SEARCH_HITS: u32 = 50;

/// The largest id Deemix's JavaScript holds exactly.
const MAX_DEEZER_ID: u64 = (1 << 53) - 1;

/// A Deemix route DroppedNeedle calls. There are no others.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Route {
    Connect,
    LoginArl,
    Search,
    GetTracklist,
    AddToQueue,
    GetQueue,
    RemoveFromQueue,
}

impl Route {
    /// The segment below `/api`.
    fn name(self) -> &'static str {
        match self {
            Route::Connect => "connect",
            Route::LoginArl => "loginArl",
            Route::Search => "search",
            Route::GetTracklist => "getTracklist",
            Route::AddToQueue => "addToQueue",
            Route::GetQueue => "getQueue",
            Route::RemoveFromQueue => "removeFromQueue",
        }
    }

    fn method(self) -> Method {
        match self {
            Route::Connect | Route::Search | Route::GetTracklist | Route::GetQueue => Method::GET,
            Route::LoginArl | Route::AddToQueue | Route::RemoveFromQueue => Method::POST,
        }
    }

    fn cap(self) -> usize {
        match self {
            Route::Connect | Route::GetQueue => MAX_QUEUE_BODY_BYTES,
            _ => MAX_BODY_BYTES,
        }
    }
}

impl fmt::Display for Route {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "/api/{}", self.name())
    }
}

/// A Deemix call that failed. No variant holds a sign-in, a cookie or an
/// answer's text.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum DeemixError {
    /// The base URL is not an http or https address.
    #[error("the Deemix URL is not a usable address")]
    BadUrl,
    /// A value no request is built from. Nothing was sent.
    #[error("not a request Deemix takes: {0}")]
    InvalidRequest(&'static str),
    /// No answer: the transport broke before a status line.
    #[error("Deemix is unavailable: {0}")]
    Unavailable(String),
    /// A status other than 2xx. A 3xx is a redirect, which is not followed.
    #[error("Deemix answered {route} with HTTP {status}")]
    Status { route: Route, status: u16 },
    /// The answer is not JSON: almost always the web UI's page, because
    /// the URL points somewhere other than Deemix's root.
    #[error("the answer to {route} is not from Deemix; check the URL points at Deemix")]
    NotDeemix { route: Route },
    /// JSON this version does not read.
    #[error("Deemix answered {route} in a shape this version does not read: {detail}")]
    Shape { route: Route, detail: String },
    /// The answer passed its size cap.
    #[error("the answer to {route} is too large to read")]
    TooLarge { route: Route },
    /// The add was refused: this session is not signed in to Deezer.
    #[error("Deemix is not signed in to Deezer")]
    NotSignedIn,
    /// The add was refused: the account cannot download that bitrate.
    #[error("the Deezer account cannot download at that quality")]
    CantStream,
    /// Deemix refused the request for another reason.
    #[error("Deemix refused the {route} request")]
    Refused { route: Route },
}

/// The quality an add asks for, by Deemix's bitrate code.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Deserialize)]
#[serde(try_from = "u8")]
pub enum Bitrate {
    /// MP3 128 kbps (1).
    Mp3At128,
    /// MP3 320 kbps (3).
    Mp3At320,
    /// FLAC (9).
    Flac,
}

impl Bitrate {
    /// Deemix's code for the bitrate.
    #[must_use]
    pub fn code(self) -> u8 {
        match self {
            Bitrate::Mp3At128 => 1,
            Bitrate::Mp3At320 => 3,
            Bitrate::Flac => 9,
        }
    }
}

impl TryFrom<u8> for Bitrate {
    type Error = DeemixError;

    fn try_from(code: u8) -> Result<Self, Self::Error> {
        match code {
            1 => Ok(Bitrate::Mp3At128),
            3 => Ok(Bitrate::Mp3At320),
            9 => Ok(Bitrate::Flac),
            _ => Err(DeemixError::InvalidRequest(
                "a bitrate other than 1, 3 or 9",
            )),
        }
    }
}

/// What a queue item is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ItemKind {
    Album,
    Track,
}

impl ItemKind {
    fn name(self) -> &'static str {
        match self {
            ItemKind::Album => "album",
            ItemKind::Track => "track",
        }
    }
}

/// One Deezer album or track to queue.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct QueueTarget {
    kind: ItemKind,
    id: u64,
}

impl QueueTarget {
    /// The album `id`.
    pub fn album(id: u64) -> Result<Self, DeemixError> {
        QueueTarget::new(ItemKind::Album, id)
    }

    /// The track `id`.
    pub fn track(id: u64) -> Result<Self, DeemixError> {
        QueueTarget::new(ItemKind::Track, id)
    }

    fn new(kind: ItemKind, id: u64) -> Result<Self, DeemixError> {
        if id == 0 || id > MAX_DEEZER_ID {
            return Err(DeemixError::InvalidRequest("a Deezer id out of range"));
        }
        Ok(QueueTarget { kind, id })
    }

    #[must_use]
    pub fn kind(&self) -> ItemKind {
        self.kind
    }

    #[must_use]
    pub fn id(&self) -> u64 {
        self.id
    }

    /// The uuid Deemix names the item by at `bitrate`.
    #[must_use]
    pub fn uuid(&self, bitrate: Bitrate) -> QueueUuid {
        QueueUuid(format!(
            "{}_{}_{}",
            self.kind.name(),
            self.id,
            bitrate.code()
        ))
    }

    /// The one link an add sends. Deemix parses it as a link, not an
    /// address it fetches.
    fn link(&self) -> String {
        format!("https://www.deezer.com/{}/{}", self.kind.name(), self.id)
    }
}

/// A queue item uuid of the shape DroppedNeedle adds:
/// `^(album|track)_\d+_(1|3|9)$`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Deserialize)]
#[serde(try_from = "String")]
pub struct QueueUuid(String);

impl TryFrom<String> for QueueUuid {
    type Error = &'static str;

    fn try_from(text: String) -> Result<Self, Self::Error> {
        QueueUuid::parse(&text).ok_or("a queue uuid DroppedNeedle does not add")
    }
}

impl QueueUuid {
    /// The uuid in `text`, when it has that shape.
    #[must_use]
    pub fn parse(text: &str) -> Option<QueueUuid> {
        let (kind, rest) = text.split_once('_')?;
        let (id, bitrate) = rest.split_once('_')?;
        let digits = !id.is_empty() && id.bytes().all(|byte| byte.is_ascii_digit());
        (matches!(kind, "album" | "track") && digits && matches!(bitrate, "1" | "3" | "9"))
            .then(|| QueueUuid(text.to_owned()))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// One Deemix server, over the factory's client that follows no redirect.
/// The base URL is parsed once; every route is built on it.
pub struct DeemixClient {
    http: Client,
    base: Url,
    session: Arc<DeemixSession>,
    timeout: Duration,
}

impl fmt::Debug for DeemixClient {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DeemixClient")
            .field("base", &self.base.as_str())
            .field("session", &self.session)
            .field("timeout", &self.timeout)
            .finish()
    }
}

/// The base URL a client for the Deemix at `raw` calls: an http or https
/// address with a host, its query and fragment left out.
pub fn parse_base(raw: &str) -> Result<Url, DeemixError> {
    let mut base = Url::parse(raw.trim()).map_err(|_| DeemixError::BadUrl)?;
    if !matches!(base.scheme(), "http" | "https")
        || base.cannot_be_a_base()
        || base.host().is_none()
    {
        return Err(DeemixError::BadUrl);
    }
    base.set_query(None);
    base.set_fragment(None);
    Ok(base)
}

impl DeemixClient {
    /// Build over `http` (the factory's `no_redirect()` client) for the
    /// Deemix at `base_url`, sharing `session`. Each call gives up after
    /// `timeout`.
    pub fn new(
        http: Client,
        base_url: &str,
        session: Arc<DeemixSession>,
        timeout: Duration,
    ) -> Result<Self, DeemixError> {
        Ok(DeemixClient {
            http,
            base: parse_base(base_url)?,
            session,
            timeout,
        })
    }

    /// The parsed base URL.
    #[must_use]
    pub fn base_url(&self) -> &Url {
        &self.base
    }

    /// The session this client's calls carry.
    #[must_use]
    pub fn session(&self) -> &DeemixSession {
        &self.session
    }

    /// `GET /api/connect`: versions, sign-in state, settings, and the saved
    /// ARL when this session is signed out.
    pub async fn connect(&self) -> Result<Connect, DeemixError> {
        self.call(Route::Connect, &[], None).await
    }

    /// `POST /api/loginArl` with `arl`, which is used up. Only the answer's
    /// `status` is read; it echoes the ARL back.
    pub async fn login_arl(&self, arl: Arl) -> Result<LoginStatus, DeemixError> {
        let body = serde_json::json!({ "arl": arl.secret() }).to_string();
        let answer: LoginAnswer = self
            .call(Route::LoginArl, &[], Some(body.into_bytes()))
            .await?;
        Ok(answer.status)
    }

    /// `GET /api/search?type=album`: at most `limit` album hits for `term`,
    /// which is cut to 200 characters with control characters taken out.
    pub async fn search_albums(
        &self,
        term: &str,
        limit: u32,
    ) -> Result<Vec<SearchAlbum>, DeemixError> {
        let term = search_term(term).ok_or(DeemixError::InvalidRequest("an empty search term"))?;
        let limit = limit.clamp(1, MAX_SEARCH_HITS).to_string();
        let answer: SearchAnswer = self
            .call(
                Route::Search,
                &[
                    ("term", term.as_str()),
                    ("type", "album"),
                    ("start", "0"),
                    ("nb", limit.as_str()),
                ],
                None,
            )
            .await?;
        if answer.error.is_some_and(|error| !error.is_empty()) {
            return Err(DeemixError::Refused {
                route: Route::Search,
            });
        }
        Ok(answer.data)
    }

    /// `GET /api/getTracklist?type=album`: the album's tracks, or `None`
    /// when Deezer has no such album.
    pub async fn album_tracklist(&self, id: u64) -> Result<Option<Tracklist>, DeemixError> {
        let album = QueueTarget::album(id)?;
        let id = album.id().to_string();
        let answer: TracklistAnswer = self
            .call(
                Route::GetTracklist,
                &[("id", id.as_str()), ("type", "album")],
                None,
            )
            .await?;
        answer
            .into_tracklist()
            .map_err(|detail| DeemixError::Shape {
                route: Route::GetTracklist,
                detail: detail.to_owned(),
            })
    }

    /// `POST /api/addToQueue` with one link and a bitrate: the uuid of the
    /// item it queued, or [`AddOutcome::NothingAdded`].
    pub async fn add_to_queue(
        &self,
        target: QueueTarget,
        bitrate: Bitrate,
    ) -> Result<AddOutcome, DeemixError> {
        let body = serde_json::json!({ "url": target.link(), "bitrate": bitrate.code() });
        let answer: AddAnswer = self
            .call(Route::AddToQueue, &[], Some(body.to_string().into_bytes()))
            .await?;
        if !answer.result {
            return Err(match answer.errid {
                Some(AddRefusal::NotLoggedIn) => DeemixError::NotSignedIn,
                Some(AddRefusal::CantStream) => DeemixError::CantStream,
                Some(AddRefusal::Other) | None => DeemixError::Refused {
                    route: Route::AddToQueue,
                },
            });
        }
        let shape = |detail: &str| DeemixError::Shape {
            route: Route::AddToQueue,
            detail: detail.to_owned(),
        };
        let queued = answer
            .data
            .and_then(|data| data.obj)
            .ok_or_else(|| shape("an accepted add that lists nothing"))?;
        match queued.first() {
            None => Ok(AddOutcome::NothingAdded),
            Some(item) => QueueUuid::parse(&item.uuid)
                .map(AddOutcome::Queued)
                .ok_or_else(|| shape("an added item with a uuid DroppedNeedle does not add")),
        }
    }

    /// `GET /api/getQueue`.
    pub async fn queue(&self) -> Result<Queue, DeemixError> {
        self.call(Route::GetQueue, &[], None).await
    }

    /// `POST /api/removeFromQueue?uuid=`. `uuid` must be one of the shape
    /// DroppedNeedle adds; anything else is refused before it is sent.
    /// Removing a uuid Deemix does not hold is not an error.
    pub async fn remove_from_queue(&self, uuid: &str) -> Result<(), DeemixError> {
        let uuid = QueueUuid::parse(uuid).ok_or(DeemixError::InvalidRequest(
            "a queue uuid DroppedNeedle does not add",
        ))?;
        let answer: RemoveAnswer = self
            .call(Route::RemoveFromQueue, &[("uuid", uuid.as_str())], None)
            .await?;
        if answer.result {
            Ok(())
        } else {
            Err(DeemixError::Refused {
                route: Route::RemoveFromQueue,
            })
        }
    }

    /// The address of `route`, built on the parsed base.
    fn url(&self, route: Route, query: &[(&str, &str)]) -> Result<Url, DeemixError> {
        let mut url = self.base.clone();
        url.path_segments_mut()
            .map_err(|()| DeemixError::BadUrl)?
            .pop_if_empty()
            .extend(["api", route.name()]);
        if !query.is_empty() {
            url.query_pairs_mut().extend_pairs(query);
        }
        Ok(url)
    }

    async fn call<T: DeserializeOwned>(
        &self,
        route: Route,
        query: &[(&str, &str)],
        body: Option<Vec<u8>>,
    ) -> Result<T, DeemixError> {
        let mut request = self
            .http
            .request(route.method(), self.url(route, query)?)
            .timeout(self.timeout);
        if let Some(body) = body {
            request = request.header(CONTENT_TYPE, "application/json").body(body);
        }
        if let Some(cookie) = self.session.cookie_for(&self.base) {
            request = request.header(COOKIE, format!("{SESSION_COOKIE}={cookie}"));
        }
        let response = request
            .send()
            .await
            .map_err(|error| DeemixError::Unavailable(transport_error(error)))?;
        self.session.keep_from(&self.base, response.headers());
        let status = response.status().as_u16();
        if !(200..300).contains(&status) {
            return Err(DeemixError::Status { route, status });
        }
        if !is_json(response.headers()) {
            return Err(DeemixError::NotDeemix { route });
        }
        let bytes = read_capped(response, route.cap())
            .await
            .map_err(|error| match error {
                BodyError::TooLarge => DeemixError::TooLarge { route },
                BodyError::Transport(detail) => DeemixError::Unavailable(detail),
            })?;
        serde_json::from_slice(&bytes).map_err(|error| match error.classify() {
            serde_json::error::Category::Data => DeemixError::Shape {
                route,
                detail: decode_failure(&error),
            },
            _ => DeemixError::NotDeemix { route },
        })
    }
}

/// Whether the answer says it is JSON.
fn is_json(headers: &HeaderMap) -> bool {
    headers
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(';').next())
        .is_some_and(|essence| essence.trim().eq_ignore_ascii_case("application/json"))
}

/// `term` with control characters taken out, trimmed and cut to 200
/// characters; `None` when nothing is left.
fn search_term(term: &str) -> Option<String> {
    let clean: String = term.chars().filter(|c| !c.is_control()).collect();
    let cut: String = clean.trim().chars().take(MAX_TERM_CHARS).collect();
    let cut = cut.trim_end().to_owned();
    (!cut.is_empty()).then_some(cut)
}
