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
//! Tag staging goes through the real tags slice save
//! wrapper (stage-8 integration); publish-path briefs plant real
//! committed fixtures, never fake bytes with audio extensions.

use droppedneedle::library::publish;

use droppedneedle::library::tags;

use std::collections::BTreeMap;
use std::path::PathBuf;

use crate::common::ScratchDir;
use publish::paths::Root;
use publish::place::{PlacementMode, Seams};
use publish::planner::{
    Capability, CapabilityGate, CollisionGate, FileFingerprint, PlanBundle, PlanItem, PlanKind,
    ReleaseIdentity, SealError, SealRecheck, SealedPreview, SidecarPlan, SpaceProbe, TrackMapping,
};
use publish::recovery::reconcile_with_seams;
use publish::snapshots::{BaselineStore, BlobStore, SnapshotStore, sha256_hex};
use publish::tags_seam::TagDocument;
use publish::undo::{
    BaselineBlock, BaselineInput, BeforeState, UndoBlock, UndoInput, UndoLive,
    confirm_baseline_purge, plan_baseline_restore, plan_undo,
};
use publish::{
    ArchiveEntry, ArchivePolicy, AutomaticEligibility, AutomaticHold, Catalog, CrashPoint,
    JournalState, JournalStore, PublishError, PublishOutcome, Publisher, Sandbox, SqliteCatalog,
    reconcile, validate_archive,
};
use rusqlite::Connection;

/// Tracks the bundles in these tests publish, seeded into the catalog
/// at their source paths.
const CATALOG_TRACKS: [(&str, &str); 4] = [
    ("track-a", "staging/a.flac"),
    ("track-b", "staging/b.flac"),
    ("track-s", "staging/s.flac"),
    ("t", "s.flac"),
];

/// A migrated scratch database with the test tracks in the catalog.
/// The catalog revision a fresh fixture database starts at (migrations
/// may move it), so bundles plan against the real value.
fn db_catalog_revision(fix: &Fixture) -> u64 {
    let conn = droppedneedle::db::open_connection(&fix.db_path).unwrap();
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

fn scratch_db(dir: &std::path::Path) -> PathBuf {
    let db_path = dir.join("app.db");
    crate::common::migrated_db(&db_path);
    let conn = droppedneedle::db::open_connection(&db_path).unwrap();
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
    }
    db_path
}

fn open_db(fix: &Fixture) -> Connection {
    publish::recovery::open_recovery_connection(&fix.db_path).unwrap()
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
        Root {
            id: "music".to_string(),
            dir: music,
        },
        Root {
            id: "incoming".to_string(),
            dir: incoming,
        },
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
    let mut managed_updates = BTreeMap::new();
    managed_updates.insert("title".to_string(), vec![format!("Title {track}")]);
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
        format: "flac".to_string(),
        managed_updates,
        sidecars: Vec::new(),
        staged_bytes_estimate: 1024,
        adopt: None,
        replaces: None,
        outputs: Vec::new(),
        removals: Vec::new(),
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
        catalog_revision: db_catalog_revision(fix),
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
        catalog_revision: bundle.catalog_revision,
        settings_revision: 11,
        today_day: 50,
        token_hash: "tok".to_string(),
    };
    (sealed, live)
}

