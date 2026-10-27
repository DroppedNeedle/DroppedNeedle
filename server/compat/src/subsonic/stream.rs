//! Serving what the stream gateway opened over the stream routes' range
//! framing.
//!
//! The handlers translate Subsonic's parameters into [`StreamParams`];
//! the gateway, through [`GatewayAudio`], makes the one transcode decision
//! and says which plan it ran. Everything else here (download
//! filenames, cover buckets) is ported from v2 and covered by goldens.
//!
//! v2: the local files service (ranges, HEAD, MIME) and the Subsonic
//! router (`_serve_file`, `_stream`, `_download`, `_cover_size`).

use axum::http::HeaderMap;
pub use droppedneedle_media::stream::routes::StreamParams;
use droppedneedle_media::stream::routes::{DirectSpan, StreamEngine};

use super::error::{GENERIC, NOT_AUTHORIZED, SubsonicError};
use crate::adapters::audio::GatewayAudio;
pub use crate::body::AudioBody;

/// Max `timeOffset` / transcode `offset` seconds.
pub const MAX_OFFSET_SECONDS: f64 = 604_800.0;

/// Transcode output MIME/suffix (v2 `out_media_type`/`out_suffix`).
/// The one copy: `opus` maps to the ogg pair, every other format
/// (including `mp3` and anything unexpected) to the mp3 pair. The server
/// default is validated to `mp3`/`opus`, so every reachable input maps
/// as v2 did.
pub fn transcode_hint(format: &str) -> (&'static str, &'static str) {
    match format {
        "opus" => ("audio/ogg", "opus"),
        _ => ("audio/mpeg", "mp3"),
    }
}

/// Download filename: sanitize the stem (`[^A-Za-z0-9._ -]` -> `_`,
/// quotes -> `_`, trim dots/spaces) and force the audio suffix.
pub fn download_filename(title: &str, file_format: &str) -> String {
    let suffix: String = file_format
        .to_lowercase()
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .collect();
    let suffix = if suffix.is_empty() {
        "bin".to_owned()
    } else {
        suffix
    };
    let mut stem: String = title
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == ' ' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();
    stem = stem.trim_matches([' ', '.']).to_owned().replace('"', "_");
    if stem.is_empty() {
        stem = "track".to_owned();
    }
    format!("{stem}.{suffix}")
}

/// Cover-art size bucket: <=300 -> 250, <=750 -> 500, else 1200
/// (v2 `_cover_size`; absent size means 500).
pub fn cover_bucket(size: Option<i64>) -> &'static str {
    match size {
        None => "500",
        Some(px) if px <= 300 => "250",
        Some(px) if px <= 750 => "500",
        _ => "1200",
    }
}

/// Side of the placeholder cover in pixels. A smooth gradient scales up
/// cleanly, so one small size serves every bucket.
const PLACEHOLDER_SIDE: u32 = 128;

/// The web UI's placeholder palette, in its order, as the default (dark)
/// theme draws it. These mirror the theme tokens `--color-primary`,
/// `-secondary`, `-accent`, `-info`, `-success`, `-warning` and `-error`:
/// primary and accent are set in `frontend/src/theme.css`, the rest are
/// daisyUI 5's stock dark values, converted from oklch to sRGB. Keep them
/// in step with those tokens.
const PLACEHOLDER_PALETTE: [[u8; 3]; 7] = [
    [174, 213, 242],
    [244, 48, 152],
    [187, 219, 155],
    [0, 186, 254],
    [0, 211, 144],
    [252, 183, 0],
    [255, 98, 125],
];
/// The pair an empty seed gets: `--color-base-300` (from `theme.css`) to
/// `--color-neutral` (daisyUI's dark), at 135 degrees.
const PLACEHOLDER_NEUTRAL: ([u8; 3], [u8; 3], u32) = ([31, 39, 27], [9, 9, 11], 135);
const PLACEHOLDER_ANGLES: [u32; 4] = [45, 135, 225, 315];

/// FNV-1a over UTF-16 code units, then the murmur3 finaliser: the same
/// hash as the web UI's, so a seed picks the same colours in both.
fn placeholder_hash(text: &str) -> u32 {
    let mut h: u32 = 0x811c_9dc5;
    for unit in text.encode_utf16() {
        h = (h ^ u32::from(unit)).wrapping_mul(0x0100_0193);
    }
    h ^= h >> 16;
    h = h.wrapping_mul(0x85eb_ca6b);
    h ^= h >> 13;
    h = h.wrapping_mul(0xc2b2_ae35);
    h ^= h >> 16;
    h
}

