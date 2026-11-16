//! NZBGet contract tests against the in-repo JSON-RPC mock, whose shapes
//! were checked against NZBGet 26.3. Transport is real HTTP over
//! `127.0.0.1` only.

use std::path::PathBuf;
use std::time::Duration;

use droppedneedle_acquire::usenet::nzbget::{NzbgetClient, NzbgetError, NzbgetQueue};
use droppedneedle_acquire::usenet::policy::UsenetPolicy;
use droppedneedle_acquire::usenet::sabnzbd::{NzbGrab, TaskHandle};
use droppedneedle_testkit::nzbget::NzbgetMock;
use droppedneedle_testkit::usenet::serve_loopback;
use serde_json::json;

const TIMEOUT: Duration = Duration::from_secs(10);

fn queue_for(base: &str, username: &str, password: &str, mount: PathBuf) -> NzbgetQueue {
    let factory =
        droppedneedle_kernel::http_client::HttpClientFactory::new().expect("http client factory");
    let client = NzbgetClient::new(
        reqwest::Client::builder().build().expect("test client"),
        factory.no_redirect().clone(),
        base,
        username,
        password,
        3,
        Duration::from_millis(1),
    );
    let policy = UsenetPolicy {
        indexer_timeout: TIMEOUT,
        enqueue_timeout: TIMEOUT,
        poll_timeout: TIMEOUT,
        ..UsenetPolicy::v2_defaults()
    };
    NzbgetQueue::new(client, base, mount, policy)
}

fn handle(job: &str, id: &str) -> TaskHandle {
    TaskHandle {
        source: "usenet".to_owned(),
        job_name: job.to_owned(),
        nzo_id: id.to_owned(),
    }
}

/// An add sends the fetched NZB as base64 with the job name as both the
/// NZB name and the dupe key, the category and priority given, and basic
/// auth; the answered id becomes the handle. An id of 0 is a refusal, and
/// no link is no add.
#[tokio::test]
async fn nzbget_enqueue_wire_shape() {
    let mock = NzbgetMock::new();
    mock.state().credentials = Some(("nzbget".to_owned(), "secret".to_owned()));
    let (base, _server) = serve_loopback(mock.router()).await.expect("mock serves");
    let mount = crate::common::ScratchDir::new("nzbget-add");
    let queue = queue_for(&base, "nzbget", "secret", mount.to_path_buf());
    let link = format!("{base}/nzb/good");

    let added = queue
        .enqueue_album(
            "t1",
            Some(&link),
            NzbGrab::Direct,
            Some("Music"),
            Some(50),
            None,
        )
        .await
        .expect("added");
    assert_eq!(added.job_name, "droppedneedle-t1");
    assert_eq!(added.nzo_id, "1");
    assert_eq!(added.source, "usenet");

    let appends = mock.calls_to("append");
    assert_eq!(appends.len(), 1);
    let params = &appends[0].params;
    assert!(appends[0].authorized);
    assert_eq!(params[0], json!("droppedneedle-t1.nzb"));
    let content = params[1].as_str().expect("content");
    assert!(
        !content.is_empty() && !content.starts_with("http"),
        "NZB bytes, not the link"
    );
    assert_eq!(params[2], json!("Music"));
    assert_eq!(params[3], json!(50));
    assert_eq!(params[6], json!("droppedneedle-t1"));
    assert_eq!(params[8], json!("FORCE"));

    mock.state().next_id = 0;
    let refused = queue
        .enqueue_album("t2", Some(&link), NzbGrab::Direct, None, None, None)
        .await;
    assert!(
        matches!(refused, Err(NzbgetError::RejectedNzb)),
        "{refused:?}"
    );
    let missing = queue
        .enqueue_album("t3", None, NzbGrab::Direct, None, None, None)
        .await;
    assert!(
        matches!(missing, Err(NzbgetError::MissingNzbUrl)),
        "{missing:?}"
    );
}

