//! A single-track request is fetched like a manual search: the recording
//! resolves to its album, the album folder wins over a peer sharing the
//! lone track, and only the wanted track is queued. Runs the real
//! dispatch, worker pass and slskd source against the loopback mock and
//! fixed MusicBrainz answers.

use std::sync::Arc;
use std::time::Duration;

use crate::common::bundles::quiet_ticks;
use droppedneedle_acquire::db::AcquireDb;
use droppedneedle_acquire::dispatch::{
    DispatchOrigin, DispatchOutcome, DispatchRequest, DispatchTarget, DownloadDispatch,
};
use droppedneedle_acquire::downloads::TrackPosition;
use droppedneedle_acquire::downloads::journal::Journal;
use droppedneedle_acquire::downloads::sources::SourceHandle;
use droppedneedle_acquire::downloads::worker::{DownloadWorker, WorkerConfig};
use droppedneedle_acquire::slskd::{
    DownloadPolicy, ReqwestSlskdHttp, SlskdClient, SlskdRepository,
};
use droppedneedle_acquire::sources::SlskdSource;
use droppedneedle_acquire::target::Targets;
use droppedneedle_acquire::target::lookup::{
    AlbumRelease, AlbumTrack, FixedAlbums, ReleaseCandidate,
};
use droppedneedle_kernel::ids::UuidGenerator;
use droppedneedle_testkit::slskd::{MOCK_API_KEY, MockSlskd};
use serde_json::{Value, json};

const RECORDING: &str = "11111111-1111-4111-8111-111111111111";
const ALBUM_RELEASE: &str = "22222222-2222-4222-8222-222222222222";
const ALBUM_GROUP: &str = "33333333-3333-4333-8333-333333333333";
const COMPILATION: &str = "44444444-4444-4444-8444-444444444444";

fn peer(username: &str, free: bool, speed: i64, files: &[(&str, f64)]) -> Value {
    let files: Vec<Value> = files
        .iter()
        .map(|(name, length)| {
            json!({"filename": name, "size": 30_000_000, "extension": "flac",
                   "bitDepth": 16, "sampleRate": 44100, "length": length})
        })
        .collect();
    json!({"username": username, "hasFreeUploadSlot": free, "uploadSpeed": speed,
           "queueLength": if free { 0 } else { 4 }, "fileCount": files.len(),
           "lockedFileCount": 0, "files": files, "lockedFiles": [], "token": 1})
}

fn album() -> AlbumRelease {
    let titles = [
        ("Safe From Harm", 331.0),
        ("One Love", 288.0),
        ("Blue Lines", 261.0),
        ("Be Thankful for What You've Got", 249.0),
    ];
    AlbumRelease {
        id: ALBUM_RELEASE.to_owned(),
        title: "Blue Lines".to_owned(),
        release_group_mbid: ALBUM_GROUP.to_owned(),
        artist: "Massive Attack".to_owned(),
        various_artists: false,
        year: Some(1991),
        tracks: titles
            .iter()
            .enumerate()
            .map(|(index, (title, length))| AlbumTrack {
                disc: 1,
                position: u32::try_from(index + 1).expect("small"),
                title: (*title).to_owned(),
                duration_seconds: Some(*length),
                recording_mbid: if index == 1 {
                    RECORDING.to_owned()
                } else {
                    format!("aaaaaaaa-0000-4000-8000-00000000000{index}")
                },
                release_track_mbid: format!("bbbbbbbb-0000-4000-8000-00000000000{index}"),
            })
            .collect(),
    }
}

