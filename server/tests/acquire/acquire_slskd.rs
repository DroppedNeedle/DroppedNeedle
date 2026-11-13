//! slskd/Soulseek per-client contract tests.
//!
//! Coverage of the slskd module against the in-repo mock
//! server on loopback: wire-shape tests (bitRate-absent lossless,
//! extension-from-filename, comma-joined state flags, PascalCase enqueue,
//! searchTimeout milliseconds), single-op tests (429 retry, semaphore
//! serialization), search fan-out tests (ladder fallback past silent
//! specific rungs, multi-peer fan-out), correlation tests
//! (accepted-filenames handles, latest-attempt status, truncated stubs),
//! and query construction. No live contact ever.

use droppedneedle_acquire::client_error::{ClientError, Service};
use droppedneedle_acquire::slskd;

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use droppedneedle_acquire::db::AcquireDb;
use droppedneedle_acquire::downloads::SourceTag;
use droppedneedle_acquire::downloads::journal::Journal;
use droppedneedle_acquire::downloads::sources::{DownloadSource as _, SourceHandle};
use droppedneedle_acquire::downloads::store::NewTask;
use droppedneedle_acquire::target::SearchTarget;
use droppedneedle_kernel::runtime_config::secret_sections::SlskdConnection;
use droppedneedle_kernel::runtime_config::sections::{
    DownloadPolicy as KernelPolicy, QualityRecipeEntry, RecipeFormat, RecipeQuality,
};

use crate::common::acquire::{FixedAlbums, no_targets, worker_over};
use crate::common::bundles::quiet_ticks;
use droppedneedle_testkit::slskd::{MOCK_API_KEY, MockSlskd};
use slskd::{
    DownloadPolicy, EnqueueFile, HttpFault, HttpReply, Locator, ReqwestSlskdHttp, SlskdClient,
    SlskdHttp, SlskdRepository, SlskdSearchResponse, SlskdUserSearchResponse, TaskHandle,
    aggregate_status, album_query_ladder, match_transfers,
};

fn test_policy() -> DownloadPolicy {
    DownloadPolicy {
        search_timeout: Duration::from_secs(2),
        completion_grace: Duration::from_secs(2),
        poll_interval: Duration::from_millis(10),
        ..DownloadPolicy::default()
    }
}

fn repository(mock: &MockSlskd) -> SlskdRepository<ReqwestSlskdHttp> {
    repository_with_key(mock, MOCK_API_KEY)
}

fn repository_with_key(mock: &MockSlskd, api_key: &str) -> SlskdRepository<ReqwestSlskdHttp> {
    let http = ReqwestSlskdHttp::new(reqwest::Client::new(), &mock.base_url(), api_key);
    // Never created: these repositories only talk to the mock.
    let mount = std::env::temp_dir().join(format!("slskd-unused-{}", std::process::id()));
    SlskdRepository::new(
        SlskdClient::new(http),
        &mock.base_url(),
        api_key,
        mount,
        test_policy(),
    )
}

/// Scratch mount, removed when the test ends.
fn temp_mount(tag: &str) -> crate::common::ScratchDir {
    crate::common::ScratchDir::new(tag)
}

/// slskd's key travels in a header, but the base URL is the user's and
/// can hold a secret of its own (a proxy path, basic-auth userinfo). A
/// dead slskd must not put it in the error the worker logs, the transport's
/// Debug output never shows the key, and a wrong key's health message is
/// the same fixed sentence with no URL, host or key in it.
#[tokio::test]
async fn slskd_never_shows_its_key_or_url() {
    const MARKER: &str = "MARKERKEY7f3a91";
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let port = listener.local_addr().expect("addr").port();
    drop(listener);
    let base = format!("http://127.0.0.1:{port}/{MARKER}");
    let client = SlskdClient::new(ReqwestSlskdHttp::new(
        reqwest::Client::new(),
        &base,
        MOCK_API_KEY,
    ));
    let err = client.health_check().await.expect_err("refused");
    assert!(
        matches!(err, ClientError::Unavailable { .. }),
        "transport: {err:?}"
    );
    let shown = format!("{err} {err:?}");
    assert!(!shown.contains(MARKER), "transport: {shown}");

    let transport =
        ReqwestSlskdHttp::new(reqwest::Client::new(), "http://127.0.0.1:9", "SUPERSECRET");
    let debug = format!("{transport:?}");
    assert!(debug.contains("<redacted>"), "debug: {debug}");
    assert!(!debug.contains("SUPERSECRET"), "debug: {debug}");

    let mock = MockSlskd::start().await.expect("mock starts");
    mock.set_expected_api_key(Some(MOCK_API_KEY));
    let repo = repository_with_key(&mock, "wrong-key");
    let health = repo.health_check().await;
    assert!(!health.ok, "wrong key");
    assert!(
        health.message.contains("Authentication rejected (401)"),
        "wrong key: {}",
        health.message
    );
    // Never the URL/host/key/headers.
    assert!(!health.message.contains("wrong-key"), "wrong key");
    assert!(!health.message.contains(MOCK_API_KEY), "wrong key");
    assert!(!health.message.contains("127.0.0.1"), "wrong key");
}

