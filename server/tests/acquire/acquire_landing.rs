//! Download import journey: a finished download is verified, matched to
//! the requested release, published into the library, and its request
//! resolved; a download whose files are not the release is held for
//! review, fails over to the next candidate, and settles held once the
//! candidates run out. Real worker, landing, publisher and catalog over
//! one scratch database; MusicBrainz is a scripted release source.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use droppedneedle_acquire::db::AcquireDb;
use droppedneedle_acquire::downloads::SourceTag;
use droppedneedle_acquire::downloads::journal::Journal;
use droppedneedle_acquire::downloads::landing_rows::ImportDecisionRow;
use droppedneedle_acquire::downloads::queue_rows::Viewer as HeldViewer;
use droppedneedle_acquire::downloads::sources::SourceHandle;
use droppedneedle_acquire::downloads::state::{AttemptState, TaskStatus};
use droppedneedle_acquire::downloads::store::{NewTask, TaskOutcome};
use droppedneedle_acquire::downloads::worker::{
    DownloadWorker, FixedSource, WorkerConfig, run_startup_recovery,
};
use droppedneedle_acquire::downloads::{PlannedAlbum, ReplaceVersion, TaskPlan};
use droppedneedle_acquire::intake::{self, Gate};
use droppedneedle_acquire::landing::library::LibraryLanding;
use droppedneedle_acquire::landing::ports::{
    ImportFailure, ImportReceipt, ImportRequest, LandingLibrary, OwnedCopy, OwnedTracks, VersionRow,
};
use droppedneedle_acquire::landing::specs::{Disposition, Rejection};
use droppedneedle_acquire::landing::{Landed, LandingReport, LandingResult, Reported};
use droppedneedle_acquire::reason::{Reason, ReasonKind};
use droppedneedle_auth::session::store::SessionKind;
use droppedneedle_auth::{Principal, Role};
use droppedneedle_kernel::ids::UuidGenerator;
use droppedneedle_kernel::runtime_config::{ConfigStore, Crypto};
use droppedneedle_library::identify::models::LocalTrackFacts;
use droppedneedle_library::identify::sources::{
    AudioMatches, Edition, EditionPage, EditionQuery, FingerprintSource, ReleaseHit, ReleaseSource,
    SourceError,
};
use droppedneedle_library::matching::{CreditedArtist, Release, ReleaseMedium, ReleaseTrack};
use droppedneedle_library::scan::EffectivePolicy;
use droppedneedle_library::tags::TagField;
use droppedneedle_library::tags::save::{TagEdit, save_tags};
use droppedneedle_library::wiring::LibrarySetup;
use droppedneedle_providers::RequestPriority;
use droppedneedle_providers::musicbrainz::is_valid_mbid;
use futures_util::future::BoxFuture;

use crate::common::TestPlatform;
use crate::common::auth::{AuthRig, test_principal, with_test_principal};
use crate::common::library::TestSql as _;
use crate::common::library::{every_priority, library_at};

const GOOD_GROUP: &str = "6d0b7a1e-0000-4000-8000-000000000001";
const GOOD_RELEASE: &str = "6d0b7a1e-0000-4000-8000-000000000002";
const BAD_GROUP: &str = "6d0b7a1e-0000-4000-8000-000000000003";
const BAD_RELEASE: &str = "6d0b7a1e-0000-4000-8000-000000000004";

/// MusicBrainz as a fixed set of releases: every search returns them all.
pub(crate) struct Releases(pub(crate) Vec<Release>);

impl ReleaseSource for Releases {
    fn search<'a>(
        &'a self,
        _title: &'a str,
        _artist: &'a str,
    ) -> BoxFuture<'a, Result<Vec<ReleaseHit>, SourceError>> {
        let hits = self
            .0
            .iter()
            .map(|release| ReleaseHit {
                id: release.id.clone(),
                release_group_id: release.release_group_id.clone(),
                score: 100,
                track_count: Some(release.tracks.len() as u32),
                title: release.title.clone(),
            })
            .collect();
        Box::pin(async move { Ok(hits) })
    }

    fn release<'a>(&'a self, mbid: &'a str) -> BoxFuture<'a, Result<Option<Release>, SourceError>> {
        let found = self
            .0
            .iter()
            .find(|release| release.answers_to(mbid))
            .cloned();
        Box::pin(async move { Ok(found) })
    }

    fn canonical_recording<'a>(
        &'a self,
        _mbid: &'a str,
    ) -> BoxFuture<'a, Result<Option<String>, SourceError>> {
        Box::pin(async { Ok(None) })
    }
}

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/library")
        .join(name)
}

/// A landed file: a fixture copy tagged as one track, with no
/// MusicBrainz ids of its own.
pub(crate) fn land_file(
    dir: &Path,
    name: &str,
    fixture_name: &str,
    title: &str,
    track: u32,
) -> PathBuf {
    std::fs::create_dir_all(dir).expect("download dir");
    let path = dir.join(name);
    std::fs::copy(fixture(fixture_name), &path).expect("copy fixture");
    let mut edits = vec![
        TagEdit::new(TagField::Title, vec![title.to_owned()]),
        TagEdit::new(TagField::Artist, vec!["Portishead".to_owned()]),
        TagEdit::new(TagField::AlbumArtist, vec!["Portishead".to_owned()]),
        TagEdit::new(TagField::Album, vec!["Dummy".to_owned()]),
        TagEdit::new(TagField::TrackNumber, vec![track.to_string()]),
        TagEdit::new(TagField::DiscNumber, vec!["1".to_owned()]),
    ];
    for field in [
        TagField::MusicBrainzRecordingId,
        TagField::MusicBrainzReleaseTrackId,
        TagField::MusicBrainzReleaseId,
        TagField::MusicBrainzReleaseGroupId,
    ] {
        edits.push(TagEdit::new(field, Vec::new()));
    }
    save_tags(&path, &edits).expect("tags save");
    path
}

pub(crate) fn length_ms(path: &Path) -> Option<u64> {
    let format = droppedneedle_library::tags::format_for_path(path).expect("format");
    let (_, header) =
        droppedneedle_library::tags::read::read_scan_metadata(path, format).expect("header");
    header
        .duration_seconds
        .map(|seconds| (seconds * 1000.0) as u64)
}

pub(crate) fn release(id: &str, group: &str, titles: &[&str], lengths: &[Option<u64>]) -> Release {
    let artist = CreditedArtist {
        id: "8f6bd1e4-fbe1-4f50-aa9b-94c450ec0f11".to_owned(),
        name: "Portishead".to_owned(),
        sort_name: Some("Portishead".to_owned()),
        join: String::new(),
    };
    Release {
        id: id.to_owned(),
        release_group_id: group.to_owned(),
        title: "Dummy".to_owned(),
        artists: vec![artist],
        status: Some("Official".to_owned()),
        media: vec![ReleaseMedium {
            position: 1,
            format: Some("CD".to_owned()),
            title: None,
            track_count: titles.len() as u32,
        }],
        tracks: titles
            .iter()
            .zip(lengths)
            .enumerate()
            .map(|(index, (title, length))| ReleaseTrack {
                isrcs: Vec::new(),
                genres: Vec::new(),
                id: format!("{}{:x}", &id[..id.len() - 1], 8 + index),
                recording_id: format!("{}{:x}", &group[..group.len() - 1], 8 + index),
                title: (*title).to_owned(),
                artists: Vec::new(),
                disc: 1,
                position: index as u32 + 1,
                absolute_position: index as u32 + 1,
                length_ms: *length,
            })
            .collect(),
        ..Release::default()
    }
}

fn new_task(id: &str, group: &str) -> NewTask {
    NewTask {
        id: id.to_owned(),
        user_id: "u-ada".to_owned(),
        artist_name: "Portishead".to_owned(),
        album_title: "Dummy".to_owned(),
        release_group_mbid: group.to_owned(),
        origin: "user".to_owned(),
        retry_count: 0,
    }
}

#[tokio::test]
async fn finished_downloads_land_in_the_library_or_wait_for_review() {
    let db = AcquireDb::scratch().expect("scratch db");
    db.add_user("u-ada", "Ada", "user").await.expect("user");
    let dir = db.path().parent().expect("scratch dir").to_path_buf();
    let config = Arc::new(
        ConfigStore::scratch(&dir, Crypto::from_key_bytes(&[7u8; 32]).expect("key"))
            .expect("config"),
    );
    let platform = TestPlatform::on(db.db(), &dir)
        .settings(config)
        .build()
        .await;
    let (library, _) = library_at(&platform);
    let music = dir.join("music");
    std::fs::create_dir_all(&music).expect("music root");
    library
        .scans
        .add_root(
            Some("music".to_owned()),
            music.to_string_lossy().into_owned(),
            EffectivePolicy::Automatic,
        )
        .expect("root adds");

    let downloads = dir.join("downloads");
    let good = vec![
        land_file(
            &downloads.join("good"),
            "01 - Mysterons.flac",
            "flac_full_01.flac",
            "Mysterons",
            1,
        ),
        land_file(
            &downloads.join("good"),
            "02 - Sour Times.flac",
            "flac_full_02.flac",
            "Sour Times",
            2,
        ),
    ];
    let bad = vec![
        land_file(
            &downloads.join("bad"),
            "01.flac",
            "flac_full_01.flac",
            "Glory Box",
            1,
        ),
        land_file(
            &downloads.join("bad"),
            "02.flac",
            "flac_full_02.flac",
            "Roads",
            2,
        ),
    ];
    let lengths: Vec<Option<u64>> = good.iter().map(|path| length_ms(path)).collect();
    let releases: Arc<dyn ReleaseSource> = Arc::new(Releases(vec![
        release(
            GOOD_RELEASE,
            GOOD_GROUP,
            &["Mysterons", "Sour Times"],
            &lengths,
        ),
        release(
            BAD_RELEASE,
            BAD_GROUP,
            &["It Could Be Sweet", "Wandering Star"],
            &lengths,
        ),
    ]));
    let acquire = acquire_over(
        &db,
        Some(Arc::new(LibraryLanding::new(
            &library,
            every_priority(releases),
        ))),
    );
    let worker = |paths: Vec<PathBuf>| {
        Arc::new(
            DownloadWorker::fixed(
                acquire.journal.clone(),
                vec![crate::as_source(Arc::new(FixedSource::new(
                    downloads.clone(),
                    paths,
                )))],
                WorkerConfig::default(),
                droppedneedle_kernel::events::EventHub::new(),
                Default::default(),
            )
            .with_landing(acquire.landing.clone()),
        )
    };

    // The good download, with the album request it serves.
    acquire
        .journal
        .run("test.seed", |store| {
            store.insert_task(&new_task("t-good", GOOD_GROUP), 1.0)
        })
        .await
        .expect("task");
    acquire
        .db
        .write("test.request", |tx| {
            tx.execute(
                "INSERT INTO request_history (musicbrainz_id_lower, musicbrainz_id, artist_name, \
                 album_title, requested_at, status, user_id, download_task_id, request_kind) \
                 VALUES (?1, ?1, 'Portishead', 'Dummy', 1700000000000, 'downloading', 'u-ada', \
                 't-good', 'album')",
                [GOOD_GROUP],
            )?;
            Ok(())
        })
        .await
        .expect("request");
    let good_worker = worker(good.clone());
    good_worker.run_once(1).await;
    good_worker.wait_for_landings().await;

    let task = acquire.journal.read_task("t-good").await.unwrap().unwrap();
    assert_eq!(
        task.status,
        TaskStatus::Completed,
        "{:?}",
        task.error_message
    );
    let placed = music.join("Portishead/Dummy/0101 Mysterons.flac");
    assert!(placed.is_file(), "published by the naming template");
    assert!(
        music
            .join("Portishead/Dummy/0102 Sour Times.flac")
            .is_file()
    );
    assert!(
        good.iter().all(|path| !path.exists()),
        "sources leave the download dir"
    );
    let (tracks, sealed): (i64, i64) = sqlx::query_as(
        "SELECT COUNT(*), SUM(i.release_mbid = ?1) FROM local_tracks t \
         JOIN local_album_external_identities i ON i.local_album_id = t.local_album_id \
         WHERE t.availability = 'indexed'",
    )
    .bind(GOOD_RELEASE)
    .fetch_one(acquire.db.db().sqlx())
    .await
    .expect("catalog");
    assert_eq!(
        (tracks, sealed),
        (2, 2),
        "in the catalog with the matched identity"
    );
    let request: String =
        sqlx::query_scalar("SELECT status FROM request_history WHERE musicbrainz_id_lower = ?1")
            .bind(GOOD_GROUP)
            .fetch_one(acquire.db.db().sqlx())
            .await
            .expect("request row");
    assert_eq!(request, "imported");
    assert!(
        acquire.flows.library.contains(GOOD_GROUP).await,
        "the wanted watcher sees the album as owned"
    );

    // A download whose files name other tracks is held, not imported.
    acquire
        .journal
        .run("test.seed", |store| {
            store.insert_task(&new_task("t-bad", BAD_GROUP), 2.0)
        })
        .await
        .expect("task");
    // Each candidate lands the same files and is held; once the failover
    // attempts are spent the task settles held.
    let bad_worker = worker(bad.clone());
    for pass in 2..10 {
        bad_worker.run_once(pass).await;
        bad_worker.wait_for_landings().await;
        let task = acquire.journal.read_task("t-bad").await.unwrap().unwrap();
        if task.status.is_terminal() {
            break;
        }
    }

    let task = acquire.journal.read_task("t-bad").await.unwrap().unwrap();
    assert_eq!(task.status, TaskStatus::Failed);
    assert!(
        task.error_message
            .as_deref()
            .unwrap_or_default()
            .starts_with("Held for review"),
        "{:?}",
        task.error_message
    );
    let held: Vec<String> = sqlx::query_scalar(
        "SELECT path FROM acquire_held_files WHERE source_task_id = 't-bad' AND state = 'held'",
    )
    .fetch_all(acquire.db.db().sqlx())
    .await
    .expect("held rows");
    assert_eq!(held.len(), 2);
    let held_dir = &acquire.held.folders().held;
    assert!(held.iter().all(|path| held_dir.join(path).is_file()));
    let outcomes: Vec<String> =
        sqlx::query_scalar("SELECT outcome FROM download_import_decisions WHERE task_id = 't-bad'")
            .fetch_all(acquire.db.db().sqlx())
            .await
            .expect("decisions");
    assert!(outcomes.len() > 1, "failed over: {outcomes:?}");
    assert!(outcomes.iter().all(|outcome| outcome == "held"));
    // Every hold and decision stores a reason this version knows, and the
    // task's outcome is that hold, typed: the sentence comes from it.
    let codes: Vec<String> = sqlx::query_scalar(
        "SELECT reason FROM acquire_held_files WHERE source_task_id = 't-bad' \
         UNION ALL SELECT reason_code FROM download_import_decisions WHERE task_id = 't-bad'",
    )
    .fetch_all(acquire.db.db().sqlx())
    .await
    .expect("reason codes");
    assert!(
        codes.iter().all(|code| Reason::from_code(code).is_some()),
        "{codes:?}"
    );
    assert!(
        task.outcome_code
            .is_some_and(|reason| reason.kind() == ReasonKind::Hold),
        "{:?}",
        task.outcome_code
    );
    assert!(!acquire.flows.library.contains(BAD_GROUP).await);
}

/// MusicBrainz as [`Releases`], where one retired recording id was merged
/// into another. Like the live client, it refuses an id that is not a
/// MusicBrainz id.
struct Merged {
    releases: Releases,
    retired: &'static str,
    kept: String,
}

impl ReleaseSource for Merged {
    fn search<'a>(
        &'a self,
        title: &'a str,
        artist: &'a str,
    ) -> BoxFuture<'a, Result<Vec<ReleaseHit>, SourceError>> {
        self.releases.search(title, artist)
    }

    fn release<'a>(&'a self, mbid: &'a str) -> BoxFuture<'a, Result<Option<Release>, SourceError>> {
        self.releases.release(mbid)
    }

    fn canonical_recording<'a>(
        &'a self,
        mbid: &'a str,
    ) -> BoxFuture<'a, Result<Option<String>, SourceError>> {
        let answer = if !is_valid_mbid(mbid) {
            Err(SourceError("not a MusicBrainz id".to_owned()))
        } else if mbid == self.retired {
            Ok(Some(self.kept.clone()))
        } else {
            Ok(Some(mbid.to_owned()))
        };
        Box::pin(async move { answer })
    }
}

/// A recording id MusicBrainz merged into the second track's recording.
const RETIRED: &str = "6d0b7a1e-0000-4000-8000-0000000000ff";

/// Land a two-file download of Dummy whose first file carries the
/// release's own recording id and whose second carries `second`, one
/// worker pass. Answers the bundle, the music folder and the platform it
/// runs on.
async fn land_with_recording(
    task_id: &'static str,
    second: &str,
) -> (
    droppedneedle_acquire::AcquireSetup,
    PathBuf,
    droppedneedle_kernel::platform::Platform,
) {
    let db = AcquireDb::scratch().expect("scratch db");
    db.add_user("u-ada", "Ada", "user").await.expect("user");
    let dir = db.path().parent().expect("scratch dir").to_path_buf();
    let config = Arc::new(
        ConfigStore::scratch(&dir, Crypto::from_key_bytes(&[7u8; 32]).expect("key"))
            .expect("config"),
    );
    let platform = TestPlatform::on(db.db(), &dir)
        .settings(config)
        .build()
        .await;
    let (library, _) = library_at(&platform);
    let music = dir.join("music");
    std::fs::create_dir_all(&music).expect("music root");
    library
        .scans
        .add_root(
            Some("music".to_owned()),
            music.to_string_lossy().into_owned(),
            EffectivePolicy::Automatic,
        )
        .expect("root adds");

    let downloads = dir.join("downloads");
    let files = vec![
        land_file(
            &downloads.join("dummy"),
            "01 - Mysterons.flac",
            "flac_full_01.flac",
            "Mysterons",
            1,
        ),
        land_file(
            &downloads.join("dummy"),
            "02 - Sour Times.flac",
            "flac_full_02.flac",
            "Sour Times",
            2,
        ),
    ];
    let lengths: Vec<Option<u64>> = files.iter().map(|path| length_ms(path)).collect();
    let good = release(
        GOOD_RELEASE,
        GOOD_GROUP,
        &["Mysterons", "Sour Times"],
        &lengths,
    );
    let tagged = [good.tracks[0].recording_id.as_str(), second];
    for (path, recording) in files.iter().zip(tagged) {
        let edit = TagEdit::new(TagField::MusicBrainzRecordingId, vec![recording.to_owned()]);
        save_tags(path, &[edit]).expect("recording id saves");
    }
    let releases: Arc<dyn ReleaseSource> = Arc::new(Merged {
        kept: good.tracks[1].recording_id.clone(),
        releases: Releases(vec![good]),
        retired: RETIRED,
    });
    let acquire = acquire_over(
        &db,
        Some(Arc::new(LibraryLanding::new(
            &library,
            every_priority(releases),
        ))),
    );
    acquire
        .journal
        .run("test.seed", |store| {
            store.insert_task(&new_task(task_id, GOOD_GROUP), 1.0)
        })
        .await
        .expect("task");
    let worker = Arc::new(
        DownloadWorker::fixed(
            acquire.journal.clone(),
            vec![crate::as_source(Arc::new(FixedSource::new(
                downloads.clone(),
                files.clone(),
            )))],
            WorkerConfig::default(),
            droppedneedle_kernel::events::EventHub::new(),
            Default::default(),
        )
        .with_landing(acquire.landing.clone()),
    );
    worker.run_once(1).await;
    worker.wait_for_landings().await;
    (acquire, music, platform)
}

/// A download tagged long ago carries a recording id MusicBrainz has since
/// merged into the release's recording. The landing follows the merge and
/// imports; an unresolved id would veto the release and hold the files.
#[tokio::test]
async fn a_download_with_a_merged_recording_id_imports() {
    let (acquire, music, _platform) = land_with_recording("t-merged", RETIRED).await;
    let task = acquire
        .journal
        .read_task("t-merged")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        task.status,
        TaskStatus::Completed,
        "{:?}",
        task.error_message
    );
    assert!(
        music
            .join("Portishead/Dummy/0102 Sour Times.flac")
            .is_file()
    );
}

/// A recording id tag that is not a MusicBrainz id is never looked up: it
/// contradicts the release, so the download is held as a tag mismatch
/// and fails over, instead of waiting on MusicBrainz for ever.
#[tokio::test]
async fn a_download_with_a_junk_recording_id_is_held() {
    let (acquire, _music, _platform) = land_with_recording("t-junk", "not-an-id").await;
    let decisions: Vec<(String, Option<String>)> = sqlx::query_as(
        "SELECT outcome, reason_code FROM download_import_decisions WHERE task_id = 't-junk'",
    )
    .fetch_all(acquire.db.db().sqlx())
    .await
    .expect("decisions");
    assert_eq!(
        decisions,
        vec![("held".to_owned(), Some("tag_mismatch".to_owned()))]
    );
}

/// Remove a landed file's names and numbers, as a rip that was never
/// tagged comes: only the file name and its folders say what it is.
fn untag(path: &Path) {
    let edits: Vec<TagEdit> = [
        TagField::Title,
        TagField::Artist,
        TagField::AlbumArtist,
        TagField::Album,
        TagField::TrackNumber,
        TagField::DiscNumber,
    ]
    .into_iter()
    .map(|field| TagEdit::new(field, Vec::new()))
    .collect();
    save_tags(path, &edits).expect("tags clear");
}

/// Land `files` for `task`, matched against the release `release` builds
/// from the files' lengths, in one worker pass. Answers the bundle, the
/// music folder and the platform it runs on.
async fn land_for(
    task: NewTask,
    downloads: impl FnOnce(&Path) -> Vec<PathBuf>,
    release: impl FnOnce(&[Option<u64>]) -> Release,
) -> (
    droppedneedle_acquire::AcquireSetup,
    PathBuf,
    droppedneedle_kernel::platform::Platform,
) {
    land_reported(task, &[], downloads, release).await
}

/// [`land_for`], where the client reported the files under the remote
/// names `remote` (as Soulseek does). With names, the landing runs once
/// directly, for an attempt whose journaled handle records them.
async fn land_reported(
    task: NewTask,
    remote: &[&str],
    downloads: impl FnOnce(&Path) -> Vec<PathBuf>,
    release: impl FnOnce(&[Option<u64>]) -> Release,
) -> (
    droppedneedle_acquire::AcquireSetup,
    PathBuf,
    droppedneedle_kernel::platform::Platform,
) {
    land_through(task, remote, downloads, |lengths| {
        Arc::new(Releases(vec![release(lengths)]))
    })
    .await
}

/// [`land_reported`] over the MusicBrainz `source` builds from the files'
/// lengths.
async fn land_through(
    task: NewTask,
    remote: &[&str],
    downloads: impl FnOnce(&Path) -> Vec<PathBuf>,
    source: impl FnOnce(&[Option<u64>]) -> Arc<dyn ReleaseSource>,
) -> (
    droppedneedle_acquire::AcquireSetup,
    PathBuf,
    droppedneedle_kernel::platform::Platform,
) {
    land_heard(task, remote, downloads, source, None).await
}

/// [`land_through`], with AcoustID answering from `heard` when given.
async fn land_heard(
    task: NewTask,
    remote: &[&str],
    downloads: impl FnOnce(&Path) -> Vec<PathBuf>,
    source: impl FnOnce(&[Option<u64>]) -> Arc<dyn ReleaseSource>,
    heard: Option<Arc<dyn FingerprintSource>>,
) -> (
    droppedneedle_acquire::AcquireSetup,
    PathBuf,
    droppedneedle_kernel::platform::Platform,
) {
    let db = AcquireDb::scratch().expect("scratch db");
    db.add_user("u-ada", "Ada", "user").await.expect("user");
    let dir = db.path().parent().expect("scratch dir").to_path_buf();
    let config = Arc::new(
        ConfigStore::scratch(&dir, Crypto::from_key_bytes(&[7u8; 32]).expect("key"))
            .expect("config"),
    );
    let platform = TestPlatform::on(db.db(), &dir)
        .settings(config)
        .build()
        .await;
    let (mut library, _) = library_at(&platform);
    if let Some(heard) = heard {
        library.imports.fingerprints = heard;
    }
    let music = dir.join("music");
    std::fs::create_dir_all(&music).expect("music root");
    library
        .scans
        .add_root(
            Some("music".to_owned()),
            music.to_string_lossy().into_owned(),
            EffectivePolicy::Automatic,
        )
        .expect("root adds");
    let downloads_dir = dir.join("downloads");
    let files = downloads(&downloads_dir);
    let lengths: Vec<Option<u64>> = files.iter().map(|path| length_ms(path)).collect();
    let releases = source(&lengths);
    let acquire = acquire_over(
        &db,
        Some(Arc::new(LibraryLanding::new(
            &library,
            every_priority(releases),
        ))),
    );
    let task_id = task.id.clone();
    acquire
        .journal
        .run("test.seed", move |store| store.insert_task(&task, 1.0))
        .await
        .expect("task");
    if !remote.is_empty() {
        let handle = serde_json::json!({
            "source": "soulseek", "username": "peer", "filenames": remote, "job_name": "",
        })
        .to_string();
        let attempt = format!("{task_id}-a0");
        let (row_task, row_attempt) = (task_id.clone(), attempt.clone());
        acquire
            .journal
            .run("test.attempt", move |store| {
                store.insert_attempt(
                    &row_attempt,
                    &row_task,
                    "soulseek",
                    0,
                    "",
                    &handle,
                    AttemptState::InUse,
                    1.0,
                )
            })
            .await
            .expect("attempt");
        let row = acquire.journal.read_task(&task_id).await.unwrap().unwrap();
        let landed = Landed {
            mounts: vec![downloads_dir.clone().into()],
            files: files.into_iter().map(Reported::path).collect(),
        };
        acquire
            .landing
            .land(&row, Some(&attempt), landed, false)
            .await;
        return (acquire, music, platform);
    }
    let worker = Arc::new(
        DownloadWorker::fixed(
            acquire.journal.clone(),
            vec![crate::as_source(Arc::new(FixedSource::new(
                downloads_dir,
                files,
            )))],
            WorkerConfig::default(),
            droppedneedle_kernel::events::EventHub::new(),
            Default::default(),
        )
        .with_landing(acquire.landing.clone()),
    );
    worker.run_once(1).await;
    worker.wait_for_landings().await;
    (acquire, music, platform)
}

/// The landing decisions recorded for `task_id`: outcome and reason.
async fn decisions(
    acquire: &droppedneedle_acquire::AcquireSetup,
    task_id: &str,
) -> Vec<(String, Option<String>)> {
    sqlx::query_as(
        "SELECT outcome, reason_code FROM download_import_decisions WHERE task_id = ?1 \
         ORDER BY decided_at",
    )
    .bind(task_id)
    .fetch_all(acquire.db.db().sqlx())
    .await
    .expect("decisions")
}

/// The July wrong single, untagged: a file named like the requested
/// single, as long as it, from a folder that names another artist. With
/// no artist tag, no artist in its path, no fingerprint and one file, the
/// request's own names are all that agree, so it is held.
#[tokio::test]
async fn an_untagged_file_without_artist_evidence_is_held() {
    let task = NewTask {
        artist_name: "Yan Qing".to_owned(),
        album_title: "The Arrival".to_owned(),
        ..new_task("t-untagged-wrong", GOOD_GROUP)
    };
    let (acquire, music, _platform) = land_for(
        task,
        |downloads| {
            let path = land_file(
                &downloads.join("Dan Romer - Original Score"),
                "01 - The Arrival.flac",
                "flac_full_01.flac",
                "The Arrival",
                1,
            );
            untag(&path);
            vec![path]
        },
        |lengths| {
            let mut single = release(GOOD_RELEASE, GOOD_GROUP, &["The Arrival"], lengths);
            single.title = "The Arrival".to_owned();
            single.artists[0].name = "Yan Qing".to_owned();
            single
        },
    )
    .await;
    assert_eq!(
        decisions(&acquire, "t-untagged-wrong").await,
        vec![("held".to_owned(), Some("no_artist_evidence".to_owned()))]
    );
    assert!(files_under(&music, "flac").is_empty(), "nothing imported");
}

