//! A manual album search: start it, read the ranked candidates, pick one
//! that is not the top pick, and the worker fetches exactly that folder
//! without searching again. A folder outside the quality band is listed,
//! picked only with a confirm, and then kept, for that attempt only. Runs
//! the real search service, worker pass and slskd source against the
//! loopback mock with a fixed MusicBrainz tracklist.

use crate::common::Probe;
use std::sync::Arc;

use droppedneedle_acquire::db::AcquireDb;
use droppedneedle_acquire::downloads::journal::Journal;
use droppedneedle_acquire::downloads::sources::SourceHandle;
use droppedneedle_acquire::downloads::store::{NewTask, TaskDetails};
use droppedneedle_acquire::downloads::worker::DownloadWorker;
use droppedneedle_acquire::flows::stores::UpgradeWorklist;
use droppedneedle_acquire::reason::Reason;
use droppedneedle_acquire::requests::quota::QuotaLedger;
use droppedneedle_acquire::search_jobs::candidates::{
    CandidateNoteCode, CandidateRefusal, CandidateTier,
};
use droppedneedle_acquire::search_jobs::store::{
    JobPayload, JobStatus, JobStore, NewJob, PickRefusal, PickedTask, ReviewOutcome,
};
use droppedneedle_acquire::search_jobs::{AlbumSearch, SearchJobError, SearchJobs, StartOutcome};
use droppedneedle_acquire::target::lookup::{AlbumRelease, AlbumTrack};
use droppedneedle_auth::session::store::SessionKind;
use droppedneedle_auth::{Principal, Role};
use droppedneedle_kernel::events::model::names;
use droppedneedle_kernel::events::{EventHub, Subscription};
use droppedneedle_testkit::slskd::MockSlskd;
use serde_json::{Value, json};

use crate::common::acquire::{FixedAlbums, review_off_in, save_slskd, worker_over, worker_with};
use crate::common::auth::test_principal;
use crate::common::bundles::quiet_ticks;

const RELEASE: &str = "22222222-2222-4222-8222-222222222222";
const GROUP: &str = "33333333-3333-4333-8333-333333333333";
const TITLES: [(&str, f64); 4] = [
    ("Safe From Harm", 331.0),
    ("One Love", 288.0),
    ("Blue Lines", 261.0),
    ("Daydreaming", 249.0),
];

fn peer(username: &str, folder: &str, tracks: &[usize]) -> Value {
    let files: Vec<Value> = tracks
        .iter()
        .map(|&index| {
            let (title, length) = TITLES[index];
            json!({"filename": format!("{folder}\\0{} {title}.flac", index + 1),
                   "size": 30_000_000, "extension": "flac", "bitDepth": 16,
                   "sampleRate": 44100, "length": length})
        })
        .collect();
    json!({"username": username, "hasFreeUploadSlot": true, "uploadSpeed": 1_000_000,
           "queueLength": 0, "fileCount": files.len(), "lockedFileCount": 0,
           "files": files, "lockedFiles": [], "token": 1})
}

fn album() -> AlbumRelease {
    AlbumRelease {
        id: RELEASE.to_owned(),
        title: "Blue Lines".to_owned(),
        release_group_mbid: GROUP.to_owned(),
        artist: "Massive Attack".to_owned(),
        various_artists: false,
        year: Some(1991),
        tracks: TITLES
            .iter()
            .enumerate()
            .map(|(index, (title, length))| AlbumTrack {
                disc: 1,
                position: u32::try_from(index + 1).expect("small"),
                title: (*title).to_owned(),
                duration_seconds: Some(*length),
                recording_mbid: format!("aaaaaaaa-0000-4000-8000-00000000000{index}"),
                release_track_mbid: format!("bbbbbbbb-0000-4000-8000-00000000000{index}"),
            })
            .collect(),
    }
}

struct Rig {
    journal: Arc<Journal>,
    staging: crate::common::ScratchDir,
    worker: Arc<DownloadWorker>,
    jobs: Arc<SearchJobs>,
    events: EventHub,
}

/// The search service and a worker over the slskd mock, with a fixed
/// MusicBrainz tracklist. `review` leaves the look before an automatic
/// grab on, as saved by default.
async fn rig(mock: &MockSlskd, review: bool) -> Rig {
    let db = AcquireDb::scratch().expect("scratch db");
    db.add_user("u1", "U", "user").await.expect("user seeds");
    db.add_user("u2", "V", "user").await.expect("user seeds");
    let staging = crate::common::ScratchDir::new("search-jobs-staging");
    let journal = Arc::new(Journal::new(db.clone(), quiet_ticks()));
    let mut albums = FixedAlbums::default();
    albums.releases.insert(RELEASE.to_owned(), album());
    let settings = droppedneedle_acquire::settings::scratch_settings(&staging.join("settings"))
        .expect("settings open");
    save_slskd(&settings, &mock.base_url());
    if !review {
        review_off_in(&settings);
    }
    let events = EventHub::new();
    let (worker, targets) = worker_over(
        &db,
        journal.clone(),
        &settings,
        staging.join("unused-mount"),
        Arc::new(albums),
        None,
        events.clone(),
    )
    .await;
    let jobs = Arc::new(SearchJobs::new(
        JobStore::new(db.clone()),
        worker.clone(),
        targets,
        Arc::new(QuotaLedger::unlimited(db.clone())),
        events.clone(),
    ));
    Rig {
        journal,
        staging,
        worker,
        jobs,
        events,
    }
}

