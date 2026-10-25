//! Usenet contract tests: SABnzbd, Newznab, Prowlarr.
//!
//! Coverage of the Usenet module against the in-repo loopback
//! mocks: per-client contract tests plus the live-version-cited quirk
//! tests (SABnzbd 5.0.4 suffix params, the addurl fallback, and the
//! multipart add; Newznab's 202 fallback; Prowlarr's never-logged key).
//! Transport is real HTTP over `127.0.0.1` only: no live contact, ever.

use droppedneedle_acquire::client_error::ClientError;
use droppedneedle_acquire::usenet;

use std::path::PathBuf;
use std::time::Duration;

use crate::common::bundles::quiet_ticks;
use droppedneedle_testkit::usenet::{
    NewznabMock, NewznabScenario, ProwlarrMock, ProwlarrScenario, SabnzbdMock, serve_loopback,
};
use usenet::newznab::{
    NewznabClient, NewznabError, NewznabIndexer, NewznabIndexerEntry, UsenetRelease,
    normalize_newznab_query, usenet_identity,
};
use usenet::policy::UsenetPolicy;
use usenet::prowlarr::{ProwlarrClient, ProwlarrIndexerInfo, ProwlarrRelease};
use usenet::sabnzbd::{
    HistorySlot, NzbFetchError, QueueSlot, SabnzbdClient, SabnzbdError, SabnzbdQueue, TaskHandle,
    redact_query_secrets,
};

const TIMEOUT: Duration = Duration::from_secs(10);

fn http() -> reqwest::Client {
    reqwest::Client::builder().build().expect("test client")
}

fn sab_client(base: &str) -> SabnzbdClient {
    SabnzbdClient::new(http(), base, "SABKEY", 3, Duration::from_millis(1))
}

fn test_policy() -> UsenetPolicy {
    UsenetPolicy {
        indexer_timeout: TIMEOUT,
        enqueue_timeout: TIMEOUT,
        poll_timeout: TIMEOUT,
        ..UsenetPolicy::v2_defaults()
    }
}

fn queue_for(_mock: &SabnzbdMock, base: &str, mount: PathBuf) -> SabnzbdQueue {
    SabnzbdQueue::new(sab_client(base), base, "SABKEY", mount, test_policy())
}

/// Scratch directory, removed when the test ends.
fn temp_dir(name: &str) -> crate::common::ScratchDir {
    crate::common::ScratchDir::new(name)
}

fn write_file(path: &std::path::Path, bytes: &[u8]) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("parents");
    }
    std::fs::write(path, bytes).expect("write");
}

/// A loopback URL nothing answers (listener bound, then dropped).
async fn refused_url(path: &str) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let port = listener.local_addr().expect("addr").port();
    drop(listener);
    format!("http://127.0.0.1:{port}{path}")
}

// ---------------------------------------------------------------------------
// SABnzbd: suffix params + multipart add.
// ---------------------------------------------------------------------------

/// Every call appends `output=json` + `apikey` as suffix query params
/// (Lidarr `SabnzbdProxy`), verified on 5.0.4.
#[tokio::test]
async fn sab_suffix_params_output_apikey_last() {
    let mock = SabnzbdMock::new();
    let (base, _server) = serve_loopback(mock.router()).await.expect("mock serves");
    let client = sab_client(&base);

    client.version(TIMEOUT).await.expect("version");
    client.queue(TIMEOUT).await.expect("queue");

    let calls = mock.state().api_calls.clone();
    assert_eq!(calls.len(), 2);
    for call in &calls {
        let keys: Vec<&str> = call
            .raw_query
            .split('&')
            .map(|pair| pair.split('=').next().unwrap_or(""))
            .collect();
        assert!(keys.len() >= 3, "params carry mode + suffixes: {keys:?}");
        assert_eq!(&keys[keys.len() - 2..], &["output", "apikey"]);
        assert!(call.raw_query.contains("output=json"));
        assert!(call.raw_query.contains("apikey=SABKEY"));
    }
}

/// `mode=addfile` is a multipart POST with `cat` (not `category`), the
/// `name` field carrying `{job}.nzb` as `application/x-nzb`, and the
/// validated NZB bytes in the body.
#[tokio::test]
async fn sab_addfile_multipart_shape() {
    let mock = SabnzbdMock::new();
    let (base, _server) = serve_loopback(mock.router()).await.expect("mock serves");
    let mount = temp_dir("addfile");
    let queue = queue_for(&mock, &base, mount.to_path_buf());

    let handle = queue
        .enqueue_album(
            "t1",
            Some(&format!("{base}/nzb/good")),
            Some("audio"),
            None,
            None,
        )
        .await
        .expect("enqueue");
    assert_eq!(handle.source, "usenet");
    assert_eq!(handle.job_name, "droppedneedle-t1");
    assert_eq!(handle.nzo_id, "nzo-test-1");

    let calls = mock.state().add_file_requests.clone();
    assert_eq!(calls.len(), 1);
    let call = &calls[0];
    let get = |key: &str| {
        call.params
            .iter()
            .find(|(name, _)| name == key)
            .map(|(_, value)| value.clone())
    };
    assert_eq!(get("mode").as_deref(), Some("addfile"));
    assert_eq!(get("nzbname").as_deref(), Some("droppedneedle-t1"));
    assert_eq!(get("cat").as_deref(), Some("audio"));
    assert!(get("category").is_none(), "cat, never category, on add");

    let content_type = call.content_type.clone().unwrap_or_default();
    assert!(
        content_type.starts_with("multipart/form-data; boundary="),
        "{content_type}"
    );
    let body = String::from_utf8_lossy(&call.body);
    assert!(
        body.contains("name=\"name\"; filename=\"droppedneedle-t1.nzb\""),
        "{body}"
    );
    assert!(body.contains("application/x-nzb"), "{body}");
    assert!(body.contains("<nzb"), "{body}");

    std::fs::remove_dir_all(&mount).ok();
}

/// A no-response transport failure falls back to `addurl`: SABnzbd
/// fetches the enclosure itself, for indexers only it can reach.
#[tokio::test]
async fn sab_addurl_fallback_on_transport_failure() {
    let mock = SabnzbdMock::new();
    let (base, _server) = serve_loopback(mock.router()).await.expect("mock serves");
    let mount = temp_dir("addurl");
    let queue = queue_for(&mock, &base, mount.to_path_buf());

    let nzb_url = refused_url("/getnzb/x.nzb?apikey=SECRET").await;
    let handle = queue
        .enqueue_album("t9", Some(&nzb_url), Some("audio"), None, None)
        .await
        .expect("addurl fallback");

    assert_eq!(handle.job_name, "droppedneedle-t9");
    assert_eq!(handle.nzo_id, "nzo-test-1");
    assert!(mock.state().add_file_requests.is_empty());
    let calls = mock.state().add_url_requests.clone();
    assert_eq!(calls.len(), 1);
    let get = |key: &str| {
        calls[0]
            .params
            .iter()
            .find(|(name, _)| name == key)
            .map(|(_, value)| value.clone())
    };
    assert_eq!(get("mode").as_deref(), Some("addurl"));
    assert_eq!(get("name").as_deref(), Some(nzb_url.as_str()));
    assert_eq!(get("nzbname").as_deref(), Some("droppedneedle-t9"));

    std::fs::remove_dir_all(&mount).ok();
}

/// A deterministic content rejection (indexer error page, not an NZB)
/// never falls back to addurl: the indexer answered, so SABnzbd would
/// fetch the same error page.
#[tokio::test]
async fn sab_no_addurl_on_content_rejection() {
    let mock = SabnzbdMock::new();
    let (base, _server) = serve_loopback(mock.router()).await.expect("mock serves");
    let mount = temp_dir("rejection");
    let queue = queue_for(&mock, &base, mount.to_path_buf());

    let err = queue
        .enqueue_album(
            "t2",
            Some(&format!("{base}/nzb/errorpage")),
            None,
            None,
            None,
        )
        .await
        .expect_err("content rejection fails");
    assert!(err.to_string().contains("non-NZB body"), "{err}");
    assert!(mock.state().add_file_requests.is_empty());
    assert!(mock.state().add_url_requests.is_empty());

    // The raw client marks the rejection for blocklist handling.
    let client = sab_client(&base);
    let err = client
        .fetch_nzb(&format!("{base}/nzb/errorpage"), TIMEOUT)
        .await
        .expect_err("rejection");
    assert!(
        matches!(err, NzbFetchError::ContentRejection { .. }),
        "{err:?}"
    );

    std::fs::remove_dir_all(&mount).ok();
}

