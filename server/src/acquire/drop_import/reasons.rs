//! Why a drop-import item stopped where it did, in words a person reads.
//!
//! Each item that is not simply imported carries one stable code, one
//! plain sentence and one suggested action. The sentence goes in the
//! item's `detail` (with specifics such as counts appended), the code and
//! action in their own columns.

pub use crate::acquire::landing::reasons::Reason;

const fn reason(code: &'static str, message: &'static str, action: &'static str) -> Reason {
    Reason {
        code,
        message,
        action,
    }
}

/// No audio file in the folder could be read.
pub const UNREADABLE: Reason = reason(
    "unreadable",
    "None of the audio files could be read; they are damaged or not really audio.",
    "Discard this folder, or download the files again from where you bought them and drop them in.",
);
/// More files than one album holds.
pub const TOO_MANY_FILES: Reason = reason(
    "too_many_files",
    "This folder holds more files than one album can.",
    "Split it into one folder per album and drop those in, or pick the album by hand with Match.",
);
/// Nothing identified the album.
pub const NO_ALBUM_MATCH: Reason = reason(
    "no_album_match",
    "DroppedNeedle couldn't work out which album this is.",
    "Pick the album by hand with Match, or discard the folder.",
);
/// The album a person picked has no release the files fit.
pub const ALBUM_NOT_FOUND: Reason = reason(
    "album_not_found",
    "MusicBrainz has no release of the album you picked that these files can be matched to.",
    "Check that you picked the right album, then try Match again.",
);
/// The files' own MusicBrainz tags name an album they do not match.
pub const TAGS_DISAGREE: Reason = reason(
    "tags_disagree",
    "These files are tagged with an album they don't match closely enough to import on their own.",
    "Pick the album by hand with Match, or discard the folder.",
);
/// The files are another edition than the one the library keeps.
pub const NOT_CHOSEN_EDITION: Reason = reason(
    "not_chosen_edition",
    "These files don't match the edition your library uses for this album.",
    "Use Match to import them anyway, or change the album's edition in the library first.",
);
/// MusicBrainz could not be asked.
pub const MUSICBRAINZ_UNAVAILABLE: Reason = reason(
    "musicbrainz_unavailable",
    "MusicBrainz could not be reached, so the album couldn't be identified.",
    "Try Match again in a few minutes.",
);
/// The library could not take files.
pub const LIBRARY_UNAVAILABLE: Reason = reason(
    "library_unavailable",
    "The library couldn't take the files right now.",
    "Check that a library folder is set up and has free space, then try Match again.",
);
/// Files of an imported album that matched none of its tracks well.
pub const LEFTOVER_FILES: Reason = reason(
    "leftover_files",
    "These files came with the album, but their tags name tracks they don't match closely enough.",
    "Pick the right album for them with Match, or discard them.",
);
/// AcoustID heard another recording than the track the file pairs with.
pub const FINGERPRINT_MISMATCH: Reason = reason(
    "fingerprint_mismatch",
    "AcoustID says these files are a different recording than the tracks they are tagged as.",
    "Check what the files are, then pick the right album with Match, or discard them.",
);
/// Files named as samples or previews.
pub const SAMPLES: Reason = reason(
    "samples",
    "These look like sample clips or previews, not full tracks.",
    "Discard them, or use Match if they really are full tracks.",
);
/// An upgrade's old copy could not be put back after the new file failed.
pub const RESTORE_FAILED: Reason = reason(
    "restore_failed",
    "This better copy couldn't be placed, and the old copy couldn't be put back from the recycle bin. The upload is kept so nothing is lost.",
    "Move the old file back from the recycle bin by hand, then use Match on this item again or discard it.",
);
/// Every track is already in the library.
pub const ALREADY_OWNED: Reason = reason(
    "already_owned",
    "Every track in this folder is already in your library.",
    "Nothing to do.",
);
/// Files bound for a place another file already sits.
pub const TARGET_OCCUPIED: Reason = reason(
    "target_occupied",
    "A file already sits where these tracks would go in your library.",
    "If the library file is the same track, discard these. Otherwise move or remove it, then use Match on these again.",
);
/// The folder could not be handled at all.
pub const IMPORT_FAILED: Reason = reason(
    "import_failed",
    "Something went wrong importing this folder.",
    "Try Match again. If it keeps failing, check the server log.",
);
/// A restart cut the import off.
pub const INTERRUPTED: Reason = reason(
    "interrupted",
    "The server restarted before this import finished. The files were kept.",
    "Pick the album with Match, or discard the files.",
);