/// The two colours and the CSS gradient angle for a seed, picked as the
/// web UI's `coverPlaceholder` picks them.
fn placeholder_look(seed: &str) -> ([u8; 3], [u8; 3], u32) {
    let text = seed.trim().to_lowercase();
    if text.is_empty() {
        return PLACEHOLDER_NEUTRAL;
    }
    let h = placeholder_hash(&text) as usize;
    let count = PLACEHOLDER_PALETTE.len();
    let from = h % count;
    let to = (from + 1 + (h / count) % (count - 1)) % count;
    let angle = PLACEHOLDER_ANGLES[(h / (count * (count - 1))) % PLACEHOLDER_ANGLES.len()];
    (PLACEHOLDER_PALETTE[from], PLACEHOLDER_PALETTE[to], angle)
}

/// Placeholder cover for an item with no picture, as WebP (many phone apps
/// cannot draw SVG): a two-colour diagonal gradient worked out from the
/// seed (the item's MusicBrainz id, or its name), drawn like CSS
/// `linear-gradient(<angle>deg, from, to)` so it matches the web UI.
/// `None` only if encoding fails, which is logged.
pub fn placeholder_cover(seed: &str) -> Option<Vec<u8>> {
    let (from, to, angle) = placeholder_look(seed);
    let side = PLACEHOLDER_SIDE as f32;
    let (sin, cos) = (angle as f32).to_radians().sin_cos();
    // CSS angles point up at 0 and turn clockwise; image rows run down.
    let (dx, dy) = (sin, -cos);
    let length = side * (sin.abs() + cos.abs());
    let picture = image::RgbImage::from_fn(PLACEHOLDER_SIDE, PLACEHOLDER_SIDE, |x, y| {
        let along = (x as f32 + 0.5 - side / 2.0) * dx + (y as f32 + 0.5 - side / 2.0) * dy;
        let t = (0.5 + along / length).clamp(0.0, 1.0);
        image::Rgb(std::array::from_fn(|i| {
            (f32::from(from[i]) + (f32::from(to[i]) - f32::from(from[i])) * t).round() as u8
        }))
    });
    let mut bytes = std::io::Cursor::new(Vec::new());
    if let Err(err) = picture.write_to(&mut bytes, image::ImageFormat::WebP) {
        tracing::warn!(error = %err, "placeholder cover failed to encode");
        return None;
    }
    Some(bytes.into_inner())
}

/// Facts about an original the backend opened.
#[derive(Debug, Clone)]
pub struct AudioFacts {
    /// File size bytes.
    pub size: u64,
    /// The content type the backend serves it under.
    pub content_type: String,
}

/// One served audio response: headers plus the body (already ranged).
/// The HTTP adapter streams a live body as it is read.
#[derive(Debug, Clone)]
pub struct ServedAudio {
    /// 200 or 206.
    pub status: u16,
    /// Content type.
    pub content_type: String,
    /// Headers (Content-Length, Content-Range, Accept-Ranges, ...).
    pub headers: Vec<(String, String)>,
    /// The body.
    pub body: AudioBody,
}

/// Reads one byte span of an opened original: `(start, len)`.
type SpanReader = Box<dyn FnOnce(u64, u64) -> AudioBody + Send>;

/// An opened original: its facts now, one span of its bytes later. The
/// backend's stream lease rides inside the reader and then the body.
pub struct OpenedAudio {
    /// Size, suffix and the rest, known before any byte is read.
    pub facts: AudioFacts,
    read: SpanReader,
}

impl OpenedAudio {
    /// Facts plus the reader that serves one span.
    pub fn new(
        facts: AudioFacts,
        read: impl FnOnce(u64, u64) -> AudioBody + Send + 'static,
    ) -> Self {
        Self {
            facts,
            read: Box::new(read),
        }
    }

    /// The body for `len` bytes from `start`.
    pub fn read(self, start: u64, len: u64) -> AudioBody {
        (self.read)(start, len)
    }
}

/// A transcode the backend started, or for HEAD would start.
pub struct TranscodedAudio {
    /// Content type of the output.
    pub content_type: String,
    /// Estimated length, when the request asked for one.
    pub estimated_len: Option<u64>,
    /// The output as ffmpeg produces it; empty for HEAD.
    pub body: AudioBody,
}

/// What the backend opened for one request: the original, or a transcode.
pub enum PlayedAudio {
    /// The original bytes, served by range.
    Original(OpenedAudio),
    /// Transcode output, served whole.
    Transcoded(TranscodedAudio),
}

/// Why the backend could not serve.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BackendFailure {
    /// A storage or server failure (code 0).
    Failed,
    /// Playback refused (code 50).
    Forbidden,
    /// A full concurrency pool (429 + `Retry-After: 1`).
    Full,
}