/// A wrong password is HTTP 401: health names the sign-in, and other
/// calls fail as auth, never as "unreachable".
#[tokio::test]
async fn nzbget_refused_sign_in_reads_as_auth() {
    let mock = NzbgetMock::new();
    mock.state().credentials = Some(("nzbget".to_owned(), "secret".to_owned()));
    let (base, _server) = serve_loopback(mock.router()).await.expect("mock serves");
    let mount = crate::common::ScratchDir::new("nzbget-auth");
    let queue = queue_for(&base, "nzbget", "wrong", mount.to_path_buf());

    let health = queue.health_check().await;
    assert_eq!(health.status, "error");
    assert!(
        health.message.contains("username or password"),
        "{}",
        health.message
    );
    let categories = queue.get_categories().await.expect_err("refused");
    assert!(categories.is_auth(), "{categories:?}");

    let good = queue_for(&base, "nzbget", "secret", mount.to_path_buf());
    let health = good.health_check().await;
    assert_eq!(health.status, "ok");
    assert_eq!(health.version.as_deref(), Some("26.3"));
}

/// Queue then history: only DOWNLOADING is active, queued states move no
/// bytes, history maps SUCCESS, FAILURE and a manual delete, and two jobs
/// under one name refuse to guess.
#[tokio::test]
async fn nzbget_status_walk() {
    let mock = NzbgetMock::new();
    mock.group_job(1, "job-down", "DOWNLOADING", 400, 100)
        .group_job(2, "job-paused", "PAUSED", 400, 400)
        .group_job(3, "job-unpack", "UNPACKING", 400, 0)
        .history_job(
            4,
            "job-done",
            "SUCCESS/ALL",
            "/downloads/completed/job-done",
            400,
        );
    mock.state().history.push(json!({
        "NZBID": 5, "Name": "job-bad", "NZBName": "job-bad", "DupeKey": "job-bad",
        "Status": "FAILURE/UNPACK", "DestDir": "/downloads/intermediate/job-bad",
        "FinalDir": "", "ParStatus": "SUCCESS", "UnpackStatus": "FAILURE",
        "MoveStatus": "NONE", "ScriptStatus": "NONE",
        "FileSizeLo": 400, "FileSizeHi": 0, "Category": "Music",
    }));
    mock.group_job(6, "job-gone", "QUEUED", 400, 400)
        .group_job(7, "job-twice", "QUEUED", 1, 1)
        .group_job(8, "job-twice", "QUEUED", 1, 1);
    let (base, _server) = serve_loopback(mock.router()).await.expect("mock serves");
    let mount = crate::common::ScratchDir::new("nzbget-walk");
    let queue = queue_for(&base, "", "", mount.to_path_buf());
    let status = |job: &'static str, id: &'static str| {
        let queue = &queue;
        async move { queue.get_status(&handle(job, id)).await.expect("status") }
    };

    let down = status("job-down", "1").await;
    assert_eq!(down.status, "downloading");
    assert!(down.has_active_transfer);
    assert_eq!((down.bytes_total, down.bytes_downloaded), (400, 300));
    assert_eq!(down.progress_percent, 75.0);

    let paused = status("job-paused", "2").await;
    assert_eq!(paused.status, "queued");
    assert!(!paused.has_active_transfer);
    assert_eq!(paused.bytes_downloaded, 0);

    let unpack = status("job-unpack", "3").await;
    assert_eq!(unpack.status, "processing");
    assert_eq!(unpack.progress_percent, 100.0);

    let done = status("job-done", "4").await;
    assert_eq!(done.status, "completed");
    assert_eq!(done.bytes_downloaded, 400);

    let bad = status("job-bad", "5").await;
    assert_eq!(bad.status, "failed");
    assert_eq!(bad.error.as_deref(), Some("unpack failed: FAILURE"));

    // A job deleted in NZBGet's own UI lands in history as DELETED.
    let deleted = mock.state().groups.len();
    let removed = reqwest::Client::new()
        .post(format!("{base}/jsonrpc"))
        .body(json!({"method": "editqueue", "params": ["GroupDelete", "", [6]]}).to_string())
        .send()
        .await
        .expect("delete");
    assert!(removed.status().is_success());
    assert_eq!(mock.state().groups.len(), deleted - 1);
    let gone = status("job-gone", "6").await;
    assert_eq!(gone.status, "failed");
    assert_eq!(gone.error.as_deref(), Some("job removed from NZBGet"));

    // Before the id is known the name matches, once.
    assert_eq!(status("job-down", "").await.status, "downloading");
    let twice = queue.get_status(&handle("job-twice", "")).await;
    assert!(
        matches!(twice, Err(NzbgetError::AmbiguousIdentity)),
        "{twice:?}"
    );
    assert_eq!(status("nothing", "99").await.status, "missing");
}

