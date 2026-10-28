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

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::Instant;

use droppedneedle_kernel::db::{CancelFlag, Db, Durable, DurableLane, Lane};
use droppedneedle_kernel::jobs::queue::{JobError, JobQueue};
use rusqlite::{Connection, OptionalExtension};

use super::journal::{
    FileJournal, JournalKind, JournalReplacement, JournalState, JournalStore, JournalWriter,
    MgmtState,
};
use super::paths::Sandbox;
use super::planner::{
    AlbumJoin, CapabilityGate, CollisionGate, CommitSeal, DiskPreflight, JoinedTrack, PlanBundle,
    ReleaseIdentity, SealError, SealRecheck, SealedPreview, SpaceProbe,
};
use super::snapshots::{BlobStore, SnapshotStore, sha256_hex};
use super::step::{AdoptionFrom, Advanced, Hooks, Run};
use super::tags_seam::TagDocument;
use super::{PublishError, paths};
use crate::clock::LibraryClock as _;
use droppedneedle_kernel::clock::Clock;

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
    /// How the commit records the track, kept as its last outcome.
    pub mgmt_state: MgmtState,
    /// The track is new to the catalog: index the published file instead
    /// of moving an existing row (see [`MgmtState::Imported`]).
    pub adopt: Option<super::planner::Adoption>,
    /// The track whose file this one replaced at its destination, retired
    /// in the same commit, and where in the recycle bin that file goes.
    pub replaces: Option<(String, String)>,
    /// What an adopted file indexes as, read from the published file
    /// before the commit's transaction by [`read_adopted`].
    pub indexed: Option<crate::scan::store::CommitIndexedItem>,
    /// The rows a track that is not adopted was sealed against. The commit
    /// refuses the bundle when they moved, or when there is no seal.
    pub sealed: Option<CommitSeal>,
    /// The release track an imported file was matched to, as its plan item
    /// journaled it: what the commit seals as the file's identity. Set only
    /// with `adopt`.
    pub identity: Option<ReleaseIdentity>,
}

/// The single-transaction catalog bundle commit.
#[derive(Debug, Clone)]
pub struct BundleCommit {
    /// Bundle id for history and invalidation.
    pub bundle_id: String,
    /// Track adoptions in this bundle.
    pub tracks: Vec<TrackCommit>,
    /// The tracks a removal marks missing, checked against their seals.
    pub removal: Option<super::planner::Removal>,
    /// The tracks a put-back indexes again.
    pub put_back: Option<super::put_back::PutBackCommit>,
}

/// Queue what a committed bundle asks to run after it, on the commit's own
/// transaction: a removed album's clean-up. A refusal fails the commit, so
/// the removal and its clean-up land together or not at all.
pub(super) fn enqueue_after_commit(
    tx: &rusqlite::Transaction<'_>,
    jobs: &JobQueue,
    commit: &BundleCommit,
    clock: &dyn Clock,
) -> Result<(), PublishError> {
    let Some(removed) = commit
        .removal
        .as_ref()
        .and_then(|removal| removal.album_removed.as_ref())
    else {
        return Ok(());
    };
    super::planner::AlbumRemovedJob::enqueue(tx, jobs, &commit.bundle_id, removed, clock.now_secs())
        .map_err(|error| match error {
            JobError::Db(db) => PublishError::from(db),
            other => PublishError::Store(other.to_string()),
        })
}

/// The catalog revision: a change counter every commit bumps once. Read
/// caches key on it, so a commit invalidates them.
pub fn read_revision(conn: &Connection) -> Result<u64, PublishError> {
    let rev: i64 = conn.query_row(
        "SELECT value FROM library_catalog_revision WHERE singleton = 1",
        [],
        |row| row.get(0),
    )?;
    Ok(rev as u64)
}

/// Where a track is, and what the publisher last wrote to it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Located {
    pub root_id: String,
    pub rel_path: String,
    /// The SHA-256 the publisher last wrote; empty for a track it never
    /// wrote.
    pub published_sha256: String,
    /// The track's last management outcome, when it has one.
    pub last_outcome: Option<String>,
}

/// Locate a track in `local_tracks`, with its management state from
/// `library_track_management_state`.
pub fn locate(conn: &Connection, track_id: &str) -> Result<Option<Located>, PublishError> {
    conn.query_row(
        "SELECT t.root_id, t.relative_path, COALESCE(m.applied_projection_hash, ''), \
             m.last_outcome \
             FROM local_tracks t \
             LEFT JOIN library_track_management_state m ON m.local_track_id = t.id \
             WHERE t.id = ?1",
        rusqlite::params![track_id],
        |row| {
            Ok(Located {
                root_id: row.get(0)?,
                rel_path: row.get(1)?,
                published_sha256: row.get(2)?,
                last_outcome: row
                    .get::<_, Option<String>>(3)?
                    .filter(|outcome| !outcome.is_empty()),
            })
        },
    )
    .optional()
    .map_err(PublishError::from)
}

/// Commit one bundle: check every moved track's rows are still as
/// sealed, move every track row (and its album's root) to its
/// published location, and bump the catalog revision once. Track and
/// album ids stay as they are. Files an import fetched for a library
/// album join it, and are placed on its edition in the same
/// transaction. An import's match is sealed in it too
/// ([`crate::identity::adopt_import`]).
pub(super) fn commit_bundle(
    tx: &Durable<'_>,
    commit: &BundleCommit,
    clock: &dyn Clock,
) -> Result<(), PublishError> {
    let conn: &Connection = tx;
    // The folders the bundle made hold its files now: no unwind removes
    // them.
    super::folders::forget(conn, &commit.bundle_id)?;
    // A file added with no track, or its undo: no catalog row moves.
    if commit.tracks.is_empty() && commit.removal.is_none() && commit.put_back.is_none() {
        return Ok(());
    }
    if let Some(removal) = &commit.removal {
        super::remove::commit(conn, &commit.bundle_id, removal, clock.now_secs())?;
    }
    if let Some(put_back) = &commit.put_back {
        super::put_back::commit(conn, &commit.bundle_id, put_back, clock.now_secs())?;
    }
    // The bundle's own rows, read in this transaction: a scan or an
    // identification that changed one since the seal refuses the
    // whole bundle before any row moves. Adoptions have no rows yet.
    let sealed: Vec<(&str, Option<&CommitSeal>)> = commit
        .tracks
        .iter()
        .filter(|track| track.adopt.is_none())
        .map(|track| (track.track_id.as_str(), track.sealed.as_ref()))
        .collect();
    if let Some(moved) = super::planner::first_moved(conn, &sealed)? {
        return Err(PublishError::Catalog(
            format!("track {moved} changed under bundle {}", commit.bundle_id).into(),
        ));
    }
    crate::catalog_revision::bump_catalog(conn)?;
    let now = clock.now_secs();
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
    let mut imported = Vec::new();
    for (track, written) in commit.tracks.iter().zip(&written) {
        if let Some((replaced, recycle_to)) = &track.replaces {
            retire_replaced(conn, track, replaced, recycle_to, now)?;
        }
        if let Some(adoption) = &track.adopt {
            let joined = adopt_track(conn, track, adoption)?;
            if let Some(identity) = &track.identity {
                imported.push(crate::identity::ImportedFile {
                    track_id: &track.track_id,
                    identity,
                    joined: joined.is_some(),
                });
            }
            if let (Some(joined), Some(join), Some(on)) = (joined, &adoption.join, &adoption.track)
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
                track.mgmt_state.as_str(),
                now,
                written
            ],
        )?;
    }
    for (album, (release, files)) in &placing {
        crate::operations::choice::place_imported(tx, album, release, files, now)?;
    }
    // After the placements: a person's edition keeps them, and the
    // seal never writes over a person's rows.
    if !imported.is_empty() {
        crate::identity::adopt_import(tx, &imported, now)?;
    }
    Ok(())
}