/// An untagged album in the usual "Artist - Album" folder: the folder
/// names the artist, so it imports.
#[tokio::test]
async fn an_untagged_album_in_its_artists_folder_imports() {
    let (acquire, music, _platform) = land_for(
        new_task("t-untagged-right", GOOD_GROUP),
        |downloads| {
            let folder = downloads.join("Portishead - Dummy (1994) [FLAC]");
            let files = vec![
                land_file(
                    &folder,
                    "01 - Mysterons.flac",
                    "flac_full_01.flac",
                    "Mysterons",
                    1,
                ),
                land_file(
                    &folder,
                    "02 - Sour Times.flac",
                    "flac_full_02.flac",
                    "Sour Times",
                    2,
                ),
            ];
            files.iter().for_each(|path| untag(path));
            files
        },
        |lengths| {
            release(
                GOOD_RELEASE,
                GOOD_GROUP,
                &["Mysterons", "Sour Times"],
                lengths,
            )
        },
    )
    .await;
    assert_eq!(
        decisions(&acquire, "t-untagged-right").await,
        vec![("imported".to_owned(), None)]
    );
    assert!(
        music
            .join("Portishead/Dummy/0102 Sour Times.flac")
            .is_file()
    );
}

/// MusicBrainz where a title search ranks other editions of the group
/// first, and the group's listing holds them all.
struct Listed {
    releases: Releases,
    /// What the title search answers: every release but the last.
    searched: usize,
}

impl ReleaseSource for Listed {
    fn search<'a>(
        &'a self,
        title: &'a str,
        artist: &'a str,
    ) -> BoxFuture<'a, Result<Vec<ReleaseHit>, SourceError>> {
        let searched = self.searched;
        Box::pin(async move {
            let mut hits = self.releases.search(title, artist).await?;
            hits.truncate(searched);
            Ok(hits)
        })
    }

    fn release<'a>(&'a self, mbid: &'a str) -> BoxFuture<'a, Result<Option<Release>, SourceError>> {
        self.releases.release(mbid)
    }

    fn canonical_recording<'a>(
        &'a self,
        mbid: &'a str,
    ) -> BoxFuture<'a, Result<Option<String>, SourceError>> {
        self.releases.canonical_recording(mbid)
    }

    fn search_editions<'a>(
        &'a self,
        query: &'a EditionQuery,
    ) -> BoxFuture<'a, Result<EditionPage, SourceError>> {
        let items: Vec<Edition> = self
            .releases
            .0
            .iter()
            .filter(|release| {
                query
                    .release_group_mbid
                    .as_deref()
                    .is_some_and(|group| release.release_group_id == group)
            })
            .map(|release| Edition {
                release_mbid: release.id.clone(),
                release_group_mbid: release.release_group_id.clone(),
                title: release.title.clone(),
                track_count: release.tracks.len() as u32,
                score: 100,
                ..Edition::default()
            })
            .skip(query.offset as usize)
            .collect();
        let total = items.len() as u64;
        Box::pin(async move {
            Ok(EditionPage {
                items,
                total,
                offset: 0,
            })
        })
    }
}

/// Without a pinned edition, the landing reads the requested group's
/// editions from its listing, closest track count first: the edition the
/// files are is found even when a title search ranks three others above
/// it.
#[tokio::test]
async fn the_landing_finds_the_edition_in_the_groups_listing() {
    let (acquire, music, _platform) = land_through(
        new_task("t-listed", GOOD_GROUP),
        &[],
        |downloads| {
            let folder = downloads.join("Portishead - Dummy");
            vec![
                land_file(
                    &folder,
                    "01 - Mysterons.flac",
                    "flac_full_01.flac",
                    "Mysterons",
                    1,
                ),
                land_file(
                    &folder,
                    "02 - Sour Times.flac",
                    "flac_full_02.flac",
                    "Sour Times",
                    2,
                ),
            ]
        },
        |lengths| {
            let others = [
                "Strangers",
                "It Could Be Sweet",
                "Wandering Star",
                "Numb",
                "Roads",
                "Pedestal",
                "Biscuit",
                "Glory Box",
                "Undenied",
                "Elysium",
            ];
            let mut releases: Vec<Release> = (0..3)
                .map(|index| {
                    release(
                        &format!("6d0b7a1e-0000-4000-8000-0000000001{index}0"),
                        GOOD_GROUP,
                        &others,
                        &[None; 10],
                    )
                })
                .collect();
            releases.push(release(
                GOOD_RELEASE,
                GOOD_GROUP,
                &["Mysterons", "Sour Times"],
                lengths,
            ));
            Arc::new(Listed {
                releases: Releases(releases),
                searched: 3,
            })
        },
    )
    .await;
    assert_eq!(
        decisions(&acquire, "t-listed").await,
        vec![("imported".to_owned(), None)]
    );
    assert!(
        music
            .join("Portishead/Dummy/0102 Sour Times.flac")
            .is_file()
    );
}

/// Soulseek keeps only the last folder of a share: `Music\Portishead\Singles`
/// lands as `downloads/Singles`. The remote name the client reported still
/// names the artist, so an untagged single from there imports.
#[tokio::test]
async fn an_untagged_single_whose_remote_folder_names_the_artist_imports() {
    let (acquire, music, _platform) = land_reported(
        new_task("t-untagged-remote", GOOD_GROUP),
        &[r"@@x\Music\Portishead\Singles\01 - Glory Box.flac"],
        |downloads| {
            let path = land_file(
                &downloads.join("Singles"),
                "01 - Glory Box.flac",
                "flac_full_01.flac",
                "Glory Box",
                1,
            );
            untag(&path);
            vec![path]
        },
        |lengths| release(GOOD_RELEASE, GOOD_GROUP, &["Glory Box"], lengths),
    )
    .await;
    assert_eq!(
        decisions(&acquire, "t-untagged-remote").await,
        vec![("imported".to_owned(), None)]
    );
    assert!(music.join("Portishead/Dummy/0101 Glory Box.flac").is_file());
}

/// A file whose own artist tag names someone else, and whose length is
/// not the track's, is another song (a tag conflict): it is held with
/// that reason and the rest of the album imports.
#[tokio::test]
async fn a_file_whose_artist_tag_names_someone_else_is_held() {
    let (acquire, music, _platform) = land_for(
        new_task("t-other-artist", GOOD_GROUP),
        |downloads| {
            let folder = downloads.join("dummy");
            let files = vec![
                land_file(
                    &folder,
                    "01 - Mysterons.flac",
                    "flac_full_01.flac",
                    "Mysterons",
                    1,
                ),
                land_file(
                    &folder,
                    "02 - Sour Times.flac",
                    "flac_full_02.flac",
                    "Sour Times",
                    2,
                ),
            ];
            let other = TagEdit::new(TagField::Artist, vec!["Dan Romer".to_owned()]);
            save_tags(&files[1], &[other]).expect("artist saves");
            files
        },
        |lengths| {
            let mut lengths = lengths.to_vec();
            lengths[1] = lengths[1].map(|ms| ms + 25_000);
            release(
                GOOD_RELEASE,
                GOOD_GROUP,
                &["Mysterons", "Sour Times"],
                &lengths,
            )
        },
    )
    .await;
    assert!(music.join("Portishead/Dummy/0101 Mysterons.flac").is_file());
    assert!(!music.join("Portishead/Dummy/0102 Sour Times.flac").exists());
    let held: Vec<(String, String)> = sqlx::query_as(
        "SELECT reason, COALESCE(reason_detail, '') FROM acquire_held_files \
         WHERE source_task_id = 't-other-artist'",
    )
    .fetch_all(acquire.db.db().sqlx())
    .await
    .expect("held rows");
    assert_eq!(held.len(), 1, "{held:?}");
    assert_eq!(held[0].0, "tag_mismatch");
    assert!(held[0].1.contains("Dan Romer"), "{held:?}");
}

/// AcoustID hearing one recording in every file, as a landing test
/// scripts it.
struct HearsEverywhere(&'static str);

impl FingerprintSource for HearsEverywhere {
    fn identify<'a>(&'a self, _tracks: &'a [LocalTrackFacts]) -> BoxFuture<'a, AudioMatches> {
        Box::pin(async { AudioMatches::default() })
    }

    fn identify_files<'a>(
        &'a self,
        files: &'a [(String, PathBuf)],
    ) -> BoxFuture<'a, HashMap<String, Vec<String>>> {
        Box::pin(async move {
            files
                .iter()
                .map(|(id, _)| (id.clone(), vec![self.0.to_owned()]))
                .collect()
        })
    }
}

/// A file AcoustID hears as another recording imports only when its
/// length is within 3 s of the track's, as copies of one master are: 2 s
/// off imports, 5 s off is held.
#[tokio::test]
async fn a_file_heard_as_another_recording_needs_the_tracks_length() {
    let (acquire, music, _platform) = land_heard(
        new_task("t-heard", GOOD_GROUP),
        &[],
        |downloads| {
            let folder = downloads.join("dummy");
            vec![
                land_file(
                    &folder,
                    "01 - Mysterons.flac",
                    "flac_full_01.flac",
                    "Mysterons",
                    1,
                ),
                land_file(
                    &folder,
                    "02 - Sour Times.flac",
                    "flac_full_02.flac",
                    "Sour Times",
                    2,
                ),
            ]
        },
        |lengths| {
            let lengths = [
                lengths[0].map(|ms| ms + 5_000),
                lengths[1].map(|ms| ms + 2_000),
            ];
            Arc::new(Releases(vec![release(
                GOOD_RELEASE,
                GOOD_GROUP,
                &["Mysterons", "Sour Times"],
                &lengths,
            )]))
        },
        // The recording in the recorded AcoustID answer
        // (fixtures/acoustid/batch_lookup.json).
        Some(Arc::new(HearsEverywhere(
            "fe000616-c72c-43d4-8cba-a4083e99636b",
        ))),
    )
    .await;
    assert!(!music.join("Portishead/Dummy/0101 Mysterons.flac").exists());
    assert!(
        music
            .join("Portishead/Dummy/0102 Sour Times.flac")
            .is_file()
    );
    let held: Vec<(String,)> =
        sqlx::query_as("SELECT reason FROM acquire_held_files WHERE source_task_id = 't-heard'")
            .fetch_all(acquire.db.db().sqlx())
            .await
            .expect("held rows");
    assert_eq!(held, vec![("fingerprint_mismatch".to_owned(),)]);
}

/// POST or GET one held-import route as `principal`.
async fn held_call(
    acquire: &droppedneedle_acquire::AcquireSetup,
    method: &str,
    uri: &str,
    body: Option<serde_json::Value>,
    principal: &Principal,
) -> (axum::http::StatusCode, serde_json::Value) {
    use tower::ServiceExt as _;
    let mut request = axum::http::Request::builder().method(method).uri(uri);
    let body = match body {
        Some(body) => {
            request = request.header("content-type", "application/json");
            axum::body::Body::from(body.to_string())
        }
        None => axum::body::Body::empty(),
    };
    let request = request.body(body).expect("request");
    let router = droppedneedle_acquire::held::http::routes()
        .with_state(acquire.held.clone())
        .into_router();
    let response = with_test_principal(router, principal.clone())
        .oneshot(request)
        .await
        .expect("response");
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), 1 << 20)
        .await
        .expect("body");
    (status, serde_json::from_slice(&bytes).unwrap_or_default())
}

pub(crate) fn files_under(dir: &Path, extension: &str) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return found;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            found.extend(files_under(&path, extension));
        } else if path.extension().is_some_and(|ext| ext == extension) {
            found.push(path);
        }
    }
    found
}

// A held upgrade file a person imports replaces the library's weaker copy,
// which goes to the recycle bin (never deleted); another user's held file
// answers 404; a discarded file is deleted and leaves the list.
#[tokio::test]
async fn held_files_import_as_upgrades_or_discard() {
    let db = AcquireDb::scratch().expect("scratch db");
    db.add_user("u-ada", "Ada", "user").await.expect("user");
    db.add_user("u-bob", "Bob", "user").await.expect("user");
    let dir = db.path().parent().expect("scratch dir").to_path_buf();
    let config = Arc::new(
        ConfigStore::scratch(&dir, Crypto::from_key_bytes(&[7u8; 32]).expect("key"))
            .expect("config"),
    );
    let platform = TestPlatform::on(db.db(), &dir)
        .settings(config)
        .build()
        .await;
    let (library, _) = library_at(&platform);
    let music = dir.join("music");
    std::fs::create_dir_all(&music).expect("music root");
    library
        .scans
        .add_root(
            Some("music".to_owned()),
            music.to_string_lossy().into_owned(),
            EffectivePolicy::Automatic,
        )
        .expect("root adds");
    let release = release(
        GOOD_RELEASE,
        GOOD_GROUP,
        &["Mysterons", "Sour Times"],
        &[None, None],
    );
    let first = release.tracks[0].clone();
    let releases: Arc<dyn ReleaseSource> = Arc::new(Releases(vec![release]));
    let acquire = acquire_over(
        &db,
        Some(Arc::new(LibraryLanding::new(
            &library,
            every_priority(releases),
        ))),
    );
    let worker = |paths: Vec<PathBuf>| {
        Arc::new(
            DownloadWorker::fixed(
                acquire.journal.clone(),
                vec![crate::as_source(Arc::new(FixedSource::new(
                    dir.join("downloads"),
                    paths,
                )))],
                WorkerConfig::default(),
                droppedneedle_kernel::events::EventHub::new(),
                Default::default(),
            )
            .with_landing(acquire.landing.clone()),
        )
    };

    // The library holds an MP3 of the first track, from an earlier download.
    let mp3 = land_file(
        &dir.join("downloads/mp3"),
        "01 - Mysterons.mp3",
        "mp3_full_01.mp3",
        "Mysterons",
        1,
    );
    acquire
        .journal
        .run("test.seed", |store| {
            store.insert_task(&new_task("t-mp3", GOOD_GROUP), 1.0)
        })
        .await
        .expect("task");
    let mp3_worker = worker(vec![mp3]);
    for pass in 1..10 {
        mp3_worker.run_once(pass).await;
        mp3_worker.wait_for_landings().await;
        let task = acquire.journal.read_task("t-mp3").await.unwrap().unwrap();
        if task.status.is_terminal() {
            break;
        }
    }
    let old_copy = music.join("Portishead/Dummy/0101 Mysterons.mp3");
    assert!(old_copy.is_file(), "the MP3 is in the library");

    // A better FLAC of that track waits as a held upgrade file, plus one
    // held file of someone else's and one to discard.
    let held_dir = dir.join("held");
    let flac = land_file(&held_dir, "up.flac", "flac_full_01.flac", "Mysterons", 1);
    let spare = land_file(&held_dir, "spare.flac", "flac_full_02.flac", "Roads", 2);
    // And an upgrade file that cannot be read as audio.
    let broken = held_dir.join("broken.flac");
    std::fs::write(&broken, b"not audio").expect("broken file");
    let (track_id, recording_id) = (first.id.clone(), first.recording_id.clone());
    let name = |path: &PathBuf| {
        path.file_name()
            .expect("name")
            .to_string_lossy()
            .into_owned()
    };
    let (flac, spare, broken) = (name(&flac), name(&spare), name(&broken));
    let seeded: Vec<(i64, i64)> = acquire
        .db
        .write("test.held", move |tx| {
            let mut ids = Vec::new();
            for (entry, user, path, reason) in [
                (1, "u-ada", &flac, "fingerprint_mismatch"),
                (2, "u-bob", &flac, "fingerprint_mismatch"),
                (1, "u-ada", &spare, "tag_mismatch"),
                (1, "u-ada", &broken, "fingerprint_mismatch"),
            ] {
                tx.execute(
                    "INSERT OR IGNORE INTO acquire_held_entries (id, user_id, origin, group_key, \
                     release_group_mbid, state, created_at, expires_at) \
                     VALUES (?1, ?2, 'download', ?3, ?4, 'held', 1, 2)",
                    rusqlite::params![entry, user, format!("album:{GOOD_GROUP}"), GOOD_GROUP],
                )?;
                tx.execute(
                    "INSERT INTO acquire_held_files (entry_id, root, path, original_name, \
                     release_mbid, release_track_mbid, recording_mbid, track_number, \
                     disc_number, reason, source, source_task_id, state, created_at) \
                     VALUES (?1, 'held', ?2, ?2, ?3, ?4, ?5, 1, 1, ?6, 'soulseek', 't-up', \
                     'held', 1)",
                    rusqlite::params![entry, path, GOOD_RELEASE, track_id, recording_id, reason],
                )?;
                ids.push((entry, tx.last_insert_rowid()));
            }
            Ok(ids)
        })
        .await
        .expect("held rows");
    let ada = &test_principal("u-ada", Role::User, "sess-ada", SessionKind::Standard);
    let only = |file: i64| Some(serde_json::json!({ "file_ids": [file] }));

    let import = |entry: i64| format!("/api/v3/held/{entry}/import");
    let (status, _) = held_call(
        &acquire,
        "POST",
        &import(seeded[1].0),
        only(seeded[1].1),
        ada,
    )
    .await;
    assert_eq!(status, 404, "someone else's held file is not found");

    let (status, body) = held_call(
        &acquire,
        "POST",
        &import(seeded[0].0),
        only(seeded[0].1),
        ada,
    )
    .await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["imported"], 1, "{body}");
    let new_copy = music.join("Portishead/Dummy/0101 Mysterons.flac");
    assert!(new_copy.is_file(), "the FLAC replaced it: {body}");
    assert!(!old_copy.exists(), "the MP3 left the album folder");
    let recycled = files_under(&music.join(".recycle"), "mp3");
    assert_eq!(recycled.len(), 1, "the MP3 went to the recycle bin");
    let formats: Vec<String> = sqlx::query_scalar(
        "SELECT file_format FROM local_tracks WHERE availability = 'indexed' \
         AND relative_path LIKE '%Mysterons%'",
    )
    .fetch_all(acquire.db.db().sqlx())
    .await
    .expect("catalog");
    assert_eq!(formats, vec!["flac".to_owned()]);

    let discard = format!("/api/v3/held/{}/discard", seeded[2].0);
    let (status, body) = held_call(&acquire, "POST", &discard, only(seeded[2].1), ada).await;
    assert_eq!(status, 200, "{body}");
    assert!(
        !held_dir.join("spare.flac").exists(),
        "discarded file deleted"
    );

    // An upgrade file whose quality cannot be read is refused, never taken
    // for "no better" and deleted: the file and its row stay.
    let (status, body) = held_call(
        &acquire,
        "POST",
        &import(seeded[3].0),
        only(seeded[3].1),
        ada,
    )
    .await;
    assert_eq!(status, 409, "{body}");
    assert!(
        held_dir.join("broken.flac").is_file(),
        "unreadable file kept"
    );
    assert!(new_copy.is_file(), "the library copy is untouched");

    let (status, body) = held_call(&acquire, "GET", "/api/v3/held", None, ada).await;
    assert_eq!(status, 200);
    let entries = body["entries"].as_array().cloned().unwrap_or_default();
    assert_eq!(entries.len(), 1, "one entry for the album: {body}");
    let left: Vec<i64> = entries[0]["files"]
        .as_array()
        .map(|files| {
            files
                .iter()
                .filter_map(|file| file["id"].as_i64())
                .collect()
        })
        .unwrap_or_default();
    assert_eq!(left, vec![seeded[3].1], "{body}");
}

/// An import a restart cut off after the library took the file: once the
/// library's writes settle, the prune marks that file imported and gives
/// the entry back with the file that did not go in. Nothing is placed a
/// second time.
#[tokio::test]
async fn a_held_import_cut_off_by_a_restart_never_places_its_file_twice() {
    let db = AcquireDb::scratch().expect("scratch db");
    db.add_user("u-ada", "Ada", "user").await.expect("user");
    let dir = db.path().parent().expect("scratch dir").to_path_buf();
    let config = Arc::new(
        ConfigStore::scratch(&dir, Crypto::from_key_bytes(&[7u8; 32]).expect("key"))
            .expect("config"),
    );
    let platform = TestPlatform::on(db.db(), &dir)
        .settings(config)
        .build()
        .await;
    let (library, _) = library_at(&platform);
    let music = dir.join("music");
    std::fs::create_dir_all(&music).expect("music root");
    library
        .scans
        .add_root(
            Some("music".to_owned()),
            music.to_string_lossy().into_owned(),
            EffectivePolicy::Automatic,
        )
        .expect("root adds");
    let release = release(
        GOOD_RELEASE,
        GOOD_GROUP,
        &["Mysterons", "Sour Times"],
        &[None, None],
    );
    let tracks = release.tracks.clone();
    let releases: Arc<dyn ReleaseSource> = Arc::new(Releases(vec![release]));
    let acquire = acquire_over(
        &db,
        Some(Arc::new(LibraryLanding::new(
            &library,
            every_priority(releases),
        ))),
    );
    let held_dir = acquire.held.folders().held.clone();
    let first = land_file(&held_dir, "first.flac", "flac_full_01.flac", "Mysterons", 1);
    land_file(
        &held_dir,
        "second.flac",
        "flac_full_02.flac",
        "Sour Times",
        2,
    );
    let bytes = std::fs::read(&first).expect("held copy");
    let seeded: Vec<i64> = acquire
        .db
        .write("test.held", move |tx| {
            tx.execute(
                "INSERT INTO acquire_held_entries (id, user_id, origin, group_key, \
                 release_group_mbid, state, created_at, expires_at) VALUES (1, 'u-ada', \
                 'download', ?1, ?2, 'held', 1, 9999999999999)",
                rusqlite::params![format!("album:{GOOD_GROUP}"), GOOD_GROUP],
            )?;
            let mut ids = Vec::new();
            for (name, track) in [("first.flac", &tracks[0]), ("second.flac", &tracks[1])] {
                tx.execute(
                    "INSERT INTO acquire_held_files (entry_id, root, path, original_name, \
                     release_mbid, release_track_mbid, recording_mbid, disc_number, \
                     track_number, reason, source, state, created_at) VALUES (1, 'held', ?1, \
                     ?1, ?2, ?3, ?4, 1, ?5, 'weak_match', 'soulseek', 'held', 1)",
                    rusqlite::params![
                        name,
                        GOOD_RELEASE,
                        track.id,
                        track.recording_id,
                        track.position
                    ],
                )?;
                ids.push(tx.last_insert_rowid());
            }
            Ok(ids)
        })
        .await
        .expect("held rows");
    let viewer = HeldViewer {
        user_id: "u-ada".to_owned(),
        admin: false,
    };
    acquire
        .held
        .import(&viewer, 1, Some(vec![seeded[0]]), "u-ada")
        .await
        .expect("imported");
    let placed = music.join("Portishead/Dummy/0101 Mysterons.flac");
    assert!(placed.is_file(), "the file went in");

    // The server stopped after the library took the file and before the
    // held list recorded it: the entry is still claimed, the file still
    // held, its copy still there.
    std::fs::write(&first, &bytes).expect("held copy back");
    acquire
        .db
        .write("test.crash", move |tx| {
            tx.execute(
                "UPDATE acquire_held_entries SET state = 'placing', resolved_at = NULL",
                [],
            )?;
            tx.execute(
                "UPDATE acquire_held_files SET state = 'held', resolved_at = NULL",
                [],
            )?;
            Ok(())
        })
        .await
        .expect("cut off");

    acquire.held.recover().await;
    acquire.held.tend().await;
    let states: Vec<(String, String)> = sqlx::query_as(
        "SELECT f.state, e.state FROM acquire_held_files f \
         JOIN acquire_held_entries e ON e.id = f.entry_id ORDER BY f.id",
    )
    .fetch_all(acquire.db.db().sqlx())
    .await
    .expect("held rows");
    assert_eq!(
        states,
        vec![
            ("imported".to_owned(), "held".to_owned()),
            ("held".to_owned(), "held".to_owned()),
        ],
        "the file that went in is imported; the other waits"
    );
    assert!(!first.exists(), "the placed file's held copy is removed");
    assert!(held_dir.join("second.flac").is_file());
    let copies: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM local_tracks WHERE availability = 'indexed' \
         AND title = 'Mysterons'",
    )
    .fetch_one(acquire.db.db().sqlx())
    .await
    .expect("catalog");
    assert_eq!(copies, 1, "placed once");
    assert!(files_under(&music.join(".recycle"), "flac").is_empty());
}

/// The 90-day prune: an entry past its expiry loses its held copies and
/// nothing else, a library file and a downloads folder file of the same
/// name stay, an entry an action holds is skipped, and an expired upload
/// leaves its drop item discarded.
#[tokio::test]
async fn expired_entries_lose_their_held_copies_and_nothing_else() {
    let (acquire, dir, _auth) = bare_acquire().await;
    let folders = acquire.held.folders().clone();
    for (folder, name) in [
        (folders.held.clone(), "old.flac"),
        (folders.held.clone(), "busy.flac"),
        (dir.join("music"), "old.flac"),
        (dir.join("downloads"), "old.flac"),
        (folders.drop.join("job-old"), "up.flac"),
    ] {
        std::fs::create_dir_all(&folder).expect("folder");
        std::fs::write(folder.join(name), b"audio").expect("file");
    }
    let staging = folders.drop.join("job-old").to_string_lossy().into_owned();
    acquire
        .db
        .write("test.held", move |tx| {
            tx.execute_batch(
                "INSERT INTO drop_import_jobs (id, user_id, user_name, status, created_at, \
                 upload_name, staging_dir) VALUES ('job-old', 'u-ada', 'Ada', 'completed', 1.0, \
                 'up.zip', 'STAGING'); \
                 INSERT INTO drop_import_items (id, job_id, folder_name, status, updated_at) \
                 VALUES (7, 'job-old', 'Up', 'needs_review', 1.0); \
                 INSERT INTO acquire_held_entries (id, user_id, origin, group_key, state, \
                 drop_item_id, created_at, expires_at) VALUES \
                 (1, 'u-ada', 'download', 'album:rg-old', 'held', NULL, 1, 2), \
                 (2, 'u-ada', 'download', 'album:rg-busy', 'placing', NULL, 1, 2), \
                 (3, 'u-ada', 'upload', 'upload:7', 'held', 7, 1, 2), \
                 (4, 'u-ada', 'download', 'album:rg-new', 'held', NULL, 1, 9999999999999); \
                 INSERT INTO acquire_held_files (entry_id, root, path, original_name, source, \
                 reason, state, created_at) VALUES \
                 (1, 'held', 'old.flac', 'old.flac', 'soulseek', 'weak_match', 'held', 1), \
                 (2, 'held', 'busy.flac', 'busy.flac', 'soulseek', 'weak_match', 'held', 1), \
                 (3, 'drop', 'job-old/up.flac', 'up.flac', 'upload', 'no_album_match', 'held', 1), \
                 (4, 'held', 'new.flac', 'new.flac', 'soulseek', 'weak_match', 'held', 1);"
                    .replace("STAGING", &staging)
                    .as_str(),
            )?;
            Ok(())
        })
        .await
        .expect("held rows");

    assert_eq!(acquire.held.tend().await, 2);
    let states: Vec<(i64, String)> =
        sqlx::query_as("SELECT id, state FROM acquire_held_entries ORDER BY id")
            .fetch_all(acquire.db.db().sqlx())
            .await
            .expect("entries");
    assert_eq!(
        states,
        vec![
            (1, "expired".to_owned()),
            (2, "placing".to_owned()),
            (3, "expired".to_owned()),
            (4, "held".to_owned()),
        ]
    );
    assert!(
        !folders.held.join("old.flac").exists(),
        "the held copy went"
    );
    assert!(dir.join("music/old.flac").is_file(), "a library file stays");
    assert!(dir.join("downloads/old.flac").is_file(), "a download stays");
    assert!(
        folders.held.join("busy.flac").is_file(),
        "an entry in use is skipped"
    );
    assert!(
        !folders.drop.join("job-old/up.flac").exists(),
        "the upload went"
    );
    let item: String = sqlx::query_scalar("SELECT status FROM drop_import_items WHERE id = 7")
        .fetch_one(acquire.db.db().sqlx())
        .await
        .expect("item");
    assert_eq!(item, "discarded");
    assert_eq!(
        acquire.held.tend().await,
        0,
        "a second pass changes nothing"
    );
}

