//! Durable per-file journals with a monotonic state machine.
//!
//! Every audio file, sidecar, and external-art output owns one journal
//! row. States only move forward along the prepare/publish/commit/
//! cleanup protocol, and every transition is compare-and-swap on the
//! current state so a crash between the filesystem step and its SQLite
//! transition is visible to recovery instead of silently lost.

use std::path::Path;

use droppedneedle_kernel::db::Durable;
use rusqlite::{Connection, OptionalExtension};

use super::paths::FileStat;
use super::sql::JournalRow;
use super::{PublishError, sql};
use crate::scan::models::spelled;

/// Monotonic journal states. Terminal states are `Cleaned`,
/// `Compensated`, and `NeedsAttention`; recovery never moves a
/// committed journal backward.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JournalState {
    /// Intent recorded; nothing staged yet.
    Prepared,
    /// Staged temp written, re-read, validated, and fsynced.
    Staged,
    /// Staged temp renamed onto the destination.
    Published,
    /// Catalog transaction committed. Never rolled back.
    Committed,
    /// Verified sources/backups removed after commit.
    Cleaned,
    /// Pre-commit failure unwound: published bytes removed, backups
    /// restored, temps deleted.
    Compensated,
    /// Ambiguous evidence; an administrator must look. Deletes nothing.
    NeedsAttention,
    /// Commit stands but post-commit cleanup failed and stays retryable.
    CleanupPending,
}

spelled!(JournalState {
    Prepared => "prepared",
    Staged => "staged",
    Published => "published",
    Committed => "committed",
    Cleaned => "cleaned",
    Compensated => "compensated",
    NeedsAttention => "needs_attention",
    CleanupPending => "cleanup_pending",
});

impl JournalState {
    /// Whether the state ends the protocol without further work.
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Cleaned | Self::Compensated | Self::NeedsAttention
        )
    }

    /// Monotonic rank used to reject backward transitions.
    fn rank(self) -> u8 {
        match self {
            Self::Prepared => 0,
            Self::Staged => 1,
            Self::Published => 2,
            Self::Committed => 3,
            Self::Cleaned | Self::Compensated | Self::NeedsAttention | Self::CleanupPending => 4,
        }
    }

    /// Whether a transition from `self` to `next` is allowed.
    pub fn can_transition_to(self, next: Self) -> bool {
        if self.is_terminal() {
            return false;
        }
        match (self, next) {
            (from, to) if from == to => true,
            (Self::Prepared, Self::Staged) => true,
            (Self::Staged, Self::Published) => true,
            (Self::Published, Self::Committed) => true,
            (Self::Committed, Self::Cleaned | Self::CleanupPending) => true,
            (Self::CleanupPending, Self::Cleaned) => true,
            (Self::Prepared | Self::Staged | Self::Published, Self::Compensated) => true,
            (_, Self::NeedsAttention) => true,
            _ => next.rank() > self.rank(),
        }
    }
}

/// How the catalog commit records an audio row's track.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MgmtState {
    /// A library track the publish moved or retagged: its row moves.
    Managed,
    /// A file the commit adds to the catalog (an import), so a commit
    /// resumed after a crash adds it too.
    Imported,
}

spelled!(MgmtState {
    Managed => "managed",
    Imported => "imported",
});

/// What one journal row publishes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JournalKind {
    /// Managed audio file.
    Audio,
    /// Sidecar travelling with its album.
    Sidecar,
    /// External artwork file.
    Artwork,
    /// A file generated from no source (cover image, lyrics file).
    Output,
    /// Fingerprinted delete of generated art on undo: the staged copy
    /// is a safety backup and must never become a live destination.
    Delete,
    /// A library file leaving the library: set aside under the row's
    /// hidden name, then moved to its recycle bin entry after the commit.
    /// Its copy is never deleted, and goes home on any unwind.
    Remove,
    /// A removed file going home: its recycled copy is copied to the
    /// row's hidden temp beside home and placed there. The copy is never
    /// touched before the commit.
    PutBack,
    /// A file added with no track behind it (a plugin's file): written to
    /// the row's hidden temp and placed only onto a free name. Undo takes
    /// it away under a `Delete` row.
    Add,
}

spelled!(JournalKind {
    Audio => "audio",
    Sidecar => "sidecar",
    Artwork => "artwork",
    Output => "output",
    Delete => "delete",
    Remove => "remove",
    PutBack => "put_back",
    Add => "add",
});

