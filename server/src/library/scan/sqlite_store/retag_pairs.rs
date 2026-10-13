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
    /// Half or more of the old album's track rows stayed behind.
    MostlyMissing,
    /// The new row also holds files that arrived with the retag.
    NewFilesAdded,
    /// Some of the old album's tracks went to another album earlier.
    TracksElsewhere,
    /// Each row holds a match a person chose, and the two differ.
    ChosenMatchesDiffer,
    /// The old row holds a custom edition.
    CustomEdition,
    /// A preview or a plan is open on one of the rows.
    LiveWork,
    /// Work started on the new row names it, so it cannot be deleted.
    WorkOnNewRow,
    /// The store failed while joining them. Tried again.
    StoreError,
}

impl RetagPairReason {
    pub const ALL: [Self; 8] = [
        Self::MostlyMissing,
        Self::NewFilesAdded,
        Self::TracksElsewhere,
        Self::ChosenMatchesDiffer,
        Self::CustomEdition,
        Self::LiveWork,
        Self::WorkOnNewRow,
        Self::StoreError,
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
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|reason| reason.as_str() == value)
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

/// The reason the pair is recorded under, when it is.
pub(super) fn recorded(
    conn: &Connection,
    old_album_id: &str,
    new_album_id: &str,
) -> rusqlite::Result<Option<RetagPairReason>> {
    let reason: Option<String> = conn
        .prepare_cached(
            "SELECT reason FROM library_retag_pairs \
             WHERE old_album_id = ?1 AND new_album_id = ?2",
        )?
        .query_row(params![old_album_id, new_album_id], |row| row.get(0))
        .optional()?;
    reason
        .map(|reason| {
            RetagPairReason::parse(&reason).ok_or_else(|| {
                rusqlite::Error::FromSqlConversionFailure(
                    0,
                    rusqlite::types::Type::Text,
                    format!("unknown retag pair reason {reason}").into(),
                )
            })
        })
        .transpose()
}

/// Record the pair as open. A pair already recorded stays as it is,
/// except that a store error gives way to a reason found since. Returns
/// whether anything was written, so the caller says it once.
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
             WHERE library_retag_pairs.reason = ?6 AND excluded.reason <> ?6",
        )?
        .execute(params![
            old_album_id,
            new_album_id,
            reason.as_str(),
            RetagPairState::Open.as_str(),
            now_ms,
            RetagPairReason::StoreError.as_str(),
        ])?;
    Ok(written > 0)
}
