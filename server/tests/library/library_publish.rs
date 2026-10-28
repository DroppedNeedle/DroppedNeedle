//! Publisher slice briefs: seal, crash matrix, preservation, archive safety.
//!
//! Each test pins one publisher behavior against a scratch sandbox and a
//! scratch application database (every migration applied, the bundle's
//! tracks seeded into the catalog): sealed previews reject stale state,
//! crash injection at every phase resumes or compensates without
//! half-renames or occupied overwrites, unknown tags and frames
//! survive Apply, and hostile archives never extract. The publisher
//! writes only under sandbox roots; nothing here touches real media.
//!
//! Tag staging goes through the real tag save
//! wrapper; publish-path briefs plant real
//! committed fixtures, never fake bytes with audio extensions.

use droppedneedle_library::publish;

use droppedneedle_library::tags;

use std::collections::BTreeMap;
use std::path::PathBuf;

use crate::common::ScratchDir;
use droppedneedle_kernel::clock::{ManualClock, SystemClock};
use droppedneedle_kernel::db::undo_dir::{self, Stored, UndoDir};
use droppedneedle_kernel::db::{Db, DurableLane, Lane};
use droppedneedle_kernel::jobs::queue::JobQueue;
use publish::add::{Added, AddedFile, Origin, TakeBackRefusal};
use publish::paths::Root;
use publish::place::{PlacementMode, Seams};
use publish::planner::{
    Capability, CapabilityGate, CollisionGate, FileFingerprint, OutputKind, PlanBundle, PlanItem,
    PlanKind, ReleaseIdentity, SealError, SealRecheck, SealedPreview, SealedRow, SidecarPlan,
    SpaceProbe,
};
use publish::publisher::{locate, read_revision};
use publish::remove;
use publish::snapshots::{BaselineStore, BlobStore, SnapshotStore, sha256_hex};
use publish::tags_seam::{TagDocument, TagTarget, TagUpdates};
use publish::undo::{
    BaselineBlock, BaselineInput, BeforeState, UndoBlock, UndoInput, UndoLive,
    confirm_baseline_purge, plan_baseline_restore, plan_undo,
};
use publish::{
    ArchiveEntry, ArchivePolicy, CrashPoint, JournalState, JournalStore, PublishError,
    PublishOutcome, Publisher, Sandbox, validate_archive,
};
use rusqlite::Connection;
use tags::AudioFormat;
use tags::TagField;

/// Tracks the bundles in these tests publish, seeded into the catalog
/// at their source paths.
const CATALOG_TRACKS: [(&str, &str); 4] = [
    ("track-a", "staging/a.flac"),
    ("track-b", "staging/b.flac"),
    ("track-s", "staging/s.flac"),
    ("t", "s.flac"),
];

/// The catalog revision a fresh fixture database starts at (migrations
/// may move it), so bundles plan against the real value.
fn db_catalog_revision(fix: &Fixture) -> u64 {
    let conn = droppedneedle_kernel::db::open_connection(&fix.db_path).unwrap();
    let value: i64 = conn
        .query_row(
            "SELECT value FROM library_catalog_revision WHERE singleton = 1",
            [],
            |row| row.get(0),
        )
        .unwrap();
    value as u64
}

/// The catalog revision every fresh fixture starts from.
fn start_revision() -> u64 {
    db_catalog_revision(&fixture("start-revision"))
}

/// A migrated scratch database with the test tracks in the catalog, and
/// the album and track identities [`test_identity`] names.
fn scratch_db(dir: &std::path::Path) -> PathBuf {
    let db_path = dir.join("app.db");
    crate::common::migrated_db(&db_path);
    seed_catalog(&db_path);
    db_path
}

/// The test tracks, album and identities of [`scratch_db`].
fn seed_catalog(db_path: &std::path::Path) {
    let conn = droppedneedle_kernel::db::open_connection(db_path).unwrap();
    conn.execute_batch(
        "INSERT INTO local_albums (id, root_id, grouping_key, title, title_folded, \
         album_artist_id, grouping_source, created_at, updated_at) VALUES ('album', 'music', \
         'k', 'Album', 'album', '00000000-0000-4000-8000-000000000002', 'automatic', 0, 0);",
    )
    .unwrap();
    for (id, rel) in CATALOG_TRACKS {
        conn.execute(
            "INSERT INTO local_tracks (id, local_album_id, root_id, file_path, relative_path, \
             path_hash, file_size_bytes, file_mtime_ns, stat_revision, title, title_folded, \
             album_title, album_title_folded, file_format, ingest_source, imported_at, \
             membership_source) VALUES (?1, 'album', 'music', ?2, ?2, ?1, 0, 0, '0:0', ?1, ?1, \
             'Album', 'album', 'flac', 'scan', 0, 'automatic')",
            rusqlite::params![id, rel],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO local_track_external_identities (local_track_id, provider, \
             recording_mbid, release_mbid, release_track_mbid, decision_source, selected_at, \
             row_revision) VALUES (?1, 'musicbrainz', 'rec-1', 'release-1', 'rt-1', 'manual', \
             0, 4)",
            [id],
        )
        .unwrap();
    }
    conn.execute_batch(
        "INSERT INTO local_album_external_identities (local_album_id, provider, \
         release_group_mbid, release_mbid, decision_source, selected_at, row_revision) \
         VALUES ('album', 'musicbrainz', 'rg-1', 'release-1', 'manual', 0, 9);",
    )
    .unwrap();
}

/// Another track of the album in the catalog at `rel`, with no file.
fn seed_track(fix: &Fixture, id: &str, rel: &str, availability: &str, missing_since: Option<f64>) {
    open_db(fix)
        .execute(
            "INSERT INTO local_tracks (id, local_album_id, root_id, file_path, relative_path, \
             path_hash, file_size_bytes, file_mtime_ns, stat_revision, title, title_folded, \
             album_title, album_title_folded, file_format, ingest_source, imported_at, \
             membership_source, availability, missing_since) VALUES (?1, 'album', 'music', ?2, \
             ?2, ?1, 0, 0, '0:0', ?1, ?1, 'Album', 'album', 'flac', 'scan', 0, 'automatic', \
             ?3, ?4)",
            rusqlite::params![id, rel, availability, missing_since],
        )
        .unwrap();
}

/// A track row's path, availability and `missing_since`.
fn track_row(conn: &Connection, id: &str) -> (String, String, Option<f64>) {
    conn.query_row(
        "SELECT relative_path, availability, missing_since FROM local_tracks WHERE id = ?1",
        [id],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    )
    .unwrap()
}

/// A side connection for seeding and checking rows.
fn open_db(fix: &Fixture) -> Connection {
    droppedneedle_kernel::db::open_connection(&fix.db_path).unwrap()
}

/// The undo copies beside the fixture's database.
fn blobs(fix: &Fixture) -> BlobStore {
    BlobStore::new(UndoDir::new(undo_dir::beside(&fix.db_path)))
}

/// Put bytes in the fixture's undo folder, naming them by their hash.
fn undo_put(fix: &Fixture, bytes: &[u8]) -> String {
    let dir = blobs(fix).dir().clone();
    let hold = dir.hold_shared().unwrap();
    dir.put(&hold, bytes).unwrap()
}

/// The database as the next start sees it: a new handle on the file. The
/// publisher that ran before is dropped first, as a stopped server is.
fn restart(fix: &Fixture) -> Db {
    Db::open_for_tests(&fix.db_path).unwrap()
}

/// The jobs table a publisher queues a commit's follow-up work on.
fn jobs(db: &Db) -> JobQueue {
    JobQueue::new(db.clone(), std::sync::Arc::new(SystemClock))
}

/// Publish recovery over `db`, queueing on its jobs table.
fn reconcile(
    db: &Db,
    sandbox: &Sandbox,
) -> Result<Vec<publish::recovery::BundleRecovery>, PublishError> {
    publish::recovery::reconcile(db, sandbox, &jobs(db), &SystemClock)
}

/// [`reconcile`] on a filesystem made to lack a placement step.
fn reconcile_with_seams(
    db: &Db,
    sandbox: &Sandbox,
    seams: Seams,
) -> Result<Vec<publish::recovery::BundleRecovery>, PublishError> {
    publish::recovery::reconcile_with_seams(db, sandbox, &jobs(db), &SystemClock, seams)
}

/// Fixed free-space probe so disk preflight pins exact values.
struct FixedSpace(u64);

impl SpaceProbe for FixedSpace {
    fn free_bytes(&self, _root_id: &str) -> Result<u64, PublishError> {
        Ok(self.0)
    }
}

struct Fixture {
    dir: PathBuf,
    sandbox: Sandbox,
    db_path: PathBuf,
    _scratch: ScratchDir,
}

fn fixture(tag: &str) -> Fixture {
    let scratch = ScratchDir::new(&format!("publish-{tag}"));
    let dir = scratch.to_path_buf();
    let music = dir.join("music");
    let incoming = dir.join("incoming");
    std::fs::create_dir_all(&music).unwrap();
    std::fs::create_dir_all(&incoming).unwrap();
    let sandbox = Sandbox::new(vec![
        Root::library("music", music),
        Root::library("incoming", incoming),
        Root::bin("bin", dir.join("bin")),
    ])
    .unwrap();
    let db_path = scratch_db(&dir);
    Fixture {
        dir,
        sandbox,
        db_path,
        _scratch: scratch,
    }
}

fn write_source(fix: &Fixture, root: &str, rel: &str, bytes: &[u8]) -> FileFingerprint {
    let path = fix.sandbox.resolve(root, rel).unwrap();
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, bytes).unwrap();
    FileFingerprint {
        size: bytes.len() as u64,
        sha256: sha256_hex(bytes),
    }
}

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/library")
}

/// Plant a real committed audio fixture as a publish source. Staging
/// runs the real save wrapper, so audio sources must parse.
fn write_fixture_source(fix: &Fixture, root: &str, rel: &str, name: &str) -> FileFingerprint {
    let bytes = std::fs::read(fixtures_dir().join(name)).unwrap();
    write_source(fix, root, rel, &bytes)
}

fn test_identity() -> ReleaseIdentity {
    ReleaseIdentity {
        release_mbid: "release-1".to_string(),
        release_group_mbid: "rg-1".to_string(),
        recording_mbid: "rec-1".to_string(),
        release_track_mbid: "rt-1".to_string(),
        album_identity_revision: 9,
        mapping_revision: 4,
    }
}

fn plan_item(
    track: &str,
    source_rel: &str,
    dest_rel: &str,
    fingerprint: FileFingerprint,
    kind: PlanKind,
) -> PlanItem {
    let mut managed_updates = TagUpdates::new();
    managed_updates.insert(
        TagTarget::Field(TagField::Title),
        vec![format!("Title {track}")],
    );
    PlanItem {
        track_id: track.to_string(),
        source_root: "music".to_string(),
        source_rel: source_rel.to_string(),
        dest_root: "music".to_string(),
        dest_rel: dest_rel.to_string(),
        kind,
        fingerprint,
        identity: test_identity(),
        override_revision: 2,
        capabilities: vec![Capability::Metadata, Capability::SameRootMove],
        format: AudioFormat::Flac,
        managed_updates,
        sidecars: Vec::new(),
        staged_bytes_estimate: 1024,
        adopt: None,
        replaces: None,
        outputs: Vec::new(),
        removals: Vec::new(),
        sealed: Some(SealedRow {
            album_id: "album".to_string(),
            stat_revision: "0:0".to_string(),
            album_release_group_mbid: "rg-1".to_string(),
            album_release_mbid: Some("release-1".to_string()),
            recording_mbid: Some("rec-1".to_string()),
            release_track_mbid: Some("rt-1".to_string()),
        }),
    }
}

fn two_track_bundle(fix: &Fixture, kind: PlanKind) -> (PlanBundle, BTreeMap<String, TagDocument>) {
    let dest_a = match kind {
        PlanKind::Move => "organized/a.flac",
        PlanKind::SamePath => "staging/a.flac",
    };
    let dest_b = match kind {
        PlanKind::Move => "organized/b.flac",
        PlanKind::SamePath => "staging/b.flac",
    };
    let fp_a = write_fixture_source(fix, "music", "staging/a.flac", "management_full.flac");
    let fp_b = write_fixture_source(fix, "music", "staging/b.flac", "management_full.flac");
    let mut item_a = plan_item("track-a", "staging/a.flac", dest_a, fp_a, kind);
    if kind == PlanKind::Move {
        let fp_cover = write_source(fix, "music", "staging/cover.jpg", b"cover-bytes");
        item_a.sidecars.push(SidecarPlan {
            source_root: "music".to_string(),
            source_rel: "staging/cover.jpg".to_string(),
            dest_root: "music".to_string(),
            dest_rel: "organized/cover.jpg".to_string(),
            fingerprint: fp_cover,
        });
    }
    let item_b = plan_item("track-b", "staging/b.flac", dest_b, fp_b, kind);
    let bundle = PlanBundle {
        id: "bundle-1".to_string(),
        items: vec![item_a, item_b],
        profile_revision: 7,
        naming_revision: 3,
        policy_revision: 5,
        put_back: None,
        added: None,
        removal: None,
    };
    let mut docs = BTreeMap::new();
    docs.insert("track-a".to_string(), TagDocument::empty());
    docs.insert("track-b".to_string(), TagDocument::empty());
    (bundle, docs)
}

fn seal(bundle: &PlanBundle) -> (SealedPreview, SealRecheck) {
    let sealed = SealedPreview::seal(bundle.clone(), "tok".to_string(), 11, 100);
    let mut fingerprints = BTreeMap::new();
    let mut identities = BTreeMap::new();
    let mut overrides = BTreeMap::new();
    for item in bundle.items.iter() {
        fingerprints.insert(item.track_id.clone(), item.fingerprint.clone());
        identities.insert(item.track_id.clone(), item.identity.clone());
        overrides.insert(item.track_id.clone(), item.override_revision);
    }
    let live = SealRecheck {
        fingerprints,
        identities,
        overrides,
        profile_revision: bundle.profile_revision,
        naming_revision: bundle.naming_revision,
        policy_revision: bundle.policy_revision,
        settings_revision: 11,
        today_day: 50,
        token_hash: "tok".to_string(),
    };
    (sealed, live)
}

/// Day 50, in unix milliseconds.
const DAY_50_MS: i64 = 50 * 86_400_000;

fn open_publisher(fix: &Fixture) -> Publisher<FixedSpace> {
    Publisher::new(
        fix.sandbox.clone(),
        restart(fix),
        FixedSpace(u64::MAX),
        jobs(&restart(fix)),
        std::sync::Arc::new(ManualClock::at(DAY_50_MS)),
        blobs(fix),
    )
}

/// Staged bytes carry the planned title through the real save
/// wrapper and still decode as audio.
fn assert_staged(dest: &std::path::Path, track_id: &str) {
    let tag = tags::read::read_tag_only(dest, tags::AudioFormat::Flac).unwrap();
    assert_eq!(tag.title, format!("Title {track_id}"));
    assert_eq!(tag.genres, vec!["Electronic", "Ambient"]);
    tags::probe(dest).unwrap();
}

fn hidden_leftovers(dir: &std::path::Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(current) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&current) else {
            continue;
        };
        for entry in entries.flatten() {
            let name = entry.file_name();
            let text = name.to_string_lossy().to_string();
            if text.starts_with(publish::HIDDEN_PREFIX) {
                out.push(entry.path());
            }
            if entry.file_type().map(|kind| kind.is_dir()).unwrap_or(false) {
                stack.push(entry.path());
            }
        }
    }
    out
}

// --- Seal briefs: stale state is rejected before any mutation. ---

#[test]
fn seal_rejects_stale_file() {
    let fix = fixture("stale-file");
    let (bundle, docs) = two_track_bundle(&fix, PlanKind::Move);
    let (sealed, mut live) = seal(&bundle);
    live.fingerprints.insert(
        "track-a".to_string(),
        FileFingerprint {
            size: 3,
            sha256: "changed".to_string(),
        },
    );
    let mut publisher = open_publisher(&fix);
    let err = publisher.publish(&sealed, &live, &docs).unwrap_err();
    assert!(matches!(err, PublishError::Validation(_)), "got {err:?}");
    assert!(matches!(
        sealed.recheck(&live),
        Err(SealError::StaleFile(_))
    ));
    assert!(
        fix.sandbox
            .resolve("music", "organized/a.flac")
            .unwrap()
            .symlink_metadata()
            .is_err()
    );
}

#[test]
fn seal_rejects_stale_identity() {
    let fix = fixture("stale-identity");
    let (bundle, docs) = two_track_bundle(&fix, PlanKind::Move);
    let (sealed, mut live) = seal(&bundle);
    let mut changed = test_identity();
    changed.release_track_mbid = "rt-other".to_string();
    live.identities.insert("track-b".to_string(), changed);
    let mut publisher = open_publisher(&fix);
    let err = publisher.publish(&sealed, &live, &docs).unwrap_err();
    assert!(matches!(err, PublishError::Validation(_)), "got {err:?}");
    assert!(matches!(
        sealed.recheck(&live),
        Err(SealError::StaleIdentity(_))
    ));
}

#[test]
fn seal_rejects_stale_profile() {
    let fix = fixture("stale-profile");
    let (bundle, docs) = two_track_bundle(&fix, PlanKind::Move);
    let (sealed, mut live) = seal(&bundle);
    live.profile_revision = 8;
    let mut publisher = open_publisher(&fix);
    let err = publisher.publish(&sealed, &live, &docs).unwrap_err();
    assert!(matches!(err, PublishError::Validation(_)), "got {err:?}");
    assert!(matches!(
        sealed.recheck(&live),
        Err(SealError::StaleProfile(_))
    ));
}

#[test]
fn seal_rejects_stale_policy() {
    let fix = fixture("stale-policy");
    let (bundle, docs) = two_track_bundle(&fix, PlanKind::Move);
    let (sealed, mut live) = seal(&bundle);
    live.policy_revision = 6;
    let mut publisher = open_publisher(&fix);
    let err = publisher.publish(&sealed, &live, &docs).unwrap_err();
    assert!(matches!(err, PublishError::Validation(_)), "got {err:?}");
    assert!(matches!(
        sealed.recheck(&live),
        Err(SealError::StalePolicy(_))
    ));
}

#[test]
fn seal_rejects_expired_and_bad_token() {
    let fix = fixture("stale-token");
    let (bundle, _) = two_track_bundle(&fix, PlanKind::Move);
    let (sealed, mut live) = seal(&bundle);
    live.today_day = 100;
    assert!(matches!(sealed.recheck(&live), Err(SealError::Expired)));
    live.today_day = 50;
    live.token_hash = "wrong".to_string();
    assert!(matches!(sealed.recheck(&live), Err(SealError::BadToken)));
}

// --- Happy path: two-file album plus sidecar commits atomically. ---

#[test]
fn move_bundle_commits_atomically() {
    let fix = fixture("happy-move");
    let (bundle, docs) = two_track_bundle(&fix, PlanKind::Move);
    let (sealed, live) = seal(&bundle);
    let mut publisher = open_publisher(&fix);
    let outcome = publisher.publish(&sealed, &live, &docs).unwrap();
    assert_eq!(outcome, PublishOutcome::Committed);

    for item in bundle.items.iter() {
        let dest = fix
            .sandbox
            .resolve(&item.dest_root, &item.dest_rel)
            .unwrap();
        assert_staged(&dest, &item.track_id);
        let source = fix
            .sandbox
            .resolve(&item.source_root, &item.source_rel)
            .unwrap();
        assert!(
            source.symlink_metadata().is_err(),
            "source removed after commit"
        );
    }
    let cover = fix.sandbox.resolve("music", "organized/cover.jpg").unwrap();
    assert_eq!(std::fs::read(&cover).unwrap(), b"cover-bytes");
    assert!(
        hidden_leftovers(&fix.dir).is_empty(),
        "no staging debris remains"
    );

    let journals = JournalStore::new(&open_db(&fix))
        .bundle("bundle-1")
        .unwrap();
    assert_eq!(journals.len(), 3);
    for journal in journals {
        assert_eq!(journal.state, JournalState::Cleaned);
        assert!(journal.seq >= 4, "monotonic transitions recorded");
    }
    // The catalog the reads API serves follows the move in the same
    // transaction, and its revision bump invalidates read caches.
    assert_eq!(read_revision(&open_db(&fix)).unwrap(), start_revision() + 1);
    let located = locate(&open_db(&fix), "track-a").unwrap().unwrap();
    assert_eq!(located.root_id, "music");
    assert_eq!(located.rel_path, "organized/a.flac");
    let at_dest: i64 = open_db(&fix)
        .query_row(
            "SELECT COUNT(*) FROM local_tracks WHERE id = 'track-a' \
             AND relative_path = 'organized/a.flac'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(at_dest, 1);
    // Nothing the server owns is left inside the music root.
    assert!(hidden_leftovers(&fix.dir.join("music")).is_empty());
}

// --- Crash matrix: restart resumes or compensates at every phase. ---

#[test]
fn crash_at_every_phase_resumes_without_half_state() {
    for point in CrashPoint::all() {
        let fix = fixture(&format!("crash-{}", point.as_str()));
        let (bundle, docs) = two_track_bundle(&fix, PlanKind::Move);
        // The app recorded other bytes for track-a's file than the write
        // reads: it was changed outside since, and keeps its record.
        // track-b's record is the bytes the write reads: it records what
        // it places.
        let elsewhere = sha256_hex(b"bytes the app wrote earlier");
        open_db(&fix)
            .execute(
                "INSERT INTO library_track_management_state (local_track_id, written_sha256) \
                 VALUES ('track-a', ?1), ('track-b', ?2)",
                [&elsewhere, &bundle.items[1].fingerprint.sha256],
            )
            .unwrap();
        // A track whose file went before the write still holds track-a's
        // destination in the catalog.
        seed_track(&fix, "gone", "organized/a.flac", "missing", Some(1234.5));
        let (sealed, live) = seal(&bundle);
        let mut publisher = open_publisher(&fix);
        publisher.set_crash_point(Some(point));
        let err = publisher.publish(&sealed, &live, &docs).unwrap_err();
        assert!(
            matches!(err, PublishError::InjectedCrash(_)),
            "{point:?}: got {err:?}"
        );
        drop(publisher);

        let db = restart(&fix);

        let conn = open_db(&fix);
        let results = reconcile(&db, &fix.sandbox).unwrap();
        assert_eq!(results.len(), 1, "{point:?}: one bundle reconciled");

        for item in bundle.items.iter() {
            let dest = fix
                .sandbox
                .resolve(&item.dest_root, &item.dest_rel)
                .unwrap();
            // Full staged bytes, never half: the planned title is in
            // the tags and the file still decodes.
            assert_staged(&dest, &item.track_id);
            // The roll-forward records the bytes it placed as the app's,
            // except over a file changed since the app wrote it.
            let recorded: Option<String> = conn
                .query_row(
                    "SELECT written_sha256 FROM library_track_management_state \
                     WHERE local_track_id = ?1",
                    [&item.track_id],
                    |row| row.get(0),
                )
                .unwrap();
            if item.track_id == "track-a" {
                assert_eq!(
                    recorded.as_deref(),
                    Some(elsewhere.as_str()),
                    "{point:?}: the record is left"
                );
            } else {
                assert_eq!(
                    recorded,
                    Some(sha256_hex(&std::fs::read(&dest).unwrap())),
                    "{point:?}: the placed bytes are recorded"
                );
            }
        }
        // Committed, the missing row gave the path up and kept the rest;
        // compensated, it still holds it.
        let committed = track_row(&conn, "track-a").0 == "organized/a.flac";
        let held = if committed {
            format!("{}gone/gone/organized/a.flac", publish::HIDDEN_PREFIX)
        } else {
            "organized/a.flac".to_string()
        };
        let file_path: String = conn
            .query_row(
                "SELECT file_path FROM local_tracks WHERE id = 'gone'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(file_path.ends_with(&held), "{point:?}: {file_path}");
        assert_eq!(
            track_row(&conn, "gone"),
            (held, "missing".to_string(), Some(1234.5)),
            "{point:?}: the missing row"
        );
        let marks: i64 = conn
            .query_row("SELECT COUNT(*) FROM library_album_takeovers", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(marks, 0, "{point:?}: the app's own write marks nothing");
        assert!(
            hidden_leftovers(&fix.dir).is_empty(),
            "{point:?}: no staging debris remains"
        );
        let journals = JournalStore::new(&conn).bundle("bundle-1").unwrap();
        assert!(
            journals
                .iter()
                .all(|journal| journal.state == JournalState::Cleaned),
            "{point:?}: all journals cleaned"
        );
        assert_eq!(
            read_revision(&conn).unwrap(),
            start_revision() + 1,
            "{point:?}: catalog committed once"
        );
    }
}

#[test]
fn same_path_crash_keeps_original_until_commit() {
    let fix = fixture("crash-same-path");
    let (bundle, docs) = two_track_bundle(&fix, PlanKind::SamePath);
    let (sealed, live) = seal(&bundle);
    let mut publisher = open_publisher(&fix);
    publisher.set_crash_point(Some(CrashPoint::BeforeCatalogCommit));
    let err = publisher.publish(&sealed, &live, &docs).unwrap_err();
    assert!(matches!(err, PublishError::InjectedCrash(_)));
    drop(publisher);

    let db = restart(&fix);

    let conn = open_db(&fix);
    let results = reconcile(&db, &fix.sandbox).unwrap();
    assert_eq!(results.len(), 1);
    for item in bundle.items.iter() {
        let dest = fix
            .sandbox
            .resolve(&item.dest_root, &item.dest_rel)
            .unwrap();
        assert_staged(&dest, &item.track_id);
    }
    assert!(hidden_leftovers(&fix.dir).is_empty());
    let journals = JournalStore::new(&conn).bundle("bundle-1").unwrap();
    assert!(
        journals
            .iter()
            .all(|journal| journal.state == JournalState::Cleaned)
    );
}

/// A bundle whose library folder is not there at recovery (a share not
/// yet mounted) waits with its rows as they were, and finishes once the
/// folder is back. It is never handed to an admin for it.
#[test]
fn a_bundle_waits_for_its_missing_folder_and_then_finishes() {
    let fix = fixture("crash-folder-missing");
    let (bundle, docs) = two_track_bundle(&fix, PlanKind::SamePath);
    let (sealed, live) = seal(&bundle);
    let mut publisher = open_publisher(&fix);
    publisher.set_crash_point(Some(CrashPoint::BeforeCatalogCommit));
    let err = publisher.publish(&sealed, &live, &docs).unwrap_err();
    assert!(matches!(err, PublishError::InjectedCrash(_)));
    drop(publisher);
    let db = restart(&fix);
    let conn = open_db(&fix);
    let music = fix.dir.join("music");
    let away = fix.dir.join("unmounted");
    std::fs::rename(&music, &away).unwrap();

    let results = reconcile(&db, &fix.sandbox).unwrap();
    assert_eq!(results.len(), 1);
    assert!(
        matches!(results[0].action, publish::RecoveryAction::Deferred(_)),
        "{:?}",
        results[0].action
    );
    let journals = JournalStore::new(&conn).bundle("bundle-1").unwrap();
    assert!(
        journals.iter().all(|journal| !journal.state.is_terminal()),
        "{journals:?}"
    );

    std::fs::rename(&away, &music).unwrap();
    let results = reconcile(&db, &fix.sandbox).unwrap();
    assert_eq!(results.len(), 1);
    for item in bundle.items.iter() {
        let dest = fix
            .sandbox
            .resolve(&item.dest_root, &item.dest_rel)
            .unwrap();
        assert_staged(&dest, &item.track_id);
    }
    let journals = JournalStore::new(&conn).bundle("bundle-1").unwrap();
    assert!(
        journals
            .iter()
            .all(|journal| journal.state == JournalState::Cleaned),
        "{journals:?}"
    );
}

/// A file the writer refuses in the middle of an album unwinds the files
/// staged before it: nothing stays held, and recovery never finishes the
/// part that was staged.
#[test]
fn a_refused_file_unwinds_the_files_staged_before_it() {
    let fix = fixture("refused-second-file");
    let (mut bundle, docs) = two_track_bundle(&fix, PlanKind::SamePath);
    bundle.items[1].managed_updates.insert(
        TagTarget::Native("id3:TXXX:X".to_string()),
        vec!["x".to_string()],
    );
    let path_a = fix.sandbox.resolve("music", "staging/a.flac").unwrap();
    let original_a = std::fs::read(&path_a).unwrap();
    let (sealed, live) = seal(&bundle);
    let mut publisher = open_publisher(&fix);
    let err = publisher.publish(&sealed, &live, &docs).unwrap_err();
    assert!(matches!(err, PublishError::Capability(_)), "got {err:?}");
    drop(publisher);

    let db = restart(&fix);

    let conn = open_db(&fix);
    assert!(
        JournalStore::new(&conn)
            .active_bundles()
            .unwrap()
            .is_empty(),
        "no write is held"
    );
    assert!(
        hidden_leftovers(&fix.dir).is_empty(),
        "no staged copy is left"
    );
    assert_eq!(std::fs::read(&path_a).unwrap(), original_a);
    let results = reconcile(&db, &fix.sandbox).unwrap();
    assert!(
        results.is_empty(),
        "recovery has nothing to do: {results:?}"
    );
    assert_eq!(std::fs::read(&path_a).unwrap(), original_a);
    drop(conn);

    let mut alone = bundle.clone();
    alone.id = "bundle-2".to_string();
    alone.items.truncate(1);
    let (sealed, live) = seal(&alone);
    let mut publisher = open_publisher(&fix);
    publisher.publish(&sealed, &live, &docs).unwrap();
    assert_staged(&path_a, "track-a");
}

/// A crash after the first file of an album staged leaves a bundle that
/// looks complete on disk; recovery unwinds it instead of committing it.
#[test]
fn a_crash_after_the_first_file_staged_unwinds() {
    let fix = fixture("crash-first-file-staged");
    let (bundle, docs) = two_track_bundle(&fix, PlanKind::Move);
    let source_a = fix.sandbox.resolve("music", "staging/a.flac").unwrap();
    let original_a = std::fs::read(&source_a).unwrap();
    let row_a = track_row(&open_db(&fix), "track-a");
    let (sealed, live) = seal(&bundle);
    let mut publisher = open_publisher(&fix);
    publisher.set_crash_point(Some(CrashPoint::AfterFirstFileStaged));
    let err = publisher.publish(&sealed, &live, &docs).unwrap_err();
    assert!(matches!(err, PublishError::InjectedCrash(_)), "got {err:?}");
    drop(publisher);

    let db = restart(&fix);

    let conn = open_db(&fix);
    let results = reconcile(&db, &fix.sandbox).unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].action, publish::RecoveryAction::Compensated);
    assert_eq!(std::fs::read(&source_a).unwrap(), original_a);
    assert!(
        std::fs::symlink_metadata(fix.sandbox.resolve("music", "organized/a.flac").unwrap())
            .is_err()
    );
    assert_eq!(track_row(&conn, "track-a"), row_a);
    assert_eq!(
        std::fs::read(fix.sandbox.resolve("music", "staging/cover.jpg").unwrap()).unwrap(),
        b"cover-bytes"
    );
    assert!(
        std::fs::symlink_metadata(fix.sandbox.resolve("music", "organized/cover.jpg").unwrap())
            .is_err()
    );
    assert!(hidden_leftovers(&fix.dir).is_empty());
    assert!(
        JournalStore::new(&conn)
            .active_bundles()
            .unwrap()
            .is_empty()
    );
}

/// A crash before a file's before-state is captured unwinds its bundle:
/// a write is never finished without the snapshot its undo needs.
#[test]
fn a_crash_before_the_before_state_unwinds() {
    let fix = fixture("crash-before-state");
    let (mut bundle, docs) = two_track_bundle(&fix, PlanKind::SamePath);
    bundle.items.truncate(1);
    let path_a = fix.sandbox.resolve("music", "staging/a.flac").unwrap();
    let original_a = std::fs::read(&path_a).unwrap();
    let (sealed, live) = seal(&bundle);
    let mut publisher = open_publisher(&fix);
    publisher.set_crash_point(Some(CrashPoint::AfterTempWritten));
    let err = publisher.publish(&sealed, &live, &docs).unwrap_err();
    assert!(matches!(err, PublishError::InjectedCrash(_)), "got {err:?}");
    drop(publisher);

    let db = restart(&fix);

    let results = reconcile(&db, &fix.sandbox).unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].action, publish::RecoveryAction::Compensated);
    assert_eq!(std::fs::read(&path_a).unwrap(), original_a);
    assert!(hidden_leftovers(&fix.dir).is_empty());
}

