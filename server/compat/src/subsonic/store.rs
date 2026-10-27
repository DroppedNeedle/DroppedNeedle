//! The records the Subsonic handlers read from and hand to the
//! [`SubsonicStore`](crate::adapters::subsonic_store::SubsonicStore).
//!
//! v2: `CompatServices` behind the Subsonic router.

/// Album-list sort keys (v2 `_ALBUMLIST_SORTS` + byYear directions).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AlbumSort {
    /// Recently added.
    Recent,
    /// By title.
    Title,
    /// By artist name.
    Artist,
    /// Random order.
    Random,
    /// Year ascending (byYear with fromYear <= toYear).
    YearAsc,
    /// Year descending (byYear with fromYear > toYear).
    YearDesc,
    /// Insertion order: the search3 empty-query sync order, stable while
    /// a scan adds albums.
    Natural,
}

/// One playlist entry: `library_file_id` is None for legacy/outbound
/// entries, which are skipped as not streamable (#181).
#[derive(Debug, Clone, Default)]
pub struct PlaylistEntry {
    /// Entry id.
    pub id: String,
    /// Linked library file id, when streamable.
    pub library_file_id: Option<String>,
}

/// Playlist record facts.
#[derive(Debug, Clone, Default)]
pub struct PlaylistRecord {
    /// Playlist id.
    pub id: String,
    /// Playlist name.
    pub name: String,
    /// Public flag.
    pub is_public: bool,
    /// Created ISO timestamp.
    pub created_at: Option<String>,
    /// Updated ISO timestamp.
    pub changed_at: Option<String>,
}

/// The last scan run that finished, however it ended.
#[derive(Debug, Clone, Default)]
pub struct LastScan {
    /// When it ended, Unix seconds.
    pub finished_at: i64,
    /// Its kind, as the scanner names it.
    pub kind: String,
    /// How long it ran, nanoseconds. None when it ended before it started.
    pub elapsed_nanos: Option<i64>,
}

/// Scan status facts.
#[derive(Debug, Clone, Default)]
pub struct ScanStatusData {
    /// Whether a scan is running.
    pub scanning: bool,
    /// Files looked at so far during a scan; playable tracks at rest.
    pub count: i64,
    /// Folders holding at least one playable track.
    pub folder_count: i64,
    /// The last finished run, or None before the first.
    pub last: Option<LastScan>,
}

/// Playlist list row (None record = redacted private list, skipped).
#[derive(Debug, Clone, Default)]
pub struct PlaylistSummary {
    /// Record, or None when redacted.
    pub record: Option<PlaylistRecord>,
    /// True when the caller owns it.
    pub is_owner: bool,
    /// Owner display name.
    pub owner_name: String,
}

/// Playlist detail: record plus entries in order.
#[derive(Debug, Clone, Default)]
pub struct PlaylistDetail {
    /// Record.
    pub record: PlaylistRecord,
    /// Entries in order.
    pub tracks: Vec<PlaylistEntry>,
    /// True when the caller owns it.
    pub is_owner: bool,
    /// Owner display name.
    pub owner_name: String,
}

/// Saved play queue.
#[derive(Debug, Clone, Default)]
pub struct QueueState {
    /// File ids in order.
    pub file_ids: Vec<String>,
    /// Current index into `file_ids`.
    pub current_index: Option<usize>,
    /// Position ms.
    pub position_ms: i64,
    /// Updated unix seconds.
    pub updated_at: f64,
    /// Client that last changed it.
    pub changed_by_client: String,
}

/// Bookmark record.
#[derive(Debug, Clone, Default)]
pub struct BookmarkRecord {
    /// File id.
    pub file_id: String,
    /// Position ms.
    pub position_ms: i64,
    /// Comment.
    pub comment: String,
    /// Created unix seconds.
    pub created_at: i64,
    /// Changed unix seconds.
    pub changed_at: i64,
}

/// Now-playing presence row.
#[derive(Debug, Clone, Default)]
pub struct NowPlayingRow {
    /// Listener display name.
    pub user_name: String,
    /// File id.
    pub file_id: String,
    /// Last update unix seconds.
    pub updated_at: f64,
    /// Source label (preferred for playerName).
    pub source: Option<String>,
    /// Device name (fallback for playerName).
    pub device_name: Option<String>,
}

/// One lyric line.
#[derive(Debug, Clone, Default)]
pub struct LyricLine {
    /// Line text.
    pub value: String,
    /// Start ms (synced only).
    pub start_ms: Option<i64>,
}

/// Stored lyrics for a track.
#[derive(Debug, Clone, Default)]
pub struct LyricsData {
    /// Language code.
    pub language: String,
    /// Whether lines carry timestamps.
    pub synced: bool,
    /// Lines.
    pub lines: Vec<LyricLine>,
}

/// Stream facts inside an advanced transcode decision.
#[derive(Debug, Clone, Default)]
pub struct StreamDetailsData {
    /// Protocol.
    pub protocol: String,
    /// Container.
    pub container: String,
    /// Codec.
    pub codec: String,
    /// Audio channels.
    pub audio_channels: Option<i64>,
    /// Audio bitrate.
    pub audio_bitrate: Option<i64>,
    /// Sample rate.
    pub audio_samplerate: Option<i64>,
    /// Bit depth.
    pub audio_bitdepth: Option<i64>,
}

/// Advanced transcode decision (signed params minted inside).
#[derive(Debug, Clone, Default)]
pub struct TranscodeDecisionData {
    /// Direct play possible.
    pub can_direct_play: bool,
    /// Transcode possible.
    pub can_transcode: bool,
    /// Why a transcode is needed.
    pub transcode_reason: Vec<String>,
    /// Why nothing is possible.
    pub error_reason: Option<String>,
    /// Signed params for getTranscodeStream.
    pub transcode_params: Option<String>,
    /// Source stream facts.
    pub source_stream: Option<StreamDetailsData>,
    /// Target stream facts.
    pub transcode_stream: Option<StreamDetailsData>,
}

/// Client playback capabilities (getTranscodeDecision JSON body).
/// Parsed, validated and passed to [`SubsonicStore::advanced_decide`](crate::adapters::subsonic_store::SubsonicStore::advanced_decide).
#[derive(Debug, Clone, Default)]
pub struct ClientInfo {
    /// Client name.
    pub name: String,
    /// Client platform.
    pub platform: String,
    /// Max audio bitrate (0 = unset, Feishin #464/#468).
    pub max_audio_bitrate: Option<i64>,
    /// Max transcoding audio bitrate (0 = unset).
    pub max_transcoding_audio_bitrate: Option<i64>,
    /// Formats the client plays as-is.
    pub direct_play_profiles: Vec<super::media::client_info::DirectPlayProfile>,
    /// Formats the client accepts from the transcoder.
    pub transcoding_profiles: Vec<super::media::client_info::TranscodingProfile>,
    /// Per-codec limits.
    pub codec_profiles: Vec<super::media::client_info::CodecProfile>,
}