/// All three SABnzbd error forms, plus auth detection by message.
#[tokio::test]
async fn sab_error_forms() {
    use droppedneedle_testkit::usenet::SabErrorForm;

    let mock = SabnzbdMock::new();
    let (base, _server) = serve_loopback(mock.router()).await.expect("mock serves");
    let client = sab_client(&base);

    mock.state().api_error = Some(SabErrorForm::JsonFalse("nope".to_owned()));
    let err = client.queue(TIMEOUT).await.expect_err("json false");
    assert_eq!(err.to_string(), "nope");
    assert!(!err.is_auth());

    mock.state().api_error = Some(SabErrorForm::JsonFalseString("stringly".to_owned()));
    let err = client.queue(TIMEOUT).await.expect_err("stringly false");
    assert_eq!(err.to_string(), "stringly");

    mock.state().api_error = Some(SabErrorForm::PlainText("boom".to_owned()));
    let err = client.queue(TIMEOUT).await.expect_err("plain text");
    assert_eq!(err.to_string(), "boom");

    mock.state().api_error = Some(SabErrorForm::JsonFalse("API Key Incorrect".to_owned()));
    let err = client.queue(TIMEOUT).await.expect_err("auth");
    assert!(err.is_auth(), "{err:?}");

    mock.state().api_error = Some(SabErrorForm::JsonFalse("API key required".to_owned()));
    let err = client.queue(TIMEOUT).await.expect_err("auth");
    assert!(err.is_auth(), "{err:?}");

    mock.state().api_error = None;
    client.queue(TIMEOUT).await.expect("recovered");
}

/// Idempotent GETs retry 5xx with backoff; queue mutations never retry
/// (a retried add could double-add the job).
#[tokio::test]
async fn sab_retry_discipline() {
    let mock = SabnzbdMock::new();
    mock.state().queue_fail_500 = 2;
    let (base, _server) = serve_loopback(mock.router()).await.expect("mock serves");
    let client = sab_client(&base);

    client.queue(TIMEOUT).await.expect("retried queue");
    let queue_calls = mock
        .state()
        .api_calls
        .iter()
        .filter(|call| call.raw_query.contains("mode=queue"))
        .count();
    assert_eq!(queue_calls, 3, "two blips + success");

    mock.state().addfile_fail_500 = 1;
    let err = client
        .add_file("job", b"<?xml <nzb/>", None, None, None, TIMEOUT)
        .await
        .expect_err("add is not retried");
    assert!(
        matches!(&err, SabnzbdError::Client(ClientError::Unavailable { detail, .. }) if detail == "HTTP 500"),
        "{err:?}"
    );
    let add_calls = mock
        .state()
        .api_calls
        .iter()
        .filter(|call| call.raw_query.contains("mode=addfile"))
        .count();
    assert_eq!(add_calls, 1, "mutations fire exactly once");
}

fn handle(job: &str, nzo: &str) -> TaskHandle {
    TaskHandle {
        source: "usenet".to_owned(),
        job_name: job.to_owned(),
        nzo_id: nzo.to_owned(),
    }
}

async fn status_for(
    mock: &SabnzbdMock,
    queue: &SabnzbdQueue,
    job: &str,
    nzo: &str,
) -> usenet::sabnzbd::TaskStatus {
    let _ = mock;
    queue.get_status(&handle(job, nzo)).await.expect("status")
}

/// The queue→history walk: only true `Downloading` is active; queued
/// states stay `queued`; post-processing holds 100% as `processing`;
/// history maps completed/failed/deleted; ambiguity refuses to guess.
#[tokio::test]
async fn sab_status_walk() {
    // Downloading: active, bytes from the stringly megabytes.
    let mock = SabnzbdMock::new();
    mock.queue_job(
        "nzo-1",
        "droppedneedle-a",
        "Downloading",
        "100.0",
        "25.0",
        "75",
    );
    let (base, _server) = serve_loopback(mock.router()).await.expect("mock serves");
    let mount = temp_dir("walk");
    let queue = queue_for(&mock, &base, mount.to_path_buf());
    let status = status_for(&mock, &queue, "droppedneedle-a", "nzo-1").await;
    assert_eq!(status.status, "downloading");
    assert!(status.has_active_transfer);
    assert_eq!(status.matched_transfers, 1);
    assert_eq!(status.bytes_total, 100 * 1024 * 1024);
    assert_eq!(status.bytes_downloaded, 75 * 1024 * 1024);
    assert_eq!(status.progress_percent, 75.0);

    // Queued-family states move 0 bytes: never active transfers.
    for state in ["Queued", "Grabbing", "Paused"] {
        let mock = SabnzbdMock::new();
        mock.queue_job("nzo-q", "droppedneedle-q", state, "100.0", "100.0", "0");
        let (base, _server) = serve_loopback(mock.router()).await.expect("mock serves");
        let queue = queue_for(&mock, &base, mount.to_path_buf());
        let status = status_for(&mock, &queue, "droppedneedle-q", "nzo-q").await;
        assert_eq!(status.status, "queued", "{state}");
        assert!(!status.has_active_transfer, "{state}");
    }

    // Post-download phases: all bytes in, bar held at 100%.
    let mock = SabnzbdMock::new();
    mock.queue_job(
        "nzo-p",
        "droppedneedle-p",
        "Verifying",
        "100.0",
        "100.0",
        "0",
    );
    let (base, _server) = serve_loopback(mock.router()).await.expect("mock serves");
    let queue = queue_for(&mock, &base, mount.to_path_buf());
    let status = status_for(&mock, &queue, "droppedneedle-p", "nzo-p").await;
    assert_eq!(status.status, "processing");
    assert!(!status.has_active_transfer);
    assert_eq!(status.progress_percent, 100.0);
    assert_eq!(status.bytes_downloaded, status.bytes_total);

    // History: completed, failed (verbatim message), deleted (terminal).
    let mock = SabnzbdMock::new();
    mock.history_job(
        "nzo-c",
        "job-c",
        "Completed",
        "/data/Downloads/complete/job-c",
        42,
        "",
    );
    mock.history_job("nzo-f", "job-f", "Failed", "", 10, "out of articles");
    mock.history_job("nzo-d", "job-d", "Deleted", "", 0, "");
    mock.history_job("nzo-x", "job-x", "Extracting", "", 99, "");
    let (base, _server) = serve_loopback(mock.router()).await.expect("mock serves");
    let queue = queue_for(&mock, &base, mount.to_path_buf());
    let status = status_for(&mock, &queue, "job-c", "nzo-c").await;
    assert_eq!(status.status, "completed");
    assert_eq!((status.bytes_total, status.bytes_downloaded), (42, 42));
    let status = status_for(&mock, &queue, "job-f", "nzo-f").await;
    assert_eq!(status.status, "failed");
    assert_eq!(status.error.as_deref(), Some("out of articles"));
    let status = status_for(&mock, &queue, "job-d", "nzo-d").await;
    assert_eq!(status.status, "failed");
    assert_eq!(status.error.as_deref(), Some("job removed from SABnzbd"));
    let status = status_for(&mock, &queue, "job-x", "nzo-x").await;
    assert_eq!(status.status, "processing");
    assert_eq!(status.progress_percent, 100.0);

    // Missing everywhere: just added or gone, never reported as live.
    let status = status_for(&mock, &queue, "job-?", "nzo-?").await;
    assert_eq!(status.status, "missing");
    assert_eq!(status.matched_transfers, 0);

    // Ambiguous identity: two rows, one handle, no guessing.
    let mock = SabnzbdMock::new();
    mock.queue_job("nzo-amb", "job-one", "Downloading", "1.0", "1.0", "0");
    mock.queue_job("nzo-amb", "job-two", "Queued", "1.0", "1.0", "0");
    let (base, _server) = serve_loopback(mock.router()).await.expect("mock serves");
    let queue = queue_for(&mock, &base, mount.to_path_buf());
    let err = queue
        .get_status(&handle("", "nzo-amb"))
        .await
        .expect_err("ambiguous");
    assert!(matches!(err, SabnzbdError::AmbiguousIdentity));

    std::fs::remove_dir_all(&mount).ok();
}