#[tokio::test]
async fn track_request_fetches_one_file_from_the_album_folder() {
    let mock = MockSlskd::start().await.expect("mock starts");
    // The lone peer has the free slot and the speed, which used to win.
    mock.script_responses(vec![
        peer(
            "lonely",
            true,
            9_000_000,
            &[("@@share\\Singles\\Massive Attack - One Love.flac", 288.0)],
        ),
        peer(
            "collector",
            false,
            100_000,
            &[
                (
                    "@@music\\Massive Attack\\Blue Lines\\01 Safe From Harm.flac",
                    331.0,
                ),
                (
                    "@@music\\Massive Attack\\Blue Lines\\02 One Love.flac",
                    288.0,
                ),
                (
                    "@@music\\Massive Attack\\Blue Lines\\03 Blue Lines.flac",
                    261.0,
                ),
                (
                    "@@music\\Massive Attack\\Blue Lines\\04 Be Thankful for What You've Got.flac",
                    249.0,
                ),
            ],
        ),
    ]);

    let db = AcquireDb::scratch().expect("scratch db");
    db.add_user("u1", "U", "user").await.expect("user seeds");
    let staging = crate::common::ScratchDir::new("track-album-staging");
    let journal = Arc::new(Journal::new(db, quiet_ticks()));
    let settings_dir = journal
        .db()
        .path()
        .parent()
        .expect("scratch dir")
        .join("settings");
    let settings =
        droppedneedle_acquire::settings::scratch_settings(&settings_dir).expect("settings open");
    let dispatch = DownloadDispatch::new(journal.clone(), Arc::new(UuidGenerator), settings.live());
    let outcome = dispatch
        .dispatch(&DispatchRequest {
            user_id: "u1".to_owned(),
            target: DispatchTarget::Track {
                recording_mbid: RECORDING.to_owned(),
                album_version: false,
            },
            artist_name: "Massive Attack".to_owned(),
            title: "One Love".to_owned(),
            origin: DispatchOrigin::User,
            release_mbid: None,
            wanted: Vec::new(),
            planned_album: None,
            idempotency_key: None,
        })
        .await
        .expect("track dispatches");
    let DispatchOutcome::Dispatched { task_id } = outcome else {
        panic!("track was not dispatched: {outcome:?}");
    };

    // MusicBrainz: the recording is on the album and on a compilation.
    let mut albums = FixedAlbums::default();
    let candidate = |id: &str, group: &str, secondary: &[&str]| ReleaseCandidate {
        id: id.to_owned(),
        status: Some("Official".to_owned()),
        date: Some("1991-04-08".to_owned()),
        release_group_mbid: group.to_owned(),
        primary_type: Some("Album".to_owned()),
        secondary_types: secondary.iter().map(|s| (*s).to_owned()).collect(),
    };
    albums.recordings.insert(
        RECORDING.to_owned(),
        vec![
            candidate(
                COMPILATION,
                "55555555-5555-4555-8555-555555555555",
                &["Compilation"],
            ),
            candidate(ALBUM_RELEASE, ALBUM_GROUP, &[]),
        ],
    );
    albums.releases.insert(ALBUM_RELEASE.to_owned(), album());
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
    let source = SlskdSource::new(Arc::new(repo), journal.clone()).with_targets(targets);
    let worker = Arc::new(DownloadWorker::fixed(
        journal.clone(),
        vec![crate::as_source(Arc::new(source))],
        WorkerConfig::default(),
        droppedneedle_kernel::events::EventHub::new(),
        Default::default(),
    ));
    worker.run_once(1).await;

    // The album was searched for, not the lone track.
    let searches = mock.search_texts();
    assert!(
        searches.iter().all(|text| text.contains("Blue Lines")),
        "only album queries ran: {searches:?}"
    );

    // One attempt: the album folder's peer, and only the wanted file.
    let lookup = task_id.clone();
    let handles = journal
        .read_source_handles(
            &lookup,
            &droppedneedle_acquire::downloads::SourceTag::Soulseek,
        )
        .await
        .expect("handles read");
    assert_eq!(handles.len(), 1, "one enqueue: {handles:?}");
    let handle: SourceHandle = serde_json::from_str(&handles[0]).expect("handle decodes");
    assert_eq!(handle.username, "collector");
    assert_eq!(
        handle.filenames,
        ["@@music\\Massive Attack\\Blue Lines\\02 One Love.flac"]
    );

    // The task row and its plan carry the album for the landing.
    let task = journal
        .read_task(&task_id)
        .await
        .expect("task reads")
        .expect("task exists");
    assert_eq!(task.release_group_mbid, ALBUM_GROUP);
    assert_eq!(task.album_title, "Blue Lines");
    let plan_task = task_id.clone();
    let plan = journal
        .read("test.plan", move |store| store.task_plan(&plan_task))
        .await
        .expect("plan reads");
    assert_eq!(plan.release_mbid.as_deref(), Some(ALBUM_RELEASE));
    assert_eq!(plan.expected_tracks.len(), 4);
    let context = plan.track_album.expect("album context recorded");
    assert_eq!(context.basis, "best_official");
    assert_eq!(context.wanted, [TrackPosition { disc: 1, track: 2 }]);
}
