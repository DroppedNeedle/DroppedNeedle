//! Queue transfer over the signed-in playback routes and the rig's clock.
use super::{app, call, rig};
use crate::common::TestApp;
use crate::common::auth::{test_principal, with_test_principal};
use axum::{Router, http::StatusCode};
use droppedneedle_auth::{Role, session::store::SessionKind};
use serde_json::{Value, json};

const SOURCE: &str = "00000000-0000-4000-8000-000000000001";
const DEST: &str = "00000000-0000-4000-8000-000000000002";
const TRANSFER: &str = "00000000-0000-4000-8000-000000000003";
const START: &str = "/api/v3/playback/handoff";
const SYNC: &str = "/api/v3/playback/handoff/sync";

fn signed(router: &Router, user: &str, session: &str) -> Router {
    with_test_principal(
        router.clone(),
        test_principal(user, Role::User, session, SessionKind::Standard),
    )
}

async fn post(router: &Router, path: &str, body: Value, expected: StatusCode) -> Value {
    let (status, value, _) = call(router.clone(), "POST", path, Some(body)).await;
    assert_eq!(status, expected, "{path} {value}");
    value
}

fn heartbeat(id: &str, receipt: Value) -> Value {
    json!({"player_id": id, "name": id, "can_send": id == SOURCE, "receipt": receipt})
}

fn request() -> Value {
    json!({"transfer_id": TRANSFER, "player_id": DEST, "source_id": SOURCE, "destination_id": DEST})
}

async fn sync(router: &Router, id: &str, receipt: Value) -> Value {
    post(router, SYNC, heartbeat(id, receipt), StatusCode::OK).await["command"].clone()
}

fn receipt(kind: &str) -> Value {
    json!({"kind":kind, "transfer_id":TRANSFER})
}