/// Abort drops a queued job and its partial data for good and clears a
/// failed record, but leaves completed output to the cleanup service.
/// Discard hides a completed record after local cleanup.
#[tokio::test]
async fn nzbget_abort_and_discard_ownership() {
    let mock = NzbgetMock::new();
    mock.group_job(1, "job-q", "QUEUED", 10, 10)
        .history_job(
            2,
            "job-done",
            "SUCCESS/ALL",
            "/downloads/completed/job-done",
            10,
        )
        .history_job(
            3,
            "job-bad",
            "FAILURE/HEALTH",
            "/downloads/intermediate/job-bad",
            10,
        );
    let (base, _server) = serve_loopback(mock.router()).await.expect("mock serves");
    let mount = crate::common::ScratchDir::new("nzbget-abort");
    let queue = queue_for(&base, "", "", mount.to_path_buf());

    assert!(queue.abort(&handle("job-q", "1")).await.expect("abort"));
    assert!(!queue.abort(&handle("job-done", "2")).await.expect("abort"));
    assert!(queue.abort(&handle("job-bad", "3")).await.expect("abort"));
    let commands: Vec<_> = mock
        .calls_to("editqueue")
        .into_iter()
        .map(|call| (call.params[0].clone(), call.params[2].clone()))
        .collect();
    assert_eq!(
        commands,
        vec![
            (json!("GroupFinalDelete"), json!([1])),
            (json!("HistoryFinalDelete"), json!([3])),
        ]
    );
    assert!(mock.state().groups.is_empty());

    assert!(
        queue
            .discard_client_artifacts(&handle("job-done", "2"))
            .await
            .expect("discard")
    );
    let last = mock.calls_to("editqueue").pop().expect("discard call");
    assert_eq!(last.params[0], json!("HistoryDelete"));
    assert!(mock.state().history.is_empty());
}

/// NZBGet's `DestDir` expands its `${MainDir}` reference, and a finished
/// job's folder maps under the mount; the categories feed the picker.
#[tokio::test]
async fn nzbget_materialization_and_mount() {
    let mount = crate::common::ScratchDir::new("nzbget-material");
    let job = mount.join("Music/job1");
    std::fs::create_dir_all(&job).expect("job dir");
    std::fs::write(job.join("track.flac"), b"FLAC").expect("write");
    let mock = NzbgetMock::new();
    mock.group_job(1, "job-live", "DOWNLOADING", 10, 5)
        .history_job(
            2,
            "job1",
            "SUCCESS/ALL",
            "/downloads/completed/Music/job1",
            4,
        );
    let (base, _server) = serve_loopback(mock.router()).await.expect("mock serves");
    let queue = queue_for(&base, "", "", mount.to_path_buf());

    assert_eq!(queue.client_name(), "nzbget");
    assert!(queue.is_configured());
    assert_eq!(
        queue.get_complete_dir().await.expect("dest dir"),
        "/downloads/completed"
    );
    assert_eq!(
        queue.get_categories().await.expect("categories"),
        vec!["Movies".to_owned(), "Music".to_owned()]
    );

    let live = queue
        .inspect_materialization(&handle("job-live", "1"))
        .await
        .expect("active");
    assert_eq!(live.state, "active");
    assert!(live.mount_healthy);

    let done = queue
        .inspect_materialization(&handle("job1", "2"))
        .await
        .expect("done");
    assert_eq!(done.state, "completed");
    assert_eq!(done.remote_storage, "/downloads/completed/Music/job1");
    assert_eq!(done.workspace_path, job.display().to_string());

    let diagnosis = queue.diagnose_downloads_mount().await;
    assert_eq!(diagnosis.sampled_downloads, 1);
    assert_eq!(diagnosis.resolvable_downloads, 1);
    assert!(diagnosis.mount_has_files);
    assert_eq!(
        diagnosis.client_downloads_dir.as_deref(),
        Some("/downloads/completed")
    );
}
