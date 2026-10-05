//! Remote media servers (Jellyfin, Navidrome, Plex) through the real
//! adapters and routes against the in-repo loopback mocks: paging wire
//! shapes, auth mapping, folder and section scoping that fails closed,
//! sealed credentials that never echo, and the per-source feature matrix.

use droppedneedle::remotes;

use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use droppedneedle::auth::session::extract::Transport;
use droppedneedle::auth::session::middleware::CurrentSession;
use droppedneedle::auth::users::memory::TestRig;
use droppedneedle::auth::users::roles::{Role, SessionKind};
use droppedneedle::runtime_config::crypto::Crypto;
use remotes::adapter::{
    AdapterError, AlbumBrowse, ArtistBrowse, MemoryImportSink, RemoteHandle, TrackBrowse,
};
use remotes::connections::{
    ConnectionDraft, ConnectionStore, CredentialCoder, MemoryConnectionStore, ResolveError,
    rekey_store, resolve_connection, save_connection,
};
use remotes::folders::{FolderPreference, MemoryFolderStore, resolve_scope};
use remotes::handlers::{RemotesDeps, remotes_router};
use remotes::jellyfin::JellyfinAdapter;
use remotes::mocks::{
    JELLYFIN_KEY, MATCH_MBID, NAVIDROME_USER, PLEX_TOKEN, serve_jellyfin, serve_navidrome,
    serve_plex,
};
use remotes::models::{ConnectionSave, SourceName};
use remotes::navidrome::NavidromeAdapter;
use remotes::plex::PlexAdapter;
use serde_json::Value;
use tower::ServiceExt as _;

/// Fixed error id for leak-envelope assertions. A valid UUID.
const FIXED_ID: &str = "123e4567-e89b-12d3-a456-426614174000";

/// Id generator returning one fixed value.
#[derive(Debug, Clone)]
struct FixedIdGenerator;

impl droppedneedle::ids::IdGenerator for FixedIdGenerator {
    fn new_id(&self) -> String {
        FIXED_ID.to_owned()
    }
}

fn test_crypto() -> Crypto {
    Crypto::from_key_bytes(&[9u8; 32]).expect("test key builds")
}

fn other_crypto() -> Crypto {
    Crypto::from_key_bytes(&[4u8; 32]).expect("test key builds")
}

fn http_client() -> reqwest::Client {
    reqwest::Client::new()
}

async fn auth_bundle() -> (TestRig, String) {
    let rig = TestRig::new().expect("rig builds");
    let user = rig.seed_user("ada", Role::User).await;
    (rig, user.id)
}

fn deps_for(rig: &TestRig) -> (RemotesDeps, Arc<MemoryConnectionStore>) {
    let connections = Arc::new(MemoryConnectionStore::new());
    let deps = RemotesDeps {
        http: http_client(),
        connections: connections.clone(),
        coder: Arc::new(CredentialCoder::new(Arc::new(test_crypto()))),
        folders: Arc::new(MemoryFolderStore::new()),
        imports: Arc::new(MemoryImportSink::new()),
        auth: rig.deps.clone(),
        ids: Arc::new(FixedIdGenerator),
    };
    (deps, connections)
}

