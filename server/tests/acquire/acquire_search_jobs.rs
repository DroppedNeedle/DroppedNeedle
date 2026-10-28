//! A manual album search: start it, read the ranked candidates, pick one
//! that is not the top pick, and the worker fetches exactly that folder
//! without searching again. A folder outside the quality band is listed,
//! picked only with a confirm, and then kept, for that attempt only. Runs
//! the real search service, worker pass and slskd source against the
//! loopback mock with a fixed MusicBrainz tracklist.

use std::sync::Arc;
use std::time::Duration;

use droppedneedle_acquire::db::AcquireDb;
use droppedneedle_acquire::downloads::journal::Journal;
use droppedneedle_acquire::downloads::sources::SourceHandle;
use droppedneedle_acquire::downloads::store::{NewTask, TaskDetails};
use droppedneedle_acquire::downloads::worker::{DownloadWorker, WorkerConfig};
use droppedneedle_acquire::requests::quota::QuotaLedger;
use droppedneedle_acquire::search_jobs::candidates::{
    CandidateNoteCode, CandidateRefusal, CandidateTier,
};
use droppedneedle_acquire::search_jobs::store::{
    COMPLETED, JobPayload, JobStore, NewJob, PickRefusal, PickedTask,
};
use droppedneedle_acquire::search_jobs::{AlbumSearch, SearchJobError, SearchJobs, StartOutcome};
use droppedneedle_acquire::slskd::{
    DownloadPolicy, ReqwestSlskdHttp, SlskdClient, SlskdRepository,
};
use droppedneedle_acquire::sources::SlskdSource;
use droppedneedle_acquire::target::Targets;
use droppedneedle_acquire::target::lookup::{AlbumRelease, AlbumTrack, FixedAlbums};
use droppedneedle_auth::session::store::SessionKind;
use droppedneedle_auth::{Principal, Role};
use droppedneedle_kernel::events::EventHub;
use droppedneedle_testkit::slskd::{MOCK_API_KEY, MockSlskd};
use serde_json::{Value, json};

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
/// MusicBrainz tracklist. `review` turns on the look before an automatic
/// grab.
async fn rig(mock: &MockSlskd, review: bool) -> Rig {
    let db = AcquireDb::scratch().expect("scratch db");
    db.add_user("u1", "U", "user").await.expect("user seeds");
    db.add_user("u2", "V", "user").await.expect("user seeds");
    let staging = crate::common::ScratchDir::new("search-jobs-staging");
    let journal = Arc::new(Journal::new(db.clone(), quiet_ticks()));
    let mut albums = FixedAlbums::default();
    albums.releases.insert(RELEASE.to_owned(), album());
    let targets = Arc::new(Targets::new(journal.clone(), Some(Arc::new(albums))));
    let http = ReqwestSlskdHttp::new(reqwest::Client::new(), &mock.base_url(), MOCK_API_KEY);
    let repo = SlskdRepository::new(
        SlskdClient::new(http),
        &mock.base_url(),
        MOCK_API_KEY,
        staging.join("unused-mount"),
        DownloadPolicy {
            search_timeout: Duration::from_secs(2),
            completion_grace: Duration::from_secs(2),
            poll_interval: Duration::from_millis(10),
            ..DownloadPolicy::default()
        },
    );
    let source = SlskdSource::new(Arc::new(repo), journal.clone()).with_targets(targets.clone());
    let events = EventHub::new();
    let mut worker = DownloadWorker::fixed(
        journal.clone(),
        vec![crate::as_source(Arc::new(source))],
        WorkerConfig::default(),
        events.clone(),
        Default::default(),
    );
    if review {
        worker = worker.with_targets(targets.clone());
    }
    let worker = Arc::new(worker);
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
    let view = tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let view = jobs.get(&owner, &job_id).await.expect("job reads");
            if view.row.status != "searching" {
                break view;
            }
            updates.next().await.expect("the hub stays open");
        }
    })
    .await
    .expect("the search settles");
    assert_eq!(view.row.status, "completed");
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
    assert_eq!(view.row.status, "matched");
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

    rig.worker.run_once(1).await;
    // The look runs off the pass.
    for _ in 0..200 {
        if !rig.worker.reviews_running() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    assert!(!rig.worker.reviews_running(), "the look finished");
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
    assert_eq!(view.row.status, "completed");
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
        .finish("job-busy", COMPLETED, JobPayload::default(), None)
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
    assert_eq!(job.status, COMPLETED);
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
}

/// The search service and a worker that lands into a real library, over
/// the slskd mock whose peers share `copies` (each already downloaded
/// into the mock's folder). The download settings keep lossless only, so
/// every copy, an MP3, is outside the band.
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

