//! Why a file in a management preview is left alone, or why applying it
//! failed: a stable code, one plain sentence, and what to do about it.
//!
//! Codes match the ones v2 stored, so migrated history and the web app's
//! labels keep working. Script faults carry the script's own sentence,
//! which is why the text here is owned rather than static.

use serde::{Deserialize, Serialize};

use crate::operations::reasons::Reason;
use crate::publish::PublishError;
use crate::publish::undo::UndoBlock;

/// One reason, as the user reads it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ItemReason {
    pub code: String,
    pub message: String,
    pub action: String,
}

impl ItemReason {
    fn new(code: &str, message: impl Into<String>, action: &str) -> Self {
        Self {
            code: code.to_owned(),
            message: message.into(),
            action: action.to_owned(),
        }
    }
}

impl From<Reason> for ItemReason {
    fn from(reason: Reason) -> Self {
        Self::new(reason.code, reason.message, reason.action)
    }
}

/// Lyrics or ReplayGain the profile requires could not be had (v2's
/// `METADATA_UNAVAILABLE`).
pub fn metadata_unavailable(what: &str) -> ItemReason {
    ItemReason {
        code: "METADATA_UNAVAILABLE".to_owned(),
        message: format!(
            "The profile requires {what} for every file, and none could be found for this one."
        ),
        action: format!(
            "Make a new preview later, or turn off \"required\" for {what} in the profile."
        ),
    }
}

pub fn identity_not_accepted() -> ItemReason {
    ItemReason::new(
        "IDENTITY_NOT_ACCEPTED",
        "This album is not matched to MusicBrainz yet, so there is nothing to tag or name it from.",
        "Wait for identification to run, or pick the album's edition on its page.",
    )
}

pub fn release_not_selected() -> ItemReason {
    ItemReason::new(
        "RELEASE_NOT_SELECTED",
        "This album is kept as your own edition, so it has no exact MusicBrainz release to copy tags from.",
        "Pick an exact edition on the album page if you want its files managed.",
    )
}

pub fn unconfirmed_match() -> ItemReason {
    ItemReason::new(
        "UNCONFIRMED_MATCH",
        "This album's edition is a best guess nobody has confirmed yet, so its files are left alone.",
        "Confirm the match on the album page, or turn on \"Also manage unconfirmed matches\" in Settings > Edition preferences.",
    )
}

pub fn unmatched_album() -> ItemReason {
    ItemReason::new(
        "UNMATCHED_ALBUM",
        "Nothing on MusicBrainz fits this album, so it keeps its own tags.",
        "Pick its edition on the album page first.",
    )
}

/// The album's chosen edition is still being placed. The wording changes
/// once MusicBrainz has failed to answer, since the wait can then be long.
pub fn edition_remap_pending(attempts: i64) -> ItemReason {
    let action = if attempts > 0 {
        "MusicBrainz is not answering, so this waits until it does."
    } else {
        "Try again in a moment."
    };
    ItemReason::new(
        "EDITION_REMAP_PENDING",
        "Still matching this album to the edition you chose.",
        action,
    )
}

pub fn release_unavailable() -> ItemReason {
    ItemReason::new(
        "METADATA_UNAVAILABLE",
        "The details of this album's edition are not saved on this server yet.",
        "Open the album page so its edition is fetched again, then make a new preview.",
    )
}

pub fn track_not_mapped() -> ItemReason {
    ItemReason::new(
        "TRACK_NOT_MAPPED",
        "This file is not tied to a track of the album's edition.",
        "Open the album page and check the tracklist, or pick an edition that has this track.",
    )
}

pub fn format_unsupported(format: &str) -> ItemReason {
    ItemReason::new(
        "FORMAT_UNSUPPORTED",
        format!("Tags cannot be written to {} files.", format.to_uppercase()),
        "Turn off tag writing for this run, or convert the file to a supported format.",
    )
}

pub fn match_unreadable() -> ItemReason {
    ItemReason::new(
        "MATCH_UNREADABLE",
        "The file's MusicBrainz match could not be read.",
        "Make a new preview. If it keeps happening, check the server log.",
    )
}

pub fn file_unreadable() -> ItemReason {
    ItemReason::new(
        "FILE_UNREADABLE",
        "The file could not be read, so its tags are unknown.",
        "Check that the file exists and the server can read it, then rescan the library.",
    )
}

pub fn root_unavailable() -> ItemReason {
    ItemReason::new(
        "ROOT_UNAVAILABLE",
        "The library folder this file lives in is not available or is excluded.",
        "Check the folder in Settings > Library, then make a new preview.",
    )
}

/// An album in a folder set to Local metadata: its files keep their own
/// tags and names, whatever asked for them to be managed.
pub const KEEPS_OWN_TAGS: Reason = Reason {
    code: "LOCAL_METADATA",
    message: "This album's folder keeps its own tags, so its files are left alone.",
    action: "To have them managed, change the folder's policy, or the path rule that covers them, in Settings > Library. The tag editor still works here.",
};

