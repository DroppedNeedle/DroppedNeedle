//! Cover Art Archive provider client: front covers.
//!
//! Image bytes pass through untouched: originals may be PNG, so the client
//! never assumes JPEG.
//!
//! Every request goes through the archive's [`Executor`], which paces at
//! the `coverartarchive` row of the rate table and the caller's priority.
//! The archive answers covers with a redirect to archive.org; each hop
//! passes [`check_redirect_hop`] and is not paced, so one cover costs one
//! token.

use std::time::Duration;

use crate::RequestPriority;
use crate::error::{PayloadKind, ProviderError};
use crate::executor::{
    Backoff, Executor, RedirectRule, Redirects, Reply, Request, Spec, Url, standard_classify,
};
use crate::musicbrainz::is_valid_mbid;

/// Cover Art Archive root.
pub const COVER_ART_ARCHIVE_BASE: &str = "https://coverartarchive.org";
/// The provider name errors carry, and the archive's rate row.
pub const SOURCE: &str = "coverartarchive";
/// Largest body read from the archive: the biggest cap any caller applies
/// (artwork bytes). A larger body is never
/// buffered whole.
pub const TRANSPORT_MAX_BODY_BYTES: usize = 20 * 1024 * 1024;
/// Redirect hops followed for one request before giving up.
pub const MAX_REDIRECT_HOPS: usize = 5;

/// The archive on the wire: standard statuses, and at most 30 seconds for
/// a rate limit that names its wait.
///
/// The archive's API page documents no rate limit, only that some
/// endpoints answer 503 to a user over a limit (Cover_Art_Archive/API, read
/// 2026-10-18). A headerless 429 is the MetaBrainz gateway's answer (seen
/// 2026-10-20 from api.listenbrainz.org, lasting tens of minutes), and a
/// burst of cover fetches has met a five-minute refusal from every
/// MetaBrainz host, this one included. So a 429 that names no wait rests
/// the row at least a minute, then at least two, doubling while they keep
/// coming (jittered upward, by at most the same again), up to fifteen
/// minutes.
pub static SPEC: Spec = Spec {
    source: SOURCE,
    health_key: SOURCE,
    timeout: Duration::from_secs(30),
    max_body: TRANSPORT_MAX_BODY_BYTES,
    retry_after_default: Backoff::ExponentialAtLeast {
        base: Duration::from_secs(60),
        max: Duration::from_secs(15 * 60),
    },
    retry_after_cap: Duration::from_secs(30),
    classify: standard_classify,
};

/// Redirects stay on the archive hosts.
static HOPS: RedirectRule = RedirectRule {
    max_hops: MAX_REDIRECT_HOPS,
    next: |_, next| check_redirect_hop(next),
    foreign: is_storage_node,
};

/// Whether a hop goes to one of the Internet Archive's storage nodes (such
/// as `ia600508.us.archive.org` or `dn720905.ca.archive.org`, both seen on
/// 2026-10-22), which hold the image files. One failing node says nothing
/// about the archive; `archive.org` itself and the archive's own hosts are
/// the provider.
fn is_storage_node(url: &Url) -> bool {
    url.domain().is_some_and(|host| {
        let host = host.to_ascii_lowercase();
        host != "archive.org" && host.ends_with(".archive.org")
    })
}

/// Which entity the artwork was listed for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntityKind {
    /// Exact release: artwork belongs to this edition.
    Release,
    /// Release group: images come from a representative release and must
    /// be labelled release-group fallback, never exact-edition artwork.
    ReleaseGroup,
}

impl EntityKind {
    /// Path segment for the listing URL.
    pub fn path(&self) -> &'static str {
        match self {
            Self::Release => "release",
            Self::ReleaseGroup => "release-group",
        }
    }

    /// Source label distinguishing exact from fallback artwork.
    pub fn source(&self) -> &'static str {
        match self {
            Self::Release => "cover_art_archive_release",
            Self::ReleaseGroup => "cover_art_archive_release_group",
        }
    }
}