/// One durable file intent: every path plus the fingerprints that let
/// recovery decide by byte comparison instead of path existence.
#[derive(Debug, Clone)]
pub struct FileJournal {
    /// Stable journal id, also embedded in temp/backup names.
    pub id: String,
    /// Album bundle this file publishes with.
    pub bundle_id: String,
    /// What this row publishes.
    pub kind: JournalKind,
    /// Source root and relative path (`None` for generated outputs).
    pub source: Option<(String, String)>,
    /// Destination root and relative path.
    pub dest_root: String,
    pub dest_rel: String,
    /// Absolute staged temp path.
    pub staged: String,
    /// Absolute same-path backup, when one was retained.
    pub backup: Option<String>,
    /// SHA-256 of the source bytes at plan time (`None` when generated).
    pub source_sha256: Option<String>,
    /// SHA-256 the destination must hold after publish.
    pub staged_sha256: String,
    /// Stable local track id, so recovery can rebuild the catalog commit.
    pub track_id: Option<String>,
    /// How the catalog commit records the row's track; `None` for rows
    /// that carry no track.
    pub commit_as: Option<MgmtState>,
    /// Current protocol state.
    pub state: JournalState,
    /// Monotonic per-file sequence, bumped on every transition.
    pub seq: i64,
    /// The file at the destination this journal replaces, when it does.
    pub replaces: Option<JournalReplacement>,
}

/// A file a journal replaces at its destination: set aside under the
/// journal's backup before the rename, recycled after commit, and put
/// back on any pre-commit unwind.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JournalReplacement {
    /// Where it stood, relative to the destination root.
    pub rel: String,
    /// The bytes it held when the bundle was planned.
    pub sha256: String,
    /// Its library track, retired in the catalog commit.
    pub track_id: Option<String>,
    /// Absolute recycle-bin path it moves to after commit.
    pub recycle_to: String,
}

impl FileJournal {
    /// The bytes this journal's backup must hold: the replaced file's,
    /// or the source's for a same-path write.
    pub fn backup_sha256(&self) -> Option<&str> {
        match &self.replaces {
            Some(replaced) => Some(replaced.sha256.as_str()),
            None => self.source_sha256.as_deref(),
        }
    }
}

/// The publish journal's reads, on any connection: a pool read, or the
/// transaction of a write that decides from them.
pub struct JournalStore<'a> {
    conn: &'a Connection,
}

impl<'a> JournalStore<'a> {
    /// Read the journal through `conn`.
    pub fn new(conn: &'a Connection) -> Self {
        Self { conn }
    }

    /// Load one journal by id.
    pub fn get(&self, id: &str) -> Result<Option<FileJournal>, PublishError> {
        self.conn
            .prepare_cached(sql::JOURNAL_BY_ID.text())?
            .query_row(rusqlite::params![id], read_journal)
            .optional()
            .map_err(PublishError::from)
    }

    /// Load every journal in a bundle, ordered by id for determinism.
    pub fn bundle(&self, bundle_id: &str) -> Result<Vec<FileJournal>, PublishError> {
        let mut stmt = self.conn.prepare_cached(sql::JOURNAL_BY_BUNDLE.text())?;
        let rows = stmt.query_map(rusqlite::params![bundle_id], read_journal)?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// Another bundle that still holds an unsettled journal for this
    /// track, if any.
    pub fn unsettled_bundle_for_track(
        &self,
        track_id: &str,
        except_bundle: &str,
    ) -> Result<Option<String>, PublishError> {
        Ok(self
            .conn
            .prepare_cached(sql::UNSETTLED_FOR_TRACK.text())?
            .query_row(
                rusqlite::params![
                    track_id,
                    except_bundle,
                    JournalState::Cleaned,
                    JournalState::Compensated,
                    JournalState::NeedsAttention,
                    JournalState::Prepared,
                    JournalState::Staged,
                    JournalState::Published,
                ],
                |row| row.get(0),
            )
            .optional()?)
    }

    /// Distinct bundle ids that still hold nonterminal journals,
    /// ordered for deterministic startup recovery.
    pub fn active_bundles(&self) -> Result<Vec<String>, PublishError> {
        let mut stmt = self.conn.prepare_cached(sql::ACTIVE_BUNDLES.text())?;
        let rows = stmt.query_map(
            rusqlite::params![
                JournalState::Cleaned,
                JournalState::Compensated,
                JournalState::NeedsAttention,
            ],
            |row| row.get::<_, String>(0),
        )?;
        Ok(rows.collect::<Result<_, _>>()?)
    }
}

/// The publish journal's writes. They take the transaction of a durable
/// write, so every journal row is on disk before the file step it
/// describes. A failed write here leaves the transaction to be rolled
/// back: no closure may catch one of these errors and return `Ok`.
pub struct JournalWriter<'a> {
    tx: &'a Durable<'a>,
}

