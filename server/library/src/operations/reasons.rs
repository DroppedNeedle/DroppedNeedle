//! Why an operation request was refused, why a job ended where it did, or
//! a note on a planned file: one stable code, one plain sentence, and what
//! to do about it.
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
    "Change the edition from the album page instead.",
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
pub const LOCAL_METADATA_MANAGED: Reason = reason(
    "LOCAL_METADATA_MANAGED",
    "This folder keeps its own tags and DroppedNeedle manages it, so the album can't be identified again.",
    "Choose its edition on the album page if you want it changed.",
);
pub const EDITION_IS_CHOSEN: Reason = reason(
    "EDITION_IS_CHOSEN",
    "Someone chose this album's edition, so identifying it again would change nothing.",
    "Use \"Let DroppedNeedle choose\" on the album page to have it matched again.",
);
pub const CONTROL_KEY_REUSED: Reason = reason(
    "CONTROL_KEY_REUSED",
    "This idempotency key was already used for a different request.",
    "Send the request again with a new key.",
);

// Stale: the album or job moved since the caller read it.
pub const OPERATION_CHANGED: Reason = reason(
    STALE,
    "The operation changed since you last loaded it.",
    "Reload the operation and try again.",
);
pub const UNDO_STALE: Reason = reason(
    STALE,
    "The album's match changed after the automatic choice, so undoing it would bring back an outdated match.",
    "Reload the album and change its edition there.",
);

// The exact release does not fit.

// Choosing an edition.
pub const EDITION_NOT_FOUND: Reason = reason(
    "EDITION_NOT_FOUND",
    "MusicBrainz does not know this release.",
    "Check the release ID, or pick an edition from the list.",
);
pub const EDITION_UNREADABLE: Reason = reason(
    "EDITION_UNREADABLE",
    "MusicBrainz's answer for this release could not be read.",
    "Try again later, or pick another edition.",
);
pub const EDITION_FITS_NO_FILE: Reason = reason(
    "EDITION_FITS_NO_FILE",
    "None of this album's files match a track on that edition, so it is probably another album.",
    "Pick an edition that holds these songs, or search MusicBrainz for the right album.",
);
pub const NO_EDITION_TO_PLAN: Reason = reason(
    "NO_EDITION_TO_PLAN",
    "This album has no edition yet, so there is nothing to compare its files with.",
    "Pick an edition from the list first.",
);
pub const FILE_KEEPS_ITS_PLACE: Reason = reason(
    "FILE_KEEPS_ITS_PLACE",
    "One of the picked files sits on a track of the album's edition, so it is not an extra.",
    "Reload the plan and pick only files marked as a second copy or not on the edition.",
);
pub const NOTHING_PICKED: Reason = reason(
    "NOTHING_PICKED",
    "No files were picked to remove.",
    "Tick the extra files you want moved to the recycle bin, then try again.",
);
pub const NO_EDITION_CHOICE: Reason = reason(
    "NO_EDITION_CHOICE",
    "There is no edition choice on this album to take back.",
    "Nothing to do; pick an edition if you want to change it.",
);
pub const EDITION_CHOICE_STALE: Reason = reason(
    STALE,
    "The album's edition changed after your choice, so undoing it would bring back an outdated match.",
    "Reload the album and pick the edition you want.",
);
pub const EDITION_CHANGED: Reason = reason(
    STALE,
    "The album's edition or files changed since you opened it.",
    "Reload the album and try again.",
);
pub const NOTHING_TO_CONFIRM: Reason = reason(
    "NOTHING_TO_CONFIRM",
    "This album has no match waiting for confirmation.",
    "Nothing to do; pick another edition if this one is wrong.",
);
pub const NO_MATCH_TO_CONFIRM: Reason = reason(
    "NO_MATCH_TO_CONFIRM",
    "This album has no MusicBrainz match yet, so there is nothing to confirm.",
    "Pick an edition for it instead.",
);