/// Unwinding a file whose staged copy was never written leaves the file
/// it was to replace, even when that file holds the very bytes the write
/// would have placed.
#[test]
fn an_unwind_keeps_an_identical_file_it_never_replaced() {
    let fix = fixture("unwind-identical-replaced");
    let (mut bundle, docs) = two_track_bundle(&fix, PlanKind::Move);
    bundle.items.truncate(1);
    bundle.items[0].sidecars.clear();
    bundle.items[0].managed_updates = TagUpdates::new();
    let source = std::fs::read(fix.sandbox.resolve("music", "staging/a.flac").unwrap()).unwrap();
    let occupier = fix.sandbox.resolve("music", "organized/a.flac").unwrap();
    std::fs::create_dir_all(occupier.parent().unwrap()).unwrap();
    std::fs::write(&occupier, &source).unwrap();
    open_db(&fix)
        .execute(
            "UPDATE local_tracks SET relative_path = 'organized/a.flac' WHERE id = 'track-s'",
            [],
        )
        .unwrap();
    bundle.items[0].replaces = Some(publish::Replacement {
        sha256: sha256_hex(&source),
        track_id: Some("track-s".to_string()),
        recycle_to: fix.dir.join("bin/entry/a.flac").display().to_string(),
    });
    let (sealed, live) = seal(&bundle);
    let mut publisher = open_publisher(&fix);
    publisher.set_crash_point(Some(CrashPoint::AfterTempWritten));
    assert!(matches!(
        publisher.publish(&sealed, &live, &docs),
        Err(PublishError::InjectedCrash(_))
    ));
    drop(publisher);
    // The process stopped before its staged copy reached the disk.
    for temp in hidden_leftovers(&fix.dir) {
        std::fs::remove_file(temp).unwrap();
    }

    let db = restart(&fix);

    let results = reconcile(&db, &fix.sandbox).unwrap();
    assert_eq!(results[0].action, publish::RecoveryAction::Compensated);
    assert_eq!(std::fs::read(&occupier).unwrap(), source);
}

/// A publisher stays open for days. Each write is timed when it is made,
/// and one still unsettled 24 hours later is listed as being retried, not
/// before, whatever midnight it crossed. A committed write whose cleanup
/// is pending is finished and is not listed.
#[test]
fn a_write_counts_as_stuck_after_24_hours() {
    use droppedneedle_library::management::recovery::retrying_bundles;

    const HOUR: i64 = 3_600_000;
    let fix = fixture("stuck-after-24-hours");
    let now = ManualClock::at(DAY_50_MS);
    let mut publisher = Publisher::new(
        fix.sandbox.clone(),
        restart(&fix),
        FixedSpace(u64::MAX),
        jobs(&restart(&fix)),
        std::sync::Arc::new(now.clone()),
        blobs(&fix),
    );
    // Day 53, 23:00.
    let made = 53 * 86_400_000 + 23 * HOUR;
    now.set(made);
    let (bundle, docs) = two_track_bundle(&fix, PlanKind::Move);
    let (sealed, live) = seal(&bundle);
    publisher.set_crash_point(Some(CrashPoint::AfterStage));
    publisher.publish(&sealed, &live, &docs).unwrap_err();
    drop(publisher);

    let conn = open_db(&fix);
    let stuck = |at: i64| -> Vec<String> {
        retrying_bundles(&conn, at)
            .unwrap()
            .into_iter()
            .map(|bundle| bundle.bundle_id)
            .collect()
    };
    assert!(
        stuck(made + 23 * HOUR).is_empty(),
        "23 hours, past midnight: not yet"
    );
    assert_eq!(stuck(made + 24 * HOUR), vec!["bundle-1".to_string()]);
    conn.execute(
        "UPDATE library_publish_journal SET state = 'cleanup_pending'",
        [],
    )
    .unwrap();
    assert!(
        stuck(made + 24 * HOUR).is_empty(),
        "a committed write is finished"
    );
}

/// A set-aside original whose journal rows are gone is reported once it
/// is more than a day old, and stays where it is. A file a row names, or
/// one young enough to belong to a publish in flight, is not reported.
#[test]
fn leftover_pass_reports_old_unjournaled_hidden_files_and_keeps_them() {
    let fix = fixture("leftover-pass");
    let (bundle, docs) = two_track_bundle(&fix, PlanKind::SamePath);
    let (sealed, live) = seal(&bundle);
    let mut publisher = open_publisher(&fix);
    publisher.set_crash_point(Some(CrashPoint::BeforeCatalogCommit));
    publisher.publish(&sealed, &live, &docs).unwrap_err();
    drop(publisher);
    let hidden = hidden_leftovers(&fix.dir);
    assert!(!hidden.is_empty(), "the crash left set-aside originals");
    // Names other cleanups own are never reported.
    let music = fix.dir.join("music");
    let import = music.join(format!("{}import/task", publish::HIDDEN_PREFIX));
    std::fs::create_dir_all(&import).unwrap();
    std::fs::write(import.join("01.flac"), b"staged").unwrap();
    std::fs::create_dir_all(music.join(publish::place::PROBE_DIR)).unwrap();

    let conn = open_db(&fix);
    let db = restart(&fix);
    let roots = [Root::library("music", fix.dir.join("music"))];
    let now = std::time::SystemTime::now();
    let later = now + std::time::Duration::from_secs(2 * 86_400);
    let pass = |at| {
        publish::recovery::find_leftovers(&db, &roots, at, &|| false)
            .unwrap()
            .expect("not cancelled")
    };
    assert_eq!(pass(later).count, 0, "the journal names every hidden file");

    conn.execute("DELETE FROM library_publish_journal", [])
        .unwrap();
    assert_eq!(pass(now).count, 0, "a fresh hidden file is left alone");
    let report = pass(later);
    assert_eq!(report.count, hidden.len());
    let mut reported: Vec<PathBuf> = report
        .files
        .iter()
        .map(|file| roots[0].dir.join(&file.rel_path))
        .collect();
    reported.sort();
    let mut expected = hidden.clone();
    expected.sort();
    assert_eq!(reported, expected);
    assert!(
        hidden.iter().all(|path| path.exists()),
        "nothing is removed"
    );
}

#[test]
fn foreign_bytes_at_an_unplaced_dest_unwind_without_deleting() {
    let fix = fixture("foreign-dest");
    let (bundle, docs) = two_track_bundle(&fix, PlanKind::Move);
    let (sealed, live) = seal(&bundle);
    let mut publisher = open_publisher(&fix);
    publisher.set_crash_point(Some(CrashPoint::AfterStage));
    assert!(matches!(
        publisher.publish(&sealed, &live, &docs),
        Err(PublishError::InjectedCrash(_))
    ));
    drop(publisher);

    let occupier = fix.sandbox.resolve("music", "organized/a.flac").unwrap();
    std::fs::create_dir_all(occupier.parent().unwrap()).unwrap();
    std::fs::write(&occupier, b"external-file").unwrap();

    let db = restart(&fix);

    let conn = open_db(&fix);
    let results = reconcile(&db, &fix.sandbox).unwrap();
    assert_eq!(results.len(), 1);
    assert!(
        matches!(results[0].action, publish::RecoveryAction::Compensated),
        "got {:?}",
        results[0].action
    );
    assert_eq!(std::fs::read(&occupier).unwrap(), b"external-file");
    assert_eq!(
        std::fs::read(fix.sandbox.resolve("music", "staging/a.flac").unwrap()).unwrap(),
        std::fs::read(fixtures_dir().join("management_full.flac")).unwrap()
    );
    assert_eq!(read_revision(&conn).unwrap(), start_revision());
}

#[test]
fn occupied_destination_is_never_overwritten() {
    let fix = fixture("occupied");
    let (bundle, docs) = two_track_bundle(&fix, PlanKind::Move);
    let occupier = fix.sandbox.resolve("music", "organized/a.flac").unwrap();
    std::fs::create_dir_all(occupier.parent().unwrap()).unwrap();
    std::fs::write(&occupier, b"existing-library-file").unwrap();
    let (sealed, live) = seal(&bundle);
    let mut publisher = open_publisher(&fix);
    let err = publisher.publish(&sealed, &live, &docs).unwrap_err();
    assert!(matches!(err, PublishError::Collision(_)), "got {err:?}");
    assert_eq!(std::fs::read(&occupier).unwrap(), b"existing-library-file");
    assert!(hidden_leftovers(&fix.dir).is_empty());
}

/// A row the catalog still lists as indexed at the destination, whose
/// file went before any scan saw it, is not given up: the commit fails
/// and compensates, and nothing moves.
#[test]
fn an_indexed_row_on_the_destination_fails_the_commit() {
    let fix = fixture("indexed-occupant");
    let (bundle, docs) = two_track_bundle(&fix, PlanKind::Move);
    seed_track(&fix, "stale", "organized/a.flac", "indexed", None);
    let (sealed, live) = seal(&bundle);
    let mut publisher = open_publisher(&fix);
    let err = publisher.publish(&sealed, &live, &docs).unwrap_err();
    assert!(matches!(err, PublishError::Catalog(_)), "got {err:?}");
    drop(publisher);

    let conn = open_db(&fix);
    assert_eq!(
        track_row(&conn, "stale"),
        ("organized/a.flac".to_string(), "indexed".to_string(), None)
    );
    for item in bundle.items.iter() {
        assert_eq!(track_row(&conn, &item.track_id).0, item.source_rel);
        let source = fix.sandbox.resolve("music", &item.source_rel).unwrap();
        assert_eq!(
            sha256_hex(&std::fs::read(&source).unwrap()),
            item.fingerprint.sha256
        );
        let dest = fix.sandbox.resolve("music", &item.dest_rel).unwrap();
        assert!(dest.symlink_metadata().is_err(), "nothing placed");
    }
    assert_eq!(read_revision(&conn).unwrap(), start_revision());
    assert!(hidden_leftovers(&fix.dir).is_empty());
}

/// A file another program creates at the destination after the publisher's
/// last check is never replaced, whether the file is placed by the
/// no-replace rename or by link-then-unlink (the step an NFS library
/// takes). The bundle reports a collision and nothing of the user's is
/// lost.
#[test]
fn late_occupier_survives_placement() {
    let original = std::fs::read(fixtures_dir().join("management_full.flac")).unwrap();
    for (kind, skip_rename) in [
        (PlanKind::Move, false),
        (PlanKind::Move, true),
        (PlanKind::SamePath, false),
        (PlanKind::SamePath, true),
    ] {
        let case = format!("{kind:?} skip_rename={skip_rename}");
        let fix = fixture("late-occupier");
        let (bundle, docs) = two_track_bundle(&fix, kind);
        let (sealed, live) = seal(&bundle);
        let target = fix
            .sandbox
            .resolve("music", &bundle.items[0].dest_rel)
            .unwrap();
        let mut publisher = open_publisher(&fix);
        publisher.set_place_seams(Seams {
            skip_rename,
            ..Seams::default()
        });
        let planted = target.clone();
        publisher.set_before_place(Some(Box::new(move |dest| {
            if dest == planted {
                std::fs::write(dest, b"arrived-late").unwrap();
            }
        })));
        let err = publisher.publish(&sealed, &live, &docs).unwrap_err();
        assert!(
            matches!(err, PublishError::Collision(_)),
            "{case}: got {err:?}"
        );
        assert_eq!(std::fs::read(&target).unwrap(), b"arrived-late", "{case}");

        let journals = JournalStore::new(&open_db(&fix))
            .bundle("bundle-1")
            .unwrap();
        match kind {
            PlanKind::Move => {
                for rel in ["staging/a.flac", "staging/b.flac"] {
                    let source = fix.sandbox.resolve("music", rel).unwrap();
                    assert_eq!(std::fs::read(source).unwrap(), original, "{case}");
                }
                assert!(hidden_leftovers(&fix.dir).is_empty(), "{case}");
                assert!(
                    journals
                        .iter()
                        .all(|journal| journal.state == JournalState::Compensated),
                    "{case}"
                );
            }
            PlanKind::SamePath => {
                // The original cannot go home: it waits under its backup
                // name, untouched.
                let backup = journals[0].backup.as_deref().expect("backup journaled");
                assert_eq!(std::fs::read(backup).unwrap(), original, "{case}");
                assert_eq!(journals[0].state, JournalState::NeedsAttention, "{case}");
                let other = fix.sandbox.resolve("music", "staging/b.flac").unwrap();
                assert_eq!(std::fs::read(other).unwrap(), original, "{case}");
            }
        }
        assert_eq!(
            read_revision(&open_db(&fix)).unwrap(),
            start_revision(),
            "{case}"
        );
    }
}

/// A placement that stopped between the link and the unlink leaves the
/// staged file under two names. Recovery, taking whichever step the disk
/// offers, sees one file, drops the hidden name and finishes the bundle.
#[test]
fn interrupted_link_is_finished_by_recovery() {
    use std::os::unix::fs::MetadataExt as _;
    for kind in [PlanKind::Move, PlanKind::SamePath] {
        let fix = fixture("link-interrupted");
        let (bundle, docs) = two_track_bundle(&fix, kind);
        let (sealed, live) = seal(&bundle);
        let mut publisher = open_publisher(&fix);
        publisher.set_place_seams(Seams {
            skip_rename: true,
            stop_after_link: true,
            ..Seams::default()
        });
        let err = publisher.publish(&sealed, &live, &docs).unwrap_err();
        assert!(
            matches!(err, PublishError::InjectedCrash(_)),
            "{kind:?}: got {err:?}"
        );
        drop(publisher);

        let db = restart(&fix);

        let conn = open_db(&fix);
        let first = JournalStore::new(&conn)
            .bundle("bundle-1")
            .unwrap()
            .remove(0);
        assert_eq!(first.state, JournalState::Staged);
        let dest = fix
            .sandbox
            .resolve(&first.dest_root, &first.dest_rel)
            .unwrap();
        assert_eq!(
            std::fs::metadata(&first.staged).unwrap().ino(),
            std::fs::metadata(&dest).unwrap().ino(),
            "{kind:?}: linked, not yet unlinked"
        );

        let results = reconcile(&db, &fix.sandbox).unwrap();
        assert_eq!(
            results[0].action,
            publish::RecoveryAction::ResumedCommitted,
            "{kind:?}"
        );
        for item in bundle.items.iter() {
            let dest = fix
                .sandbox
                .resolve(&item.dest_root, &item.dest_rel)
                .unwrap();
            assert_staged(&dest, &item.track_id);
            assert_eq!(std::fs::metadata(&dest).unwrap().nlink(), 1, "{kind:?}");
        }
        assert!(hidden_leftovers(&fix.dir).is_empty(), "{kind:?}");
        let journals = JournalStore::new(&conn).bundle("bundle-1").unwrap();
        assert!(
            journals
                .iter()
                .all(|journal| journal.state == JournalState::Cleaned),
            "{kind:?}"
        );
        assert_eq!(read_revision(&conn).unwrap(), start_revision() + 1);
    }
}

/// After the commit, recovery restores a missing output from a staged
/// copy that is still there, but only onto a free name. A destination
/// holding anything else is the user's: both files stay and the bundle
/// waits for an administrator.
#[test]
fn committed_output_is_restored_only_onto_a_free_name() {
    for occupied in [true, false] {
        let fix = fixture("committed-restore");
        let (bundle, docs) = two_track_bundle(&fix, PlanKind::Move);
        let (sealed, live) = seal(&bundle);
        let mut publisher = open_publisher(&fix);
        publisher.set_crash_point(Some(CrashPoint::AfterCatalogCommit));
        assert!(matches!(
            publisher.publish(&sealed, &live, &docs),
            Err(PublishError::InjectedCrash(_))
        ));
        drop(publisher);

        let db = restart(&fix);

        let conn = open_db(&fix);
        let first = JournalStore::new(&conn)
            .bundle("bundle-1")
            .unwrap()
            .remove(0);
        assert_eq!(first.state, JournalState::Committed);
        let dest = fix
            .sandbox
            .resolve(&first.dest_root, &first.dest_rel)
            .unwrap();
        let staged_bytes = std::fs::read(&dest).unwrap();
        std::fs::write(&first.staged, &staged_bytes).unwrap();
        std::fs::remove_file(&dest).unwrap();
        if occupied {
            std::fs::write(&dest, b"users-newer-file").unwrap();
        }

        let results = reconcile(&db, &fix.sandbox).unwrap();
        if occupied {
            assert!(
                matches!(
                    results[0].action,
                    publish::RecoveryAction::NeedsAttention(_)
                ),
                "got {:?}",
                results[0].action
            );
            assert_eq!(std::fs::read(&dest).unwrap(), b"users-newer-file");
            assert_eq!(std::fs::read(&first.staged).unwrap(), staged_bytes);
        } else {
            assert_eq!(results[0].action, publish::RecoveryAction::CleanupFinished);
            assert_eq!(std::fs::read(&dest).unwrap(), staged_bytes);
            assert!(hidden_leftovers(&fix.dir).is_empty());
        }
    }
}

/// An unwind never calls a row compensated while its set-aside original
/// is still under the hidden backup name: a backup whose bytes changed
/// stays where it is, with the published file, for an administrator.
#[test]
fn unwind_keeps_a_backup_it_cannot_restore() {
    let original = std::fs::read(fixtures_dir().join("management_full.flac")).unwrap();
    let fix = fixture("unwind-backup-changed");
    let (bundle, docs) = two_track_bundle(&fix, PlanKind::SamePath);
    let (sealed, live) = seal(&bundle);
    let first_dest = fix.sandbox.resolve("music", "staging/a.flac").unwrap();
    let second_dest = fix.sandbox.resolve("music", "staging/b.flac").unwrap();
    let mut publisher = open_publisher(&fix);
    let late = second_dest.clone();
    let first_backup = fix
        .sandbox
        .backup_path_for(&first_dest, "bundle-1-000-audio")
        .unwrap();
    publisher.set_before_place(Some(Box::new(move |dest| {
        if dest != late {
            return;
        }
        // The first file is published by now. Its backup is tampered with,
        // then a late occupier fails the second file and so the bundle.
        std::fs::write(&first_backup, b"tampered").unwrap();
        std::fs::write(dest, b"arrived-late").unwrap();
    })));
    let err = publisher.publish(&sealed, &live, &docs).unwrap_err();
    assert!(matches!(err, PublishError::Collision(_)), "got {err:?}");

    let journals = JournalStore::new(&open_db(&fix))
        .bundle("bundle-1")
        .unwrap();
    let first_backup = journals[0].backup.as_deref().expect("backup journaled");
    assert_eq!(journals[0].state, JournalState::NeedsAttention);
    assert_eq!(std::fs::read(first_backup).unwrap(), b"tampered");
    assert_staged(&first_dest, "track-a");
    let second_backup = journals[1].backup.as_deref().expect("backup journaled");
    assert_eq!(journals[1].state, JournalState::NeedsAttention);
    assert_eq!(std::fs::read(second_backup).unwrap(), original);
    assert_eq!(std::fs::read(&second_dest).unwrap(), b"arrived-late");
}

/// The two unwinds that do finish. A backup path that was journaled but
/// never used leaves nothing to restore. A put-back that linked the
/// original home and stopped before the unlink is finished by removing
/// the hidden name. Both end compensated with every original in place.
#[test]
fn unwind_finishes_when_no_backup_is_left() {
    use std::os::unix::fs::MetadataExt as _;
    let original = std::fs::read(fixtures_dir().join("management_full.flac")).unwrap();
    for put_back_interrupted in [false, true] {
        let case = format!("put_back_interrupted={put_back_interrupted}");
        let fix = fixture("unwind-finishes");
        let (bundle, docs) = two_track_bundle(&fix, PlanKind::SamePath);
        let (sealed, live) = seal(&bundle);
        let mut publisher = open_publisher(&fix);
        publisher.set_crash_point(Some(if put_back_interrupted {
            CrashPoint::BeforeCatalogCommit
        } else {
            CrashPoint::AfterBackupJournaled
        }));
        assert!(matches!(
            publisher.publish(&sealed, &live, &docs),
            Err(PublishError::InjectedCrash(_))
        ));
        drop(publisher);

        let db = restart(&fix);

        let conn = open_db(&fix);
        let first = JournalStore::new(&conn)
            .bundle("bundle-1")
            .unwrap()
            .remove(0);
        let backup = first.backup.as_deref().expect("backup journaled");
        let dest = fix
            .sandbox
            .resolve(&first.dest_root, &first.dest_rel)
            .unwrap();
        if put_back_interrupted {
            // An unwind removed the published copy and linked the original
            // home, then stopped.
            std::fs::remove_file(&dest).unwrap();
            std::fs::hard_link(backup, &dest).unwrap();
        } else {
            // With the staged copy gone the bundle cannot resume.
            assert!(std::fs::symlink_metadata(backup).is_err());
            std::fs::remove_file(&first.staged).unwrap();
        }

        let results = reconcile(&db, &fix.sandbox).unwrap();
        assert_eq!(
            results[0].action,
            publish::RecoveryAction::Compensated,
            "{case}"
        );
        for rel in ["staging/a.flac", "staging/b.flac"] {
            let path = fix.sandbox.resolve("music", rel).unwrap();
            assert_eq!(std::fs::read(&path).unwrap(), original, "{case}");
            assert_eq!(std::fs::metadata(&path).unwrap().nlink(), 1, "{case}");
        }
        assert!(hidden_leftovers(&fix.dir).is_empty(), "{case}");
        let journals = JournalStore::new(&conn).bundle("bundle-1").unwrap();
        assert!(
            journals
                .iter()
                .all(|journal| journal.state == JournalState::Compensated),
            "{case}"
        );
        assert_eq!(read_revision(&conn).unwrap(), start_revision());
    }
}

/// An unwind that stopped between two rows is finished as an unwind. The
/// row it had not reached still looks ready to commit, but its sibling is
/// already compensated, so the bundle can never commit: both originals go
/// home and the bundle is settled for good.
#[test]
fn recovery_finishes_an_unwind_that_stopped_between_rows() {
    let original = std::fs::read(fixtures_dir().join("management_full.flac")).unwrap();
    let fix = fixture("unwind-between-rows");
    let (bundle, docs) = two_track_bundle(&fix, PlanKind::SamePath);
    let (sealed, live) = seal(&bundle);
    let second_dest = fix.sandbox.resolve("music", "staging/b.flac").unwrap();
    let mut publisher = open_publisher(&fix);
    // A scan rewrites track-b's row while the files are placed, so the
    // commit fails with both files published and both originals set aside.
    let db_path = fix.db_path.clone();
    publisher.set_before_place(Some(Box::new(move |dest| {
        if dest == second_dest {
            let conn = droppedneedle_kernel::db::open_connection(&db_path).unwrap();
            conn.execute(
                "UPDATE local_tracks SET stat_revision = '1:1' WHERE id = 'track-b'",
                [],
            )
            .unwrap();
        }
    })));
    publisher.set_crash_point(Some(CrashPoint::AfterFirstUnwind));
    let err = publisher.publish(&sealed, &live, &docs).unwrap_err();
    assert!(matches!(err, PublishError::InjectedCrash(_)), "got {err:?}");
    let journals = JournalStore::new(&open_db(&fix))
        .bundle("bundle-1")
        .unwrap();
    assert_eq!(journals[0].state, JournalState::Compensated);
    assert_eq!(journals[1].state, JournalState::Published);
    drop(publisher);

    let db = restart(&fix);

    let conn = open_db(&fix);
    let results = reconcile(&db, &fix.sandbox).unwrap();
    assert_eq!(results[0].action, publish::RecoveryAction::Compensated);
    for rel in ["staging/a.flac", "staging/b.flac"] {
        let path = fix.sandbox.resolve("music", rel).unwrap();
        assert!(std::fs::read(&path).unwrap() == original, "{rel} is home");
    }
    assert!(hidden_leftovers(&fix.dir).is_empty());
    let journals = JournalStore::new(&conn).bundle("bundle-1").unwrap();
    assert!(
        journals
            .iter()
            .all(|journal| journal.state == JournalState::Compensated)
    );
    let again = reconcile(&db, &fix.sandbox).unwrap();
    assert!(again.is_empty(), "a second pass finds nothing to do");
}

