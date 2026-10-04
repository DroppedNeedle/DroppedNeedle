//! Stage-7 request-intake briefs: album/track/batch/edition-acquire intake,
//! the approval gate, quota/role gating, and the wanted + approval views.
//!
//! These briefs mount the slice router directly with the slice-local
//! principal header. App wiring lands separately (the integrator nests the
//! bare routes behind the session gate with principal translation). Each
//! brief pins one behavior; the dispatch fake stands in for the downloads
//! slice, which owns durable fetch.

#[path = "../src/acquire/requests/mod.rs"]
// Partial-view harness: this file exercises a slice of the module, so
// items it never touches read as dead or unused here (they are live in
// the wired lib build).
#[allow(dead_code, unused_imports)]
mod requests;

use axum::{
    Router,
    body::Body,
    http::{HeaderMap, Method, Request, StatusCode},
};
use requests::{
    RequestsState,
    dispatch::{DispatchOrigin, DispatchOutcome, TaskProgress},
    ledger::{ApprovalBatch, FollowApproval, MixApproval, WantedRetrying, WantedWatch},
    quota::QuotaPolicy,
};
use serde_json::{Value, json};
use tower::ServiceExt as _;

const ADA: &str = "u-ada:user:Ada";
const BOB: &str = "u-bob:user:Bob";
const TRUSTED: &str = "u-tris:trusted:Tris";
const ADMIN: &str = "u-root:admin:Root";

const MBID_A: &str = "11111111-1111-4111-8111-111111111111";
const MBID_B: &str = "22222222-2222-4222-8222-222222222222";
const MBID_C: &str = "33333333-3333-4333-8333-333333333333";
const MBID_D: &str = "44444444-4444-4444-8444-444444444444";
const MBID_E: &str = "55555555-5555-4555-8555-555555555555";
const ARTIST: &str = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";

fn app(state: &RequestsState) -> Router {
    requests::requests_router(state.clone())
        .fallback(requests::error::fallback_404)
        .method_not_allowed_fallback(requests::error::fallback_405)
}

fn request(
    method: Method,
    uri: &str,
    identity: Option<&str>,
    body: Option<Value>,
) -> Request<Body> {
    let mut builder = Request::builder().method(method).uri(uri);
    if let Some(identity) = identity {
        builder = builder.header("x-slice-principal", identity);
    }
    let body = match body {
        Some(json) => {
            builder = builder.header("content-type", "application/json");
            Body::from(json.to_string())
        }
        None => Body::empty(),
    };
    builder.body(body).unwrap()
}

async fn send(app: Router, req: Request<Body>) -> (StatusCode, HeaderMap, Value) {
    let response = app.oneshot(req).await.unwrap();
    let status = response.status();
    let headers = response.headers().clone();
    let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let json: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, headers, json)
}

fn album_body(mbid: &str) -> Value {
    json!({
        "musicbrainz_id": mbid,
        "artist": "Test Artist",
        "album": "Test Album",
        "year": 2024,
    })
}

fn error_code(body: &Value) -> &str {
    body.pointer("/error/code")
        .and_then(Value::as_str)
        .unwrap_or("")
}