fn principal(user_id: &str) -> Principal {
    test_principal(user_id, Role::User, "sess-test", SessionKind::Standard)
}

/// Wait for the look a pass started: it tells the owner once it parks or
/// takes, and leaves the running set in the same step on this runtime.
async fn look_finishes(updates: &mut Subscription, worker: &DownloadWorker) {
    tokio::time::timeout(crate::common::PATIENCE, async {
        while updates.next().await.expect("the hub stays open").name() != names::SEARCH_JOB_UPDATED
        {
        }
    })
    .await
    .expect("the look tells its owner");
    assert!(!worker.reviews_running(), "the look finished");
}

#[tokio::test]
async fn manual_search_ranks_folders_and_fetches_the_picked_one() {
    let mock = MockSlskd::start().await.expect("mock starts");
    mock.script_responses(vec![
        peer("halfway", "@@music\\Massive Attack\\Blue Lines", &[0, 1]),
        peer("complete", "@@music\\MA\\Blue Lines (1991)", &[0, 1, 2, 3]),
    ]);

    let Rig {
        journal,
        staging: _staging,
        worker,
        jobs,
        events,
    } = rig(&mock, false).await;
    let owner = principal("u1");
    let mut updates = events.subscribe(owner.user_id());
    let stranger = principal("u2");

    let started = jobs
        .start(
            &owner,
            AlbumSearch {
                artist_name: "Massive Attack".to_owned(),
                album_title: "Blue Lines".to_owned(),
                year: Some(1991),
                release_group_mbid: Some(GROUP.to_owned()),
                release_mbid: Some(RELEASE.to_owned()),
            },
        )
        .await
        .expect("search starts");
    let StartOutcome::Searching(job_id) = started else {
        panic!("nothing was searched: {started:?}");
    };

    // The job tells its owner about every change of status.
    let view = tokio::time::timeout(crate::common::PATIENCE, async {
        loop {
            let view = jobs.get(&owner, &job_id).await.expect("job reads");
            if view.row.status != JobStatus::Searching {
                break view;
            }
            updates.next().await.expect("the hub stays open");
        }
    })
    .await
    .expect("the search settles");
    assert_eq!(view.row.status, JobStatus::Completed);
    let payload = view.payload.expect("candidates decode");
    assert_eq!(payload.tracks_total, Some(4));
    let ranked: Vec<_> = payload
        .candidates
        .iter()
        .map(|c| (c.view.candidate_index, c.view.username.clone(), c.view.tier))
        .collect();
    assert_eq!(
        ranked,
        [
            (0, Some("complete".to_owned()), CandidateTier::Recommended),
            (1, Some("halfway".to_owned()), CandidateTier::Possible),
        ]
    );
    assert_eq!(
        payload.candidates[1].view.note.code,
        CandidateNoteCode::Incomplete
    );
    assert!(matches!(
        jobs.get(&stranger, &job_id).await,
        Err(SearchJobError::Forbidden)
    ));

    // Pick the second folder; the worker fetches it without searching.
    let searches_before = mock.search_texts().len();
    let task_id = jobs
        .pick(&owner, &job_id, 1, false)
        .await
        .expect("pick starts a download");
    assert!(matches!(
        jobs.pick(&owner, &job_id, 0, false).await,
        Err(SearchJobError::Conflict(_))
    ));
    worker.run_once(1).await;
    assert_eq!(mock.search_texts().len(), searches_before);
    let handles = journal
        .read_source_handles(
            &task_id,
            &droppedneedle_acquire::downloads::SourceTag::Soulseek,
        )
        .await
        .expect("handles read");
    assert_eq!(handles.len(), 1, "one enqueue: {handles:?}");
    let handle: SourceHandle = serde_json::from_str(&handles[0]).expect("handle decodes");
    assert_eq!(handle.username, "halfway");
    assert_eq!(handle.filenames.len(), 2);

    let task = journal
        .read_task(&task_id)
        .await
        .expect("task reads")
        .expect("task exists");
    assert_eq!(task.search_job_id.as_deref(), Some(job_id.as_str()));
    assert_eq!(task.candidate_index, Some(1));
    assert_eq!(task.release_group_mbid, GROUP);
    let plan_task = task_id.clone();
    let plan = journal
        .read("test.plan", move |store| store.task_plan(&plan_task))
        .await
        .expect("plan reads");
    assert_eq!(plan.release_mbid.as_deref(), Some(RELEASE));
    assert_eq!(plan.expected_tracks.len(), 4);

    let view = jobs.get(&owner, &job_id).await.expect("job reads");
    assert_eq!(view.row.status, JobStatus::Matched);
    assert_eq!(view.task_id.as_deref(), Some(task_id.as_str()));
    assert!(matches!(
        jobs.cancel(&owner, &job_id).await,
        Err(SearchJobError::Conflict(_))
    ));
}