// Matching one file to an edition track by hand.
pub const FILE_NOT_ON_ALBUM: Reason = reason(
    "FILE_NOT_ON_ALBUM",
    "That file is not one of this album's files.",
    "Reload the album and try again.",
);
pub const TRACK_NOT_ON_EDITION: Reason = reason(
    "TRACK_NOT_ON_EDITION",
    "That track is not on this album's edition.",
    "Reload the album and pick one of its tracks.",
);
pub const FILE_ALREADY_PLACED: Reason = reason(
    "FILE_ALREADY_PLACED",
    "This file is already on a track.",
    "To move a file you matched, undo its match first.",
);
pub const EDITION_TRACK_TAKEN: Reason = reason(
    "EDITION_TRACK_TAKEN",
    "Another file is on that track now.",
    "Pick another track.",
);
pub const CUSTOM_EDITION_KEEPS_FILES: Reason = reason(
    "CUSTOM_EDITION_KEEPS_FILES",
    "This album is a custom edition: its files are kept as they are, not matched to a MusicBrainz edition.",
    "Choose a MusicBrainz edition first.",
);
pub const NO_HAND_MATCH: Reason = reason(
    "NO_HAND_MATCH",
    "This file has no match of yours to undo.",
    "Nothing to do.",
);
pub const HAND_MATCH_STALE: Reason = reason(
    STALE,
    "This match was changed after you made it, so it can't be undone.",
    "Nothing to do.",
);
pub const HAND_MATCH_WRITTEN: Reason = reason(
    "HAND_MATCH_WRITTEN",
    "DroppedNeedle has written this file's tags since the match.",
    "Undo that write in History first.",
);

// Undoing a fold of a leftover album.
pub const FOLD_NOT_FOUND: Reason = reason(
    "FOLD_NOT_FOUND",
    "This join is not one DroppedNeedle can undo.",
    "Reload the album.",
);
pub const FOLD_STALE: Reason = reason(
    STALE,
    "The albums changed after the join, so it can't be undone.",
    "Reload the album.",
);

// Undoing the artist an album on "Unknown Artist" took from its tracks.
pub const ARTIST_REPAIR_NOT_FOUND: Reason = reason(
    "ARTIST_REPAIR_NOT_FOUND",
    "This artist change is not one DroppedNeedle can undo.",
    "Reload the album.",
);
pub const ARTIST_REPAIR_STALE: Reason = reason(
    STALE,
    "The album's artist changed after this, so it can't be undone.",
    "Reload the album.",
);

// Notes on one planned file.
pub const TRACK_ID_CORRECTED: Reason = reason(
    "TRACK_ID_CORRECTED",
    "This file's track ID belongs to another edition. It is matched by its recording instead, and the right ID is written when DroppedNeedle writes this album's MusicBrainz tags.",
    "Nothing to do.",
);
pub const TRACK_ID_ELSEWHERE: Reason = reason(
    "TRACK_ID_ELSEWHERE",
    "This file names this edition, but its track ID belongs to another edition and it has no recording ID to match it by.",
    "Match it to its track, fix its MusicBrainz tags and rescan, or keep it as an extra.",
);

// A custom edition cannot be sealed.
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
pub const REVIEW_NOT_FOUND: Reason = reason(
    "REVIEW_NOT_FOUND",
    "This review is not in the list any more.",
    "Reload the list; the album may have been removed or merged.",
);
pub const REVIEW_SETTLED: Reason = reason(
    "REVIEW_SETTLED",
    "Someone already settled this review.",
    "Reload the list to see what was decided.",
);
pub const REVIEW_NOT_EXCLUDED: Reason = reason(
    "REVIEW_NOT_EXCLUDED",
    "This album's files were not taken out from its review, so there is nothing to put back.",
    "Reload the list to see the review's current state.",
);
pub const REVIEW_SELECTION_CHANGED: Reason = reason(
    "REVIEW_SELECTION_CHANGED",
    "The reviews you selected changed after the preview.",
    "Preview again, then apply.",
);
pub const REVIEW_NO_CANDIDATE: Reason = reason(
    "REVIEW_NO_CANDIDATE",
    "This review has no release to use.",
    "Open the album and pick an edition, or keep it as tagged.",
);
pub const REVIEW_CANDIDATE_MISSING: Reason = reason(
    "REVIEW_CANDIDATE_MISSING",
    "This review does not list the release you chose.",
    "Leave the release empty to use each review's best match, or select fewer reviews.",
);
pub const REVIEW_ACTION_FAILED: Reason = reason(
    "REVIEW_ACTION_FAILED",
    "The change could not be saved for this review.",
    "Try it again from the review; if it keeps failing, check the server log.",
);
pub const REVIEW_ALBUM_PINNED: Reason = reason(
    "REVIEW_ALBUM_PINNED",
    "Someone picked this album's edition by hand, so a bulk accept leaves it alone.",
    "Open the review and choose a release yourself if you want to replace that pick.",
);
pub const REVIEW_MERGE_NEEDS_CHOICE: Reason = reason(
    "REVIEW_MERGE_NEEDS_CHOICE",
    "This review lists editions dropped when albums were combined; none is the obvious one to take back.",
    "Open the review and pick the edition you want, or keep the album as it is.",
);
pub const REVIEW_FILTER_INVALID: Reason = reason(
    "REVIEW_FILTER_INVALID",
    "One of the review filters has a value this version does not know.",
    "Reload the page and pick the filter again.",
);
pub const REVIEW_BULK_ACTION_INVALID: Reason = reason(
    "REVIEW_BULK_ACTION_INVALID",
    "That is not something a bulk review change can do.",
    "Use keep_tagged, exclude or accept.",
);
pub const REVIEW_STATE_INVALID: Reason = reason(
    "REVIEW_STATE_INVALID",
    "That is not a review state.",
    "Use pending, approved, rejected, dismissed, excluded or all.",
);

