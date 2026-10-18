//! Staged publisher: prepare, publish, catalog commit, cleanup.
//!
//! Apply consumes the exact sealed preview. Every phase runs its
//! gates first: capability, collision, snapshot, journal, validation,
//! catalog commit, cleanup, and cache invalidation. Manual Apply
//! rejects stale file, identity, profile, or policy state before a
//! single byte moves; late collisions and catalog failures compensate
//! instead of leaving half-renamed bundles.
//!
//! Crash injection (`CrashPoint`) stops the protocol between durable
//! steps so the tests can prove that restart recovery always resumes
//! or compensates. Production never sets a crash point.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use droppedneedle_kernel::db::{CancelFlag, Db, Durable, DurableLane, Lane};
use rusqlite::{Connection, OptionalExtension};

use super::journal::{
    FileJournal, JournalKind, JournalReplacement, JournalState, JournalStore, JournalWriter,
};
use super::paths::Sandbox;
use super::place::Original;
use super::planner::{
    AlbumJoin, CapabilityGate, CollisionGate, DiskPreflight, JoinedTrack, PlanBundle, PlanKind,
    SealError, SealRecheck, SealedPreview, SpaceProbe,
};
use super::snapshots::{BlobStore, SnapshotStore, sha256_hex};
use super::tags_seam::TagDocument;
use super::{PublishError, paths};

/// One track row in the atomic catalog commit.
#[derive(Debug, Clone)]
pub struct TrackCommit {
    /// Stable local track id.
    pub track_id: String,
    /// Adopted root and relative path.
    pub root_id: String,
    pub rel_path: String,
    /// Adopted absolute path, stored as the track's `file_path`.
    pub file_path: String,
    /// SHA-256 the catalog must record.
    pub fingerprint: String,
    /// Where the write read the file it replaces, and the SHA-256 it
    /// verified there. The commit keeps the app's old record instead of
    /// `fingerprint` when those bytes were changed since the app wrote
    /// them (see [`super::record::written_after`]).
    pub source: Option<(String, String)>,
    pub source_sha256: Option<String>,
    /// Management state after commit.
    pub mgmt_state: String,
    /// The track is new to the catalog: index the published file instead
    /// of moving an existing row (see [`ADOPTED_STATE`]).
    pub adopt: Option<super::planner::Adoption>,
    /// The track whose file this one replaced at its destination, retired
    /// in the same commit, and where in the recycle bin that file goes.
    pub replaces: Option<(String, String)>,
    /// What an adopted file indexes as, read from the published file
    /// before the commit's transaction by [`read_adopted`].
    pub indexed: Option<crate::scan::store::CommitIndexedItem>,
}

/// Management-state prefix journals carry for files a commit adds to the
/// catalog, so a resumed commit after a crash adopts them too.
pub const ADOPTED_STATE: &str = "imported:";

/// The management state a commit records for one plan item.
fn mgmt_state_for(adopt: bool, profile_revision: u64) -> String {
    if adopt {
        format!("{ADOPTED_STATE}{profile_revision}")
    } else {
        format!("managed:{profile_revision}")
    }
}

/// The single-transaction catalog bundle commit.
#[derive(Debug, Clone)]
pub struct BundleCommit {
    /// Bundle id for history and invalidation.
    pub bundle_id: String,
    /// Catalog revision the plan was built against; the commit
    /// compare-and-swaps on it.
    pub expected_catalog_revision: u64,
    /// Track adoptions in this bundle.
    pub tracks: Vec<TrackCommit>,
}

/// Catalog port: where a managed track lives and what the publisher
/// last wrote there. Production is [`SqliteCatalog`], the catalog tables
/// the scan writes and the reads API serves.
pub trait Catalog {
    /// Current catalog revision.
    fn revision(&self, conn: &Connection) -> Result<u64, PublishError>;
    /// Locate a track: (root id, relative path, last published SHA-256,
    /// management state). The last two are empty for a track the
    /// publisher never wrote.
    fn locate(
        &self,
        conn: &Connection,
        track_id: &str,
    ) -> Result<Option<(String, String, String, String)>, PublishError>;
    /// Commit one bundle: CAS on the expected revision, move every track
    /// row (and its album's root) to its published location, bump the
    /// catalog once. Track and album ids stay as they are. Files an
    /// import fetched for a library album join it, and are placed on its
    /// edition in the same transaction.
    fn commit_bundle(&self, tx: &Durable<'_>, commit: &BundleCommit) -> Result<(), PublishError>;
    /// Mark a track missing so streaming and search stay accurate when
    /// committed bytes cannot be recovered.
    fn mark_missing(&self, conn: &Connection, track_id: &str) -> Result<(), PublishError>;
}

/// Flush a directory entry change where a failure must not undo work
/// already done (cleanup and compensation): the next sync or recovery
/// pass covers it, so it is logged instead of returned.
fn sync_dir_logged(dir: &Path) {
    if let Err(error) = super::journal::fsync_dir(dir) {
        tracing::warn!(dir = %dir.display(), %error, "directory sync failed");
    }
}

fn now_secs() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|span| span.as_secs_f64())
        .unwrap_or(0.0)
}

/// The application catalog: `local_tracks` holds where a track lives,
/// `library_track_management_state` what the publisher last wrote, and
/// `library_catalog_revision` the compare-and-swap counter (the same one
/// read caches key on, so a commit invalidates them).
#[derive(Debug, Clone, Default)]
pub struct SqliteCatalog;

impl Catalog for SqliteCatalog {
    fn revision(&self, conn: &Connection) -> Result<u64, PublishError> {
        let rev: i64 = conn.query_row(
            "SELECT value FROM library_catalog_revision WHERE singleton = 1",
            [],
            |row| row.get(0),
        )?;
        Ok(rev as u64)
    }

    fn locate(
        &self,
        conn: &Connection,
        track_id: &str,
    ) -> Result<Option<(String, String, String, String)>, PublishError> {
        conn.query_row(
            "SELECT t.root_id, t.relative_path, COALESCE(m.applied_projection_hash, ''), \
             COALESCE(m.last_outcome, '') \
             FROM local_tracks t \
             LEFT JOIN library_track_management_state m ON m.local_track_id = t.id \
             WHERE t.id = ?1",
            rusqlite::params![track_id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                ))
            },
        )
        .optional()
        .map_err(PublishError::from)
    }

    fn commit_bundle(&self, tx: &Durable<'_>, commit: &BundleCommit) -> Result<(), PublishError> {
        let conn: &Connection = tx;
        let changed = conn.execute(
            "UPDATE library_catalog_revision SET value = value + 1
             WHERE singleton = 1 AND value = ?1",
            rusqlite::params![commit.expected_catalog_revision as i64],
        )?;
        if changed != 1 {
            return Err(PublishError::Catalog(format!(
                "catalog moved under bundle {}",
                commit.bundle_id
            )));
        }
        let now = now_secs();
        // `written_sha256` is the app's record of the bytes it placed: a
        // file that no longer has them was changed outside the app. A write
        // over a file already changed outside (a person's rename, say)
        // keeps the old record, so the change stays the user's. Judged
        // before any row of the bundle moves.
        let mut written = Vec::with_capacity(commit.tracks.len());
        for track in &commit.tracks {
            written.push(if track.adopt.is_some() {
                Some(track.fingerprint.clone())
            } else {
                super::record::written_after(
                    conn,
                    &track.track_id,
                    track
                        .source
                        .as_ref()
                        .map(|(root, rel)| (root.as_str(), rel.as_str())),
                    track.source_sha256.as_deref(),
                    &track.fingerprint,
                )?
            });
        }
        // Joined files to place, by album, with the release they join.
        let mut placing: BTreeMap<String, (String, Vec<(String, JoinedTrack)>)> = BTreeMap::new();
        for (track, written) in commit.tracks.iter().zip(&written) {
            if let Some((replaced, recycle_to)) = &track.replaces {
                retire_replaced(conn, track, replaced, recycle_to, now)?;
            }
            if let Some(adoption) = &track.adopt {
                let joined = adopt_track(conn, track, adoption)?;
                if let (Some(joined), Some(join), Some(on)) =
                    (joined, &adoption.join, &adoption.track)
                    && joined.placed
                {
                    placing
                        .entry(joined.album_id)
                        .or_insert_with(|| (join.release_mbid.clone(), Vec::new()))
                        .1
                        .push((track.track_id.clone(), on.clone()));
                }
            } else {
                give_up_destination(conn, track)?;
                move_track(conn, track)?;
            }
            conn.execute(
                "INSERT INTO library_track_management_state (local_track_id, managed_root_id, \
                 applied_projection_hash, written_sha256, last_outcome, last_managed_at) \
                 VALUES (?1, ?2, ?3, ?6, ?4, ?5) \
                 ON CONFLICT (local_track_id) DO UPDATE SET \
                 managed_root_id = excluded.managed_root_id, \
                 applied_projection_hash = excluded.applied_projection_hash, \
                 written_sha256 = excluded.written_sha256, \
                 last_outcome = excluded.last_outcome, \
                 last_managed_at = excluded.last_managed_at, \
                 row_revision = library_track_management_state.row_revision + 1",
                rusqlite::params![
                    track.track_id,
                    track.root_id,
                    track.fingerprint,
                    track.mgmt_state,
                    now,
                    written
                ],
            )?;
        }
        for (album, (release, files)) in &placing {
            crate::operations::choice::place_imported(tx, album, release, files, now)?;
        }
        Ok(())
    }

    fn mark_missing(&self, conn: &Connection, track_id: &str) -> Result<(), PublishError> {
        conn.execute(
            "UPDATE local_tracks SET availability = 'missing', missing_since = ?2 WHERE id = ?1",
            rusqlite::params![track_id, now_secs()],
        )?;
        conn.execute(
            "UPDATE library_catalog_revision SET value = value + 1 WHERE singleton = 1",
            [],
        )?;
        Ok(())
    }
}