pub fn keeps_own_tags() -> ItemReason {
    KEEPS_OWN_TAGS.into()
}

pub fn symlink_unsupported() -> ItemReason {
    ItemReason::new(
        "SYMLINK_UNSUPPORTED",
        "This file or its destination is a symbolic link, and links are never written through.",
        "Replace the link with the real file, or exclude it from the library.",
    )
}

pub fn recovery_needs_attention() -> ItemReason {
    ItemReason::new(
        "RECOVERY_NEEDS_ATTENTION",
        "An earlier change to this file did not finish, so it takes no new changes until that is sorted out.",
        "Wait for the next maintenance pass to finish the earlier change, then make a new preview.",
    )
}

pub fn script_failed(script_name: &str, fault: &str) -> ItemReason {
    ItemReason::new(
        "SCRIPT_VALIDATION_FAILED",
        format!("The script \"{script_name}\" could not handle this file: {fault}"),
        "Fix the script in Settings > Library Management, then make a new preview.",
    )
}

pub fn path_too_long() -> ItemReason {
    ItemReason::new(
        "PATH_TOO_LONG",
        "The path this file would get is longer than the profile allows.",
        "Shorten the naming script or raise the path length limit in the profile.",
    )
}

pub fn path_collision(identical: bool) -> ItemReason {
    if identical {
        ItemReason::new(
            "PATH_COLLISION_IDENTICAL",
            "A copy of this exact file already sits where it would go.",
            "Remove one of the copies, then make a new preview.",
        )
    } else {
        ItemReason::new(
            "PATH_COLLISION_DIFFERENT",
            "Another file already sits where this file would go, and files are never overwritten.",
            "Move or remove the other file, or change the naming script, then make a new preview.",
        )
    }
}

pub fn position_collision() -> ItemReason {
    ItemReason::new(
        "POSITION_COLLISION",
        "Two files in this run would end up at the same path.",
        "Check the album for duplicate tracks, or add the disc or track number to the naming script.",
    )
}

pub fn sidecar_collision() -> ItemReason {
    ItemReason::new(
        "SIDECAR_COLLISION",
        "A file that travels with this album (such as cover.jpg or a .cue sheet) already exists where the album would go, and files are never overwritten.",
        "Move or remove the file at the destination, or turn off moving sidecar files in the profile, then make a new preview.",
    )
}

pub fn sidecar_unreadable() -> ItemReason {
    ItemReason::new(
        "SIDECAR_UNREADABLE",
        "A file that travels with this album could not be read, is a symbolic link, or there are too many of them to move safely.",
        "Check the album folder, or turn off moving sidecar files in the profile, then make a new preview.",
    )
}

pub fn album_changed() -> ItemReason {
    ItemReason::new(
        "ALBUM_CHANGED",
        "Files were added to, removed from or regrouped in this album after the preview was made, so it was left alone.",
        "Make a new preview.",
    )
}

/// What a person's preview of a taken-over album says on each file.
pub const TAKEN_OVER_WARNING: &str = "This album is left unmanaged because its files were changed outside DroppedNeedle. Applying this preview overwrites those changes.";

pub fn taken_over() -> ItemReason {
    ItemReason::new(
        super::takeover::TAKEN_OVER,
        "This album is left unmanaged because its files were changed outside DroppedNeedle.",
        "Preview the album by itself to see what would change, or choose \"Manage it again\" on its page.",
    )
}

pub fn bundle_blocked() -> ItemReason {
    ItemReason::new(
        "BUNDLE_BLOCKED",
        "Another file in this album can't be managed, and an album is always changed as a whole so it never ends up half moved.",
        "Fix the other file's problem (its own reason says how), then make a new preview.",
    )
}

pub fn bundle_too_large(limit: usize) -> ItemReason {
    ItemReason::new(
        "BUNDLE_TOO_LARGE",
        format!("This album has more than {limit} files, the most one change can hold."),
        "Split the album into smaller albums on the album page, then make a new preview.",
    )
}

pub fn insufficient_space() -> ItemReason {
    ItemReason::new(
        "INSUFFICIENT_SPACE",
        "There is not enough free disk space to write this album safely.",
        "Free up space in the library folder, then try again.",
    )
}

pub fn file_changed() -> ItemReason {
    ItemReason::new(
        "FILE_CHANGED",
        "The file, its match or its place in the library changed after this preview was made.",
        "Make a new preview.",
    )
}

pub fn catalog_changed() -> ItemReason {
    ItemReason::new(
        "CATALOG_CHANGED",
        "The library changed while this album was being saved, so nothing was written.",
        "Make a new preview.",
    )
}