/// History is queried by `nzo_ids` ALONE when known: also passing
/// `search=job_name` risks an AND that drops renamed rows.
#[tokio::test]
async fn sab_history_filter_nzo_only() {
    let mock = SabnzbdMock::new();
    mock.history_job("nzo-1", "renamed-by-sab", "Completed", "", 1, "");
    let (base, _server) = serve_loopback(mock.router()).await.expect("mock serves");
    let mount = temp_dir("filter");
    let queue = queue_for(&mock, &base, mount.to_path_buf());

    let status = queue
        .get_status(&handle("droppedneedle-t", "nzo-1"))
        .await
        .expect("status");
    assert_eq!(status.status, "completed");
    let calls = mock.state().history_requests.clone();
    assert_eq!(calls.len(), 1);
    assert!(
        calls[0]
            .iter()
            .any(|(key, value)| key == "nzo_ids" && value == "nzo-1")
    );
    assert!(
        !calls[0].iter().any(|(key, _)| key == "search"),
        "{calls:?}"
    );

    // Without an nzo_id the job_name search is the crash-recovery fallback.
    let status = queue
        .get_status(&handle("renamed-by-sab", ""))
        .await
        .expect("status");
    assert_eq!(status.status, "completed");
    let calls = mock.state().history_requests.clone();
    assert_eq!(calls.len(), 2);
    assert!(calls[1].iter().any(|(key, _)| key == "search"));

    std::fs::remove_dir_all(&mount).ok();
}

/// Abort and discard split ownership: queue bytes and failed history go
/// with `del_files=1`; completed output is never client-owned.
#[tokio::test]
async fn sab_abort_and_discard_ownership() {
    let mock = SabnzbdMock::new();
    mock.queue_job("nzo-live", "job-live", "Downloading", "10.0", "5.0", "50");
    mock.history_job(
        "nzo-fail",
        "job-fail",
        "Failed",
        "/incomplete/job-fail",
        5,
        "bad",
    );
    mock.history_job(
        "nzo-done",
        "job-done",
        "Completed",
        "/data/Downloads/complete/job-done",
        9,
        "",
    );
    let (base, _server) = serve_loopback(mock.router()).await.expect("mock serves");
    let mount = temp_dir("abort");
    let queue = queue_for(&mock, &base, mount.to_path_buf());

    assert!(
        queue
            .abort(&handle("job-live", "nzo-live"))
            .await
            .expect("abort queue")
    );
    assert!(
        queue
            .abort(&handle("job-fail", "nzo-fail"))
            .await
            .expect("abort failed")
    );
    assert!(
        !queue
            .abort(&handle("job-done", "nzo-done"))
            .await
            .expect("abort done")
    );
    assert!(
        queue
            .abort(&handle("job-gone", "nzo-gone"))
            .await
            .expect("abort gone")
    );

    let deletes = mock.state().delete_requests.clone();
    assert_eq!(deletes.len(), 2);
    let flag = |call: &Vec<(String, String)>, key: &str| {
        call.iter()
            .find(|(name, _)| name == key)
            .map(|(_, value)| value.clone())
    };
    assert_eq!(flag(&deletes[0], "mode").as_deref(), Some("queue"));
    assert_eq!(flag(&deletes[0], "del_files").as_deref(), Some("1"));
    assert_eq!(flag(&deletes[1], "mode").as_deref(), Some("history"));
    assert_eq!(flag(&deletes[1], "del_files").as_deref(), Some("1"));
    assert_eq!(flag(&deletes[1], "archive").as_deref(), Some("0"));
    assert_eq!(
        mock.state().deleted_storage,
        vec!["/incomplete/job-fail".to_owned()]
    );

    // Discard after durable local cleanup: completed MUST use del_files=0.
    assert!(
        queue
            .discard_client_artifacts(&handle("job-done", "nzo-done"))
            .await
            .expect("discard")
    );
    let deletes = mock.state().delete_requests.clone();
    assert_eq!(deletes.len(), 3);
    assert_eq!(flag(&deletes[2], "mode").as_deref(), Some("history"));
    assert_eq!(flag(&deletes[2], "del_files").as_deref(), Some("0"));
    assert!(mock.state().retained_completed_storage.is_empty());

    std::fs::remove_dir_all(&mount).ok();
}

/// Storage remap: `complete_dir` prefix strip, backslash folding for a
/// Windows-native SAB, basename fallback; completed files enumerate
/// audio only, confined to the mount.
#[tokio::test]
async fn sab_remap_and_completed_files() {
    let mount = temp_dir("remap");
    write_file(&mount.join("job1/track.flac"), b"FLAC");
    write_file(&mount.join("job1/cover.jpg"), b"JPEG");
    write_file(&mount.join("job1/notes.txt"), b"notes");
    write_file(&mount.join("job1/sub/nested.mp3"), b"MP3");

    // Prefix strip: the mock's complete_dir is /data/Downloads/complete.
    let mock = SabnzbdMock::new();
    mock.history_job(
        "nzo-1",
        "job1",
        "Completed",
        "/data/Downloads/complete/job1",
        4,
        "",
    );
    let (base, _server) = serve_loopback(mock.router()).await.expect("mock serves");
    let queue = queue_for(&mock, &base, mount.to_path_buf());
    let mut files = queue
        .list_completed_files(&handle("job1", "nzo-1"))
        .await
        .expect("files");
    files.sort();
    assert_eq!(
        files,
        vec![
            mount.join("job1/sub/nested.mp3"),
            mount.join("job1/track.flac")
        ]
    );

    // Exact basenames resolve; sizes recover sanitised names.
    let found = queue
        .get_file_path(&handle("job1", "nzo-1"), "track.flac", None)
        .await
        .expect("lookup")
        .expect("found");
    assert_eq!(found, mount.join("job1/track.flac"));
    let found = queue
        .get_file_path(&handle("job1", "nzo-1"), "renamed.flac", Some(4))
        .await
        .expect("lookup")
        .expect("found by size");
    assert_eq!(found, mount.join("job1/track.flac"));
    assert!(
        queue
            .get_file_path(&handle("job1", "nzo-1"), "missing.flac", Some(12345))
            .await
            .expect("lookup")
            .is_none()
    );

    // Backslash paths from a Windows-native SAB fold before the remap.
    let mock = SabnzbdMock::new();
    mock.state().complete_dir = "C:\\data\\complete".to_owned();
    mock.history_job(
        "nzo-2",
        "job1",
        "Completed",
        "C:\\data\\complete\\job1",
        4,
        "",
    );
    let (base, _server) = serve_loopback(mock.router()).await.expect("mock serves");
    let queue = queue_for(&mock, &base, mount.to_path_buf());
    let files = queue
        .list_completed_files(&handle("job1", "nzo-2"))
        .await
        .expect("files");
    assert_eq!(files.len(), 2);

    // Prefix mismatch falls back to the job-folder basename.
    let mock = SabnzbdMock::new();
    mock.history_job("nzo-3", "job1", "Completed", "/elsewhere/job1", 4, "");
    let (base, _server) = serve_loopback(mock.router()).await.expect("mock serves");
    let queue = queue_for(&mock, &base, mount.to_path_buf());
    let files = queue
        .list_completed_files(&handle("job1", "nzo-3"))
        .await
        .expect("files");
    assert_eq!(files.len(), 2);

    std::fs::remove_dir_all(&mount).ok();
}