/// Retire the track whose file a commit replaced: it stays in the
/// catalog as a missing track whose file is in the recycle bin. When the
/// file that took its place is on the same album and mapped to the same
/// release track, the end of the next scan moves its plays, favorites and
/// playlist entries to that track and its id opens it. Its path
/// moves to a hidden name under the bundle, so the new file can hold the
/// real one.
fn retire_replaced(
    conn: &Connection,
    track: &TrackCommit,
    replaced: &str,
    recycle_to: &str,
    now: f64,
) -> Result<(), PublishError> {
    let retired = format!(
        "{}replaced/{}/{}",
        super::HIDDEN_PREFIX,
        track.track_id,
        track.rel_path
    );
    conn.execute(
        "UPDATE local_tracks SET availability = 'missing', missing_since = ?5, \
         relative_path = ?4, file_path = ?6, path_hash = ?7, \
         row_revision = row_revision + 1 \
         WHERE id = ?1 AND root_id = ?2 AND relative_path = ?3",
        rusqlite::params![
            replaced,
            track.root_id,
            track.rel_path,
            retired,
            now,
            recycle_to,
            sha256_hex(retired.as_bytes())
        ],
    )?;
    // Its file is in the recycle bin: no record of it may make a file on
    // its album look like it took its place.
    conn.execute(
        "UPDATE library_track_management_state SET written_sha256 = NULL, \
         managed_path_revision = NULL, row_revision = row_revision + 1 \
         WHERE local_track_id = ?1",
        rusqlite::params![replaced],
    )?;
    Ok(())
}

