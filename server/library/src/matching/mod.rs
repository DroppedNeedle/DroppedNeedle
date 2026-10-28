//! Release matching: which MusicBrainz release a set of files is.
//!
//! Pure code, no IO: callers fetch candidates (with their tracklists) and
//! fingerprints, and this module scores and decides. Library
//! identification uses it today; importing a finished download will use
//! the same engine with stricter rules.
//!
//! The model is beets' and Lidarr's: pair local tracks with release
//! tracks by optimal assignment ([`assign`]), score each pair and the
//! album with a weighted distance ([`distance`], [`score`]), and turn the
//! scores into a verdict with fixed thresholds, MBID vetoes, and edition
//! preferences ([`decide`], which documents the numbers).

use droppedneedle_providers::musicbrainz::is_valid_mbid;

pub mod assign;
pub mod decide;
pub mod distance;
pub mod model;
pub mod score;
pub mod strings;

pub use decide::{EditionPrefs, ReviewReason, Verdict, decide, should_fingerprint};
pub use distance::{Distance, PenaltyShare};
pub use model::{
    CreditedArtist, EditionHints, Genre, LocalAlbum, LocalTrack, Release, ReleaseMedium,
    ReleaseTrack,
};
pub use score::{ReleaseMatch, Support, match_release, unknown_recordings};

/// The MusicBrainz id a file's tag gives the matcher: trimmed and lowercased
/// when it has the id shape, else none. Tags are free text, so a value that
/// is not an id is never looked up or compared. `dropped` counts the
/// non-blank values refused.
pub fn usable_mbid(raw: Option<&str>, dropped: &mut usize) -> Option<String> {
    let id = raw?.trim().to_ascii_lowercase();
    if id.is_empty() {
        return None;
    }
    if is_valid_mbid(&id) {
        return Some(id);
    }
    *dropped += 1;
    None
}
