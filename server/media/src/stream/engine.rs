//! What the stream routes and the gateway share: the sources, one read's
//! request and answer, the engine trait they meet at and the audio
//! content-type table.
//!
//! [`StreamEngine`] is implemented by [`super::gateway::Gateway`]
//! (leases + `Transcoder` + direct/transcode reads). [`StreamEngine::open_stream`]
//! hands back a [`MediaBody`]: a local file read by range on demand, a
//! transcode read chunk by chunk, or in-memory bytes.
//!
//! [`content_type_for_extension`] is the one audio content-type table
//! without WMA, which is not supported. The engine resolves the final
//! content type (upstream wins for remotes); local reads go through this
//! table.

use futures_util::stream::BoxStream;

/// The stream sources behind one gateway. Display names feed the logged
/// cause of a failed read only; wire identity is the lowercase route
/// segment.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AudioSource {
    /// Local library file bytes.
    Local,
    /// Jellyfin proxied audio.
    Jellyfin,
    /// Navidrome proxied audio.
    Navidrome,
    /// Plex proxied audio (part keys may contain slashes).
    Plex,
    /// A `streaming_source` plugin; the key is a recording MBID.
    Plugin,
}

impl AudioSource {
    /// Parse the `{source}` path segment. Case-insensitive; `None` rejects.
    pub fn parse(segment: &str) -> Option<Self> {
        match segment.to_lowercase().as_str() {
            "local" => Some(Self::Local),
            "jellyfin" => Some(Self::Jellyfin),
            "navidrome" => Some(Self::Navidrome),
            "plex" => Some(Self::Plex),
            "plugin" => Some(Self::Plugin),
            _ => None,
        }
    }

    /// Route segment spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Local => "local",
            Self::Jellyfin => "jellyfin",
            Self::Navidrome => "navidrome",
            Self::Plex => "plex",
            Self::Plugin => "plugin",
        }
    }

    /// Human name for the logged cause of a failed read.
    pub(super) fn display_name(self) -> &'static str {
        match self {
            Self::Local => "local file",
            Self::Jellyfin => "Jellyfin",
            Self::Navidrome => "Navidrome",
            Self::Plex => "Plex",
            Self::Plugin => "plugin",
        }
    }
}

/// What a client asked of one stream, in every protocol's terms: the
/// native query maps 1:1, and the Subsonic and Jellyfin routes translate
/// their own parameter names into it. The engine's `decide()` reads it.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct StreamParams {
    /// Requested output codec (`mp3`, `opus`, `flac`, `raw`, or unset). `raw`
    /// always serves the original bytes.
    pub format: Option<String>,
    /// Client bitrate cap in kbps. 0 or unset means uncapped.
    pub max_bitrate_kbps: Option<i64>,
    /// Ask a transcode landing for its estimated `Content-Length`.
    pub estimate_content_length: bool,
    /// Seek offset in seconds (`-ss`, transcode only).
    pub start_seconds: f64,
}

impl StreamParams {
    /// The original bytes, whatever the transcode policy says.
    pub fn original() -> Self {
        Self {
            format: Some("raw".to_owned()),
            ..Self::default()
        }
    }
}

/// One engine read: identity, lease principal, and transcode hints.
#[derive(Debug, Clone, PartialEq)]
pub struct StreamOpen {
    /// Which source backend to read from.
    pub source: AudioSource,
    /// File id, item id, or Plex part key.
    pub key: String,
    /// Lease principal (the session user id).
    pub user_id: String,
    /// Transcode hints from the query string.
    pub params: StreamParams,
}

/// Body chunks of a streamed response.
pub type ChunkStream = BoxStream<'static, Result<Vec<u8>, std::io::Error>>;

/// Opens `len` bytes from `start` of a remote object. The stream is lazy:
/// nothing goes upstream until the response body is first polled.
pub type SpanOpener = Box<dyn FnOnce(u64, u64) -> ChunkStream + Send>;

/// Where the response body comes from.
pub enum MediaBody {
    /// The whole object in memory.
    Bytes(Vec<u8>),
    /// A local file, read by range when the response streams.
    File(super::local_files::LocatedFile),
    /// Live transcode output, chunk by chunk.
    Chunks(ChunkStream),
    /// A sized remote object opened by span on demand (plugin URLs), so a
    /// seek asks the upstream for just that range.
    Spans(SpanOpener),
    /// Nothing to send: HEAD of a transcode, answered without ffmpeg.
    Empty,
}

