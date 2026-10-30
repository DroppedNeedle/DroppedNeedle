//! Artist images, TheAudioDB thumbnails and Spotify playlist covers,
//! fetched by the URL a provider's own data names.
//!
//! The URLs come from upstream answers, so only https addresses on the
//! hosts TheAudioDB, Wikimedia and Deezer serve images from are fetched
//! ([`check_image_url`]), and every redirect hop is checked the same way.
//! TheAudioDB's image host paces at the `audiodb-media` row, the Wikimedia
//! hosts at the `wikimedia-media` row, with its documented concurrency, and
//! Deezer's image host at the `deezer` row its API shares. Spotify playlist
//! covers come only from Spotify's CDN ([`check_spotify_cover_url`]), with
//! no redirect followed, at the `spotify` row its API shares.

use std::time::Duration;

use crate::RequestPriority;
use crate::client::parse_base;
use crate::error::{MAX_RETRY_AFTER, PayloadKind, ProviderError};
use crate::executor::{
    Backoff, Executor, RedirectRule, Redirects, Request, Spec, Url, standard_classify,
};

/// The rate row and provider name of TheAudioDB's image host.
pub const AUDIODB_MEDIA: &str = "audiodb-media";
/// The rate row and provider name of the Wikimedia image hosts.
pub const WIKIMEDIA_MEDIA: &str = "wikimedia-media";
/// The origin Deezer serves artist pictures from. Checked 2026-10-20: every
/// picture URL of `api.deezer.com/artist/415` was on this host, and the
/// 1000 px one answered 200 image/jpeg with no redirect. Exact origin only.
pub const DEEZER_CDN: &str = "https://cdn-images.dzcdn.net";
/// The domain Spotify serves playlist covers from: any host under it, over
/// https.
const SPOTIFY_CDN_SUFFIX: &str = ".scdn.co";
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
/// Spotify's CDN on the wire, paced at the `spotify` row.
pub static SPOTIFY_SPEC: Spec = spec(crate::spotify::SOURCE);

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

/// A redirect is followed only to a TheAudioDB or Wikimedia address
/// [`check_image_url`] allows. Deezer's pictures answer directly, so no hop
/// goes to its origin.
static HOPS: RedirectRule = RedirectRule {
    max_hops: MAX_IMAGE_REDIRECTS,
    next: |_, next| check_image_url(next.as_str(), None),
    foreign: crate::executor::never_foreign,
};

/// Validate a third-party image URL: no credentials, and either an https
/// address on the default port on a host TheAudioDB or Wikimedia serves
/// images from, or one on the `deezer` origin. The URLs come from upstream
/// data, so this keeps the server from being pointed anywhere else.
#[must_use]
pub fn check_image_url(raw: &str, deezer: Option<&Url>) -> Option<Url> {
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
    if !url.username().is_empty() || url.password().is_some() {
        return None;
    }
    if deezer.is_some_and(|deezer| deezer.origin() == url.origin()) {
        return Some(url);
    }
    if url.scheme() != "https" || url.port().is_some_and(|port| port != 443) {
        return None;
    }
    let host = url.domain()?.to_ascii_lowercase();
    let allowed = is_audiodb(&host)
        || host == "upload.wikimedia.org"
        || host == "commons.wikimedia.org"
        // Scaled Commons portraits come from this host. Checked 2026-10-19:
        // the 1280 px thumburl of File:RadioheadO2211125 composite.jpg
        // answered 200 image/jpeg with no redirect. Exact host only.
        || host == "thumb.wikimedia.org";
    allowed.then_some(url)
}

/// A Spotify cover URL, parsed, when it may be fetched: https without
/// credentials to a host under Spotify's CDN domain, or any URL on the
/// `extra` origin (the tests' mock CDN; production has none).
#[must_use]
pub fn check_spotify_cover_url(raw: &str, extra: Option<&Url>) -> Option<Url> {
    let url = Url::parse(raw.trim()).ok()?;
    if extra.is_some_and(|extra| extra.origin() == url.origin()) {
        return Some(url);
    }
    let spotify_cdn = url.scheme() == "https"
        && url.username().is_empty()
        && url.password().is_none()
        && url
            .host_str()
            .is_some_and(|host| host.to_ascii_lowercase().ends_with(SPOTIFY_CDN_SUFFIX));
    spotify_cdn.then_some(url)
}

fn is_audiodb(host: &str) -> bool {
    host == "theaudiodb.com" || host.ends_with(".theaudiodb.com")
}

