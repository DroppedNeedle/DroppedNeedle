//! Album and track identity: who decided an album's MusicBrainz match, and
//! the commands that write it.
//!
//! An album's match is its row in `local_album_external_identities`; its
//! files' matches are rows in `local_track_external_identities`. Beside
//! them sit the album's match state (`library_album_match_state`) and its
//! two undo rows (`library_automatic_edition_undo`,
//! `library_edition_choice_undo`). The commands here take the caller's
//! transaction and move the catalog revision when they change a row; the
//! import's seal runs in the publisher's commit, which moves it already.
//!
//! Who decided is a [`DecisionSource`]. A row identification wrote, or one
//! whose ids the files' tags carried, stays revisable: a later automatic
//! pass may replace it, and new evidence that contradicts it takes it
//! away. A person's choice, and one carried over from v2, is never touched
//! by automatic work, with one exception that can be undone: the catalog
//! repair that folds the stray half of an album a v2 import split in two
//! ([`fold_legacy`], [`undo_fold`]).
//!
//! Only this module writes these tables; a source gate holds it.
//!
//! Beside the five tables the module also writes what only means something
//! with a match: the queued placement of an album's files
//! (`library_edition_remap_queue`), the active custom edition and its
//! manifests, and the records of files matched by hand.

mod automatic;
mod custom;
mod fold;
pub(crate) mod follow;
mod import;
mod moves;
mod person;
pub mod reads;
pub mod remap;
pub(crate) mod sql;
mod undo;

use rusqlite::types::{FromSql, FromSqlError, FromSqlResult, ToSql, ToSqlOutput, ValueRef};
use rusqlite::{Transaction, params};
use serde::{Deserialize, Serialize};

pub(crate) use automatic::write_match_flag;
pub use automatic::{
    Accepted, AutomaticOutcome, Expected, Inputs, Placed, accept_automatic, retract, seal_automatic,
};
pub use custom::{Custom, custom_edition};
pub(crate) use fold::fold_legacy;
pub use fold::{
    FOLD_REASON, FOLD_UNDONE, FoldUndo, Folded, TakeoverMark, fold_undoable, fold_undone, undo_fold,
};
pub use import::{ImportedFile, adopt_import, import_album};
pub(crate) use import::{save_album, save_track};
pub(crate) use moves::{
    Link, LinkedTrack, Spot, clear_chosen_flag, link, move_edition, place_mapping, place_scanned,
    release_track, settle_tracks,
};
pub use person::{
    Approved, Chosen, Matched, Offer, Removal, approve, choose, confirm, hand_back, hand_match,
    leave_unmanaged, place, remove, unmatch,
};
pub(crate) use person::{TrackRow, upsert_track_identity};
pub use undo::{AutomaticUndo, AutomaticUndone, undo, undo_automatic};

/// Why a person's identity command wrote nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    /// The album has no indexed file.
    AlbumNotFound,
    /// The edition has a track for none of the album's files.
    EditionFitsNoFile,
    /// The album has no match naming a release to confirm.
    NoMatchToConfirm,
    /// The album's match is not an unconfirmed best guess.
    NothingToConfirm,
    /// The album has no edition change to take back.
    NoEditionChoice,
    /// The album's identity changed since its last edition change.
    EditionChoiceStale,
    /// The album has no automatic edition to undo.
    NoAutomaticEdition,
    /// The album changed since its automatic edition.
    AutomaticUndoStale,
    /// The album's identity is not at the revision the person saw.
    EditionChanged,
    /// The custom edition's release belongs to another release group.
    CustomEditionGroupConflict,
    /// The files lack an album title or an album artist.
    CustomEditionNamesMissing,
    /// The album artist's match names another artist.
    CustomEditionArtistConflict,
    /// The files lack a unique disc and track number of their own.
    CustomEditionPositions,
}

/// A person's identity command failed: the store failed, or the command
/// refused and wrote nothing.
#[derive(Debug)]
pub enum IdentityError {
    Db(rusqlite::Error),
    Refused(Refusal),
}

impl std::fmt::Display for IdentityError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Db(error) => write!(f, "{error}"),
            Self::Refused(refusal) => write!(f, "refused: {refusal:?}"),
        }
    }
}

impl std::error::Error for IdentityError {}

impl From<rusqlite::Error> for IdentityError {
    fn from(error: rusqlite::Error) -> Self {
        Self::Db(error)
    }
}

impl From<Refusal> for IdentityError {
    fn from(refusal: Refusal) -> Self {
        Self::Refused(refusal)
    }
}

/// The provider every identity row here names.
pub(crate) const PROVIDER: &str = "musicbrainz";

/// Who last set an identity row.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum DecisionSource {
    /// Identification chose it.
    Automatic,
    /// A person chose it.
    Manual,
    /// A person's choice carried over from v2.
    LegacyImport,
    /// v2 took it from the ids in the files' tags.
    Embedded,
}