impl<'a> JournalWriter<'a> {
    /// Write the journal inside the durable write `tx`.
    pub fn new(tx: &'a Durable<'a>) -> Self {
        Self { tx }
    }

    /// Insert a fresh intent in `Prepared` state, with the file it
    /// replaces. Bundle ids are new for every publish, so the row cannot
    /// exist yet; one that does fails the insert.
    pub fn insert(&self, journal: &FileJournal) -> Result<(), PublishError> {
        self.tx
            .prepare_cached(sql::JOURNAL_INSERT.text())?
            .execute(rusqlite::params![
                journal.id,
                journal.bundle_id,
                journal.kind.as_str(),
                journal.source.as_ref().map(|(root, _)| root.as_str()),
                journal.source.as_ref().map(|(_, rel)| rel.as_str()),
                journal.dest_root,
                journal.dest_rel,
                journal.staged,
                journal.backup.as_deref(),
                journal.source_sha256.as_deref(),
                journal.staged_sha256,
                journal.track_id.as_deref(),
                journal.commit_as.map(MgmtState::as_str),
                JournalState::Prepared.as_str(),
            ])?;
        if let Some(replaced) = &journal.replaces {
            self.tx
                .prepare_cached(sql::REPLACEMENT_INSERT.text())?
                .execute(rusqlite::params![
                    journal.id,
                    replaced.rel,
                    replaced.sha256,
                    replaced.track_id.as_deref(),
                    replaced.recycle_to,
                ])?;
        }
        Ok(())
    }

    /// Record that a replaced file reached the recycle bin, with its
    /// copy's identity there when it could be read.
    pub fn set_recycled(
        &self,
        id: &str,
        at: f64,
        binned: Option<&FileStat>,
    ) -> Result<(), PublishError> {
        let binned = binned.map(serde_json::to_string).transpose()?;
        self.tx
            .prepare_cached(sql::JOURNAL_SET_RECYCLED.text())?
            .execute(rusqlite::params![id, crate::clock::ms(at), binned])?;
        Ok(())
    }

    /// Point a replaced file that has not reached the bin yet at a fresh
    /// bin entry. False when the row is gone or already recycled.
    pub fn set_recycle_to(&self, id: &str, recycle_to: &str) -> Result<bool, PublishError> {
        let changed = self
            .tx
            .prepare_cached(sql::JOURNAL_SET_RECYCLE_TO.text())?
            .execute(rusqlite::params![id, recycle_to])?;
        Ok(changed == 1)
    }

    /// Point retired track `track_id`, whose path still names the old bin
    /// entry `from`, at the fresh entry `to`. A track whose path is
    /// anything else (put back, removed, moved since) keeps it.
    pub fn follow_rebin(&self, track_id: &str, from: &str, to: &str) -> Result<(), PublishError> {
        let changed = self
            .tx
            .prepare_cached(sql::TRACK_FOLLOW_REBIN.text())?
            .execute(rusqlite::params![track_id, from, to])?;
        if changed > 0 {
            crate::catalog_revision::bump_catalog(self.tx)?;
        }
        Ok(())
    }

    /// Move a row from `expected` to `next`, bumping its sequence. A row
    /// already at `next` is touched the same way, so the write always
    /// changes a row and syncs what came before it. A row in any other
    /// state, or no row, is an error: the file step after this must not
    /// run.
    pub fn advance(
        &self,
        id: &str,
        expected: JournalState,
        next: JournalState,
    ) -> Result<(), PublishError> {
        legal(expected, next)?;
        let changed = self
            .tx
            .prepare_cached(sql::JOURNAL_ADVANCE.text())?
            .execute(rusqlite::params![next.as_str(), id, expected.as_str()])?;
        if changed == 1 {
            return Ok(());
        }
        let state: Option<String> = self
            .tx
            .prepare_cached(sql::JOURNAL_STATE.text())?
            .query_row(rusqlite::params![id], |row| row.get(0))
            .optional()?;
        Err(PublishError::Journal(
            match state {
                Some(state) => format!(
                    "journal {id} is {state}, not {} or {}",
                    expected.as_str(),
                    next.as_str()
                ),
                None => format!("missing journal {id}"),
            }
            .into(),
        ))
    }

