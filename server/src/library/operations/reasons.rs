//! Why an operation request was refused, or why a job ended where it did:
//! one stable code, one plain sentence, and what to do about it.
//!
//! Refusals travel in [`super::models::OperationError`]; job endings are
//! looked up from the stored `terminal_code` with [`terminal`]. Several
//! refusals of one kind share a code (every "it changed under you" refusal
//! is `STALE_REVISION`) and differ only in the sentence.

/// A refusal or ending, as the user reads it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Reason {
    pub code: &'static str,
    pub message: &'static str,
    pub action: &'static str,
}

const fn reason(code: &'static str, message: &'static str, action: &'static str) -> Reason {
    Reason {
        code,
        message,
        action,
    }
}

const STALE: &str = "STALE_REVISION";
const NOT_SEALABLE: &str = "CUSTOM_EDITION_NOT_SEALABLE";

// Not found.
pub const OPERATION_NOT_FOUND: Reason = reason(
    "OPERATION_NOT_FOUND",
    "This library operation does not exist.",
    "Start it again from the album page.",
);
pub const ALBUM_NOT_FOUND: Reason = reason(
    "ALBUM_NOT_FOUND",
    "This album is not in the library, or none of its files are left.",
    "Rescan the library, then open the album again.",
);
pub const NO_AUTOMATIC_EDITION: Reason = reason(
    "NO_AUTOMATIC_EDITION",
    "This album has no automatic edition choice to undo.",
    "Use Re-identify to change the album's match instead.",
);

// Bad input.
pub const RELEASE_MBID_INVALID: Reason = reason(
    "RELEASE_MBID_INVALID",
    "That is not a valid MusicBrainz release ID.",
    "Copy the ID from the release's MusicBrainz page and try again.",
);
pub const RELEASE_GROUP_MBID_INVALID: Reason = reason(
    "RELEASE_GROUP_MBID_INVALID",
    "That is not a valid MusicBrainz release group ID.",
    "Copy the ID from the release group's MusicBrainz page and try again.",
);
pub const SEARCH_NEEDS_TITLE: Reason = reason(
    "SEARCH_NEEDS_TITLE",
    "Enter a release title to search for, or pick a release group to list.",
    "Type the album title and search again.",
);
pub const SEARCH_TOO_LONG: Reason = reason(
    "SEARCH_TOO_LONG",
    "Titles and artists can be at most 250 characters, and a page holds 1 to 12 releases.",
    "Shorten the search and try again.",
);
pub const CONTROL_KEY_EMPTY: Reason = reason(
    "CONTROL_KEY_EMPTY",
    "The request carried an empty idempotency key.",
    "Send a key with some text in it, or leave it out.",
);
pub const CONFIRMATION_REQUIRED: Reason = reason(
    "CONFIRMATION_REQUIRED",
    "The evidence does not fully support this release, so choosing it needs your confirmation.",
    "Review the evidence, then confirm your choice.",
);

// Conflicts with the album or job as it is.
pub const ALBUM_EXCLUDED: Reason = reason(
    "ALBUM_EXCLUDED",
    "This album sits under an Excluded library policy, so it is never identified.",
    "Change the folder's policy in Settings > Library, rescan, then try again.",
);
pub const LOCAL_METADATA_NEEDS_CONFIRMATION: Reason = reason(
    "LOCAL_METADATA_NEEDS_CONFIRMATION",
    "This album uses the Local metadata policy, so looking it up on MusicBrainz needs your confirmation.",
    "Confirm the one-off lookup and start again.",
);
pub const CONTROL_KEY_REUSED: Reason = reason(
    "CONTROL_KEY_REUSED",
    "This idempotency key was already used for a different request.",
    "Send the request again with a new key.",
);

// Stale: the album or job moved since the caller read it.
pub const ALBUM_CHANGED: Reason = reason(
    STALE,
    "The album changed before re-identification started.",
    "Reload the album and start again.",
);
pub const ALBUM_FILES_CHANGED: Reason = reason(
    STALE,
    "The album's files changed before re-identification started.",
    "Reload the album and start again.",
);
pub const OPERATION_CHANGED: Reason = reason(
    STALE,
    "The operation changed since you last loaded it.",
    "Reload the operation and try again.",
);
pub const CANDIDATES_CHANGED: Reason = reason(
    STALE,
    "The candidates changed before your choice reached the server.",
    "Reload the candidates and choose again.",
);
pub const ALBUM_CHANGED_SINCE_EVALUATION: Reason = reason(
    STALE,
    "The album changed after its candidates were found.",
    "Run Re-identify again to get fresh candidates.",
);
pub const CANDIDATE_GONE: Reason = reason(
    STALE,
    "The release you chose is no longer among the candidates.",
    "Reload the candidates and choose again.",
);
pub const UNDO_STALE: Reason = reason(
    STALE,
    "The album's match changed after the automatic choice, so undoing it would bring back an outdated match.",
    "Reload the album; use Re-identify to change the match.",
);

