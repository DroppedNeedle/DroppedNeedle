//! `/health` keeps the exact v2 fields, answers JSON, and resolves request
//! ids (minting one, or echoing the caller's).

use crate::common::TestApp;

use axum::http::StatusCode;

#[tokio::test]
async fn health_keeps_v2_shape_and_resolves_request_ids() {
    let app = TestApp::start("health").await;
    let (status, headers, body) = app.raw("GET", "/health", &[], None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers["content-type"], "application/json");
    let minted = headers["x-request-id"].to_str().unwrap();
    assert!(uuid::Uuid::parse_str(minted).is_ok(), "{minted}");
    let mut json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    // The one field v3 adds: the latest database checkpoint pass, once one ran.
    let checkpoint = json.as_object_mut().unwrap().remove("checkpoint");
    assert!(checkpoint.is_none_or(|pass| pass.is_object()));
    assert_eq!(
        json,
        serde_json::json!({"status": "ok", "message": "DroppedNeedle backend running"})
    );

    let (_, echoed, _) = app
        .raw("GET", "/health", &[("x-request-id", "caller-1")], None)
        .await;
    assert_eq!(echoed["x-request-id"], "caller-1");
}