/// Verified slskd wire quirks on one mock: bitRate absent for lossless,
/// the extension from the filename, searchTimeout in milliseconds, a plain
/// enqueue array with PascalCase `Enqueued`, and usernames kept across
/// peers in the flattened downloads list.
#[tokio::test]
async fn slskd_wire_shapes() {
    let mock = MockSlskd::start().await.expect("mock starts");
    let repo = repository(&mock);

    let hits = repo
        .search_track("Radiohead", "Paranoid Android", Some("OK Computer"))
        .await
        .expect("search succeeds");

    // Multi-peer fan-out: 12 FLAC + 5 MP3 + 1 junk (the mock's canned set).
    assert_eq!(hits.len(), 18, "all three peers fan out: {hits:?}");
    let flac: Vec<_> = hits.iter().filter(|hit| hit.extension == "flac").collect();
    assert_eq!(flac.len(), 12, "bitrate");
    for hit in &flac {
        // bitRate ABSENT on the wire for lossless: None, never 0.
        assert_eq!(hit.bitrate, None, "lossless keeps bitrate None: {hit:?}");
        assert_eq!(hit.bit_depth, Some(16), "bitrate");
        assert_eq!(hit.sample_rate, Some(44100), "bitrate");
        // extension parsed from filename even though the wire field is "".
        assert_eq!(hit.extension, "flac", "extension");
        assert_eq!(
            hit.parent_directory, "Radiohead - OK Computer (1997)",
            "parent directory"
        );
    }
    let mp3: Vec<_> = hits.iter().filter(|hit| hit.extension == "mp3").collect();
    assert_eq!(mp3.len(), 6, "bitrate");
    assert!(mp3.iter().any(|hit| hit.bitrate == Some(320)), "bitrate");
    assert!(mp3.iter().any(|hit| hit.bitrate == Some(128)), "bitrate");

    mock.reset();
    repo.search_track("Radiohead", "Paranoid Android", None)
        .await
        .expect("search succeeds");
    // searchTimeout is MILLISECONDS (verified live).
    assert_eq!(mock.search_timeouts(), vec![2000], "search timeout");

    mock.reset();
    let handle = repo
        .enqueue(&[
            EnqueueFile {
                username: "alice".to_owned(),
                filename: "@@music\\Radiohead - OK Computer (1997)\\01 Track 1.flac".to_owned(),
                size: 30_000_000,
            },
            EnqueueFile {
                username: "alice".to_owned(),
                filename: "@@music\\Radiohead - OK Computer (1997)\\02 Track 2.flac".to_owned(),
                size: 30_000_000,
            },
        ])
        .await
        .expect("enqueue succeeds");
    // No batch GUID: correlation is (username, filenames).
    assert_eq!(handle.source, "soulseek", "enqueue");
    assert_eq!(handle.username, "alice", "enqueue");
    assert_eq!(handle.filenames.len(), 2, "enqueue");
    let status = repo.get_status(&handle).await.expect("status succeeds");
    assert_eq!(status.status, "completed", "enqueue");
    assert_eq!(status.files_completed, 2, "enqueue");
    assert_eq!(status.matched_transfers, 2, "enqueue");

    mock.reset();
    let client = SlskdClient::new(ReqwestSlskdHttp::new(
        reqwest::Client::new(),
        &mock.base_url(),
        MOCK_API_KEY,
    ));
    repo.enqueue(&[EnqueueFile {
        username: "alice".to_owned(),
        filename: "a.flac".to_owned(),
        size: 10,
    }])
    .await
    .expect("alice enqueues");
    repo.enqueue(&[EnqueueFile {
        username: "bob".to_owned(),
        filename: "b.mp3".to_owned(),
        size: 10,
    }])
    .await
    .expect("bob enqueues");
    // The flatten loses the username, so it is carried down from the
    // per-user block.
    let mut owners: Vec<String> = client
        .get_all_downloads()
        .await
        .expect("all downloads succeed")
        .iter()
        .map(|transfer| transfer.username.clone())
        .collect();
    owners.sort();
    assert_eq!(
        owners,
        vec!["alice".to_owned(), "bob".to_owned()],
        "usernames across peers"
    );
}

// Single-op tests.

/// A 429 on enqueue is retried with backoff; one whose `Retry-After` asks
/// for longer than a call may wait is not hammered: the call stops at once
/// and carries the wait to its caller.
#[tokio::test]
async fn rate_limits_are_retried_or_returned() {
    let mock = MockSlskd::start().await.expect("mock starts");
    let repo = repository(&mock);
    mock.fail_next_enqueue();
    let handle = repo
        .enqueue(&[EnqueueFile {
            username: "alice".to_owned(),
            filename: "a.flac".to_owned(),
            size: 10,
        }])
        .await
        .expect("one 429 is retried into success");
    assert_eq!(handle.filenames, vec!["a.flac".to_owned()], "short 429");

    mock.reset();
    for _ in 0..4 {
        mock.statuses().answer_next(429, Some("3600"));
    }
    let client = SlskdClient::new(ReqwestSlskdHttp::new(
        reqwest::Client::new(),
        &mock.base_url(),
        MOCK_API_KEY,
    ));
    let err = client.health_check().await.expect_err("rate limited");
    assert_eq!(
        err,
        ClientError::RateLimited {
            service: Service::Slskd,
            retry_after: Some(Duration::from_secs(3600)),
        },
        "long Retry-After"
    );
    assert_eq!(
        mock.statuses().requests(),
        1,
        "long Retry-After: no retry inside the wait"
    );
}

