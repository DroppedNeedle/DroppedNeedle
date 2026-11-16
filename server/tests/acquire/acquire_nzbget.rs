//! NZBGet contract tests against the in-repo JSON-RPC mock, which answers
//! from the recordings of NZBGet 26.3 in `tests/fixtures/nzbget/`.
//! Transport is real HTTP over `127.0.0.1` only.

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
        factory.same_host_redirects().clone(),
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
/// bytes, and history follows NZBGet's documented statuses: SUCCESS and
/// WARNING/SCRIPT complete, any other WARNING fails like FAILURE and a
/// delete, so the worker fails over. Two jobs under one name refuse to
/// guess.
#[tokio::test]
async fn nzbget_status_walk() {
    let mock = NzbgetMock::new();
    mock.group_job(1, "job-down", "DOWNLOADING", 400, 100)
        .group_job(2, "job-paused", "PAUSED", 400, 400)
        .group_job(3, "job-unpack", "UNPACKING", 400, 0)
        .group_job(7, "job-twice", "QUEUED", 1, 1)
        .group_job(8, "job-twice", "QUEUED", 1, 1);
    for (id, status) in [
        (10, "SUCCESS/ALL"),
        (11, "SUCCESS/HEALTH"),
        (12, "WARNING/SCRIPT"),
        (13, "WARNING/DAMAGED"),
        (14, "WARNING/REPAIRABLE"),
        (15, "WARNING/PASSWORD"),
        (16, "WARNING/SPACE"),
        (17, "WARNING/HEALTH"),
        (18, "FAILURE/PAR"),
        (19, "DELETED/MANUAL"),
    ] {
        mock.history_job(
            id,
            &format!("job-{id}"),
            status,
            "/downloads/completed/x",
            400,
        );
    }
    let (base, _server) = serve_loopback(mock.router()).await.expect("mock serves");
    let mount = crate::common::ScratchDir::new("nzbget-walk");
    let queue = queue_for(&base, "", "", mount.to_path_buf());
    let status = |job: String, id: &'static str| {
        let queue = &queue;
        async move { queue.get_status(&handle(&job, id)).await.expect("status") }
    };

    let down = status("job-down".into(), "1").await;
    assert_eq!(down.status, "downloading");
    assert!(down.has_active_transfer);
    assert_eq!((down.bytes_total, down.bytes_downloaded), (400, 300));

    let paused = status("job-paused".into(), "2").await;
    assert_eq!(paused.status, "queued");
    assert!(!paused.has_active_transfer);
    assert_eq!(paused.bytes_downloaded, 0);

    assert_eq!(status("job-unpack".into(), "3").await.status, "processing");

    let ids = ["10", "11", "12", "13", "14", "15", "16", "17", "18", "19"];
    let mut walked = Vec::new();
    for id in ids {
        walked.push(status(format!("job-{id}"), id).await.status);
    }
    assert_eq!(
        walked,
        [
            "completed",
            "completed",
            "completed",
            "failed",
            "failed",
            "failed",
            "failed",
            "failed",
            "failed",
            "failed",
        ]
    );
    let deleted = status("job-19".into(), "19").await;
    assert_eq!(deleted.error.as_deref(), Some("job removed from NZBGet"));

    // Before the id is known the name matches, once.
    assert_eq!(status("job-down".into(), "").await.status, "downloading");
    let twice = queue.get_status(&handle("job-twice", "")).await;
    assert!(
        matches!(twice, Err(NzbgetError::AmbiguousIdentity)),
        "{twice:?}"
    );
}

/// A stored id matches only with the job name DroppedNeedle sent: NZBGet
/// numbers jobs from 1 again on a fresh queue, so after a reinstall id 1
/// can be another app's job. That job reads as missing, and abort and
/// discard leave it alone. Our own job is aborted for good, a failed
/// record is cleared, completed output is left to the cleanup service,
/// and discard hides a completed record.
#[tokio::test]
async fn nzbget_id_matches_only_with_its_job_name() {
    let mock = NzbgetMock::new();
    mock.group_job(1, "someone-elses-job", "DOWNLOADING", 10, 5)
        .group_job(4, "job-q", "QUEUED", 10, 10)
        .history_job(2, "other-done", "SUCCESS/ALL", "/downloads/completed/o", 10)
        .history_job(5, "job-done", "SUCCESS/ALL", "/downloads/completed/d", 10)
        .history_job(
            6,
            "job-bad",
            "FAILURE/HEALTH",
            "/downloads/intermediate/b",
            10,
        );
    let (base, _server) = serve_loopback(mock.router()).await.expect("mock serves");
    let mount = crate::common::ScratchDir::new("nzbget-ids");
    let queue = queue_for(&base, "", "", mount.to_path_buf());

    let reused = handle("droppedneedle-t1-0", "1");
    assert_eq!(
        queue.get_status(&reused).await.expect("status").status,
        "missing"
    );
    assert!(queue.abort(&reused).await.expect("abort"));
    let finished = handle("droppedneedle-t2-0", "2");
    assert!(
        queue
            .discard_client_artifacts(&finished)
            .await
            .expect("discard")
    );
    assert!(
        mock.calls_to("editqueue").is_empty(),
        "nothing of theirs is touched"
    );

    assert!(queue.abort(&handle("job-q", "4")).await.expect("abort"));
    assert!(!queue.abort(&handle("job-done", "5")).await.expect("abort"));
    assert!(queue.abort(&handle("job-bad", "6")).await.expect("abort"));
    assert!(
        queue
            .discard_client_artifacts(&handle("job-done", "5"))
            .await
            .expect("discard")
    );
    let commands: Vec<_> = mock
        .calls_to("editqueue")
        .into_iter()
        .map(|call| (call.params[0].clone(), call.params[2].clone()))
        .collect();
    assert_eq!(
        commands,
        vec![
            (json!("GroupFinalDelete"), json!([4])),
            (json!("HistoryFinalDelete"), json!([6])),
            (json!("HistoryDelete"), json!([5])),
        ]
    );
    let state = mock.state();
    assert_eq!(state.groups.len(), 1);
    assert_eq!(state.history.len(), 1);
}
