//! Publisher slice briefs: seal, crash matrix, preservation, archive safety.
//!
//! Each brief pins one stage-8 behavior against a scratch sandbox and a
//! throwaway SQLite database: sealed previews reject stale state,
//! crash injection at every phase resumes or compensates without
//! half-renames or occupied overwrites, unknown tags and frames
//! survive Apply, and hostile archives never extract. The publisher
//! writes only under sandbox roots; nothing here touches real media.
//!
//! The slice is not wired into the crate yet, so the briefs include it
//! by path. Tag staging goes through the real tags slice save
//! wrapper (stage-8 integration); publish-path briefs plant real
//! committed fixtures, never fake bytes with audio extensions.

#[path = "../src/library/publish/mod.rs"]
#[allow(dead_code, unused_imports)]
mod publish;

#[path = "../src/library/tags/mod.rs"]
#[allow(dead_code, unused_imports)]
mod tags;

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use publish::paths::Root;
use publish::planner::{
    Capability, CapabilityGate, CollisionGate, FileFingerprint, PlanBundle, PlanItem, PlanKind,
    ReleaseIdentity, SealError, SealRecheck, SealedPreview, SidecarPlan, SpaceProbe, TrackMapping,
};
use publish::snapshots::{BaselineStore, BlobStore, SnapshotStore, sha256_hex};
use publish::tags_seam::TagDocument;
use publish::undo::{
    BaselineBlock, BaselineInput, BeforeState, UndoBlock, UndoInput, UndoLive,
    confirm_baseline_purge, plan_baseline_restore, plan_undo,
};
use publish::{
    ArchiveEntry, ArchivePolicy, AutomaticEligibility, AutomaticHold, Catalog, CrashPoint,
    JournalState, JournalStore, PublishError, PublishOutcome, Publisher, Sandbox, SqliteCatalog,
    apply_schema, reconcile, validate_archive,
};
use rusqlite::Connection;

static COUNTER: AtomicU64 = AtomicU64::new(0);

fn scratch_dir(tag: &str) -> PathBuf {
    let id = COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!(
        "dn-library-publish-{}-{}-{id}",
        std::process::id(),
        tag
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
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
}

fn fixture(tag: &str) -> Fixture {
    let dir = scratch_dir(tag);
    let music = dir.join("music");
    let incoming = dir.join("incoming");
    std::fs::create_dir_all(&music).unwrap();
    std::fs::create_dir_all(&incoming).unwrap();
    let meta = music.join("meta");
    std::fs::create_dir_all(&meta).unwrap();
    let sandbox = Sandbox::new(
        vec![
            Root {
                id: "music".to_string(),
                dir: music,
            },
            Root {
                id: "incoming".to_string(),
                dir: incoming,
            },
        ],
        meta.clone(),
    )
    .unwrap();
    let db_path = meta.join("publish.db");
    Fixture {
        dir,
        sandbox,
        db_path,
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
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../backend/tests/fixtures/library")
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
        catalog_revision: 0,
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
        fix.db_path.clone(),
        SqliteCatalog,
        FixedSpace(u64::MAX),
        50,
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
    let catalog = SqliteCatalog;
    assert_eq!(catalog.revision(publisher.connection()).unwrap(), 1);
    let located = catalog
        .locate(publisher.connection(), "track-a")
        .unwrap()
        .unwrap();
    assert_eq!(located.0, "music");
    assert_eq!(located.1, "organized/a.flac");
    let invalidations: i64 = publisher
        .connection()
        .query_row("SELECT COUNT(*) FROM publish_invalidations", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(invalidations, 2);
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

        let mut conn = Connection::open(&fix.db_path).unwrap();
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
            1,
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

    let mut conn = Connection::open(&fix.db_path).unwrap();
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

    let mut conn = Connection::open(&fix.db_path).unwrap();
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
    assert_eq!(SqliteCatalog.revision(&conn).unwrap(), 0);
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
        catalog_revision: 0,
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
fn sandbox_rejects_escape_symlink_and_foreign_db() {
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

    let foreign_db = scratch_dir("foreign-db").join("publish.db");
    assert!(
        Publisher::open(
            fix.sandbox.clone(),
            foreign_db,
            SqliteCatalog,
            FixedSpace(u64::MAX),
            50,
        )
        .is_err()
    );
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
    let dir = scratch_dir("snapshots");
    let conn = Connection::open(dir.join("snap.db")).unwrap();
    apply_schema(&conn).unwrap();
    let blobs = BlobStore::open(&dir).unwrap();
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
    for capability in [
        Capability::Genre,
        Capability::Rename,
        Capability::Sidecars,
        Capability::Scrub,
    ] {
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
    let err =
        publish::staging::render_staged_bytes(&[0u8; 64], "flac", &updates, &temp).unwrap_err();
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

    let mut conn = Connection::open(&fix.db_path).unwrap();
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
    assert_eq!(SqliteCatalog.revision(&conn).unwrap(), 0);
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

    let mut conn = Connection::open(&fix.db_path).unwrap();
    let err = reconcile(&mut conn, &fix.sandbox, &SqliteCatalog).unwrap_err();
    assert!(matches!(err, PublishError::UnsafePath(_)), "got {err:?}");
    assert_eq!(std::fs::read(&canary).unwrap(), b"canary");
    let source = fix.sandbox.resolve("music", "staging/a.flac").unwrap();
    assert!(source.is_file(), "source was cleaned through a symlink");

    std::fs::remove_file(&dest).unwrap();
    std::fs::write(&dest, &staged_bytes).unwrap();
    let results = reconcile(&mut conn, &fix.sandbox, &SqliteCatalog).unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].action, publish::RecoveryAction::CleanupFinished);
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
    let mut conn = Connection::open(&fix.db_path).unwrap();
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
    assert_eq!(SqliteCatalog.revision(&conn).unwrap(), 1);
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
        catalog_revision: 0,
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
