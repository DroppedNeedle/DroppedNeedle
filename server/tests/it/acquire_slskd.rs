//! slskd/Soulseek per-client contract briefs.
//!
//! Brief-first coverage of the stage-7 slskd slice against the in-repo mock
//! server on loopback: wire-shape briefs (bitRate-absent lossless,
//! extension-from-filename, comma-joined state flags, PascalCase enqueue,
//! searchTimeout milliseconds), single-op briefs (429 retry, semaphore
//! serialization), search fan-out briefs (ladder fallback past silent
//! specific rungs, multi-peer fan-out), correlation briefs
//! (accepted-filenames handles, latest-attempt status, truncated stubs),
//! and policy briefs (recipe validation/ranking, lossless ladder, query
//! construction). No live contact ever.

use droppedneedle::acquire::slskd;

use std::path::PathBuf;
use std::time::Duration;

use slskd::{
    DownloadPolicy, EnqueueFile, Locator, MOCK_API_KEY, MockSlskd, QualityRecipeEntry,
    ReqwestSlskdHttp, SlskdClient, SlskdError, SlskdRepository, TaskHandle, aggregate_status,
    album_query_ladder, extension_from_filename, lossless_detail_step, match_transfers,
    sanitize_query, state_flags, stripped_album_title, track_query_ladder, validate_quality_recipe,
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
    let mount = std::env::temp_dir().join(format!("slskd-brief-{}", std::process::id()));
    SlskdRepository::new(
        SlskdClient::new(http),
        &mock.base_url(),
        api_key,
        mount,
        test_policy(),
    )
}

fn temp_mount(tag: &str) -> PathBuf {
    let mount = std::env::temp_dir().join(format!(
        "slskd-brief-{}-{}-{}",
        std::process::id(),
        tag,
        uuid::Uuid::new_v4().simple()
    ));
    std::fs::create_dir_all(&mount).expect("brief mount creates");
    mount
}

// Wire-shape briefs.

#[tokio::test]
async fn bitrate_absent_lossless_stays_none_and_extension_comes_from_filename() {
    let mock = MockSlskd::start().await.expect("mock starts");
    let repo = repository(&mock);

    let hits = repo
        .search_track("Radiohead", "Paranoid Android", Some("OK Computer"))
        .await
        .expect("search succeeds");

    // Multi-peer fan-out: 12 FLAC + 5 MP3 + 1 junk (v2 mock canned set).
    assert_eq!(hits.len(), 18, "all three peers fan out: {hits:?}");
    let flac: Vec<_> = hits.iter().filter(|hit| hit.extension == "flac").collect();
    assert_eq!(flac.len(), 12);
    for hit in &flac {
        // bitRate ABSENT on the wire for lossless (v2 C6b): None, never 0.
        assert_eq!(hit.bitrate, None, "lossless keeps bitrate None: {hit:?}");
        assert_eq!(hit.bit_depth, Some(16));
        assert_eq!(hit.sample_rate, Some(44100));
        // extension parsed from filename even though the wire field is ""
        // (v2 C6a).
        assert_eq!(hit.extension, "flac");
        assert_eq!(hit.parent_directory, "Radiohead - OK Computer (1997)");
    }
    let mp3: Vec<_> = hits.iter().filter(|hit| hit.extension == "mp3").collect();
    assert_eq!(mp3.len(), 6);
    assert!(mp3.iter().any(|hit| hit.bitrate == Some(320)));
    assert!(mp3.iter().any(|hit| hit.bitrate == Some(128)));
}

#[tokio::test]
async fn search_timeout_is_sent_in_milliseconds() {
    let mock = MockSlskd::start().await.expect("mock starts");
    let repo = repository(&mock);

    repo.search_track("Radiohead", "Paranoid Android", None)
        .await
        .expect("search succeeds");

    // searchTimeout is MILLISECONDS (v2: verified).
    assert_eq!(mock.search_timeouts(), vec![2000]);
}

#[tokio::test]
async fn enqueue_posts_a_plain_array_and_reads_pascal_case_enqueued() {
    let mock = MockSlskd::start().await.expect("mock starts");
    let repo = repository(&mock);

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

    // No batch GUID: correlation is (username, filenames) (v2 C2).
    assert_eq!(handle.source, "soulseek");
    assert_eq!(handle.username, "alice");
    assert_eq!(handle.filenames.len(), 2);

    let status = repo.get_status(&handle).await.expect("status succeeds");
    assert_eq!(status.status, "completed");
    assert_eq!(status.files_completed, 2);
    assert_eq!(status.matched_transfers, 2);
}

