//! Download import journey: a finished download is verified, matched to
//! the requested release, published into the library, and its request
//! resolved; a download whose files are not the release is held for
//! review, fails over to the next candidate, and settles held once the
//! candidates run out. Real worker, landing, publisher and catalog over
//! one scratch database; MusicBrainz is a scripted release source.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use droppedneedle::acquire::downloads::state::TaskStatus;
use droppedneedle::acquire::downloads::store::NewTask;
use droppedneedle::acquire::landing::library::LibraryLanding;
use droppedneedle::acquire::wiring::settled_hook;
use droppedneedle::acquire::worker::{DownloadWorker, FixedSource, Source, WorkerConfig};
use droppedneedle::ids::{IdGenerator, UuidGenerator};
use droppedneedle::library::identify::sources::{ReleaseHit, ReleaseSource, SourceError};
use droppedneedle::library::matching::{CreditedArtist, Release, ReleaseMedium, ReleaseTrack};
use droppedneedle::library::scan::EffectivePolicy;
use droppedneedle::library::tags::TagField;
use droppedneedle::library::tags::save::{TagEdit, save_tags};
use droppedneedle::library::wiring::LibrarySetup;
use droppedneedle::runtime_config::{ConfigStore, Crypto};
use futures_util::future::BoxFuture;

const GOOD_GROUP: &str = "6d0b7a1e-0000-4000-8000-000000000001";
const GOOD_RELEASE: &str = "6d0b7a1e-0000-4000-8000-000000000002";
const BAD_GROUP: &str = "6d0b7a1e-0000-4000-8000-000000000003";
const BAD_RELEASE: &str = "6d0b7a1e-0000-4000-8000-000000000004";

/// MusicBrainz as a fixed set of releases: every search returns them all.
struct Releases(Vec<Release>);

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
fn land_file(dir: &Path, name: &str, fixture_name: &str, title: &str, track: u32) -> PathBuf {
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

fn length_ms(path: &Path) -> Option<u64> {
    let format = droppedneedle::library::tags::format_for_path(path).expect("format");
    let (_, header) =
        droppedneedle::library::tags::read::read_scan_metadata(path, format).expect("header");
    header
        .duration_seconds
        .map(|seconds| (seconds * 1000.0) as u64)
}

fn release(id: &str, group: &str, titles: &[&str], lengths: &[Option<u64>]) -> Release {
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
    let auth = droppedneedle::auth::wiring::AuthSetup::for_tests().expect("test auth");
    let ids: Arc<dyn IdGenerator> = Arc::new(UuidGenerator);
    let mut reads = droppedneedle::reads::ReadsSetup::for_tests(auth.users.clone(), ids.clone())
        .expect("reads bundle");
    let acquire = droppedneedle::acquire::AcquireSetup::for_tests(
        auth.users.clone(),
        ids.clone(),
        &mut reads.collections,
    )
    .expect("acquire bundle");
    acquire
        .db
        .add_user("u-ada", "Ada", "user")
        .await
        .expect("user");
    let dir = acquire
        .db
        .path()
        .parent()
        .expect("scratch dir")
        .to_path_buf();
    let config = Arc::new(
        ConfigStore::open(
            &dir.join("config.json"),
            Crypto::from_key_bytes(&[7u8; 32]).expect("key"),
        )
        .expect("config"),
    );
    let library = LibrarySetup::for_tests_at(auth.users.clone(), ids, acquire.db.path(), config)
        .expect("library bundle");
    let music = dir.join("music");
    std::fs::create_dir_all(&music).expect("music root");
    library
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
    let pool = acquire.db.pool().clone();
    let acquire = acquire.with_library(Arc::new(LibraryLanding::new(
        library.clone(),
        releases,
        pool,
    )));
    let worker = |paths: Vec<PathBuf>| {
        Arc::new(
            DownloadWorker::fixed(
                acquire.journal.clone(),
                vec![Source::Fixed(Arc::new(FixedSource::new(paths)))],
                WorkerConfig {
                    staging_root: acquire.staging_root.clone(),
                    ..WorkerConfig::default()
                },
            )
            .with_landing(acquire.landing.clone())
            .with_settled(settled_hook(acquire.flows.clone())),
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
                 VALUES (?1, ?1, 'Portishead', 'Dummy', '1700000000', 'downloading', 'u-ada', \
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
    .fetch_one(acquire.db.pool())
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
            .fetch_one(acquire.db.pool())
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
        "SELECT held_path FROM held_imports WHERE source_task_id = 't-bad' AND status = 'held'",
    )
    .fetch_all(acquire.db.pool())
    .await
    .expect("held rows");
    assert_eq!(held.len(), 2);
    assert!(held.iter().all(|path| Path::new(path).is_file()));
    let outcomes: Vec<String> =
        sqlx::query_scalar("SELECT outcome FROM download_import_decisions WHERE task_id = 't-bad'")
            .fetch_all(acquire.db.pool())
            .await
            .expect("decisions");
    assert!(outcomes.len() > 1, "failed over: {outcomes:?}");
    assert!(outcomes.iter().all(|outcome| outcome == "held"));
    // Every hold reads as a sentence with an action, never a bare code.
    let unexplained: i64 = sqlx::query_scalar(
        "SELECT (SELECT COUNT(*) FROM held_imports WHERE source_task_id = 't-bad' \
           AND (reason_text IS NULL OR reason_action IS NULL)) \
         + (SELECT COUNT(*) FROM download_import_decisions WHERE task_id = 't-bad' \
           AND (reason_text IS NULL OR reason_action IS NULL))",
    )
    .fetch_one(acquire.db.pool())
    .await
    .expect("reason text");
    assert_eq!(unexplained, 0);
    assert!(!acquire.flows.library.contains(BAD_GROUP).await);
}
