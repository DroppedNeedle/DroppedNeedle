//! Cover Art Archive provider client: metadata listing and downloads.
//!
//! This module ports the v2 CAA stack (`coverart_repository.py` management
//! path plus `coverart_album.py` front-fetch shapes). The live provider
//! boundary was probed on 2026-07-21 against coverartarchive.org and is
//! recorded in `coverart_MANAGEMENT_API_NOTES.md`; every handler below
//! cites it. Reads stay conservative (~1/s with backoff on 429/503) per
//! the verified v3 policy-table row, and image bytes pass through
//! untouched: originals may be PNG, so the client never assumes JPEG.
//!
//! Seam notes (the shared infrastructure lives in the provider core):
//! - [`CaaTransport`] stays a typed local port: its requests carry
//!   redirect-hop validation the catalog GET port cannot express, and the
//!   reqwest adapter below serves production from the shared client.
//! - [`RateGate`] and [`is_valid_mbid`] are small local copies of the
//!   MusicBrainz module's helpers, left duplicated on purpose: they are
//!   structs and functions, not traits, and unifying
//!   them means reconciling real differences (per-source poison log
//!   lines). Retry-After parsing already delegates to the core
//!   [`parse_retry_after`](super::error::parse_retry_after), whose
//!   [`Duration`](std::time::Duration) answer converts to the f64 seconds
//!   this client carries.

use std::sync::Mutex;
use std::time::Duration;

use serde::Deserialize;
use thiserror::Error;

/// Cover Art Archive root.
pub const COVER_ART_ARCHIVE_BASE: &str = "https://coverartarchive.org";
/// Conservative metadata/download pacing per the verified v3 policy row
/// ("no documented allocation; stay conservative (~1/s), back off on
/// 429/503").
pub const CAA_RATE_PER_SEC: f64 = 1.0;
/// Metadata documents larger than this are refused (v2
/// `MANAGEMENT_ARTWORK_METADATA_MAX_BYTES`, 5 MiB).
pub const METADATA_MAX_BYTES: usize = 5 * 1024 * 1024;
/// Default ceiling for one backoff sleep before the single retry.
pub const DEFAULT_MAX_BACKOFF: Duration = Duration::from_secs(30);

/// One outbound CAA GET, transport-agnostic so fakes stay script-only.
#[derive(Debug, Clone)]
pub struct CaaRequest {
    /// Full URL to fetch.
    pub url: String,
    /// Headers in send order; the client always sets User-Agent.
    pub headers: Vec<(String, String)>,
}

impl CaaRequest {
    /// Fetch one header value, case-insensitively.
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }
}

/// Minimal raw response: status, content type, and bytes.
#[derive(Debug, Clone)]
pub struct RawResponse {
    /// HTTP status code.
    pub status: u16,
    /// Response headers in arrival order.
    pub headers: Vec<(String, String)>,
    /// Raw body bytes.
    pub body: Vec<u8>,
}

impl RawResponse {
    /// Build a response from the parts fakes script.
    pub fn new(status: u16, headers: Vec<(&str, &str)>, body: impl Into<Vec<u8>>) -> Self {
        Self {
            status,
            headers: headers
                .into_iter()
                .map(|(key, value)| (key.to_owned(), value.to_owned()))
                .collect(),
            body: body.into(),
        }
    }

    /// Fetch one header value, case-insensitively.
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }
}

/// Wire failure below HTTP semantics (DNS, connect, TLS, timeout, reset).
#[derive(Debug, Clone, Error)]
#[error("coverartarchive transport failure: {0}")]
pub struct TransportError(pub String);

/// Minimal local transport port (see the module seam notes).
pub trait CaaTransport: Send + Sync {
    /// Perform one GET, following no redirects.
    fn get(
        &self,
        request: &CaaRequest,
    ) -> impl Future<Output = Result<RawResponse, TransportError>> + Send;
}

/// Production adapter over the factory's no-redirect client, which carries
/// the shared timeouts and User-Agent.
pub struct ReqwestCaaTransport {
    client: reqwest::Client,
}

impl ReqwestCaaTransport {
    /// Wrap `HttpClientFactory::no_redirect`: the client walks and checks
    /// each redirect hop itself.
    pub fn new(no_redirect: reqwest::Client) -> Self {
        Self {
            client: no_redirect,
        }
    }
}

