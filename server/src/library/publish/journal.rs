//! Durable per-file journals with a monotonic state machine.
//!
//! Every audio file, sidecar, and external-art output owns one journal
//! row. States only move forward along the prepare/publish/commit/
//! cleanup protocol, and every transition is compare-and-swap on the
//! current state so a crash between the filesystem step and its SQLite
//! transition is visible to recovery instead of silently lost.

use std::path::Path;

use rusqlite::{Connection, OptionalExtension};

use super::PublishError;

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

impl JournalState {
    /// Text form stored in SQLite.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Prepared => "prepared",
            Self::Staged => "staged",
            Self::Published => "published",
            Self::Committed => "committed",
            Self::Cleaned => "cleaned",
            Self::Compensated => "compensated",
            Self::NeedsAttention => "needs_attention",
            Self::CleanupPending => "cleanup_pending",
        }
    }

    /// Parse the stored text form.
    pub fn parse(text: &str) -> Result<Self, PublishError> {
        match text {
            "prepared" => Ok(Self::Prepared),
            "staged" => Ok(Self::Staged),
            "published" => Ok(Self::Published),
            "committed" => Ok(Self::Committed),
            "cleaned" => Ok(Self::Cleaned),
            "compensated" => Ok(Self::Compensated),
            "needs_attention" => Ok(Self::NeedsAttention),
            "cleanup_pending" => Ok(Self::CleanupPending),
            other => Err(PublishError::Journal(format!(
                "unknown journal state {other}"
            ))),
        }
    }

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

/// What one journal row publishes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JournalKind {
    /// Managed audio file.
    Audio,
    /// Sidecar travelling with its album.
    Sidecar,
    /// External artwork file.
    Artwork,
    /// Fingerprinted delete of generated art on undo: the staged copy
    /// is a safety backup and must never become a live destination.
    Delete,
}

impl JournalKind {
    /// Text form stored in SQLite.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Audio => "audio",
            Self::Sidecar => "sidecar",
            Self::Artwork => "artwork",
            Self::Delete => "delete",
        }
    }

    /// Parse the stored text form.
    pub fn parse(text: &str) -> Result<Self, PublishError> {
        match text {
            "audio" => Ok(Self::Audio),
            "sidecar" => Ok(Self::Sidecar),
            "artwork" => Ok(Self::Artwork),
            "delete" => Ok(Self::Delete),
            other => Err(PublishError::Journal(format!(
                "unknown journal kind {other}"
            ))),
        }
    }
}

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
    /// Catalog revision the bundle commit compare-and-swaps on.
    pub catalog_revision: Option<i64>,
    /// Management state the catalog commit records.
    pub mgmt_state: Option<String>,
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

const JOURNAL_COLUMNS: &str = "j.id, j.bundle_id, j.kind, j.source_root, j.source_rel, \
     j.dest_root, j.dest_rel, j.staged, j.backup, j.source_sha256, j.staged_sha256, \
     j.track_id, j.catalog_revision, j.mgmt_state, j.state, j.seq, \
     r.replaced_rel, r.replaced_sha256, r.replaced_track_id, r.recycle_to \
     FROM library_publish_journal j \
     LEFT JOIN library_publish_replacements r ON r.journal_id = j.id";

/// The publish journal in the application database, next to the catalog
/// a bundle commits against in the same transaction.
pub struct JournalStore<'a> {
    conn: &'a Connection,
}

impl<'a> JournalStore<'a> {
    /// Borrow a connection as a journal store.
    pub fn new(conn: &'a Connection) -> Self {
        Self { conn }
    }

    /// Insert a fresh intent in `Prepared` state, with the file it
    /// replaces in the same savepoint.
    pub fn insert(&self, journal: &FileJournal) -> Result<(), PublishError> {
        self.conn.execute_batch("SAVEPOINT journal_insert")?;
        let inserted = self.insert_rows(journal);
        match inserted {
            Ok(()) => self.conn.execute_batch("RELEASE journal_insert")?,
            Err(_) => self
                .conn
                .execute_batch("ROLLBACK TO journal_insert; RELEASE journal_insert")?,
        }
        inserted
    }