// The exact release does not fit.
pub const EXACT_RELEASE_MAPPING_INCOMPLETE: Reason = reason(
    "EXACT_RELEASE_MAPPING_INCOMPLETE",
    "This MusicBrainz release does not fit every file on the album one to one.",
    "Choose Custom edition, get the missing tracks, or leave the album unmanaged.",
);

// A custom edition cannot be sealed.
pub const CUSTOM_EDITION_NEEDS_CONFIRMATION: Reason = reason(
    NOT_SEALABLE,
    "Creating a custom edition needs your confirmation.",
    "Confirm the custom edition and try again.",
);
pub const CUSTOM_EDITION_NAMES_CONFLICT: Reason = reason(
    NOT_SEALABLE,
    "The album's title or artist conflicts with the chosen release group.",
    "Pick a candidate whose title and artist match your files, or fix the tags and rescan.",
);
pub const CUSTOM_EDITION_GROUP_CONFLICT: Reason = reason(
    NOT_SEALABLE,
    "The chosen release group differs from the album's current match.",
    "Choose a release from the matched release group, or leave the album unmanaged.",
);
pub const CUSTOM_EDITION_NAMES_MISSING: Reason = reason(
    NOT_SEALABLE,
    "The album needs a title and an album artist before it can be sealed.",
    "Tag the album's title and album artist, rescan, then try again.",
);
pub const CUSTOM_EDITION_ARTIST_CONFLICT: Reason = reason(
    NOT_SEALABLE,
    "The chosen album artist differs from the artist the library already matched.",
    "Pick a candidate by the matched artist, or fix the artist match first.",
);
pub const CUSTOM_EDITION_POSITIONS: Reason = reason(
    NOT_SEALABLE,
    "Every file needs its own disc and track number before the album can be sealed.",
    "Fix the disc and track numbers in the tags, rescan, then try again.",
);

// Provider.
pub const MUSICBRAINZ_UNAVAILABLE: Reason = reason(
    "MUSICBRAINZ_UNAVAILABLE",
    "MusicBrainz is not answering right now.",
    "Try again in a few minutes.",
);

/// Why a job ended where it did, by its stored terminal code.
pub fn terminal(code: &str) -> Reason {
    match code {
        "CANDIDATES_READY" => reason(
            "CANDIDATES_READY",
            "Candidates are ready for your review.",
            "Choose a release, or keep the album as it is.",
        ),
        "IDENTIFIED" => reason(
            "IDENTIFIED",
            "The album is matched to the release you chose.",
            "Nothing to do; Re-identify again if you change your mind.",
        ),
        "CUSTOM_EDITION_SEALED" => reason(
            "CUSTOM_EDITION_SEALED",
            "The album is kept as your own edition of the chosen release group.",
            "Nothing to do; Re-identify again if you change your mind.",
        ),
        "LEFT_UNMANAGED" => reason(
            "LEFT_UNMANAGED",
            "The album is kept out of file organizing.",
            "Re-enable organizing from the album page when you want it back.",
        ),
        "NO_CANDIDATE" => reason(
            "NO_CANDIDATE",
            "MusicBrainz has no release that fits these files.",
            "Search MusicBrainz for the release yourself, or keep the album as it is.",
        ),
        "INSUFFICIENT_EVIDENCE" => reason(
            "INSUFFICIENT_EVIDENCE",
            "Nothing on MusicBrainz fits these files closely enough to suggest.",
            "Search MusicBrainz for the release yourself, or fix the tags and rescan.",
        ),
        "STALE_INPUT" => reason(
            "STALE_INPUT",
            "The album's files or match changed while it was being checked.",
            "Run Re-identify again.",
        ),
        "SUBJECT_NOT_AVAILABLE" => reason(
            "SUBJECT_NOT_AVAILABLE",
            "The album has no files in the library any more.",
            "Rescan the library; re-identify the album if it comes back.",
        ),
        "MISSING_WORK" | "MISSING_SNAPSHOT" => reason(
            "OPERATION_INCOMPLETE",
            "This operation lost the record of what it was checking.",
            "Start Re-identify again.",
        ),
        "PROVIDER_TEMPORARILY_UNAVAILABLE" => reason(
            "PROVIDER_TEMPORARILY_UNAVAILABLE",
            "MusicBrainz did not answer after several tries.",
            "Resume the operation once MusicBrainz is back.",
        ),
        "STOPPED" => reason(
            "STOPPED",
            "This operation was stopped.",
            "Resume it to start over.",
        ),
        "PAUSED" => reason(
            "PAUSED",
            "This operation is paused.",
            "Resume it to continue.",
        ),
        _ => reason(
            "UNDESCRIBED_ENDING",
            "This operation ended in a way this version does not describe.",
            "Start the operation again if you still need it.",
        ),
    }
}
