//! Remote media servers (Jellyfin, Navidrome, Plex) through the real
//! adapters and routes against the in-repo loopback mocks: paging wire
//! shapes, auth mapping, folder and section scoping that fails closed,
//! sealed credentials that never echo, and the per-source feature matrix.

use droppedneedle_media::remotes;

use crate::common::auth::{test_principal, with_test_principal};

use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use droppedneedle_auth::users::roles::{Role, SessionKind};
use droppedneedle_catalog::reads::collections::remote_import::PlaylistImportSink;
use droppedneedle_kernel::platform::Platform;
use droppedneedle_kernel::runtime_config::Secret;
use droppedneedle_kernel::runtime_config::secret_sections::{
    JellyfinConnection, NavidromeConnection, PlexConnection,
};
use droppedneedle_testkit::remotes::{
    JELLYFIN_KEY, MATCH_MBID, MockAlbum, MockOptions, NAVIDROME_USER, PLEX_TOKEN, serve_jellyfin,
    serve_navidrome, serve_navidrome_with, serve_plex, serve_plex_with,
};
use remotes::adapter::{AdapterError, AlbumBrowse, ArtistBrowse, RemoteHandle, TrackBrowse};
use remotes::connections::{ConnectionResolver, ResolveError, UserLink};
use remotes::folders::{FolderPreference, SqliteFolderStore, resolve_scope};
use remotes::handlers::{self as remotes_handlers, RemotesDeps};
use remotes::jellyfin::JellyfinAdapter;
use remotes::models::SourceName;
use remotes::navidrome::NavidromeAdapter;
use remotes::plex::PlexAdapter;
use remotes::service::RemotesService;
use remotes::warmup::{MbidIndex, Warmups};
use serde_json::Value;
use tower::ServiceExt as _;

fn http_client() -> reqwest::Client {
    reqwest::Client::new()
}

/// The signed-in caller of the route tests.
const USER_ID: &str = "user-ada";

/// The remotes stores the app builds, on a scratch copy of the migrated
/// database holding the signed-in user's row. Admin servers are saved
/// settings sections, read live as in the app.
struct Rig {
    platform: Platform,
    runtime: droppedneedle_kernel::db::DbRuntime,
    index: MbidIndex,
    _dir: crate::common::ScratchDir,
}

impl Rig {
    async fn open(tag: &str) -> Self {
        let dir = crate::common::ScratchDir::new(tag);
        let runtime = crate::common::scratch_runtime(&dir.join("app.db")).await;
        runtime
            .db()
            .write(droppedneedle_kernel::db::Lane::Foreground, "seed", |tx| {
                tx.execute(
                    "INSERT INTO auth_users (id, username, display_name, role, created_at) \
                     VALUES (?1, 'ada', 'Ada', 'user', '2026-01-01T00:00:00Z')",
                    [USER_ID],
                )?;
                Ok::<_, droppedneedle_kernel::db::DbError>(())
            })
            .await
            .expect("user seeds");
        let platform = crate::common::TestPlatform::on(runtime.db(), &dir)
            .build()
            .await;
        Self {
            platform,
            runtime,
            index: MbidIndex::default(),
            _dir: dir,
        }
    }

    /// Save the admin's Jellyfin server with its API key and user.
    async fn jellyfin(&self, base_url: &str, api_key: &str, user_id: &str) {
        self.platform
            .settings
            .save_secret(JellyfinConnection {
                jellyfin_url: base_url.to_owned(),
                api_key: Secret::new(api_key),
                user_id: user_id.to_owned(),
                enabled: true,
                ..JellyfinConnection::default()
            })
            .await
            .expect("jellyfin saves");
    }

    /// Save the admin's Navidrome server; an empty username shares no
    /// account.
    async fn navidrome(&self, base_url: &str, username: &str, password: &str) {
        self.platform
            .settings
            .save_secret(NavidromeConnection {
                navidrome_url: base_url.to_owned(),
                username: username.to_owned(),
                password: Secret::new(password),
                enabled: true,
                ..NavidromeConnection::default()
            })
            .await
            .expect("navidrome saves");
    }

    /// Save the admin's Plex server; an empty token shares no account.
    async fn plex(&self, base_url: &str, token: &str) {
        self.platform
            .settings
            .save_secret(PlexConnection {
                plex_url: base_url.to_owned(),
                plex_token: Secret::new(token),
                enabled: true,
                ..PlexConnection::default()
            })
            .await
            .expect("plex saves");
    }

    fn resolver(&self) -> ConnectionResolver {
        crate::common::bundles::remotes_resolver(&self.platform)
    }

    /// One warmup pass for `source`, filling the index the routes read.
    async fn warm(&self, source: SourceName) {
        Warmups::new(http_client(), Arc::new(self.resolver()), self.index.clone())
            .pass(source)
            .await
            .expect("warmup passes");
    }

    fn folders(&self) -> SqliteFolderStore {
        SqliteFolderStore::new(self.platform.db.clone(), self.platform.clock.clone())
    }

    /// Route deps over the real stores, imports landing in the user's
    /// playlists.
    fn deps(&self) -> RemotesDeps {
        RemotesDeps {
            service: RemotesService::new(
                http_client(),
                Arc::new(self.resolver()),
                Arc::new(self.folders()),
                Arc::new(PlaylistImportSink::new(
                    crate::common::bundles::collections_at(&self.platform).wired(),
                )),
                self.index.clone(),
                self.platform.clock.clone(),
            ),
        }
    }

    fn app(&self) -> Router {
        authed_app(self.deps())
    }
}

fn authed_app(deps: RemotesDeps) -> Router {
    with_test_principal(
        remotes_handlers::routes().with_state(deps).into_router(),
        test_principal(USER_ID, Role::User, "sess-1", SessionKind::Standard),
    )
}

async fn get_json(app: Router, path: &str) -> (StatusCode, Value) {
    let response = app
        .oneshot(
            Request::get(path)
                .body(Body::empty())
                .expect("request builds"),
        )
        .await
        .expect("router answers");
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("body reads");
    let json: Value = serde_json::from_slice(&bytes).expect("body is json");
    (status, json)
}

async fn post_empty(app: Router, path: &str) -> (StatusCode, Value) {
    let response = app
        .oneshot(
            Request::post(path)
                .body(Body::empty())
                .expect("request builds"),
        )
        .await
        .expect("router answers");
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("body reads");
    let json: Value = serde_json::from_slice(&bytes).expect("body is json");
    (status, json)
}

async fn put_json(app: Router, path: &str, payload: Value) -> (StatusCode, Value) {
    let response = app
        .oneshot(
            Request::put(path)
                .header("content-type", "application/json")
                .body(Body::from(payload.to_string()))
                .expect("request builds"),
        )
        .await
        .expect("router answers");
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("body reads");
    let json: Value = serde_json::from_slice(&bytes).expect("body is json");
    (status, json)
}

async fn delete_json(app: Router, path: &str) -> (StatusCode, Value) {
    let response = app
        .oneshot(
            Request::delete(path)
                .body(Body::empty())
                .expect("request builds"),
        )
        .await
        .expect("router answers");
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("body reads");
    let json: Value = serde_json::from_slice(&bytes).expect("body is json");
    (status, json)
}

async fn get_bytes(app: Router, path: &str) -> (StatusCode, Vec<(String, String)>, Vec<u8>) {
    let response = app
        .oneshot(
            Request::get(path)
                .body(Body::empty())
                .expect("request builds"),
        )
        .await
        .expect("router answers");
    let status = response.status();
    let headers = response
        .headers()
        .iter()
        .map(|(key, value)| (key.to_string(), value.to_str().unwrap_or("").to_owned()))
        .collect();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("body reads");
    (status, headers, bytes.to_vec())
}

fn error_code(body: &Value) -> &str {
    body.get("error")
        .and_then(|error| error.get("code"))
        .and_then(Value::as_str)
        .unwrap_or("<missing>")
}

fn jellyfin_handle(base_url: &str) -> RemoteHandle {
    RemoteHandle::Jellyfin(JellyfinAdapter::new(
        http_client(),
        base_url.to_owned(),
        JELLYFIN_KEY.to_owned(),
        "jf-user-1".to_owned(),
    ))
}

fn navidrome_handle(base_url: &str, folders: Option<Vec<String>>) -> RemoteHandle {
    RemoteHandle::Navidrome(
        NavidromeAdapter::new(
            http_client(),
            base_url.to_owned(),
            NAVIDROME_USER.to_owned(),
            "nd-pass".to_owned(),
        )
        .with_folders(folders),
    )
}

fn plex_handle(base_url: &str, sections: Vec<String>) -> RemoteHandle {
    RemoteHandle::Plex(PlexAdapter::new(
        http_client(),
        base_url.to_owned(),
        PLEX_TOKEN.to_owned(),
        String::new(),
        sections,
    ))
}