/// The test acquire bundle over `db`, finished downloads landing in
/// `library` when there is one.
fn acquire_over(
    db: &AcquireDb,
    library: Option<Arc<dyn LandingLibrary>>,
) -> droppedneedle_acquire::AcquireSetup {
    droppedneedle_acquire::AcquireSetup::for_tests(
        db.clone(),
        Arc::new(UuidGenerator),
        droppedneedle_kernel::events::EventHub::new(),
        library,
    )
    .expect("acquire bundle")
}

/// A scratch database with the test user, its folder, and the auth rig its
/// users live on.
async fn bare_db() -> (AcquireDb, PathBuf, AuthRig) {
    let auth = AuthRig::new("landing-bare").await;
    let db = AcquireDb::scratch().expect("scratch db");
    db.add_user("u-ada", "Ada", "user").await.expect("user");
    let dir = db.path().parent().expect("scratch dir").to_path_buf();
    (db, dir, auth)
}

/// An acquire bundle with no library, its scratch folder, and the auth rig
/// its users live on.
async fn bare_acquire() -> (droppedneedle_acquire::AcquireSetup, PathBuf, AuthRig) {
    let (db, dir, auth) = bare_db().await;
    (acquire_over(&db, None), dir, auth)
}

/// Land `landed` for a fresh task and return the rejection code, if any.
async fn rejection_code(
    acquire: &droppedneedle_acquire::AcquireSetup,
    task_id: &'static str,
    landed: Landed,
) -> Option<Reason> {
    rejection(acquire, task_id, landed, false)
        .await
        .map(|rejection| rejection.code)
}

/// Land `landed` for a fresh task and return the rejection, if any.
async fn rejection(
    acquire: &droppedneedle_acquire::AcquireSetup,
    task_id: &'static str,
    landed: Landed,
    patient: bool,
) -> Option<Rejection> {
    acquire
        .journal
        .run("test.seed", move |store| {
            store.insert_task(&new_task(task_id, GOOD_GROUP), 1.0)
        })
        .await
        .expect("task");
    let task = acquire.journal.read_task(task_id).await.unwrap().unwrap();
    match acquire
        .landing
        .land(&task, None, landed, patient)
        .await
        .result
    {
        LandingResult::Rejected(rejection) => Some(rejection),
        _ => None,
    }
}

/// A finished download is only read from inside its client's downloads
/// folder. A link standing in for the download, a path somewhere else, a
/// `..` path and a path under a linked folder are each refused with the
/// reason that is given before anything is read. (Reading or removing the
/// file cannot be seen here: this rig has no library to import into.)
#[cfg(unix)]
#[tokio::test]
async fn a_download_outside_its_clients_folder_is_refused() {
    use std::os::unix::fs::symlink;
    let (acquire, dir, _auth) = bare_acquire().await;
    let mount = dir.join("downloads");
    let outside = dir.join("elsewhere");
    let secret = land_file(&outside, "01.flac", "flac_full_01.flac", "Mysterons", 1);
    std::fs::create_dir_all(&mount).expect("mount");
    symlink(&secret, mount.join("01.flac")).expect("file link");
    symlink(&outside, mount.join("job")).expect("folder link");

    let cases = [
        ("t-link", mount.join("01.flac")),
        ("t-linked-folder", mount.join("job")),
        ("t-linked-parent", mount.join("job/01.flac")),
        ("t-elsewhere", secret.clone()),
        ("t-dotdot", mount.join("../elsewhere/01.flac")),
    ];
    for (task_id, path) in cases {
        let landed = Landed {
            mounts: vec![mount.clone().into()],
            files: vec![Reported::path(path)],
        };
        assert_eq!(
            rejection_code(&acquire, task_id, landed).await,
            Some(Reason::OutsideDownloadsFolder),
            "{task_id}"
        );
    }

    // A downloads folder that cannot be resolved right now (a share that
    // is not answering, a folder not there yet) says nothing against the
    // path: the files read as missing and the landing waits.
    let unmounted = dir.join("not-mounted");
    let landed = Landed {
        mounts: vec![unmounted.clone().into()],
        files: vec![Reported::path(unmounted.join("job"))],
    };
    let waiting = rejection(&acquire, "t-unmounted", landed, true)
        .await
        .expect("rejected");
    assert_eq!(
        (waiting.code, waiting.disposition),
        (Reason::FilesMissing, Disposition::Temporary)
    );

    // The same file inside the folder gets past the path check (and stops
    // at the next one: this rig has no library).
    let inside = land_file(
        &mount.join("real"),
        "01.flac",
        "flac_full_01.flac",
        "Mysterons",
        1,
    );
    let landed = Landed {
        mounts: vec![mount.clone().into()],
        files: vec![Reported::path(inside)],
    };
    assert_eq!(
        rejection_code(&acquire, "t-inside", landed).await,
        Some(Reason::LocalFault)
    );
}

/// A download policy that does not read is never the default quality band:
/// the landing waits with a temporary rejection, the files stay where they
/// are, and nothing is said against the source.
#[tokio::test]
async fn a_landing_waits_while_the_download_policy_does_not_read() {
    let (acquire, dir, _auth) = bare_acquire().await;
    let store = crate::acquire_requests::unreadable_policy_store(&dir.join("broken-settings"));
    let landing = droppedneedle_acquire::landing::LandingService::new(
        acquire.journal.clone(),
        None,
        store.live(),
        dir.join("held"),
    );
    let mount = dir.join("downloads");
    let file = land_file(
        &mount.join("job"),
        "01.mp3",
        "mp3_full_01.mp3",
        "Mysterons",
        1,
    );
    acquire
        .journal
        .run("test.seed", |store| {
            store.insert_task(&new_task("t-unreadable", GOOD_GROUP), 1.0)
        })
        .await
        .expect("task");
    let task = acquire
        .journal
        .read_task("t-unreadable")
        .await
        .unwrap()
        .unwrap();
    let landed = Landed {
        mounts: vec![mount.into()],
        files: vec![Reported::path(file.clone())],
    };
    let LandingResult::Rejected(rejection) = landing.land(&task, None, landed, false).await.result
    else {
        panic!("the landing went ahead under unreadable settings");
    };
    assert_eq!(
        (rejection.code, rejection.disposition),
        (Reason::SettingsUnreadable, Disposition::Temporary)
    );
    assert!(file.exists());
    let quarantined: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM download_quarantine")
        .fetch_one(acquire.db.db().sqlx())
        .await
        .expect("quarantine count");
    assert_eq!(quarantined, 0);
}

/// A person confirmed a pick outside the quality band: the attempt that
/// fetched exactly that pick imports its files, while the same files from
/// any other attempt of the task (a failover) are refused on quality.
#[tokio::test]
async fn a_quality_waiver_keeps_only_the_pinned_attempts_files() {
    for (pinned, kept) in [(true, true), (false, false)] {
        let db = AcquireDb::scratch().expect("scratch db");
        db.add_user("u-ada", "Ada", "user").await.expect("user");
        let dir = db.path().parent().expect("scratch dir").to_path_buf();
        let config = Arc::new(
            ConfigStore::scratch(&dir, Crypto::from_key_bytes(&[7u8; 32]).expect("key"))
                .expect("config"),
        );
        let platform = TestPlatform::on(db.db(), &dir)
            .settings(config)
            .build()
            .await;
        let (library, _) = library_at(&platform);
        let music = dir.join("music");
        std::fs::create_dir_all(&music).expect("music root");
        library
            .scans
            .add_root(
                Some("music".to_owned()),
                music.to_string_lossy().into_owned(),
                EffectivePolicy::Automatic,
            )
            .expect("root adds");
        let downloads = dir.join("downloads");
        let file = land_file(
            &downloads.join("job"),
            "01 - Mysterons.flac",
            "flac_full_01.flac",
            "Mysterons",
            1,
        );
        let releases: Arc<dyn ReleaseSource> = Arc::new(Releases(vec![release(
            GOOD_RELEASE,
            GOOD_GROUP,
            &["Mysterons"],
            &[length_ms(&file)],
        )]));
        let acquire = acquire_over(
            &db,
            Some(Arc::new(LibraryLanding::new(
                &library,
                every_priority(releases),
            ))),
        );
        // Lossy only: the FLAC files are outside the band.
        let band = dir.join("band");
        std::fs::create_dir_all(&band).expect("settings dir");
        std::fs::write(
            band.join("config.json"),
            serde_json::json!({"download_policy": {"quality_min": "low", "quality_max": "mp3_320"}})
                .to_string(),
        )
        .expect("config.json");
        let store = droppedneedle_acquire::settings::scratch_settings(&band).expect("settings");
        let landing = droppedneedle_acquire::landing::LandingService::new(
            acquire.journal.clone(),
            acquire.landing.library().cloned(),
            store.live(),
            dir.join("held"),
        );
        let handle = serde_json::json!({
            "source": "soulseek", "username": "peer", "job_name": "", "pinned": pinned,
            "filenames": ["@@music\\Portishead\\Dummy\\01 - Mysterons.flac"],
        })
        .to_string();
        acquire
            .journal
            .run("test.seed", move |store| {
                store.insert_task(&new_task("t-waived", GOOD_GROUP), 1.0)?;
                store.set_task_plan(
                    "t-waived",
                    &TaskPlan {
                        quality_waived: true,
                        ..TaskPlan::default()
                    },
                    1.0,
                )?;
                store.insert_attempt(
                    "t-waived-a0",
                    "t-waived",
                    "soulseek",
                    0,
                    "",
                    &handle,
                    AttemptState::InUse,
                    1.0,
                )
            })
            .await
            .expect("task");
        let task = acquire
            .journal
            .read_task("t-waived")
            .await
            .unwrap()
            .unwrap();
        let landed = Landed {
            mounts: vec![downloads.into()],
            files: vec![Reported::path(file)],
        };
        let result = landing
            .land(&task, Some("t-waived-a0"), landed, false)
            .await
            .result;
        if kept {
            assert!(
                matches!(result, LandingResult::Imported { .. }),
                "the pinned attempt keeps its files: {result:?}"
            );
            assert_eq!(files_under(&music, "flac").len(), 1);
        } else {
            let LandingResult::Rejected(rejection) = result else {
                panic!("an attempt that is not the pick was kept: {result:?}");
            };
            assert_eq!(rejection.code, Reason::QualityRejected);
            assert!(files_under(&music, "flac").is_empty());
        }
    }
}

/// An attempt that imported its files and then had a landing wait (a busy
/// database) is still answered as imported when the next landing finds
/// nothing: the wait never hides the import.
#[tokio::test]
async fn a_wait_after_an_import_does_not_hide_it() {
    let (acquire, dir, _auth) = bare_acquire().await;
    let mount = dir.join("downloads");
    std::fs::create_dir_all(&mount).expect("mount");
    acquire
        .journal
        .run("test.seed", |store| {
            store.insert_task(&new_task("t-again", GOOD_GROUP), 1.0)?;
            store.insert_attempt(
                "t-again-a0",
                "t-again",
                "soulseek",
                0,
                "",
                "",
                AttemptState::InUse,
                1.0,
            )?;
            for (outcome, at) in [("imported", 2.0), ("deferred", 3.0)] {
                store.record_import_decision(&ImportDecisionRow {
                    task_id: "t-again".to_owned(),
                    attempt_id: Some("t-again-a0".to_owned()),
                    outcome: outcome.to_owned(),
                    files_total: 2,
                    files_imported: if outcome == "imported" { 2 } else { 0 },
                    checks_json: "[]".to_owned(),
                    missing_positions: "[]".to_owned(),
                    decided_at: at,
                    ..ImportDecisionRow::default()
                })?;
            }
            Ok(())
        })
        .await
        .expect("seed");
    let task = acquire.journal.read_task("t-again").await.unwrap().unwrap();
    let landed = Landed {
        mounts: vec![mount.into()],
        files: Vec::new(),
    };
    let report = acquire
        .landing
        .land(&task, Some("t-again-a0"), landed, false)
        .await;
    assert!(
        matches!(report.result, LandingResult::Imported { complete: true }),
        "the earlier import is answered again, got {:?}",
        report.result
    );
}

/// A plugin's finished download is confined to the plugin's own folders
/// (its data folder and its `downloads_dir` setting), whatever workspace
/// the plugin itself names.
#[tokio::test]
async fn a_plugin_download_is_confined_to_the_plugins_folders() {
    use crate::common::plugin_fakes::FakeLauncher;
    use droppedneedle_acquire::downloads::{DownloadSource, SourceHandle, SourceTag};
    use droppedneedle_acquire::plugin_source::{PluginDownloadSource, ReleasePolicy};
    use droppedneedle_acquire::usenet::policy::UsenetPolicy;
    use droppedneedle_plugins::host::PluginHost;
    use droppedneedle_plugins::protocol::methods;

    let (acquire, dir, _auth) = bare_acquire().await;
    let plugin_dir = dir.join("plugins/toy");
    std::fs::create_dir_all(&plugin_dir).expect("plugin dir");
    std::fs::write(
        plugin_dir.join("plugin.toml"),
        "[plugin]\n\
         name = \"toy\"\n\
         version = \"1.0.0\"\n\
         api_version = 2\n\
         entrypoint = \"plugin:Toy\"\n\
         capabilities = [\"download_client\"]\n\
         [[capability]]\n\
         id = \"download_client\"\n\
         source = \"toy\"\n\
         display_name = \"Toy\"\n\
         [[settings]]\n\
         key = \"downloads_dir\"\n\
         label = \"Downloads directory\"\n",
    )
    .expect("manifest");
    let config = Arc::new(
        ConfigStore::scratch(
            &dir.join("plugin-config"),
            Crypto::from_key_bytes(&[7u8; 32]).expect("key"),
        )
        .expect("config"),
    );
    let launcher = Arc::new(FakeLauncher::new());
    crate::common::migrated_db(&dir.join("library.db"));
    let db = droppedneedle_kernel::db::Db::open_for_tests(&dir.join("library.db")).expect("db");
    let platform = TestPlatform::on(&db, &dir).build().await;
    let library = crate::common::library::library_core(
        &platform,
        droppedneedle_kernel::jobs::queue::JobQueue::new(
            db,
            Arc::new(droppedneedle_kernel::clock::SystemClock),
        ),
    );
    let host = PluginHost::new(
        dir.join("plugins"),
        config,
        launcher.clone(),
        droppedneedle_plugins::wiring::tick_store(&platform),
        library,
    );
    host.load_all();
    let allowed = dir.join("plugin-downloads");
    // Both folders exist, as they do once a real plugin has started.
    std::fs::create_dir_all(&allowed).expect("downloads dir");
    std::fs::create_dir_all(host.data_dir("toy")).expect("data dir");
    host.update_settings(
        "toy",
        true,
        std::collections::HashMap::from([(
            "downloads_dir".to_owned(),
            allowed.to_string_lossy().into_owned(),
        )]),
    )
    .expect("plugin enabled");
    let runtime = launcher.runtime("toy");
    let source = PluginDownloadSource::new(
        host,
        "plugin:toy".to_owned(),
        acquire.journal.clone(),
        ReleasePolicy {
            usenet: UsenetPolicy::v2_defaults(),
            ignored_terms: Vec::new(),
            required_terms: Vec::new(),
            auto_accept: 0.70,
            manual_min: 0.50,
        },
    );
    let handle = SourceHandle::new(SourceTag::plugin("toy"));

    // The plugin names a workspace of its own choosing, and a file in it.
    let outside = dir.join("elsewhere");
    let secret = land_file(&outside, "01.flac", "flac_full_01.flac", "Mysterons", 1);
    runtime.answer(
        methods::INSPECT,
        Ok(serde_json::json!({
            "state": "completed",
            "workspace_path": outside,
            "file_paths": [secret],
        })),
    );
    let landed = source.landed(&handle).await.expect("inspect");
    assert_eq!(
        rejection_code(&acquire, "t-plugin-out", landed).await,
        Some(Reason::OutsideDownloadsFolder)
    );

    // A file in the folder the admin set gets past the path check.
    let inside = land_file(
        &allowed.join("job"),
        "01.flac",
        "flac_full_01.flac",
        "Mysterons",
        1,
    );
    runtime.answer(
        methods::INSPECT,
        Ok(serde_json::json!({
            "state": "completed",
            "workspace_path": allowed.join("job"),
            "file_paths": [inside],
        })),
    );
    let landed = source.landed(&handle).await.expect("inspect");
    assert_eq!(
        rejection_code(&acquire, "t-plugin-in", landed).await,
        Some(Reason::LocalFault)
    );
}

/// The hourly cleanup of finished download folders asks the publisher
/// first: a folder stays while the write putting its files into the
/// library is unfinished or needs an administrator, and goes once that
/// write is done.
#[tokio::test]
async fn orphan_cleanup_waits_for_the_publish_to_finish() {
    use droppedneedle_acquire::landing::ports::{
        ImportFile, ImportOrigin, ImportRequest, LandingLibrary,
    };
    use droppedneedle_acquire::sources::SabnzbdSource;
    use droppedneedle_acquire::usenet::policy::UsenetPolicy;
    use droppedneedle_acquire::usenet::sabnzbd::{SabnzbdClient, SabnzbdQueue};
    use droppedneedle_testkit::usenet::{SabnzbdMock, serve_loopback};
    use std::time::{Duration, SystemTime};

    const TASK: &str = "0123456789abcdef0123456789abcdef";

    let db = AcquireDb::scratch().expect("scratch db");
    let dir = db.path().parent().expect("scratch dir").to_path_buf();
    let config = Arc::new(
        ConfigStore::scratch(&dir, Crypto::from_key_bytes(&[7u8; 32]).expect("key"))
            .expect("config"),
    );
    let platform = TestPlatform::on(db.db(), &dir)
        .settings(config)
        .build()
        .await;
    let (library, _) = library_at(&platform);
    let music = dir.join("music");
    std::fs::create_dir_all(&music).expect("music root");
    library
        .scans
        .add_root(
            Some("music".to_owned()),
            music.to_string_lossy().into_owned(),
            EffectivePolicy::Automatic,
        )
        .expect("root adds");

    // A finished job's folder, old enough to be cleaned up.
    let complete = dir.join("complete");
    let folder = complete.join(format!("droppedneedle-{TASK}-0"));
    let file = land_file(&folder, "01.flac", "flac_full_01.flac", "Mysterons", 1);
    let release = release(
        GOOD_RELEASE,
        GOOD_GROUP,
        &["Mysterons"],
        &[length_ms(&file)],
    );
    std::fs::File::open(&folder)
        .and_then(|dir| dir.set_modified(SystemTime::now() - Duration::from_secs(7 * 3600)))
        .expect("folder aged");

    let port: Arc<dyn LandingLibrary> = Arc::new(LibraryLanding::new(
        &library,
        every_priority(Arc::new(Releases(vec![release.clone()]))),
    ));
    let acquire = acquire_over(&db, Some(port.clone()));
    let receipt = port
        .import(ImportRequest {
            task_id: TASK.to_owned(),
            origin: ImportOrigin::Download,
            staging: None,
            read_beneath: None,
            release,
            join: None,
            files: vec![ImportFile {
                path: file,
                track: 0,
                replaces: Vec::new(),
            }],
            extras: Vec::new(),
            actor: "u-ada".to_owned(),
        })
        .await
        .expect("import publishes");
    assert!(music.join("Portishead/Dummy/0101 Mysterons.flac").is_file());

    // SABnzbd no longer knows the job.
    let mock = SabnzbdMock::new();
    let (base, _server) = serve_loopback(mock.router()).await.expect("mock serves");
    let policy = UsenetPolicy::v2_defaults();
    let source = SabnzbdSource::new(
        Arc::new(SabnzbdQueue::new(
            SabnzbdClient::new(
                reqwest::Client::new(),
                &base,
                "SABKEY",
                3,
                Duration::from_millis(1),
            ),
            &base,
            "SABKEY",
            complete.clone(),
            policy.clone(),
        )),
        Vec::new(),
        policy,
        acquire.journal.clone(),
        None,
    );
    let worker = Arc::new(
        DownloadWorker::fixed(
            acquire.journal.clone(),
            vec![crate::as_source(Arc::new(source))],
            WorkerConfig {
                orphan_roots: vec![(
                    droppedneedle_acquire::downloads::SourceTag::Usenet,
                    complete.clone(),
                )],
                ..WorkerConfig::default()
            },
            droppedneedle_kernel::events::EventHub::new(),
            Default::default(),
        )
        .with_landing(acquire.landing.clone()),
    );

    // The publish journal as a stopped server leaves it at each step.
    for (state, kept) in [
        ("prepared", true),
        ("staged", true),
        ("published", true),
        ("committed", true),
        ("needs_attention", true),
        ("cleaned", false),
    ] {
        let bundle = receipt.bundle_id.clone();
        let changed = acquire
            .db
            .write("test.journal_state", move |tx| {
                Ok(tx.execute(
                    "UPDATE library_publish_journal SET state = ?1 WHERE bundle_id = ?2",
                    [state, bundle.as_str()],
                )?)
            })
            .await
            .expect("journal state set");
        assert_eq!(changed, 1, "the import wrote one journal row");
        // Pass 0 is one of the passes the cleanup runs on.
        worker.run_once(0).await;
        assert_eq!(folder.is_dir(), kept, "publish {state}");
    }
}

/// A failed download's files kept for "Retry import" outlive the hourly
/// sweep of finished download folders, however old the folder, and go to
/// cleanup once they have been kept for 90 days.
#[tokio::test]
async fn a_failed_downloads_kept_folder_stays_until_it_expires() {
    use droppedneedle_acquire::sources::SabnzbdSource;
    use droppedneedle_acquire::usenet::policy::UsenetPolicy;
    use droppedneedle_acquire::usenet::sabnzbd::{SabnzbdClient, SabnzbdQueue};
    use droppedneedle_testkit::usenet::{SabnzbdMock, serve_loopback};
    use std::time::{Duration, SystemTime};

    const TASK: &str = "0123456789abcdef0123456789abcdee";

    let db = AcquireDb::scratch().expect("scratch db");
    db.add_user("u-ada", "Ada", "user").await.expect("user");
    let dir = db.path().parent().expect("scratch dir").to_path_buf();
    let config = Arc::new(
        ConfigStore::scratch(&dir, Crypto::from_key_bytes(&[7u8; 32]).expect("key"))
            .expect("config"),
    );
    let platform = TestPlatform::on(db.db(), &dir)
        .settings(config)
        .build()
        .await;
    let (library, _) = library_at(&platform);
    let acquire = acquire_over(
        &db,
        Some(Arc::new(LibraryLanding::new(
            &library,
            every_priority(Arc::new(Releases(Vec::new()))),
        ))),
    );
    let complete = dir.join("complete");
    let job = format!("droppedneedle-{TASK}-0");
    let folder = complete.join(&job);
    land_file(&folder, "01.flac", "flac_full_01.flac", "Mysterons", 1);
    std::fs::File::open(&folder)
        .and_then(|dir| dir.set_modified(SystemTime::now() - Duration::from_secs(7 * 3600)))
        .expect("folder aged");
    let handle = serde_json::to_string(&SourceHandle {
        job_name: job.clone(),
        ..SourceHandle::new(SourceTag::Usenet)
    })
    .expect("handle");
    let now = db.now_unix_f64();
    acquire
        .journal
        .run("test.seed", move |store| {
            store.insert_task(&new_task(TASK, GOOD_GROUP), now)?;
            store.insert_attempt(
                "kept-a0",
                TASK,
                "usenet",
                0,
                &job,
                &handle,
                AttemptState::Preserved,
                now,
            )?;
            store.transition_task(
                TASK,
                TaskStatus::Failed,
                now,
                Some(TaskOutcome::new(Reason::LocalFault, "kept for a retry")),
            )
        })
        .await
        .expect("seed");

    let mock = SabnzbdMock::new();
    let (base, _server) = serve_loopback(mock.router()).await.expect("mock serves");
    let policy = UsenetPolicy::v2_defaults();
    let source = SabnzbdSource::new(
        Arc::new(SabnzbdQueue::new(
            SabnzbdClient::new(
                reqwest::Client::new(),
                &base,
                "SABKEY",
                3,
                Duration::from_millis(1),
            ),
            &base,
            "SABKEY",
            complete.clone(),
            policy.clone(),
        )),
        Vec::new(),
        policy,
        acquire.journal.clone(),
        None,
    );
    let worker = Arc::new(
        DownloadWorker::fixed(
            acquire.journal.clone(),
            vec![crate::as_source(Arc::new(source))],
            WorkerConfig {
                orphan_roots: vec![(SourceTag::Usenet, complete.clone())],
                ..WorkerConfig::default()
            },
            droppedneedle_kernel::events::EventHub::new(),
            Default::default(),
        )
        .with_landing(acquire.landing.clone()),
    );

    // Pass 0 is one of the passes the hourly sweep runs on.
    worker.run_once(0).await;
    assert!(folder.is_dir(), "kept files outlive the 6-hour sweep");
    assert!(
        acquire
            .dispatch
            .reimportable(TASK)
            .await
            .expect("reimportable"),
        "and can still be imported again"
    );

    // Kept 90 days: the sweep hands them to cleanup, which removes them.
    acquire
        .db
        .write("test.age", |tx| {
            Ok(tx.execute(
                "UPDATE download_attempts SET completed_at = 1.0, updated_at = 1.0",
                [],
            )?)
        })
        .await
        .expect("attempt aged");
    worker.run_once(0).await;
    worker.run_once(0).await;
    assert!(!folder.exists(), "expired kept files are cleaned up");
    assert!(
        !acquire
            .dispatch
            .reimportable(TASK)
            .await
            .expect("reimportable")
    );
}