/// An unwind that finds no safe way to put an original back moves nothing
/// more. Recovery keeps to that when the unwind stopped before every row
/// was marked: the next file keeps its placed copy and its backup, and
/// needs attention with the rest.
#[test]
fn recovery_moves_nothing_after_a_put_back_with_no_safe_way() {
    let original = std::fs::read(fixtures_dir().join("management_full.flac")).unwrap();
    let fix = fixture("unwind-no-way-back");
    let (bundle, docs) = two_track_bundle(&fix, PlanKind::SamePath);
    let (sealed, live) = seal(&bundle);
    let mut publisher = open_publisher(&fix);
    publisher.set_crash_point(Some(CrashPoint::BeforeCatalogCommit));
    let err = publisher.publish(&sealed, &live, &docs).unwrap_err();
    assert!(matches!(err, PublishError::InjectedCrash(_)), "got {err:?}");
    drop(publisher);

    let db = restart(&fix);

    let conn = open_db(&fix);
    let store = JournalStore::new(&conn);
    let first = store.bundle("bundle-1").unwrap().remove(0);
    // As that unwind leaves its row: the placed copy is off the name, the
    // original is still under its backup name.
    let first_dest = fix.sandbox.resolve("music", "staging/a.flac").unwrap();
    std::fs::remove_file(&first_dest).unwrap();
    assert!(
        db.write_durable(DurableLane::Background, "test.flag", |tx| {
            publish::JournalWriter::new(tx).transition(
                &first.id,
                first.state,
                JournalState::NeedsAttention,
            )
        })
        .unwrap()
    );

    let results = reconcile(&db, &fix.sandbox).unwrap();
    assert!(
        matches!(
            results[0].action,
            publish::RecoveryAction::NeedsAttention(_)
        ),
        "got {:?}",
        results[0].action
    );
    let journals = JournalStore::new(&conn).bundle("bundle-1").unwrap();
    for journal in journals.iter() {
        assert_eq!(journal.state, JournalState::NeedsAttention);
        let backup = journal.backup.as_deref().expect("backup journaled");
        assert!(std::fs::read(backup).unwrap() == original);
    }
    assert!(first_dest.symlink_metadata().is_err());
    let second_dest = fix.sandbox.resolve("music", "staging/b.flac").unwrap();
    assert_staged(&second_dest, "track-b");
}

/// A resume that cannot set an original aside unwinds the bundle, as a
/// publish does. The file resumed just before it gets its original back
/// under its own name. The refused file never moved, and the foreign file
/// under its backup name is kept, so that row alone needs attention.
#[test]
fn a_refused_set_aside_during_resume_unwinds_the_bundle() {
    let original = std::fs::read(fixtures_dir().join("management_full.flac")).unwrap();
    let fix = fixture("resume-aside-refused");
    let (bundle, docs) = two_track_bundle(&fix, PlanKind::SamePath);
    let (sealed, live) = seal(&bundle);
    let mut publisher = open_publisher(&fix);
    publisher.set_crash_point(Some(CrashPoint::AfterStage));
    let err = publisher.publish(&sealed, &live, &docs).unwrap_err();
    assert!(matches!(err, PublishError::InjectedCrash(_)), "got {err:?}");
    drop(publisher);

    let db = restart(&fix);

    let conn = open_db(&fix);
    let second = JournalStore::new(&conn)
        .bundle("bundle-1")
        .unwrap()
        .remove(1);
    let second_dest = fix.sandbox.resolve("music", "staging/b.flac").unwrap();
    let taken = fix
        .sandbox
        .backup_path_for(&second_dest, &second.id)
        .unwrap();
    std::fs::write(&taken, b"not-ours").unwrap();

    let results = reconcile(&db, &fix.sandbox).unwrap();
    assert!(
        matches!(
            &results[0].action,
            publish::RecoveryAction::NeedsAttention(reason)
                if reason.contains("the hidden name is taken")
        ),
        "got {:?}",
        results[0].action
    );
    for rel in ["staging/a.flac", "staging/b.flac"] {
        let path = fix.sandbox.resolve("music", rel).unwrap();
        assert!(std::fs::read(&path).unwrap() == original, "{rel} is home");
    }
    assert_eq!(std::fs::read(&taken).unwrap(), b"not-ours");
    let journals = JournalStore::new(&conn).bundle("bundle-1").unwrap();
    assert_eq!(journals[0].state, JournalState::Compensated);
    let first_backup = journals[0].backup.as_deref().expect("backup journaled");
    assert!(std::fs::symlink_metadata(first_backup).is_err());
    assert_eq!(journals[1].state, JournalState::NeedsAttention);
}

/// A resume that finds no safe way to place a file unwinds the bundle, as
/// a publish does: the file placed before the stop comes off its new name
/// and every file is where it was, under its own name.
#[test]
fn a_refused_placement_during_resume_unwinds_the_bundle() {
    let original = std::fs::read(fixtures_dir().join("management_full.flac")).unwrap();
    let fix = fixture("resume-place-refused");
    let (bundle, docs) = two_track_bundle(&fix, PlanKind::Move);
    let (sealed, live) = seal(&bundle);
    let mut publisher = open_publisher(&fix);
    publisher.set_crash_point(Some(CrashPoint::AfterFirstRename));
    let err = publisher.publish(&sealed, &live, &docs).unwrap_err();
    assert!(matches!(err, PublishError::InjectedCrash(_)), "got {err:?}");
    drop(publisher);
    let placed = fix.sandbox.resolve("music", "organized/a.flac").unwrap();
    assert_staged(&placed, "track-a");

    let db = restart(&fix);

    let conn = open_db(&fix);
    let no_safe_step = Seams {
        skip_rename: true,
        link_errno: Some(libc::EPERM),
        ..Seams::default()
    };
    let results = reconcile_with_seams(&db, &fix.sandbox, no_safe_step).unwrap();
    assert_eq!(results[0].action, publish::RecoveryAction::Compensated);
    assert!(placed.symlink_metadata().is_err(), "the placed copy is off");
    for rel in ["staging/a.flac", "staging/b.flac"] {
        let path = fix.sandbox.resolve("music", rel).unwrap();
        assert!(std::fs::read(&path).unwrap() == original, "{rel} is home");
    }
    let cover = fix.sandbox.resolve("music", "staging/cover.jpg").unwrap();
    assert_eq!(std::fs::read(cover).unwrap(), b"cover-bytes");
    assert!(hidden_leftovers(&fix.dir).is_empty());
    let journals = JournalStore::new(&conn).bundle("bundle-1").unwrap();
    assert!(
        journals
            .iter()
            .all(|journal| journal.state == JournalState::Compensated)
    );
}

/// A move whose destination already holds the user's own copy of the
/// source is not resumable and not ambiguous. The bundle unwinds once:
/// the copy is never touched, the source stays, and nothing is left to
/// retry.
#[test]
fn a_users_copy_at_the_destination_unwinds_the_resume() {
    use std::os::unix::fs::MetadataExt as _;
    let original = std::fs::read(fixtures_dir().join("management_full.flac")).unwrap();
    let fix = fixture("resume-users-copy");
    let (bundle, docs) = two_track_bundle(&fix, PlanKind::Move);
    let (sealed, live) = seal(&bundle);
    let mut publisher = open_publisher(&fix);
    publisher.set_crash_point(Some(CrashPoint::AfterStage));
    let err = publisher.publish(&sealed, &live, &docs).unwrap_err();
    assert!(matches!(err, PublishError::InjectedCrash(_)), "got {err:?}");
    drop(publisher);

    let source = fix.sandbox.resolve("music", "staging/b.flac").unwrap();
    let copy = fix.sandbox.resolve("music", "organized/b.flac").unwrap();
    std::fs::create_dir_all(copy.parent().unwrap()).unwrap();
    std::fs::copy(&source, &copy).unwrap();
    let inode = std::fs::metadata(&copy).unwrap().ino();

    let db = restart(&fix);

    let conn = open_db(&fix);
    let results = reconcile(&db, &fix.sandbox).unwrap();
    assert_eq!(results[0].action, publish::RecoveryAction::Compensated);
    assert!(std::fs::read(&copy).unwrap() == original);
    assert_eq!(std::fs::metadata(&copy).unwrap().ino(), inode);
    for rel in ["staging/a.flac", "staging/b.flac"] {
        let path = fix.sandbox.resolve("music", rel).unwrap();
        assert!(std::fs::read(&path).unwrap() == original, "{rel} is home");
    }
    let other = fix.sandbox.resolve("music", "organized/a.flac").unwrap();
    assert!(other.symlink_metadata().is_err());
    assert!(hidden_leftovers(&fix.dir).is_empty());
    let journals = JournalStore::new(&conn).bundle("bundle-1").unwrap();
    assert!(
        journals
            .iter()
            .all(|journal| journal.state == JournalState::Compensated)
    );
    let again = reconcile(&db, &fix.sandbox).unwrap();
    assert!(again.is_empty(), "a second pass finds nothing to do");
}

/// A same-path row with no tag change stages the file's own bytes. When
/// an earlier row fails the bundle before that row is placed, the unwind
/// must not take the user's untouched file for the staged copy.
#[test]
fn unwind_keeps_an_original_that_was_never_set_aside() {
    use std::os::unix::fs::MetadataExt as _;
    let original = std::fs::read(fixtures_dir().join("management_full.flac")).unwrap();
    let fix = fixture("unwind-unchanged");
    let (mut bundle, docs) = two_track_bundle(&fix, PlanKind::SamePath);
    for item in bundle.items.iter_mut() {
        item.managed_updates = TagUpdates::new();
    }
    let (sealed, live) = seal(&bundle);
    let first = fix.sandbox.resolve("music", "staging/a.flac").unwrap();
    let second = fix.sandbox.resolve("music", "staging/b.flac").unwrap();
    let inode = std::fs::metadata(&second).unwrap().ino();
    let mut publisher = open_publisher(&fix);
    let late = first.clone();
    publisher.set_before_place(Some(Box::new(move |dest| {
        if dest == late {
            std::fs::write(dest, b"arrived-late").unwrap();
        }
    })));
    let err = publisher.publish(&sealed, &live, &docs).unwrap_err();
    assert!(matches!(err, PublishError::Collision(_)), "got {err:?}");

    assert_eq!(std::fs::read(&second).unwrap(), original);
    assert_eq!(std::fs::metadata(&second).unwrap().ino(), inode);
    let journals = JournalStore::new(&open_db(&fix))
        .bundle("bundle-1")
        .unwrap();
    assert_eq!(journals[1].state, JournalState::Compensated);
    assert!(std::fs::symlink_metadata(&journals[1].staged).is_err());
}

/// A write in place with no tag change stages the original's own bytes.
/// A crash before it is placed leaves the original and its staged copy
/// side by side, which is not a second copy of ours: recovery finishes the
/// write, as the publish would have.
#[test]
fn a_same_path_write_with_unchanged_tags_resumes_after_a_crash() {
    let original = std::fs::read(fixtures_dir().join("management_full.flac")).unwrap();
    let fix = fixture("resume-unchanged");
    let (mut bundle, docs) = two_track_bundle(&fix, PlanKind::SamePath);
    for item in bundle.items.iter_mut() {
        item.managed_updates = TagUpdates::new();
    }
    let (sealed, live) = seal(&bundle);
    let mut publisher = open_publisher(&fix);
    publisher.set_crash_point(Some(CrashPoint::AfterStage));
    let err = publisher.publish(&sealed, &live, &docs).unwrap_err();
    assert!(matches!(err, PublishError::InjectedCrash(_)), "got {err:?}");
    drop(publisher);

    let db = restart(&fix);
    let results = reconcile(&db, &fix.sandbox).unwrap();
    assert_eq!(results[0].action, publish::RecoveryAction::ResumedCommitted);
    for rel in ["staging/a.flac", "staging/b.flac"] {
        let path = fix.sandbox.resolve("music", rel).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), original, "{rel}");
    }
    assert!(hidden_leftovers(&fix.dir).is_empty());
    let journals = JournalStore::new(&open_db(&fix))
        .bundle("bundle-1")
        .unwrap();
    assert!(
        journals
            .iter()
            .all(|journal| journal.state == JournalState::Cleaned)
    );
}

/// The placement probe writes only under a root a bundle targets, and
/// leaves nothing there.
#[test]
fn only_a_targeted_root_is_probed() {
    let fix = fixture("probe-lazy");
    let incoming = fix.dir.join("incoming");
    let touched = || std::fs::metadata(&incoming).unwrap().modified().unwrap();
    let before = touched();
    let (bundle, docs) = two_track_bundle(&fix, PlanKind::Move);
    let (sealed, live) = seal(&bundle);
    let mut publisher = open_publisher(&fix);
    assert_eq!(
        fix.sandbox.placement_mode("music"),
        PlacementMode::Unknown,
        "nothing is probed before a bundle"
    );
    publisher.publish(&sealed, &live, &docs).unwrap();

    assert_eq!(fix.sandbox.placement_mode("music"), PlacementMode::Rename);
    assert!(hidden_leftovers(&fix.dir.join("music")).is_empty());
    assert_eq!(
        fix.sandbox.placement_mode("incoming"),
        PlacementMode::Unknown
    );
    assert_eq!(touched(), before, "the other root was never written to");
    assert_eq!(std::fs::read_dir(&incoming).unwrap().count(), 0);
}

/// A file neither safe step can place is left alone on a root whose
/// probe found a safe step: check-then-rename is not a fallback there.
/// Only a root probed as having neither step places that way.
#[test]
fn check_then_rename_needs_a_check_root() {
    let original = std::fs::read(fixtures_dir().join("management_full.flac")).unwrap();
    let no_safe_step = Seams {
        skip_rename: true,
        link_errno: Some(libc::EPERM),
        ..Seams::default()
    };
    for mode in [PlacementMode::Rename, PlacementMode::Check] {
        let fix = fixture("no-safe-step");
        fix.sandbox.set_placement_mode("music", mode);
        let (bundle, docs) = two_track_bundle(&fix, PlanKind::Move);
        let (sealed, live) = seal(&bundle);
        let mut publisher = open_publisher(&fix);
        publisher.set_place_seams(no_safe_step);
        let outcome = publisher.publish(&sealed, &live, &docs);
        if mode == PlacementMode::Check {
            assert_eq!(outcome.unwrap(), PublishOutcome::Committed);
            let dest = fix.sandbox.resolve("music", "organized/a.flac").unwrap();
            assert_staged(&dest, "track-a");
            continue;
        }
        let err = outcome.unwrap_err();
        assert!(
            matches!(&err, PublishError::NoSafePlacement(text) if text.message == publish::place::NO_SAFE_METHOD),
            "got {err:?}"
        );
        for rel in ["staging/a.flac", "staging/b.flac"] {
            let source = fix.sandbox.resolve("music", rel).unwrap();
            assert_eq!(std::fs::read(source).unwrap(), original);
        }
        let dest = fix.sandbox.resolve("music", "organized/a.flac").unwrap();
        assert!(dest.symlink_metadata().is_err(), "nothing was placed");
        assert!(hidden_leftovers(&fix.dir).is_empty());
    }
}

/// A tagger can save a file after the publisher last read it and before
/// the file is set aside for a write in place. The newer file is never
/// lost or left under a hidden name: it goes back under its own name and
/// the bundle fails with a collision. The same holds on a disk without a
/// no-replace rename, where the file is set aside by a plain rename.
#[test]
fn a_file_replaced_before_its_set_aside_goes_back() {
    let original = std::fs::read(fixtures_dir().join("management_full.flac")).unwrap();
    for skip_rename in [false, true] {
        let case = format!("skip_rename={skip_rename}");
        let fix = fixture("aside-replaced");
        let (bundle, docs) = two_track_bundle(&fix, PlanKind::SamePath);
        let (sealed, live) = seal(&bundle);
        let target = fix.sandbox.resolve("music", "staging/a.flac").unwrap();
        let mut publisher = open_publisher(&fix);
        publisher.set_place_seams(Seams {
            skip_rename,
            ..Seams::default()
        });
        let saved = target.clone();
        publisher.set_before_set_aside(Some(Box::new(move |dest| {
            if dest == saved {
                // As a tagger saves: a new file renamed over the old one.
                let temp = dest.with_extension("tagger");
                std::fs::write(&temp, b"saved-by-a-tagger").unwrap();
                std::fs::rename(&temp, dest).unwrap();
            }
        })));
        let err = publisher.publish(&sealed, &live, &docs).unwrap_err();
        assert!(
            matches!(err, PublishError::Collision(_)),
            "{case}: got {err:?}"
        );

        assert_eq!(
            std::fs::read(&target).unwrap(),
            b"saved-by-a-tagger",
            "{case}"
        );
        let other = fix.sandbox.resolve("music", "staging/b.flac").unwrap();
        assert!(std::fs::read(other).unwrap() == original, "{case}");
        assert!(hidden_leftovers(&fix.dir).is_empty(), "{case}");
        let journals = JournalStore::new(&open_db(&fix))
            .bundle("bundle-1")
            .unwrap();
        assert!(
            journals
                .iter()
                .all(|journal| journal.state == JournalState::Compensated),
            "{case}"
        );
        assert_eq!(
            read_revision(&open_db(&fix)).unwrap(),
            start_revision(),
            "{case}"
        );
    }
}

/// A file written in place is set aside first, so it has to be able to
/// come back. Where neither safe step works for it, it is refused before
/// anything moves: the original keeps its own name, nothing hidden is
/// left, and the error names the file.
#[test]
fn a_file_that_could_not_come_back_is_never_set_aside() {
    use std::os::unix::fs::MetadataExt as _;
    let original = std::fs::read(fixtures_dir().join("management_full.flac")).unwrap();
    let no_safe_step = Seams {
        skip_rename: true,
        link_errno: Some(libc::EPERM),
        ..Seams::default()
    };
    for mode in [PlacementMode::Rename, PlacementMode::Link] {
        let fix = fixture("aside-refused");
        fix.sandbox.set_placement_mode("music", mode);
        let (bundle, docs) = two_track_bundle(&fix, PlanKind::SamePath);
        let (sealed, live) = seal(&bundle);
        let first = fix.sandbox.resolve("music", "staging/a.flac").unwrap();
        let inode = std::fs::metadata(&first).unwrap().ino();
        let mut publisher = open_publisher(&fix);
        publisher.set_place_seams(no_safe_step);
        let err = publisher.publish(&sealed, &live, &docs).unwrap_err();

        for rel in ["staging/a.flac", "staging/b.flac"] {
            let path = fix.sandbox.resolve("music", rel).unwrap();
            assert!(
                std::fs::read(path).is_ok_and(|bytes| bytes == original),
                "{mode:?}: {rel} is in place"
            );
        }
        assert_eq!(
            std::fs::metadata(&first).unwrap().ino(),
            inode,
            "{mode:?}: never moved"
        );
        assert!(hidden_leftovers(&fix.dir).is_empty(), "{mode:?}");
        assert!(
            matches!(&err, PublishError::NotLinkable(text) if text.message.contains("\"staging/a.flac\"")),
            "{mode:?}: got {err:?}"
        );
        let journals = JournalStore::new(&open_db(&fix))
            .bundle("bundle-1")
            .unwrap();
        assert!(
            journals
                .iter()
                .all(|journal| journal.state == JournalState::Compensated),
            "{mode:?}"
        );
    }
}

/// A planned replacement sets the file in the way aside under the
/// journal. A crash before the commit resumes; the replaced file reaches
/// the recycle bin only after the commit, and its track is retired as
/// missing in the same transaction.
#[test]
fn replacement_recycles_only_after_commit() {
    let fix = fixture("replace");
    let (mut bundle, docs) = two_track_bundle(&fix, PlanKind::Move);
    let occupier = fix.sandbox.resolve("music", "organized/a.flac").unwrap();
    std::fs::create_dir_all(occupier.parent().unwrap()).unwrap();
    std::fs::write(&occupier, b"replaced-library-file").unwrap();
    open_db(&fix)
        .execute(
            "UPDATE local_tracks SET relative_path = 'organized/a.flac' WHERE id = 'track-s'",
            [],
        )
        .unwrap();
    let bin = fix.dir.join("bin/entry/a.flac");
    bundle.items[0].replaces = Some(publish::Replacement {
        sha256: sha256_hex(b"replaced-library-file"),
        track_id: Some("track-s".to_string()),
        recycle_to: bin.display().to_string(),
    });
    open_db(&fix)
        .execute(
            "INSERT INTO library_track_management_state (local_track_id, written_sha256) \
             VALUES ('track-s', ?1)",
            [sha256_hex(b"replaced-library-file")],
        )
        .unwrap();
    let (sealed, live) = seal(&bundle);
    let mut publisher = open_publisher(&fix);
    publisher.set_crash_point(Some(CrashPoint::AfterFirstRename));
    assert!(matches!(
        publisher.publish(&sealed, &live, &docs),
        Err(PublishError::InjectedCrash(_))
    ));
    drop(publisher);
    assert!(!bin.exists(), "nothing is recycled before the commit");
    assert!(
        hidden_leftovers(&fix.dir)
            .iter()
            .any(|path| std::fs::read(path).ok().as_deref()
                == Some(b"replaced-library-file".as_slice())),
        "set aside, not gone"
    );

    let db = restart(&fix);

    let conn = open_db(&fix);
    let results = reconcile(&db, &fix.sandbox).unwrap();
    assert_eq!(results[0].action, publish::RecoveryAction::ResumedCommitted);
    assert_staged(&occupier, "track-a");
    assert_eq!(std::fs::read(&bin).unwrap(), b"replaced-library-file");
    assert!(hidden_leftovers(&fix.dir).is_empty());
    let retired: (String, String) = conn
        .query_row(
            "SELECT availability, file_path FROM local_tracks WHERE id = 'track-s'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(retired, ("missing".to_string(), bin.display().to_string()));

    // The retired row's record goes with its file, so a file added by hand
    // to the album ('t', no record) does not stand in for it; nor for an
    // excluded row. A row whose file went missing still counts.
    let recorded: Option<String> = conn
        .query_row(
            "SELECT written_sha256 FROM library_track_management_state \
             WHERE local_track_id = 'track-s'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(recorded, None, "the retired row keeps no record");
    let stands_in = |conn: &Connection| {
        publish::record::changed_since_written(conn, "t", "music", "s.flac", "added-by-hand")
            .unwrap()
    };
    assert!(!stands_in(&conn));
    conn.execute(
        "UPDATE local_tracks SET availability = 'excluded' WHERE id = 'track-b'",
        [],
    )
    .unwrap();
    assert!(!stands_in(&conn), "an excluded row is no stand-in");
    conn.execute(
        "UPDATE local_tracks SET availability = 'missing' WHERE id = 'track-b'",
        [],
    )
    .unwrap();
    assert!(stands_in(&conn), "a missing written file is");
}

/// A generated file lands only on an empty path, and a later bundle takes
/// it away again (set aside, then deleted after the commit) only while it
/// still holds the bytes that were written.
#[test]
fn generated_files_land_and_go_away_journaled() {
    let fix = fixture("generated");
    let cover = b"generated-cover-bytes".to_vec();
    let sha = undo_put(&fix, &cover);
    let output = publish::GeneratedOutput {
        dest_root: "music".to_string(),
        dest_rel: "staging/cover.jpg".to_string(),
        sha256: sha.clone(),
        size: cover.len() as u64,
        kind: OutputKind::Artwork,
    };
    let fp = write_fixture_source(&fix, "music", "staging/a.flac", "management_full.flac");
    let mut item = plan_item(
        "track-a",
        "staging/a.flac",
        "staging/a.flac",
        fp,
        PlanKind::SamePath,
    );
    item.outputs.push(output.clone());
    let bundle = PlanBundle {
        id: "bundle-gen".to_string(),
        items: vec![item],
        profile_revision: 7,
        naming_revision: 3,
        policy_revision: 5,
        put_back: None,
        added: None,
        removal: None,
    };
    let mut docs = BTreeMap::new();
    docs.insert("track-a".to_string(), TagDocument::empty());
    let (sealed, live) = seal(&bundle);
    let mut publisher = open_publisher(&fix);
    publisher.publish(&sealed, &live, &docs).unwrap();
    let path = fix.sandbox.resolve("music", "staging/cover.jpg").unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), cover);
    assert_eq!(
        outputs_follow_the_rule(&open_db(&fix)),
        1,
        "kept at the commit"
    );

    // Taking it away: a crash after it was set aside unwinds or finishes,
    // never loses it half way.
    let fp = FileFingerprint {
        size: std::fs::metadata(fix.sandbox.resolve("music", "staging/a.flac").unwrap())
            .unwrap()
            .len(),
        sha256: sha256_hex(
            &std::fs::read(fix.sandbox.resolve("music", "staging/a.flac").unwrap()).unwrap(),
        ),
    };
    let mut item = plan_item(
        "track-a",
        "staging/a.flac",
        "staging/a.flac",
        fp,
        PlanKind::SamePath,
    );
    item.managed_updates = TagUpdates::new();
    item.removals.push(output);
    let bundle = PlanBundle {
        id: "bundle-undo".to_string(),
        items: vec![item],
        ..bundle
    };
    let (sealed, live) = seal(&bundle);
    publisher.set_crash_point(Some(CrashPoint::BeforeCatalogCommit));
    assert!(matches!(
        publisher.publish(&sealed, &live, &docs),
        Err(PublishError::InjectedCrash(_))
    ));
    drop(publisher);
    assert!(!path.exists(), "set aside before the commit");
    let db = restart(&fix);
    let results = reconcile(&db, &fix.sandbox).unwrap();
    assert_eq!(results[0].action, publish::RecoveryAction::ResumedCommitted);
    assert!(!path.exists(), "gone after the commit");
    assert_eq!(
        outputs_follow_the_rule(&open_db(&fix)),
        1,
        "taking a file away keeps the run that made it"
    );
    assert!(hidden_leftovers(&fix.dir).is_empty());
}

