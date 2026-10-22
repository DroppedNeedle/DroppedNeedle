//! Artist images and TheAudioDB thumbnails, fetched by the URL a
//! provider's own data names.
//!
//! The URLs come from upstream answers, so only https addresses on the
//! hosts TheAudioDB, Wikimedia and Deezer serve images from are fetched
//! ([`check_image_url`]), and every redirect hop is checked the same way.
//! TheAudioDB's image host paces at the `audiodb-media` row, the Wikimedia
//! hosts at the `wikimedia-media` row, with its documented concurrency, and
//! Deezer's image host at the `deezer` row its API shares.

use std::time::Duration;

use crate::RequestPriority;
use crate::error::{MAX_RETRY_AFTER, ProviderError};
use crate::executor::{
    Backoff, Executor, RedirectRule, Redirects, Request, Spec, Url, standard_classify,
};

/// The rate row and provider name of TheAudioDB's image host.
pub const AUDIODB_MEDIA: &str = "audiodb-media";
/// The rate row and provider name of the Wikimedia image hosts.
pub const WIKIMEDIA_MEDIA: &str = "wikimedia-media";
/// The host Deezer serves artist pictures from. Checked 2026-10-20: every
/// picture URL of `api.deezer.com/artist/415` was on this host, and the
/// 1000 px one answered 200 image/jpeg with no redirect. Exact host only.
pub const DEEZER_CDN_HOST: &str = "cdn-images.dzcdn.net";
/// Redirect hops followed for one image, each checked like the first URL.
pub const MAX_IMAGE_REDIRECTS: usize = 3;
/// Largest image read unless the caller asks for less.
const MAX_IMAGE_BYTES: usize = 20 * 1024 * 1024;

/// TheAudioDB's image host on the wire.
pub static AUDIODB_SPEC: Spec = spec(AUDIODB_MEDIA);
/// The Wikimedia image hosts on the wire.
pub static WIKIMEDIA_SPEC: Spec = spec(WIKIMEDIA_MEDIA);
/// Deezer's image host on the wire, paced at the `deezer` row.
pub static DEEZER_SPEC: Spec = spec(crate::preview::DEEZER);

const fn spec(source: &'static str) -> Spec {
    Spec {
        source,
        health_key: source,
        timeout: Duration::from_secs(15),
        max_body: MAX_IMAGE_BYTES,
        retry_after_default: Backoff::Fixed(Duration::from_secs(1)),
        retry_after_cap: MAX_RETRY_AFTER,
        classify: standard_classify,
    }
}

/// A redirect is followed only to an address [`check_image_url`] allows.
static HOPS: RedirectRule = RedirectRule {
    max_hops: MAX_IMAGE_REDIRECTS,
    next: |_, next| check_image_url(next.as_str()),
    foreign: crate::executor::never_foreign,
};

/// Validate a third-party image URL: https on the default port, no credentials,
/// and a host TheAudioDB, Wikimedia or Deezer serves images from. The URLs come
/// from upstream data, so this keeps the server from being pointed anywhere
/// else.
#[must_use]
pub fn check_image_url(raw: &str) -> Option<Url> {
    let mut url = Url::parse(raw.trim()).ok()?;
    // TheAudioDB still names some pictures over http. Its host serves the
    // same path over https (the http address answers 301 to it; checked
    // 2026-10-22 on r2.theaudiodb.com), so those are fetched over https.
    if url.scheme() == "http"
        && url.port().is_none()
        && url
            .domain()
            .is_some_and(|host| is_audiodb(&host.to_ascii_lowercase()))
    {
        url.set_scheme("https").ok()?;
    }
    if url.scheme() != "https"
        || !url.username().is_empty()
        || url.password().is_some()
        || url.port().is_some_and(|port| port != 443)
    {
        return None;
    }
    let host = url.domain()?.to_ascii_lowercase();
    let allowed = is_audiodb(&host)
        || host == "upload.wikimedia.org"
        || host == "commons.wikimedia.org"
        // Scaled Commons portraits come from this host. Checked 2026-10-19:
        // the 1280 px thumburl of File:RadioheadO2211125 composite.jpg
        // answered 200 image/jpeg with no redirect. Exact host only.
        || host == "thumb.wikimedia.org"
        || host == DEEZER_CDN_HOST;
    allowed.then_some(url)
}

fn is_audiodb(host: &str) -> bool {
    host == "theaudiodb.com" || host.ends_with(".theaudiodb.com")
}

/// The image client over its three executors. Built once with the
/// providers; cheap to clone.
#[derive(Debug, Clone)]
pub struct ImageClient {
    audiodb: Executor,
    wikimedia: Executor,
    deezer: Executor,
}

impl ImageClient {
    /// The client sending TheAudioDB's images through `audiodb`,
    /// Wikimedia's through `wikimedia` and Deezer's through `deezer`.
    #[must_use]
    pub fn new(audiodb: Executor, wikimedia: Executor, deezer: Executor) -> Self {
        Self {
            audiodb,
            wikimedia,
            deezer,
        }
    }

    /// The bytes at `raw`, read up to `max_body`, at `priority`.
    ///
    /// # Errors
    ///
    /// [`ProviderError::Invalid`] for an address outside the allowed hosts
    /// (nothing is sent), [`ProviderError::Redirect`] for a hop to one,
    /// otherwise the typed failure of the call.
    pub async fn fetch(
        &self,
        raw: &str,
        max_body: usize,
        priority: RequestPriority,
    ) -> Result<Vec<u8>, ProviderError> {
        let Some(url) = check_image_url(raw) else {
            return Err(ProviderError::Invalid {
                provider: WIKIMEDIA_MEDIA,
                reason: "the image address is outside the allowed hosts",
            });
        };
        let host = url.domain().map(str::to_ascii_lowercase);
        let executor = match host.as_deref() {
            Some(host) if is_audiodb(host) => &self.audiodb,
            Some(DEEZER_CDN_HOST) => &self.deezer,
            _ => &self.wikimedia,
        };
        let request = Request::get_url(url)
            .max_body(max_body)
            .redirects(Redirects::Follow(&HOPS));
        let reply = executor.send(request, priority).await?;
        Ok(reply.bytes().to_vec())
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
            (
                "https://cdn-images.dzcdn.net/images/artist/3ec5542ff520ee74e2befdaba32ef2ef/1000x1000-000000-80-0-0.jpg",
                true,
            ),
            (
                "https://CDN-IMAGES.DZCDN.NET/images/artist/3ec5542ff520ee74e2befdaba32ef2ef/1000x1000-000000-80-0-0.jpg",
                true,
            ),
            (
                "https://cdn-images.dzcdn.net.example.com/images/artist/x/1000x1000-000000-80-0-0.jpg",
                false,
            ),
            (
                "https://evil-cdn-images.dzcdn.net/images/artist/x/1000x1000-000000-80-0-0.jpg",
                false,
            ),
            (
                "https://e-cdns-images.dzcdn.net/images/artist/x/1000x1000-000000-80-0-0.jpg",
                false,
            ),
            (
                "https://dzcdn.net/images/artist/x/1000x1000-000000-80-0-0.jpg",
                false,
            ),
            (
                "http://cdn-images.dzcdn.net/images/artist/x/1000x1000-000000-80-0-0.jpg",
                false,
            ),
        ];
        for (raw, allowed) in cases {
            assert_eq!(check_image_url(raw).is_some(), allowed, "{raw}");
        }
    }
}