/// A download does not start, and a finished one does not land, while
/// the library folder downloads go to is not there: both wait, the queue
/// names the folder, and nothing is read or decided meanwhile. A landing
/// that meets the folder gone anyway waits with `library_folder_missing`.
/// Both go ahead by themselves once the folder is back.
#[tokio::test]
async fn a_missing_library_folder_holds_downloads_until_it_is_back() {
    use droppedneedle_acquire::downloads::queue::DownloadQueue;

    let db = AcquireDb::scratch().expect("scratch db");
    db.add_user("u-ada", "Ada", "user").await.expect("user");
    let dir = db.path().parent().expect("scratch dir").to_path_buf();
    let config = Arc::new(
        ConfigStore::scratch(&dir, Crypto::from_key_bytes(&[7u8; 32]).expect("key"))
            .expect("config"),
    );
    let platform = TestPlatform::on(db.db(), &dir)
        .settings(config)
        .build()
        .await;
    let (library, _) = library_at(&platform);
    let music = dir.join("music");
    std::fs::create_dir_all(&music).expect("music root");
    library
        .scans
        .add_root(
            Some("music".to_owned()),
            music.to_string_lossy().into_owned(),
            EffectivePolicy::Automatic,
        )
        .expect("root adds");
    std::fs::remove_dir(&music).expect("music root unmounted");

    let downloads = dir.join("downloads");
    let files = vec![
        land_file(
            &downloads.join("job"),
            "01 - Mysterons.flac",
            "flac_full_01.flac",
            "Mysterons",
            1,
        ),
        land_file(
            &downloads.join("job"),
            "02 - Sour Times.flac",
            "flac_full_02.flac",
            "Sour Times",
            2,
        ),
    ];
    let lengths: Vec<Option<u64>> = files.iter().map(|path| length_ms(path)).collect();
    let releases: Arc<dyn ReleaseSource> = Arc::new(Releases(vec![release(
        GOOD_RELEASE,
        GOOD_GROUP,
        &["Mysterons", "Sour Times"],
        &lengths,
    )]));
    let acquire = acquire_over(
        &db,
        Some(Arc::new(LibraryLanding::new(
            &library,
            every_priority(releases),
        ))),
    );
    let source = Arc::new(FixedSource::new(downloads.clone(), files.clone()));
    let worker = Arc::new(
        DownloadWorker::fixed(
            acquire.journal.clone(),
            vec![crate::as_source(source.clone())],
            WorkerConfig::default(),
            droppedneedle_kernel::events::EventHub::new(),
            Default::default(),
        )
        .with_landing(acquire.landing.clone()),
    );
    let queue = DownloadQueue::new(acquire.journal.clone(), worker.clone());
    let viewer = HeldViewer {
        user_id: "u-ada".to_owned(),
        admin: true,
    };

    // "landing" finished its transfer before the folder went; "queued"
    // has not started.
    let handle = serde_json::to_string(&SourceHandle {
        filenames: files
            .iter()
            .map(|path| path.to_string_lossy().into_owned())
            .collect(),
        job_name: "fixed-landing-0".to_owned(),
        ..SourceHandle::new(SourceTag::Plugin("fixed".to_owned()))
    })
    .expect("handle");
    acquire
        .journal
        .run("test.seed", move |store| {
            store.insert_task(&new_task("landing", GOOD_GROUP), 1.0)?;
            store.insert_attempt(
                "landing-a0",
                "landing",
                "plugin:fixed",
                0,
                "fixed-landing-0",
                &handle,
                AttemptState::InUse,
                1.0,
            )?;
            store.transition_task("landing", TaskStatus::Downloading, 1.0, None)?;
            store.insert_task(&new_task("queued", BAD_GROUP), 1.0)
        })
        .await
        .expect("seed");

    for pass in 1..=3 {
        worker.run_once(pass).await;
        worker.wait_for_landings().await;
    }
    let waiting = format!("Waiting: library folder {} isn't there.", music.display());
    let queued = queue.get(&viewer, "queued").await.expect("queued task");
    assert_eq!(queued.row.task.status, TaskStatus::Queued);
    assert_eq!(queued.facts.attempts, 0, "nothing was fetched");
    assert_eq!(queued.reason, Some(Reason::LibraryFolderMissing));
    assert_eq!(queued.reason_text.as_deref(), Some(waiting.as_str()));
    let landing = queue.get(&viewer, "landing").await.expect("landing task");
    assert_eq!(landing.row.task.status, TaskStatus::Downloading, "it waits");
    assert_eq!(landing.reason, Some(Reason::LibraryFolderMissing));
    assert_eq!(landing.reason_text.as_deref(), Some(waiting.as_str()));
    let decisions: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM download_import_decisions")
        .fetch_one(acquire.db.db().sqlx())
        .await
        .expect("decision count");
    assert_eq!(decisions, 0, "nothing was landed while it waits");
    assert!(
        files.iter().all(|path| path.is_file()),
        "files wait in place"
    );

    // A landing that meets the folder gone waits on it, naming it.
    let task = acquire.journal.read_task("landing").await.unwrap().unwrap();
    let landed = Landed {
        mounts: vec![downloads.clone().into()],
        files: files.iter().cloned().map(Reported::path).collect(),
    };
    let LandingResult::Rejected(rejection) = acquire
        .landing
        .land(&task, None, landed, false)
        .await
        .result
    else {
        panic!("the landing went ahead without its library folder");
    };
    assert_eq!(
        (rejection.code, rejection.detail.as_str()),
        (Reason::LibraryFolderMissing, music.to_str().unwrap())
    );

    // The folder is back: the landing imports and the queued task starts.
    std::fs::create_dir_all(&music).expect("music root back");
    worker.run_once(3).await;
    worker.wait_for_landings().await;
    let landed = acquire.journal.read_task("landing").await.unwrap().unwrap();
    assert_eq!(
        landed.status,
        TaskStatus::Completed,
        "{:?}",
        landed.error_message
    );
    assert!(music.join("Portishead/Dummy/0101 Mysterons.flac").is_file());
    let started = queue.get(&viewer, "queued").await.expect("queued task");
    assert!(started.facts.attempts > 0, "the queued download started");
}

/// Downloads wait only for the folder they go to: a read-only first
/// folder that keeps its own tags does not hold downloads bound for an
/// Automatic folder after it.
#[tokio::test]
async fn a_read_only_archive_folder_does_not_hold_downloads() {
    use std::os::unix::fs::PermissionsExt as _;

    let db = AcquireDb::scratch().expect("scratch db");
    db.add_user("u-ada", "Ada", "user").await.expect("user");
    let dir = db.path().parent().expect("scratch dir").to_path_buf();
    let config = Arc::new(
        ConfigStore::scratch(&dir, Crypto::from_key_bytes(&[7u8; 32]).expect("key"))
            .expect("config"),
    );
    let platform = TestPlatform::on(db.db(), &dir)
        .settings(config)
        .build()
        .await;
    let (library, _) = library_at(&platform);
    let archive = dir.join("archive");
    let music = dir.join("music");
    for (name, folder, policy) in [
        ("archive", &archive, EffectivePolicy::LocalMetadata),
        ("music", &music, EffectivePolicy::Automatic),
    ] {
        std::fs::create_dir_all(folder).expect("root");
        library
            .scans
            .add_root(
                Some(name.to_owned()),
                folder.to_string_lossy().into_owned(),
                policy,
            )
            .expect("root adds");
    }
    std::fs::set_permissions(&archive, std::fs::Permissions::from_mode(0o555))
        .expect("archive read-only");
    let port: Arc<dyn LandingLibrary> = Arc::new(LibraryLanding::new(
        &library,
        every_priority(Arc::new(Releases(Vec::new()))),
    ));
    assert_eq!(port.import_folder(), Some(music.clone()));
    let acquire = acquire_over(&db, Some(port));
    let downloads = dir.join("downloads");
    std::fs::create_dir_all(&downloads).expect("downloads");
    let worker = Arc::new(
        DownloadWorker::fixed(
            acquire.journal.clone(),
            vec![crate::as_source(Arc::new(FixedSource::new(
                downloads,
                Vec::new(),
            )))],
            WorkerConfig::default(),
            droppedneedle_kernel::events::EventHub::new(),
            Default::default(),
        )
        .with_landing(acquire.landing.clone()),
    );
    acquire
        .journal
        .run("test.seed", |store| {
            store.insert_task(&new_task("t-archive", GOOD_GROUP), 1.0)
        })
        .await
        .expect("task");
    worker.run_once(1).await;
    worker.wait_for_landings().await;
    std::fs::set_permissions(&archive, std::fs::Permissions::from_mode(0o755))
        .expect("archive writable again");
    assert_eq!(worker.library_wait(), None);
    let attempts = acquire
        .journal
        .run("test.attempts", |store| store.list_attempts("t-archive"))
        .await
        .expect("attempts");
    assert!(!attempts.is_empty(), "the download started");
}

// Files fetched for a library copy: "Get missing tracks" and upgrades.

const TITLES: [&str; 2] = ["Mysterons", "Sour Times"];
const FIXTURES: [&str; 2] = ["flac_full_01.flac", "flac_full_02.flac"];

/// A MusicBrainz id of one scenario: kind 1 is a release group, 2 a
/// release, 3 a recording and 4 a release track.
fn mb(scenario: u16, kind: u16, index: u16) -> String {
    format!("6d0b7a1e-{scenario:04x}-4{kind:03x}-8000-{index:012x}")
}

/// Edition `number` of scenario `scenario`'s album, in release group
/// `group`; `discs` lists the tracks (indexes into [`TITLES`]) per disc.
/// Recordings are shared by every edition of the scenario.
fn edition(
    scenario: u16,
    group: u16,
    number: u16,
    discs: &[&[usize]],
    lengths: &[Option<u64>],
) -> Release {
    let mut tracks = Vec::new();
    for (disc, indexes) in discs.iter().enumerate() {
        for (at, &index) in indexes.iter().enumerate() {
            tracks.push(ReleaseTrack {
                isrcs: Vec::new(),
                genres: Vec::new(),
                id: mb(scenario, 4, number * 16 + index as u16),
                recording_id: mb(scenario, 3, index as u16),
                title: TITLES[index].to_owned(),
                artists: Vec::new(),
                disc: disc as u32 + 1,
                position: at as u32 + 1,
                absolute_position: tracks.len() as u32 + 1,
                length_ms: lengths[index],
            });
        }
    }
    Release {
        id: mb(scenario, 2, number),
        release_group_id: mb(scenario, 1, group),
        title: format!("Dummy {scenario}"),
        artists: vec![CreditedArtist {
            id: "8f6bd1e4-fbe1-4f50-aa9b-94c450ec0f11".to_owned(),
            name: "Portishead".to_owned(),
            sort_name: Some("Portishead".to_owned()),
            join: String::new(),
        }],
        status: Some("Official".to_owned()),
        media: discs
            .iter()
            .enumerate()
            .map(|(disc, indexes)| ReleaseMedium {
                position: disc as u32 + 1,
                format: Some("CD".to_owned()),
                title: None,
                track_count: indexes.len() as u32,
            })
            .collect(),
        tracks,
        ..Release::default()
    }
}

/// The editions of one scenario: OLD (the copy's tags, one disc), NEW
/// (two discs, "Sour Times" alone on disc 2), NEW2 (another edition) and
/// OTHER (a release of another album).
struct Editions {
    group: String,
    old: Release,
    new: Release,
    new2: Release,
    other: Release,
}

fn editions(scenario: u16, lengths: &[Option<u64>]) -> Editions {
    Editions {
        group: mb(scenario, 1, 0),
        old: edition(scenario, 0, 1, &[&[0, 1]], lengths),
        new: edition(scenario, 0, 2, &[&[0], &[1]], lengths),
        new2: edition(scenario, 0, 3, &[&[0, 1]], lengths),
        other: edition(scenario, 1, 9, &[&[0, 1]], lengths),
    }
}

fn fixture_lengths() -> Vec<Option<u64>> {
    FIXTURES
        .iter()
        .map(|name| length_ms(&fixture(name)))
        .collect()
}

/// Acquisition's view of the same releases, for the album target rewrite.
struct Lookup(Vec<Release>);

impl droppedneedle_acquire::target::lookup::AlbumLookup for Lookup {
    fn recording_releases<'a>(
        &'a self,
        _recording_mbid: &'a str,
    ) -> BoxFuture<
        'a,
        Result<
            Option<Vec<droppedneedle_acquire::target::lookup::ReleaseCandidate>>,
            droppedneedle_acquire::target::lookup::LookupError,
        >,
    > {
        Box::pin(async { Ok(None) })
    }

    fn release<'a>(
        &'a self,
        release_mbid: &'a str,
    ) -> BoxFuture<
        'a,
        Result<
            Option<droppedneedle_acquire::target::lookup::AlbumRelease>,
            droppedneedle_acquire::target::lookup::LookupError,
        >,
    > {
        use droppedneedle_acquire::target::lookup::{AlbumRelease, AlbumTrack};
        let found = self
            .0
            .iter()
            .find(|release| release.answers_to(release_mbid))
            .map(|release| AlbumRelease {
                id: release.id.clone(),
                title: release.title.clone(),
                release_group_mbid: release.release_group_id.clone(),
                artist: "Portishead".to_owned(),
                various_artists: false,
                year: None,
                tracks: release
                    .tracks
                    .iter()
                    .map(|track| AlbumTrack {
                        disc: track.disc,
                        position: track.position,
                        title: track.title.clone(),
                        duration_seconds: track.length_ms.map(|ms| ms as f64 / 1000.0),
                        recording_mbid: track.recording_id.clone(),
                        release_track_mbid: track.id.clone(),
                    })
                    .collect(),
            });
        Box::pin(async move { Ok(found) })
    }

    fn first_release_date<'a>(
        &'a self,
        _release_group_mbid: &'a str,
    ) -> BoxFuture<'a, Result<Option<String>, droppedneedle_acquire::target::lookup::LookupError>>
    {
        Box::pin(async { Ok(None) })
    }
}

/// The real worker, landing, publisher and catalog over one scratch
/// database, with scripted MusicBrainz.
struct JoinRig {
    acquire: droppedneedle_acquire::AcquireSetup,
    library: LibrarySetup,
    targets: Arc<droppedneedle_acquire::target::Targets>,
    music: PathBuf,
    downloads: PathBuf,
    _auth: AuthRig,
}

async fn join_rig(releases: Vec<Release>) -> JoinRig {
    let (db, dir, auth) = bare_db().await;
    let config = Arc::new(
        ConfigStore::scratch(&dir, Crypto::from_key_bytes(&[7u8; 32]).expect("key"))
            .expect("config"),
    );
    let platform = TestPlatform::on(db.db(), &dir)
        .settings(config)
        .build()
        .await;
    let (library, _) = library_at(&platform);
    let music = dir.join("music");
    std::fs::create_dir_all(&music).expect("music root");
    library
        .scans
        .add_root(
            Some("music".to_owned()),
            music.to_string_lossy().into_owned(),
            EffectivePolicy::Automatic,
        )
        .expect("root adds");
    let acquire = acquire_over(
        &db,
        Some(Arc::new(LibraryLanding::new(
            &library,
            every_priority(Arc::new(Releases(releases.clone()))),
        ))),
    );
    let targets = Arc::new(droppedneedle_acquire::target::Targets::new(
        acquire.journal.clone(),
        Some(Arc::new(Lookup(releases))),
    ));
    JoinRig {
        acquire,
        library,
        targets,
        music,
        downloads: dir.join("downloads"),
        _auth: auth,
    }
}

impl JoinRig {
    fn sql(&self, sql: &str) {
        self.library
            .scan_store
            .db()
            .run_sql(sql)
            .expect("test sql runs");
    }

    async fn scan(&self) {
        use droppedneedle_library::scan::{ScanKind, ScanRequest, ScanTrigger};
        let registry = self.library.core.live_registry();
        self.library
            .coordinator
            .request_run(&ScanRequest {
                kind: ScanKind::Incremental,
                trigger: ScanTrigger::Manual,
                scopes: registry.scheduled_root_scopes(),
                requested_by_user_id: None,
                policy_revision: registry.policy_revision().to_owned(),
            })
            .expect("scan requested");
        self.library.scans.scan_startup_recovery().await;
        for _ in 0..100 {
            self.library.scans.supervisor_tick().await;
            if self.library.coordinator.current().is_empty() {
                break;
            }
        }
        assert!(self.library.coordinator.current().is_empty());
    }

    /// Plant one file of a copy, tagged as track `index` of `release`.
    fn plant(&self, rel: &str, index: usize, release: &Release) -> PathBuf {
        self.plant_from(rel, FIXTURES[index], index, release)
    }

    fn plant_from(&self, rel: &str, from: &str, index: usize, release: &Release) -> PathBuf {
        let path = self.music.join(rel);
        std::fs::create_dir_all(path.parent().expect("folder")).expect("copy folder");
        std::fs::copy(fixture(from), &path).expect("copy fixture");
        let track = release
            .tracks
            .iter()
            .find(|track| track.title == TITLES[index])
            .expect("track on release");
        let tag = |field, value: &str| TagEdit::new(field, vec![value.to_owned()]);
        save_tags(
            &path,
            &[
                tag(TagField::Title, TITLES[index]),
                tag(TagField::Artist, "Portishead"),
                tag(TagField::AlbumArtist, "Portishead"),
                tag(TagField::Album, &release.title),
                tag(TagField::TrackNumber, &track.position.to_string()),
                tag(TagField::DiscNumber, &track.disc.to_string()),
                tag(TagField::MusicBrainzReleaseId, &release.id),
                tag(
                    TagField::MusicBrainzReleaseGroupId,
                    &release.release_group_id,
                ),
                tag(TagField::MusicBrainzRecordingId, &track.recording_id),
                tag(TagField::MusicBrainzReleaseTrackId, &track.id),
            ],
        )
        .expect("copy tagged");
        path
    }

    fn track_at(&self, rel: &str) -> String {
        self.library
            .scan_store
            .track_at("music", rel)
            .unwrap()
            .expect("track indexed")
    }

    fn album_of(&self, track: &str) -> String {
        self.library
            .scan_store
            .album_for_track(track)
            .unwrap()
            .expect("track has an album")
    }

    /// The album's MusicBrainz match: `source` names `release`, chosen by
    /// u-ada at time 5, on its third revision. No match state.
    fn set_edition(&self, album: &str, group: &str, release: &str, source: &str) {
        self.sql(&format!(
            "DELETE FROM local_album_external_identities WHERE local_album_id = '{album}'; \
             DELETE FROM library_album_match_state WHERE local_album_id = '{album}'; \
             INSERT INTO local_album_external_identities (local_album_id, provider, \
             release_group_mbid, release_mbid, decision_source, selected_by_user_id, \
             selected_at, row_revision) VALUES ('{album}', 'musicbrainz', '{group}', \
             '{release}', '{source}', 'u-ada', 5, 3);"
        ));
    }

    /// Track `track` placed on `release`'s track `index` by `source`.
    fn place(&self, track: &str, release: &Release, index: usize, source: &str) {
        let on = release
            .tracks
            .iter()
            .find(|on| on.title == TITLES[index])
            .expect("track on release");
        self.sql(&format!(
            "DELETE FROM local_track_external_identities WHERE local_track_id = '{track}'; \
             INSERT INTO local_track_external_identities (local_track_id, provider, \
             recording_mbid, release_mbid, release_track_mbid, medium_position, \
             release_track_position, decision_source, selected_at) VALUES ('{track}', \
             'musicbrainz', '{}', '{}', '{}', {}, {}, '{source}', 5);",
            on.recording_id, release.id, on.id, on.disc, on.position
        ));
    }

    /// A play and a star on `track` by u-ada.
    fn listen(&self, track: &str) {
        self.sql(&format!(
            "INSERT INTO library_play_history (id, user_id, local_track_id, local_album_id, \
             track_name, artist_name, played_at) SELECT 'play-{track}', 'u-ada', id, \
             local_album_id, title, 'Portishead', '2026-01-01T00:00:00Z' FROM local_tracks \
             WHERE id = '{track}'; \
             INSERT INTO library_user_favorites (user_id, item_kind, item_id, created_at) \
             VALUES ('u-ada', 'track', '{track}', 0);"
        ));
    }

    /// A queued album download of `group` on `release`, fetched for
    /// `planned` (album id, release) when given, asking for `wanted`. Its
    /// plan has no tracklist, so the album target rewrite runs.
    #[allow(clippy::too_many_arguments)]
    async fn ask(
        &self,
        task_id: &str,
        group: &str,
        title: &str,
        origin: &str,
        release: Option<&str>,
        planned: Option<(&str, &str)>,
        wanted: &[(u32, u32)],
    ) {
        self.ask_replacing(task_id, group, title, origin, release, planned, wanted, &[])
            .await;
    }

    /// [`Self::ask`], with the planned copy's `replace` list: (release
    /// track, track) pairs.
    #[allow(clippy::too_many_arguments)]
    async fn ask_replacing(
        &self,
        task_id: &str,
        group: &str,
        title: &str,
        origin: &str,
        release: Option<&str>,
        planned: Option<(&str, &str)>,
        wanted: &[(u32, u32)],
        replace: &[(&str, &str)],
    ) {
        let task = NewTask {
            album_title: title.to_owned(),
            origin: origin.to_owned(),
            ..new_task(task_id, group)
        };
        let details = droppedneedle_acquire::downloads::store::TaskDetails {
            release_mbid: release.map(str::to_owned),
            ..Default::default()
        };
        let plan: TaskPlan = match planned {
            Some((album_id, planned_release)) => serde_json::from_value(serde_json::json!({
                "release_mbid": release,
                "album_wanted": wanted
                    .iter()
                    .map(|(disc, track)| serde_json::json!({"disc": disc, "track": track}))
                    .collect::<Vec<_>>(),
                "planned_album": {
                    "album_id": album_id,
                    "release_mbid": planned_release,
                    "replace": replace
                        .iter()
                        .map(|(on, track)| serde_json::json!({
                            "release_track_mbid": on,
                            "local_track_id": track,
                        }))
                        .collect::<Vec<_>>(),
                },
            }))
            .expect("plan"),
            None => TaskPlan::default(),
        };
        let id = task_id.to_owned();
        self.acquire
            .journal
            .run("test.seed", move |store| {
                store.insert_task(&task, 1.0)?;
                store.set_task_details(&id, &details, 1.0)?;
                store.set_task_plan(&id, &plan, 1.0)
            })
            .await
            .expect("task");
        if planned.is_some() {
            let row = self.task(task_id).await;
            self.targets.target(&row).await.expect("album target");
        }
    }

    async fn task(&self, task_id: &str) -> droppedneedle_acquire::downloads::store::TaskRow {
        self.acquire
            .journal
            .read_task(task_id)
            .await
            .expect("task read")
            .expect("task exists")
    }

    async fn plan(&self, task_id: &str) -> TaskPlan {
        let id = task_id.to_owned();
        self.acquire
            .journal
            .read("test.plan", move |store| store.task_plan(&id))
            .await
            .expect("plan reads")
    }

    /// The finished download of scenario `release`'s "Sour Times" (disc 2,
    /// track 1, as on NEW), or of `index` laid out on `release`.
    fn landed(&self, task_id: &str, release: &Release, index: usize, ext_fixture: &str) -> PathBuf {
        let track = release
            .tracks
            .iter()
            .find(|track| track.title == TITLES[index])
            .expect("track on release");
        let dir = self.downloads.join(task_id);
        std::fs::create_dir_all(&dir).expect("download dir");
        let extension = Path::new(ext_fixture)
            .extension()
            .and_then(|ext| ext.to_str())
            .expect("extension");
        let path = dir.join(format!(
            "{:02} - {}.{extension}",
            track.position, TITLES[index]
        ));
        std::fs::copy(fixture(ext_fixture), &path).expect("copy fixture");
        let mut edits = vec![
            TagEdit::new(TagField::Title, vec![TITLES[index].to_owned()]),
            TagEdit::new(TagField::Artist, vec!["Portishead".to_owned()]),
            TagEdit::new(TagField::AlbumArtist, vec!["Portishead".to_owned()]),
            TagEdit::new(TagField::Album, vec![release.title.clone()]),
            TagEdit::new(TagField::TrackNumber, vec![track.position.to_string()]),
            TagEdit::new(TagField::DiscNumber, vec![track.disc.to_string()]),
        ];
        for field in [
            TagField::MusicBrainzRecordingId,
            TagField::MusicBrainzReleaseTrackId,
            TagField::MusicBrainzReleaseId,
            TagField::MusicBrainzReleaseGroupId,
        ] {
            edits.push(TagEdit::new(field, Vec::new()));
        }
        save_tags(&path, &edits).expect("download tagged");
        path
    }

    fn worker(&self, paths: Vec<PathBuf>, retry: bool) -> Arc<DownloadWorker> {
        let retry = droppedneedle_acquire::downloads::watchdog::RetryPolicy {
            enabled: retry,
            base_interval_minutes: 0.0,
            ..Default::default()
        };
        Arc::new(
            DownloadWorker::fixed(
                self.acquire.journal.clone(),
                vec![crate::as_source(Arc::new(FixedSource::new(
                    self.downloads.clone(),
                    paths,
                )))],
                WorkerConfig {
                    retry,
                    ..WorkerConfig::default()
                },
                droppedneedle_kernel::events::EventHub::new(),
                Default::default(),
            )
            .with_landing(self.acquire.landing.clone()),
        )
    }

    /// Run the worker until the landing of what it finds settles.
    async fn land(&self, paths: Vec<PathBuf>) {
        let worker = self.worker(paths, false);
        worker.run_once(1).await;
        worker.wait_for_landings().await;
    }

    async fn scalar<T>(&self, sql: &str) -> T
    where
        T: Send + Unpin + for<'r> sqlx::Decode<'r, sqlx::Sqlite> + sqlx::Type<sqlx::Sqlite>,
    {
        sqlx::query_scalar(sql)
            .fetch_one(self.acquire.db.db().sqlx())
            .await
            .expect("query runs")
    }

    async fn album_count(&self) -> i64 {
        self.scalar("SELECT COUNT(*) FROM local_albums WHERE retired_into_album_id IS NULL")
            .await
    }

    /// The track at `rel`: id, album, membership (source, locked) and
    /// availability.
    async fn row_at(&self, rel: &str) -> (String, String, String, i64, String) {
        sqlx::query_as(
            "SELECT id, local_album_id, membership_source, membership_locked, availability \
             FROM local_tracks WHERE root_id = 'music' AND relative_path = ?1",
        )
        .bind(rel)
        .fetch_one(self.acquire.db.db().sqlx())
        .await
        .expect("a track at the path")
    }

    /// A track's MusicBrainz identity: recording, release, release
    /// track, disc, position and source.
    async fn identity(&self, track: &str) -> Option<TrackIdentityRow> {
        sqlx::query_as(
            "SELECT recording_mbid, release_mbid, release_track_mbid, medium_position, \
             release_track_position, decision_source FROM local_track_external_identities \
             WHERE local_track_id = ?1",
        )
        .bind(track)
        .fetch_optional(self.acquire.db.db().sqlx())
        .await
        .expect("identity read")
    }

    /// The album's identity row: release, source, chooser, time, revision.
    async fn album_identity(
        &self,
        album: &str,
    ) -> (Option<String>, String, Option<String>, f64, i64) {
        sqlx::query_as(
            "SELECT release_mbid, decision_source, selected_by_user_id, selected_at, \
             row_revision FROM local_album_external_identities WHERE local_album_id = ?1",
        )
        .bind(album)
        .fetch_one(self.acquire.db.db().sqlx())
        .await
        .expect("album identity")
    }

    /// Where u-ada's play and star of `track` point: (play track, play
    /// album, starred).
    async fn history(&self, track: &str) -> (String, String, bool) {
        sqlx::query_as(
            "SELECT p.local_track_id, p.local_album_id, EXISTS(SELECT 1 FROM \
             library_user_favorites f WHERE f.item_kind = 'track' AND f.item_id = ?2) \
             FROM library_play_history p WHERE p.id = ?1",
        )
        .bind(format!("play-{track}"))
        .bind(track)
        .fetch_one(self.acquire.db.db().sqlx())
        .await
        .expect("history")
    }
}

type TrackIdentityRow = (
    String,
    Option<String>,
    Option<String>,
    Option<i64>,
    Option<i64>,
    String,
);

/// The placement `release`'s track `index` gives, as a person's.
fn placed_on(release: &Release, index: usize) -> TrackIdentityRow {
    let on = release
        .tracks
        .iter()
        .find(|on| on.title == TITLES[index])
        .expect("track on release");
    (
        on.recording_id.clone(),
        Some(release.id.clone()),
        Some(on.id.clone()),
        Some(i64::from(on.disc)),
        Some(i64::from(on.position)),
        "manual".to_owned(),
    )
}

/// The names the import gives "Mysterons" and "Sour Times" on NEW.
const MYSTERONS: &str = "0101 Mysterons.flac";
const SOUR_TIMES: &str = "0201 Sour Times.flac";

/// A file of scenario `scenario`'s copy, in a folder of its own. Copy 4
/// sits where the naming template puts its album.
fn copy_rel(scenario: u16, name: &str) -> String {
    if scenario == 4 {
        format!("Portishead/Dummy 4/{name}")
    } else {
        format!("Copy {scenario}/{name}")
    }
}