/// A job whose storage is SABnzbd's complete folder itself names no job
/// folder: nothing in the downloads folder is read as that job's output,
/// though other jobs' files sit there.
#[tokio::test]
async fn sab_storage_at_the_complete_folder_lands_nothing() {
    let mount = temp_dir("complete-root");
    write_file(&mount.join("other-job/track.flac"), b"FLAC");
    write_file(&mount.join("loose.flac"), b"FLAC");
    let mock = SabnzbdMock::new();
    mock.history_job(
        "nzo-root",
        "job-root",
        "Completed",
        "/data/Downloads/complete",
        4,
        "",
    );
    mock.history_job("nzo-slash", "job-slash", "Completed", "/", 4, "");
    let (base, _server) = serve_loopback(mock.router()).await.expect("mock serves");
    let queue = queue_for(&mock, &base, mount.to_path_buf());

    for job in [
        handle("job-root", "nzo-root"),
        handle("job-slash", "nzo-slash"),
    ] {
        let material = queue.inspect_materialization(&job).await.expect("done");
        assert_eq!(material.state, "completed");
        assert_eq!(material.workspace_path, "", "{}", material.remote_storage);
        let files = queue.list_completed_files(&job).await.expect("files");
        assert!(files.is_empty(), "{}: {files:?}", material.remote_storage);
    }
    assert_eq!(
        queue.diagnose_downloads_mount().await.resolvable_downloads,
        0
    );
}

/// Materialization evidence and the mount-health split: a missing mount
/// root is an environment fault; an empty per-job folder is not.
#[tokio::test]
async fn sab_materialization_and_mount() {
    let mount = temp_dir("material");
    write_file(&mount.join("job1/track.flac"), b"FLAC");
    let mock = SabnzbdMock::new();
    mock.queue_job("nzo-live", "job-live", "Downloading", "10.0", "5.0", "50");
    mock.history_job(
        "nzo-1",
        "job1",
        "Completed",
        "/data/Downloads/complete/job1",
        4,
        "",
    );
    let (base, _server) = serve_loopback(mock.router()).await.expect("mock serves");
    let queue = queue_for(&mock, &base, mount.to_path_buf());

    let material = queue
        .inspect_materialization(&handle("job-live", "nzo-live"))
        .await
        .expect("active");
    assert_eq!(material.state, "active");
    assert_eq!(material.nzo_id, "nzo-live");
    assert!(material.mount_healthy);

    let material = queue
        .inspect_materialization(&handle("job1", "nzo-1"))
        .await
        .expect("done");
    assert_eq!(material.state, "completed");
    assert_eq!(material.remote_storage, "/data/Downloads/complete/job1");
    assert_eq!(
        material.workspace_path,
        mount.join("job1").display().to_string()
    );

    let material = queue
        .inspect_materialization(&handle("job-?", "nzo-?"))
        .await
        .expect("missing");
    assert_eq!(material.state, "missing");
    assert!(material.mount_healthy);

    let broken = queue_for(&mock, &base, mount.join("no-such-mount"));
    let material = broken
        .inspect_materialization(&handle("job-?", "nzo-?"))
        .await
        .expect("missing");
    assert!(!material.mount_healthy);

    let diagnosis = queue.diagnose_downloads_mount().await;
    assert!(diagnosis.supported);
    assert_eq!(diagnosis.completed_downloads, 1);
    assert_eq!(diagnosis.sampled_downloads, 1);
    assert_eq!(diagnosis.resolvable_downloads, 1);
    assert!(diagnosis.mount_has_files);
    assert_eq!(
        diagnosis.client_downloads_dir.as_deref(),
        Some("/data/Downloads/complete")
    );

    let health = queue.health_check().await;
    assert_eq!(health.status, "ok");
    assert_eq!(health.version.as_deref(), Some("5.0.4"));
    assert!(queue.is_configured());
    assert_eq!(queue.client_name(), "sabnzbd");
    assert!(
        queue
            .get_categories()
            .await
            .expect("cats")
            .contains(&"audio".to_owned())
    );

    std::fs::remove_dir_all(&mount).ok();
}

/// Addurl errors scrub echoed enclosure credentials (`apikey=`,
/// `api_key=`, DrunkenSlug `r=`, Prowlarr `link=`); identifiers and
/// innocent words pass through untouched.
#[tokio::test]
async fn sab_addurl_redaction() {
    assert_eq!(
        redact_query_secrets("fetch failed for https://x/getnzb?a=1&apikey=SECRET&x=2"),
        "fetch failed for https://x/getnzb?a=1&apikey=***&x=2"
    );
    assert_eq!(
        redact_query_secrets("https://x/nzb?i=7&r=PERUSERKEY"),
        "https://x/nzb?i=7&r=***"
    );
    assert_eq!(
        redact_query_secrets("error=oops&apikey=Z"),
        "error=oops&apikey=***"
    );
    assert_eq!(
        redact_query_secrets("https://prowlarr/9/download?apikey=K&link=ezQxYw&file=a"),
        "https://prowlarr/9/download?apikey=***&link=***&file=a"
    );
    assert_eq!(
        redact_query_secrets("nothing secret here, hyperlink=kept"),
        "nothing secret here, hyperlink=kept"
    );

    let mock = SabnzbdMock::new();
    mock.state().addurl_echo_url = true;
    let (base, _server) = serve_loopback(mock.router()).await.expect("mock serves");
    let mount = temp_dir("redact");
    let queue = queue_for(&mock, &base, mount.to_path_buf());

    let nzb_url = format!("{}/getnzb/x.nzb?i=7&r=PERUSERKEY", refused_url("").await);
    let err = queue
        .enqueue_album("t7", Some(&nzb_url), None, None, None)
        .await
        .expect_err("echoed");
    assert!(err.to_string().contains("r=***"), "{err}");
    assert!(!err.to_string().contains("PERUSERKEY"), "{err}");
    assert!(err.to_string().contains("i=7"), "{err}");

    std::fs::remove_dir_all(&mount).ok();
}

// ---------------------------------------------------------------------------
// Newznab: caps gating, 202 fallback, ladder, fan-out.
// ---------------------------------------------------------------------------

fn newznab_client(base: &str, id: &str, name: &str) -> NewznabClient {
    NewznabClient::new(http(), &format!("{base}/api"), "NZKEY", id, name)
}

fn newznab_indexer(entries: Vec<NewznabIndexerEntry>) -> NewznabIndexer {
    NewznabIndexer::new(
        entries,
        Duration::from_secs(300),
        Duration::from_secs(60),
        Duration::from_secs(300),
        TIMEOUT,
    )
}

fn entry_for(client: NewznabClient, id: &str, name: &str, priority: u32) -> NewznabIndexerEntry {
    NewznabIndexerEntry {
        client,
        id: id.to_owned(),
        name: name.to_owned(),
        categories: vec![3040, 3010],
        enabled: true,
        priority,
        limit: 100,
    }
}

/// A `t=music` 202 falls back to `t=search`, even when caps advertised
/// audio-search (a real indexer quirk).
#[tokio::test]
async fn newznab_music_202_fallback() {
    let log = NewznabMock::new();
    let (base, _server) = serve_loopback(log.router(NewznabScenario::BrokenMusic))
        .await
        .expect("mock serves");
    let indexer = newznab_indexer(vec![entry_for(
        newznab_client(&base, "bm", "BM"),
        "bm",
        "BM",
        1,
    )]);
    let hits = indexer
        .search_album("Radiohead", "In Rainbows", None, TIMEOUT)
        .await;
    assert_eq!(hits.len(), 3, "fell back to the search feed");
    let recorded = log.calls();
    let kinds: Vec<&str> = recorded.iter().map(|call| call.kind.as_str()).collect();
    assert_eq!(kinds, vec!["music", "search"]);

    // The raw client surfaces the 202 with its code intact.
    let client = newznab_client(&base, "bm", "BM");
    let err = client
        .music_search("Radiohead", "In Rainbows", &[], None, 0, 100, TIMEOUT)
        .await
        .expect_err("202");
    assert_eq!(err.code(), Some(202));
}