/// Why a review was opened, by the code it was filed with. Edition
/// corrections and undo file their own codes; the rest are match codes.
pub fn review_reason(code: &str) -> Reason {
    match code {
        "EXCLUDED" => reason(
            "EXCLUDED",
            "This album was taken out of the library in an earlier version.",
            "Put it back if you want it in the library again.",
        ),
        "KEEP_TAGGED" => reason(
            "KEEP_TAGGED",
            "This album was kept as tagged in an earlier version.",
            "Nothing to do.",
        ),
        "AUTOMATIC_EDITION_CLEARED_TO_REVIEW" => reason(
            "AUTOMATIC_EDITION_CLEARED_TO_REVIEW",
            "The automatic edition was undone and there was no earlier match to go back to.",
            "Pick one of the candidates, keep the album as tagged, or retry identification.",
        ),
        "EDITION_CLEARED_CONFLICT" | "EDITION_CLEARED_AMBIGUOUS" => reason(
            "EDITION_CLEARED",
            "Albums with different editions were combined, so the edition was cleared.",
            "Pick the edition you want from the candidates, or keep the album as tagged.",
        ),
        "EDITION_KEPT_OVER_CONFLICT" | "EDITION_CHOSEN_WINS" => reason(
            "EDITION_DROPPED_BY_MERGE",
            "Albums were combined and their other editions were dropped; they are listed here.",
            "Nothing to do, or pick one of them to use it instead.",
        ),
        other => match_reason(other),
    }
}

/// The retry code a held choice keeps while MusicBrainz is not answering.
pub const MUSICBRAINZ_UNAVAILABLE: &str = "MUSICBRAINZ_UNAVAILABLE";