// "Get missing tracks" on a copy whose files carry another edition's
// release id: the fetched track joins that copy, beside its files, and is
// placed on the edition a person chose. An old row at its path keeps its
// id, plays and star when it is that copy's own; never otherwise. Each
// scenario is one copy, all in one library.
#[tokio::test]
async fn a_fetched_track_joins_the_copy_it_was_fetched_for() {
    let lengths = fixture_lengths();
    let scenarios: Vec<Editions> = (1..=9).map(|s| editions(s, &lengths)).collect();
    let releases: Vec<Release> = scenarios
        .iter()
        .flat_map(|e| {
            [
                e.old.clone(),
                e.new.clone(),
                e.new2.clone(),
                e.other.clone(),
            ]
        })
        .collect();
    let rig = join_rig(releases).await;
    let ed = |s: u16| &scenarios[usize::from(s) - 1];

    // Every copy holds "Mysterons"; copies 2 to 5 also had "Sour Times",
    // whose file is gone. Copy 8 is merged into copy 9 later.
    let copies = [1, 2, 3, 4, 5, 7, 8, 9];
    for s in copies {
        rig.plant(&copy_rel(s, MYSTERONS), 0, &ed(s).old);
    }
    for s in 2..=5 {
        rig.plant(&copy_rel(s, SOUR_TIMES), 1, &ed(s).old);
    }
    rig.scan().await;
    let x: Vec<Option<String>> = (1..=9)
        .map(|s| {
            (2..=5)
                .contains(&s)
                .then(|| rig.track_at(&copy_rel(s, SOUR_TIMES)))
        })
        .collect();
    for s in 2..=5 {
        std::fs::remove_file(rig.music.join(copy_rel(s, SOUR_TIMES))).expect("file gone");
    }
    rig.scan().await;
    let albums: Vec<String> = (1..=9)
        .map(|s| {
            if copies.contains(&s) {
                rig.album_of(&rig.track_at(&copy_rel(s, MYSTERONS)))
            } else {
                String::new()
            }
        })
        .collect();
    let album = |s: u16| albums[usize::from(s) - 1].as_str();
    let x_of = |s: u16| x[usize::from(s) - 1].clone().expect("an old row");
    for s in copies {
        rig.set_edition(album(s), &ed(s).group, &ed(s).new.id, "manual");
        rig.place(
            &rig.track_at(&copy_rel(s, MYSTERONS)),
            &ed(s).new,
            0,
            "manual",
        );
    }
    for s in 2..=5 {
        rig.place(&x_of(s), &ed(s).new, 1, "manual");
        rig.listen(&x_of(s));
    }
    rig.sql(&format!(
        "UPDATE local_tracks SET availability = 'excluded', manual_excluded = 1 \
         WHERE id = '{}'",
        x_of(5)
    ));
    let before = rig.album_count().await;
    let rig = &rig;
    let ask = |s: u16, task: &'static str| {
        let e = ed(s);
        let copy = album(s);
        async move {
            rig.ask(
                task,
                &e.group,
                &e.new.title,
                "user",
                Some(&e.new.id),
                Some((copy, &e.new.id)),
                &[(2, 1)],
            )
            .await;
        }
    };

    // 1. No row for the track. The first attempt fails; its automatic
    // retry fetches for the same copy.
    ask(1, "t-join-1").await;
    rig.sql(
        "UPDATE download_tasks SET status = 'failed', completed_at = 1, updated_at = 1 \
         WHERE id = 't-join-1'",
    );
    let identity_before = rig.album_identity(album(1)).await;
    let retrying = rig.worker(
        vec![rig.landed("t-join-1", &ed(1).new, 1, FIXTURES[1])],
        true,
    );
    retrying.run_once(1).await;
    retrying.run_once(2).await;
    retrying.wait_for_landings().await;
    let fetched = copy_rel(1, SOUR_TIMES);
    let (track, on, source, locked, availability) = rig.row_at(&fetched).await;
    assert_eq!(
        (on.as_str(), source.as_str(), locked, availability.as_str()),
        (album(1), "manual", 1, "indexed"),
        "the retry's file joins the copy, beside its files, locked"
    );
    assert_eq!(rig.album_count().await, before, "no new album");
    assert_eq!(
        rig.identity(&track).await,
        Some(placed_on(&ed(1).new, 1)),
        "placed on the chosen edition's disc 2"
    );
    assert_eq!(
        rig.album_identity(album(1)).await,
        identity_before,
        "the copy's match is left as it was"
    );

    // 2. The copy's own old row at the path: the file takes its id, and
    // its play and star stay where they were.
    ask(2, "t-join-2").await;
    rig.land(vec![rig.landed("t-join-2", &ed(2).new, 1, FIXTURES[1])])
        .await;
    let (track, on, _, locked, availability) = rig.row_at(&copy_rel(2, SOUR_TIMES)).await;
    assert_eq!(
        (track.as_str(), on.as_str(), locked, availability.as_str()),
        (x_of(2).as_str(), album(2), 1, "indexed")
    );
    assert_eq!(rig.identity(&track).await, Some(placed_on(&ed(2).new, 1)));
    assert_eq!(
        rig.history(&x_of(2)).await,
        (x_of(2), album(2).to_owned(), true)
    );

    // 3. The copy's edition changed to another edition of the album after
    // the ask: the file still joins it under the old row's id, unplaced.
    ask(3, "t-join-3").await;
    rig.set_edition(album(3), &ed(3).group, &ed(3).new2.id, "manual");
    let identity_before = rig.album_identity(album(3)).await;
    rig.land(vec![rig.landed("t-join-3", &ed(3).new, 1, FIXTURES[1])])
        .await;
    let (track, on, _, locked, availability) = rig.row_at(&copy_rel(3, SOUR_TIMES)).await;
    assert_eq!(
        (track.as_str(), on.as_str(), locked, availability.as_str()),
        (x_of(3).as_str(), album(3), 1, "indexed")
    );
    assert_eq!(rig.identity(&track).await, None, "its old place is gone");
    assert_eq!(
        rig.history(&x_of(3)).await,
        (x_of(3), album(3).to_owned(), true)
    );
    assert_eq!(rig.album_identity(album(3)).await, identity_before);

    // 3b. The copy now names another album: the file is filed by its
    // tags under a new id, and the old row keeps everything, off the path.
    ask(4, "t-join-4").await;
    rig.set_edition(album(4), &mb(4, 1, 1), &ed(4).other.id, "manual");
    let (missing_since, old_identity): (f64, Option<String>) = sqlx::query_as(
        "SELECT t.missing_since, i.release_track_mbid FROM local_tracks t \
         LEFT JOIN local_track_external_identities i ON i.local_track_id = t.id WHERE t.id = ?1",
    )
    .bind(x_of(4))
    .fetch_one(rig.acquire.db.db().sqlx())
    .await
    .expect("old row");
    rig.land(vec![rig.landed("t-join-4", &ed(4).new, 1, FIXTURES[1])])
        .await;
    let (filed, filed_on, _, _, _) = rig.row_at(&copy_rel(4, SOUR_TIMES)).await;
    assert_ne!(filed, x_of(4), "a new id");
    assert_ne!(filed_on, album(4), "filed by its tags");
    let kept: (String, String, f64, Option<String>, String) = sqlx::query_as(
        "SELECT t.local_album_id, t.availability, t.missing_since, i.release_track_mbid, \
         t.relative_path FROM local_tracks t LEFT JOIN local_track_external_identities i \
         ON i.local_track_id = t.id WHERE t.id = ?1",
    )
    .bind(x_of(4))
    .fetch_one(rig.acquire.db.db().sqlx())
    .await
    .expect("old row");
    assert_eq!(
        (kept.0.as_str(), kept.1.as_str(), kept.2, kept.3),
        (album(4), "missing", missing_since, old_identity)
    );
    assert_ne!(kept.4, copy_rel(4, SOUR_TIMES), "off the path");
    assert_eq!(
        rig.history(&x_of(4)).await,
        (x_of(4), album(4).to_owned(), true)
    );
    let on_copy: i64 = rig
        .scalar(&format!(
            "SELECT COUNT(*) FROM local_tracks WHERE local_album_id = '{}'",
            album(4)
        ))
        .await;
    assert_eq!(on_copy, 2, "nothing joins the copy");

    // An excluded row at the path is never brought back: the file joins
    // under a new id, placed, and the row keeps its id, off the path.
    ask(5, "t-join-5").await;
    rig.land(vec![rig.landed("t-join-5", &ed(5).new, 1, FIXTURES[1])])
        .await;
    let (track, on, _, _, availability) = rig.row_at(&copy_rel(5, SOUR_TIMES)).await;
    assert_ne!(track, x_of(5));
    assert_eq!((on.as_str(), availability.as_str()), (album(5), "indexed"));
    assert_eq!(rig.identity(&track).await, Some(placed_on(&ed(5).new, 1)));
    let excluded: (String, String) =
        sqlx::query_as("SELECT availability, relative_path FROM local_tracks WHERE id = ?1")
            .bind(x_of(5))
            .fetch_one(rig.acquire.db.db().sqlx())
            .await
            .expect("excluded row");
    assert_eq!(excluded.0, "excluded");
    assert_ne!(excluded.1, copy_rel(5, SOUR_TIMES));

    // A custom edition sealed after the ask: the file joins, unplaced.
    ask(7, "t-join-7").await;
    rig.sql(&format!(
        "INSERT INTO library_custom_edition_manifests (id, local_album_id, version, \
         release_group_mbid, album_title, album_artist_name, source_album_revision, \
         input_revision, content_hash, sealed_by_user_id, sealed_at) \
         VALUES ('custom-7', '{0}', 1, '{1}', 'T', 'A', 1, 'i', 'h', 'u-ada', 0); \
         INSERT INTO library_custom_edition_active (local_album_id, manifest_id, \
         activated_at) VALUES ('{0}', 'custom-7', 0);",
        album(7),
        ed(7).group
    ));
    rig.land(vec![rig.landed("t-join-7", &ed(7).new, 1, FIXTURES[1])])
        .await;
    let (track, on, _, locked, _) = rig.row_at(&copy_rel(7, SOUR_TIMES)).await;
    assert_eq!((on.as_str(), locked), (album(7), 1));
    assert_eq!(rig.identity(&track).await, None);

    // The copy merged into another after the ask: the file follows it and
    // is placed there when it names the same edition.
    ask(8, "t-join-8").await;
    rig.sql(&format!(
        "UPDATE local_tracks SET local_album_id = '{1}' WHERE local_album_id = '{0}'; \
         UPDATE local_albums SET retired_into_album_id = '{1}' WHERE id = '{0}'; \
         UPDATE local_album_external_identities SET release_group_mbid = '{2}', \
         release_mbid = '{3}' WHERE local_album_id = '{1}';",
        album(8),
        album(9),
        ed(8).group,
        ed(8).new.id
    ));
    rig.land(vec![rig.landed("t-join-8", &ed(8).new, 1, FIXTURES[1])])
        .await;
    // Its files now sit in two folders: the file goes where the template
    // puts it.
    let (track, on, _, _, _) = rig
        .row_at(&format!("Portishead/Dummy 8/{SOUR_TIMES}"))
        .await;
    assert_eq!(on, album(9), "on the album its copy joined");
    assert_eq!(rig.identity(&track).await, Some(placed_on(&ed(8).new, 1)));

    // A later scan files nothing elsewhere.
    rig.scan().await;
    for (s, rel) in [(1, copy_rel(1, SOUR_TIMES)), (2, copy_rel(2, SOUR_TIMES))] {
        let (track, on, _, _, availability) = rig.row_at(&rel).await;
        assert_eq!((on.as_str(), availability.as_str()), (album(s), "indexed"));
        assert_eq!(rig.identity(&track).await, Some(placed_on(&ed(s).new, 1)));
    }
}

// An automatic edition: the fetched track joins the copy and the import's
// own seal places it on that edition, without confirming the match.
#[tokio::test]
async fn a_fetched_track_joins_a_copy_matched_automatically() {
    let lengths = fixture_lengths();
    let e = editions(1, &lengths);
    let rig = join_rig(vec![e.old.clone(), e.new.clone()]).await;
    rig.plant(&copy_rel(1, MYSTERONS), 0, &e.old);
    rig.scan().await;
    let album = rig.album_of(&rig.track_at(&copy_rel(1, MYSTERONS)));
    rig.set_edition(&album, &e.group, &e.new.id, "automatic");
    let before = rig.album_count().await;
    rig.ask(
        "t-auto",
        &e.group,
        &e.new.title,
        "user",
        Some(&e.new.id),
        Some((&album, &e.new.id)),
        &[(2, 1)],
    )
    .await;
    rig.land(vec![rig.landed("t-auto", &e.new, 1, FIXTURES[1])])
        .await;
    let (track, on, _, locked, _) = rig.row_at(&copy_rel(1, SOUR_TIMES)).await;
    assert_eq!((on.as_str(), locked), (album.as_str(), 1));
    assert_eq!(rig.album_count().await, before);
    let identity = rig.identity(&track).await.expect("sealed");
    assert_eq!(
        (identity.2.as_deref(), identity.5.as_str()),
        (
            Some(placed_on(&e.new, 1).2.as_deref().unwrap_or_default()),
            "automatic"
        )
    );
    assert_eq!(
        rig.album_identity(&album).await.4,
        3,
        "the match is not resealed"
    );
}

// An import's match and its automatic-management queueing are part of the
// catalog commit that adds its files, so a stop around that commit never
// leaves one without the other: stopped right after it, the file already
// holds its identity and the album is queued; stopped just before it, the
// commit recovery resumes seals and queues the same way.
#[tokio::test]
async fn an_import_stopped_around_its_commit_keeps_its_match() {
    use droppedneedle_library::publish::CrashPoint;
    for point in [
        CrashPoint::AfterCatalogCommit,
        CrashPoint::BeforeCatalogCommit,
    ] {
        let lengths = fixture_lengths();
        let e = editions(1, &lengths);
        let rig = join_rig(vec![e.old.clone(), e.new.clone()]).await;
        rig.plant(&copy_rel(1, MYSTERONS), 0, &e.old);
        rig.scan().await;
        let album = rig.album_of(&rig.track_at(&copy_rel(1, MYSTERONS)));
        rig.set_edition(&album, &e.group, &e.new.id, "automatic");
        rig.sql("DELETE FROM library_management_automatic_queue");
        rig.ask(
            "t-stop",
            &e.group,
            &e.new.title,
            "user",
            Some(&e.new.id),
            Some((&album, &e.new.id)),
            &[(2, 1)],
        )
        .await;
        let row = rig.task("t-stop").await;
        let landed = rig.landed("t-stop", &e.new, 1, FIXTURES[1]);
        rig.library.publishing.arm_publish_crash(Some(point));
        rig.acquire
            .landing
            .land(
                &row,
                None,
                Landed {
                    files: vec![Reported {
                        path: landed,
                        size: None,
                    }],
                    mounts: vec![rig.downloads.clone().into()],
                },
                false,
            )
            .await;
        rig.library.publishing.arm_publish_crash(None);
        if point == CrashPoint::BeforeCatalogCommit {
            rig.library
                .publishing
                .publish_tick()
                .expect("recovery runs");
        }

        let (track, on, _, _, availability) = rig.row_at(&copy_rel(1, SOUR_TIMES)).await;
        assert_eq!(
            (on.as_str(), availability.as_str()),
            (album.as_str(), "indexed"),
            "{point:?}"
        );
        let identity = rig
            .identity(&track)
            .await
            .unwrap_or_else(|| panic!("sealed with the commit at {point:?}"));
        assert_eq!(
            (identity.2.as_deref(), identity.5.as_str()),
            (
                Some(placed_on(&e.new, 1).2.as_deref().unwrap_or_default()),
                "automatic"
            ),
            "{point:?}"
        );
        let queued: i64 = rig
            .scalar(&format!(
                "SELECT COUNT(*) FROM library_management_automatic_queue \
                 WHERE local_album_id = '{album}'"
            ))
            .await;
        assert_eq!(
            queued, 1,
            "queued for automatic management with the commit at {point:?}"
        );
    }
}

// A fetched track joining a copy in a folder that keeps its own tags (a
// Local metadata path rule inside an automatically managed folder) lands
// as it was downloaded, byte for byte, and is still matched; the naming
// script alone would have put it outside that rule. A landing stopped just
// before its catalog commit recovers the same way, with the folder's
// policy on its row. The same download into an ordinary folder is tagged.
#[tokio::test]
async fn a_download_into_a_folder_that_keeps_its_own_tags_keeps_them() {
    use droppedneedle_kernel::runtime_config::secret_sections::{
        IdentificationPolicy, LibraryPathRule, TypedLibrary,
    };
    use droppedneedle_library::publish::CrashPoint;
    use sha2::{Digest as _, Sha256};
    let sha256 = |path: &Path| -> String {
        Sha256::digest(std::fs::read(path).expect("file reads"))
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect()
    };
    for (own, crash) in [
        (true, None),
        (true, Some(CrashPoint::BeforeCatalogCommit)),
        (false, None),
    ] {
        let case = format!("own tags {own}, stopped {crash:?}");
        let lengths = fixture_lengths();
        let e = editions(1, &lengths);
        let rig = join_rig(vec![e.old.clone(), e.new.clone()]).await;
        rig.plant(&copy_rel(1, MYSTERONS), 0, &e.old);
        rig.scan().await;
        let album = rig.album_of(&rig.track_at(&copy_rel(1, MYSTERONS)));
        rig.set_edition(&album, &e.group, &e.new.id, "manual");
        if own {
            rig.library
                .settings
                .store()
                .update_secret_blocking::<TypedLibrary, String, _>(|stored| {
                    let mut library = stored.into_inner();
                    for root in &mut library.library_roots {
                        root.rules.push(LibraryPathRule {
                            id: "own-rule".to_owned(),
                            relative_path: "Copy 1".to_owned(),
                            policy: IdentificationPolicy::LocalMetadata,
                        });
                    }
                    Ok(library)
                })
                .expect("path rule saved");
            rig.library.core.refresh_registry();
        }
        rig.ask(
            "t-own",
            &e.group,
            &e.new.title,
            "user",
            Some(&e.new.id),
            Some((&album, &e.new.id)),
            &[(2, 1)],
        )
        .await;
        let row = rig.task("t-own").await;
        let landed = rig.landed("t-own", &e.new, 1, FIXTURES[1]);
        // An album tag the release's tags would change.
        let downloaded_album = e.new.title.to_lowercase();
        save_tags(
            &landed,
            &[TagEdit::new(
                TagField::Album,
                vec![downloaded_album.clone()],
            )],
        )
        .expect("download retagged");
        let downloaded = sha256(&landed);
        rig.library.publishing.arm_publish_crash(crash);
        rig.acquire
            .landing
            .land(
                &row,
                None,
                Landed {
                    files: vec![Reported {
                        path: landed,
                        size: None,
                    }],
                    mounts: vec![rig.downloads.clone().into()],
                },
                false,
            )
            .await;
        rig.library.publishing.arm_publish_crash(None);
        if crash.is_some() {
            rig.library
                .publishing
                .publish_tick()
                .expect("recovery runs");
        }

        let (track, on, _, _, availability) = rig.row_at(&copy_rel(1, SOUR_TIMES)).await;
        assert_eq!(
            (on.as_str(), availability.as_str()),
            (album.as_str(), "indexed"),
            "{case}"
        );
        let identity = rig
            .identity(&track)
            .await
            .unwrap_or_else(|| panic!("matched: {case}"));
        assert_eq!(identity.2, placed_on(&e.new, 1).2, "{case}");
        let placed = rig.music.join(copy_rel(1, SOUR_TIMES));
        let format = droppedneedle_library::tags::format_for_path(&placed).expect("format");
        let (tag, _) = droppedneedle_library::tags::read::read_scan_metadata(&placed, format)
            .expect("tags read");
        let policy: String = rig
            .scalar(&format!(
                "SELECT applied_policy FROM local_tracks WHERE id = '{track}'"
            ))
            .await;
        if own {
            assert_eq!(sha256(&placed), downloaded, "{case}");
            assert_eq!(tag.album, downloaded_album, "{case}");
            assert_eq!(policy, "local_metadata", "{case}");
        } else {
            assert_ne!(sha256(&placed), downloaded, "{case}");
            assert_eq!(tag.album, e.new.title, "{case}");
            assert_eq!(policy, "automatic", "{case}");
        }
    }
}

// A plain download skips a first library folder that keeps its own tags
// for the first one with automatic tags, and is tagged there. When every
// folder keeps its own tags, it lands in the first, as it came. Missing
// tracks of an album the first folder already holds stay with it.
#[tokio::test]
async fn a_plain_download_prefers_a_folder_with_automatic_tags() {
    use droppedneedle_kernel::runtime_config::secret_sections::{
        IdentificationPolicy, TypedLibrary,
    };
    use sha2::{Digest as _, Sha256};
    let sha256 = |path: &Path| -> String {
        Sha256::digest(std::fs::read(path).expect("file reads"))
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect()
    };
    for (second, owned) in [
        (EffectivePolicy::Automatic, false),
        (EffectivePolicy::LocalMetadata, false),
        (EffectivePolicy::Automatic, true),
    ] {
        let case = format!("second folder {second:?}, album already held {owned}");
        let lengths = fixture_lengths();
        let e = editions(1, &lengths);
        let rig = join_rig(vec![e.new2.clone()]).await;
        rig.library
            .settings
            .store()
            .update_secret_blocking::<TypedLibrary, String, _>(|stored| {
                let mut library = stored.into_inner();
                for root in &mut library.library_roots {
                    root.policy = IdentificationPolicy::LocalMetadata;
                }
                Ok(library)
            })
            .expect("first folder keeps its own tags");
        let second_dir = rig.music.with_file_name("second");
        std::fs::create_dir_all(&second_dir).expect("second root");
        rig.library
            .scans
            .add_root(
                Some("second".to_owned()),
                second_dir.to_string_lossy().into_owned(),
                second,
            )
            .expect("second root adds");
        if owned {
            rig.plant(&copy_rel(1, MYSTERONS), 0, &e.new2);
            rig.scan().await;
        }
        rig.ask(
            "t-plain",
            &e.group,
            &e.new2.title,
            "user",
            Some(&e.new2.id),
            None,
            &[],
        )
        .await;
        let wanted = if owned { 1..2 } else { 0..2 };
        let files: Vec<PathBuf> = wanted
            .map(|index| rig.landed("t-plain", &e.new2, index, FIXTURES[index]))
            .collect();
        let downloaded: Vec<String> = files.iter().map(|file| sha256(file)).collect();
        rig.land(files).await;

        let rows: Vec<(String, String, String)> = sqlx::query_as(
            "SELECT root_id, relative_path, applied_policy FROM local_tracks \
             ORDER BY relative_path",
        )
        .fetch_all(rig.acquire.db.db().sqlx())
        .await
        .expect("tracks");
        assert_eq!(rows.len(), 2, "{case}: {rows:?}");
        if owned {
            for (root, _, _) in &rows {
                assert_eq!(root, "music", "{case}: {rows:?}");
            }
            continue;
        }
        for (row, downloaded) in rows.iter().zip(&downloaded) {
            let (root, rel, policy) = row;
            if second == EffectivePolicy::Automatic {
                assert_eq!(
                    (root.as_str(), policy.as_str()),
                    ("second", "automatic"),
                    "{case}"
                );
                assert_ne!(&sha256(&second_dir.join(rel)), downloaded, "{case}");
            } else {
                assert_eq!(
                    (root.as_str(), policy.as_str()),
                    ("music", "local_metadata"),
                    "{case}"
                );
                assert_eq!(&sha256(&rig.music.join(rel)), downloaded, "{case}");
            }
        }
    }
}

// An upgrade lands in the album its old copy was on, on the edition a
// person chose, even when the copy's files carry another edition's tags;
// the old copy's play follows. With two copies of the album, "Get missing
// tracks" on one matches that copy's edition and leaves the other alone,
// even when the other holds the recording.
#[tokio::test]
async fn upgrades_and_second_copies_keep_to_their_album() {
    let lengths = fixture_lengths();
    let e = editions(1, &lengths);
    let two = editions(2, &lengths);
    // Copy B of album 2 is on Q, an edition of its own.
    let q = edition(2, 0, 5, &[&[0, 1]], &lengths);
    let rig = join_rig(vec![
        e.old.clone(),
        e.new.clone(),
        two.old.clone(),
        two.new.clone(),
        q.clone(),
    ])
    .await;

    // Upgrade: "Mysterons" is an mp3 on NEW's track 1, with a play.
    let mp3 = copy_rel(1, "0101 Mysterons.mp3");
    rig.plant_from(&mp3, "mp3_full_01.mp3", 0, &e.old);
    rig.plant(&copy_rel(1, SOUR_TIMES), 1, &e.old);
    // Copy B (older) of album 2, then copy A.
    rig.plant(&copy_rel(20, MYSTERONS), 0, &two.old);
    rig.plant(&copy_rel(20, SOUR_TIMES), 1, &two.old);
    rig.scan().await;
    rig.plant(&copy_rel(21, MYSTERONS), 0, &two.old);
    rig.scan().await;
    let old = rig.track_at(&mp3);
    let album = rig.album_of(&old);
    rig.set_edition(&album, &e.group, &e.new.id, "manual");
    rig.place(&old, &e.new, 0, "manual");
    rig.place(&rig.track_at(&copy_rel(1, SOUR_TIMES)), &e.new, 1, "manual");
    rig.listen(&old);
    let copy_b = rig.album_of(&rig.track_at(&copy_rel(20, MYSTERONS)));
    let copy_a = rig.album_of(&rig.track_at(&copy_rel(21, MYSTERONS)));
    assert_ne!(copy_a, copy_b, "two copies");
    rig.set_edition(&copy_b, &two.group, &q.id, "manual");
    rig.set_edition(&copy_a, &two.group, &two.new.id, "manual");
    rig.sql(&format!(
        "UPDATE local_albums SET created_at = 1 WHERE id = '{copy_b}'; \
         UPDATE local_albums SET created_at = 2 WHERE id = '{copy_a}';"
    ));
    let before = rig.album_count().await;

    rig.ask(
        "t-upgrade",
        &e.group,
        &e.new.title,
        "upgrade",
        None,
        None,
        &[],
    )
    .await;
    rig.land(vec![rig.landed(
        "t-upgrade",
        &e.new,
        0,
        "flac_full_01.flac",
    )])
    .await;
    let (flac, on, _, locked, _) = rig.row_at(&copy_rel(1, MYSTERONS)).await;
    assert_ne!(flac, old);
    assert_eq!((on.as_str(), locked), (album.as_str(), 1));
    assert_eq!(rig.album_count().await, before);
    assert_eq!(rig.identity(&flac).await, Some(placed_on(&e.new, 0)));
    assert!(
        !rig.music.join(&mp3).exists(),
        "the mp3 is in the recycle bin"
    );
    assert_eq!(
        rig.history(&old).await.0,
        flac,
        "the play follows the new file"
    );
    let alias: String = rig
        .scalar(&format!(
            "SELECT local_track_id FROM local_track_aliases WHERE alias = '{old}'"
        ))
        .await;
    assert_eq!(alias, flac, "the mp3's id opens the flac");

    // Two copies: "Get missing tracks" on copy A.
    let b_before: Vec<(String, String)> = sqlx::query_as(
        "SELECT id, relative_path FROM local_tracks WHERE local_album_id = ?1 ORDER BY id",
    )
    .bind(&copy_b)
    .fetch_all(rig.acquire.db.db().sqlx())
    .await
    .expect("copy B");
    rig.ask(
        "t-copy-a",
        &two.group,
        &two.new.title,
        "user",
        Some(&two.new.id),
        Some((&copy_a, &two.new.id)),
        &[(2, 1)],
    )
    .await;
    rig.land(vec![rig.landed("t-copy-a", &two.new, 1, FIXTURES[1])])
        .await;
    let (track, on, _, _, _) = rig.row_at(&copy_rel(21, SOUR_TIMES)).await;
    assert_eq!(on, copy_a);
    assert_eq!(rig.identity(&track).await, Some(placed_on(&two.new, 1)));
    let b_after: Vec<(String, String)> = sqlx::query_as(
        "SELECT id, relative_path FROM local_tracks WHERE local_album_id = ?1 ORDER BY id",
    )
    .bind(&copy_b)
    .fetch_all(rig.acquire.db.db().sqlx())
    .await
    .expect("copy B");
    assert_eq!(b_after, b_before, "copy B is untouched");
}