// ---------------------------------------------------------------------------
// Jellyfin
// ---------------------------------------------------------------------------

#[tokio::test]
async fn jellyfin_browse_pagination_uses_start_index_and_total() {
    let mock = serve_jellyfin().await.expect("mock serves");
    let handle = jellyfin_handle(&mock.base_url);
    let first = handle
        .albums(&AlbumBrowse {
            limit: 2,
            offset: 0,
            ..AlbumBrowse::default()
        })
        .await
        .expect("page one loads");
    assert_eq!(first.items.len(), 2);
    assert_eq!(first.total, 3);
    let second = handle
        .albums(&AlbumBrowse {
            limit: 2,
            offset: 2,
            ..AlbumBrowse::default()
        })
        .await
        .expect("page two loads");
    assert_eq!(second.items.len(), 1);
    assert_eq!(second.items[0].title, "Paper Satellites");

    let calls = mock.recorder.snapshot();
    assert!(calls.jellyfin_items_queries.iter().any(|query| {
        query.contains(&("startIndex".to_owned(), "2".to_owned()))
            && query.contains(&("limit".to_owned(), "2".to_owned()))
    }));
}

#[tokio::test]
async fn jellyfin_auth_rides_the_mediabrowser_header_only() {
    let mock = serve_jellyfin().await.expect("mock serves");
    let handle = jellyfin_handle(&mock.base_url);
    handle.stats().await.expect("authed call passes");
    for seen in &mock.recorder.snapshot().jellyfin_auth {
        assert_eq!(seen, &format!("MediaBrowser Token=\"{JELLYFIN_KEY}\""));
    }

    // The live 10.11.11 behavior: legacy Emby headers 401 the same key.
    let denied = http_client()
        .get(format!("{}/System/Info", mock.base_url))
        .header("X-Emby-Token", JELLYFIN_KEY)
        .send()
        .await
        .expect("mock answers");
    assert_eq!(denied.status(), StatusCode::UNAUTHORIZED);

    let wrong = JellyfinAdapter::new(
        http_client(),
        mock.base_url.clone(),
        "wrong".to_owned(),
        "jf-user-1".to_owned(),
    );
    assert_eq!(wrong.stats().await, Err(AdapterError::Auth));

    let probe = JellyfinAdapter::new(
        http_client(),
        mock.base_url.clone(),
        JELLYFIN_KEY.to_owned(),
        "jf-user-1".to_owned(),
    );
    let label = probe.validate_connection().await.expect("probe runs");
    assert!(label.contains("10.11.11"), "probe reports the live version");
}

// ---------------------------------------------------------------------------
// Navidrome
// ---------------------------------------------------------------------------

#[tokio::test]
async fn navidrome_folder_scoping_all_selected_and_empty() {
    let mock = serve_navidrome().await.expect("mock serves");
    let calls_before = mock.recorder.snapshot().navidrome_queries.len();

    let all = navidrome_handle(&mock.base_url, None);
    assert_eq!(
        all.artists(&ArtistBrowse::default())
            .await
            .expect("all loads")
            .total,
        2
    );
    let snapshot = mock.recorder.snapshot();
    let folders = snapshot
        .navidrome_folder_params
        .get("getArtists")
        .and_then(|calls| calls.last())
        .cloned()
        .unwrap_or_default();
    assert!(folders.is_empty(), "all-folders scope omits the param");

    let selected = navidrome_handle(
        &mock.base_url,
        Some(vec!["folder-1".to_owned(), "folder-2".to_owned()]),
    );
    assert_eq!(
        selected
            .artists(&ArtistBrowse::default())
            .await
            .expect("selected loads")
            .total,
        2
    );
    let folders = mock
        .recorder
        .snapshot()
        .navidrome_folder_params
        .get("getArtists")
        .and_then(|calls| calls.last())
        .cloned()
        .unwrap_or_default();
    assert_eq!(folders, vec!["folder-1".to_owned(), "folder-2".to_owned()]);

    let empty = navidrome_handle(&mock.base_url, Some(Vec::new()));
    assert!(
        empty
            .artists(&ArtistBrowse::default())
            .await
            .expect("empty is fine")
            .items
            .is_empty()
    );
    assert!(
        empty
            .search("x", 5)
            .await
            .expect("empty is fine")
            .albums
            .is_empty()
    );
    assert_eq!(
        mock.recorder.snapshot().navidrome_queries.len(),
        calls_before + 2,
        "empty scope fails closed without any request"
    );
}

#[tokio::test]
async fn navidrome_stats_scan_and_album_totals() {
    let mock = serve_navidrome().await.expect("mock serves");
    let handle = navidrome_handle(&mock.base_url, None);
    let stats = handle.stats().await.expect("stats load");
    assert_eq!(
        (stats.total_albums, stats.total_artists, stats.total_tracks),
        (3, 2, 4)
    );

    let full = handle
        .albums(&AlbumBrowse {
            limit: 2,
            offset: 0,
            ..AlbumBrowse::default()
        })
        .await
        .expect("page loads");
    assert_eq!(full.items.len(), 2);
    assert_eq!(full.total, 3, "full pages report the stats total");
    let tail = handle
        .albums(&AlbumBrowse {
            limit: 2,
            offset: 2,
            ..AlbumBrowse::default()
        })
        .await
        .expect("page loads");
    assert_eq!((tail.items.len(), tail.total), (1, 3));
    let queries = mock.recorder.snapshot().navidrome_queries;
    assert!(
        queries
            .iter()
            .any(|(endpoint, params)| endpoint == "getAlbumList2"
                && params.contains(&("size".to_owned(), "2".to_owned()))
                && params.contains(&("offset".to_owned(), "2".to_owned()))),
        "the second album page spells offset=2"
    );

    let songs = handle
        .tracks(&TrackBrowse {
            limit: 2,
            offset: 2,
            ..TrackBrowse::default()
        })
        .await
        .expect("songs page loads");
    assert_eq!(songs.items.len(), 2);
    let queries = mock.recorder.snapshot().navidrome_queries;
    assert!(
        queries
            .iter()
            .any(|(endpoint, params)| endpoint == "search3"
                && params.contains(&("songCount".to_owned(), "2".to_owned()))
                && params.contains(&("songOffset".to_owned(), "2".to_owned()))),
        "the second songs page spells songOffset=2"
    );
}

#[tokio::test]
async fn navidrome_subsonic_auth_codes_map_to_auth() {
    let mock = serve_navidrome().await.expect("mock serves");
    let wrong = NavidromeAdapter::new(
        http_client(),
        mock.base_url.clone(),
        "intruder".to_owned(),
        "nope".to_owned(),
    );
    assert_eq!(wrong.stats().await, Err(AdapterError::Auth));

    let probe = NavidromeAdapter::new(
        http_client(),
        mock.base_url.clone(),
        NAVIDROME_USER.to_owned(),
        "nd-pass".to_owned(),
    );
    let label = probe.validate_connection().await.expect("probe runs");
    assert!(label.contains("1.16.1"), "probe reports the API version");
    assert_eq!(
        probe.music_folders().await.expect("folders list"),
        vec![("folder-1".to_owned(), "Library".to_owned())],
        "the 0.62.0 probe shape is a single folder"
    );
}

// ---------------------------------------------------------------------------
// Plex
// ---------------------------------------------------------------------------

#[tokio::test]
async fn plex_container_paging_merges_music_sections() {
    let mock = serve_plex().await.expect("mock serves");
    let handle = plex_handle(&mock.base_url, Vec::new());
    let stats = handle.stats().await.expect("stats load");
    assert_eq!(
        (stats.total_albums, stats.total_artists, stats.total_tracks),
        (3, 2, 4)
    );

    let first = handle
        .albums(&AlbumBrowse {
            limit: 2,
            offset: 0,
            ..AlbumBrowse::default()
        })
        .await
        .expect("page one loads");
    assert_eq!(first.items.len(), 2);
    assert_eq!(first.total, 3);
    let second = handle
        .albums(&AlbumBrowse {
            limit: 2,
            offset: 2,
            ..AlbumBrowse::default()
        })
        .await
        .expect("page two loads");
    assert_eq!(second.items.len(), 1);
    assert_eq!(second.items[0].title, "Paper Satellites");

    let calls = mock.recorder.snapshot();
    let endpoints: Vec<&str> = calls
        .plex_section_queries
        .iter()
        .map(|(endpoint, _)| endpoint.as_str())
        .collect();
    assert!(endpoints.contains(&"/library/sections/1/all"));
    assert!(endpoints.contains(&"/library/sections/2/all"));
    assert!(
        !endpoints.iter().any(|endpoint| endpoint.contains("/3/")),
        "movie sections stay out"
    );
    assert!(
        calls.plex_section_queries.iter().any(|(_, query)| {
            query.contains(&("X-Plex-Container-Size".to_owned(), "0".to_owned()))
        }),
        "counts query with zero size"
    );
}