/// Free the destination of a track the commit moves. The file is already
/// placed there and placement refuses a taken name, so another row still
/// on that path describes a file that is gone. A missing or excluded row
/// gives the path up for a hidden name of its own and keeps everything
/// else: its id, availability, `missing_since`, album and record. A row
/// the catalog still lists as indexed fails the commit until a scan has
/// seen its file go.
fn give_up_destination(conn: &Connection, track: &TrackCommit) -> Result<(), PublishError> {
    let occupant: Option<(String, String)> = conn
        .query_row(
            "SELECT id, availability FROM local_tracks \
             WHERE root_id = ?1 AND relative_path = ?2 AND id <> ?3",
            rusqlite::params![track.root_id, track.rel_path, track.track_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    let Some((occupant, availability)) = occupant else {
        return Ok(());
    };
    if availability == "indexed" {
        return Err(PublishError::Catalog(format!(
            "track {occupant} is still listed at the destination of track {}",
            track.track_id
        )));
    }
    let hidden = format!(
        "{}gone/{}/{}",
        super::HIDDEN_PREFIX,
        occupant,
        track.rel_path
    );
    // The hidden name under the same root, so the old row never points
    // at the file placed on its old path.
    let depth = Path::new(&track.rel_path).components().count();
    let root_dir = Path::new(&track.file_path)
        .ancestors()
        .nth(depth)
        .ok_or_else(|| PublishError::Catalog(format!("track {} has no root", track.track_id)))?;
    let file_path = root_dir.join(&hidden).to_string_lossy().into_owned();
    conn.execute(
        "UPDATE local_tracks SET relative_path = ?2, file_path = ?3, path_hash = ?4, \
         row_revision = row_revision + 1 WHERE id = ?1",
        rusqlite::params![occupant, hidden, file_path, sha256_hex(hidden.as_bytes())],
    )?;
    tracing::info!(
        track_id = %occupant,
        placed = %track.track_id,
        "a track whose file is gone gave up its path"
    );
    Ok(())
}

/// Move an existing track row to its published location. An organize
/// that moves a whole album into another root takes the album row along;
/// track and album ids (and with them the identity) stay.
fn move_track(conn: &Connection, track: &TrackCommit) -> Result<(), PublishError> {
    let moved = conn.execute(
        "UPDATE local_tracks SET root_id = ?2, relative_path = ?3, file_path = ?4, \
         path_hash = ?5, row_revision = row_revision + 1 WHERE id = ?1",
        rusqlite::params![
            track.track_id,
            track.root_id,
            track.rel_path,
            track.file_path,
            sha256_hex(track.rel_path.as_bytes())
        ],
    )?;
    if moved != 1 {
        return Err(PublishError::Catalog(format!(
            "track {} is not in the catalog",
            track.track_id
        )));
    }
    conn.execute(
        "UPDATE local_albums SET root_id = ?2, row_revision = row_revision + 1 \
         WHERE id = (SELECT local_album_id FROM local_tracks WHERE id = ?1) \
         AND root_id <> ?2 AND NOT EXISTS (SELECT 1 FROM local_tracks t \
         WHERE t.local_album_id = local_albums.id AND t.root_id <> ?2 \
         AND t.availability = 'indexed')",
        rusqlite::params![track.track_id, track.root_id],
    )?;
    Ok(())
}

/// The album files fetched for a library copy go to: the copy, followed
/// to the album it was merged into, while it still names the join's
/// release (`placed`) or another release of its group. `None` when the
/// album is gone, has no MusicBrainz identity or names another group: the
/// files are then filed by their tags.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct JoinedAlbum {
    pub album_id: String,
    pub placed: bool,
}

pub(crate) fn joined_album(
    conn: &Connection,
    join: &AlbumJoin,
) -> rusqlite::Result<Option<JoinedAlbum>> {
    let found: Option<(String, Option<String>, Option<String>)> = conn
        .query_row(
            "SELECT b.id, i.release_mbid, i.release_group_mbid FROM local_albums b \
             LEFT JOIN local_album_external_identities i \
             ON i.local_album_id = b.id AND i.provider = 'musicbrainz' \
             WHERE b.id = (SELECT COALESCE(retired_into_album_id, id) FROM local_albums \
             WHERE id = ?1)",
            rusqlite::params![join.album_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()?;
    let Some((album_id, release, group)) = found else {
        return Ok(None);
    };
    let names = |held: &Option<String>, wanted: &str| {
        !wanted.trim().is_empty()
            && held
                .as_deref()
                .is_some_and(|held| held.trim().eq_ignore_ascii_case(wanted.trim()))
    };
    Ok(if names(&release, &join.release_mbid) {
        Some(JoinedAlbum {
            album_id,
            placed: true,
        })
    } else if names(&group, &join.release_group_mbid) {
        Some(JoinedAlbum {
            album_id,
            placed: false,
        })
    } else {
        None
    })
}

/// Add a file the bundle brought into the library: index the published
/// file from its own tags and header, exactly as a scan would, at the
/// track id the plan pinned. A file fetched for a library album joins it
/// (see [`joined_album`]) and is locked there, as the album tools' move
/// locks a track, so a later scan does not refile it by its tags. With a
/// join, a row at the destination that the plan did not adopt gives up
/// its path first ([`give_up_destination`]); a row it did adopt must
/// still be on the joined album, and loses the track identity of the file
/// it held. Answers where the file joined.
fn adopt_track(
    conn: &Connection,
    track: &TrackCommit,
    adoption: &super::planner::Adoption,
) -> Result<Option<JoinedAlbum>, PublishError> {
    use crate::scan::sqlite_store::Adopted;

    let joined = match &adoption.join {
        Some(join) => joined_album(conn, join)?,
        None => None,
    };
    if adoption.join.is_some() {
        let at_path: Option<(String, String)> = conn
            .query_row(
                "SELECT id, local_album_id FROM local_tracks \
                 WHERE root_id = ?1 AND relative_path = ?2",
                rusqlite::params![track.root_id, track.rel_path],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        match at_path {
            Some((id, album)) if id == track.track_id => {
                if joined
                    .as_ref()
                    .is_none_or(|joined| joined.album_id != album)
                {
                    return Err(PublishError::Catalog(format!(
                        "track {id} is no longer on the album it was fetched for"
                    )));
                }
                // It described the file this one replaced.
                conn.execute(
                    "DELETE FROM local_track_external_identities \
                     WHERE local_track_id = ?1 AND provider = 'musicbrainz'",
                    rusqlite::params![id],
                )?;
            }
            Some(_) => give_up_destination(conn, track)?,
            None => {}
        }
    }

    let item = track.indexed.as_ref().ok_or_else(|| {
        PublishError::Catalog(format!(
            "track {} was not read before its commit",
            track.track_id
        ))
    })?;
    let adopted = crate::scan::sqlite_store::adopt_file(
        conn,
        item,
        &Adopted {
            track_id: &track.track_id,
            album_id: joined.as_ref().map(|joined| joined.album_id.as_str()),
        },
    )?;
    if adopted != track.track_id {
        return Err(PublishError::Catalog(format!(
            "imported file took track {adopted}, not the planned {}",
            track.track_id
        )));
    }
    if joined.is_some() {
        conn.execute(
            "UPDATE local_tracks SET membership_source = 'manual', membership_locked = 1, \
             row_revision = row_revision + 1 WHERE id = ?1",
            rusqlite::params![adopted],
        )?;
    }
    // Provenance, as v2 recorded it: how the file came in, and which
    // download (if any) brought it.
    conn.execute(
        "UPDATE local_tracks SET ingest_source = ?4, download_task_id = ?2, \
         source_path = ?3 WHERE id = ?1",
        rusqlite::params![
            adopted,
            adoption.download_task_id,
            adoption.source_path,
            adoption.ingest_source
        ],
    )?;
    Ok(joined)
}

/// Read each adopted file of a commit before its transaction: what the
/// catalog indexes it as, exactly as a scan would. The transaction then
/// never waits on a disk or a share.
pub(super) fn read_adopted(tracks: &mut [TrackCommit]) -> Result<(), PublishError> {
    use crate::scan::seams::ScannedTags;
    use crate::scan::store::CommitIndexedItem;

    for track in tracks.iter_mut() {
        let Some(adoption) = &track.adopt else {
            continue;
        };
        let path = Path::new(&track.file_path);
        // The stat comes before the tag read: a scan re-reads a file whose
        // size or modified time differs from the recorded stat, so a change
        // made after this point is still caught.
        let meta = std::fs::symlink_metadata(path).map_err(PublishError::from)?;
        let format = crate::tags::format_for_path(path)
            .map_err(|error| PublishError::Catalog(error.to_string()))?;
        let (tag, header) =
            crate::tags::read::read_scan_metadata(path, format).map_err(|error| {
                PublishError::Catalog(format!("published file unreadable: {error}"))
            })?;
        track.indexed = Some(CommitIndexedItem {
            root_id: track.root_id.clone(),
            relative_path: track.rel_path.clone(),
            absolute_path: track.file_path.clone(),
            size_bytes: meta.len(),
            mtime_ns: crate::scan::revision::mtime_ns_from_metadata(&meta),
            tags_read_at: now_secs(),
            tags: ScannedTags { tag, header },
            effective_policy: adoption.policy,
            policy_revision: adoption.policy_revision.clone(),
            verdict_counter: "new_count",
        });
    }
    Ok(())
}

/// Crash-injection point between durable steps. Each variant names the
/// last completed step; recovery must resume or compensate from there.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CrashPoint {
    /// After staging and journaling, before publish.
    AfterStage,
    /// After a same-path write journaled its backup path, before the
    /// original moves there. Same-path bundles only, so not in
    /// [`CrashPoint::all`].
    AfterBackupJournaled,
    /// After the first destination rename.
    AfterFirstRename,
    /// After all renames, before the catalog transaction.
    BeforeCatalogCommit,
    /// After the catalog transaction commits, before cleanup.
    AfterCatalogCommit,
    /// Midway through post-commit cleanup.
    DuringCleanup,
    /// After the unwind of a failed publish settled its first row. Needs
    /// a publish that fails by itself, so not in [`CrashPoint::all`].
    AfterFirstUnwind,
    /// After the first file of a bundle of several staged, before the
    /// next one. Recovery must unwind such a bundle, so not in
    /// [`CrashPoint::all`].
    AfterFirstFileStaged,
    /// After a file's staged temp is written, before its before-state is
    /// captured. Recovery must unwind such a bundle, so not in
    /// [`CrashPoint::all`].
    AfterTempWritten,
}

impl CrashPoint {
    /// Text form used in the injected error.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::AfterStage => "after-stage",
            Self::AfterBackupJournaled => "after-backup-journaled",
            Self::AfterFirstRename => "after-first-rename",
            Self::BeforeCatalogCommit => "before-catalog-commit",
            Self::AfterCatalogCommit => "after-catalog-commit",
            Self::DuringCleanup => "during-cleanup",
            Self::AfterFirstUnwind => "after-first-unwind",
            Self::AfterFirstFileStaged => "after-first-file-staged",
            Self::AfterTempWritten => "after-temp-written",
        }
    }

    /// Every injection point, for the per-phase crash test.
    pub fn all() -> Vec<Self> {
        vec![
            Self::AfterStage,
            Self::AfterFirstRename,
            Self::BeforeCatalogCommit,
            Self::AfterCatalogCommit,
            Self::DuringCleanup,
        ]
    }
}

/// Outcome of one bundle publication.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PublishOutcome {
    /// Committed and cleaned.
    Committed,
    /// Committed, but cleanup stays pending and retryable.
    CleanupPending,
}

/// Staged publisher. It holds the database and no connection of its own:
/// every journal row goes through a durable write, so it is on disk before
/// the file step it describes, and the catalog commit with its journal
/// transitions is one of them.
pub struct Publisher<C: Catalog, P: SpaceProbe> {
    sandbox: Sandbox,
    db: Db,
    catalog: C,
    space: P,
    gate: CapabilityGate,
    injector: Option<CrashPoint>,
    place_seams: super::place::Seams,
    before_place: Option<BeforePlace>,
    before_set_aside: Option<BeforePlace>,
    /// Unix milliseconds, read at the start of each publish: a publisher
    /// stays open for as long as the root set does.
    clock: Box<dyn Fn() -> i64 + Send>,
    now_ms: i64,
    today_day: i64,
    undo_retention_days: i64,
}

/// What a file fails with when no safe step can place it and the unwind
/// could not put the file it replaces back either.
pub const NO_SAFE_METHOD_ORIGINAL_ASIDE: &str = "This folder has no safe way to add a file \
     without risking an overwrite. The file that was there is kept under a hidden name beside \
     it until an administrator puts it back.";

/// Test hook run with the destination just before a staged file is
/// placed there, after every check has passed.
pub type BeforePlace = Box<dyn Fn(&Path) + Send>;

/// The unix day of a time in unix milliseconds.
fn day_of(ms: i64) -> i64 {
    ms.div_euclid(86_400_000)
}

/// A read made once a publish or a recovery pass has started. It waits for
/// a connection for as long as it takes: a request read's `Busy` answer
/// would stop the work halfway.
pub(super) fn long_read<R>(
    db: &Db,
    name: &'static str,
    op: impl FnOnce(&Connection) -> Result<R, PublishError>,
) -> Result<R, PublishError> {
    db.read_long(name, &CancelFlag::never(), op)
}