/// Any Soulseek user picks their own name. One holding `/`, `?` or `#`
/// reaches slskd as one path segment on every call that names a peer, and
/// a name no segment can carry (`..`) is refused before anything is sent.
#[tokio::test]
async fn a_peer_name_is_one_path_segment() {
    let mock = MockSlskd::start().await.expect("mock starts");
    let client = SlskdClient::new(ReqwestSlskdHttp::new(
        reqwest::Client::new(),
        &mock.base_url(),
        MOCK_API_KEY,
    ));
    let peer = "dj/../a?b#c";
    let file = r"@@music\Artist\Album\01 - Song.flac".to_owned();

    let enqueued = client
        .enqueue(peer, &[(file.clone(), 1000)])
        .await
        .expect("enqueue reaches the peer's route");
    assert_eq!(enqueued.enqueued.len(), 1, "{enqueued:?}");
    let transfers = client.get_downloads(peer).await.expect("peer's transfers");
    assert_eq!(transfers.len(), 1, "{transfers:?}");
    assert_eq!(transfers[0].username, peer);
    assert_eq!(transfers[0].filename, file);
    assert!(
        client
            .cancel_transfer(peer, &transfers[0].id)
            .await
            .expect("removal reaches the peer's route"),
        "the transfer is removed"
    );

    let sent = mock.statuses().requests();
    let refused = client
        .enqueue("..", &[(file, 1000)])
        .await
        .expect_err("no segment carries the name");
    assert!(
        matches!(refused, ClientError::BadRequest { .. }),
        "{refused:?}"
    );
    assert_eq!(mock.statuses().requests(), sent, "nothing is sent");
}

/// Scripted slskd answers, served in order, for the paused-clock test.
struct ScriptedHttp(Mutex<VecDeque<HttpReply>>);

impl ScriptedHttp {
    fn next(&self) -> Result<HttpReply, HttpFault> {
        self.0
            .lock()
            .expect("lock")
            .pop_front()
            .ok_or_else(|| HttpFault("no scripted answer left".to_owned()))
    }
}

impl SlskdHttp for ScriptedHttp {
    async fn get(&self, _: &[&str], _: &[(&str, &str)]) -> Result<HttpReply, HttpFault> {
        self.next()
    }
    async fn post(&self, _: &[&str], _: Vec<u8>) -> Result<HttpReply, HttpFault> {
        self.next()
    }
    async fn delete(&self, _: &[&str], _: &[(&str, &str)]) -> Result<HttpReply, HttpFault> {
        self.next()
    }
}

/// A 429 naming a short wait is waited out in full, then retried.
#[tokio::test(start_paused = true)]
async fn a_short_retry_after_is_waited_out_then_retried() {
    let client = SlskdClient::new(ScriptedHttp(Mutex::new(VecDeque::from([
        HttpReply {
            status: 429,
            retry_after: Some(Duration::from_secs(7)),
            body: Vec::new(),
        },
        HttpReply {
            status: 200,
            retry_after: None,
            body: br#"{"version": {"current": "0.25.1.0"}}"#.to_vec(),
        },
    ]))));
    let started = tokio::time::Instant::now();

    client.health_check().await.expect("retried into success");

    assert!(
        started.elapsed() >= Duration::from_secs(7),
        "{:?}",
        started.elapsed()
    );
}

/// Each refusal keeps its own meaning: a wrong key, a missing item, a bad
/// request and a rate limit with its wait never fold into one another.
#[tokio::test]
async fn statuses_map_to_distinct_client_errors() {
    let mock = MockSlskd::start().await.expect("mock starts");
    let client = SlskdClient::new(ReqwestSlskdHttp::new(
        reqwest::Client::new(),
        &mock.base_url(),
        MOCK_API_KEY,
    ));
    let mut seen = Vec::new();
    for (status, retry_after) in [(401, None), (404, None), (400, None), (429, Some("3600"))] {
        mock.statuses().answer_next(status, retry_after);
        seen.push(client.get_options().await.expect_err("scripted status"));
    }

    assert!(
        matches!(
            seen[0],
            ClientError::Auth {
                status: Some(401),
                ..
            }
        ),
        "{seen:?}"
    );
    assert!(matches!(seen[1], ClientError::NotFound { .. }), "{seen:?}");
    assert!(
        matches!(seen[2], ClientError::BadRequest { .. }),
        "{seen:?}"
    );
    assert_eq!(seen[3].retry_after(), Some(Duration::from_secs(3600)));
}

/// An answer missing its identity field fails decoding instead of
/// reading as an empty id.
#[test]
fn answers_without_their_identity_fail_to_decode() {
    let search = serde_json::json!({"state": "Completed, Succeeded", "isComplete": true});
    assert!(serde_json::from_value::<SlskdSearchResponse>(search).is_err());
    let peer = serde_json::json!({"files": [{"filename": "a.flac", "size": 1}]});
    assert!(serde_json::from_value::<SlskdUserSearchResponse>(peer).is_err());
    let nameless = serde_json::json!({"username": "alice", "files": [{"size": 1}]});
    assert!(serde_json::from_value::<SlskdUserSearchResponse>(nameless).is_err());
}