fn authed_app(deps: RemotesDeps, user_id: &str) -> Router {
    let user_id = user_id.to_owned();
    remotes_router(deps).layer(axum::middleware::from_fn(
        move |mut req: Request<Body>, next: axum::middleware::Next| {
            let user_id = user_id.clone();
            async move {
                req.extensions_mut().insert(CurrentSession {
                    user_id,
                    session_id: "sess-1".to_owned(),
                    kind: SessionKind::Standard,
                    transport: Transport::Bearer,
                });
                next.run(req).await
            }
        },
    ))
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

#[test]
fn connection_debug_output_redacts_the_credential() {
    let save = ConnectionSave {
        base_url: Some("http://plex.test".to_owned()),
        username: Some("ada".to_owned()),
        credential: Some("super-secret-token".to_owned()),
        client_id: None,
        user_id: None,
        section_id: None,
    };
    let draft = ConnectionDraft {
        base_url: save.base_url.clone(),
        username: save.username.clone(),
        credential: save.credential.clone(),
        client_id: None,
        user_id: None,
        section_id: None,
    };
    for rendered in [format!("{save:?}"), format!("{draft:?}")] {
        assert!(!rendered.contains("super-secret-token"), "{rendered}");
        assert!(rendered.contains("http://plex.test"), "{rendered}");
    }
}

#[tokio::test]
async fn connections_seal_credentials_and_resolve_modes() {
    let store = MemoryConnectionStore::new();
    let coder = CredentialCoder::new(Arc::new(test_crypto()));
    save_connection(
        &store,
        &coder,
        "ada",
        SourceName::Navidrome,
        ConnectionDraft {
            base_url: Some("http://navidrome.test".to_owned()),
            username: Some("ada".to_owned()),
            credential: Some("s3cret".to_owned()),
            client_id: None,
            user_id: None,
            section_id: None,
        },
    )
    .await
    .expect("save works");

    let row = store
        .get("ada", SourceName::Navidrome)
        .await
        .expect("row exists");
    assert_ne!(
        row.sealed_credential, "s3cret",
        "ciphertext at rest, never plaintext"
    );
    assert!(row.sealed_credential.starts_with("v3:"));

    let resolved = resolve_connection(&store, &coder, "ada", SourceName::Navidrome)
        .await
        .expect("own row resolves");
    assert_eq!(resolved.credential, "s3cret");
    assert_eq!(resolved.account_mode, "linked");
    assert_eq!(resolved.account_label, "ada");
    assert!(resolved.cache_scope.starts_with("user:ada:"));
    assert_eq!(store.list_owner("ada").await.len(), 1);
    assert!(store.list_owner("bea").await.is_empty());

    save_connection(
        &store,
        &coder,
        remotes::connections::SHARED_OWNER,
        SourceName::Plex,
        ConnectionDraft {
            base_url: Some("http://plex.test".to_owned()),
            username: None,
            credential: Some("shared-token".to_owned()),
            client_id: None,
            user_id: None,
            section_id: None,
        },
    )
    .await
    .expect("shared save works");
    let shared = resolve_connection(&store, &coder, "ada", SourceName::Plex)
        .await
        .expect("shared row resolves");
    assert_eq!(shared.account_mode, "shared");

    assert!(matches!(
        resolve_connection(&store, &coder, "ada", SourceName::Jellyfin).await,
        Err(ResolveError::NotConfigured)
    ));
}

#[tokio::test]
async fn connections_rekey_rotates_and_flags_stale_rows() {
    let store = MemoryConnectionStore::new();
    let old = CredentialCoder::new(Arc::new(test_crypto()));
    let new = CredentialCoder::new(Arc::new(other_crypto()));
    save_connection(
        &store,
        &old,
        "ada",
        SourceName::Plex,
        ConnectionDraft {
            base_url: Some("http://plex.test".to_owned()),
            username: None,
            credential: Some("tok".to_owned()),
            client_id: None,
            user_id: None,
            section_id: None,
        },
    )
    .await
    .expect("save works");

    // Corrupt one row so it no longer opens under any key.
    let mut row = store
        .get("ada", SourceName::Plex)
        .await
        .expect("row exists");
    row.sealed_credential = "v3:corrupted-ciphertext".to_owned();
    store.put(row).await;
    save_connection(
        &store,
        &old,
        "bea",
        SourceName::Plex,
        ConnectionDraft {
            base_url: Some("http://plex.test".to_owned()),
            username: None,
            credential: Some("tok2".to_owned()),
            client_id: None,
            user_id: None,
            section_id: None,
        },
    )
    .await
    .expect("save works");

    let stale = rekey_store(&store, &old, &new).await;
    assert_eq!(stale, vec![("ada".to_owned(), SourceName::Plex)]);
    assert_eq!(
        resolve_connection(&store, &new, "bea", SourceName::Plex)
            .await
            .expect("rotated resolves")
            .credential,
        "tok2"
    );
    assert!(matches!(
        resolve_connection(&store, &old, "bea", SourceName::Plex).await,
        Err(ResolveError::Stale)
    ));
    assert!(matches!(
        resolve_connection(&store, &new, "ada", SourceName::Plex).await,
        Err(ResolveError::Stale)
    ));
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

async fn connected_testbed() -> (Router, String) {
    let (rig, user_id) = auth_bundle().await;
    let (deps, connections) = deps_for(&rig);
    let coder = CredentialCoder::new(Arc::new(test_crypto()));
    // Mocks leak for the life of the test process; routes need their URLs
    // after this helper returns, so the servers stay up detached.
    let jellyfin = Box::leak(Box::new(serve_jellyfin().await.expect("mock serves")));
    let navidrome = Box::leak(Box::new(serve_navidrome().await.expect("mock serves")));
    let plex = Box::leak(Box::new(serve_plex().await.expect("mock serves")));
    save_connection(
        connections.as_ref(),
        &coder,
        &user_id,
        SourceName::Jellyfin,
        ConnectionDraft {
            base_url: Some(jellyfin.base_url.clone()),
            username: Some("Listener".to_owned()),
            credential: Some(JELLYFIN_KEY.to_owned()),
            client_id: None,
            user_id: Some("jf-user-1".to_owned()),
            section_id: None,
        },
    )
    .await
    .expect("jellyfin connects");
    save_connection(
        connections.as_ref(),
        &coder,
        &user_id,
        SourceName::Navidrome,
        ConnectionDraft {
            base_url: Some(navidrome.base_url.clone()),
            username: Some(NAVIDROME_USER.to_owned()),
            credential: Some("nd-pass".to_owned()),
            client_id: None,
            user_id: None,
            section_id: None,
        },
    )
    .await
    .expect("navidrome connects");
    save_connection(
        connections.as_ref(),
        &coder,
        &user_id,
        SourceName::Plex,
        ConnectionDraft {
            base_url: Some(plex.base_url.clone()),
            username: None,
            credential: Some(PLEX_TOKEN.to_owned()),
            client_id: None,
            user_id: None,
            section_id: None,
        },
    )
    .await
    .expect("plex connects");
    (authed_app(deps, &user_id), user_id)
}

#[tokio::test]
async fn routes_reject_anonymous_callers_with_challenge() {
    let (rig, _) = auth_bundle().await;
    let (deps, _) = deps_for(&rig);
    let response = remotes_router(deps)
        .oneshot(
            Request::get("/remotes/plex/hub")
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
    let (app, _) = connected_testbed().await;
    let (status, body) = get_json(app.clone(), "/remotes/emby/hub").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(error_code(&body), "INVALID_INPUT");

    let (status, body) = get_json(app.clone(), "/remotes/plex/albums?limit=nope").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(error_code(&body), "INVALID_INPUT");

    let (status, body) = get_json(app.clone(), "/remotes/plex/search?q=%20").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(error_code(&body), "INVALID_INPUT");
}

#[tokio::test]
async fn routes_report_unconfigured_sources() {
    let (rig, user_id) = auth_bundle().await;
    let (deps, _) = deps_for(&rig);
    let app = authed_app(deps, &user_id);
    let (status, body) = get_json(app, "/remotes/plex/hub").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(error_code(&body), "REMOTE_NOT_CONFIGURED");
}

#[tokio::test]
async fn routes_browse_albums_reports_stats_totals() {
    let (app, _) = connected_testbed().await;
    for source in ["jellyfin", "navidrome", "plex"] {
        let (status, body) =
            get_json(app.clone(), &format!("/remotes/{source}/albums?limit=2")).await;
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

#[tokio::test]
async fn routes_import_playlists_idempotently() {
    let (app, _) = connected_testbed().await;
    for source in ["jellyfin", "navidrome", "plex"] {
        let id = match source {
            "jellyfin" => "jf-pl-1",
            "navidrome" => "nd-pl-1",
            _ => "px-pl-1",
        };
        let (status, first) = post_empty(
            app.clone(),
            &format!("/remotes/{source}/playlists/{id}/import"),
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
            &format!("/remotes/{source}/playlists/{id}/import"),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            repeat.get("already_imported").and_then(Value::as_bool),
            Some(true)
        );
    }
    let (status, body) = post_empty(app.clone(), "/remotes/plex/playlists/px-pl-9/import").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(error_code(&body), "NOT_FOUND");
}

/// Which optional features each source serves; the rest answer
/// `REMOTE_UNSUPPORTED` rather than an empty success.
#[tokio::test]
async fn routes_serve_optional_features_per_source() {
    let (app, _) = connected_testbed().await;
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
        let (status, body) = get_json(app.clone(), &format!("/remotes/{source}/{path}")).await;
        if supported {
            assert_eq!(status, StatusCode::OK, "{source} {path}: {body}");
        } else {
            assert_eq!(status, StatusCode::BAD_REQUEST, "{source} {path}");
            assert_eq!(error_code(&body), "REMOTE_UNSUPPORTED", "{source} {path}");
        }
    }
}

#[tokio::test]
async fn routes_serve_images_with_cache_contract() {
    let (app, _) = connected_testbed().await;
    let (status, headers, bytes) =
        get_bytes(app.clone(), "/remotes/navidrome/images/nd-al-1?size=200").await;
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

    let (status, headers, _) =
        get_bytes(app.clone(), "/remotes/plex/covers/playlists/px-pl-2").await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        headers
            .iter()
            .any(|(key, value)| key == "cache-control" && value == "private, no-store")
    );
}

#[tokio::test]
async fn routes_manage_connections_without_echoing_secrets() {
    let (rig, user_id) = auth_bundle().await;
    let (deps, _) = deps_for(&rig);
    let app = authed_app(deps, &user_id);
    let payload = serde_json::json!({
        "base_url": "http://plex.test",
        "credential": "super-secret-token",
    });
    let (status, body) = put_json(app.clone(), "/remotes/plex/connection", payload).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body.get("connected").and_then(Value::as_bool), Some(true));
    assert!(!body.to_string().contains("super-secret-token"));

    let (status, body) = get_json(app.clone(), "/remotes/plex/connection").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body.get("connected").and_then(Value::as_bool), Some(true));

    let (status, body) = delete_json(app.clone(), "/remotes/plex/connection").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body.get("connected").and_then(Value::as_bool), Some(false));
}

#[tokio::test]
async fn routes_manage_folder_preferences() {
    let (app, _) = connected_testbed().await;
    let (status, body) = get_json(app.clone(), "/remotes/navidrome/folders").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body.get("mode").and_then(Value::as_str), Some("all"));

    let payload = serde_json::json!({"mode": "selected", "selected_folder_ids": ["folder-1"]});
    let (status, body) = put_json(app.clone(), "/remotes/navidrome/folders", payload).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body.get("mode").and_then(Value::as_str), Some("selected"));
    assert_eq!(
        body.get("folder_ids")
            .and_then(Value::as_array)
            .map(Vec::len),
        Some(1)
    );

    let bad = serde_json::json!({"mode": "selected", "selected_folder_ids": ["nope"]});
    let (status, body) = put_json(app.clone(), "/remotes/navidrome/folders", bad).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(error_code(&body), "INVALID_INPUT");

    let duplicate =
        serde_json::json!({"mode": "selected", "selected_folder_ids": ["folder-1", "folder-1"]});
    let (status, body) = put_json(app.clone(), "/remotes/navidrome/folders", duplicate).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(error_code(&body), "CONFLICT");
}