/// An unwind that linked a set-aside generated file back to its name and
/// stopped before removing the hidden name leaves one file under two
/// names. Recovery reads that as the put-back it is: the hidden name goes,
/// the generated file stays, and the bundle ends unwound.
#[test]
fn interrupted_generated_put_back_is_finished_by_recovery() {
    use std::os::unix::fs::MetadataExt as _;
    let fix = fixture("generated-put-back");
    let cover = b"generated-cover-bytes".to_vec();
    let sha = undo_put(&fix, &cover);
    let path = fix.sandbox.resolve("music", "staging/cover.jpg").unwrap();
    let audio = fix.sandbox.resolve("music", "staging/a.flac").unwrap();
    let fp = write_fixture_source(&fix, "music", "staging/a.flac", "management_full.flac");
    let audio_bytes = std::fs::read(&audio).unwrap();
    std::fs::write(&path, &cover).unwrap();
    let mut item = plan_item(
        "track-a",
        "staging/a.flac",
        "staging/a.flac",
        fp,
        PlanKind::SamePath,
    );
    item.managed_updates = TagUpdates::new();
    item.removals.push(publish::GeneratedOutput {
        dest_root: "music".to_string(),
        dest_rel: "staging/cover.jpg".to_string(),
        sha256: sha,
        size: cover.len() as u64,
        kind: OutputKind::Artwork,
    });
    let bundle = PlanBundle {
        id: "bundle-undo".to_string(),
        items: vec![item],
        profile_revision: 7,
        naming_revision: 3,
        policy_revision: 5,
        put_back: None,
        added: None,
        removal: None,
    };
    let mut docs = BTreeMap::new();
    docs.insert("track-a".to_string(), TagDocument::empty());
    let (sealed, live) = seal(&bundle);
    let mut publisher = open_publisher(&fix);
    publisher.set_crash_point(Some(CrashPoint::BeforeCatalogCommit));
    assert!(matches!(
        publisher.publish(&sealed, &live, &docs),
        Err(PublishError::InjectedCrash(_))
    ));
    drop(publisher);

    let db = restart(&fix);

    let conn = open_db(&fix);
    let removal = JournalStore::new(&conn)
        .bundle("bundle-undo")
        .unwrap()
        .into_iter()
        .find(|journal| journal.kind == publish::JournalKind::Delete)
        .expect("removal journaled");
    assert!(!path.exists(), "set aside before the commit");
    std::fs::hard_link(&removal.staged, &path).unwrap();

    let results = reconcile(&db, &fix.sandbox).unwrap();
    assert_eq!(results[0].action, publish::RecoveryAction::Compensated);
    assert_eq!(std::fs::read(&path).unwrap(), cover);
    assert_eq!(std::fs::metadata(&path).unwrap().nlink(), 1);
    assert!(std::fs::read(&audio).unwrap() == audio_bytes);
    assert!(hidden_leftovers(&fix.dir).is_empty());
    let journals = JournalStore::new(&conn).bundle("bundle-undo").unwrap();
    assert!(
        journals
            .iter()
            .all(|journal| journal.state == JournalState::Compensated)
    );
}

/// An occupant that changed since planning is never set aside.
#[test]
fn replacement_of_a_changed_file_is_refused() {
    let fix = fixture("replace-changed");
    let (mut bundle, docs) = two_track_bundle(&fix, PlanKind::Move);
    let occupier = fix.sandbox.resolve("music", "organized/a.flac").unwrap();
    std::fs::create_dir_all(occupier.parent().unwrap()).unwrap();
    std::fs::write(&occupier, b"edited-since-planning").unwrap();
    let bin = fix.dir.join("bin/entry/a.flac");
    bundle.items[0].replaces = Some(publish::Replacement {
        sha256: sha256_hex(b"replaced-library-file"),
        track_id: None,
        recycle_to: bin.display().to_string(),
    });
    let (sealed, live) = seal(&bundle);
    let mut publisher = open_publisher(&fix);
    assert!(publisher.publish(&sealed, &live, &docs).is_err());
    assert_eq!(std::fs::read(&occupier).unwrap(), b"edited-since-planning");
    assert!(!bin.exists());
    assert!(hidden_leftovers(&fix.dir).is_empty());
}

/// A file in the way that a bundle replaces, planted at the bundle's
/// first destination, with its bin entry `entry`.
fn replace_into(fix: &Fixture, bundle: &mut PlanBundle, entry: &std::path::Path) -> PathBuf {
    let occupier = fix.sandbox.resolve("music", "organized/a.flac").unwrap();
    std::fs::create_dir_all(occupier.parent().unwrap()).unwrap();
    std::fs::write(&occupier, b"replaced-library-file").unwrap();
    bundle.items[0].replaces = Some(publish::Replacement {
        sha256: sha256_hex(b"replaced-library-file"),
        track_id: None,
        recycle_to: entry.display().to_string(),
    });
    occupier
}

/// A replaced file goes into its recycle bin by the bin's own placement
/// mode, not the library's. On a bin whose disk has neither a no-replace
/// rename nor hard links it still reaches its entry, by check-then-rename
/// into the fresh entry folder, and nothing is left pending.
#[test]
fn a_replaced_file_reaches_a_bin_with_no_safe_step() {
    let fix = fixture("replace-check-bin");
    let (mut bundle, docs) = two_track_bundle(&fix, PlanKind::Move);
    let entry = fix.dir.join("bin/entry/a.flac");
    let occupier = replace_into(&fix, &mut bundle, &entry);
    let (sealed, live) = seal(&bundle);
    fix.sandbox
        .set_bin_placement_mode("bin", PlacementMode::Check);
    let mut publisher = open_publisher(&fix);
    publisher.set_bin_seams(Seams {
        skip_rename: true,
        link_errno: Some(libc::EPERM),
        ..Seams::default()
    });

    let outcome = publisher.publish(&sealed, &live, &docs).unwrap();

    assert_eq!(outcome, PublishOutcome::Committed);
    assert_staged(&occupier, "track-a");
    assert_eq!(std::fs::read(&entry).unwrap(), b"replaced-library-file");
    assert!(hidden_leftovers(&fix.dir).is_empty());
}

/// A bin entry never reaches through a symlink below the bin's folder:
/// one that would is refused before anything moves. A configured bin that
/// is itself a symlink still works: it is resolved once, and its checks
/// run on the folder it points to, so a link into a library folder is
/// not used and each root's `.recycle` takes its place. A root's own
/// `.recycle` that links into another library folder is not used either:
/// that root has no bin, and no other bin stands in.
#[test]
fn bin_entries_never_follow_a_symlink_below_the_bin() {
    use publish::bin::{BinRefusal, CONFIGURED_BIN_ID, choose, fresh_entry};
    use std::os::unix::fs::symlink;

    let fix = fixture("bin-symlink");
    let (mut bundle, docs) = two_track_bundle(&fix, PlanKind::Move);
    let elsewhere = fix.dir.join("elsewhere");
    std::fs::create_dir_all(&elsewhere).unwrap();
    std::fs::create_dir_all(fix.dir.join("bin")).unwrap();
    symlink(&elsewhere, fix.dir.join("bin/entry")).unwrap();
    let occupier = replace_into(&fix, &mut bundle, &fix.dir.join("bin/entry/a.flac"));
    let (sealed, live) = seal(&bundle);
    let err = open_publisher(&fix)
        .publish(&sealed, &live, &docs)
        .unwrap_err();
    assert!(matches!(err, PublishError::UnsafePath(_)), "got {err:?}");
    assert_eq!(std::fs::read(&occupier).unwrap(), b"replaced-library-file");
    assert_eq!(std::fs::read_dir(&elsewhere).unwrap().count(), 0);
    assert!(hidden_leftovers(&fix.dir).is_empty());

    let fix = fixture("bin-symlinked");
    let music = fix.dir.join("music");
    let library = [Root::library("music", music.clone())];
    let disk = fix.dir.join("bin-disk");
    std::fs::create_dir_all(&disk).unwrap();
    let linked = fix.dir.join("bin-link");
    symlink(&disk, &linked).unwrap();
    let choice = choose(&linked.display().to_string(), &library, &[]);
    let resolved = std::fs::canonicalize(&disk).unwrap();
    assert_eq!(choice.refused, None);
    assert_eq!(
        choice.roots,
        vec![Root::bin(CONFIGURED_BIN_ID, resolved.clone())]
    );
    std::fs::create_dir_all(music.join("Band")).unwrap();
    let into_library = fix.dir.join("into-library");
    symlink(music.join("Band"), &into_library).unwrap();
    let refused = choose(&into_library.display().to_string(), &library, &[]);
    assert_eq!(refused.refused, Some(BinRefusal::OverlapsLibrary));
    assert_eq!(refused.roots, vec![Root::recycle_of("music", &music)]);

    let mut roots = vec![
        Root::library("music", music),
        Root::library("incoming", fix.dir.join("incoming")),
    ];
    roots.extend(choice.roots);
    let sandbox = Sandbox::new(roots).unwrap();
    let (mut bundle, docs) = two_track_bundle(&fix, PlanKind::Move);
    let entry = fresh_entry(&resolved, "a.flac", std::time::SystemTime::now()).unwrap();
    let occupier = replace_into(&fix, &mut bundle, &entry);
    let (sealed, live) = seal(&bundle);
    let mut publisher = Publisher::new(
        sandbox,
        restart(&fix),
        FixedSpace(u64::MAX),
        jobs(&restart(&fix)),
        std::sync::Arc::new(ManualClock::at(DAY_50_MS)),
        blobs(&fix),
    );
    assert_eq!(
        publisher.publish(&sealed, &live, &docs).unwrap(),
        PublishOutcome::Committed
    );
    assert_staged(&occupier, "track-a");
    assert_eq!(std::fs::read(&entry).unwrap(), b"replaced-library-file");

    let music = fix.dir.join("music");
    let other = fix.dir.join("other");
    std::fs::create_dir_all(other.join("Album")).unwrap();
    symlink(other.join("Album"), music.join(".recycle")).unwrap();
    let both = [
        Root::library("music", music),
        Root::library("other", other.clone()),
    ];
    let choice = choose("", &both, &[]);
    assert_eq!(choice.roots, vec![Root::recycle_of("other", &other)]);
    assert_eq!(
        choice.unusable,
        vec![("music".to_owned(), BinRefusal::OverlapsLibrary)]
    );
}

/// When the bin is on another disk the replaced file is copied there, and
/// the set-aside copy goes only once the bin's folder is synced. A sync
/// that fails keeps the set-aside copy and leaves the row pending; the
/// next pass finishes the move.
#[test]
fn a_bin_copy_that_did_not_sync_keeps_the_set_aside_file() {
    let fix = fixture("bin-unsynced");
    let (mut bundle, docs) = two_track_bundle(&fix, PlanKind::Move);
    let entry = fix.dir.join("bin/entry/a.flac");
    let occupier = replace_into(&fix, &mut bundle, &entry);
    let (sealed, live) = seal(&bundle);
    let mut publisher = open_publisher(&fix);
    publisher.set_bin_seams(Seams {
        cross_device: true,
        unsynced: true,
        ..Seams::default()
    });

    let outcome = publisher.publish(&sealed, &live, &docs).unwrap();

    assert_eq!(outcome, PublishOutcome::CleanupPending);
    assert_staged(&occupier, "track-a");
    let set_aside = hidden_leftovers(&fix.dir.join("music"))
        .iter()
        .filter(|path| {
            std::fs::read(path).ok().as_deref() == Some(b"replaced-library-file".as_slice())
        })
        .count();
    assert_eq!(set_aside, 1, "the set-aside copy is kept");
    drop(publisher);
    reconcile(&restart(&fix), &fix.sandbox).unwrap();
    assert_eq!(std::fs::read(&entry).unwrap(), b"replaced-library-file");
    assert!(hidden_leftovers(&fix.dir).is_empty());
}

/// While the bins are unknown (a setting they come from cannot be read),
/// nothing moves into a bin and no other bin stands in. A publish that
/// would replace a file waits before anything moves; a replacement
/// already committed stays set aside, with no `.recycle` made, until the
/// bins are known again.
#[test]
fn nothing_goes_to_a_bin_while_the_bins_are_unknown() {
    let fix = fixture("bins-unknown");
    let music = fix.dir.join("music");
    let (mut bundle, docs) = two_track_bundle(&fix, PlanKind::Move);
    let entry = fix.dir.join("bin/entry/a.flac");
    let occupier = replace_into(&fix, &mut bundle, &entry);
    let (sealed, live) = seal(&bundle);
    let unknown = fix
        .sandbox
        .clone()
        .with_bins_unknown("the download settings cannot be read");
    let mut publisher = Publisher::new(
        unknown.clone(),
        restart(&fix),
        FixedSpace(u64::MAX),
        jobs(&restart(&fix)),
        std::sync::Arc::new(ManualClock::at(DAY_50_MS)),
        blobs(&fix),
    );
    let err = publisher.publish(&sealed, &live, &docs).unwrap_err();
    assert!(matches!(err, PublishError::Interrupted(_)), "got {err:?}");
    assert_eq!(std::fs::read(&occupier).unwrap(), b"replaced-library-file");
    drop(publisher);

    let mut publisher = open_publisher(&fix);
    publisher.set_crash_point(Some(CrashPoint::AfterCatalogCommit));
    assert!(matches!(
        publisher.publish(&sealed, &live, &docs),
        Err(PublishError::InjectedCrash(_))
    ));
    drop(publisher);
    reconcile(&restart(&fix), &unknown).unwrap();
    assert!(!entry.exists(), "nothing reached the bin");
    assert!(!music.join(".recycle").exists(), "no other bin stood in");
    let rows = JournalStore::new(&open_db(&fix))
        .bundle("bundle-1")
        .unwrap();
    assert!(
        rows.iter()
            .any(|row| row.state == JournalState::CleanupPending)
    );

    reconcile(&restart(&fix), &fix.sandbox).unwrap();
    assert_eq!(std::fs::read(&entry).unwrap(), b"replaced-library-file");
    assert!(hidden_leftovers(&fix.dir).is_empty());
}

// --- Removal briefs: a removed file is set aside, then binned; never lost. ---

/// A removal of `tracks` (id, path under `music`) with the files the test
/// planted, planned as a request plans one, against `sandbox`.
fn removal_bundle(
    fix: &Fixture,
    sandbox: &Sandbox,
    root: &str,
    tracks: &[(&str, &str)],
) -> PlanBundle {
    let targets: Vec<remove::Target> = tracks
        .iter()
        .map(|(track, rel)| remove::Target {
            track_id: (*track).to_string(),
            root_id: root.to_string(),
            relative_path: (*rel).to_string(),
            probe: remove::probe(
                sandbox.removal_root_dir(root).unwrap(),
                &sandbox.resolve_removal(root, rel).unwrap(),
            )
            .unwrap(),
            expect_sha256: None,
        })
        .collect();
    let removal = remove::plan(
        &restart(fix),
        sandbox,
        "bundle-r",
        &targets,
        "FILE_RECYCLED",
        None,
        None,
        std::time::SystemTime::now(),
    )
    .unwrap();
    PlanBundle {
        id: "bundle-r".to_string(),
        items: Vec::new(),
        profile_revision: 7,
        naming_revision: 3,
        policy_revision: 5,
        put_back: None,
        added: None,
        removal: Some(removal),
    }
}

/// Every regular file under `dir`.
fn files_under(dir: &std::path::Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(current) = stack.pop() {
        for entry in std::fs::read_dir(&current).into_iter().flatten().flatten() {
            match entry.file_type() {
                Ok(kind) if kind.is_dir() => stack.push(entry.path()),
                Ok(kind) if kind.is_file() => out.push(entry.path()),
                _ => {}
            }
        }
    }
    out
}

/// A removal that stops after the first of its two files is journaled
/// never counts as staged in full: recovery unwinds it. Both files stay
/// where they were, both tracks stay listed, and nothing reaches the bin.
#[test]
fn a_crash_after_the_first_of_two_removals_unwinds() {
    let fix = fixture("remove-first-row");
    write_fixture_source(&fix, "music", "staging/a.flac", "management_full.flac");
    write_fixture_source(&fix, "music", "staging/b.flac", "management_full.flac");
    let bundle = removal_bundle(
        &fix,
        &fix.sandbox,
        "music",
        &[("track-a", "staging/a.flac"), ("track-b", "staging/b.flac")],
    );
    let (sealed, live) = seal(&bundle);
    let mut publisher = open_publisher(&fix);
    publisher.set_crash_point(Some(CrashPoint::AfterFirstFileStaged));
    assert!(matches!(
        publisher.publish(&sealed, &live, &BTreeMap::new()),
        Err(PublishError::InjectedCrash(_))
    ));
    drop(publisher);

    reconcile(&restart(&fix), &fix.sandbox).unwrap();
    let conn = open_db(&fix);
    for (track, rel) in [("track-a", "staging/a.flac"), ("track-b", "staging/b.flac")] {
        assert!(
            fix.sandbox.resolve("music", rel).unwrap().exists(),
            "{rel} is home"
        );
        assert_eq!(track_row(&conn, track).1, "indexed", "{track} is listed");
    }
    assert!(
        files_under(&fix.dir.join("bin")).is_empty(),
        "nothing in the bin"
    );
    assert!(hidden_leftovers(&fix.dir).is_empty());
}

/// A file that went after its removal was planned fails the removal with
/// the collision the caller reads as "changed", and the bundle is unwound.
#[test]
fn a_removal_whose_file_went_since_its_plan_reports_a_change() {
    let fix = fixture("remove-file-went");
    write_fixture_source(&fix, "music", "staging/a.flac", "management_full.flac");
    let bundle = removal_bundle(
        &fix,
        &fix.sandbox,
        "music",
        &[("track-a", "staging/a.flac")],
    );
    std::fs::remove_file(fix.sandbox.resolve("music", "staging/a.flac").unwrap()).unwrap();
    let (sealed, live) = seal(&bundle);
    let mut publisher = open_publisher(&fix);
    let err = publisher
        .publish(&sealed, &live, &BTreeMap::new())
        .unwrap_err();
    assert!(matches!(err, PublishError::Collision(_)), "got {err:?}");
    let journals = JournalStore::new(&open_db(&fix))
        .bundle("bundle-r")
        .unwrap();
    assert!(
        journals
            .iter()
            .all(|journal| journal.state == JournalState::Compensated)
    );
    assert_eq!(track_row(&open_db(&fix), "track-a").1, "indexed");
}

/// A removed file its recycle bin cannot take waits under its hidden name
/// with its row pending, and every retry keeps it there. Once the bin can
/// take it, it moves in. It is never deleted.
#[test]
fn a_removal_the_bin_cannot_take_stays_set_aside() {
    let fix = fixture("remove-bin-blocked");
    write_fixture_source(&fix, "music", "staging/a.flac", "management_full.flac");
    let bytes = std::fs::read(fix.sandbox.resolve("music", "staging/a.flac").unwrap()).unwrap();
    let bundle = removal_bundle(
        &fix,
        &fix.sandbox,
        "music",
        &[("track-a", "staging/a.flac")],
    );
    let (sealed, live) = seal(&bundle);
    let mut publisher = open_publisher(&fix);
    // Something takes the bin's folder name once every check has passed.
    let blocker = fix.dir.join("bin");
    let planted = blocker.clone();
    publisher.set_before_set_aside(Some(Box::new(move |_| {
        std::fs::write(&planted, b"in the way").unwrap();
    })));

    let outcome = publisher.publish(&sealed, &live, &BTreeMap::new()).unwrap();

    assert_eq!(outcome, PublishOutcome::CleanupPending);
    drop(publisher);
    let held = |fix: &Fixture| {
        let hidden = hidden_leftovers(&fix.dir.join("music"));
        assert_eq!(hidden.len(), 1, "one hidden copy");
        assert_eq!(
            std::fs::read(&hidden[0]).unwrap(),
            bytes,
            "it is the removed file"
        );
        let rows = JournalStore::new(&open_db(fix)).bundle("bundle-r").unwrap();
        assert!(
            rows.iter()
                .all(|row| row.state == JournalState::CleanupPending)
        );
    };
    held(&fix);
    assert_eq!(track_row(&open_db(&fix), "track-a").1, "missing");
    reconcile(&restart(&fix), &fix.sandbox).unwrap();
    held(&fix);

    std::fs::remove_file(&blocker).unwrap();
    reconcile(&restart(&fix), &fix.sandbox).unwrap();
    let binned = files_under(&blocker);
    assert_eq!(binned.len(), 1);
    assert_eq!(std::fs::read(&binned[0]).unwrap(), bytes);
    assert!(hidden_leftovers(&fix.dir).is_empty());
}

/// A removal whose second file changes as it is set aside unwinds as a
/// whole: the first file, already set aside, goes back home, the changed
/// one stays as it is, both tracks stay listed, and nothing is binned.
#[test]
fn a_file_changed_during_its_removal_unwinds_the_removal() {
    let fix = fixture("remove-changed");
    write_fixture_source(&fix, "music", "staging/a.flac", "management_full.flac");
    write_fixture_source(&fix, "music", "staging/b.flac", "management_full.flac");
    let a = fix.sandbox.resolve("music", "staging/a.flac").unwrap();
    let a_bytes = std::fs::read(&a).unwrap();
    let bundle = removal_bundle(
        &fix,
        &fix.sandbox,
        "music",
        &[("track-a", "staging/a.flac"), ("track-b", "staging/b.flac")],
    );
    let (sealed, live) = seal(&bundle);
    let mut publisher = open_publisher(&fix);
    publisher.set_before_set_aside(Some(Box::new(|dest| {
        if dest.ends_with("b.flac") {
            std::fs::write(dest, b"saved by another program").unwrap();
        }
    })));

    let err = publisher
        .publish(&sealed, &live, &BTreeMap::new())
        .unwrap_err();

    assert!(matches!(err, PublishError::Collision(_)), "got {err:?}");
    assert_eq!(
        std::fs::read(&a).unwrap(),
        a_bytes,
        "the first file is home"
    );
    assert_eq!(
        std::fs::read(fix.sandbox.resolve("music", "staging/b.flac").unwrap()).unwrap(),
        b"saved by another program"
    );
    let conn = open_db(&fix);
    assert_eq!(track_row(&conn, "track-a").1, "indexed");
    assert_eq!(track_row(&conn, "track-b").1, "indexed");
    assert!(
        files_under(&fix.dir.join("bin")).is_empty(),
        "nothing in the bin"
    );
    assert!(hidden_leftovers(&fix.dir).is_empty());
}

/// A removal whose unwind put a file home and stopped before it recorded
/// so is unwound by recovery, never committed: the file stays home with
/// its track listed, and the other file comes back too.
#[test]
fn a_removal_unwind_that_stopped_after_a_file_went_home_unwinds() {
    let fix = fixture("remove-unwind-stopped");
    write_fixture_source(&fix, "music", "staging/a.flac", "management_full.flac");
    write_fixture_source(&fix, "music", "staging/b.flac", "management_full.flac");
    let bundle = removal_bundle(
        &fix,
        &fix.sandbox,
        "music",
        &[("track-a", "staging/a.flac"), ("track-b", "staging/b.flac")],
    );
    let (sealed, live) = seal(&bundle);
    let mut publisher = open_publisher(&fix);
    publisher.set_crash_point(Some(CrashPoint::BeforeCatalogCommit));
    assert!(matches!(
        publisher.publish(&sealed, &live, &BTreeMap::new()),
        Err(PublishError::InjectedCrash(_))
    ));
    drop(publisher);
    // The unwind put track-a's file home, then the process died before
    // the row was marked compensated.
    let rows = JournalStore::new(&open_db(&fix))
        .bundle("bundle-r")
        .unwrap();
    let row_a = rows
        .iter()
        .find(|row| {
            row.replaces
                .as_ref()
                .and_then(|replaced| replaced.track_id.as_deref())
                == Some("track-a")
        })
        .unwrap();
    assert_eq!(row_a.state, JournalState::Published);
    let a = fix.sandbox.resolve("music", "staging/a.flac").unwrap();
    std::fs::rename(&row_a.staged, &a).unwrap();

    reconcile(&restart(&fix), &fix.sandbox).unwrap();

    let conn = open_db(&fix);
    for (track, rel) in [("track-a", "staging/a.flac"), ("track-b", "staging/b.flac")] {
        assert!(
            fix.sandbox.resolve("music", rel).unwrap().exists(),
            "{rel} is home"
        );
        assert_eq!(track_row(&conn, track).1, "indexed", "{track} is listed");
    }
    assert!(
        files_under(&fix.dir.join("bin")).is_empty(),
        "nothing in the bin"
    );
    assert!(hidden_leftovers(&fix.dir).is_empty());
}

// --- Upgrade briefs: the old file and the new one in one bundle. ---

/// One upgrade bundle: track-a's file (staged at `staging/a.flac`) is
/// placed at `dest` while track-b's file (`staging/b.flac`, the old copy)
/// is taken to the bin. `dest` is the old copy's own path or a new one.
/// Answers the bundle and the old copy's bytes.
fn upgrade_bundle(
    fix: &Fixture,
    dest: &str,
) -> (PlanBundle, BTreeMap<String, TagDocument>, Vec<u8>) {
    let fp_a = write_fixture_source(fix, "music", "staging/a.flac", "management_full.flac");
    write_fixture_source(fix, "music", "staging/b.flac", "flac_full_01.flac");
    let old = std::fs::read(fix.sandbox.resolve("music", "staging/b.flac").unwrap()).unwrap();
    let target = remove::Target {
        track_id: "track-b".to_string(),
        root_id: "music".to_string(),
        relative_path: "staging/b.flac".to_string(),
        probe: remove::probe(
            fix.sandbox.removal_root_dir("music").unwrap(),
            &fix.sandbox
                .resolve_removal("music", "staging/b.flac")
                .unwrap(),
        )
        .unwrap(),
        expect_sha256: None,
    };
    let removal = remove::plan(
        &restart(fix),
        &fix.sandbox,
        "bundle-u",
        &[target],
        "FILE_RECYCLED",
        None,
        None,
        std::time::SystemTime::now(),
    )
    .unwrap();
    let bundle = PlanBundle {
        id: "bundle-u".to_string(),
        items: vec![plan_item(
            "track-a",
            "staging/a.flac",
            dest,
            fp_a,
            PlanKind::Move,
        )],
        profile_revision: 7,
        naming_revision: 3,
        policy_revision: 5,
        put_back: None,
        added: None,
        removal: Some(removal),
    };
    let mut docs = BTreeMap::new();
    docs.insert("track-a".to_string(), TagDocument::empty());
    (bundle, docs, old)
}

/// How an upgrade bundle ended once recovery ran.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Ended {
    /// The new file is at its destination and listed; the old copy is in
    /// the bin and its track missing.
    Upgraded,
    /// The old copy is at its path and listed; the new file never landed.
    OldInPlace,
}

fn assert_ended(fix: &Fixture, dest: &str, old: &[u8], ended: Ended, case: &str) {
    let conn = open_db(fix);
    let old_path = fix.sandbox.resolve("music", "staging/b.flac").unwrap();
    let binned: Vec<PathBuf> = files_under(&fix.dir.join("bin"));
    match ended {
        Ended::Upgraded => {
            assert_staged(&fix.sandbox.resolve("music", dest).unwrap(), "track-a");
            assert_eq!(
                track_row(&conn, "track-a").0,
                dest,
                "{case}: the new file is listed at its destination"
            );
            assert_eq!(track_row(&conn, "track-a").1, "indexed", "{case}");
            assert_eq!(track_row(&conn, "track-b").1, "missing", "{case}");
            assert_eq!(binned.len(), 1, "{case}: the old copy is in the bin");
            assert_eq!(std::fs::read(&binned[0]).unwrap(), old, "{case}");
            if dest != "staging/b.flac" {
                assert!(!old_path.exists(), "{case}: the old copy left its path");
            }
        }
        Ended::OldInPlace => {
            assert_eq!(
                std::fs::read(&old_path).unwrap(),
                old,
                "{case}: the old copy is home"
            );
            assert_eq!(
                track_row(&conn, "track-b"),
                ("staging/b.flac".to_string(), "indexed".to_string(), None),
                "{case}"
            );
            assert_eq!(track_row(&conn, "track-a").0, "staging/a.flac", "{case}");
            assert!(binned.is_empty(), "{case}: nothing in the bin");
        }
    }
    assert!(
        hidden_leftovers(&fix.dir).is_empty(),
        "{case}: no hidden file is left"
    );
}

