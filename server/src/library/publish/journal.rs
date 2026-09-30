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
}

/// SQLite over the journal, snapshot, baseline, catalog-shadow, and
/// invalidation tables. One database, one bundle commit transaction.
pub struct JournalStore<'a> {
    conn: &'a Connection,
}

impl<'a> JournalStore<'a> {
    /// Borrow a connection as a journal store.
    pub fn new(conn: &'a Connection) -> Self {
        Self { conn }
    }

    /// Insert a fresh intent in `Prepared` state.
    pub fn insert(&self, journal: &FileJournal) -> Result<(), PublishError> {
        self.conn.execute(
            "INSERT INTO publish_journal
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
            "UPDATE publish_journal SET state = ?1, seq = seq + 1
             WHERE id = ?2 AND state = ?3",
            rusqlite::params![next.as_str(), id, expected.as_str()],
        )?;
        Ok(changed == 1)
    }

    /// Record the absolute backup path retained for a same-path write.
    pub fn set_backup(&self, id: &str, backup: &str) -> Result<(), PublishError> {
        self.conn.execute(
            "UPDATE publish_journal SET backup = ?1 WHERE id = ?2",
            rusqlite::params![backup, id],
        )?;
        Ok(())
    }

    /// Load one journal by id.
    pub fn get(&self, id: &str) -> Result<Option<FileJournal>, PublishError> {
        self.conn
            .query_row(
                "SELECT id, bundle_id, kind, source_root, source_rel, dest_root,
                        dest_rel, staged, backup, source_sha256, staged_sha256,
                        track_id, catalog_revision, mgmt_state, state, seq
                 FROM publish_journal WHERE id = ?1",
                rusqlite::params![id],
                read_journal,
            )
            .optional()
            .map_err(PublishError::from)
    }

    /// Load every journal in a bundle, ordered by id for determinism.
    pub fn bundle(&self, bundle_id: &str) -> Result<Vec<FileJournal>, PublishError> {
        let mut stmt = self.conn.prepare(
            "SELECT id, bundle_id, kind, source_root, source_rel, dest_root,
                    dest_rel, staged, backup, source_sha256, staged_sha256,
                    track_id, catalog_revision, mgmt_state, state, seq
             FROM publish_journal WHERE bundle_id = ?1 ORDER BY id",
        )?;
        let rows = stmt.query_map(rusqlite::params![bundle_id], read_journal)?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    /// Distinct bundle ids that still hold nonterminal journals,
    /// ordered for deterministic startup recovery.
    pub fn active_bundles(&self) -> Result<Vec<String>, PublishError> {
        let mut stmt = self.conn.prepare(
            "SELECT DISTINCT bundle_id FROM publish_journal
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
    })
}

/// Create the slice tables. Idempotent so scratch stores and the wired
/// build can both apply it safely.
pub fn apply_schema(conn: &Connection) -> Result<(), PublishError> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS publish_journal (
           id TEXT PRIMARY KEY,
           bundle_id TEXT NOT NULL,
           kind TEXT NOT NULL,
           source_root TEXT,
           source_rel TEXT,
           dest_root TEXT NOT NULL,
           dest_rel TEXT NOT NULL,
           staged TEXT NOT NULL,
           backup TEXT,
           source_sha256 TEXT,
           staged_sha256 TEXT NOT NULL,
           track_id TEXT,
           catalog_revision INTEGER,
           mgmt_state TEXT,
           state TEXT NOT NULL,
           seq INTEGER NOT NULL DEFAULT 0
         );
         CREATE INDEX IF NOT EXISTS idx_publish_journal_bundle
           ON publish_journal(bundle_id);
         CREATE INDEX IF NOT EXISTS idx_publish_journal_state
           ON publish_journal(state);
         CREATE TABLE IF NOT EXISTS publish_blob_refs (
           sha256 TEXT NOT NULL,
           owner_kind TEXT NOT NULL,
           owner_id TEXT NOT NULL,
           PRIMARY KEY (sha256, owner_kind, owner_id)
         );
         CREATE TABLE IF NOT EXISTS publish_snapshots (
           id TEXT PRIMARY KEY,
           bundle_id TEXT NOT NULL,
           track_id TEXT NOT NULL,
           blob_sha256 TEXT NOT NULL,
           created_day INTEGER NOT NULL,
           expires_day INTEGER NOT NULL
         );
         CREATE INDEX IF NOT EXISTS idx_publish_snapshots_bundle
           ON publish_snapshots(bundle_id);
         CREATE TABLE IF NOT EXISTS publish_baselines (
           track_id TEXT PRIMARY KEY,
           blob_sha256 TEXT NOT NULL,
           original_root TEXT NOT NULL,
           original_rel TEXT NOT NULL,
           created_day INTEGER NOT NULL
         );
         CREATE TABLE IF NOT EXISTS publish_catalog_shadow (
           track_id TEXT PRIMARY KEY,
           root_id TEXT NOT NULL,
           rel_path TEXT NOT NULL,
           fingerprint TEXT NOT NULL,
           mgmt_state TEXT NOT NULL,
           revision INTEGER NOT NULL DEFAULT 0
         );
         CREATE TABLE IF NOT EXISTS publish_catalog_meta (
           id INTEGER PRIMARY KEY CHECK (id = 1),
           revision INTEGER NOT NULL DEFAULT 0
         );
         INSERT INTO publish_catalog_meta (id, revision)
           SELECT 1, 0 WHERE NOT EXISTS (SELECT 1 FROM publish_catalog_meta);
         CREATE TABLE IF NOT EXISTS publish_invalidations (
           id INTEGER PRIMARY KEY AUTOINCREMENT,
           bundle_id TEXT NOT NULL,
           track_id TEXT NOT NULL,
           created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now'))
         );
         CREATE TABLE IF NOT EXISTS publish_operations (
           bundle_id TEXT PRIMARY KEY,
           bundle_json TEXT NOT NULL,
           created_day INTEGER NOT NULL
         );",
    )?;
    Ok(())
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