    fn insert_rows(&self, journal: &FileJournal) -> Result<(), PublishError> {
        self.conn.execute(
            "INSERT INTO library_publish_journal
             (id, bundle_id, kind, source_root, source_rel, dest_root, dest_rel,
              staged, backup, source_sha256, staged_sha256, track_id,
              catalog_revision, mgmt_state, state, seq)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12,
                     ?13, ?14, ?15, 0)",
            rusqlite::params![
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
                journal.catalog_revision,
                journal.mgmt_state.as_deref(),
                journal.state.as_str(),
            ],
        )?;
        if let Some(replaced) = &journal.replaces {
            self.conn.execute(
                "INSERT INTO library_publish_replacements
                 (journal_id, replaced_rel, replaced_sha256, replaced_track_id, recycle_to)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                rusqlite::params![
                    journal.id,
                    replaced.rel,
                    replaced.sha256,
                    replaced.track_id.as_deref(),
                    replaced.recycle_to,
                ],
            )?;
        }
        Ok(())
    }

    /// Record that a replaced file reached the recycle bin.
    pub fn set_recycled(&self, id: &str, at: f64) -> Result<(), PublishError> {
        self.conn.execute(
            "UPDATE library_publish_replacements SET recycled_at = ?2 WHERE journal_id = ?1",
            rusqlite::params![id, at],
        )?;
        Ok(())
    }

    /// Compare-and-swap transition: moves only when the row still holds
    /// `expected`, and bumps the monotonic sequence. Returns `false`
    /// when another worker (or recovery) already moved the row.
    pub fn transition(
        &self,
        id: &str,
        expected: JournalState,
        next: JournalState,
    ) -> Result<bool, PublishError> {
        if !expected.can_transition_to(next) {
            return Err(PublishError::Journal(format!(
                "illegal transition {} -> {}",
                expected.as_str(),
                next.as_str()
            )));
        }
        let changed = self.conn.execute(
            "UPDATE library_publish_journal SET state = ?1, seq = seq + 1
             WHERE id = ?2 AND state = ?3",
            rusqlite::params![next.as_str(), id, expected.as_str()],
        )?;
        Ok(changed == 1)
    }

    /// Record the absolute backup path retained for a same-path write.
    pub fn set_backup(&self, id: &str, backup: &str) -> Result<(), PublishError> {
        self.conn.execute(
            "UPDATE library_publish_journal SET backup = ?1 WHERE id = ?2",
            rusqlite::params![backup, id],
        )?;
        Ok(())
    }

    /// Load one journal by id.
    pub fn get(&self, id: &str) -> Result<Option<FileJournal>, PublishError> {
        self.conn
            .query_row(
                &format!("SELECT {JOURNAL_COLUMNS} WHERE j.id = ?1"),
                rusqlite::params![id],
                read_journal,
            )
            .optional()
            .map_err(PublishError::from)
    }

    /// Load every journal in a bundle, ordered by id for determinism.
    pub fn bundle(&self, bundle_id: &str) -> Result<Vec<FileJournal>, PublishError> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {JOURNAL_COLUMNS} WHERE j.bundle_id = ?1 ORDER BY j.id"
        ))?;
        let rows = stmt.query_map(rusqlite::params![bundle_id], read_journal)?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    /// Distinct bundle ids that still hold nonterminal journals,
    /// ordered for deterministic startup recovery.
    /// Another bundle that still holds an unsettled journal for this
    /// track, if any.
    pub fn unsettled_bundle_for_track(
        &self,
        track_id: &str,
        except_bundle: &str,
    ) -> Result<Option<String>, PublishError> {
        use rusqlite::OptionalExtension as _;
        Ok(self
            .conn
            .query_row(
                "SELECT bundle_id FROM library_publish_journal
                 WHERE track_id = ?1 AND bundle_id <> ?2
                 AND state NOT IN ('cleaned', 'compensated', 'needs_attention')
                 LIMIT 1",
                rusqlite::params![track_id, except_bundle],
                |row| row.get(0),
            )
            .optional()?)
    }

    pub fn active_bundles(&self) -> Result<Vec<String>, PublishError> {
        let mut stmt = self.conn.prepare(
            "SELECT DISTINCT bundle_id FROM library_publish_journal
             WHERE state NOT IN ('cleaned', 'compensated', 'needs_attention')
             ORDER BY bundle_id",
        )?;
        let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }
}

fn read_journal(row: &rusqlite::Row<'_>) -> Result<FileJournal, rusqlite::Error> {
    let state_text: String = row.get(14)?;
    let kind_text: String = row.get(2)?;
    let state = JournalState::parse(&state_text).map_err(|err| {
        rusqlite::Error::FromSqlConversionFailure(14, rusqlite::types::Type::Text, Box::new(err))
    })?;
    let kind = JournalKind::parse(&kind_text).map_err(|err| {
        rusqlite::Error::FromSqlConversionFailure(2, rusqlite::types::Type::Text, Box::new(err))
    })?;
    let source_root: Option<String> = row.get(3)?;
    let source_rel: Option<String> = row.get(4)?;
    let replaced_rel: Option<String> = row.get(16)?;
    let replaced_sha: Option<String> = row.get(17)?;
    let recycle_to: Option<String> = row.get(19)?;
    let replaces = match (replaced_rel, replaced_sha, recycle_to) {
        (Some(rel), Some(sha256), Some(recycle_to)) => Some(JournalReplacement {
            rel,
            sha256,
            track_id: row.get(18)?,
            recycle_to,
        }),
        _ => None,
    };
    Ok(FileJournal {
        id: row.get(0)?,
        bundle_id: row.get(1)?,
        kind,
        source: source_root.zip(source_rel),
        dest_root: row.get(5)?,
        dest_rel: row.get(6)?,
        staged: row.get(7)?,
        backup: row.get(8)?,
        source_sha256: row.get(9)?,
        staged_sha256: row.get(10)?,
        track_id: row.get(11)?,
        catalog_revision: row.get(12)?,
        mgmt_state: row.get(13)?,
        state,
        seq: row.get(15)?,
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