// A plain user's album ask waits for approval and dispatches nothing.
#[tokio::test]
async fn user_album_intake_waits_for_approval() {
    let (state, dispatch) = RequestsState::for_tests();
    let (status, _, body) = send(
        app(&state),
        request(
            Method::POST,
            "/requests/albums",
            Some(ADA),
            Some(album_body(MBID_A)),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    assert_eq!(body["success"], true);
    assert_eq!(body["status"], "awaiting_approval");
    assert_eq!(
        body["message"],
        "Request submitted, awaiting admin approval"
    );
    assert!(dispatch.take_calls().is_empty());
}

// A trusted ask auto-approves and dispatches under the owner's id at once.
#[tokio::test]
async fn trusted_album_intake_dispatches_at_once() {
    let (state, dispatch) = RequestsState::for_tests();
    let (status, _, body) = send(
        app(&state),
        request(
            Method::POST,
            "/requests/albums",
            Some(TRUSTED),
            Some(album_body(MBID_A)),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    assert_eq!(body["status"], "pending");
    assert_eq!(body["message"], "Request accepted");
    let task_id = body["task_id"].as_str().unwrap().to_owned();
    let calls = dispatch.take_calls();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].user_id, "u-tris");
    assert_eq!(calls[0].origin, DispatchOrigin::User);
    assert_eq!(calls[0].key, MBID_A.to_lowercase());
    assert!(!task_id.is_empty());
}

// Full journey: user asks, admin approves, fetch lands, history reads imported.
#[tokio::test]
async fn lifecycle_intake_approval_dispatch_landed() {
    let (state, dispatch) = RequestsState::for_tests();
    let (status, _, _) = send(
        app(&state),
        request(
            Method::POST,
            "/requests/albums",
            Some(ADA),
            Some(album_body(MBID_A)),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);

    let (_, _, approvals) = send(
        app(&state),
        request(Method::GET, "/requests/approvals", Some(ADMIN), None),
    )
    .await;
    assert_eq!(approvals["count"], 1);
    assert_eq!(
        approvals["items"][0]["musicbrainz_id"],
        MBID_A.to_lowercase()
    );

    let (status, _, approved) = send(
        app(&state),
        request(
            Method::POST,
            &format!("/requests/approvals/{MBID_A}/approve"),
            Some(ADMIN),
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(approved["success"], true);
    let calls = dispatch.take_calls();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].origin, DispatchOrigin::Approval);
    // Approval keeps the immutable primary owner, not the acting admin.
    assert_eq!(calls[0].user_id, "u-ada");

    let (_, _, active) = send(
        app(&state),
        request(Method::GET, "/requests/active", Some(ADA), None),
    )
    .await;
    assert_eq!(active["count"], 1);
    let task_id = active["items"][0]["task_id"].as_str().unwrap().to_owned();

    dispatch.land(&task_id);
    let (status, _, synced) = send(
        app(&state),
        request(Method::POST, "/requests/sync", Some(ADMIN), None),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(synced["reconciled"], 1);

    let (_, _, active) = send(
        app(&state),
        request(Method::GET, "/requests/active", Some(ADA), None),
    )
    .await;
    assert_eq!(active["count"], 0);
    let (_, _, history) = send(
        app(&state),
        request(Method::GET, "/requests/history", Some(ADA), None),
    )
    .await;
    assert_eq!(history["total"], 1);
    assert_eq!(history["items"][0]["status"], "imported");
}

// A duplicate ask attaches as co-requester and mirrors the live row.
#[tokio::test]
async fn duplicate_album_attaches_co_requester() {
    let (state, dispatch) = RequestsState::for_tests();
    send(
        app(&state),
        request(
            Method::POST,
            "/requests/albums",
            Some(TRUSTED),
            Some(album_body(MBID_A)),
        ),
    )
    .await;
    let (status, _, body) = send(
        app(&state),
        request(
            Method::POST,
            "/requests/albums",
            Some(BOB),
            Some(album_body(MBID_A)),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    assert_eq!(body["success"], true);
    assert_eq!(body["message"], "Request already in progress");
    assert_eq!(dispatch.take_calls().len(), 1);

    let (_, _, active) = send(
        app(&state),
        request(Method::GET, "/requests/active", Some(BOB), None),
    )
    .await;
    assert_eq!(active["count"], 1);
    assert_eq!(active["items"][0]["requester_count"], 1);
}

// Batches dedupe raw and canonical ids, attach to live rows, and dispatch
// each created row on its own.
#[tokio::test]
async fn batch_dedupes_live_and_dispatches_new() {
    let (state, dispatch) = RequestsState::for_tests();
    send(
        app(&state),
        request(
            Method::POST,
            "/requests/albums",
            Some(TRUSTED),
            Some(album_body(MBID_A)),
        ),
    )
    .await;
    dispatch.take_calls();

    let body = json!({
        "items": [
            {"musicbrainz_id": MBID_A, "artist_name": "A", "album_title": "A"},
            {"musicbrainz_id": MBID_A, "artist_name": "A", "album_title": "A"},
            {"musicbrainz_id": MBID_B, "artist_name": "B", "album_title": "B"},
            {"musicbrainz_id": "not-a-mbid", "artist_name": "X", "album_title": "X"},
            {"musicbrainz_id": MBID_C, "artist_name": "Unknown", "album_title": "  "},
        ],
    });
    let (status, _, out) = send(
        app(&state),
        request(Method::POST, "/requests/batches", Some(TRUSTED), Some(body)),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    assert_eq!(out["success"], true);
    assert_eq!(out["requested"], 1);
    // Live row + raw dupe + invalid id + unresolvable names = 4 skipped.
    assert_eq!(out["skipped"], 4);
    assert_eq!(out["overflow"], 0);
    assert_eq!(out["status"], "pending");
    assert_eq!(dispatch.take_calls().len(), 1);

    // A batch of only live rows reports already-requested without dispatch.
    let (status, _, out) = send(
        app(&state),
        request(
            Method::POST,
            "/requests/batches",
            Some(TRUSTED),
            Some(json!({"items": [
                {"musicbrainz_id": MBID_A, "artist_name": "A", "album_title": "A"},
            ]})),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    assert_eq!(out["status"], "already_requested");
    assert!(dispatch.take_calls().is_empty());
}

// R10: owners cancel their own rows, co-requesters detach, strangers fail.
#[tokio::test]
async fn batch_cancel_requester_matrix() {
    let (state, _) = RequestsState::for_tests();
    for (id, mbid) in [(TRUSTED, MBID_A), (BOB, MBID_B)] {
        send(
            app(&state),
            request(
                Method::POST,
                "/requests/albums",
                Some(id),
                Some(album_body(mbid)),
            ),
        )
        .await;
    }
    // Ada attaches to Tris's row, then cancels: she detaches, the row lives.
    send(
        app(&state),
        request(
            Method::POST,
            "/requests/albums",
            Some(ADA),
            Some(album_body(MBID_A)),
        ),
    )
    .await;
    let (status, _, out) = send(
        app(&state),
        request(
            Method::POST,
            "/requests/batches/cancel",
            Some(ADA),
            Some(json!({"musicbrainz_ids": [MBID_A, MBID_B, MBID_C]})),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(out["cancelled"], 1);
    assert_eq!(out["failed"], 2);
    assert_eq!(out["success"], true);

    let (_, _, active) = send(
        app(&state),
        request(Method::GET, "/requests/active", Some(TRUSTED), None),
    )
    .await;
    assert_eq!(active["count"], 1);
    assert_eq!(active["items"][0]["requester_count"], 0);
}

// R10: admins cancel anything live, including waiting rows (which also lose
// their dispatch capability).
#[tokio::test]
async fn batch_cancel_admin_cancels_live_rows() {
    let (state, _) = RequestsState::for_tests();
    send(
        app(&state),
        request(
            Method::POST,
            "/requests/albums",
            Some(ADA),
            Some(album_body(MBID_A)),
        ),
    )
    .await;
    let (status, _, out) = send(
        app(&state),
        request(
            Method::POST,
            "/requests/batches/cancel",
            Some(ADMIN),
            Some(json!({"musicbrainz_ids": [MBID_A, MBID_A, MBID_B]})),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(out["cancelled"], 1);
    assert_eq!(out["failed"], 1);

    let (_, _, history) = send(
        app(&state),
        request(
            Method::GET,
            "/requests/history?status=cancelled",
            Some(ADMIN),
            None,
        ),
    )
    .await;
    assert_eq!(history["total"], 1);
}

// Quota/role matrix: plain users spend a rolling budget, curators skip it,
// and the global storage cap binds everyone.
#[tokio::test]
async fn quota_role_matrix() {
    let (state, _) = RequestsState::for_tests();
    state.quota.set_policy(QuotaPolicy {
        request_count: 1,
        request_days: 7,
        storage_gb_per_user: 0,
        max_library_gb: 0,
    });
    let (status, _, _) = send(
        app(&state),
        request(
            Method::POST,
            "/requests/albums",
            Some(ADA),
            Some(album_body(MBID_A)),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);

    // Second ask inside the window is 429 with the window in details.
    let (status, _, body) = send(
        app(&state),
        request(
            Method::POST,
            "/requests/albums",
            Some(ADA),
            Some(album_body(MBID_B)),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(error_code(&body), "QUOTA_EXCEEDED");
    assert_eq!(body["error"]["details"]["limit"], 1);

    // Trusted and admin asks skip the count gate entirely.
    for (id, mbid) in [(TRUSTED, MBID_C), (ADMIN, MBID_D)] {
        let (status, _, _) = send(
            app(&state),
            request(
                Method::POST,
                "/requests/albums",
                Some(id),
                Some(album_body(mbid)),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::ACCEPTED);
    }

    // The global storage cap binds even curators, at exactly the cap.
    state.quota.set_policy(QuotaPolicy {
        request_count: 0,
        request_days: 7,
        storage_gb_per_user: 0,
        max_library_gb: 1,
    });
    state.quota.seed_usage("u-tris", 0, 1024 * 1024 * 1024);
    let (status, _, body) = send(
        app(&state),
        request(
            Method::POST,
            "/requests/albums",
            Some(TRUSTED),
            Some(album_body(MBID_E)),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(error_code(&body), "STORAGE_FULL");
}

// Exact tracks share the approval gate and land through the same sync.
#[tokio::test]
async fn track_lifecycle_queues_and_lands() {
    let (state, dispatch) = RequestsState::for_tests();
    let track = json!({
        "recording_mbid": MBID_A,
        "artist_name": "Test Artist",
        "track_title": "Test Track",
        "album_title": "Test Album",
    });
    // Plain users wait here too.
    let (status, _, body) = send(
        app(&state),
        request(
            Method::POST,
            "/requests/tracks",
            Some(ADA),
            Some(track.clone()),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    assert_eq!(body["status"], "awaiting_approval");

    // Duplicate asks mirror the winner.
    let (_, _, dupe) = send(
        app(&state),
        request(
            Method::POST,
            "/requests/tracks",
            Some(BOB),
            Some(track.clone()),
        ),
    )
    .await;
    assert_eq!(dupe["status"], "awaiting_approval");

    let (status, _, approved) = send(
        app(&state),
        request(
            Method::POST,
            &format!("/requests/approvals/{MBID_A}/approve?kind=track"),
            Some(ADMIN),
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(approved["success"], true);
    assert_eq!(dispatch.take_calls().len(), 1);

    let (_, _, active) = send(
        app(&state),
        request(Method::GET, "/requests/active", Some(ADA), None),
    )
    .await;
    let task_id = active["items"][0]["task_id"].as_str().unwrap().to_owned();
    dispatch.land(&task_id);
    send(
        app(&state),
        request(Method::POST, "/requests/sync", Some(ADMIN), None),
    )
    .await;
    let (_, _, history) = send(
        app(&state),
        request(Method::GET, "/requests/history?kind=track", Some(ADA), None),
    )
    .await;
    assert_eq!(history["total"], 1);
    assert_eq!(history["items"][0]["status"], "imported");
    assert_eq!(history["items"][0]["request_kind"], "track");
}

// Approval semantics: only waiting rows decide, second claims lose, and a
// rejected row's retry rejoins the queue without provenance.
#[tokio::test]
async fn approval_claims_and_rejection_retry() {
    let (state, _) = RequestsState::for_tests();
    send(
        app(&state),
        request(
            Method::POST,
            "/requests/albums",
            Some(ADA),
            Some(album_body(MBID_A)),
        ),
    )
    .await;
    // Approving a missing row reports not-found, not an error.
    let (_, _, out) = send(
        app(&state),
        request(
            Method::POST,
            &format!("/requests/approvals/{MBID_B}/approve"),
            Some(ADMIN),
            None,
        ),
    )
    .await;
    assert_eq!(out["success"], false);
    assert_eq!(out["message"], "Request not found");

    let (_, _, rejected) = send(
        app(&state),
        request(
            Method::POST,
            &format!("/requests/approvals/{MBID_A}/reject"),
            Some(ADMIN),
            None,
        ),
    )
    .await;
    assert_eq!(rejected["success"], true);

    // A second decision on the same row loses with the current status.
    let (_, _, again) = send(
        app(&state),
        request(
            Method::POST,
            &format!("/requests/approvals/{MBID_A}/approve"),
            Some(ADMIN),
            None,
        ),
    )
    .await;
    assert_eq!(again["success"], false);
    assert!(again["message"].as_str().unwrap().contains("rejected"));

    // Rejected rows are not retryable; the user re-asks through intake.
    let (_, _, retried) = send(
        app(&state),
        request(
            Method::POST,
            &format!("/requests/retry/{MBID_A}"),
            Some(ADA),
            None,
        ),
    )
    .await;
    assert_eq!(retried["success"], false);
    let (_, _, reasked) = send(
        app(&state),
        request(
            Method::POST,
            "/requests/albums",
            Some(ADA),
            Some(album_body(MBID_A)),
        ),
    )
    .await;
    assert_eq!(reasked["status"], "awaiting_approval");

    // A cancelled wait carries no approval provenance, so the owner's retry
    // rejoins the queue instead of dispatching.
    send(
        app(&state),
        request(
            Method::POST,
            "/requests/albums",
            Some(ADA),
            Some(album_body(MBID_C)),
        ),
    )
    .await;
    send(
        app(&state),
        request(
            Method::POST,
            "/requests/batches/cancel",
            Some(ADMIN),
            Some(json!({"musicbrainz_ids": [MBID_C]})),
        ),
    )
    .await;
    let (_, _, queued) = send(
        app(&state),
        request(
            Method::POST,
            &format!("/requests/retry/{MBID_C}"),
            Some(ADA),
            None,
        ),
    )
    .await;
    assert_eq!(queued["success"], true);
    assert_eq!(
        queued["message"],
        "Retry submitted, awaiting admin approval"
    );
}

// A failed fetch retries under the retry origin, which skips quota gates.
#[tokio::test]
async fn retry_failed_row_redispatches() {
    let (state, dispatch) = RequestsState::for_tests();
    send(
        app(&state),
        request(
            Method::POST,
            "/requests/albums",
            Some(TRUSTED),
            Some(album_body(MBID_A)),
        ),
    )
    .await;
    let (_, _, active) = send(
        app(&state),
        request(Method::GET, "/requests/active", Some(TRUSTED), None),
    )
    .await;
    let task_id = active["items"][0]["task_id"].as_str().unwrap().to_owned();
    dispatch.fail(&task_id);
    send(
        app(&state),
        request(Method::POST, "/requests/sync", Some(ADMIN), None),
    )
    .await;
    dispatch.take_calls();

    let (_, _, retried) = send(
        app(&state),
        request(
            Method::POST,
            &format!("/requests/retry/{MBID_A}"),
            Some(TRUSTED),
            None,
        ),
    )
    .await;
    assert_eq!(retried["success"], true);
    let calls = dispatch.take_calls();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].origin, DispatchOrigin::Retry);
    assert_eq!(calls[0].user_id, "u-tris");
}

// Already-in-library dispatches mark the row imported without a task.
#[tokio::test]
async fn already_in_library_completes_without_task() {
    let (state, dispatch) = RequestsState::for_tests();
    dispatch.push_outcome(DispatchOutcome::AlreadyInLibrary);
    let (status, _, body) = send(
        app(&state),
        request(
            Method::POST,
            "/requests/albums",
            Some(TRUSTED),
            Some(album_body(MBID_A)),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    assert_eq!(body["message"], "Album is already in the library");

    let (_, _, history) = send(
        app(&state),
        request(Method::GET, "/requests/history", Some(TRUSTED), None),
    )
    .await;
    assert_eq!(history["items"][0]["status"], "imported");
}

// Edition acquire (A:12) is curator-only, dedupes while live, and reports
// completion without a task when the edition is whole.
#[tokio::test]
async fn edition_acquire_curator_flow() {
    let (state, dispatch) = RequestsState::for_tests();
    let uri = format!("/albums/{MBID_A}/edition/acquire");
    let (status, _, body) = send(app(&state), request(Method::POST, &uri, Some(ADA), None)).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(error_code(&body), "FORBIDDEN");

    let (status, _, started) = send(
        app(&state),
        request(Method::POST, &uri, Some(TRUSTED), None),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(started["status"], "started");
    assert_eq!(dispatch.take_calls().len(), 1);

    let (_, _, dupe) = send(app(&state), request(Method::POST, &uri, Some(ADMIN), None)).await;
    assert_eq!(dupe["status"], "already_in_progress");
    assert_eq!(dispatch.take_calls().len(), 0);

    dispatch.push_outcome(DispatchOutcome::AlreadyInLibrary);
    let other = format!("/albums/{MBID_B}/edition/acquire");
    let (_, _, complete) = send(
        app(&state),
        request(Method::POST, &other, Some(ADMIN), None),
    )
    .await;
    assert_eq!(complete["status"], "already_complete");
}

// Wanted views scope to the owner (admins see all) and the actions flip state.
#[tokio::test]
async fn wanted_views_and_actions() {
    let (state, _) = RequestsState::for_tests();
    state.wanted.seed_watch(WantedWatch {
        key: MBID_A.to_owned(),
        artist_name: "A".to_owned(),
        album_title: "A".to_owned(),
        kind: "missing".to_owned(),
        state: "watching".to_owned(),
        check_count: 3,
        next_check_at: Some(4_000),
        new_candidate_count: 2,
        created_at: 100,
        artist_mbid: None,
        year: Some(2021),
        cover_url: None,
        user_id: "u-ada".to_owned(),
        user_name: Some("Ada".to_owned()),
    });
    state.wanted.seed_watch(WantedWatch {
        key: MBID_B.to_owned(),
        artist_name: "B".to_owned(),
        album_title: "B".to_owned(),
        kind: "partial".to_owned(),
        state: "watching".to_owned(),
        check_count: 1,
        next_check_at: Some(5_000),
        new_candidate_count: 0,
        created_at: 200,
        artist_mbid: None,
        year: None,
        cover_url: None,
        user_id: "u-bob".to_owned(),
        user_name: Some("Bob".to_owned()),
    });
    state.wanted.seed_retrying(WantedRetrying {
        key: MBID_C.to_owned(),
        artist_name: "C".to_owned(),
        album_title: "C".to_owned(),
        retry_count: 2,
        max_attempts: 5,
        next_retry_at: Some(6_000),
        artist_mbid: None,
        year: Some(2022),
        cover_url: None,
        user_id: "u-ada".to_owned(),
        user_name: Some("Ada".to_owned()),
    });

    let (_, _, ada) = send(
        app(&state),
        request(Method::GET, "/requests/wanted", Some(ADA), None),
    )
    .await;
    assert_eq!(ada["count"], 1);
    assert_eq!(ada["retrying"].as_array().unwrap().len(), 1);

    let (_, _, admin) = send(
        app(&state),
        request(Method::GET, "/requests/wanted", Some(ADMIN), None),
    )
    .await;
    assert_eq!(admin["count"], 2);

    let (_, _, stopped) = send(
        app(&state),
        request(
            Method::POST,
            &format!("/requests/wanted/{MBID_A}/stop"),
            Some(ADA),
            None,
        ),
    )
    .await;
    assert_eq!(stopped["state"], "paused");
    let (status, _, _) = send(
        app(&state),
        request(
            Method::POST,
            &format!("/requests/wanted/{MBID_B}/stop"),
            Some(ADA),
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    let (_, _, seen) = send(
        app(&state),
        request(
            Method::POST,
            &format!("/requests/wanted/{MBID_A}/seen"),
            Some(ADA),
            None,
        ),
    )
    .await;
    assert_eq!(seen["success"], true);
    let (_, _, ada) = send(
        app(&state),
        request(Method::GET, "/requests/wanted", Some(ADA), None),
    )
    .await;
    assert_eq!(ada["items"][0]["new_candidate_count"], 0);

    let (_, _, resumed) = send(
        app(&state),
        request(
            Method::POST,
            &format!("/requests/wanted/{MBID_A}/resume"),
            Some(ADA),
            None,
        ),
    )
    .await;
    assert_eq!(resumed["state"], "watching");
}

// The approvals badge sums all three queues.
#[tokio::test]
async fn approvals_count_sums_three_queues() {
    let (state, _) = RequestsState::for_tests();
    send(
        app(&state),
        request(
            Method::POST,
            "/requests/albums",
            Some(ADA),
            Some(album_body(MBID_A)),
        ),
    )
    .await;
    state.follows.seed_pending(FollowApproval {
        user_id: "u-ada".to_owned(),
        user_name: "Ada".to_owned(),
        artist_mbid: ARTIST.to_owned(),
        artist_name: "Test Artist".to_owned(),
        state: "pending".to_owned(),
        requested_at: 100,
    });
    state.mixes.seed_pending(MixApproval {
        user_id: "u-bob".to_owned(),
        user_name: "Bob".to_owned(),
        state: "pending".to_owned(),
        requested_at: 100,
    });

    let (status, _, count) = send(
        app(&state),
        request(Method::GET, "/requests/approvals/count", Some(ADMIN), None),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(count["count"], 3);

    let (status, _, _) = send(
        app(&state),
        request(Method::GET, "/requests/approvals/count", Some(ADA), None),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

// Auto-download approvals: decide once, revoke only grants, batches by count.
#[tokio::test]
async fn auto_download_approval_flow() {
    let (state, _) = RequestsState::for_tests();
    state.follows.seed_pending(FollowApproval {
        user_id: "u-ada".to_owned(),
        user_name: "Ada".to_owned(),
        artist_mbid: ARTIST.to_owned(),
        artist_name: "Test Artist".to_owned(),
        state: "pending".to_owned(),
        requested_at: 100,
    });
    let (_, _, list) = send(
        app(&state),
        request(
            Method::GET,
            "/requests/auto-download-approvals",
            Some(ADMIN),
            None,
        ),
    )
    .await;
    assert_eq!(list["count"], 1);

    let approve = format!("/requests/auto-download-approvals/u-ada/{ARTIST}/approve");
    let (_, _, ok) = send(
        app(&state),
        request(Method::POST, &approve, Some(ADMIN), None),
    )
    .await;
    assert_eq!(ok["success"], true);
    let (_, _, again) = send(
        app(&state),
        request(Method::POST, &approve, Some(ADMIN), None),
    )
    .await;
    assert_eq!(again["success"], false);

    let revoke = format!("/requests/auto-download-approvals/u-ada/{ARTIST}/revoke");
    let (_, _, revoked) = send(
        app(&state),
        request(Method::POST, &revoke, Some(ADMIN), None),
    )
    .await;
    assert_eq!(revoked["success"], true);

    let reject = format!("/requests/auto-download-approvals/u-ada/{ARTIST}/reject");
    let (_, _, missing) = send(
        app(&state),
        request(Method::POST, &reject, Some(ADMIN), None),
    )
    .await;
    assert_eq!(missing["success"], false);
    assert_eq!(missing["message"], "No matching approval found");
}

// Personal-mix approvals plus the A:393 refresh route with its
// already-running guard.
#[tokio::test]
async fn personal_mix_approvals_and_refresh() {
    let (state, _) = RequestsState::for_tests();
    state.mixes.seed_pending(MixApproval {
        user_id: "u-ada".to_owned(),
        user_name: "Ada".to_owned(),
        state: "pending".to_owned(),
        requested_at: 100,
    });
    let (_, _, list) = send(
        app(&state),
        request(
            Method::GET,
            "/requests/personal-mix-approvals",
            Some(ADMIN),
            None,
        ),
    )
    .await;
    assert_eq!(list["count"], 1);
    let (_, _, ok) = send(
        app(&state),
        request(
            Method::POST,
            "/requests/personal-mix-approvals/u-ada/approve",
            Some(ADMIN),
            None,
        ),
    )
    .await;
    assert_eq!(ok["message"], "Weekly Mix auto-request approved");

    // Reject then re-ask: Bob's rejected row decides once, and a fresh ask
    // lands pending again.
    state.mixes.seed_pending(MixApproval {
        user_id: "u-bob".to_owned(),
        user_name: "Bob".to_owned(),
        state: "pending".to_owned(),
        requested_at: 200,
    });
    let (_, _, rejected) = send(
        app(&state),
        request(
            Method::POST,
            "/requests/personal-mix-approvals/u-bob/reject",
            Some(ADMIN),
            None,
        ),
    )
    .await;
    assert_eq!(rejected["success"], true);
    let (_, _, again) = send(
        app(&state),
        request(
            Method::POST,
            "/requests/personal-mix-approvals/u-bob/reject",
            Some(ADMIN),
            None,
        ),
    )
    .await;
    assert_eq!(again["success"], false);
    state.mixes.seed_pending(MixApproval {
        user_id: "u-bob".to_owned(),
        user_name: "Bob".to_owned(),
        state: "pending".to_owned(),
        requested_at: 300,
    });
    let (_, _, list) = send(
        app(&state),
        request(
            Method::GET,
            "/requests/personal-mix-approvals",
            Some(ADMIN),
            None,
        ),
    )
    .await;
    assert_eq!(list["count"], 1);

    // Approve then revoke: the grant clears, and a second revoke loses.
    let (_, _, ok) = send(
        app(&state),
        request(
            Method::POST,
            "/requests/personal-mix-approvals/u-bob/approve",
            Some(ADMIN),
            None,
        ),
    )
    .await;
    assert_eq!(ok["success"], true);
    let (_, _, revoked) = send(
        app(&state),
        request(
            Method::POST,
            "/requests/personal-mix-approvals/u-bob/revoke",
            Some(ADMIN),
            None,
        ),
    )
    .await;
    assert_eq!(revoked["success"], true);
    let (_, _, revoked_again) = send(
        app(&state),
        request(
            Method::POST,
            "/requests/personal-mix-approvals/u-bob/revoke",
            Some(ADMIN),
            None,
        ),
    )
    .await;
    assert_eq!(revoked_again["success"], false);

    // Unlinked users get 400, not a build.
    state.mixes.seed_unlinked("u-bob");
    let (status, _, body) = send(
        app(&state),
        request(
            Method::POST,
            "/requests/personal-mix/refresh",
            Some(BOB),
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(error_code(&body), "INVALID_INPUT");

    let (_, _, first) = send(
        app(&state),
        request(
            Method::POST,
            "/requests/personal-mix/refresh",
            Some(ADA),
            None,
        ),
    )
    .await;
    assert_eq!(first["status"], "started");
    let (_, _, second) = send(
        app(&state),
        request(
            Method::POST,
            "/requests/personal-mix/refresh",
            Some(ADA),
            None,
        ),
    )
    .await;
    assert_eq!(second["status"], "already_running");
    state.mixes.refresh_finish("u-ada").unwrap();
    let (_, _, third) = send(
        app(&state),
        request(
            Method::POST,
            "/requests/personal-mix/refresh",
            Some(ADA),
            None,
        ),
    )
    .await;
    assert_eq!(third["status"], "started");
}

// History pages, filters, and clears: owners dismiss, admins delete,
// strangers get 403.
#[tokio::test]
async fn history_paging_and_clear_matrix() {
    let (state, dispatch) = RequestsState::for_tests();
    for mbid in [MBID_A, MBID_B, MBID_C] {
        send(
            app(&state),
            request(
                Method::POST,
                "/requests/albums",
                Some(TRUSTED),
                Some(album_body(mbid)),
            ),
        )
        .await;
    }
    let (_, _, active) = send(
        app(&state),
        request(Method::GET, "/requests/active", Some(TRUSTED), None),
    )
    .await;
    for item in active["items"].as_array().unwrap() {
        dispatch.land(item["task_id"].as_str().unwrap());
    }
    send(
        app(&state),
        request(Method::POST, "/requests/sync", Some(ADMIN), None),
    )
    .await;

    let (_, _, page) = send(
        app(&state),
        request(
            Method::GET,
            "/requests/history?page=1&page_size=2",
            Some(TRUSTED),
            None,
        ),
    )
    .await;
    assert_eq!(page["total"], 3);
    assert_eq!(page["items"].as_array().unwrap().len(), 2);
    assert_eq!(page["total_pages"], 2);

    let (_, _, filtered) = send(
        app(&state),
        request(
            Method::GET,
            "/requests/history?status=failed",
            Some(TRUSTED),
            None,
        ),
    )
    .await;
    assert_eq!(filtered["total"], 0);

    // A stranger may not clear someone else's row.
    let (status, _, _) = send(
        app(&state),
        request(
            Method::DELETE,
            &format!("/requests/history/{MBID_A}"),
            Some(BOB),
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    // The owner dismisses: gone for them, still there for admins.
    let (_, _, cleared) = send(
        app(&state),
        request(
            Method::DELETE,
            &format!("/requests/history/{MBID_A}"),
            Some(TRUSTED),
            None,
        ),
    )
    .await;
    assert_eq!(cleared["success"], true);
    let (_, _, own) = send(
        app(&state),
        request(Method::GET, "/requests/history", Some(TRUSTED), None),
    )
    .await;
    assert_eq!(own["total"], 2);
    let (status, _, deleted) = send(
        app(&state),
        request(
            Method::DELETE,
            &format!("/requests/history/{MBID_B}"),
            Some(ADMIN),
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(deleted["success"], true);
    let (_, _, admin) = send(
        app(&state),
        request(Method::GET, "/requests/history", Some(ADMIN), None),
    )
    .await;
    assert_eq!(admin["total"], 2);
}

// Single cancel: co-requesters detach with the shared message, owners stop
// the fetch, and strangers on live rows get 403.
#[tokio::test]
async fn cancel_one_detach_and_owner_flows() {
    let (state, dispatch) = RequestsState::for_tests();
    send(
        app(&state),
        request(
            Method::POST,
            "/requests/albums",
            Some(TRUSTED),
            Some(album_body(MBID_A)),
        ),
    )
    .await;
    send(
        app(&state),
        request(
            Method::POST,
            "/requests/albums",
            Some(ADA),
            Some(album_body(MBID_A)),
        ),
    )
    .await;
    let uri = format!("/requests/active/{MBID_A}");

    let (status, _, _) = send(app(&state), request(Method::DELETE, &uri, Some(BOB), None)).await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    let (_, _, detached) = send(app(&state), request(Method::DELETE, &uri, Some(ADA), None)).await;
    assert_eq!(detached["success"], true);
    assert!(
        detached["message"]
            .as_str()
            .unwrap()
            .contains("continues for another listener")
    );
    assert!(dispatch.take_cancels().is_empty());

    let (_, _, cancelled) = send(
        app(&state),
        request(Method::DELETE, &uri, Some(TRUSTED), None),
    )
    .await;
    assert_eq!(cancelled["success"], true);
    assert_eq!(dispatch.take_cancels().len(), 1);
}

// Auth and envelope posture: 401 with bearer hint, typed 400s, 404s, 405s.
#[tokio::test]
async fn auth_and_envelope_posture() {
    let (state, _) = RequestsState::for_tests();
    let (status, headers, body) = send(
        app(&state),
        request(Method::GET, "/requests/active", None, None),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(error_code(&body), "UNAUTHORIZED");
    assert_eq!(headers.get("www-authenticate").unwrap(), "Bearer");

    let (status, _, body) = send(
        app(&state),
        request(
            Method::POST,
            "/requests/albums",
            Some(ADA),
            Some(json!({"musicbrainz_id": "nope", "artist": "A", "album": "B"})),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(error_code(&body), "INVALID_INPUT");

    // Blank and placeholder names are missing values, not real names.
    let (status, _, _) = send(
        app(&state),
        request(
            Method::POST,
            "/requests/albums",
            Some(ADA),
            Some(json!({"musicbrainz_id": MBID_D, "artist": "Unknown", "album": "  "})),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    let (status, _, body) = send(
        app(&state),
        request(Method::GET, "/requests/nope", Some(ADA), None),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(error_code(&body), "NOT_FOUND");

    let (status, _, body) = send(
        app(&state),
        request(Method::DELETE, "/requests/active", Some(ADA), None),
    )
    .await;
    assert_eq!(status, StatusCode::METHOD_NOT_ALLOWED);
    assert_eq!(error_code(&body), "METHOD_NOT_ALLOWED");

    // Oversized batches and bad kinds are typed 400s, not panics.
    let big: Vec<Value> = (0..501)
        .map(|_| json!({"musicbrainz_id": MBID_E}))
        .collect();
    let (status, _, body) = send(
        app(&state),
        request(
            Method::POST,
            "/requests/batches",
            Some(ADA),
            Some(json!({"items": big})),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(error_code(&body), "INVALID_INPUT");

    let (status, _, body) = send(
        app(&state),
        request(
            Method::GET,
            "/requests/history?kind=playlist",
            Some(ADA),
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(error_code(&body), "INVALID_INPUT");
}

// Bulk auto-download batches: approve drains the batch from the list and a
// second decision loses; reject clears a batch the same way.
#[tokio::test]
async fn auto_download_batch_approve_and_reject() {
    let (state, _) = RequestsState::for_tests();
    state.follows.seed_batch(ApprovalBatch {
        batch_id: "batch-1".to_owned(),
        user_id: "u-ada".to_owned(),
        user_name: "Ada".to_owned(),
        artists: vec![
            (ARTIST.to_owned(), "Test Artist".to_owned()),
            (MBID_A.to_owned(), "Other Artist".to_owned()),
        ],
        state: "pending".to_owned(),
        requested_at: 100,
    });
    let (_, _, list) = send(
        app(&state),
        request(
            Method::GET,
            "/requests/auto-download-approval-batches",
            Some(ADMIN),
            None,
        ),
    )
    .await;
    assert_eq!(list["count"], 1);
    assert_eq!(list["batches"][0]["batch_id"], "batch-1");
    assert_eq!(list["batches"][0]["artist_count"], 2);

    let (_, _, approved) = send(
        app(&state),
        request(
            Method::POST,
            "/requests/auto-download-approval-batches/batch-1/approve",
            Some(ADMIN),
            None,
        ),
    )
    .await;
    assert_eq!(approved["success"], true);
    let (_, _, drained) = send(
        app(&state),
        request(
            Method::GET,
            "/requests/auto-download-approval-batches",
            Some(ADMIN),
            None,
        ),
    )
    .await;
    assert_eq!(drained["count"], 0);
    let (_, _, again) = send(
        app(&state),
        request(
            Method::POST,
            "/requests/auto-download-approval-batches/batch-1/approve",
            Some(ADMIN),
            None,
        ),
    )
    .await;
    assert_eq!(again["success"], false);

    state.follows.seed_batch(ApprovalBatch {
        batch_id: "batch-2".to_owned(),
        user_id: "u-bob".to_owned(),
        user_name: "Bob".to_owned(),
        artists: vec![(ARTIST.to_owned(), "Test Artist".to_owned())],
        state: "pending".to_owned(),
        requested_at: 200,
    });
    let (_, _, rejected) = send(
        app(&state),
        request(
            Method::POST,
            "/requests/auto-download-approval-batches/batch-2/reject",
            Some(ADMIN),
            None,
        ),
    )
    .await;
    assert_eq!(rejected["success"], true);
    let (_, _, drained) = send(
        app(&state),
        request(
            Method::GET,
            "/requests/auto-download-approval-batches",
            Some(ADMIN),
            None,
        ),
    )
    .await;
    assert_eq!(drained["count"], 0);
}

// Live-ask count: two live rows read 2, landing one via sync reads 1.
#[tokio::test]
async fn active_count_tracks_live_rows() {
    let (state, dispatch) = RequestsState::for_tests();
    for mbid in [MBID_A, MBID_B] {
        send(
            app(&state),
            request(
                Method::POST,
                "/requests/albums",
                Some(TRUSTED),
                Some(album_body(mbid)),
            ),
        )
        .await;
    }
    let (_, _, count) = send(
        app(&state),
        request(Method::GET, "/requests/active/count", Some(TRUSTED), None),
    )
    .await;
    assert_eq!(count["count"], 2);

    let (_, _, active) = send(
        app(&state),
        request(Method::GET, "/requests/active", Some(TRUSTED), None),
    )
    .await;
    let task_id = active["items"][0]["task_id"].as_str().unwrap().to_owned();
    dispatch.land(&task_id);
    send(
        app(&state),
        request(Method::POST, "/requests/sync", Some(ADMIN), None),
    )
    .await;
    let (_, _, count) = send(
        app(&state),
        request(Method::GET, "/requests/active/count", Some(TRUSTED), None),
    )
    .await;
    assert_eq!(count["count"], 1);
}

// Auto-download reject happy path: a pending row rejects once, leaves the
// list, and a second decision loses.
#[tokio::test]
async fn auto_download_reject_happy_path() {
    let (state, _) = RequestsState::for_tests();
    state.follows.seed_pending(FollowApproval {
        user_id: "u-ada".to_owned(),
        user_name: "Ada".to_owned(),
        artist_mbid: ARTIST.to_owned(),
        artist_name: "Test Artist".to_owned(),
        state: "pending".to_owned(),
        requested_at: 100,
    });
    let reject = format!("/requests/auto-download-approvals/u-ada/{ARTIST}/reject");
    let (_, _, rejected) = send(
        app(&state),
        request(Method::POST, &reject, Some(ADMIN), None),
    )
    .await;
    assert_eq!(rejected["success"], true);
    let (_, _, list) = send(
        app(&state),
        request(
            Method::GET,
            "/requests/auto-download-approvals",
            Some(ADMIN),
            None,
        ),
    )
    .await;
    assert_eq!(list["count"], 0);
    let (_, _, again) = send(
        app(&state),
        request(Method::POST, &reject, Some(ADMIN), None),
    )
    .await;
    assert_eq!(again["success"], false);
}

// Active rows carry the linked task's progress snapshot; rows without a
// task read exactly as the record maps.
#[tokio::test]
async fn active_items_carry_task_progress() {
    let (state, dispatch) = RequestsState::for_tests();
    let (_, _, body) = send(
        app(&state),
        request(
            Method::POST,
            "/requests/albums",
            Some(TRUSTED),
            Some(album_body(MBID_A)),
        ),
    )
    .await;
    let task_id = body["task_id"].as_str().unwrap().to_owned();
    dispatch.set_progress(
        &task_id,
        TaskProgress {
            status: "downloading".to_owned(),
            progress_percent: 40,
            total_size_bytes: Some(1000),
            downloaded_bytes: 400,
            error_message: None,
            quality: Some("FLAC".to_owned()),
            protocol: "soulseek".to_owned(),
        },
    );

    let (_, _, active) = send(
        app(&state),
        request(Method::GET, "/requests/active", Some(TRUSTED), None),
    )
    .await;
    let item = &active["items"][0];
    assert_eq!(item["progress"], 40.0);
    assert_eq!(item["size"], 1000.0);
    assert_eq!(item["size_remaining"], 600.0);
    assert_eq!(item["quality"], "FLAC");
    assert_eq!(item["protocol"], "soulseek");
    assert!(item.get("error_message").is_none());
    // No live ETA source exists, so the field stays absent, never zero.
    assert!(item.get("eta").is_none());
    assert!(item.get("status_messages").is_none());
}

// Track rows keep their album context and approvals stamp review time,
// so the card can link artwork and name the reviewer.
#[tokio::test]
async fn track_rows_keep_album_context_and_review_stamp() {
    let (state, _) = RequestsState::for_tests();
    let track = json!({
        "recording_mbid": MBID_B,
        "artist_name": "Test Artist",
        "track_title": "Test Track",
        "album_title": "Test Album",
        "release_group_mbid": MBID_A,
    });
    send(
        app(&state),
        request(Method::POST, "/requests/tracks", Some(ADA), Some(track)),
    )
    .await;
    let approve = format!("/requests/approvals/{MBID_B}/approve?kind=track");
    send(
        app(&state),
        request(Method::POST, &approve, Some(ADMIN), None),
    )
    .await;

    let (_, _, history) = send(
        app(&state),
        request(Method::GET, "/requests/history", Some(ADMIN), None),
    )
    .await;
    let item = &history["items"][0];
    assert_eq!(item["track_release_group_mbid"], MBID_A);
    assert_eq!(item["reviewed_by_name"], "Root");
    assert!(item["reviewed_at"].as_u64().unwrap() > 0);
}

// Failed rows offer the admin reimport only while the task stays
// linked; the failure text rides along for the card's error line.
#[tokio::test]
async fn failed_rows_offer_reimport_only_when_linked() {
    let (state, dispatch) = RequestsState::for_tests();
    let (_, _, first) = send(
        app(&state),
        request(
            Method::POST,
            "/requests/albums",
            Some(TRUSTED),
            Some(album_body(MBID_A)),
        ),
    )
    .await;
    let (_, _, second) = send(
        app(&state),
        request(
            Method::POST,
            "/requests/albums",
            Some(TRUSTED),
            Some(album_body(MBID_B)),
        ),
    )
    .await;
    let task_a = first["task_id"].as_str().unwrap().to_owned();
    let task_b = second["task_id"].as_str().unwrap().to_owned();
    dispatch.fail(&task_a);
    dispatch.fail(&task_b);
    dispatch.set_reimportable(&task_a);
    dispatch.set_progress(
        &task_a,
        TaskProgress {
            status: "failed".to_owned(),
            error_message: Some("mount gone".to_owned()),
            ..Default::default()
        },
    );

    let (_, _, history) = send(
        app(&state),
        request(
            Method::GET,
            "/requests/history?status=failed",
            Some(TRUSTED),
            None,
        ),
    )
    .await;
    assert_eq!(history["total"], 2);
    let items = history["items"].as_array().unwrap();
    let row_a = items
        .iter()
        .find(|row| row["musicbrainz_id"] == MBID_A.to_lowercase())
        .unwrap();
    let row_b = items
        .iter()
        .find(|row| row["musicbrainz_id"] == MBID_B.to_lowercase())
        .unwrap();
    assert_eq!(row_a["can_reimport"], true);
    assert_eq!(row_a["error_message"], "mount gone");
    assert_eq!(row_a["task_id"], task_a);
    assert_eq!(row_b["can_reimport"], false);
    assert!(row_b.get("error_message").is_none());
}

// The wanted view serves the restored card fields: kind, the next-check
// ETA, year, and the admin-only owner name. Dormant rows pass through.
#[tokio::test]
async fn wanted_view_serves_restored_card_fields() {
    let (state, _) = RequestsState::for_tests();
    state.wanted.seed_watch(WantedWatch {
        key: MBID_A.to_owned(),
        artist_name: "A".to_owned(),
        album_title: "A".to_owned(),
        kind: "partial".to_owned(),
        state: "dormant".to_owned(),
        check_count: 9,
        next_check_at: Some(9_000),
        new_candidate_count: 0,
        created_at: 100,
        artist_mbid: Some(ARTIST.to_owned()),
        year: Some(1977),
        cover_url: None,
        user_id: "u-ada".to_owned(),
        user_name: Some("Ada".to_owned()),
    });
    state.wanted.seed_retrying(WantedRetrying {
        key: MBID_C.to_owned(),
        artist_name: "C".to_owned(),
        album_title: "C".to_owned(),
        retry_count: 2,
        max_attempts: 5,
        next_retry_at: Some(6_000),
        artist_mbid: None,
        year: Some(2022),
        cover_url: None,
        user_id: "u-ada".to_owned(),
        user_name: Some("Ada".to_owned()),
    });

    let (_, _, admin) = send(
        app(&state),
        request(Method::GET, "/requests/wanted", Some(ADMIN), None),
    )
    .await;
    let watch = &admin["items"][0];
    assert_eq!(watch["kind"], "partial");
    assert_eq!(watch["state"], "dormant");
    assert_eq!(watch["next_check_at"], 9_000);
    assert_eq!(watch["year"], 1977);
    assert_eq!(watch["artist_mbid"], ARTIST);
    assert_eq!(watch["user_name"], "Ada");
    let retrying = &admin["retrying"][0];
    assert_eq!(retrying["next_retry_at"], 6_000);
    assert_eq!(retrying["year"], 2022);
    assert_eq!(retrying["user_name"], "Ada");

    // Owners see their own rows but never the owner chip.
    let (_, _, own) = send(
        app(&state),
        request(Method::GET, "/requests/wanted", Some(ADA), None),
    )
    .await;
    assert!(own["items"][0].get("user_name").is_none());
    assert!(own["items"][0].get("user_id").is_none());
    assert_eq!(own["items"][0]["next_check_at"], 9_000);
}