/// One durable journal write on the background lane.
pub(super) fn durable<R>(
    db: &Db,
    name: &'static str,
    op: impl FnOnce(&Durable<'_>) -> Result<R, PublishError>,
) -> Result<R, PublishError> {
    db.write_durable(DurableLane::Background, name, op)
}

/// Keep the picture bytes a before-state names, owned like it.
fn keep_pictures(
    conn: &Connection,
    before: &TagDocument,
    owner_kind: &str,
    owner_id: &str,
) -> Result<(), PublishError> {
    let blobs = BlobStore::new(conn);
    let snapshots = SnapshotStore::new(conn);
    for (sha256, bytes) in &before.picture_bytes {
        let stored = blobs.put(bytes)?;
        if &stored != sha256 {
            return Err(PublishError::Snapshot("picture bytes hash mismatch".into()));
        }
        snapshots.add_ref(sha256, owner_kind, owner_id)?;
    }
    Ok(())
}

impl<C: Catalog, P: SpaceProbe> Publisher<C, P> {
    /// A publisher over `db`. `clock` reads the time in unix milliseconds.
    pub fn new(
        sandbox: Sandbox,
        db: Db,
        catalog: C,
        space: P,
        clock: impl Fn() -> i64 + Send + 'static,
    ) -> Self {
        let now_ms = clock();
        Self {
            sandbox,
            db,
            catalog,
            space,
            gate: CapabilityGate::production(),
            injector: None,
            place_seams: super::place::Seams::default(),
            before_place: None,
            before_set_aside: None,
            now_ms,
            today_day: day_of(now_ms),
            clock: Box::new(clock),
            undo_retention_days: super::DEFAULT_UNDO_RETENTION_DAYS as i64,
        }
    }

    /// Keep before-state snapshots for this many days (the Library
    /// Management undo retention). Values below one keep the default.
    pub fn set_undo_retention_days(&mut self, days: i64) {
        if days >= 1 {
            self.undo_retention_days = days;
        }
    }

    /// Arm one crash-injection point. Tests only; production never calls this.
    #[cfg(any(test, feature = "test-support"))]
    pub fn set_crash_point(&mut self, point: Option<CrashPoint>) {
        self.injector = point;
    }

    /// Make placement behave like a filesystem without one of its steps.
    /// Tests only; production never calls this.
    #[cfg(any(test, feature = "test-support"))]
    pub fn set_place_seams(&mut self, seams: super::place::Seams) {
        self.place_seams = seams;
    }

    /// Run `hook` between the last destination check and the placement.
    /// Tests only; production never calls this.
    #[cfg(any(test, feature = "test-support"))]
    pub fn set_before_place(&mut self, hook: Option<BeforePlace>) {
        self.before_place = hook;
    }

    /// Run `hook` with the destination while the file there is being set
    /// aside, just before it moves. Tests only; production never calls
    /// this.
    #[cfg(any(test, feature = "test-support"))]
    pub fn set_before_set_aside(&mut self, hook: Option<BeforePlace>) {
        self.before_set_aside = hook;
    }

    /// The catalog revision a bundle plans against, read on the request
    /// pool before anything is written.
    pub fn catalog_revision(&self) -> Result<u64, PublishError> {
        self.db.read_blocking("publish.catalog_revision", |conn| {
            self.catalog.revision(conn)
        })
    }

    /// Publish one sealed bundle through prepare, publish, catalog
    /// commit, and cleanup. `docs` carries the current semantic tag
    /// document per track id for the staged writer seam.
    pub fn publish(
        &mut self,
        sealed: &SealedPreview,
        live: &SealRecheck,
        docs: &BTreeMap<String, TagDocument>,
    ) -> Result<PublishOutcome, PublishError> {
        self.publish_as(sealed, live, docs, None)
    }

    /// [`Self::publish`], recording the account that applied the bundle
    /// so that account may undo it.
    ///
    /// A database that refuses a call (busy, or shutting down) stops the
    /// publish where it is, with no unwind: the unwind's own rows would
    /// hear the same answer, and recovery finishes or unwinds the bundle
    /// at the next start or maintenance tick.
    pub fn publish_as(
        &mut self,
        sealed: &SealedPreview,
        live: &SealRecheck,
        docs: &BTreeMap<String, TagDocument>,
        applied_by: Option<&str>,
    ) -> Result<PublishOutcome, PublishError> {
        let bundle = &sealed.bundle;
        self.now_ms = (self.clock)();
        self.today_day = day_of(self.now_ms);
        self.run_pregates(sealed, live)?;
        // Durable source operation for undo: recorded after the gates
        // pass, before the first byte stages. Rows for operations that
        // then fail linger harmlessly: undo of one compares the published
        // bytes, which never landed, so it stays put.
        let (today, now) = (self.today_day, self.now_ms);
        self.db
            .write_blocking(Lane::Background, "publish.record_operation", |tx| {
                super::operations::record_operation(tx, bundle, today, now, applied_by)
            })?;
        // A file that cannot be staged unwinds the ones staged before it,
        // so no part of the bundle is held or later finished by recovery.
        let staged = match self.prepare(bundle, docs) {
            Ok(staged) => staged,
            Err(err) => {
                if Self::compensatable(&err) {
                    self.compensate(bundle)?;
                }
                return Err(err);
            }
        };
        self.maybe_crash(CrashPoint::AfterStage)?;
        let outcome = self.publish_and_commit(bundle, &staged);
        match outcome {
            Ok(outcome) => Ok(outcome),
            Err(err) => {
                if Self::compensatable(&err) {
                    let backup_kept = self.compensate(bundle)?;
                    // Only the unwind knows whether a set-aside original
                    // made it home, so the sentence is chosen after it.
                    if backup_kept && matches!(err, PublishError::NoSafePlacement(_)) {
                        return Err(PublishError::NoSafePlacement(
                            NO_SAFE_METHOD_ORIGINAL_ASIDE.to_owned(),
                        ));
                    }
                }
                Err(err)
            }
        }
    }

    /// Pre-gates, in order: seal, capability (plus a v2 original that
    /// never arrived), collision, snapshot inputs, disk preflight. Nothing
    /// is staged until all pass.
    fn run_pregates(&self, sealed: &SealedPreview, live: &SealRecheck) -> Result<(), PublishError> {
        sealed.recheck(live).map_err(|err| match err {
            SealError::StaleFile(text) | SealError::StaleIdentity(text) => {
                PublishError::Validation(text)
            }
            SealError::StaleProfile(text) | SealError::StalePolicy(text) => {
                PublishError::Validation(text)
            }
            SealError::StaleCatalog(text) => PublishError::Validation(text),
            SealError::BadToken => PublishError::Validation("bad confirmation token".into()),
            SealError::Expired => PublishError::Validation("preview expired".into()),
        })?;
        let items = &sealed.bundle.items;
        let facts = self.db.read_blocking("publish.pregates", |conn| {
            let journals = JournalStore::new(conn);
            let baselines = super::snapshots::BaselineStore::new(conn);
            items
                .iter()
                .map(|item| {
                    Ok((
                        baselines.v2_original_missing(&item.track_id)?,
                        journals.unsettled_bundle_for_track(&item.track_id, &sealed.bundle.id)?,
                    ))
                })
                .collect::<Result<Vec<_>, PublishError>>()
        })?;
        for (item, (v2_missing, held)) in items.iter().zip(facts) {
            self.gate.check(item)?;
            if v2_missing {
                return Err(PublishError::Snapshot(format!(
                    "track {} has an original-file baseline from v2 that did not come across; \
                     restore V2_ROOT/cache/library-management from your backup, export from v2 \
                     again, and import into an empty V3_ROOT before managing it",
                    item.track_id
                )));
            }
            if let Some(held) = held {
                return Err(PublishError::Journal(format!(
                    "track {} has an unfinished managed write (bundle {held}) waiting for recovery",
                    item.track_id
                )));
            }
        }
        CollisionGate::check_bundle(&self.sandbox, &sealed.bundle)?;
        DiskPreflight::check(&sealed.bundle, &self.space)?;
        Ok(())
    }

    /// Insert one fresh journal row, durably, before its temp is written.
    fn journal_insert(&self, journal: &FileJournal) -> Result<(), PublishError> {
        durable(&self.db, "publish.journal_prepare", |tx| {
            JournalWriter::new(tx).insert(journal)
        })
    }

    /// Move one row from `Prepared` to `Staged`, durably.
    fn journal_staged(&self, id: &str) -> Result<(), PublishError> {
        durable(&self.db, "publish.journal_staged", |tx| {
            JournalWriter::new(tx).advance(id, JournalState::Prepared, JournalState::Staged)
        })
    }

    /// Prepare: verify sources, render staged bytes through the tag
    /// seam, write destination-side temps, capture before-state
    /// snapshots, and journal every intent as staged.
    fn prepare(
        &mut self,
        bundle: &PlanBundle,
        docs: &BTreeMap<String, TagDocument>,
    ) -> Result<Vec<PreparedFile>, PublishError> {
        let mut staged_files = Vec::new();
        for (ordinal, item) in bundle.items.iter().enumerate() {
            let source = self
                .sandbox
                .resolve_no_symlink(&item.source_root, &item.source_rel)?;
            let source_bytes = paths::read_regular_file(&source)?;
            if source_bytes.len() as u64 != item.fingerprint.size
                || sha256_hex(&source_bytes) != item.fingerprint.sha256
            {
                return Err(PublishError::Validation(format!(
                    "source for track {} changed after planning",
                    item.track_id
                )));
            }
            let before = docs
                .get(&item.track_id)
                .cloned()
                .unwrap_or_else(TagDocument::empty);
            let journal_id = audio_journal_id(&bundle.id, ordinal);
            let dest = self
                .sandbox
                .resolve_no_symlink(&item.dest_root, &item.dest_rel)?;
            self.sandbox.ensure_under_roots(&dest)?;
            if let Some(parent) = dest.parent() {
                std::fs::create_dir_all(parent).map_err(PublishError::from)?;
            }
            if let Some(replaced) = &item.replaces {
                let holds = paths::read_regular_file(&dest)
                    .is_ok_and(|bytes| sha256_hex(&bytes) == replaced.sha256);
                if !holds {
                    return Err(PublishError::Validation(format!(
                        "the file this one replaces at {} changed after planning",
                        item.dest_rel
                    )));
                }
            }
            let temp = self.sandbox.temp_path_for(&dest, &journal_id)?;
            self.sandbox.ensure_under_roots(&temp)?;
            // Real tag staging through the tags save wrapper.
            // Refusals surface before any journal or temp lands.
            let pictures = self.planned_pictures(item)?;
            let staged_bytes = super::staging::render_staged_bytes(
                &source_bytes,
                &item.format,
                &item.managed_updates,
                pictures.as_deref(),
                &temp,
            )?;
            let staged_sha = sha256_hex(&staged_bytes);
            let journal = FileJournal {
                id: journal_id.clone(),
                bundle_id: bundle.id.clone(),
                kind: JournalKind::Audio,
                source: Some((item.source_root.clone(), item.source_rel.clone())),
                dest_root: item.dest_root.clone(),
                dest_rel: item.dest_rel.clone(),
                staged: temp.to_string_lossy().to_string(),
                backup: None,
                source_sha256: Some(item.fingerprint.sha256.clone()),
                staged_sha256: staged_sha.clone(),
                track_id: Some(item.track_id.clone()),
                catalog_revision: Some(bundle.catalog_revision as i64),
                mgmt_state: Some(mgmt_state_for(
                    item.adopt.is_some(),
                    bundle.profile_revision,
                )),
                state: JournalState::Prepared,
                seq: 0,
                replaces: item.replaces.as_ref().map(|replaced| JournalReplacement {
                    rel: item.dest_rel.clone(),
                    sha256: replaced.sha256.clone(),
                    track_id: replaced.track_id.clone(),
                    recycle_to: replaced.recycle_to.clone(),
                }),
            };
            // Journal first, then the temp: a crash between the two leaves
            // a row recovery knows about, never an orphan temp.
            self.journal_insert(&journal)?;
            // The file this one stands in for lends its permission bits:
            // the one it replaces at the destination, else its own source
            // in the library. A download coming in keeps the default.
            let mode = if item.replaces.is_some() {
                Some(paths::permission_bits(&dest)?)
            } else if item.adopt.is_none() {
                Some(paths::permission_bits(&source)?)
            } else {
                None
            };
            write_staged_temp(&temp, &staged_bytes, mode)?;
            self.maybe_crash(CrashPoint::AfterTempWritten)?;
            if item.adopt.is_none() {
                self.capture_before(bundle, item, before)?;
            }
            // Staged only once its before-state is kept: recovery resumes
            // a bundle only when every row is staged. This durable write
            // also puts the before-state on disk ahead of the placement
            // that overwrites the file.
            self.journal_staged(&journal_id)?;
            for (side_ordinal, sidecar) in item.sidecars.iter().enumerate() {
                self.prepare_sidecar(
                    bundle,
                    &journal_id,
                    side_ordinal,
                    sidecar,
                    item.adopt.is_none(),
                )?;
            }
            for (index, output) in item.outputs.iter().enumerate() {
                self.prepare_output(bundle, &journal_id, index, output)?;
            }
            for (index, removal) in item.removals.iter().enumerate() {
                self.prepare_removal(bundle, &journal_id, index, removal)?;
            }
            staged_files.push(PreparedFile {
                journal_id,
                track_id: item.track_id.clone(),
                kind: item.kind,
                staged_sha256: staged_sha,
                source_root: item.source_root.clone(),
                source_rel: item.source_rel.clone(),
            });
            if ordinal == 0 && bundle.items.len() > 1 {
                self.maybe_crash(CrashPoint::AfterFirstFileStaged)?;
            }
        }
        Ok(staged_files)
    }

    /// One blob's bytes, hash-verified.
    fn blob(&self, sha256: &str) -> Result<Vec<u8>, PublishError> {
        long_read(&self.db, "publish.blob", |conn| {
            BlobStore::new(conn).get(sha256)
        })
    }

    /// The pictures a plan item sets, with their bytes from the blob
    /// store; `None` when it leaves the file's pictures alone.
    fn planned_pictures(
        &self,
        item: &super::planner::PlanItem,
    ) -> Result<Option<Vec<crate::tags::save::PictureSpec>>, PublishError> {
        let Some(values) = item.managed_updates.get(super::PICTURES_KEY) else {
            return Ok(None);
        };
        let mut specs = Vec::with_capacity(values.len());
        for value in values {
            let picture = super::PictureRef::decode(value).ok_or_else(|| {
                PublishError::Validation(format!("picture entry {value} is malformed"))
            })?;
            let data = self.blob(&picture.sha256)?;
            if sha256_hex(&data) != picture.sha256 {
                return Err(PublishError::Validation(
                    "a planned picture is not the stored one".into(),
                ));
            }
            specs.push(crate::tags::save::PictureSpec {
                kind: picture.kind,
                mime: picture.mime,
                description: picture.description,
                data,
                info: picture.info,
            });
        }
        Ok(Some(specs))
    }

    /// Before-state for undo and the first-management baseline, in one
    /// ordinary write. The durable `Staged` write that follows puts it on
    /// disk before the file is overwritten.
    fn capture_before(
        &self,
        bundle: &PlanBundle,
        item: &super::planner::PlanItem,
        before: TagDocument,
    ) -> Result<(), PublishError> {
        let (today, expires) = (self.today_day, self.today_day + self.undo_retention_days);
        self.db
            .write_blocking(Lane::Background, "publish.capture_before", |tx| {
                let prior_mgmt = self
                    .catalog
                    .locate(tx, &item.track_id)?
                    .map(|(_, _, _, state)| state)
                    .filter(|state| !state.is_empty());
                let before_state = super::undo::BeforeState {
                    doc: before,
                    source_root: item.source_root.clone(),
                    source_rel: item.source_rel.clone(),
                    source_sha256: item.fingerprint.sha256.clone(),
                    mgmt_state_before: prior_mgmt,
                };
                let blobs = BlobStore::new(tx);
                let baselines = super::snapshots::BaselineStore::new(tx);
                // First-management baseline: captured once, immutable
                // forever. A baseline that appeared concurrently wins; the
                // only Snapshot error capture raises is already-exists.
                if baselines.get(&item.track_id)?.is_none() {
                    keep_pictures(tx, &before_state.doc, "baseline", &item.track_id)?;
                    let baseline_blob = blobs.put(&before_state.to_bytes()?)?;
                    match baselines.capture(
                        &item.track_id,
                        &baseline_blob,
                        &item.source_root,
                        &item.source_rel,
                        today,
                    ) {
                        Ok(()) | Err(PublishError::Snapshot(_)) => {}
                        Err(other) => return Err(other),
                    }
                }
                keep_pictures(tx, &before_state.doc, "operation", &bundle.id)?;
                let blob = blobs.put(&before_state.to_bytes()?)?;
                SnapshotStore::new(tx).record(
                    &format!("{}-{}", bundle.id, item.track_id),
                    &bundle.id,
                    &item.track_id,
                    &blob,
                    today,
                    expires,
                )
            })
    }

    /// Stage one sidecar as a verified byte copy with its own journal.
    /// `keep_mode` gives the copy its source's permission bits (a sidecar
    /// already in the library, not one arriving with a download).
    fn prepare_sidecar(
        &mut self,
        bundle: &PlanBundle,
        audio_journal_id: &str,
        side_ordinal: usize,
        sidecar: &super::planner::SidecarPlan,
        keep_mode: bool,
    ) -> Result<(), PublishError> {
        let source = self
            .sandbox
            .resolve_no_symlink(&sidecar.source_root, &sidecar.source_rel)?;
        let bytes = paths::read_regular_file(&source)?;
        if bytes.len() as u64 != sidecar.fingerprint.size
            || sha256_hex(&bytes) != sidecar.fingerprint.sha256
        {
            return Err(PublishError::Validation(format!(
                "sidecar {} changed after planning",
                sidecar.source_rel
            )));
        }
        let journal_id = sidecar_journal_id(audio_journal_id, side_ordinal);
        let dest = self
            .sandbox
            .resolve_no_symlink(&sidecar.dest_root, &sidecar.dest_rel)?;
        self.sandbox.ensure_under_roots(&dest)?;
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent).map_err(PublishError::from)?;
        }
        let temp = self.sandbox.temp_path_for(&dest, &journal_id)?;
        self.sandbox.ensure_under_roots(&temp)?;
        self.journal_insert(&FileJournal {
            id: journal_id.clone(),
            bundle_id: bundle.id.clone(),
            kind: JournalKind::Sidecar,
            source: Some((sidecar.source_root.clone(), sidecar.source_rel.clone())),
            dest_root: sidecar.dest_root.clone(),
            dest_rel: sidecar.dest_rel.clone(),
            staged: temp.to_string_lossy().to_string(),
            backup: None,
            source_sha256: Some(sidecar.fingerprint.sha256.clone()),
            staged_sha256: sha256_hex(&bytes),
            track_id: None,
            catalog_revision: Some(bundle.catalog_revision as i64),
            mgmt_state: None,
            state: JournalState::Prepared,
            seq: 0,
            replaces: None,
        })?;
        let mode = keep_mode
            .then(|| paths::permission_bits(&source))
            .transpose()?;
        write_staged_temp(&temp, &bytes, mode)?;
        self.journal_staged(&journal_id)
    }

    /// Stage one generated file from the blob store under its own journal.
    fn prepare_output(
        &mut self,
        bundle: &PlanBundle,
        audio_journal_id: &str,
        index: usize,
        output: &super::planner::GeneratedOutput,
    ) -> Result<(), PublishError> {
        let bytes = self.blob(&output.sha256)?;
        if sha256_hex(&bytes) != output.sha256 {
            return Err(PublishError::Validation(format!(
                "generated {} for {} is not the planned file",
                output.kind, output.dest_rel
            )));
        }
        let journal_id = output_journal_id(audio_journal_id, index);
        let dest = self
            .sandbox
            .resolve_no_symlink(&output.dest_root, &output.dest_rel)?;
        self.sandbox.ensure_under_roots(&dest)?;
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent).map_err(PublishError::from)?;
        }
        let temp = self.sandbox.temp_path_for(&dest, &journal_id)?;
        self.sandbox.ensure_under_roots(&temp)?;
        self.journal_insert(&FileJournal {
            id: journal_id.clone(),
            bundle_id: bundle.id.clone(),
            kind: JournalKind::Output,
            source: None,
            dest_root: output.dest_root.clone(),
            dest_rel: output.dest_rel.clone(),
            staged: temp.to_string_lossy().to_string(),
            backup: None,
            source_sha256: None,
            staged_sha256: output.sha256.clone(),
            track_id: None,
            catalog_revision: Some(bundle.catalog_revision as i64),
            mgmt_state: None,
            state: JournalState::Prepared,
            seq: 0,
            replaces: None,
        })?;
        write_staged_temp(&temp, &bytes, None)?;
        self.journal_staged(&journal_id)
    }

    /// Journal taking away one generated file (an undo), inserted and
    /// staged in one durable write. Nothing moves until publish; the file
    /// must still hold the bytes that were written.
    fn prepare_removal(
        &mut self,
        bundle: &PlanBundle,
        audio_journal_id: &str,
        index: usize,
        removal: &super::planner::GeneratedOutput,
    ) -> Result<(), PublishError> {
        let journal_id = removal_journal_id(audio_journal_id, index);
        let dest = self
            .sandbox
            .resolve_no_symlink(&removal.dest_root, &removal.dest_rel)?;
        self.sandbox.ensure_under_roots(&dest)?;
        let aside = self.sandbox.temp_path_for(&dest, &journal_id)?;
        self.sandbox.ensure_under_roots(&aside)?;
        super::generated::check(&dest, &removal.sha256)?;
        let journal = FileJournal {
            id: journal_id.clone(),
            bundle_id: bundle.id.clone(),
            kind: JournalKind::Delete,
            source: None,
            dest_root: removal.dest_root.clone(),
            dest_rel: removal.dest_rel.clone(),
            staged: aside.to_string_lossy().to_string(),
            backup: None,
            source_sha256: Some(removal.sha256.clone()),
            staged_sha256: removal.sha256.clone(),
            track_id: None,
            catalog_revision: Some(bundle.catalog_revision as i64),
            mgmt_state: None,
            state: JournalState::Prepared,
            seq: 0,
            replaces: None,
        };
        durable(&self.db, "publish.journal_removal", |tx| {
            let writer = JournalWriter::new(tx);
            writer.insert(&journal)?;
            writer.advance(&journal_id, JournalState::Prepared, JournalState::Staged)
        })
    }

    /// Publish renames plus the atomic catalog commit, then cleanup.
    fn publish_and_commit(
        &mut self,
        bundle: &PlanBundle,
        staged: &[PreparedFile],
    ) -> Result<PublishOutcome, PublishError> {
        let journals = long_read(&self.db, "publish.bundle_journals", |conn| {
            JournalStore::new(conn).bundle(&bundle.id)
        })?;
        let roots: BTreeSet<&str> = journals
            .iter()
            .map(|journal| journal.dest_root.as_str())
            .collect();
        for root in roots {
            self.sandbox.probe_placement(root)?;
        }
        let mut first = true;
        for journal in journals.iter() {
            self.publish_one(bundle, staged, journal)?;
            if first {
                first = false;
                self.maybe_crash(CrashPoint::AfterFirstRename)?;
            }
        }
        self.maybe_crash(CrashPoint::BeforeCatalogCommit)?;
        self.commit(bundle, staged)?;
        self.maybe_crash(CrashPoint::AfterCatalogCommit)?;
        self.cleanup(bundle, staged)
    }

    /// Record one placement, durably.
    fn journal_published(&self, id: &str) -> Result<(), PublishError> {
        durable(&self.db, "publish.journal_published", |tx| {
            JournalWriter::new(tx).advance(id, JournalState::Staged, JournalState::Published)
        })
    }

    /// Place one staged journal on its destination. Same-path
    /// writes retain the original under a hidden backup first; moves
    /// keep the source until after catalog commit. A late occupier
    /// fails the bundle instead of being overwritten, and so does an
    /// original that another program replaced since it was read or that
    /// could not be put back once set aside.
    fn publish_one(
        &mut self,
        bundle: &PlanBundle,
        staged: &[PreparedFile],
        journal: &FileJournal,
    ) -> Result<(), PublishError> {
        if journal.bundle_id != bundle.id {
            return Err(PublishError::Journal("journal bundle mismatch".into()));
        }
        let current = long_read(&self.db, "publish.journal", |conn| {
            JournalStore::new(conn).get(&journal.id)
        })?
        .ok_or_else(|| PublishError::Journal(format!("missing journal {}", journal.id)))?;
        if current.state == JournalState::Published {
            return Ok(());
        }
        if current.state != JournalState::Staged {
            return Err(PublishError::Journal(format!(
                "journal {} is in state {}",
                journal.id,
                current.state.as_str()
            )));
        }
        let dest = self
            .sandbox
            .resolve_no_symlink(&journal.dest_root, &journal.dest_rel)?;
        self.sandbox.ensure_under_roots(&dest)?;
        if current.kind == JournalKind::Delete {
            let aside = Path::new(&journal.staged);
            self.sandbox.ensure_under_roots(aside)?;
            super::generated::set_aside(
                self.sandbox.placement_mode(&journal.dest_root),
                (&journal.dest_root, &journal.dest_rel),
                &dest,
                aside,
                &journal.staged_sha256,
                self.place_seams,
            )?;
            return self.journal_published(&journal.id);
        }
        let staged_path = Path::new(&journal.staged);
        self.sandbox.ensure_under_roots(staged_path)?;
        let staged_bytes = paths::read_regular_file(staged_path)?;
        if sha256_hex(&staged_bytes) != journal.staged_sha256 {
            return Err(PublishError::Validation(format!(
                "staged bytes for journal {} changed",
                journal.id
            )));
        }
        let prepared = staged.iter().find(|file| file.journal_id == journal.id);
        // A same-path write, or a case-only rename on a filesystem that
        // folds case (the new name reaches the source itself): both set
        // the original aside under a journaled backup, then publish.
        let same_path_self = match prepared {
            Some(file) => {
                let source = self
                    .sandbox
                    .resolve_no_symlink(&file.source_root, &file.source_rel)?;
                match file.kind {
                    PlanKind::SamePath => source == dest,
                    PlanKind::Move => paths::case_alias(
                        &self.sandbox,
                        (&file.source_root, &file.source_rel),
                        (&journal.dest_root, &journal.dest_rel),
                    ),
                }
            }
            None => false,
        };
        match std::fs::symlink_metadata(&dest) {
            Ok(meta) => {
                if meta.file_type().is_symlink() {
                    return Err(PublishError::UnsafePath(format!(
                        "destination is a symlink: {}:{}",
                        journal.dest_root, journal.dest_rel
                    )));
                }
                let replaced_here = current.replaces.as_ref().is_some_and(|replaced| {
                    paths::read_regular_file(&dest)
                        .is_ok_and(|bytes| sha256_hex(&bytes) == replaced.sha256)
                });
                if !same_path_self && !replaced_here {
                    return Err(PublishError::Collision(format!(
                        "late occupier at {}:{}",
                        journal.dest_root, journal.dest_rel
                    )));
                }
                // Journal the backup before the rename: a crash between
                // the two then still knows where the original went.
                let backup = self.sandbox.backup_path_for(&dest, &journal.id)?;
                self.sandbox.ensure_under_roots(&backup)?;
                let backup_text = backup.to_string_lossy();
                durable(&self.db, "publish.journal_backup", |tx| {
                    JournalWriter::new(tx).set_backup(&journal.id, &backup_text)
                })?;
                self.maybe_crash(CrashPoint::AfterBackupJournaled)?;
                let home = paths::backup_home(
                    &self.sandbox,
                    current.source.as_ref(),
                    &journal.dest_root,
                    &journal.dest_rel,
                )?;
                let original = Original {
                    visible: &dest,
                    hidden: &backup,
                    home: &home,
                    sha256: current.backup_sha256(),
                };
                let hook = self
                    .before_set_aside
                    .as_deref()
                    .map(|hook| hook as &dyn Fn(&Path));
                let mode = self.sandbox.placement_mode(&journal.dest_root);
                super::place::set_aside_with(mode, original, self.place_seams, hook)
                    .map_err(|error| error.into_publish(&journal.dest_root, &journal.dest_rel))?;
            }
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(err) => return Err(PublishError::Io(err.to_string())),
        }
        if let Some(hook) = self.before_place.as_ref() {
            hook(&dest);
        }
        // The checks above can go stale before the file lands, so the
        // placement itself refuses a name that is taken by then.
        let mode = self.sandbox.placement_mode(&journal.dest_root);
        super::place::place_with(mode, staged_path, &dest, self.place_seams)
            .map_err(|error| error.into_publish(&journal.dest_root, &journal.dest_rel))?;
        self.journal_published(&journal.id)
    }

    /// Single durable transaction: catalog CAS plus every journal
    /// `published -> committed`. The adopted files are read before it.
    fn commit(&mut self, bundle: &PlanBundle, staged: &[PreparedFile]) -> Result<(), PublishError> {
        let mut tracks = Vec::with_capacity(staged.len());
        for file in staged {
            let item = bundle
                .items
                .iter()
                .find(|item| item.track_id == file.track_id)
                .ok_or_else(|| PublishError::Journal("staged file without a plan item".into()))?;
            let dest = self
                .sandbox
                .resolve_no_symlink(&item.dest_root, &item.dest_rel)?;
            tracks.push(TrackCommit {
                track_id: file.track_id.clone(),
                root_id: item.dest_root.clone(),
                rel_path: item.dest_rel.clone(),
                file_path: dest.to_string_lossy().into_owned(),
                fingerprint: file.staged_sha256.clone(),
                source: Some((item.source_root.clone(), item.source_rel.clone())),
                source_sha256: Some(item.fingerprint.sha256.clone()),
                mgmt_state: mgmt_state_for(item.adopt.is_some(), bundle.profile_revision),
                adopt: item.adopt.clone(),
                replaces: item.replaces.as_ref().and_then(|replaced| {
                    replaced
                        .track_id
                        .clone()
                        .map(|track| (track, replaced.recycle_to.clone()))
                }),
                indexed: None,
            });
        }
        read_adopted(&mut tracks)?;
        let commit = BundleCommit {
            bundle_id: bundle.id.clone(),
            expected_catalog_revision: bundle.catalog_revision,
            tracks,
        };
        durable(&self.db, "publish.bundle_commit", |tx| {
            self.catalog.commit_bundle(tx, &commit)?;
            let writer = JournalWriter::new(tx);
            for journal in JournalStore::new(tx).bundle(&bundle.id)? {
                if journal.state != JournalState::Published {
                    return Err(PublishError::Journal(format!(
                        "journal {} is {} at commit",
                        journal.id,
                        journal.state.as_str()
                    )));
                }
                writer.advance(
                    &journal.id,
                    JournalState::Published,
                    JournalState::Committed,
                )?;
            }
            Ok(())
        })
    }

    /// Post-commit cleanup: remove verified old sources and backups,
    /// prune newly empty source directories, and mark journals
    /// cleaned. Failures stay durable `cleanup_pending` and never roll
    /// back the commit. A database that refuses a call, or a write that
    /// runs out of time, ends the cleanup with the bundle committed and
    /// its cleanup pending: recovery finishes it.
    fn cleanup(
        &mut self,
        bundle: &PlanBundle,
        staged: &[PreparedFile],
    ) -> Result<PublishOutcome, PublishError> {
        match self.cleanup_rows(bundle, staged) {
            Ok(false) => Ok(PublishOutcome::Committed),
            Ok(true) => Ok(PublishOutcome::CleanupPending),
            Err(error) if error.wrote_nothing() => {
                tracing::warn!(bundle_id = %bundle.id, %error, "publish cleanup stopped; recovery finishes it");
                Ok(PublishOutcome::CleanupPending)
            }
            Err(error) => Err(error),
        }
    }

    /// [`Self::cleanup`] row by row; true when some cleanup stays pending.
    fn cleanup_rows(
        &self,
        bundle: &PlanBundle,
        staged: &[PreparedFile],
    ) -> Result<bool, PublishError> {
        let mut pending = false;
        let mut cleaned_any = false;
        let journals = long_read(&self.db, "publish.bundle_journals", |conn| {
            JournalStore::new(conn).bundle(&bundle.id)
        })?;
        for journal in journals.iter() {
            if journal.state != JournalState::Committed {
                continue;
            }
            if let Err(error) = self.cleanup_one(journal, staged) {
                if error.wrote_nothing() {
                    return Err(error);
                }
                pending = true;
                tracing::warn!(journal_id = %journal.id, %error, "publish cleanup failed; left pending");
                let recorded = durable(&self.db, "publish.journal_cleanup_pending", |tx| {
                    JournalWriter::new(tx).transition(
                        &journal.id,
                        JournalState::Committed,
                        JournalState::CleanupPending,
                    )
                });
                match recorded {
                    Err(error) if error.wrote_nothing() => return Err(error),
                    Err(error) => {
                        tracing::warn!(journal_id = %journal.id, %error, "cleanup-pending not recorded");
                    }
                    Ok(_) => {}
                }
                continue;
            }
            let moved = durable(&self.db, "publish.journal_cleaned", |tx| {
                JournalWriter::new(tx).transition(
                    &journal.id,
                    JournalState::Committed,
                    JournalState::Cleaned,
                )
            })?;
            if !moved {
                pending = true;
            }
            if !cleaned_any {
                cleaned_any = true;
                self.maybe_crash(CrashPoint::DuringCleanup)?;
            }
        }
        Ok(pending)
    }

    /// Remove one journal's verified source or backup, then prune
    /// newly empty source parents. Unknown files are never deleted.
    fn cleanup_one(
        &self,
        journal: &FileJournal,
        staged: &[PreparedFile],
    ) -> Result<(), PublishError> {
        if journal.kind == JournalKind::Delete {
            let aside = Path::new(&journal.staged);
            self.sandbox.ensure_under_roots(aside)?;
            return super::generated::discard(aside, &journal.staged_sha256);
        }
        if let (Some(replaced), Some(backup_text)) =
            (journal.replaces.as_ref(), journal.backup.as_deref())
        {
            let backup = Path::new(backup_text);
            self.sandbox.ensure_under_roots(backup)?;
            let mode = self.sandbox.placement_mode(&journal.dest_root);
            let recycle_to = Path::new(&replaced.recycle_to);
            if paths::recycle_backup(mode, backup, recycle_to, &replaced.sha256)? {
                durable(&self.db, "publish.journal_recycled", |tx| {
                    JournalWriter::new(tx).set_recycled(&journal.id, now_secs())
                })?;
            }
            if let Some(parent) = backup.parent() {
                sync_dir_logged(parent);
            }
        } else if let Some(backup_text) = journal.backup.as_deref() {
            let backup = Path::new(backup_text);
            self.sandbox.ensure_under_roots(backup)?;
            let bytes = paths::read_regular_file(backup)?;
            let expected = journal.source_sha256.as_deref().unwrap_or_default();
            if !expected.is_empty() && sha256_hex(&bytes) == expected {
                std::fs::remove_file(backup).map_err(PublishError::from)?;
                if let Some(parent) = backup.parent() {
                    sync_dir_logged(parent);
                }
            } else if expected.is_empty() {
                std::fs::remove_file(backup).map_err(PublishError::from)?;
            }
        }
        if let Some((root, rel)) = journal.source.as_ref() {
            let prepared = staged.iter().find(|file| file.journal_id == journal.id);
            let is_move = prepared
                .map(|file| file.kind == PlanKind::Move)
                .unwrap_or(true);
            if is_move || journal.kind == JournalKind::Sidecar {
                let source = self.sandbox.resolve_no_symlink(root, rel)?;
                let dest = self
                    .sandbox
                    .resolve_no_symlink(&journal.dest_root, &journal.dest_rel)?;
                // After a case-only rename on a case-folding filesystem the
                // old name reaches the published file: never delete it.
                if source != dest
                    && !paths::same_file(&source, &dest)
                    && let Ok(bytes) = paths::read_regular_file(&source)
                {
                    let expected = journal.source_sha256.as_deref().unwrap_or_default();
                    if expected.is_empty() || sha256_hex(&bytes) == expected {
                        std::fs::remove_file(&source).map_err(PublishError::from)?;
                        prune_empty_parents(&self.sandbox, root, &source)?;
                    }
                }
            }
        }
        Ok(())
    }

    /// Compensate a pre-commit failure with the unwind recovery uses:
    /// remove published destinations whose bytes still match the staged
    /// copy, restore same-path backups, delete unpublished temps, and
    /// mark journals compensated. A row whose original could not go home
    /// keeps its backup and needs attention; the publish still fails with
    /// its own error. True when a backup was kept.
    fn compensate(&mut self, bundle: &PlanBundle) -> Result<bool, PublishError> {
        let journals = long_read(&self.db, "publish.bundle_journals", |conn| {
            JournalStore::new(conn).bundle(&bundle.id)
        })?;
        let rows = if self.injector == Some(CrashPoint::AfterFirstUnwind) {
            &journals[..journals.len().min(1)]
        } else {
            &journals[..]
        };
        let kept =
            super::recovery::compensate_bundle(&self.db, &self.sandbox, rows, self.place_seams)?;
        self.maybe_crash(CrashPoint::AfterFirstUnwind)?;
        Ok(kept.is_some())
    }

    /// Whether a publish failure compensates (pre-commit) rather than
    /// surfacing a post-commit state. Injected crashes compensate or
    /// resume through recovery, never here, and so does a database that
    /// refused a call: the unwind's own rows would hear the same answer.
    /// A write that ran out of time committed nothing and unwinds like
    /// any other failure.
    fn compensatable(err: &PublishError) -> bool {
        !matches!(
            err,
            PublishError::InjectedCrash(_)
                | PublishError::Cleanup(_)
                | PublishError::Interrupted(_)
        )
    }

    /// Fail with the armed crash point, if it matches.
    fn maybe_crash(&self, point: CrashPoint) -> Result<(), PublishError> {
        if self.injector == Some(point) {
            return Err(PublishError::InjectedCrash(point.as_str().to_string()));
        }
        Ok(())
    }
}