#[tokio::test]
async fn concurrent_enqueues_serialize_through_semaphore_one() {
    let mock = MockSlskd::start().await.expect("mock starts");
    let repo = repository(&mock);
    mock.set_handler_delay_ms(150);

    let first_files = [EnqueueFile {
        username: "alice".to_owned(),
        filename: "a.flac".to_owned(),
        size: 10,
    }];
    let second_files = [EnqueueFile {
        username: "bob".to_owned(),
        filename: "b.flac".to_owned(),
        size: 10,
    }];
    let (first, second) = tokio::join!(repo.enqueue(&first_files), repo.enqueue(&second_files));
    first.expect("first enqueue succeeds");
    second.expect("second enqueue succeeds");

    // slskd permits only one concurrent enqueue; the repository owns a
    // Semaphore(1), so the mock never sees overlap.
    assert_eq!(mock.max_in_flight(), 1);
}

// Search fan-out tests.

/// The album ladder: specific rungs first with wildcard siblings, the
/// primary artist only, deduped; and a search falls back past silent
/// specific rungs to a broader one that answers.
#[tokio::test]
async fn album_ladder() {
    // Multi-artist credits query the primary artist only: Soulseek ANDs
    // every term (issue #373).
    let ladder = album_query_ladder("YMO, Some Guest", "Solid State Survivor", Some(1979));
    assert_eq!(ladder[0], "YMO Solid State Survivor 1979", "built");
    // The blocked-artist wildcard sibling follows the exact rung.
    assert_eq!(
        ladder[1],
        "YMO Solid State Survivor 1979".replace("YMO", "*MO"),
        "built"
    );
    assert!(
        ladder.contains(&"YMO Solid State Survivor".to_owned()),
        "built: {ladder:?}"
    );
    assert!(ladder.contains(&"YMO".to_owned()), "built: {ladder:?}");
    assert!(
        !ladder.iter().any(|query| query.contains("Guest")),
        "built: {ladder:?}"
    );
    // Deduped and non-empty throughout.
    let mut seen = std::collections::HashSet::new();
    for query in &ladder {
        assert!(!query.is_empty(), "built: {ladder:?}");
        assert!(seen.insert(query.clone()), "ladder dedupes: {ladder:?}");
    }

    let mock = MockSlskd::start().await.expect("mock starts");
    let repo = repository(&mock);
    let hits = repo
        .search_album("Radiohead", "OK Computer", Some(1997))
        .await
        .expect("ladder search succeeds");
    assert_eq!(hits.len(), 18, "walked: a broader rung fans out");
    let texts = mock.search_texts();
    assert!(
        texts.len() >= 2,
        "the year rungs ran and came back empty: {texts:?}"
    );
    // The specific rung leads (verified-live escalation).
    assert!(
        texts[0].contains("1997"),
        "most-specific rung first: {texts:?}"
    );
    assert!(
        !texts.last().unwrap().contains("1997"),
        "a broader rung answered: {texts:?}"
    );
}

// Correlation + status tests.