/// Item parsing: MIME-enforced enclosures, entity decoding, attr mapping
/// with the enclosure-length fallback, Torznab bail, and the hardening
/// path for malformed XML.
#[tokio::test]
async fn newznab_item_parsing() {
    let log = NewznabMock::new();
    let (base, _server) = serve_loopback(log.router(NewznabScenario::DrunkenSlug))
        .await
        .expect("mock serves");
    let client = newznab_client(&base, "ds", "DrunkenSlug");
    let (releases, limits) = client
        .search("x", &[], 0, 100, TIMEOUT)
        .await
        .expect("feed");
    assert_eq!(releases.len(), 3);
    let first = &releases[0];
    assert!(
        first.title.contains("\"Radiohead-In_Rainbows"),
        "entities decoded: {}",
        first.title
    );
    assert!(
        first
            .nzb_url
            .starts_with("https://drunkenslug.com/getnzb/93bc.nzb")
    );
    assert!(
        first.nzb_url.contains("r=KEY"),
        "enclosure kept: {}",
        first.nzb_url
    );
    assert_eq!(first.size_bytes, 2_315_726_631);
    assert_eq!(first.category_ids, vec![3040]);
    assert_eq!(first.grabs, Some(205));
    assert_eq!(first.files, Some(113));
    assert_eq!(first.password, 0);
    assert!(first.usenet_date.is_some_and(|date| date > 1_700_000_000.0));
    assert_eq!(releases[1].category_ids, vec![3999]);
    assert_eq!(releases[2].password, 0, "absent password attr means none");
    assert!(releases[2].usenet_date.is_none());
    let limits = limits.expect("apilimits");
    assert_eq!(
        (limits.api_current, limits.grab_current),
        (Some(183), Some(46))
    );

    // Torrent enclosures mean a Torznab mix-up: the whole feed bails.
    let log = NewznabMock::new();
    let (base, _server) = serve_loopback(log.router(NewznabScenario::TorznabFeed))
        .await
        .expect("mock serves");
    let client = newznab_client(&base, "tor", "Tor");
    let (releases, _) = client
        .search("x", &[], 0, 100, TIMEOUT)
        .await
        .expect("feed");
    assert!(releases.is_empty());

    // Bare ampersands, &nbsp;, and control chars still parse.
    let log = NewznabMock::new();
    let (base, _server) = serve_loopback(log.router(NewznabScenario::MalformedXml))
        .await
        .expect("mock serves");
    let client = newznab_client(&base, "mal", "Mal");
    let (releases, _) = client
        .search("x", &[], 0, 100, TIMEOUT)
        .await
        .expect("feed");
    assert_eq!(releases.len(), 1);
    assert!(
        releases[0].title.contains("Fish & Chips"),
        "{}",
        releases[0].title
    );
    assert!(
        releases[0].title.contains("Trouble"),
        "{}",
        releases[0].title
    );
    assert!(!releases[0].title.contains('\u{7}'));
}

/// The #259 ladder: canonical first, one normalized retry on a genuine
/// clean empty (free-text even when caps offers `t=music`), silence on
/// unclean pools.
#[tokio::test]
async fn newznab_query_ladder() {
    assert_eq!(
        normalize_newznab_query("Drake Honestly, Nevermind"),
        "Drake Honestly Nevermind"
    );
    assert_eq!(
        normalize_newznab_query("Man\u{2019}s Best Friend"),
        "Mans Best Friend"
    );
    assert_eq!(normalize_newznab_query("plain query"), "plain query");

    let log = NewznabMock::new();
    let (base, _server) = serve_loopback(log.router(NewznabScenario::DrunkenSlug))
        .await
        .expect("mock serves");
    let indexer = newznab_indexer(vec![entry_for(
        newznab_client(&base, "ds", "DS"),
        "ds",
        "DS",
        1,
    )]);
    let hits = indexer
        .search_album("Drake", "Honestly, Nevermind", None, TIMEOUT)
        .await;
    assert_eq!(hits.len(), 1);
    assert!(hits[0].usenet.title.contains("Honestly_Nevermind"));
    let queries: Vec<String> = log.calls().iter().map(|call| call.q.clone()).collect();
    assert_eq!(
        queries,
        vec!["Drake Honestly, Nevermind", "Drake Honestly Nevermind"]
    );

    // A genuinely empty (but punctuated) query runs both rungs, then gives up.
    let hits = indexer
        .search_album("x,", "xyzzynomatch", None, TIMEOUT)
        .await;
    assert!(hits.is_empty());
    assert_eq!(log.calls().len(), 4);

    // An unclean pool (rate-limited member) never retries.
    let log = NewznabMock::new();
    let (base, _server) = serve_loopback(log.router(NewznabScenario::RateLimitError))
        .await
        .expect("mock serves");
    let indexer = newznab_indexer(vec![entry_for(
        newznab_client(&base, "lim", "L"),
        "lim",
        "L",
        1,
    )]);
    let hits = indexer
        .search_album("Drake", "Honestly, Nevermind", None, TIMEOUT)
        .await;
    assert!(hits.is_empty());
    assert_eq!(log.calls().len(), 1);

    // The retry rung forces t=search even when caps offers t=music.
    let log = NewznabMock::new();
    let (base, _server) = serve_loopback(log.router(NewznabScenario::BrokenMusic))
        .await
        .expect("mock serves");
    let indexer = newznab_indexer(vec![entry_for(
        newznab_client(&base, "bm", "BM"),
        "bm",
        "BM",
        1,
    )]);
    let hits = indexer
        .search_album("Drake", "Honestly, Nevermind", None, TIMEOUT)
        .await;
    assert_eq!(hits.len(), 1);
    let recorded = log.calls();
    let kinds: Vec<&str> = recorded.iter().map(|call| call.kind.as_str()).collect();
    assert_eq!(kinds, vec!["music", "search", "search"]);
}

/// Fan-out: cross-indexer dedup by (title, size) with priority winning,
/// one bad member never failing the pool, backoff skips, cache hits.
#[tokio::test]
async fn newznab_fanout() {
    assert_eq!(
        usenet_identity("  Radiohead  X ", 2_315_726_631),
        usenet_identity("radiohead x", 2_315_726_632)
    );
    assert_ne!(usenet_identity("a", 100), usenet_identity("b", 100));

    let ds_log = NewznabMock::new();
    let (ds_base, _ds) = serve_loopback(ds_log.router(NewznabScenario::DrunkenSlug))
        .await
        .expect("mock serves");
    let ax_log = NewznabMock::new();
    let (ax_base, _ax) = serve_loopback(ax_log.router(NewznabScenario::Audionix))
        .await
        .expect("mock serves");
    // Audionix outranks DrunkenSlug: its FLAC copy (grabs 999) must win.
    let indexer = newznab_indexer(vec![
        entry_for(newznab_client(&ds_base, "ds", "DS"), "ds", "DS", 1),
        entry_for(newznab_client(&ax_base, "ax", "AX"), "ax", "AX", 0),
    ]);
    let hits = indexer
        .search_album("Radiohead", "In Rainbows", None, TIMEOUT)
        .await;
    let flac: Vec<_> = hits
        .iter()
        .filter(|hit| hit.usenet.title.contains("REETKEVER"))
        .collect();
    assert_eq!(flac.len(), 1, "cross-indexer duplicate deduped");
    assert_eq!(flac[0].usenet.grabs, Some(999), "priority copy wins");
    assert!(hits.iter().any(|hit| hit.usenet.title.contains("MP3-320")));

    // One member erroring (auth) never fails the pool.
    let bad_log = NewznabMock::new();
    let (bad_base, _bad) = serve_loopback(bad_log.router(NewznabScenario::AuthError))
        .await
        .expect("mock serves");
    let indexer = newznab_indexer(vec![
        entry_for(newznab_client(&bad_base, "bad", "Bad"), "bad", "Bad", 0),
        entry_for(newznab_client(&ds_base, "ds", "DS"), "ds", "DS", 1),
    ]);
    let hits = indexer
        .search_album("Radiohead", "In Rainbows", None, TIMEOUT)
        .await;
    assert_eq!(hits.len(), 3);

    // A dead caps fetch keeps the indexer on permissive defaults.
    let dead_log = NewznabMock::new();
    let (dead_base, _dead) = serve_loopback(dead_log.router(NewznabScenario::CapsDead))
        .await
        .expect("mock serves");
    let indexer = newznab_indexer(vec![entry_for(
        newznab_client(&dead_base, "dead", "Dead"),
        "dead",
        "Dead",
        1,
    )]);
    let hits = indexer
        .search_album("Radiohead", "In Rainbows", None, TIMEOUT)
        .await;
    assert_eq!(hits.len(), 3);
    assert_eq!(dead_log.calls()[0].kind, "search");

    // Backoff: after a 429 the member sits out the next search.
    let lim_log = NewznabMock::new();
    let (lim_base, _lim) = serve_loopback(lim_log.router(NewznabScenario::Http429))
        .await
        .expect("mock serves");
    let indexer = newznab_indexer(vec![entry_for(
        newznab_client(&lim_base, "lim", "L"),
        "lim",
        "L",
        1,
    )]);
    assert!(
        indexer
            .search_album("a", "b", None, TIMEOUT)
            .await
            .is_empty()
    );
    assert!(
        indexer
            .search_album("a", "b", None, TIMEOUT)
            .await
            .is_empty()
    );
    assert_eq!(lim_log.calls().len(), 1);

    // Cache: a repeat query never re-hits the indexer.
    let hits = indexer
        .search_album("Radiohead", "In Rainbows", None, TIMEOUT)
        .await;
    assert!(hits.is_empty());
    let before = ds_log.calls().len();
    let indexer = newznab_indexer(vec![entry_for(
        newznab_client(&ds_base, "ds", "DS"),
        "ds",
        "DS",
        1,
    )]);
    assert_eq!(
        indexer
            .search_album("Radiohead", "In Rainbows", None, TIMEOUT)
            .await
            .len(),
        3
    );
    assert_eq!(
        indexer
            .search_album("Radiohead", "In Rainbows", None, TIMEOUT)
            .await
            .len(),
        3
    );
    assert_eq!(ds_log.calls().len(), before + 1);

    // Health counts reachable members; an empty roster errors.
    let health = indexer.health_check().await;
    assert_eq!(health.status, "ok");
    assert!(health.message.contains("1/1"));
    assert!(indexer.is_configured());
    let empty = newznab_indexer(vec![]);
    assert_eq!(empty.health_check().await.status, "error");
}

