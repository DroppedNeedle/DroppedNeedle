//! Download import journey: a finished download is verified, matched to
//! the requested release, published into the library, and its request
//! resolved; a download whose files are not the release is held for
//! review, fails over to the next candidate, and settles held once the
//! candidates run out. Real worker, landing, publisher and catalog over
//! one scratch database; MusicBrainz is a scripted release source.

use crate::common::Probe;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use droppedneedle_acquire::db::AcquireDb;
use droppedneedle_acquire::downloads::SourceTag;
use droppedneedle_acquire::downloads::journal::Journal;
use droppedneedle_acquire::downloads::landing_rows::{ImportDecisionRow, ImportOutcome};
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
use droppedneedle_kernel::db::DbError;
use droppedneedle_kernel::runtime_config::{ConfigStore, Crypto};
use droppedneedle_library::identify::models::LocalTrackFacts;
use droppedneedle_library::identify::sources::{
    AcoustIdUnavailable, AudioMatches, Edition, EditionPage, EditionQuery, FingerprintSource,
    HeardFiles, ReleaseHit, ReleaseSource, SourceError,
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
        &platform,
        &library,
        Arc::new(LibraryLanding::new(&library, every_priority(releases))),
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
    let (tracks, sealed): (i64, i64) = acquire.db.db().row(
        "SELECT COUNT(*), SUM(i.release_mbid = ?1) FROM local_tracks t \
         JOIN local_album_external_identities i ON i.local_album_id = t.local_album_id \
         WHERE t.availability = 'indexed'",
        rusqlite::params![GOOD_RELEASE],
    );
    assert_eq!(
        (tracks, sealed),
        (2, 2),
        "in the catalog with the matched identity"
    );
    let request: String = acquire.db.db().one(
        "SELECT status FROM request_history WHERE musicbrainz_id_lower = ?1",
        rusqlite::params![GOOD_GROUP],
    );
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
    let held: Vec<String> = acquire.db.db().column(
        "SELECT path FROM acquire_held_files WHERE source_task_id = 't-bad' AND state = 'held'",
        [],
    );
    assert_eq!(held.len(), 2);
    let held_dir = &acquire.held.folders().held;
    assert!(held.iter().all(|path| held_dir.join(path).is_file()));
    let outcomes: Vec<String> = acquire.db.db().column(
        "SELECT outcome FROM download_import_decisions WHERE task_id = 't-bad'",
        [],
    );
    assert!(outcomes.len() > 1, "failed over: {outcomes:?}");
    assert!(outcomes.iter().all(|outcome| outcome == "held"));
    // Every hold and decision stores a reason this version knows, and the
    // task's outcome is that hold, typed: the sentence comes from it.
    let codes: Vec<String> = acquire.db.db().column(
        "SELECT reason FROM acquire_held_files WHERE source_task_id = 't-bad' \
         UNION ALL SELECT reason_code FROM download_import_decisions WHERE task_id = 't-bad'",
        [],
    );
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
/// worker pass. Answers the bundle, the music folder and what it runs
/// on.
async fn land_with_recording(
    task_id: &'static str,
    second: &str,
) -> (droppedneedle_acquire::AcquireSetup, PathBuf, Grounds) {
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
        &platform,
        &library,
        Arc::new(LibraryLanding::new(&library, every_priority(releases))),
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
    (acquire, music, (platform, db))
}

/// What a landing's names, tags and folders prove decides whether a file
/// imports or is held: recording ids, untagged rips, folders, remote names,
/// the group's listing and artist tags.
#[tokio::test]
async fn landing_name_and_tag_evidence_decides() {
    // A recording id MusicBrainz has since merged into the release's
    // recording is followed; an unresolved id would veto the release.
    let case = "merged recording id";
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
        "{case}: {:?}",
        task.error_message
    );
    assert!(
        music
            .join("Portishead/Dummy/0102 Sour Times.flac")
            .is_file(),
        "{case}"
    );

    // A recording id that is not a MusicBrainz id counts as no id: the
    // download imports on its other evidence, without a lookup or a wait.
    let case = "junk recording id";
    let (acquire, music, _platform) = land_with_recording("t-junk", "not-an-id").await;
    let decisions_junk: Vec<(String, Option<String>)> = acquire.db.db().rows(
        "SELECT outcome, reason_code FROM download_import_decisions WHERE task_id = 't-junk'",
        [],
    );
    assert_eq!(
        decisions_junk,
        vec![("imported".to_owned(), None)],
        "{case}"
    );
    assert!(
        music
            .join("Portishead/Dummy/0102 Sour Times.flac")
            .is_file(),
        "{case}"
    );

    // The July wrong single, untagged: named like the requested single, as
    // long as it, from a folder naming another artist. With no artist tag,
    // path or fingerprint, only the request's own names agree: held.
    let case = "untagged file without artist evidence";
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
        decisions(&acquire, "t-untagged-wrong"),
        vec![("held".to_owned(), Some("no_artist_evidence".to_owned()))],
        "{case}"
    );
    assert!(
        files_under(&music, "flac").is_empty(),
        "{case}: nothing imported"
    );

    // An untagged album in the usual "Artist - Album" folder: the folder
    // names the artist, so it imports.
    let case = "untagged album in its artist's folder";
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
        decisions(&acquire, "t-untagged-right"),
        vec![("imported".to_owned(), None)],
        "{case}"
    );
    assert!(
        music
            .join("Portishead/Dummy/0102 Sour Times.flac")
            .is_file(),
        "{case}"
    );

    // Without a pinned edition the landing reads the group's editions from
    // its listing, closest track count first, so the files' edition is
    // found even when a title search ranks three others above it.
    let case = "edition from the group's listing";
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
        decisions(&acquire, "t-listed"),
        vec![("imported".to_owned(), None)],
        "{case}"
    );
    assert!(
        music
            .join("Portishead/Dummy/0102 Sour Times.flac")
            .is_file(),
        "{case}"
    );

    // Soulseek keeps only the last folder of a share
    // (`Music\Portishead\Singles` lands as `downloads/Singles`); the remote
    // name still names the artist, so an untagged single imports.
    let case = "untagged single whose remote folder names the artist";
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
        decisions(&acquire, "t-untagged-remote"),
        vec![("imported".to_owned(), None)],
        "{case}"
    );
    assert!(
        music.join("Portishead/Dummy/0101 Glory Box.flac").is_file(),
        "{case}"
    );

    // A file whose artist tag names someone else and whose length is not
    // the track's is another song: held as a tag conflict, the rest imports.
    let case = "artist tag names someone else";
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
    assert!(
        music.join("Portishead/Dummy/0101 Mysterons.flac").is_file(),
        "{case}"
    );
    assert!(
        !music.join("Portishead/Dummy/0102 Sour Times.flac").exists(),
        "{case}"
    );
    let held: Vec<(String, String)> = acquire.db.db().rows(
        "SELECT reason, COALESCE(reason_detail, '') FROM acquire_held_files \
         WHERE source_task_id = 't-other-artist'",
        [],
    );
    assert_eq!(held.len(), 1, "{case}: {held:?}");
    assert_eq!(held[0].0, "tag_mismatch", "{case}");
    assert!(held[0].1.contains("Dan Romer"), "{case}: {held:?}");

    // A flat file put straight in the downloads folder: once imported, the
    // downloads folder itself stays, even though it is empty.
    let case = "flat file landed alone";
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
        decisions(&acquire, "t-flat"),
        vec![("imported".to_owned(), None)],
        "{case}"
    );
    assert_eq!(
        files_under(&music, "flac").len(),
        1,
        "{case}: the file is imported"
    );
    let downloads = music.parent().expect("scratch dir").join("downloads");
    assert!(downloads.is_dir(), "{case}: the downloads folder stays");
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
/// music folder and what it runs on.
async fn land_for(
    task: NewTask,
    downloads: impl FnOnce(&Path) -> Vec<PathBuf>,
    release: impl FnOnce(&[Option<u64>]) -> Release,
) -> (droppedneedle_acquire::AcquireSetup, PathBuf, Grounds) {
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
) -> (droppedneedle_acquire::AcquireSetup, PathBuf, Grounds) {
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
) -> (droppedneedle_acquire::AcquireSetup, PathBuf, Grounds) {
    land_heard(task, remote, downloads, source, None).await
}

/// [`land_through`], with AcoustID answering from `heard` when given.
async fn land_heard(
    task: NewTask,
    remote: &[&str],
    downloads: impl FnOnce(&Path) -> Vec<PathBuf>,
    source: impl FnOnce(&[Option<u64>]) -> Arc<dyn ReleaseSource>,
    heard: Option<Arc<dyn FingerprintSource>>,
) -> (droppedneedle_acquire::AcquireSetup, PathBuf, Grounds) {
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
        &platform,
        &library,
        Arc::new(LibraryLanding::new(&library, every_priority(releases))),
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
        return (acquire, music, (platform, db));
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
    (acquire, music, (platform, db))
}

/// The landing decisions recorded for `task_id`: outcome and reason.
fn decisions(
    acquire: &droppedneedle_acquire::AcquireSetup,
    task_id: &str,
) -> Vec<(String, Option<String>)> {
    acquire.db.db().rows(
        "SELECT outcome, reason_code FROM download_import_decisions WHERE task_id = ?1 \
         ORDER BY decided_at",
        rusqlite::params![task_id],
    )
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

/// AcoustID hearing one recording in every file, as a landing test
/// scripts it.
struct HearsEverywhere(&'static str);

impl FingerprintSource for HearsEverywhere {
    fn identify<'a>(
        &'a self,
        _tracks: &'a [LocalTrackFacts],
        _priority: RequestPriority,
    ) -> BoxFuture<'a, Result<AudioMatches, AcoustIdUnavailable>> {
        Box::pin(async { Ok(AudioMatches::default()) })
    }

    fn identify_files<'a>(
        &'a self,
        files: &'a [(String, PathBuf)],
        _priority: RequestPriority,
    ) -> BoxFuture<'a, HeardFiles> {
        Box::pin(async move {
            Ok(files
                .iter()
                .map(|(id, _)| (id.clone(), vec![self.0.to_owned()]))
                .collect())
        })
    }
}

/// AcoustID failing as `down` says until that is cleared, then hearing one
/// recording in every file.
pub(crate) struct Outage {
    pub(crate) down: std::sync::Mutex<Option<AcoustIdUnavailable>>,
    pub(crate) heard: String,
}

impl Outage {
    pub(crate) fn new(down: AcoustIdUnavailable) -> Arc<Self> {
        Arc::new(Self {
            down: Some(down).into(),
            heard: "fe000616-c72c-43d4-8cba-a4083e99636b".to_owned(),
        })
    }

    pub(crate) fn end(&self) {
        *self.down.lock().expect("outage") = None;
    }
}

impl FingerprintSource for Outage {
    fn identify<'a>(
        &'a self,
        _tracks: &'a [LocalTrackFacts],
        _priority: RequestPriority,
    ) -> BoxFuture<'a, Result<AudioMatches, AcoustIdUnavailable>> {
        Box::pin(async { Ok(AudioMatches::default()) })
    }

    fn identify_files<'a>(
        &'a self,
        files: &'a [(String, PathBuf)],
        _priority: RequestPriority,
    ) -> BoxFuture<'a, HeardFiles> {
        Box::pin(async move {
            if let Some(down) = *self.down.lock().expect("outage") {
                return Err(down);
            }
            Ok(files
                .iter()
                .map(|(id, _)| (id.clone(), vec![self.heard.clone()]))
                .collect())
        })
    }
}

/// A worker holding one finished download of one tagged track, with AcoustID
/// as `outage` says.
async fn landing_during(
    outage: Arc<Outage>,
) -> (
    droppedneedle_acquire::AcquireSetup,
    Arc<DownloadWorker>,
    PathBuf,
    Grounds,
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
    library.imports.fingerprints = outage;
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
    let files = vec![land_file(
        &downloads.join("job"),
        "01 - Mysterons.flac",
        "flac_full_01.flac",
        "Mysterons",
        1,
    )];
    let lengths: Vec<Option<u64>> = files.iter().map(|path| length_ms(path)).collect();
    let releases: Arc<dyn ReleaseSource> = Arc::new(Releases(vec![release(
        GOOD_RELEASE,
        GOOD_GROUP,
        &["Mysterons"],
        &lengths,
    )]));
    let acquire = acquire_over(
        &platform,
        &library,
        Arc::new(LibraryLanding::new(&library, every_priority(releases))),
    );
    let worker = Arc::new(
        DownloadWorker::fixed(
            acquire.journal.clone(),
            vec![crate::as_source(Arc::new(FixedSource::new(
                downloads,
                files.clone(),
            )))],
            WorkerConfig::default(),
            droppedneedle_kernel::events::EventHub::new(),
            Default::default(),
        )
        .with_landing(acquire.landing.clone()),
    );
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
            store.transition_task("landing", TaskStatus::Downloading, 1.0, None)
        })
        .await
        .expect("seed");
    (acquire, worker, music, (platform, db))
}