/// Download size: the original file or one thumbnail width.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DownloadSize {
    /// Original file as uploaded (any raster format).
    Full,
    /// 1200px thumbnail.
    Size1200,
    /// 500px thumbnail.
    Size500,
    /// 250px thumbnail.
    Size250,
}

impl DownloadSize {
    /// The size as the archive names its thumbnails.
    pub fn label(&self) -> &'static str {
        match self {
            Self::Full => "full",
            Self::Size1200 => "1200",
            Self::Size500 => "500",
            Self::Size250 => "250",
        }
    }
}

/// Downloaded artwork bytes with their effective content type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtworkBytes {
    /// Raw image bytes, untouched.
    pub bytes: Vec<u8>,
    /// Effective content type: the response header when it names a raster
    /// image, else the sniffed type, else the raw header.
    pub content_type: String,
}

/// Validate one artwork redirect target and return the https URL to
/// fetch next.
///
/// The location has already been joined onto the current URL as a parsed
/// URL, never matched as text, so fragments, queries, backslashes or
/// userinfo cannot smuggle another host past the check. It must be http or
/// https, carry no credentials, use a default port, and name the archive
/// (`coverartarchive.org`) or the Internet Archive (`archive.org` and its
/// subdomains) by domain; IP hosts are refused. The answer is the same URL
/// as https, without the fragment.
pub fn check_redirect_hop(next: &Url) -> Option<Url> {
    if !matches!(next.scheme(), "http" | "https") {
        return None;
    }
    if !next.username().is_empty() || next.password().is_some() {
        return None;
    }
    if !matches!(next.port(), None | Some(80) | Some(443)) {
        return None;
    }
    // `domain` is None for IP hosts.
    let host = next.domain()?.to_ascii_lowercase();
    let allowed =
        host == "coverartarchive.org" || host == "archive.org" || host.ends_with(".archive.org");
    if !allowed {
        return None;
    }
    let mut next = next.clone();
    next.set_fragment(None);
    next.set_scheme("https").ok()?;
    next.set_port(None).ok()?;
    Some(next)
}

/// Raster image MIME from magic bytes, or `None` for anything this client
/// will not serve mislabeled: embedded
/// declarations are often wrong, and a blob could be SVG or junk, so bytes
/// decide. Needs at least 12 bytes.
pub fn sniff_image_content_type(data: &[u8]) -> Option<&'static str> {
    if data.len() < 12 {
        return None;
    }
    if data.starts_with(b"\xff\xd8\xff") {
        return Some("image/jpeg");
    }
    if data.starts_with(b"\x89PNG\r\n\x1a\n") {
        return Some("image/png");
    }
    if data.starts_with(b"GIF87a") || data.starts_with(b"GIF89a") {
        return Some("image/gif");
    }
    if data.starts_with(b"RIFF") && data.get(8..12) == Some(b"WEBP".as_slice()) {
        return Some("image/webp");
    }
    None
}

/// Cover Art Archive client over the archive's executor. Built once with
/// the providers; every call names its priority.
#[derive(Debug, Clone)]
pub struct CaaClient {
    executor: Executor,
    base: Option<Url>,
}

impl CaaClient {
    /// The client sending through `executor` to the archive at `base_url`.
    #[must_use]
    pub fn new(executor: Executor, base_url: &str) -> Self {
        let base = Url::parse(base_url.trim())
            .ok()
            .filter(|url| matches!(url.scheme(), "http" | "https") && !url.cannot_be_a_base());
        if base.is_none() {
            tracing::error!("the Cover Art Archive address is not an http or https URL");
        }
        Self { executor, base }
    }

    /// The executor every archive request goes through.
    #[must_use]
    pub fn executor(&self) -> &Executor {
        &self.executor
    }