/// Backend failure: a plain storage failure (surfaced as code 0), a
/// refusal (code 50), or a full concurrency pool (429 + `Retry-After: 1`).
#[derive(Debug, Clone)]
pub struct BackendError {
    message: String,
    failure: BackendFailure,
}

impl BackendError {
    /// Storage failure.
    pub fn failed(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            failure: BackendFailure::Failed,
        }
    }

    /// Playback refused, with a message the client may see.
    pub fn forbidden(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            failure: BackendFailure::Forbidden,
        }
    }

    /// Full pool (direct 32/8, transcode 2/1 in v2).
    pub fn full() -> Self {
        Self {
            message: "stream capacity exhausted".to_owned(),
            failure: BackendFailure::Full,
        }
    }

    /// True when the caller must answer 429.
    pub fn is_full(&self) -> bool {
        self.failure == BackendFailure::Full
    }
}

impl std::fmt::Display for BackendError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for BackendError {}

/// Failure serving original bytes: a protocol error, a 416 range
/// refusal carrying the file size for `Content-Range: bytes */N`, or a
/// full pool (429 + `Retry-After: 1`).
#[derive(Debug, Clone)]
pub enum ServeError {
    /// Protocol error (70, 0, ...).
    Subsonic(SubsonicError),
    /// Unsatisfiable range over a file of this size.
    RangeUnsatisfiable(u64),
    /// Concurrency pool full.
    CapacityFull,
}

impl From<SubsonicError> for ServeError {
    fn from(err: SubsonicError) -> Self {
        ServeError::Subsonic(err)
    }
}

impl From<BackendError> for ServeError {
    fn from(err: BackendError) -> Self {
        match err.failure {
            BackendFailure::Full => ServeError::CapacityFull,
            BackendFailure::Forbidden => {
                ServeError::Subsonic(SubsonicError::new(NOT_AUTHORIZED, err.message))
            }
            BackendFailure::Failed => {
                ServeError::Subsonic(SubsonicError::new(GENERIC, err.message))
            }
        }
    }
}

/// Serve one file id as `params` ask (v2 `_serve_file` and the transcode
/// pipe, minus the lease plumbing, which lives in [`GatewayAudio`]).
///
/// An original takes its ranges, lengths and the 200/206/416 verdict from
/// the stream routes' [`DirectSpan`], so they match the native and Jellyfin
/// routes; HEAD honors Range too. A transcode is 200 with
/// `Accept-Ranges: none` and `Cache-Control: no-store`, sized only by the
/// estimate the request asked for. HEAD answers the headers GET would,
/// with an empty body and no transcode started. GET opens the backend
/// exactly once.
pub async fn serve<E: StreamEngine + 'static>(
    backend: &GatewayAudio<E>,
    file_id: &str,
    params: &StreamParams,
    range_header: Option<&str>,
    head_only: bool,
    content_disposition: Option<String>,
) -> Result<ServedAudio, ServeError> {
    let played = backend
        .play_subsonic(file_id, params, head_only)
        .await?
        .ok_or_else(|| SubsonicError::code_only(super::error::NOT_FOUND))?;
    let mut headers = vec![("Content-Encoding".to_owned(), "identity".to_owned())];
    let opened = match played {
        PlayedAudio::Original(opened) => opened,
        PlayedAudio::Transcoded(transcoded) => {
            headers.push(("Accept-Ranges".to_owned(), "none".to_owned()));
            headers.push(("Cache-Control".to_owned(), "no-store".to_owned()));
            if let Some(len) = transcoded.estimated_len {
                headers.push(("Content-Length".to_owned(), len.to_string()));
            }
            return Ok(ServedAudio {
                status: 200,
                content_type: transcoded.content_type,
                headers,
                body: transcoded.body,
            });
        }
    };
    let content_type = opened.facts.content_type.clone();
    let span = DirectSpan::for_request(range_header, opened.facts.size)
        .ok_or(ServeError::RangeUnsatisfiable(opened.facts.size))?;
    let mut framing = HeaderMap::new();
    span.write_headers(&mut framing);
    headers.extend(framing.iter().filter_map(|(name, value)| {
        Some((name.as_str().to_owned(), value.to_str().ok()?.to_owned()))
    }));
    if let Some(disposition) = content_disposition {
        headers.push(("Content-Disposition".to_owned(), disposition));
    }
    let body = if head_only {
        headers.push(("Content-Type".to_owned(), content_type.clone()));
        AudioBody::empty()
    } else {
        opened.read(span.start, span.len)
    };
    Ok(ServedAudio {
        status: span.status().as_u16(),
        content_type,
        headers,
        body,
    })
}
