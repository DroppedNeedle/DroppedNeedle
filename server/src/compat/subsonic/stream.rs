//! Streaming seam: range math, content types, the `decide()` policy,
//! and the audio backend trait.
//!
//! The stream engine owns real bytes; the compat adapters implement
//! [`AudioBackend`] on top of it. Everything else here (byte-exact range
//! handling, the Feishin bitrate-0 quirk, download filenames) is ported
//! from v2 and covered by goldens.
//!
//! v2: the compat transcode service (`decide()`), the local files service
//! (ranges, HEAD, MIME), and the Subsonic router (`_serve_file`, `_stream`,
//! `_download`, `_cover_size`).

use super::error::{GENERIC, SubsonicError};
pub use crate::compat::body::AudioBody;

/// Minimum transcode bitrate kbps (v2 `_MIN_BITRATE_KBPS`).
pub const MIN_BITRATE_KBPS: i64 = 32;
/// Effectively-unset client ceiling (v2 `_HUGE`).
pub const UNSET_BITRATE_KBPS: i64 = 1_000_000_000;
/// Max `timeOffset` / transcode `offset` seconds.
pub const MAX_OFFSET_SECONDS: f64 = 604_800.0;

/// Streamable audio suffixes. Anything else is never streamed
/// (v2 refuses non-audio suffixes in `stream_track`).
pub fn is_audio_suffix(suffix: &str) -> bool {
    matches!(
        suffix.to_lowercase().as_str(),
        "flac" | "mp3" | "ogg" | "opus" | "m4a" | "aac" | "wav" | "wma"
    )
}

/// Content type for an audio suffix (v2 `LocalFilesService` table).
pub fn content_type_for(suffix: &str) -> Option<&'static str> {
    match suffix.to_lowercase().as_str() {
        "flac" => Some("audio/flac"),
        "mp3" => Some("audio/mpeg"),
        "ogg" => Some("audio/ogg"),
        "m4a" => Some("audio/mp4"),
        "aac" => Some("audio/aac"),
        "wav" => Some("audio/wav"),
        "wma" => Some("audio/x-ms-wma"),
        "opus" => Some("audio/opus"),
        _ => None,
    }
}

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
        crate::stream::transcode::default_bitrate_kbps(&out_format)
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

/// Parsed single range: (start, end-inclusive).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ByteRange {
    /// First byte.
    pub start: u64,
    /// Last byte, inclusive.
    pub end: u64,
}

/// Range outcome: full body, partial body, or 416.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RangeOutcome {
    /// No (valid) range: whole body, 200.
    Full,
    /// Satisfiable range: partial body, 206.
    Partial(ByteRange),
    /// Unsatisfiable: 416 with `Content-Range: bytes */N`, no body.
    Unsatisfiable,
}