/// An AcoustID outage makes a tagged download wait (no waits spent, the wait
/// recorded once, nothing against the source) and it lands once AcoustID
/// answers; a steady error may never end, so it spends the landing's waits.
#[tokio::test]
async fn acoustid_errors_wait_or_spend_the_waits() {
    let case = "outage";
    let outage = Outage::new(AcoustIdUnavailable::Unreachable);
    let (acquire, worker, music, _grounds) = landing_during(outage.clone()).await;
    for pass in 1..=12 {
        worker.run_once(pass).await;
        worker.wait_for_landings().await;
    }
    assert_eq!(
        decisions(&acquire, "landing"),
        vec![(
            "deferred".to_owned(),
            Some("acoustid_unavailable".to_owned())
        )],
        "{case}"
    );
    assert!(
        files_under(&music, "flac").is_empty(),
        "{case}: nothing imported"
    );
    let task = acquire.journal.read_task("landing").await.unwrap().unwrap();
    assert_eq!(task.status, TaskStatus::Processing, "{case}: it waits");

    outage.end();
    worker.run_once(13).await;
    worker.wait_for_landings().await;
    let task = acquire.journal.read_task("landing").await.unwrap().unwrap();
    assert_eq!(task.status, TaskStatus::Completed, "{case}");
    assert_eq!(files_under(&music, "flac").len(), 1, "{case}: it landed");

    let case = "steady error";
    let outage = Outage::new(AcoustIdUnavailable::Refused);
    let (acquire, worker, music, _grounds) = landing_during(outage).await;
    for pass in 1..=12 {
        worker.run_once(pass).await;
        worker.wait_for_landings().await;
    }
    let task = acquire.journal.read_task("landing").await.unwrap().unwrap();
    assert_eq!(task.status, TaskStatus::Failed, "{case}");
    assert!(
        files_under(&music, "flac").is_empty(),
        "{case}: nothing imported"
    );
}