/// Correlation and status on one mock: a handle keeps only the filenames
/// slskd accepted, each file is judged by its latest attempt, a truncated
/// stub flagged succeeded is failed, an exact spelling wins before a single
/// NFC alias, and abort and discard remove the matched records.
#[tokio::test]
async fn status_matches_and_judges_each_transfer() {
    let mock = MockSlskd::start().await.expect("mock starts");
    let repo = repository(&mock);

    let handle = repo
        .enqueue(&[
            EnqueueFile {
                username: "alice".to_owned(),
                filename: "good.flac".to_owned(),
                size: 10,
            },
            EnqueueFile {
                username: "alice".to_owned(),
                filename: "bad-REJECT-ME.flac".to_owned(),
                size: 10,
            },
        ])
        .await
        .expect("enqueue succeeds");
    // The correlation key reflects what slskd accepted, not the input set,
    // or status/cancel poll forever on transfers never created.
    assert_eq!(handle.filenames, vec!["good.flac".to_owned()], "accepted");
    let status = repo.get_status(&handle).await.expect("status succeeds");
    assert_eq!(status.status, "completed", "accepted");
    assert_eq!(status.matched_transfers, 1, "accepted");

    // Stale Succeeded shadowed by a newer TimedOut (issues #131/#253): the file
    // is failed, not completed.
    mock.reset();
    mock.inject_transfer(
        "alice",
        "stale.flac",
        100,
        "Completed, Succeeded",
        Some("2026-01-01T00:00:00Z"),
        None,
    );
    mock.inject_transfer(
        "alice",
        "stale.flac",
        100,
        "Completed, TimedOut",
        Some("2026-02-01T00:00:00Z"),
        None,
    );
    let handle = TaskHandle::new("alice", vec!["stale.flac".to_owned()]);
    let status = repo.get_status(&handle).await.expect("status succeeds");
    assert_eq!(status.status, "failed", "latest attempt");
    assert_eq!(status.files_failed, 1, "latest attempt");
    assert_eq!(status.files_completed, 0, "latest attempt");
    // Byte totals stay sum-over-all-records: both attempts count.
    assert_eq!(status.bytes_total, 200, "latest attempt");

    // And vice versa: a newer Succeeded beats an older Errored.
    mock.inject_transfer(
        "bob",
        "recovered.flac",
        100,
        "Completed, Errored",
        Some("2026-01-01T00:00:00Z"),
        None,
    );
    mock.inject_transfer(
        "bob",
        "recovered.flac",
        100,
        "Completed, Succeeded",
        Some("2026-02-01T00:00:00Z"),
        None,
    );
    let handle = TaskHandle::new("bob", vec!["recovered.flac".to_owned()]);
    let status = repo.get_status(&handle).await.expect("status succeeds");
    assert_eq!(status.status, "completed", "latest attempt");
    assert_eq!(
        status.succeeded_filenames,
        vec!["recovered.flac".to_owned()],
        "latest attempt"
    );

    // Succeeded flag but only half the bytes moved: a truncated stub (issue
    // #122): never importable, so failed when terminal.
    mock.reset();
    mock.inject_transfer_progress(
        "alice",
        "stub.flac",
        100,
        50,
        "Completed, Succeeded",
        Some("2026-02-01T00:00:00Z"),
        None,
    );
    let handle = TaskHandle::new("alice", vec!["stub.flac".to_owned()]);
    let status = repo.get_status(&handle).await.expect("status succeeds");
    assert_eq!(status.status, "failed", "truncated stub");
    assert_eq!(status.files_failed, 1, "truncated stub");

    mock.reset();
    mock.inject_transfer(
        "alice",
        "caf\u{e9}.flac",
        100,
        "Completed, Succeeded",
        None,
        None,
    );
    let handle = TaskHandle::new("alice", vec!["cafe\u{301}.flac".to_owned()]);
    let status = repo.get_status(&handle).await.expect("status succeeds");
    // One distinct NFC-equivalent spelling is claimed.
    assert_eq!(status.matched_transfers, 1, "nfc alias");
    assert_eq!(status.status, "completed", "nfc alias");
    assert!(
        aggregate_status(&handle, &[]).status == "queued",
        "nfc alias"
    );
    assert!(match_transfers(&handle, &[]).is_empty(), "nfc alias");

    mock.reset();
    let handle = repo
        .enqueue(&[EnqueueFile {
            username: "alice".to_owned(),
            filename: "doomed.flac".to_owned(),
            size: 10,
        }])
        .await
        .expect("enqueue succeeds");
    assert!(repo.abort(&handle).await.expect("abort succeeds"), "abort");
    let status = repo.get_status(&handle).await.expect("status succeeds");
    assert_eq!(status.matched_transfers, 0, "abort");
    assert_eq!(status.status, "queued", "abort");
    // Post-import record removal shares the same path.
    let second = repo
        .enqueue(&[EnqueueFile {
            username: "alice".to_owned(),
            filename: "imported.flac".to_owned(),
            size: 10,
        }])
        .await
        .expect("enqueue succeeds");
    assert!(
        repo.discard_client_artifacts(&second)
            .await
            .expect("discard succeeds"),
        "discard"
    );
}

// Locator tests.

