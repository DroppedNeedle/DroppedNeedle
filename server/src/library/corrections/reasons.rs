//! Why a catalog correction was refused, or what it did to an album's
//! edition: one stable code, one plain sentence, and what to do next.

use crate::library::operations::reasons::Reason;

const fn reason(code: &'static str, message: &'static str, action: &'static str) -> Reason {
    Reason {
        code,
        message,
        action,
    }
}

// Not found.
pub const ALBUM_NOT_FOUND: Reason = reason(
    "ALBUM_NOT_FOUND",
    "This album is not in the library any more, or it was merged into another one.",
    "Reload the page and open the album again.",
);
pub const TRACK_NOT_FOUND: Reason = reason(
    "TRACK_NOT_FOUND",
    "One of the selected tracks is not in the library any more.",
    "Reload the album and select the tracks again.",
);
pub const TARGET_NOT_FOUND: Reason = reason(
    "TARGET_ALBUM_NOT_FOUND",
    "The album you picked to receive the tracks is not in the library any more.",
    "Search for the album again and pick it from the list.",
);
pub const ARTIST_NOT_FOUND: Reason = reason(
    "ARTIST_NOT_FOUND",
    "One of the selected artists is not in the library, or it was already merged.",
    "Reload the page and pick the artists again.",
);

// Bad input.
pub const NO_TRACKS: Reason = reason(
    "NO_TRACKS_SELECTED",
    "No tracks were selected.",
    "Tick at least one track and preview again.",
);
pub const TRACK_OUTSIDE_ALBUM: Reason = reason(
    "TRACK_NOT_ON_ALBUM",
    "A selected track does not belong to this album.",
    "Reload the album and select its tracks again.",
);
pub const TARGET_REQUIRED: Reason = reason(
    "TARGET_ALBUM_REQUIRED",
    "Pick the album that should receive the tracks.",
    "Search for the other album and select it.",
);
pub const TARGET_IS_SOURCE: Reason = reason(
    "TARGET_IS_SOURCE",
    "The tracks are already on the album you picked.",
    "Pick a different album to move or merge into.",
);
pub const SPLIT_TAKES_EVERYTHING: Reason = reason(
    "SPLIT_NEEDS_REMAINDER",
    "A split has to leave at least one track behind.",
    "Untick some tracks, or use Merge to move the whole album.",
);
pub const ROOT_MISMATCH: Reason = reason(
    "ALBUM_ROOT_MISMATCH",
    "These albums live in different library folders, so they cannot become one album.",
    "Pick an album from the same library folder.",
);
pub const NOTHING_TO_RESET: Reason = reason(
    "NOTHING_TO_RESET",
    "None of the selected tracks were grouped by hand, so there is nothing to reset.",
    "Use Split, Merge or Move to change this album instead.",
);
pub const NO_DUPLICATE_ARTIST: Reason = reason(
    "NO_DUPLICATE_ARTIST",
    "Choose at least one other artist to merge into the one you keep.",
    "Select the duplicate artists, then preview again.",
);
pub const RESERVED_ARTIST: Reason = reason(
    "RESERVED_ARTIST",
    "Various Artists and Unknown Artist cannot be merged away.",
    "Keep that artist as the survivor, or leave it out of the merge.",
);
pub const TOKEN_INVALID: Reason = reason(
    "PREVIEW_TOKEN_INVALID",
    "This change was not previewed, or the preview could not be read.",
    "Preview the change, then apply it.",
);

// Stale.
pub const PREVIEW_EXPIRED: Reason = reason(
    "PREVIEW_EXPIRED",
    "The preview is more than 15 minutes old.",
    "Preview the change again, then apply it.",
);
pub const PREVIEW_STALE: Reason = reason(
    "STALE_REVISION",
    "The albums or tracks changed after the preview, so the result would differ.",
    "Preview the change again and check the new result.",
);
pub const REVISION_STALE: Reason = reason(
    "STALE_REVISION",
    "Something changed since you opened this page.",
    "Reload the page and try again.",
);

// Edition outcomes (shown in the preview, stored on reviews).
pub const EDITION_KEPT: Reason = reason(
    "EDITION_KEPT",
    "The album keeps its edition.",
    "Nothing to do.",
);
pub const EDITION_MOVED: Reason = reason(
    "EDITION_MOVED",
    "The edition moves with the album it belonged to.",
    "Nothing to do.",
);
pub const EDITION_CONFLICT_CLEARED: Reason = reason(
    "EDITION_CLEARED_CONFLICT",
    "The merged albums had different editions, and you chose to drop them.",
    "Open the album and pick its edition, or let DroppedNeedle choose one.",
);
pub const EDITION_CONFLICT_KEPT: Reason = reason(
    "EDITION_KEPT_OVER_CONFLICT",
    "The merged albums had different editions; the album keeps its own and the others are dropped.",
    "Check the album's edition once the change is applied.",
);
pub const EDITION_AMBIGUOUS: Reason = reason(
    "EDITION_CLEARED_AMBIGUOUS",
    "The albums being combined had different editions and none of them clearly wins.",
    "Open the album and pick its edition, or let DroppedNeedle choose one.",
);
pub const EDITION_CUSTOM_CLEARED: Reason = reason(
    "EDITION_CLEARED_CUSTOM",
    "A custom edition only fits the album it was built for, so it does not carry over.",
    "Open the album and pick its edition again.",
);
pub const EDITION_ALBUM_GONE: Reason = reason(
    "EDITION_CLEARED_ALBUM_GONE",
    "The album is emptied and its tracks scatter, so its edition has nowhere to go.",
    "Check the editions of the albums the tracks land on.",
);
pub const EDITION_REMAP: Reason = reason(
    "EDITION_REMAP_QUEUED",
    "The album keeps its chosen edition; the new tracks are matched to it in the background.",
    "Nothing to do. Tracks that do not fit the edition show up on the album page.",
);
