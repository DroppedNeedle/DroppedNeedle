//! Search-result counts run through the app against a scripted
//! ListenBrainz: a failed row stays on its row.

/// ListenBrainz on a loopback origin, refusing the second artist.
fn listenbrainz_stub() -> axum::Router {
    use axum::extract::Path;
    use axum::http::StatusCode;
    use axum::response::IntoResponse;
    use axum::routing::{get, post};
    axum::Router::new()
        .route(
            "/listenbrainz/1/popularity/top-release-groups-for-artist/{mbid}",
            get(|Path(mbid): Path<String>| async move {
                if mbid == "artist-mbid-2" {
                    return StatusCode::FORBIDDEN.into_response();
                }
                axum::Json(serde_json::json!([
                    {"release_group_mbid": "rg-a", "total_listen_count": 3_000_000},
                    {"release_group_mbid": "rg-b", "total_listen_count": 1_000_000},
                ]))
                .into_response()
            }),
        )
        .route(
            "/listenbrainz/1/popularity/release-group",
            post(|| async {
                axum::Json(serde_json::json!([
                    {"release_group_mbid": "rg-mbid-1", "total_listen_count": 90_000},
                ]))
            }),
        )
}

/// ListenBrainz counts sum an artist's top release groups and read album
/// counts in one call; a refused artist call leaves that row without a
/// count and notes ListenBrainz once, and the other rows still count.
#[tokio::test]
async fn a_failed_count_stays_on_its_row() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let server =
        tokio::spawn(async move { axum::serve(listener, listenbrainz_stub()).await.unwrap() });
    let app =
        crate::common::TestApp::start_with("search-counts", crate::common::outbound_at(&origin))
            .await;
    let admin = app.login_admin().await;
    let auth = [("authorization", admin.as_str())];
    let (status, body) = app
        .call(
            "PUT",
            "/api/v3/settings/listenbrainz",
            &auth,
            Some(serde_json::json!({"enabled": true, "username": "", "user_token": ""})),
        )
        .await;
    assert!(status.is_success(), "{status} {body}");

    let (status, body) = app
        .call(
            "POST",
            "/api/v3/search/enrich/batch",
            &auth,
            Some(serde_json::json!({
                "artists": [
                    {"musicbrainz_id": "artist-mbid-1", "name": "Portishead"},
                    {"musicbrainz_id": "artist-mbid-2", "name": "Massive Attack"},
                ],
                "albums": [
                    {"musicbrainz_id": "rg-mbid-1", "artist_name": "Portishead", "album_name": "Dummy"},
                ],
            })),
        )
        .await;

    assert!(status.is_success(), "{status} {body}");
    assert_eq!(body["source"], "listenbrainz", "{body}");
    assert_eq!(body["artists"][0]["listen_count"], 4_000_000, "{body}");
    assert!(body["artists"][1]["listen_count"].is_null(), "{body}");
    assert_eq!(body["albums"][0]["listen_count"], 90_000, "{body}");
    let notes = body["degradations"].as_array().expect("degradations");
    assert_eq!(notes.len(), 1, "{body}");
    assert_eq!(notes[0]["source"], "listenbrainz", "{body}");
    app.stop().await;
    server.abort();
}