/// The locator finds only this transfer's file: the leaf, flat and
/// username layouts, never through traversal or a link, never another
/// peer's same-named leftover, and partial files only by basename inside
/// the incomplete mount.
#[tokio::test]
async fn locator_finds_only_this_transfers_file() {
    let mount = temp_mount("locate");
    std::fs::create_dir_all(mount.join("OK Computer")).expect("leaf dir");
    std::fs::create_dir_all(mount.join("alice").join("deep")).expect("user dir");
    std::fs::write(mount.join("OK Computer").join("01 Track 1.flac"), b"leaf").expect("leaf file");
    std::fs::write(mount.join("flat.flac"), b"flat").expect("flat file");
    std::fs::write(
        mount.join("alice").join("deep").join("buried.flac"),
        b"buried",
    )
    .expect("user file");
    let locator = Locator::new(mount.to_path_buf(), None);

    // Step 1: {mount}/{leaf remote folder}/{filename}.
    let hit = locator
        .locate_file(
            "alice",
            "@@music\\Radiohead - OK Computer\\OK Computer\\01 Track 1.flac",
            None,
        )
        .expect("leaf layout resolves");
    assert!(
        hit.starts_with(mount.canonicalize().expect("mount resolves")),
        "leaf layout"
    );
    assert_eq!(hit.file_name().unwrap(), "01 Track 1.flac", "leaf layout");

    // Step 2: flat layout.
    let hit = locator
        .locate_file("alice", "@@music\\Other\\flat.flac", None)
        .expect("flat layout resolves");
    assert_eq!(hit.file_name().unwrap(), "flat.flac", "flat layout");

    // Step 3: {mount}/{username}/ at any depth.
    let hit = locator
        .locate_file("alice", "@@music\\Other\\buried.flac", None)
        .expect("username layout resolves");
    assert_eq!(hit.file_name().unwrap(), "buried.flac", "username layout");

    // Traversal is refused outright.
    assert_eq!(
        locator.locate_file("alice", "..\\..\\etc\\passwd", None),
        None,
        "traversal"
    );
    assert_eq!(locator.locate_file("alice", "", None), None, "empty name");

    // A link is never followed, even to a file inside the mount: importing
    // through it would remove the other download's file.
    #[cfg(unix)]
    {
        std::fs::create_dir_all(mount.join("Linked")).expect("link dir");
        std::os::unix::fs::symlink(
            mount.join("OK Computer").join("01 Track 1.flac"),
            mount.join("Linked").join("09 Link.flac"),
        )
        .expect("file link");
        std::os::unix::fs::symlink(mount.join("OK Computer"), mount.join("Alias"))
            .expect("folder link");
        assert_eq!(
            locator.locate_file("alice", "@@music\\Linked\\09 Link.flac", None),
            None,
            "file link"
        );
        assert_eq!(
            locator.locate_file("alice", "@@music\\Alias\\01 Track 1.flac", None),
            Some(
                mount
                    .canonicalize()
                    .expect("mount resolves")
                    .join("OK Computer")
                    .join("01 Track 1.flac")
            ),
            "found where it really is, never through the linked folder"
        );
    }

    // A same-named file with the wrong bytes is another peer's stale
    // leftover, not this transfer (issue #397).
    let mount = temp_mount("sizes");
    std::fs::write(mount.join("stale.flac"), b"tiny").expect("stale file");
    let locator = Locator::new(mount.to_path_buf(), None);
    assert_eq!(
        locator.locate_file("alice", "stale.flac", Some(1_000_000)),
        None,
        "size mismatch"
    );
    // Unknown size keeps the old name-only behavior.
    assert!(
        locator.locate_file("alice", "stale.flac", None).is_some(),
        "unknown size"
    );

    let downloads = temp_mount("downloads");
    let incomplete = temp_mount("incomplete");
    std::fs::create_dir_all(incomplete.join("OK Computer")).expect("album dir");
    std::fs::write(incomplete.join("OK Computer").join("part.flac"), b"partial")
        .expect("partial file");
    let locator = Locator::new(downloads.to_path_buf(), Some(incomplete.to_path_buf()));
    // The incomplete layout is not username-scoped.
    let hit = locator
        .locate_partial("anyone", "@@music\\X\\part.flac", Some(30_000_000))
        .expect("partial resolves");
    assert_eq!(hit.file_name().unwrap(), "part.flac", "partial");
    // No incomplete mount disables the fallback entirely.
    let bare_mount = temp_mount("bare");
    let bare = Locator::new(bare_mount.to_path_buf(), None);
    assert_eq!(
        bare.locate_partial("anyone", "part.flac", Some(1)),
        None,
        "no incomplete mount"
    );

    // The repository looks for a partial file in its incomplete mount.
    let mock = MockSlskd::start().await.expect("mock starts");
    let downloads = temp_mount("repo-downloads");
    let incomplete = temp_mount("repo-incomplete");
    std::fs::write(incomplete.join("stranded.flac"), b"partial").expect("partial file");
    let http = ReqwestSlskdHttp::new(reqwest::Client::new(), &mock.base_url(), MOCK_API_KEY);
    let repo = SlskdRepository::new(
        SlskdClient::new(http),
        &mock.base_url(),
        MOCK_API_KEY,
        downloads.to_path_buf(),
        test_policy(),
    )
    .with_incomplete_mount(incomplete.to_path_buf());
    let handle = TaskHandle::new("alice", vec!["stranded.flac".to_owned()]);
    let hit = repo
        .locate_partial(&handle, "@@music\\X\\stranded.flac", Some(30_000_000))
        .await
        .expect("partial lookup succeeds")
        .expect("partial resolves");
    assert_eq!(
        hit.file_name().unwrap(),
        "stranded.flac",
        "repository partial"
    );
}

/// The downloads subfolder from settings sits in the volume slskd writes,
/// so slskd's host could swap it for a link to any folder the server can
/// read. Only the admin's root may be a link: a linked subfolder finds
/// nothing and confines nothing, whether a path names it or its target.
#[cfg(unix)]
#[test]
fn a_downloads_subfolder_that_is_a_link_is_refused() {
    use droppedneedle_acquire::landing::DownloadsFolder;
    use droppedneedle_acquire::landing::confine::{Place, place};

    let scratch = temp_mount("subpath-link");
    let real_root = scratch.join("volume");
    let elsewhere = scratch.join("elsewhere");
    std::fs::create_dir_all(real_root.join("complete").join("Album")).expect("subfolder");
    std::fs::create_dir_all(elsewhere.join("Album")).expect("outside folder");
    std::fs::write(real_root.join("complete/Album/01.flac"), b"ours").expect("file");
    std::fs::write(elsewhere.join("Album/01.flac"), b"secret").expect("file");
    // The admin's root may itself be a link.
    let root = scratch.join("root-link");
    std::os::unix::fs::symlink(&real_root, &root).expect("root link");
    let folder = DownloadsFolder::with_subpath(root.clone(), "complete");

    let found = Locator::new(folder.clone(), None)
        .locate_file("alice", "@@music\\Album\\01.flac", None)
        .expect("a real subfolder under a linked root is read");
    assert_eq!(std::fs::read(&found).expect("read"), b"ours");
    assert!(matches!(
        place(
            &root.join("complete/Album/01.flac"),
            std::slice::from_ref(&folder)
        ),
        Place::Inside(_)
    ));

    std::fs::rename(real_root.join("complete"), scratch.join("moved")).expect("move aside");
    std::os::unix::fs::symlink(&elsewhere, real_root.join("complete")).expect("subfolder link");

    assert_eq!(
        Locator::new(folder.clone(), None).locate_file("alice", "@@music\\Album\\01.flac", None),
        None
    );
    for path in [
        root.join("complete/Album/01.flac"),
        real_root.join("complete/Album/01.flac"),
        elsewhere.join("Album/01.flac"),
    ] {
        assert_eq!(
            place(&path, std::slice::from_ref(&folder)),
            Place::Outside,
            "{}",
            path.display()
        );
    }
    assert_eq!(
        std::fs::read(elsewhere.join("Album/01.flac")).expect("kept"),
        b"secret"
    );
}