impl CaaTransport for ReqwestCaaTransport {
    async fn get(&self, request: &CaaRequest) -> Result<RawResponse, TransportError> {
        let mut outgoing = self.client.get(request.url.clone());
        for (key, value) in &request.headers {
            outgoing = outgoing.header(key.as_str(), value.as_str());
        }
        let response = outgoing
            .send()
            .await
            .map_err(|error| TransportError(error.to_string()))?;
        let status = response.status().as_u16();
        let mut headers = Vec::new();
        for name in ["location", "retry-after", "content-type"] {
            if let Some(value) = response.headers().get(name)
                && let Ok(text) = value.to_str()
            {
                headers.push((name.to_owned(), text.to_owned()));
            }
        }
        let body = response
            .bytes()
            .await
            .map_err(|error| TransportError(error.to_string()))?
            .to_vec();
        Ok(RawResponse {
            status,
            headers,
            body,
        })
    }
}

/// Interval gate for the conservative ~1/s pacing. Local copy; unify with
/// the shared limiters when core lands. The mutex never crosses an await.
pub struct RateGate {
    interval: Duration,
    next_allowed: Mutex<Option<tokio::time::Instant>>,
}

impl RateGate {
    /// Gate allowing `rate_per_sec` acquisitions per second, one at a time.
    pub fn new(rate_per_sec: f64) -> Self {
        let interval = Duration::from_secs_f64(1.0 / rate_per_sec.max(f64::MIN_POSITIVE));
        Self {
            interval,
            next_allowed: Mutex::new(None),
        }
    }

    /// The conservative CAA limiter.
    pub fn coverart() -> Self {
        Self::new(CAA_RATE_PER_SEC)
    }

    /// Configured rate, for the policy-table test.
    pub fn rate_per_sec(&self) -> f64 {
        1.0 / self.interval.as_secs_f64()
    }

    /// Wait until one acquisition is due, then take it.
    pub async fn acquire(&self) {
        loop {
            let wait = {
                let mut guard = self.next_allowed.lock().unwrap_or_else(|poison| {
                    tracing::warn!("coverart rate-gate lock poisoned; resetting");
                    poison.into_inner()
                });
                let now = tokio::time::Instant::now();
                match *guard {
                    None => {
                        *guard = Some(now + self.interval);
                        None
                    }
                    Some(due) if now >= due => {
                        *guard = Some(now + self.interval);
                        None
                    }
                    Some(due) => Some(due.saturating_duration_since(now)),
                }
            };
            match wait {
                None => return,
                Some(delay) => tokio::time::sleep(delay).await,
            }
        }
    }
}

/// Typed Cover Art Archive failures.
#[derive(Debug, Error, PartialEq)]
pub enum CaaError {
    /// Transport dead or 5xx (other than the backed-off 503): retriable.
    #[error("coverartarchive unavailable: {0}")]
    Unavailable(String),
    /// 429/503 after the single backoff retry: the caller waits out
    /// `retry_after_secs` when the response gave one.
    #[error("coverartarchive rate limited")]
    RateLimited {
        /// Seconds to wait before retrying, when the response said.
        retry_after_secs: Option<f64>,
    },
    /// Caller passed a malformed MBID; nothing was sent.
    #[error("coverartarchive refused the request as invalid: {0}")]
    InvalidMbid(String),
    /// Other 4xx: the request is wrong, retrying will not help.
    #[error("coverartarchive rejected the request (HTTP {0})")]
    Rejected(u16),
    /// 200 with an unparseable or over-limit body.
    #[error("coverartarchive contract break: {0}")]
    Contract(String),
    /// The archive returned an artwork URL this client will not fetch
    /// (wrong host, scheme, credentials, or port).
    #[error("coverartarchive returned an invalid artwork location")]
    RejectedUrl,
}

/// Parse Retry-After in either legal shape: delay seconds or an HTTP date.
/// Delegates to the core [`parse_retry_after`](super::error::parse_retry_after),
/// which covers all three HTTP date shapes where the old local parser read
/// IMF-fixdate only. Unparseable, non-finite, and negative values yield
/// `None`; valid values clamp to 60 seconds.
pub fn parse_retry_after_secs(value: Option<&str>) -> Option<f64> {
    super::error::parse_retry_after(value).map(|delay| delay.as_secs_f64())
}

/// True for the `8-4-4-4-12` hex MBID shape. Local copy of the MusicBrainz helper; see the module seam notes.
pub fn is_valid_mbid(value: &str) -> bool {
    let trimmed = value.trim();
    if trimmed.len() != 36 {
        return false;
    }
    for (index, byte) in trimmed.bytes().enumerate() {
        let is_dash_slot = matches!(index, 8 | 13 | 18 | 23);
        if is_dash_slot != (byte == b'-') {
            return false;
        }
        if !is_dash_slot && !byte.is_ascii_hexdigit() {
            return false;
        }
    }
    true
}