#[tokio::test]
async fn plex_section_allowlist_pins_queries_to_one_section() {
    let mock = serve_plex().await.expect("mock serves");
    let handle = plex_handle(&mock.base_url, vec!["1".to_owned()]);
    let page = handle
        .albums(&AlbumBrowse {
            limit: 50,
            ..AlbumBrowse::default()
        })
        .await
        .expect("pinned albums load");
    assert_eq!(page.items.len(), 2, "section 1 holds two albums");

    let queries = mock.recorder.snapshot().plex_section_queries;
    assert!(!queries.is_empty(), "the pinned browse queried upstream");
    for (endpoint, _) in &queries {
        assert!(
            endpoint.starts_with("/library/sections/1/"),
            "allowlist pins every query to section 1: {endpoint}"
        );
    }
}

/// A stub that answers 403 to everything. Dropping aborts the listener.
struct ForbiddenStub {
    base_url: String,
    handle: Option<tokio::task::JoinHandle<()>>,
}

impl ForbiddenStub {
    async fn serve() -> Self {
        let app = Router::new().fallback(|| async { StatusCode::FORBIDDEN });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("stub binds");
        let base_url = format!(
            "http://{}",
            listener.local_addr().expect("stub has an address")
        );
        let handle = tokio::spawn(async move {
            axum::serve(listener, app).await.expect("stub serves");
        });
        Self {
            base_url,
            handle: Some(handle),
        }
    }
}

impl Drop for ForbiddenStub {
    fn drop(&mut self) {
        if let Some(handle) = self.handle.take() {
            handle.abort();
        }
    }
}

#[tokio::test]
async fn upstream_forbidden_maps_to_auth_on_every_source() {
    let stub = ForbiddenStub::serve().await;
    let jellyfin = jellyfin_handle(&stub.base_url);
    assert_eq!(jellyfin.stats().await, Err(AdapterError::Auth));
    let navidrome = navidrome_handle(&stub.base_url, None);
    assert_eq!(navidrome.stats().await, Err(AdapterError::Auth));
    let plex = plex_handle(&stub.base_url, Vec::new());
    assert_eq!(plex.stats().await, Err(AdapterError::Auth));
}

// ---------------------------------------------------------------------------
// Connection store
// ---------------------------------------------------------------------------

/// A user with no link of their own uses the admin's server account;
/// once they link, their own account wins, sealed at rest. A link that no
/// longer opens falls back to the shared account.
#[tokio::test]
async fn own_link_wins_over_the_shared_admin_account() {
    use remotes::connections::ConnectionStore as _;

    let rig = Rig::open("remotes-own-link").await;
    rig.navidrome("http://navidrome.test", "admin", "admin-pass")
        .await;
    let resolver = rig.resolver();
    let shared = resolver
        .resolve(USER_ID, SourceName::Navidrome)
        .await
        .expect("shared resolves");
    assert_eq!(shared.account_mode, "shared");
    assert_eq!(shared.credential, "admin-pass");

    resolver
        .save_link(
            USER_ID,
            &UserLink::Navidrome {
                username: "ada".to_owned(),
                password: "s3cret".to_owned(),
            },
        )
        .await
        .expect("link saves");
    let rows = remotes::connections::SqliteConnectionStore::new(
        rig.platform.db.clone(),
        rig.platform.clock.clone(),
    );
    let row = rows
        .get(USER_ID, "navidrome")
        .await
        .expect("row reads")
        .expect("row exists");
    assert!(row.data.starts_with("v3:"), "sealed at rest");
    assert!(!row.data.contains("s3cret"));

    let own = resolver
        .resolve(USER_ID, SourceName::Navidrome)
        .await
        .expect("own resolves");
    assert_eq!(own.account_mode, "linked");
    assert_eq!(own.credential, "s3cret");

    rows.upsert(USER_ID, "navidrome", "v3:garbage".to_owned())
        .await
        .expect("row writes");
    let fallback = resolver
        .resolve(USER_ID, SourceName::Navidrome)
        .await
        .expect("stale link falls back");
    assert_eq!(fallback.account_mode, "shared");
}

/// A disabled or missing admin server hides every link: the URL is the
/// admin's, so nothing resolves without it.
#[tokio::test]
async fn links_need_the_admin_server() {
    let rig = Rig::open("remotes-no-server").await;
    let resolver = rig.resolver();
    resolver
        .save_link(
            USER_ID,
            &UserLink::Navidrome {
                username: "ada".to_owned(),
                password: "s3cret".to_owned(),
            },
        )
        .await
        .expect("link saves");
    assert!(matches!(
        resolver.resolve(USER_ID, SourceName::Navidrome).await,
        Err(ResolveError::NotConfigured)
    ));
}

/// A linked account and a folder preference are SQLite rows: they survive
/// closing and reopening the database, and the credential is sealed at
/// rest.
#[tokio::test]
async fn linked_connections_survive_a_restart() {
    use droppedneedle_kernel::db::{DbConfig, open_runtime};
    use remotes::folders::FolderStore as _;

    let rig = Rig::open("remotes-durable").await;
    rig.navidrome("http://navidrome.test", "", "").await;
    rig.resolver()
        .save_link(
            USER_ID,
            &UserLink::Navidrome {
                username: "ada".to_owned(),
                password: "s3cret".to_owned(),
            },
        )
        .await
        .expect("link saves");
    rig.folders()
        .set(
            USER_ID,
            FolderPreference {
                mode: "selected".to_owned(),
                selected_folder_ids: vec!["folder-1".to_owned()],
                server_identity: Some("server-a".to_owned()),
            },
        )
        .await
        .expect("preference saves");
    let stored: String = sqlx::query_scalar(
        "SELECT connection_data FROM user_connections WHERE user_id = 'user-ada'",
    )
    .fetch_one(rig.runtime.db().sqlx())
    .await
    .expect("row reads");
    assert!(!stored.contains("s3cret"), "sealed at rest");

    let Rig {
        platform,
        runtime,
        _dir: dir,
        ..
    } = rig;
    drop(platform);
    runtime.shutdown().await;
    let runtime = open_runtime(&DbConfig::new(&dir.join("app.db")))
        .await
        .expect("db reopens");
    let platform = crate::common::TestPlatform::on(runtime.db(), &dir)
        .build()
        .await;
    let rig = Rig {
        platform,
        runtime,
        index: MbidIndex::default(),
        _dir: dir,
    };
    let resolved = rig
        .resolver()
        .resolve(USER_ID, SourceName::Navidrome)
        .await
        .expect("link resolves after restart");
    assert_eq!(resolved.account_mode, "linked");
    assert_eq!(resolved.credential, "s3cret");
    let preference = rig.folders().get(USER_ID).await.expect("preference reads");
    assert_eq!(preference.selected_folder_ids, vec!["folder-1".to_owned()]);
}

/// A Plex link saved without a server token (v2 rows from the login flow)
/// looks the token up once through the server's machine id and plex.tv,
/// stores it, and browses with it.
#[tokio::test]
async fn plex_links_without_a_server_token_look_one_up_and_keep_it() {
    use droppedneedle_kernel::runtime_config::sections::InternalState;
    use droppedneedle_testkit::remotes::PLEX_ACCOUNT_TOKEN;
    use remotes::plex::PlexTokenProbe;

    let mock = serve_plex().await.expect("mock serves");
    let rig = Rig::open("remotes-plex-token").await;
    rig.plex(&mock.base_url, "").await;
    rig.platform
        .settings
        .save(InternalState {
            plex_client_id: Some("client-1".to_owned()),
            ..InternalState::default()
        })
        .await
        .expect("client id saves");
    let resolver = rig
        .resolver()
        .with_plex_tokens(Arc::new(PlexTokenProbe::with_plex_tv(
            http_client(),
            format!("{}/api/v2", mock.base_url),
        )));
    resolver
        .save_link(
            USER_ID,
            &UserLink::Plex {
                auth_token: PLEX_ACCOUNT_TOKEN.to_owned(),
                server_access_token: String::new(),
                plex_user_id: "plex-ada".to_owned(),
                username: "ada".to_owned(),
            },
        )
        .await
        .expect("link saves");

    let resolved = resolver
        .resolve(USER_ID, SourceName::Plex)
        .await
        .expect("link resolves");
    assert_eq!(resolved.account_mode, "linked");
    assert_eq!(resolved.credential, PLEX_TOKEN);
    let stored = resolver
        .link(USER_ID, SourceName::Plex)
        .await
        .expect("link reads");
    assert!(
        matches!(stored, Some(UserLink::Plex { server_access_token, .. }) if server_access_token == PLEX_TOKEN),
        "server token stored"
    );
}

// ---------------------------------------------------------------------------
// Folder preferences
// ---------------------------------------------------------------------------

fn available_folders() -> Vec<(String, String)> {
    vec![
        ("folder-1".to_owned(), "Library".to_owned()),
        ("folder-2".to_owned(), "Singles".to_owned()),
    ]
}