/// Mark a track missing so streaming and search stay accurate when
/// committed bytes cannot be recovered.
pub(super) fn mark_missing(
    conn: &Connection,
    track_id: &str,
    clock: &dyn Clock,
) -> Result<(), PublishError> {
    conn.execute(
        "UPDATE local_tracks SET availability = 'missing', missing_since = ?2 WHERE id = ?1",
        rusqlite::params![track_id, clock.now_secs()],
    )?;
    crate::catalog_revision::bump_catalog(conn)?;
    Ok(())
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
        return Err(PublishError::Catalog(
            format!(
                "track {occupant} is still listed at the destination of track {}",
                track.track_id
            )
            .into(),
        ));
    }
    give_up(conn, &occupant, &track.rel_path, &track.file_path)?;
    tracing::info!(
        track_id = %occupant,
        placed = %track.track_id,
        "a track whose file is gone gave up its path"
    );
    Ok(())
}

/// Move the row of `occupant`, whose file is gone, off `rel_path` onto a
/// hidden name of its own under the same root, keeping everything else.
/// `file_path` is the absolute path of the file now placed at `rel_path`,
/// which names the root.
pub(super) fn give_up(
    conn: &Connection,
    occupant: &str,
    rel_path: &str,
    file_path: &str,
) -> Result<(), PublishError> {
    let hidden = format!("{}gone/{}/{}", super::HIDDEN_PREFIX, occupant, rel_path);
    // The hidden name under the same root, so the old row never points
    // at the file placed on its old path.
    let depth = Path::new(rel_path).components().count();
    let root_dir = Path::new(file_path)
        .ancestors()
        .nth(depth)
        .ok_or_else(|| PublishError::Catalog(format!("{rel_path} has no root").into()))?;
    let hidden_path = root_dir.join(&hidden).to_string_lossy().into_owned();
    conn.prepare_cached(super::sql::TRACK_SET_PATH.text())?
        .execute(rusqlite::params![
            occupant,
            hidden,
            hidden_path,
            sha256_hex(hidden.as_bytes())
        ])?;
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
        return Err(PublishError::Catalog(
            format!("track {} is not in the catalog", track.track_id).into(),
        ));
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
                    return Err(PublishError::Catalog(
                        format!("track {id} is no longer on the album it was fetched for").into(),
                    ));
                }
                // It described the file this one replaced.
                crate::identity::release_track(conn, &id)?;
            }
            Some(_) => give_up_destination(conn, track)?,
            None => {}
        }
    }

    let item = track.indexed.as_ref().ok_or_else(|| {
        PublishError::Catalog(
            format!("track {} was not read before its commit", track.track_id).into(),
        )
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
        return Err(PublishError::Catalog(
            format!(
                "imported file took track {adopted}, not the planned {}",
                track.track_id
            )
            .into(),
        ));
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
pub(super) fn read_adopted(
    tracks: &mut [TrackCommit],
    clock: &dyn Clock,
) -> Result<(), PublishError> {
    for track in tracks.iter_mut() {
        let Some(adoption) = &track.adopt else {
            continue;
        };
        track.indexed = Some(index_file(
            &track.root_id,
            &track.rel_path,
            Path::new(&track.file_path),
            adoption.policy,
            &adoption.policy_revision,
            clock,
        )?);
    }
    Ok(())
}

/// What the published file at `path` (`root_id:rel_path`) indexes as,
/// read exactly as a scan would.
pub(super) fn index_file(
    root_id: &str,
    rel_path: &str,
    path: &Path,
    policy: crate::scan::models::EffectivePolicy,
    policy_revision: &str,
    clock: &dyn Clock,
) -> Result<crate::scan::store::CommitIndexedItem, PublishError> {
    use crate::scan::seams::ScannedTags;
    use crate::scan::store::CommitIndexedItem;

    // The stat comes before the tag read: a scan re-reads a file whose
    // size or modified time differs from the recorded stat, so a change
    // made after this point is still caught.
    let meta = std::fs::symlink_metadata(path).map_err(PublishError::from)?;
    let format = crate::tags::format_for_path(path)
        .map_err(|error| PublishError::Catalog(error.to_string().into()))?;
    let (tag, header) = crate::tags::read::read_scan_metadata(path, format).map_err(|error| {
        PublishError::Catalog(format!("published file unreadable: {error}").into())
    })?;
    Ok(CommitIndexedItem {
        root_id: root_id.to_owned(),
        relative_path: rel_path.to_owned(),
        absolute_path: path.to_string_lossy().into_owned(),
        size_bytes: meta.len(),
        mtime_ns: crate::scan::revision::mtime_ns_from_metadata(&meta),
        tags_read_at: clock.now_secs(),
        tags: ScannedTags { tag, header },
        effective_policy: policy,
        policy_revision: policy_revision.to_owned(),
        verdict_counter: "new_count",
    })
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
    /// After the unwind of a failed publish finished, before the folders
    /// it made are removed. Needs a publish that fails by itself, so not
    /// in [`CrashPoint::all`].
    BeforeFolderRemoval,
    /// After the first file of a bundle of several staged, before the
    /// next one. Recovery must unwind such a bundle, so not in
    /// [`CrashPoint::all`].
    AfterFirstFileStaged,
    /// After a file's staged temp is written, before its before-state is
    /// captured. Recovery must unwind such a bundle, so not in
    /// [`CrashPoint::all`].
    AfterTempWritten,
    /// Between linking a placed file onto its name and removing its hidden
    /// name. Raised by the placement test seam, never armed, so not in
    /// [`CrashPoint::all`].
    AfterLink,
}

impl std::fmt::Display for CrashPoint {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
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
            Self::BeforeFolderRemoval => "before-folder-removal",
            Self::AfterFirstFileStaged => "after-first-file-staged",
            Self::AfterTempWritten => "after-temp-written",
            Self::AfterLink => "after-link",
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
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum PublishOutcome {
    /// Committed and cleaned.
    Committed,
    /// Committed, but cleanup stays pending and retryable.
    CleanupPending,
}

impl PublishOutcome {
    /// Wire name, the same as the serde form.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Committed => "committed",
            Self::CleanupPending => "cleanup_pending",
        }
    }
}

/// Staged publisher. It holds the database and no connection of its own:
/// every journal row goes through a durable write, so it is on disk before
/// the file step it describes, and the catalog commit with its journal
/// transitions is one of them.
pub struct Publisher<P: SpaceProbe> {
    sandbox: Sandbox,
    db: Db,
    space: P,
    /// The jobs table a commit queues its follow-up work on.
    jobs: JobQueue,
    gate: CapabilityGate,
    injector: Option<CrashPoint>,
    place_seams: super::place::Seams,
    /// Seams for the move of a replaced file into its bin, so a test can
    /// give the bin a filesystem the library does not share.
    bin_seams: super::place::Seams,
    before_place: Option<BeforePlace>,
    before_set_aside: Option<BeforePlace>,
    /// Each put-back temp's identity as this publish wrote it, by journal
    /// id: publish and cleanup check it instead of reading the file again.
    temps: BTreeMap<String, paths::FileStat>,
    /// What this publish hashed while it copied and wrote files: later
    /// steps check a file by its identity instead of reading it again.
    known: paths::Known,
    /// The bytes of the file the bundle being published adds.
    added_bytes: Option<Vec<u8>>,
    /// When the file the bundle being published adds stops being wanted:
    /// its deadline, and the flag set when whoever asked stopped waiting.
    add_budget: Option<(Instant, Arc<AtomicBool>)>,
    /// Unix milliseconds, read at the start of each publish: a publisher
    /// stays open for as long as the root set does.
    clock: Arc<dyn Clock>,
    now_ms: i64,
    today_day: i64,
    undo_retention_days: i64,
    /// The undo folder before-states are kept in.
    blobs: BlobStore,
}

/// What a file fails with when no safe step can place it and the unwind
/// could not put the file it replaces back either.
pub const NO_SAFE_METHOD_ORIGINAL_ASIDE: &str = "This folder has no safe way to add a file \
     without risking an overwrite. The file that was there is kept under a hidden name beside \
     it until an administrator puts it back.";

/// Test hook run with the destination just before a staged file is
/// placed there, after every check has passed.
pub type BeforePlace = Box<dyn Fn(&Path) + Send>;

/// Runs in the transaction that records a change, with the bundle id and
/// the publish's time in unix milliseconds. An error records nothing.
pub type OnRecord<'a> = &'a dyn Fn(&Connection, &str, i64) -> Result<(), PublishError>;

/// What bounds the publish of one added file, and what else its record
/// holds.
pub struct AddTerms<'a> {
    /// Past this instant the file is not placed: the bundle unwinds.
    pub deadline: Instant,
    /// Set once whoever asked stopped waiting: the bundle unwinds the same
    /// way if the file is not placed yet.
    pub cancelled: Arc<AtomicBool>,
    /// Runs in the transaction that records the change.
    pub on_record: OnRecord<'a>,
}

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