// ---------------------------------------------------------------------------
// Wire models: the `{images: [...]}` document. Each image carries approved,
// front, back, comment, numeric id, the original image URL, a thumbnails
// object (observed `250`, `500`, `1200`), and a types array (live
// 2026-07-21, `coverart_MANAGEMENT_API_NOTES.md`). Unknown fields are
// ignored; the numeric id is required identity.
// ---------------------------------------------------------------------------

/// Thumbnail URLs keyed by pixel width.
#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
pub struct CaaThumbnails {
    /// 1200px thumbnail, when the archive generated one.
    #[serde(rename = "1200", default)]
    pub size_1200: Option<String>,
    /// 500px thumbnail, when the archive generated one.
    #[serde(rename = "500", default)]
    pub size_500: Option<String>,
    /// 250px thumbnail, when the archive generated one.
    #[serde(rename = "250", default)]
    pub size_250: Option<String>,
}

/// One artwork image in the archive's listing.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct CaaImage {
    /// Whether MusicBrainz editors approved this image.
    #[serde(default)]
    pub approved: bool,
    /// Whether this image serves as the back cover.
    #[serde(default)]
    pub back: bool,
    /// Editor comment.
    #[serde(default)]
    pub comment: String,
    /// Whether this image serves as the front cover.
    #[serde(default)]
    pub front: bool,
    /// Numeric image id: required identity.
    pub id: u64,
    /// Original-file URL (observed with an `http` scheme and PNG
    /// originals: upgrade, never assume).
    #[serde(default)]
    pub image: String,
    /// Thumbnails by width.
    #[serde(default)]
    pub thumbnails: CaaThumbnails,
    /// Type labels (`Front`, `Back`, `Spine`, `Booklet`; one image may
    /// carry several).
    #[serde(default)]
    pub types: Vec<String>,
}

/// Archive listing for one release or release group.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct CaaResponse {
    /// Listed images, in archive order.
    #[serde(default)]
    pub images: Vec<CaaImage>,
}

/// Which entity the artwork was listed for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntityKind {
    /// Exact release: artwork belongs to this edition.
    Release,
    /// Release group: images come from a representative release and must
    /// be labelled release-group fallback, never exact-edition artwork
    /// (live 2026-07-21).
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

/// Validate an archive artwork URL and upgrade it to https (v2
/// `_management_artwork_url`): only `coverartarchive.org` over http/https,
/// no credentials, default ports only, absolute path. Query strings
/// survive; fragments do not. Anything else is rejected, since response
/// image URLs arrive with an `http` scheme (live 2026-07-21) and only
/// validated archive URLs may be upgraded.
pub fn upgrade_artwork_url(url: &str) -> Result<String, CaaError> {
    let (scheme, authority, path_and_query) =
        split_artwork_url(url).ok_or(CaaError::RejectedUrl)?;
    if scheme != "http" && scheme != "https" {
        return Err(CaaError::RejectedUrl);
    }
    if authority.contains('@') {
        return Err(CaaError::RejectedUrl);
    }
    let (host, port) = match authority.rsplit_once(':') {
        Some((host, port_text)) => {
            let port: u16 = port_text.parse().map_err(|_| CaaError::RejectedUrl)?;
            (host, Some(port))
        }
        None => (authority, None),
    };
    if !host.eq_ignore_ascii_case("coverartarchive.org")
        || !matches!(port, None | Some(80) | Some(443))
    {
        return Err(CaaError::RejectedUrl);
    }
    if !path_and_query.starts_with('/') {
        return Err(CaaError::RejectedUrl);
    }
    let path = path_and_query.split('#').next().unwrap_or("/");
    Ok(format!("https://coverartarchive.org{path}"))
}

/// Redirect hops [`CaaClient::fetch_front`] follows before giving up.
pub const MAX_REDIRECT_HOPS: usize = 5;