/// Search review: an automatic download with no recommended candidate
/// waits on its search instead of grabbing, stays parked across passes,
/// and the owner's pick resumes that same download.
#[tokio::test]
async fn automatic_download_waits_for_a_pick_when_unsure() {
    let mock = MockSlskd::start().await.expect("mock starts");
    mock.script_responses(vec![peer(
        "halfway",
        "@@music\\Massive Attack\\Blue Lines",
        &[0, 1],
    )]);
    let rig = rig(&mock, true).await;
    let task_id = "0123456789abcdef0123456789abcdef";
    rig.journal
        .run("test.seed", move |store| {
            store.insert_task(
                &NewTask {
                    id: task_id.to_owned(),
                    user_id: "u1".to_owned(),
                    artist_name: "Massive Attack".to_owned(),
                    album_title: "Blue Lines".to_owned(),
                    release_group_mbid: GROUP.to_owned(),
                    origin: "user".to_owned(),
                    retry_count: 0,
                },
                1_000.0,
            )?;
            store.set_task_details(
                task_id,
                &TaskDetails {
                    release_mbid: Some(RELEASE.to_owned()),
                    year: Some(1991),
                    ..TaskDetails::default()
                },
                1_000.0,
            )
        })
        .await
        .expect("task seeds");

    let mut updates = rig.events.subscribe("u1");
    rig.worker.run_once(1).await;
    // The look runs off the pass.
    look_finishes(&mut updates, &rig.worker).await;
    let searched = mock.search_texts().len();
    assert!(searched > 0, "the worker searched before grabbing");
    rig.worker.run_once(2).await;
    let task = rig
        .journal
        .read_task(task_id)
        .await
        .expect("task reads")
        .expect("task exists");
    assert_eq!(task.status.as_str(), "queued");
    assert_eq!(task.candidate_index, None);
    let job_id = task.search_job_id.expect("parked on its search");
    assert_eq!(
        mock.search_texts().len(),
        searched,
        "a parked task is not searched again"
    );
    assert!(
        rig.journal
            .read_source_handles(
                task_id,
                &droppedneedle_acquire::downloads::SourceTag::Soulseek
            )
            .await
            .expect("handles read")
            .is_empty()
    );

    let owner = principal("u1");
    let view = rig.jobs.get(&owner, &job_id).await.expect("job reads");
    assert_eq!(view.row.status, JobStatus::Completed);
    let payload = view.payload.expect("candidates decode");
    assert_eq!(payload.candidates.len(), 1);
    assert_eq!(payload.candidates[0].view.tier, CandidateTier::Possible);
    assert_eq!(payload.release_mbid.as_deref(), Some(RELEASE));

    let picked = rig
        .jobs
        .pick(&owner, &job_id, 0, false)
        .await
        .expect("pick resumes");
    assert_eq!(picked, task_id, "the parked download resumes");
    rig.worker.run_once(3).await;
    let handles = rig
        .journal
        .read_source_handles(
            task_id,
            &droppedneedle_acquire::downloads::SourceTag::Soulseek,
        )
        .await
        .expect("handles read");
    assert_eq!(handles.len(), 1, "one enqueue: {handles:?}");
    let handle: SourceHandle = serde_json::from_str(&handles[0]).expect("handle decodes");
    assert_eq!(handle.username, "halfway");
    drop(rig.staging);
}

async fn seed_album_task(journal: &Journal, task_id: &'static str, origin: &'static str) {
    journal
        .run("test.seed", move |store| {
            store.insert_task(
                &NewTask {
                    id: task_id.to_owned(),
                    user_id: "u1".to_owned(),
                    artist_name: "Massive Attack".to_owned(),
                    album_title: "Blue Lines".to_owned(),
                    release_group_mbid: GROUP.to_owned(),
                    origin: origin.to_owned(),
                    retry_count: 0,
                },
                1_000.0,
            )?;
            store.set_task_details(
                task_id,
                &TaskDetails {
                    release_mbid: Some(RELEASE.to_owned()),
                    year: Some(1991),
                    ..TaskDetails::default()
                },
                1_000.0,
            )
        })
        .await
        .expect("task seeds");
}