impl DecisionSource {
    /// Every source, in the order the SQL sets list them.
    pub const ALL: [Self; 4] = [
        Self::Automatic,
        Self::Manual,
        Self::LegacyImport,
        Self::Embedded,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Automatic => "automatic",
            Self::Manual => "manual",
            Self::LegacyImport => "legacy_import",
            Self::Embedded => "embedded",
        }
    }

    /// The source a stored value names; `None` for a value no source has.
    pub fn parse(raw: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|source| source.as_str() == raw)
    }

    /// Automatic work may replace this row, and contradicting evidence
    /// takes it away.
    pub fn is_revisable(self) -> bool {
        matches!(self, Self::Automatic | Self::Embedded)
    }

    /// A person decided it, now or in v2: nothing automatic replaces or
    /// removes it.
    pub fn is_persons_choice(self) -> bool {
        matches!(self, Self::Manual | Self::LegacyImport)
    }
}

impl FromSql for DecisionSource {
    fn column_result(value: ValueRef<'_>) -> FromSqlResult<Self> {
        let raw = value.as_str()?;
        Self::parse(raw)
            .ok_or_else(|| FromSqlError::Other(format!("unknown decision source {raw:?}").into()))
    }
}

impl ToSql for DecisionSource {
    fn to_sql(&self) -> rusqlite::Result<ToSqlOutput<'_>> {
        Ok(ToSqlOutput::from(self.as_str()))
    }
}

/// Which album row [`delete_album_row`] may take.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Rows {
    /// Whoever decided it.
    Any,
    /// Only a revisable row; a person's stays.
    Revisable,
}

/// Delete the album's match row and both of its undo rows. A row
/// inserted again starts at revision 1, so an undo kept for an earlier row
/// could otherwise match the next one and put back what it replaced.
/// `Rows::Any` clears the undo rows whenever the album is left with no
/// match, so it also runs before another album's row is moved onto this
/// one. `Rows::Revisable` clears them only when it deleted a row: an album
/// a person took the match off keeps that change's undo. A retraction is
/// automatic work, so an undo carried through it (a hand back's) stays,
/// keyed to the album having no match. `true` when a match row went.
pub(crate) fn delete_album_row(
    tx: &Transaction<'_>,
    album_id: &str,
    rows: Rows,
) -> rusqlite::Result<bool> {
    match rows {
        Rows::Any => {
            let deleted = tx
                .prepare_cached(sql::DELETE_ALBUM_ROW.text())?
                .execute(params![album_id, PROVIDER])?;
            let matched: bool = tx
                .prepare_cached(sql::HAS_ALBUM_ROW.text())?
                .query_row(params![album_id, PROVIDER], |row| row.get(0))?;
            if !matched {
                for statement in [sql::FORGET_AUTOMATIC_UNDO, sql::FORGET_CHOICE_UNDO] {
                    tx.prepare_cached(statement.text())?
                        .execute(params![album_id])?;
                }
            }
            Ok(deleted > 0)
        }
        Rows::Revisable => {
            let revision = reads::identity_revision(tx, album_id)?;
            let deleted = tx
                .prepare_cached(sql::DELETE_REVISABLE_ALBUM_ROW.text())?
                .execute(params![album_id, PROVIDER])?;
            if deleted > 0 {
                tx.prepare_cached(sql::FORGET_AUTOMATIC_UNDO.text())?
                    .execute(params![album_id])?;
                tx.prepare_cached(sql::FORGET_UNCARRIED_CHOICE_UNDO.text())?
                    .execute(params![album_id, revision])?;
                undo::carry_undo(tx, album_id, revision, 0)?;
            }
            Ok(deleted > 0)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The SQL set: `('a','b')` over the sources `keep` admits, in
    /// [`DecisionSource::ALL`] order.
    fn set(keep: fn(DecisionSource) -> bool) -> String {
        let names: Vec<String> = DecisionSource::ALL
            .into_iter()
            .filter(|source| keep(*source))
            .map(|source| format!("'{}'", source.as_str()))
            .collect();
        format!("({})", names.join(","))
    }

    #[test]
    fn the_sql_sets_match_the_predicates() {
        assert_eq!(sql::REVISABLE, set(DecisionSource::is_revisable));
        assert_eq!(sql::PERSONS, set(DecisionSource::is_persons_choice));
    }

    #[test]
    fn every_source_is_revisable_or_a_persons_and_never_both() {
        for source in DecisionSource::ALL {
            assert_ne!(
                source.is_revisable(),
                source.is_persons_choice(),
                "{source:?}"
            );
            assert_eq!(DecisionSource::parse(source.as_str()), Some(source));
        }
        assert_eq!(DecisionSource::parse("pinned"), None);
    }
}