/// The image origins set at boot.
#[derive(Debug, Clone, Default)]
pub struct ImageHosts {
    /// The origin Deezer serves pictures from.
    pub deezer: Option<Url>,
    /// A Spotify cover origin allowed beside Spotify's CDN; tests only.
    pub spotify_extra: Option<Url>,
}

impl ImageHosts {
    /// Deezer's pictures at `deezer` and, for tests, Spotify covers also
    /// at `spotify_extra`.
    #[must_use]
    pub fn new(deezer: &str, spotify_extra: Option<&str>) -> Self {
        Self {
            deezer: parse_base(deezer, crate::preview::DEEZER),
            spotify_extra: spotify_extra
                .and_then(|extra| parse_base(extra, crate::spotify::SOURCE)),
        }
    }
}

/// The executors the image client sends through, one per image host.
#[derive(Debug, Clone)]
pub struct ImageExecutors {
    /// TheAudioDB's image host.
    pub audiodb: Executor,
    /// The Wikimedia image hosts.
    pub wikimedia: Executor,
    /// Deezer's image host.
    pub deezer: Executor,
    /// Spotify's CDN.
    pub spotify: Executor,
}

/// The image client over its executors. Built once with the providers;
/// cheap to clone.
#[derive(Debug, Clone)]
pub struct ImageClient {
    executors: ImageExecutors,
    hosts: ImageHosts,
}

impl ImageClient {
    /// The client sending each host's images through its executor.
    #[must_use]
    pub fn new(executors: ImageExecutors, hosts: ImageHosts) -> Self {
        Self { executors, hosts }
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
        let deezer = self.hosts.deezer.as_ref();
        let Some(url) = check_image_url(raw, deezer) else {
            return Err(ProviderError::Invalid {
                provider: WIKIMEDIA_MEDIA,
                reason: "the image address is outside the allowed hosts",
            });
        };
        let executor = if deezer.is_some_and(|deezer| deezer.origin() == url.origin()) {
            &self.executors.deezer
        } else if url
            .domain()
            .is_some_and(|host| is_audiodb(&host.to_ascii_lowercase()))
        {
            &self.executors.audiodb
        } else {
            &self.executors.wikimedia
        };
        let request = Request::get_url(url)
            .max_body(max_body)
            .redirects(Redirects::Follow(&HOPS));
        let reply = executor.send(request, priority).await?;
        Ok(reply.bytes().to_vec())
    }

    /// A Spotify playlist cover and its content type, read up to
    /// `max_body`. A CDN URL must answer directly: no redirect is followed.
    /// An address outside the CDN (nothing is sent), a refusal, a redirect,
    /// an answer that is not an image and one over the cap are `None`.
    ///
    /// # Errors
    ///
    /// The typed failure of a call that got no answer to judge: the
    /// network, a timeout, a rate limit or a server error.
    pub async fn spotify_cover(
        &self,
        raw: &str,
        max_body: usize,
        priority: RequestPriority,
    ) -> Result<Option<(Vec<u8>, String)>, ProviderError> {
        let Some(url) = check_spotify_cover_url(raw, self.hosts.spotify_extra.as_ref()) else {
            return Ok(None);
        };
        let request = Request::get_url(url)
            .max_body(max_body)
            .redirects(Redirects::Refuse);
        let reply = match self.executors.spotify.send(request, priority).await {
            Ok(reply) => reply,
            Err(
                ProviderError::Redirect { .. }
                | ProviderError::Unauthorized { .. }
                | ProviderError::Forbidden { .. }
                | ProviderError::NotFound { .. }
                | ProviderError::BadRequest { .. }
                | ProviderError::Payload {
                    kind: PayloadKind::TooLarge,
                    ..
                },
            ) => return Ok(None),
            Err(error) => return Err(error),
        };
        let content_type = reply
            .content_type()
            .unwrap_or("")
            .split(';')
            .next()
            .unwrap_or("")
            .trim()
            .to_ascii_lowercase();
        if reply.status() != 200 || !content_type.starts_with("image/") {
            return Ok(None);
        }
        Ok(Some((reply.bytes().to_vec(), content_type)))
    }
}

#[cfg(test)]
mod tests {
    use super::{DEEZER_CDN, check_image_url};
    use crate::executor::Url;

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
                "https://user@cdn-images.dzcdn.net/images/artist/x/1000x1000-000000-80-0-0.jpg",
                false,
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
        let deezer = Url::parse(DEEZER_CDN).unwrap();
        for (raw, allowed) in cases {
            assert_eq!(
                check_image_url(raw, Some(&deezer)).is_some(),
                allowed,
                "{raw}"
            );
        }
    }
}