// ---------------------------------------------------------------------------
// Prowlarr: header auth, mixed-feed mapping, degraded status.
// ---------------------------------------------------------------------------

fn prowlarr_client(base: &str) -> ProwlarrClient {
    ProwlarrClient::new(http(), base, "SUPERSECRET", "prowlarr")
}

/// The mixed feed maps to usenet-with-URL only; repeated params and the
/// limit ride the wire; the key travels in the header.
#[tokio::test]
async fn prowlarr_search_mapping() {
    let log = ProwlarrMock::new();
    let (base, _server) = serve_loopback(log.router(ProwlarrScenario::default()))
        .await
        .expect("mock serves");
    let client = prowlarr_client(&base);

    let releases = client
        .search("Radiohead In Rainbows", &[3040, 3000], &[7], 50, TIMEOUT)
        .await
        .expect("search");
    assert_eq!(releases.len(), 1, "torrent + URL-less rows skipped");
    let release = &releases[0];
    assert_eq!(release.indexer_id, "prowlarr:7");
    assert_eq!(release.indexer_name, "NZBGeek");
    assert_eq!(release.size_bytes, 2_315_726_631);
    assert_eq!(release.category_ids, vec![3040]);
    assert_eq!(release.grabs, Some(205));
    assert_eq!(release.files, Some(113));
    assert_eq!(
        release.password, 0,
        "ReleaseResource carries no password signal"
    );
    assert!(
        release
            .usenet_date
            .is_some_and(|date| date > 1_700_000_000.0)
    );
    assert!(
        release.nzb_url.contains("apikey=MOCKKEY"),
        "self-contained URL kept as-is"
    );

    let calls = log.calls();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].query, "Radiohead In Rainbows");
    assert_eq!(
        calls[0].categories,
        vec!["3040".to_owned(), "3000".to_owned()]
    );
    assert_eq!(calls[0].indexer_ids, vec!["7".to_owned()]);
    assert_eq!(calls[0].limit, "50");
    assert!(calls[0].api_key_present);

    let log = ProwlarrMock::new();
    let (base, _server) = serve_loopback(log.router(ProwlarrScenario {
        torrents_only: true,
        ..ProwlarrScenario::default()
    }))
    .await
    .expect("mock serves");
    let client = prowlarr_client(&base);
    let releases = client
        .search("x", &[], &[], 100, TIMEOUT)
        .await
        .expect("search");
    assert!(releases.is_empty(), "v1 is usenet-only");
}

/// The instance key is never logged: it appears in no Debug output and
/// no error string, even though the NZB URLs embed it.
#[tokio::test]
async fn prowlarr_apikey_never_logged() {
    let log = ProwlarrMock::new();
    let (base, _server) = serve_loopback(log.router(ProwlarrScenario {
        search_500: true,
        ..ProwlarrScenario::default()
    }))
    .await
    .expect("mock serves");
    let client = prowlarr_client(&base);

    let debug = format!("{client:?}");
    assert!(debug.contains("<redacted>"), "{debug}");
    assert!(!debug.contains("SUPERSECRET"), "{debug}");

    let err = client
        .search("x", &[], &[], 100, TIMEOUT)
        .await
        .expect_err("500");
    assert!(!err.to_string().contains("SUPERSECRET"));

    let dead = ProwlarrClient::new(http(), &refused_url("").await, "SUPERSECRET", "prowlarr");
    let err = dead
        .search("x", &[], &[], 100, TIMEOUT)
        .await
        .expect_err("transport");
    assert!(!err.to_string().contains("SUPERSECRET"), "{err}");

    // The URL-less and torrent rows never leak their (empty) URLs either:
    // mapping output carries only the self-contained usenet URL.
    let log = ProwlarrMock::new();
    let (base, _server) = serve_loopback(log.router(ProwlarrScenario::default()))
        .await
        .expect("mock serves");
    let client = prowlarr_client(&base);
    let releases = client
        .search("x", &[], &[], 100, TIMEOUT)
        .await
        .expect("search");
    assert_eq!(releases.len(), 1);
}

// ---------------------------------------------------------------------------
// Hostile indexer responses: too deep, too large.
// ---------------------------------------------------------------------------

/// An indexer that answers every path with `body`, streamed in pieces
/// with no `Content-Length`, the way a hostile one would hide its size.
async fn serve_streamed(body: Vec<u8>) -> (String, tokio::task::JoinHandle<()>) {
    let body = std::sync::Arc::new(body);
    let router = axum::Router::new().fallback(move || {
        let body = body.clone();
        async move {
            let pieces: Vec<Result<axum::body::Bytes, std::io::Error>> = body
                .chunks(64 * 1024)
                .map(|piece| Ok(axum::body::Bytes::copy_from_slice(piece)))
                .collect();
            axum::body::Body::from_stream(futures_util::stream::iter(pieces))
        }
    });
    serve_loopback(router).await.expect("mock serves")
}

/// A document nested far deeper than any feed is an error, not a stack
/// overflow that takes the server down.
#[tokio::test]
async fn newznab_deeply_nested_document_is_refused() {
    let depth = 100_000;
    let body = format!("{}{}", "<a>".repeat(depth), "</a>".repeat(depth));
    let (base, _server) = serve_streamed(body.into_bytes()).await;
    let err = newznab_client(&base, "deep", "Deep")
        .caps(TIMEOUT)
        .await
        .expect_err("too deep");
    assert!(
        matches!(err, NewznabError::Client(ClientError::Decode { .. })),
        "{err:?}"
    );
}

/// A body past the cap is refused while it is being read, whatever it
/// holds: here a well-formed feed padded with one long comment.
#[tokio::test]
async fn oversized_indexer_bodies_are_refused() {
    let padding = "x".repeat(usenet::MAX_INDEXER_BODY_BYTES);
    let feed = format!("<rss><channel><!-- {padding} --></channel></rss>");
    let (base, _server) = serve_streamed(feed.into_bytes()).await;
    let err = newznab_client(&base, "big", "Big")
        .search("x", &[], 0, 100, TIMEOUT)
        .await
        .expect_err("too large");
    assert!(
        matches!(err, NewznabError::Client(ClientError::Decode { .. })),
        "{err:?}"
    );

    let rows = format!("[{}]", " ".repeat(usenet::MAX_INDEXER_BODY_BYTES));
    let (base, _server) = serve_streamed(rows.into_bytes()).await;
    let err = prowlarr_client(&base)
        .search("x", &[], &[], 100, TIMEOUT)
        .await
        .expect_err("too large");
    assert!(matches!(err, ClientError::Decode { .. }), "{err:?}");
}