/// Park `task_id` on a recorded search `job_id` with nothing picked.
async fn park_on_search(journal: &Journal, task_id: &'static str, job_id: &str) {
    let recorded = JobStore::new(journal.db().clone())
        .record_review(
            NewJob {
                id: job_id.to_owned(),
                user_id: "u1".to_owned(),
                artist_name: "Massive Attack".to_owned(),
                album_title: "Blue Lines".to_owned(),
                year: Some(1991),
                release_group_mbid: Some(GROUP.to_owned()),
                release_mbid: Some(RELEASE.to_owned()),
            },
            JobPayload {
                release_mbid: Some(RELEASE.to_owned()),
                tracks_total: Some(4),
                candidates: Vec::new(),
            },
            task_id,
            ReviewOutcome::Park,
        )
        .await
        .expect("review records");
    assert!(recorded, "{task_id} is parked on its search");
}

/// An upgrade never waits for a pick: when its look finds no recommended
/// copy it stops with "no better copy", nothing is fetched, and the album
/// rests under the upgrade back-off. One parked before this rule settles
/// the same way on the next pass, unless a person's request joined it: that
/// one waits for their pick, as any request does.
#[tokio::test]
async fn an_upgrade_with_no_recommended_copy_settles_instead_of_waiting() {
    let mock = MockSlskd::start().await.expect("mock starts");
    mock.script_responses(vec![peer(
        "halfway",
        "@@music\\Massive Attack\\Blue Lines",
        &[0, 1],
    )]);
    let rig = rig(&mock, true).await;
    let looked = "0123456789abcdef0123456789abcdea";
    seed_album_task(&rig.journal, looked, "upgrade").await;
    let parked = "0123456789abcdef0123456789abcdeb";
    let joined = "0123456789abcdef0123456789abcdec";
    for (task_id, job_id) in [
        (parked, "fedcba9876543210fedcba9876543210"),
        (joined, "fedcba9876543210fedcba9876543211"),
    ] {
        seed_album_task(&rig.journal, task_id, "upgrade").await;
        park_on_search(&rig.journal, task_id, job_id).await;
    }
    rig.journal
        .db()
        .write("test.request", |tx| {
            tx.execute(
                "INSERT INTO request_history (musicbrainz_id_lower, musicbrainz_id, artist_name, \
                 album_title, requested_at, status, user_id, download_task_id, request_kind) \
                 VALUES (?1, ?1, 'Massive Attack', 'Blue Lines', 1, 'downloading', 'u1', ?2, \
                 'album')",
                [GROUP, joined],
            )?;
            Ok(())
        })
        .await
        .expect("a request joins the upgrade");

    let mut updates = rig.events.subscribe("u1");
    rig.worker.run_once(1).await;
    look_finishes(&mut updates, &rig.worker).await;
    rig.worker.run_once(2).await;

    let waiting = rig
        .journal
        .read_task(joined)
        .await
        .expect("task reads")
        .expect("task exists");
    assert_eq!(
        waiting.status.as_str(),
        "queued",
        "a person's request waits"
    );
    let mut finished_at = 0.0_f64;
    for task_id in [looked, parked] {
        let task = rig
            .journal
            .read_task(task_id)
            .await
            .expect("task reads")
            .expect("task exists");
        assert_eq!(task.status.as_str(), "cancelled", "{task_id}");
        assert_eq!(task.outcome_code, Some(Reason::NoBetterCopy), "{task_id}");
        assert!(
            rig.journal
                .read_source_handles(
                    task_id,
                    &droppedneedle_acquire::downloads::SourceTag::Soulseek
                )
                .await
                .expect("handles read")
                .is_empty(),
            "nothing fetched for {task_id}"
        );
        finished_at = finished_at.max(task.completed_at.expect("settled"));
    }
    let now = finished_at as i64 + 60;
    let resting = UpgradeWorklist::new(rig.journal.db().clone())
        .list_resting(now)
        .await
        .expect("worklist reads");
    assert!(resting.contains(GROUP), "the album rests after the settle");
    drop(rig.staging);
}