// A joined import stopped before its catalog commit is finished by
// recovery exactly as it would have been: joined and placed. Until then
// the same download landing again is refused and leaves the record that
// recovery reads alone; the staging folder goes once the journal settles.
#[tokio::test]
async fn a_joined_import_recovers_joined() {
    let lengths = fixture_lengths();
    let e = editions(1, &lengths);
    let x_case = editions(2, &lengths);
    let rig = join_rig(vec![
        e.old.clone(),
        e.new.clone(),
        x_case.old.clone(),
        x_case.new.clone(),
    ])
    .await;
    rig.plant(&copy_rel(1, MYSTERONS), 0, &e.old);
    rig.plant(&copy_rel(2, MYSTERONS), 0, &x_case.old);
    rig.plant(&copy_rel(2, SOUR_TIMES), 1, &x_case.old);
    rig.scan().await;
    let x = rig.track_at(&copy_rel(2, SOUR_TIMES));
    std::fs::remove_file(rig.music.join(copy_rel(2, SOUR_TIMES))).expect("file gone");
    rig.scan().await;
    let album = rig.album_of(&rig.track_at(&copy_rel(1, MYSTERONS)));
    let album_x = rig.album_of(&rig.track_at(&copy_rel(2, MYSTERONS)));
    rig.set_edition(&album, &e.group, &e.new.id, "manual");
    rig.set_edition(&album_x, &x_case.group, &x_case.new.id, "manual");
    rig.sql(&format!(
        "UPDATE local_tracks SET availability = 'excluded', manual_excluded = 1 WHERE id = '{x}'"
    ));
    let staging = rig.music.join(".droppedneedle-management-import");

    for (task, s, eds, copy) in [
        ("t-crash", 1u16, &e, &album),
        ("t-crash-x", 2, &x_case, &album_x),
    ] {
        rig.ask(
            task,
            &eds.group,
            &eds.new.title,
            "user",
            Some(&eds.new.id),
            Some((copy, &eds.new.id)),
            &[(2, 1)],
        )
        .await;
        rig.library.publishing.arm_publish_crash(Some(
            droppedneedle_library::publish::CrashPoint::BeforeCatalogCommit,
        ));
        let landed = rig.landed(task, &eds.new, 1, FIXTURES[1]);
        let row = rig.task(task).await;
        let landing = |paths: Vec<PathBuf>| Landed {
            files: paths
                .into_iter()
                .map(|path| Reported { path, size: None })
                .collect(),
            mounts: vec![rig.downloads.clone().into()],
        };
        rig.acquire
            .landing
            .land(&row, None, landing(vec![landed.clone()]), false)
            .await;
        rig.library.publishing.arm_publish_crash(None);
        let folder = staging.join(task);
        let record = std::fs::read(folder.join("provenance.json")).expect("record kept");

        // The same download lands again before recovery.
        let again = rig.landed(task, &eds.new, 1, FIXTURES[1]);
        let report = rig
            .acquire
            .landing
            .land(&row, None, landing(vec![again]), false)
            .await;
        assert!(
            matches!(report.result, LandingResult::Rejected(_)),
            "{:?}",
            report.result
        );
        assert_eq!(
            std::fs::read(folder.join("provenance.json")).expect("record kept"),
            record,
            "the record recovery reads is unchanged"
        );

        rig.library
            .publishing
            .publish_tick()
            .expect("recovery runs");
        let (track, on, _, locked, availability) = rig.row_at(&copy_rel(s, SOUR_TIMES)).await;
        assert_eq!(
            (on.as_str(), locked, availability.as_str()),
            (copy.as_str(), 1, "indexed")
        );
        assert_eq!(rig.identity(&track).await, Some(placed_on(&eds.new, 1)));
        if s == 2 {
            assert_ne!(track, x, "the excluded row is not brought back");
            let gone: String = rig
                .scalar(&format!(
                    "SELECT relative_path FROM local_tracks WHERE id = '{x}'"
                ))
                .await;
            assert_ne!(gone, copy_rel(2, SOUR_TIMES));
        }
    }
    crate::common::library::services(&rig.library)
        .run_recovery()
        .await;
    assert!(!staging.exists(), "staging removed once settled");
}

// The release track a file is fetched for is already held by another
// file of the copy: the new file joins the copy unplaced, and the holder
// keeps its place.
#[tokio::test]
async fn a_held_release_track_is_not_given_away() {
    use droppedneedle_acquire::landing::ports::{
        ImportFile, ImportOrigin, ImportRequest, LandingLibrary,
    };
    let lengths = fixture_lengths();
    let e = editions(1, &lengths);
    let rig = join_rig(vec![e.old.clone(), e.new.clone()]).await;
    rig.plant(&copy_rel(1, MYSTERONS), 0, &e.old);
    rig.plant(&copy_rel(1, "bonus.flac"), 1, &e.old);
    rig.scan().await;
    let holder = rig.track_at(&copy_rel(1, "bonus.flac"));
    let album = rig.album_of(&holder);
    rig.set_edition(&album, &e.group, &e.new.id, "manual");
    rig.place(&holder, &e.new, 1, "manual");
    let port = LibraryLanding::new(
        &rig.library,
        every_priority(Arc::new(Releases(vec![e.new.clone()]))),
    );
    let file = rig.landed("t-held", &e.new, 1, FIXTURES[1]);
    port.import(ImportRequest {
        task_id: "t-held".to_owned(),
        origin: ImportOrigin::Download,
        staging: None,
        read_beneath: None,
        release: e.new.clone(),
        join: Some(droppedneedle_library::import::ImportJoin {
            album_id: album.clone(),
            release_mbid: e.new.id.clone(),
            replaced: Vec::new(),
        }),
        files: vec![ImportFile {
            path: file,
            track: 1,
            replaces: Vec::new(),
        }],
        extras: Vec::new(),
        actor: "u-ada".to_owned(),
    })
    .await
    .expect("import publishes");
    let (track, on, _, locked, _) = rig.row_at(&copy_rel(1, SOUR_TIMES)).await;
    assert_eq!((on.as_str(), locked), (album.as_str(), 1));
    assert_eq!(rig.identity(&track).await, None);
    assert_eq!(rig.identity(&holder).await, Some(placed_on(&e.new, 1)));
}

/// One copy of scenario `s` on OLD's files whose edition a person set to
/// NEW, with a rig of its own. The copy's track `1 - index` is placed on
/// NEW; its track `index` sits at `o_name`, on no track of NEW: another
/// version of NEW's track, row O, with u-ada's play, star and playlist
/// entry and a title override. With `other_recording`, NEW's track is
/// another recording than O's.
struct VersionCase {
    rig: JoinRig,
    e: Editions,
    index: usize,
    album: String,
    old: String,
    o_name: String,
}

impl VersionCase {
    async fn new(s: u16, index: usize, other_recording: bool, o_name: &str) -> Self {
        let mut e = editions(s, &fixture_lengths());
        if other_recording {
            for track in e.new.tracks.iter_mut().filter(|t| t.title == TITLES[index]) {
                track.recording_id = mb(s, 3, 7);
            }
        }
        let rig = join_rig(vec![e.old.clone(), e.new.clone(), e.new2.clone()]).await;
        let kept = 1 - index;
        let kept_rel = copy_rel(s, [MYSTERONS, SOUR_TIMES][kept]);
        rig.plant(&kept_rel, kept, &e.old);
        rig.plant(&copy_rel(s, o_name), index, &e.old);
        rig.scan().await;
        let old = rig.track_at(&copy_rel(s, o_name));
        let album = rig.album_of(&old);
        rig.set_edition(&album, &e.group, &e.new.id, "manual");
        rig.place(&rig.track_at(&kept_rel), &e.new, kept, "manual");
        rig.listen(&old);
        rig.sql(&format!(
            "INSERT INTO playlists (id, name, created_at, updated_at) \
             VALUES ('list', 'L', '', ''); \
             INSERT INTO playlist_tracks (id, playlist_id, position, track_name, artist_name, \
             album_name, source_type, created_at, library_file_id) \
             VALUES ('entry', 'list', 0, 'T', 'A', 'B', 'local', '', '{old}'); \
             INSERT INTO library_management_overrides (id, subject_kind, local_track_id, \
             field_name, value_json, mode, subject_revision, created_at, updated_at) \
             VALUES ('title', 'track', '{old}', 'title', '\"Radio Edit\"', 'replace', 1, 0, 0);"
        ));
        Self {
            rig,
            e,
            index,
            album,
            old,
            o_name: o_name.to_owned(),
        }
    }

    /// "Get missing tracks" for NEW's track, replacing O.
    async fn ask(&self, task_id: &str) {
        let track = self
            .e
            .new
            .tracks
            .iter()
            .find(|track| track.title == TITLES[self.index])
            .expect("track on NEW");
        self.rig
            .ask_replacing(
                task_id,
                &self.e.group,
                &self.e.new.title,
                "user",
                Some(&self.e.new.id),
                Some((&self.album, &self.e.new.id)),
                &[(track.disc, track.position)],
                &[(&track.id, &self.old)],
            )
            .await;
    }

    /// The download of NEW's "Sour Times".
    fn sour(&self, task_id: &str) -> PathBuf {
        self.rig.landed(task_id, &self.e.new, 1, FIXTURES[1])
    }

    /// Land `file` for the task through the landing alone.
    async fn land_directly(&self, task_id: &str, file: PathBuf) -> LandingReport {
        let row = self.rig.task(task_id).await;
        self.rig
            .acquire
            .landing
            .land(
                &row,
                None,
                Landed {
                    files: vec![Reported::path(file)],
                    mounts: vec![self.rig.downloads.clone().into()],
                },
                false,
            )
            .await
    }
}

impl JoinRig {
    async fn availability(&self, track: &str) -> String {
        self.scalar(&format!(
            "SELECT availability FROM local_tracks WHERE id = '{track}'"
        ))
        .await
    }

    /// Where O's play, star, playlist entry and title override are:
    /// (play's track, starred track, entry's track, override's track).
    async fn version_history(&self, old: &str) -> (String, String, String, String) {
        sqlx::query_as(
            "SELECT (SELECT local_track_id FROM library_play_history WHERE id = 'play-' || ?1), \
             (SELECT item_id FROM library_user_favorites WHERE item_kind = 'track' \
               AND user_id = 'u-ada'), \
             (SELECT library_file_id FROM playlist_tracks WHERE id = 'entry'), \
             (SELECT local_track_id FROM library_management_overrides WHERE id = 'title')",
        )
        .bind(old)
        .fetch_one(self.acquire.db.db().sqlx())
        .await
        .expect("history")
    }

    /// The track O's id opens, when it is an alias.
    async fn alias_of(&self, old: &str) -> Option<String> {
        sqlx::query_scalar("SELECT local_track_id FROM local_track_aliases WHERE alias = ?1")
            .bind(old)
            .fetch_optional(self.acquire.db.db().sqlx())
            .await
            .expect("alias read")
    }

    /// Whether O's field overrides follow its alias (1) or stay (0).
    async fn overrides_follow(&self, old: &str) -> i64 {
        self.scalar(&format!(
            "SELECT moves_overrides FROM local_track_aliases WHERE alias = '{old}'"
        ))
        .await
    }

    async fn held_reasons(&self, task_id: &str) -> Vec<String> {
        sqlx::query_scalar("SELECT reason FROM acquire_held_files WHERE source_task_id = ?1")
            .bind(task_id)
            .fetch_all(self.acquire.db.db().sqlx())
            .await
            .expect("held read")
    }

    /// Whether a file named `name` sits in the root's recycle bin.
    fn in_recycle_bin(&self, name: &str) -> bool {
        fn find(dir: &Path, name: &str) -> bool {
            std::fs::read_dir(dir)
                .into_iter()
                .flatten()
                .flatten()
                .any(|entry| {
                    let path = entry.path();
                    if path.is_dir() {
                        find(&path, name)
                    } else {
                        path.file_name().is_some_and(|file| file == name)
                    }
                })
        }
        find(&self.music.join(".recycle"), name)
    }
}

// "Replace tracks you have in another version": the edition's version of
// a track lands in the copy, on its edition track; the person's version
// goes to the recycle bin and its play, star and playlist entry follow the
// new file, while its field overrides stay on it.
async fn version_replaced(s: u16, other_recording: bool) {
    let v = VersionCase::new(s, 1, other_recording, "02 Sour Times.flac").await;
    let rig = &v.rig;
    let before = rig.album_count().await;
    v.ask("t-version").await;
    rig.land(vec![v.sour("t-version")]).await;
    let (new, on, _, locked, availability) = rig.row_at(&copy_rel(s, SOUR_TIMES)).await;
    assert_eq!(
        (on.as_str(), locked, availability.as_str()),
        (v.album.as_str(), 1, "indexed"),
        "the edition's version is in the copy"
    );
    assert_ne!(new, v.old);
    assert_eq!(rig.identity(&new).await, Some(placed_on(&v.e.new, 1)));
    assert!(!rig.music.join(copy_rel(s, &v.o_name)).exists());
    assert!(rig.in_recycle_bin(&v.o_name), "O's file is recycled");
    assert_eq!(rig.availability(&v.old).await, "missing");
    assert_eq!(
        rig.version_history(&v.old).await,
        (new.clone(), new.clone(), new.clone(), v.old.clone()),
        "the history moves, the override stays on O"
    );
    assert_eq!(rig.alias_of(&v.old).await, Some(new.clone()));
    assert_eq!(rig.overrides_follow(&v.old).await, 0);
    assert!(rig.held_reasons("t-version").await.is_empty());
    assert_eq!(rig.album_count().await, before, "no album rows change");
    let file = rig.music.join(copy_rel(s, SOUR_TIMES));
    let format = droppedneedle_library::tags::format_for_path(&file).expect("format");
    let (tag, _) =
        droppedneedle_library::tags::read::read_scan_metadata(&file, format).expect("tags");
    assert_eq!(tag.title, "Sour Times", "not retagged with O's override");
}

// Another recording than NEW's.
#[tokio::test]
async fn a_version_of_another_recording_is_replaced() {
    version_replaced(11, true).await;
}

// The same recording as NEW's: replaced all the same, never kept as owned.
#[tokio::test]
async fn a_version_of_the_same_recording_is_replaced() {
    version_replaced(12, false).await;
}

// The edition's version came in worse than the person's: it is held, O is
// left exactly as it was, and the landing settles on our side, so no
// other source is asked and this one is not blamed.
#[tokio::test]
async fn a_worse_version_is_held_and_yours_stays() {
    let v = VersionCase::new(13, 0, false, "01 Mysterons.flac").await;
    let rig = &v.rig;
    let o_path = rig.music.join(copy_rel(13, &v.o_name));
    let o_bytes = std::fs::read(&o_path).expect("O's file");
    v.ask("t-version").await;
    let landed = rig.landed("t-version", &v.e.new, 0, "mp3_full_01.mp3");
    let report = v.land_directly("t-version", landed).await;
    assert!(
        matches!(&report.result, LandingResult::Held { code, .. } if *code == Reason::VersionLowerQuality),
        "{:?}",
        report.result
    );
    assert!(report.local_only, "the source is not blamed");
    assert_eq!(std::fs::read(&o_path).expect("O's file"), o_bytes);
    let (track, _, _, _, availability) = rig.row_at(&copy_rel(13, &v.o_name)).await;
    assert_eq!(
        (track.as_str(), availability.as_str()),
        (v.old.as_str(), "indexed")
    );
    assert_eq!(rig.alias_of(&v.old).await, None);
}

// The edition's version goes to O's own path. It takes an id of its own,
// so O keeps its override, and O's id opens the new track.
#[tokio::test]
async fn a_version_at_its_own_path_keeps_its_override() {
    let v = VersionCase::new(14, 1, true, SOUR_TIMES).await;
    let rig = &v.rig;
    v.ask("t-version").await;
    rig.land(vec![v.sour("t-version")]).await;
    let (new, on, _, _, availability) = rig.row_at(&copy_rel(14, SOUR_TIMES)).await;
    assert_eq!(
        (on.as_str(), availability.as_str()),
        (v.album.as_str(), "indexed")
    );
    assert_ne!(new, v.old);
    assert_eq!(
        rig.version_history(&v.old).await,
        (new.clone(), new.clone(), new.clone(), v.old.clone())
    );
    assert_eq!(rig.alias_of(&v.old).await, Some(new.clone()));
    assert_eq!(rig.overrides_follow(&v.old).await, 0);
}

// A version swap stopped part way and recovered as boot does: the new
// file is placed, the history is on it and O's file is in the recycle
// bin. The new file goes to a path of its own, stopped before the catalog
// commit, or to O's own path, stopped before O was set aside, so recovery
// sets O aside before it places the new file.
#[tokio::test]
async fn a_version_replacement_recovers_after_a_crash() {
    use droppedneedle_library::publish::CrashPoint;
    for (s, o_name, point) in [
        (15u16, "02 Sour Times.flac", CrashPoint::BeforeCatalogCommit),
        (18, SOUR_TIMES, CrashPoint::AfterStage),
    ] {
        let v = VersionCase::new(s, 1, true, o_name).await;
        let rig = &v.rig;
        v.ask("t-version").await;
        rig.library.publishing.arm_publish_crash(Some(point));
        v.land_directly("t-version", v.sour("t-version")).await;
        rig.library.publishing.arm_publish_crash(None);
        rig.library
            .publishing
            .publish_tick()
            .expect("recovery runs");
        let (new, on, _, _, availability) = rig.row_at(&copy_rel(s, SOUR_TIMES)).await;
        assert_eq!(
            (on.as_str(), availability.as_str()),
            (v.album.as_str(), "indexed"),
            "{s}"
        );
        assert_ne!(new, v.old, "{s}");
        assert_eq!(
            rig.identity(&new).await,
            Some(placed_on(&v.e.new, 1)),
            "{s}"
        );
        assert_eq!(
            rig.version_history(&v.old).await,
            (new.clone(), new.clone(), new.clone(), v.old.clone()),
            "{s}"
        );
        assert_eq!(rig.alias_of(&v.old).await, Some(new), "{s}");
        assert!(rig.in_recycle_bin(o_name), "{s}: O's file is recycled");
        assert_eq!(rig.availability(&v.old).await, "missing", "{s}");
    }
}

// A restart while the download runs: recovery reads the task's rows only,
// resumes polling the attempt it journaled, and the landing still joins
// the copy and replaces O from the plan on the row.
#[tokio::test]
async fn a_restart_mid_download_lands_from_the_task_row() {
    let v = VersionCase::new(20, 1, true, "02 Sour Times.flac").await;
    let rig = &v.rig;
    v.ask("t-version").await;
    let sour = v.sour("t-version");
    let handle = serde_json::to_string(&SourceHandle {
        filenames: vec![sour.to_string_lossy().into_owned()],
        job_name: "fixed-t-version-0".to_owned(),
        ..SourceHandle::new(SourceTag::Plugin("fixed".to_owned()))
    })
    .expect("handle");
    rig.acquire
        .journal
        .run("test.enqueued", move |store| {
            store.insert_attempt(
                "t-version-a0",
                "t-version",
                "plugin:fixed",
                0,
                "fixed-t-version-0",
                &handle,
                AttemptState::InUse,
                1.0,
            )?;
            store.transition_task("t-version", TaskStatus::Downloading, 1.0, None)
        })
        .await
        .expect("attempt journaled");

    let report = run_startup_recovery(&rig.acquire.journal)
        .await
        .expect("recovery runs");
    assert_eq!((report.resumed, report.restarted), (1, 0), "{report:?}");
    rig.land(vec![sour]).await;

    let (new, on, _, _, availability) = rig.row_at(&copy_rel(20, SOUR_TIMES)).await;
    assert_eq!(
        (on.as_str(), availability.as_str()),
        (v.album.as_str(), "indexed"),
        "the download joined the copy"
    );
    assert_ne!(new, v.old);
    assert!(rig.in_recycle_bin(&v.o_name), "O's file is replaced");
    assert_eq!(rig.alias_of(&v.old).await, Some(new));
}

// A version swap that stopped before its old copy could leave, unwound by
// recovery, leaves O where it was: a worse version arriving next is held
// and O, still in place, is the version that stays.
#[tokio::test]
async fn a_worse_version_after_a_stopped_swap_finds_yours_in_place() {
    let v = VersionCase::new(19, 0, false, "01 Mysterons.flac").await;
    let rig = &v.rig;
    let o_path = rig.music.join(copy_rel(19, &v.o_name));
    let o_bytes = std::fs::read(&o_path).expect("O's file");
    v.ask("t-version").await;
    rig.library.publishing.arm_publish_crash(Some(
        droppedneedle_library::publish::CrashPoint::AfterFirstFileStaged,
    ));
    let better = rig.landed("t-version", &v.e.new, 0, FIXTURES[0]);
    v.land_directly("t-version", better).await;
    rig.library.publishing.arm_publish_crash(None);
    rig.library
        .publishing
        .publish_tick()
        .expect("recovery runs");
    assert_eq!(std::fs::read(&o_path).expect("O's file"), o_bytes);
    assert!(!rig.in_recycle_bin(&v.o_name), "nothing went to the bin");
    assert_eq!(rig.availability(&v.old).await, "indexed");

    let worse = rig.landed("t-version", &v.e.new, 0, "mp3_full_01.mp3");
    let report = v.land_directly("t-version", worse).await;
    assert!(
        matches!(&report.result, LandingResult::Held { code, .. } if *code == Reason::VersionLowerQuality),
        "{:?}",
        report.result
    );
    assert_eq!(std::fs::read(&o_path).expect("O's file"), o_bytes);
    assert_eq!(rig.availability(&v.old).await, "indexed");
}

// After the ask the copy moved to another edition, or the app stepped
// back from it: nothing is recycled and O keeps its file and history.
#[tokio::test]
async fn a_stale_version_plan_replaces_nothing() {
    for (s, takeover) in [(16u16, false), (17, true)] {
        let v = VersionCase::new(s, 1, true, "02 Sour Times.flac").await;
        let rig = &v.rig;
        v.ask("t-version").await;
        if takeover {
            rig.sql(&format!(
                "INSERT INTO library_album_takeovers (local_album_id, cause, noticed_at) \
                 VALUES ('{}', 'files_changed', 1);",
                v.album
            ));
        } else {
            rig.set_edition(&v.album, &v.e.group, &v.e.new2.id, "manual");
        }
        rig.land(vec![v.sour("t-version")]).await;
        assert!(rig.music.join(copy_rel(s, &v.o_name)).exists(), "{s}");
        let (track, _, _, _, availability) = rig.row_at(&copy_rel(s, &v.o_name)).await;
        assert_eq!(
            (track.as_str(), availability.as_str()),
            (v.old.as_str(), "indexed")
        );
        let o = v.old.clone();
        assert_eq!(
            rig.version_history(&v.old).await,
            (o.clone(), o.clone(), o.clone(), o),
            "{s}"
        );
        assert_eq!(rig.alias_of(&v.old).await, None);
    }
}

// The album target rewrite, which fills in an edition's tracklist before
// the search, keeps the copy a download was fetched for while the
// plan's release is the one being searched, and drops it otherwise.
#[tokio::test]
async fn the_album_target_keeps_the_planned_copy_only_for_its_release() {
    let lengths = fixture_lengths();
    let e = editions(1, &lengths);
    let rig = join_rig(vec![e.new.clone(), e.new2.clone()]).await;
    let sour = &e.new.tracks[1].id;
    rig.ask_replacing(
        "t-same",
        &e.group,
        &e.new.title,
        "user",
        Some(&e.new.id),
        Some(("al-1", &e.new.id)),
        &[(2, 1)],
        &[(sour, "tr-1")],
    )
    .await;
    let plan = rig.plan("t-same").await;
    assert_eq!(plan.expected_tracks.len(), 2, "the rewrite ran");
    assert_eq!(
        plan.planned_album,
        Some(PlannedAlbum {
            album_id: "al-1".to_owned(),
            release_mbid: e.new.id.clone(),
            replace: vec![ReplaceVersion {
                release_track_mbid: sour.clone(),
                local_track_id: "tr-1".to_owned(),
            }],
        })
    );

    // The task now searches another edition than the plan names.
    rig.ask_replacing(
        "t-other",
        &e.group,
        &e.new.title,
        "user",
        Some(&e.new.id),
        Some(("al-1", &e.new.id)),
        &[(2, 1)],
        &[(sour, "tr-1")],
    )
    .await;
    let details = droppedneedle_acquire::downloads::store::TaskDetails {
        release_mbid: Some(e.new2.id.clone()),
        ..Default::default()
    };
    let mut stale = rig.plan("t-other").await;
    stale.expected_tracks.clear();
    rig.acquire
        .journal
        .run("test.details", move |store| {
            store.set_task_details("t-other", &details, 2.0)?;
            store.set_task_plan("t-other", &stale, 2.0)
        })
        .await
        .expect("details");
    let row = rig.task("t-other").await;
    rig.targets.target(&row).await.expect("album target");
    let plan = rig.plan("t-other").await;
    assert_eq!(plan.release_mbid.as_deref(), Some(e.new2.id.as_str()));
    assert!(plan.planned_album.is_none(), "{plan:?}");
}