/// One staged file: the journal plus the plan facts publish needs.
#[derive(Debug, Clone)]
struct PreparedFile {
    journal_id: String,
    track_id: String,
    kind: PlanKind,
    staged_sha256: String,
    source_root: String,
    source_rel: String,
}

/// Deterministic journal ids for idempotent retry: an audio file's row,
/// then one per sidecar, generated output and removal it carries.
fn audio_journal_id(bundle_id: &str, ordinal: usize) -> String {
    format!("{bundle_id}-{ordinal:03}-audio")
}

fn sidecar_journal_id(audio_journal_id: &str, index: usize) -> String {
    format!("{audio_journal_id}-sidecar-{index}")
}

fn output_journal_id(audio_journal_id: &str, index: usize) -> String {
    format!("{audio_journal_id}-output-{index}")
}

fn removal_journal_id(audio_journal_id: &str, index: usize) -> String {
    format!("{audio_journal_id}-remove-{index}")
}

/// Every journal row `prepare` writes for a bundle staged in full.
pub(super) fn journal_ids(bundle: &PlanBundle) -> Vec<String> {
    let mut ids = Vec::new();
    for (ordinal, item) in bundle.items.iter().enumerate() {
        let audio = audio_journal_id(&bundle.id, ordinal);
        ids.extend((0..item.sidecars.len()).map(|index| sidecar_journal_id(&audio, index)));
        ids.extend((0..item.outputs.len()).map(|index| output_journal_id(&audio, index)));
        ids.extend((0..item.removals.len()).map(|index| removal_journal_id(&audio, index)));
        ids.push(audio);
    }
    ids
}