/// An upgrade publishes its new file and takes its old copy away in one
/// bundle. Stopped at any step and recovered, the new file is in place and
/// the old copy in the bin, or the old copy is in place and the new file
/// never landed: never neither. The new file goes to a path of its own or
/// to the old copy's own path. A resume whose commit is refused unwinds:
/// the new file comes off before the old copy goes home.
#[test]
fn an_upgrade_stopped_at_any_step_keeps_one_copy() {
    let resumed = CrashPoint::all()
        .into_iter()
        .map(|point| (point, Ended::Upgraded));
    let unwound = [
        (CrashPoint::AfterFirstFileStaged, Ended::OldInPlace),
        (CrashPoint::AfterTempWritten, Ended::OldInPlace),
    ];
    for dest in ["organized/a.flac", "staging/b.flac"] {
        for (point, ended) in resumed.clone().chain(unwound) {
            let case = format!("{dest} {}", point.as_str());
            let fix = fixture(&format!("upgrade-{}-{}", dest.len(), point.as_str()));
            let (bundle, docs, old) = upgrade_bundle(&fix, dest);
            let (sealed, live) = seal(&bundle);
            let mut publisher = open_publisher(&fix);
            publisher.set_crash_point(Some(point));
            let err = publisher.publish(&sealed, &live, &docs).unwrap_err();
            assert!(
                matches!(err, PublishError::InjectedCrash(_)),
                "{case}: {err:?}"
            );
            drop(publisher);
            reconcile(&restart(&fix), &fix.sandbox).unwrap();
            assert_ended(&fix, dest, &old, ended, &case);
        }

        // Stopped with both files moved, then the old copy's row changed:
        // the resumed commit is refused and the bundle unwinds.
        let case = format!("{dest} refused resume");
        let fix = fixture(&format!("upgrade-{}-refused", dest.len()));
        let (bundle, docs, old) = upgrade_bundle(&fix, dest);
        let (sealed, live) = seal(&bundle);
        let mut publisher = open_publisher(&fix);
        publisher.set_crash_point(Some(CrashPoint::BeforeCatalogCommit));
        assert!(publisher.publish(&sealed, &live, &docs).is_err(), "{case}");
        drop(publisher);
        open_db(&fix)
            .execute(
                "UPDATE local_tracks SET stat_revision = '1:1' WHERE id = 'track-b'",
                [],
            )
            .unwrap();
        reconcile(&restart(&fix), &fix.sandbox).unwrap();
        let conn = open_db(&fix);
        assert_eq!(
            std::fs::read(fix.sandbox.resolve("music", "staging/b.flac").unwrap()).unwrap(),
            old,
            "{case}: the old copy is home"
        );
        assert_eq!(track_row(&conn, "track-b").1, "indexed", "{case}");
        assert_eq!(track_row(&conn, "track-a").0, "staging/a.flac", "{case}");
        assert!(files_under(&fix.dir.join("bin")).is_empty(), "{case}");
        assert!(hidden_leftovers(&fix.dir).is_empty(), "{case}");
    }

    // A placement refused by a file that turned up at the new path, and
    // the unwind stopped after its first row: recovery finishes it, and
    // the old copy and the file in the way both stay.
    let fix = fixture("upgrade-first-unwind");
    let (bundle, docs, old) = upgrade_bundle(&fix, "organized/a.flac");
    let (sealed, live) = seal(&bundle);
    let mut publisher = open_publisher(&fix);
    publisher.set_before_place(Some(Box::new(|dest| {
        std::fs::write(dest, b"someone else's file").unwrap();
    })));
    publisher.set_crash_point(Some(CrashPoint::AfterFirstUnwind));
    assert!(publisher.publish(&sealed, &live, &docs).is_err());
    drop(publisher);
    reconcile(&restart(&fix), &fix.sandbox).unwrap();
    let conn = open_db(&fix);
    assert_eq!(
        std::fs::read(fix.sandbox.resolve("music", "staging/b.flac").unwrap()).unwrap(),
        old
    );
    assert_eq!(track_row(&conn, "track-b").1, "indexed");
    assert_eq!(
        std::fs::read(fix.sandbox.resolve("music", "organized/a.flac").unwrap()).unwrap(),
        b"someone else's file"
    );
    assert!(files_under(&fix.dir.join("bin")).is_empty());
    assert!(hidden_leftovers(&fix.dir).is_empty());
}

/// Rewrite `path` in place with other bytes of the same length and put its
/// modified time back, as a tag editor that keeps timestamps does.
fn edit_keeping_size_and_mtime(path: &std::path::Path) {
    use std::io::{Seek, Write};
    let mut bytes = std::fs::read(path).unwrap();
    let modified = std::fs::metadata(path).unwrap().modified().unwrap();
    bytes[0] ^= 0xff;
    let mut file = std::fs::OpenOptions::new().write(true).open(path).unwrap();
    file.seek(std::io::SeekFrom::Start(0)).unwrap();
    file.write_all(&bytes).unwrap();
    file.set_modified(modified).unwrap();
}

/// A file edited in place after its removal read it, keeping its size and
/// modified time, is not removed: the check just before it moves sees its
/// change time, and the file stays home as it is now.
#[test]
fn a_file_edited_in_place_after_it_was_read_is_not_removed() {
    let fix = fixture("remove-edited-before-move");
    write_fixture_source(&fix, "music", "staging/a.flac", "management_full.flac");
    let bundle = removal_bundle(
        &fix,
        &fix.sandbox,
        "music",
        &[("track-a", "staging/a.flac")],
    );
    let a = fix.sandbox.resolve("music", "staging/a.flac").unwrap();
    edit_keeping_size_and_mtime(&a);
    let edited = std::fs::read(&a).unwrap();
    let (sealed, live) = seal(&bundle);
    let mut publisher = open_publisher(&fix);

    let err = publisher
        .publish(&sealed, &live, &BTreeMap::new())
        .unwrap_err();

    assert!(matches!(err, PublishError::Collision(_)), "got {err:?}");
    assert_eq!(
        std::fs::read(&a).unwrap(),
        edited,
        "the file is home as edited"
    );
    assert_eq!(track_row(&open_db(&fix), "track-a").1, "indexed");
    assert!(hidden_leftovers(&fix.dir).is_empty());
}

/// A set-aside file edited in place after its removal committed, keeping
/// its size and modified time, still reaches the bin as it is now (the
/// change is logged) instead of waiting for good: on recovery on the same
/// disk, and when the bin is on another disk.
#[test]
fn a_removed_file_edited_in_place_still_reaches_the_bin() {
    let fix = fixture("remove-edited");
    write_fixture_source(&fix, "music", "staging/a.flac", "management_full.flac");
    let bundle = removal_bundle(
        &fix,
        &fix.sandbox,
        "music",
        &[("track-a", "staging/a.flac")],
    );
    let (sealed, live) = seal(&bundle);
    let mut publisher = open_publisher(&fix);
    publisher.set_crash_point(Some(CrashPoint::AfterCatalogCommit));
    assert!(matches!(
        publisher.publish(&sealed, &live, &BTreeMap::new()),
        Err(PublishError::InjectedCrash(_))
    ));
    drop(publisher);
    let hidden = hidden_leftovers(&fix.dir.join("music"));
    assert_eq!(hidden.len(), 1);
    edit_keeping_size_and_mtime(&hidden[0]);
    let edited = std::fs::read(&hidden[0]).unwrap();

    reconcile(&restart(&fix), &fix.sandbox).unwrap();

    let binned = files_under(&fix.dir.join("bin"));
    assert_eq!(binned.len(), 1, "the edited file is in the bin");
    assert_eq!(std::fs::read(&binned[0]).unwrap(), edited);
    assert!(hidden_leftovers(&fix.dir).is_empty());
    let rows = JournalStore::new(&open_db(&fix))
        .bundle("bundle-r")
        .unwrap();
    assert!(rows.iter().all(|row| row.state == JournalState::Cleaned));

    // The bin on another disk: the bytes copied are the ones read now.
    let from = fix.sandbox.resolve("music", "staging/c.flac").unwrap();
    std::fs::write(&from, b"removed-library-file").unwrap();
    let known = publish::paths::FileStat::read(&from).unwrap();
    edit_keeping_size_and_mtime(&from);
    let edited = std::fs::read(&from).unwrap();
    let to = fix.dir.join("bin/entry/c.flac");
    let moved = publish::paths::recycle_backup(
        PlacementMode::Unknown,
        Seams {
            cross_device: true,
            ..Seams::default()
        },
        &from,
        &to,
        &sha256_hex(b"removed-library-file"),
        Some(known),
    )
    .unwrap();
    assert!(moved);
    assert_eq!(std::fs::read(&to).unwrap(), edited);
    assert!(!from.exists());
}

/// A copy to another disk that stops part way leaves nothing behind: no
/// file at the destination, no hidden temp, and the source keeps its
/// bytes. The move into the bin copies this way.
#[test]
fn a_copy_that_fails_part_way_leaves_nothing_behind() {
    let fix = fixture("copy-short");
    let from = fix.dir.join("bin/entry/a.flac");
    std::fs::create_dir_all(from.parent().unwrap()).unwrap();
    std::fs::write(&from, b"removed-library-file").unwrap();
    let to = fix.sandbox.resolve("music", "staging/a.flac").unwrap();
    let seams = Seams {
        cross_device: true,
        short_write: true,
        ..Seams::default()
    };

    let err = publish::paths::recycle_backup(
        PlacementMode::Unknown,
        seams,
        &from,
        &to,
        &sha256_hex(b"removed-library-file"),
        None,
    )
    .unwrap_err();

    assert!(matches!(err, PublishError::Io(_)), "got {err:?}");
    assert!(
        std::fs::symlink_metadata(&to).is_err(),
        "nothing at the destination"
    );
    assert!(hidden_leftovers(&fix.dir).is_empty(), "no unfinished copy");
    assert_eq!(std::fs::read(&from).unwrap(), b"removed-library-file");
}

/// A library folder only removals may take files from (an excluded
/// folder whose included subfolders are in the catalog): a removal moves
/// its file to the bin, and any other write aimed at it is refused before
/// anything moves.
#[test]
fn a_remove_only_root_takes_removals_only() {
    let fix = fixture("remove-only");
    let excluded = fix.dir.join("excluded");
    std::fs::create_dir_all(&excluded).unwrap();
    let sandbox = Sandbox::new(vec![
        Root::library("music", fix.dir.join("music")),
        Root::remove_only("excluded", excluded.clone()),
        Root::bin("bin", fix.dir.join("bin")),
    ])
    .unwrap();
    open_db(&fix)
        .execute(
            "UPDATE local_tracks SET root_id = 'excluded' WHERE id = 't'",
            [],
        )
        .unwrap();
    std::fs::write(excluded.join("s.flac"), b"excluded-file").unwrap();
    let mut publisher = Publisher::new(
        sandbox.clone(),
        restart(&fix),
        FixedSpace(u64::MAX),
        jobs(&restart(&fix)),
        std::sync::Arc::new(ManualClock::at(DAY_50_MS)),
        blobs(&fix),
    );

    let removal = removal_bundle(&fix, &sandbox, "excluded", &[("t", "s.flac")]);
    let (sealed, live) = seal(&removal);
    let outcome = publisher.publish(&sealed, &live, &BTreeMap::new()).unwrap();

    assert_eq!(outcome, PublishOutcome::Committed);
    assert!(!excluded.join("s.flac").exists());
    let binned = files_under(&fix.dir.join("bin"));
    assert_eq!(binned.len(), 1);
    assert_eq!(std::fs::read(&binned[0]).unwrap(), b"excluded-file");
    assert_eq!(track_row(&open_db(&fix), "t").1, "missing");

    let fp = write_fixture_source(&fix, "music", "staging/a.flac", "management_full.flac");
    let mut item = plan_item(
        "track-a",
        "staging/a.flac",
        "organized/a.flac",
        fp,
        PlanKind::Move,
    );
    item.dest_root = "excluded".to_string();
    item.capabilities.push(Capability::CrossRootMove);
    let bundle = PlanBundle {
        id: "bundle-into-excluded".to_string(),
        items: vec![item],
        profile_revision: 7,
        naming_revision: 3,
        policy_revision: 5,
        put_back: None,
        added: None,
        removal: None,
    };
    let (sealed, live) = seal(&bundle);
    let mut docs = BTreeMap::new();
    docs.insert("track-a".to_string(), TagDocument::empty());
    let err = publisher.publish(&sealed, &live, &docs).unwrap_err();

    assert!(matches!(err, PublishError::UnsafePath(_)), "got {err:?}");
    assert!(!excluded.join("organized/a.flac").exists());
    assert!(
        fix.sandbox
            .resolve("music", "staging/a.flac")
            .unwrap()
            .exists()
    );
}

#[test]
fn hardlinked_audio_publishes_an_independent_inode() {
    let fix = fixture("hardlink");
    let (bundle, docs) = two_track_bundle(&fix, PlanKind::Move);
    let source = fix.sandbox.resolve("music", "staging/a.flac").unwrap();
    let link = fix.sandbox.resolve("music", "staging/a-link.flac").unwrap();
    std::fs::hard_link(&source, &link).unwrap();
    let (sealed, live) = seal(&bundle);
    let mut publisher = open_publisher(&fix);
    publisher.publish(&sealed, &live, &docs).unwrap();
    assert_eq!(
        std::fs::read(&link).unwrap(),
        std::fs::read(fixtures_dir().join("management_full.flac")).unwrap()
    );
}

// --- Preservation brief: unknown, empty, and multi-valued tags plus
// unknown native frames survive Apply through the real save wrapper. ---

#[test]
fn unknown_empty_and_multivalued_tags_survive_apply() {
    let mut before = TagDocument::empty();
    before
        .custom
        .insert("custom_rating".to_string(), vec!["5".to_string()]);
    before
        .custom
        .insert("empty_note".to_string(), vec!["".to_string()]);
    before.managed.insert(
        TagTarget::Field(TagField::Genre),
        vec!["Rock".to_string(), "Indie".to_string()],
    );
    before
        .unknown_frames
        .insert("TXXX:PRIV".to_string(), vec![0, 1, 2, 3]);
    before
        .unknown_frames
        .insert("APIC:odd".to_string(), vec![9, 9, 9]);

    let mut updates = TagUpdates::new();
    updates.insert(
        TagTarget::Field(TagField::Title),
        vec!["New Title".to_string()],
    );

    let fix = fixture("preserve-apply");
    let fp = write_fixture_source(&fix, "music", "staging/s.flac", "management_full.flac");
    let mut item = plan_item(
        "track-s",
        "staging/s.flac",
        "staging/s.flac",
        fp,
        PlanKind::SamePath,
    );
    item.managed_updates = updates;
    let bundle = PlanBundle {
        id: "bundle-preserve".to_string(),
        items: vec![item],
        profile_revision: 7,
        naming_revision: 3,
        policy_revision: 5,
        put_back: None,
        added: None,
        removal: None,
    };
    let mut docs = BTreeMap::new();
    docs.insert("track-s".to_string(), before);
    let (sealed, live) = seal(&bundle);
    let mut publisher = open_publisher(&fix);
    publisher.publish(&sealed, &live, &docs).unwrap();
    let dest = fix.sandbox.resolve("music", "staging/s.flac").unwrap();
    // The planned title landed through the real save wrapper while
    // multi-values, custom fields, and the vendor spelling survived.
    let tag = tags::read::read_tag_only(&dest, tags::AudioFormat::Flac).unwrap();
    assert_eq!(tag.title, "New Title");
    assert_eq!(tag.genres, vec!["Electronic", "Ambient"]);
    let bytes = std::fs::read(&dest).unwrap();
    for needle in [b"opaque local value".as_slice(), b"TOTALTRACKS".as_slice()] {
        assert!(
            bytes.windows(needle.len()).any(|window| window == needle),
            "missing {needle:?}"
        );
    }
    tags::probe(&dest).unwrap();
}

// --- Archive safety briefs. ---

fn entry(name: &str, size: u64, compressed: u64) -> ArchiveEntry {
    ArchiveEntry {
        name: name.to_string(),
        size,
        compressed_size: compressed,
        is_link: false,
        is_dir: false,
    }
}

#[test]
fn archive_safety_blocks_hostile_manifests() {
    let policy = ArchivePolicy::default();
    assert!(validate_archive(&[entry("a/../../evil.flac", 10, 10)], &policy).is_err());
    assert!(validate_archive(&[entry("/abs/evil.flac", 10, 10)], &policy).is_err());
    assert!(validate_archive(&[entry("C:\\evil.flac", 10, 10)], &policy).is_err());
    assert!(
        validate_archive(
            &[ArchiveEntry {
                name: "link.flac".to_string(),
                size: 4,
                compressed_size: 4,
                is_link: true,
                is_dir: false,
            }],
            &policy,
        )
        .is_err()
    );
    assert!(validate_archive(&[entry("bomb", 200 * 1024 * 1024, 1024 * 1024)], &policy).is_err());
    let many: Vec<ArchiveEntry> = (0..10_001)
        .map(|i| entry(&format!("f{i}.flac"), 10, 10))
        .collect();
    assert!(validate_archive(&many, &policy).is_err());
    assert!(validate_archive(&[entry(&"d/".repeat(20), 10, 10)], &policy).is_err());

    let ok = validate_archive(
        &[
            entry("album/01.flac", 1024, 900),
            entry("album/cover.jpg", 512, 500),
        ],
        &policy,
    )
    .unwrap();
    assert_eq!(ok.files, 2);
    assert_eq!(ok.total_bytes, 1536);
}

// --- Sandbox brief: every write stays under sandbox roots. ---

#[test]
fn sandbox_rejects_escapes_and_symlinks() {
    let fix = fixture("sandbox");
    assert!(fix.sandbox.resolve("music", "../incoming/x").is_err());
    assert!(fix.sandbox.resolve("music", "/abs/x").is_err());
    assert!(fix.sandbox.resolve("music", "a\0b").is_err());
    assert!(fix.sandbox.resolve("nope", "x").is_err());

    #[cfg(unix)]
    {
        let target = fix.dir.join("foreign");
        std::fs::create_dir_all(&target).unwrap();
        let link = fix.sandbox.resolve("music", "linked").unwrap();
        std::os::unix::fs::symlink(&target, &link).unwrap();
        assert!(
            fix.sandbox
                .resolve_no_symlink("music", "linked/x.flac")
                .is_err()
        );
    }
}

// --- Undo briefs: later external edits are never rolled back. ---

fn undo_input(published_sha: &str) -> UndoInput {
    let mut doc = TagDocument::empty();
    doc.managed.insert(
        TagTarget::Field(TagField::Title),
        vec!["Before".to_string()],
    );
    UndoInput {
        track_id: "track-a".to_string(),
        before: BeforeState {
            doc,
            source_root: "music".to_string(),
            source_rel: "staging/a.flac".to_string(),
            source_sha256: "source-sha".to_string(),
            mgmt_state_before: Some("managed:6".to_string()),
        },
        published: FileFingerprint {
            size: 10,
            sha256: published_sha.to_string(),
        },
        published_root: "music".to_string(),
        published_rel: "organized/a.flac".to_string(),
        identity: test_identity(),
        override_revision: 2,
        expires_day: 140,
    }
}

fn undo_live(published_sha: &str) -> UndoLive {
    let mut fingerprints = BTreeMap::new();
    fingerprints.insert(
        "track-a".to_string(),
        FileFingerprint {
            size: 10,
            sha256: published_sha.to_string(),
        },
    );
    let mut locations = BTreeMap::new();
    locations.insert(
        "track-a".to_string(),
        ("music".to_string(), "organized/a.flac".to_string()),
    );
    let mut identities = BTreeMap::new();
    identities.insert("track-a".to_string(), test_identity());
    let mut overrides = BTreeMap::new();
    overrides.insert("track-a".to_string(), 2);
    UndoLive {
        fingerprints,
        locations,
        identities,
        overrides,
        today_day: 50,
    }
}

#[test]
fn undo_rejects_later_external_edits() {
    let input = undo_input("published-sha");
    let live = undo_live("published-sha");
    let plan = plan_undo("op-1", std::slice::from_ref(&input), &live, &|_, _| false);
    assert!(plan.writable());
    assert_eq!(plan.eligible[0].restore_rel, "staging/a.flac");
    assert_eq!(
        plan.eligible[0].mgmt_state_before.as_deref(),
        Some("managed:6")
    );

    let edited = undo_live("externally-retagged");
    let plan = plan_undo("op-1", std::slice::from_ref(&input), &edited, &|_, _| false);
    assert!(!plan.writable());
    assert_eq!(
        plan.blocked,
        vec![("track-a".to_string(), UndoBlock::ExternallyChanged)]
    );
}

#[test]
fn undo_bundle_stays_nonwritable_while_any_sibling_is_stale() {
    let first = undo_input("published-sha");
    let mut second = undo_input("published-sha");
    second.track_id = "track-b".to_string();
    let mut live = undo_live("published-sha");
    live.fingerprints.insert(
        "track-b".to_string(),
        FileFingerprint {
            size: 10,
            sha256: "published-sha".to_string(),
        },
    );
    live.locations.insert(
        "track-b".to_string(),
        ("music".to_string(), "elsewhere/b.flac".to_string()),
    );
    live.identities
        .insert("track-b".to_string(), test_identity());
    live.overrides.insert("track-b".to_string(), 2);
    let plan = plan_undo("op-1", &[first, second], &live, &|_, _| false);
    assert!(!plan.writable());
    assert_eq!(plan.blocked.len(), 1);
    assert_eq!(plan.blocked[0].1, UndoBlock::Moved);
}

#[test]
fn undo_blocks_occupied_restore_paths() {
    let input = undo_input("published-sha");
    let live = undo_live("published-sha");
    let plan = plan_undo("op-1", &[input], &live, &|_, _| true);
    assert!(!plan.writable());
    assert_eq!(plan.blocked[0].1, UndoBlock::RestoreOccupied);
}

// --- Baseline restore and purge briefs. ---

fn baseline_input() -> BaselineInput {
    let mut doc = TagDocument::empty();
    doc.managed.insert(
        TagTarget::Field(TagField::Title),
        vec!["Original".to_string()],
    );
    BaselineInput {
        track_id: "track-a".to_string(),
        baseline: Some(BeforeState {
            doc,
            source_root: "music".to_string(),
            source_rel: "original/a.flac".to_string(),
            source_sha256: "orig-sha".to_string(),
            mgmt_state_before: None,
        }),
        current: FileFingerprint {
            size: 12,
            sha256: "current-sha".to_string(),
        },
        pinned_current: FileFingerprint {
            size: 12,
            sha256: "current-sha".to_string(),
        },
        identity: test_identity(),
        pinned_identity: test_identity(),
        format: AudioFormat::Flac,
        pinned_format: AudioFormat::Flac,
    }
}

#[test]
fn baseline_restore_blocks_missing_changed_and_occupied() {
    let input = baseline_input();
    let plan = plan_baseline_restore(std::slice::from_ref(&input), &|_, _| Some(false));
    assert!(plan.writable());

    let missing = BaselineInput {
        baseline: None,
        ..input.clone()
    };
    let plan = plan_baseline_restore(std::slice::from_ref(&missing), &|_, _| Some(false));
    assert_eq!(plan.blocked[0].1, BaselineBlock::MissingBaseline);

    let plan = plan_baseline_restore(std::slice::from_ref(&input), &|_, _| Some(true));
    assert_eq!(plan.blocked[0].1, BaselineBlock::OriginalOccupied);

    let plan = plan_baseline_restore(std::slice::from_ref(&input), &|_, _| None);
    assert_eq!(plan.blocked[0].1, BaselineBlock::MissingRoot);

    let changed = BaselineInput {
        current: FileFingerprint {
            size: 99,
            sha256: "other".to_string(),
        },
        ..input.clone()
    };
    let plan = plan_baseline_restore(std::slice::from_ref(&changed), &|_, _| Some(false));
    assert_eq!(plan.blocked[0].1, BaselineBlock::CurrentChanged);

    let reformatted = BaselineInput {
        format: AudioFormat::Mp3,
        ..input
    };
    let plan = plan_baseline_restore(std::slice::from_ref(&reformatted), &|_, _| Some(false));
    assert_eq!(plan.blocked[0].1, BaselineBlock::FormatMismatch);
}

#[test]
fn baseline_purge_requires_phrase_token_and_quiet_journals() {
    assert!(confirm_baseline_purge("purge", "tok", "tok", 3, 0).is_err());
    assert!(confirm_baseline_purge("PURGE BASELINES", "wrong", "tok", 3, 0).is_err());
    assert!(confirm_baseline_purge("PURGE BASELINES", "tok", "tok", 3, 1).is_err());
    assert!(confirm_baseline_purge("PURGE BASELINES", "tok", "tok", 0, 0).is_err());
    assert_eq!(
        confirm_baseline_purge("PURGE BASELINES", "tok", "tok", 3, 0).unwrap(),
        3
    );
}

// --- Snapshot briefs: expiry GC, immutable baselines, dedup. ---

#[test]
fn snapshots_expire_baselines_stay_and_blobs_dedup() {
    let fix = fixture("snapshots");
    let db = restart(&fix);
    let conn = open_db(&fix);
    let blobs = blobs(&fix);
    let keep = |bytes: &[u8], job: &str| {
        blobs
            .keep(
                &db,
                Lane::Background,
                "test.keep",
                &[bytes],
                |tx, hashes| {
                    SnapshotStore::new(tx).add_ref(&hashes[0], "preview", job)?;
                    Ok(hashes[0].clone())
                },
            )
            .unwrap()
    };
    let first = keep(b"same-bytes", "job-1");
    let second = keep(b"same-bytes", "job-2");
    assert_eq!(first, second);
    assert_eq!(blobs.get(&conn, &first).unwrap(), b"same-bytes");

    let snapshots = SnapshotStore::new(&conn);
    snapshots.record("old", "b1", "t1", &first, 10, 20).unwrap();
    snapshots
        .record("fresh", "b1", "t2", &first, 40, 130)
        .unwrap();
    assert_eq!(
        publish::snapshots::purge_expired(&restart(&fix), 50).unwrap(),
        1
    );
    assert!(snapshots.get("old").unwrap().is_none());
    assert!(snapshots.get("fresh").unwrap().is_some());

    let baselines = BaselineStore::new(&conn);
    baselines
        .capture("t1", &first, "music", "original/a.flac", 50)
        .unwrap();
    assert!(
        baselines
            .capture("t1", &first, "music", "original/a.flac", 51)
            .is_err()
    );
    assert_eq!(baselines.impact().unwrap(), (1, 1));

    // A copy whose bytes do not match its name is refused, never used;
    // keeping the right bytes again puts them back.
    let path = blobs.dir().file_path(&first).unwrap();
    std::fs::write(&path, b"other-bytes").unwrap();
    assert!(blobs.get(&conn, &first).is_err());
    keep(b"same-bytes", "job-3");
    assert_eq!(blobs.get(&conn, &first).unwrap(), b"same-bytes");

    // The sweep removes only the copies nothing names.
    let loose = undo_put(&fix, b"loose-bytes");
    assert_eq!(blobs.sweep(&db).unwrap(), 1);
    assert!(blobs.get(&conn, &loose).is_err());
    assert_eq!(blobs.get(&conn, &first).unwrap(), b"same-bytes");

    // A copy whose reference is still being written is held, so a sweep
    // in between leaves it.
    let dir = blobs.dir().clone();
    let hold = dir.hold_shared().unwrap();
    let pending = dir.put(&hold, b"pending-bytes").unwrap();
    assert_eq!(blobs.sweep(&db).unwrap(), 0);
    drop(hold);
    assert_eq!(blobs.get(&conn, &pending).unwrap(), b"pending-bytes");
}