pub fn write_failed() -> ItemReason {
    ItemReason::new(
        "WRITE_FAILED",
        "The files could not be written; the album was left as it was.",
        "Check that the library folder is writable and has free space, then try again. The server log has the details.",
    )
}

/// The folder has no way to add a file that cannot replace another one.
/// `message` is the publisher's sentence.
pub fn no_safe_placement(message: &str) -> ItemReason {
    ItemReason::new(
        "WRITE_FAILED",
        message,
        "Move this library to a disk that supports hard links, such as ext4 or an NFS share, then make a new preview.",
    )
}

/// A file that could be set aside but not put back, so it was left alone.
/// `message` is the publisher's sentence, which names the file.
pub fn file_not_linkable(message: &str) -> ItemReason {
    ItemReason::new(
        "WRITE_FAILED",
        message,
        "Make the server's user the owner of the file and remove any lock on it, or move the library to a disk that supports hard links, then make a new preview.",
    )
}

/// A tag the writer cannot keep whole, so its file is left alone.
/// `message` is the tag reader's sentence.
pub fn tags_not_kept(message: &str) -> ItemReason {
    ItemReason::new(
        "WRITE_FAILED",
        message,
        "Remove or fix the tag it names with a tag editor, then make a new preview.",
    )
}

pub fn root_read_only() -> ItemReason {
    ItemReason::new(
        "ROOT_READ_ONLY",
        "The destination library folder does not exist or is excluded.",
        "Pick another destination folder, or check it in Settings > Library.",
    )
}

pub fn not_scheduled() -> ItemReason {
    ItemReason::new(
        "NOT_SCHEDULED",
        "This file was not part of the apply because it had nothing to change or was blocked.",
        "Nothing to do for files with no changes; for blocked files, see the reason in the preview.",
    )
}

pub fn undo_nothing() -> ItemReason {
    ItemReason::new(
        "UNDO_EXPIRED",
        "There is nothing left to undo for this album: its before-state was not kept or has expired.",
        "Restore the files from a backup if you need the old state back.",
    )
}

/// Why undo leaves one file alone.
pub fn undo_block(block: &UndoBlock) -> ItemReason {
    match block {
        UndoBlock::ExternallyChanged => ItemReason::new(
            "FILE_CHANGED",
            "The file changed after the change being undone, so undoing it would lose those edits.",
            "Undo by hand, or restore the original from a backup.",
        ),
        UndoBlock::Moved => ItemReason::new(
            "FILE_CHANGED",
            "The file moved after the change being undone.",
            "Move it back, or rescan the library, then make a new undo preview.",
        ),
        UndoBlock::IdentityChanged => ItemReason::new(
            "FILE_CHANGED",
            "The album's match changed after the change being undone.",
            "Run a new management preview with the current match instead.",
        ),
        UndoBlock::OverrideChanged => ItemReason::new(
            "OVERRIDE_CHANGED",
            "The folder's management overrides changed after the change being undone.",
            "Run a new management preview instead.",
        ),
        UndoBlock::SnapshotExpired => ItemReason::new(
            "UNDO_EXPIRED",
            "This file's before-state was kept for the undo period, which has passed.",
            "Restore the original from a backup if you need it.",
        ),
        UndoBlock::RestoreOccupied => ItemReason::new(
            "PATH_COLLISION_DIFFERENT",
            "Another file now sits at this file's old path, and files are never overwritten.",
            "Move the other file away, then make a new undo preview.",
        ),
    }
}

/// A publish failure as the reason for every file in its album.
pub fn from_publish(error: &PublishError) -> ItemReason {
    match error {
        PublishError::Capability(_) => ItemReason::new(
            "FORMAT_UNSUPPORTED",
            "One of the album's files is in a format that cannot be changed this way.",
            "Turn off tag writing for this run, or convert the file to a supported format.",
        ),
        PublishError::Collision(_) => path_collision(false),
        PublishError::Validation(_) => file_changed(),
        PublishError::Space { .. } => insufficient_space(),
        PublishError::Catalog(_) => catalog_changed(),
        PublishError::Journal(_) => recovery_needs_attention(),
        PublishError::Snapshot(_) => ItemReason::new(
            "BASELINE_UNAVAILABLE",
            "This file's original state from v2 did not come across with the migration.",
            "Restore the v2 library-management cache from a backup and migrate again before managing it.",
        ),
        PublishError::UnsafePath(_) => symlink_unsupported(),
        PublishError::FolderMissing => root_unavailable(),
        PublishError::NoSafePlacement(detail) => no_safe_placement(&detail.message),
        PublishError::NotLinkable(detail) => file_not_linkable(&detail.message),
        PublishError::Cleanup(_)
        | PublishError::Archive(_)
        | PublishError::Store(_)
        | PublishError::Interrupted(_)
        | PublishError::TimedOut(_)
        | PublishError::Io(_)
        | PublishError::InjectedCrash(_) => write_failed(),
    }
}