/// Validate one artwork redirect and return the https URL to fetch next.
/// Only the archive itself and the Internet Archive (`archive.org` and its
/// subdomains) may serve covers; no credentials, default ports only.
/// Relative locations resolve against the current URL's origin.
pub fn check_redirect_hop(current: &str, location: &str) -> Result<String, CaaError> {
    let absolute = if location.starts_with('/') {
        let (scheme, authority, _) = split_artwork_url(current).ok_or(CaaError::RejectedUrl)?;
        format!("{scheme}://{authority}{location}")
    } else {
        location.to_owned()
    };
    let (scheme, authority, path) = split_artwork_url(&absolute).ok_or(CaaError::RejectedUrl)?;
    if scheme != "http" && scheme != "https" {
        return Err(CaaError::RejectedUrl);
    }
    if authority.contains('@') || !path.starts_with('/') {
        return Err(CaaError::RejectedUrl);
    }
    let (host, port) = match authority.rsplit_once(':') {
        Some((host, port_text)) => {
            let port: u16 = port_text.parse().map_err(|_| CaaError::RejectedUrl)?;
            (host, Some(port))
        }
        None => (authority, None),
    };
    let host = host.to_ascii_lowercase();
    let allowed =
        host == "coverartarchive.org" || host == "archive.org" || host.ends_with(".archive.org");
    if !allowed || !matches!(port, None | Some(80) | Some(443)) {
        return Err(CaaError::RejectedUrl);
    }
    let path = path.split('#').next().unwrap_or("/");
    Ok(format!("https://{host}{path}"))
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
/// flags (v2 `_management_image_types`): casefolded, `front` prepended and
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
/// will not serve mislabeled (v2 `_sniff_image_content_type`): embedded
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

/// Cover Art Archive client over any [`CaaTransport`]. Owns conservative
/// pacing plus one Retry-After-honoring retry on 429/503 (v2 covers ride
/// at most two attempts: a cover that fails twice degrades rather than
/// stalling the hot path).
pub struct CaaClient<T: CaaTransport> {
    transport: T,
    gate: RateGate,
    max_backoff: Duration,
    fallback_backoff: Duration,
}

impl<T: CaaTransport> CaaClient<T> {
    /// Client with production pacing and backoff ceilings.
    pub fn new(transport: T) -> Self {
        Self {
            transport,
            gate: RateGate::coverart(),
            max_backoff: DEFAULT_MAX_BACKOFF,
            fallback_backoff: Duration::from_secs(1),
        }
    }

    /// Swap the pacing gate (tests pace faster than production).
    pub fn with_gate(mut self, gate: RateGate) -> Self {
        self.gate = gate;
        self
    }

    /// Swap the backoff ceilings (tests sleep milliseconds, not seconds).
    pub fn with_backoff(mut self, max_backoff: Duration, fallback_backoff: Duration) -> Self {
        self.max_backoff = max_backoff;
        self.fallback_backoff = fallback_backoff;
        self
    }

    /// List typed artwork images for an exact release or a labelled group
    /// fallback. A missing entity (404) is authoritative empty artwork;
    /// any other failure is a provider error, never silent emptiness.
    pub async fn list_artwork(
        &self,
        entity: EntityKind,
        mbid: &str,
        size: DownloadSize,
    ) -> Result<Vec<ArtworkCandidate>, CaaError> {
        let normalized = mbid.trim().to_ascii_lowercase();
        if !is_valid_mbid(&normalized) {
            return Err(CaaError::InvalidMbid(format!("invalid {entity:?} MBID")));
        }
        let url = format!("{}/{}/{normalized}", COVER_ART_ARCHIVE_BASE, entity.path());
        let response = self.get(&url).await?;
        if response.status == 404 {
            return Ok(Vec::new());
        }
        if response.status != 200 {
            return Err(CaaError::Rejected(response.status));
        }
        if response.body.len() > METADATA_MAX_BYTES {
            return Err(CaaError::Contract(
                "artwork metadata exceeded the safety limit".to_owned(),
            ));
        }
        let listing: CaaResponse = serde_json::from_slice(&response.body)
            .map_err(|error| CaaError::Contract(format!("invalid artwork metadata: {error}")))?;
        let mut candidates = Vec::new();
        for image in &listing.images {
            let selected = size.select(image);
            if selected.is_empty() {
                continue;
            }
            candidates.push(ArtworkCandidate {
                candidate_id: format!(
                    "caa:{}:{normalized}:{}:{}",
                    entity.path(),
                    image.id,
                    size.label()
                ),
                source: entity.source().to_owned(),
                locator: upgrade_artwork_url(selected)?,
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
    pub async fn download_artwork(
        &self,
        candidate: &ArtworkCandidate,
        maximum_bytes: usize,
    ) -> Result<ArtworkBytes, CaaError> {
        if candidate.source != EntityKind::Release.source()
            && candidate.source != EntityKind::ReleaseGroup.source()
        {
            return Err(CaaError::Contract(
                "the cover client only downloads CAA candidates".to_owned(),
            ));
        }
        if maximum_bytes == 0 {
            return Err(CaaError::Contract(
                "artwork byte limit must be positive".to_owned(),
            ));
        }
        let url = upgrade_artwork_url(&candidate.locator)?;
        let response = self.get(&url).await?;
        if response.status != 200 {
            return Err(CaaError::Rejected(response.status));
        }
        if response.body.len() > maximum_bytes {
            return Err(CaaError::Contract(
                "artwork body exceeded the byte limit".to_owned(),
            ));
        }
        let declared = response.header("content-type").unwrap_or("").to_owned();
        let content_type = if is_image_content_type(&declared) {
            declared
        } else if let Some(sniffed) = sniff_image_content_type(&response.body) {
            sniffed.to_owned()
        } else {
            declared
        };
        Ok(ArtworkBytes {
            bytes: response.body,
            content_type,
        })
    }

    /// Fetch the front cover of a release or release group at one size,
    /// the way v2 served covers: `/{entity}/{mbid}/front-{size}`, which the
    /// archive renders at 250, 500 and 1200 pixels, so nothing is resized
    /// here. The archive answers with a redirect to archive.org; each hop
    /// passes [`check_redirect_hop`] before it is followed. `None` means the
    /// archive has no front cover (404), which is authoritative.
    pub async fn fetch_front(
        &self,
        entity: EntityKind,
        mbid: &str,
        size: DownloadSize,
        maximum_bytes: usize,
    ) -> Result<Option<ArtworkBytes>, CaaError> {
        let normalized = mbid.trim().to_ascii_lowercase();
        if !is_valid_mbid(&normalized) {
            return Err(CaaError::InvalidMbid(format!("invalid {entity:?} MBID")));
        }
        let suffix = match size {
            DownloadSize::Full => String::new(),
            other => format!("-{}", other.label()),
        };
        let mut url = format!(
            "{COVER_ART_ARCHIVE_BASE}/{}/{normalized}/front{suffix}",
            entity.path()
        );
        let mut response = self.get(&url).await?;
        let mut hops = 0;
        while matches!(response.status, 301 | 302 | 303 | 307 | 308) {
            hops += 1;
            if hops > MAX_REDIRECT_HOPS {
                return Err(CaaError::Contract("too many artwork redirects".to_owned()));
            }
            let location = response.header("location").ok_or_else(|| {
                CaaError::Contract("artwork redirect without a location".to_owned())
            })?;
            url = check_redirect_hop(&url, location)?;
            // Hops land on the archive.org CDN, which the archive pacing
            // does not cover; only the first request is paced.
            response = self
                .transport
                .get(&CaaRequest {
                    url: url.clone(),
                    headers: Vec::new(),
                })
                .await
                .map_err(|error| CaaError::Unavailable(error.0))?;
            if response.status == 429 || response.status == 503 {
                return Err(CaaError::RateLimited {
                    retry_after_secs: parse_retry_after_secs(response.header("Retry-After")),
                });
            }
            if (500..600).contains(&response.status) {
                return Err(CaaError::Unavailable(format!("HTTP {}", response.status)));
            }
        }
        if response.status == 404 {
            return Ok(None);
        }
        if response.status != 200 {
            return Err(CaaError::Rejected(response.status));
        }
        if response.body.len() > maximum_bytes {
            return Err(CaaError::Contract(
                "artwork body exceeded the byte limit".to_owned(),
            ));
        }
        let declared = response.header("content-type").unwrap_or("").to_owned();
        let content_type = match sniff_image_content_type(&response.body) {
            Some(sniffed) => sniffed.to_owned(),
            None => {
                return Err(CaaError::Contract(format!(
                    "artwork is not a raster image ({declared})"
                )));
            }
        };
        Ok(Some(ArtworkBytes {
            bytes: response.body,
            content_type,
        }))
    }

    /// One paced GET with a single backoff retry on 429/503.
    async fn get(&self, url: &str) -> Result<RawResponse, CaaError> {
        for attempt in 0..2 {
            self.gate.acquire().await;
            let request = CaaRequest {
                url: url.to_owned(),
                headers: Vec::new(),
            };
            let response = self
                .transport
                .get(&request)
                .await
                .map_err(|error| CaaError::Unavailable(error.0))?;
            if response.status == 429 || response.status == 503 {
                let retry_after_secs = parse_retry_after_secs(response.header("Retry-After"));
                if attempt == 0 {
                    let delay = retry_after_secs
                        .map(Duration::from_secs_f64)
                        .unwrap_or(self.fallback_backoff)
                        .min(self.max_backoff);
                    tokio::time::sleep(delay).await;
                    continue;
                }
                return Err(CaaError::RateLimited { retry_after_secs });
            }
            if (500..600).contains(&response.status) {
                return Err(CaaError::Unavailable(format!("HTTP {}", response.status)));
            }
            return Ok(response);
        }
        Err(CaaError::Unavailable(
            "coverartarchive request failed".to_owned(),
        ))
    }
}
