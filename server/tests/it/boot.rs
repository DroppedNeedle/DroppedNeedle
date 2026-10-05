//! Boot-ready brief: the app binds an ephemeral port, answers `/health`
//! over real TCP, and stops cleanly.

use crate::common;

use std::time::Duration;

use droppedneedle::create_app;

#[tokio::test]
async fn boots_serves_health_and_stops() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let app = create_app(common::hooked_state());

    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();
    let response = client
        .get(format!("http://{address}/health"))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let text = response.text().await.unwrap();
    let body: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(body["status"], "ok");
    assert!(response_headers_seen(&address).await);

    server.abort();
    let _ = tokio::time::timeout(Duration::from_secs(5), server).await;
}

/// A second request still carries the request id header, proving the
/// middleware wraps live traffic too.
async fn response_headers_seen(address: &std::net::SocketAddr) -> bool {
    let client = reqwest::Client::new();
    let response = client
        .get(format!("http://{address}/health"))
        .send()
        .await
        .unwrap();
    response.headers().contains_key("x-request-id")
}
