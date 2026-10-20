//! The network sources behind cover art: Cover Art Archive front covers,
//! and artist images and AudioDB thumbnails by URL.

use futures_util::future::BoxFuture;

use droppedneedle_kernel::http_client::{BodyError, read_capped};
use droppedneedle_providers::coverart::{ArtworkBytes, CaaClient, DownloadSize, EntityKind};
use droppedneedle_providers::{ProviderError, RateLimiter, RatePolicy, RequestPriority};

use super::MAX_REMOTE_BYTES;

/// Fetch one front cover. Object-safe so the service holds any client.
pub trait RemoteCovers: Send + Sync {
    /// Front cover bytes at `priority`, `Ok(None)` when the archive has
    /// none.
    fn front<'a>(
        &'a self,
        entity: EntityKind,
        mbid: &'a str,
        size: DownloadSize,
        priority: RequestPriority,
    ) -> BoxFuture<'a, Result<Option<ArtworkBytes>, ProviderError>>;
}

impl RemoteCovers for CaaClient {
    fn front<'a>(
        &'a self,
        entity: EntityKind,
        mbid: &'a str,
        size: DownloadSize,
        priority: RequestPriority,
    ) -> BoxFuture<'a, Result<Option<ArtworkBytes>, ProviderError>> {
        Box::pin(self.fetch_front(entity, mbid, size, MAX_REMOTE_BYTES, priority))
    }
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
        || host == "commons.wikimedia.org"
        // Scaled Commons portraits come from this host. Checked 2026-10-19:
        // the 1280 px thumburl of File:RadioheadO2211125 composite.jpg
        // answered 200 image/jpeg with no redirect. Exact host only.
        || host == "thumb.wikimedia.org";
    allowed.then_some(url)
}

/// Image downloads over the factory's no-redirect client: every redirect
/// hop passes [`check_image_url`] before it is followed.
pub struct HttpImages {
    client: reqwest::Client,
    gate: RateLimiter,
}

impl HttpImages {
    /// Wrap the no-redirect client at the image pacing.
    pub fn new(no_redirect: reqwest::Client) -> Self {
        Self {
            client: no_redirect,
            gate: RateLimiter::new(RatePolicy::new(IMAGE_FETCH_RATE_PER_SEC, 1)),
        }
    }

    async fn download(&self, raw: &str) -> ImageFetch {
        let Some(mut url) = check_image_url(raw) else {
            tracing::warn!(url = raw, "image URL outside the allowed hosts; skipped");
            return ImageFetch::Unusable;
        };
        if self
            .gate
            .acquire_with_priority(1, RequestPriority::ImageFetch)
            .await
            .is_err()
        {
            tracing::warn!("image pacing refused a request");
            return ImageFetch::Failed;
        }
        let mut hops = 0;
        let response = loop {
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
        if !status.is_success() {
            return ImageFetch::Unusable;
        }
        let bytes = match read_capped(response, MAX_REMOTE_BYTES).await {
            Ok(bytes) => bytes,
            Err(BodyError::TooLarge) => return ImageFetch::Unusable,
            Err(BodyError::Transport(cause)) => {
                tracing::debug!(%cause, "image download cut off");
                return ImageFetch::Failed;
            }
        };
        match droppedneedle_providers::coverart::sniff_image_content_type(&bytes) {
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

#[cfg(test)]
mod tests {
    use super::check_image_url;

    /// Only the listed hosts, over https on the default port, without
    /// credentials. A host that merely starts or ends like a listed one is
    /// refused.
    #[test]
    fn allows_listed_hosts_over_https_without_credentials() {
        let cases = [
            ("https://r2.theaudiodb.com/images/radiohead.jpg", true),
            ("https://upload.wikimedia.org/commons/Radiohead.jpg", true),
            (
                "https://commons.wikimedia.org/wiki/File:Radiohead.jpg",
                true,
            ),
            (
                "https://thumb.wikimedia.org/commons/1280px-Radiohead.jpg",
                true,
            ),
            (
                "https://thumb.wikimedia.org.example.com/commons/1280px-Radiohead.jpg",
                false,
            ),
            (
                "https://xthumb.wikimedia.org/commons/1280px-Radiohead.jpg",
                false,
            ),
            (
                "http://thumb.wikimedia.org/commons/1280px-Radiohead.jpg",
                false,
            ),
            (
                "https://thumb.wikimedia.org:8443/commons/1280px-Radiohead.jpg",
                false,
            ),
            (
                "https://user@thumb.wikimedia.org/commons/1280px-Radiohead.jpg",
                false,
            ),
        ];
        for (raw, allowed) in cases {
            assert_eq!(check_image_url(raw).is_some(), allowed, "{raw}");
        }
    }
}
