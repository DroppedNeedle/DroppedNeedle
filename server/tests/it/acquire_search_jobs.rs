//! A manual album search: start it, read the ranked candidates, pick one
//! that is not the top pick, and the worker fetches exactly that folder
//! without searching again. Runs the real search service, worker pass and
//! slskd source against the loopback mock with a fixed MusicBrainz
//! tracklist.

use std::sync::Arc;
use std::time::Duration;

use droppedneedle::acquire::db::AcquireDb;
use droppedneedle::acquire::dispatch::Journal;
use droppedneedle::acquire::downloads::manifest::ManifestCodec;
use droppedneedle::acquire::downloads::sources::SourceHandle;
use droppedneedle::acquire::requests::auth::{Principal, Role};
use droppedneedle::acquire::requests::quota::QuotaLedger;
use droppedneedle::acquire::search_jobs::candidates::CandidateTier;
use droppedneedle::acquire::search_jobs::store::JobStore;
use droppedneedle::acquire::search_jobs::{AlbumSearch, SearchJobError, SearchJobs, StartOutcome};
use droppedneedle::acquire::slskd::{
    DownloadPolicy, MOCK_API_KEY, MockSlskd, ReqwestSlskdHttp, SlskdClient, SlskdRepository,
};
use droppedneedle::acquire::sources::SlskdSource;
use droppedneedle::acquire::target::Targets;
use droppedneedle::acquire::target::lookup::{AlbumRelease, AlbumTrack, FixedAlbums};
use droppedneedle::acquire::worker::{DownloadWorker, Source, WorkerConfig};
use droppedneedle::events::EventSink;
use serde_json::{Value, json};

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

#[tokio::test]
async fn manual_search_ranks_folders_and_fetches_the_picked_one() {
    let mock = MockSlskd::start().await.expect("mock starts");
    mock.script_responses(vec![
        peer("halfway", "@@music\\Massive Attack\\Blue Lines", &[0, 1]),
        peer("complete", "@@music\\MA\\Blue Lines (1991)", &[0, 1, 2, 3]),
    ]);

    let db = AcquireDb::scratch().expect("scratch db");
    db.add_user("u1", "U", "user").await.expect("user seeds");
    db.add_user("u2", "V", "user").await.expect("user seeds");
    let staging = crate::common::ScratchDir::new("search-jobs-staging");
    let journal = Arc::new(Journal::new(db.clone()));
    let mut albums = FixedAlbums::default();
    albums.releases.insert(RELEASE.to_owned(), album());
    let targets = Arc::new(Targets::new(journal.clone(), staging.to_path_buf()));
    assert!(targets.lookup_slot().set(Arc::new(albums)).is_ok());
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
    let worker = Arc::new(DownloadWorker::fixed(
        journal.clone(),
        vec![Source::Slskd(Arc::new(source))],
        WorkerConfig {
            staging_root: staging.to_path_buf(),
            ..WorkerConfig::default()
        },
    ));
    let jobs = Arc::new(SearchJobs::new(
        JobStore::new(db.clone()),
        worker.clone(),
        targets,
        Arc::new(QuotaLedger::unlimited(db.clone())),
        EventSink::default(),
    ));
    let owner = Principal {
        user_id: "u1".to_owned(),
        username: None,
        role: Role::User,
    };
    let stranger = Principal {
        user_id: "u2".to_owned(),
        username: None,
        role: Role::User,
    };

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

    let mut view = jobs.get(&owner, &job_id).await.expect("job reads");
    for _ in 0..200 {
        if view.row.status != "searching" {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
        view = jobs.get(&owner, &job_id).await.expect("job reads");
    }
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
    assert_eq!(payload.candidates[1].view.note.code, "incomplete");
    assert!(matches!(
        jobs.get(&stranger, &job_id).await,
        Err(SearchJobError::Forbidden)
    ));

    // Pick the second folder; the worker fetches it without searching.
    let searches_before = mock.search_texts().len();
    let task_id = jobs
        .pick(&owner, &job_id, 1)
        .await
        .expect("pick starts a download");
    assert!(matches!(
        jobs.pick(&owner, &job_id, 0).await,
        Err(SearchJobError::Conflict(_))
    ));
    worker.run_once(1).await;
    assert_eq!(mock.search_texts().len(), searches_before);
    let handles = journal
        .read_source_handles(&task_id, "soulseek")
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
    let bytes = std::fs::read(ManifestCodec::path(&staging, &task_id)).expect("manifest written");
    let manifest = ManifestCodec.decode(&bytes).expect("manifest decodes");
    assert_eq!(manifest.release_mbid.as_deref(), Some(RELEASE));
    assert_eq!(manifest.expected_tracks.len(), 4);

    let view = jobs.get(&owner, &job_id).await.expect("job reads");
    assert_eq!(view.row.status, "matched");
    assert_eq!(view.task_id.as_deref(), Some(task_id.as_str()));
    assert!(matches!(
        jobs.cancel(&owner, &job_id).await,
        Err(SearchJobError::Conflict(_))
    ));
}
