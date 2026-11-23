//! The job that finds the album of each song a playlist file import kept.
//!
//! The import queues one [`IDENTIFY`] job keyed by its playlist in the
//! write that creates it, when it kept a song that names an album. The
//! handler lives with the album proof, in acquire's imports: it reads the
//! playlist's songs that still have no source, no file and no album,
//! proves each album and writes what it found back onto the songs, so the
//! link pass points them at the library once the album lands.

use std::time::Duration;

use droppedneedle_kernel::jobs::queue::{JobKind, KindSpec};
use serde::{Deserialize, Serialize};

/// The identify job kind.
pub const IDENTIFY: JobKind = JobKind::new("playlist.identify");

/// How identify jobs run: one search a song at background priority, so a
/// long playlist takes a while. A MusicBrainz outage is waited out, never
/// read as nothing found: tries back off from a minute, doubling to six
/// hours, and 20 of them span about three days (8.5 hours to reach the
/// cap, then eleven six-hour waits). Each try reads only the songs still
/// without an album, so repeating it is safe.
pub const SPEC: KindSpec = KindSpec::new(IDENTIFY)
    .max_attempts(20)
    .backoff(Duration::from_secs(60), Duration::from_secs(6 * 3600))
    .run_timeout(Duration::from_secs(3600));

/// What one identify job carries.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IdentifyJob {
    /// The playlist whose kept songs it looks at.
    pub playlist_id: String,
}

/// The job key for one playlist.
pub fn identify_key(playlist_id: &str) -> String {
    format!("playlist:identify:{playlist_id}")
}