// A failover walks every peer: each attempt takes the best group the task
// has not tried yet, even as failed peers are blocklisted and the search
// result shrinks, so no candidate is skipped.
#[tokio::test]
async fn failover_walks_every_peer() {
    use droppedneedle_acquire::downloads::state::AttemptState;
    use droppedneedle_acquire::sources::SlskdSource;

    let mock = MockSlskd::start().await.expect("mock starts");
    let db = AcquireDb::scratch().expect("scratch db");
    db.add_user("u1", "U", "user").await.expect("user seeds");
    let journal = std::sync::Arc::new(Journal::new(db, quiet_ticks()));
    journal
        .run("test.seed", |store| {
            store.insert_task(
                &NewTask {
                    id: "t1".to_owned(),
                    user_id: "u1".to_owned(),
                    artist_name: "Massive Attack".to_owned(),
                    album_title: "Blue Lines".to_owned(),
                    release_group_mbid: "rg-1".to_owned(),
                    origin: "user".to_owned(),
                    retry_count: 0,
                },
                1.0,
            )
        })
        .await
        .expect("task seeds");
    let source = SlskdSource::new(
        std::sync::Arc::new(repository(&mock)),
        journal.clone(),
        no_targets(&journal),
    );

    let mut peers: Vec<String> = Vec::new();
    for index in 0..10 {
        let Ok(handle) = source.enqueue("t1", index).await else {
            break;
        };
        let json = serde_json::to_string(&handle).expect("handle encodes");
        let attempt = format!("t1-a{index}");
        let (user, files) = (handle.username.clone(), handle.filenames.clone());
        journal
            .run("test.fail", move |store| {
                store.insert_attempt(
                    &attempt,
                    "t1",
                    "soulseek",
                    index,
                    "",
                    &json,
                    AttemptState::Complete,
                    1.0,
                )?;
                // The worker blocklists a failed peer's files.
                for file in &files {
                    store.record_quarantine(
                        "soulseek",
                        &droppedneedle_acquire::downloads::quarantine::canonical_soulseek_identity(
                            &format!("{user}/{file}"),
                        ),
                        "failed",
                        None,
                        1.0e12,
                        86_400.0,
                    )?;
                }
                Ok(())
            })
            .await
            .expect("attempt records");
        peers.push(handle.username);
    }
    let mut distinct = peers.clone();
    distinct.sort();
    distinct.dedup();
    assert_eq!(
        distinct.len(),
        peers.len(),
        "no peer is tried twice: {peers:?}"
    );
    // Charlie's 128 kbps MP3 is below the default band, so it is never
    // offered; every other canned peer is tried.
    assert_eq!(
        distinct,
        ["alice", "bob"],
        "every peer in the band is tried"
    );
}

// Quality policy at the pick, through the production client builder.

/// A complete two-track album folder: FLAC or APE at 16-bit, or MP3 at 320 kbps.
fn album_peer(username: &str, extension: &str) -> serde_json::Value {
    let files: Vec<serde_json::Value> = ["Alpha", "Beta"]
        .iter()
        .enumerate()
        .map(|(index, title)| {
            let mut file = serde_json::json!({
                "filename": format!("@@music\\Artist\\Album\\0{} {title}.{extension}", index + 1),
                "size": 30_000_000, "extension": extension, "length": 200 + index,
            });
            if matches!(extension, "flac" | "ape") {
                file["bitDepth"] = 16.into();
                file["sampleRate"] = 44_100.into();
            } else {
                file["bitRate"] = 320.into();
            }
            file
        })
        .collect();
    serde_json::json!({"username": username, "hasFreeUploadSlot": true, "uploadSpeed": 1_000_000,
        "queueLength": 0, "fileCount": files.len(), "lockedFileCount": 0,
        "files": files, "lockedFiles": [], "token": 1})
}

/// A saved policy keeping one tier, with a one-entry recipe, each source
/// grabbing its own best guess (no search review).
fn one_tier_policy(tier: &str, format: RecipeFormat, quality: RecipeQuality) -> KernelPolicy {
    KernelPolicy {
        quality_min: tier.to_owned(),
        quality_max: tier.to_owned(),
        quality_cutoff: tier.to_owned(),
        quality_recipe: vec![QualityRecipeEntry {
            format,
            quality,
            min_bitrate_kbps: (format == RecipeFormat::Mp3).then_some(320),
            max_bitrate_kbps: None,
            bit_depth: None,
            sample_rate_hz: None,
        }],
        search_review: "off".to_owned(),
        ..KernelPolicy::default()
    }
}