// A request that failed follows its automatic retry: the successor's
// landing imports the album, the request reads imported and linked to the
// successor, and it is announced once, however often it settles again.
#[tokio::test]
async fn a_failed_request_follows_its_retry_to_imported() {
    let db = AcquireDb::scratch().expect("scratch db");
    db.add_user("u-ada", "Ada", "user").await.expect("user");
    let dir = db.path().parent().expect("scratch dir").to_path_buf();
    let config = Arc::new(
        ConfigStore::scratch(&dir, Crypto::from_key_bytes(&[7u8; 32]).expect("key"))
            .expect("config"),
    );
    let platform = TestPlatform::on(db.db(), &dir)
        .settings(config)
        .build()
        .await;
    let (library, _) = library_at(&platform);
    let music = dir.join("music");
    std::fs::create_dir_all(&music).expect("music root");
    library
        .scans
        .add_root(
            Some("music".to_owned()),
            music.to_string_lossy().into_owned(),
            EffectivePolicy::Automatic,
        )
        .expect("root adds");
    let downloads = dir.join("downloads");
    let good = vec![
        land_file(
            &downloads.join("good"),
            "01 - Mysterons.flac",
            "flac_full_01.flac",
            "Mysterons",
            1,
        ),
        land_file(
            &downloads.join("good"),
            "02 - Sour Times.flac",
            "flac_full_02.flac",
            "Sour Times",
            2,
        ),
    ];
    let lengths: Vec<Option<u64>> = good.iter().map(|path| length_ms(path)).collect();
    let releases: Arc<dyn ReleaseSource> = Arc::new(Releases(vec![release(
        GOOD_RELEASE,
        GOOD_GROUP,
        &["Mysterons", "Sour Times"],
        &lengths,
    )]));
    let acquire = acquire_over(
        &db,
        Some(Arc::new(LibraryLanding::new(
            &library,
            every_priority(releases),
        ))),
    );
    // The first attempt failed and its request says so.
    acquire
        .journal
        .run("test.seed", |store| {
            store.insert_task(&new_task("t-first", GOOD_GROUP), 1.0)?;
            store.transition_task(
                "t-first",
                TaskStatus::Failed,
                1.0,
                Some(TaskOutcome::new(Reason::NoSource, "no peers")),
            )
        })
        .await
        .expect("task");
    acquire
        .db
        .write("test.request", |tx| {
            tx.execute(
                "INSERT INTO request_history (musicbrainz_id_lower, musicbrainz_id, artist_name, \
                 album_title, requested_at, completed_at, status, user_id, download_task_id, \
                 request_kind) VALUES (?1, ?1, 'Portishead', 'Dummy', 1700000000000, \
                 1700000000000, 'failed', 'u-ada', 't-first', 'album')",
                [GOOD_GROUP],
            )?;
            Ok(())
        })
        .await
        .expect("request");
    let recorded = Arc::new(crate::common::bundles::FulfilledTicks::default());
    let journal = Arc::new(Journal::new(acquire.db.clone(), recorded.clone()));
    let worker = Arc::new(
        DownloadWorker::fixed(
            journal.clone(),
            vec![crate::as_source(Arc::new(FixedSource::new(
                downloads.clone(),
                good.clone(),
            )))],
            WorkerConfig::default(),
            droppedneedle_kernel::events::EventHub::new(),
            Default::default(),
        )
        .with_landing(acquire.landing.clone()),
    );
    worker.run_once(1).await;
    worker.run_once(2).await;
    worker.wait_for_landings().await;
    worker.run_once(3).await;
    worker.wait_for_landings().await;

    let rows: Vec<(String, String)> =
        sqlx::query_as("SELECT id, status FROM download_tasks ORDER BY retry_count")
            .fetch_all(acquire.db.db().sqlx())
            .await
            .expect("tasks");
    let (status, linked): (String, Option<String>) = sqlx::query_as(
        "SELECT status, download_task_id FROM request_history WHERE musicbrainz_id_lower = ?1",
    )
    .bind(GOOD_GROUP)
    .fetch_one(acquire.db.db().sqlx())
    .await
    .expect("request row");
    assert_eq!(rows.len(), 2, "one retry successor");
    assert_eq!(rows[1].1, "completed", "the successor imported");
    assert_eq!(
        linked.as_deref(),
        Some(rows[1].0.as_str()),
        "the request follows the retry"
    );
    assert_eq!(status, "imported", "the request reads imported");
    assert_eq!(recorded.keys(), [GOOD_GROUP], "announced once");

    // The status sweep finds nothing left to settle.
    let summary = droppedneedle_acquire::flows::loops::sync_tick(
        0,
        &droppedneedle_acquire::flows::loops::SyncDeps {
            ledger: acquire.flows.ledger.clone(),
            journal,
            library: acquire.flows.library.clone(),
            ticks: recorded.clone(),
        },
    )
    .await;
    assert_eq!((summary.settled, summary.imported), (0, 0));
    assert_eq!(recorded.keys().len(), 1, "nothing is announced twice");
}

/// A single requested as one track, landed as a file tagged `title` from
/// the album "Dummy", at the single's length: the landing's decisions.
async fn land_single(task_id: &'static str, title: &str) -> Vec<(String, Option<String>)> {
    const SINGLE_GROUP: &str = "6d0b7a1e-0000-4000-8000-000000000031";
    const SINGLE_RELEASE: &str = "6d0b7a1e-0000-4000-8000-000000000032";
    const RECORDING: &str = "6d0b7a1e-0000-4000-8000-000000000038";
    let db = AcquireDb::scratch().expect("scratch db");
    db.add_user("u-ada", "Ada", "user").await.expect("user");
    let dir = db.path().parent().expect("scratch dir").to_path_buf();
    let config = Arc::new(
        ConfigStore::scratch(&dir, Crypto::from_key_bytes(&[7u8; 32]).expect("key"))
            .expect("config"),
    );
    let platform = TestPlatform::on(db.db(), &dir)
        .settings(config)
        .build()
        .await;
    let (library, _) = library_at(&platform);
    let downloads = dir.join("downloads");
    let file = land_file(
        &downloads.join("Dummy"),
        &format!("04 - {title}.flac"),
        "flac_full_01.flac",
        title,
        4,
    );
    let mut single = release(
        SINGLE_RELEASE,
        SINGLE_GROUP,
        &["Love Me"],
        &[length_ms(&file)],
    );
    single.title = "Love Me".to_owned();
    assert_eq!(single.tracks[0].recording_id, RECORDING);
    let acquire = acquire_over(
        &db,
        Some(Arc::new(LibraryLanding::new(
            &library,
            every_priority(Arc::new(Releases(vec![single]))),
        ))),
    );
    let task = NewTask {
        album_title: "Love Me".to_owned(),
        ..new_task(task_id, SINGLE_GROUP)
    };
    acquire
        .journal
        .run("test.seed", move |store| {
            store.insert_track_task(&task, RECORDING, 1.0)
        })
        .await
        .expect("task");
    let row = acquire.journal.read_task(task_id).await.unwrap().unwrap();
    let landed = Landed {
        mounts: vec![downloads.into()],
        files: vec![Reported::path(file)],
    };
    acquire.landing.land(&row, None, landed, false).await;
    decisions(&acquire, task_id).await
}

/// The album's version is offered only for the same song: "Love Me Do"
/// by the same artist at the single's length is another song, held as a
/// weak match, while the album's "Love Me" is held as the album version.
#[tokio::test]
async fn only_the_same_song_counts_as_the_album_version() {
    assert_eq!(
        land_single("t-love-me", "Love Me").await,
        vec![("held".to_owned(), Some("album_version_only".to_owned()))]
    );
    assert_eq!(
        land_single("t-love-me-do", "Love Me Do").await,
        vec![("held".to_owned(), Some("weak_match".to_owned()))]
    );
}

/// A single requested by a person, which Soulseek offers only inside its
/// album's folder: the first landing holds it as "only the album version"
/// and the task stops there, with no other source tried. "Get it from the
/// album" then fetches the same track, and it imports as the requested
/// song. Runs the request service, the slskd source against the loopback
/// mock, the worker and the landing.
#[tokio::test]
async fn a_single_found_only_on_its_album_is_held_then_taken_from_the_album() {
    use droppedneedle_acquire::requests::models::{HistoryQuery, TrackIntake};
    use droppedneedle_acquire::requests::service::RequestsService;
    use droppedneedle_acquire::slskd::{
        DownloadPolicy, ReqwestSlskdHttp, SlskdClient, SlskdRepository,
    };
    use droppedneedle_acquire::sources::SlskdSource;
    use droppedneedle_acquire::target::Targets;
    use droppedneedle_acquire::target::lookup::{
        AlbumRelease, AlbumTrack, FixedAlbums, ReleaseCandidate,
    };
    use droppedneedle_testkit::slskd::{MOCK_API_KEY, MockSlskd};
    use std::time::Duration;

    const SINGLE_GROUP: &str = "6d0b7a1e-0000-4000-8000-000000000021";
    const SINGLE_RELEASE: &str = "6d0b7a1e-0000-4000-8000-000000000022";
    // The single's one track, as `release` numbers it.
    const RECORDING: &str = "6d0b7a1e-0000-4000-8000-000000000028";
    const REMOTE: &str = "@@music\\Portishead\\Dummy\\04 - Glory Box.flac";

    let db = AcquireDb::scratch().expect("scratch db");
    db.add_user("u-ada", "Ada", "trusted").await.expect("user");
    let dir = db.path().parent().expect("scratch dir").to_path_buf();
    let config = Arc::new(
        ConfigStore::scratch(&dir, Crypto::from_key_bytes(&[7u8; 32]).expect("key"))
            .expect("config"),
    );
    let platform = TestPlatform::on(db.db(), &dir)
        .settings(config)
        .build()
        .await;
    let (library, _) = library_at(&platform);
    let music = dir.join("music");
    std::fs::create_dir_all(&music).expect("music root");
    library
        .scans
        .add_root(
            Some("music".to_owned()),
            music.to_string_lossy().into_owned(),
            EffectivePolicy::Automatic,
        )
        .expect("root adds");

    // The album's track, as the peer's folder holds it.
    let downloads = dir.join("slskd");
    let place = || {
        land_file(
            &downloads.join("Dummy"),
            "04 - Glory Box.flac",
            "flac_full_01.flac",
            "Glory Box",
            4,
        )
    };
    let landed = place();
    let size = std::fs::metadata(&landed).expect("file").len();
    let length = length_ms(&landed);
    let seconds = length.map(|ms| ms as f64 / 1000.0);

    let mut single = release(SINGLE_RELEASE, SINGLE_GROUP, &["Glory Box"], &[length]);
    single.title = "Glory Box".to_owned();
    assert_eq!(single.tracks[0].recording_id, RECORDING);
    let releases: Arc<dyn ReleaseSource> = Arc::new(Releases(vec![single]));
    let acquire = acquire_over(
        &db,
        Some(Arc::new(LibraryLanding::new(
            &library,
            every_priority(releases),
        ))),
    );

    let mut albums = FixedAlbums::default();
    albums.recordings.insert(
        RECORDING.to_owned(),
        vec![ReleaseCandidate {
            id: SINGLE_RELEASE.to_owned(),
            status: Some("Official".to_owned()),
            date: Some("1995-02-27".to_owned()),
            release_group_mbid: SINGLE_GROUP.to_owned(),
            primary_type: Some("Single".to_owned()),
            secondary_types: Vec::new(),
        }],
    );
    albums.releases.insert(
        SINGLE_RELEASE.to_owned(),
        AlbumRelease {
            id: SINGLE_RELEASE.to_owned(),
            title: "Glory Box".to_owned(),
            release_group_mbid: SINGLE_GROUP.to_owned(),
            artist: "Portishead".to_owned(),
            various_artists: false,
            year: Some(1995),
            tracks: vec![AlbumTrack {
                disc: 1,
                position: 1,
                title: "Glory Box".to_owned(),
                duration_seconds: seconds,
                recording_mbid: RECORDING.to_owned(),
                release_track_mbid: RECORDING.to_owned(),
            }],
        },
    );

    let mock = MockSlskd::start().await.expect("mock starts");
    // Three peers, each with the album's folder.
    let peer = |username: &str| {
        serde_json::json!({
            "username": username, "hasFreeUploadSlot": true, "uploadSpeed": 1_000_000,
            "queueLength": 0, "fileCount": 1, "lockedFileCount": 0, "lockedFiles": [],
            "token": 1,
            "files": [{"filename": REMOTE, "size": size, "extension": "flac",
                       "bitDepth": 16, "sampleRate": 44100, "length": seconds}],
        })
    };
    mock.script_responses(vec![peer("collector"), peer("second"), peer("third")]);
    let repo = SlskdRepository::new(
        SlskdClient::new(ReqwestSlskdHttp::new(
            reqwest::Client::new(),
            &mock.base_url(),
            MOCK_API_KEY,
        )),
        &mock.base_url(),
        MOCK_API_KEY,
        downloads.clone(),
        DownloadPolicy {
            search_timeout: Duration::from_secs(2),
            completion_grace: Duration::from_secs(2),
            poll_interval: Duration::from_millis(10),
            ..DownloadPolicy::default()
        },
    );
    let targets = Arc::new(Targets::new(
        acquire.journal.clone(),
        Some(Arc::new(albums)),
    ));
    let source = SlskdSource::new(Arc::new(repo), acquire.journal.clone()).with_targets(targets);
    let worker = Arc::new(
        DownloadWorker::fixed(
            acquire.journal.clone(),
            vec![crate::as_source(Arc::new(source))],
            WorkerConfig::default(),
            droppedneedle_kernel::events::EventHub::new(),
            Default::default(),
        )
        .with_landing(acquire.landing.clone()),
    );
    let settle = |task_id: String| {
        let worker = worker.clone();
        let journal = acquire.journal.clone();
        async move {
            for pass in 1..12 {
                worker.run_once(pass).await;
                worker.wait_for_landings().await;
                let task = journal.read_task(&task_id).await.unwrap().unwrap();
                if task.status.is_terminal() {
                    return task;
                }
            }
            panic!("task {task_id} never settled");
        }
    };

    let ada = test_principal("u-ada", Role::Trusted, "s-ada", SessionKind::Standard);
    let requests = RequestsService::new(&acquire.requests);
    let intake: TrackIntake = serde_json::from_value(serde_json::json!({
        "recording_mbid": RECORDING,
        "artist_name": "Portishead",
        "track_title": "Glory Box",
        "release_group_mbid": SINGLE_GROUP,
    }))
    .expect("intake");
    let asked = requests.request_track(&ada, &intake).await.expect("asked");
    let first = asked.task_id.expect("dispatched");

    // Held after one landing, with the reason, and no other source tried.
    let task = settle(first.clone()).await;
    let attempts: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM download_attempts WHERE task_id = ?1")
            .bind(&first)
            .fetch_one(acquire.db.db().sqlx())
            .await
            .expect("attempts");
    assert_eq!(attempts, 1, "no other source was tried");
    assert_eq!(task.status, TaskStatus::Failed);
    assert_eq!(
        task.error_message.as_deref(),
        Some("Held for review: Only the album version was found.")
    );
    assert_eq!(
        decisions(&acquire, &first).await,
        vec![("held".to_owned(), Some("album_version_only".to_owned()))]
    );
    assert!(files_under(&music, "flac").is_empty(), "nothing imported");
    let history = requests
        .history(
            &ada,
            &serde_json::from_value::<HistoryQuery>(serde_json::json!({"kind": "track"}))
                .expect("query"),
        )
        .await
        .expect("history");
    assert_eq!(history.items.len(), 1);
    assert_eq!(history.items[0].status, "failed");
    assert_eq!(history.items[0].album_version_only, Some(true));

    // "Get it from the album": the same track, now imported as the song.
    if !downloads.join("Dummy/04 - Glory Box.flac").is_file() {
        place();
    }
    let taken = requests
        .take_album_version(&ada, RECORDING)
        .await
        .expect("taken");
    assert!(taken.applied(), "{taken:?}");
    let request_key = format!("track:{RECORDING}");
    let second: String = sqlx::query_scalar(
        "SELECT download_task_id FROM request_history WHERE musicbrainz_id_lower = ?1",
    )
    .bind(&request_key)
    .fetch_one(acquire.db.db().sqlx())
    .await
    .expect("request row");
    assert_ne!(second, first);
    let task = settle(second).await;
    assert_eq!(
        task.status,
        TaskStatus::Completed,
        "{:?}",
        task.error_message
    );
    assert_eq!(files_under(&music, "flac").len(), 1);
    let recording: String = sqlx::query_scalar(
        "SELECT i.recording_mbid FROM local_track_external_identities i \
         JOIN local_tracks t ON t.id = i.local_track_id WHERE t.availability = 'indexed'",
    )
    .fetch_one(acquire.db.db().sqlx())
    .await
    .expect("identity");
    assert_eq!(recording, RECORDING, "imported as the requested song");
    let held: Vec<(String, String)> =
        sqlx::query_as("SELECT state, path FROM acquire_held_files WHERE source_task_id = ?1")
            .bind(&first)
            .fetch_all(acquire.db.db().sqlx())
            .await
            .expect("held rows");
    assert!(!held.is_empty(), "the first download held its file");
    let held_dir = &acquire.held.folders().held;
    assert!(
        held.iter()
            .all(|(state, path)| state == "discarded" && !held_dir.join(path).exists()),
        "the album version hold is resolved: {held:?}"
    );
    let status: String =
        sqlx::query_scalar("SELECT status FROM request_history WHERE musicbrainz_id_lower = ?1")
            .bind(&request_key)
            .fetch_one(acquire.db.db().sqlx())
            .await
            .expect("request row");
    assert_eq!(status, "imported");
}

/// A flat file a client put straight in its downloads folder: once it is
/// imported, the downloads folder itself stays, even though it is empty.
#[tokio::test]
async fn a_flat_file_landed_alone_leaves_the_downloads_folder() {
    let (acquire, music, _platform) = land_for(
        new_task("t-flat", GOOD_GROUP),
        |downloads| {
            vec![land_file(
                downloads,
                "01 - Mysterons.flac",
                "flac_full_01.flac",
                "Mysterons",
                1,
            )]
        },
        |lengths| release(GOOD_RELEASE, GOOD_GROUP, &["Mysterons"], lengths),
    )
    .await;
    assert_eq!(
        decisions(&acquire, "t-flat").await,
        vec![("imported".to_owned(), None)]
    );
    assert_eq!(files_under(&music, "flac").len(), 1, "the file is imported");
    let downloads = music.parent().expect("scratch dir").join("downloads");
    assert!(downloads.is_dir(), "the downloads folder stays");
}

/// Swaps a download's job folder for a link to a folder outside the
/// downloads folder holding a file of the same name, once, as a machine
/// that can write the client's folder could after the landing's check.
#[derive(Clone)]
struct Swap {
    job: PathBuf,
    stash: PathBuf,
    outside: PathBuf,
    done: Arc<std::sync::atomic::AtomicBool>,
}

impl Swap {
    fn run(&self) {
        if self.done.swap(true, std::sync::atomic::Ordering::SeqCst) {
            return;
        }
        std::fs::rename(&self.job, &self.stash).expect("job folder moved away");
        std::os::unix::fs::symlink(&self.outside, &self.job).expect("link swapped in");
    }
}

/// MusicBrainz that swaps the folder at its first question, which the
/// landing asks after its check and before it copies anything.
struct SwapOnMatch {
    inner: Releases,
    swap: Swap,
}

impl ReleaseSource for SwapOnMatch {
    fn search<'a>(
        &'a self,
        title: &'a str,
        artist: &'a str,
    ) -> BoxFuture<'a, Result<Vec<ReleaseHit>, SourceError>> {
        self.swap.run();
        self.inner.search(title, artist)
    }

    fn release<'a>(&'a self, mbid: &'a str) -> BoxFuture<'a, Result<Option<Release>, SourceError>> {
        self.swap.run();
        self.inner.release(mbid)
    }

    fn canonical_recording<'a>(
        &'a self,
        mbid: &'a str,
    ) -> BoxFuture<'a, Result<Option<String>, SourceError>> {
        self.inner.canonical_recording(mbid)
    }
}

/// When the job folder is swapped for a link.
#[derive(Debug, Clone, Copy, PartialEq)]
enum SwapAt {
    /// After the check, before the copy into staging or the held folder.
    Match,
    /// After the import, before its sources are removed.
    Import,
}

/// Land one file from `downloads/job` for a fresh task, with the job
/// folder swapped at `at`. With `other_artist` the file is held.
/// Answers the bundle, the music folder, the file outside the downloads
/// folder and the platform it runs on.
async fn land_swapped(
    at: SwapAt,
    other_artist: bool,
) -> (
    droppedneedle_acquire::AcquireSetup,
    PathBuf,
    PathBuf,
    droppedneedle_kernel::platform::Platform,
) {
    let db = AcquireDb::scratch().expect("scratch db");
    db.add_user("u-ada", "Ada", "user").await.expect("user");
    let dir = db.path().parent().expect("scratch dir").to_path_buf();
    let config = Arc::new(
        ConfigStore::scratch(&dir, Crypto::from_key_bytes(&[7u8; 32]).expect("key"))
            .expect("config"),
    );
    let platform = TestPlatform::on(db.db(), &dir)
        .settings(config)
        .build()
        .await;
    let (library, _) = library_at(&platform);
    let music = dir.join("music");
    std::fs::create_dir_all(&music).expect("music root");
    library
        .scans
        .add_root(
            Some("music".to_owned()),
            music.to_string_lossy().into_owned(),
            EffectivePolicy::Automatic,
        )
        .expect("root adds");
    let downloads = dir.join("downloads");
    let job = downloads.join("job");
    let name = "01 - Mysterons.flac";
    let file = land_file(&job, name, "flac_full_01.flac", "Mysterons", 1);
    let outside = dir.join("outside");
    let secret = land_file(&outside, name, "flac_full_01.flac", "Mysterons", 1);
    let mut length = length_ms(&file);
    if other_artist {
        let other = TagEdit::new(TagField::Artist, vec!["Dan Romer".to_owned()]);
        save_tags(&file, &[other]).expect("artist saves");
        length = length.map(|ms| ms + 25_000);
    }
    let swap = Swap {
        job,
        stash: dir.join("stash"),
        outside,
        done: Default::default(),
    };
    let releases = Releases(vec![release(
        GOOD_RELEASE,
        GOOD_GROUP,
        &["Mysterons"],
        &[length],
    )]);
    let source: Arc<dyn ReleaseSource> = match at {
        SwapAt::Match => Arc::new(SwapOnMatch {
            inner: releases,
            swap: swap.clone(),
        }),
        SwapAt::Import => Arc::new(releases),
    };
    let port: Arc<dyn LandingLibrary> = Arc::new(Shaky {
        inner: Arc::new(LibraryLanding::new(&library, every_priority(source))),
        changes: 0.into(),
        after_import: (at == SwapAt::Import).then_some(swap),
    });
    let acquire = acquire_over(&db, Some(port));
    acquire
        .journal
        .run("test.seed", |store| {
            store.insert_task(&new_task("t-swap", GOOD_GROUP), 1.0)
        })
        .await
        .expect("task");
    let worker = Arc::new(
        DownloadWorker::fixed(
            acquire.journal.clone(),
            vec![crate::as_source(Arc::new(FixedSource::new(
                downloads,
                vec![file],
            )))],
            WorkerConfig::default(),
            droppedneedle_kernel::events::EventHub::new(),
            Default::default(),
        )
        .with_landing(acquire.landing.clone()),
    );
    worker.run_once(1).await;
    worker.wait_for_landings().await;
    (acquire, music, secret, platform)
}

// A machine that runs the download client can swap a folder in the
// downloads folder for a link after the landing checked it. The copy into
// the library, the held copy and the removal of imported sources never
// follow it to a file outside.
#[tokio::test]
async fn a_link_swapped_in_after_the_check_is_never_followed() {
    let (_acquire, music, secret, _platform) = land_swapped(SwapAt::Match, false).await;
    assert!(
        files_under(&music, "flac").is_empty(),
        "nothing outside the downloads folder is imported"
    );
    assert!(secret.is_file());

    let (acquire, _music, secret, _platform) = land_swapped(SwapAt::Match, true).await;
    assert!(
        files_under(&acquire.held.folders().held, "flac").is_empty(),
        "nothing outside the downloads folder is held"
    );
    assert!(secret.is_file());

    let (_acquire, music, secret, _platform) = land_swapped(SwapAt::Import, false).await;
    assert_eq!(
        files_under(&music, "flac").len(),
        1,
        "imported before the swap"
    );
    assert!(
        secret.is_file(),
        "removing the imported source leaves the file outside alone"
    );
}

/// MusicBrainz for the intake table: every search returns every release,
/// and a release group lists its releases.
struct GroupListing(Vec<Release>);

impl ReleaseSource for GroupListing {
    fn search<'a>(
        &'a self,
        _title: &'a str,
        _artist: &'a str,
    ) -> BoxFuture<'a, Result<Vec<ReleaseHit>, SourceError>> {
        let hits = self
            .0
            .iter()
            .map(|release| ReleaseHit {
                id: release.id.clone(),
                release_group_id: release.release_group_id.clone(),
                score: 100,
                track_count: Some(release.tracks.len() as u32),
                title: release.title.clone(),
            })
            .collect();
        Box::pin(async move { Ok(hits) })
    }

    fn release<'a>(&'a self, mbid: &'a str) -> BoxFuture<'a, Result<Option<Release>, SourceError>> {
        let found = self
            .0
            .iter()
            .find(|release| release.answers_to(mbid))
            .cloned();
        Box::pin(async move { Ok(found) })
    }

    fn canonical_recording<'a>(
        &'a self,
        _mbid: &'a str,
    ) -> BoxFuture<'a, Result<Option<String>, SourceError>> {
        Box::pin(async { Ok(None) })
    }

    fn search_editions<'a>(
        &'a self,
        query: &'a EditionQuery,
    ) -> BoxFuture<'a, Result<EditionPage, SourceError>> {
        let items: Vec<Edition> = self
            .0
            .iter()
            .filter(|release| {
                query
                    .release_group_mbid
                    .as_deref()
                    .is_some_and(|group| group.eq_ignore_ascii_case(&release.release_group_id))
            })
            .map(|release| Edition {
                release_mbid: release.id.clone(),
                release_group_mbid: release.release_group_id.clone(),
                title: release.title.clone(),
                disc_count: release.media.len() as u32,
                track_count: release.tracks.len() as u32,
                score: 100,
                ..Edition::default()
            })
            .collect();
        let total = items.len() as u64;
        Box::pin(async move {
            Ok(EditionPage {
                items,
                total,
                offset: 0,
            })
        })
    }
}

/// AcoustID hearing one recording in every file, or nothing at all.
struct Hears(Option<String>);

impl FingerprintSource for Hears {
    fn identify<'a>(&'a self, _tracks: &'a [LocalTrackFacts]) -> BoxFuture<'a, AudioMatches> {
        Box::pin(async { AudioMatches::default() })
    }

    fn identify_files<'a>(
        &'a self,
        files: &'a [(String, PathBuf)],
    ) -> BoxFuture<'a, HashMap<String, Vec<String>>> {
        Box::pin(async move {
            match &self.0 {
                Some(recording) => files
                    .iter()
                    .map(|(id, _)| (id.clone(), vec![recording.clone()]))
                    .collect(),
                None => HashMap::new(),
            }
        })
    }
}

/// The real library port, whose first `changes` imports find the library
/// changed under them (a destination taken between the plan and the
/// publish), and which runs `after_import` once an import is done.
struct Shaky {
    inner: Arc<dyn LandingLibrary>,
    changes: std::sync::atomic::AtomicUsize,
    after_import: Option<Swap>,
}

impl LandingLibrary for Shaky {
    fn search<'a>(
        &'a self,
        title: &'a str,
        artist: &'a str,
        priority: RequestPriority,
    ) -> BoxFuture<'a, Result<Vec<ReleaseHit>, SourceError>> {
        self.inner.search(title, artist, priority)
    }

    fn release<'a>(
        &'a self,
        mbid: &'a str,
        priority: RequestPriority,
    ) -> BoxFuture<'a, Result<Option<Release>, SourceError>> {
        self.inner.release(mbid, priority)
    }

    fn canonical_recording<'a>(
        &'a self,
        mbid: &'a str,
        priority: RequestPriority,
    ) -> BoxFuture<'a, Result<Option<String>, SourceError>> {
        self.inner.canonical_recording(mbid, priority)
    }

    fn chosen_edition<'a>(&'a self, group: &'a str) -> BoxFuture<'a, Option<String>> {
        self.inner.chosen_edition(group)
    }

    fn owned<'a>(&'a self, group: &'a str, album: Option<&'a str>) -> BoxFuture<'a, OwnedTracks> {
        self.inner.owned(group, album)
    }

    fn held_tier<'a>(
        &'a self,
        group: &'a str,
        album: Option<&'a str>,
    ) -> BoxFuture<'a, Option<String>> {
        self.inner.held_tier(group, album)
    }

    fn owned_copies<'a>(
        &'a self,
        group: &'a str,
        album: Option<&'a str>,
        release: &'a str,
        release_track: &'a str,
        recording: &'a str,
    ) -> BoxFuture<'a, Vec<OwnedCopy>> {
        self.inner
            .owned_copies(group, album, release, release_track, recording)
    }

    fn version_row<'a>(
        &'a self,
        track_id: &'a str,
        planned: &'a PlannedAlbum,
        release: &'a Release,
    ) -> BoxFuture<'a, Option<VersionRow>> {
        self.inner.version_row(track_id, planned, release)
    }

    fn fingerprints(
        &self,
        files: Vec<(String, PathBuf)>,
    ) -> BoxFuture<'_, HashMap<String, Vec<String>>> {
        self.inner.fingerprints(files)
    }

    fn group_releases<'a>(
        &'a self,
        group: &'a str,
        priority: RequestPriority,
    ) -> BoxFuture<'a, Result<Vec<ReleaseHit>, SourceError>> {
        self.inner.group_releases(group, priority)
    }

    fn fingerprint_releases(
        &self,
        files: Vec<(String, PathBuf)>,
    ) -> BoxFuture<'_, HashMap<String, Vec<String>>> {
        self.inner.fingerprint_releases(files)
    }

    fn library_dirs(&self) -> Vec<PathBuf> {
        self.inner.library_dirs()
    }

    fn placed_sources<'a>(
        &'a self,
        staging: &'a str,
    ) -> BoxFuture<'a, Result<Option<std::collections::HashSet<String>>, String>> {
        self.inner.placed_sources(staging)
    }

    fn import(
        &self,
        request: ImportRequest,
    ) -> BoxFuture<'_, Result<ImportReceipt, ImportFailure>> {
        use std::sync::atomic::Ordering;
        let changed = self
            .changes
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |left| {
                left.checked_sub(1)
            })
            .is_ok();
        if changed {
            return Box::pin(async {
                Err(ImportFailure::Occupied(
                    "a file appeared where this one goes".to_owned(),
                ))
            });
        }
        let Some(swap) = self.after_import.clone() else {
            return self.inner.import(request);
        };
        Box::pin(async move {
            let receipt = self.inner.import(request).await;
            swap.run();
            receipt
        })
    }

    fn publish_settled<'a>(&'a self, task_id: &'a str) -> BoxFuture<'a, Result<bool, String>> {
        self.inner.publish_settled(task_id)
    }
}