#[tokio::test]
async fn folders_fail_closed_on_server_change_or_outage() {
    let available = available_folders();
    let moved = FolderPreference {
        mode: "selected".to_owned(),
        selected_folder_ids: vec!["folder-1".to_owned()],
        server_identity: Some("old-server".to_owned()),
    };
    let resolved = resolve_scope(&moved, Some(&available), "new-server");
    assert_eq!(resolved.scope.folder_ids, Some(Vec::new()));
    assert_eq!(resolved.stale_folder_ids, vec!["folder-1".to_owned()]);

    let down = resolve_scope(&moved, None, "new-server");
    assert!(!down.source_available);
    assert_eq!(
        down.scope.folder_ids,
        Some(vec!["folder-1".to_owned()]),
        "outage echoes the preference"
    );
}

// ---------------------------------------------------------------------------
// Routes
// ---------------------------------------------------------------------------

/// A plain user with no links of their own, on an instance where the admin
/// configured all three servers with shared credentials (the v2 shared
/// mode). Every route below runs through the admin's accounts.
async fn connected_testbed() -> (Router, Rig) {
    // Mocks leak for the life of the test process; routes need their URLs
    // after this helper returns, so the servers stay up detached.
    let jellyfin = Box::leak(Box::new(serve_jellyfin().await.expect("mock serves")));
    let navidrome = Box::leak(Box::new(serve_navidrome().await.expect("mock serves")));
    let plex = Box::leak(Box::new(serve_plex().await.expect("mock serves")));
    let rig = Rig::open("remotes-routes").await;
    rig.jellyfin(&jellyfin.base_url, JELLYFIN_KEY, "jf-user-1")
        .await;
    rig.navidrome(&navidrome.base_url, NAVIDROME_USER, "nd-pass")
        .await;
    rig.plex(&plex.base_url, PLEX_TOKEN).await;
    (rig.app(), rig)
}

#[tokio::test]
async fn routes_reject_anonymous_callers_with_challenge() {
    let rig = Rig::open("remotes-anonymous").await;
    let response = remotes_handlers::routes()
        .with_state(rig.deps())
        .into_router()
        .oneshot(
            Request::get("/api/v3/remotes/plex/hub")
                .body(Body::empty())
                .expect("request builds"),
        )
        .await
        .expect("router answers");
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(
        response
            .headers()
            .get(axum::http::header::WWW_AUTHENTICATE)
            .map(|value| value.to_str().unwrap_or("")),
        Some("Bearer"),
    );
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("body reads");
    let body: Value = serde_json::from_slice(&bytes).expect("body is json");
    assert_eq!(error_code(&body), "UNAUTHORIZED");
}

#[tokio::test]
async fn routes_reject_unknown_sources_and_bad_queries_in_envelope() {
    let (app, _rig) = connected_testbed().await;
    let (status, body) = get_json(app.clone(), "/api/v3/remotes/emby/hub").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(error_code(&body), "INVALID_INPUT");

    let (status, body) = get_json(app.clone(), "/api/v3/remotes/plex/albums?limit=nope").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(error_code(&body), "INVALID_INPUT");

    let (status, body) = get_json(app.clone(), "/api/v3/remotes/plex/search?q=%20").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(error_code(&body), "INVALID_INPUT");
}

#[tokio::test]
async fn routes_report_unconfigured_sources() {
    let rig = Rig::open("remotes-unconfigured").await;
    let app = rig.app();
    let (status, body) = get_json(app, "/api/v3/remotes/plex/hub").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(error_code(&body), "REMOTE_NOT_CONFIGURED");
}