/// Write a staged temp atomically: create, write, fsync, re-read, and
/// fsync the directory. An existing temp with identical bytes is
/// reused so crashed attempts retry idempotently; mismatched bytes
/// fail closed. Anything else under the name (a symlink) is unlinked and
/// the temp created new. `mode` is the permission bits of the file this
/// one replaces; a file that replaces nothing keeps the default.
fn write_staged_temp(temp: &Path, bytes: &[u8], mode: Option<u32>) -> Result<(), PublishError> {
    use std::io::Write as _;
    use std::os::unix::fs::OpenOptionsExt as _;
    if let Ok(current) = paths::read_regular_file(temp) {
        if current == bytes {
            let reopened = std::fs::OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_NOFOLLOW)
                .open(temp);
            lend_mode(reopened, mode, temp);
            return Ok(());
        }
        let name = temp
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("staging temp");
        return Err(PublishError::Journal(format!(
            "staged temp {name} holds foreign bytes"
        )));
    }
    let written = (|| {
        let mut file = paths::create_hidden(temp).map_err(PublishError::from)?;
        file.write_all(bytes).map_err(PublishError::from)?;
        super::journal::fsync_file(&file)?;
        lend_mode(Ok(file), mode, temp);
        let reread = paths::read_regular_file(temp)?;
        if reread != bytes {
            return Err(PublishError::Validation(
                "staged temp failed re-read".into(),
            ));
        }
        if let Some(parent) = temp.parent() {
            super::journal::fsync_dir(parent)?;
        }
        Ok(())
    })();
    if written.is_err() {
        // A partial temp would hold foreign bytes for the retry; it is
        // ours (just created), so it goes.
        if let Err(error) = std::fs::remove_file(temp)
            && error.kind() != std::io::ErrorKind::NotFound
        {
            tracing::warn!(temp = %temp.display(), %error, "partial staging temp not removed");
        }
    }
    written
}