#[tokio::test]
async fn destination_accepts_before_source_can_release() {
    let rig = rig();
    let router = app(&rig, None);
    let phone = signed(&router, "listener", "phone");
    let browser = signed(&router, "listener", "browser");

    sync(&phone, SOURCE, Value::Null).await;
    sync(&browser, DEST, Value::Null).await;
    post(&browser, START, request(), StatusCode::NO_CONTENT).await;
    post(&browser, START, request(), StatusCode::NO_CONTENT).await;
    let mut competing = request();
    competing["transfer_id"] = json!("00000000-0000-4000-8000-000000000004");
    post(&browser, START, competing.clone(), StatusCode::CONFLICT).await;

    assert_eq!(sync(&browser, DEST, receipt("accepted")).await, Value::Null);
    assert_eq!(sync(&phone, SOURCE, Value::Null).await, receipt("offer"));

    let track = json!({"id":"track-1","title":"Roads","artist":"Portishead","album":"Dummy","duration":240.0,"album_id":null,"artist_id":null});
    let queue = json!({"tracks":[track, track], "index":1,"position_ms":12345,"playing":false});
    let mut offered = receipt("offered");
    offered["queue"] = queue.clone();
    assert_eq!(sync(&phone, SOURCE, offered.clone()).await, Value::Null);
    assert_eq!(sync(&phone, SOURCE, receipt("accepted")).await, Value::Null);
    let received = sync(&browser, DEST, Value::Null).await;
    assert_eq!(
        received,
        json!({"kind":"receive","transfer_id":TRANSFER,"queue":queue})
    );
    sync(&phone, SOURCE, offered).await;
    assert_eq!(sync(&browser, DEST, Value::Null).await, received);
    let mut stale = receipt("accepted");
    stale["transfer_id"] = json!("00000000-0000-4000-8000-000000000004");
    assert_eq!(sync(&browser, DEST, stale).await, received);
    sync(&browser, DEST, receipt("accepted")).await;
    assert_eq!(
        sync(&phone, SOURCE, Value::Null).await,
        json!({"kind":"release","transfer_id":TRANSFER,"destination_id":DEST})
    );
    sync(&phone, SOURCE, receipt("released")).await;
    assert_eq!(sync(&phone, SOURCE, Value::Null).await, Value::Null);

    // The same players can attempt another transfer. If the destination cannot
    // play this time, the source never receives a release.
    post(&browser, START, competing.clone(), StatusCode::NO_CONTENT).await;
    let next_id = &competing["transfer_id"];
    sync(
        &phone,
        SOURCE,
        json!({"kind":"offered","transfer_id":next_id,"queue":queue}),
    )
    .await;
    assert_eq!(sync(&browser, DEST, Value::Null).await["kind"], "receive");
    sync(
        &browser,
        DEST,
        json!({"kind":"declined","transfer_id":next_id}),
    )
    .await;
    assert_eq!(sync(&phone, SOURCE, Value::Null).await["kind"], "failed");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn player_discovery_and_control_require_the_owning_session() {
    let server = TestApp::start("handoff-ownership").await;
    let listener = server.login_user("listener").await;
    let neighbor = server.login_admin().await;
    let (status, session) = server
        .call(
            "POST",
            "/api/v3/auth/login",
            &[],
            Some(json!({
                "username":"listener", "password":"listener-password-1234", "transport":"bearer"
            })),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let browser = format!("Bearer {}", session["token"].as_str().unwrap());

    async fn send(
        server: &TestApp,
        token: &str,
        path: &str,
        body: Value,
        expected: StatusCode,
    ) -> Value {
        let (status, value) = server
            .call("POST", path, &[("authorization", token)], Some(body))
            .await;
        assert_eq!(status, expected, "{value}");
        value
    }

    let phone = listener.authorization.as_str();
    send(
        &server,
        "",
        SYNC,
        heartbeat(SOURCE, Value::Null),
        StatusCode::UNAUTHORIZED,
    )
    .await;
    send(
        &server,
        phone,
        SYNC,
        heartbeat(SOURCE, Value::Null),
        StatusCode::OK,
    )
    .await;
    send(
        &server,
        &browser,
        SYNC,
        heartbeat(DEST, Value::Null),
        StatusCode::OK,
    )
    .await;
    send(
        &server,
        &browser,
        SYNC,
        heartbeat(SOURCE, Value::Null),
        StatusCode::NOT_FOUND,
    )
    .await;
    send(&server, phone, START, request(), StatusCode::NOT_FOUND).await;
    let peers = send(
        &server,
        &neighbor,
        SYNC,
        heartbeat(DEST, Value::Null),
        StatusCode::OK,
    )
    .await;
    assert_eq!(peers["players"].as_array().unwrap().len(), 1);
    send(&server, &neighbor, START, request(), StatusCode::NOT_FOUND).await;
    send(&server, &browser, START, request(), StatusCode::NO_CONTENT).await;
    server.stop().await;
}

#[tokio::test]
async fn disconnected_and_expired_players_cannot_take_over() {
    let rig = rig();
    let router = app(&rig, Some("listener"));
    sync(&router, SOURCE, Value::Null).await;
    sync(&router, DEST, Value::Null).await;
    post(&router, START, request(), StatusCode::NO_CONTENT).await;
    rig.clock.advance(30_000);
    assert_eq!(sync(&router, SOURCE, Value::Null).await, receipt("failed"));
    rig.clock.advance(15_000);
    let response = post(
        &router,
        SYNC,
        heartbeat(SOURCE, Value::Null),
        StatusCode::OK,
    )
    .await;
    assert_eq!(response["players"].as_array().unwrap().len(), 1);
    sync(&router, DEST, Value::Null).await;
    let mut new_request = request();
    new_request["transfer_id"] = json!("00000000-0000-4000-8000-000000000005");
    post(&router, START, new_request.clone(), StatusCode::NO_CONTENT).await;
    let mut disconnect = heartbeat(DEST, Value::Null);
    disconnect["disconnect"] = json!(true);
    post(&router, SYNC, disconnect, StatusCode::OK).await;
    let failed = sync(&router, SOURCE, Value::Null).await;
    assert_eq!(
        failed,
        json!({"kind":"failed","transfer_id":new_request["transfer_id"]})
    );
}

#[tokio::test]
async fn invalid_local_queue_never_reaches_destination() {
    let rig = rig();
    let router = app(&rig, Some("listener"));
    sync(&router, SOURCE, Value::Null).await;
    sync(&router, DEST, Value::Null).await;
    post(&router, START, request(), StatusCode::NO_CONTENT).await;
    let mut offered = receipt("offered");
    offered["queue"] = json!({"tracks":[{"id":"https://example.com/stream","title":"Title","artist":"","album":""}], "index":0,"position_ms":1000,"playing":true});
    assert_eq!(sync(&router, SOURCE, offered).await, receipt("failed"));
    assert_eq!(sync(&router, DEST, Value::Null).await, receipt("failed"));
}
