//! `GET /api/v3/admin/problems` lists what is wrong from what already
//! knows it: a settings section that does not read, a download client that
//! is on and does not answer, and a publish journal row that needs a
//! person each come back as one broken row, and nothing else is broken. A
//! plain user is refused.

use std::collections::BTreeSet;

use axum::http::StatusCode;
use serde_json::json;

use crate::common::TestApp;

#[tokio::test]
async fn the_list_names_each_broken_thing_once_and_only_to_an_admin() {
    let app = TestApp::start("problems").await;
    let admin = app.login_admin().await;
    let headers = [("authorization", admin.as_str())];
    // slskd is on and set up, at an address nothing answers.
    let (status, body) = app
        .call(
            "PUT",
            "/api/v3/settings/download-client/config",
            &headers,
            Some(json!({
                "enabled": true,
                "url": "http://127.0.0.1:9",
                "api_key": "problems-test-key",
                "downloads_subpath": "",
                "slskd_incomplete_mount": "",
            })),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");

    // The Usenet clients' section holds a value that does not open, and
    // one file change waits for a person.
    let (app, ()) = app
        .while_stopped(|config| {
            let db = rusqlite::Connection::open(&config.library_db_path).unwrap();
            db.execute_batch(
                "INSERT INTO config_sections (key, value_json, revision, updated_at) \
                 VALUES ('download_clients', '{\"sabnzbd\": {\"api_key\": \"v3:bm90LWEtcmVhbC1zZWFs\"}}', 1, 1) \
                 ON CONFLICT (key) DO UPDATE SET value_json = excluded.value_json; \
                 INSERT INTO library_publish_journal (id, bundle_id, kind, dest_root, dest_rel, \
                 staged, staged_sha256, state) VALUES ('stuck-000', 'stuck', 'audio', 'music', \
                 'Stuck/01.flac', '.droppedneedle-management-stuck', '', 'needs_attention');",
            )
            .unwrap();
        })
        .await;
    let admin = app.login_admin().await;
    let headers = [("authorization", admin.as_str())];

    let (status, body) = app
        .call("GET", "/api/v3/admin/problems", &headers, None)
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let broken: BTreeSet<(String, String)> = body["problems"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|problem| problem["severity"] == "broken")
        .map(|problem| {
            (
                problem["kind"].as_str().unwrap().to_owned(),
                problem["subject"].as_str().unwrap_or("").to_owned(),
            )
        })
        .collect();
    let want: BTreeSet<(String, String)> = [
        ("settings_unreadable", "download_clients"),
        ("download_client_down", "slskd"),
        ("publish_needs_attention", ""),
    ]
    .into_iter()
    .map(|(kind, subject)| (kind.to_owned(), subject.to_owned()))
    .collect();
    assert_eq!(broken, want, "{body}");
    assert!(body["checked_at"].is_i64(), "{body}");

    let user = app.login_user("plain").await.authorization;
    let (status, _) = app
        .call(
            "GET",
            "/api/v3/admin/problems",
            &[("authorization", user.as_str())],
            None,
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    app.stop().await;
}

/// A folder check that does not answer never holds the list: it answers
/// within its bound with a "could not check" row, and the check's answer,
/// when it comes, is what the next look reads.
#[tokio::test]
async fn a_hung_folder_check_answers_could_not_check_within_its_bound() {
    use std::sync::Arc;
    use std::time::Duration;

    use droppedneedle::admin::problems::{ProblemKind, Problems};
    use droppedneedle_acquire::import_folder::ImportFolderState;
    use tokio::sync::Notify;

    let app = TestApp::start("problems-hang").await;
    let release = Arc::new(Notify::new());
    let gate = Arc::clone(&release);
    let problems = Problems::with_import_status(
        app.state(),
        Arc::new(move || {
            let gate = Arc::clone(&gate);
            Box::pin(async move {
                // Blocks until the test lets go, as a dead mount would.
                gate.notified().await;
                ImportFolderState::Off
            })
        }),
    );
    let could_not_check = |list: &droppedneedle::admin::problems::ProblemsResponse| {
        list.problems.iter().any(|p| {
            p.kind == ProblemKind::CouldNotCheck && p.subject.as_deref() == Some("folder_checks")
        })
    };

    let list = tokio::time::timeout(Duration::from_secs(30), problems.list())
        .await
        .expect("the list answers while the check hangs");
    assert!(could_not_check(&list), "{:?}", list.problems);

    release.notify_one();
    let fresh = app
        .until("the check's answer lands", || async {
            let list = problems.list().await;
            (!could_not_check(&list)).then_some(list)
        })
        .await;
    assert!(
        !fresh.problems.iter().any(|p| p.stale),
        "{:?}",
        fresh.problems
    );

    app.stop().await;
}