/// The peer the worker enqueued `task_id` from.
async fn enqueued_peer(journal: &Journal, task_id: &str) -> String {
    let handles = journal
        .read_source_handles(task_id, &SourceTag::Soulseek)
        .await
        .expect("handles read");
    assert_eq!(handles.len(), 1, "one enqueue: {handles:?}");
    let handle: SourceHandle = serde_json::from_str(&handles[0]).expect("handle decodes");
    handle.username
}

async fn seed_album_task(journal: &Journal, task_id: &'static str) {
    journal
        .run("test.seed", move |store| {
            store.insert_task(
                &NewTask {
                    id: task_id.to_owned(),
                    user_id: "u1".to_owned(),
                    artist_name: "Artist".to_owned(),
                    album_title: "Album".to_owned(),
                    release_group_mbid: "33333333-3333-4333-8333-333333333333".to_owned(),
                    origin: "user".to_owned(),
                    retry_count: 0,
                },
                1_000.0,
            )
        })
        .await
        .expect("task seeds");
}

// Under a saved MP3-only policy the worker fetches the MP3 folder and a
// search offers the FLAC one only last, as outside the settings. A
// FLAC-only policy saved afterwards
// steers the next search, with no restart.
#[tokio::test]
async fn soulseek_picks_follow_the_saved_quality_policy() {
    let mock = MockSlskd::start().await.expect("mock starts");
    mock.script_responses(vec![
        album_peer("lossless", "flac"),
        album_peer("lossy", "mp3"),
    ]);
    let db = AcquireDb::scratch().expect("scratch db");
    db.add_user("u1", "U", "user").await.expect("user seeds");
    let dir = db.path().parent().expect("scratch dir").to_path_buf();
    let settings = droppedneedle_acquire::test_support::scratch_settings(&dir.join("settings"))
        .expect("settings");
    settings
        .save_secret_blocking(SlskdConnection {
            enabled: true,
            url: mock.base_url(),
            api_key: MOCK_API_KEY.into(),
            ..SlskdConnection::default()
        })
        .expect("slskd connection saves");
    settings
        .save_blocking(one_tier_policy(
            "mp3_320",
            RecipeFormat::Mp3,
            RecipeQuality::Kbps320Plus,
        ))
        .expect("an MP3-only policy saves");

    let journal = Arc::new(Journal::new(db.clone(), quiet_ticks()));
    let (worker, _) = worker_over(
        &db,
        journal.clone(),
        &settings,
        dir.join("slskd"),
        Arc::new(FixedAlbums::default()),
        None,
        droppedneedle_kernel::events::EventHub::new(),
    )
    .await;
    let target = SearchTarget {
        artist: "Artist".to_owned(),
        album_title: "Album".to_owned(),
        year: None,
        tracklist: Vec::new(),
        wanted: Vec::new(),
        track: None,
    };

    seed_album_task(&journal, "task-mp3-only").await;
    worker.run_once(1).await;
    assert_eq!(enqueued_peer(&journal, "task-mp3-only").await, "lossy");
    let sources = worker.sources_now();
    let offered: Vec<(String, bool)> = sources[0]
        .search(&target)
        .await
        .expect("search runs")
        .into_iter()
        .filter_map(|candidate| {
            let refused = candidate.is_refused();
            candidate.view.username.map(|peer| (peer, refused))
        })
        .collect();
    assert_eq!(
        offered,
        [("lossy".to_owned(), false), ("lossless".to_owned(), true)],
        "the FLAC folder is offered last, as outside the settings"
    );

    settings
        .save_blocking(one_tier_policy(
            "lossless",
            RecipeFormat::Flac,
            RecipeQuality::Cd,
        ))
        .expect("a FLAC-only policy saves");
    seed_album_task(&journal, "task-flac-only").await;
    worker.run_once(2).await;
    assert_eq!(enqueued_peer(&journal, "task-flac-only").await, "lossless");
}

// A file counts as audio only when the landing can read it. APE has no
// reader there, so an APE folder is never offered, alone or beside a FLAC
// folder of the same album.
#[tokio::test]
async fn soulseek_counts_only_readable_files_as_audio() {
    use droppedneedle_acquire::sources::SlskdSource;

    let mock = MockSlskd::start().await.expect("mock starts");
    let db = AcquireDb::scratch().expect("scratch db");
    let journal = Arc::new(Journal::new(db, quiet_ticks()));
    let source = SlskdSource::new(
        Arc::new(repository(&mock)),
        journal.clone(),
        no_targets(&journal),
    );
    let target = SearchTarget {
        artist: "Artist".to_owned(),
        album_title: "Album".to_owned(),
        year: None,
        tracklist: Vec::new(),
        wanted: Vec::new(),
        track: None,
    };

    mock.script_responses(vec![
        album_peer("monkeys", "ape"),
        album_peer("lossless", "flac"),
    ]);
    let offered: Vec<String> = source
        .search(&target)
        .await
        .expect("search runs")
        .into_iter()
        .filter_map(|candidate| candidate.view.username)
        .collect();
    assert_eq!(offered, ["lossless"], "the APE folder is not offered");

    mock.script_responses(vec![album_peer("monkeys", "ape")]);
    let offered = source.search(&target).await.expect("search runs");
    assert!(offered.is_empty(), "an APE-only folder is never offered");
}
