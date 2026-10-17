//! What each repair finding means, in words, with what to do about it.
//! Codes are v2's, so reports carried over from v2 read the same.

use crate::operations::reasons::Reason;

const fn reason(code: &'static str, message: &'static str, action: &'static str) -> Reason {
    Reason {
        code,
        message,
        action,
    }
}

// Refusals.
pub const REPAIR_NOT_FOUND: Reason = reason(
    "REPAIR_NOT_FOUND",
    "This identity check does not exist.",
    "Start a new identity check.",
);
pub const CONFIRMATION_REQUIRED: Reason = reason(
    "CONFIRMATION_REQUIRED",
    "Applying a report changes album matches, so it needs your confirmation.",
    "Read the report, then confirm to apply it.",
);
pub const REPORT_CHANGED: Reason = reason(
    "STALE_REVISION",
    "The report changed since you opened it.",
    "Reload the report and try again.",
);
pub const RULES_CHANGED: Reason = reason(
    "IDENTITY_RULES_CHANGED",
    "This report was made with older rules.",
    "Run a fresh identity check, then apply that one.",
);
pub const IDEMPOTENCY_KEY_EMPTY: Reason = reason(
    "INVALID_INPUT",
    "The request is missing its idempotency key.",
    "Send a non-empty idempotency key.",
);
pub const IDEMPOTENCY_KEY_REUSED: Reason = reason(
    "IDEMPOTENCY_KEY_REUSED",
    "That idempotency key already started a different kind of job.",
    "Send a new idempotency key.",
);
pub const PAGE_SIZE_INVALID: Reason = reason(
    "INVALID_INPUT",
    "The page size is out of range.",
    "Ask for between 1 and 50 jobs, or 1 and 200 findings.",
);
pub const CURSOR_INVALID: Reason = reason(
    "INVALID_INPUT",
    "The page cursor is not one this list handed out.",
    "Start again from the first page.",
);
pub const CATEGORY_INVALID: Reason = reason(
    "INVALID_INPUT",
    "That finding category does not exist for this report.",
    "Pick one of the categories the report lists.",
);
pub const RUN_ID_INVALID: Reason = reason(
    "INVALID_INPUT",
    "That is not a scan run ID.",
    "Pick the run from the scan history.",
);
pub const RUN_NOT_FOUND: Reason = reason(
    "SCAN_RUN_NOT_FOUND",
    "This scan run does not exist.",
    "Pick a run from the scan history.",
);
pub const DIAGNOSTICS_TOO_LARGE: Reason = reason(
    "DIAGNOSTICS_TOO_LARGE",
    "The diagnostic report for this run is larger than the 2 MB export limit.",
    "Export a smaller run, or read the failed paths list instead.",
);