#[tokio::test]
async fn state_flags_split_comma_joined_succeeded() {
    assert!(state_flags("Completed, Succeeded").contains("succeeded"));
    assert!(state_flags("InProgress, Queued").contains("inprogress"));
    assert!(!state_flags("").contains("succeeded"));
}

#[tokio::test]
async fn unknown_user_downloads_are_empty_and_unknown_cancel_is_false() {
    let mock = MockSlskd::start().await.expect("mock starts");
    let http = ReqwestSlskdHttp::new(reqwest::Client::new(), &mock.base_url(), MOCK_API_KEY);
    let client = SlskdClient::new(http);

    // 404 means "no such bucket" / "record already gone" (v2).
    assert_eq!(
        client
            .get_downloads("nobody")
            .await
            .expect("404 -> empty")
            .len(),
        0
    );
    assert!(
        !client
            .cancel_transfer("nobody", "missing-id")
            .await
            .expect("404 -> false")
    );
}

#[tokio::test]
async fn all_downloads_preserves_usernames_across_peers() {
    let mock = MockSlskd::start().await.expect("mock starts");
    let repo = repository(&mock);
    let http = ReqwestSlskdHttp::new(reqwest::Client::new(), &mock.base_url(), MOCK_API_KEY);
    let client = SlskdClient::new(http);

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
    // per-user block (v2 `get_all_downloads`).
    let mut owners: Vec<String> = client
        .get_all_downloads()
        .await
        .expect("all downloads succeed")
        .iter()
        .map(|transfer| transfer.username.clone())
        .collect();
    owners.sort();
    assert_eq!(owners, vec!["alice".to_owned(), "bob".to_owned()]);
}

// Single-op briefs (v2 C3).

#[tokio::test]
async fn enqueue_429_is_retried_with_backoff() {
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

    assert_eq!(handle.filenames, vec!["a.flac".to_owned()]);
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
    // Semaphore(1) (v2 C3), so the mock never sees overlap.
    assert_eq!(mock.max_in_flight(), 1);
}

#[tokio::test]
async fn search_start_429_is_retried_with_backoff() {
    let mock = MockSlskd::start().await.expect("mock starts");
    let repo = repository(&mock);
    mock.fail_next_search();

    // Search START retries the 429 single-op limit like the discrete
    // calls: slskd answers 429 while another client holds the one search
    // slot, and an unretried start would drop the whole rung. Only the
    // state/response poll reads stay unretried (the repository owns that
    // deadline).
    let results = repo
        .search_track("Radiohead", "Paranoid Android", None)
        .await
        .expect("search start 429 retries");
    assert!(!results.is_empty());
}

#[tokio::test]
async fn rate_limited_error_is_distinct_and_retriable() {
    let limited = SlskdError::for_status(429, b"busy");
    assert_eq!(limited, SlskdError::RateLimited);
    assert!(limited.is_retriable());
    assert!(matches!(
        SlskdError::for_status(401, b"nope"),
        SlskdError::Auth { status: 401, .. }
    ));
    assert!(
        !SlskdError::Auth {
            status: 401,
            detail: String::new()
        }
        .is_retriable()
    );
}

// Search fan-out briefs.

#[tokio::test]
async fn album_ladder_falls_back_past_silent_specific_rungs() {
    let mock = MockSlskd::start().await.expect("mock starts");
    let repo = repository(&mock);

    let hits = repo
        .search_album("Radiohead", "OK Computer", Some(1997))
        .await
        .expect("ladder search succeeds");

    assert_eq!(hits.len(), 18, "a broader rung fans out");
    let texts = mock.search_texts();
    assert!(
        texts.len() >= 2,
        "the year rungs ran and came back empty: {texts:?}"
    );
    // The specific rung leads (verified-live escalation, v2 `search_album`).
    assert!(
        texts[0].contains("1997"),
        "most-specific rung first: {texts:?}"
    );
    assert!(
        !texts.last().unwrap().contains("1997"),
        "a broader rung answered: {texts:?}"
    );

    // Reset clears the recorded searches between briefs (v2 `reset_state`).
    mock.reset();
    assert!(mock.search_texts().is_empty());
    assert!(mock.search_timeouts().is_empty());
}

