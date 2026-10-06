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

pub mod assign;
pub mod decide;
pub mod distance;
pub mod model;
pub mod score;
pub mod strings;

pub use decide::{EditionPrefs, ReviewReason, Verdict, decide, should_fingerprint};
pub use distance::{Distance, PenaltyShare};
pub use model::{CreditedArtist, LocalAlbum, LocalTrack, Release, ReleaseMedium, ReleaseTrack};
pub use score::{ReleaseMatch, Support, match_release};