/// A pick while another download of the same album is running is refused
/// as busy, and nothing of it is kept: the job stays open and no download
/// starts from it.
#[tokio::test]
async fn pick_is_refused_while_the_album_downloads() {
    let db = AcquireDb::scratch().expect("scratch db");
    db.add_user("u1", "U", "user").await.expect("user seeds");
    let store = JobStore::new(db.clone());
    let task = |id: &str| NewTask {
        id: id.to_owned(),
        user_id: "u1".to_owned(),
        artist_name: "Massive Attack".to_owned(),
        album_title: "Blue Lines".to_owned(),
        release_group_mbid: GROUP.to_owned(),
        origin: "user".to_owned(),
        retry_count: 0,
    };
    store
        .insert(NewJob {
            id: "job-busy".to_owned(),
            user_id: "u1".to_owned(),
            artist_name: "Massive Attack".to_owned(),
            album_title: "Blue Lines".to_owned(),
            year: Some(1991),
            release_group_mbid: Some(GROUP.to_owned()),
            release_mbid: Some(RELEASE.to_owned()),
        })
        .await
        .expect("job inserts");
    let finished = store
        .finish(
            "job-busy",
            JobStatus::Completed,
            JobPayload::default(),
            None,
        )
        .await
        .expect("job finishes");
    assert!(finished);
    let running = task("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
    Journal::new(db.clone(), quiet_ticks())
        .run("test.seed", move |store| {
            store.insert_task(&running, 1_000.0)
        })
        .await
        .expect("running download seeds");

    let refused = store
        .pick(
            "job-busy",
            PickedTask {
                task: task("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"),
                details: TaskDetails::default(),
                source: "soulseek".to_owned(),
                username: Some("complete".to_owned()),
                candidate_index: 0,
                quality_waived: false,
            },
        )
        .await
        .expect("pick answers");
    assert_eq!(refused, Err(PickRefusal::AlbumBusy));
    let job = store
        .get("job-busy")
        .await
        .expect("job reads")
        .expect("job exists");
    assert_eq!(job.status, JobStatus::Completed);
    assert_eq!(
        store.picked_task("job-busy").await.expect("task reads"),
        None
    );
}

const KEPT_GROUP: &str = "6d0b7a1e-0000-4000-8000-000000000041";
const KEPT_RELEASE: &str = "6d0b7a1e-0000-4000-8000-000000000042";

/// One Soulseek peer's copy of the one-track album `Dummy`: the folder it
/// shares, the album its file is tagged with and the bitrate the search
/// shows.
struct Copy {
    username: &'static str,
    folder: &'static str,
    album_tag: &'static str,
    bitrate: Option<i32>,
    file: KeptFile,
}

/// The file a peer shares.
#[derive(Clone, Copy)]
struct KeptFile {
    /// Its name, which says its format.
    name: &'static str,
    fixture: &'static str,
}

impl KeptFile {
    const MP3: Self = Self {
        name: "01 - Mysterons.mp3",
        fixture: "mp3_full_01.mp3",
    };
    /// An MP3 at about 192 kbps, which [`gap_policy`] does not list.
    const GAP_MP3: Self = Self {
        fixture: "management_full.mp3",
        ..Self::MP3
    };
    const OGG: Self = Self {
        name: "01 - Mysterons.ogg",
        fixture: "management_full.ogg",
    };
}

/// The search service and a worker that lands into a real library, over
/// the slskd mock whose peers share `copies` (each already downloaded
/// into the mock's folder), under the download policy `saved`.
struct KeptRig {
    jobs: Arc<SearchJobs>,
    worker: Arc<DownloadWorker>,
    journal: Arc<Journal>,
    db: AcquireDb,
    music: std::path::PathBuf,
    events: EventHub,
    _mock: MockSlskd,
    _platform: droppedneedle_kernel::platform::Platform,
}

async fn kept_rig_with(copies: &[Copy], saved: Value) -> KeptRig {
    use crate::acquire_landing::{Releases, land_file, length_ms, release};
    use crate::common::TestPlatform;
    use crate::common::library::{every_priority, library_at};
    use droppedneedle_acquire::landing::LandingService;
    use droppedneedle_acquire::landing::library::LibraryLanding;
    use droppedneedle_kernel::runtime_config::{ConfigStore, Crypto};
    use droppedneedle_library::identify::sources::ReleaseSource;
    use droppedneedle_library::scan::EffectivePolicy;
    use droppedneedle_library::tags::TagField;
    use droppedneedle_library::tags::save::{TagEdit, save_tags};

    let db = AcquireDb::scratch().expect("scratch db");
    db.add_user("u1", "U", "user").await.expect("user seeds");
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

    let downloads = dir.join("slskd");
    let mut peers = Vec::new();
    let mut length = None;
    for copy in copies {
        let file = land_file(
            &downloads.join(copy.folder),
            copy.file.name,
            copy.file.fixture,
            "Mysterons",
            1,
        );
        save_tags(
            &file,
            &[TagEdit::new(
                TagField::Album,
                vec![copy.album_tag.to_owned()],
            )],
        )
        .expect("album tag");
        length = length_ms(&file);
        let size = std::fs::metadata(&file).expect("file").len();
        let mut entry = json!({
            "filename": format!("@@music\\Portishead\\{}\\{}", copy.folder, copy.file.name),
            "size": size, "extension": copy.file.name.rsplit('.').next(),
            "length": length.map(|ms| ms as f64 / 1000.0),
        });
        if let Some(bitrate) = copy.bitrate {
            entry["bitRate"] = json!(bitrate);
        }
        peers.push(json!({
            "username": copy.username, "hasFreeUploadSlot": true, "uploadSpeed": 1_000_000,
            "queueLength": 0, "fileCount": 1, "lockedFileCount": 0, "lockedFiles": [],
            "token": 1, "files": [entry],
        }));
    }
    let mock = MockSlskd::start().await.expect("mock starts");
    mock.script_responses(peers);

    let edition = release(KEPT_RELEASE, KEPT_GROUP, &["Mysterons"], &[length]);
    let mut albums = FixedAlbums::default();
    albums.releases.insert(
        KEPT_RELEASE.to_owned(),
        AlbumRelease {
            id: KEPT_RELEASE.to_owned(),
            title: "Dummy".to_owned(),
            release_group_mbid: KEPT_GROUP.to_owned(),
            artist: "Portishead".to_owned(),
            various_artists: false,
            year: Some(1994),
            tracks: vec![AlbumTrack {
                disc: 1,
                position: 1,
                title: "Mysterons".to_owned(),
                duration_seconds: length.map(|ms| ms as f64 / 1000.0),
                recording_mbid: edition.tracks[0].recording_id.clone(),
                release_track_mbid: edition.tracks[0].id.clone(),
            }],
        },
    );
    let releases: Arc<dyn ReleaseSource> = Arc::new(Releases(vec![edition]));

    let band = dir.join("band");
    std::fs::create_dir_all(&band).expect("settings dir");
    std::fs::write(
        band.join("config.json"),
        json!({"download_policy": saved}).to_string(),
    )
    .expect("config.json");
    let settings = droppedneedle_acquire::settings::scratch_settings(&band).expect("settings");

    let journal = Arc::new(Journal::new(db.clone(), quiet_ticks()));
    let landing = Arc::new(LandingService::new(
        journal.clone(),
        Arc::new(LibraryLanding::new(&library, every_priority(releases))),
        settings.live(),
        dir.join("held"),
    ));
    let targets = Arc::new(droppedneedle_acquire::target::Targets::new(
        journal.clone(),
        Arc::new(albums),
    ));
    save_slskd(&settings, &mock.base_url());
    // Each pick is the source's own, on the pass that asks for it.
    review_off_in(&settings);
    let events = EventHub::new();
    let worker = worker_with(
        &platform,
        crate::common::bundles::idle_host(&platform, library.core.clone()),
        journal.clone(),
        &settings,
        downloads,
        targets.clone(),
        landing,
        Default::default(),
        events.clone(),
    );
    let jobs = Arc::new(SearchJobs::new(
        JobStore::new(db.clone()),
        worker.clone(),
        targets,
        Arc::new(QuotaLedger::unlimited(db.clone())),
        events.clone(),
    ));
    KeptRig {
        jobs,
        worker,
        journal,
        db,
        music,
        events,
        _mock: mock,
        _platform: platform,
    }
}

impl KeptRig {
    /// Search for `Dummy` as `owner` and wait for the search to finish.
    async fn search(&self, owner: &Principal) -> (String, JobPayload) {
        let mut updates = self.events.subscribe(owner.user_id());
        let started = self
            .jobs
            .start(
                owner,
                AlbumSearch {
                    artist_name: "Portishead".to_owned(),
                    album_title: "Dummy".to_owned(),
                    year: Some(1994),
                    release_group_mbid: Some(KEPT_GROUP.to_owned()),
                    release_mbid: Some(KEPT_RELEASE.to_owned()),
                },
            )
            .await
            .expect("search starts");
        let StartOutcome::Searching(job_id) = started else {
            panic!("nothing was searched: {started:?}");
        };
        let view = tokio::time::timeout(crate::common::PATIENCE, async {
            loop {
                let view = self.jobs.get(owner, &job_id).await.expect("job reads");
                if view.row.status != JobStatus::Searching {
                    break view;
                }
                updates.next().await.expect("the hub stays open");
            }
        })
        .await
        .expect("the search settles");
        (job_id, view.payload.expect("candidates decode"))
    }

    /// Run worker passes until the task settles.
    async fn settle(&self, task_id: &str) -> droppedneedle_acquire::downloads::store::TaskRow {
        for pass in 1..12 {
            self.worker.run_once(pass).await;
            self.worker.wait_for_landings().await;
            let task = self
                .journal
                .read_task(task_id)
                .await
                .expect("task reads")
                .expect("task exists");
            if task.status.is_terminal() {
                return task;
            }
        }
        panic!("task {task_id} never settled");
    }

    /// A queued album download nobody picked a copy for.
    async fn automatic_task(&self) -> String {
        let task_id = "0123456789abcdef0123456789abcdef";
        self.journal
            .run("test.seed", move |store| {
                store.insert_task(
                    &NewTask {
                        id: task_id.to_owned(),
                        user_id: "u1".to_owned(),
                        artist_name: "Portishead".to_owned(),
                        album_title: "Dummy".to_owned(),
                        release_group_mbid: KEPT_GROUP.to_owned(),
                        origin: "user".to_owned(),
                        retry_count: 0,
                    },
                    1_000.0,
                )?;
                store.set_task_details(
                    task_id,
                    &TaskDetails {
                        release_mbid: Some(KEPT_RELEASE.to_owned()),
                        year: Some(1994),
                        ..TaskDetails::default()
                    },
                    1_000.0,
                )
            })
            .await
            .expect("task seeds");
        task_id.to_owned()
    }

    /// Each held file's reason and state.
    fn held_files(&self) -> Vec<(String, String)> {
        self.db.db().rows(
            "SELECT reason, state FROM acquire_held_files ORDER BY id",
            [],
        )
    }

    fn task_count(&self) -> i64 {
        self.db.db().one("SELECT COUNT(*) FROM download_tasks", [])
    }
}

/// A copy outside the saved settings (an MP3 outside a lossless-only band,
/// an OGG the default recipe does not list) is listed with its reason.
/// Picking it without a confirm answers 409 and starts nothing; with the
/// confirm, the download fetches it and its files are imported.
#[tokio::test]
async fn a_confirmed_pick_keeps_a_copy_outside_the_settings() {
    let mp3 = Copy {
        username: "mp3peer",
        folder: "Dummy (MP3)",
        album_tag: "Dummy",
        bitrate: Some(320),
        file: KeptFile::MP3,
    };
    let lossless_only = json!({"quality_min": "lossless", "quality_max": "lossless"});
    for (case, copy, policy, extension) in [
        ("an mp3 outside the band", mp3, lossless_only, "mp3"),
        ("an unlisted ogg", OGG_FOLDER, default_policy(), "ogg"),
    ] {
        let username = copy.username;
        let rig = kept_rig_with(&[copy], policy).await;
        let owner = principal("u1");
        let (job_id, payload) = rig.search(&owner).await;
        let listed: Vec<_> = payload
            .candidates
            .iter()
            .map(|c| (c.view.username.clone(), c.view.tier, c.view.refusal.clone()))
            .collect();
        assert_eq!(
            listed,
            [(
                Some(username.to_owned()),
                CandidateTier::Refused,
                vec![CandidateRefusal::QualityOutsideRange]
            )],
            "{case}"
        );

        let refused = rig
            .jobs
            .pick(&owner, &job_id, 0, false)
            .await
            .expect_err("an unconfirmed pick is refused");
        assert!(
            matches!(
                refused,
                SearchJobError::PolicyBreak(CandidateRefusal::QualityOutsideRange)
            ),
            "{case}"
        );
        let answer = droppedneedle_kernel::error::ApiError::from(refused);
        assert_eq!(
            (answer.status().as_u16(), answer.code()),
            (409, "CONFLICT"),
            "{case}"
        );
        assert_eq!(rig.task_count(), 0, "{case}: no download starts");
        let view = rig.jobs.get(&owner, &job_id).await.expect("job reads");
        assert_eq!(
            view.row.status,
            JobStatus::Completed,
            "{case}: the search stays open"
        );

        let task_id = rig
            .jobs
            .pick(&owner, &job_id, 0, true)
            .await
            .expect("a confirmed pick starts");
        let task = rig.settle(&task_id).await;
        assert_eq!(
            task.status.as_str(),
            "completed",
            "{case}: {:?}",
            task.error_message
        );
        assert_eq!(
            crate::acquire_landing::files_under(&rig.music, extension).len(),
            1,
            "{case}: the picked files are in the library"
        );
    }
}

/// A confirmed pick covers only the attempt that fetched it. When that
/// attempt fails, the failover's folder (whose quality the search did not
/// show) lands MP3 files the recipe does not list, and they are held.
#[tokio::test]
async fn a_failover_after_a_confirmed_pick_is_held_to_the_recipe() {
    let rig = kept_rig_with(
        &[
            Copy {
                username: "picked",
                album_tag: "Third",
                ..OGG_FOLDER
            },
            Copy {
                username: "fallback",
                folder: "Dummy (web)",
                album_tag: "Dummy",
                bitrate: None,
                file: KeptFile::GAP_MP3,
            },
        ],
        gap_policy(),
    )
    .await;
    let owner = principal("u1");
    let (job_id, payload) = rig.search(&owner).await;
    let listed: Vec<_> = payload
        .candidates
        .iter()
        .map(|c| (c.view.username.clone(), c.view.tier))
        .collect();
    assert_eq!(
        listed,
        [
            (Some("fallback".to_owned()), CandidateTier::Possible),
            (Some("picked".to_owned()), CandidateTier::Refused),
        ]
    );
    let task_id = rig
        .jobs
        .pick(&owner, &job_id, 1, true)
        .await
        .expect("a confirmed pick starts");
    rig.settle(&task_id).await;

    let handles: Vec<SourceHandle> = rig
        .journal
        .read_source_handles(
            &task_id,
            &droppedneedle_acquire::downloads::SourceTag::Soulseek,
        )
        .await
        .expect("handles read")
        .iter()
        .map(|json| serde_json::from_str(json).expect("handle decodes"))
        .collect();
    let attempts: Vec<_> = handles
        .iter()
        .map(|handle| (handle.username.as_str(), handle.pinned))
        .collect();
    assert_eq!(attempts, [("picked", true), ("fallback", false)]);
    let reasons: Vec<Option<String>> = rig.db.db().column(
        "SELECT reason_code FROM download_import_decisions WHERE task_id = ?1 \
         ORDER BY decided_at",
        rusqlite::params![&task_id],
    );
    assert_eq!(
        reasons.last().cloned().flatten().as_deref(),
        Some("format_not_listed"),
        "{reasons:?}"
    );
    assert!(
        crate::acquire_landing::files_under(&rig.music, "mp3").is_empty(),
        "nothing outside the band was kept"
    );
}

/// A folder whose bitrate the search does not show.
const GAP_FOLDER: Copy = Copy {
    username: "gappeer",
    folder: "Dummy (MP3)",
    album_tag: "Dummy",
    bitrate: None,
    file: KeptFile::GAP_MP3,
};

const OGG_FOLDER: Copy = Copy {
    username: "oggpeer",
    folder: "Dummy (OGG)",
    album_tag: "Dummy",
    bitrate: Some(320),
    file: KeptFile::OGG,
};

/// The default policy lists FLAC and MP3 at 320, which an OGG at 320 reaches
/// by tier alone.
fn default_policy() -> Value {
    json!({"quality_min": "mp3_320", "quality_max": "lossless"})
}

/// MP3 at 320 and above, or below 192: an MP3 at 192 to 319 is inside the
/// band but between the steps. A search that shows no bitrate cannot tell.
fn gap_policy() -> Value {
    json!({"quality_recipe": [
        {"format": "mp3", "quality": "320_plus"},
        {"format": "mp3", "quality": "below_192"},
    ]})
}

/// An automatic download with only an OGG folder to take starts nothing and
/// says why, in the task's own reason.
#[tokio::test]
async fn an_automatic_download_takes_no_format_the_recipe_does_not_list() {
    let rig = kept_rig_with(&[OGG_FOLDER], default_policy()).await;
    let task = rig.automatic_task().await;
    let task = rig.settle(&task).await;

    assert_eq!(task.status.as_str(), "failed", "{:?}", task.error_message);
    assert_eq!(
        task.outcome_code.map(|reason| reason.code()),
        Some("nothing_allowed")
    );
    let attempts: i64 = rig
        .db
        .db()
        .one("SELECT COUNT(*) FROM download_attempts", []);
    assert_eq!(attempts, 0, "nothing was fetched");
}

/// Files that turn out to be a quality the recipe does not list are held,
/// not placed, and the release is not tried again.
#[tokio::test]
async fn an_unlisted_landing_is_held_not_placed() {
    let rig = kept_rig_with(&[GAP_FOLDER], gap_policy()).await;
    let task = rig.automatic_task().await;
    let task = rig.settle(&task).await;

    assert_eq!(task.status.as_str(), "failed", "{:?}", task.error_message);
    assert_eq!(
        task.outcome_code.map(|reason| reason.code()),
        Some("format_not_listed")
    );
    assert!(
        crate::acquire_landing::files_under(&rig.music, "mp3").is_empty(),
        "nothing is placed"
    );
    assert_eq!(rig.held_files(), [("format_not_listed", "held")].map(owned));
    let blocked: i64 = rig
        .db
        .db()
        .one("SELECT COUNT(*) FROM download_quarantine", []);
    assert!(blocked > 0, "the release is not tried again");
}

/// An album keeps one held set of unlisted copies: the second landing copies
/// nothing. A complete allowed import then discards the set and says so.
#[tokio::test]
async fn an_album_holds_one_unlisted_set_until_a_complete_copy_lands() {
    let rig = kept_rig_with(
        &[
            Copy {
                username: "a-gap",
                ..GAP_FOLDER
            },
            Copy {
                username: "b-gap",
                folder: "Dummy (MP3 2)",
                ..GAP_FOLDER
            },
            Copy {
                username: "c-320",
                folder: "Dummy (MP3 320)",
                file: KeptFile::MP3,
                ..GAP_FOLDER
            },
        ],
        gap_policy(),
    )
    .await;
    let task = rig.automatic_task().await;
    let task = rig.settle(&task).await;
    assert_eq!(
        task.status.as_str(),
        "completed",
        "{:?}",
        task.error_message
    );

    let decisions: Vec<(Option<String>, i64, Option<String>)> = rig.db.db().rows(
        "SELECT reason_code, files_held, detail FROM download_import_decisions \
         ORDER BY decided_at",
        [],
    );
    let held: Vec<_> = decisions
        .iter()
        .map(|(code, held, _)| (code.as_deref(), *held))
        .collect();
    assert_eq!(
        held,
        [
            (Some("format_not_listed"), 1),
            (Some("format_not_listed"), 0),
            (None, 0),
        ]
    );
    // The import that ended the held set says so where its download shows.
    assert_eq!(
        decisions[2].2.as_deref(),
        Some("Removed the held copy not in your quality settings")
    );
    assert_eq!(
        crate::acquire_landing::files_under(&rig.music, "mp3").len(),
        1
    );
    assert_eq!(
        rig.held_files(),
        [("format_not_listed", "discarded")].map(owned)
    );
}

fn owned((reason, state): (&str, &str)) -> (String, String) {
    (reason.to_owned(), state.to_owned())
}