#[tokio::test]
async fn track_ladder_keeps_the_track_title_at_every_rung() {
    for query in track_query_ladder("Radiohead", "Paranoid Android", Some("OK Computer")) {
        assert!(
            query.contains("Paranoid Android"),
            "every rung keeps the track title: {query:?}"
        );
    }
    assert_eq!(
        track_query_ladder("Radiohead", "Paranoid Android", Some("OK Computer"))[0],
        "Radiohead Paranoid Android OK Computer"
    );
}

// Correlation + status briefs.

#[tokio::test]
async fn handle_correlates_accepted_filenames_only() {
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
    // or status/cancel poll forever on transfers never created (v2).
    assert_eq!(handle.filenames, vec!["good.flac".to_owned()]);
    let status = repo.get_status(&handle).await.expect("status succeeds");
    assert_eq!(status.status, "completed");
    assert_eq!(status.matched_transfers, 1);
}

#[tokio::test]
async fn status_judges_each_file_by_its_latest_attempt() {
    let mock = MockSlskd::start().await.expect("mock starts");
    let repo = repository(&mock);

    // Stale Succeeded shadowed by a newer TimedOut (v2 #131/#253): the file
    // is failed, not completed.
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
    assert_eq!(status.status, "failed");
    assert_eq!(status.files_failed, 1);
    assert_eq!(status.files_completed, 0);
    // Byte totals stay sum-over-all-records (v2): both attempts count.
    assert_eq!(status.bytes_total, 200);

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
    assert_eq!(status.status, "completed");
    assert_eq!(
        status.succeeded_filenames,
        vec!["recovered.flac".to_owned()]
    );
}

#[tokio::test]
async fn truncated_stub_flagged_succeeded_is_failed_not_completed() {
    let mock = MockSlskd::start().await.expect("mock starts");
    let repo = repository(&mock);

    // Succeeded flag but only half the bytes moved: a truncated stub (v2
    // #122) — never importable, so failed when terminal.
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
    assert_eq!(status.status, "failed");
    assert_eq!(status.files_failed, 1);
}

#[tokio::test]
async fn transfers_match_exact_spelling_first_then_single_nfc_alias() {
    let mock = MockSlskd::start().await.expect("mock starts");
    let repo = repository(&mock);

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
    // One distinct NFC-equivalent spelling is claimed (v2 `_match_transfers`).
    assert_eq!(status.matched_transfers, 1);
    assert_eq!(status.status, "completed");
    assert!(aggregate_status(&handle, &[]).status == "queued");
    assert!(match_transfers(&handle, &[]).is_empty());
}

#[tokio::test]
async fn abort_removes_matched_transfer_records() {
    let mock = MockSlskd::start().await.expect("mock starts");
    let repo = repository(&mock);

    let handle = repo
        .enqueue(&[EnqueueFile {
            username: "alice".to_owned(),
            filename: "doomed.flac".to_owned(),
            size: 10,
        }])
        .await
        .expect("enqueue succeeds");
    assert!(repo.abort(&handle).await.expect("abort succeeds"));

    let status = repo.get_status(&handle).await.expect("status succeeds");
    assert_eq!(status.matched_transfers, 0);
    assert_eq!(status.status, "queued");

    // Post-import record removal shares the same path (v2
    // `discard_client_artifacts` / DEC-1).
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
            .expect("discard succeeds")
    );
}

// Auth + health briefs.

#[tokio::test]
async fn wrong_key_health_message_is_uniform_and_leaks_nothing() {
    let mock = MockSlskd::start().await.expect("mock starts");
    mock.set_expected_api_key(Some(MOCK_API_KEY));
    let repo = repository_with_key(&mock, "wrong-key");

    let health = repo.health_check().await;
    assert!(!health.ok);
    assert!(health.message.contains("Authentication rejected (401)"));
    // Never the URL/host/key/headers (v2 `health_check`).
    assert!(!health.message.contains("wrong-key"));
    assert!(!health.message.contains(MOCK_API_KEY));
    assert!(!health.message.contains("127.0.0.1"));
}

#[tokio::test]
async fn health_check_reports_slskd_version() {
    let mock = MockSlskd::start().await.expect("mock starts");
    let repo = repository(&mock);

    let health = repo.health_check().await;
    assert!(health.ok);
    assert_eq!(health.version.as_deref(), Some("0.25.1.0"));
    assert_eq!(health.message, "slskd 0.25.1.0");
    assert_eq!(repo.client_name(), "slskd");
    assert!(repo.is_configured());
    assert_eq!(repo.policy().search_timeout, Duration::from_secs(2));
}