fn open_publisher(fix: &Fixture) -> Publisher<SqliteCatalog, FixedSpace> {
    Publisher::open(
        fix.sandbox.clone(),
        &fix.db_path,
        SqliteCatalog,
        FixedSpace(u64::MAX),
        || 50,
    )
    .unwrap()
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

#[test]
fn automatic_work_requires_root_trigger_release_and_mapping() {
    let mut mappings = BTreeMap::new();
    mappings.insert(
        "t1".to_string(),
        TrackMapping {
            track_id: "t1".to_string(),
            release_track_mbid: "rt-1".to_string(),
            medium_position: 1,
            release_position: 1,
        },
    );
    let release = test_identity();
    assert_eq!(
        AutomaticEligibility::check(
            false,
            "acquisition",
            true,
            Some(&release),
            &mappings,
            &["t1".to_string()]
        ),
        Err(AutomaticHold::RootDisabled)
    );
    assert_eq!(
        AutomaticEligibility::check(
            true,
            "scan",
            false,
            Some(&release),
            &mappings,
            &["t1".to_string()]
        ),
        Err(AutomaticHold::TriggerDisabled("scan".to_string()))
    );
    assert_eq!(
        AutomaticEligibility::check(
            true,
            "acquisition",
            true,
            None,
            &mappings,
            &["t1".to_string()]
        ),
        Err(AutomaticHold::NoAcceptedRelease)
    );
    assert_eq!(
        AutomaticEligibility::check(
            true,
            "acquisition",
            true,
            Some(&release),
            &mappings,
            &["t1".to_string(), "t2".to_string()],
        ),
        Err(AutomaticHold::MissingTrackMapping("t2".to_string()))
    );
    assert!(
        AutomaticEligibility::check(
            true,
            "acquisition",
            true,
            Some(&release),
            &mappings,
            &["t1".to_string()]
        )
        .is_ok()
    );
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

    let journals = JournalStore::new(publisher.connection())
        .bundle("bundle-1")
        .unwrap();
    assert_eq!(journals.len(), 3);
    for journal in journals {
        assert_eq!(journal.state, JournalState::Cleaned);
        assert!(journal.seq >= 4, "monotonic transitions recorded");
    }
    // The catalog the reads API serves follows the move in the same
    // transaction, and its revision bump invalidates read caches.
    let catalog = SqliteCatalog;
    assert_eq!(
        catalog.revision(publisher.connection()).unwrap(),
        start_revision() + 1
    );
    let located = catalog
        .locate(publisher.connection(), "track-a")
        .unwrap()
        .unwrap();
    assert_eq!(located.0, "music");
    assert_eq!(located.1, "organized/a.flac");
    let at_dest: i64 = publisher
        .connection()
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
        let (sealed, live) = seal(&bundle);
        let mut publisher = open_publisher(&fix);
        publisher.set_crash_point(Some(point));
        let err = publisher.publish(&sealed, &live, &docs).unwrap_err();
        assert!(
            matches!(err, PublishError::InjectedCrash(_)),
            "{point:?}: got {err:?}"
        );
        drop(publisher);

        let mut conn = open_db(&fix);
        let results = reconcile(&mut conn, &fix.sandbox, &SqliteCatalog).unwrap();
        assert_eq!(results.len(), 1, "{point:?}: one bundle reconciled");

        for item in bundle.items.iter() {
            let dest = fix
                .sandbox
                .resolve(&item.dest_root, &item.dest_rel)
                .unwrap();
            // Full staged bytes, never half: the planned title is in
            // the tags and the file still decodes.
            assert_staged(&dest, &item.track_id);
        }
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
            SqliteCatalog.revision(&conn).unwrap(),
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

    let mut conn = open_db(&fix);
    let results = reconcile(&mut conn, &fix.sandbox, &SqliteCatalog).unwrap();
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

/// Journal rows are synced to disk before the file moves they describe:
/// the publisher's connection syncs every commit, and recovery refuses a
/// connection that does not.
#[test]
fn journal_connections_sync_every_commit() {
    let fix = fixture("journal-full-sync");
    let synchronous: i64 = open_publisher(&fix)
        .connection()
        .query_row("PRAGMA synchronous", [], |row| row.get(0))
        .unwrap();
    assert_eq!(synchronous, 2);
    let mut unsynced = droppedneedle::db::open_connection(&fix.db_path).unwrap();
    assert!(reconcile(&mut unsynced, &fix.sandbox, &SqliteCatalog).is_err());
}

/// A publisher stays open for days. Each write is dated the day it is
/// made, so one made today is never listed as retried for more than a
/// day; one still unsettled two days on is. A committed write whose
/// cleanup is pending is finished and is not listed.
#[test]
fn a_write_counts_as_stuck_from_the_day_it_was_made() {
    use droppedneedle::library::management::recovery::retrying_bundles;
    use std::sync::atomic::{AtomicI64, Ordering};

    let fix = fixture("stuck-from-its-own-day");
    let day = std::sync::Arc::new(AtomicI64::new(50));
    let clock = std::sync::Arc::clone(&day);
    let mut publisher = Publisher::open(
        fix.sandbox.clone(),
        &fix.db_path,
        SqliteCatalog,
        FixedSpace(u64::MAX),
        move || clock.load(Ordering::Relaxed),
    )
    .unwrap();
    day.store(53, Ordering::Relaxed);
    let (bundle, docs) = two_track_bundle(&fix, PlanKind::Move);
    let (sealed, live) = seal(&bundle);
    publisher.set_crash_point(Some(CrashPoint::AfterStage));
    publisher.publish(&sealed, &live, &docs).unwrap_err();

    let conn = open_db(&fix);
    let stuck = |today: i64| -> Vec<String> {
        retrying_bundles(&conn, today)
            .unwrap()
            .into_iter()
            .map(|bundle| bundle.bundle_id)
            .collect()
    };
    assert!(stuck(54).is_empty(), "made yesterday: not yet a day old");
    assert_eq!(stuck(55), vec!["bundle-1".to_string()]);
    conn.execute(
        "UPDATE library_publish_journal SET state = 'cleanup_pending'",
        [],
    )
    .unwrap();
    assert!(stuck(55).is_empty(), "a committed write is finished");
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
    let plugin_temp = format!("{}plugin-1", publish::HIDDEN_PREFIX);
    std::fs::write(music.join("staging").join(plugin_temp), b"temp").unwrap();

    let conn = open_db(&fix);
    let roots = [Root {
        id: "music".to_string(),
        dir: fix.dir.join("music"),
    }];
    let now = std::time::SystemTime::now();
    let later = now + std::time::Duration::from_secs(2 * 86_400);
    let pass = |at| {
        publish::recovery::find_leftovers(&conn, &roots, at, &|| false)
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
fn foreign_bytes_at_dest_flag_attention_without_deleting() {
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

    let mut conn = open_db(&fix);
    let results = reconcile(&mut conn, &fix.sandbox, &SqliteCatalog).unwrap();
    assert_eq!(results.len(), 1);
    assert!(
        matches!(
            results[0].action,
            publish::RecoveryAction::NeedsAttention(_)
        ),
        "got {:?}",
        results[0].action
    );
    assert_eq!(std::fs::read(&occupier).unwrap(), b"external-file");
    assert_eq!(
        std::fs::read(fix.sandbox.resolve("music", "staging/a.flac").unwrap()).unwrap(),
        std::fs::read(fixtures_dir().join("management_full.flac")).unwrap()
    );
    assert_eq!(SqliteCatalog.revision(&conn).unwrap(), start_revision());
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

        let journals = JournalStore::new(publisher.connection())
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
            SqliteCatalog.revision(publisher.connection()).unwrap(),
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

        let mut conn = open_db(&fix);
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

        let results = reconcile(&mut conn, &fix.sandbox, &SqliteCatalog).unwrap();
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
        assert_eq!(SqliteCatalog.revision(&conn).unwrap(), start_revision() + 1);
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

        let mut conn = open_db(&fix);
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

        let results = reconcile(&mut conn, &fix.sandbox, &SqliteCatalog).unwrap();
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

    let journals = JournalStore::new(publisher.connection())
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

        let mut conn = open_db(&fix);
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

        let results = reconcile(&mut conn, &fix.sandbox, &SqliteCatalog).unwrap();
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
        assert_eq!(SqliteCatalog.revision(&conn).unwrap(), start_revision());
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
    // The catalog moves on while the files are placed, so the commit
    // fails with both files published and both originals set aside.
    let db_path = fix.db_path.clone();
    publisher.set_before_place(Some(Box::new(move |dest| {
        if dest == second_dest {
            let conn = droppedneedle::db::open_connection(&db_path).unwrap();
            conn.execute(
                "UPDATE library_catalog_revision SET value = value + 1 WHERE singleton = 1",
                [],
            )
            .unwrap();
        }
    })));
    publisher.set_crash_point(Some(CrashPoint::AfterFirstUnwind));
    let err = publisher.publish(&sealed, &live, &docs).unwrap_err();
    assert!(matches!(err, PublishError::InjectedCrash(_)), "got {err:?}");
    let journals = JournalStore::new(publisher.connection())
        .bundle("bundle-1")
        .unwrap();
    assert_eq!(journals[0].state, JournalState::Compensated);
    assert_eq!(journals[1].state, JournalState::Published);
    drop(publisher);

    let mut conn = open_db(&fix);
    let results = reconcile(&mut conn, &fix.sandbox, &SqliteCatalog).unwrap();
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
    let again = reconcile(&mut conn, &fix.sandbox, &SqliteCatalog).unwrap();
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

    let mut conn = open_db(&fix);
    let store = JournalStore::new(&conn);
    let first = store.bundle("bundle-1").unwrap().remove(0);
    // As that unwind leaves its row: the placed copy is off the name, the
    // original is still under its backup name.
    let first_dest = fix.sandbox.resolve("music", "staging/a.flac").unwrap();
    std::fs::remove_file(&first_dest).unwrap();
    assert!(
        store
            .transition(&first.id, first.state, JournalState::NeedsAttention)
            .unwrap()
    );

    let results = reconcile(&mut conn, &fix.sandbox, &SqliteCatalog).unwrap();
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

    let mut conn = open_db(&fix);
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

    let results = reconcile(&mut conn, &fix.sandbox, &SqliteCatalog).unwrap();
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

    let mut conn = open_db(&fix);
    let no_safe_step = Seams {
        skip_rename: true,
        link_errno: Some(libc::EPERM),
        ..Seams::default()
    };
    let results =
        reconcile_with_seams(&mut conn, &fix.sandbox, &SqliteCatalog, no_safe_step).unwrap();
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

    let mut conn = open_db(&fix);
    let results = reconcile(&mut conn, &fix.sandbox, &SqliteCatalog).unwrap();
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
    let again = reconcile(&mut conn, &fix.sandbox, &SqliteCatalog).unwrap();
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
        item.managed_updates.clear();
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
    let journals = JournalStore::new(publisher.connection())
        .bundle("bundle-1")
        .unwrap();
    assert_eq!(journals[1].state, JournalState::Compensated);
    assert!(std::fs::symlink_metadata(&journals[1].staged).is_err());
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
            matches!(&err, PublishError::NoSafePlacement(text) if text == publish::place::NO_SAFE_METHOD),
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
        let journals = JournalStore::new(publisher.connection())
            .bundle("bundle-1")
            .unwrap();
        assert!(
            journals
                .iter()
                .all(|journal| journal.state == JournalState::Compensated),
            "{case}"
        );
        assert_eq!(
            SqliteCatalog.revision(publisher.connection()).unwrap(),
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
            matches!(&err, PublishError::NotLinkable(text) if text.contains("\"staging/a.flac\"")),
            "{mode:?}: got {err:?}"
        );
        let journals = JournalStore::new(publisher.connection())
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

    let mut conn = open_db(&fix);
    let results = reconcile(&mut conn, &fix.sandbox, &SqliteCatalog).unwrap();
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
}

/// A generated file lands only on an empty path, and a later bundle takes
/// it away again (set aside, then deleted after the commit) only while it
/// still holds the bytes that were written.
#[test]
fn generated_files_land_and_go_away_journaled() {
    let fix = fixture("generated");
    let cover = b"generated-cover-bytes".to_vec();
    let sha = BlobStore::new(&open_db(&fix)).put(&cover).unwrap();
    let output = publish::GeneratedOutput {
        dest_root: "music".to_string(),
        dest_rel: "staging/cover.jpg".to_string(),
        sha256: sha.clone(),
        size: cover.len() as u64,
        kind: "artwork".to_string(),
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
        catalog_revision: db_catalog_revision(&fix),
    };
    let mut docs = BTreeMap::new();
    docs.insert("track-a".to_string(), TagDocument::empty());
    let (sealed, live) = seal(&bundle);
    let mut publisher = open_publisher(&fix);
    publisher.publish(&sealed, &live, &docs).unwrap();
    let path = fix.sandbox.resolve("music", "staging/cover.jpg").unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), cover);

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
    item.managed_updates.clear();
    item.removals.push(output);
    let bundle = PlanBundle {
        id: "bundle-undo".to_string(),
        items: vec![item],
        catalog_revision: db_catalog_revision(&fix),
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
    let mut conn = open_db(&fix);
    let results = reconcile(&mut conn, &fix.sandbox, &SqliteCatalog).unwrap();
    assert_eq!(results[0].action, publish::RecoveryAction::ResumedCommitted);
    assert!(!path.exists(), "gone after the commit");
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
    let sha = BlobStore::new(&open_db(&fix)).put(&cover).unwrap();
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
    item.managed_updates.clear();
    item.removals.push(publish::GeneratedOutput {
        dest_root: "music".to_string(),
        dest_rel: "staging/cover.jpg".to_string(),
        sha256: sha,
        size: cover.len() as u64,
        kind: "artwork".to_string(),
    });
    let bundle = PlanBundle {
        id: "bundle-undo".to_string(),
        items: vec![item],
        profile_revision: 7,
        naming_revision: 3,
        policy_revision: 5,
        catalog_revision: db_catalog_revision(&fix),
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

    let mut conn = open_db(&fix);
    let removal = JournalStore::new(&conn)
        .bundle("bundle-undo")
        .unwrap()
        .into_iter()
        .find(|journal| journal.kind == publish::JournalKind::Delete)
        .expect("removal journaled");
    assert!(!path.exists(), "set aside before the commit");
    std::fs::hard_link(&removal.staged, &path).unwrap();

    let results = reconcile(&mut conn, &fix.sandbox, &SqliteCatalog).unwrap();
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
        "genre".to_string(),
        vec!["Rock".to_string(), "Indie".to_string()],
    );
    before
        .unknown_frames
        .insert("TXXX:PRIV".to_string(), vec![0, 1, 2, 3]);
    before
        .unknown_frames
        .insert("APIC:odd".to_string(), vec![9, 9, 9]);

    let mut updates = BTreeMap::new();
    updates.insert("title".to_string(), vec!["New Title".to_string()]);

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
        catalog_revision: db_catalog_revision(&fix),
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
    doc.managed
        .insert("title".to_string(), vec!["Before".to_string()]);
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
    doc.managed
        .insert("title".to_string(), vec!["Original".to_string()]);
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
        format: "flac".to_string(),
        pinned_format: "flac".to_string(),
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
        format: "mp3".to_string(),
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
    let conn = open_db(&fix);
    let blobs = BlobStore::new(&conn);
    let first = blobs.put(b"same-bytes").unwrap();
    let second = blobs.put(b"same-bytes").unwrap();
    assert_eq!(first, second);
    assert_eq!(blobs.get(&first).unwrap(), b"same-bytes");

    let snapshots = SnapshotStore::new(&conn);
    snapshots.record("old", "b1", "t1", &first, 10, 20).unwrap();
    snapshots
        .record("fresh", "b1", "t2", &first, 40, 130)
        .unwrap();
    assert_eq!(snapshots.purge_expired(50).unwrap(), 1);
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
}

// --- Stage-8 fixup briefs: seal coverage, gate strictness, 4xx path
// hygiene, symlink refusal, resume backups, MP3 frame survival. ---

#[test]
fn seal_rejects_stale_catalog() {
    let fix = fixture("stale-catalog");
    let (bundle, docs) = two_track_bundle(&fix, PlanKind::Move);
    let (sealed, mut live) = seal(&bundle);
    live.catalog_revision = 9;
    let mut publisher = open_publisher(&fix);
    let err = publisher.publish(&sealed, &live, &docs).unwrap_err();
    assert!(matches!(err, PublishError::Validation(_)), "got {err:?}");
    assert!(matches!(
        sealed.recheck(&live),
        Err(SealError::StaleCatalog(_))
    ));
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
    // WMA has no staged writer and no artwork path.
    let mut wma = plan_item("t", "s.flac", "d.flac", fp.clone(), PlanKind::SamePath);
    wma.format = "wma".to_string();
    let err = gate.check(&wma).unwrap_err();
    assert!(matches!(err, PublishError::Capability(_)), "got {err:?}");
    assert!(err.to_string().contains("wma"), "{err}");
    let mut wma_art = plan_item("t", "s.flac", "d.flac", fp.clone(), PlanKind::SamePath);
    wma_art.format = "WMA".to_string();
    wma_art.capabilities = vec![Capability::Artwork];
    assert!(matches!(
        gate.check(&wma_art),
        Err(PublishError::Capability(_))
    ));
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
    let err = publish::paths::read_regular_file(&missing).unwrap_err();
    assert!(matches!(err, PublishError::Validation(_)), "got {err:?}");
    assert!(err.to_string().contains("gone.flac"), "{err}");
    assert!(!err.to_string().contains(&root_text), "{err}");
    #[cfg(unix)]
    {
        let target = fix.dir.join("real.bin");
        std::fs::write(&target, b"real").unwrap();
        let link = fix.sandbox.resolve("music", "alias.flac").unwrap();
        std::os::unix::fs::symlink(&target, &link).unwrap();
        let err = publish::paths::read_regular_file(&link).unwrap_err();
        assert!(matches!(err, PublishError::UnsafePath(_)), "got {err:?}");
        assert!(err.to_string().contains("alias.flac"), "{err}");
        assert!(!err.to_string().contains(&root_text), "{err}");
    }
    // Tag staging on garbage bytes: the hidden temp path stays home.
    let mut updates = BTreeMap::new();
    updates.insert("title".to_string(), vec!["X".to_string()]);
    let temp = fix.dir.join("w.tmp");
    let err = publish::staging::render_staged_bytes(&[0u8; 64], "flac", &updates, None, &temp)
        .unwrap_err();
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

    let mut conn = open_db(&fix);
    let results = reconcile(&mut conn, &fix.sandbox, &SqliteCatalog).unwrap();
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
    assert_eq!(SqliteCatalog.revision(&conn).unwrap(), start_revision());
}

#[cfg(unix)]
#[test]
fn committed_dest_swapped_for_symlink_refuses_without_touching_target() {
    // M5 cleanup hardening: a committed destination swapped for a
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

    let mut conn = open_db(&fix);
    let results = reconcile(&mut conn, &fix.sandbox, &SqliteCatalog).unwrap();
    assert!(
        matches!(&results[0].action, publish::RecoveryAction::Deferred(reason) if reason.contains("symlink")),
        "got {results:?}"
    );
    assert_eq!(std::fs::read(&canary).unwrap(), b"canary");
    let source = fix.sandbox.resolve("music", "staging/a.flac").unwrap();
    assert!(source.is_file(), "source was cleaned through a symlink");

    std::fs::remove_file(&dest).unwrap();
    std::fs::write(&dest, &staged_bytes).unwrap();
    let results = reconcile(&mut conn, &fix.sandbox, &SqliteCatalog).unwrap();
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

    let mut conn = open_db(&fix);
    let journaled = JournalStore::new(&conn).bundle("bundle-1").unwrap();
    assert!(journaled.iter().any(|journal| journal.backup.is_some()));
    let results = reconcile(&mut conn, &fix.sandbox, &SqliteCatalog).unwrap();
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
    assert_eq!(SqliteCatalog.revision(&conn).unwrap(), start_revision() + 1);
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
    let mut conn = open_db(&fix);
    let before = JournalStore::new(&conn).bundle("bundle-1").unwrap();
    assert_eq!(before.len(), 2);
    assert!(before.iter().all(|journal| journal.backup.is_none()));

    let results = reconcile(&mut conn, &fix.sandbox, &SqliteCatalog).unwrap();
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
    assert_eq!(SqliteCatalog.revision(&conn).unwrap(), start_revision() + 1);
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
        .insert("title".to_string(), vec![title.to_string()]);
    let bundle = PlanBundle {
        id: id.to_string(),
        items: vec![item],
        profile_revision: 7,
        naming_revision: 3,
        policy_revision: 5,
        catalog_revision: db_catalog_revision(fix),
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
        catalog_revision: db_catalog_revision(&fix),
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

    // The longest hidden name is the tag work copy: the temp's name with a
    // longer ending. It keeps the start of the file's name and stays short.
    let temp = fix
        .sandbox
        .temp_path_for(&dest, "bundle-long-retag-000-audio")
        .unwrap();
    let temp = temp.file_name().unwrap().to_string_lossy().into_owned();
    assert!(temp.contains(".n\u{e9}"), "{temp}");
    let work = temp.replace(".tmp", ".staging.flac");
    assert!(work.len() <= 128, "{} bytes", work.len());
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
        .temp_path_for(&dest, "bundle-link-000-audio")
        .unwrap();
    let work = temp.with_file_name(
        temp.file_name()
            .unwrap()
            .to_string_lossy()
            .replace(".tmp", ".staging.flac"),
    );
    let victims = [fix.dir.join("victim-temp"), fix.dir.join("victim-work")];
    for (victim, link) in victims.iter().zip([&temp, &work]) {
        std::fs::write(victim, b"theirs").unwrap();
        std::os::unix::fs::symlink(victim, link).unwrap();
    }
    let (sealed, live) = seal(&bundle);
    let mut publisher = open_publisher(&fix);
    publisher.publish(&sealed, &live, &docs).unwrap();

    for victim in &victims {
        assert_eq!(std::fs::read(victim).unwrap(), b"theirs");
    }
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

    let mut conn = open_db(&fix);
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
    let results = reconcile(&mut conn, &fix.sandbox, &SqliteCatalog).unwrap();
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
    item.format = "mp3".to_string();
    let bundle = PlanBundle {
        id: "bundle-mp3".to_string(),
        items: vec![item],
        profile_revision: 7,
        naming_revision: 3,
        policy_revision: 5,
        catalog_revision: db_catalog_revision(&fix),
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
