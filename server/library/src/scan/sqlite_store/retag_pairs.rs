//! Two album rows that look like one retagged album and were not joined.
//!
//! A scan carries an album's row over to its new name when all of its
//! files are retagged together. Where it will not do that by itself, the
//! old row keeps the match, the favorites and the id while the files sit
//! on a new row. The pair is recorded here with the reason, so it can be
//! shown to a person and joined or dismissed. A pair goes with either of
//! its albums.

use rusqlite::{Connection, OptionalExtension as _, params};

/// Why the two rows were not joined.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetagPairReason {
    /// Half or more of the old album's track rows stayed behind. The scan
    /// joins such an album itself now, and no row carries this reason.
    MostlyMissing,
    /// The new row also holds files that arrived with the retag. The scan
    /// joins such an album itself now, and no row carries this reason.
    NewFilesAdded,
    /// Some of the old album's tracks went to an album of their own in an
    /// earlier scan, and the rest to another one in a later scan: a set
    /// split in two, or one track taken out and the album then renamed.
    TracksElsewhere,
    /// Each row holds a match a person chose, and the two differ.
    ChosenMatchesDiffer,
    /// The old row holds a custom edition.
    CustomEdition,
    /// A preview or a plan is open on one of the rows. Tried again when a
    /// scan ends.
    LiveWork,
    /// Work started on the new row names it, so it cannot be deleted. The
    /// merge, which keeps the new row, is tried once when the scan ends.
    WorkOnNewRow,
    /// The store failed while joining them. Tried again when a scan ends.
    StoreError,
    /// The old album, left unmanaged, kept as a custom edition or stepped
    /// back from, lost its files in the scan in which the new album's
    /// files appeared beside it. Nothing proves they are one album. Tried
    /// again so a join on proof of the same edition is not stopped by it.
    ReplacedHandsOff,
}

impl RetagPairReason {
    pub const ALL: [Self; 9] = [
        Self::MostlyMissing,
        Self::NewFilesAdded,
        Self::TracksElsewhere,
        Self::ChosenMatchesDiffer,
        Self::CustomEdition,
        Self::LiveWork,
        Self::WorkOnNewRow,
        Self::StoreError,
        Self::ReplacedHandsOff,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::MostlyMissing => "mostly_missing",
            Self::NewFilesAdded => "new_files_added",
            Self::TracksElsewhere => "tracks_elsewhere",
            Self::ChosenMatchesDiffer => "chosen_matches_differ",
            Self::CustomEdition => "custom_edition",
            Self::LiveWork => "live_work",
            Self::WorkOnNewRow => "work_on_new_row",
            Self::StoreError => "store_error",
            Self::ReplacedHandsOff => "replaced_hands_off",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|reason| reason.as_str() == value)
    }

    /// Whether an open pair with this reason is tried again when a scan
    /// ends.
    pub(super) fn tried_again(self) -> bool {
        matches!(
            self,
            Self::StoreError | Self::LiveWork | Self::WorkOnNewRow | Self::ReplacedHandsOff
        )
    }
}

/// Where a recorded pair stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetagPairState {
    /// Waiting for a person.
    Open,
    /// A person joined the two rows.
    Joined,
    /// A person said they are two albums.
    Dismissed,
}

impl RetagPairState {
    pub const ALL: [Self; 3] = [Self::Open, Self::Joined, Self::Dismissed];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Joined => "joined",
            Self::Dismissed => "dismissed",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|state| state.as_str() == value)
    }
}

/// The reason the pair is recorded under and where it stands, when it is
/// on record.
pub(super) fn recorded(
    conn: &Connection,
    old_album_id: &str,
    new_album_id: &str,
) -> rusqlite::Result<Option<(RetagPairReason, RetagPairState)>> {
    let stored: Option<(String, String)> = conn
        .prepare_cached(
            "SELECT reason, state FROM library_retag_pairs \
             WHERE old_album_id = ?1 AND new_album_id = ?2",
        )?
        .query_row(params![old_album_id, new_album_id], |row| {
            Ok((row.get(0)?, row.get(1)?))
        })
        .optional()?;
    let unknown = |column: usize, value: &str| {
        rusqlite::Error::FromSqlConversionFailure(
            column,
            rusqlite::types::Type::Text,
            format!("unknown retag pair value {value}").into(),
        )
    };
    stored
        .map(|(reason, state)| {
            Ok((
                RetagPairReason::parse(&reason).ok_or_else(|| unknown(0, &reason))?,
                RetagPairState::parse(&state).ok_or_else(|| unknown(1, &state))?,
            ))
        })
        .transpose()
}

/// Record the pair as open. A pair already recorded stays as it is,
/// except that one still open and waiting to be tried again (a store
/// error, live work, work on the new row) takes a different reason found
/// since. Returns whether anything was written, so the caller says it
/// once.
pub(super) fn record(
    conn: &Connection,
    old_album_id: &str,
    new_album_id: &str,
    reason: RetagPairReason,
    now_ms: i64,
) -> rusqlite::Result<bool> {
    let written = conn
        .prepare_cached(
            "INSERT INTO library_retag_pairs (old_album_id, new_album_id, reason, state, \
             recorded_at, updated_at) VALUES (?1, ?2, ?3, ?4, ?5, ?5) \
             ON CONFLICT (old_album_id, new_album_id) DO UPDATE SET \
             reason = excluded.reason, updated_at = excluded.updated_at \
             WHERE library_retag_pairs.reason IN (?6, ?7, ?8) \
             AND library_retag_pairs.state = ?4 \
             AND excluded.reason <> library_retag_pairs.reason",
        )?
        .execute(params![
            old_album_id,
            new_album_id,
            reason.as_str(),
            RetagPairState::Open.as_str(),
            now_ms,
            RetagPairReason::StoreError.as_str(),
            RetagPairReason::LiveWork.as_str(),
            RetagPairReason::WorkOnNewRow.as_str(),
        ])?;
    Ok(written > 0)
}

/// End the dismissed pairs whose new album is `album_id` or was merged
/// into it since, when this run
/// moved tracks from it back to the pair's old album and it holds no
/// indexed track any more. The person's answer was about a rename that is
/// undone, so it must not stop a later rename of the old album from being
/// joined. Returns each ended pair's old and new album.
pub(super) fn end_dismissed_returned(
    conn: &Connection,
    run_id: &str,
    album_id: &str,
) -> rusqlite::Result<Vec<(String, String)>> {
    conn.prepare_cached(
        "DELETE FROM library_retag_pairs WHERE state = ?3 AND new_album_id IN \
         (SELECT id FROM local_albums WHERE id = ?2 OR retired_into_album_id = ?2) \
         AND NOT EXISTS (SELECT 1 FROM local_tracks t WHERE t.local_album_id = ?2 \
           AND t.availability = 'indexed') \
         AND EXISTS (SELECT 1 FROM library_scan_album_moves m \
           JOIN local_tracks t ON t.id = m.local_track_id \
           WHERE m.run_id = ?1 AND m.previous_album_id = ?2 \
           AND t.local_album_id = library_retag_pairs.old_album_id) \
         RETURNING old_album_id, new_album_id",
    )?
    .query_map(
        params![run_id, album_id, RetagPairState::Dismissed.as_str()],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?
    .collect()
}