    /// Compare-and-swap transition: moves only when the row still holds
    /// `expected`, and bumps the monotonic sequence. Returns `false`
    /// when the row had already moved. For records that follow a file
    /// step and guard none.
    pub fn transition(
        &self,
        id: &str,
        expected: JournalState,
        next: JournalState,
    ) -> Result<bool, PublishError> {
        legal(expected, next)?;
        let changed = self
            .tx
            .prepare_cached(sql::JOURNAL_TRANSITION.text())?
            .execute(rusqlite::params![next.as_str(), id, expected.as_str()])?;
        Ok(changed == 1)
    }

    /// Move a row from `Prepared` to `Staged` with the hash of the bytes
    /// its temp now holds, which a tag write learns only once the temp is
    /// written. A row in any other state, or no row, is an error.
    pub fn stage(&self, id: &str, staged_sha256: &str) -> Result<(), PublishError> {
        let changed =
            self.tx
                .prepare_cached(sql::JOURNAL_STAGE.text())?
                .execute(rusqlite::params![
                    JournalState::Staged.as_str(),
                    id,
                    JournalState::Prepared.as_str(),
                    staged_sha256
                ])?;
        if changed != 1 {
            return Err(PublishError::Journal(
                format!("journal {id} is not prepared, or is missing").into(),
            ));
        }
        Ok(())
    }

    /// Record the absolute backup path retained for a same-path write. A
    /// row that already names this path is touched again; one naming
    /// another path, or no row, is an error.
    pub fn set_backup(&self, id: &str, backup: &str) -> Result<(), PublishError> {
        let changed = self
            .tx
            .prepare_cached(sql::JOURNAL_SET_BACKUP.text())?
            .execute(rusqlite::params![backup, id])?;
        if changed != 1 {
            return Err(PublishError::Journal(
                format!("journal {id} names another backup, or is missing").into(),
            ));
        }
        Ok(())
    }
}

fn legal(expected: JournalState, next: JournalState) -> Result<(), PublishError> {
    if expected.can_transition_to(next) {
        return Ok(());
    }
    Err(PublishError::Journal(
        format!(
            "illegal transition {} -> {}",
            expected.as_str(),
            next.as_str()
        )
        .into(),
    ))
}

fn read_journal(row: &rusqlite::Row<'_>) -> Result<FileJournal, rusqlite::Error> {
    let row = JournalRow::read(row)?;
    let replaces = match (row.replaced_rel, row.replaced_sha256, row.recycle_to) {
        (Some(rel), Some(sha256), Some(recycle_to)) => Some(JournalReplacement {
            rel,
            sha256,
            track_id: row.replaced_track_id,
            recycle_to,
        }),
        _ => None,
    };
    Ok(FileJournal {
        id: row.id,
        bundle_id: row.bundle_id,
        kind: row.kind,
        source: row.source_root.zip(row.source_rel),
        dest_root: row.dest_root,
        dest_rel: row.dest_rel,
        staged: row.staged,
        backup: row.backup,
        source_sha256: row.source_sha256,
        staged_sha256: row.staged_sha256,
        track_id: row.track_id,
        commit_as: row.commit_as,
        state: row.state,
        seq: row.seq,
        replaces,
    })
}

/// Durably persist a directory entry change (rename, unlink, create).
/// Publication calls this on the destination directory after each
/// rename batch and on temp directories after staging.
pub fn fsync_dir(path: &Path) -> Result<(), PublishError> {
    let file = std::fs::File::open(path).map_err(PublishError::from)?;
    file.sync_all().map_err(PublishError::from)?;
    Ok(())
}

/// Durably persist one file's bytes before its directory entry moves.
pub fn fsync_file(file: &std::fs::File) -> Result<(), PublishError> {
    file.sync_all().map_err(PublishError::from)?;
    Ok(())
}
