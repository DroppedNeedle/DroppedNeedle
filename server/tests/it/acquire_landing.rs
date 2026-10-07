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

/// POST or GET one held-import route as `identity` (`user:role:name`).
async fn held_call(
    worker: &Arc<DownloadWorker>,
    method: &str,
    uri: &str,
    identity: &str,
) -> (axum::http::StatusCode, serde_json::Value) {
    use tower::ServiceExt as _;
    let request = axum::http::Request::builder()
        .method(method)
        .uri(uri)
        .header("x-slice-principal", identity)
        .body(axum::body::Body::empty())
        .expect("request");
    let response = droppedneedle::acquire::downloads::downloads_router(worker.clone())
        .oneshot(request)
        .await
        .expect("response");
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), 1 << 20)
        .await
        .expect("body");
    (status, serde_json::from_slice(&bytes).unwrap_or_default())
}

fn files_under(dir: &Path, extension: &str) -> Vec<PathBuf> {
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
    let release = release(
        GOOD_RELEASE,
        GOOD_GROUP,
        &["Mysterons", "Sour Times"],
        &[None, None],
    );
    let first = release.tracks[0].clone();
    let releases: Arc<dyn ReleaseSource> = Arc::new(Releases(vec![release]));
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
    let seeded: Vec<i64> = acquire
        .db
        .write("test.held", move |tx| {
            let mut ids = Vec::new();
            for (user, path, reason, origin) in [
                ("u-ada", &flac, "fingerprint_mismatch", "upgrade"),
                ("u-bob", &flac, "fingerprint_mismatch", "upgrade"),
                ("u-ada", &spare, "tag_mismatch", "user"),
                ("u-ada", &broken, "fingerprint_mismatch", "upgrade"),
            ] {
                tx.execute(
                    "INSERT INTO held_imports (user_id, release_group_mbid, release_mbid, \
                     release_track_mbid, recording_mbid, track_number, disc_number, held_path, \
                     reason, source, source_task_id, origin, created_at) \
                     VALUES (?1, ?2, ?3, ?4, ?5, 1, 1, ?6, ?7, 'soulseek', 't-up', ?8, 1.0)",
                    rusqlite::params![
                        user,
                        GOOD_GROUP,
                        GOOD_RELEASE,
                        track_id,
                        recording_id,
                        path.to_string_lossy(),
                        reason,
                        origin
                    ],
                )?;
                ids.push(tx.last_insert_rowid());
            }
            Ok(ids)
        })
        .await
        .expect("held rows");
    let review = worker(Vec::new());
    const ADA: &str = "u-ada:user:Ada";

    let import = |id: i64| format!("/downloads/held/{id}/import");
    let (status, _) = held_call(&review, "POST", &import(seeded[1]), ADA).await;
    assert_eq!(status, 404, "someone else's held file is not found");

    let (status, body) = held_call(&review, "POST", &import(seeded[0]), ADA).await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["status"], "imported");
    let new_copy = music.join("Portishead/Dummy/0101 Mysterons.flac");
    assert!(new_copy.is_file(), "the FLAC replaced it: {body}");
    assert!(!old_copy.exists(), "the MP3 left the album folder");
    let recycled = files_under(&music.join(".recycle"), "mp3");
    assert_eq!(recycled.len(), 1, "the MP3 went to the recycle bin");
    let formats: Vec<String> = sqlx::query_scalar(
        "SELECT file_format FROM local_tracks WHERE availability = 'indexed' \
         AND relative_path LIKE '%Mysterons%'",
    )
    .fetch_all(acquire.db.pool())
    .await
    .expect("catalog");
    assert_eq!(formats, vec!["flac".to_owned()]);

    let discard = format!("/downloads/held/{}/discard", seeded[2]);
    let (status, body) = held_call(&review, "POST", &discard, ADA).await;
    assert_eq!(status, 200, "{body}");
    assert!(
        !held_dir.join("spare.flac").exists(),
        "discarded file deleted"
    );

    // An upgrade file whose quality cannot be read is refused, never taken
    // for "no better" and deleted: the file and its row stay.
    let (status, body) = held_call(&review, "POST", &import(seeded[3]), ADA).await;
    assert_eq!(status, 409, "{body}");
    assert!(
        held_dir.join("broken.flac").is_file(),
        "unreadable file kept"
    );
    assert!(new_copy.is_file(), "the library copy is untouched");

    let (status, body) = held_call(&review, "GET", "/downloads/held", ADA).await;
    assert_eq!(status, 200);
    let left: Vec<i64> = body["items"]
        .as_array()
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item["id"].as_i64())
                .collect()
        })
        .unwrap_or_default();
    assert_eq!(left, vec![seeded[3]], "{body}");
}