/// An NZB or a SABnzbd answer past its cap is an error, however the
/// server sends it: here both arrive in pieces with no declared length.
#[tokio::test]
async fn oversized_nzbs_and_sabnzbd_answers_are_refused() {
    let nzb = format!(
        "<nzb><!-- {} --></nzb>",
        "x".repeat(usenet::sabnzbd::MAX_NZB_BYTES)
    );
    let (base, _server) = serve_streamed(nzb.into_bytes()).await;
    let err = sab_client(&base)
        .fetch_nzb(&format!("{base}/big.nzb"), TIMEOUT)
        .await
        .expect_err("too large");
    assert!(matches!(err, NzbFetchError::TooLarge), "{err:?}");

    let answer = format!(
        "{{\"queue\": {{\"slots\": []}}{}}}",
        " ".repeat(usenet::sabnzbd::MAX_SABNZBD_BODY_BYTES)
    );
    let (base, _server) = serve_streamed(answer.into_bytes()).await;
    let err = sab_client(&base)
        .queue(TIMEOUT)
        .await
        .expect_err("too large");
    assert!(
        matches!(err, SabnzbdError::Client(ClientError::Decode { .. })),
        "{err:?}"
    );
}

/// If SABnzbd's failure text for a job quotes the URL it was given, the
/// indexer key in it never reaches the task error.
#[tokio::test]
async fn sab_failure_text_is_stored_without_the_indexer_key() {
    let mock = SabnzbdMock::new();
    mock.history_job(
        "nzo-f",
        "job-f",
        "Failed",
        "",
        0,
        "URL Fetching failed; https://idx.test/getnzb?id=1&apikey=SECRETKEY",
    );
    let (base, _server) = serve_loopback(mock.router()).await.expect("mock serves");
    let mount = temp_dir("fail-text");
    let queue = queue_for(&mock, &base, mount.to_path_buf());
    let status = status_for(&mock, &queue, "job-f", "nzo-f").await;
    assert_eq!(
        status.error.as_deref(),
        Some("URL Fetching failed; https://idx.test/getnzb?id=1&apikey=***")
    );
    std::fs::remove_dir_all(&mount).ok();
}

// ---------------------------------------------------------------------------
// Transport failures never carry the request URL.
// ---------------------------------------------------------------------------

/// Stands in for an API key. Anything that prints it has leaked a URL.
const MARKER: &str = "MARKERKEY7f3a91";

type LogBuffer = std::sync::Arc<std::sync::Mutex<Vec<u8>>>;