/// The one-time move of undo copies out of the database survives a crash
/// at every point it can stop: a file written whose row is still there, a
/// file at its name with the wrong bytes, a file left half written. After
/// the move runs again every copy reads from the folder and its row is
/// gone. A row whose bytes do not match its name is never moved or used.
#[test]
fn a_crash_in_the_move_out_of_the_database_loses_no_undo_copy() {
    let fix = fixture("undo-move");
    let conn = open_db(&fix);
    let blobs = blobs(&fix);
    let dir = blobs.dir().clone();
    let copies: [&[u8]; 4] = [
        b"written-before-the-crash",
        b"not-started",
        b"wrong-bytes-at-its-name",
        b"half-written",
    ];
    let hashes: Vec<String> = copies.iter().map(|bytes| sha256_hex(bytes)).collect();
    for (index, (sha256, bytes)) in hashes.iter().zip(copies).enumerate() {
        conn.execute(
            "INSERT INTO library_publish_blobs (sha256, bytes) VALUES (?1, ?2)",
            rusqlite::params![sha256, bytes],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO library_publish_blob_refs (sha256, owner_kind, owner_id) \
             VALUES (?1, 'baseline', ?2)",
            rusqlite::params![sha256, format!("t{index}")],
        )
        .unwrap();
    }
    let damaged = sha256_hex(b"what-it-should-hold");
    conn.execute(
        "INSERT INTO library_publish_blobs (sha256, bytes) VALUES (?1, ?2)",
        rusqlite::params![damaged, b"what-it-holds".as_slice()],
    )
    .unwrap();
    undo_put(&fix, copies[0]);
    let wrong = dir.file_path(&hashes[2]).unwrap();
    std::fs::create_dir_all(wrong.parent().unwrap()).unwrap();
    std::fs::write(&wrong, b"something else").unwrap();
    let half = dir.file_path(&hashes[3]).unwrap();
    std::fs::create_dir_all(half.parent().unwrap()).unwrap();
    std::fs::write(
        half.with_file_name(format!(".tmp-{}-1-0", hashes[3])),
        b"half",
    )
    .unwrap();

    let db = restart(&fix);
    assert_eq!(blobs.move_out_of_database(&db, &|| true).unwrap(), 4);
    blobs.sweep(&db).unwrap();
    for (sha256, bytes) in hashes.iter().zip(copies) {
        assert_eq!(dir.read(sha256).unwrap(), Stored::Found(bytes.to_vec()));
        assert_eq!(blobs.get(&conn, sha256).unwrap(), bytes);
    }
    let left: Vec<String> = conn
        .prepare("SELECT sha256 FROM library_publish_blobs")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(left, vec![damaged.clone()]);
    assert!(dir.shard(&hashes[3][..2]).unwrap().temps.is_empty());
    assert!(blobs.get(&conn, &damaged).is_err());
}

// --- Publish hardening briefs: seal coverage, gate strictness, 4xx path
// hygiene, symlink refusal, resume backups, MP3 frame survival. ---

/// An item that moves a library track but was never sealed against its
/// rows is refused before anything is recorded or staged.
#[test]
fn seal_refuses_an_unsealed_item() {
    let fix = fixture("unsealed-item");
    let (mut bundle, docs) = two_track_bundle(&fix, PlanKind::Move);
    bundle.items[0].sealed = None;
    let (sealed, live) = seal(&bundle);
    let mut publisher = open_publisher(&fix);
    let err = publisher.publish(&sealed, &live, &docs).unwrap_err();
    assert!(matches!(err, PublishError::Validation(_)), "got {err:?}");
    let conn = open_db(&fix);
    assert!(
        JournalStore::new(&conn)
            .bundle("bundle-1")
            .unwrap()
            .is_empty()
    );
    let operations: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM library_publish_operations WHERE bundle_id = 'bundle-1'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(operations, 0);
}

#[test]
fn seal_rejects_stale_override() {
    let fix = fixture("stale-override");
    let (bundle, docs) = two_track_bundle(&fix, PlanKind::Move);
    let (sealed, mut live) = seal(&bundle);
    live.overrides.insert("track-a".to_string(), 99);
    let mut publisher = open_publisher(&fix);
    let err = publisher.publish(&sealed, &live, &docs).unwrap_err();
    assert!(matches!(err, PublishError::Validation(_)), "got {err:?}");
    match sealed.recheck(&live) {
        Err(SealError::StaleProfile(text)) => assert!(text.contains("override"), "{text}"),
        other => panic!("got {other:?}"),
    }
}

#[test]
fn seal_rejects_stale_naming() {
    let fix = fixture("stale-naming");
    let (bundle, docs) = two_track_bundle(&fix, PlanKind::Move);
    let (sealed, mut live) = seal(&bundle);
    live.naming_revision = 8;
    let mut publisher = open_publisher(&fix);
    let err = publisher.publish(&sealed, &live, &docs).unwrap_err();
    assert!(matches!(err, PublishError::Validation(_)), "got {err:?}");
    assert!(matches!(
        sealed.recheck(&live),
        Err(SealError::StaleProfile(_))
    ));
}

#[test]
fn capability_gate_rejects_wma_and_unhandled_capabilities() {
    let gate = CapabilityGate::production();
    let fp = FileFingerprint {
        size: 1,
        sha256: "x".to_string(),
    };
    // WMA has no reader, so no plan can name it: a stored one that does
    // never decodes.
    let mut stored = serde_json::to_value(plan_item(
        "t",
        "s.flac",
        "d.flac",
        fp.clone(),
        PlanKind::SamePath,
    ))
    .unwrap();
    stored["format"] = "wma".into();
    assert!(serde_json::from_value::<PlanItem>(stored).is_err());
    // Raw AAC has no artwork path.
    let mut aac_art = plan_item("t", "s.flac", "d.flac", fp.clone(), PlanKind::SamePath);
    aac_art.format = AudioFormat::Aac;
    aac_art.capabilities = vec![Capability::Artwork];
    let err = gate.check(&aac_art).unwrap_err();
    assert!(matches!(err, PublishError::Capability(_)), "got {err:?}");
    assert!(err.to_string().contains("aac"), "{err}");
    // Capabilities staging cannot express fail loudly, never as a
    // silent half-manage downstream.
    for capability in [Capability::Genre, Capability::Rename, Capability::Scrub] {
        let mut item = plan_item("t", "s.flac", "d.flac", fp.clone(), PlanKind::SamePath);
        item.capabilities = vec![capability];
        let err = gate.check(&item).unwrap_err();
        assert!(matches!(err, PublishError::Capability(_)), "got {err:?}");
        assert!(
            err.to_string().contains(capability.as_str()),
            "{capability:?}: {err}"
        );
    }
    // The wired surface still passes: metadata plus moves.
    let item = plan_item("t", "s.flac", "d.flac", fp, PlanKind::SamePath);
    gate.check(&item).unwrap();
}

#[test]
fn error_messages_name_files_not_server_paths() {
    let fix = fixture("m1-paths");
    let root_text = fix.dir.to_string_lossy().into_owned();
    // Sandbox escape: no path at all.
    let outside = fix.dir.join("elsewhere");
    let err = fix.sandbox.ensure_under_roots(&outside).unwrap_err();
    assert!(!err.to_string().contains(&root_text), "{err}");
    // Missing and symlinked files: file name only.
    let missing = fix.sandbox.resolve("music", "gone.flac").unwrap();
    let err = publish::paths::hash_regular_file(&missing).unwrap_err();
    assert!(matches!(err, PublishError::Validation(_)), "got {err:?}");
    assert!(err.to_string().contains("gone.flac"), "{err}");
    assert!(!err.to_string().contains(&root_text), "{err}");
    #[cfg(unix)]
    {
        let target = fix.dir.join("real.bin");
        std::fs::write(&target, b"real").unwrap();
        let link = fix.sandbox.resolve("music", "alias.flac").unwrap();
        std::os::unix::fs::symlink(&target, &link).unwrap();
        let err = publish::paths::hash_regular_file(&link).unwrap_err();
        assert!(matches!(err, PublishError::UnsafePath(_)), "got {err:?}");
        assert!(err.to_string().contains("alias.flac"), "{err}");
        assert!(!err.to_string().contains(&root_text), "{err}");
    }
    // Tag staging on garbage bytes: the hidden temp path stays home.
    let mut updates = TagUpdates::new();
    updates.insert(TagTarget::Field(TagField::Title), vec!["X".to_string()]);
    let temp = fix.dir.join("w.tmp.flac");
    std::fs::write(&temp, [0u8; 64]).unwrap();
    let err = publish::staging::stage_tags(&temp, &updates, None).unwrap_err();
    assert!(matches!(err, PublishError::Validation(_)), "got {err:?}");
    assert!(!err.to_string().contains(&root_text), "{err}");
    // Reading the current tag document off garbage: file name only.
    let garbage = fix.sandbox.resolve("music", "garbage.flac").unwrap();
    std::fs::write(&garbage, [0u8; 64]).unwrap();
    let err = publish::staging::document_from_file(&garbage).unwrap_err();
    assert!(!err.to_string().contains(&root_text), "{err}");
    assert!(err.to_string().contains("garbage.flac"), "{err}");
}

#[cfg(unix)]
#[test]
fn collision_gate_reports_symlink_dest_as_root_and_rel() {
    let fix = fixture("m1-symlink");
    let dest = fix.sandbox.resolve("music", "organized/a.flac").unwrap();
    std::fs::create_dir_all(dest.parent().unwrap()).unwrap();
    std::os::unix::fs::symlink("target", &dest).unwrap();
    let (bundle, _) = two_track_bundle(&fix, PlanKind::Move);
    let err = CollisionGate::check_bundle(&fix.sandbox, &bundle).unwrap_err();
    assert!(matches!(err, PublishError::UnsafePath(_)), "got {err:?}");
    let root_text = fix.dir.to_string_lossy().into_owned();
    assert!(!err.to_string().contains(&root_text), "{err}");
}

#[cfg(unix)]
#[test]
fn resolve_no_symlink_refuses_linked_components() {
    let fix = fixture("m5-resolve");
    let target = fix.dir.join("foreign");
    std::fs::create_dir_all(&target).unwrap();
    std::fs::write(target.join("secret.flac"), b"secret").unwrap();
    let link = fix.sandbox.resolve("music", "linked").unwrap();
    std::os::unix::fs::symlink(&target, &link).unwrap();
    // Lexical resolve succeeds, proving the bypass plain resolve()
    // allows; every mutation-time site must use the refusing form.
    assert!(fix.sandbox.resolve("music", "linked/secret.flac").is_ok());
    assert!(
        fix.sandbox
            .resolve_no_symlink("music", "linked/secret.flac")
            .is_err()
    );
    let file_link = fix.sandbox.resolve("music", "alias.flac").unwrap();
    std::os::unix::fs::symlink(target.join("secret.flac"), &file_link).unwrap();
    assert!(
        fix.sandbox
            .resolve_no_symlink("music", "alias.flac")
            .is_err()
    );
}

/// A library folder may be reached through a link, at the folder or
/// above it, and below it no link is followed. A folder that is a link to
/// nothing refuses, and so does a path outside its folder. A remove-only
/// folder reached through a link keeps its plain `.recycle` as a bin.
#[cfg(unix)]
#[test]
fn a_library_folder_may_be_reached_through_a_link() {
    use std::os::unix::fs::symlink;
    let scratch = ScratchDir::new("publish-linked-root");
    let dir = scratch.to_path_buf();
    let real = dir.join("real");
    for folder in ["music", "kept", "foreign"] {
        std::fs::create_dir_all(real.join(folder)).unwrap();
    }
    std::fs::write(real.join("foreign/secret.flac"), b"secret").unwrap();
    symlink(&real, dir.join("parent")).unwrap();
    symlink(real.join("music"), dir.join("alias")).unwrap();
    symlink(real.join("foreign"), real.join("music/linked")).unwrap();
    symlink(
        real.join("foreign/secret.flac"),
        real.join("music/alias.flac"),
    )
    .unwrap();
    let kept = dir.join("parent/kept");
    for root in [dir.join("parent/music"), dir.join("alias")] {
        let sandbox = publish::Sandbox::new(vec![
            Root::library("music", &root),
            Root::remove_only("kept", &kept),
            Root::recycle_of("kept", &kept),
        ])
        .unwrap();
        assert_eq!(
            sandbox.resolve_no_symlink("music", "Band/01.flac").unwrap(),
            root.join("Band/01.flac")
        );
        for rel in ["linked/secret.flac", "alias.flac"] {
            assert!(sandbox.resolve_no_symlink("music", rel).is_err(), "{rel}");
            assert!(sandbox.resolve_removal("music", rel).is_err(), "{rel}");
        }
        let entry = kept.join(".recycle/20260101T000000-0a1b2c3d/01.flac");
        assert_eq!(
            sandbox.ensure_under_bin(&entry).unwrap().id,
            ".recycle:kept",
            "the remove-only folder's bin"
        );
    }

    symlink(dir.join("nowhere"), dir.join("dangling")).unwrap();
    let dangling =
        publish::Sandbox::new(vec![Root::library("music", dir.join("dangling"))]).unwrap();
    assert!(
        dangling
            .resolve_no_symlink("music", "Band/01.flac")
            .is_err()
    );
    let music = real.join("music");
    for outside in [music.join("../foreign/secret.flac"), real.join("foreign")] {
        assert!(
            publish::paths::ensure_no_symlink_below(&music, &outside).is_err(),
            "{outside:?}"
        );
    }
}

#[cfg(unix)]
#[test]
fn symlink_at_dest_flags_attention_without_touching_target() {
    let fix = fixture("m5-symlink-dest");
    let (bundle, docs) = two_track_bundle(&fix, PlanKind::Move);
    let (sealed, live) = seal(&bundle);
    let mut publisher = open_publisher(&fix);
    publisher.set_crash_point(Some(CrashPoint::AfterStage));
    assert!(matches!(
        publisher.publish(&sealed, &live, &docs),
        Err(PublishError::InjectedCrash(_))
    ));
    drop(publisher);

    let dest = fix.sandbox.resolve("music", "organized/a.flac").unwrap();
    std::fs::create_dir_all(dest.parent().unwrap()).unwrap();
    let canary = fix.dir.join("canary.bin");
    std::fs::write(&canary, b"canary").unwrap();
    std::os::unix::fs::symlink(&canary, &dest).unwrap();

    let db = restart(&fix);

    let conn = open_db(&fix);
    let results = reconcile(&db, &fix.sandbox).unwrap();
    assert_eq!(results.len(), 1);
    assert!(
        matches!(
            results[0].action,
            publish::RecoveryAction::NeedsAttention(_)
        ),
        "got {:?}",
        results[0].action
    );
    assert_eq!(std::fs::read(&canary).unwrap(), b"canary");
    assert_eq!(read_revision(&conn).unwrap(), start_revision());
}

#[cfg(unix)]
#[test]
fn committed_dest_swapped_for_symlink_refuses_without_touching_target() {
    // Cleanup hardening: a committed destination swapped for a
    // symlink must fail closed. Reconcile refuses through the
    // symlink-refusing resolver, the link target keeps its bytes,
    // the uncleaned sources survive, and restoring the real output
    // lets the next pass finish cleanup.
    let fix = fixture("m5-cleanup-symlink");
    let (bundle, docs) = two_track_bundle(&fix, PlanKind::Move);
    let (sealed, live) = seal(&bundle);
    let mut publisher = open_publisher(&fix);
    publisher.set_crash_point(Some(CrashPoint::AfterCatalogCommit));
    assert!(matches!(
        publisher.publish(&sealed, &live, &docs),
        Err(PublishError::InjectedCrash(_))
    ));
    drop(publisher);

    let dest = fix.sandbox.resolve("music", "organized/a.flac").unwrap();
    let staged_bytes = std::fs::read(&dest).unwrap();
    std::fs::remove_file(&dest).unwrap();
    let canary = fix.dir.join("canary.bin");
    std::fs::write(&canary, b"canary").unwrap();
    std::os::unix::fs::symlink(&canary, &dest).unwrap();

    let db = restart(&fix);

    let results = reconcile(&db, &fix.sandbox).unwrap();
    assert!(
        matches!(&results[0].action, publish::RecoveryAction::Deferred(reason) if reason.contains("symlink")),
        "got {results:?}"
    );
    assert_eq!(std::fs::read(&canary).unwrap(), b"canary");
    let source = fix.sandbox.resolve("music", "staging/a.flac").unwrap();
    assert!(source.is_file(), "source was cleaned through a symlink");

    std::fs::remove_file(&dest).unwrap();
    std::fs::write(&dest, &staged_bytes).unwrap();
    let results = reconcile(&db, &fix.sandbox).unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].action, publish::RecoveryAction::CleanupFinished);
}

/// A crash after the backup path is journaled but before the original
/// moves there resumes with the original intact and the bundle whole.
#[test]
fn crash_after_backup_journaled_resumes() {
    let fix = fixture("crash-backup-journaled");
    let (bundle, docs) = two_track_bundle(&fix, PlanKind::SamePath);
    let (sealed, live) = seal(&bundle);
    let mut publisher = open_publisher(&fix);
    publisher.set_crash_point(Some(CrashPoint::AfterBackupJournaled));
    assert!(matches!(
        publisher.publish(&sealed, &live, &docs),
        Err(PublishError::InjectedCrash(_))
    ));
    drop(publisher);

    let db = restart(&fix);

    let conn = open_db(&fix);
    let journaled = JournalStore::new(&conn).bundle("bundle-1").unwrap();
    assert!(journaled.iter().any(|journal| journal.backup.is_some()));
    let results = reconcile(&db, &fix.sandbox).unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].action, publish::RecoveryAction::ResumedCommitted);
    for item in bundle.items.iter() {
        let dest = fix
            .sandbox
            .resolve(&item.dest_root, &item.dest_rel)
            .unwrap();
        assert_staged(&dest, &item.track_id);
    }
    assert!(hidden_leftovers(&fix.dir).is_empty());
    let journals = JournalStore::new(&conn).bundle("bundle-1").unwrap();
    assert!(
        journals
            .iter()
            .all(|journal| journal.state == JournalState::Cleaned)
    );
    assert_eq!(read_revision(&conn).unwrap(), start_revision() + 1);
}

#[test]
fn same_path_after_stage_resume_retains_backup() {
    let fix = fixture("resume-backup");
    let (bundle, docs) = two_track_bundle(&fix, PlanKind::SamePath);
    let (sealed, live) = seal(&bundle);
    let mut publisher = open_publisher(&fix);
    publisher.set_crash_point(Some(CrashPoint::AfterStage));
    assert!(matches!(
        publisher.publish(&sealed, &live, &docs),
        Err(PublishError::InjectedCrash(_))
    ));
    drop(publisher);

    // No backup could exist yet: publish never reached its first rename.
    let db = restart(&fix);
    let conn = open_db(&fix);
    let before = JournalStore::new(&conn).bundle("bundle-1").unwrap();
    assert_eq!(before.len(), 2);
    assert!(before.iter().all(|journal| journal.backup.is_none()));

    let results = reconcile(&db, &fix.sandbox).unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].action, publish::RecoveryAction::ResumedCommitted);
    for item in bundle.items.iter() {
        let dest = fix
            .sandbox
            .resolve(&item.dest_root, &item.dest_rel)
            .unwrap();
        assert_staged(&dest, &item.track_id);
    }
    assert!(hidden_leftovers(&fix.dir).is_empty());
    let journals = JournalStore::new(&conn).bundle("bundle-1").unwrap();
    assert_eq!(journals.len(), 2);
    for journal in journals.iter() {
        assert_eq!(journal.state, JournalState::Cleaned);
        // Only the resume-time backup rename could have recorded this.
        assert!(
            journal.backup.is_some(),
            "journal {} lost its backup",
            journal.id
        );
    }
    assert_eq!(read_revision(&conn).unwrap(), start_revision() + 1);
}

/// One same-path retag of `rel`, which holds the committed FLAC fixture.
fn retag_bundle(
    fix: &Fixture,
    id: &str,
    rel: &str,
    title: &str,
) -> (PlanBundle, BTreeMap<String, TagDocument>) {
    let bytes = std::fs::read(fix.sandbox.resolve("music", rel).unwrap()).unwrap();
    let fingerprint = FileFingerprint {
        size: bytes.len() as u64,
        sha256: sha256_hex(&bytes),
    };
    let mut item = plan_item("track-s", rel, rel, fingerprint, PlanKind::SamePath);
    item.managed_updates
        .insert(TagTarget::Field(TagField::Title), vec![title.to_string()]);
    let bundle = PlanBundle {
        id: id.to_string(),
        items: vec![item],
        profile_revision: 7,
        naming_revision: 3,
        policy_revision: 5,
        put_back: None,
        added: None,
        removal: None,
    };
    let mut docs = BTreeMap::new();
    docs.insert("track-s".to_string(), TagDocument::empty());
    (bundle, docs)
}

/// A file name close to the 255-byte limit leaves no room for a hidden
/// name built around it: the track is still published and retagged.
#[test]
fn a_240_byte_file_name_is_published_and_retagged() {
    let fix = fixture("long-name");
    // Two-byte letters after one plain one: the cut falls inside a letter.
    let long_rel = format!("organized/n{}.flac", "\u{e9}".repeat(117));
    assert_eq!(long_rel.len() - "organized/".len(), 240);
    let fp = write_fixture_source(&fix, "music", "staging/s.flac", "management_full.flac");
    let item = plan_item("track-s", "staging/s.flac", &long_rel, fp, PlanKind::Move);
    let bundle = PlanBundle {
        id: "bundle-long-move".to_string(),
        items: vec![item],
        profile_revision: 7,
        naming_revision: 3,
        policy_revision: 5,
        put_back: None,
        added: None,
        removal: None,
    };
    let mut docs = BTreeMap::new();
    docs.insert("track-s".to_string(), TagDocument::empty());
    let (sealed, live) = seal(&bundle);
    let mut publisher = open_publisher(&fix);
    publisher.publish(&sealed, &live, &docs).unwrap();

    let (bundle, docs) = retag_bundle(&fix, "bundle-long-retag", &long_rel, "Retagged");
    let (sealed, live) = seal(&bundle);
    publisher.publish(&sealed, &live, &docs).unwrap();

    let dest = fix.sandbox.resolve("music", &long_rel).unwrap();
    let tag = tags::read::read_tag_only(&dest, tags::AudioFormat::Flac).unwrap();
    assert_eq!(tag.title, "Retagged");
    tags::probe(&dest).unwrap();
    assert!(hidden_leftovers(&fix.dir).is_empty());

    // The longest hidden name is a tag write's temp, which ends in the
    // container's extension. It keeps the start of the file's name and
    // stays short.
    let temp = fix
        .sandbox
        .audio_temp_path_for(&dest, "bundle-long-retag-000-audio", "flac")
        .unwrap();
    let temp = temp.file_name().unwrap().to_string_lossy().into_owned();
    assert!(temp.contains(".n\u{e9}"), "{temp}");
    assert!(temp.len() <= 128, "{} bytes", temp.len());
}

/// A hidden working name that someone turned into a symlink is unlinked,
/// never written through: the files the links point at keep their bytes.
#[test]
fn a_symlink_at_a_hidden_working_name_is_never_written_through() {
    let fix = fixture("temp-symlink");
    write_fixture_source(&fix, "music", "staging/s.flac", "management_full.flac");
    let (bundle, docs) = retag_bundle(&fix, "bundle-link", "staging/s.flac", "Retagged");
    let dest = fix.sandbox.resolve("music", "staging/s.flac").unwrap();
    let temp = fix
        .sandbox
        .audio_temp_path_for(&dest, "bundle-link-000-audio", "flac")
        .unwrap();
    let victim = fix.dir.join("victim-temp");
    std::fs::write(&victim, b"theirs").unwrap();
    std::os::unix::fs::symlink(&victim, &temp).unwrap();
    let (sealed, live) = seal(&bundle);
    let mut publisher = open_publisher(&fix);
    publisher.publish(&sealed, &live, &docs).unwrap();

    assert_eq!(std::fs::read(&victim).unwrap(), b"theirs");

    let tag = tags::read::read_tag_only(&dest, tags::AudioFormat::Flac).unwrap();
    assert_eq!(tag.title, "Retagged");
    assert!(hidden_leftovers(&fix.dir).is_empty());
}

/// A retag keeps the permission bits of the file it replaces. Two modes,
/// so one of them is never what a new file gets by default here.
#[test]
fn a_retag_keeps_the_files_permission_bits() {
    use std::os::unix::fs::PermissionsExt as _;
    let fix = fixture("keep-mode");
    write_fixture_source(&fix, "music", "staging/s.flac", "management_full.flac");
    let dest = fix.sandbox.resolve("music", "staging/s.flac").unwrap();
    let mut publisher = open_publisher(&fix);
    for bits in [0o664, 0o640] {
        std::fs::set_permissions(&dest, std::fs::Permissions::from_mode(bits)).unwrap();
        let title = format!("Retagged {bits:o}");
        let (bundle, docs) = retag_bundle(
            &fix,
            &format!("bundle-mode-{bits:o}"),
            "staging/s.flac",
            &title,
        );
        let (sealed, live) = seal(&bundle);
        publisher.publish(&sealed, &live, &docs).unwrap();

        let tag = tags::read::read_tag_only(&dest, tags::AudioFormat::Flac).unwrap();
        assert_eq!(tag.title, title);
        let mode = std::fs::metadata(&dest).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, bits, "{bits:o}");
    }
}

/// A bundle journaled before hidden names changed still resumes: recovery
/// keeps the backup name its row holds, so a link an interrupted put-back
/// left under that name is recognised and removed, not stranded.
#[test]
fn rows_with_old_style_hidden_names_resume() {
    let fix = fixture("old-names");
    let (bundle, docs) = two_track_bundle(&fix, PlanKind::SamePath);
    let (sealed, live) = seal(&bundle);
    let mut publisher = open_publisher(&fix);
    publisher.set_crash_point(Some(CrashPoint::AfterBackupJournaled));
    assert!(matches!(
        publisher.publish(&sealed, &live, &docs),
        Err(PublishError::InjectedCrash(_))
    ));
    drop(publisher);

    let db = restart(&fix);

    let conn = open_db(&fix);
    let mut backups = 0;
    for journal in JournalStore::new(&conn).bundle("bundle-1").unwrap() {
        let dest = fix
            .sandbox
            .resolve(&journal.dest_root, &journal.dest_rel)
            .unwrap();
        let old_name = |ending: &str| {
            dest.with_file_name(format!(
                "{}{}.{}.{ending}",
                publish::HIDDEN_PREFIX,
                journal.id,
                dest.file_name().unwrap().to_string_lossy()
            ))
        };
        let staged = old_name("tmp");
        std::fs::rename(&journal.staged, &staged).unwrap();
        conn.execute(
            "UPDATE library_publish_journal SET staged = ?1 WHERE id = ?2",
            rusqlite::params![staged.to_string_lossy(), journal.id],
        )
        .unwrap();
        if journal.backup.is_some() {
            let backup = old_name("bak");
            std::fs::hard_link(&dest, &backup).unwrap();
            conn.execute(
                "UPDATE library_publish_journal SET backup = ?1 WHERE id = ?2",
                rusqlite::params![backup.to_string_lossy(), journal.id],
            )
            .unwrap();
            backups += 1;
        }
    }
    assert_eq!(backups, 1);
    let results = reconcile(&db, &fix.sandbox).unwrap();
    assert_eq!(results[0].action, publish::RecoveryAction::ResumedCommitted);
    for item in bundle.items.iter() {
        let dest = fix
            .sandbox
            .resolve(&item.dest_root, &item.dest_rel)
            .unwrap();
        assert_staged(&dest, &item.track_id);
    }
    assert!(hidden_leftovers(&fix.dir).is_empty());
}

