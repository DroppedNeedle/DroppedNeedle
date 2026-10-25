//! Acquire journeys through the whole app: request, approve, land; follow
//! toggle, approval, armed follow; a request held until a source is added.
//! The app starts with no download client, so the worker holds the queue
//! and the first journey simulates landing through the journal.

use std::{path::Path, time::Duration};

use axum::http::StatusCode;
use serde_json::{Value, json};

use droppedneedle_acquire::downloads::state::TaskStatus;

use crate::common::TestApp;

const RG_MBID: &str = "123e4567-e89b-12d3-a456-426614174000";
const ARTIST_MBID: &str = "223e4567-e89b-12d3-a456-426614174001";

/// `(task id, status)` rows in the scratch download journal.
fn journal_tasks(db_path: &Path) -> Vec<(String, String)> {
    let conn = rusqlite::Connection::open(db_path).expect("journal opens");
    let mut stmt = conn
        .prepare("SELECT id, status FROM download_tasks ORDER BY created_at")
        .expect("tasks query prepares");
    stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .expect("tasks query runs")
        .collect::<Result<Vec<_>, _>>()
        .expect("task rows read")
}

// ---------------------------------------------------------------------------
// Journey 1: user requests an album → admin approves → download lands.
// The scan picks the files up later: the journey asserts the staged state
// (journal row completed, ask imported), not the library.
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn acquire_request_approve_land() {
    let app = TestApp::start("request").await;
    let admin = app.login_admin().await;
    let molly = app.login_user("Molly").await.authorization;
    let db_path = app.config.library_db_path.clone();
    let (status, body) = app
        .call(
            "PATCH",
            "/api/v3/me",
            &[("authorization", molly.as_str())],
            Some(json!({"display_name": "M. Profile"})),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");

    // Molly asks; a plain user needs approval.
    let (status, body) = app
        .call(
            "POST",
            "/api/v3/requests/albums",
            &[("authorization", molly.as_str())],
            Some(json!({
                "musicbrainz_id": RG_MBID,
                "artist": "Massive Attack",
                "album": "Blue Lines",
            })),
        )
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    assert_eq!(body["success"], true);
    assert_eq!(body["status"], "awaiting_approval");

    // The request carries the name the session gate read with her token:
    // her login as she typed it, not the lowercase login or her profile
    // name.
    let (status, body) = app
        .call(
            "GET",
            "/api/v3/requests/approvals",
            &[("authorization", admin.as_str())],
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["items"][0]["requested_by_name"], "Molly", "{body}");

    // The admin approves: one queued download task lands in the journal.
    let (status, body) = app
        .call(
            "POST",
            &format!("/api/v3/requests/approvals/{RG_MBID}/approve"),
            &[("authorization", admin.as_str())],
            Some(json!({})),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["success"], true);
    let tasks = journal_tasks(&db_path);
    assert_eq!(tasks.len(), 1, "approval dispatches one task");
    let (task_id, status_text) = &tasks[0];
    assert_eq!(status_text, "queued");
    assert_eq!(task_id.len(), 32, "task ids are 32 hex chars");

    // The worker lands the download (simulated through the journal: the
    // loop settles the task the same way, and the scan picks the files up
    // later). The request follows in the same write.
    let landed = task_id.clone();
    app.state()
        .acquire
        .journal
        .run("test.land", move |store| {
            store.transition_task(&landed, TaskStatus::Completed, 1.0, None)
        })
        .await
        .expect("task completes");

    // Molly's history shows the ask imported.
    let (status, body) = app
        .call(
            "GET",
            "/api/v3/requests/history",
            &[("authorization", molly.as_str())],
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let items = body["items"].as_array().expect("history items");
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["status"], "imported");
    assert_eq!(items[0]["task_id"], Value::String(task_id.clone()));
    app.stop().await;
}

// ---------------------------------------------------------------------------
// Journey 2: follow toggle → approval queue → armed follow. The collections
// legs and the requests mutations share one approval store through the
// wiring bridges.
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn acquire_follow_approval_arms_auto_download() {
    let app = TestApp::start("follow").await;
    let admin = app.login_admin().await;
    let molly = app.login_user("molly").await;
    let (molly, molly_id) = (molly.authorization, molly.user_id);

    // Molly follows, then asks for auto-download (pending for a plain user).
    let (status, body) = app
        .call(
            "PUT",
            &format!("/api/v3/artists/{ARTIST_MBID}/follow"),
            &[("authorization", molly.as_str())],
            Some(json!({"followed": true, "artist_name": "Massive Attack"})),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, body) = app
        .call(
            "PUT",
            &format!("/api/v3/artists/{ARTIST_MBID}/auto-download"),
            &[("authorization", molly.as_str())],
            Some(json!({"enabled": true})),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["auto_download_state"], "pending");

    // The collections read leg shows the ask from the shared store.
    let (status, body) = app
        .call(
            "GET",
            "/api/v3/requests/auto-download-approvals",
            &[("authorization", admin.as_str())],
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["count"], 1);
    assert_eq!(body["items"][0]["user_id"], molly_id);

    // The admin approves through the requests mutation; the follow arms.
    let (status, body) = app
        .call(
            "POST",
            &format!("/api/v3/requests/auto-download-approvals/{molly_id}/{ARTIST_MBID}/approve"),
            &[("authorization", admin.as_str())],
            Some(json!({})),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["success"], true);
    let (status, body) = app
        .call(
            "GET",
            &format!("/api/v3/artists/{ARTIST_MBID}/follow-status"),
            &[("authorization", molly.as_str())],
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["auto_download_state"], "approved");

    // Revoking flips the intent off and the verdict shows.
    let (status, body) = app
        .call(
            "POST",
            &format!("/api/v3/requests/auto-download-approvals/{molly_id}/{ARTIST_MBID}/revoke"),
            &[("authorization", admin.as_str())],
            Some(json!({})),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (_, body) = app
        .call(
            "GET",
            &format!("/api/v3/artists/{ARTIST_MBID}/follow-status"),
            &[("authorization", molly.as_str())],
            None,
        )
        .await;
    assert_eq!(
        (
            body["auto_download"].as_bool(),
            body["auto_download_state"].as_str()
        ),
        (Some(false), Some("revoked"))
    );
    app.stop().await;
}

// ---------------------------------------------------------------------------
// Journey 3: no download source → the request is accepted and says it is
// waiting → the admin saves the slskd connection → the request starts.
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn acquire_held_request_starts_when_a_source_is_saved() {
    let app = TestApp::start("held").await;
    let admin = app.login_admin().await;
    let molly = app.login_user("molly").await.authorization;

    let (status, body) = app
        .call(
            "POST",
            "/api/v3/requests/albums",
            &[("authorization", molly.as_str())],
            Some(json!({
                "musicbrainz_id": RG_MBID,
                "artist": "Massive Attack",
                "album": "Blue Lines",
            })),
        )
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    let (status, body) = app
        .call(
            "POST",
            &format!("/api/v3/requests/approvals/{RG_MBID}/approve"),
            &[("authorization", admin.as_str())],
            Some(json!({})),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");

    // Approved, with nowhere to download from: Molly sees why it waits.
    let (status, body) = app
        .call(
            "GET",
            "/api/v3/requests/active",
            &[("authorization", molly.as_str())],
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["count"], 1, "{body}");
    assert_eq!(body["items"][0]["waiting_reason"], "no_download_source");
    assert!(app.slskd.search_texts().is_empty());

    // The admin turns slskd on in Settings.
    let (status, body) = app
        .call(
            "PUT",
            "/api/v3/settings/download-client/config",
            &[("authorization", admin.as_str())],
            Some(json!({
                "enabled": true,
                "url": app.slskd.base_url(),
                "api_key": "slskd-key",
            })),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");

    // The save itself starts the request: slskd is searched for the album
    // long before the worker's next pass on its own would come round.
    let searched = async {
        while app.slskd.search_texts().is_empty() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    };
    tokio::time::timeout(Duration::from_secs(5), searched)
        .await
        .expect("saving a source starts the held request");
    let (status, body) = app
        .call(
            "GET",
            "/api/v3/requests/active",
            &[("authorization", molly.as_str())],
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["count"], 1, "{body}");
    assert_eq!(body["items"][0]["waiting_reason"], Value::Null);
    app.stop().await;
}
