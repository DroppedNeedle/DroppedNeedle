//! Streaming seam: the `decide()` policy, the audio backend trait, and
//! original-byte serving over the stream routes' range framing.
//!
//! The stream engine owns real bytes; the compat adapters implement
//! [`AudioBackend`] on top of it. Everything else here (the Feishin
//! bitrate-0 quirk, download filenames) is ported from v2 and covered by
//! goldens.
//!
//! v2: the compat transcode service (`decide()`), the local files service
//! (ranges, HEAD, MIME), and the Subsonic router (`_serve_file`, `_stream`,
//! `_download`, `_cover_size`).

use axum::http::HeaderMap;
use droppedneedle_media::stream::routes::{DirectSpan, content_type_for_extension};

use super::error::{GENERIC, SubsonicError};
pub use crate::body::AudioBody;

/// Minimum transcode bitrate kbps (v2 `_MIN_BITRATE_KBPS`).
pub const MIN_BITRATE_KBPS: i64 = 32;
/// Effectively-unset client ceiling (v2 `_HUGE`).
pub const UNSET_BITRATE_KBPS: i64 = 1_000_000_000;
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

/// Planned stream: direct file bytes or a transcode pipe.
#[derive(Debug, Clone, PartialEq)]
pub struct StreamPlan {
    /// True when the client must be transcoded for.
    pub transcode: bool,
    /// Output format (`mp3`/`opus`) when transcoding.
    pub out_format: Option<String>,
    /// Output bitrate kbps when transcoding.
    pub out_bitrate_kbps: Option<i64>,
    /// Start offset seconds (>= 0).
    pub start_seconds: f64,
}

/// The `decide()` policy, rules in order:
/// 1. force-original, transcoding off, or no ffmpeg -> direct.
/// 2. A transcode needs an explicit client trigger: codec mismatch
///    (`req` non-empty, not `raw`, not the source format) or the client
///    ceiling under the source bitrate. The server max is a quality
///    ceiling, never a trigger. `max_bitrate <= 0`/None means unset
///    (Feishin sends bitrate cap 0, issues #464/#468).
/// 3. Output is `req` when it is mp3/opus else the server default;
///    bitrate is the client cap, or the codec default (opus 128, mp3 192)
///    when the client set none, clamped to `max(min(that, server), 32)`.
#[allow(clippy::too_many_arguments)]
pub fn decide(
    source_format: &str,
    source_bitrate_kbps: Option<i64>,
    requested_format: Option<&str>,
    max_bitrate_kbps: Option<i64>,
    force_original: bool,
    start_seconds: f64,
    transcoding_enabled: bool,
    ffmpeg_available: bool,
    default_format: &str,
    server_max_bitrate_kbps: i64,
) -> StreamPlan {
    let start_seconds = start_seconds.max(0.0);
    if force_original || !transcoding_enabled || !ffmpeg_available {
        return StreamPlan {
            transcode: false,
            out_format: None,
            out_bitrate_kbps: None,
            start_seconds,
        };
    }
    let client_ceiling = match max_bitrate_kbps {
        Some(bitrate) if bitrate > 0 => bitrate,
        _ => UNSET_BITRATE_KBPS,
    };
    let source = source_format.to_lowercase();
    let codec_mismatch = requested_format.is_some_and(|req| {
        let req = req.to_lowercase();
        !req.is_empty() && req != "raw" && req != source
    });
    let bitrate_trigger =
        source_bitrate_kbps.is_some_and(|source_bitrate| client_ceiling < source_bitrate);
    if !codec_mismatch && !bitrate_trigger {
        return StreamPlan {
            transcode: false,
            out_format: None,
            out_bitrate_kbps: None,
            start_seconds,
        };
    }
    let out_format = match requested_format.map(str::to_lowercase) {
        Some(req) if req == "mp3" || req == "opus" => req,
        _ => default_format.to_lowercase(),
    };
    // No client cap: the codec's default bitrate, not the server ceiling.
    let wanted = if client_ceiling == UNSET_BITRATE_KBPS {
        droppedneedle_media::stream::transcode::default_bitrate_kbps(&out_format)
    } else {
        client_ceiling
    };
    let bitrate = wanted.min(server_max_bitrate_kbps).max(MIN_BITRATE_KBPS);
    StreamPlan {
        transcode: true,
        out_format: Some(out_format),
        out_bitrate_kbps: Some(bitrate),
        start_seconds,
    }
}