/// What AcoustID hears decides an untagged or mismatched file: a failed
/// merge lookup waits, a merged id is audio proof, and a file heard as
/// another recording imports only within 3 s of the track's length.
#[tokio::test]
async fn landing_audio_evidence_decides() {
    // A merge lookup that failed while nothing else proves the files is an
    // outage, not evidence: the landing waits instead of holding them.
    let case = "merge lookup fails";
    let task = NewTask {
        artist_name: "Yan Qing".to_owned(),
        album_title: "The Arrival".to_owned(),
        ..new_task("t-merge-down", GOOD_GROUP)
    };
    let (acquire, music, _platform) = land_heard(
        task,
        &[],
        |downloads| {
            let path = land_file(
                &downloads.join("Some Other Folder"),
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
            Arc::new(Merged {
                releases: Releases(vec![single]),
                retired: RETIRED,
                kept: String::new(),
            })
        },
        // The fake refuses this id, as MusicBrainz down would.
        Some(Arc::new(HearsEverywhere("not-a-musicbrainz-id"))),
    )
    .await;
    assert_eq!(
        decisions(&acquire, "t-merge-down"),
        vec![(
            "deferred".to_owned(),
            Some("musicbrainz_unavailable".to_owned())
        )],
        "{case}"
    );
    assert!(
        files_under(&music, "flac").is_empty(),
        "{case}: nothing imported"
    );

    // An untagged file heard under an id MusicBrainz merged into the paired
    // track's recording has audio proof of who it is by.
    let case = "heard under a merged id";
    let task = NewTask {
        artist_name: "Yan Qing".to_owned(),
        album_title: "The Arrival".to_owned(),
        ..new_task("t-untagged-merged", GOOD_GROUP)
    };
    let (acquire, music, _platform) = land_heard(
        task,
        &[],
        |downloads| {
            let path = land_file(
                &downloads.join("Some Other Folder"),
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
            let kept = single.tracks[0].recording_id.clone();
            Arc::new(Merged {
                releases: Releases(vec![single]),
                retired: RETIRED,
                kept,
            })
        },
        Some(Arc::new(HearsEverywhere(RETIRED))),
    )
    .await;
    assert_eq!(
        decisions(&acquire, "t-untagged-merged"),
        vec![("imported".to_owned(), None)],
        "{case}"
    );
    assert_eq!(files_under(&music, "flac").len(), 1, "{case}");

    // A file heard as another recording imports only within 3 s of the
    // track's length, as copies of one master are: 2 s off imports, 5 s off
    // is held.
    let case = "heard as another recording";
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
    assert!(
        !music.join("Portishead/Dummy/0101 Mysterons.flac").exists(),
        "{case}"
    );
    assert!(
        music
            .join("Portishead/Dummy/0102 Sour Times.flac")
            .is_file(),
        "{case}"
    );
    let held: Vec<(String,)> = acquire.db.db().rows(
        "SELECT reason FROM acquire_held_files WHERE source_task_id = 't-heard'",
        [],
    );
    assert_eq!(held, vec![("fingerprint_mismatch".to_owned(),)], "{case}");
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
        &platform,
        &library,
        Arc::new(LibraryLanding::new(&library, every_priority(releases))),
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
    let held_dir = acquire.held.folders().held.clone();
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
    let formats: Vec<String> = acquire.db.db().column(
        "SELECT file_format FROM local_tracks WHERE availability = 'indexed' \
         AND relative_path LIKE '%Mysterons%'",
        [],
    );
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
        &platform,
        &library,
        Arc::new(LibraryLanding::new(&library, every_priority(releases))),
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
    let states: Vec<(String, String)> = acquire.db.db().rows(
        "SELECT f.state, e.state FROM acquire_held_files f \
         JOIN acquire_held_entries e ON e.id = f.entry_id ORDER BY f.id",
        [],
    );
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
    let copies: i64 = acquire.db.db().one(
        "SELECT COUNT(*) FROM local_tracks WHERE availability = 'indexed' \
         AND title = 'Mysterons'",
        [],
    );
    assert_eq!(copies, 1, "placed once");
    assert!(files_under(&music.join(".recycle"), "flac").is_empty());
}

/// A discard a restart cut off after it removed the copy: the row is
/// discarded with it. While the held folder itself is not there nothing is
/// judged gone.
#[tokio::test]
async fn a_cut_off_entry_loses_a_row_whose_copy_is_gone() {
    let rig = SingleRig::new(0, None).await;
    let acquire = &rig.acquire;
    let held_dir = acquire.held.folders().held.clone();
    acquire
        .db
        .write("test.held", |tx| {
            tx.execute_batch(
                "INSERT INTO acquire_held_entries (id, user_id, origin, group_key, \
                 release_group_mbid, state, created_at, expires_at) VALUES (1, 'u-ada', \
                 'download', 'album:g', 'g', 'placing', 1, 9999999999999); \
                 INSERT INTO acquire_held_files (entry_id, root, path, original_name, reason, \
                 source, state, created_at) VALUES (1, 'held', 'gone.flac', 'gone.flac', \
                 'weak_match', 'soulseek', 'held', 1);",
            )?;
            Ok(())
        })
        .await
        .expect("held rows");
    let state = || async {
        let state: String = acquire
            .db
            .db()
            .one("SELECT state FROM acquire_held_files", []);
        state
    };

    let _ = std::fs::remove_dir_all(&held_dir);
    acquire.held.recover().await;
    acquire.held.tend().await;
    assert_eq!(state().await, "held", "no folder says nothing about a file");

    std::fs::create_dir_all(&held_dir).expect("held folder back");
    acquire
        .db
        .write("test.claim", |tx| {
            tx.execute("UPDATE acquire_held_entries SET state = 'placing'", [])?;
            Ok(())
        })
        .await
        .expect("claimed");
    acquire.held.recover().await;
    acquire.held.tend().await;
    assert_eq!(state().await, "discarded");
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
    let states: Vec<(i64, String)> = acquire
        .db
        .db()
        .rows("SELECT id, state FROM acquire_held_entries ORDER BY id", []);
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
    let item: String = acquire
        .db
        .db()
        .one("SELECT status FROM drop_import_items WHERE id = 7", []);
    assert_eq!(item, "discarded");
    assert_eq!(
        acquire.held.tend().await,
        0,
        "a second pass changes nothing"
    );
}

/// What a bundle from [`acquire_over`] runs on and must not outlive: its
/// platform, and the scratch database whose folder goes with the last
/// handle.
type Grounds = (droppedneedle_kernel::platform::Platform, AcquireDb);

/// The production acquire bundle over `platform`, finished downloads
/// landing through `landing`.
fn acquire_over(
    platform: &droppedneedle_kernel::platform::Platform,
    library: &LibrarySetup,
    landing: Arc<dyn LandingLibrary>,
) -> droppedneedle_acquire::AcquireSetup {
    crate::common::bundles::acquire_on(platform, library, landing)
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

/// An acquire bundle over a library with no folder, its scratch folder,
/// and what must live as long as the bundle: the auth rig and the scratch
/// database.
async fn bare_acquire() -> (
    droppedneedle_acquire::AcquireSetup,
    PathBuf,
    (AuthRig, AcquireDb),
) {
    let (db, dir, auth) = bare_db().await;
    let platform = TestPlatform::on(db.db(), &dir).build().await;
    let (library, _) = library_at(&platform);
    let landing = Arc::new(LibraryLanding::new(
        &library,
        library.imports.releases.clone(),
    ));
    (acquire_over(&platform, &library, landing), dir, (auth, db))
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
    // at the next one: this rig's MusicBrainz is closed).
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
        Some(Reason::MusicbrainzUnavailable)
    );
}

/// A landing whose download policy or chosen edition does not read never
/// takes the default or none: it waits with a temporary rejection, the files
/// stay where they are, and nothing is said against the source.
#[tokio::test]
async fn a_landing_waits_while_what_it_reads_does_not_read() {
    // An unreadable download policy is never the default quality band.
    {
        let case = "download policy";
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
        let LandingResult::Rejected(rejection) =
            landing.land(&task, None, landed, false).await.result
        else {
            panic!("{case}: the landing went ahead under unreadable settings");
        };
        assert_eq!(
            (rejection.code, rejection.disposition),
            (Reason::SettingsUnreadable, Disposition::Temporary),
            "{case}"
        );
        assert!(file.exists(), "{case}");
        let quarantined: i64 = acquire
            .db
            .db()
            .one("SELECT COUNT(*) FROM download_quarantine", []);
        assert_eq!(quarantined, 0, "{case}");
    }

    // The chosen edition decides what a landing is matched against; while
    // it does not read (a busy database), it is never taken as none.
    let case = "chosen edition";
    let (db, dir, _auth) = bare_db().await;
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
    let mount = dir.join("downloads");
    let file = land_file(
        &mount.join("job"),
        "01 - Mysterons.flac",
        "flac_full_01.flac",
        "Mysterons",
        1,
    );
    let releases = Releases(vec![release(
        GOOD_RELEASE,
        GOOD_GROUP,
        &["Mysterons"],
        &[length_ms(&file)],
    )]);
    let port: Arc<dyn LandingLibrary> = Arc::new(Shaky {
        inner: Arc::new(LibraryLanding::new(
            &library,
            every_priority(Arc::new(releases)),
        )),
        changes: 0.into(),
        after_import: None,
        edition_busy: true,
        busy_once: Default::default(),
    });
    let acquire = acquire_over(&platform, &library, port);
    let landed = Landed {
        mounts: vec![mount.into()],
        files: vec![Reported::path(file.clone())],
    };
    let waiting = rejection(&acquire, "t-edition-busy", landed, false)
        .await
        .expect("the landing waits");
    assert_eq!(
        (waiting.code, waiting.disposition),
        (Reason::JournalUnreadable, Disposition::Temporary),
        "{case}"
    );
    assert!(file.exists(), "{case}: the files stay where they are");
    let quarantined: i64 = acquire
        .db
        .db()
        .one("SELECT COUNT(*) FROM download_quarantine", []);
    assert_eq!(quarantined, 0, "{case}");
}

/// An import whose reads find the database busy places nothing and waits
/// like an unreadable journal, so the download is not failed and spends
/// none of its waits; the next landing imports the files.
#[tokio::test(flavor = "multi_thread")]
async fn a_landing_whose_import_finds_the_database_busy_waits_and_lands_next() {
    use droppedneedle_kernel::db::{DbConfig, open_runtime};

    let dir = crate::common::ScratchDir::new("landing-import-busy");
    crate::common::migrated_db(&dir.join("app.db"));
    // One reader and a short wait for it: while a read holds it, the
    // import's reads answer busy at once.
    let mut db_config = DbConfig::new(
        &dir.join("app.db"),
        std::sync::Arc::new(droppedneedle_kernel::clock::SystemClock),
    )
    .with_read_wait(std::time::Duration::from_millis(100));
    db_config.readers = 1;
    let runtime = open_runtime(&db_config).await.expect("database opens");
    let db = AcquireDb::new(
        runtime.db().clone(),
        Arc::new(droppedneedle_kernel::clock::SystemClock),
    );
    db.add_user("u-ada", "Ada", "user").await.expect("user");
    let config = Arc::new(
        ConfigStore::scratch(&dir, Crypto::from_key_bytes(&[7u8; 32]).expect("key"))
            .expect("config"),
    );
    let platform = TestPlatform::on(runtime.db(), &dir)
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
    let mount = dir.join("downloads");
    let file = land_file(
        &mount.join("job"),
        "01 - Mysterons.flac",
        "flac_full_01.flac",
        "Mysterons",
        1,
    );
    let releases = Releases(vec![release(
        GOOD_RELEASE,
        GOOD_GROUP,
        &["Mysterons"],
        &[length_ms(&file)],
    )]);
    let port: Arc<dyn LandingLibrary> = Arc::new(Shaky {
        inner: Arc::new(LibraryLanding::new(
            &library,
            every_priority(Arc::new(releases)),
        )),
        changes: 0.into(),
        after_import: None,
        edition_busy: false,
        busy_once: std::sync::Mutex::new(Some(runtime.db().clone())),
    });
    let acquire = acquire_over(&platform, &library, port);
    let landed = || Landed {
        mounts: vec![mount.clone().into()],
        files: vec![Reported::path(file.clone())],
    };
    let waiting = rejection(&acquire, "t-import-busy", landed(), false)
        .await
        .expect("the landing waits");
    assert_eq!(
        (waiting.code, waiting.disposition),
        (Reason::JournalUnreadable, Disposition::Temporary),
        "a busy import waits: {}",
        waiting.detail
    );
    assert!(file.exists(), "the files stay where they are");
    assert!(files_under(&music, "flac").is_empty(), "nothing is placed");

    let task = acquire
        .journal
        .read_task("t-import-busy")
        .await
        .expect("task reads")
        .expect("task");
    let again = acquire.landing.land(&task, None, landed(), false).await;
    assert!(
        matches!(again.result, LandingResult::Imported { complete: true }),
        "the next landing imports: {:?}",
        again.result
    );
    assert_eq!(files_under(&music, "flac").len(), 1);
    drop((acquire, library, platform, db));
    runtime.shutdown().await;
}

/// A publish a busy database stopped leaves open journal rows. The next
/// landing of that download recovers it and waits, and the one after
/// imports; only a bundle that needs a person stops the landing.
#[tokio::test]
async fn a_landing_waits_out_its_own_stopped_publish() {
    let (db, dir, _auth) = bare_db().await;
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
    let mount = dir.join("downloads");
    let file = land_file(
        &mount.join("job"),
        "01 - Mysterons.flac",
        "flac_full_01.flac",
        "Mysterons",
        1,
    );
    let releases = Releases(vec![release(
        GOOD_RELEASE,
        GOOD_GROUP,
        &["Mysterons"],
        &[length_ms(&file)],
    )]);
    let port = Arc::new(LibraryLanding::new(
        &library,
        every_priority(Arc::new(releases)),
    ));
    let acquire = acquire_over(&platform, &library, port);
    let landed = || Landed {
        mounts: vec![mount.clone().into()],
        files: vec![Reported::path(file.clone())],
    };
    let seed = |task: &str, state: &str| {
        let source = format!(
            "{}import/{task}/01.flac",
            droppedneedle_library::publish::HIDDEN_PREFIX
        );
        let staged = music.join(&source);
        let staged = staged.display();
        db.db()
            .run_sql(&format!(
                "INSERT INTO library_publish_journal (id, bundle_id, kind, source_root, \
                 source_rel, dest_root, dest_rel, staged, staged_sha256, state) VALUES \
                 ('{task}-000', '{task}', 'audio', 'music', '{source}', 'music', \
                 'gone/01.flac', '{staged}', 'abc', '{state}')"
            ))
            .expect("journal row");
    };

    seed("t-attention", "needs_attention");
    let stuck = rejection(&acquire, "t-attention", landed(), false)
        .await
        .expect("a person must act");
    assert_eq!(
        (stuck.code, stuck.disposition),
        (Reason::LocalFault, Disposition::LocalFault),
        "{}",
        stuck.detail
    );

    seed("t-stopped", "prepared");
    let waiting = rejection(&acquire, "t-stopped", landed(), false)
        .await
        .expect("the landing waits");
    assert_eq!(
        (waiting.code, waiting.disposition),
        (Reason::JournalUnreadable, Disposition::Temporary),
        "{}",
        waiting.detail
    );
    assert!(file.exists(), "the files stay where they are");
    let task = acquire
        .journal
        .read_task("t-stopped")
        .await
        .expect("task reads")
        .expect("task");
    assert_ne!(task.status, TaskStatus::Failed);
    let again = acquire.landing.land(&task, None, landed(), false).await;
    assert!(
        matches!(again.result, LandingResult::Imported { complete: true }),
        "the next landing imports: {:?}",
        again.result
    );
    assert_eq!(files_under(&music, "flac").len(), 1);
}

/// A complete album that arrives with a bonus file in a format the recipe
/// does not list (a Usenet or plugin release, fetched whole) is imported
/// without that file, which pairs with no track.
#[tokio::test]
async fn a_bonus_file_the_recipe_does_not_list_is_left_out() {
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
    let job = dir.join("downloads").join("job");
    let titles = &TITLES[..];
    let tracks: Vec<PathBuf> = TITLES
        .iter()
        .zip(FIXTURES)
        .zip(1u32..)
        .map(|((title, fixture), number)| {
            let file = land_file(
                &job,
                &format!("{number:02} - {title}.flac"),
                fixture,
                title,
                number,
            );
            // The tracks name their release, as a scene release's do.
            save_tags(
                &file,
                &[TagEdit::new(
                    TagField::MusicBrainzReleaseId,
                    vec![GOOD_RELEASE.to_owned()],
                )],
            )
            .expect("release id");
            file
        })
        .collect();
    let bonus = land_file(
        &job,
        "99 - Interview.ogg",
        "management_full.ogg",
        "Interview",
        99,
    );
    let lengths: Vec<_> = tracks.iter().map(|track| length_ms(track)).collect();
    let releases: Arc<dyn ReleaseSource> = Arc::new(Releases(vec![release(
        GOOD_RELEASE,
        GOOD_GROUP,
        titles,
        &lengths,
    )]));
    let acquire = acquire_over(
        &platform,
        &library,
        Arc::new(LibraryLanding::new(&library, every_priority(releases))),
    );
    acquire
        .journal
        .run("test.seed", |store| {
            store.insert_task(&new_task("t-bonus", GOOD_GROUP), 1.0)
        })
        .await
        .expect("task");
    let task = acquire.journal.read_task("t-bonus").await.unwrap().unwrap();
    let report = acquire
        .landing
        .land(
            &task,
            None,
            Landed {
                mounts: vec![dir.join("downloads").into()],
                files: tracks
                    .into_iter()
                    .chain([bonus])
                    .map(Reported::path)
                    .collect(),
            },
            false,
        )
        .await;
    assert!(
        matches!(report.result, LandingResult::Imported { complete: true }),
        "{:?}",
        report.result
    );
    assert_eq!(files_under(&music, "flac").len(), 2);
    assert!(
        files_under(&music, "ogg").is_empty(),
        "the bonus is left out"
    );
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
            &platform,
            &library,
            Arc::new(LibraryLanding::new(&library, every_priority(releases))),
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
            let LandingResult::Held { code, .. } = result else {
                panic!("an attempt that is not the pick was kept: {result:?}");
            };
            assert_eq!(code.code(), "format_not_listed");
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
            for (outcome, at) in [
                (ImportOutcome::Imported, 2.0),
                (ImportOutcome::Deferred, 3.0),
            ] {
                store.record_import_decision(&ImportDecisionRow {
                    task_id: "t-again".to_owned(),
                    attempt_id: Some("t-again-a0".to_owned()),
                    outcome,
                    files_total: 2,
                    files_imported: if outcome == ImportOutcome::Imported {
                        2
                    } else {
                        0
                    },
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
        Some(Reason::MusicbrainzUnavailable)
    );
}

/// A worker whose SABnzbd no longer knows any job, sweeping `complete` as
/// its finished-download folder. The guard keeps the mock serving.
async fn sab_sweeper(
    acquire: &droppedneedle_acquire::AcquireSetup,
    complete: &Path,
) -> (
    Arc<DownloadWorker>,
    droppedneedle_testkit::usenet::ServerGuard,
) {
    use droppedneedle_acquire::sources::SabnzbdSource;
    use droppedneedle_acquire::usenet::policy::UsenetPolicy;
    use droppedneedle_acquire::usenet::sabnzbd::{SabnzbdClient, SabnzbdQueue};
    use droppedneedle_testkit::usenet::{SabnzbdMock, serve_loopback};

    let mock = SabnzbdMock::new();
    let (base, server) = serve_loopback(mock.router()).await.expect("mock serves");
    let policy = UsenetPolicy::v2_defaults();
    let source = SabnzbdSource::new(
        Arc::new(SabnzbdQueue::new(
            SabnzbdClient::new(
                reqwest::Client::new(),
                &base,
                "SABKEY",
                3,
                std::time::Duration::from_millis(1),
            ),
            &base,
            "SABKEY",
            complete.to_path_buf(),
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
                orphan_roots: vec![(SourceTag::Usenet, complete.to_path_buf())],
                ..WorkerConfig::default()
            },
            droppedneedle_kernel::events::EventHub::new(),
            Default::default(),
        )
        .with_landing(acquire.landing.clone()),
    );
    (worker, server)
}

/// The hourly sweep of finished download folders keeps a folder while the
/// publish putting its files into the library is unfinished or needs an
/// administrator, and a failed download's files kept for "Retry import"
/// until they have been kept for 90 days.
#[tokio::test]
async fn the_orphan_sweep_keeps_folders_still_needed() {
    use droppedneedle_acquire::landing::ports::{ImportFile, ImportOrigin};
    use std::time::{Duration, SystemTime};

    // The sweep asks the publisher first, and the folder goes once the
    // write is done.
    {
        let case = "publish";
        const PUBLISHED: &str = "0123456789abcdef0123456789abcdef";

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
        let folder = complete.join(format!("droppedneedle-{PUBLISHED}-0"));
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
        let acquire = acquire_over(&platform, &library, port.clone());
        let receipt = port
            .import(ImportRequest {
                task_id: PUBLISHED.to_owned(),
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
        assert!(
            music.join("Portishead/Dummy/0101 Mysterons.flac").is_file(),
            "{case}"
        );

        let (worker, _server) = sab_sweeper(&acquire, &complete).await;

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
            assert_eq!(changed, 1, "{case}: the import wrote one journal row");
            // Pass 0 is one of the passes the cleanup runs on.
            worker.run_once(0).await;
            assert_eq!(folder.is_dir(), kept, "{case}: publish {state}");
        }
    }

    // A failed download's kept files outlive the sweep however old the
    // folder, and go to cleanup once kept for 90 days.
    let case = "kept for a retry";
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
        &platform,
        &library,
        Arc::new(LibraryLanding::new(
            &library,
            every_priority(Arc::new(Releases(Vec::new()))),
        )),
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

    let (worker, _server) = sab_sweeper(&acquire, &complete).await;

    // Pass 0 is one of the passes the hourly sweep runs on.
    worker.run_once(0).await;
    assert!(
        folder.is_dir(),
        "{case}: kept files outlive the 6-hour sweep"
    );
    assert!(
        acquire
            .dispatch
            .reimportable(TASK)
            .await
            .expect("reimportable"),
        "{case}: and can still be imported again"
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
    assert!(
        !folder.exists(),
        "{case}: expired kept files are cleaned up"
    );
    assert!(
        !acquire
            .dispatch
            .reimportable(TASK)
            .await
            .expect("reimportable"),
        "{case}"
    );
}

/// A library over one Automatic folder and a worker over a finished
/// two-file download, seeded with "landing" (its transfer done) and
/// "queued" (not started). With `unmounted`, the library folder is gone.
struct WaitRig {
    acquire: droppedneedle_acquire::AcquireSetup,
    library: LibrarySetup,
    worker: Arc<DownloadWorker>,
    queue: droppedneedle_acquire::downloads::queue::DownloadQueue,
    viewer: HeldViewer,
    music: PathBuf,
    downloads: PathBuf,
    files: Vec<PathBuf>,
    _grounds: Grounds,
}

async fn wait_rig(unmounted: bool) -> WaitRig {
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
    if unmounted {
        std::fs::remove_dir(&music).expect("music root unmounted");
    }

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
        &platform,
        &library,
        Arc::new(LibraryLanding::new(&library, every_priority(releases))),
    );
    let source = Arc::new(FixedSource::new(downloads.clone(), files.clone()));
    let worker = Arc::new(
        DownloadWorker::fixed(
            acquire.journal.clone(),
            vec![crate::as_source(source)],
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
    WaitRig {
        acquire,
        library,
        worker,
        queue,
        viewer,
        music,
        downloads,
        files,
        _grounds: (platform, db),
    }
}

/// While the library folder is not there or the library is switched off, a
/// finished download does not land and a queued one does not start: both
/// wait, the queue says why, nothing is read or decided and the files stay.
/// Both go ahead by themselves once the library can take them again.
#[tokio::test]
async fn downloads_wait_while_the_library_cannot_take_them() {
    use droppedneedle_kernel::runtime_config::secret_sections::TypedLibrary;

    // The library folder is not there. A landing that meets it gone anyway
    // waits with `library_folder_missing`, naming it.
    {
        let case = "missing folder";
        let rig = wait_rig(true).await;
        let (acquire, worker, queue, viewer, music) = (
            &rig.acquire,
            &rig.worker,
            &rig.queue,
            &rig.viewer,
            &rig.music,
        );
        for pass in 1..=3 {
            worker.run_once(pass).await;
            worker.wait_for_landings().await;
        }
        let waiting = format!("Waiting: library folder {} isn't there.", music.display());
        let queued = task_now(queue, viewer, "queued").await;
        assert_eq!(queued.row.task.status, TaskStatus::Queued, "{case}");
        assert_eq!(queued.facts.attempts, 0, "{case}: nothing was fetched");
        assert_eq!(queued.reason, Some(Reason::LibraryFolderMissing), "{case}");
        assert_eq!(
            queued.reason_text.as_deref(),
            Some(waiting.as_str()),
            "{case}"
        );
        let landing = task_now(queue, viewer, "landing").await;
        assert_eq!(
            landing.row.task.status,
            TaskStatus::Downloading,
            "{case}: it waits"
        );
        assert_eq!(landing.reason, Some(Reason::LibraryFolderMissing), "{case}");
        assert_eq!(
            landing.reason_text.as_deref(),
            Some(waiting.as_str()),
            "{case}"
        );
        let decided: i64 = acquire
            .db
            .db()
            .one("SELECT COUNT(*) FROM download_import_decisions", []);
        assert_eq!(decided, 0, "{case}: nothing was landed while it waits");
        assert!(
            rig.files.iter().all(|path| path.is_file()),
            "{case}: files wait in place"
        );

        let task = acquire.journal.read_task("landing").await.unwrap().unwrap();
        let landed = Landed {
            mounts: vec![rig.downloads.clone().into()],
            files: rig.files.iter().cloned().map(Reported::path).collect(),
        };
        let LandingResult::Rejected(rejection) = acquire
            .landing
            .land(&task, None, landed, false)
            .await
            .result
        else {
            panic!("{case}: the landing went ahead without its library folder");
        };
        assert_eq!(
            (rejection.code, rejection.detail.as_str()),
            (Reason::LibraryFolderMissing, music.to_str().unwrap()),
            "{case}"
        );

        // The folder is back: the landing imports and the queued task starts.
        std::fs::create_dir_all(music).expect("music root back");
        worker.run_once(3).await;
        worker.wait_for_landings().await;
        let landed = acquire.journal.read_task("landing").await.unwrap().unwrap();
        assert_eq!(
            landed.status,
            TaskStatus::Completed,
            "{case}: {:?}",
            landed.error_message
        );
        assert!(
            music.join("Portishead/Dummy/0101 Mysterons.flac").is_file(),
            "{case}"
        );
        let started = task_now(queue, viewer, "queued").await;
        assert!(
            started.facts.attempts > 0,
            "{case}: the queued download started"
        );
    }

    // The library is switched off. A person's Import of a held file is told
    // the library is off and the held file is kept.
    let case = "switched off";
    let rig = wait_rig(false).await;
    let (acquire, worker, queue, viewer, music) = (
        &rig.acquire,
        &rig.worker,
        &rig.queue,
        &rig.viewer,
        &rig.music,
    );
    let switch = |on: bool| {
        rig.library
            .settings
            .store()
            .update_secret_blocking::<TypedLibrary, String, _>(move |stored| {
                let mut settings = stored.into_inner();
                settings.enabled = on;
                Ok(settings)
            })
            .expect("library switch saved");
        rig.library.core.refresh_registry();
    };

    switch(false);
    for pass in 1..=2 {
        worker.run_once(pass).await;
        worker.wait_for_landings().await;
    }
    let waiting = "Waiting: the library is switched off.";
    let queued = task_now(queue, viewer, "queued").await;
    assert_eq!(queued.facts.attempts, 0, "{case}: nothing was fetched");
    assert_eq!(queued.reason_text.as_deref(), Some(waiting), "{case}");
    let landing = task_now(queue, viewer, "landing").await;
    assert_eq!(
        landing.row.task.status,
        TaskStatus::Downloading,
        "{case}: it waits"
    );
    assert_eq!(landing.reason_text.as_deref(), Some(waiting), "{case}");
    let decided: i64 = acquire
        .db
        .db()
        .one("SELECT COUNT(*) FROM download_import_decisions", []);
    assert_eq!(
        decided, 0,
        "{case}: nothing was landed while the library is off"
    );
    assert!(
        rig.files.iter().all(|path| path.is_file()),
        "{case}: files wait in place"
    );

    let held_dir = acquire.held.folders().held.clone();
    std::fs::create_dir_all(&held_dir).expect("held dir");
    std::fs::copy(&rig.files[0], held_dir.join("held.flac")).expect("held copy");
    let local_fault = Reason::LocalFault.code();
    let first_track = format!("{}8", &GOOD_RELEASE[..GOOD_RELEASE.len() - 1]);
    let first_recording = format!("{}8", &GOOD_GROUP[..GOOD_GROUP.len() - 1]);
    let entry: i64 = acquire
        .db
        .write("test.held", move |tx| {
            tx.execute(
                "INSERT INTO acquire_held_entries (user_id, origin, group_key, \
                 release_group_mbid, state, created_at, expires_at) VALUES ('u-ada', \
                 'download', ?1, ?2, 'held', 1, 2)",
                rusqlite::params![format!("album:{GOOD_GROUP}"), GOOD_GROUP],
            )?;
            let entry = tx.last_insert_rowid();
            tx.execute(
                "INSERT INTO acquire_held_files (entry_id, root, path, original_name, \
                 release_mbid, release_track_mbid, recording_mbid, track_number, \
                 disc_number, reason, source, source_task_id, state, created_at) VALUES \
                 (?1, 'held', 'held.flac', 'held.flac', ?2, ?3, ?4, 1, 1, ?5, 'soulseek', \
                 't-held', 'held', 1)",
                rusqlite::params![
                    entry,
                    GOOD_RELEASE,
                    first_track,
                    first_recording,
                    local_fault
                ],
            )?;
            Ok(entry)
        })
        .await
        .expect("held row");
    let refused = acquire
        .held
        .import(viewer, entry, None, "u-ada")
        .await
        .expect_err("a held import waits while the library is off");
    assert!(
        refused.to_string().contains("switched off"),
        "{case}: {refused}"
    );
    assert!(
        held_dir.join("held.flac").is_file(),
        "{case}: the held file is kept"
    );

    // Back on: the download lands by itself on the next pass.
    switch(true);
    worker.run_once(3).await;
    worker.wait_for_landings().await;
    let landed = acquire.journal.read_task("landing").await.unwrap().unwrap();
    assert_eq!(
        landed.status,
        TaskStatus::Completed,
        "{case}: {:?}",
        landed.error_message
    );
    assert!(
        music.join("Portishead/Dummy/0101 Mysterons.flac").is_file(),
        "{case}"
    );
    let started = task_now(queue, viewer, "queued").await;
    assert!(
        started.facts.attempts > 0,
        "{case}: the queued download started"
    );
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
    let acquire = acquire_over(&platform, &library, port);
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
    _grounds: Grounds,
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
        &platform,
        &library,
        Arc::new(LibraryLanding::new(
            &library,
            every_priority(Arc::new(Releases(releases.clone()))),
        )),
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
        _grounds: (platform, db),
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
                policy_revision: registry.fingerprint().to_owned(),
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
             local_album_id, title, 'Portishead', 1767225600000 FROM local_tracks \
             WHERE id = '{track}'; \
             INSERT INTO catalog_user_favorites (user_id, item_kind, item_id, created_at) \
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

    fn scalar<T>(&self, sql: &str) -> T
    where
        T: rusqlite::types::FromSql,
    {
        self.acquire.db.db().one(sql, [])
    }

    fn album_count(&self) -> i64 {
        self.scalar("SELECT COUNT(*) FROM local_albums WHERE retired_into_album_id IS NULL")
    }

    /// The track at `rel`: id, album, membership (source, locked) and
    /// availability.
    fn row_at(&self, rel: &str) -> (String, String, String, i64, String) {
        self.acquire.db.db().row(
            "SELECT id, local_album_id, membership_source, membership_locked, availability \
             FROM local_tracks WHERE root_id = 'music' AND relative_path = ?1",
            rusqlite::params![rel],
        )
    }

    /// A track's MusicBrainz identity: recording, release, release
    /// track, disc, position and source.
    fn identity(&self, track: &str) -> Option<TrackIdentityRow> {
        self.acquire.db.db().maybe_row(
            "SELECT recording_mbid, release_mbid, release_track_mbid, medium_position, \
             release_track_position, decision_source FROM local_track_external_identities \
             WHERE local_track_id = ?1",
            rusqlite::params![track],
        )
    }

    /// The album's identity row: release, source, chooser, time, revision.
    fn album_identity(&self, album: &str) -> (Option<String>, String, Option<String>, i64, i64) {
        self.acquire.db.db().row(
            "SELECT release_mbid, decision_source, selected_by_user_id, selected_at, \
             row_revision FROM local_album_external_identities WHERE local_album_id = ?1",
            rusqlite::params![album],
        )
    }

    /// Where u-ada's play and star of `track` point: (play track, play
    /// album, starred).
    fn history(&self, track: &str) -> (String, String, bool) {
        self.acquire.db.db().row(
            "SELECT p.local_track_id, p.local_album_id, EXISTS(SELECT 1 FROM \
             catalog_user_favorites f WHERE f.item_kind = 'track' AND f.item_id = ?2) \
             FROM library_play_history p WHERE p.id = ?1",
            rusqlite::params![format!("play-{track}"), track],
        )
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
// id, plays and star when it is that copy's own; never otherwise. On an
// automatic edition the import's own seal places it without confirming the
// match. Each scenario is one copy, all in one library.
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
    // whose file is gone. Copy 6 is matched automatically. Copy 8 is merged
    // into copy 9 later.
    let copies = [1, 2, 3, 4, 5, 7, 8, 9];
    for s in copies.into_iter().chain([6]) {
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
            if copies.contains(&s) || s == 6 {
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
    rig.set_edition(album(6), &ed(6).group, &ed(6).new.id, "automatic");
    for s in 2..=5 {
        rig.place(&x_of(s), &ed(s).new, 1, "manual");
        rig.listen(&x_of(s));
    }
    rig.sql(&format!(
        "UPDATE local_tracks SET availability = 'excluded', manual_excluded = 1 \
         WHERE id = '{}'",
        x_of(5)
    ));
    let before = rig.album_count();
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
    let identity_before = rig.album_identity(album(1));
    let retrying = rig.worker(
        vec![rig.landed("t-join-1", &ed(1).new, 1, FIXTURES[1])],
        true,
    );
    retrying.run_once(1).await;
    retrying.run_once(2).await;
    retrying.wait_for_landings().await;
    let fetched = copy_rel(1, SOUR_TIMES);
    let (track, on, source, locked, availability) = rig.row_at(&fetched);
    assert_eq!(
        (on.as_str(), source.as_str(), locked, availability.as_str()),
        (album(1), "manual", 1, "indexed"),
        "the retry's file joins the copy, beside its files, locked"
    );
    assert_eq!(rig.album_count(), before, "no new album");
    assert_eq!(
        rig.identity(&track),
        Some(placed_on(&ed(1).new, 1)),
        "placed on the chosen edition's disc 2"
    );
    assert_eq!(
        rig.album_identity(album(1)),
        identity_before,
        "the copy's match is left as it was"
    );

    // 2. The copy's own old row at the path: the file takes its id, and
    // its play and star stay where they were.
    ask(2, "t-join-2").await;
    rig.land(vec![rig.landed("t-join-2", &ed(2).new, 1, FIXTURES[1])])
        .await;
    let (track, on, _, locked, availability) = rig.row_at(&copy_rel(2, SOUR_TIMES));
    assert_eq!(
        (track.as_str(), on.as_str(), locked, availability.as_str()),
        (x_of(2).as_str(), album(2), 1, "indexed")
    );
    assert_eq!(rig.identity(&track), Some(placed_on(&ed(2).new, 1)));
    assert_eq!(rig.history(&x_of(2)), (x_of(2), album(2).to_owned(), true));

    // 3. The copy's edition changed to another edition of the album after
    // the ask: the file still joins it under the old row's id, unplaced.
    ask(3, "t-join-3").await;
    rig.set_edition(album(3), &ed(3).group, &ed(3).new2.id, "manual");
    let identity_before = rig.album_identity(album(3));
    rig.land(vec![rig.landed("t-join-3", &ed(3).new, 1, FIXTURES[1])])
        .await;
    let (track, on, _, locked, availability) = rig.row_at(&copy_rel(3, SOUR_TIMES));
    assert_eq!(
        (track.as_str(), on.as_str(), locked, availability.as_str()),
        (x_of(3).as_str(), album(3), 1, "indexed")
    );
    assert_eq!(rig.identity(&track), None, "its old place is gone");
    assert_eq!(rig.history(&x_of(3)), (x_of(3), album(3).to_owned(), true));
    assert_eq!(rig.album_identity(album(3)), identity_before);

    // 3b. The copy now names another album: the file is filed by its
    // tags under a new id, and the old row keeps everything, off the path.
    ask(4, "t-join-4").await;
    rig.set_edition(album(4), &mb(4, 1, 1), &ed(4).other.id, "manual");
    let (missing_at, old_identity): (i64, Option<String>) = rig.acquire.db.db().row(
        "SELECT t.missing_at, i.release_track_mbid FROM local_tracks t \
         LEFT JOIN local_track_external_identities i ON i.local_track_id = t.id WHERE t.id = ?1",
        rusqlite::params![x_of(4)],
    );
    rig.land(vec![rig.landed("t-join-4", &ed(4).new, 1, FIXTURES[1])])
        .await;
    let (filed, filed_on, _, _, _) = rig.row_at(&copy_rel(4, SOUR_TIMES));
    assert_ne!(filed, x_of(4), "a new id");
    assert_ne!(filed_on, album(4), "filed by its tags");
    let kept: (String, String, i64, Option<String>, String) = rig.acquire.db.db().row(
        "SELECT t.local_album_id, t.availability, t.missing_at, i.release_track_mbid, \
         t.relative_path FROM local_tracks t LEFT JOIN local_track_external_identities i \
         ON i.local_track_id = t.id WHERE t.id = ?1",
        rusqlite::params![x_of(4)],
    );
    assert_eq!(
        (kept.0.as_str(), kept.1.as_str(), kept.2, kept.3),
        (album(4), "missing", missing_at, old_identity)
    );
    assert_ne!(kept.4, copy_rel(4, SOUR_TIMES), "off the path");
    assert_eq!(rig.history(&x_of(4)), (x_of(4), album(4).to_owned(), true));
    let on_copy: i64 = rig.scalar(&format!(
        "SELECT COUNT(*) FROM local_tracks WHERE local_album_id = '{}'",
        album(4)
    ));
    assert_eq!(on_copy, 2, "nothing joins the copy");

    // An excluded row at the path is never brought back: the file joins
    // under a new id, placed, and the row keeps its id, off the path.
    ask(5, "t-join-5").await;
    rig.land(vec![rig.landed("t-join-5", &ed(5).new, 1, FIXTURES[1])])
        .await;
    let (track, on, _, _, availability) = rig.row_at(&copy_rel(5, SOUR_TIMES));
    assert_ne!(track, x_of(5));
    assert_eq!((on.as_str(), availability.as_str()), (album(5), "indexed"));
    assert_eq!(rig.identity(&track), Some(placed_on(&ed(5).new, 1)));
    let excluded: (String, String) = rig.acquire.db.db().row(
        "SELECT availability, relative_path FROM local_tracks WHERE id = ?1",
        rusqlite::params![x_of(5)],
    );
    assert_eq!(excluded.0, "excluded");
    assert_ne!(excluded.1, copy_rel(5, SOUR_TIMES));

    // 6. An automatic edition: the file joins the copy and the import's
    // own seal places it on that edition, without confirming the match.
    let before_6 = rig.album_count();
    ask(6, "t-join-6").await;
    rig.land(vec![rig.landed("t-join-6", &ed(6).new, 1, FIXTURES[1])])
        .await;
    let (track, on, _, locked, _) = rig.row_at(&copy_rel(6, SOUR_TIMES));
    assert_eq!((on.as_str(), locked), (album(6), 1), "6: joins the copy");
    assert_eq!(rig.album_count(), before_6, "6: no new album");
    let identity = rig.identity(&track).expect("6: sealed");
    assert_eq!(
        (identity.2.as_deref(), identity.5.as_str()),
        (
            Some(placed_on(&ed(6).new, 1).2.as_deref().unwrap_or_default()),
            "automatic"
        ),
        "6: placed on the automatic edition"
    );
    assert_eq!(
        rig.album_identity(album(6)).4,
        3,
        "6: the match is not resealed"
    );

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
    let (track, on, _, locked, _) = rig.row_at(&copy_rel(7, SOUR_TIMES));
    assert_eq!((on.as_str(), locked), (album(7), 1));
    assert_eq!(rig.identity(&track), None);

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
    let (track, on, _, _, _) = rig.row_at(&format!("Portishead/Dummy 8/{SOUR_TIMES}"));
    assert_eq!(on, album(9), "on the album its copy joined");
    assert_eq!(rig.identity(&track), Some(placed_on(&ed(8).new, 1)));

    // A later scan files nothing elsewhere.
    rig.scan().await;
    for (s, rel) in [(1, copy_rel(1, SOUR_TIMES)), (2, copy_rel(2, SOUR_TIMES))] {
        let (track, on, _, _, availability) = rig.row_at(&rel);
        assert_eq!((on.as_str(), availability.as_str()), (album(s), "indexed"));
        assert_eq!(rig.identity(&track), Some(placed_on(&ed(s).new, 1)));
    }
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

        let (track, on, _, _, availability) = rig.row_at(&copy_rel(1, SOUR_TIMES));
        assert_eq!(
            (on.as_str(), availability.as_str()),
            (album.as_str(), "indexed"),
            "{point:?}"
        );
        let identity = rig
            .identity(&track)
            .unwrap_or_else(|| panic!("sealed with the commit at {point:?}"));
        assert_eq!(
            (identity.2.as_deref(), identity.5.as_str()),
            (
                Some(placed_on(&e.new, 1).2.as_deref().unwrap_or_default()),
                "automatic"
            ),
            "{point:?}"
        );
        let queued: i64 = rig.scalar(&format!(
            "SELECT COUNT(*) FROM library_management_automatic_queue \
                 WHERE local_album_id = '{album}'"
        ));
        assert_eq!(
            queued, 1,
            "queued for automatic management with the commit at {point:?}"
        );
        let imported: i64 = rig.scalar(&format!(
            "SELECT COUNT(*) FROM library_track_management_state \
                 WHERE local_track_id = '{track}' AND last_outcome = 'imported'"
        ));
        assert_eq!(
            imported, 1,
            "the commit records the landed file as imported at {point:?}"
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

        let (track, on, _, _, availability) = rig.row_at(&copy_rel(1, SOUR_TIMES));
        assert_eq!(
            (on.as_str(), availability.as_str()),
            (album.as_str(), "indexed"),
            "{case}"
        );
        let identity = rig
            .identity(&track)
            .unwrap_or_else(|| panic!("matched: {case}"));
        assert_eq!(identity.2, placed_on(&e.new, 1).2, "{case}");
        let placed = rig.music.join(copy_rel(1, SOUR_TIMES));
        let format = droppedneedle_library::tags::format_for_path(&placed).expect("format");
        let (tag, _) = droppedneedle_library::tags::read::read_scan_metadata(&placed, format)
            .expect("tags read");
        let policy: String = rig.scalar(&format!(
            "SELECT applied_policy FROM local_tracks WHERE id = '{track}'"
        ));
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

        let rows: Vec<(String, String, String)> = rig.acquire.db.db().rows(
            "SELECT root_id, relative_path, applied_policy FROM local_tracks \
             ORDER BY relative_path",
            [],
        );
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
    let before = rig.album_count();

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
    let (flac, on, _, locked, _) = rig.row_at(&copy_rel(1, MYSTERONS));
    assert_ne!(flac, old);
    assert_eq!((on.as_str(), locked), (album.as_str(), 1));
    assert_eq!(rig.album_count(), before);
    assert_eq!(rig.identity(&flac), Some(placed_on(&e.new, 0)));
    assert!(
        !rig.music.join(&mp3).exists(),
        "the mp3 is in the recycle bin"
    );
    assert_eq!(rig.history(&old).0, flac, "the play follows the new file");
    let alias: String = rig.scalar(&format!(
        "SELECT local_track_id FROM local_track_aliases WHERE alias = '{old}'"
    ));
    assert_eq!(alias, flac, "the mp3's id opens the flac");

    // Two copies: "Get missing tracks" on copy A.
    let b_before: Vec<(String, String)> = rig.acquire.db.db().rows(
        "SELECT id, relative_path FROM local_tracks WHERE local_album_id = ?1 ORDER BY id",
        rusqlite::params![&copy_b],
    );
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
    let (track, on, _, _, _) = rig.row_at(&copy_rel(21, SOUR_TIMES));
    assert_eq!(on, copy_a);
    assert_eq!(rig.identity(&track), Some(placed_on(&two.new, 1)));
    let b_after: Vec<(String, String)> = rig.acquire.db.db().rows(
        "SELECT id, relative_path FROM local_tracks WHERE local_album_id = ?1 ORDER BY id",
        rusqlite::params![&copy_b],
    );
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
        let (track, on, _, locked, availability) = rig.row_at(&copy_rel(s, SOUR_TIMES));
        assert_eq!(
            (on.as_str(), locked, availability.as_str()),
            (copy.as_str(), 1, "indexed")
        );
        assert_eq!(rig.identity(&track), Some(placed_on(&eds.new, 1)));
        if s == 2 {
            assert_ne!(track, x, "the excluded row is not brought back");
            let gone: String = rig.scalar(&format!(
                "SELECT relative_path FROM local_tracks WHERE id = '{x}'"
            ));
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
    let (track, on, _, locked, _) = rig.row_at(&copy_rel(1, SOUR_TIMES));
    assert_eq!((on.as_str(), locked), (album.as_str(), 1));
    assert_eq!(rig.identity(&track), None);
    assert_eq!(rig.identity(&holder), Some(placed_on(&e.new, 1)));
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
            "INSERT INTO catalog_playlists (id, name, created_at, updated_at) \
             VALUES ('list', 'L', 0, 0); \
             INSERT INTO catalog_playlist_tracks (id, playlist_id, position, track_name, artist_name, \
             album_name, source_type, created_at, library_file_id) \
             VALUES ('entry', 'list', 0, 'T', 'A', 'B', 'local', 0, '{old}'); \
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
    fn availability(&self, track: &str) -> String {
        self.scalar(&format!(
            "SELECT availability FROM local_tracks WHERE id = '{track}'"
        ))
    }

    /// Where O's play, star, playlist entry and title override are:
    /// (play's track, starred track, entry's track, override's track).
    fn version_history(&self, old: &str) -> (String, String, String, String) {
        self.acquire.db.db().row(
            "SELECT (SELECT local_track_id FROM library_play_history WHERE id = 'play-' || ?1), \
             (SELECT item_id FROM catalog_user_favorites WHERE item_kind = 'track' \
               AND user_id = 'u-ada'), \
             (SELECT library_file_id FROM catalog_playlist_tracks WHERE id = 'entry'), \
             (SELECT local_track_id FROM library_management_overrides WHERE id = 'title')",
            rusqlite::params![old],
        )
    }

    /// The track O's id opens, when it is an alias.
    fn alias_of(&self, old: &str) -> Option<String> {
        self.acquire.db.db().maybe(
            "SELECT local_track_id FROM local_track_aliases WHERE alias = ?1",
            rusqlite::params![old],
        )
    }

    /// Whether O's field overrides follow its alias (1) or stay (0).
    fn overrides_follow(&self, old: &str) -> i64 {
        self.scalar(&format!(
            "SELECT moves_overrides FROM local_track_aliases WHERE alias = '{old}'"
        ))
    }

    fn held_reasons(&self, task_id: &str) -> Vec<String> {
        self.acquire.db.db().column(
            "SELECT reason FROM acquire_held_files WHERE source_task_id = ?1",
            rusqlite::params![task_id],
        )
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

// A move that fails after the file landed keeps the pair: the alias stays,
// and the next drain moves what people made, the old row's overrides
// staying on it.
#[tokio::test]
async fn a_failed_continuation_keeps_its_pair_for_the_drain() {
    let v = VersionCase::new(16, 1, true, "02 Sour Times.flac").await;
    let rig = &v.rig;
    rig.sql(
        "CREATE TRIGGER no_move BEFORE UPDATE OF local_track_id ON library_play_history \
         BEGIN SELECT RAISE(ABORT, 'refused'); END",
    );
    v.ask("t-version").await;
    rig.land(vec![v.sour("t-version")]).await;
    let (new, ..) = rig.row_at(&copy_rel(16, SOUR_TIMES));
    assert_eq!(rig.alias_of(&v.old), Some(new.clone()));
    assert_eq!(rig.overrides_follow(&v.old), 0);
    assert_eq!(
        rig.version_history(&v.old),
        (v.old.clone(), v.old.clone(), v.old.clone(), v.old.clone()),
        "the failed move left everything on O"
    );
    rig.sql("DROP TRIGGER no_move");
    let store = rig.library.identify_store.clone();
    tokio::task::spawn_blocking(move || store.sweep_artist_links())
        .await
        .expect("sweep ran")
        .expect("sweep");
    assert_eq!(
        rig.version_history(&v.old),
        (new.clone(), new.clone(), new.clone(), v.old.clone()),
        "the drain moved the history; the override stays on O"
    );
}

// A copy that leaves its folder while its landing is being written: the
// commit refuses, the files are put back, and the landing is planned again.
// The file never lands in the folder the copy left.
#[tokio::test]
async fn a_copy_gone_at_commit_is_planned_again() {
    let e = editions(30, &fixture_lengths());
    let rig = join_rig(vec![e.old.clone(), e.new.clone()]).await;
    rig.plant(&copy_rel(30, MYSTERONS), 0, &e.old);
    rig.scan().await;
    let track = rig.track_at(&copy_rel(30, MYSTERONS));
    let album = rig.album_of(&track);
    rig.set_edition(&album, &e.group, &e.new.id, "manual");
    rig.place(&track, &e.new, 0, "manual");
    rig.ask(
        "t-gone",
        &e.group,
        &e.new.title,
        "user",
        Some(&e.new.id),
        Some((&album, &e.new.id)),
        &[(2, 1)],
    )
    .await;
    rig.sql(&format!(
        "CREATE TRIGGER copy_gone AFTER INSERT ON library_publish_journal BEGIN \
         UPDATE local_tracks SET availability = 'missing', missing_at = 1 \
         WHERE local_album_id = '{album}'; END"
    ));
    rig.land(vec![rig.landed("t-gone", &e.new, 1, FIXTURES[1])])
        .await;
    assert!(
        !rig.music.join(copy_rel(30, SOUR_TIMES)).exists(),
        "nothing in the folder the copy left"
    );
    assert_eq!(
        files_under(&rig.music, "flac").len(),
        2,
        "the second plan filed the file by its tags"
    );
}

// "Replace tracks you have in another version": the edition's version of
// a track lands in the copy, on its edition track; the person's version
// goes to the recycle bin and its play, star and playlist entry follow the
// new file, while its field overrides stay on it. Rows: another recording
// than NEW's, the same recording (never kept as owned), and O at the
// edition's own path (the new file takes an id of its own, O's id opens it).
#[tokio::test]
async fn a_version_is_replaced_and_its_history_follows() {
    for (s, other_recording, o_name) in [
        (11u16, true, "02 Sour Times.flac"),
        (12, false, "02 Sour Times.flac"),
        (14, true, SOUR_TIMES),
    ] {
        let v = VersionCase::new(s, 1, other_recording, o_name).await;
        let rig = &v.rig;
        let before = rig.album_count();
        v.ask("t-version").await;
        rig.land(vec![v.sour("t-version")]).await;
        let (new, on, _, locked, availability) = rig.row_at(&copy_rel(s, SOUR_TIMES));
        assert_eq!(
            (on.as_str(), locked, availability.as_str()),
            (v.album.as_str(), 1, "indexed"),
            "{s}: the edition's version is in the copy"
        );
        assert_ne!(new, v.old, "{s}");
        assert_eq!(rig.identity(&new), Some(placed_on(&v.e.new, 1)), "{s}");
        if o_name != SOUR_TIMES {
            assert!(
                !rig.music.join(copy_rel(s, &v.o_name)).exists(),
                "{s}: O's file left its path"
            );
        }
        assert!(rig.in_recycle_bin(&v.o_name), "{s}: O's file is recycled");
        assert_eq!(rig.availability(&v.old), "missing", "{s}");
        assert_eq!(
            rig.version_history(&v.old),
            (new.clone(), new.clone(), new.clone(), v.old.clone()),
            "{s}: the history moves, the override stays on O"
        );
        assert_eq!(rig.alias_of(&v.old), Some(new.clone()), "{s}");
        assert_eq!(rig.overrides_follow(&v.old), 0, "{s}");
        assert!(rig.held_reasons("t-version").is_empty(), "{s}");
        assert_eq!(rig.album_count(), before, "{s}: no album rows change");
        let file = rig.music.join(copy_rel(s, SOUR_TIMES));
        let format = droppedneedle_library::tags::format_for_path(&file).expect("format");
        let (tag, _) =
            droppedneedle_library::tags::read::read_scan_metadata(&file, format).expect("tags");
        assert_eq!(
            tag.title, "Sour Times",
            "{s}: not retagged with O's override"
        );
    }
}

// The edition's version came in worse than the person's: it is held, O is
// left exactly as it was, and the landing settles on our side, so no other
// source is asked and this one is not blamed. The second row first runs a
// better version's swap that stopped before O could leave, unwound by
// recovery: O is still in place and is the version that stays.
#[tokio::test]
async fn a_worse_version_is_held_and_yours_stays() {
    for (s, stopped_swap) in [(13u16, false), (19, true)] {
        let v = VersionCase::new(s, 0, false, "01 Mysterons.flac").await;
        let rig = &v.rig;
        let o_path = rig.music.join(copy_rel(s, &v.o_name));
        let o_bytes = std::fs::read(&o_path).expect("O's file");
        v.ask("t-version").await;
        if stopped_swap {
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
            assert_eq!(
                std::fs::read(&o_path).expect("O's file"),
                o_bytes,
                "{s}: the stopped swap left O"
            );
            assert!(
                !rig.in_recycle_bin(&v.o_name),
                "{s}: nothing went to the bin"
            );
            assert_eq!(rig.availability(&v.old), "indexed", "{s}");
        }

        let landed = rig.landed("t-version", &v.e.new, 0, "mp3_full_01.mp3");
        let report = v.land_directly("t-version", landed).await;
        assert!(
            matches!(&report.result, LandingResult::Held { code, .. } if *code == Reason::VersionLowerQuality),
            "{s}: {:?}",
            report.result
        );
        assert!(report.local_only, "{s}: the source is not blamed");
        assert_eq!(std::fs::read(&o_path).expect("O's file"), o_bytes, "{s}");
        let (track, _, _, _, availability) = rig.row_at(&copy_rel(s, &v.o_name));
        assert_eq!(
            (track.as_str(), availability.as_str()),
            (v.old.as_str(), "indexed"),
            "{s}"
        );
        assert_eq!(rig.availability(&v.old), "indexed", "{s}");
        assert_eq!(rig.alias_of(&v.old), None, "{s}");
    }
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
        let (new, on, _, _, availability) = rig.row_at(&copy_rel(s, SOUR_TIMES));
        assert_eq!(
            (on.as_str(), availability.as_str()),
            (v.album.as_str(), "indexed"),
            "{s}"
        );
        assert_ne!(new, v.old, "{s}");
        assert_eq!(rig.identity(&new), Some(placed_on(&v.e.new, 1)), "{s}");
        assert_eq!(
            rig.version_history(&v.old),
            (new.clone(), new.clone(), new.clone(), v.old.clone()),
            "{s}"
        );
        assert_eq!(rig.alias_of(&v.old), Some(new), "{s}");
        assert!(rig.in_recycle_bin(o_name), "{s}: O's file is recycled");
        assert_eq!(rig.availability(&v.old), "missing", "{s}");
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

    let (new, on, _, _, availability) = rig.row_at(&copy_rel(20, SOUR_TIMES));
    assert_eq!(
        (on.as_str(), availability.as_str()),
        (v.album.as_str(), "indexed"),
        "the download joined the copy"
    );
    assert_ne!(new, v.old);
    assert!(rig.in_recycle_bin(&v.o_name), "O's file is replaced");
    assert_eq!(rig.alias_of(&v.old), Some(new));
}

// The edition's version was asked for and the first source lacks it: it
// is still missing, so the next source is asked for it, even though the
// library holds the same recording in the person's version.
#[tokio::test]
async fn a_missing_version_is_asked_of_the_next_source() {
    let v = VersionCase::new(21, 1, false, "02 Sour Times.flac").await;
    let rig = &v.rig;
    v.ask("t-version").await;
    let mysterons = rig.landed("t-version", &v.e.new, 0, FIXTURES[0]);
    let report = v.land_directly("t-version", mysterons).await;
    assert_eq!(
        (report.result.clone(), report.local_only),
        (LandingResult::Imported { complete: false }, false),
        "{report:?}"
    );
    let sour =
        v.e.new
            .tracks
            .iter()
            .find(|track| track.title == TITLES[1])
            .expect("track on NEW");
    let missing: String = rig.scalar(
        "SELECT missing_positions FROM download_import_decisions \
             WHERE task_id = 't-version'",
    );
    assert_eq!(
        missing,
        format!("[[{},{}]]", sour.disc.max(1), sour.position)
    );
}

// Two tracks asked for, each brought by a different source: the second
// landing finds the first's track in the library, so the download is
// complete and the second source, which delivered, is not failed over.
#[tokio::test]
async fn asked_tracks_brought_by_two_sources_complete_the_download() {
    let e = editions(22, &fixture_lengths());
    let rig = join_rig(vec![e.new2.clone()]).await;
    let task_id = "t-two-sources";
    rig.ask(
        task_id,
        &e.group,
        &e.new2.title,
        "user",
        Some(&e.new2.id),
        None,
        &[],
    )
    .await;
    let plan: TaskPlan = serde_json::from_value(serde_json::json!({
        "release_mbid": e.new2.id,
        "album_wanted": [{"disc": 1, "track": 1}, {"disc": 1, "track": 2}],
    }))
    .expect("plan");
    rig.acquire
        .journal
        .run("test.plan", move |store| {
            store.set_task_plan(task_id, &plan, 1.0)
        })
        .await
        .expect("plan set");
    for (index, complete) in [(0, false), (1, true)] {
        let row = rig.task(task_id).await;
        let file = rig.landed(task_id, &e.new2, index, FIXTURES[index]);
        let landed = Landed {
            files: vec![Reported::path(file)],
            mounts: vec![rig.downloads.clone().into()],
        };
        let report = rig.acquire.landing.land(&row, None, landed, false).await;
        assert_eq!(
            report.result,
            LandingResult::Imported { complete },
            "{report:?}"
        );
    }
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
        let (track, _, _, _, availability) = rig.row_at(&copy_rel(s, &v.o_name));
        assert_eq!(
            (track.as_str(), availability.as_str()),
            (v.old.as_str(), "indexed")
        );
        let o = v.old.clone();
        assert_eq!(
            rig.version_history(&v.old),
            (o.clone(), o.clone(), o.clone(), o),
            "{s}"
        );
        assert_eq!(rig.alias_of(&v.old), None);
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
        &platform,
        &library,
        Arc::new(LibraryLanding::new(&library, every_priority(releases))),
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

    let rows: Vec<(String, String)> = acquire.db.db().rows(
        "SELECT id, status FROM download_tasks ORDER BY retry_count",
        [],
    );
    let (status, linked): (String, Option<String>) = acquire.db.db().row(
        "SELECT status, download_task_id FROM request_history WHERE musicbrainz_id_lower = ?1",
        rusqlite::params![GOOD_GROUP],
    );
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

const SINGLE_GROUP: &str = "6d0b7a1e-0000-4000-8000-000000000031";
const SINGLE_RELEASE: &str = "6d0b7a1e-0000-4000-8000-000000000032";
/// The single's one track, as `release` numbers it.
const SINGLE_RECORDING: &str = "6d0b7a1e-0000-4000-8000-000000000038";
const DUMMY_GROUP: &str = "6d0b7a1e-0000-4000-8000-000000000041";
const DUMMY_RELEASE: &str = "6d0b7a1e-0000-4000-8000-000000000051";
/// "Love Me" on the album, the fourth track as `release` numbers it.
const DUMMY_LOVE_ME: &str = "6d0b7a1e-0000-4000-8000-00000000004b";
const DUMMY_LOVE_ME_TRACK: &str = "6d0b7a1e-0000-4000-8000-00000000005b";
/// The album's first track, as `release` numbers it.
const DUMMY_MYSTERONS: &str = "6d0b7a1e-0000-4000-8000-000000000048";
const DUMMY_TITLES: [&str; 12] = [
    "Mysterons",
    "Sour Times",
    "Strangers",
    "Love Me",
    "Wandering Star",
    "It's a Fire",
    "Numb",
    "Roads",
    "Pedestal",
    "Biscuit",
    "Glory Box",
    "Hidden",
];

/// The single "Love Me" asked for as one track, with MusicBrainz knowing
/// the single and the album "Dummy" that also holds the song.
struct SingleRig {
    acquire: droppedneedle_acquire::AcquireSetup,
    music: PathBuf,
    downloads: PathBuf,
    _grounds: Grounds,
}

/// One landing for the single.
#[derive(Clone, Copy)]
struct SingleCase {
    /// The landed song's title tag.
    title: &'static str,
    /// Its album tag.
    album: &'static str,
    /// The person chose "Get it from the album".
    from_album: bool,
    /// The whole album landed, the song among its twelve files.
    whole_album: bool,
    /// The song carries the album's own MusicBrainz ids, as Picard tags it.
    album_ids: bool,
    /// With `album_ids`, its recording tag, when not "Love Me"'s.
    recording_tag: Option<&'static str>,
    /// With `album_ids`, its release track tag.
    release_track_tag: Option<&'static str>,
    /// A release group tag alone.
    group_tag: Option<&'static str>,
}

impl SingleCase {
    fn titled(title: &'static str, album: &'static str) -> Self {
        Self {
            title,
            album,
            from_album: false,
            whole_album: false,
            album_ids: false,
            recording_tag: None,
            release_track_tag: None,
            group_tag: None,
        }
    }
}

impl SingleRig {
    /// `single_off_ms` is how far the single's length is off the file's;
    /// AcoustID hears `heard` in every file when given.
    async fn new(single_off_ms: u64, heard: Option<&'static str>) -> Self {
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
            library.imports.fingerprints = Arc::new(HearsEverywhere(heard));
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
        let length = length_ms(&fixture("flac_full_01.flac")).map(|ms| ms + single_off_ms);
        let mut single = release(SINGLE_RELEASE, SINGLE_GROUP, &["Love Me"], &[length]);
        single.title = "Love Me".to_owned();
        assert_eq!(single.tracks[0].recording_id, SINGLE_RECORDING);
        let album = release(DUMMY_RELEASE, DUMMY_GROUP, &DUMMY_TITLES, &[None; 12]);
        assert_eq!(album.tracks[3].recording_id, DUMMY_LOVE_ME);
        let acquire = acquire_over(
            &platform,
            &library,
            Arc::new(LibraryLanding::new(
                &library,
                every_priority(Arc::new(Releases(vec![single, album]))),
            )),
        );
        Self {
            acquire,
            music,
            downloads: dir.join("downloads"),
            _grounds: (platform, db),
        }
    }

    /// Land `case` for a new track task `task_id`; answers its decisions.
    async fn land(&self, task_id: &str, case: SingleCase) -> Vec<(String, Option<String>)> {
        let folder = self.downloads.join(task_id);
        let song = land_file(
            &folder,
            &format!("04 - {}.flac", case.title),
            "flac_full_01.flac",
            case.title,
            4,
        );
        let mut files = vec![song.clone()];
        if case.whole_album {
            for (index, title) in DUMMY_TITLES.iter().enumerate().filter(|(i, _)| *i != 3) {
                let name = format!("{:02} - {title}.flac", index + 1);
                files.push(land_file(
                    &folder,
                    &name,
                    "flac_full_01.flac",
                    title,
                    index as u32 + 1,
                ));
            }
        }
        let mut edits = vec![TagEdit::new(TagField::Album, vec![case.album.to_owned()])];
        if case.album_ids {
            for (field, id) in [
                (
                    TagField::MusicBrainzRecordingId,
                    case.recording_tag.unwrap_or(DUMMY_LOVE_ME),
                ),
                (TagField::MusicBrainzReleaseId, DUMMY_RELEASE),
                (TagField::MusicBrainzReleaseGroupId, DUMMY_GROUP),
            ] {
                edits.push(TagEdit::new(field, vec![id.to_owned()]));
            }
            if let Some(id) = case.release_track_tag {
                edits.push(TagEdit::new(
                    TagField::MusicBrainzReleaseTrackId,
                    vec![id.to_owned()],
                ));
            }
        }
        if let Some(id) = case.group_tag {
            edits.push(TagEdit::new(
                TagField::MusicBrainzReleaseGroupId,
                vec![id.to_owned()],
            ));
        }
        save_tags(&song, &edits).expect("tags save");
        let task = NewTask {
            album_title: "Love Me".to_owned(),
            ..new_task(task_id, SINGLE_GROUP)
        };
        let id = task_id.to_owned();
        self.acquire
            .journal
            .run("test.seed", move |store| {
                store.insert_track_task(&task, SINGLE_RECORDING, 1.0)?;
                store.set_task_plan(
                    &id,
                    &TaskPlan {
                        album_version: case.from_album,
                        ..TaskPlan::default()
                    },
                    1.0,
                )
            })
            .await
            .expect("task");
        let row = self
            .acquire
            .journal
            .read_task(task_id)
            .await
            .unwrap()
            .unwrap();
        let landed = Landed {
            mounts: vec![self.downloads.clone().into()],
            files: files.into_iter().map(Reported::path).collect(),
        };
        self.acquire.landing.land(&row, None, landed, false).await;
        decisions(&self.acquire, task_id)
    }

    fn held(&self, task_id: &str) -> Vec<(String, String)> {
        self.acquire.db.db().rows(
            "SELECT state, path FROM acquire_held_files WHERE source_task_id = ?1",
            rusqlite::params![task_id],
        )
    }
}

fn held_as(code: &str) -> Vec<(String, Option<String>)> {
    vec![("held".to_owned(), Some(code.to_owned()))]
}

fn imported() -> Vec<(String, Option<String>)> {
    vec![("imported".to_owned(), None)]
}

/// A single stops as "only the album version" only on proof that the file
/// is the album's copy (its album tag alone is not; a placeholder album tag
/// is none, an unknown album or another song is a weak match). "Get it from
/// the album" takes only the song, and an album copy carrying the album's
/// recording id imports as the single when AcoustID agrees.
#[tokio::test]
async fn a_single_lands_as_the_song_only_on_proof() {
    let picard = SingleCase {
        album_ids: true,
        ..SingleCase::titled("Love Me", "Dummy")
    };
    // Each rig: how far the single's length is off the file's (5 s means
    // its length cannot vouch for the file against what AcoustID heard),
    // what AcoustID hears, then its landings in order with their decisions.
    #[allow(clippy::type_complexity)]
    let rigs: Vec<(u64, Option<&'static str>, Vec<(&str, SingleCase, _)>)> = vec![
        (
            0,
            None,
            vec![
                (
                    "t-unknown",
                    SingleCase::titled("Love Me", "Unknown Album"),
                    imported(),
                ),
                (
                    "t-singles",
                    SingleCase::titled("Love Me", "Singles 2020"),
                    held_as("weak_match"),
                ),
                // A malformed group tag proves nothing.
                (
                    "t-malformed",
                    SingleCase {
                        group_tag: Some("not-a-release-group"),
                        ..SingleCase::titled("Love Me", "Singles 2020")
                    },
                    held_as("weak_match"),
                ),
                (
                    "t-love-me-do",
                    SingleCase::titled("Love Me Do", "Dummy"),
                    held_as("weak_match"),
                ),
                (
                    "t-dummy",
                    SingleCase::titled("Love Me", "Dummy"),
                    held_as("album_version_only"),
                ),
            ],
        ),
        // "Get it from the album" on a whole-album landing: only the song
        // imports, and the other eleven are neither imported nor held.
        (
            0,
            None,
            vec![(
                "t-whole",
                SingleCase {
                    from_album: true,
                    whole_album: true,
                    ..SingleCase::titled("Love Me", "Dummy")
                },
                imported(),
            )],
        ),
        // A Picard-tagged album copy is held, unless the person chose the
        // album: then it imports as the single.
        (
            5_000,
            Some(DUMMY_LOVE_ME),
            vec![
                ("t-picard", picard, held_as("tag_mismatch")),
                (
                    "t-chosen",
                    SingleCase {
                        from_album: true,
                        ..picard
                    },
                    imported(),
                ),
            ],
        ),
        // A recording of the album that is another song, under this song's
        // title tag, is not the album's copy of it, even when heard.
        (
            5_000,
            Some(DUMMY_MYSTERONS),
            vec![(
                "t-false-title",
                SingleCase {
                    from_album: true,
                    album_ids: true,
                    recording_tag: Some(DUMMY_MYSTERONS),
                    ..SingleCase::titled("Love Me", "Dummy")
                },
                held_as("tag_mismatch"),
            )],
        ),
        // A retired recording id is not on the album; the file's release
        // track still places it there.
        (
            5_000,
            Some(DUMMY_LOVE_ME),
            vec![(
                "t-retired",
                SingleCase {
                    from_album: true,
                    album_ids: true,
                    recording_tag: Some("6d0b7a1e-0000-4000-8000-0000000000ff"),
                    release_track_tag: Some(DUMMY_LOVE_ME_TRACK),
                    ..SingleCase::titled("Love Me", "Dummy")
                },
                imported(),
            )],
        ),
    ];
    for (off, heard, cases) in rigs {
        let rig = SingleRig::new(off, heard).await;
        for (task, case, expected) in cases {
            assert_eq!(rig.land(task, case).await, expected, "{task}");
            match task {
                "t-unknown" => {
                    assert_eq!(files_under(&rig.music, "flac").len(), 1, "{task}");
                }
                "t-whole" => {
                    assert_eq!(files_under(&rig.music, "flac").len(), 1, "{task}");
                    assert!(rig.held(task).is_empty(), "{task}");
                }
                "t-chosen" => {
                    let recording: String = rig.acquire.db.db().one(
                        "SELECT i.recording_mbid FROM local_track_external_identities i \
                         JOIN local_tracks t ON t.id = i.local_track_id \
                         WHERE t.availability = 'indexed'",
                        [],
                    );
                    assert_eq!(
                        recording, SINGLE_RECORDING,
                        "{task}: imported as the single"
                    );
                }
                _ => {}
            }
        }
    }

    // The request's button reads the held file even when the landing's
    // decision was never recorded.
    let rig = SingleRig::new(0, None).await;
    rig.land("t-dummy", SingleCase::titled("Love Me", "Dummy"))
        .await;
    rig.acquire
        .db
        .db()
        .run_sql("DELETE FROM download_import_decisions WHERE task_id = 't-dummy'")
        .expect("decision removed");
    assert!(
        rig.acquire
            .journal
            .read_album_version_only("t-dummy")
            .await
            .expect("read"),
        "the held file answers without its decision"
    );
}

/// Once the song came in from its album, the copies held as "only the
/// album version" are discarded, but never one in an entry a person is
/// acting on.
#[tokio::test]
async fn a_claimed_album_version_hold_is_left_alone() {
    let rig = SingleRig::new(0, None).await;
    assert_eq!(
        rig.land("t-first", SingleCase::titled("Love Me", "Dummy"))
            .await,
        held_as("album_version_only")
    );
    rig.acquire
        .db
        .db()
        .run_sql(
            "UPDATE acquire_held_entries SET state = 'placing' WHERE id IN \
             (SELECT entry_id FROM acquire_held_files WHERE source_task_id = 't-first')",
        )
        .expect("claimed");
    let chosen = SingleCase {
        from_album: true,
        ..SingleCase::titled("Love Me", "Dummy")
    };
    assert_eq!(rig.land("t-second", chosen).await, imported());
    let held = rig.held("t-first");
    let held_dir = &rig.acquire.held.folders().held;
    assert_eq!(held.len(), 1);
    assert_eq!(held[0].0, "held");
    assert!(held_dir.join(&held[0].1).is_file(), "the copy stays");
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
    // MusicBrainz lists the song on the album too, which shows the peers'
    // file is the album's copy.
    let album = release(DUMMY_RELEASE, DUMMY_GROUP, &DUMMY_TITLES, &[None; 12]);
    let releases: Arc<dyn ReleaseSource> = Arc::new(Releases(vec![single, album]));
    let acquire = acquire_over(
        &platform,
        &library,
        Arc::new(LibraryLanding::new(&library, every_priority(releases))),
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
    let attempts: i64 = acquire.db.db().one(
        "SELECT COUNT(*) FROM download_attempts WHERE task_id = ?1",
        rusqlite::params![&first],
    );
    assert_eq!(attempts, 1, "no other source was tried");
    assert_eq!(task.status, TaskStatus::Failed);
    assert_eq!(
        task.error_message.as_deref(),
        Some("Held for review: Only the album version was found.")
    );
    assert_eq!(
        decisions(&acquire, &first),
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
    assert_eq!(
        history.items[0].status,
        droppedneedle_acquire::requests::ledger::RequestStatus::Failed
    );
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
    let second: String = acquire.db.db().one(
        "SELECT download_task_id FROM request_history WHERE musicbrainz_id_lower = ?1",
        rusqlite::params![&request_key],
    );
    assert_ne!(second, first);
    let task = settle(second).await;
    assert_eq!(
        task.status,
        TaskStatus::Completed,
        "{:?}",
        task.error_message
    );
    assert_eq!(files_under(&music, "flac").len(), 1);
    let recording: String = acquire.db.db().one(
        "SELECT i.recording_mbid FROM local_track_external_identities i \
         JOIN local_tracks t ON t.id = i.local_track_id WHERE t.availability = 'indexed'",
        [],
    );
    assert_eq!(recording, RECORDING, "imported as the requested song");
    let held: Vec<(String, String)> = acquire.db.db().rows(
        "SELECT state, path FROM acquire_held_files WHERE source_task_id = ?1",
        rusqlite::params![&first],
    );
    assert!(!held.is_empty(), "the first download held its file");
    let held_dir = &acquire.held.folders().held;
    assert!(
        held.iter()
            .all(|(state, path)| state == "discarded" && !held_dir.join(path).exists()),
        "the album version hold is resolved: {held:?}"
    );
    let status: String = acquire.db.db().one(
        "SELECT status FROM request_history WHERE musicbrainz_id_lower = ?1",
        rusqlite::params![&request_key],
    );
    assert_eq!(status, "imported");
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
/// folder and what it runs on.
async fn land_swapped(
    at: SwapAt,
    other_artist: bool,
) -> (
    droppedneedle_acquire::AcquireSetup,
    PathBuf,
    PathBuf,
    Grounds,
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
        edition_busy: false,
        busy_once: Default::default(),
    });
    let acquire = acquire_over(&platform, &library, port);
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
    (acquire, music, secret, (platform, db))
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
    fn identify<'a>(
        &'a self,
        _tracks: &'a [LocalTrackFacts],
        _priority: RequestPriority,
    ) -> BoxFuture<'a, Result<AudioMatches, AcoustIdUnavailable>> {
        Box::pin(async { Ok(AudioMatches::default()) })
    }

    fn identify_files<'a>(
        &'a self,
        files: &'a [(String, PathBuf)],
        _priority: RequestPriority,
    ) -> BoxFuture<'a, HeardFiles> {
        Box::pin(async move {
            Ok(match &self.0 {
                Some(recording) => files
                    .iter()
                    .map(|(id, _)| (id.clone(), vec![recording.clone()]))
                    .collect(),
                None => HashMap::new(),
            })
        })
    }
}

/// The real library port, whose first `changes` imports find the library
/// changed under them (a destination taken between the plan and the
/// publish), and which runs `after_import` once an import is done. With
/// `edition_busy`, the album's chosen edition does not read. With
/// `busy_once`, the first import runs while that database's only reader
/// is held, so its reads answer busy.
struct Shaky {
    inner: Arc<dyn LandingLibrary>,
    changes: std::sync::atomic::AtomicUsize,
    after_import: Option<Swap>,
    edition_busy: bool,
    busy_once: std::sync::Mutex<Option<droppedneedle_kernel::db::Db>>,
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

    fn chosen_edition<'a>(
        &'a self,
        group: &'a str,
    ) -> BoxFuture<'a, Result<Option<String>, DbError>> {
        if self.edition_busy {
            return Box::pin(async {
                Err(DbError::Busy {
                    operation: "acquire.edition.chosen".to_owned(),
                })
            });
        }
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
    ) -> BoxFuture<'a, Result<Vec<OwnedCopy>, DbError>> {
        self.inner
            .owned_copies(group, album, release, release_track, recording)
    }

    fn version_row<'a>(
        &'a self,
        track_id: &'a str,
        planned: &'a PlannedAlbum,
        release: &'a Release,
    ) -> BoxFuture<'a, Result<Option<VersionRow>, DbError>> {
        self.inner.version_row(track_id, planned, release)
    }

    fn fingerprints(
        &self,
        files: Vec<(String, PathBuf)>,
        priority: RequestPriority,
    ) -> BoxFuture<'_, HeardFiles> {
        self.inner.fingerprints(files, priority)
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
        priority: RequestPriority,
    ) -> BoxFuture<'_, HeardFiles> {
        self.inner.fingerprint_releases(files, priority)
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
        let busy = self.busy_once.lock().expect("busy flag").take();
        if let Some(db) = busy {
            return Box::pin(async move {
                let (entered, inside) = std::sync::mpsc::channel();
                let (let_go, release) = std::sync::mpsc::channel::<()>();
                let held = std::thread::spawn(move || {
                    db.read_blocking("test.held", |_| {
                        entered.send(()).expect("test waits");
                        release.recv().expect("test lets go");
                        Ok::<_, DbError>(())
                    })
                });
                inside.recv().expect("reader held");
                let answer = self.inner.import(request).await;
                let_go.send(()).expect("reader waits");
                held.join().expect("reader joins").expect("held read");
                answer
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
    /// An album with no match whose "Mysterons" from this fixture names
    /// the group and whose "Sour Times" names another: it owns nothing.
    Mixed(&'static str),
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
    /// AcoustID cannot be reached.
    down: bool,
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
    /// The copy's match is a best guess, which tagging would not touch.
    flagged: bool,
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
    /// Re-check on a file held because AcoustID heard another recording.
    HeldRecheck,
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
        library.imports.fingerprints = if row.down {
            Outage::new(AcoustIdUnavailable::Unreachable)
        } else {
            Arc::new(Hears(
                row.heard
                    .map(|right| if right { recording } else { mb(s, 3, 9) }),
            ))
        };
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
            edition_busy: false,
            busy_once: Default::default(),
        });
        let acquire = acquire_over(&platform, &library, port);
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
            _grounds: (platform, db),
        };
        let sour = copy_rel(s, SOUR_TIMES);
        let source = if row.flagged { "automatic" } else { "manual" };
        let (copy, old) = match row.have {
            Have::Nothing => (None, None),
            Have::Copy(fixture) => {
                let mysterons = copy_rel(s, &format!("0101 Mysterons.{}", ext(fixture)));
                rig.plant_from(&mysterons, fixture, 0, &e.new);
                rig.plant(&sour, 1, &e.new);
                rig.scan().await;
                let old = rig.track_at(&mysterons);
                let album = rig.album_of(&old);
                rig.set_edition(&album, &e.group, &e.new.id, source);
                rig.place(&old, &e.new, 0, source);
                rig.place(&rig.track_at(&sour), &e.new, 1, source);
                (Some(album), Some(old))
            }
            Have::Mixed(fixture) => {
                let mysterons = copy_rel(s, &format!("0101 Mysterons.{}", ext(fixture)));
                rig.plant_from(&mysterons, fixture, 0, &e.new);
                rig.plant(&sour, 1, &e.new);
                rig.scan().await;
                let old = rig.track_at(&mysterons);
                let album = rig.album_of(&old);
                rig.sql(&format!(
                    "DELETE FROM local_album_external_identities WHERE local_album_id = '{album}'; \
                     DELETE FROM library_album_match_state WHERE local_album_id = '{album}'; \
                     UPDATE local_tracks SET embedded_release_group_mbid = '{}' \
                     WHERE id = '{}';",
                    e.other.release_group_id,
                    rig.track_at(&sour)
                ));
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
                rig.set_edition(&album, &e.group, &e.new.id, source);
                rig.place(&rig.track_at(&sour), &e.new, 1, source);
                rig.listen(&old);
                (Some(album), Some(old))
            }
        };
        if row.flagged
            && let Some(album) = &copy
        {
            rig.sql(&format!(
                "INSERT INTO library_album_match_state (local_album_id, state, reason_code, \
                 candidates_json, updated_at) VALUES ('{album}', 'unconfirmed', 'WEAK_MATCH', \
                 '[]', 1);"
            ));
        }
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
    fn mysterons(&self) -> Vec<(String, String)> {
        self.rig.acquire.db.db().rows(
            "SELECT id, local_album_id FROM local_tracks WHERE availability = 'indexed' \
             AND title = 'Mysterons'",
            [],
        )
    }

    /// Whether an old "Mysterons" went to the recycle bin.
    fn old_recycled(&self) -> bool {
        let bin = self.rig.music.join(".recycle");
        files_under(&bin, "flac")
            .into_iter()
            .chain(files_under(&bin, "mp3"))
            .any(|path| path.to_string_lossy().contains("Mysterons"))
    }

    /// Run `row` through `door` and say what became of the file.
    async fn run(&self, row: &Row, door: Door) -> Seen {
        let before = self.mysterons();
        let file = self.rig.landed("t-intake", &self.e.new, 0, row.file);
        let said = match door {
            Door::Place => self.through_place(row, file).await,
            Door::Landing => self.through_landing(row, file).await,
            Door::DropMatch => self.through_drop(file).await,
            Door::HeldImport | Door::HeldRecheck => self.through_held(row, file, door).await,
        };
        if let Some(seen) = said {
            return seen;
        }
        let after = self.mysterons();
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
        let last = decisions(&self.rig.acquire, "t-intake").pop();
        if let Some((outcome, Some(code))) = &last
            && outcome == "deferred"
        {
            return Some(Seen::Deferred(Reason::from_stored(code)));
        }
        let held: Vec<String> = self.rig.acquire.db.db().column(
            "SELECT reason FROM acquire_held_files WHERE source_task_id = 't-intake' \
             AND state = 'held'",
            [],
        );
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
        let (state, reason): (String, String) = self.rig.acquire.db.db().row(
            "SELECT state, reason FROM acquire_held_files WHERE entry_id = ?1",
            rusqlite::params![entry],
        );
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

    async fn through_held(&self, row: &Row, file: PathBuf, door: Door) -> Option<Seen> {
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
        // The file's length as its header gives it, and the track's, as a
        // hold records them.
        let lengths = (
            length_ms(&copy).map(|ms| ms as f64 / 1000.0),
            track.length_ms.map(|ms| ms as f64 / 1000.0),
        );
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
                     disc_number, reason, source, source_task_id, state, created_at, \
                     duration_seconds, expected_duration_seconds) \
                     VALUES (?1, 'held', ?2, ?2, ?3, ?4, ?5, 1, 1, ?6, 'soulseek', 't-intake', \
                     'held', 1, ?7, ?8)",
                    rusqlite::params![
                        entry,
                        path,
                        release,
                        release_track,
                        recording,
                        reason,
                        lengths.0,
                        lengths.1
                    ],
                )?;
                Ok(entry)
            })
            .await
            .expect("held row");
        let viewer = HeldViewer {
            user_id: "u-ada".to_owned(),
            admin: false,
        };
        let held = &self.rig.acquire.held;
        let acted = if door == Door::HeldRecheck {
            held.recheck(&viewer, entry, None, "u-ada").await
        } else {
            held.import(&viewer, entry, None, "u-ada").await
        };
        if acted.is_ok_and(|acted| acted.held.is_empty()) {
            assert!(!copy.exists(), "an imported held copy is removed");
            return None;
        }
        assert!(copy.is_file(), "a file that stays held is kept");
        let still: Vec<String> = self.rig.acquire.db.db().column(
            "SELECT reason FROM acquire_held_files WHERE path = ?1 AND state = 'held'",
            rusqlite::params![&name],
        );
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
        down: false,
        length_agrees: false,
        taken: false,
        changes: 0,
        gate: Auto,
        held_for: Reason::TagMismatch,
        flagged: false,
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
    // A person's Match keeps your copy and holds the file saying why.
    rows.extend(by_gate(
        "AcoustID cannot be reached",
        Row {
            held_for: Reason::FingerprintMismatch,
            down: true,
            ..owned(MP3, None)
        },
        [
            Seen::Deferred(Reason::AcoustidUnavailable),
            Seen::Held(Reason::AcoustidUnavailable),
            joined,
        ],
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
                let (played, _, starred) = case.rig.history(old);
                assert_ne!(&played, old, "{name}, through {door:?}: the play moved");
                assert!(!starred, "{name}, through {door:?}: the star moved");
            }
        }
    }
}

/// A row's doors for its gate.
fn doors_of(gate: Gate) -> &'static [Door] {
    match gate {
        Gate::Automatic => &[Door::Place, Door::Landing],
        Gate::PersonChoseAlbum => &[Door::Place, Door::DropMatch],
        Gate::PersonChoseFile => &[Door::Place, Door::HeldImport],
        Gate::HeldRetry => &[],
    }
}

/// A better "Mysterons" for an album tagging would not touch: the copy's
/// match is a best guess (its file replaced in place, or "Replace tracks
/// you have in another version"), or it has no match and its files' tags
/// disagree. Downloaded or uploaded, the new file is held and the old one
/// stays; a person importing that held file passes the check, so a better
/// file replaces the old copy and one no better stays held.
#[tokio::test]
async fn a_flagged_album_takes_an_upgrade_only_from_a_person() {
    use Gate::{Automatic as Auto, PersonChoseAlbum as Album};
    let base = Row {
        have: Have::Copy("mp3_full_01.mp3"),
        file: "flac_full_01.flac",
        upgrade: true,
        heard: Some(true),
        down: false,
        length_agrees: false,
        taken: false,
        changes: 0,
        gate: Auto,
        held_for: Reason::TagMismatch,
        flagged: true,
        expect: Seen::Held(Reason::AlbumNeedsCheck),
    };
    let held_rows = [
        ("flagged copy, downloaded", base),
        (
            "flagged copy, uploaded",
            Row {
                gate: Album,
                ..base
            },
        ),
        (
            "flagged version",
            Row {
                have: Have::Version,
                upgrade: false,
                ..base
            },
        ),
        (
            "tags disagree, downloaded",
            Row {
                have: Have::Mixed("mp3_full_01.mp3"),
                flagged: false,
                ..base
            },
        ),
        (
            "tags disagree, uploaded",
            Row {
                have: Have::Mixed("mp3_full_01.mp3"),
                flagged: false,
                gate: Album,
                ..base
            },
        ),
    ];
    for (name, row) in held_rows {
        for door in doors_of(row.gate) {
            let case = IntakeCase::new(&row).await;
            let seen = case.run(&row, *door).await;
            assert_eq!(seen, row.expect, "{name}, through {door:?}");
            let old = case.old.as_deref().expect("an old copy");
            let kept = case.mysterons();
            assert!(
                kept.iter().any(|(track, _)| track == old),
                "{name}, through {door:?}: the old copy stays indexed"
            );
            assert!(
                !case.old_recycled(),
                "{name}, through {door:?}: nothing went to the recycle bin"
            );
        }
    }

    let person = Row {
        gate: Gate::PersonChoseFile,
        held_for: Reason::AlbumNeedsCheck,
        expect: Seen::Imported { joined: true },
        ..base
    };
    let person_rows = [
        ("person imports: flagged copy", person),
        (
            "person imports: flagged version",
            Row {
                have: Have::Version,
                upgrade: false,
                ..person
            },
        ),
        (
            "person imports: flagged copy, no better",
            Row {
                have: Have::Copy("flac_full_01.flac"),
                expect: Seen::Held(Reason::NoBetterThanYours),
                ..person
            },
        ),
        (
            "person imports: tags disagree",
            Row {
                have: Have::Mixed("mp3_full_01.mp3"),
                flagged: false,
                ..person
            },
        ),
    ];
    for (name, row) in person_rows {
        for door in doors_of(row.gate) {
            let case = IntakeCase::new(&row).await;
            let seen = case.run(&row, *door).await;
            let replaced = matches!(row.expect, Seen::Imported { .. });
            if matches!(row.have, Have::Mixed(_)) {
                assert!(
                    matches!(seen, Seen::Imported { .. }),
                    "{name}, through {door:?}: {seen:?}"
                );
            } else {
                assert_eq!(seen, row.expect, "{name}, through {door:?}");
            }
            assert_eq!(
                case.old_recycled(),
                replaced,
                "{name}, through {door:?}: the old copy is in the recycle bin"
            );
        }
    }
}

// Only a measured length outranks a fingerprint. An MP3 with no frame
// count has only an estimated length: inside 3 s of the track's, it still
// does not vouch for a file AcoustID hears as another recording, so the
// app's own choice holds the file and a re-check keeps it held.
#[tokio::test]
async fn an_estimated_length_never_outranks_a_fingerprint() {
    let row = Row {
        have: Have::Nothing,
        file: "mp3_vbr_no_xing.mp3",
        upgrade: false,
        heard: Some(false),
        down: false,
        length_agrees: true,
        taken: false,
        changes: 0,
        gate: Gate::Automatic,
        held_for: Reason::FingerprintMismatch,
        flagged: false,
        expect: Seen::Held(Reason::FingerprintMismatch),
    };
    for door in [Door::Place, Door::HeldRecheck] {
        let case = IntakeCase::new(&row).await;
        let seen = case.run(&row, door).await;
        assert_eq!(seen, row.expect, "through {door:?}");
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
    let albums: Vec<(String, String)> = rig.acquire.db.db().rows(
        "SELECT i.release_group_mbid, t.local_album_id FROM local_tracks t \
         JOIN local_track_external_identities x ON x.local_track_id = t.id \
         JOIN local_album_external_identities i ON i.local_album_id = t.local_album_id \
         WHERE t.availability = 'indexed' AND t.title = 'Mysterons' ORDER BY i.release_group_mbid",
        [],
    );
    assert_eq!(albums.len(), 2, "{albums:?}");
    assert_eq!(
        albums[0],
        (e.group.clone(), copy.clone()),
        "joined the copy"
    );
    assert_eq!(albums[1].0, f.group, "filed with its own album");
    assert_ne!(albums[1].1, copy);
}

/// One task as its owner's list shows it.
async fn task_now(
    queue: &droppedneedle_acquire::downloads::queue::DownloadQueue,
    viewer: &HeldViewer,
    id: &str,
) -> droppedneedle_acquire::downloads::queue::TaskView {
    let filter = droppedneedle_acquire::downloads::queue_rows::ListFilter {
        status: None,
        release_group_mbid: None,
        page: 1,
        page_size: 50,
    };
    queue
        .list(viewer, &filter)
        .await
        .expect("tasks read")
        .into_iter()
        .find(|task| task.row.task.id == id)
        .expect("task")
}
