//! Collections routes (playlists, favorites, follows, edition pins) mounted
//! directly with the slice principal header: private playlists redact for
//! other users, rows never leak across users, auto-download approvals are
//! admin-only, pins never write identity, and store faults stay hidden.

use droppedneedle::reads::collections;

use std::sync::atomic::Ordering;

use axum::{
    Router,
    body::Body,
    http::{HeaderMap, Method, Request, StatusCode},
};
use collections::{
    CollectionsState,
    state::{EditionCatalogRow, ExternalIdentityRow},
};
use serde_json::{Value, json};
use tower::ServiceExt as _;

const ADA: &str = "u-ada:user:Ada";
const BOB: &str = "u-bob:user:Bob";
const TRUSTED: &str = "u-tris:trusted:Tris";
const ADMIN: &str = "u-root:admin:Root";

/// One-pixel PNG, base64.
const PIXEL_PNG: &str = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg==";

fn app(state: &CollectionsState) -> Router {
    collections::collections_router(state.clone())
        .fallback(collections::error::fallback_404)
        .method_not_allowed_fallback(collections::error::fallback_405)
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

fn raw_request(method: Method, uri: &str, identity: Option<&str>, body: &str) -> Request<Body> {
    let mut builder = Request::builder()
        .method(method)
        .uri(uri)
        .header("content-type", "application/json");
    if let Some(identity) = identity {
        builder = builder.header("x-slice-principal", identity);
    }
    builder.body(Body::from(body.to_owned())).unwrap()
}

async fn send(app: Router, req: Request<Body>) -> (StatusCode, HeaderMap, Vec<u8>) {
    let response = app.oneshot(req).await.unwrap();
    let status = response.status();
    let headers = response.headers().clone();
    let bytes = axum::body::to_bytes(response.into_body(), 16 * 1024 * 1024)
        .await
        .unwrap()
        .to_vec();
    (status, headers, bytes)
}

async fn send_json(app: Router, req: Request<Body>) -> (StatusCode, HeaderMap, Value) {
    let (status, headers, bytes) = send(app, req).await;
    let json: Value = serde_json::from_slice(&bytes).unwrap();
    (status, headers, json)
}

fn seed_catalog(state: &CollectionsState) {
    state.edition_catalog.albums.write().unwrap().insert(
        "alb-1".to_owned(),
        EditionCatalogRow {
            editions: vec!["rel-a".to_owned(), "rel-b".to_owned()],
            default_release_mbid: "rel-a".to_owned(),
        },
    );
}

fn seed_identity(state: &CollectionsState) {
    state
        .identities
        .seed_identity(ExternalIdentityRow {
            album_id: "alb-1".to_owned(),
            release_mbid: "rel-a".to_owned(),
            decision_source: "manual".to_owned(),
        })
        .unwrap();
}

fn track(name: &str) -> Value {
    json!({"track_name": name, "artist_name": "Art", "album_name": "Alb"})
}

// Playlists.

#[tokio::test]
async fn playlist_auth_matrix() {
    let state = CollectionsState::new();
    let app = app(&state);

    let (status, headers, body) =
        send_json(app.clone(), request(Method::GET, "/playlists", None, None)).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body["error"]["code"], "UNAUTHORIZED");
    assert_eq!(headers.get("www-authenticate").unwrap(), "Bearer");

    let (_, _, created) = send_json(
        app.clone(),
        request(
            Method::POST,
            "/playlists",
            Some(ADA),
            Some(json!({"name": "Secret"})),
        ),
    )
    .await;
    let id = created["id"].as_str().unwrap().to_owned();

    let (status, _, _) = send_json(
        app.clone(),
        request(Method::GET, &format!("/playlists/{id}"), Some(BOB), None),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "private hides from others");

    let (status, _, _) = send_json(
        app.clone(),
        request(
            Method::PUT,
            &format!("/playlists/{id}"),
            Some(BOB),
            Some(json!({"name": "Hijack"})),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "private mutation hides too");

    let (_, _, list) = send_json(
        app.clone(),
        request(Method::GET, "/playlists", Some(BOB), None),
    )
    .await;
    assert_eq!(list["playlists"][0]["is_redacted"], true);
    assert_eq!(list["playlists"][0]["owner_name"], "Ada");
    assert!(
        list["playlists"][0].get("name").is_none(),
        "redacted hides the name"
    );

    let (_, _, admin_list) = send_json(
        app.clone(),
        request(Method::GET, "/playlists", Some(ADMIN), None),
    )
    .await;
    assert_eq!(
        admin_list["playlists"][0]["is_redacted"], true,
        "admins read redacted too"
    );

    let (_, _, visible) = send_json(
        app.clone(),
        request(
            Method::PATCH,
            &format!("/playlists/{id}/visibility"),
            Some(ADA),
            Some(json!({"is_public": true})),
        ),
    )
    .await;
    assert_eq!(visible["is_public"], true);

    let (status, _, full) = send_json(
        app.clone(),
        request(Method::GET, &format!("/playlists/{id}"), Some(BOB), None),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(full["name"], "Secret");
    assert_eq!(full["is_owner"], false);

    let (status, _, forbidden) = send_json(
        app.clone(),
        request(
            Method::PUT,
            &format!("/playlists/{id}"),
            Some(BOB),
            Some(json!({"name": "Hijack"})),
        ),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "public rows deny others openly"
    );
    assert_eq!(forbidden["error"]["code"], "FORBIDDEN");
}

#[tokio::test]
async fn playlist_journey_create_add_reorder_cover_visibility() {
    let state = CollectionsState::new();
    let app = app(&state);

    let (_, _, created) = send_json(
        app.clone(),
        request(
            Method::POST,
            "/playlists",
            Some(ADA),
            Some(json!({"name": "Trip"})),
        ),
    )
    .await;
    let id = created["id"].as_str().unwrap().to_owned();

    let (_, _, added) = send_json(
        app.clone(),
        request(
            Method::POST,
            &format!("/playlists/{id}/tracks"),
            Some(ADA),
            Some(json!({"tracks": [track("One"), track("Two"), track("Three")]})),
        ),
    )
    .await;
    assert_eq!(added["tracks"].as_array().unwrap().len(), 3);
    let first_id = added["tracks"][0]["id"].as_str().unwrap().to_owned();

    let (_, _, reordered) = send_json(
        app.clone(),
        request(
            Method::PATCH,
            &format!("/playlists/{id}/tracks/reorder"),
            Some(ADA),
            Some(json!({"track_id": first_id, "new_position": 2})),
        ),
    )
    .await;
    assert_eq!(reordered["actual_position"], 2);

    let (_, _, detail) = send_json(
        app.clone(),
        request(Method::GET, &format!("/playlists/{id}"), Some(ADA), None),
    )
    .await;
    assert_eq!(detail["tracks"][2]["track_name"], "One");
    assert_eq!(detail["tracks"][2]["position"], 2);

    let (_, _, upload) = send_json(
        app.clone(),
        request(
            Method::POST,
            &format!("/playlists/{id}/cover"),
            Some(ADA),
            Some(json!({"image_base64": PIXEL_PNG, "content_type": "image/png"})),
        ),
    )
    .await;
    assert_eq!(upload["cover_url"], format!("/api/v3/playlists/{id}/cover"));

    let (status, headers, bytes) = send(
        app.clone(),
        request(
            Method::GET,
            &format!("/playlists/{id}/cover"),
            Some(ADA),
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers.get("content-type").unwrap(), "image/png");
    assert!(!bytes.is_empty());

    let (_, _, visible) = send_json(
        app.clone(),
        request(
            Method::PATCH,
            &format!("/playlists/{id}/visibility"),
            Some(ADA),
            Some(json!({"is_public": true})),
        ),
    )
    .await;
    assert_eq!(
        visible["custom_cover_url"],
        format!("/api/v3/playlists/{id}/cover")
    );

    let (status, _, _) = send(
        app.clone(),
        request(
            Method::GET,
            &format!("/playlists/{id}/cover"),
            Some(BOB),
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "public covers are world-readable");

    let (status, _, _) = send_json(
        app.clone(),
        request(
            Method::DELETE,
            &format!("/playlists/{id}/cover"),
            Some(ADA),
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let (status, _, _) = send(
        app.clone(),
        request(
            Method::GET,
            &format!("/playlists/{id}/cover"),
            Some(ADA),
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

/// Create one playlist with two tracks; returns (playlist id, first id, second id).
async fn seed_two_tracks(app: Router, name: &str) -> (String, String, String) {
    let (_, _, created) = send_json(
        app.clone(),
        request(
            Method::POST,
            "/playlists",
            Some(ADA),
            Some(json!({"name": name})),
        ),
    )
    .await;
    let id = created["id"].as_str().unwrap().to_owned();
    let (_, _, added) = send_json(
        app.clone(),
        request(
            Method::POST,
            &format!("/playlists/{id}/tracks"),
            Some(ADA),
            Some(json!({"tracks": [track("Alpha"), track("Beta")], "position": 0})),
        ),
    )
    .await;
    let alpha = added["tracks"][0]["id"].as_str().unwrap().to_owned();
    let beta = added["tracks"][1]["id"].as_str().unwrap().to_owned();
    (id, alpha, beta)
}

#[tokio::test]
async fn playlist_input_validation() {
    let state = CollectionsState::new();
    let app = app(&state);
    let (id, _, _) = seed_two_tracks(app.clone(), "Mix").await;

    for body in [json!({"name": "  "}), json!({"name": "x".repeat(201)})] {
        let (status, _, invalid) = send_json(
            app.clone(),
            request(Method::POST, "/playlists", Some(ADA), Some(body)),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(invalid["error"]["code"], "INVALID_INPUT");
    }

    let (status, _, invalid) = send_json(
        app.clone(),
        request(
            Method::POST,
            &format!("/playlists/{id}/tracks"),
            Some(ADA),
            Some(json!({"tracks": [{"track_name": "", "artist_name": "A", "album_name": "B"}]})),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(invalid["error"]["code"], "INVALID_INPUT");

    let (status, _, _) = send_json(
        app.clone(),
        request(
            Method::POST,
            &format!("/playlists/{id}/cover"),
            Some(ADA),
            Some(json!({"image_base64": PIXEL_PNG, "content_type": "image/gif"})),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    let (status, _, invalid) = send_json(
        app.clone(),
        request(
            Method::POST,
            &format!("/playlists/{id}/cover"),
            Some(ADA),
            Some(json!({"image_base64": "!!!not-base64!!!", "content_type": "image/png"})),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(invalid["error"]["code"], "INVALID_INPUT");

    let (status, _, _) = send_json(
        app.clone(),
        request(
            Method::POST,
            &format!("/playlists/{id}/cover"),
            Some(ADA),
            Some(json!({"image_base64": "A".repeat(7_000_000), "content_type": "image/png"})),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "oversized covers refuse");

    let (status, _, malformed) = send_json(
        app.clone(),
        raw_request(Method::POST, "/playlists", Some(ADA), "{not json"),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(malformed["error"]["code"], "INVALID_INPUT");
}

// Favorites.

#[tokio::test]
async fn favorites_shape_and_isolation() {
    let state = CollectionsState::new();
    let app = app(&state);

    let (status, _, _) =
        send_json(app.clone(), request(Method::GET, "/favorites", None, None)).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    for (kind, item, name) in [("album", "alb-1", "First"), ("track", "trk-9", "Ninth")] {
        let (status, _, status_body) = send_json(
            app.clone(),
            request(
                Method::PUT,
                &format!("/favorites/{kind}/{item}"),
                Some(ADA),
                Some(json!({"favorited": true, "name": name})),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(status_body["favorited"], true);
    }

    let (_, _, list) = send_json(
        app.clone(),
        request(Method::GET, "/favorites", Some(ADA), None),
    )
    .await;
    assert_eq!(list["items"].as_array().unwrap().len(), 2);
    assert_eq!(list["counts"], json!({"album": 1, "artist": 0, "track": 1}));

    let (_, _, filtered) = send_json(
        app.clone(),
        request(Method::GET, "/favorites?kind=album", Some(ADA), None),
    )
    .await;
    assert_eq!(filtered["items"].as_array().unwrap().len(), 1);
    assert_eq!(filtered["counts"]["track"], 1, "counts ignore the filter");

    let (_, _, empty) = send_json(
        app.clone(),
        request(Method::GET, "/favorites", Some(BOB), None),
    )
    .await;
    assert_eq!(
        empty["items"],
        json!([]),
        "favorites never leak across users"
    );

    let (status, _, unfaved) = send_json(
        app.clone(),
        request(
            Method::PUT,
            "/favorites/album/alb-1",
            Some(ADA),
            Some(json!({"favorited": false})),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(unfaved["favorited"], false);

    let (status, _, invalid) = send_json(
        app.clone(),
        request(
            Method::PUT,
            "/favorites/genre/rock",
            Some(ADA),
            Some(json!({"favorited": true})),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(invalid["error"]["code"], "INVALID_INPUT");
}

// Follows.

#[tokio::test]
async fn follow_journey_follow_then_auto_download_toggle() {
    let state = CollectionsState::new();
    let app = app(&state);

    let (_, _, default) = send_json(
        app.clone(),
        request(
            Method::GET,
            "/artists/mb-followed/follow-status",
            Some(ADA),
            None,
        ),
    )
    .await;
    assert_eq!(default["followed"], false);
    assert_eq!(default["auto_download_state"], "off");

    let (_, _, followed) = send_json(
        app.clone(),
        request(
            Method::PUT,
            "/artists/mb-followed/follow",
            Some(ADA),
            Some(json!({"followed": true, "artist_name": "Followed One"})),
        ),
    )
    .await;
    assert_eq!(followed["followed"], true);

    let (_, _, pending) = send_json(
        app.clone(),
        request(
            Method::PUT,
            "/artists/mb-followed/auto-download",
            Some(ADA),
            Some(json!({"enabled": true})),
        ),
    )
    .await;
    assert_eq!(pending["auto_download"], true);
    assert_eq!(pending["auto_download_state"], "pending");

    let (_, _, approvals) = send_json(
        app.clone(),
        request(
            Method::GET,
            "/requests/auto-download-approvals",
            Some(ADMIN),
            None,
        ),
    )
    .await;
    assert_eq!(approvals["count"], 1);
    assert_eq!(approvals["items"][0]["artist_mbid"], "mb-followed");

    let (_, _, off) = send_json(
        app.clone(),
        request(
            Method::PUT,
            "/artists/mb-followed/auto-download",
            Some(ADA),
            Some(json!({"enabled": false})),
        ),
    )
    .await;
    assert_eq!(off["auto_download_state"], "off");

    let (_, _, unfollowed) = send_json(
        app.clone(),
        request(
            Method::PUT,
            "/artists/mb-followed/follow",
            Some(ADA),
            Some(json!({"followed": false})),
        ),
    )
    .await;
    assert_eq!(unfollowed["followed"], false);

    let (_, _, followed_list) = send_json(
        app.clone(),
        request(Method::GET, "/following/artists", Some(ADA), None),
    )
    .await;
    assert_eq!(followed_list["artists"], json!([]));

    let (_, _, trusted_follow) = send_json(
        app.clone(),
        request(
            Method::PUT,
            "/artists/mb-followed/follow",
            Some(TRUSTED),
            Some(json!({"followed": true, "artist_name": "Followed One"})),
        ),
    )
    .await;
    assert_eq!(trusted_follow["followed"], true);

    let (_, _, active) = send_json(
        app.clone(),
        request(
            Method::PUT,
            "/artists/mb-followed/auto-download",
            Some(TRUSTED),
            Some(json!({"enabled": true})),
        ),
    )
    .await;
    assert_eq!(
        active["auto_download_state"], "active",
        "trusted auto-approves"
    );
}

#[tokio::test]
async fn follow_guards_and_matrix() {
    let state = CollectionsState::new();
    let app = app(&state);

    let (status, _, _) = send_json(
        app.clone(),
        request(Method::GET, "/artists/mb-x/follow-status", None, None),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    let (status, _, conflict) = send_json(
        app.clone(),
        request(
            Method::PUT,
            "/artists/mb-x/auto-download",
            Some(ADA),
            Some(json!({"enabled": true})),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(conflict["error"]["code"], "CONFLICT");

    let (status, _, _) = send_json(
        app.clone(),
        request(
            Method::PUT,
            "/artists/mb-x/auto-download",
            Some(ADA),
            Some(json!({"enabled": false})),
        ),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "disabling an unfollowed artist is absence"
    );

    let (_, _, followed) = send_json(
        app.clone(),
        request(
            Method::PUT,
            "/artists/mb-x/follow",
            Some(ADA),
            Some(json!({"followed": true})),
        ),
    )
    .await;
    assert_eq!(followed["followed"], true);

    let (_, _, list) = send_json(
        app.clone(),
        request(Method::GET, "/following/artists", Some(ADA), None),
    )
    .await;
    assert_eq!(list["artists"].as_array().unwrap().len(), 1);
    assert_eq!(
        list["artists"][0]["name"], "",
        "missing names stay empty, not failed"
    );

    let (_, _, other_list) = send_json(
        app.clone(),
        request(Method::GET, "/following/artists", Some(BOB), None),
    )
    .await;
    assert_eq!(
        other_list["artists"],
        json!([]),
        "follows never leak across users"
    );
}

// Approvals.

#[tokio::test]
async fn approvals_admin_only_with_batches() {
    let state = CollectionsState::new();
    let app = app(&state);

    for (identity, mbid, name) in [
        (ADA, "mb-a", "Artist A"),
        (BOB, "mb-b1", "Artist B1"),
        (BOB, "mb-b2", "Artist B2"),
    ] {
        let (_, _, followed) = send_json(
            app.clone(),
            request(
                Method::PUT,
                &format!("/artists/{mbid}/follow"),
                Some(identity),
                Some(json!({"followed": true, "artist_name": name})),
            ),
        )
        .await;
        assert_eq!(followed["followed"], true);
        let (_, _, pending) = send_json(
            app.clone(),
            request(
                Method::PUT,
                &format!("/artists/{mbid}/auto-download"),
                Some(identity),
                Some(json!({"enabled": true})),
            ),
        )
        .await;
        assert_eq!(pending["auto_download_state"], "pending");
    }

    let (status, _, _) = send_json(
        app.clone(),
        request(Method::GET, "/requests/auto-download-approvals", None, None),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    for uri in [
        "/requests/auto-download-approvals",
        "/requests/auto-download-approval-batches",
    ] {
        let (status, _, forbidden) =
            send_json(app.clone(), request(Method::GET, uri, Some(ADA), None)).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert_eq!(forbidden["error"]["code"], "FORBIDDEN");
    }

    let (_, _, approvals) = send_json(
        app.clone(),
        request(
            Method::GET,
            "/requests/auto-download-approvals",
            Some(ADMIN),
            None,
        ),
    )
    .await;
    assert_eq!(approvals["count"], 3);
    assert_eq!(approvals["items"][0]["user_name"], "Ada");

    let (_, _, batches) = send_json(
        app.clone(),
        request(
            Method::GET,
            "/requests/auto-download-approval-batches",
            Some(ADMIN),
            None,
        ),
    )
    .await;
    assert_eq!(batches["count"], 2);
    let bob_batch = batches["batches"]
        .as_array()
        .unwrap()
        .iter()
        .find(|batch| batch["user_id"] == "u-bob")
        .unwrap();
    assert_eq!(bob_batch["artist_count"], 2);
    assert_eq!(bob_batch["sample_names"].as_array().unwrap().len(), 2);
    assert_eq!(bob_batch["source"], "follow_request");
}

// Pins.

#[tokio::test]
async fn pin_auth_matrix() {
    let state = CollectionsState::new();
    seed_catalog(&state);
    let app = app(&state);

    let (status, _, _) = send_json(
        app.clone(),
        request(Method::GET, "/library/albums/alb-1/edition-pin", None, None),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    let (status, _, _) = send_json(
        app.clone(),
        request(
            Method::PUT,
            "/library/albums/alb-1/edition-pin",
            Some(ADA),
            Some(json!({"release_mbid": "rel-b"})),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "pin writes need curator");

    let (status, _, _) = send_json(
        app.clone(),
        request(
            Method::DELETE,
            "/library/albums/alb-1/edition-pin",
            Some(ADA),
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    let (status, _, _) = send_json(
        app.clone(),
        request(
            Method::GET,
            "/library/albums/alb-1/edition-pin",
            Some(ADA),
            None,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "pin reads stay open");
}

#[tokio::test]
async fn pin_hint_only_brief() {
    let state = CollectionsState::new();
    seed_catalog(&state);
    seed_identity(&state);
    let app = app(&state);

    assert_eq!(
        state.identities.writes.load(Ordering::SeqCst),
        0,
        "fixture seeding bypasses the write counter"
    );
    state
        .identities
        .save_identity(ExternalIdentityRow {
            album_id: "alb-1".to_owned(),
            release_mbid: "rel-a".to_owned(),
            decision_source: "manual".to_owned(),
        })
        .unwrap();
    assert_eq!(
        state.identities.writes.load(Ordering::SeqCst),
        1,
        "the counter observes genuine engine writes"
    );
    let before = state
        .identities
        .identities
        .read()
        .unwrap()
        .get("alb-1")
        .cloned()
        .unwrap();

    let (_, _, pinned) = send_json(
        app.clone(),
        request(
            Method::PUT,
            "/library/albums/alb-1/edition-pin",
            Some(TRUSTED),
            Some(json!({"release_mbid": "rel-b"})),
        ),
    )
    .await;
    assert_eq!(pinned["selected_release_mbid"], "rel-b");

    let pin_row = state
        .pins
        .pins
        .read()
        .unwrap()
        .get("alb-1")
        .cloned()
        .unwrap();
    assert_eq!(pin_row.pinned_by, "u-tris");
    assert!(pin_row.pinned_at > 0);

    let (_, _, _) = send_json(
        app.clone(),
        request(
            Method::DELETE,
            "/library/albums/alb-1/edition-pin",
            Some(TRUSTED),
            None,
        ),
    )
    .await;

    let identities = state.identities.identities.read().unwrap();
    assert_eq!(identities.len(), 1, "pins add zero identity rows");
    assert_eq!(
        identities.get("alb-1"),
        Some(&before),
        "the accepted identity survives pin set and clear untouched"
    );
    assert_eq!(
        state.identities.writes.load(Ordering::SeqCst),
        1,
        "pin paths perform zero identity writes"
    );
}

// Leaks.

#[tokio::test]
async fn leak_briefs_fixed_500_and_envelopes() {
    let state = CollectionsState::new();
    seed_catalog(&state);
    let app = app(&state);

    state.fail_stores.store(true, Ordering::SeqCst);
    for (method, uri) in [
        (Method::GET, "/playlists"),
        (Method::GET, "/favorites"),
        (Method::GET, "/following/artists"),
        (Method::GET, "/requests/auto-download-approvals"),
        (Method::GET, "/library/albums/alb-1/edition-pin"),
    ] {
        let (status, _, bytes) = send(app.clone(), request(method, uri, Some(ADMIN), None)).await;
        assert_eq!(
            status,
            StatusCode::INTERNAL_SERVER_ERROR,
            "{uri} fails closed"
        );
        let body: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(body["error"]["code"], "INTERNAL_ERROR");
        assert_eq!(body["error"]["message"], "Internal server error");
        assert!(body["error"]["details"]["error_id"].is_string());
        let raw = String::from_utf8(bytes).unwrap();
        assert!(
            !raw.contains("inject") && !raw.contains("poison") && !raw.contains("alb-1"),
            "5xx bodies carry no cause text: {raw}"
        );
    }
    state.fail_stores.store(false, Ordering::SeqCst);

    let (status, _, missing) = send_json(
        app.clone(),
        request(Method::GET, "/no-such-route", Some(ADA), None),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(
        missing,
        json!({"error": {"code": "NOT_FOUND", "message": "Not found", "details": Value::Null}})
    );

    let (status, _, wrong_method) = send_json(
        app.clone(),
        request(Method::DELETE, "/playlists/check-tracks", Some(ADA), None),
    )
    .await;
    assert_eq!(status, StatusCode::METHOD_NOT_ALLOWED);
    assert_eq!(wrong_method["error"]["code"], "METHOD_NOT_ALLOWED");
}

#[tokio::test]
async fn playlist_source_ref_stays_out_of_redacted_rows() {
    let state = CollectionsState::new();
    let app = app(&state);

    let (_, _, created) = send_json(
        app.clone(),
        request(
            Method::POST,
            "/playlists",
            Some(ADA),
            Some(json!({"name": "Secret", "source_ref": "plex:xyz"})),
        ),
    )
    .await;
    let id = created["id"].as_str().unwrap().to_owned();
    assert_eq!(created["source_ref"], "plex:xyz");

    let (_, _, list) = send_json(
        app.clone(),
        request(Method::GET, "/playlists", Some(BOB), None),
    )
    .await;
    let row = &list["playlists"][0];
    assert_eq!(row["id"], id);
    assert_eq!(row["is_redacted"], true);
    assert!(
        row.get("source_ref").is_none(),
        "redacted rows never carry provenance"
    );
}
