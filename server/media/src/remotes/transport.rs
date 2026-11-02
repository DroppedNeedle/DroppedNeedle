//! The HTTP side the three source clients share: the size caps and capped
//! reads, the status mapping, JSON decoding, the per-source bound on a
//! person's reads and addresses built only from the parsed base.

use std::sync::Arc;
use std::time::Duration;

use serde::de::DeserializeOwned;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

use super::models::SourceName;
use super::source::AdapterError;
use droppedneedle_kernel::http_client::{BodyError, read_capped};

/// The most one remote image may weigh, the same 20 MiB as the cover-art
/// fetch (`providers::coverart::TRANSPORT_MAX_BODY_BYTES`). Jellyfin and
/// Navidrome resize to the size asked for, a few megabytes at most. Plex
/// may send the stored picture, so a very large scan can pass the cap and
/// then shows as a missing cover.
pub const MAX_IMAGE_BYTES: usize = 20 * 1024 * 1024;

/// The most one API answer may weigh. Paged calls ask for 500 items at
/// most, a few megabytes. Three lists come whole: a playlist's tracks,
/// the starred items and Navidrome's artist index. At 1 to 3 KB a track
/// that is tens of megabytes for a playlist of ten thousand tracks, so
/// 128 MiB leaves room for a far larger one and still bounds the read.
pub const MAX_API_BYTES: usize = 128 * 1024 * 1024;

/// Read one image body under [`MAX_IMAGE_BYTES`]. An oversized image is
/// no image: the caller answers as it does for a missing one.
async fn read_image(
    source: SourceName,
    endpoint: &str,
    response: reqwest::Response,
) -> Result<Vec<u8>, AdapterError> {
    match read_capped(response, MAX_IMAGE_BYTES).await {
        Ok(bytes) => Ok(bytes),
        Err(BodyError::TooLarge) => {
            tracing::warn!(
                source = source.as_str(),
                endpoint,
                cap_bytes = MAX_IMAGE_BYTES,
                "remote image is over the size cap, treating it as missing"
            );
            Err(AdapterError::NotFound)
        }
        Err(BodyError::Transport(detail)) => Err(AdapterError::Transport(detail)),
    }
}

/// Read one API answer under [`MAX_API_BYTES`]. One over the cap is an
/// upstream error like any other answer the adapter cannot use.
async fn read_api_body(
    source: SourceName,
    endpoint: &str,
    response: reqwest::Response,
) -> Result<Vec<u8>, AdapterError> {
    match read_capped(response, MAX_API_BYTES).await {
        Ok(bytes) => Ok(bytes),
        Err(BodyError::TooLarge) => {
            tracing::warn!(
                source = source.as_str(),
                endpoint,
                cap_bytes = MAX_API_BYTES,
                "remote API answer is over the size cap"
            );
            Err(AdapterError::Api(format!(
                "{endpoint} answer is over the size cap"
            )))
        }
        Err(BodyError::Transport(detail)) => Err(AdapterError::Transport(detail)),
    }
}

/// The longest id accepted from a request. Real ones are 36 characters at
/// most.
const MAX_REQUEST_ID_BYTES: usize = 128;

/// An id that came from a request, kept only when it is a plain id:
/// ASCII letters, digits, `-` and `_`, not empty and not over-long. Plex
/// and Jellyfin take ids as part of the request path, and the web
/// framework has already decoded the id, so `../Users` or `12?x=1` would
/// send the held credential to another path. Anything else is not found,
/// with no request made.
///
/// Shapes checked: every `Id`, `ItemId`, `AlbumId` and `ParentId` in the
/// recorded Jellyfin 10.10.6 answers under `server/tests/reference/compat/`
/// is 32 hex characters, and the in-repo mocks use forms like `jf-al-1` and
/// `px-pl-1`. Plex rating keys are digits and Jellyfin also accepts the
/// dashed UUID form; neither was probed against a live server here.
pub(crate) fn request_id(id: &str) -> Result<&str, AdapterError> {
    let plain = !id.is_empty()
        && id.len() <= MAX_REQUEST_ID_BYTES
        && id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'));
    if plain {
        Ok(id)
    } else {
        Err(AdapterError::NotFound)
    }
}

/// The address a path is sent to, checked on what was built and not on
/// the text: the same server as the base address, exactly the path asked
/// for under it, and no query or fragment of the path's own.
fn checked_url(base: &str, path: &str) -> Option<reqwest::Url> {
    let url = reqwest::Url::parse(&format!("{base}{path}")).ok()?;
    let base = reqwest::Url::parse(base).ok()?;
    let expected = format!("{}{path}", base.path().trim_end_matches('/'));
    let same_server = url.origin() == base.origin()
        && url.username() == base.username()
        && url.password() == base.password();
    (same_server && url.path() == expected && url.query().is_none() && url.fragment().is_none())
        .then_some(url)
}

/// How long one remote API call may take, matching the v2 repositories.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(15);

/// The most API answers a person's reads download from one source at
/// once. Each answer may weigh up to [`MAX_API_BYTES`], so ten parallel
/// downloads from a hostile server could hold over a gigabyte for a
/// moment; a place is held until the body is read and freed before it is
/// decoded. Four keeps a page's own parallel reads (the hub reads a few at
/// once) moving. Images, audio streams and background work are not
/// counted.
pub const USER_READS_PER_SOURCE: usize = 4;

/// A transport failure with the address left out: it can carry
/// credentials (Subsonic `u`/`t`/`s`, Plex and Jellyfin tokens).
pub(crate) fn transport(cause: reqwest::Error) -> AdapterError {
    AdapterError::Transport(cause.without_url().to_string().chars().take(200).collect())
}

