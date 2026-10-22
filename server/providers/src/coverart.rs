//! Cover Art Archive provider client: metadata listing, downloads and
//! front covers.
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

mod models;

use models::{CaaImage, CaaResponse};

/// Cover Art Archive root.
pub const COVER_ART_ARCHIVE_BASE: &str = "https://coverartarchive.org";
/// The provider name errors carry, and the archive's rate row.
pub const SOURCE: &str = "coverartarchive";
/// Metadata documents larger than this are refused (5 MiB).
pub const METADATA_MAX_BYTES: usize = 5 * 1024 * 1024;
/// Largest body read from the archive: the biggest cap any caller applies
/// (artwork bytes). A larger body is never
/// buffered whole.
pub const TRANSPORT_MAX_BODY_BYTES: usize = 20 * 1024 * 1024;
/// Redirect hops followed for one request before giving up.
pub const MAX_REDIRECT_HOPS: usize = 5;

/// The archive on the wire: standard statuses, a second's rest after a
/// rate limit that names no wait, and at most 30 seconds for one that
/// does.
pub static SPEC: Spec = Spec {
    source: SOURCE,
    health_key: SOURCE,
    timeout: Duration::from_secs(30),
    max_body: TRANSPORT_MAX_BODY_BYTES,
    retry_after_default: Backoff::Fixed(Duration::from_secs(1)),
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
    /// Label used in candidate ids.
    pub fn label(&self) -> &'static str {
        match self {
            Self::Full => "full",
            Self::Size1200 => "1200",
            Self::Size500 => "500",
            Self::Size250 => "250",
        }
    }

    /// Pick the listing URL for this size, falling back to the original
    /// when the archive has no such thumbnail.
    pub fn select<'image>(&self, image: &'image CaaImage) -> &'image str {
        match self {
            Self::Full => image.image.as_str(),
            Self::Size1200 => image
                .thumbnails
                .size_1200
                .as_deref()
                .unwrap_or(image.image.as_str()),
            Self::Size500 => image
                .thumbnails
                .size_500
                .as_deref()
                .unwrap_or(image.image.as_str()),
            Self::Size250 => image
                .thumbnails
                .size_250
                .as_deref()
                .unwrap_or(image.image.as_str()),
        }
    }
}

/// Normalized artwork image type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageType {
    /// Front cover.
    Front,
    /// Back cover.
    Back,
    /// Booklet page.
    Booklet,
    /// Medium photo.
    Medium,
    /// Tray photo.
    Tray,
    /// Obi strip.
    Obi,
    /// Spine.
    Spine,
    /// Track image.
    Track,
    /// Anything else, including an empty type list.
    Other,
}

/// One typed artwork candidate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtworkCandidate {
    /// Stable id: `caa:{entity}:{mbid}:{image_id}:{size}`.
    pub candidate_id: String,
    /// Exact or fallback source label.
    pub source: String,
    /// Validated https artwork URL.
    pub locator: String,
    /// Normalized image types, front flag first.
    pub image_types: Vec<ImageType>,
    /// Editor approval flag.
    pub approved: bool,
    /// Whether this is the front (primary) image.
    pub primary: bool,
    /// Editor comment.
    pub description: String,
    /// Entity the listing was read for.
    pub source_entity_mbid: String,
    /// True only for exact-release listings.
    pub source_is_exact_release: bool,
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