/// Why a job ended where it did, by its stored terminal code.
pub fn terminal(code: &str) -> Reason {
    match code {
        "HANDED_TO_IDENTIFICATION" => reason(
            "HANDED_TO_IDENTIFICATION",
            "The album was queued for identification.",
            "Nothing to do.",
        ),
        "ALBUM_NOT_FOUND" => ALBUM_NOT_FOUND,
        "ALBUM_EXCLUDED" => ALBUM_EXCLUDED,
        "LOCAL_METADATA_NEEDS_CONFIRMATION" => LOCAL_METADATA_NEEDS_CONFIRMATION,
        "LOCAL_METADATA_MANAGED" => LOCAL_METADATA_MANAGED,
        "EDITION_IS_CHOSEN" => EDITION_IS_CHOSEN,
        "IDENTIFIED" => reason(
            "IDENTIFIED",
            "The album is matched to the release you chose.",
            "Change it from the album page if it is wrong.",
        ),
        "CUSTOM_EDITION_SEALED" => reason(
            "CUSTOM_EDITION_SEALED",
            "The album is kept as your own edition of the chosen release group.",
            "Change it from the album page if it is wrong.",
        ),
        "LEFT_UNMANAGED" => reason(
            "LEFT_UNMANAGED",
            "The album is kept out of file organizing.",
            "Use \"Manage it again\" on the album page when you want it back.",
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
            "Open the album to check its edition.",
        ),
        "SUBJECT_NOT_AVAILABLE" => reason(
            "SUBJECT_NOT_AVAILABLE",
            "The album has no files in the library any more.",
            "Rescan the library if the files should still be there.",
        ),
        "MISSING_WORK" | "MISSING_SNAPSHOT" => reason(
            "OPERATION_INCOMPLETE",
            "This operation lost the record of what it was checking.",
            "Start it again.",
        ),
        "PROVIDER_TEMPORARILY_UNAVAILABLE" => reason(
            "PROVIDER_TEMPORARILY_UNAVAILABLE",
            "MusicBrainz did not answer after several tries.",
            "Resume the operation once MusicBrainz is back.",
        ),
        "MANAGEMENT_APPLIED" => reason(
            "MANAGEMENT_APPLIED",
            "Every album in this run was written.",
            "Nothing to do; preview an undo if you want the old state back.",
        ),
        "MANAGEMENT_PARTIAL" => reason(
            "MANAGEMENT_PARTIAL",
            "Some albums in this run could not be written; the others were.",
            "Open the results to see each file's reason, fix it, then make a new preview.",
        ),
        "MANAGEMENT_FAILED" => reason(
            "MANAGEMENT_FAILED",
            "This run stopped on an unexpected problem.",
            "Make a new preview. If it happens again, the server log has the details.",
        ),
        "MANAGEMENT_UNCHANGED" => reason(
            "MANAGEMENT_UNCHANGED",
            "Every file already looked the way the profile wants; nothing was written.",
            "Nothing to do.",
        ),
        "TAG_EDIT_SAVED" => reason(
            "TAG_EDIT_SAVED",
            "The files already held these values, so only the kept value changed.",
            "Nothing to do.",
        ),
        "DUPLICATE_RECYCLED" => reason(
            "DUPLICATE_RECYCLED",
            "The file was moved to the recycle bin; the file in its way stays.",
            "Nothing to do; move it back from the recycle bin if you change your mind.",
        ),
        "DUPLICATE_KEPT" => reason(
            "DUPLICATE_KEPT",
            "Both files were kept where they are; nothing was written.",
            "Nothing to do; the clash stays settled until you make a new preview.",
        ),
        "DISCARDED" => reason(
            "DISCARDED",
            "This preview was discarded; no file was changed.",
            "Make a new preview when you want to run it.",
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
        "DRY_RUN_READY" => reason(
            "DRY_RUN_READY",
            "The check is done and its report is ready. Nothing has changed yet.",
            "Read the findings, then apply the safe changes or leave them.",
        ),
        "APPLY_COMPLETED" => reason(
            "APPLY_COMPLETED",
            "The safe changes from this report are applied.",
            "Nothing to do; each album's change can be undone from its page.",
        ),
        "IDENTITY_PREPARATION_DISCARDED" => reason(
            "IDENTITY_PREPARATION_DISCARDED",
            "This identity report was dismissed without changing anything.",
            "Run a fresh identity check when you want a new report.",
        ),
        "CATALOG_IDENTITY_HYGIENE_COMPLETED" => reason(
            "CATALOG_IDENTITY_HYGIENE_COMPLETED",
            "The catalog cleanup finished.",
            "Nothing to do.",
        ),
        "PLAY_HISTORY_FILED" => reason(
            "PLAY_HISTORY_FILED",
            "Plays that named no song were matched to the songs in the library.",
            "Nothing to do; a play that fits no single song stays in history by name.",
        ),
        "PROVIDER_DEFERRED" => reason(
            "PROVIDER_DEFERRED",
            "MusicBrainz did not answer, so this album waits for another try.",
            "Nothing to do; it is retried in a couple of minutes.",
        ),
        "STALE_SUBJECT" => reason(
            "STALE_SUBJECT",
            "The album changed after it was checked, so the change was skipped.",
            "Run a fresh check to see the album as it is now.",
        ),
        _ => reason(
            "UNDESCRIBED_ENDING",
            "This operation ended in a way this version does not describe.",
            "Start the operation again if you still need it.",
        ),
    }
}

/// Why an album's edition is what it is, by the reason code
/// identification stored. Every answer says what to do next.
pub fn match_reason(code: &str) -> Reason {
    match code {
        "CHOSEN" => reason(
            "CHOSEN",
            "Someone chose this edition, so automatic matching leaves it alone.",
            "Pick another edition, or let DroppedNeedle choose again.",
        ),
        "CHOSEN_EDITION_FITS_NO_FILE" => reason(
            "CHOSEN_EDITION_FITS_NO_FILE",
            "The edition you chose doesn't match these files.",
            "Pick another edition, or let DroppedNeedle choose.",
        ),
        "CHOSEN_EDITION_NOT_LISTED" => reason(
            "CHOSEN_EDITION_NOT_LISTED",
            "MusicBrainz no longer lists the edition you chose, so these files cannot be placed on it.",
            "Pick another edition, or let DroppedNeedle choose.",
        ),
        "SUPPORTED" => reason(
            "SUPPORTED",
            "This edition fits the files closely: titles, track order and lengths agree.",
            "Nothing to do; pick another edition if it is wrong.",
        ),
        "EDITION_NOT_CHOSEN" => reason(
            "EDITION_NOT_CHOSEN",
            "Edition not chosen yet: the album is matched, but not which release your files are.",
            "Pick an edition if you know which one your files are.",
        ),
        "EDITION_UNCERTAIN" => reason(
            "EDITION_UNCERTAIN",
            "The album is clear, but this is a live or compilation release, so the exact edition is a best guess.",
            "Check the tracklist; confirm it or pick another edition.",
        ),
        "AMBIGUOUS_CANDIDATES" => reason(
            "AMBIGUOUS_CANDIDATES",
            "Two different albums fit the files about equally well; this one fit slightly better.",
            "Compare the candidates and confirm the right one.",
        ),
        "TAGGED_RELEASE_UNREADABLE" => reason(
            "TAGGED_RELEASE_UNREADABLE",
            "MusicBrainz's answer for the release your files name could not be read, so this edition is a best guess.",
            "Confirm it or pick another edition.",
        ),
        "NAMED_GROUP_DIFFERS" => reason(
            "NAMED_GROUP_DIFFERS",
            "Fits best, but the files' tags or earlier match name another album.",
            "Confirm it or pick another edition.",
        ),
        "AUDIO_DISAGREES" => reason(
            "AUDIO_DISAGREES",
            "Fits best, but some files' audio matches other tracks.",
            "Check the tracklist and confirm it.",
        ),
        "WEAK_MATCH" => reason(
            "WEAK_MATCH",
            "This edition is the closest fit, but some titles, lengths or track counts differ.",
            "Check the tracklist; confirm it or pick another edition.",
        ),
        "INSUFFICIENT_EVIDENCE" => reason(
            "INSUFFICIENT_EVIDENCE",
            "Only one release came close and the files carry too little to be sure.",
            "Check the tracklist; confirm it or pick another edition.",
        ),
        "CONFLICTING_TRACK_EVIDENCE" => reason(
            "CONFLICTING_TRACK_EVIDENCE",
            "The IDs in the files point away from every release MusicBrainz offered, so the album keeps its own tags.",
            "Pick the right edition yourself, or fix the tags and rescan.",
        ),
        "NO_CANDIDATE" => reason(
            "NO_CANDIDATE",
            "MusicBrainz has no release that looks like this album, so it keeps its own tags.",
            "Search MusicBrainz for the release, or fix the album title and artist tags and rescan.",
        ),
        "IDENTITY_FAILED_RECHECK" => reason(
            "IDENTITY_FAILED_RECHECK",
            "An identity recheck found these files do not fit the release they were linked to, so the link was removed.",
            "Pick the right edition yourself, or undo the change from this album.",
        ),
        "DETACHED_BY_PERSON" => reason(
            "DETACHED_BY_PERSON",
            "Someone took this album's MusicBrainz match off from its review, so it keeps its own tags.",
            "Pick an edition yourself when you're ready, or leave it as it is.",
        ),
        "UNIDENTIFIED" => reason(
            "UNIDENTIFIED",
            "This album has not been matched to MusicBrainz yet.",
            "Wait for identification to run, or pick an edition yourself.",
        ),
        "FOLDER_KEEPS_TAGS" => reason(
            "FOLDER_KEEPS_TAGS",
            "This folder keeps its own tags, so DroppedNeedle doesn't match its albums by itself.",
            "Pick an edition yourself if you want one.",
        ),
        other => reason(
            "UNDESCRIBED_MATCH",
            if other.is_empty() {
                "This album's match has no recorded reason."
            } else {
                "This album's match has a reason this version does not describe."
            },
            "Check the tracklist; confirm it or pick another edition.",
        ),
    }
}