/// Estimated transcode length (bitrate*1000/8*(duration-start)),
/// attached only with `estimateContentLength=true` on transcodes.
pub fn estimate_transcode_length(
    bitrate_kbps: i64,
    duration_seconds: f64,
    start_seconds: f64,
) -> i64 {
    let remaining = (duration_seconds - start_seconds).max(0.0);
    (bitrate_kbps as f64 * 1000.0 / 8.0 * remaining) as i64
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

/// Audio facts the backend reports for one file id.
#[derive(Debug, Clone)]
pub struct AudioFacts {
    /// File size bytes.
    pub size: u64,
    /// Audio suffix (`mp3`, `flac`, ...).
    pub suffix: String,
    /// Source bitrate kbps, if known.
    pub bitrate_kbps: Option<i64>,
    /// Duration seconds, if known.
    pub duration_seconds: Option<f64>,
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

/// Backend failure: a plain storage failure (surfaced as code 0) or
/// a full concurrency pool (surfaced as 429 + `Retry-After: 1`).
#[derive(Debug, Clone)]
pub struct BackendError {
    message: String,
    capacity_full: bool,
}

impl BackendError {
    /// Storage failure.
    pub fn failed(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            capacity_full: false,
        }
    }

    /// Full pool (direct 32/8, transcode 2/1 in v2).
    pub fn full() -> Self {
        Self {
            message: "stream capacity exhausted".to_owned(),
            capacity_full: true,
        }
    }

    /// True when the caller must answer 429.
    pub fn is_full(&self) -> bool {
        self.capacity_full
    }
}

impl std::fmt::Display for BackendError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for BackendError {}

/// Minimal audio backend, implemented over the stream engine (local files
/// plus the transcode pipeline) in `compat::adapters::engines`. Leases,
/// concurrency pools and cancellation live behind this trait.
pub trait AudioBackend: Clone + Send + Sync {
    /// The backend scoped to one authenticated caller, so stream leases
    /// count against that user. Backends without leases return themselves.
    fn for_caller(&self, _user_id: &str) -> Self {
        self.clone()
    }

    /// Facts for a file id from metadata alone (HEAD), or None when
    /// nothing can serve the track.
    fn audio_facts(
        &self,
        file_id: &str,
    ) -> impl Future<Output = Result<Option<AudioFacts>, BackendError>> + Send;

    /// Open the original once for a GET: facts first, then one span of
    /// bytes read as the response streams.
    fn open_original(
        &self,
        file_id: &str,
    ) -> impl Future<Output = Result<Option<OpenedAudio>, BackendError>> + Send;

    /// Transcode output + content type for a plan (ffmpeg pipe behind
    /// the seam), streamed as ffmpeg produces it.
    fn transcode(
        &self,
        file_id: &str,
        plan: &StreamPlan,
    ) -> impl Future<Output = Result<(AudioBody, String), BackendError>> + Send;
}

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
        if err.is_full() {
            ServeError::CapacityFull
        } else {
            ServeError::Subsonic(SubsonicError::new(GENERIC, err.to_string()))
        }
    }
}

/// Serve original bytes (v2 `_serve_file` minus the lease plumbing, which
/// lives behind [`AudioBackend`]). Ranges, lengths and the 200/206/416
/// verdict come from the stream routes' [`DirectSpan`], so they match the
/// native and Jellyfin routes. HEAD answers the status and headers GET
/// would (HEAD honors Range, like the stream engine) with an empty body,
/// from facts alone. GET opens the backend exactly once
/// ([`AudioBackend::open_original`]) and streams the asked span.
pub async fn serve_original<B: AudioBackend>(
    backend: &B,
    file_id: &str,
    range_header: Option<&str>,
    head_only: bool,
    content_disposition: Option<String>,
) -> Result<ServedAudio, ServeError> {
    let not_found = || SubsonicError::code_only(super::error::NOT_FOUND);
    let (facts, opened) = if head_only {
        let facts = backend.audio_facts(file_id).await?.ok_or_else(not_found)?;
        (facts, None)
    } else {
        let opened = backend
            .open_original(file_id)
            .await?
            .ok_or_else(not_found)?;
        (opened.facts.clone(), Some(opened))
    };
    let content_type = content_type_for_extension(&facts.suffix).ok_or_else(|| {
        SubsonicError::new(GENERIC, format!("Cannot stream .{} files", facts.suffix))
    })?;
    let span = DirectSpan::for_request(range_header, facts.size)
        .ok_or(ServeError::RangeUnsatisfiable(facts.size))?;
    let mut framing = HeaderMap::new();
    span.write_headers(&mut framing);
    let mut headers = vec![("Content-Encoding".to_owned(), "identity".to_owned())];
    headers.extend(framing.iter().filter_map(|(name, value)| {
        Some((name.as_str().to_owned(), value.to_str().ok()?.to_owned()))
    }));
    if let Some(disposition) = content_disposition {
        headers.push(("Content-Disposition".to_owned(), disposition));
    }
    let body = match opened {
        Some(opened) => opened.read(span.start, span.len),
        None => {
            headers.push(("Content-Type".to_owned(), content_type.to_owned()));
            AudioBody::empty()
        }
    };
    Ok(ServedAudio {
        status: span.status().as_u16(),
        content_type: content_type.to_owned(),
        headers,
        body,
    })
}