/// Byte-exact range parsing (v2 `stream_track`): single-range `bytes=`
/// only. `bytes=N-M`, open `bytes=N-`, and suffix `bytes=-N` all return
/// 206. Multi-range, malformed, empty, or unsatisfiable (start >= size,
/// `bytes=-0`, zero-length file) returns 416.
pub fn parse_range(header: Option<&str>, file_size: u64) -> RangeOutcome {
    let Some(header) = header else {
        return RangeOutcome::Full;
    };
    let header = header.trim();
    let Some(spec) = header.strip_prefix("bytes=") else {
        return RangeOutcome::Unsatisfiable;
    };
    if spec.contains(',') || spec.is_empty() || file_size == 0 {
        return RangeOutcome::Unsatisfiable;
    }
    let (start_part, end_part) = match spec.split_once('-') {
        Some(parts) => parts,
        None => return RangeOutcome::Unsatisfiable,
    };
    if start_part.is_empty() {
        let suffix: u64 = match end_part.parse() {
            Ok(0) | Err(_) => return RangeOutcome::Unsatisfiable,
            Ok(n) => n,
        };
        let take = suffix.min(file_size);
        return RangeOutcome::Partial(ByteRange {
            start: file_size - take,
            end: file_size - 1,
        });
    }
    let start: u64 = match start_part.parse() {
        Ok(n) => n,
        Err(_) => return RangeOutcome::Unsatisfiable,
    };
    if start >= file_size {
        return RangeOutcome::Unsatisfiable;
    }
    if end_part.is_empty() {
        return RangeOutcome::Partial(ByteRange {
            start,
            end: file_size - 1,
        });
    }
    let end: u64 = match end_part.parse() {
        Ok(n) => n,
        Err(_) => return RangeOutcome::Unsatisfiable,
    };
    if end < start {
        return RangeOutcome::Unsatisfiable;
    }
    RangeOutcome::Partial(ByteRange {
        start,
        end: end.min(file_size - 1),
    })
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

/// Gray SVG placeholder served on art misses (v2 `_PLACEHOLDER_SVG`).
pub const PLACEHOLDER_SVG: &str = "<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 200 200\"><rect fill=\"#374151\" width=\"200\" height=\"200\"/><circle cx=\"100\" cy=\"100\" r=\"70\" fill=\"#1f2937\" stroke=\"#4B5563\" stroke-width=\"2\"/><circle cx=\"100\" cy=\"100\" r=\"12\" fill=\"#4B5563\"/></svg>";

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

/// Serve original bytes with HEAD/range/416 handling (v2 `_serve_file`
/// minus the lease plumbing, which lives behind [`AudioBackend`]).
/// HEAD answers the same status and headers GET would (200/206/416;
/// HEAD honors Range, like the stream engine), always with an empty
/// body. GET opens the backend exactly once
/// ([`AudioBackend::open_original`]) and streams the asked span.
pub async fn serve_original<B: AudioBackend>(
    backend: &B,
    file_id: &str,
    range_header: Option<&str>,
    head_only: bool,
    content_disposition: Option<String>,
) -> Result<ServedAudio, ServeError> {
    if head_only {
        return serve_head(backend, file_id, range_header, content_disposition).await;
    }
    let opened = backend
        .open_original(file_id)
        .await?
        .ok_or_else(|| SubsonicError::code_only(super::error::NOT_FOUND))?;
    let size = opened.facts.size;
    let content_type = content_type_for(&opened.facts.suffix).ok_or_else(|| {
        SubsonicError::new(
            GENERIC,
            format!("Cannot stream .{} files", opened.facts.suffix),
        )
    })?;
    let mut headers = vec![
        ("Accept-Ranges".to_owned(), "bytes".to_owned()),
        ("Content-Encoding".to_owned(), "identity".to_owned()),
    ];
    if let Some(disposition) = content_disposition {
        headers.push(("Content-Disposition".to_owned(), disposition));
    }
    match parse_range(range_header, size) {
        RangeOutcome::Full => {
            headers.push(("Content-Length".to_owned(), size.to_string()));
            Ok(ServedAudio {
                status: 200,
                content_type: content_type.to_owned(),
                headers,
                body: opened.read(0, size),
            })
        }
        RangeOutcome::Partial(range) => {
            let len = range.end - range.start + 1;
            headers.push(("Content-Length".to_owned(), len.to_string()));
            headers.push((
                "Content-Range".to_owned(),
                format!("bytes {}-{}/{}", range.start, range.end, size),
            ));
            Ok(ServedAudio {
                status: 206,
                content_type: content_type.to_owned(),
                headers,
                body: opened.read(range.start, len),
            })
        }
        RangeOutcome::Unsatisfiable => Err(ServeError::RangeUnsatisfiable(size)),
    }
}

/// HEAD over original bytes: facts only (no object read), then the same
/// 200/206/416 outcome GET would answer, with an empty body.
async fn serve_head<B: AudioBackend>(
    backend: &B,
    file_id: &str,
    range_header: Option<&str>,
    content_disposition: Option<String>,
) -> Result<ServedAudio, ServeError> {
    let facts = backend
        .audio_facts(file_id)
        .await?
        .ok_or_else(|| SubsonicError::code_only(super::error::NOT_FOUND))?;
    let content_type = content_type_for(&facts.suffix).ok_or_else(|| {
        SubsonicError::new(GENERIC, format!("Cannot stream .{} files", facts.suffix))
    })?;
    let mut headers = vec![
        ("Accept-Ranges".to_owned(), "bytes".to_owned()),
        ("Content-Encoding".to_owned(), "identity".to_owned()),
    ];
    if let Some(disposition) = content_disposition {
        headers.push(("Content-Disposition".to_owned(), disposition));
    }
    match parse_range(range_header, facts.size) {
        RangeOutcome::Full => {
            headers.push(("Content-Length".to_owned(), facts.size.to_string()));
            headers.push(("Content-Type".to_owned(), content_type.to_owned()));
            Ok(ServedAudio {
                status: 200,
                content_type: content_type.to_owned(),
                headers,
                body: AudioBody::empty(),
            })
        }
        RangeOutcome::Partial(range) => {
            headers.push((
                "Content-Length".to_owned(),
                (range.end - range.start + 1).to_string(),
            ));
            headers.push((
                "Content-Range".to_owned(),
                format!("bytes {}-{}/{}", range.start, range.end, facts.size),
            ));
            headers.push(("Content-Type".to_owned(), content_type.to_owned()));
            Ok(ServedAudio {
                status: 206,
                content_type: content_type.to_owned(),
                headers,
                body: AudioBody::empty(),
            })
        }
        RangeOutcome::Unsatisfiable => Err(ServeError::RangeUnsatisfiable(facts.size)),
    }
}
