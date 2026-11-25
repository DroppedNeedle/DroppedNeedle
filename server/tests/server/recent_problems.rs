//! A server fault's ref finds its entry, cause and all, in the admin's
//! recent problems. The ring is installed on this thread only
//! (`set_default` is thread-local), so the test runs on a current-thread
//! runtime and every request it makes is served on this thread.

use axum::http::StatusCode;
use tracing_subscriber::layer::SubscriberExt as _;

use crate::common::{TestApp, closed_outbound};

#[tokio::test(flavor = "current_thread")]
async fn a_fault_is_found_by_the_ref_its_answer_gave() {
    let app = TestApp::start_hooked_with("recent-problems", closed_outbound()).await;
    let _ring = tracing::subscriber::set_default(
        tracing_subscriber::registry().with(app.state().admin.recent.layer()),
    );
    let admin = app.login_admin().await;
    let headers = [("authorization", admin.as_str())];

    let (status, body) = app.call("GET", "/__test__/typed-error", &[], None).await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    let error_id = body["error"]["details"]["error_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let short = &error_id[..8];

    let (status, found) = app
        .call(
            "GET",
            &format!("/api/v3/admin/problems/recent?ref={short}"),
            &headers,
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{found}");
    let problems = found["problems"].as_array().unwrap();
    assert_eq!(problems.len(), 1, "{found}");
    let problem = &problems[0];
    assert_eq!(problem["level"], "error");
    assert!(
        problem["refs"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r == &error_id.as_str()),
        "{found}"
    );
    assert!(
        problem["fields"]["cause"]
            .as_str()
            .unwrap()
            .contains("boom reading"),
        "{found}"
    );

    let (status, _) = app
        .call(
            "GET",
            &format!("/api/v3/admin/problems/recent?ref={}", "x".repeat(129)),
            &headers,
            None,
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    app.stop().await;
}