async fn kept_rig(copies: &[Copy]) -> KeptRig {
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
            "01 - Mysterons.mp3",
            "mp3_full_01.mp3",
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
            "filename": format!("@@music\\Portishead\\{}\\01 - Mysterons.mp3", copy.folder),
            "size": size, "extension": "mp3",
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

    let lossless_only = DownloadPolicy {
        quality_min: "lossless".to_owned(),
        quality_max: "lossless".to_owned(),
        search_timeout: Duration::from_secs(2),
        completion_grace: Duration::from_secs(2),
        poll_interval: Duration::from_millis(10),
        ..DownloadPolicy::default()
    };
    let band = dir.join("band");
    std::fs::create_dir_all(&band).expect("settings dir");
    std::fs::write(
        band.join("config.json"),
        json!({"download_policy": {"quality_min": "lossless", "quality_max": "lossless"}})
            .to_string(),
    )
    .expect("config.json");
    let settings = droppedneedle_acquire::settings::scratch_settings(&band).expect("settings");

    let journal = Arc::new(Journal::new(db.clone(), quiet_ticks()));
    let landing = Arc::new(LandingService::new(
        journal.clone(),
        Some(Arc::new(LibraryLanding::new(
            &library,
            every_priority(releases),
        ))),
        settings.live(),
        dir.join("held"),
    ));
    let targets = Arc::new(Targets::new(journal.clone(), Some(Arc::new(albums))));
    let repo = SlskdRepository::new(
        SlskdClient::new(ReqwestSlskdHttp::new(
            reqwest::Client::new(),
            &mock.base_url(),
            MOCK_API_KEY,
        )),
        &mock.base_url(),
        MOCK_API_KEY,
        downloads,
        lossless_only,
    );
    let source = SlskdSource::new(Arc::new(repo), journal.clone()).with_targets(targets.clone());
    let events = EventHub::new();
    let worker = Arc::new(
        DownloadWorker::fixed(
            journal.clone(),
            vec![crate::as_source(Arc::new(source))],
            WorkerConfig::default(),
            events.clone(),
            Default::default(),
        )
        .with_landing(landing),
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
        let view = tokio::time::timeout(Duration::from_secs(30), async {
            loop {
                let view = self.jobs.get(owner, &job_id).await.expect("job reads");
                if view.row.status != "searching" {
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

    async fn task_count(&self) -> i64 {
        sqlx::query_scalar("SELECT COUNT(*) FROM download_tasks")
            .fetch_one(self.db.db().sqlx())
            .await
            .expect("task count")
    }
}

/// A folder outside the quality band is listed with its reason. Picking it
/// without a confirm answers 409 and starts nothing; with the confirm, the
/// download fetches it and its files are imported although their quality
/// is outside the band.
#[tokio::test]
async fn a_folder_outside_the_band_is_kept_once_its_pick_is_confirmed() {
    let rig = kept_rig(&[Copy {
        username: "mp3peer",
        folder: "Dummy (MP3)",
        album_tag: "Dummy",
        bitrate: Some(320),
    }])
    .await;
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
            Some("mp3peer".to_owned()),
            CandidateTier::Refused,
            vec![CandidateRefusal::QualityOutsideRange]
        )]
    );

    let refused = rig
        .jobs
        .pick(&owner, &job_id, 0, false)
        .await
        .expect_err("an unconfirmed pick is refused");
    assert!(matches!(
        refused,
        SearchJobError::PolicyBreak(CandidateRefusal::QualityOutsideRange)
    ));
    let answer = droppedneedle_kernel::error::ApiError::from(refused);
    assert_eq!((answer.status().as_u16(), answer.code()), (409, "CONFLICT"));
    assert_eq!(rig.task_count().await, 0, "no download starts");
    let view = rig.jobs.get(&owner, &job_id).await.expect("job reads");
    assert_eq!(view.row.status, COMPLETED, "the search stays open");

    let task_id = rig
        .jobs
        .pick(&owner, &job_id, 0, true)
        .await
        .expect("a confirmed pick starts");
    let task = rig.settle(&task_id).await;
    assert_eq!(
        task.status.as_str(),
        "completed",
        "{:?}",
        task.error_message
    );
    assert_eq!(
        crate::acquire_landing::files_under(&rig.music, "mp3").len(),
        1,
        "the picked files are in the library"
    );
}

/// A confirmed pick covers only the attempt that fetched it. When that
/// attempt fails, the failover's folder (whose quality the search did not
/// show) lands MP3 files outside the band, and they are refused on
/// quality.
#[tokio::test]
async fn a_failover_after_a_confirmed_pick_is_held_to_the_band() {
    let rig = kept_rig(&[
        Copy {
            username: "picked",
            folder: "Dummy (MP3)",
            album_tag: "Third",
            bitrate: Some(320),
        },
        Copy {
            username: "fallback",
            folder: "Dummy (web)",
            album_tag: "Dummy",
            bitrate: None,
        },
    ])
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
    let reasons: Vec<Option<String>> = sqlx::query_scalar(
        "SELECT reason_code FROM download_import_decisions WHERE task_id = ?1 \
         ORDER BY decided_at",
    )
    .bind(&task_id)
    .fetch_all(rig.db.db().sqlx())
    .await
    .expect("decisions");
    assert_eq!(
        reasons.last().cloned().flatten().as_deref(),
        Some("quality_rejected"),
        "{reasons:?}"
    );
    assert!(
        crate::acquire_landing::files_under(&rig.music, "mp3").is_empty(),
        "nothing outside the band was kept"
    );
}