// Policy briefs (tiers, recipe, timeouts).

#[test]
fn recipe_validation_rejects_duplicates_and_overlaps() {
    let flac_cd =
        QualityRecipeEntry::new("flac", "cd", None, None, None, None, None).expect("valid");
    // Duplicate standard entries rejected (v2 `validate_quality_recipe`).
    assert!(validate_quality_recipe(&[flac_cd.clone(), flac_cd.clone()]).is_err());
    // MP3 ranges must not overlap.
    let low = QualityRecipeEntry::new("mp3", "custom", Some(192), Some(200), Some(255), None, None)
        .expect("valid");
    let overlapping =
        QualityRecipeEntry::new("mp3", "custom", Some(200), Some(220), Some(256), None, None)
            .expect("valid");
    assert!(validate_quality_recipe(&[low, overlapping]).is_err());
    // Closed formats only.
    assert!(QualityRecipeEntry::new("ogg", "cd", None, None, None, None, None).is_err());
    // FLAC entries cannot define a bitrate; MP3 entries cannot define FLAC
    // resolution (v2).
    assert!(
        QualityRecipeEntry::new("flac", "custom", Some(1), None, None, Some(16), Some(44100))
            .is_err()
    );
    assert!(
        QualityRecipeEntry::new(
            "mp3",
            "custom",
            Some(192),
            Some(192),
            Some(255),
            Some(16),
            None
        )
        .is_err()
    );
    // Standard MP3 entries canonicalise to their fixed bounds (v2).
    let high =
        QualityRecipeEntry::new("mp3", "320_plus", None, None, None, None, None).expect("valid");
    assert_eq!(
        (
            high.min_bitrate_kbps,
            high.target_bitrate_kbps,
            high.max_bitrate_kbps
        ),
        (Some(320), Some(320), None)
    );
}

#[test]
fn recipe_ranking_prefers_flac_then_320_with_dsd_rejected() {
    let policy = DownloadPolicy::default();

    // FLAC (bitrate unknown — NOT a fidelity axis, v2) outranks 320 MP3.
    let flac = policy.recipe_rank("flac", None, Some(16), Some(44100));
    let mp3 = policy.recipe_rank("mp3", Some(320), None, None);
    let low_mp3 = policy.recipe_rank("mp3", Some(128), None, None);
    assert_eq!(flac, Some(0));
    assert_eq!(mp3, Some(1));
    // 128 kbps is outside the default 320_plus entry.
    assert_eq!(low_mp3, None);
    // DSD must not spend bandwidth even when a noisy title claims
    // "lossless" (v2 `NOT_IMPORTABLE_EXTENSIONS`).
    assert_eq!(
        policy.recipe_rank("dsf", None, Some(1), Some(2822400)),
        None
    );
    assert_eq!(
        policy.recipe_rank("dff", None, Some(1), Some(2822400)),
        None
    );
}

#[test]
fn lossless_detail_ladder_maps_cd_through_partial() {
    assert_eq!(lossless_detail_step(Some(16), Some(44100)), 0);
    assert_eq!(lossless_detail_step(Some(24), Some(48000)), 1);
    assert_eq!(lossless_detail_step(Some(24), Some(96000)), 2);
    assert_eq!(lossless_detail_step(Some(24), Some(192000)), 3);
    assert_eq!(lossless_detail_step(Some(32), Some(384000)), 4);
    // Either axis absent -> the `partial` step (v2).
    assert_eq!(lossless_detail_step(None, Some(44100)), 5);
    assert_eq!(lossless_detail_step(Some(16), None), 5);
}

#[test]
fn poll_deadline_is_timeout_plus_grace() {
    let policy = test_policy();
    assert_eq!(
        slskd::repository::poll_deadline(&policy),
        Duration::from_secs(4)
    );
}

// Query-construction briefs.

#[test]
fn sanitize_strips_operators_but_keeps_in_word_hyphens() {
    assert_eq!(
        sanitize_query("AC-DC - Back In Black"),
        "AC-DC Back In Black"
    );
    assert_eq!(
        sanitize_query("Euphoria (International Edition)"),
        "Euphoria International Edition"
    );
    // Typographic apostrophes normalise to straight ASCII (v2).
    assert_eq!(sanitize_query("D\u{2019}Angelo"), "D'Angelo");
}

