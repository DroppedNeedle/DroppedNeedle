//! The network sources behind cover art: Cover Art Archive front covers,
//! and artist images and AudioDB thumbnails by URL.

use futures_util::future::BoxFuture;

use crate::providers::coverart::{
    ArtworkBytes, CaaClient, CaaError, CaaTransport, DownloadSize, EntityKind, RateGate,
};

use super::MAX_REMOTE_BYTES;

/// Cover fetch pacing toward the archive. The archive documents no limit
/// and serves bytes from the Internet Archive CDN; v2 used 10 per second
/// (burst 20) so a cold grid of covers does not load one per second.
pub const COVER_FETCH_RATE_PER_SEC: f64 = 10.0;

/// Fetch one front cover. Object-safe so the service holds any client.
pub trait RemoteCovers: Send + Sync {
    /// Front cover bytes, `Ok(None)` when the archive has none.
    fn front<'a>(
        &'a self,
        entity: EntityKind,
        mbid: &'a str,
        size: DownloadSize,
    ) -> BoxFuture<'a, Result<Option<ArtworkBytes>, CaaError>>;
}

impl<T: CaaTransport + Send + Sync + 'static> RemoteCovers for CaaClient<T> {
    fn front<'a>(
        &'a self,
        entity: EntityKind,
        mbid: &'a str,
        size: DownloadSize,
    ) -> BoxFuture<'a, Result<Option<ArtworkBytes>, CaaError>> {
        Box::pin(self.fetch_front(entity, mbid, size, MAX_REMOTE_BYTES))
    }
}

/// The archive client covers use: the shared no-redirect HTTP client (each
/// redirect hop is checked by the client) at the cover pacing.
pub fn cover_client<T: CaaTransport>(transport: T) -> CaaClient<T> {
    CaaClient::new(transport).with_gate(RateGate::new(COVER_FETCH_RATE_PER_SEC))
}

/// Pacing for artist and AudioDB image downloads. These come from CDNs
/// (TheAudioDB's and Wikimedia's), not the lookup APIs, so a modest pace
/// keeps a cold artist grid moving without hammering them.
pub const IMAGE_FETCH_RATE_PER_SEC: f64 = 5.0;
/// Redirect hops followed for one image, each checked like the first URL.
pub const MAX_IMAGE_REDIRECTS: usize = 3;
/// Per-request timeout for one image download.
pub const IMAGE_FETCH_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);

/// What one image download produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImageFetch {
    /// The image.
    Found(ArtworkBytes),
    /// The host answered, but not with a usable image (404, wrong type,
    /// too large, or a URL outside the allowed hosts).
    Unusable,
    /// The host could not be reached or failed; worth asking again soon.
    Failed,
}

/// Download one artist image or AudioDB thumbnail by URL.
pub trait RemoteImages: Send + Sync {
    /// Fetch `url`. Only https URLs on the allowed image hosts are fetched.
    fn fetch<'a>(&'a self, url: &'a str) -> BoxFuture<'a, ImageFetch>;
}

/// Validate a third-party image URL: https on the default port, no
/// credentials, and a host TheAudioDB or Wikimedia serves images from (v2
/// `validate_audiodb_image_url`, widened to Wikimedia for the Wikidata
/// portraits). The URLs come from upstream data, so this keeps the server
/// from being pointed anywhere else.
pub fn check_image_url(raw: &str) -> Option<reqwest::Url> {
    let url = reqwest::Url::parse(raw.trim()).ok()?;
    if url.scheme() != "https"
        || !url.username().is_empty()
        || url.password().is_some()
        || url.port().is_some_and(|port| port != 443)
    {
        return None;
    }
    let host = url.domain()?.to_ascii_lowercase();
    let allowed = host == "theaudiodb.com"
        || host.ends_with(".theaudiodb.com")
        || host == "upload.wikimedia.org"
        || host == "commons.wikimedia.org";
    allowed.then_some(url)
}

/// Image downloads over the factory's no-redirect client: every redirect
/// hop passes [`check_image_url`] before it is followed.
pub struct HttpImages {
    client: reqwest::Client,
    gate: RateGate,
}

impl HttpImages {
    /// Wrap the no-redirect client at the image pacing.
    pub fn new(no_redirect: reqwest::Client) -> Self {
        Self {
            client: no_redirect,
            gate: RateGate::new(IMAGE_FETCH_RATE_PER_SEC),
        }
    }

    async fn download(&self, raw: &str) -> ImageFetch {
        let Some(mut url) = check_image_url(raw) else {
            tracing::warn!(url = raw, "image URL outside the allowed hosts; skipped");
            return ImageFetch::Unusable;
        };
        self.gate.acquire().await;
        let mut hops = 0;
        let mut response = loop {
            let response = match self
                .client
                .get(url.clone())
                .timeout(IMAGE_FETCH_TIMEOUT)
                .send()
                .await
            {
                Ok(response) => response,
                Err(error) => {
                    tracing::debug!(%error, "image download failed");
                    return ImageFetch::Failed;
                }
            };
            if !response.status().is_redirection() {
                break response;
            }
            hops += 1;
            let next = response
                .headers()
                .get(reqwest::header::LOCATION)
                .and_then(|value| value.to_str().ok())
                .and_then(|location| url.join(location).ok())
                .and_then(|next| check_image_url(next.as_str()));
            match next {
                Some(next) if hops <= MAX_IMAGE_REDIRECTS => url = next,
                _ => return ImageFetch::Unusable,
            }
        };
        let status = response.status();
        if status.is_server_error() || status.as_u16() == 429 {
            return ImageFetch::Failed;
        }
        if !status.is_success()
            || response
                .content_length()
                .is_some_and(|length| length > MAX_REMOTE_BYTES as u64)
        {
            return ImageFetch::Unusable;
        }
        let mut bytes = Vec::new();
        loop {
            match response.chunk().await {
                Ok(Some(chunk)) => {
                    bytes.extend_from_slice(&chunk);
                    if bytes.len() > MAX_REMOTE_BYTES {
                        return ImageFetch::Unusable;
                    }
                }
                Ok(None) => break,
                Err(error) => {
                    tracing::debug!(%error, "image download cut off");
                    return ImageFetch::Failed;
                }
            }
        }
        match crate::providers::coverart::sniff_image_content_type(&bytes) {
            Some(content_type) => ImageFetch::Found(ArtworkBytes {
                content_type: content_type.to_owned(),
                bytes,
            }),
            None => ImageFetch::Unusable,
        }
    }
}

impl RemoteImages for HttpImages {
    fn fetch<'a>(&'a self, url: &'a str) -> BoxFuture<'a, ImageFetch> {
        Box::pin(self.download(url))
    }
}
