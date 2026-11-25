//! Saved rules use live account ratings and page independently of the library size.
use axum::http::StatusCode;
use serde_json::json;
use std::collections::HashSet;

use crate::common::TestApp;

const BASE: &str = "/api/v3/smart-playlists";

#[tokio::test]
async fn rated_tracks_page_in_stable_order_and_follow_rating_changes() {
    let app = TestApp::start("smart-playlists").await;
    let ada = app.login_user("ada").await;
    let bob = app.login_user("bob").await;
    let auth = [("authorization", ada.authorization.as_str())];
    // Equal titles throughout exercise the file-id tie breaker at every boundary.
    app.seed(
        "INSERT INTO local_artists (id, display_name, folded_name, kind, created_at, updated_at)
         VALUES ('artist', 'Artist', 'artist', 'group', 1, 1);
         INSERT INTO local_albums (id, root_id, grouping_key, title, title_folded, album_artist_id,
             grouping_source, created_at, updated_at)
         VALUES ('alb', 'r1', 'alb', 'Album', 'album', 'artist', 'automatic', 1, 1);
         WITH RECURSIVE seq(n) AS (VALUES(1) UNION ALL SELECT n + 1 FROM seq WHERE n < 10002)
         INSERT INTO local_tracks (id, local_album_id, root_id, file_path, relative_path,
             path_hash, file_size_bytes, file_mtime_ns, stat_revision, title, title_folded, album_title, album_title_folded,
             availability, file_format, ingest_source, imported_at, membership_source)
         SELECT printf('t%05d',n), 'alb','r1','/music/'||n,n,n,1,1,'s','Song','song','Album','album',
             CASE WHEN n=10002 THEN 'missing' ELSE 'indexed' END,'flac','scan',1,'automatic'
         FROM seq;",
    ).await;
    app.seed_with(
        "INSERT INTO catalog_user_ratings (user_id, item_kind, item_id, rating, rated_at)
         SELECT ?1, 'track', id, 5, 1 FROM local_tracks WHERE id <> 't10001'",
        [&ada.user_id],
    )
    .await;
    app.seed_with(
        "INSERT INTO catalog_user_ratings (user_id, item_kind, item_id, rating, rated_at)
         VALUES (?1, 'track', 't10001', 5, 1)",
        [&bob.user_id],
    )
    .await;

    for invalid in [
        json!({"name":" ","min_rating":5}),
        json!({"name":"Stars","min_rating":0}),
        json!({"name":"Stars","min_rating":6}),
        json!({"name":"Stars","min_rating":5,"unknown_rule":true}),
    ] {
        let (status, _) = app.call("POST", BASE, &auth, Some(invalid)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }
    let (status, def) = app
        .call(
            "POST",
            BASE,
            &auth,
            Some(json!({"name":"  Five stars  ","min_rating":5})),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{def}");
    assert_eq!(def["name"], "Five stars");
    let (status, others) = app
        .call("GET", BASE, &[("authorization", &bob.authorization)], None)
        .await;
    assert_eq!((status, others), (StatusCode::OK, json!([])));
    let path = format!("{BASE}/{}/tracks", def["id"].as_str().unwrap());
    let mut url = format!("{path}?limit=500");
    let mut seen = HashSet::new();
    loop {
        let (status, page) = app.call("GET", &url, &auth, None).await;
        assert_eq!(status, StatusCode::OK, "{page}");
        if seen.is_empty() {
            assert_eq!(page["total"], 10000);
        }
        for track in page["items"].as_array().unwrap() {
            let id = track["id"].as_str().unwrap();
            assert!(seen.insert(id.to_owned()), "Duplicate {id}");
        }
        let Some(next) = page["next"].as_str() else {
            break;
        };
        url = format!("{path}?limit=500&after={next}");
    }
    assert_eq!(seen.len(), 10000);
    assert!(!seen.contains("t10001"), "Another user's rating");
    assert!(!seen.contains("t10002"), "A missing file");

    app.seed_with(
        "UPDATE catalog_user_ratings SET rating=4 WHERE user_id=?1 AND item_id='t00001'",
        [&ada.user_id],
    )
    .await;
    let (status, page) = app
        .call("GET", &format!("{path}?limit=1"), &auth, None)
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(page["total"], 9999);
    assert_eq!(page["items"][0]["id"], "t00002");

    let definition = format!("{BASE}/{}", def["id"].as_str().unwrap());
    let (status, changed) = app
        .call(
            "PUT",
            &definition,
            &auth,
            Some(json!({"name":"Four and five", "min_rating":4})),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(changed["id"], def["id"]);
    let (status, page) = app
        .call("GET", &format!("{path}?limit=1"), &auth, None)
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(page["total"], 10000);
    assert_eq!(page["items"][0]["id"], "t00001");
    app.stop().await;
}