/// The failure a status stands for, `None` on success: 401 and 403 are a
/// rejected credential, anything else an API error naming `what`.
pub(crate) fn status_error(status: reqwest::StatusCode, what: &str) -> Option<AdapterError> {
    if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
        Some(AdapterError::Auth)
    } else if status.is_success() {
        None
    } else {
        Some(AdapterError::Api(format!("{what} failed ({status})")))
    }
}

/// Decode one payload. A shape we cannot read (a missing id included) is
/// an upstream contract error; the detail stays in the log.
pub(crate) fn decode<T: DeserializeOwned>(
    source: SourceName,
    endpoint: &str,
    bytes: &[u8],
) -> Result<T, AdapterError> {
    serde_json::from_slice(bytes).map_err(|error| {
        AdapterError::Api(format!(
            "{} returned an unreadable {endpoint} payload: {error}",
            source.display()
        ))
    })
}

/// The HTTP side the three source clients share: the timeout, the status
/// mapping, capped reads, JSON decoding and addresses built only from the
/// parsed base. Each client adds its own auth to the requests it builds.
#[derive(Clone)]
pub(crate) struct RemoteHttp {
    client: reqwest::Client,
    source: SourceName,
    bound: Option<Arc<Semaphore>>,
}

impl RemoteHttp {
    /// Calls to `source` over `client`. With a `bound`, each call waits for
    /// a place in it and holds that place until its body is read.
    pub(crate) fn new(
        client: reqwest::Client,
        source: SourceName,
        bound: Option<Arc<Semaphore>>,
    ) -> Self {
        Self {
            client,
            source,
            bound,
        }
    }

    /// The client requests are built on.
    pub(crate) fn client(&self) -> &reqwest::Client {
        &self.client
    }

    /// The address of `path` on the server at `base`. A base that does not
    /// parse names no server, which reads as not reaching it; a path that
    /// would land anywhere but exactly there is no such item.
    pub(crate) fn url(base: &str, path: &str) -> Result<reqwest::Url, AdapterError> {
        if reqwest::Url::parse(base).is_err() {
            return Err(AdapterError::Transport(
                "the server address does not parse".to_owned(),
            ));
        }
        checked_url(base, path).ok_or(AdapterError::NotFound)
    }

    /// Send one request for `endpoint` under the timeout. No answer is a
    /// transport error, and 401 or 403 a rejected credential.
    pub(crate) async fn send(
        &self,
        endpoint: &str,
        request: reqwest::RequestBuilder,
    ) -> Result<Answer, AdapterError> {
        let request = request
            .timeout(REQUEST_TIMEOUT)
            .build()
            .map_err(transport)?;
        let what = format!("{} {endpoint}", request.method());
        // The semaphore is never closed, so a place always comes.
        let permit = match &self.bound {
            Some(bound) => Arc::clone(bound).acquire_owned().await.ok(),
            None => None,
        };
        let response = self.client.execute(request).await.map_err(transport)?;
        if let Some(AdapterError::Auth) = status_error(response.status(), &what) {
            return Err(AdapterError::Auth);
        }
        Ok(Answer {
            response,
            source: self.source,
            endpoint: endpoint.to_owned(),
            what,
            _permit: permit,
        })
    }
}

/// One answer from a remote server. It keeps its place in the source's
/// bound until its body is read or it is dropped.
pub(crate) struct Answer {
    response: reqwest::Response,
    source: SourceName,
    endpoint: String,
    what: String,
    _permit: Option<OwnedSemaphorePermit>,
}

impl Answer {
    /// The answer's status.
    pub(crate) fn status(&self) -> reqwest::StatusCode {
        self.response.status()
    }

    /// The address that answered, after any redirect.
    pub(crate) fn url(&self) -> &reqwest::Url {
        self.response.url()
    }

    fn success(&self) -> Result<(), AdapterError> {
        status_error(self.response.status(), &self.what).map_or(Ok(()), Err)
    }

    /// A success status, the body left unread.
    pub(crate) fn done(self) -> Result<(), AdapterError> {
        self.success()
    }

    /// An answer to a call whose body carries nothing we use: 404 is no
    /// such call at that address, and any other failure an error. The
    /// body is still read under [`MAX_API_BYTES`].
    pub(crate) async fn acknowledged(self) -> Result<(), AdapterError> {
        if self.response.status() == reqwest::StatusCode::NOT_FOUND {
            return Err(AdapterError::NotFound);
        }
        self.body().await.map(drop)
    }

    /// The body of a successful answer, read under [`MAX_API_BYTES`].
    pub(crate) async fn body(self) -> Result<Vec<u8>, AdapterError> {
        self.success()?;
        read_api_body(self.source, &self.endpoint, self.response).await
    }

    /// The JSON body of a successful answer.
    pub(crate) async fn json<T: DeserializeOwned>(self) -> Result<T, AdapterError> {
        let (source, endpoint) = (self.source, self.endpoint.clone());
        let bytes = self.body().await?;
        decode(source, &endpoint, &bytes)
    }

    /// The image body of a successful answer, read under
    /// [`MAX_IMAGE_BYTES`], with its content type or
    /// `fallback_content_type` when it names none. A 404 is an item with
    /// no image, which is absence.
    pub(crate) async fn image(
        self,
        fallback_content_type: &str,
    ) -> Result<(Vec<u8>, String), AdapterError> {
        if self.response.status() == reqwest::StatusCode::NOT_FOUND {
            return Err(AdapterError::NotFound);
        }
        self.success()?;
        let content_type = self
            .response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .unwrap_or(fallback_content_type)
            .to_owned();
        let bytes = read_image(self.source, &self.endpoint, self.response).await?;
        Ok((bytes, content_type))
    }
}