/// Why an album got its finding.
pub fn finding(code: &str) -> Reason {
    match code {
        // Identity repair.
        "CURRENT_IDENTITY_PASSES" => reason(
            "CURRENT_IDENTITY_PASSES",
            "The files still fit the release they are linked to.",
            "Nothing to do.",
        ),
        "ZERO_SUPPORT" => reason(
            "ZERO_SUPPORT",
            "Not one file fits the release this album is linked to.",
            "Apply the report to remove the link; the album is then matched again.",
        ),
        "HARD_CONTRADICTION" => reason(
            "HARD_CONTRADICTION",
            "IDs in the files point to a different release than the one this album is linked to.",
            "Apply the report to remove the link; the album is then matched again.",
        ),
        "NON_TERMINAL_SAFETY_CONCERN" => reason(
            "NON_TERMINAL_SAFETY_CONCERN",
            "The files only partly fit the linked release; it may be a different edition.",
            "Open the album and check its edition.",
        ),
        "MANUAL_IDENTITY_REPORT_ONLY" => reason(
            "MANUAL_IDENTITY_REPORT_ONLY",
            "Someone chose this album's edition, so a recheck never removes it.",
            "Change the edition from the album page if it is wrong.",
        ),
        "EVIDENCE_UNAVAILABLE" => reason(
            "EVIDENCE_UNAVAILABLE",
            "MusicBrainz no longer has the release this album is linked to, or the link names no exact release.",
            "Choose its edition from the album page.",
        ),
        // Identity preparation.
        "EXACT_RELEASE_MAPPINGS_VERIFIED" => reason(
            "EXACT_RELEASE_MAPPINGS_VERIFIED",
            "Every file is placed on its track of the album's exact release.",
            "Nothing to do; the album is ready for file management.",
        ),
        "CUSTOM_EDITION_MANIFEST_VERIFIED" => reason(
            "CUSTOM_EDITION_MANIFEST_VERIFIED",
            "The album is kept as your own edition and its files have not changed.",
            "Nothing to do; the album is ready for file management.",
        ),
        "EXACT_RELEASE_MAPPING_SUPPORTED" => reason(
            "EXACT_RELEASE_MAPPING_SUPPORTED",
            "Every file fits a track of the album's release, but not all of them are placed yet.",
            "Apply the report to place them.",
        ),
        "EXACT_EDITION_SUGGESTED" => reason(
            "EXACT_EDITION_SUGGESTED",
            "The album has no exact edition yet, and one fits every file.",
            "Apply the report to use the suggested edition, or pick another from the album page.",
        ),
        "EXACT_EDITION_AUTO_ACCEPTED" => reason(
            "EXACT_EDITION_AUTO_ACCEPTED",
            "Automatic edition acceptance applied this edition: it fits every file and clearly beats every other edition that does.",
            "Nothing to do; undo it here or pick another edition from the album page if it is wrong.",
        ),
        "EXACT_EDITION_NOT_ACCEPTED" => reason(
            "EXACT_EDITION_NOT_ACCEPTED",
            "The album has no exact edition, and no edition fits every file closely enough to suggest.",
            "Pick the edition yourself from the album page.",
        ),
        "SELECTED_RELEASE_UNAVAILABLE" => reason(
            "SELECTED_RELEASE_UNAVAILABLE",
            "MusicBrainz no longer has this album's chosen release.",
            "Pick another edition from the album page.",
        ),
        "SELECTED_RELEASE_CONFLICT" => reason(
            "SELECTED_RELEASE_CONFLICT",
            "MusicBrainz now files this album's release under a different release group.",
            "Open the album and pick its edition again.",
        ),
        "CONFLICTING_TRACK_EVIDENCE" => reason(
            "CONFLICTING_TRACK_EVIDENCE",
            "Some files are already placed on different tracks than the release suggests.",
            "Open the album and check its edition and tracklist.",
        ),
        "EXACT_RELEASE_MAPPING_INCOMPLETE" => reason(
            "EXACT_RELEASE_MAPPING_INCOMPLETE",
            "Some files do not fit any track of the album's release.",
            "Open the album: pick an edition that fits, or keep these files out of file management.",
        ),
        "CUSTOM_MANIFEST_STALE" => reason(
            "CUSTOM_MANIFEST_STALE",
            "The album's files changed after it was kept as your own edition.",
            "Open the album's edition and use \"Keep your own track list\" again.",
        ),
        // Both.
        "LOCAL_METADATA_POLICY" => reason(
            "LOCAL_METADATA_POLICY",
            "This album's folder is set to keep its own tags, so it is not looked up on MusicBrainz.",
            "Change the folder's policy in Settings > Library if you want it checked.",
        ),
        "IDENTITY_CHANGED" => reason(
            "IDENTITY_CHANGED",
            "The album changed or lost its files while it was being checked.",
            "Run a fresh check.",
        ),
        "PROVIDER_DEFERRED" => reason(
            "PROVIDER_DEFERRED",
            "MusicBrainz did not answer, so this album could not be checked.",
            "Run a fresh check once MusicBrainz is back.",
        ),
        "STALE_SUBJECT" => reason(
            "STALE_SUBJECT",
            "The album changed after it was checked, so nothing was changed.",
            "Run a fresh check to see the album as it is now.",
        ),
        _ => reason(
            "UNDESCRIBED_FINDING",
            "This finding has a reason this version does not describe.",
            "Run a fresh check.",
        ),
    }
}