/// Collect this thread's log output until the guard drops.
fn capture_logs() -> (LogBuffer, tracing::subscriber::DefaultGuard) {
    struct Capture(LogBuffer);
    impl std::io::Write for Capture {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().expect("log buffer").extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let buffer = LogBuffer::default();
    let subscriber = tracing_subscriber::fmt()
        .with_writer({
            let buffer = buffer.clone();
            move || Capture(buffer.clone())
        })
        .with_max_level(tracing::level_filters::LevelFilter::TRACE)
        .finish();
    (buffer, tracing::subscriber::set_default(subscriber))
}

fn logged(buffer: &LogBuffer) -> String {
    String::from_utf8_lossy(&buffer.lock().expect("log buffer")).into_owned()
}

/// The error as a log line, a stored reason or a debug dump would show it.
fn shown(error: &(impl std::fmt::Display + std::fmt::Debug)) -> String {
    format!("{error} {error:?}")
}

/// SABnzbd takes its key as a query parameter on every call, so a dead
/// SABnzbd must not put the request URL in an error or a log line. The
/// NZB fetch is the same: the enclosure URL carries the indexer's key.
#[tokio::test]
async fn sab_transport_errors_never_carry_the_url() {
    let (logs, _guard) = capture_logs();
    let base = refused_url("").await;
    let client = || SabnzbdClient::new(http(), &base, MARKER, 1, Duration::from_millis(1));

    let err = client().queue(TIMEOUT).await.expect_err("refused");
    assert!(
        matches!(err, SabnzbdError::Client(ClientError::Unavailable { .. })),
        "{err:?}"
    );
    assert!(!shown(&err).contains(MARKER), "{}", shown(&err));

    let err = client()
        .add_file("job", b"<nzb/>", None, None, None, TIMEOUT)
        .await
        .expect_err("refused");
    assert!(!shown(&err).contains(MARKER), "{}", shown(&err));

    let enclosure = format!("{base}/getnzb?id=1&token={MARKER}");
    let err = client()
        .add_url("job", &enclosure, None, None, None, TIMEOUT)
        .await
        .expect_err("refused");
    assert!(!shown(&err).contains(MARKER), "{}", shown(&err));

    let err = client()
        .fetch_nzb(&enclosure, TIMEOUT)
        .await
        .expect_err("refused");
    assert!(matches!(err, NzbFetchError::Transport(_)), "{err:?}");
    assert!(!shown(&err).contains(MARKER), "{}", shown(&err));

    let mount = temp_dir("sab-dead");
    let queue = SabnzbdQueue::new(client(), &base, MARKER, mount.to_path_buf(), test_policy());
    assert_eq!(queue.health_check().await.status, "error");
    let text = logged(&logs);
    assert!(text.contains("sabnzbd health check failed"), "{text}");
    assert!(!text.contains(MARKER), "{text}");
}

/// Newznab takes its key as a query parameter too. A dead indexer is
/// logged by the health check, the caps fetch and the search fan-out.
#[tokio::test]
async fn newznab_transport_errors_never_carry_the_url() {
    let (logs, _guard) = capture_logs();
    let base = refused_url("/api").await;
    let client = || NewznabClient::new(http(), &base, MARKER, "dead", "Dead");

    let err = client().caps(TIMEOUT).await.expect_err("refused");
    assert!(!shown(&err).contains(MARKER), "{}", shown(&err));

    let indexer = newznab_indexer(vec![entry_for(client(), "dead", "Dead", 0)]);
    assert_eq!(indexer.health_check().await.status, "error");
    assert!(
        indexer
            .search_album("Radiohead", "In Rainbows", None, TIMEOUT)
            .await
            .is_empty()
    );
    let text = logged(&logs);
    assert!(text.contains("newznab indexer search failed"), "{text}");
    assert!(!text.contains(MARKER), "{text}");
}

/// Prowlarr's key travels in a header, but the base URL is the user's
/// and can hold a secret of its own (a proxy path, basic-auth userinfo).
#[tokio::test]
async fn prowlarr_transport_errors_never_carry_the_url() {
    let (logs, _guard) = capture_logs();
    let base = refused_url(&format!("/{MARKER}")).await;
    let client = || ProwlarrClient::new(http(), &base, "SUPERSECRET", "prowlarr");

    let err = client()
        .search("x", &[], &[], 100, TIMEOUT)
        .await
        .expect_err("refused");
    assert!(!shown(&err).contains(MARKER), "{}", shown(&err));

    let indexer = usenet::prowlarr::ProwlarrIndexer::new(
        Some(client()),
        vec![3000],
        true,
        Duration::from_secs(300),
        Duration::from_secs(300),
        TIMEOUT,
    );
    assert!(
        indexer
            .search_album("Radiohead", "In Rainbows", TIMEOUT)
            .await
            .is_empty()
    );
    let text = logged(&logs);
    assert!(text.contains("prowlarr search failed"), "{text}");
    assert!(!text.contains(MARKER), "{text}");
}

// ---------------------------------------------------------------------------
// Policy: tiers, recipe, timeouts, retention.
// ---------------------------------------------------------------------------

/// Secrets never surface in Debug: the SAB key, the key-bearing NZB URLs,
/// and the history password all render redacted.
#[test]
fn usenet_secrets_never_logged() {
    let client = sab_client("http://127.0.0.1:9");
    let debug = format!("{client:?}");
    assert!(debug.contains("<redacted>"), "{debug}");
    assert!(!debug.contains("SABKEY"), "{debug}");

    let release = UsenetRelease {
        indexer_id: "idx".to_owned(),
        indexer_name: "idx".to_owned(),
        guid: "g".to_owned(),
        title: "t".to_owned(),
        nzb_url: "https://idx/getnzb?apikey=SUPERSECRET".to_owned(),
        size_bytes: 1,
        category_ids: Vec::new(),
        grabs: None,
        files: None,
        usenet_date: None,
        password: 0,
    };
    let debug = format!("{release:?}");
    assert!(!debug.contains("SUPERSECRET"), "{debug}");

    let prowlarr = ProwlarrRelease {
        download_url: "https://prowlarr/dl?apikey=SUPERSECRET".to_owned(),
        ..ProwlarrRelease::default()
    };
    let debug = format!("{prowlarr:?}");
    assert!(!debug.contains("SUPERSECRET"), "{debug}");

    let slot = HistorySlot {
        password: Some("nzb-password".to_owned()),
        ..HistorySlot::default()
    };
    let debug = format!("{slot:?}");
    assert!(!debug.contains("nzb-password"), "{debug}");
}

/// Crash-retry usenet enqueue re-attaches instead of double-adding: the
/// deterministic job name is already on the client, so the adapter
/// returns it with zero add calls. With no job present the search still
/// runs and reports no candidate.
#[tokio::test]
async fn usenet_enqueue_reattaches_to_existing_job() {
    use droppedneedle_acquire::downloads::journal::Journal;
    use droppedneedle_acquire::downloads::sources::{DownloadSource, SourceError};
    use droppedneedle_acquire::downloads::store::NewTask;
    use droppedneedle_acquire::sources::SabnzbdSource;
    use std::sync::Arc;

    let task_id = "0123456789abcdef0123456789abcdef";
    let job_name = format!("droppedneedle-{task_id}-0");
    let mock = SabnzbdMock::new();
    mock.queue_job("nzo-1", &job_name, "Downloading", "100.0", "50.0", "50");
    let (base, _server) = serve_loopback(mock.router()).await.expect("mock serves");

    let db = droppedneedle_acquire::db::AcquireDb::scratch().expect("scratch db");
    db.add_user("u1", "U", "user").await.expect("user seeds");
    let journal = Arc::new(Journal::new(db, quiet_ticks()));
    journal
        .run("test.seed", |store| {
            store.insert_task(
                &NewTask {
                    id: task_id.to_owned(),
                    user_id: "u1".to_owned(),
                    artist_name: "artist".to_owned(),
                    album_title: "album".to_owned(),
                    release_group_mbid: "rg-1".to_owned(),
                    origin: "user".to_owned(),
                    retry_count: 0,
                },
                1_700_000_000.0,
            )
        })
        .await
        .expect("task inserts");
    let policy = UsenetPolicy::v2_defaults();
    let queue = Arc::new(SabnzbdQueue::new(
        SabnzbdClient::new(
            reqwest::Client::new(),
            &base,
            "SABKEY",
            3,
            Duration::from_millis(1),
        ),
        &base,
        "SABKEY",
        std::env::temp_dir(),
        policy.clone(),
    ));
    let source = SabnzbdSource::new(
        queue,
        vec![Arc::new(NewznabIndexer::new(
            Vec::new(),
            Duration::from_secs(5),
            Duration::from_secs(5),
            Duration::from_secs(5),
            Duration::from_secs(5),
        ))],
        policy,
        journal,
        None,
    );
    let handle = source.enqueue(task_id, 0).await.expect("re-attaches");
    assert_eq!(handle.job_name, job_name);
    assert!(mock.state().add_file_requests.is_empty());
    assert!(mock.state().add_url_requests.is_empty());
    assert_eq!(handle.nzo_id, "nzo-1", "the handle keeps SABnzbd's job id");

    // SABnzbd renames the finished job; the poll still finds it by id.
    mock.state().queue_slots.clear();
    mock.history_job(
        "nzo-1",
        &format!("{job_name}.1"),
        "Completed",
        "/downloads/complete/renamed",
        1024,
        "",
    );
    let progress = source.poll(&handle).await.expect("poll answers");
    assert!(
        progress.all_terminal && progress.all_succeeded,
        "{progress:?}"
    );

    let missing = "fedcba9876543210fedcba9876543210";
    let err = source
        .enqueue(missing, 0)
        .await
        .expect_err("unknown task rejects");
    assert!(matches!(err, SourceError::Rejected { .. }));
}

// ---------------------------------------------------------------------------
// One error for every client.
// ---------------------------------------------------------------------------

/// The statuses each mock answers in turn: a wrong key, a missing item, a
/// bad request, and a rate limit naming its wait.
const REFUSALS: [(u16, Option<&str>); 4] =
    [(401, None), (404, None), (400, None), (429, Some("5"))];

/// The four refusals came back as four different errors, the rate limit
/// with its wait.
fn assert_distinct(errors: &[ClientError]) {
    assert!(
        matches!(
            errors[0],
            ClientError::Auth {
                status: Some(401),
                ..
            }
        ),
        "{errors:?}"
    );
    assert!(
        matches!(errors[1], ClientError::NotFound { .. }),
        "{errors:?}"
    );
    assert!(
        matches!(errors[2], ClientError::BadRequest { .. }),
        "{errors:?}"
    );
    assert_eq!(
        errors[3].retry_after(),
        Some(Duration::from_secs(5)),
        "{errors:?}"
    );
}

/// SABnzbd, Newznab and Prowlarr each keep a wrong key, a missing item, a
/// bad request and a rate limit apart.
#[tokio::test]
async fn statuses_map_to_distinct_client_errors() {
    let sab = SabnzbdMock::new();
    let (base, _sab) = serve_loopback(sab.router()).await.expect("mock serves");
    let client = sab_client(&base);
    let mut errors = Vec::new();
    for (status, wait) in REFUSALS {
        sab.statuses().answer_next(status, wait);
        match client.version(TIMEOUT).await.expect_err("scripted") {
            SabnzbdError::Client(error) => errors.push(error),
            other => panic!("not a client error: {other:?}"),
        }
    }
    assert_distinct(&errors);

    let newznab = NewznabMock::new();
    let (base, _newznab) = serve_loopback(newznab.router(NewznabScenario::DrunkenSlug))
        .await
        .expect("mock serves");
    let client = newznab_client(&base, "ds", "DrunkenSlug");
    let mut errors = Vec::new();
    for (status, wait) in REFUSALS {
        newznab.statuses().answer_next(status, wait);
        match client.caps(TIMEOUT).await.expect_err("scripted") {
            NewznabError::Client(error) => errors.push(error),
            other => panic!("not a client error: {other:?}"),
        }
    }
    assert_distinct(&errors);

    let prowlarr = ProwlarrMock::new();
    let (base, _prowlarr) = serve_loopback(prowlarr.router(ProwlarrScenario::default()))
        .await
        .expect("mock serves");
    let client = prowlarr_client(&base);
    let mut errors = Vec::new();
    for (status, wait) in REFUSALS {
        prowlarr.statuses().answer_next(status, wait);
        errors.push(client.list_indexers(TIMEOUT).await.expect_err("scripted"));
    }
    assert_distinct(&errors);
}

/// A row missing its identity field fails decoding instead of reading as
/// an empty id.
#[test]
fn rows_without_their_identity_fail_to_decode() {
    let queue = serde_json::json!({"status": "Downloading", "filename": "job"});
    assert!(serde_json::from_value::<QueueSlot>(queue).is_err());
    let history = serde_json::json!({"nzo_id": "nzo-1", "status": "Completed"});
    assert!(serde_json::from_value::<HistorySlot>(history).is_err());

    let release = serde_json::json!({
        "title": "Radiohead - In Rainbows (2007) [FLAC]",
        "size": 2315726631_i64,
        "indexerId": 7,
        "protocol": "usenet",
        "downloadUrl": "https://prowlarr.test/9/download",
    });
    assert!(serde_json::from_value::<ProwlarrRelease>(release).is_err());
    let member = serde_json::json!({"name": "NZBGeek", "protocol": "usenet", "enable": true});
    assert!(serde_json::from_value::<ProwlarrIndexerInfo>(member).is_err());
}