    /// Fetch the front cover of a release or release group at one size,
    /// the way v2 served covers: `/{entity}/{mbid}/front-{size}`, which the
    /// archive renders at 250, 500 and 1200 pixels, so nothing is resized
    /// here. `None` means the archive has no front cover (404 at the end of
    /// the redirects), which is authoritative.
    ///
    /// # Errors
    ///
    /// [`ProviderError::Invalid`] for a malformed MBID (nothing is sent),
    /// [`ProviderError::Redirect`] for a hop off the archive hosts, a
    /// payload error for an image past `maximum_bytes` ([`PayloadKind::TooLarge`])
    /// or bytes that are not a raster image ([`PayloadKind::Contract`]),
    /// otherwise the archive's typed failure.
    pub async fn fetch_front(
        &self,
        entity: EntityKind,
        mbid: &str,
        size: DownloadSize,
        maximum_bytes: usize,
        priority: RequestPriority,
    ) -> Result<Option<ArtworkBytes>, ProviderError> {
        let normalized = normalize(mbid)?;
        let front = match size {
            DownloadSize::Full => "front".to_owned(),
            other => format!("front-{}", other.label()),
        };
        let request = Request::get(self.base()?, &[entity.path(), &normalized, &front], &[])
            .map_err(|_| invalid("the Cover Art Archive address cannot carry a path"))?
            .max_body(maximum_bytes);
        let Some(reply) = self.send(request, priority).await? else {
            return Ok(None);
        };
        let content_type = sniff_image_content_type(reply.bytes())
            .ok_or_else(|| contract("the artwork is not a raster image"))?;
        Ok(Some(ArtworkBytes {
            bytes: reply.bytes().to_vec(),
            content_type: content_type.to_owned(),
        }))
    }

    /// One request through the executor, following the archive's
    /// redirects. A 404 is `None`.
    async fn send(
        &self,
        request: Request,
        priority: RequestPriority,
    ) -> Result<Option<Reply>, ProviderError> {
        match self
            .executor
            .send(request.redirects(Redirects::Follow(&HOPS)), priority)
            .await
        {
            Ok(reply) => Ok(Some(reply)),
            Err(ProviderError::NotFound { .. }) => Ok(None),
            Err(error) => Err(error),
        }
    }

    fn base(&self) -> Result<&Url, ProviderError> {
        self.base
            .as_ref()
            .ok_or_else(|| invalid("the Cover Art Archive address is not usable"))
    }
}

/// A lowercase MBID, or the refusal to send a malformed one.
fn normalize(mbid: &str) -> Result<String, ProviderError> {
    let normalized = mbid.trim().to_ascii_lowercase();
    if is_valid_mbid(&normalized) {
        Ok(normalized)
    } else {
        Err(invalid("malformed MBID"))
    }
}

fn invalid(reason: &'static str) -> ProviderError {
    ProviderError::Invalid {
        provider: SOURCE,
        reason,
    }
}

fn contract(message: &str) -> ProviderError {
    ProviderError::Payload {
        provider: SOURCE,
        kind: PayloadKind::Contract,
        message: message.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redirect_hops_stay_on_the_archive_hosts() {
        let current = Url::parse("https://coverartarchive.org/release/x/front-500").unwrap();
        let hop = |location: &str| {
            current
                .join(location)
                .ok()
                .and_then(|next| check_redirect_hop(&next))
                .map(String::from)
        };
        assert_eq!(
            hop("http://archive.org/download/a/b.jpg#frag").as_deref(),
            Some("https://archive.org/download/a/b.jpg")
        );
        assert_eq!(
            hop("https://ia800.us.archive.org:443/x.png?y=1").as_deref(),
            Some("https://ia800.us.archive.org/x.png?y=1")
        );
        assert_eq!(
            hop("/release/x/1-500.jpg").as_deref(),
            Some("https://coverartarchive.org/release/x/1-500.jpg")
        );
        for bad in [
            "https://evil.com#.archive.org/x",
            "https://evil.com?.archive.org/x",
            "https://evil.com\\.archive.org/x",
            "https://archive.org@evil.com/x",
            "https://user:pw@archive.org/x",
            "https://archive.org.evil.com/x",
            "https://archive.org:8443/x",
            "ftp://archive.org/x",
            "https://127.0.0.1/x",
            "https://[::1]/x",
            "//evil.com/x",
        ] {
            assert!(hop(bad).is_none(), "{bad} must be refused");
        }
    }
}