/// What the library holds of the album before a row's file comes in.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Have {
    Nothing,
    /// A copy on NEW holding "Mysterons" from this fixture, and "Sour
    /// Times".
    Copy(&'static str),
    /// A copy on NEW holding only "Sour Times"; the download was fetched
    /// for it ("Get missing tracks" for "Mysterons").
    Planned,
    /// A copy holding "Mysterons" in another version, on no track of NEW;
    /// the download replaces it.
    Version,
}

/// One row: the plan (what the library has, the file, its origin, what
/// AcoustID hears, the destination, the library changing) and the gate.
#[derive(Debug, Clone, Copy)]
struct Row {
    have: Have,
    /// The fixture the incoming "Mysterons" is made from.
    file: &'static str,
    upgrade: bool,
    /// `Some(true)`: AcoustID hears the track's recording; `Some(false)`:
    /// another one; `None`: no answer.
    heard: Option<bool>,
    /// The release's track lengths agree with the file's, so a length
    /// vouches for it whatever AcoustID hears.
    length_agrees: bool,
    /// A file the catalog does not know already sits where it goes.
    taken: bool,
    /// Imports that find the library changed under them.
    changes: usize,
    gate: Gate,
    /// The hold a person imports it from, under `PersonChoseFile`.
    held_for: Reason,
    expect: Seen,
}

/// What became of the incoming file, as every door shows it.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Seen {
    /// In the library; `joined` when it went into the existing copy.
    Imported {
        joined: bool,
    },
    AlreadyOwned,
    Held(Reason),
    Deferred(Reason),
}

/// The ways a batch reaches `place`.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Door {
    Place,
    Landing,
    DropMatch,
    HeldImport,
}

/// The scenario every row runs in.
const INTAKE: u16 = 40;

/// One row's world: the rig, the scenario's editions, the copy's album and
/// the track a download may replace.
struct IntakeCase {
    rig: JoinRig,
    e: Editions,
    copy: Option<String>,
    old: Option<String>,
}

impl IntakeCase {
    async fn new(row: &Row) -> Self {
        let s = INTAKE;
        let mut lengths = fixture_lengths();
        if row.heard == Some(false) && !row.length_agrees {
            // A length that does not vouch for the track either.
            lengths = lengths.iter().map(|ms| ms.map(|ms| ms + 5_000)).collect();
        }
        let e = editions(s, &lengths);
        let (db, dir, auth) = bare_db().await;
        let config = Arc::new(
            ConfigStore::scratch(&dir, Crypto::from_key_bytes(&[7u8; 32]).expect("key"))
                .expect("config"),
        );
        let platform = TestPlatform::on(db.db(), &dir)
            .settings(config)
            .build()
            .await;
        let (mut library, _) = library_at(&platform);
        let recording = e.new.tracks[0].recording_id.clone();
        library.imports.fingerprints = Arc::new(Hears(
            row.heard
                .map(|right| if right { recording } else { mb(s, 3, 9) }),
        ));
        let music = dir.join("music");
        std::fs::create_dir_all(&music).expect("music root");
        library
            .scans
            .add_root(
                Some("music".to_owned()),
                music.to_string_lossy().into_owned(),
                EffectivePolicy::Automatic,
            )
            .expect("root adds");
        let releases = if row.have == Have::Version {
            vec![e.old.clone(), e.new.clone()]
        } else {
            vec![e.new.clone()]
        };
        let port: Arc<dyn LandingLibrary> = Arc::new(Shaky {
            inner: Arc::new(LibraryLanding::new(
                &library,
                every_priority(Arc::new(GroupListing(releases.clone()))),
            )),
            changes: row.changes.into(),
            after_import: None,
        });
        let acquire = acquire_over(&db, Some(port));
        let targets = Arc::new(droppedneedle_acquire::target::Targets::new(
            acquire.journal.clone(),
            Some(Arc::new(Lookup(releases))),
        ));
        let rig = JoinRig {
            acquire,
            library,
            targets,
            music,
            downloads: dir.join("downloads"),
            _auth: auth,
        };
        let sour = copy_rel(s, SOUR_TIMES);
        let (copy, old) = match row.have {
            Have::Nothing => (None, None),
            Have::Copy(fixture) => {
                let mysterons = copy_rel(s, &format!("0101 Mysterons.{}", ext(fixture)));
                rig.plant_from(&mysterons, fixture, 0, &e.new);
                rig.plant(&sour, 1, &e.new);
                rig.scan().await;
                let old = rig.track_at(&mysterons);
                let album = rig.album_of(&old);
                rig.set_edition(&album, &e.group, &e.new.id, "manual");
                rig.place(&old, &e.new, 0, "manual");
                rig.place(&rig.track_at(&sour), &e.new, 1, "manual");
                (Some(album), Some(old))
            }
            Have::Planned => {
                rig.plant(&sour, 1, &e.new);
                rig.scan().await;
                let kept = rig.track_at(&sour);
                let album = rig.album_of(&kept);
                rig.set_edition(&album, &e.group, &e.new.id, "manual");
                rig.place(&kept, &e.new, 1, "manual");
                (Some(album), None)
            }
            Have::Version => {
                let o = copy_rel(s, "01 Mysterons.flac");
                rig.plant(&sour, 1, &e.old);
                rig.plant(&o, 0, &e.old);
                rig.scan().await;
                let old = rig.track_at(&o);
                let album = rig.album_of(&old);
                rig.set_edition(&album, &e.group, &e.new.id, "manual");
                rig.place(&rig.track_at(&sour), &e.new, 1, "manual");
                rig.listen(&old);
                (Some(album), Some(old))
            }
        };
        if row.taken {
            let taken = rig
                .music
                .join(format!("Portishead/Dummy {s}/0101 Mysterons.flac"));
            std::fs::create_dir_all(taken.parent().expect("folder")).expect("folder");
            std::fs::copy(fixture("flac_full_02.flac"), &taken).expect("stray file");
        }
        Self { rig, e, copy, old }
    }

    /// The copy the download was fetched for, with the version it
    /// replaces.
    fn join(&self, row: &Row) -> Option<PlannedAlbum> {
        let copy = self.copy.clone()?;
        if !matches!(row.have, Have::Planned | Have::Version) {
            return None;
        }
        Some(PlannedAlbum {
            album_id: copy,
            release_mbid: self.e.new.id.clone(),
            replace: self
                .old
                .iter()
                .filter(|_| row.have == Have::Version)
                .map(|old| ReplaceVersion {
                    release_track_mbid: self.e.new.tracks[0].id.clone(),
                    local_track_id: old.clone(),
                })
                .collect(),
        })
    }

    /// The task the file was downloaded for, with its plan.
    async fn ask(&self, row: &Row) {
        let origin = if row.upgrade { "upgrade" } else { "user" };
        let e = &self.e;
        match self.join(row) {
            Some(join) => {
                let replace: Vec<(&str, &str)> = join
                    .replace
                    .iter()
                    .map(|version| {
                        (
                            version.release_track_mbid.as_str(),
                            version.local_track_id.as_str(),
                        )
                    })
                    .collect();
                self.rig
                    .ask_replacing(
                        "t-intake",
                        &e.group,
                        &e.new.title,
                        origin,
                        Some(&e.new.id),
                        Some((&join.album_id, &e.new.id)),
                        &[(1, 1)],
                        &replace,
                    )
                    .await;
            }
            None => {
                self.rig
                    .ask("t-intake", &e.group, &e.new.title, origin, None, None, &[])
                    .await;
            }
        }
    }

    /// Indexed "Mysterons" tracks: id and album.
    async fn mysterons(&self) -> Vec<(String, String)> {
        sqlx::query_as(
            "SELECT id, local_album_id FROM local_tracks WHERE availability = 'indexed' \
             AND title = 'Mysterons'",
        )
        .fetch_all(self.rig.acquire.db.db().sqlx())
        .await
        .expect("tracks")
    }

    /// Run `row` through `door` and say what became of the file.
    async fn run(&self, row: &Row, door: Door) -> Seen {
        let before = self.mysterons().await;
        let file = self.rig.landed("t-intake", &self.e.new, 0, row.file);
        let said = match door {
            Door::Place => self.through_place(row, file).await,
            Door::Landing => self.through_landing(row, file).await,
            Door::DropMatch => self.through_drop(file).await,
            Door::HeldImport => self.through_held(row, file).await,
        };
        if let Some(seen) = said {
            return seen;
        }
        let after = self.mysterons().await;
        match after.iter().find(|track| !before.contains(track)) {
            Some((_, album)) => Seen::Imported {
                joined: self.copy.as_deref() == Some(album.as_str()),
            },
            // A file that replaced one at its own path keeps that row; the
            // copy it replaced is in the recycle bin.
            None if files_under(&self.rig.music.join(".recycle"), "flac")
                .into_iter()
                .chain(files_under(&self.rig.music.join(".recycle"), "mp3"))
                .any(|path| path.to_string_lossy().contains("Mysterons")) =>
            {
                Seen::Imported {
                    joined: self.copy.is_some(),
                }
            }
            None => Seen::AlreadyOwned,
        }
    }

    async fn through_place(&self, row: &Row, file: PathBuf) -> Option<Seen> {
        let mut probed = droppedneedle_acquire::landing::probe::probe(&[file], &[]);
        let file = probed.audio.remove(0);
        let mut pairing = intake::Pairing::default();
        if matches!(row.have, Have::Copy(_)) {
            pairing.owned.push((0, 0));
        } else {
            pairing.import.push((0, 0));
        }
        let join = self.join(row);
        let batch = intake::IntakeBatch {
            files: vec![intake::IntakeFile {
                file,
                held_for: (row.gate == Gate::PersonChoseFile).then_some(row.held_for),
            }],
            target: intake::Target {
                release: self.e.new.clone(),
                asked: if join.is_some() {
                    vec![(1, 1)]
                } else {
                    Vec::new()
                },
                join,
            },
            origin: if row.gate == Gate::PersonChoseAlbum {
                intake::Origin::Upload { drop_item_id: 1 }
            } else {
                intake::Origin::Download {
                    task_id: "t-intake".to_owned(),
                    upgrade: row.upgrade,
                    staging: None,
                    downloads_folders: None,
                }
            },
            gate: row.gate,
            owner: "u-ada".to_owned(),
        };
        let library = self.rig.acquire.landing.library().expect("library").clone();
        match intake::place(library.as_ref(), &batch, pairing, None).await {
            Ok(placement) => match &placement.outcomes[0] {
                intake::Outcome::Held { reason, .. } => Some(Seen::Held(*reason)),
                intake::Outcome::AlreadyOwned { .. } => Some(Seen::AlreadyOwned),
                _ => None,
            },
            Err(deferred) => Some(Seen::Deferred(deferred.reason)),
        }
    }

    async fn through_landing(&self, row: &Row, file: PathBuf) -> Option<Seen> {
        self.ask(row).await;
        self.rig.land(vec![file]).await;
        let last = decisions(&self.rig.acquire, "t-intake").await.pop();
        if let Some((outcome, Some(code))) = &last
            && outcome == "deferred"
        {
            return Some(Seen::Deferred(Reason::from_stored(code)));
        }
        let held: Vec<String> = sqlx::query_scalar(
            "SELECT reason FROM acquire_held_files WHERE source_task_id = 't-intake' \
             AND state = 'held'",
        )
        .fetch_all(self.rig.acquire.db.db().sqlx())
        .await
        .expect("held rows");
        held.first()
            .map(|code| Seen::Held(Reason::from_stored(code)))
    }

    async fn through_drop(&self, file: PathBuf) -> Option<Seen> {
        let staging = self
            .rig
            .acquire
            .drops
            .incoming_dir()
            .parent()
            .expect("drop root")
            .join("job-intake");
        std::fs::create_dir_all(&staging).expect("staging");
        let name = file
            .file_name()
            .expect("name")
            .to_string_lossy()
            .into_owned();
        let staged = staging.join(&name);
        std::fs::copy(&file, &staged).expect("staged upload");
        let dir = staging.to_string_lossy().into_owned();
        let entry: i64 = self
            .rig
            .acquire
            .db
            .write("test.drop", move |tx| {
                tx.execute(
                    "INSERT INTO drop_import_jobs (id, user_id, user_name, status, created_at, \
                     upload_name, staging_dir) VALUES ('job-intake', 'u-ada', 'Ada', \
                     'completed', 1.0, 'up', ?1)",
                    [&dir],
                )?;
                tx.execute(
                    "INSERT INTO drop_import_items (job_id, folder_name, status, files_total, \
                     updated_at, reason_code) VALUES ('job-intake', 'Odds', 'needs_review', 1, \
                     1.0, 'no_album_match')",
                    [],
                )?;
                let item = tx.last_insert_rowid();
                tx.execute(
                    "INSERT INTO acquire_held_entries (user_id, origin, group_key, drop_item_id, \
                     state, created_at, expires_at) VALUES ('u-ada', 'upload', ?1, ?2, 'held', \
                     1, 2)",
                    rusqlite::params![format!("upload:{item}"), item],
                )?;
                let entry = tx.last_insert_rowid();
                tx.execute(
                    "INSERT INTO acquire_held_files (entry_id, root, path, original_name, \
                     source, reason, state, created_at) VALUES (?1, 'drop', ?2, ?3, 'upload', \
                     'no_album_match', 'held', 1)",
                    rusqlite::params![entry, format!("job-intake/{name}"), name],
                )?;
                Ok(entry)
            })
            .await
            .expect("drop item");
        let viewer = HeldViewer {
            user_id: "u-ada".to_owned(),
            admin: false,
        };
        let matched = self
            .rig
            .acquire
            .held
            .match_album(&viewer, entry, &self.e.group, None, "u-ada")
            .await;
        let (state, reason): (String, String) =
            sqlx::query_as("SELECT state, reason FROM acquire_held_files WHERE entry_id = ?1")
                .bind(entry)
                .fetch_one(self.rig.acquire.db.db().sqlx())
                .await
                .expect("held file");
        // A Match the library put off says why and leaves the file as it
        // was, so no later retry places it on the album it was paired to.
        if let Err(error) = matched {
            let message = error.to_string();
            let why = Reason::ALL
                .iter()
                .copied()
                .find(|reason| message.starts_with(reason.message()))
                .unwrap_or_else(|| panic!("matched by hand: {message}"));
            assert_eq!(
                (state.as_str(), reason.as_str()),
                ("held", "no_album_match"),
                "a deferred Match leaves the file as it was"
            );
            assert!(staged.is_file(), "a waiting upload is kept");
            return Some(Seen::Deferred(why));
        }
        match state.as_str() {
            "held" => {
                assert!(staged.is_file(), "a waiting upload is kept");
                let reason = Reason::from_stored(&reason);
                Some(if reason == Reason::LibraryChanged {
                    Seen::Deferred(reason)
                } else {
                    Seen::Held(reason)
                })
            }
            "already_owned" => Some(Seen::AlreadyOwned),
            _ => None,
        }
    }

    async fn through_held(&self, row: &Row, file: PathBuf) -> Option<Seen> {
        self.ask(row).await;
        let held_dir = self.rig.acquire.held.folders().held.clone();
        std::fs::create_dir_all(&held_dir).expect("held dir");
        let name = file
            .file_name()
            .expect("name")
            .to_string_lossy()
            .into_owned();
        let copy = held_dir.join(&name);
        std::fs::copy(&file, &copy).expect("held copy");
        let track = &self.e.new.tracks[0];
        let (group, release, release_track, recording) = (
            self.e.group.clone(),
            self.e.new.id.clone(),
            track.id.clone(),
            track.recording_id.clone(),
        );
        let reason = row.held_for.code();
        let path = name.clone();
        let entry: i64 = self
            .rig
            .acquire
            .db
            .write("test.held", move |tx| {
                tx.execute(
                    "INSERT INTO acquire_held_entries (user_id, origin, group_key, \
                     release_group_mbid, state, created_at, expires_at) VALUES ('u-ada', \
                     'download', ?1, ?2, 'held', 1, 2)",
                    rusqlite::params![format!("album:{group}"), group],
                )?;
                let entry = tx.last_insert_rowid();
                tx.execute(
                    "INSERT INTO acquire_held_files (entry_id, root, path, original_name, \
                     release_mbid, release_track_mbid, recording_mbid, track_number, \
                     disc_number, reason, source, source_task_id, state, created_at) \
                     VALUES (?1, 'held', ?2, ?2, ?3, ?4, ?5, 1, 1, ?6, 'soulseek', 't-intake', \
                     'held', 1)",
                    rusqlite::params![entry, path, release, release_track, recording, reason],
                )?;
                Ok(entry)
            })
            .await
            .expect("held row");
        let viewer = HeldViewer {
            user_id: "u-ada".to_owned(),
            admin: false,
        };
        if self
            .rig
            .acquire
            .held
            .import(&viewer, entry, None, "u-ada")
            .await
            .is_ok()
        {
            assert!(!copy.exists(), "an imported held copy is removed");
            return None;
        }
        assert!(copy.is_file(), "a file that stays held is kept");
        let still: Vec<String> = sqlx::query_scalar(
            "SELECT reason FROM acquire_held_files WHERE path = ?1 AND state = 'held'",
        )
        .bind(&name)
        .fetch_all(self.rig.acquire.db.db().sqlx())
        .await
        .expect("held rows");
        let reason = Reason::from_stored(still.first().expect("still held"));
        Some(if reason == Reason::LibraryChanged {
            Seen::Deferred(reason)
        } else {
            Seen::Held(reason)
        })
    }
}

fn ext(name: &str) -> String {
    Path::new(name)
        .extension()
        .map(|ext| ext.to_string_lossy().into_owned())
        .unwrap_or_default()
}

// One intake: the same plan and gate give the same outcome whichever door
// built the batch. Each row runs through `place` itself and through every
// door that produces its gate: a landing for the app's own choice, Match
// on an upload for a person's album, Import on a held file for a person's
// file. A file a person imported is never dropped: one no better than
// theirs stays held and is kept, and importing it again replaces theirs.
#[tokio::test]
async fn one_plan_and_gate_give_one_outcome_through_every_door() {
    use Gate::{Automatic as Auto, PersonChoseAlbum as Album, PersonChoseFile as File};
    const FLAC: &str = "flac_full_01.flac";
    const MP3: &str = "mp3_full_01.mp3";
    let base = Row {
        have: Have::Nothing,
        file: FLAC,
        upgrade: false,
        heard: None,
        length_agrees: false,
        taken: false,
        changes: 0,
        gate: Auto,
        held_for: Reason::TagMismatch,
        expect: Seen::Imported { joined: false },
    };
    let owned = |fixture, heard| Row {
        have: Have::Copy(fixture),
        upgrade: true,
        heard,
        ..base
    };
    let joined = Seen::Imported { joined: true };
    let mut rows: Vec<(&str, Row)> = Vec::new();
    for gate in [Auto, Album, File] {
        rows.push(("plain import", Row { gate, ..base }));
        rows.push((
            "destination taken",
            Row {
                gate,
                taken: true,
                expect: Seen::Held(Reason::TargetOccupied),
                ..base
            },
        ));
        rows.push((
            "library changed once",
            Row {
                gate,
                changes: 1,
                ..base
            },
        ));
        rows.push((
            "library changed twice",
            Row {
                gate,
                changes: 2,
                expect: Seen::Deferred(Reason::LibraryChanged),
                ..base
            },
        ));
        rows.push((
            "owned better, AcoustID confirms",
            Row {
                gate,
                expect: joined,
                ..owned(MP3, Some(true))
            },
        ));
    }
    let by_gate = |name, row: Row, expect: [Seen; 3]| {
        [Auto, Album, File]
            .into_iter()
            .zip(expect)
            .map(move |(gate, expect)| {
                (
                    name,
                    Row {
                        gate,
                        expect,
                        ..row
                    },
                )
            })
    };
    rows.extend(by_gate(
        "owned equal",
        owned(FLAC, Some(true)),
        [
            Seen::AlreadyOwned,
            Seen::AlreadyOwned,
            Seen::Held(Reason::NoBetterThanYours),
        ],
    ));
    rows.extend(by_gate(
        "AcoustID gives no answer",
        Row {
            held_for: Reason::FingerprintMismatch,
            ..owned(MP3, None)
        },
        [joined, Seen::Held(Reason::FingerprintUnconfirmed), joined],
    ));
    rows.extend(by_gate(
        "AcoustID hears another recording",
        Row {
            held_for: Reason::FingerprintMismatch,
            ..owned(MP3, Some(false))
        },
        [
            Seen::Held(Reason::FingerprintMismatch),
            Seen::Held(Reason::FingerprintMismatch),
            joined,
        ],
    ));
    rows.extend(by_gate(
        "AcoustID hears another recording, the length vouches",
        Row {
            held_for: Reason::FingerprintMismatch,
            length_agrees: true,
            ..owned(MP3, Some(false))
        },
        [joined, Seen::AlreadyOwned, joined],
    ));
    rows.push((
        "use this file",
        Row {
            gate: File,
            held_for: Reason::NoBetterThanYours,
            expect: joined,
            ..owned(FLAC, Some(true))
        },
    ));
    for gate in [Auto, File] {
        rows.push((
            "planned copy joins",
            Row {
                gate,
                have: Have::Planned,
                expect: joined,
                ..base
            },
        ));
        rows.push((
            "version replaced",
            Row {
                gate,
                have: Have::Version,
                expect: joined,
                ..base
            },
        ));
    }
    rows.push((
        "version worse",
        Row {
            have: Have::Version,
            file: MP3,
            expect: Seen::Held(Reason::VersionLowerQuality),
            ..base
        },
    ));
    rows.push((
        "version worse, used anyway",
        Row {
            have: Have::Version,
            file: MP3,
            gate: File,
            held_for: Reason::VersionLowerQuality,
            expect: joined,
            ..base
        },
    ));

    for (name, row) in rows {
        let doors: &[Door] = match row.gate {
            Auto => &[Door::Place, Door::Landing],
            Album => &[Door::Place, Door::DropMatch],
            File => &[Door::Place, Door::HeldImport],
            // The held list's own retry, tested with its loop.
            Gate::HeldRetry => &[],
        };
        for door in doors {
            let case = IntakeCase::new(&row).await;
            let seen = case.run(&row, *door).await;
            assert_eq!(seen, row.expect, "{name}, {:?}, through {door:?}", row.gate);
            if row.have == Have::Version
                && matches!(row.expect, Seen::Imported { .. })
                && let Some(old) = &case.old
            {
                assert!(
                    case.rig.in_recycle_bin("01 Mysterons.flac"),
                    "{name}, through {door:?}: your version is in the recycle bin"
                );
                let (played, _, starred) = case.rig.history(old).await;
                assert_ne!(&played, old, "{name}, through {door:?}: the play moved");
                assert!(!starred, "{name}, through {door:?}: the star moved");
            }
        }
    }
}

// Held files from two downloads imported in one go are placed one batch
// per download, each with its own plan: the file fetched for a copy joins
// it, and the other download's file goes into its own album.
#[tokio::test]
async fn held_files_of_two_downloads_keep_their_own_plans() {
    let lengths = fixture_lengths();
    let (e, f) = (editions(41, &lengths), editions(42, &lengths));
    let rig = join_rig(vec![e.new.clone(), f.new.clone()]).await;
    let sour = copy_rel(41, SOUR_TIMES);
    rig.plant(&sour, 1, &e.new);
    rig.scan().await;
    let kept = rig.track_at(&sour);
    let copy = rig.album_of(&kept);
    rig.set_edition(&copy, &e.group, &e.new.id, "manual");
    rig.place(&kept, &e.new, 1, "manual");
    rig.ask(
        "t-copy",
        &e.group,
        &e.new.title,
        "user",
        Some(&e.new.id),
        Some((&copy, &e.new.id)),
        &[(1, 1)],
    )
    .await;
    rig.ask("t-other", &f.group, &f.new.title, "user", None, None, &[])
        .await;
    let held_dir = rig.acquire.held.folders().held.clone();
    std::fs::create_dir_all(&held_dir).expect("held dir");
    let mut entries = Vec::new();
    for (task, editions) in [("t-copy", &e), ("t-other", &f)] {
        let file = rig.landed(task, &editions.new, 0, FIXTURES[0]);
        let name = format!("{task}.flac");
        std::fs::copy(&file, held_dir.join(&name)).expect("held copy");
        let track = &editions.new.tracks[0];
        let values = (
            editions.group.clone(),
            editions.new.id.clone(),
            track.id.clone(),
            track.recording_id.clone(),
            name,
            task.to_owned(),
        );
        let entry: i64 = rig
            .acquire
            .db
            .write("test.held", move |tx| {
                tx.execute(
                    "INSERT INTO acquire_held_entries (user_id, origin, group_key, \
                     release_group_mbid, state, created_at, expires_at) VALUES ('u-ada', \
                     'download', ?1, ?2, 'held', 1, 2)",
                    rusqlite::params![format!("album:{}", values.0), values.0],
                )?;
                let entry = tx.last_insert_rowid();
                tx.execute(
                    "INSERT INTO acquire_held_files (entry_id, root, path, original_name, \
                     release_mbid, release_track_mbid, recording_mbid, track_number, \
                     disc_number, reason, source, source_task_id, state, created_at) \
                     VALUES (?1, 'held', ?2, ?2, ?3, ?4, ?5, 1, 1, 'tag_mismatch', 'soulseek', \
                     ?6, 'held', 1)",
                    rusqlite::params![entry, values.4, values.1, values.2, values.3, values.5],
                )?;
                Ok(entry)
            })
            .await
            .expect("held row");
        entries.push(entry);
    }
    let viewer = HeldViewer {
        user_id: "u-ada".to_owned(),
        admin: false,
    };
    for entry in entries {
        let acted = rig
            .acquire
            .held
            .import(&viewer, entry, None, "u-ada")
            .await
            .expect("imports");
        assert_eq!(acted.imported, 1);
    }
    let albums: Vec<(String, String)> = sqlx::query_as(
        "SELECT i.release_group_mbid, t.local_album_id FROM local_tracks t \
         JOIN local_track_external_identities x ON x.local_track_id = t.id \
         JOIN local_album_external_identities i ON i.local_album_id = t.local_album_id \
         WHERE t.availability = 'indexed' AND t.title = 'Mysterons' ORDER BY i.release_group_mbid",
    )
    .fetch_all(rig.acquire.db.db().sqlx())
    .await
    .expect("tracks");
    assert_eq!(albums.len(), 2, "{albums:?}");
    assert_eq!(
        albums[0],
        (e.group.clone(), copy.clone()),
        "joined the copy"
    );
    assert_eq!(albums[1].0, f.group, "filed with its own album");
    assert_ne!(albums[1].1, copy);
}
