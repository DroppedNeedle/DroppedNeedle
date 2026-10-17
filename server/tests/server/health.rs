//! `/health` keeps the exact v2 fields, answers JSON, and resolves request
//! ids (minting one, or echoing the caller's). It is public, so nothing
//! internal rides along: the checkpoint pass is in the admin queue stats.

use crate::common::TestApp;

use axum::http::StatusCode;

#[tokio::test]
async fn health_keeps_v2_shape_and_resolves_request_ids() {
    let app = TestApp::start("health").await;
    let admin = app.login_admin().await;
    let headers = [("authorization", admin.as_str())];
    // The checkpoint loop beats its heartbeat after each pass.
    app.until("the first checkpoint pass", || async {
        let (_, stats) = app
            .call("GET", "/api/v3/admin/queue-stats", &headers, None)
            .await;
        stats["jobs"]
            .as_array()?
            .iter()
            .any(|job| job["name"] == "wal-checkpoint" && job["last_heartbeat_at"].is_number())
            .then_some(())
    })
    .await;

    let (status, headers_out, body) = app.raw("GET", "/health", &[], None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers_out["content-type"], "application/json");
    let minted = headers_out["x-request-id"].to_str().unwrap();
    assert!(uuid::Uuid::parse_str(minted).is_ok(), "{minted}");
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(
        json,
        serde_json::json!({"status": "ok", "message": "DroppedNeedle backend running"})
    );

    let (status, stats) = app
        .call("GET", "/api/v3/admin/queue-stats", &headers, None)
        .await;
    assert_eq!(status, StatusCode::OK);
    assert!(stats["checkpoint"]["mode"].is_string(), "{stats}");
    assert!(stats["checkpoint"].get("error").is_none(), "{stats}");

    let (_, echoed, _) = app
        .raw("GET", "/health", &[("x-request-id", "caller-1")], None)
        .await;
    assert_eq!(echoed["x-request-id"], "caller-1");
    app.stop().await;
}
