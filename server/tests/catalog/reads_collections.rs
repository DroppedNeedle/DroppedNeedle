//! Collections routes (playlists, favorites, follows, approvals, edition
//! pins) over a scratch SQLite database, signed in with the test principal
//! layer: private playlists redact for other users, rows never leak across
//! users, everything survives a restart, pins never write identity, and
//! store faults stay hidden.

use droppedneedle_acquire::db::AcquireDb;
use droppedneedle_acquire::follows::{self, Follows};
use droppedneedle_auth::Role;
use droppedneedle_auth::session::store::SessionKind;
use droppedneedle_catalog::reads::collections::{self, CollectionsState, db::CollectionsDb};
use droppedneedle_kernel::db::{DbConfig, DbRuntime, open_runtime};

use axum::{
    Router,
    body::Body,
    http::{HeaderMap, Method, Request, StatusCode},
};
use serde_json::{Value, json};
use tower::ServiceExt as _;

use crate::common::auth::{test_principal, with_test_principal};
use crate::common::{ScratchDir, TestApp};

/// A signed-in caller: user id and role.
type Identity = (&'static str, Role);

const ADA: Identity = ("u-ada", Role::User);
const BOB: Identity = ("u-bob", Role::User);
const TRUSTED: Identity = ("u-tris", Role::Trusted);
const ADMIN: Identity = ("u-root", Role::Admin);

/// One-pixel PNG, base64.
const PIXEL_PNG: &str = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg==";

/// A migrated scratch database with the four test accounts.
struct Rig {
    dir: ScratchDir,
    runtime: DbRuntime,
}

impl Rig {
    async fn open() -> Self {
        let dir = ScratchDir::new("collections");
        crate::common::migrated_db(&dir.join("app.db"));
        let runtime = Self::boot(&dir).await;
        let rows = [
            ("u-ada", "Ada", "user"),
            ("u-bob", "Bob", "user"),
            ("u-tris", "Tris", "trusted"),
            ("u-root", "Root", "admin"),
        ];
        runtime
            .db()
            .write(
                droppedneedle_kernel::db::Lane::Foreground,
                "seed users",
                move |tx| {
                    for (id, name, role) in rows {
                        tx.execute(
                            "INSERT INTO auth_users (id, display_name, role, created_at) \
                         VALUES (?1, ?2, ?3, '2024-01-01T00:00:00Z')",
                            rusqlite::params![id, name, role],
                        )?;
                    }
                    Ok::<_, droppedneedle_kernel::db::DbError>(())
                },
            )
            .await
            .unwrap();
        Self { dir, runtime }
    }

    async fn boot(dir: &ScratchDir) -> DbRuntime {
        open_runtime(&DbConfig::new(&dir.join("app.db")))
            .await
            .unwrap()
    }

    fn state(&self) -> CollectionsState {
        CollectionsState::new(
            CollectionsDb::new(
                self.runtime.db().clone(),
                std::sync::Arc::new(droppedneedle_kernel::clock::SystemClock),
            ),
            droppedneedle_catalog::reads::library::store::LocalCatalog::new(
                self.runtime.db().clone(),
            ),
        )
    }

    /// The collections routes beside the follows' routes, as the app
    /// mounts them.
    fn app(&self) -> Router {
        let follows = Follows::new(AcquireDb::new(
            self.runtime.db().clone(),
            std::sync::Arc::new(droppedneedle_kernel::clock::SystemClock),
        ));
        collections::routes()
            .with_state(self.state())
            .into_router()
            .merge(follows::routes().with_state(follows).into_router())
    }

    /// Close the database and open it again, as a server restart does.
    async fn restart(self) -> Self {
        let Self { dir, runtime } = self;
        runtime.shutdown().await;
        let runtime = Self::boot(&dir).await;
        Self { dir, runtime }
    }
}

fn request(method: Method, uri: &str, body: Option<Value>) -> Request<Body> {
    let mut builder = Request::builder().method(method).uri(uri);
    let body = match body {
        Some(json) => {
            builder = builder.header("content-type", "application/json");
            Body::from(json.to_string())
        }
        None => Body::empty(),
    };
    builder.body(body).unwrap()
}

/// Send `req` to `app`, signed in as `identity` when there is one.
async fn send(
    app: &Router,
    identity: Option<Identity>,
    req: Request<Body>,
) -> (StatusCode, HeaderMap, Vec<u8>) {
    let app = match identity {
        Some((user_id, role)) => with_test_principal(
            app.clone(),
            test_principal(user_id, role, "sess-test", SessionKind::Standard),
        ),
        None => app.clone(),
    };
    let response = app.oneshot(req).await.unwrap();
    let status = response.status();
    let headers = response.headers().clone();
    let bytes = axum::body::to_bytes(response.into_body(), 16 * 1024 * 1024)
        .await
        .unwrap()
        .to_vec();
    (status, headers, bytes)
}

async fn call(
    app: &Router,
    method: Method,
    uri: &str,
    identity: Option<Identity>,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let (status, _, bytes) = send(app, identity, request(method, uri, body)).await;
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

fn track(name: &str) -> Value {
    json!({"track_name": name, "artist_name": "Art", "album_name": "Alb"})
}

#[tokio::test]
async fn playlist_auth_matrix() {
    let rig = Rig::open().await;
    let app = rig.app();

    let (status, headers, _) =
        send(&app, None, request(Method::GET, "/api/v3/playlists", None)).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(headers.get("www-authenticate").unwrap(), "Bearer");

    let (_, created) = call(
        &app,
        Method::POST,
        "/api/v3/playlists",
        Some(ADA),
        Some(json!({"name": "Secret", "source_ref": "plex:xyz"})),
    )
    .await;
    let id = created["id"].as_str().unwrap().to_owned();
    let one = format!("/api/v3/playlists/{id}");

    let (status, _) = call(&app, Method::GET, &one, Some(BOB), None).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "private hides from others");
    let (status, _) = call(
        &app,
        Method::PUT,
        &one,
        Some(BOB),
        Some(json!({"name": "X"})),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "private mutation hides too");

    for identity in [BOB, ADMIN] {
        let (_, list) = call(&app, Method::GET, "/api/v3/playlists", Some(identity), None).await;
        let row = &list["playlists"][0];
        assert_eq!(row["is_redacted"], true, "admins read redacted too");
        assert_eq!(row["owner_name"], "Ada");
        assert!(row.get("name").is_none() && row.get("source_ref").is_none());
    }

    let (_, visible) = call(
        &app,
        Method::PATCH,
        &format!("{one}/visibility"),
        Some(ADA),
        Some(json!({"is_public": true})),
    )
    .await;
    assert_eq!(visible["is_public"], true);
    let (status, full) = call(&app, Method::GET, &one, Some(BOB), None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        (full["name"].as_str(), full["is_owner"].as_bool()),
        (Some("Secret"), Some(false))
    );
    let (status, body) = call(
        &app,
        Method::PUT,
        &one,
        Some(BOB),
        Some(json!({"name": "X"})),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "public rows deny others openly"
    );
    assert_eq!(body["error"]["code"], "FORBIDDEN");
}

#[tokio::test]
async fn playlists_favorites_and_follows_survive_a_restart() {
    let rig = Rig::open().await;
    let app = rig.app();

    let (_, created) = call(
        &app,
        Method::POST,
        "/api/v3/playlists",
        Some(ADA),
        Some(json!({"name": "Trip"})),
    )
    .await;
    let id = created["id"].as_str().unwrap().to_owned();
    let (_, added) = call(
        &app,
        Method::POST,
        &format!("/api/v3/playlists/{id}/tracks"),
        Some(ADA),
        Some(json!({"tracks": [track("One"), track("Two"), track("Three")]})),
    )
    .await;
    let first = added["tracks"][0]["id"].as_str().unwrap().to_owned();
    let (_, moved) = call(
        &app,
        Method::PATCH,
        &format!("/api/v3/playlists/{id}/tracks/reorder"),
        Some(ADA),
        Some(json!({"track_id": first, "new_position": 9})),
    )
    .await;
    assert_eq!(moved["actual_position"], 2, "past-the-end clamps");
    let (status, _) = call(
        &app,
        Method::POST,
        &format!("/api/v3/playlists/{id}/cover"),
        Some(ADA),
        Some(json!({"image_base64": PIXEL_PNG, "content_type": "image/png"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    call(
        &app,
        Method::PUT,
        "/api/v3/favorites/album/alb-1",
        Some(ADA),
        Some(json!({"favorited": true, "name": "First"})),
    )
    .await;
    call(
        &app,
        Method::PUT,
        "/api/v3/artists/MB-Followed/follow",
        Some(ADA),
        Some(json!({"followed": true, "artist_name": "Followed"})),
    )
    .await;

    let rig = rig.restart().await;
    let app = rig.app();

    let (_, detail) = call(
        &app,
        Method::GET,
        &format!("/api/v3/playlists/{id}"),
        Some(ADA),
        None,
    )
    .await;
    let names = detail["tracks"]
        .as_array()
        .unwrap()
        .iter()
        .map(|track| track["track_name"].as_str().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(names, ["Two", "Three", "One"]);
    assert_eq!(
        detail["custom_cover_url"],
        format!("/api/v3/playlists/{id}/cover")
    );
    let (status, headers, bytes) = send(
        &app,
        Some(ADA),
        request(Method::GET, &format!("/api/v3/playlists/{id}/cover"), None),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers.get("content-type").unwrap(), "image/png");
    assert!(!bytes.is_empty());

    let (_, favorites) = call(&app, Method::GET, "/api/v3/favorites", Some(ADA), None).await;
    assert_eq!(favorites["items"][0]["name"], "First");
    assert_eq!(
        favorites["counts"],
        json!({"album": 1, "artist": 0, "track": 0})
    );
    let (_, follow) = call(
        &app,
        Method::GET,
        "/api/v3/artists/mb-followed/follow-status",
        Some(ADA),
        None,
    )
    .await;
    assert_eq!(
        follow["followed"], true,
        "follows key on the lowercased MBID"
    );
}

#[tokio::test]
async fn playlist_input_validation() {
    let rig = Rig::open().await;
    let app = rig.app();
    let (_, created) = call(
        &app,
        Method::POST,
        "/api/v3/playlists",
        Some(ADA),
        Some(json!({"name": "Mix"})),
    )
    .await;
    let id = created["id"].as_str().unwrap().to_owned();

    let bad = [
        ("/api/v3/playlists".to_owned(), json!({"name": "  "})),
        (
            "/api/v3/playlists".to_owned(),
            json!({"name": "x".repeat(201)}),
        ),
        (
            format!("/api/v3/playlists/{id}/tracks"),
            json!({"tracks": [{"track_name": "", "artist_name": "A", "album_name": "B"}]}),
        ),
        (
            format!("/api/v3/playlists/{id}/tracks"),
            json!({"tracks": [{"track_name": "T", "artist_name": "A", "album_name": "B", "source_type": "napster"}]}),
        ),
        (
            format!("/api/v3/playlists/{id}/cover"),
            json!({"image_base64": PIXEL_PNG, "content_type": "image/gif"}),
        ),
        (
            format!("/api/v3/playlists/{id}/cover"),
            json!({"image_base64": "!!!", "content_type": "image/png"}),
        ),
    ];
    for (uri, body) in bad {
        let (status, body) = call(&app, Method::POST, &uri, Some(ADA), Some(body)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{uri}");
        assert_eq!(body["error"]["code"], "INVALID_INPUT");
    }

    let oversized = json!({"image_base64": "A".repeat(7_000_000), "content_type": "image/png"});
    let (status, body) = call(
        &app,
        Method::POST,
        &format!("/api/v3/playlists/{id}/cover"),
        Some(ADA),
        Some(oversized),
    )
    .await;
    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
    assert_eq!(body["error"]["code"], "PAYLOAD_TOO_LARGE");
}

#[tokio::test]
async fn favorites_stay_per_user() {
    let rig = Rig::open().await;
    let app = rig.app();
    for (kind, item) in [("album", "alb-1"), ("track", "trk-9")] {
        let (status, body) = call(
            &app,
            Method::PUT,
            &format!("/api/v3/favorites/{kind}/{item}"),
            Some(ADA),
            Some(json!({"favorited": true})),
        )
        .await;
        assert_eq!(
            (status, body["favorited"].as_bool()),
            (StatusCode::OK, Some(true))
        );
    }
    let (_, filtered) = call(
        &app,
        Method::GET,
        "/api/v3/favorites?kind=album",
        Some(ADA),
        None,
    )
    .await;
    assert_eq!(filtered["items"].as_array().unwrap().len(), 1);
    assert_eq!(filtered["counts"]["track"], 1, "counts ignore the filter");
    let (_, other) = call(&app, Method::GET, "/api/v3/favorites", Some(BOB), None).await;
    assert_eq!(
        other["items"],
        json!([]),
        "favorites never leak across users"
    );
    let (status, _) = call(
        &app,
        Method::PUT,
        "/api/v3/favorites/genre/rock",
        Some(ADA),
        Some(json!({"favorited": true})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn follow_auto_download_states_and_admin_reads() {
    let rig = Rig::open().await;
    let app = rig.app();

    let (status, _) = call(
        &app,
        Method::PUT,
        "/api/v3/artists/mb-x/auto-download",
        Some(ADA),
        Some(json!({"enabled": true})),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "auto-download needs a follow");

    for (identity, mbid) in [
        (ADA, "mb-a"),
        (BOB, "mb-b1"),
        (BOB, "mb-b2"),
        (TRUSTED, "mb-a"),
    ] {
        call(
            &app,
            Method::PUT,
            &format!("/api/v3/artists/{mbid}/follow"),
            Some(identity),
            Some(json!({"followed": true, "artist_name": mbid})),
        )
        .await;
        let (_, body) = call(
            &app,
            Method::PUT,
            &format!("/api/v3/artists/{mbid}/auto-download"),
            Some(identity),
            Some(json!({"enabled": true})),
        )
        .await;
        let want = if identity == TRUSTED {
            "approved"
        } else {
            "pending"
        };
        assert_eq!(body["auto_download_state"], want, "{identity:?} {mbid}");
    }

    for uri in [
        "/api/v3/requests/auto-download-approvals",
        "/api/v3/requests/auto-download-approval-batches",
    ] {
        let (status, _) = call(&app, Method::GET, uri, Some(ADA), None).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
    }
    let (_, approvals) = call(
        &app,
        Method::GET,
        "/api/v3/requests/auto-download-approvals",
        Some(ADMIN),
        None,
    )
    .await;
    assert_eq!(approvals["count"], 3, "self-approving roles never queue");
    let (_, batches) = call(
        &app,
        Method::GET,
        "/api/v3/requests/auto-download-approval-batches",
        Some(ADMIN),
        None,
    )
    .await;
    assert_eq!(batches["count"], 0, "single asks are not import batches");

    let (_, off) = call(
        &app,
        Method::PUT,
        "/api/v3/artists/mb-a/auto-download",
        Some(ADA),
        Some(json!({"enabled": false})),
    )
    .await;
    assert_eq!(off["auto_download_state"], "none");
    let (_, approvals) = call(
        &app,
        Method::GET,
        "/api/v3/requests/auto-download-approvals",
        Some(ADMIN),
        None,
    )
    .await;
    assert_eq!(approvals["count"], 2, "turning it off withdraws the ask");
    let (_, list) = call(
        &app,
        Method::GET,
        "/api/v3/following/artists",
        Some(BOB),
        None,
    )
    .await;
    assert_eq!(
        list["artists"].as_array().unwrap().len(),
        2,
        "follows stay per user"
    );
}

#[tokio::test]
async fn remote_playlist_import_lands_in_the_users_playlists() {
    use droppedneedle_catalog::reads::collections::remote_import::PlaylistImportSink;
    use droppedneedle_media::remotes::adapter::ImportSink as _;
    use droppedneedle_media::remotes::models::{SourceName, TrackView};

    let rig = Rig::open().await;
    let sink = PlaylistImportSink::new(rig.state());
    let track = |id: &str, part: Option<&str>| TrackView {
        source: SourceName::Plex,
        id: id.to_owned(),
        title: format!("Song {id}"),
        album_name: "Album".to_owned(),
        album_id: None,
        artist_name: "Artist".to_owned(),
        artist_id: None,
        track_number: Some(1),
        disc_number: Some(1),
        duration_secs: Some(200),
        year: None,
        recording_mbid: None,
        image_url: None,
        part_key: part.map(str::to_owned),
    };
    let tracks = vec![track("1", Some("/library/parts/1")), track("2", None)];

    let first = sink
        .import(
            "u-ada",
            SourceName::Plex,
            "pl9",
            "Road Trip",
            tracks.clone(),
        )
        .await
        .unwrap();
    assert_eq!((first.tracks_imported, first.tracks_failed), (1, 1));
    let again = sink
        .import("u-ada", SourceName::Plex, "pl9", "Road Trip", tracks)
        .await
        .unwrap();
    assert!(again.already_imported);
    assert_eq!(again.local_playlist_id, first.local_playlist_id);

    let (_, detail) = call(
        &rig.app(),
        Method::GET,
        &format!("/api/v3/playlists/{}", first.local_playlist_id),
        Some(ADA),
        None,
    )
    .await;
    assert_eq!(detail["source_ref"], "plex:pl9");
    assert_eq!(detail["tracks"][0]["track_source_id"], "/library/parts/1");
    assert_eq!(detail["tracks"][0]["plex_rating_key"], "1");
}

#[tokio::test]
async fn store_faults_render_fixed_500_envelopes() {
    let scope = TestApp::start("reads-collections-faults").await;
    let app = scope.request_scope(
        collections::routes()
            .with_state(CollectionsState::unwired())
            .into_router(),
    );
    for uri in [
        "/api/v3/playlists",
        "/api/v3/favorites",
        "/api/v3/following/artists",
    ] {
        let (status, headers, bytes) =
            send(&app, Some(ADMIN), request(Method::GET, uri, None)).await;
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR, "{uri}");
        let body: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(body["error"]["message"], "Internal server error");
        assert_eq!(
            body["error"]["details"]["error_id"],
            headers["x-request-id"].to_str().unwrap()
        );
        let raw = String::from_utf8(bytes).unwrap();
        assert!(!raw.contains("wired"), "{raw}");
    }
}

/// Migration 0010 upgrades an older database in place: playlists and
/// favorites written before it stay, covers and favorite names get their
/// tables, and applying it again changes nothing.
#[tokio::test]
async fn migration_0010_upgrades_in_place() {
    use droppedneedle_kernel::schema::{MIGRATOR, apply_migrations};

    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    let through = sqlx::migrate::Migrator {
        migrations: std::borrow::Cow::Owned(
            MIGRATOR
                .migrations
                .iter()
                .filter(|migration| migration.version < 10)
                .cloned()
                .collect(),
        ),
        ignore_missing: false,
        locking: true,
        no_tx: false,
    };
    through.run(&pool).await.unwrap();
    sqlx::query(
        "INSERT INTO playlists (id, name, created_at, updated_at, user_id) \
         VALUES ('p1', 'Kept', '2024-01-01T00:00:00+00:00', '2024-01-01T00:00:00+00:00', 'u1'); \
         INSERT INTO library_user_favorites (user_id, item_kind, item_id, created_at) \
         VALUES ('u1', 'album', 'a1', 1.0);",
    )
    .execute(&pool)
    .await
    .unwrap();

    apply_migrations(&pool).await.unwrap();
    let kept: (String, String) = sqlx::query_as(
        "SELECT p.name, f.item_id FROM playlists p, library_user_favorites f WHERE p.id = 'p1'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(kept, ("Kept".to_owned(), "a1".to_owned()));
    let migration = MIGRATOR
        .migrations
        .iter()
        .find(|migration| migration.version == 10)
        .unwrap();
    sqlx::raw_sql(&migration.sql)
        .execute(&pool)
        .await
        .expect("0010 applies twice");
    let covers: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM playlist_covers")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(covers, 0);
}