/// Splice raw ID3v2.4 frames ahead of the existing tag, fixing the
/// header size. Mirrors the tags-slice helper: lofty's own save
/// cannot plant unknown frames while upstream #732 is open.
fn id3_inject_frames(path: &std::path::Path, frames: &[(&str, &[u8])]) {
    let bytes = std::fs::read(path).unwrap();
    assert_eq!(&bytes[..3], b"ID3");
    assert_eq!(bytes[3], 4);
    let size = bytes[6..10]
        .iter()
        .fold(0usize, |acc, byte| (acc << 7) | usize::from(byte & 0x7F));
    let mut injected = Vec::new();
    for (id, payload) in frames {
        injected.extend_from_slice(id.as_bytes());
        let len = payload.len();
        injected.extend_from_slice(&[
            ((len >> 21) & 0x7F) as u8,
            ((len >> 14) & 0x7F) as u8,
            ((len >> 7) & 0x7F) as u8,
            (len & 0x7F) as u8,
        ]);
        injected.extend_from_slice(&[0, 0]);
        injected.extend_from_slice(payload);
    }
    let new_size = size + injected.len();
    let mut out = bytes[..6].to_vec();
    out.extend_from_slice(&[
        ((new_size >> 21) & 0x7F) as u8,
        ((new_size >> 14) & 0x7F) as u8,
        ((new_size >> 7) & 0x7F) as u8,
        (new_size & 0x7F) as u8,
    ]);
    out.extend_from_slice(&injected);
    out.extend_from_slice(&bytes[10..]);
    std::fs::write(path, out).unwrap();
}

#[test]
fn mp3_unknown_frames_survive_publish_staging() {
    let fix = fixture("preserve-mp3");
    open_db(&fix)
        .execute(
            "UPDATE local_tracks SET relative_path = 'staging/s.mp3' WHERE id = 'track-s'",
            [],
        )
        .unwrap();
    let source = fix.sandbox.resolve("music", "staging/s.mp3").unwrap();
    std::fs::create_dir_all(source.parent().unwrap()).unwrap();
    std::fs::copy(fixtures_dir().join("management_full.mp3"), &source).unwrap();
    let mut wxxx = vec![3u8];
    wxxx.extend_from_slice(b"ZZZ_LINK\0https://example.invalid/publish");
    let mut priv_frame = b"com.example.publish\0".to_vec();
    priv_frame.extend_from_slice(b"opaque-publish");
    id3_inject_frames(&source, &[("PRIV", &priv_frame), ("WXXX", &wxxx)]);
    let bytes = std::fs::read(&source).unwrap();
    let fp = FileFingerprint {
        size: bytes.len() as u64,
        sha256: sha256_hex(&bytes),
    };
    let mut item = plan_item(
        "track-s",
        "staging/s.mp3",
        "staging/s.mp3",
        fp,
        PlanKind::SamePath,
    );
    item.format = AudioFormat::Mp3;
    let bundle = PlanBundle {
        id: "bundle-mp3".to_string(),
        items: vec![item],
        profile_revision: 7,
        naming_revision: 3,
        policy_revision: 5,
        put_back: None,
        added: None,
        removal: None,
    };
    let mut docs = BTreeMap::new();
    docs.insert("track-s".to_string(), TagDocument::empty());
    let (sealed, live) = seal(&bundle);
    let mut publisher = open_publisher(&fix);
    publisher.publish(&sealed, &live, &docs).unwrap();

    let dest = fix.sandbox.resolve("music", "staging/s.mp3").unwrap();
    let tag = tags::read::read_tag_only(&dest, tags::AudioFormat::Mp3).unwrap();
    assert_eq!(tag.title, "Title track-s");
    let saved = std::fs::read(&dest).unwrap();
    for needle in [
        b"opaque-publish".as_slice(),
        b"https://example.invalid/publish".as_slice(),
        b"com.example.publish".as_slice(),
        b"ZZZ_LINK".as_slice(),
    ] {
        assert!(
            saved.windows(needle.len()).any(|window| window == needle),
            "missing {needle:?}"
        );
    }
    tags::probe(&dest).unwrap();
}

// --- Recycle-bin expiry briefs: only a copy a row names, as it was binned,
// in a bin set now, with nothing holding it, is ever deleted. ---

/// The expiry passes' clock: a fixed point well after every fixture.
const EXPIRY_NOW_MS: i64 = 4_000_000_000_000;
const RETENTION_DAYS: i64 = 30;
/// A copy recycled 31 days before [`EXPIRY_NOW_MS`], due to expire.
const RECYCLED_OLD: f64 = (EXPIRY_NOW_MS / 1000 - 31 * 86_400) as f64;
/// A copy recycled 29 days before it, still kept.
const RECYCLED_YOUNG: f64 = (EXPIRY_NOW_MS / 1000 - 29 * 86_400) as f64;

/// Remove `track-a` and `track-b` into the fixture's bin, and answer each
/// track's copy there.
fn recycle_two(fix: &Fixture) -> BTreeMap<String, PathBuf> {
    write_source(fix, "music", "staging/a.flac", b"first removed file");
    write_source(fix, "music", "staging/b.flac", b"second removed file");
    let bundle = removal_bundle(
        fix,
        &fix.sandbox,
        "music",
        &[("track-a", "staging/a.flac"), ("track-b", "staging/b.flac")],
    );
    let (sealed, live) = seal(&bundle);
    let outcome = open_publisher(fix)
        .publish(&sealed, &live, &BTreeMap::new())
        .unwrap();
    assert_eq!(outcome, PublishOutcome::Committed);
    let conn = open_db(fix);
    let mut statement = conn
        .prepare("SELECT replaced_track_id, recycle_to FROM library_publish_replacements")
        .unwrap();
    let copies: BTreeMap<String, PathBuf> = statement
        .query_map([], |row| {
            Ok((row.get(0)?, PathBuf::from(row.get::<_, String>(1)?)))
        })
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert_eq!(copies.len(), 2);
    assert!(copies.values().all(|copy| copy.is_file()));
    copies
}

/// Say `track`'s copy reached the bin at `secs`.
fn recycled_at(fix: &Fixture, track: &str, secs: f64) {
    open_db(fix)
        .execute(
            "UPDATE library_publish_replacements SET recycled_at = ?2 \
             WHERE replaced_track_id = ?1",
            rusqlite::params![track, secs],
        )
        .unwrap();
}

/// When `track`'s copy was expired, if it was.
fn expired_at(fix: &Fixture, track: &str) -> Option<i64> {
    open_db(fix)
        .query_row(
            "SELECT expired_at FROM library_publish_replacements WHERE replaced_track_id = ?1",
            [track],
            |row| row.get(0),
        )
        .unwrap()
}

/// One expiry pass over `sandbox`'s bins, reading `page` rows at a time.
fn run_expiry(fix: &Fixture, sandbox: &Sandbox, page: usize) -> publish::expiry::ExpiryReport {
    publish::expiry::expire(
        &restart(fix),
        &publish::expiry::ExpiryPass {
            retention_days: RETENTION_DAYS,
            now_ms: EXPIRY_NOW_MS,
            page,
            due_before_ms: None,
        },
        &publish::expiry::ExpiryMemory::default(),
        &|| Some(sandbox.clone()),
        || (),
        &|| true,
    )
    .unwrap()
}

/// A copy past its retention goes with its entry folder and its row is
/// marked; one inside it stays. A copy someone deleted is marked, as long
/// as its entry folder is still there.
#[test]
fn an_expired_copy_goes_and_a_young_one_stays() {
    let fix = fixture("expiry-age");
    let copies = recycle_two(&fix);
    recycled_at(&fix, "track-a", RECYCLED_OLD);
    recycled_at(&fix, "track-b", RECYCLED_YOUNG);

    let report = run_expiry(&fix, &fix.sandbox, 200);

    assert_eq!(report.deleted, 1);
    assert!(!copies["track-a"].parent().unwrap().exists());
    assert_eq!(expired_at(&fix, "track-a"), Some(EXPIRY_NOW_MS));
    assert!(copies["track-b"].is_file());
    assert_eq!(expired_at(&fix, "track-b"), None);

    std::fs::remove_file(&copies["track-b"]).unwrap();
    recycled_at(&fix, "track-b", RECYCLED_OLD);
    let report = run_expiry(&fix, &fix.sandbox, 200);
    assert_eq!(report.gone, 1);
    assert_eq!(expired_at(&fix, "track-b"), Some(EXPIRY_NOW_MS));
    assert!(!copies["track-b"].parent().unwrap().exists());
}

/// An empty bin folder may be a share that is not mounted: a copy whose
/// entry folder is gone counts as gone only while the bin holds another
/// entry. Until then its row is left for a later pass.
#[test]
fn a_copy_counts_as_gone_only_beside_other_entries() {
    let fix = fixture("expiry-unmounted");
    let copies = recycle_two(&fix);
    recycled_at(&fix, "track-a", RECYCLED_OLD);
    recycled_at(&fix, "track-b", RECYCLED_OLD);
    for copy in copies.values() {
        std::fs::remove_dir_all(copy.parent().unwrap()).unwrap();
    }

    let report = run_expiry(&fix, &fix.sandbox, 200);
    assert_eq!(report.waited, 2);
    assert_eq!(expired_at(&fix, "track-a"), None);
    assert_eq!(expired_at(&fix, "track-b"), None);

    std::fs::create_dir_all(fix.dir.join("bin/29990101T000000-0a0b0c0d")).unwrap();
    run_expiry(&fix, &fix.sandbox, 200);
    assert_eq!(expired_at(&fix, "track-a"), Some(EXPIRY_NOW_MS));
    assert_eq!(expired_at(&fix, "track-b"), Some(EXPIRY_NOW_MS));
}

/// The bin is looked at again under the lock: one that empties between the
/// first look and the lock (a share dropping out) proves nothing gone.
#[test]
fn a_bin_that_empties_before_the_lock_marks_nothing_gone() {
    let fix = fixture("expiry-unmounted-late");
    let copies = recycle_two(&fix);
    recycled_at(&fix, "track-a", RECYCLED_OLD);
    recycled_at(&fix, "track-b", RECYCLED_YOUNG);
    std::fs::remove_dir_all(copies["track-a"].parent().unwrap()).unwrap();
    let other = copies["track-b"].parent().unwrap().to_path_buf();
    let aside = fix.dir.join("aside");

    let report = publish::expiry::expire(
        &restart(&fix),
        &publish::expiry::ExpiryPass {
            retention_days: RETENTION_DAYS,
            now_ms: EXPIRY_NOW_MS,
            page: 200,
            due_before_ms: None,
        },
        &publish::expiry::ExpiryMemory::default(),
        &|| Some(fix.sandbox.clone()),
        || {
            let _ = std::fs::rename(&other, &aside);
        },
        &|| true,
    )
    .unwrap();

    assert_eq!(report.waited, 1);
    assert_eq!(report.gone, 0);
    assert_eq!(expired_at(&fix, "track-a"), None);
}

/// A pass that deletes the last other entry in a bin still marks a copy
/// it found gone there: what proves a copy gone is read before the pass
/// deletes anything.
#[test]
fn emptying_the_bin_marks_a_gone_copy_beside_the_one_it_deletes() {
    let fix = fixture("expiry-gone-and-due");
    let copies = recycle_two(&fix);
    recycled_at(&fix, "track-a", RECYCLED_OLD);
    recycled_at(&fix, "track-b", RECYCLED_OLD + 10.0);
    std::fs::remove_dir_all(copies["track-b"].parent().unwrap()).unwrap();

    let report = publish::expiry::expire(
        &restart(&fix),
        &publish::expiry::ExpiryPass {
            retention_days: 0,
            now_ms: EXPIRY_NOW_MS,
            page: 200,
            due_before_ms: Some(EXPIRY_NOW_MS),
        },
        &publish::expiry::ExpiryMemory::default(),
        &|| Some(fix.sandbox.clone()),
        || (),
        &|| true,
    )
    .unwrap();

    assert_eq!((report.deleted, report.gone), (1, 1));
    assert!(!copies["track-a"].exists());
    assert_eq!(expired_at(&fix, "track-a"), Some(EXPIRY_NOW_MS));
    assert_eq!(expired_at(&fix, "track-b"), Some(EXPIRY_NOW_MS));
}

/// A copy whose bytes in the bin are no longer the ones recycled is not
/// deleted, however old, and its row stays unmarked.
#[test]
fn an_expired_copy_whose_bytes_changed_is_kept() {
    let fix = fixture("expiry-changed");
    let copies = recycle_two(&fix);
    recycled_at(&fix, "track-a", RECYCLED_OLD);
    recycled_at(&fix, "track-b", RECYCLED_OLD);
    std::fs::write(&copies["track-a"], b"FIRST REMOVED FILE").unwrap();

    let report = run_expiry(&fix, &fix.sandbox, 200);

    assert_eq!(report.changed, 1);
    assert_eq!(
        std::fs::read(&copies["track-a"]).unwrap(),
        b"FIRST REMOVED FILE"
    );
    assert_eq!(expired_at(&fix, "track-a"), None);
    assert!(!copies["track-b"].exists());
}

/// A copy in a bin that is no longer set is kept, and does not hold up a
/// due copy behind it, however small the page. A copy reached through a
/// link below the bin is kept, and what the link leads to is untouched.
#[test]
fn copies_outside_the_current_bins_or_below_a_link_are_kept() {
    let fix = fixture("expiry-old-bin");
    let copies = recycle_two(&fix);
    let entry = copies["track-a"].parent().unwrap();
    let moved = fix.dir.join("old-bin").join(entry.file_name().unwrap());
    std::fs::create_dir_all(fix.dir.join("old-bin")).unwrap();
    std::fs::rename(entry, &moved).unwrap();
    let moved_copy = moved.join(copies["track-a"].file_name().unwrap());
    open_db(&fix)
        .execute(
            "UPDATE library_publish_replacements SET recycle_to = ?1 \
             WHERE replaced_track_id = 'track-a'",
            [moved_copy.to_string_lossy()],
        )
        .unwrap();
    recycled_at(&fix, "track-a", RECYCLED_OLD - 10.0);
    recycled_at(&fix, "track-b", RECYCLED_OLD);

    run_expiry(&fix, &fix.sandbox, 1);

    assert!(moved_copy.is_file(), "a bin no longer set is left alone");
    assert_eq!(expired_at(&fix, "track-a"), None);
    assert!(!copies["track-b"].exists(), "the due copy behind it goes");

    let fix = fixture("expiry-link");
    let copies = recycle_two(&fix);
    recycled_at(&fix, "track-a", RECYCLED_OLD);
    let entry = copies["track-a"].parent().unwrap();
    let elsewhere = fix.dir.join("elsewhere");
    std::fs::create_dir_all(&elsewhere).unwrap();
    let target = elsewhere.join(copies["track-a"].file_name().unwrap());
    std::fs::rename(&copies["track-a"], &target).unwrap();
    std::fs::remove_dir(entry).unwrap();
    std::os::unix::fs::symlink(&elsewhere, entry).unwrap();

    run_expiry(&fix, &fix.sandbox, 200);

    assert_eq!(std::fs::read(&target).unwrap(), b"first removed file");
    assert_eq!(expired_at(&fix, "track-a"), None);
}

/// A copy a put-back still holds stays until the put-back is done.
#[test]
fn a_copy_a_put_back_holds_is_kept() {
    let fix = fixture("expiry-put-back");
    let copies = recycle_two(&fix);
    recycled_at(&fix, "track-a", RECYCLED_OLD);
    open_db(&fix)
        .execute_batch(
            "INSERT INTO library_publish_journal (id, bundle_id, kind, dest_root, dest_rel, \
             staged, staged_sha256, track_id, state) VALUES ('pb-putback-000', 'pb', \
             'put_back', 'music', 'staging/a.flac', '.dn-pb', 'x', 'track-a', 'staged')",
        )
        .unwrap();

    run_expiry(&fix, &fix.sandbox, 200);
    assert!(copies["track-a"].is_file());
    assert_eq!(expired_at(&fix, "track-a"), None);

    open_db(&fix)
        .execute_batch(
            "UPDATE library_publish_journal SET state = 'cleaned' WHERE id = 'pb-putback-000'",
        )
        .unwrap();
    run_expiry(&fix, &fix.sandbox, 200);
    assert!(!copies["track-a"].exists());
}

/// Entries no row names keep the old rule, narrowed: a folder named like
/// ours and past the retention by its name loses its plain files, then
/// itself. One holding anything else, or named otherwise, is kept, and so
/// is any entry a row names.
#[test]
fn old_entries_no_row_names_lose_only_their_plain_files() {
    let fix = fixture("expiry-legacy");
    let copies = recycle_two(&fix);
    recycled_at(&fix, "track-a", RECYCLED_YOUNG);
    recycled_at(&fix, "track-b", RECYCLED_YOUNG);
    let bin = fix.dir.join("bin");
    let plain = bin.join("20200101T000000-0a1b2c3d");
    let nested = bin.join("20200101T000000-1a2b3c4d");
    let foreign = bin.join("20200101T000000-notours");
    let young = bin.join("20960920T000000-2a2b2c2d");
    for dir in [&plain, &nested.join("sub"), &foreign, &young] {
        std::fs::create_dir_all(dir).unwrap();
    }
    std::fs::write(plain.join("old.flac"), b"old").unwrap();
    std::fs::write(nested.join("sub/old.flac"), b"old").unwrap();
    std::fs::write(foreign.join("old.flac"), b"old").unwrap();
    std::fs::write(young.join("young.flac"), b"young").unwrap();

    let report = run_expiry(&fix, &fix.sandbox, 200);

    assert_eq!(report.legacy_deleted, 1);
    assert!(!plain.exists());
    assert!(nested.join("sub/old.flac").is_file());
    assert!(foreign.join("old.flac").is_file());
    assert!(young.join("young.flac").is_file());
    assert!(copies.values().all(|copy| copy.is_file()));
}

/// Expiry never deletes from a bin that is `/`, is or holds a download
/// folder, or holds a library folder, a library folder's own `.recycle`
/// included. A bin beside them, or inside a download folder, is used.
#[test]
fn expiry_never_prunes_a_bin_over_downloads_or_a_library() {
    let scratch = ScratchDir::new("expiry-guard");
    let dir = scratch.to_path_buf();
    let music = dir.join("music");
    let staging = music.join(".recycle/staging");
    let complete = dir.join("downloads/complete");
    for folder in [&staging, &complete, &dir.join("bin")] {
        std::fs::create_dir_all(folder).unwrap();
    }
    let bins = vec![
        Root::bin("root", "/"),
        Root::bin("staging", staging.clone()),
        Root::bin("holds-downloads", dir.join("downloads")),
        Root::bin("holds-library", dir.clone()),
        Root::recycle_of("music", &music),
        Root::bin("inside-downloads", complete.join("bin")),
        Root::bin("beside", dir.join("bin")),
    ];
    let (kept, dropped) = publish::bin::expiry_guard(
        bins,
        &[Root::library("music", music.clone())],
        &[staging, complete],
        &[],
    );
    let kept: Vec<&str> = kept.iter().map(|bin| bin.id.as_str()).collect();
    assert_eq!(kept, ["inside-downloads", "beside"]);
    assert_eq!(dropped.len(), 5);
}

// --- A file a plugin adds, and its undo. ---

/// A bundle that adds `bytes` at `rel` under the music root for the
/// plugin `toy`.
fn plugin_add(id: &str, rel: &str, bytes: &[u8]) -> PlanBundle {
    PlanBundle {
        id: id.to_string(),
        items: Vec::new(),
        profile_revision: 1,
        naming_revision: 1,
        policy_revision: 5,
        put_back: None,
        added: Some(Added {
            origin: Origin::Plugin("toy".to_string()),
            file: AddedFile {
                root_id: "music".to_string(),
                relative_path: rel.to_string(),
                sha256: sha256_hex(bytes),
                size: bytes.len() as u64,
            },
            undoes: None,
        }),
        removal: None,
    }
}

/// The states of a bundle's rows.
fn row_states(fix: &Fixture, bundle_id: &str) -> Vec<JournalState> {
    JournalStore::new(&open_db(fix))
        .bundle(bundle_id)
        .unwrap()
        .iter()
        .map(|row| row.state)
        .collect()
}

/// A crash at any step of a plugin's add leaves, after recovery, the file
/// placed whole with its change recorded under the plugin's name, or no
/// file at all; never a hidden temp. A crash while the temp was still
/// being written (a part copy) unwinds too.
#[test]
fn a_plugin_file_lands_whole_or_not_at_all_after_a_crash() {
    let bytes = b"bytes a plugin wrote".as_slice();
    let mut cases: Vec<(CrashPoint, bool)> = CrashPoint::all()
        .into_iter()
        .map(|point| (point, false))
        .collect();
    cases.push((CrashPoint::AfterTempWritten, false));
    cases.push((CrashPoint::AfterTempWritten, true));
    for (point, part_written) in cases {
        let case = format!("{point:?} part_written={part_written}");
        let fix = fixture("plugin-add-crash");
        let bundle = plugin_add("add-1", "notes/one.txt", bytes);
        let (sealed, live) = seal(&bundle);
        let mut publisher = open_publisher(&fix);
        publisher.set_crash_point(Some(point));
        assert!(
            matches!(
                publisher.publish_add(&sealed, &live, bytes.to_vec()),
                Err(PublishError::InjectedCrash(_))
            ),
            "{case}"
        );
        drop(publisher);
        if part_written {
            let temps = hidden_leftovers(&fix.dir);
            assert_eq!(temps.len(), 1, "{case}");
            std::fs::write(&temps[0], &bytes[..5]).unwrap();
        }
        let db = restart(&fix);
        reconcile(&db, &fix.sandbox).unwrap();
        let dest = fix.sandbox.resolve("music", "notes/one.txt").unwrap();
        if point == CrashPoint::AfterTempWritten {
            assert!(dest.symlink_metadata().is_err(), "{case}: nothing placed");
            assert_eq!(
                row_states(&fix, "add-1"),
                [JournalState::Compensated],
                "{case}"
            );
        } else {
            assert_eq!(std::fs::read(&dest).unwrap(), bytes, "{case}");
            assert_eq!(row_states(&fix, "add-1"), [JournalState::Cleaned], "{case}");
            let recorded = publish::operations::load_operation(&open_db(&fix), "add-1")
                .unwrap()
                .unwrap();
            assert_eq!(
                recorded.added.unwrap().origin.to_string(),
                "plugin:toy",
                "{case}"
            );
        }
        assert!(hidden_leftovers(&fix.dir).is_empty(), "{case}");
    }
}

/// A plugin's add never replaces a file: one already there, one that turns
/// up just before placement, and a folder that can only check and then
/// rename all refuse it, with the other file and the folder as they were.
#[test]
fn a_plugin_file_never_replaces_one() {
    let bytes = b"bytes a plugin wrote".as_slice();
    let theirs = b"a file that was there".as_slice();

    let fix = fixture("plugin-add-taken");
    let dest = fix.sandbox.resolve("music", "notes/one.txt").unwrap();
    std::fs::create_dir_all(dest.parent().unwrap()).unwrap();
    std::fs::write(&dest, theirs).unwrap();
    let (sealed, live) = seal(&plugin_add("add-1", "notes/one.txt", bytes));
    let mut publisher = open_publisher(&fix);
    assert!(matches!(
        publisher.publish_add(&sealed, &live, bytes.to_vec()),
        Err(PublishError::Collision(_))
    ));
    assert_eq!(std::fs::read(&dest).unwrap(), theirs);
    assert!(hidden_leftovers(&fix.dir).is_empty());

    let fix = fixture("plugin-add-late");
    let dest = fix.sandbox.resolve("music", "notes/one.txt").unwrap();
    let (sealed, live) = seal(&plugin_add("add-1", "notes/one.txt", bytes));
    let mut publisher = open_publisher(&fix);
    publisher.set_before_place(Some(Box::new(move |dest| {
        std::fs::write(dest, b"a file that was there").unwrap();
    })));
    assert!(matches!(
        publisher.publish_add(&sealed, &live, bytes.to_vec()),
        Err(PublishError::Collision(_))
    ));
    assert_eq!(std::fs::read(&dest).unwrap(), theirs);
    assert_eq!(row_states(&fix, "add-1"), [JournalState::Compensated]);
    assert!(hidden_leftovers(&fix.dir).is_empty());

    let fix = fixture("plugin-add-check");
    fix.sandbox
        .set_placement_mode("music", PlacementMode::Check);
    let dest = fix.sandbox.resolve("music", "notes/one.txt").unwrap();
    let (sealed, live) = seal(&plugin_add("add-1", "notes/one.txt", bytes));
    let mut publisher = open_publisher(&fix);
    publisher.set_place_seams(Seams {
        skip_rename: true,
        link_errno: Some(libc::EPERM),
        ..Seams::default()
    });
    assert!(matches!(
        publisher.publish_add(&sealed, &live, bytes.to_vec()),
        Err(PublishError::NoSafePlacement(_))
    ));
    assert!(dest.symlink_metadata().is_err(), "nothing was placed");
    assert!(hidden_leftovers(&fix.dir).is_empty());
}

/// Undo of a plugin's add takes the file away while it still holds the
/// bytes written, once. A file changed since stays and the undo says so;
/// an add that never landed, and the undo itself, cannot be undone.
#[test]
fn a_plugins_files_list_until_taken_back() {
    let bytes = b"bytes a plugin wrote".as_slice();
    let fix = fixture("plugin-files-list");
    let mut publisher = open_publisher(&fix);
    for (id, rel) in [("add-1", "notes/one.txt"), ("add-2", "notes/two.txt")] {
        let (sealed, live) = seal(&plugin_add(id, rel, bytes));
        publisher
            .publish_add(&sealed, &live, bytes.to_vec())
            .unwrap();
    }
    let (sealed, live) = seal(&plugin_add("add-3", "notes/three.txt", bytes));
    publisher.set_crash_point(Some(CrashPoint::AfterTempWritten));
    publisher
        .publish_add(&sealed, &live, bytes.to_vec())
        .unwrap_err();
    publisher.set_crash_point(None);
    reconcile(&restart(&fix), &fix.sandbox).unwrap();
    let listed = |plugin: &str| {
        let mut files = publish::add::plugin_files(&open_db(&fix), plugin, None, 10).unwrap();
        files.sort_by(|a, b| a.bundle_id.cmp(&b.bundle_id));
        files
            .into_iter()
            .map(|file| (file.bundle_id, file.file.relative_path, file.taken_back))
            .collect::<Vec<_>>()
    };
    let row = |id: &str, rel: &str, taken_back: bool| (id.to_string(), rel.to_string(), taken_back);
    assert_eq!(
        listed("toy"),
        [
            row("add-1", "notes/one.txt", false),
            row("add-2", "notes/two.txt", false)
        ],
        "a landed file is listed; one that never landed is not"
    );
    assert!(listed("other").is_empty());

    let added = publish::add::plan_take_back(&open_db(&fix), "add-1")
        .unwrap()
        .unwrap();
    let (sealed, live) = seal(&PlanBundle {
        added: Some(added),
        ..plugin_add("undo-1", "unused", b"")
    });
    publisher
        .publish_as(&sealed, &live, &BTreeMap::new(), None)
        .unwrap();
    assert_eq!(
        listed("toy"),
        [
            row("add-1", "notes/one.txt", true),
            row("add-2", "notes/two.txt", false)
        ],
        "the undo is not listed as a file of its own"
    );

    let first = publish::add::plugin_files(&open_db(&fix), "toy", None, 1).unwrap();
    let rest = publish::add::plugin_files(
        &open_db(&fix),
        "toy",
        Some((first[0].added_at, &first[0].bundle_id)),
        10,
    )
    .unwrap();
    assert_eq!(first.len() + rest.len(), 2);
    assert_ne!(first[0].bundle_id, rest[0].bundle_id);
}