/// A media server that refuses connections answers the closed-list
/// `REMOTE_UNAVAILABLE` through the whole app: the session gate and the rate
/// limiter pass the cause on to the request boundary, which writes the body.
#[tokio::test]
async fn routes_answer_remote_unavailable_when_the_server_is_down() {
    let app = crate::common::TestApp::start("remotes-down").await;
    let auth = app.login_admin().await;
    let closed = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("loopback binds");
    let plex_url = format!("http://{}", closed.local_addr().expect("has an address"));
    drop(closed);
    let (status, body) = app
        .call(
            "PUT",
            "/api/v3/settings/plex",
            &[("authorization", auth.as_str())],
            Some(serde_json::json!({
                "plex_url": plex_url,
                "plex_token": PLEX_TOKEN,
                "enabled": true,
            })),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let (status, headers, bytes) = app
        .raw(
            "GET",
            "/api/v3/remotes/plex/albums",
            &[
                ("authorization", auth.as_str()),
                ("x-request-id", "remote-down-1"),
            ],
            None,
        )
        .await;
    assert_eq!(status, StatusCode::BAD_GATEWAY);
    assert_eq!(headers.get("x-request-id").unwrap(), "remote-down-1");
    let body: Value = serde_json::from_slice(&bytes).expect("body is json");
    assert_eq!(
        body,
        serde_json::json!({"error": {
            "code": "REMOTE_UNAVAILABLE",
            "message": "Your media server did not answer or returned an error.",
            "details": {"error_id": "remote-down-1"},
        }})
    );
    app.stop().await;
}

#[tokio::test]
async fn routes_browse_albums_reports_stats_totals() {
    let (app, _rig) = connected_testbed().await;
    for source in ["jellyfin", "navidrome", "plex"] {
        let (status, body) = get_json(
            app.clone(),
            &format!("/api/v3/remotes/{source}/albums?limit=2"),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{source}");
        assert_eq!(
            body.get("total").and_then(Value::as_i64),
            Some(3),
            "{source} full pages report the stats total"
        );
        assert_eq!(
            body.get("items").and_then(Value::as_array).map(Vec::len),
            Some(2),
            "{source}"
        );
    }
}

/// The album match rule against the Plex and Navidrome mocks, whose search
/// here cannot find an album by its MBID: an indexed album matches by MBID;
/// one without an id matches by title and artist once folded; two albums
/// sharing that title and artist match nothing; and an indexed album
/// outside the caller's Navidrome folders does not match. On Jellyfin an
/// MBID miss reads no album pages.
#[tokio::test]
async fn albums_match_by_index_then_by_name() {
    const UNKNOWN: &str = "99999999-9999-9999-9999-999999999999";
    const FAR_MBID: &str = "44444444-4444-4444-4444-444444444444";
    let extra = |id: &str, title: &str, artist: &str, mbid: Option<&str>, folder: &str| MockAlbum {
        id: id.to_owned(),
        title: title.to_owned(),
        artist: artist.to_owned(),
        mbid: mbid.map(str::to_owned),
        folder: folder.to_owned(),
    };
    let navidrome = serve_navidrome_with(MockOptions {
        mbid_search: false,
        extra_albums: vec![
            extra("nd-al-8", "GLASS TIDES", "AURORA CURRENT", None, "folder-1"),
            extra(
                "nd-al-9",
                "Far Shore",
                "Boreal Static",
                Some(FAR_MBID),
                "folder-2",
            ),
        ],
        ..MockOptions::default()
    })
    .await
    .expect("mock serves");
    let plex = serve_plex_with(MockOptions {
        mbid_search: false,
        extra_albums: vec![extra("px-al-8", "GLASS TIDES", "AURORA CURRENT", None, "1")],
        ..MockOptions::default()
    })
    .await
    .expect("mock serves");
    let jellyfin = serve_jellyfin().await.expect("mock serves");
    let rig = Rig::open("remotes-match").await;
    rig.navidrome(&navidrome.base_url, NAVIDROME_USER, "nd-pass")
        .await;
    rig.plex(&plex.base_url, PLEX_TOKEN).await;
    rig.jellyfin(&jellyfin.base_url, JELLYFIN_KEY, "jf-user-1")
        .await;
    for source in [
        SourceName::Navidrome,
        SourceName::Plex,
        SourceName::Jellyfin,
    ] {
        rig.warm(source).await;
    }
    let app = rig.app();
    let scoped = serde_json::json!({"mode": "selected", "selected_folder_ids": ["folder-1"]});
    let (status, _) = put_json(app.clone(), "/api/v3/remotes/navidrome/folders", scoped).await;
    assert_eq!(status, StatusCode::OK);
    let album_pages = || {
        jellyfin
            .recorder
            .snapshot()
            .jellyfin_items_queries
            .iter()
            .filter(|query| {
                query
                    .iter()
                    .any(|(key, value)| key == "includeItemTypes" && value == "MusicAlbum")
            })
            .count()
    };
    let pages_before = album_pages();

    let by_name = format!("mbid={UNKNOWN}&title=paper%20satellites&artist=BOREAL%20STATIC");
    let shared = format!("mbid={UNKNOWN}&title=Glass%20Tides&artist=Aurora%20Current");
    let index = format!("mbid={MATCH_MBID}");
    let far = format!("mbid={FAR_MBID}");
    let unknown = format!("mbid={UNKNOWN}");
    let rows: [(&str, &str, &str, Option<&str>); 8] = [
        ("by index", "navidrome", &index, Some("nd-al-1")),
        ("by index", "plex", &index, Some("px-al-1")),
        (
            "by title and artist",
            "navidrome",
            &by_name,
            Some("nd-al-3"),
        ),
        ("by title and artist", "plex", &by_name, Some("px-al-3")),
        ("two share title and artist", "navidrome", &shared, None),
        ("two share title and artist", "plex", &shared, None),
        ("indexed outside the folders", "navidrome", &far, None),
        ("unknown id", "jellyfin", &unknown, None),
    ];
    let mut wrong = Vec::new();
    for (case, source, query, want) in rows {
        let (status, body) = get_json(
            app.clone(),
            &format!("/api/v3/remotes/{source}/match?{query}"),
        )
        .await;
        let got = body
            .get("remote_album_id")
            .and_then(Value::as_str)
            .map(str::to_owned);
        let found = body.get("found").and_then(Value::as_bool);
        if status != StatusCode::OK || got.as_deref() != want || found != Some(want.is_some()) {
            wrong.push(format!("{source} {case}: {status} {body}"));
        }
    }
    if album_pages() != pages_before {
        wrong.push("jellyfin: an MBID miss read album pages".to_owned());
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
}

/// An index read from one Plex server is not used once the admin points
/// Plex at another: the old ids would open other albums there.
#[tokio::test]
async fn an_index_from_another_server_is_not_used() {
    let no_mbid_search = || MockOptions {
        mbid_search: false,
        ..MockOptions::default()
    };
    let first = serve_plex_with(no_mbid_search())
        .await
        .expect("mock serves");
    let second = serve_plex_with(no_mbid_search())
        .await
        .expect("mock serves");
    let rig = Rig::open("remotes-match-moved").await;
    rig.plex(&first.base_url, PLEX_TOKEN).await;
    rig.warm(SourceName::Plex).await;
    let path = format!("/api/v3/remotes/plex/match?mbid={MATCH_MBID}");
    let (_, body) = get_json(rig.app(), &path).await;
    assert_eq!(body["remote_album_id"], "px-al-1", "{body}");

    rig.plex(&second.base_url, PLEX_TOKEN).await;
    let (status, body) = get_json(rig.app(), &path).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["found"], false, "{body}");
}

/// An indexed album the server fails to open stops the match with the
/// outage instead of falling through to a same-name album without an id.
#[tokio::test]
async fn an_outage_on_an_indexed_album_is_not_guessed_past() {
    const BROKEN_MBID: &str = "55555555-5555-5555-5555-555555555555";
    let navidrome = serve_navidrome_with(MockOptions {
        mbid_search: false,
        extra_albums: vec![MockAlbum {
            id: "nd-al-7".to_owned(),
            title: "Broken Record".to_owned(),
            artist: "Aurora Current".to_owned(),
            mbid: Some(BROKEN_MBID.to_owned()),
            folder: "folder-1".to_owned(),
        }],
        failing_albums: vec!["nd-al-7".to_owned()],
    })
    .await
    .expect("mock serves");
    let rig = Rig::open("remotes-match-outage").await;
    rig.navidrome(&navidrome.base_url, NAVIDROME_USER, "nd-pass")
        .await;
    rig.warm(SourceName::Navidrome).await;
    let (status, body) = get_json(
        rig.app(),
        &format!("/api/v3/remotes/navidrome/match?mbid={BROKEN_MBID}&title=Glass%20Tides&artist=Aurora%20Current"),
    )
    .await;
    assert!(status.is_server_error(), "{status} {body}");
    assert_eq!(error_code(&body), "REMOTE_UNAVAILABLE", "{body}");
}

#[tokio::test]
async fn routes_import_playlists_idempotently() {
    let (app, _rig) = connected_testbed().await;
    for source in ["jellyfin", "navidrome", "plex"] {
        let id = match source {
            "jellyfin" => "jf-pl-1",
            "navidrome" => "nd-pl-1",
            _ => "px-pl-1",
        };
        let (status, first) = post_empty(
            app.clone(),
            &format!("/api/v3/remotes/{source}/playlists/{id}/import"),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{source} import works");
        assert_eq!(
            first.get("tracks_imported").and_then(Value::as_i64),
            Some(2)
        );
        assert_eq!(
            first.get("already_imported").and_then(Value::as_bool),
            Some(false)
        );
        let (status, repeat) = post_empty(
            app.clone(),
            &format!("/api/v3/remotes/{source}/playlists/{id}/import"),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            repeat.get("already_imported").and_then(Value::as_bool),
            Some(true)
        );
    }
    let (status, body) =
        post_empty(app.clone(), "/api/v3/remotes/plex/playlists/px-pl-9/import").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(error_code(&body), "NOT_FOUND");
}

/// Which optional features each source serves; the rest answer
/// `REMOTE_UNSUPPORTED` rather than an empty success.
#[tokio::test]
async fn routes_serve_optional_features_per_source() {
    let (app, _rig) = connected_testbed().await;
    let matched = format!("match?mbid={MATCH_MBID}");
    let cases = [
        ("navidrome", "info/artists/nd-ar-1", true),
        ("jellyfin", "info/artists/jf-ar-1", false),
        ("plex", "info/artists/px-ar-1", false),
        ("navidrome", "lyrics/nd-t-1", true),
        ("jellyfin", "lyrics/jf-t-1", true),
        ("plex", "lyrics/px-t-1", false),
        ("navidrome", "top/Aurora%20Current", true),
        ("jellyfin", "top/Aurora%20Current", false),
        ("plex", "top/Aurora%20Current", false),
        ("navidrome", "similar/nd-t-1", true),
        ("jellyfin", "similar/jf-t-1", true),
        ("plex", "similar/px-t-1", false),
        ("navidrome", "random?limit=5", true),
        ("jellyfin", "random?limit=5", true),
        ("plex", "random", false),
        ("plex", "discovery?count=5", true),
        ("navidrome", "discovery", false),
        ("jellyfin", "discovery", false),
        ("jellyfin", "mix/jf-al-1?kind=item", true),
        ("navidrome", "mix/nd-al-1?kind=item", false),
        ("plex", "mix/px-al-1?kind=item", false),
        ("plex", "history?limit=10", true),
        ("jellyfin", "history?limit=10", false),
        ("navidrome", "history?limit=10", false),
        ("jellyfin", matched.as_str(), true),
        ("navidrome", matched.as_str(), true),
        ("plex", matched.as_str(), true),
    ];
    for (source, path, supported) in cases {
        let (status, body) =
            get_json(app.clone(), &format!("/api/v3/remotes/{source}/{path}")).await;
        if supported {
            assert_eq!(status, StatusCode::OK, "{source} {path}: {body}");
        } else {
            assert_eq!(status, StatusCode::BAD_REQUEST, "{source} {path}");
            assert_eq!(error_code(&body), "REMOTE_UNSUPPORTED", "{source} {path}");
        }
    }
}

/// A real Navidrome 0.62.0 gives each song a disc cover id with a colon
/// in it (`dc-<album id>:1_0`). A track row still gets an image, and the
/// image route serves it.
#[tokio::test]
async fn navidrome_track_rows_show_the_disc_cover() {
    let (app, _rig) = connected_testbed().await;
    let (status, body) = get_json(
        app.clone(),
        "/api/v3/remotes/navidrome/albums/nd-al-1/tracks",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let image = body["items"][0]["image_url"].as_str().unwrap_or_default();
    assert_eq!(image, "/api/v3/remotes/navidrome/images/dc-nd-al-1:1_0");

    let (status, _, bytes) = get_bytes(app, image).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(&bytes[..2], &[0xFF, 0xD8]);
}

/// Jellyfin art as a real 10.10.6 server holds it: a track whose album
/// folder has the cover has no image of its own, and a playlist has one
/// drawn from its members. A track row points at its album's cover, a
/// playlist cover is the playlist's own, and an item with no image is
/// absent (404), never a server failure.
#[tokio::test]
async fn jellyfin_art_follows_the_servers_image_tags() {
    let (app, _rig) = connected_testbed().await;
    let (status, body) = get_json(
        app.clone(),
        "/api/v3/remotes/jellyfin/albums/jf-al-1/tracks",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let dusk = body["items"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|track| track["id"] == "jf-t-2")
        .unwrap_or_else(|| panic!("no jf-t-2 in {body}"));
    assert_eq!(
        dusk["image_url"],
        "/api/v3/remotes/jellyfin/images/jf-al-1?tag=jf-al-1-tag"
    );

    let (status, _, bytes) = get_bytes(
        app.clone(),
        "/api/v3/remotes/jellyfin/covers/playlists/jf-pl-1",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(&bytes[..2], &[0xFF, 0xD8]);

    let (status, body) = get_json(app.clone(), "/api/v3/remotes/jellyfin/images/jf-t-2").await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
}

#[tokio::test]
async fn routes_serve_images_with_cache_contract() {
    let (app, _rig) = connected_testbed().await;
    let (status, headers, bytes) = get_bytes(
        app.clone(),
        "/api/v3/remotes/navidrome/images/nd-al-1?size=200",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(&bytes[..2], &[0xFF, 0xD8]);
    let header_map: std::collections::HashMap<&str, &str> = headers
        .iter()
        .map(|(key, value)| (key.as_str(), value.as_str()))
        .collect();
    assert_eq!(header_map.get("content-type"), Some(&"image/jpeg"));
    assert!(
        header_map
            .get("cache-control")
            .is_some_and(|value| value.contains("immutable"))
    );
    assert_eq!(header_map.get("x-content-type-options"), Some(&"nosniff"));
    assert_eq!(header_map.get("content-security-policy"), Some(&"sandbox"));

    let (status, headers, _) =
        get_bytes(app.clone(), "/api/v3/remotes/plex/covers/playlists/px-pl-2").await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        headers
            .iter()
            .any(|(key, value)| key == "cache-control" && value == "private, no-store")
    );
    assert!(
        headers
            .iter()
            .any(|(key, value)| key == "x-content-type-options" && value == "nosniff")
    );
    assert!(
        headers
            .iter()
            .any(|(key, value)| key == "content-security-policy" && value == "sandbox")
    );
}

/// A plain user sees the admin's shared account until they link their own;
/// the link never echoes the password, lists under `/me/connections`, and
/// unlinking falls back to the shared account.
#[tokio::test]
async fn routes_link_own_accounts_over_the_shared_admin_account() {
    let (app, _rig) = connected_testbed().await;
    let (status, body) = get_json(app.clone(), "/api/v3/remotes/navidrome/connection").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body.get("connected").and_then(Value::as_bool), Some(true));
    assert_eq!(
        body.get("account_mode").and_then(Value::as_str),
        Some("shared")
    );

    let payload = serde_json::json!({"username": NAVIDROME_USER, "password": "super-secret"});
    let (status, body) =
        put_json(app.clone(), "/api/v3/remotes/navidrome/connection", payload).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        body.get("account_mode").and_then(Value::as_str),
        Some("linked")
    );
    assert!(!body.to_string().contains("super-secret"));

    let (status, body) = get_json(app.clone(), "/api/v3/me/connections").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body.pointer("/connections/0/service")
            .and_then(Value::as_str),
        Some("navidrome")
    );
    assert_eq!(
        body.pointer("/connections/0/username")
            .and_then(Value::as_str),
        Some(NAVIDROME_USER)
    );
    assert!(!body.to_string().contains("super-secret"));

    let wrong = serde_json::json!({"username": "stranger", "password": "nope"});
    let (status, body) = put_json(app.clone(), "/api/v3/remotes/navidrome/connection", wrong).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(error_code(&body), "INVALID_INPUT");

    let (status, body) = delete_json(app.clone(), "/api/v3/remotes/navidrome/connection").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body.get("account_mode").and_then(Value::as_str),
        Some("shared")
    );
}

#[tokio::test]
async fn routes_manage_folder_preferences() {
    let (app, _rig) = connected_testbed().await;
    let (status, body) = get_json(app.clone(), "/api/v3/remotes/navidrome/folders").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body.get("mode").and_then(Value::as_str), Some("all"));

    let payload = serde_json::json!({"mode": "selected", "selected_folder_ids": ["folder-1"]});
    let (status, body) = put_json(app.clone(), "/api/v3/remotes/navidrome/folders", payload).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body.get("mode").and_then(Value::as_str), Some("selected"));
    assert_eq!(
        body.get("folder_ids")
            .and_then(Value::as_array)
            .map(Vec::len),
        Some(1)
    );

    let bad = serde_json::json!({"mode": "selected", "selected_folder_ids": ["nope"]});
    let (status, body) = put_json(app.clone(), "/api/v3/remotes/navidrome/folders", bad).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(error_code(&body), "INVALID_INPUT");

    let duplicate =
        serde_json::json!({"mode": "selected", "selected_folder_ids": ["folder-1", "folder-1"]});
    let (status, body) =
        put_json(app.clone(), "/api/v3/remotes/navidrome/folders", duplicate).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(error_code(&body), "CONFLICT");
}

