//! Collections routes (playlists, favorites, follows, approvals, edition
//! pins) over a scratch SQLite database, signed in with the test principal
//! layer: private playlists redact for other users, rows never leak across
//! users, everything survives a restart, pins never write identity, and
//! store faults stay hidden.

use crate::common::Probe;
use droppedneedle_acquire::db::AcquireDb;
use droppedneedle_acquire::follows::{self, Follows};
use droppedneedle_auth::Role;
use droppedneedle_auth::session::store::SessionKind;
use droppedneedle_catalog::collections::{self, CollectionsState, db::CollectionsDb};
use droppedneedle_kernel::db::{DbConfig, DbRuntime, LaneLimits, TEST_PATIENCE, open_runtime};

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
        // Everyone chose their display name but Quiet, whom the user
        // directory does not list.
        let rows = [
            ("u-ada", "Ada", "user", 1),
            ("u-bob", "Bob", "user", 1),
            ("u-tris", "Tris", "trusted", 1),
            ("u-root", "Root", "admin", 1),
            ("u-quiet", "quiet", "user", 0),
        ];
        runtime
            .db()
            .write(
                droppedneedle_kernel::db::Lane::Foreground,
                "seed users",
                move |tx| {
                    for (id, name, role, chosen) in rows {
                        tx.execute(
                            "INSERT INTO auth_users (id, display_name, role, created_at, \
                             display_name_chosen) VALUES (?1, ?2, ?3, 0, ?4)",
                            rusqlite::params![id, name, role, chosen],
                        )?;
                    }
                    Ok::<_, droppedneedle_kernel::db::DbError>(())
                },
            )
            .await
            .unwrap();
        Self { dir, runtime }
    }

    /// The library album `alb-1` and its track `trk-9`, which favourites
    /// need: a heart on an id no library item holds is refused.
    async fn seed_library(&self) {
        self.runtime
            .db()
            .write(
                droppedneedle_kernel::db::Lane::Foreground,
                "seed library",
                |tx| {
                    tx.execute_batch(
                        "INSERT INTO local_artists (id, display_name, folded_name, kind, \
                         created_at, updated_at) VALUES ('art-1', 'Aurora', 'aurora', 'group', 1, 1); \
                         INSERT INTO local_albums (id, root_id, grouping_key, title, title_folded, \
                         album_artist_id, grouping_source, created_at, updated_at) VALUES \
                         ('alb-1', 'r1', 'g1', 'First Light', 'first light', 'art-1', 'automatic', 1, 1); \
                         INSERT INTO local_tracks (id, local_album_id, root_id, file_path, \
                         relative_path, path_hash, file_size_bytes, file_mtime_ns, stat_revision, \
                         title, title_folded, album_title, album_title_folded, file_format, \
                         availability, ingest_source, imported_at, membership_source) VALUES \
                         ('trk-9', 'alb-1', 'r1', '/m/t9.flac', 't9.flac', 'h9', 1, 1, 's9', 'Opener', \
                         'opener', 'First Light', 'first light', 'flac', 'indexed', 'scan', 1, \
                         'automatic');",
                    )?;
                    Ok::<_, droppedneedle_kernel::db::DbError>(())
                },
            )
            .await
            .unwrap();
    }

    /// Open the database with patient limits, as `scratch_runtime` does:
    /// a loaded host slows a read or write instead of answering busy, which
    /// the fault test would read as a wrong status.
    async fn boot(dir: &ScratchDir) -> DbRuntime {
        let config = DbConfig::new(
            &dir.join("app.db"),
            std::sync::Arc::new(droppedneedle_kernel::clock::SystemClock),
        )
        .with_lane_limits(LaneLimits::patient())
        .with_read_wait(TEST_PATIENCE);
        let deadline = std::time::Instant::now() + crate::common::PATIENCE;
        loop {
            match open_runtime(&config).await {
                Ok(runtime) => return runtime,
                Err(error)
                    if crate::common::busy_open(&error) && std::time::Instant::now() < deadline =>
                {
                    tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                }
                Err(error) => panic!("database opens: {error}"),
            }
        }
    }

    fn state(&self) -> CollectionsState {
        CollectionsState::new(
            CollectionsDb::new(
                self.runtime.db().clone(),
                std::sync::Arc::new(droppedneedle_kernel::clock::SystemClock),
            ),
            droppedneedle_catalog::local::store::LocalCatalog::new(self.runtime.db().clone()),
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

/// Others read a private playlist's list row redacted, admins included;
/// once public it opens to them in full and refuses their writes with the
/// `FORBIDDEN` code. The per-id statuses are rows of the ownership table.
#[tokio::test]
async fn playlist_auth_matrix() {
    let rig = Rig::open().await;
    let app = rig.app();

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

/// Sharing opens a private playlist to the chosen people and no one else.
/// A member reads it in full and changes nothing: its songs, name,
/// visibility and members stay the owner's. Only other users the user
/// directory lists can be chosen, and a member may leave.
#[tokio::test]
async fn sharing_opens_a_playlist_to_chosen_people_only() {
    let rig = Rig::open().await;
    let app = rig.app();
    let (_, created) = call(
        &app,
        Method::POST,
        "/api/v3/playlists",
        Some(ADA),
        Some(json!({"name": "Road Trip"})),
    )
    .await;
    let id = created["id"].as_str().unwrap().to_owned();
    let one = format!("/api/v3/playlists/{id}");
    let members = format!("{one}/members");
    call(
        &app,
        Method::POST,
        &format!("{one}/tracks"),
        Some(ADA),
        Some(json!({"tracks": [track("One")]})),
    )
    .await;
    let (status, stub) = call(&app, Method::GET, &one, Some(BOB), None).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "private until shared");
    assert!(stub.get("tracks").is_none(), "{stub}");

    let (status, shared) = call(
        &app,
        Method::PUT,
        &members,
        Some(ADA),
        Some(json!({"user_ids": ["u-bob", "u-bob"]})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{shared}");
    assert_eq!(
        shared["members"],
        json!([{"user_id": "u-bob", "display_name": "Bob", "can_edit": false}])
    );

    let (status, full) = call(&app, Method::GET, &one, Some(BOB), None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        (
            full["is_owner"].as_bool(),
            full["is_shared"].as_bool(),
            full["tracks"].as_array().map(Vec::len)
        ),
        (Some(false), Some(false), Some(1)),
        "a member reads it in full, and not whom else it is shared with"
    );
    let (_, owned) = call(&app, Method::GET, &one, Some(ADA), None).await;
    assert_eq!(owned["is_shared"], true);
    let (_, list) = call(&app, Method::GET, "/api/v3/playlists", Some(BOB), None).await;
    assert_eq!(list["playlists"][0]["name"], "Road Trip", "listed in full");
    let (status, _) = call(&app, Method::GET, &one, Some(TRUSTED), None).await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "still private to everyone else"
    );

    for (method, uri, body) in [
        (Method::GET, members.clone(), None),
        (Method::PUT, members.clone(), Some(json!({"user_ids": []}))),
        (Method::PUT, one.clone(), Some(json!({"name": "Mine now"}))),
        (
            Method::PATCH,
            format!("{one}/visibility"),
            Some(json!({"is_public": true})),
        ),
        (
            Method::POST,
            format!("{one}/tracks"),
            Some(json!({"tracks": [track("Two")]})),
        ),
        (Method::DELETE, one.clone(), None),
    ] {
        let (status, _) = call(&app, method.clone(), &uri, Some(BOB), body).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{method} {uri}");
    }
    for user_ids in [
        json!(["u-ada"]),
        json!(["u-bob", "nobody"]),
        json!(["u-bob", "u-quiet"]),
    ] {
        let (status, _) = call(
            &app,
            Method::PUT,
            &members,
            Some(ADA),
            Some(json!({ "user_ids": user_ids })),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{user_ids}");
    }
    let (_, kept) = call(&app, Method::GET, &members, Some(ADA), None).await;
    assert_eq!(
        kept["members"].as_array().map(Vec::len),
        Some(1),
        "unchanged"
    );
    let (_, unchanged) = call(&app, Method::GET, &one, Some(ADA), None).await;
    assert_eq!(
        (
            unchanged["name"].as_str(),
            unchanged["is_public"].as_bool(),
            unchanged["tracks"].as_array().map(Vec::len)
        ),
        (Some("Road Trip"), Some(false), Some(1))
    );

    let leave = format!("{members}/me");
    let (status, _) = call(&app, Method::DELETE, &leave, Some(BOB), None).await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = call(&app, Method::GET, &one, Some(BOB), None).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "private again once left");
    let (status, _) = call(&app, Method::DELETE, &leave, Some(BOB), None).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "not shared any more");
}

/// A member the owner lets edit adds, reorders, re-sources and removes
/// songs; a member who may not edit changes nothing. Name, visibility,
/// cover, members and delete stay the owner's, though a name sent back as
/// it is (as apps do with every save) is no change. Taking the right away
/// takes the edits with it.
#[tokio::test]
async fn editors_change_songs_and_only_songs() {
    let rig = Rig::open().await;
    let app = rig.app();
    let (_, created) = call(
        &app,
        Method::POST,
        "/api/v3/playlists",
        Some(ADA),
        Some(json!({"name": "Road Trip"})),
    )
    .await;
    let id = created["id"].as_str().unwrap().to_owned();
    let one = format!("/api/v3/playlists/{id}");
    let members = format!("{one}/members");
    let tracks = format!("{one}/tracks");
    call(
        &app,
        Method::POST,
        &tracks,
        Some(ADA),
        Some(json!({"tracks": [track("One")]})),
    )
    .await;
    let (status, shared) = call(
        &app,
        Method::PUT,
        &members,
        Some(ADA),
        Some(json!({"user_ids": ["u-bob", "u-tris"], "editor_ids": ["u-bob"]})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{shared}");
    assert_eq!(
        shared["members"],
        json!([
            {"user_id": "u-bob", "display_name": "Bob", "can_edit": true},
            {"user_id": "u-tris", "display_name": "Tris", "can_edit": false}
        ])
    );
    for (who, can_edit) in [(BOB, true), (TRUSTED, false)] {
        let (_, read) = call(&app, Method::GET, &one, Some(who), None).await;
        assert_eq!(read["can_edit"], can_edit, "{}", who.0);
    }

    let (status, added) = call(
        &app,
        Method::POST,
        &tracks,
        Some(BOB),
        Some(json!({"tracks": [track("Two")]})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{added}");
    let two = added["tracks"][0]["id"].as_str().unwrap().to_owned();
    for (method, uri, body) in [
        (
            Method::PATCH,
            format!("{tracks}/reorder"),
            Some(json!({"track_id": two, "new_position": 0})),
        ),
        (
            Method::PATCH,
            format!("{tracks}/{two}"),
            Some(json!({"source_type": "local"})),
        ),
        (Method::PUT, one.clone(), Some(json!({"name": "Road Trip"}))),
        (Method::DELETE, format!("{tracks}/{two}"), None),
    ] {
        let (status, body) = call(&app, method.clone(), &uri, Some(BOB), body).await;
        assert_eq!(status, StatusCode::OK, "{method} {uri}: {body}");
    }
    for (method, uri, body) in [
        (Method::PUT, one.clone(), Some(json!({"name": "Bob's now"}))),
        (
            Method::PATCH,
            format!("{one}/visibility"),
            Some(json!({"is_public": true})),
        ),
        (Method::PUT, members.clone(), Some(json!({"user_ids": []}))),
        (
            Method::POST,
            format!("{one}/cover"),
            Some(json!({"content_type": "image/png", "image_base64": PIXEL_PNG})),
        ),
        (Method::DELETE, one.clone(), None),
    ] {
        let (status, _) = call(&app, method.clone(), &uri, Some(BOB), body).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{method} {uri}");
    }
    let (status, _) = call(
        &app,
        Method::POST,
        &tracks,
        Some(TRUSTED),
        Some(json!({"tracks": [track("Three")]})),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "a member who may not edit");

    let (status, _) = call(
        &app,
        Method::PUT,
        &members,
        Some(ADA),
        Some(json!({"user_ids": ["u-bob"], "editor_ids": ["u-tris"]})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "editors are members");
    call(
        &app,
        Method::PUT,
        &members,
        Some(ADA),
        Some(json!({"user_ids": ["u-bob"]})),
    )
    .await;
    let (status, _) = call(
        &app,
        Method::POST,
        &tracks,
        Some(BOB),
        Some(json!({"tracks": [track("Four")]})),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "the right went with it");
    let (_, after) = call(&app, Method::GET, &one, Some(ADA), None).await;
    assert_eq!(
        (
            after["name"].as_str(),
            after["is_public"].as_bool(),
            after["tracks"].as_array().map(Vec::len)
        ),
        (Some("Road Trip"), Some(false), Some(1))
    );
}

/// The store itself refuses a write to another user's playlist: Bob's
/// handle renames, publishes, adds to, reorders, re-sources, covers,
/// uncovers, empties and deletes Ada's playlist, and nothing changes.
#[tokio::test]
async fn cross_user_writes_are_refused_by_the_store() {
    use droppedneedle_catalog::collections::store::playlists::{
        CoverImage, NewEntry, PlaylistStore, Written,
    };

    let rig = Rig::open().await;
    let store = PlaylistStore::new(CollectionsDb::new(
        rig.runtime.db().clone(),
        std::sync::Arc::new(droppedneedle_kernel::clock::SystemClock),
    ));
    let entry = |name: &str| NewEntry {
        track_name: name.to_owned(),
        artist_name: "Art".to_owned(),
        album_name: "Alb".to_owned(),
        ..NewEntry::default()
    };
    let cover = || CoverImage {
        content_type: "image/png".to_owned(),
        bytes: vec![1, 2, 3],
    };
    let ada = store.owned_by(ADA.0);
    let (id, _) = ada
        .create_with("Mine", vec![entry("One"), entry("Two")])
        .await
        .unwrap();
    assert_eq!(
        ada.set_cover(&id, cover()).await.unwrap(),
        Written::Done(())
    );
    let snapshot = || async {
        let entries = ada
            .tracks(&id)
            .await
            .unwrap()
            .into_iter()
            .map(|track| {
                (
                    track.id,
                    track.position,
                    track.track_name,
                    track.source_type,
                )
            })
            .collect::<Vec<_>>();
        (ada.get(&id).await.unwrap(), entries)
    };
    let before = snapshot().await;
    let first = before.1[0].0.clone();

    let bob = store.owned_by(BOB.0);
    let bob_songs = store.editable_by(BOB.0);
    assert_eq!(bob.rename(&id, "Taken").await.unwrap(), Written::Missing);
    assert_eq!(bob.set_public(&id, true).await.unwrap(), Written::Missing);
    assert_eq!(
        bob_songs
            .insert(&id, Some(0), vec![entry("Three")])
            .await
            .unwrap(),
        Written::Missing
    );
    assert_eq!(
        bob_songs.move_entry(&id, &first, 1).await.unwrap(),
        Written::Missing
    );
    assert_eq!(
        bob_songs
            .update_sources(&id, &first, Some("plex".to_owned()), None)
            .await
            .unwrap(),
        Written::Missing
    );
    assert_eq!(bob.set_cover(&id, cover()).await.unwrap(), Written::Missing);
    assert_eq!(bob.remove_cover(&id).await.unwrap(), Written::Missing);
    assert_eq!(
        bob_songs
            .remove(&id, std::slice::from_ref(&first))
            .await
            .unwrap(),
        Written::Missing
    );
    assert!(matches!(
        bob_songs
            .replace_entries(&id, vec![entry("Four")])
            .await
            .unwrap(),
        Written::Missing
    ));
    assert_eq!(bob.delete(&id).await.unwrap(), Written::Missing);
    assert_eq!(bob.get(&id).await.unwrap(), None);
    assert!(bob.tracks(&id).await.unwrap().is_empty());
    assert!(bob.cover(&id).await.unwrap().is_none());

    assert_eq!(
        snapshot().await,
        before,
        "nothing about the playlist changed"
    );
    assert_eq!(
        ada.cover(&id).await.unwrap().map(|cover| cover.bytes),
        Some(vec![1, 2, 3])
    );
}

#[tokio::test]
async fn playlists_favorites_and_follows_survive_a_restart() {
    let rig = Rig::open().await;
    rig.seed_library().await;
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
    assert_eq!(headers.get("cache-control").unwrap(), "private");
    assert!(!bytes.is_empty());

    let kept: Vec<String> = rig.runtime.db().column(
        "SELECT item_id FROM catalog_user_favorites WHERE user_id = 'u-ada'",
        [],
    );
    assert_eq!(kept, ["alb-1"]);
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

    // A cover between the server's default body cap and the 5 MiB the
    // service takes is accepted.
    let three_mb = json!({"image_base64": "A".repeat(4_000_000), "content_type": "image/png"});
    let (status, _) = call(
        &app,
        Method::POST,
        &format!("/api/v3/playlists/{id}/cover"),
        Some(ADA),
        Some(three_mb),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "a 3 MB cover is taken");

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

/// Readers of a public playlist load only covers from DroppedNeedle's own
/// routes or known art hosts: any other address is dropped as the entry is
/// added, and a stored one reads as none.
#[tokio::test]
async fn playlist_entry_covers_only_from_known_addresses() {
    const TRACKER: &str = "https://tracker.example/pixel.png?viewer=1";
    let mbid = "8a7b6c5d-1234-4abc-9def-0123456789ab";
    let rig = Rig::open().await;
    let app = rig.app();
    let (_, created) = call(
        &app,
        Method::POST,
        "/api/v3/playlists",
        Some(ADA),
        Some(json!({"name": "Covers"})),
    )
    .await;
    let id = created["id"].as_str().unwrap().to_owned();
    let one = format!("/api/v3/playlists/{id}");
    let (status, _) = call(
        &app,
        Method::PATCH,
        &format!("{one}/visibility"),
        Some(ADA),
        Some(json!({"is_public": true})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let seeded = id.clone();
    rig.runtime
        .db()
        .write(
            droppedneedle_kernel::db::Lane::Foreground,
            "seed old entry",
            move |tx| {
                tx.execute(
                    "INSERT INTO catalog_playlist_tracks (id, playlist_id, position, \
                     track_name, artist_name, album_name, cover_url, source_type, created_at) \
                     VALUES ('old-entry', ?1, 0, 'Old', 'A', 'B', ?2, 'local', 0)",
                    rusqlite::params![seeded, TRACKER],
                )?;
                Ok::<_, droppedneedle_kernel::db::DbError>(())
            },
        )
        .await
        .unwrap();

    let tracks: Vec<Value> = [
        TRACKER.to_owned(),
        format!("/music/api/v3/covers/release-group/{mbid}?size=250"),
        "https://i.scdn.co/image/ab".to_owned(),
    ]
    .into_iter()
    .map(|cover| json!({"track_name": "T", "artist_name": "A", "album_name": "B", "cover_url": cover}))
    .collect();
    let (status, _) = call(
        &app,
        Method::POST,
        &format!("{one}/tracks"),
        Some(ADA),
        Some(json!({"tracks": tracks})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let kept = [
        format!("/api/v3/covers/release-group/{mbid}?size=250"),
        "https://i.scdn.co/image/ab".to_owned(),
    ];
    let (status, detail) = call(&app, Method::GET, &one, Some(BOB), None).await;
    assert_eq!(status, StatusCode::OK);
    let covers: Vec<Value> = detail["tracks"]
        .as_array()
        .unwrap()
        .iter()
        .map(|track| track["cover_url"].clone())
        .collect();
    assert_eq!(
        covers,
        vec![Value::Null, Value::Null, json!(kept[0]), json!(kept[1])]
    );
    assert_eq!(detail["cover_urls"], json!(kept));
    let (_, list) = call(&app, Method::GET, "/api/v3/playlists", Some(BOB), None).await;
    assert_eq!(list["playlists"][0]["cover_urls"], json!(kept));
}

#[tokio::test]
async fn favorites_stay_per_user() {
    let rig = Rig::open().await;
    rig.seed_library().await;
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
    let bobs: i64 = rig.runtime.db().one(
        "SELECT COUNT(*) FROM catalog_user_favorites WHERE user_id = 'u-bob'",
        [],
    );
    assert_eq!(bobs, 0, "favorites never leak across users");
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
    use droppedneedle_catalog::collections::remote_import::PlaylistImportSink;
    use droppedneedle_media::remotes::models::{SourceName, TrackView};
    use droppedneedle_media::remotes::source::ImportSink as _;

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
        format: None,
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

/// An imported entry with no source yet links to the library's copy once
/// its album is in (#759): resolving sources points it at the indexed track
/// at its disc and track number on an album owning its release group, when
/// the titles agree, else at the track titled the same elsewhere on that
/// album. Entries with a source, a disagreeing title or an album the
/// library lacks stay as they are, and a reader of a public playlist links
/// nothing.
#[tokio::test]
async fn resolving_sources_links_imported_entries_to_the_library() {
    const GROUP: &str = "0a0b0c0d-0001-4000-8000-000000000001";
    const MISSING: &str = "0a0b0c0d-0001-4000-8000-000000000002";
    let rig = Rig::open().await;
    rig.runtime
        .db()
        .write(
            droppedneedle_kernel::db::Lane::Foreground,
            "seed owned album",
            |tx| {
                let mut sql = "INSERT INTO local_artists (id, display_name, folded_name, kind, \
                     created_at, updated_at) VALUES ('art-a', 'Aurora', 'aurora', 'group', 1, 1); \
                     INSERT INTO local_albums (id, root_id, grouping_key, title, title_folded, \
                     album_artist_id, grouping_source, created_at, updated_at) VALUES \
                     ('alb-a', 'r1', 'ga', 'First Light', 'first light', 'art-a', \
                     'automatic', 1, 1); \
                     INSERT INTO local_album_external_identities (local_album_id, provider, \
                     release_group_mbid, decision_source, selected_at) VALUES \
                     ('alb-a', 'musicbrainz', '"
                    .to_owned()
                    + GROUP
                    + "', 'automatic', 1);";
                for (n, title) in [(1, "Opener"), (2, "Second Song")] {
                    sql.push_str(&format!(
                        "INSERT INTO local_tracks (id, local_album_id, root_id, file_path, \
                         relative_path, path_hash, file_size_bytes, file_mtime_ns, stat_revision, \
                         title, title_folded, album_title, album_title_folded, disc_number, \
                         track_number, file_format, availability, ingest_source, imported_at, \
                         membership_source) VALUES ('trk-a{n}', 'alb-a', 'r1', '/m/a{n}.flac', \
                         'a{n}.flac', 'ha{n}', 1, 1, 'sa{n}', '{title}', '{title}', \
                         'First Light', 'first light', 1, {n}, 'flac', 'indexed', \
                         'scan', 1, 'automatic');"
                    ));
                }
                tx.execute_batch(&sql)?;
                Ok::<_, droppedneedle_kernel::db::DbError>(())
            },
        )
        .await
        .unwrap();
    let app = rig.app();
    let (_, created) = call(
        &app,
        Method::POST,
        "/api/v3/playlists",
        Some(ADA),
        Some(json!({"name": "Road Trip"})),
    )
    .await;
    let id = created["id"].as_str().unwrap().to_owned();
    let entry = |name: &str, album_id: &str, track_number: i32| {
        json!({"track_name": name, "artist_name": "Aurora", "album_name": "First Light",
               "album_id": album_id, "source_type": "", "track_number": track_number})
    };
    let mut sourced = entry("Opener", GROUP, 1);
    sourced["source_type"] = json!("youtube");
    sourced["track_source_id"] = json!("yt-1");
    let (status, _) = call(
        &app,
        Method::POST,
        &format!("/api/v3/playlists/{id}/tracks"),
        Some(ADA),
        Some(json!({"tracks": [
            entry("Opener", GROUP, 1),
            entry("Nightfall", GROUP, 2),
            sourced,
            entry("Opener", MISSING, 1),
            entry("Second Song", GROUP, 7),
        ]})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let resolve = format!("/api/v3/playlists/{id}/resolve-sources");
    let one = format!("/api/v3/playlists/{id}");
    let linked = |detail: &Value| {
        detail["tracks"]
            .as_array()
            .unwrap()
            .iter()
            .map(|track| {
                (
                    track["source_type"].as_str().unwrap().to_owned(),
                    track["library_file_id"].as_str().map(str::to_owned),
                )
            })
            .collect::<Vec<_>>()
    };

    call(
        &app,
        Method::PATCH,
        &format!("{one}/visibility"),
        Some(ADA),
        Some(json!({"is_public": true})),
    )
    .await;
    let (status, _) = call(&app, Method::POST, &resolve, Some(BOB), None).await;
    assert_eq!(status, StatusCode::OK);
    let (_, detail) = call(&app, Method::GET, &one, Some(ADA), None).await;
    assert!(
        linked(&detail).iter().all(|(_, file)| file.is_none()),
        "a reader links nothing"
    );

    let (status, answer) = call(&app, Method::POST, &resolve, Some(ADA), None).await;
    assert_eq!(status, StatusCode::OK);
    let (_, detail) = call(&app, Method::GET, &one, Some(ADA), None).await;
    let first = detail["tracks"][0]["id"].as_str().unwrap();
    assert_eq!(answer["sources"][first], json!(["local"]));
    assert_eq!(detail["tracks"][0]["track_source_id"], "trk-a1");
    assert_eq!(
        linked(&detail),
        [
            ("local".to_owned(), Some("trk-a1".to_owned())),
            (String::new(), None),
            ("youtube".to_owned(), None),
            (String::new(), None),
            ("local".to_owned(), Some("trk-a2".to_owned())),
        ]
    );
}

#[tokio::test]
async fn store_faults_render_fixed_500_envelopes() {
    let rig = Rig::open().await;
    // Every read below fails inside SQLite on a table that is not there.
    crate::common::break_schema(
        rig.runtime.db(),
        "ALTER TABLE catalog_playlists RENAME TO gone_playlists; \
         ALTER TABLE catalog_user_favorites RENAME TO gone_favorites; \
         ALTER TABLE acquire_followed_artists RENAME TO gone_follows;",
    )
    .await;
    let scope = TestApp::start("reads-collections-faults").await;
    let app = scope.request_scope(collections::routes().with_state(rig.state()).into_router());
    for uri in ["/api/v3/playlists", "/api/v3/following/artists"] {
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
        assert!(!raw.contains("table"), "{raw}");
    }
}