/// The management state a track's before-state records: `None` when it
/// has none.
fn management_state(conn: &Connection, track_id: &str) -> Result<Option<String>, PublishError> {
    Ok(locate(conn, track_id)?.and_then(|located| located.last_outcome))
}

impl<P: SpaceProbe> Publisher<P> {
    /// A publisher over `db`, queueing a commit's follow-up work on `jobs`.
    /// `clock` is the wall clock its stamps and day boundaries read;
    /// before-states are kept in `blobs`.
    pub fn new(
        sandbox: Sandbox,
        db: Db,
        space: P,
        jobs: JobQueue,
        clock: Arc<dyn Clock>,
        blobs: BlobStore,
    ) -> Self {
        let now_ms = clock.now_ms();
        Self {
            sandbox,
            db,
            space,
            jobs,
            gate: CapabilityGate::production(),
            injector: None,
            place_seams: super::place::Seams::default(),
            bin_seams: super::place::Seams::default(),
            before_place: None,
            before_set_aside: None,
            temps: BTreeMap::new(),
            known: paths::Known::default(),
            added_bytes: None,
            add_budget: None,
            now_ms,
            today_day: day_of(now_ms),
            clock,
            undo_retention_days: super::DEFAULT_UNDO_RETENTION_DAYS as i64,
            blobs,
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

    /// Make the move of a replaced file into its recycle bin behave like a
    /// filesystem without one of its steps. Tests only; production never
    /// calls this.
    #[cfg(any(test, feature = "test-support"))]
    pub fn set_bin_seams(&mut self, seams: super::place::Seams) {
        self.bin_seams = seams;
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

    /// Publish a bundle that adds one file (see [`super::add`]), with the
    /// file's bytes and no time bound. The bundle runs the same steps as
    /// every other.
    pub fn publish_add(
        &mut self,
        sealed: &SealedPreview,
        live: &SealRecheck,
        bytes: Vec<u8>,
    ) -> Result<PublishOutcome, PublishError> {
        self.added_bytes = Some(bytes);
        let outcome = self.publish_recorded(sealed, live, &BTreeMap::new(), None, None);
        self.added_bytes = None;
        outcome
    }

    /// [`Self::publish_add`] within `terms`: a file not placed by its
    /// deadline, or no longer wanted, is never placed. Its bundle unwinds
    /// and the publish fails with [`PublishError::TimedOut`].
    pub fn publish_add_within(
        &mut self,
        sealed: &SealedPreview,
        live: &SealRecheck,
        bytes: Vec<u8>,
        terms: AddTerms<'_>,
    ) -> Result<PublishOutcome, PublishError> {
        self.added_bytes = Some(bytes);
        self.add_budget = Some((terms.deadline, terms.cancelled));
        let outcome =
            self.publish_recorded(sealed, live, &BTreeMap::new(), None, Some(terms.on_record));
        self.added_bytes = None;
        self.add_budget = None;
        outcome
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
        self.publish_recorded(sealed, live, docs, applied_by, None)
    }

    /// [`Self::publish_as`], running `on_record` in the transaction that
    /// records the operation.
    fn publish_recorded(
        &mut self,
        sealed: &SealedPreview,
        live: &SealRecheck,
        docs: &BTreeMap<String, TagDocument>,
        applied_by: Option<&str>,
        on_record: Option<OnRecord<'_>>,
    ) -> Result<PublishOutcome, PublishError> {
        let bundle = &sealed.bundle;
        self.temps.clear();
        self.known.clear();
        self.now_ms = self.clock.now_ms();
        self.today_day = day_of(self.now_ms);
        self.run_pregates(sealed, live)?;
        // Durable source operation for undo: recorded after the gates
        // pass, before the first byte stages. Rows for operations that
        // then fail linger harmlessly: undo of one compares the published
        // bytes, which never landed, so it stays put.
        let (today, now) = (self.today_day, self.now_ms);
        self.db
            .write_blocking(Lane::Background, "publish.record_operation", |tx| {
                super::operations::record_operation(tx, bundle, today, now, applied_by)?;
                match on_record {
                    Some(on_record) => on_record(tx, &bundle.id, now),
                    None => Ok(()),
                }
            })?;
        // A file that cannot be staged unwinds the ones staged before it,
        // so no part of the bundle is held or later finished by recovery.
        if let Err(err) = self.prepare(bundle, docs) {
            if Self::compensatable(&err) {
                self.compensate(bundle)?;
            }
            return Err(err);
        }
        self.maybe_crash(CrashPoint::AfterStage)?;
        // From here on a publish runs the steps recovery runs, from
        // `Staged`, with the plan it holds.
        let advanced = self.run().advance(&bundle.id, Some(bundle));
        let (err, backup_kept) = match advanced {
            Ok(Advanced::Committed { pending: false }) => return Ok(PublishOutcome::Committed),
            Ok(Advanced::Committed { pending: true }) => {
                return Ok(PublishOutcome::CleanupPending);
            }
            Ok(Advanced::Unwound { refused, kept }) => (
                // A file of the bundle changed under it before anything
                // committed, and the bundle was put back as it was.
                refused.map_or_else(
                    || {
                        PublishError::Validation(
                            format!(
                                "bundle {} changed while it was being written and was put back",
                                bundle.id
                            )
                            .into(),
                        )
                    },
                    |refused| refused.error,
                ),
                kept.is_some(),
            ),
            Ok(Advanced::CommitRefused(err)) => (err, false),
            Ok(Advanced::Attention(reason)) => return Err(PublishError::Journal(reason.into())),
            Ok(Advanced::Waiting(_)) => return Err(PublishError::FolderMissing),
            Ok(Advanced::Noop | Advanced::FoldersWait | Advanced::CleanupFinished) => {
                return Err(PublishError::Journal(
                    format!("bundle {} was not published", bundle.id).into(),
                ));
            }
            Err(err) => {
                if !Self::compensatable(&err) {
                    return Err(err);
                }
                let backup_kept = self.compensate(bundle)?;
                (err, backup_kept)
            }
        };
        // Only the unwind knows whether a set-aside original made it home,
        // so the sentence is chosen after it.
        if backup_kept && matches!(err, PublishError::NoSafePlacement(_)) {
            return Err(PublishError::NoSafePlacement(
                NO_SAFE_METHOD_ORIGINAL_ASIDE.to_owned().into(),
            ));
        }
        Err(err)
    }

    /// The shared steps, with this publish's own state and hooks.
    fn run(&self) -> Run<'_> {
        Run {
            db: &self.db,
            sandbox: &self.sandbox,
            jobs: &self.jobs,
            seams: self.place_seams,
            bin_seams: self.bin_seams,
            now_ms: self.now_ms,
            clock: &*self.clock,
            adoption: AdoptionFrom::Plan,
            known_removals: true,
            temps: &self.temps,
            known: &self.known,
            hooks: Hooks {
                crash: self.injector,
                before_place: self
                    .before_place
                    .as_deref()
                    .map(|hook| hook as &dyn Fn(&Path)),
                before_set_aside: self
                    .before_set_aside
                    .as_deref()
                    .map(|hook| hook as &dyn Fn(&Path)),
                add_budget: self.add_budget.as_ref(),
            },
        }
    }

    /// Pre-gates, in order: seal, capability (plus a v2 original that
    /// never arrived), collision, snapshot inputs, disk preflight. Nothing
    /// is staged until all pass.
    fn run_pregates(&self, sealed: &SealedPreview, live: &SealRecheck) -> Result<(), PublishError> {
        let removal = sealed.bundle.removal.as_ref();
        let put_back = sealed.bundle.put_back.as_ref();
        if put_back.is_some() && (removal.is_some() || !sealed.bundle.items.is_empty()) {
            return Err(PublishError::Journal(
                "a put-back bundle carries nothing else".into(),
            ));
        }
        if sealed.bundle.added.is_some()
            && (removal.is_some() || put_back.is_some() || !sealed.bundle.items.is_empty())
        {
            return Err(PublishError::Journal(
                "a bundle that adds a file carries nothing else".into(),
            ));
        }
        sealed.recheck(live).map_err(|err| match err {
            SealError::StaleFile(text) | SealError::StaleIdentity(text) => {
                PublishError::Validation(text.into())
            }
            SealError::StaleProfile(text) | SealError::StalePolicy(text) => {
                PublishError::Validation(text.into())
            }
            SealError::BadToken => PublishError::Validation("bad confirmation token".into()),
            SealError::Expired => PublishError::Validation("preview expired".into()),
        })?;
        let items = &sealed.bundle.items;
        let seals: Vec<Option<CommitSeal>> = items.iter().map(|item| item.commit_seal()).collect();
        let checked: Vec<(&str, Option<&CommitSeal>)> = items
            .iter()
            .zip(&seals)
            .filter(|(item, _)| item.adopt.is_none())
            .map(|(item, seal)| (item.track_id.as_str(), seal.as_ref()))
            .collect();
        let facts =
            self.db.read_blocking("publish.pregates", |conn| {
                if let Some(moved) = super::planner::first_moved(conn, &checked)? {
                    return Err(PublishError::Validation(
                        format!("track {moved} changed under the preview").into(),
                    ));
                }
                let journals = JournalStore::new(conn);
                if let Some(removal) = removal {
                    if let Some(moved) = super::planner::removal_moved(conn, &removal.items)? {
                        return Err(PublishError::Validation(
                            format!("track {moved} changed under the preview").into(),
                        ));
                    }
                    for item in &removal.items {
                        if let Some(held) = journals
                            .unsettled_bundle_for_track(&item.track_id, &sealed.bundle.id)?
                        {
                            return Err(PublishError::Journal(format!(
                            "track {} has an unfinished managed write (bundle {held}) waiting \
                             for recovery",
                            item.track_id
                        ).into()));
                        }
                    }
                }
                if let Some(put_back) = put_back {
                    if super::put_back::spent(conn, &put_back.removal_bundle)? {
                        return Err(PublishError::Validation(
                            "the removal was already put back".into(),
                        ));
                    }
                    for item in &put_back.items {
                        let place: Option<(String, String, String)> = conn
                            .prepare_cached(super::sql::TRACK_PLACE.text())?
                            .query_row(rusqlite::params![item.track_id], |row| {
                                Ok((row.get(0)?, row.get(1)?, row.get(2)?))
                            })
                            .optional()?;
                        let ready = place.is_some_and(|(root, rel, availability)| {
                            root == item.root_id
                                && rel == item.track_rel
                                && super::put_back::ready(&availability, &rel, &item.relative_path)
                        });
                        if !ready {
                            return Err(PublishError::Validation(
                                format!("track {} changed under the preview", item.track_id).into(),
                            ));
                        }
                        if let Some(held) = journals
                            .unsettled_bundle_for_track(&item.track_id, &sealed.bundle.id)?
                        {
                            return Err(PublishError::Journal(format!(
                            "track {} has an unfinished managed write (bundle {held}) waiting \
                             for recovery",
                            item.track_id
                        ).into()));
                        }
                    }
                }
                let baselines = super::snapshots::BaselineStore::new(conn);
                items
                    .iter()
                    .map(|item| {
                        Ok((
                            baselines.v2_original_missing(&item.track_id)?,
                            journals
                                .unsettled_bundle_for_track(&item.track_id, &sealed.bundle.id)?,
                        ))
                    })
                    .collect::<Result<Vec<_>, PublishError>>()
            })?;
        for (item, (v2_missing, held)) in items.iter().zip(facts) {
            self.gate
                .check(item)
                .map_err(|error| error.track(&item.track_id))?;
            if v2_missing {
                return Err(PublishError::Snapshot(
                    format!(
                        "track {} has an original-file baseline from v2 that did not come across; \
                     restore V2_ROOT/cache/library-management from your backup, export from v2 \
                     again, and import into an empty V3_ROOT before managing it",
                        item.track_id
                    )
                    .into(),
                ));
            }
            if let Some(held) = held {
                return Err(PublishError::Journal(format!(
                    "track {} has an unfinished managed write (bundle {held}) waiting for recovery",
                    item.track_id
                ).into()));
            }
        }
        // A replaced file's bin entry is checked before anything moves.
        // While the bins are unknown the publish waits, as a busy database
        // makes it.
        let entries = items
            .iter()
            .filter_map(|item| item.replaces.as_ref())
            .map(|replaced| replaced.recycle_to.as_str())
            .chain(
                removal
                    .into_iter()
                    .flat_map(|removal| &removal.items)
                    .filter_map(|item| item.file.as_ref())
                    .map(|file| file.recycle_to.as_str()),
            );
        for recycle_to in entries {
            if let Some(cause) = self.sandbox.bins_unknown() {
                return Err(PublishError::Interrupted(format!(
                    "no recycle bin is known while {cause}"
                )));
            }
            self.sandbox.ensure_under_bin(Path::new(recycle_to))?;
        }
        let bin_copies = put_back
            .into_iter()
            .flat_map(|put_back| &put_back.items)
            .filter(|item| !item.home_already && item.copy_in == super::planner::CopyIn::Bin);
        for item in bin_copies {
            if let Some(cause) = self.sandbox.bins_unknown() {
                return Err(PublishError::Interrupted(format!(
                    "no recycle bin is known while {cause}"
                )));
            }
            self.sandbox.ensure_under_bin(Path::new(&item.copy))?;
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

    /// Prepare: copy each source into its destination-side temp, checked
    /// in the same read, write its tags in that temp, capture before-state
    /// snapshots, and journal every intent as staged.
    fn prepare(
        &mut self,
        bundle: &PlanBundle,
        docs: &BTreeMap<String, TagDocument>,
    ) -> Result<(), PublishError> {
        if let Some(removal) = &bundle.removal {
            let files: Vec<(
                usize,
                &super::planner::RemoveItem,
                &super::planner::RemoveFile,
            )> = removal
                .items
                .iter()
                .enumerate()
                .filter_map(|(index, item)| item.file.as_ref().map(|file| (index, item, file)))
                .collect();
            for (count, (index, item, file)) in files.iter().enumerate() {
                self.prepare_remove(bundle, *index, item, file)?;
                if count == 0 && (files.len() > 1 || !bundle.items.is_empty()) {
                    self.maybe_crash(CrashPoint::AfterFirstFileStaged)?;
                }
            }
        }
        if let Some(put_back) = &bundle.put_back {
            let copies: Vec<_> = put_back
                .items
                .iter()
                .enumerate()
                .filter(|(_, item)| !item.home_already)
                .collect();
            for (count, (index, item)) in copies.iter().enumerate() {
                self.prepare_put_back(bundle, *index, item)?;
                if count == 0 && copies.len() > 1 {
                    self.maybe_crash(CrashPoint::AfterFirstFileStaged)?;
                }
            }
        }
        if let Some(added) = &bundle.added {
            self.prepare_added(bundle, added)?;
        }
        for (ordinal, item) in bundle.items.iter().enumerate() {
            let source = self
                .sandbox
                .resolve_no_symlink(&item.source_root, &item.source_rel)?;
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
                super::folders::make(&self.db, &self.sandbox, &bundle.id, &item.dest_root, parent)?;
            }
            if let Some(replaced) = &item.replaces {
                let holds = paths::hash_regular_file(&dest)
                    .ok()
                    .filter(|hashed| hashed.sha256 == replaced.sha256);
                let Some(holds) = holds else {
                    return Err(PublishError::Validation(
                        format!(
                            "the file this one replaces at {} changed after planning",
                            item.dest_rel
                        )
                        .into(),
                    ));
                };
                self.known.record(holds.stat, &holds.sha256);
            }
            // A tag write saves in the temp itself, so its name carries the
            // container's extension. A write with no tag change stages the
            // source's own bytes, known before the copy.
            let pictures = self
                .planned_pictures(item)
                .map_err(|error| error.track(&item.track_id))?;
            let tags_change = !item.managed_updates.is_empty() || pictures.is_some();
            let temp = if tags_change {
                let extension = super::staging::writable_extension(item.format)
                    .map_err(|error| error.track(&item.track_id))?;
                self.sandbox
                    .audio_temp_path_for(&dest, &journal_id, extension)?
            } else {
                self.sandbox.temp_path_for(&dest, &journal_id)?
            };
            self.sandbox.ensure_under_roots(&temp)?;
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
                // A tag write's hash is known once its temp is written; the
                // row records it when it moves to staged.
                staged_sha256: if tags_change {
                    String::new()
                } else {
                    item.fingerprint.sha256.clone()
                },
                track_id: Some(item.track_id.clone()),
                commit_as: Some(if item.adopt.is_some() {
                    MgmtState::Imported
                } else {
                    MgmtState::Managed
                }),
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
            // One read of the source: hashed while it is copied.
            let read = paths::copy_hashed(&source, &temp, mode)?;
            if read.size != item.fingerprint.size || read.sha256 != item.fingerprint.sha256 {
                return Err(PublishError::Validation(
                    format!("source for track {} changed after planning", item.track_id).into(),
                ));
            }
            self.known.record(read.stat, &read.sha256);
            let staged_sha = if tags_change {
                super::staging::stage_tags(&temp, &item.managed_updates, pictures.as_deref())
                    .map_err(|error| {
                        error
                            .track(&item.track_id)
                            .file(&item.source_root, &item.source_rel)
                    })?
            } else {
                read.sha256
            };
            let staged_stat = paths::FileStat::read(&temp).map_err(PublishError::from)?;
            self.known.record(staged_stat, &staged_sha);
            if let Some(parent) = temp.parent() {
                super::journal::fsync_dir(parent)?;
            }
            self.maybe_crash(CrashPoint::AfterTempWritten)?;
            if item.adopt.is_none() {
                self.capture_before(bundle, item, before)?;
            }
            // Staged only once its before-state is kept: recovery resumes
            // a bundle only when every row is staged. This durable write
            // also puts the before-state on disk ahead of the placement
            // that overwrites the file.
            durable(&self.db, "publish.journal_staged", |tx| {
                JournalWriter::new(tx).stage(&journal_id, &staged_sha)
            })?;
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
            if ordinal == 0 && bundle.items.len() > 1 {
                self.maybe_crash(CrashPoint::AfterFirstFileStaged)?;
            }
        }
        Ok(())
    }

    /// One blob's bytes, hash-verified.
    fn blob(&self, sha256: &str) -> Result<Vec<u8>, PublishError> {
        long_read(&self.db, "publish.blob", |conn| {
            self.blobs.get(conn, sha256)
        })
    }

    /// The pictures a plan item sets, with their bytes from the blob
    /// store; `None` when it leaves the file's pictures alone.
    fn planned_pictures(
        &self,
        item: &super::planner::PlanItem,
    ) -> Result<Option<Vec<crate::tags::save::PictureSpec>>, PublishError> {
        let Some(pictures) = item.managed_updates.pictures() else {
            return Ok(None);
        };
        let mut specs = Vec::with_capacity(pictures.len());
        for picture in pictures {
            let data = self.blob(&picture.sha256)?;
            if sha256_hex(&data) != picture.sha256 {
                return Err(PublishError::Validation(
                    "a planned picture is not the stored one".into(),
                ));
            }
            specs.push(crate::tags::save::PictureSpec {
                kind: picture.kind,
                mime: picture.mime.clone(),
                description: picture.description.clone(),
                data,
                info: picture.info,
            });
        }
        Ok(Some(specs))
    }

    /// Before-state for undo and the first-management baseline: its
    /// files in the undo folder first, then the rows naming them in one
    /// ordinary write. The durable `Staged` write that follows puts the
    /// rows on disk before the file is overwritten.
    fn capture_before(
        &self,
        bundle: &PlanBundle,
        item: &super::planner::PlanItem,
        before: TagDocument,
    ) -> Result<(), PublishError> {
        let (today, expires) = (self.today_day, self.today_day + self.undo_retention_days);
        let prior_mgmt = long_read(&self.db, "publish.capture_state", |conn| {
            management_state(conn, &item.track_id)
        })?;
        let before_state = super::undo::BeforeState {
            doc: before,
            source_root: item.source_root.clone(),
            source_sha256: item.fingerprint.sha256.clone(),
            source_rel: item.source_rel.clone(),
            mgmt_state_before: prior_mgmt.clone(),
        };
        let doc = before_state.to_bytes()?;
        let pictures = &before_state.doc.picture_bytes;
        let mut blobs: Vec<&[u8]> = vec![&doc];
        blobs.extend(pictures.values().map(Vec::as_slice));
        self.blobs.keep(
            &self.db,
            Lane::Background,
            "publish.capture_before",
            &blobs,
            |tx, hashes| {
                if management_state(tx, &item.track_id)? != prior_mgmt {
                    return Err(PublishError::Snapshot(
                        format!(
                            "track {} changed while its before-state was kept",
                            item.track_id
                        )
                        .into(),
                    ));
                }
                if !pictures.keys().eq(hashes.iter().skip(1)) {
                    return Err(PublishError::Snapshot("picture bytes hash mismatch".into()));
                }
                let blob = hashes
                    .first()
                    .ok_or_else(|| PublishError::Snapshot("before-state was not kept".into()))?;
                let snapshots = SnapshotStore::new(tx);
                let baselines = super::snapshots::BaselineStore::new(tx);
                // First-management baseline: captured once, immutable
                // forever. A baseline that appeared concurrently wins; the
                // only Snapshot error capture raises is already-exists.
                if baselines.get(&item.track_id)?.is_none() {
                    for sha256 in pictures.keys() {
                        snapshots.add_ref(sha256, "baseline", &item.track_id)?;
                    }
                    match baselines.capture(
                        &item.track_id,
                        blob,
                        &item.source_root,
                        &item.source_rel,
                        today,
                    ) {
                        Ok(()) | Err(PublishError::Snapshot(_)) => {}
                        Err(other) => return Err(other),
                    }
                }
                for sha256 in pictures.keys() {
                    snapshots.add_ref(sha256, "operation", &bundle.id)?;
                }
                snapshots.record(
                    &format!("{}-{}", bundle.id, item.track_id),
                    &bundle.id,
                    &item.track_id,
                    blob,
                    today,
                    expires,
                )
            },
        )
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
        let journal_id = sidecar_journal_id(audio_journal_id, side_ordinal);
        let dest = self
            .sandbox
            .resolve_no_symlink(&sidecar.dest_root, &sidecar.dest_rel)?;
        self.sandbox.ensure_under_roots(&dest)?;
        if let Some(parent) = dest.parent() {
            super::folders::make(
                &self.db,
                &self.sandbox,
                &bundle.id,
                &sidecar.dest_root,
                parent,
            )?;
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
            staged_sha256: sidecar.fingerprint.sha256.clone(),
            track_id: None,
            commit_as: None,
            state: JournalState::Prepared,
            seq: 0,
            replaces: None,
        })?;
        let mode = keep_mode
            .then(|| paths::permission_bits(&source))
            .transpose()?;
        let read = paths::copy_hashed(&source, &temp, mode)?;
        if read.size != sidecar.fingerprint.size || read.sha256 != sidecar.fingerprint.sha256 {
            return Err(PublishError::Validation(
                format!("sidecar {} changed after planning", sidecar.source_rel).into(),
            ));
        }
        self.known.record(read.stat, &read.sha256);
        self.known
            .record(paths::FileStat::read(&temp)?, &read.sha256);
        if let Some(parent) = temp.parent() {
            super::journal::fsync_dir(parent)?;
        }
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
            return Err(PublishError::Validation(
                format!(
                    "generated {} for {} is not the planned file",
                    output.kind.as_str(),
                    output.dest_rel
                )
                .into(),
            ));
        }
        let journal_id = output_journal_id(audio_journal_id, index);
        let dest = self
            .sandbox
            .resolve_no_symlink(&output.dest_root, &output.dest_rel)?;
        self.sandbox.ensure_under_roots(&dest)?;
        if let Some(parent) = dest.parent() {
            super::folders::make(
                &self.db,
                &self.sandbox,
                &bundle.id,
                &output.dest_root,
                parent,
            )?;
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
            commit_as: None,
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
            commit_as: None,
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

    /// Journal one file a removal takes away, inserted and staged in one
    /// durable write. Nothing moves until publish.
    fn prepare_remove(
        &mut self,
        bundle: &PlanBundle,
        index: usize,
        item: &super::planner::RemoveItem,
        file: &super::planner::RemoveFile,
    ) -> Result<(), PublishError> {
        let journal_id = super::remove::journal_id(&bundle.id, index);
        let dest = self
            .sandbox
            .resolve_removal(&item.seal.root_id, &item.seal.relative_path)?;
        let aside = self.sandbox.backup_path_for(&dest, &journal_id)?;
        self.sandbox.ensure_under_removal_roots(&aside)?;
        let journal = FileJournal {
            id: journal_id.clone(),
            bundle_id: bundle.id.clone(),
            kind: JournalKind::Remove,
            source: None,
            dest_root: item.seal.root_id.clone(),
            dest_rel: item.seal.relative_path.clone(),
            staged: aside.to_string_lossy().to_string(),
            backup: None,
            source_sha256: Some(file.fingerprint.sha256.clone()),
            staged_sha256: file.fingerprint.sha256.clone(),
            track_id: None,
            commit_as: None,
            state: JournalState::Prepared,
            seq: 0,
            replaces: Some(JournalReplacement {
                rel: item.seal.relative_path.clone(),
                sha256: file.fingerprint.sha256.clone(),
                track_id: Some(item.track_id.clone()),
                recycle_to: file.recycle_to.clone(),
            }),
        };
        durable(&self.db, "publish.journal_remove", |tx| {
            let writer = JournalWriter::new(tx);
            writer.insert(&journal)?;
            writer.advance(&journal_id, JournalState::Prepared, JournalState::Staged)
        })
    }

    /// Journal the file a bundle adds and write its bytes to the row's
    /// hidden temp, or, for the undo of an add, journal taking that file
    /// away (inserted and staged in one durable write; nothing moves until
    /// publish).
    fn prepare_added(
        &mut self,
        bundle: &PlanBundle,
        added: &super::add::Added,
    ) -> Result<(), PublishError> {
        let journal_id = super::add::journal_id(&bundle.id, added);
        let file = &added.file;
        let dest = self
            .sandbox
            .resolve_no_symlink(&file.root_id, &file.relative_path)?;
        self.sandbox.ensure_under_roots(&dest)?;
        let temp = self.sandbox.temp_path_for(&dest, &journal_id)?;
        self.sandbox.ensure_under_roots(&temp)?;
        let mut journal = FileJournal {
            id: journal_id.clone(),
            bundle_id: bundle.id.clone(),
            kind: JournalKind::Add,
            source: None,
            dest_root: file.root_id.clone(),
            dest_rel: file.relative_path.clone(),
            staged: temp.to_string_lossy().to_string(),
            backup: None,
            source_sha256: None,
            staged_sha256: file.sha256.clone(),
            track_id: None,
            commit_as: None,
            state: JournalState::Prepared,
            seq: 0,
            replaces: None,
        };
        if added.undoes.is_some() {
            super::generated::check(&dest, &file.sha256)?;
            journal.kind = JournalKind::Delete;
            journal.source_sha256 = Some(file.sha256.clone());
            return durable(&self.db, "publish.journal_take_back", |tx| {
                let writer = JournalWriter::new(tx);
                writer.insert(&journal)?;
                writer.advance(&journal_id, JournalState::Prepared, JournalState::Staged)
            });
        }
        let bytes = self
            .added_bytes
            .take()
            .ok_or_else(|| PublishError::Journal("an added file came with no bytes".into()))?;
        if bytes.len() as u64 != file.size || sha256_hex(&bytes) != file.sha256 {
            return Err(PublishError::Validation(
                "the bytes to add are not the planned file".into(),
            ));
        }
        if let Some(parent) = dest.parent() {
            super::folders::make(&self.db, &self.sandbox, &bundle.id, &file.root_id, parent)?;
            // A folder swapped for a link while it was being made.
            paths::ensure_no_symlink_below(self.sandbox.root_dir(&file.root_id)?, parent)?;
        }
        self.journal_insert(&journal)?;
        write_staged_temp(&temp, &bytes, None)?;
        self.maybe_crash(CrashPoint::AfterTempWritten)?;
        self.journal_staged(&journal_id)
    }

    /// Journal one file a put-back brings home, then make the row's hidden
    /// temp beside home: a hard link of its recycled copy when both are on
    /// one filesystem, else a copy of it with the copy's permission bits
    /// and modified time, hashed in the one read that copies it.
    fn prepare_put_back(
        &mut self,
        bundle: &PlanBundle,
        index: usize,
        item: &super::planner::PutBackItem,
    ) -> Result<(), PublishError> {
        let journal_id = super::put_back::journal_id(&bundle.id, index);
        let home = self
            .sandbox
            .resolve_removal(&item.root_id, &item.relative_path)?;
        if let Some(parent) = home.parent() {
            super::folders::make(&self.db, &self.sandbox, &bundle.id, &item.root_id, parent)?;
        }
        let temp = self.sandbox.temp_path_for(&home, &journal_id)?;
        self.sandbox.ensure_under_removal_roots(&temp)?;
        self.journal_insert(&FileJournal {
            id: journal_id.clone(),
            bundle_id: bundle.id.clone(),
            kind: JournalKind::PutBack,
            source: None,
            dest_root: item.root_id.clone(),
            dest_rel: item.relative_path.clone(),
            staged: temp.to_string_lossy().to_string(),
            backup: None,
            source_sha256: Some(item.fingerprint.sha256.clone()),
            staged_sha256: item.fingerprint.sha256.clone(),
            track_id: Some(item.track_id.clone()),
            commit_as: None,
            state: JournalState::Prepared,
            seq: 0,
            replaces: None,
        })?;
        if !super::put_back::link_temp(&self.sandbox, item, &temp, self.place_seams)? {
            let copy = std::fs::symlink_metadata(&item.copy).map_err(PublishError::from)?;
            let bits = {
                use std::os::unix::fs::PermissionsExt as _;
                copy.permissions().mode() & 0o777
            };
            super::put_back::copy_into(&self.sandbox, item, &temp, bits)?;
            // The file comes home with the time it was last changed, not today.
            let dated = copy.modified().and_then(|modified| {
                use std::os::unix::fs::OpenOptionsExt as _;
                std::fs::OpenOptions::new()
                    .write(true)
                    .custom_flags(libc::O_NOFOLLOW)
                    .open(&temp)?
                    .set_modified(modified)
            });
            if let Err(error) = dated {
                tracing::warn!(%error, "a file put back keeps today's modified time");
            }
        }
        self.temps
            .insert(journal_id.clone(), paths::FileStat::read(&temp)?);
        self.maybe_crash(CrashPoint::AfterTempWritten)?;
        self.journal_staged(&journal_id)
    }

    /// Compensate a pre-commit failure with the unwind recovery uses:
    /// remove published destinations whose bytes still match the staged
    /// copy, restore same-path backups, delete unpublished temps, and
    /// mark journals compensated. A row whose original could not go home
    /// keeps its backup and needs attention; the publish still fails with
    /// its own error. True when a backup was kept.
    fn compensate(&mut self, bundle: &PlanBundle) -> Result<bool, PublishError> {
        Ok(self.run().unwind(&bundle.id)?.is_some())
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
            return Err(PublishError::InjectedCrash(point));
        }
        Ok(())
    }
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

/// Probe the root a journal's file is under, by the steps its kind may
/// take: a removal's root may be a remove-only one.
pub(super) fn probe_root(sandbox: &Sandbox, journal: &FileJournal) -> Result<(), PublishError> {
    match journal.kind {
        JournalKind::Remove | JournalKind::PutBack => {
            sandbox.probe_removal_placement(&journal.dest_root)?
        }
        JournalKind::Audio
        | JournalKind::Sidecar
        | JournalKind::Artwork
        | JournalKind::Output
        | JournalKind::Add
        | JournalKind::Delete => sandbox.probe_placement(&journal.dest_root)?,
    };
    Ok(())
}

/// Every journal row `prepare` writes for a bundle staged in full.
pub(super) fn journal_ids(bundle: &PlanBundle) -> Vec<String> {
    let mut ids = bundle
        .removal
        .as_ref()
        .map(|removal| super::remove::journal_ids(&bundle.id, removal))
        .unwrap_or_default();
    if let Some(put_back) = &bundle.put_back {
        ids.extend(super::put_back::journal_ids(&bundle.id, put_back));
    }
    ids.extend(super::add::journal_ids(bundle));
    for (ordinal, item) in bundle.items.iter().enumerate() {
        let audio = audio_journal_id(&bundle.id, ordinal);
        ids.extend((0..item.sidecars.len()).map(|index| sidecar_journal_id(&audio, index)));
        ids.extend((0..item.outputs.len()).map(|index| output_journal_id(&audio, index)));
        ids.extend((0..item.removals.len()).map(|index| removal_journal_id(&audio, index)));
        ids.push(audio);
    }
    ids
}

/// Write bytes held in memory (a generated file, a file a plugin adds) to
/// a new staged temp, with the permission bits `mode` when given, and put
/// it and its folder on disk. Whatever was under the name goes first; a
/// temp that fails part way is ours (just created), so it goes too.
fn write_staged_temp(temp: &Path, bytes: &[u8], mode: Option<u32>) -> Result<(), PublishError> {
    use std::io::Write as _;
    let written = (|| {
        let mut file = paths::create_hidden(temp).map_err(PublishError::from)?;
        file.write_all(bytes).map_err(PublishError::from)?;
        paths::lend_mode(&file, mode, temp);
        super::journal::fsync_file(&file)?;
        if let Some(parent) = temp.parent() {
            super::journal::fsync_dir(parent)?;
        }
        Ok(())
    })();
    if written.is_err()
        && let Err(error) = std::fs::remove_file(temp)
        && error.kind() != std::io::ErrorKind::NotFound
    {
        tracing::warn!(temp = %temp.display(), %error, "partial staging temp not removed");
    }
    written
}

/// Prune newly empty source parents up to (not including) the root,
/// after a moved file's source went. Each folder goes only while empty and
/// never through a link below the root. An empty folder left behind is not
/// worth a stuck write, so a failure is logged and the cleanup stands.
pub(super) fn prune_empty_parents(sandbox: &Sandbox, root_id: &str, source: &Path) {
    let Ok(root) = sandbox.root_dir(root_id) else {
        return;
    };
    let mut cursor = source.parent();
    while let Some(dir) = cursor {
        if dir == root || !dir.starts_with(root) {
            break;
        }
        if let Err(error) = paths::ensure_no_symlink_below(root, dir) {
            tracing::warn!(dir = %dir.display(), %error, "an emptied source folder was left");
            break;
        }
        match std::fs::remove_dir(dir) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error)
                if error.kind() == std::io::ErrorKind::DirectoryNotEmpty
                    || error.raw_os_error() == Some(libc::EEXIST) =>
            {
                break;
            }
            Err(error) => {
                tracing::warn!(dir = %dir.display(), %error, "an emptied source folder was left");
                break;
            }
        }
        cursor = dir.parent();
    }
}