/// A grid of covers between two browse pages costs one Navidrome folder
/// lookup: the list is reused while fresh, and an image read by id skips
/// the folder scope. Browse lists keep the saved scope.
#[tokio::test]
async fn navidrome_folders_are_fetched_once_across_browsing_and_images() {
    let navidrome = serve_navidrome().await.expect("mock serves");
    let rig = Rig::open("remotes-folder-cache").await;
    rig.navidrome(&navidrome.base_url, NAVIDROME_USER, "nd-pass")
        .await;
    let app = rig.app();
    let folder_calls = || {
        navidrome
            .recorder
            .snapshot()
            .navidrome_queries
            .iter()
            .filter(|(endpoint, _)| endpoint == "getMusicFolders")
            .count()
    };
    let payload = serde_json::json!({"mode": "selected", "selected_folder_ids": ["folder-1"]});
    let (status, _) = put_json(app.clone(), "/api/v3/remotes/navidrome/folders", payload).await;
    assert_eq!(status, StatusCode::OK);
    let before = folder_calls();

    let (status, _) = get_json(app.clone(), "/api/v3/remotes/navidrome/albums").await;
    assert_eq!(status, StatusCode::OK);
    for _ in 0..5 {
        let (status, _, _) = get_bytes(
            app.clone(),
            "/api/v3/remotes/navidrome/images/nd-al-1?size=200",
        )
        .await;
        assert_eq!(status, StatusCode::OK);
    }
    let (status, _) = get_json(app.clone(), "/api/v3/remotes/navidrome/albums").await;
    assert_eq!(status, StatusCode::OK);

    assert_eq!(folder_calls() - before, 1);
    let scoped = navidrome
        .recorder
        .snapshot()
        .navidrome_folder_params
        .get("getAlbumList2")
        .and_then(|calls| calls.last())
        .cloned()
        .unwrap_or_default();
    assert_eq!(scoped, vec!["folder-1".to_owned()]);
}

// ---------------------------------------------------------------------------
// Presence
// ---------------------------------------------------------------------------

/// One presence cycle folds the Plex server's live session into the feed
/// `GET /now-playing` serves; a source the admin did not configure stays
/// out of it.
#[tokio::test]
async fn presence_poll_merges_a_remote_session() {
    use droppedneedle_media::playback::presence::PresenceRegistry;
    use droppedneedle_media::remotes::presence_feed::MediaJobs;

    let plex = serve_plex().await.expect("mock serves");
    let rig = Rig::open("remotes-presence").await;
    rig.plex(&plex.base_url, PLEX_TOKEN).await;
    let registry = PresenceRegistry::new(droppedneedle_kernel::events::EventHub::new());
    let media = MediaJobs::new(
        registry.clone(),
        Arc::new(rig.resolver()),
        http_client(),
        rig.platform.db.sqlx().clone(),
        rig.platform.clock.clone(),
        MbidIndex::default(),
    );
    droppedneedle_kernel::jobs::presence::run_once(&media.feed(), &media.pollers())
        .await
        .expect("cycle polls");
    let feed = registry.snapshot();
    assert_eq!(feed.len(), 1, "{feed:?}");
    let entry = &feed[0];
    assert_eq!(entry.id, "plex:px-session-1");
    assert_eq!(entry.source, "plex");
    assert_eq!(entry.user_name, "Listener");
    assert_eq!(entry.device_name, "Plexamp");
    assert_eq!(entry.progress_ms, Some(45_000));
    assert!(!entry.redacted);
}

// ---------------------------------------------------------------------------
// Hostile servers
// ---------------------------------------------------------------------------

/// Bind a scripted fake that answers every path the same way.
async fn serve_fake(app: Router) -> (String, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("fake binds");
    let address = listener.local_addr().expect("fake has an address");
    let task = tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    (format!("http://{address}"), task)
}