impl std::fmt::Debug for MediaBody {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Bytes(bytes) => write!(f, "Bytes({} bytes)", bytes.len()),
            Self::File(located) => write!(f, "File({})", located.path.display()),
            Self::Chunks(_) => f.write_str("Chunks"),
            Self::Spans(_) => f.write_str("Spans"),
            Self::Empty => f.write_str("Empty"),
        }
    }
}

/// One opened media object for the stream routes.
#[derive(Debug)]
pub struct StreamMedia {
    /// Engine-resolved content type.
    pub content_type: String,
    /// Exact object length for direct reads; unknown (0) for transcodes.
    pub total_len: u64,
    /// Transcode landing: ranges are refused, never honored.
    pub transcoded: bool,
    /// Estimated length for the optional transcode `Content-Length`.
    pub estimated_len: Option<u64>,
    /// The body source.
    pub body: MediaBody,
    /// The direct lease the open took, when the engine hands it back.
    /// Holding it until the body ends bounds response concurrency.
    pub lease: Option<super::leases::OwnedDirectLease>,
}

/// Engine failures, which the stream routes map to the wire.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StreamFault {
    /// Unknown id.
    NotFound,
    /// Playback refused or path outside the music directory. The message is
    /// user-facing detail, never a path.
    Forbidden {
        /// What was refused, e.g. "Playback not allowed".
        message: String,
    },
    /// Unsupported audio format or bad key. User-facing.
    InvalidInput {
        /// What was wrong.
        message: String,
    },
    /// Direct or transcode leases exhausted. Routes answer 429 + Retry-After.
    Capacity,
    /// The source failed the read.
    Upstream {
        /// Which source failed.
        source: AudioSource,
        /// Log-only cause.
        cause: String,
    },
    /// Server fault. The cause reaches the log only, never the wire.
    Internal {
        /// Log-only cause.
        cause: String,
    },
}

/// The engine the routes read from: open one media object or fail with a
/// [`StreamFault`]. [`super::gateway::Gateway`] is the production
/// implementation.
pub trait StreamEngine: Send + Sync {
    /// Resolve one media object for streaming. `head_only` asks for the
    /// metadata alone.
    fn open_stream(
        &self,
        request: StreamOpen,
        head_only: bool,
    ) -> impl Future<Output = Result<StreamMedia, StreamFault>> + Send;
}

/// Every audio format that streams, by extension, with its content type.
const AUDIO_TYPES: [(&str, &str); 7] = [
    ("flac", "audio/flac"),
    ("mp3", "audio/mpeg"),
    ("ogg", "audio/ogg"),
    ("m4a", "audio/mp4"),
    ("aac", "audio/aac"),
    ("wav", "audio/wav"),
    ("opus", "audio/opus"),
];

/// Content type for an audio file extension, e.g. `.flac` or `flac`.
/// Case-insensitive. `None` means the format never streams and the engine
/// answers [`StreamFault::InvalidInput`].
pub fn content_type_for_extension(extension: &str) -> Option<&'static str> {
    let extension = extension.trim_start_matches('.');
    AUDIO_TYPES
        .iter()
        .find(|(known, _)| known.eq_ignore_ascii_case(extension))
        .map(|(_, content_type)| *content_type)
}

/// The extension of a content type from [`content_type_for_extension`].
pub fn extension_for_content_type(content_type: &str) -> Option<&'static str> {
    AUDIO_TYPES
        .iter()
        .find(|(_, known)| *known == content_type)
        .map(|(extension, _)| *extension)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn content_types_cover_streamable_audio_only() {
        for (extension, media_type) in [
            (".flac", "audio/flac"),
            ("mp3", "audio/mpeg"),
            (".ogg", "audio/ogg"),
            (".m4a", "audio/mp4"),
            (".aac", "audio/aac"),
            (".wav", "audio/wav"),
            (".opus", "audio/opus"),
            (".MP3", "audio/mpeg"),
        ] {
            assert_eq!(content_type_for_extension(extension), Some(media_type));
            let bare = extension.trim_start_matches('.').to_lowercase();
            assert_eq!(extension_for_content_type(media_type), Some(bare.as_str()));
        }
        assert_eq!(content_type_for_extension(".WMA"), None);
        assert_eq!(content_type_for_extension(".zip"), None);
        assert_eq!(content_type_for_extension(""), None);
    }
}