#[test]
fn stripped_album_title_drops_editions_and_subtitles() {
    assert_eq!(
        stripped_album_title("Euphoria (International Edition)"),
        "Euphoria"
    );
    assert_eq!(
        stripped_album_title("Devil May Cry: Season 2 (Soundtrack from the Netflix Series)"),
        "Devil May Cry"
    );
}

#[test]
fn album_ladder_escalates_specific_first_with_wildcard_siblings() {
    // Multi-artist credits query the primary artist only: Soulseek ANDs
    // every term (v2 issue #373).
    let ladder = album_query_ladder("YMO, Some Guest", "Solid State Survivor", Some(1979));
    assert_eq!(ladder[0], "YMO Solid State Survivor 1979");
    // The blocked-artist wildcard sibling follows the exact rung (v2).
    assert_eq!(
        ladder[1],
        "YMO Solid State Survivor 1979".replace("YMO", "*MO")
    );
    assert!(ladder.contains(&"YMO Solid State Survivor".to_owned()));
    assert!(ladder.contains(&"YMO".to_owned()));
    assert!(!ladder.iter().any(|query| query.contains("Guest")));
    // Deduped and non-empty throughout.
    let mut seen = std::collections::HashSet::new();
    for query in &ladder {
        assert!(!query.is_empty());
        assert!(seen.insert(query.clone()), "ladder dedupes: {ladder:?}");
    }
}

#[test]
fn extension_parsing_ignores_directories_and_case() {
    assert_eq!(
        extension_from_filename("@@music\\Album\\01 Track.FLAC"),
        "flac"
    );
    assert_eq!(extension_from_filename("/home/bob/song.mp3"), "mp3");
    assert_eq!(extension_from_filename("no-extension"), "");
}

// Locator briefs.

#[test]
fn locator_resolves_leaf_flat_and_username_layouts() {
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
    let locator = Locator::new(mount.clone(), None);

    // Step 1: {mount}/{leaf remote folder}/{filename} (v2).
    let hit = locator
        .locate_file(
            "alice",
            "@@music\\Radiohead - OK Computer\\OK Computer\\01 Track 1.flac",
            None,
        )
        .expect("leaf layout resolves");
    assert!(hit.starts_with(mount.canonicalize().expect("mount resolves")));
    assert_eq!(hit.file_name().unwrap(), "01 Track 1.flac");

    // Step 2: flat layout (v2).
    let hit = locator
        .locate_file("alice", "@@music\\Other\\flat.flac", None)
        .expect("flat layout resolves");
    assert_eq!(hit.file_name().unwrap(), "flat.flac");

    // Step 3: {mount}/{username}/ at any depth (v2).
    let hit = locator
        .locate_file("alice", "@@music\\Other\\buried.flac", None)
        .expect("username layout resolves");
    assert_eq!(hit.file_name().unwrap(), "buried.flac");

    // Traversal is refused outright (v2).
    assert_eq!(
        locator.locate_file("alice", "..\\..\\etc\\passwd", None),
        None
    );
    assert_eq!(locator.locate_file("alice", "", None), None);
}

#[test]
fn locator_refuses_size_mismatched_exact_hits_but_keeps_aliases() {
    let mount = temp_mount("sizes");
    std::fs::write(mount.join("stale.flac"), b"tiny").expect("stale file");
    let locator = Locator::new(mount, None);

    // A same-named file with the wrong bytes is another peer's stale
    // leftover, not this transfer (v2 #397).
    assert_eq!(
        locator.locate_file("alice", "stale.flac", Some(1_000_000)),
        None
    );
    // Unknown size keeps the old name-only behavior (v2).
    assert!(locator.locate_file("alice", "stale.flac", None).is_some());
}

#[test]
fn partial_lookup_is_basename_keyed_and_mount_confined() {
    let downloads = temp_mount("downloads");
    let incomplete = temp_mount("incomplete");
    std::fs::create_dir_all(incomplete.join("OK Computer")).expect("album dir");
    std::fs::write(incomplete.join("OK Computer").join("part.flac"), b"partial")
        .expect("partial file");
    let locator = Locator::new(downloads, Some(incomplete));

    // The incomplete layout is not username-scoped (v2 `_locate_partial`).
    let hit = locator
        .locate_partial("anyone", "@@music\\X\\part.flac", Some(30_000_000))
        .expect("partial resolves");
    assert_eq!(hit.file_name().unwrap(), "part.flac");

    // No incomplete mount disables the fallback entirely (v2).
    let bare = Locator::new(temp_mount("bare"), None);
    assert_eq!(bare.locate_partial("anyone", "part.flac", Some(1)), None);
}