fn every_source(base_url: &str) -> [(&'static str, RemoteHandle); 3] {
    [
        ("jellyfin", jellyfin_handle(base_url)),
        ("navidrome", navidrome_handle(base_url, None)),
        ("plex", plex_handle(base_url, Vec::new())),
    ]
}

/// An image that declares a length over the cap is no image, on every
/// source, and one exactly at the cap still loads.
#[tokio::test]
async fn images_over_the_cap_by_declared_length_are_refused() {
    use axum::http::header::CONTENT_TYPE;
    let serve = |bytes: usize| {
        Router::new()
            .fallback(move || async move { ([(CONTENT_TYPE, "image/jpeg")], vec![0u8; bytes]) })
    };
    let (too_big, big_task) = serve_fake(serve(remotes::adapter::MAX_IMAGE_BYTES + 1)).await;
    for (source, handle) in every_source(&too_big) {
        let outcome = handle.image_bytes("any", 300).await;
        assert!(
            matches!(outcome, Err(AdapterError::NotFound)),
            "{source} kept an oversized image: {:?}",
            outcome.map(|(bytes, _)| bytes.len())
        );
    }
    big_task.abort();

    let (at_cap, cap_task) = serve_fake(serve(remotes::adapter::MAX_IMAGE_BYTES)).await;
    for (_, handle) in every_source(&at_cap) {
        let (bytes, content_type) = handle.image_bytes("any", 300).await.expect("image loads");
        assert_eq!(bytes.len(), remotes::adapter::MAX_IMAGE_BYTES);
        assert_eq!(content_type, "image/jpeg");
    }
    cap_task.abort();
}

/// An image with no declared length that streams past the cap is refused
/// as soon as it crosses it: most of the body is never read.
#[tokio::test]
async fn images_streaming_past_the_cap_are_cut_off() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    const CHUNK: usize = 64 * 1024;
    const TOTAL: usize = remotes::adapter::MAX_IMAGE_BYTES * 4;

    use futures_util::StreamExt as _;
    let sent = Arc::new(AtomicUsize::new(0));
    let counter = sent.clone();
    let app = Router::new().fallback(move || {
        let counter = counter.clone();
        async move {
            let chunks = futures_util::stream::iter(0..TOTAL / CHUNK).map(move |_| {
                counter.fetch_add(CHUNK, Ordering::SeqCst);
                Ok::<_, std::io::Error>(vec![0u8; CHUNK])
            });
            Body::from_stream(chunks)
        }
    });
    let (base_url, task) = serve_fake(app).await;
    for (source, handle) in every_source(&base_url) {
        sent.store(0, Ordering::SeqCst);
        let outcome = handle.image_bytes("any", 300).await;
        assert!(
            matches!(outcome, Err(AdapterError::NotFound)),
            "{source} kept an endless image: {:?}",
            outcome.map(|(bytes, _)| bytes.len())
        );
        assert!(
            sent.load(Ordering::SeqCst) < TOTAL / 2,
            "{source} read most of the oversized body"
        );
    }
    task.abort();
}

/// An API answer that declares a length over the cap is an upstream
/// error on every source, before any of it is read. The session poller
/// makes this call on a timer.
#[tokio::test]
async fn api_answers_over_the_cap_are_refused() {
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("fake binds");
    let base_url = format!(
        "http://{}",
        listener.local_addr().expect("fake has an address")
    );
    let task = tokio::spawn(async move {
        while let Ok((mut socket, _)) = listener.accept().await {
            tokio::spawn(async move {
                let mut request = [0u8; 4096];
                let _ = socket.read(&mut request).await;
                let head = format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n{{",
                    remotes::adapter::MAX_API_BYTES + 1
                );
                let _ = socket.write_all(head.as_bytes()).await;
                tokio::time::sleep(std::time::Duration::from_secs(30)).await;
            });
        }
    });
    for (source, handle) in every_source(&base_url) {
        let outcome = tokio::time::timeout(std::time::Duration::from_secs(2), handle.sessions())
            .await
            .map(|outcome| outcome.map(|_| ()));
        assert!(
            matches!(outcome, Ok(Err(AdapterError::Api(_)))),
            "{source} waited for an oversized answer: {outcome:?}"
        );
    }
    task.abort();
}

/// Navidrome's Last.fm link and image address reach the browser as a link
/// and an image source, so anything that is not a web address comes out
/// absent.
#[tokio::test]
async fn navidrome_links_are_dropped_unless_they_are_web_addresses() {
    let info_with = |link: &'static str| {
        Router::new().fallback(move || async move {
            axum::Json(serde_json::json!({"subsonic-response": {
                "status": "ok",
                "version": "1.16.1",
                "artistInfo2": {"biography": "bio", "lastFmUrl": link, "largeImageUrl": link},
                "albumInfo": {"notes": "notes", "lastFmUrl": link, "largeImageUrl": link},
            }}))
        })
    };
    for (link, expected) in [
        ("javascript:alert(1)", ""),
        ("  JavaScript:alert(1)", ""),
        ("data:text/html,<script>alert(1)</script>", ""),
        ("/music/Artist", ""),
        ("//www.last.fm/music/Artist", ""),
        ("https:/music/Artist", "https://music/Artist"),
        (
            "ht\ttps:\\\\www.last.fm\\music",
            "https://www.last.fm/music",
        ),
        (
            "https://www.last.fm/music/Artist",
            "https://www.last.fm/music/Artist",
        ),
    ] {
        let (base_url, task) = serve_fake(info_with(link)).await;
        let handle = navidrome_handle(&base_url, None);
        let artist = handle.artist_info("ar-1").await.expect("artist info");
        let album = handle.album_info("al-1").await.expect("album info");
        assert_eq!(artist.lastfm_url, expected, "artist link {link:?}");
        assert_eq!(album.lastfm_url, expected, "album link {link:?}");
        assert_eq!(artist.image_url, expected, "artist image {link:?}");
        assert_eq!(album.image_url, expected, "album image {link:?}");
        assert_eq!(artist.biography, "bio");
        task.abort();
    }
}

/// The image route never relays a page: a server that answers an image id
/// with markup or SVG gets the fixed 404, and none of its body goes out.
#[tokio::test]
async fn routes_refuse_image_answers_that_are_not_images() {
    use axum::http::header::CONTENT_TYPE;
    const PAGE: &str = "<svg xmlns=\"http://www.w3.org/2000/svg\"><script>alert(1)</script></svg>";
    for content_type in ["text/html", "image/svg+xml", "IMAGE/SVG+XML; charset=utf-8"] {
        let (base_url, task) = serve_fake(
            Router::new().fallback(move || async move { ([(CONTENT_TYPE, content_type)], PAGE) }),
        )
        .await;
        let rig = Rig::open("remotes-image-pages").await;
        rig.jellyfin(&base_url, JELLYFIN_KEY, "jf-user-1").await;
        rig.plex(&base_url, PLEX_TOKEN).await;
        let app = rig.app();
        for source in ["jellyfin", "plex"] {
            let (status, headers, bytes) =
                get_bytes(app.clone(), &format!("/api/v3/remotes/{source}/images/any")).await;
            assert_eq!(status, StatusCode::NOT_FOUND, "{source} {content_type}");
            let body = String::from_utf8_lossy(&bytes);
            assert!(!body.contains("script"), "{source} relayed {body}");
            assert_eq!(
                serde_json::from_slice::<Value>(&bytes)
                    .ok()
                    .as_ref()
                    .map(error_code),
                Some("NOT_FOUND")
            );
            assert!(
                headers
                    .iter()
                    .any(|(key, value)| key == "content-type" && value == "application/json")
            );
        }
        task.abort();
    }
}

/// A Plex playlist cover path comes from the server and is joined onto the
/// server's address. One that would turn that address into another host
/// (`@host/x` makes the real host the username) is no image, and nothing
/// is sent to the other host.
#[tokio::test]
async fn plex_cover_path_cannot_point_at_another_host() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let hits = Arc::new(AtomicUsize::new(0));
    let counter = hits.clone();
    let (other, other_task) = serve_fake(Router::new().fallback(move || {
        counter.fetch_add(1, Ordering::SeqCst);
        async {
            (
                [(axum::http::header::CONTENT_TYPE, "image/jpeg")],
                vec![0xFFu8, 0xD8],
            )
        }
    }))
    .await;
    let other_host = other.trim_start_matches("http://").to_owned();

    for composite in [
        format!("@{other_host}/x"),
        format!("//{other_host}/x"),
        format!("{other}/x"),
        format!("/\\{other_host}/x"),
        "/playlists/../x".to_owned(),
    ] {
        let listed = composite.clone();
        let (base_url, task) = serve_fake(Router::new().fallback(move || {
            let listed = listed.clone();
            async move {
                axum::Json(serde_json::json!({"MediaContainer": {"Metadata": [{
                    "ratingKey": "pl-1", "title": "Mix", "playlistType": "audio",
                    "composite": listed,
                }]}}))
            }
        }))
        .await;
        let outcome = plex_handle(&base_url, Vec::new())
            .playlist_cover_bytes("pl-1", 300)
            .await;
        assert!(
            matches!(outcome, Err(AdapterError::NotFound)),
            "{composite:?} was fetched: {:?}",
            outcome.map(|(bytes, kind)| (bytes.len(), kind))
        );
        task.abort();
    }
    assert_eq!(hits.load(Ordering::SeqCst), 0, "another host was asked");
    other_task.abort();
}

