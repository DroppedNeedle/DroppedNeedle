//! The network sources behind cover art: Cover Art Archive front covers,
//! and artist images and AudioDB thumbnails by URL, both sent through the
//! providers' executors.

use futures_util::future::BoxFuture;

use droppedneedle_providers::coverart::{ArtworkBytes, CaaClient, DownloadSize, EntityKind};
use droppedneedle_providers::images::ImageClient;
use droppedneedle_providers::{ProviderError, RequestPriority};

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
    /// Fetch `url` at `priority`. Only https URLs on the allowed image
    /// hosts are fetched.
    fn fetch<'a>(&'a self, url: &'a str, priority: RequestPriority) -> BoxFuture<'a, ImageFetch>;
}

impl RemoteImages for ImageClient {
    fn fetch<'a>(&'a self, url: &'a str, priority: RequestPriority) -> BoxFuture<'a, ImageFetch> {
        Box::pin(async move {
            let bytes = match ImageClient::fetch(self, url, MAX_REMOTE_BYTES, priority).await {
                Ok(bytes) => bytes,
                Err(error) if error.is_retriable() => {
                    tracing::debug!(%error, "image download failed");
                    return ImageFetch::Failed;
                }
                Err(error) => {
                    tracing::debug!(%error, "image unusable");
                    return ImageFetch::Unusable;
                }
            };
            match droppedneedle_providers::coverart::sniff_image_content_type(&bytes) {
                Some(content_type) => ImageFetch::Found(ArtworkBytes {
                    content_type: content_type.to_owned(),
                    bytes,
                }),
                None => ImageFetch::Unusable,
            }
        })
    }
}