/// Validate an archive artwork URL and upgrade it to https:
/// only `coverartarchive.org` over http/https,
/// no credentials, default ports only, absolute path. Query strings
/// survive; fragments do not. Anything else is `None`: a listing may name
/// artwork over http, so only validated archive URLs may be upgraded.
pub fn upgrade_artwork_url(url: &str) -> Option<String> {
    let (scheme, authority, path_and_query) = split_artwork_url(url)?;
    if scheme != "http" && scheme != "https" {
        return None;
    }
    if authority.contains('@') {
        return None;
    }
    let (host, port) = match authority.rsplit_once(':') {
        Some((host, port_text)) => {
            let port: u16 = port_text.parse().ok()?;
            (host, Some(port))
        }
        None => (authority, None),
    };
    if !host.eq_ignore_ascii_case("coverartarchive.org")
        || !matches!(port, None | Some(80) | Some(443))
    {
        return None;
    }
    if !path_and_query.starts_with('/') {
        return None;
    }
    let path = path_and_query.split('#').next().unwrap_or("/");
    Some(format!("https://coverartarchive.org{path}"))
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

/// Split a URL into (scheme, authority, path+query+fragment). A bare
/// origin has no path, and artwork locations always carry one, so the
/// empty path stays empty for the caller to reject (v2 requires
/// `parsed.path.startswith("/")`, which `""` fails).
fn split_artwork_url(url: &str) -> Option<(&str, &str, &str)> {
    let (scheme, rest) = url.split_once("://")?;
    let end = rest.find('/').unwrap_or(rest.len());
    let (authority, path) = rest.split_at(end);
    if authority.is_empty() {
        return None;
    }
    Some((scheme, authority, path))
}

/// Normalize image types from the archive's labels plus its front/back
/// flags: casefolded, `front` prepended and
/// `back` appended from the flags, known labels kept, anything else (or an
/// empty list) becoming `Other`, order-preserved and deduplicated.
pub fn classify_image_types(types: &[String], front: bool, back: bool) -> Vec<ImageType> {
    let mut raw: Vec<String> = types
        .iter()
        .map(|entry| entry.trim().to_ascii_lowercase())
        .filter(|entry| !entry.is_empty())
        .collect();
    if front {
        raw.insert(0, "front".to_owned());
    }
    if back {
        raw.push("back".to_owned());
    }
    if raw.is_empty() {
        raw.push("other".to_owned());
    }
    let mut normalized = Vec::new();
    for entry in raw {
        let image_type = match entry.as_str() {
            "front" => ImageType::Front,
            "back" => ImageType::Back,
            "booklet" => ImageType::Booklet,
            "medium" => ImageType::Medium,
            "tray" => ImageType::Tray,
            "obi" => ImageType::Obi,
            "spine" => ImageType::Spine,
            "track" => ImageType::Track,
            _ => ImageType::Other,
        };
        if !normalized.contains(&image_type) {
            normalized.push(image_type);
        }
    }
    normalized
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

/// True for content types this client serves as images.
fn is_image_content_type(content_type: &str) -> bool {
    matches!(
        content_type
            .split(';')
            .next()
            .unwrap_or("")
            .trim()
            .to_ascii_lowercase()
            .as_str(),
        "image/jpeg" | "image/jpg" | "image/png" | "image/gif" | "image/webp"
    )
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

    /// List typed artwork images for an exact release or a labelled group
    /// fallback. A missing entity (404) is authoritative empty artwork;
    /// any other failure is a provider error, never silent emptiness.
    ///
    /// # Errors
    ///
    /// [`ProviderError::Invalid`] for a malformed MBID (nothing is sent),
    /// otherwise the archive's typed failure.
    pub async fn list_artwork(
        &self,
        entity: EntityKind,
        mbid: &str,
        size: DownloadSize,
        priority: RequestPriority,
    ) -> Result<Vec<ArtworkCandidate>, ProviderError> {
        let normalized = normalize(mbid)?;
        let request = Request::get(self.base()?, &[entity.path(), &normalized], &[])
            .map_err(|_| invalid("the Cover Art Archive address cannot carry a path"))?
            .max_body(METADATA_MAX_BYTES);
        let Some(reply) = self.send(request, priority).await? else {
            return Ok(Vec::new());
        };
        let listing: CaaResponse = reply.json()?;
        let mut candidates = Vec::new();
        for image in &listing.images {
            let selected = size.select(image);
            if selected.is_empty() {
                tracing::warn!(
                    image_id = image.id,
                    "skipping a Cover Art Archive image with no file URL for this size"
                );
                continue;
            }
            let locator = upgrade_artwork_url(selected)
                .ok_or_else(|| contract("the listing named an artwork URL off the archive"))?;
            candidates.push(ArtworkCandidate {
                candidate_id: format!(
                    "caa:{}:{normalized}:{}:{}",
                    entity.path(),
                    image.id,
                    size.label()
                ),
                source: entity.source().to_owned(),
                locator,
                image_types: classify_image_types(&image.types, image.front, image.back),
                approved: image.approved,
                primary: image.front,
                description: image.comment.clone(),
                source_entity_mbid: normalized.clone(),
                source_is_exact_release: entity == EntityKind::Release,
            });
        }
        Ok(candidates)
    }

    /// Download one candidate's bytes. The client only downloads its own
    /// CAA candidates, revalidates the locator, and caps the body.
    ///
    /// # Errors
    ///
    /// [`ProviderError::Invalid`] for a candidate this client will not
    /// fetch (nothing is sent), [`ProviderError::NotFound`] when the image
    /// is gone, a [`PayloadKind::TooLarge`] payload error past
    /// `maximum_bytes`, otherwise the archive's typed failure.
    pub async fn download_artwork(
        &self,
        candidate: &ArtworkCandidate,
        maximum_bytes: usize,
        priority: RequestPriority,
    ) -> Result<ArtworkBytes, ProviderError> {
        if candidate.source != EntityKind::Release.source()
            && candidate.source != EntityKind::ReleaseGroup.source()
        {
            return Err(invalid("the cover client only downloads CAA candidates"));
        }
        if maximum_bytes == 0 {
            return Err(invalid("artwork byte limit must be positive"));
        }
        let url = upgrade_artwork_url(&candidate.locator)
            .and_then(|url| Url::parse(&url).ok())
            .ok_or_else(|| invalid("the artwork URL is off the archive"))?;
        let query: Vec<(String, String)> = url.query_pairs().into_owned().collect();
        let query: Vec<(&str, &str)> = query
            .iter()
            .map(|(key, value)| (key.as_str(), value.as_str()))
            .collect();
        let request = Request::get(&url, &[], &query)
            .map_err(|_| invalid("the artwork URL cannot carry a path"))?
            .max_body(maximum_bytes);
        let reply = self
            .send(request, priority)
            .await?
            .ok_or(ProviderError::NotFound { provider: SOURCE })?;
        let declared = reply.content_type().unwrap_or("").to_owned();
        let content_type = if is_image_content_type(&declared) {
            declared
        } else if let Some(sniffed) = sniff_image_content_type(reply.bytes()) {
            sniffed.to_owned()
        } else {
            declared
        };
        Ok(ArtworkBytes {
            bytes: reply.bytes().to_vec(),
            content_type,
        })
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