/// Give a staged file the permission bits of the file it stands in for.
/// The bits go on the open file, never on the name, so a link swapped in
/// at the name is not followed. A folder that refuses the change does not
/// fail the write: the file keeps the default bits.
fn lend_mode(file: std::io::Result<std::fs::File>, mode: Option<u32>, temp: &Path) {
    use std::os::unix::fs::PermissionsExt as _;
    let Some(bits) = mode else {
        return;
    };
    let set = file.and_then(|file| {
        file.set_permissions(std::fs::Permissions::from_mode(bits))?;
        file.sync_all()
    });
    if let Err(error) = set {
        tracing::warn!(temp = %temp.display(), %error, "staged file keeps the default permission bits");
    }
}

/// Prune newly empty source parents up to (not including) the root.
/// Stops at the first non-empty directory; unknown files are never
/// deleted.
fn prune_empty_parents(
    sandbox: &Sandbox,
    root_id: &str,
    source: &Path,
) -> Result<(), PublishError> {
    let root = sandbox.root_dir(root_id)?.to_path_buf();
    let mut cursor = source.parent().map(Path::to_path_buf);
    while let Some(dir) = cursor {
        if dir == root || !dir.starts_with(&root) {
            break;
        }
        let mut entries = std::fs::read_dir(&dir).map_err(PublishError::from)?;
        if entries.next().is_some() {
            break;
        }
        std::fs::remove_dir(&dir).map_err(PublishError::from)?;
        cursor = dir.parent().map(Path::to_path_buf);
    }
    Ok(())
}