#[test]
fn undo_takes_a_plugin_file_only_while_unchanged() {
    let bytes = b"bytes a plugin wrote".as_slice();
    let fix = fixture("plugin-add-undo");
    let mut publisher = open_publisher(&fix);
    for (id, rel) in [("add-1", "notes/one.txt"), ("add-2", "notes/two.txt")] {
        let (sealed, live) = seal(&plugin_add(id, rel, bytes));
        assert_eq!(
            publisher
                .publish_add(&sealed, &live, bytes.to_vec())
                .unwrap(),
            PublishOutcome::Committed
        );
    }
    let take_back = |publisher: &mut Publisher<FixedSpace>, id: &str, undo: &str| {
        let added = publish::add::plan_take_back(&open_db(&fix), id).unwrap()?;
        publish::add::check_take_back(&fix.sandbox, &added.file)?;
        let bundle = PlanBundle {
            added: Some(added),
            ..plugin_add(undo, "unused", b"")
        };
        let (sealed, live) = seal(&bundle);
        publisher
            .publish_as(&sealed, &live, &BTreeMap::new(), None)
            .unwrap();
        Ok::<(), TakeBackRefusal>(())
    };

    let one = fix.sandbox.resolve("music", "notes/one.txt").unwrap();
    take_back(&mut publisher, "add-1", "undo-1").unwrap();
    assert!(one.symlink_metadata().is_err(), "the plugin's file is gone");
    assert_eq!(
        take_back(&mut publisher, "add-1", "undo-1b"),
        Err(TakeBackRefusal::AlreadyTakenBack)
    );
    assert_eq!(
        take_back(&mut publisher, "undo-1", "undo-1c"),
        Err(TakeBackRefusal::IsATakeBack)
    );

    let two = fix.sandbox.resolve("music", "notes/two.txt").unwrap();
    std::fs::write(&two, b"a person's edit").unwrap();
    assert_eq!(
        take_back(&mut publisher, "add-2", "undo-2"),
        Err(TakeBackRefusal::Changed("notes/two.txt".to_string()))
    );
    assert_eq!(std::fs::read(&two).unwrap(), b"a person's edit");

    let (sealed, live) = seal(&plugin_add("add-3", "notes/three.txt", bytes));
    publisher.set_crash_point(Some(CrashPoint::AfterTempWritten));
    publisher
        .publish_add(&sealed, &live, bytes.to_vec())
        .unwrap_err();
    publisher.set_crash_point(None);
    reconcile(&restart(&fix), &fix.sandbox).unwrap();
    assert_eq!(
        take_back(&mut publisher, "add-3", "undo-3"),
        Err(TakeBackRefusal::NotLanded)
    );
    assert!(hidden_leftovers(&fix.dir).is_empty());
}

// --- Folders: a write removes only the folders it made, and prunes the
// sources it moved away from. ---

/// A move whose cleanup a crash interrupted is finished by recovery, and
/// recovery prunes the source folders the move left empty, as a publish
/// that finishes by itself does. The library folder itself stays.
#[test]
fn recovery_of_an_interrupted_move_prunes_its_empty_source_folders() {
    for point in [CrashPoint::AfterCatalogCommit, CrashPoint::DuringCleanup] {
        let fix = fixture(&format!("prune-{}", point.as_str()));
        let (bundle, docs) = two_track_bundle(&fix, PlanKind::Move);
        let (sealed, live) = seal(&bundle);
        let mut publisher = open_publisher(&fix);
        publisher.set_crash_point(Some(point));
        let err = publisher.publish(&sealed, &live, &docs).unwrap_err();
        assert!(matches!(err, PublishError::InjectedCrash(_)), "got {err:?}");
        drop(publisher);

        let db = restart(&fix);
        reconcile(&db, &fix.sandbox).unwrap();
        let staging = fix.dir.join("music/staging");
        assert!(
            staging.symlink_metadata().is_err(),
            "{point:?}: staging/ pruned"
        );
        assert!(
            fix.dir.join("music/organized/a.flac").is_file(),
            "{point:?}"
        );
        assert!(fix.dir.join("music").is_dir(), "{point:?}");
    }
}

/// A move bound for a new folder, `new/artist/`, and for `kept/`, which
/// was there before, empty.
fn folder_bundle(fix: &Fixture) -> (PlanBundle, BTreeMap<String, TagDocument>) {
    let (mut bundle, docs) = two_track_bundle(fix, PlanKind::Move);
    bundle.items[0].dest_rel = "new/artist/a.flac".to_string();
    bundle.items[0].sidecars[0].dest_rel = "new/artist/cover.jpg".to_string();
    bundle.items[1].dest_rel = "kept/b.flac".to_string();
    std::fs::create_dir_all(fix.dir.join("music/kept")).unwrap();
    (bundle, docs)
}

fn no_safe_step() -> Seams {
    Seams {
        skip_rename: true,
        link_errno: Some(libc::EPERM),
        ..Seams::default()
    }
}

fn assert_sources_home(fix: &Fixture) {
    let original = std::fs::read(fixtures_dir().join("management_full.flac")).unwrap();
    for rel in ["staging/a.flac", "staging/b.flac"] {
        let source = fix.sandbox.resolve("music", rel).unwrap();
        assert_eq!(std::fs::read(source).unwrap(), original, "{rel} is home");
    }
}

/// A publish that unwinds removes the folders it made for its files, and
/// never a folder that was there before, even an empty one.
#[test]
fn an_unwound_move_removes_only_the_folders_it_made() {
    let fix = fixture("unwind-folders");
    let (bundle, docs) = folder_bundle(&fix);
    let (sealed, live) = seal(&bundle);
    let mut publisher = open_publisher(&fix);
    publisher.set_place_seams(no_safe_step());
    let err = publisher.publish(&sealed, &live, &docs).unwrap_err();
    assert!(
        matches!(err, PublishError::NoSafePlacement(_)),
        "got {err:?}"
    );

    assert!(fix.dir.join("music/new").symlink_metadata().is_err());
    assert!(fix.dir.join("music/kept").is_dir());
    assert_sources_home(&fix);
    assert!(hidden_leftovers(&fix.dir).is_empty());
}

/// A crash while a bundle stages leaves the folders its files were going
/// to. Recovery unwinds the bundle and removes those folders, and keeps a
/// folder that was there before.
#[test]
fn a_crash_while_staging_leaves_no_folder_it_made() {
    for point in [
        CrashPoint::AfterFirstFileStaged,
        CrashPoint::AfterTempWritten,
    ] {
        let fix = fixture(&format!("staging-folders-{}", point.as_str()));
        let (bundle, docs) = folder_bundle(&fix);
        let (sealed, live) = seal(&bundle);
        let mut publisher = open_publisher(&fix);
        publisher.set_crash_point(Some(point));
        let err = publisher.publish(&sealed, &live, &docs).unwrap_err();
        assert!(matches!(err, PublishError::InjectedCrash(_)), "got {err:?}");
        drop(publisher);
        assert!(fix.dir.join("music/new/artist").is_dir(), "{point:?}");

        let db = restart(&fix);
        let results = reconcile(&db, &fix.sandbox).unwrap();
        assert_eq!(
            results[0].action,
            publish::RecoveryAction::Compensated,
            "{point:?}"
        );
        assert!(
            fix.dir.join("music/new").symlink_metadata().is_err(),
            "{point:?}: new/ removed"
        );
        assert!(fix.dir.join("music/kept").is_dir(), "{point:?}");
        assert_sources_home(&fix);
        assert!(hidden_leftovers(&fix.dir).is_empty(), "{point:?}");
        let again = reconcile(&db, &fix.sandbox).unwrap();
        assert!(again.is_empty(), "{point:?}: nothing left to do");
    }
}

/// An unwind that stopped before the folders it made were removed is
/// finished by recovery: the empty ones go, one that now holds another
/// program's file stays, and nothing is left for a later pass.
#[test]
fn a_stopped_folder_removal_is_finished_by_recovery() {
    let fix = fixture("stopped-folder-removal");
    let (mut bundle, docs) = folder_bundle(&fix);
    bundle.items[1].dest_rel = "other/b.flac".to_string();
    let (sealed, live) = seal(&bundle);
    let mut publisher = open_publisher(&fix);
    publisher.set_place_seams(no_safe_step());
    publisher.set_crash_point(Some(CrashPoint::BeforeFolderRemoval));
    let err = publisher.publish(&sealed, &live, &docs).unwrap_err();
    assert!(matches!(err, PublishError::InjectedCrash(_)), "got {err:?}");
    drop(publisher);
    assert!(fix.dir.join("music/new/artist").is_dir());
    let stray = fix.dir.join("music/other/notes.txt");
    std::fs::write(&stray, b"someone else's").unwrap();

    let db = restart(&fix);
    let results = reconcile(&db, &fix.sandbox).unwrap();
    assert_eq!(results.len(), 1, "{results:?}");
    assert!(fix.dir.join("music/new").symlink_metadata().is_err());
    assert_eq!(std::fs::read(&stray).unwrap(), b"someone else's");
    assert_sources_home(&fix);
    let again = reconcile(&db, &fix.sandbox).unwrap();
    assert!(again.is_empty(), "nothing left to do: {again:?}");
}

/// A placed row whose copy an unwind took off, while its staged temp is
/// still there, was half unwound. Recovery finishes the unwind and never
/// commits a catalog row for a file that is not in place.
#[test]
fn a_half_unwound_placed_row_unwinds() {
    let fix = fixture("half-unwound");
    let (bundle, docs) = two_track_bundle(&fix, PlanKind::Move);
    let (sealed, live) = seal(&bundle);
    let mut publisher = open_publisher(&fix);
    publisher.set_crash_point(Some(CrashPoint::BeforeCatalogCommit));
    let err = publisher.publish(&sealed, &live, &docs).unwrap_err();
    assert!(matches!(err, PublishError::InjectedCrash(_)), "got {err:?}");
    drop(publisher);
    let journals = JournalStore::new(&open_db(&fix))
        .bundle("bundle-1")
        .unwrap();
    let dest = fix.sandbox.resolve("music", "organized/a.flac").unwrap();
    std::fs::copy(&dest, &journals[0].staged).unwrap();
    std::fs::remove_file(&dest).unwrap();

    let db = restart(&fix);
    let results = reconcile(&db, &fix.sandbox).unwrap();
    assert_eq!(results[0].action, publish::RecoveryAction::Compensated);
    assert_eq!(read_revision(&open_db(&fix)).unwrap(), start_revision());
    assert_eq!(
        track_row(&open_db(&fix), "track-a").0,
        "staging/a.flac",
        "the catalog still names the file where it is"
    );
}

/// A crash while a file's journal temp was being written leaves part of
/// it. The row never staged, so recovery unwinds it: the part copy goes
/// and the source stays as it was.
#[test]
fn a_part_written_temp_unwinds_at_recovery() {
    let fix = fixture("part-temp");
    let (bundle, docs) = two_track_bundle(&fix, PlanKind::Move);
    let (sealed, live) = seal(&bundle);
    let source = fix.sandbox.resolve("music", "staging/a.flac").unwrap();
    let original = std::fs::read(&source).unwrap();
    let mut publisher = open_publisher(&fix);
    publisher.set_crash_point(Some(CrashPoint::AfterTempWritten));
    let err = publisher.publish(&sealed, &live, &docs).unwrap_err();
    assert!(matches!(err, PublishError::InjectedCrash(_)), "got {err:?}");
    drop(publisher);
    let journals = JournalStore::new(&open_db(&fix))
        .bundle("bundle-1")
        .unwrap();
    let temp = &journals[0].staged;
    let half = std::fs::read(temp).unwrap().len() as u64 / 2;
    std::fs::OpenOptions::new()
        .write(true)
        .open(temp)
        .unwrap()
        .set_len(half)
        .unwrap();

    let db = restart(&fix);
    let results = reconcile(&db, &fix.sandbox).unwrap();
    assert_eq!(results[0].action, publish::RecoveryAction::Compensated);
    assert!(hidden_leftovers(&fix.dir).is_empty());
    assert_eq!(std::fs::read(&source).unwrap(), original);
}

/// A moved file's source is deleted only once it is read: an edit made
/// while the move ran, which kept the file's size and put its modified
/// time back (a tagger set to keep timestamps), is kept, not deleted.
#[test]
fn an_edit_that_kept_size_and_time_is_never_deleted() {
    let fix = fixture("kept-edit");
    let (mut bundle, docs) = two_track_bundle(&fix, PlanKind::Move);
    bundle.items.truncate(1);
    bundle.items[0].managed_updates = TagUpdates::new();
    let (sealed, live) = seal(&bundle);
    let source = fix.sandbox.resolve("music", "staging/a.flac").unwrap();
    let original = std::fs::read(&source).unwrap();
    let mut edited = original.clone();
    let last = edited.len() - 1;
    edited[last] ^= 0xff;
    let modified = std::fs::metadata(&source).unwrap().modified().unwrap();
    let mut publisher = open_publisher(&fix);
    let (path, bytes) = (source.clone(), edited.clone());
    publisher.set_before_place(Some(Box::new(move |_| {
        let mut file = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
        std::io::Write::write_all(&mut file, &bytes).unwrap();
        file.set_modified(modified).unwrap();
    })));
    publisher.publish(&sealed, &live, &docs).unwrap();

    let dest = fix.sandbox.resolve("music", "organized/a.flac").unwrap();
    assert_eq!(std::fs::read(&dest).unwrap(), original);
    assert_eq!(std::fs::read(&source).unwrap(), edited, "the edit is kept");
}

/// A write in place with no tag change that stopped once its temp was
/// written unwinds: the file at its name is the original, not a second
/// copy, and is left as it is.
#[test]
fn an_unchanged_write_in_place_that_stopped_at_its_temp_unwinds() {
    let original = std::fs::read(fixtures_dir().join("management_full.flac")).unwrap();
    let fix = fixture("unchanged-temp");
    let (mut bundle, docs) = two_track_bundle(&fix, PlanKind::SamePath);
    for item in bundle.items.iter_mut() {
        item.managed_updates = TagUpdates::new();
    }
    let (sealed, live) = seal(&bundle);
    let mut publisher = open_publisher(&fix);
    publisher.set_crash_point(Some(CrashPoint::AfterTempWritten));
    let err = publisher.publish(&sealed, &live, &docs).unwrap_err();
    assert!(matches!(err, PublishError::InjectedCrash(_)), "got {err:?}");
    drop(publisher);

    let db = restart(&fix);
    let results = reconcile(&db, &fix.sandbox).unwrap();
    assert_eq!(results[0].action, publish::RecoveryAction::Compensated);
    assert!(hidden_leftovers(&fix.dir).is_empty());
    let path = fix.sandbox.resolve("music", "staging/a.flac").unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), original);
}

// --- Stored shapes across the typed seams. ---

/// Bundles, before-states and in-flight states stored before tag updates,
/// formats and management states were typed. The publisher wrote them at
/// 1e0545d1, from the library journeys and the in-flight scenarios below,
/// except `before_v2.json`, built by hand in the version 2 shape. Paths
/// in the in-flight states read `{root}` for the scratch folder.
fn stored(name: &str) -> String {
    let path = fixtures_dir().join("publish").join(name);
    std::fs::read_to_string(&path).unwrap_or_else(|error| panic!("{name}: {error}"))
}

/// Every stored bundle and before-state reads back as typed values and
/// writes out byte for byte as it was stored. Names, formats, output kinds
/// and picture records that do not read are refused.
#[test]
fn stored_bundles_and_before_states_read_back_unchanged() {
    for name in [
        "bundle_management.json",
        "bundle_undo.json",
        "bundle_spellings.json",
        "bundle_import.json",
    ] {
        let text = stored(name);
        let bundle: PlanBundle =
            serde_json::from_str(&text).unwrap_or_else(|e| panic!("{name}: {e}"));
        assert_eq!(serde_json::to_string(&bundle).unwrap(), text, "{name}");
    }
    for name in [
        "before_v6.json",
        "before_v6_spellings.json",
        "before_v2.json",
    ] {
        let text = stored(name);
        let before =
            BeforeState::from_bytes(text.as_bytes()).unwrap_or_else(|e| panic!("{name}: {e}"));
        assert_eq!(before.to_bytes().unwrap(), text.as_bytes(), "{name}");
    }

    let management: PlanBundle = serde_json::from_str(&stored("bundle_management.json")).unwrap();
    let item = &management.items[0];
    assert_eq!(
        (item.kind, item.format),
        (PlanKind::Move, AudioFormat::Flac)
    );
    assert_eq!(
        item.managed_updates
            .get(&TagTarget::Native("vorbis:MOOD".to_owned())),
        Some(&Vec::new()),
        "a scrubbed native item"
    );
    let pictures = item.managed_updates.pictures().expect("a pictures update");
    assert_eq!(
        (pictures[0].kind, pictures[0].mime.as_str()),
        (3, "image/jpeg")
    );
    let kinds: Vec<OutputKind> = item.outputs.iter().map(|output| output.kind).collect();
    assert_eq!(kinds, [OutputKind::Lyrics, OutputKind::Artwork]);

    let undo: PlanBundle = serde_json::from_str(&stored("bundle_undo.json")).unwrap();
    let kinds: Vec<OutputKind> = undo.items[0]
        .removals
        .iter()
        .map(|output| output.kind)
        .collect();
    assert_eq!(kinds, [OutputKind::Lyrics, OutputKind::Artwork]);
    assert!(undo.items[0].managed_updates.pictures().is_some());

    let spellings: PlanBundle = serde_json::from_str(&stored("bundle_spellings.json")).unwrap();
    assert_eq!(
        spellings.items[0]
            .managed_updates
            .get(&TagTarget::Spelling(TagField::TrackTotal, "TRACKTOTAL")),
        Some(&vec!["09".to_owned()])
    );

    let import: PlanBundle = serde_json::from_str(&stored("bundle_import.json")).unwrap();
    assert!(import.items[0].adopt.is_some());

    let v2 = BeforeState::from_bytes(stored("before_v2.json").as_bytes()).unwrap();
    assert_eq!((v2.doc.version, v2.doc.pictures.is_none()), (2, true));
    assert_eq!(
        v2.doc
            .managed
            .get(&TagTarget::Spelling(TagField::TrackTotal, "TRACKTOTAL")),
        Some(&vec!["09".to_owned()])
    );

    let refused = |edit: &dyn Fn(&mut serde_json::Value)| {
        let mut value: serde_json::Value =
            serde_json::from_str(&stored("bundle_management.json")).unwrap();
        edit(&mut value["items"][0]);
        serde_json::from_value::<PlanBundle>(value).is_err()
    };
    assert!(refused(&|item| item["format"] = "wma".into()));
    assert!(refused(&|item| item["outputs"][0]["kind"] = "video".into()));
    assert!(refused(
        &|item| item["managed_updates"]["not_a_field"] = serde_json::json!(["x"])
    ));
    assert!(refused(
        &|item| item["managed_updates"]["title:TITLE_NOT"] = serde_json::json!(["x"])
    ));
    assert!(refused(
        &|item| item["managed_updates"]["pictures"] = serde_json::json!(["3\timage/jpeg"])
    ));
}

/// The rows the publisher keeps for generated files equal the rule worked
/// out from the journal and the bundles. Returns how many there are.
fn outputs_follow_the_rule(conn: &Connection) -> usize {
    type Row = (
        String,
        String,
        i64,
        i64,
        String,
        String,
        String,
        String,
        i64,
        String,
    );
    let read = |sql: &str| -> Vec<Row> {
        conn.prepare(sql)
            .unwrap()
            .query_map([], |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                    row.get(6)?,
                    row.get(7)?,
                    row.get(8)?,
                    row.get(9)?,
                ))
            })
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap()
    };
    let columns = "journal_id, bundle_id, item_index, output_index, track_id, dest_root, \
                   dest_rel, sha256, size, kind";
    let kept = read(&format!(
        "SELECT {columns} FROM library_publish_outputs ORDER BY journal_id"
    ));
    let rule = read(&format!(
        "SELECT {columns} FROM library_publish_outputs_rule ORDER BY journal_id"
    ));
    assert_eq!(kept, rule, "the kept outputs follow the rule");
    kept.len()
}

/// A scratch folder holding a publish state stored before migration 0097,
/// on a database migrated up to it, then upgraded: what a server finds on
/// its first start after the upgrade.
fn upgraded_from_stored(tag: &str, name: &str) -> Fixture {
    use base64::Engine as _;
    use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
    let scratch = ScratchDir::new(&format!("publish-{tag}"));
    let dir = scratch.to_path_buf();
    let db_path = dir.join("app.db");
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let pool = runtime.block_on(async {
        let options = SqliteConnectOptions::new()
            .filename(&db_path)
            .create_if_missing(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await
            .unwrap();
        let before = sqlx::migrate::Migrator {
            migrations: std::borrow::Cow::Owned(
                droppedneedle_kernel::schema::MIGRATOR
                    .migrations
                    .iter()
                    .filter(|migration| migration.version < 97)
                    .cloned()
                    .collect(),
            ),
            ignore_missing: false,
            locking: true,
            no_tx: false,
        };
        before.run(&pool).await.unwrap();
        pool
    });
    seed_catalog(&db_path);

    let root = dir.to_string_lossy().into_owned();
    let state: serde_json::Value = serde_json::from_str(&stored(name)).unwrap();
    let conn = droppedneedle_kernel::db::open_connection(&db_path).unwrap();
    // Parents first.
    for table in [
        "library_publish_operations",
        "library_publish_journal",
        "library_publish_replacements",
        "library_publish_created_folders",
        "library_publish_snapshots",
        "library_publish_baselines",
        "library_track_management_state",
    ] {
        for row in state["tables"][table].as_array().unwrap() {
            let row = row.as_object().unwrap();
            let names: Vec<&str> = row.keys().map(String::as_str).collect();
            let values: Vec<rusqlite::types::Value> = row
                .values()
                .map(|value| match value {
                    serde_json::Value::Null => rusqlite::types::Value::Null,
                    serde_json::Value::Number(number) => match number.as_i64() {
                        Some(int) => rusqlite::types::Value::Integer(int),
                        None => rusqlite::types::Value::Real(number.as_f64().unwrap()),
                    },
                    serde_json::Value::String(text) => {
                        rusqlite::types::Value::Text(text.replace("{root}", &root))
                    }
                    other => panic!("{table}: {other}"),
                })
                .collect();
            let marks = vec!["?"; names.len()].join(", ");
            conn.execute(
                &format!(
                    "INSERT INTO {table} ({}) VALUES ({marks})",
                    names.join(", ")
                ),
                rusqlite::params_from_iter(values),
            )
            .unwrap_or_else(|error| panic!("{table}: {error}"));
        }
    }
    drop(conn);
    for (rel, bytes) in state["files"].as_object().unwrap() {
        let path = dir.join(rel);
        match bytes.as_str() {
            None => std::fs::create_dir_all(&path).unwrap(),
            Some(encoded) => {
                std::fs::create_dir_all(path.parent().unwrap()).unwrap();
                let bytes = base64::engine::general_purpose::STANDARD
                    .decode(encoded)
                    .unwrap();
                std::fs::write(&path, bytes).unwrap();
            }
        }
    }

    runtime.block_on(async {
        droppedneedle_kernel::schema::apply_migrations(&pool)
            .await
            .unwrap();
        pool.close().await;
    });
    let sandbox = Sandbox::new(vec![
        Root::library("music", dir.join("music")),
        Root::library("incoming", dir.join("incoming")),
        Root::bin("bin", dir.join("bin")),
    ])
    .unwrap();
    Fixture {
        dir,
        sandbox,
        db_path,
        _scratch: scratch,
    }
}

/// A move with a generated `.lrc`, stopped before its catalog commit by
/// the publisher from before the upgrade, after an earlier publish placed
/// a cover: recovery after the upgrade commits it exactly once, and undo
/// and baseline restore find both generated files.
#[test]
fn a_publish_stopped_before_the_upgrade_commits_once_after_it() {
    let fix = upgraded_from_stored("inflight-management", "inflight_management.json");
    let conn = open_db(&fix);
    let state = |id: &str| -> (String, Option<String>) {
        conn.query_row(
            "SELECT state, commit_as FROM library_publish_journal WHERE id = ?1",
            [id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap()
    };
    assert_eq!(
        state("bundle-move-000-audio"),
        ("published".to_owned(), Some("managed".to_owned()))
    );
    assert_eq!(
        outputs_follow_the_rule(&conn),
        1,
        "the earlier cover is found; the stopped .lrc is not yet"
    );
    let revision = read_revision(&conn).unwrap();

    let db = restart(&fix);
    let results = reconcile(&db, &fix.sandbox).unwrap();
    assert_eq!(results.len(), 1, "{results:?}");
    assert_eq!(results[0].bundle_id, "bundle-move");
    assert_eq!(results[0].action, publish::RecoveryAction::ResumedCommitted);
    assert_eq!(read_revision(&conn).unwrap(), revision + 1, "one commit");
    assert!(reconcile(&db, &fix.sandbox).unwrap().is_empty());

    let moved = fix.sandbox.resolve("music", "organized/b.flac").unwrap();
    assert_eq!(
        sha256_hex(&std::fs::read(&moved).unwrap()),
        JournalStore::new(&conn)
            .get("bundle-move-000-audio")
            .unwrap()
            .unwrap()
            .staged_sha256
    );
    assert!(
        !fix.sandbox
            .resolve("music", "staging/b.flac")
            .unwrap()
            .exists()
    );
    assert!(
        fix.sandbox
            .resolve("music", "organized/b.lrc")
            .unwrap()
            .exists()
    );
    assert!(hidden_leftovers(&fix.dir).is_empty());
    assert_eq!(track_row(&conn, "track-b").0, "organized/b.flac");
    assert_eq!(state("bundle-move-000-audio").0, "cleaned");
    let outcomes: Vec<String> = conn
        .prepare("SELECT last_outcome FROM library_track_management_state ORDER BY local_track_id")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(outcomes, ["managed", "managed"]);

    assert_eq!(outputs_follow_the_rule(&conn), 2);
    let cover = publish::operations::generated_for_track(&conn, "track-a").unwrap();
    assert_eq!(cover.len(), 1);
    assert_eq!(
        (cover[0].dest_rel.as_str(), cover[0].kind),
        ("staging/cover.jpg", OutputKind::Artwork)
    );
    let undone = publish::operations::outputs_of(&conn, "bundle-cover").unwrap();
    assert_eq!(
        undone["track-a"], cover,
        "undo of the earlier run finds its cover"
    );
    let lyrics = publish::operations::outputs_of(&conn, "bundle-move").unwrap();
    assert_eq!(lyrics["track-b"][0].dest_rel, "organized/b.lrc");
    assert_eq!(lyrics["track-b"][0].kind, OutputKind::Lyrics);
}

/// An import stopped before its catalog commit by the publisher from
/// before the upgrade is added to the catalog by recovery after it, as
/// an import: its file is indexed as a new track, not moved as one.
#[test]
fn an_import_stopped_before_the_upgrade_is_added_after_it() {
    let fix = upgraded_from_stored("inflight-import", "inflight_import.json");
    let conn = open_db(&fix);
    let commit_as: Option<String> = conn
        .query_row(
            "SELECT commit_as FROM library_publish_journal WHERE id = 'bundle-import-000-audio'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(commit_as.as_deref(), Some("imported"));

    let db = restart(&fix);
    let results = reconcile(&db, &fix.sandbox).unwrap();
    assert_eq!(results.len(), 1, "{results:?}");
    assert_eq!(results[0].action, publish::RecoveryAction::ResumedCommitted);
    let (rel, availability, _) = track_row(&conn, "track-new");
    assert_eq!(
        (rel.as_str(), availability.as_str()),
        ("Imported/new.flac", "indexed")
    );
    let outcome: String = conn
        .query_row(
            "SELECT last_outcome FROM library_track_management_state \
             WHERE local_track_id = 'track-new'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(outcome, "imported");
    assert!(
        fix.sandbox
            .resolve("music", "Imported/new.flac")
            .unwrap()
            .exists()
    );
    assert!(hidden_leftovers(&fix.dir).is_empty());
}
