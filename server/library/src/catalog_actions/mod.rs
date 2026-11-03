//! The rows of `library_catalog_actions`: the audit row a catalog decision
//! writes, and the history read from them.

pub mod history;
pub(crate) mod sql;

use rusqlite::{Transaction, params};

/// What a catalog decision was, in `library_catalog_actions.action_kind`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActionKind {
    AcceptManagementTrackMappings,
    AlbumArtist,
    AlbumArtistUndone,
    ArtistLinkMoved,
    ArtistLinkRemoved,
    ArtistLinkSet,
    AutomaticExactEdition,
    AutomaticMatchSealed,
    AutomaticMatchWithdrawn,
    ContinueTracks,
    CustomEdition,
    EditionChoice,
    EditionChoiceCleared,
    EditionChoiceUndone,
    EditionConfirmed,
    FilesPutBack,
    FilesRecycled,
    FoldUndone,
    FollowMergedArtistId,
    LeaveUnmanaged,
    ManagementHandBackGivenUp,
    ManagementHandedBack,
    ManagementReenabled,
    ManagementSteppedBack,
    /// Albums folded into one by a curator's membership change.
    Merge,
    MergeAlbum,
    MergeArtist,
    /// Tracks moved to another album by a curator.
    Move,
    NewCopyAtPath,
    OtherMusicAtPath,
    RemoveTrack,
    RepairDetach,
    /// An album's grouping put back to the scan's.
    Reset,
    RetagDismissalEnded,
    RetagFirstHalf,
    ReviewApprove,
    ReviewDetach,
    ReviewExclude,
    ReviewReopened,
    ReviewRestore,
    /// Tracks split off an album by a curator.
    Split,
    TrackMatchUndone,
    TrackMatched,
    UndoAutomaticEdition,
}

droppedneedle_kernel::spelled!(ActionKind {
    AcceptManagementTrackMappings => "accept_management_track_mappings",
    AlbumArtist => "album_artist",
    AlbumArtistUndone => "album_artist_undone",
    ArtistLinkMoved => "artist_link_moved",
    ArtistLinkRemoved => "artist_link_removed",
    ArtistLinkSet => "artist_link_set",
    AutomaticExactEdition => "automatic_exact_edition",
    AutomaticMatchSealed => "automatic_match_sealed",
    AutomaticMatchWithdrawn => "automatic_match_withdrawn",
    ContinueTracks => "continue_tracks",
    CustomEdition => "custom_edition",
    EditionChoice => "edition_choice",
    EditionChoiceCleared => "edition_choice_cleared",
    EditionChoiceUndone => "edition_choice_undone",
    EditionConfirmed => "edition_confirmed",
    FilesPutBack => "files_put_back",
    FilesRecycled => "files_recycled",
    FoldUndone => "fold_undone",
    FollowMergedArtistId => "follow_merged_artist_id",
    LeaveUnmanaged => "leave_unmanaged",
    ManagementHandBackGivenUp => "management_hand_back_given_up",
    ManagementHandedBack => "management_handed_back",
    ManagementReenabled => "management_reenabled",
    ManagementSteppedBack => "management_stepped_back",
    Merge => "merge",
    MergeAlbum => "merge_album",
    MergeArtist => "merge_artist",
    Move => "move",
    NewCopyAtPath => "new_copy_at_path",
    OtherMusicAtPath => "other_music_at_path",
    RemoveTrack => "remove_track",
    RepairDetach => "repair_detach",
    Reset => "reset",
    RetagDismissalEnded => "retag_dismissal_ended",
    RetagFirstHalf => "retag_first_half",
    ReviewApprove => "review_approve",
    ReviewDetach => "review_detach",
    ReviewExclude => "review_exclude",
    ReviewReopened => "review_reopened",
    ReviewRestore => "review_restore",
    Split => "split",
    TrackMatchUndone => "track_match_undone",
    TrackMatched => "track_matched",
    UndoAutomaticEdition => "undo_automatic_edition",
});

/// One audit row for a catalog decision.
pub(crate) struct CatalogAction<'a> {
    pub actor: &'a str,
    pub kind: ActionKind,
    pub album_id: &'a str,
    pub job_id: Option<&'a str>,
    pub before: serde_json::Value,
    pub after: serde_json::Value,
    pub reason: &'a str,
}

pub(crate) fn record_action(
    tx: &Transaction<'_>,
    action: CatalogAction<'_>,
    now: f64,
) -> rusqlite::Result<String> {
    let id = uuid::Uuid::new_v4().to_string();
    tx.execute(
        crate::catalog_actions::sql::RECORD_ACTION.text(),
        params![
            id,
            action.actor,
            action.kind,
            action.album_id,
            action.job_id,
            action.before.to_string(),
            action.after.to_string(),
            action.reason,
            crate::clock::ms(now),
        ],
    )?;
    Ok(id)
}

/// One audit row for a catalog decision about an artist.
pub(crate) struct ArtistAction<'a> {
    pub actor: &'a str,
    pub kind: ActionKind,
    pub artist_id: &'a str,
    pub before: serde_json::Value,
    pub after: serde_json::Value,
    pub reason: &'a str,
}

pub(crate) fn record_artist_action(
    tx: &Transaction<'_>,
    action: ArtistAction<'_>,
    now: f64,
) -> rusqlite::Result<String> {
    let id = uuid::Uuid::new_v4().to_string();
    tx.execute(
        crate::catalog_actions::sql::RECORD_ARTIST_ACTION.text(),
        params![
            id,
            action.actor,
            action.kind,
            action.artist_id,
            action.before.to_string(),
            action.after.to_string(),
            action.reason,
            crate::clock::ms(now),
        ],
    )?;
    Ok(id)
}