/// A Plex part key comes from the listener's stream address and a cover
/// path from the server. Neither may walk out of its own folder to
/// another Plex path, however the dots are spelled: nothing is sent for
/// such a value, and an ordinary key still goes to its part.
#[tokio::test]
async fn plex_keys_cannot_walk_to_other_paths() {
    use axum::response::IntoResponse as _;
    let seen = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
    let log = seen.clone();
    let listed = Arc::new(std::sync::Mutex::new(String::new()));
    let composite = listed.clone();
    let (base_url, task) = serve_fake(Router::new().fallback(move |request: Request<Body>| {
        let path = request.uri().path().to_owned();
        let composite = composite.lock().expect("lock").clone();
        log.lock().expect("lock").push(path.clone());
        async move {
            if path == "/playlists" {
                axum::Json(serde_json::json!({"MediaContainer": {"Metadata": [{
                    "ratingKey": "pl-1", "title": "Mix", "playlistType": "audio",
                    "composite": composite,
                }]}}))
                .into_response()
            } else {
                (
                    [(axum::http::header::CONTENT_TYPE, "image/jpeg")],
                    vec![0xFFu8, 0xD8],
                )
                    .into_response()
            }
        }
    }))
    .await;
    let handle = plex_handle(&base_url, Vec::new());
    let client = http_client();
    let take_seen = || std::mem::take(&mut *seen.lock().expect("lock"));

    let request = handle
        .audio_request("library/parts/7/1700000000/file.flac")
        .expect("an ordinary key builds");
    client.execute(request).await.expect("fake answers");
    assert_eq!(take_seen(), ["/library/parts/7/1700000000/file.flac"]);

    let long_key = format!("library/parts/{}/file.flac", "7".repeat(4000));
    for key in [
        "library/parts/%2e%2e/%2e%2e/status/sessions",
        "library/parts/.%2E/%2e./status/sessions",
        "library/parts/%252e%252e/%252e%252e/status/sessions",
        "library/parts/..%2f..%2fstatus/sessions",
        "library/parts/..\\..\\status/sessions",
        "library/parts/7/file.flac?X-Plex-Token=other",
        "library/parts/7/file.flac#x",
        long_key.as_str(),
    ] {
        match handle.audio_request(key) {
            Err(AdapterError::Unsupported(_)) => {}
            Err(other) => panic!("{key:?} failed as {other:?}"),
            Ok(request) => {
                let sent_to = request.url().path().to_owned();
                panic!("{key:?} would be sent to {sent_to}");
            }
        }
    }

    "/playlists/pl-1/composite/1700000000".clone_into(&mut listed.lock().expect("lock"));
    handle
        .playlist_cover_bytes("pl-1", 300)
        .await
        .expect("an ordinary cover loads");
    assert_eq!(
        take_seen(),
        ["/playlists", "/playlists/pl-1/composite/1700000000"]
    );
    for path in [
        "/playlists/%2e%2e/%2e%2e/status/sessions",
        "/playlists/%252e%252e/%252e%252e/status/sessions",
        "/playlists/pl-1/composite?X-Plex-Token=other",
    ] {
        path.clone_into(&mut listed.lock().expect("lock"));
        let outcome = handle.playlist_cover_bytes("pl-1", 300).await;
        assert!(
            matches!(outcome, Err(AdapterError::NotFound)),
            "{path:?} was fetched: {:?}",
            outcome.map(|(bytes, kind)| (bytes.len(), kind))
        );
        assert_eq!(take_seen(), ["/playlists"], "{path:?} reached Plex");
    }
    task.abort();
}

/// Ids in a request are the listener's to choose, and Plex and Jellyfin
/// take them as part of the path. One that is not a plain id (it could
/// walk to another path, or add a query) sends nothing and reads as not
/// found, on browse, image and stream calls alike; a plain id reaches
/// exactly its own path, and a genre name travels as one path segment.
#[tokio::test]
async fn request_ids_cannot_steer_plex_or_jellyfin_calls() {
    use remotes::jellyfin::MixSeed;
    let seen = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
    let log = seen.clone();
    let (base_url, task) = serve_fake(Router::new().fallback(move |request: Request<Body>| {
        let asked = request
            .uri()
            .path_and_query()
            .map(|full| full.as_str().to_owned())
            .unwrap_or_default();
        log.lock().expect("lock").push(asked);
        async { StatusCode::NOT_FOUND }
    }))
    .await;
    let take_seen = || std::mem::take(&mut *seen.lock().expect("lock"));
    let paths = |seen: Vec<String>| -> Vec<String> {
        seen.into_iter()
            .map(|asked| asked.split('?').next().unwrap_or_default().to_owned())
            .collect()
    };
    let client = http_client();
    let plex = plex_handle(&base_url, Vec::new());
    let jellyfin = jellyfin_handle(&base_url);
    let jellyfin_adapter = JellyfinAdapter::new(
        http_client(),
        base_url.clone(),
        JELLYFIN_KEY.to_owned(),
        "jf-user-1".to_owned(),
    );

    let long_id = "7".repeat(600);
    for id in [
        "../Users",
        "..",
        "12/children",
        "12?X-Plex-Token=other",
        "12#x",
        "%2e%2e",
        "",
        long_id.as_str(),
    ] {
        for (source, handle) in [("plex", &plex), ("jellyfin", &jellyfin)] {
            let refused = [
                handle.album_detail(id).await.err(),
                handle.album_tracks(id).await.err(),
                handle.artist_detail(id).await.err(),
                handle.playlist_detail(id).await.err(),
                handle.image_bytes(id, 300).await.err(),
                handle.playlist_cover_bytes(id, 300).await.err(),
            ];
            assert!(
                refused
                    .iter()
                    .all(|outcome| *outcome == Some(AdapterError::NotFound)),
                "{source} took the id {id:?}: {refused:?}"
            );
        }
        let refused = [
            jellyfin_adapter.lyrics(id).await.err(),
            jellyfin_adapter.similar(id, 10).await.err(),
            jellyfin_adapter
                .mix(&MixSeed::Item(id.to_owned()), 10)
                .await
                .err(),
            jellyfin_adapter
                .mix(&MixSeed::Artist(id.to_owned()), 10)
                .await
                .err(),
        ];
        assert!(
            refused
                .iter()
                .all(|outcome| *outcome == Some(AdapterError::NotFound)),
            "jellyfin took the id {id:?}: {refused:?}"
        );
        for (source, key) in [
            ("jellyfin", id.to_owned()),
            ("plex", format!("library/parts/{id}/file.flac")),
        ] {
            // A Plex part key is a path by nature, so a slash alone is fine.
            if source == "plex" && id == "12/children" {
                continue;
            }
            let handle = if source == "plex" { &plex } else { &jellyfin };
            match handle.audio_request(&key) {
                Err(AdapterError::Unsupported(_)) => {}
                Err(other) => panic!("{source} stream key {key:?} failed as {other:?}"),
                Ok(request) => panic!(
                    "{source} stream key {key:?} would be sent to {}",
                    request.url().path()
                ),
            }
        }
        assert_eq!(take_seen(), Vec::<String>::new(), "{id:?} reached a server");
    }
    for genre in ["..", "."] {
        let outcome = jellyfin_adapter
            .mix(&MixSeed::Genre(genre.to_owned()), 10)
            .await;
        assert_eq!(outcome.err(), Some(AdapterError::NotFound), "{genre:?}");
    }
    assert_eq!(take_seen(), Vec::<String>::new());

    let _ = plex.album_detail("12").await;
    let _ = plex.album_tracks("12").await;
    let _ = plex.playlist_detail("12").await;
    let _ = plex.image_bytes("12", 300).await;
    assert_eq!(
        paths(take_seen()),
        [
            "/library/metadata/12",
            "/library/metadata/12/children",
            "/playlists",
            "/library/metadata/12/thumb",
        ]
    );
    let id = "0123456789abcdef0123456789abcdef";
    let dashed = "01234567-89ab-cdef-0123-456789abcdef";
    let _ = jellyfin.album_detail(id).await;
    let _ = jellyfin.playlist_detail(dashed).await;
    let _ = jellyfin.image_bytes(id, 300).await;
    let _ = jellyfin_adapter.lyrics(id).await;
    let _ = jellyfin_adapter.similar(id, 10).await;
    let _ = jellyfin_adapter
        .mix(&MixSeed::Genre("Rock/Pop? 100%#1".to_owned()), 10)
        .await;
    let request = jellyfin.audio_request(id).expect("a plain id builds");
    let _ = client.execute(request).await;
    assert_eq!(
        paths(take_seen()),
        [
            format!("/Items/{id}"),
            format!("/Items/{dashed}"),
            format!("/Items/{id}/Images/Primary"),
            format!("/Audio/{id}/Lyrics"),
            format!("/Items/{id}/Similar"),
            "/MusicGenres/Rock%2FPop%3F%20100%25%231/InstantMix".to_owned(),
            format!("/Audio/{id}/stream"),
        ]
    );
    task.abort();
}