#[tokio::test]
async fn repository_partial_lookup_uses_the_incomplete_mount() {
    let mock = MockSlskd::start().await.expect("mock starts");
    let downloads = temp_mount("repo-downloads");
    let incomplete = temp_mount("repo-incomplete");
    std::fs::write(incomplete.join("stranded.flac"), b"partial").expect("partial file");
    let http = ReqwestSlskdHttp::new(reqwest::Client::new(), &mock.base_url(), MOCK_API_KEY);
    let repo = SlskdRepository::new(
        SlskdClient::new(http),
        &mock.base_url(),
        MOCK_API_KEY,
        downloads,
        test_policy(),
    )
    .with_incomplete_mount(incomplete);

    let handle = TaskHandle::new("alice", vec!["stranded.flac".to_owned()]);
    let hit = repo
        .locate_partial(&handle, "@@music\\X\\stranded.flac", Some(30_000_000))
        .await
        .expect("partial lookup succeeds")
        .expect("partial resolves");
    assert_eq!(hit.file_name().unwrap(), "stranded.flac");
}

// Mount-diagnosis briefs.

#[tokio::test]
async fn diagnosis_with_no_completed_downloads_is_empty_but_supported() {
    let mock = MockSlskd::start().await.expect("mock starts");
    let repo = repository(&mock);

    let diagnosis = repo.diagnose_downloads_mount().await;
    assert!(diagnosis.supported);
    assert_eq!(diagnosis.completed_downloads, 0);
    assert_eq!(
        diagnosis.client_downloads_dir.as_deref(),
        Some("/slskd/downloads")
    );
}

#[tokio::test]
async fn diagnosis_resolves_a_sample_under_a_correct_mount() {
    let mock = MockSlskd::start().await.expect("mock starts");
    let mount = temp_mount("diagnose");
    let http = ReqwestSlskdHttp::new(reqwest::Client::new(), &mock.base_url(), MOCK_API_KEY);
    let repo = SlskdRepository::new(
        SlskdClient::new(http),
        &mock.base_url(),
        MOCK_API_KEY,
        mount.clone(),
        test_policy(),
    );

    mock.inject_transfer(
        "alice",
        "OK Computer\\01 Track 1.flac",
        4,
        "Completed, Succeeded",
        None,
        None,
    );
    std::fs::create_dir_all(mount.join("OK Computer")).expect("leaf dir");
    std::fs::write(mount.join("OK Computer").join("01 Track 1.flac"), b"data")
        .expect("finished file");

    let diagnosis = repo.diagnose_downloads_mount().await;
    assert_eq!(diagnosis.completed_downloads, 1);
    assert_eq!(diagnosis.resolvable_downloads, 1);
    assert_eq!(diagnosis.sampled_downloads, 1);
    assert!(diagnosis.mount_has_files);
}

/// The API key never appears in transport Debug output.
#[test]
fn slskd_http_debug_redacts_key() {
    let transport =
        ReqwestSlskdHttp::new(reqwest::Client::new(), "http://127.0.0.1:9", "SUPERSECRET");
    let debug = format!("{transport:?}");
    assert!(debug.contains("<redacted>"), "{debug}");
    assert!(!debug.contains("SUPERSECRET"), "{debug}");
}

/// Unreachable slskd reads as a plain summary, never transport jargon.
#[tokio::test]
async fn health_check_unreachable_is_plain_summary() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let base = format!("http://{}", listener.local_addr().expect("addr"));
    drop(listener);
    let transport = ReqwestSlskdHttp::new(reqwest::Client::new(), &base, MOCK_API_KEY);
    let mount = std::env::temp_dir().join(format!("slskd-brief-{}", std::process::id()));
    let repo = SlskdRepository::new(
        SlskdClient::new(transport),
        &base,
        MOCK_API_KEY,
        mount,
        test_policy(),
    );
    let health = repo.health_check().await;
    assert!(!health.ok);
    assert_eq!(health.message, "slskd unreachable");
}
