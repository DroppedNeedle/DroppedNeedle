//! Request intake over the durable ledger: the approval gate, co-requesters,
//! quotas, the wanted and approval queues, and restart survival.
//!
//! Each test mounts the requests routes, signed in through the test sign-in
//! layer, over a scratch database. Asks dispatch into the real download
//! journal on the same database; a test settles a task the way the worker
//! would.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use droppedneedle_acquire::requests::mix::{
    MixGrantHooks, MixSources, PersonalMixBuilder, SkipReason,
};
use droppedneedle_catalog::reads::collections::db::CollectionsDb;
use droppedneedle_catalog::reads::collections::store::playlists::PlaylistStore;
use droppedneedle_kernel::events::EventHub;
use droppedneedle_kernel::runtime_config::sections::DownloadPolicy;
use droppedneedle_kernel::runtime_config::{ConfigStore, Live};
use droppedneedle_media::playback::listening::{ListeningStore, PrefsPatch};
use droppedneedle_media::playback::listening_routes::MixGrants as _;
use droppedneedle_providers::listenbrainz::playlists::{
    RecommendationPlaylist, RecommendationTrack,
};
use droppedneedle_providers::listenbrainz::{
    ListenBrainzCredentials, SimilarArtist, TopRecording, TopReleaseGroup,
};
use futures_util::future::BoxFuture;

use axum::{
    Router,
    body::Body,
    http::{Method, Request, StatusCode},
};
use droppedneedle_acquire::db::AcquireDb;
use droppedneedle_acquire::dispatch::DownloadDispatch;
use droppedneedle_acquire::downloads::state::TaskStatus;
use droppedneedle_acquire::downloads::{PlannedAlbum, ReplaceVersion};
use droppedneedle_acquire::follows::ImportArtist;
use droppedneedle_acquire::requests::{
    self, RequestsState,
    ledger::{WantedWatch, WatchState},
    models::RequestKind,
    quota::{QuotaLedger, QuotaPolicy},
    service::RequestsService,
    state::AcquireReadiness,
};
use droppedneedle_auth::Role;
use droppedneedle_auth::session::store::SessionKind;
use serde_json::{Value, json};
use tower::ServiceExt as _;

use crate::common::auth::{test_principal, with_test_principal};

/// A signed-in caller: user id, role and display name.
type Who = (&'static str, Role, &'static str);

const ADA: Who = ("u-ada", Role::User, "Ada");
const BOB: Who = ("u-bob", Role::User, "Bob");
const TRUSTED: Who = ("u-tris", Role::Trusted, "Tris");
const ADMIN: Who = ("u-root", Role::Admin, "Root");

const MBID_A: &str = "11111111-1111-4111-8111-111111111111";
const MBID_B: &str = "22222222-2222-4222-8222-222222222222";
const MBID_C: &str = "33333333-3333-4333-8333-333333333333";
const ARTIST: &str = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";

/// State over a scratch database with every test user present.
async fn setup() -> (RequestsState, AcquireDb, Downloads) {
    let (state, db, dispatch) = RequestsState::for_tests().expect("requests state builds");
    for (id, name, role) in [
        ("u-ada", "Ada", "user"),
        ("u-bob", "Bob", "user"),
        ("u-tris", "Tris", "trusted"),
        ("u-root", "Root", "admin"),
    ] {
        db.add_user(id, name, role).await.expect("user row");
    }
    let downloads = Downloads {
        dispatch,
        taken: Mutex::new(0),
        cancels: Mutex::new(HashSet::new()),
    };
    (state, db, downloads)
}

/// One task the dispatch wrote, with what its plan asks for.
struct Dispatched {
    id: String,
    user_id: String,
    release_mbid: Option<String>,
    wanted: Vec<(u32, u32)>,
    planned_album: Option<PlannedAlbum>,
}

/// The download journal behind a state, read the way the worker reads it.
struct Downloads {
    dispatch: Arc<DownloadDispatch>,
    /// Tasks already handed out by `take_calls`.
    taken: Mutex<usize>,
    /// Cancelled tasks already handed out by `take_cancels`.
    cancels: Mutex<HashSet<String>>,
}

impl Downloads {
    /// Every task id, oldest first.
    async fn ids(&self) -> Vec<String> {
        sqlx::query_scalar("SELECT id FROM download_tasks ORDER BY created_at, rowid")
            .fetch_all(self.dispatch.journal().db().db().sqlx())
            .await
            .expect("task ids read")
    }

    /// The `n`th task dispatched (1-based).
    async fn task(&self, n: usize) -> String {
        self.ids().await[n - 1].clone()
    }

    /// The tasks dispatched since the last call, oldest first.
    async fn take_calls(&self) -> Vec<Dispatched> {
        let ids = self.ids().await;
        let from = std::mem::replace(&mut *self.taken.lock().unwrap(), ids.len());
        let mut out = Vec::new();
        for id in &ids[from..] {
            let row = self
                .dispatch
                .journal()
                .read_task(id)
                .await
                .expect("task reads")
                .expect("task row");
            let plan_task = id.clone();
            let plan = self
                .dispatch
                .journal()
                .read("test.plan", move |store| store.task_plan(&plan_task))
                .await
                .expect("plan reads");
            out.push(Dispatched {
                id: row.id,
                user_id: row.user_id,
                release_mbid: plan.release_mbid,
                wanted: plan
                    .album_wanted
                    .iter()
                    .map(|position| (position.disc, position.track))
                    .collect(),
                planned_album: plan.planned_album,
            });
        }
        out
    }

    /// The tasks cancelled since the last call.
    async fn take_cancels(&self) -> Vec<String> {
        let cancelled: Vec<String> = sqlx::query_scalar(
            "SELECT id FROM download_tasks WHERE status = 'cancelled' \
             ORDER BY updated_at, rowid",
        )
        .fetch_all(self.dispatch.journal().db().db().sqlx())
        .await
        .expect("cancelled tasks read");
        let mut seen = self.cancels.lock().unwrap();
        cancelled
            .into_iter()
            .filter(|id| seen.insert(id.clone()))
            .collect()
    }

    /// Settle one task as the worker would.
    async fn settle(&self, task_id: &str, status: TaskStatus) {
        let task_id = task_id.to_owned();
        self.dispatch
            .journal()
            .run("test.settle", move |store| {
                store.transition_task(&task_id, status, 1_700_000_000.0, None)
            })
            .await
            .expect("task settles");
    }

    /// The task landed and imported.
    async fn land(&self, task_id: &str) {
        self.settle(task_id, TaskStatus::Completed).await;
    }

    /// The task failed.
    async fn fail(&self, task_id: &str) {
        self.settle(task_id, TaskStatus::Failed).await;
    }
}

/// The download policy beside `db`, saved as `policy`.
fn saved_policy(db: &AcquireDb, policy: DownloadPolicy) -> Live<DownloadPolicy> {
    let dir = db.path().parent().expect("scratch dir").join("settings");
    let store = droppedneedle_acquire::settings::scratch_settings(&dir).expect("settings open");
    store.save_blocking(policy).expect("policy saves");
    store.live()
}

/// A fresh state over the same database: what a restart sees.
fn rebuilt(db: &AcquireDb, quota: QuotaPolicy, downloads: &Downloads) -> RequestsState {
    let policy = saved_policy(
        db,
        DownloadPolicy {
            default_request_quota_count: i64::from(quota.request_count),
            default_request_quota_days: i64::from(quota.request_days),
            default_storage_quota_gb: i64::try_from(quota.storage_gb_per_user).expect("fits"),
            max_library_size_gb: i64::try_from(quota.max_library_gb).expect("fits"),
            ..DownloadPolicy::default()
        },
    );
    let quota = Arc::new(QuotaLedger::new(policy, db.clone()));
    RequestsState::new(db, quota, downloads.dispatch.clone())
}

async fn call(
    state: &RequestsState,
    method: Method,
    uri: &str,
    identity: Option<Who>,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let mut app: Router = requests::requests_routes(state.clone());
    if let Some((user_id, role, name)) = identity {
        let principal = test_principal(user_id, role, "sess-test", SessionKind::Standard)
            .with_username_display(name);
        app = with_test_principal(app, principal);
    }
    let mut builder = Request::builder().method(method).uri(uri);
    let body = match body {
        Some(json) => {
            builder = builder.header("content-type", "application/json");
            Body::from(json.to_string())
        }
        None => Body::empty(),
    };
    let response = app.oneshot(builder.body(body).unwrap()).await.unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

/// Follow `artist` as `who` and ask for its auto-download.
async fn ask_auto_download(state: &RequestsState, who: Who, artist: &str) {
    let follow = format!("/api/v3/artists/{artist}/follow");
    let body = json!({"followed": true, "artist_name": "Artist"});
    assert_eq!(
        call(state, Method::PUT, &follow, Some(who), Some(body))
            .await
            .0,
        StatusCode::OK
    );
    let toggle = format!("/api/v3/artists/{artist}/auto-download");
    let enable = json!({"enabled": true});
    let (status, body) = call(state, Method::PUT, &toggle, Some(who), Some(enable)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["auto_download_state"], "pending");
}

async fn ask(state: &RequestsState, who: Who, mbid: &str) -> (StatusCode, Value) {
    call(
        state,
        Method::POST,
        "/api/v3/requests/albums",
        Some(who),
        Some(json!({"musicbrainz_id": mbid, "artist": "Artist", "album": "Album"})),
    )
    .await
}

fn keys(body: &Value) -> Vec<String> {
    body["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item["musicbrainz_id"].as_str().unwrap().to_owned())
        .collect()
}

fn watch(key: &str, user: &str) -> WantedWatch {
    WantedWatch {
        key: key.to_owned(),
        user_id: user.to_owned(),
        user_name: None,
        artist_name: "Artist".to_owned(),
        album_title: "Album".to_owned(),
        artist_mbid: None,
        year: None,
        cover_url: None,
        kind: "missing".to_owned(),
        state: WatchState::Watching,
        created_at: 1_700_000_000,
        first_release_date: None,
        check_count: 0,
        quiet_streak: 0,
        next_check_at: 1_700_000_000,
        new_candidate_count: 2,
    }
}

// A pending approval, a live dispatched row with a co-requester, an
// auto-download approval and a wanted watch all survive a rebuild over the
// same database.
#[tokio::test]
async fn requests_and_approvals_survive_a_rebuild() {
    let (state, db, dispatch) = setup().await;
    assert_eq!(
        ask(&state, ADA, MBID_A).await.1["status"],
        "awaiting_approval"
    );
    assert_eq!(ask(&state, TRUSTED, MBID_B).await.1["status"], "pending");
    assert_eq!(ask(&state, BOB, MBID_B).await.0, StatusCode::ACCEPTED);
    ask_auto_download(&state, ADA, ARTIST).await;
    state.wanted.enrol(watch(MBID_C, "u-ada")).await.unwrap();

    let after = rebuilt(&db, QuotaPolicy::default(), &dispatch);
    let (_, approvals) = call(
        &after,
        Method::GET,
        "/api/v3/requests/approvals",
        Some(ADMIN),
        None,
    )
    .await;
    assert_eq!(keys(&approvals), vec![MBID_A.to_owned()]);
    let (_, bobs) = call(
        &after,
        Method::GET,
        "/api/v3/requests/active",
        Some(BOB),
        None,
    )
    .await;
    assert_eq!(keys(&bobs), vec![MBID_B.to_owned()]);
    assert_eq!(bobs["items"][0]["requester_count"], 1);
    let (_, count) = call(
        &after,
        Method::GET,
        "/api/v3/requests/approvals/count",
        Some(ADMIN),
        None,
    )
    .await;
    assert_eq!(count["count"], 2, "one album ask plus one follow approval");
    let (_, wanted) = call(
        &after,
        Method::GET,
        "/api/v3/requests/wanted",
        Some(ADA),
        None,
    )
    .await;
    assert_eq!(keys(&wanted), vec![MBID_C.to_owned()]);
}

// User ask, admin approval, dispatch, landing: the row reads imported.
#[tokio::test]
async fn approval_dispatches_and_the_landing_imports() {
    let (state, _db, dispatch) = setup().await;
    ask(&state, ADA, MBID_A).await;
    assert!(dispatch.take_calls().await.is_empty());
    let uri = format!("/api/v3/requests/approvals/{MBID_A}/approve");
    let (status, body) = call(&state, Method::POST, &uri, Some(ADMIN), None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let calls = dispatch.take_calls().await;
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].user_id, "u-ada", "the owner, never the approver");
    // A second approval loses the claim instead of dispatching again.
    let (_, again) = call(&state, Method::POST, &uri, Some(ADMIN), None).await;
    assert_eq!(again["success"], false);
    assert!(dispatch.take_calls().await.is_empty());

    dispatch.land(&calls[0].id).await;
    let (_, active) = call(
        &state,
        Method::GET,
        "/api/v3/requests/active",
        Some(ADA),
        None,
    )
    .await;
    assert!(keys(&active).is_empty());
    let (_, history) = call(
        &state,
        Method::GET,
        "/api/v3/requests/history",
        Some(ADA),
        None,
    )
    .await;
    assert_eq!(history["items"][0]["status"], "imported");
    assert_eq!(history["items"][0]["reviewed_by_name"], "Root");
}

// A duplicate ask attaches as a listener: one dispatch, one row.
#[tokio::test]
async fn duplicate_ask_attaches_a_listener() {
    let (state, _db, dispatch) = setup().await;
    ask(&state, TRUSTED, MBID_A).await;
    let (status, body) = ask(&state, BOB, MBID_A).await;
    assert_eq!(status, StatusCode::ACCEPTED);
    assert_eq!(body["message"], "Request already in progress");
    let calls = dispatch.take_calls().await;
    assert_eq!(calls.len(), 1);
    let (_, all) = call(
        &state,
        Method::GET,
        "/api/v3/requests/active",
        Some(ADMIN),
        None,
    )
    .await;
    assert_eq!(all["count"], 1);
    assert_eq!(all["items"][0]["task_id"], calls[0].id.as_str());
}

const REL_1: &str = "44444444-4444-4444-8444-444444444444";
const REL_2: &str = "55555555-5555-4555-8555-555555555555";
const REL_3: &str = "66666666-6666-4666-8666-666666666666";

/// Ask for album `mbid` in edition `release` as `who`.
async fn ask_edition(state: &RequestsState, who: Who, mbid: &str, release: &str) -> Value {
    let body = json!({
        "musicbrainz_id": mbid, "artist": "Artist", "album": "Album", "release_mbid": release
    });
    let uri = "/api/v3/requests/albums";
    call(state, Method::POST, uri, Some(who), Some(body))
        .await
        .1
}

/// The edition album `mbid`'s live request fetches, as the album page reads it.
async fn requested_edition(state: &RequestsState, mbid: &str) -> Value {
    let (_, active) = call(
        state,
        Method::GET,
        "/api/v3/requests/active",
        Some(ADMIN),
        None,
    )
    .await;
    let items = active["items"].as_array().expect("active items");
    let item = items.iter().find(|item| item["musicbrainz_id"] == mbid);
    item.expect("live request")["release_mbid"].clone()
}

// An album the library does not hold is fetched in the edition the page
// asked for, not the catalog's preferred one.
#[tokio::test]
async fn a_request_fetches_the_edition_it_asked_for() {
    let (state, _db, dispatch) = setup().await;
    assert_eq!(
        ask_edition(&state, TRUSTED, MBID_A, REL_2).await["status"],
        "pending"
    );
    let calls = dispatch.take_calls().await;
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].release_mbid.as_deref(), Some(REL_2));
    assert_eq!(requested_edition(&state, MBID_A).await, REL_2);
}

// A curator's other edition switches a live request while no download
// serves it; a plain user's ask never does, and once the download has
// started the request keeps its edition.
#[tokio::test]
async fn a_request_switches_edition_only_before_its_download_starts() {
    let (state, _db, dispatch) = setup().await;
    ask_edition(&state, ADA, MBID_A, REL_1).await;
    ask_edition(&state, BOB, MBID_A, REL_3).await;
    assert_eq!(requested_edition(&state, MBID_A).await, REL_1);
    let answer = ask_edition(&state, TRUSTED, MBID_A, REL_2).await;
    assert_eq!(answer["status"], "awaiting_approval", "{answer}");
    assert_eq!(answer["message"], "The request now gets this edition");
    assert_eq!(requested_edition(&state, MBID_A).await, REL_2);

    let uri = format!("/api/v3/requests/approvals/{MBID_A}/approve");
    let (status, body) = call(&state, Method::POST, &uri, Some(ADMIN), None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let calls = dispatch.take_calls().await;
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].release_mbid.as_deref(), Some(REL_2));

    let answer = ask_edition(&state, TRUSTED, MBID_A, REL_3).await;
    assert_eq!(answer["status"], "downloading", "{answer}");
    assert_eq!(
        answer["message"],
        "Already downloading, so the edition stays as it is"
    );
    assert_eq!(requested_edition(&state, MBID_A).await, REL_2);
    assert!(dispatch.take_calls().await.is_empty());
}

// A batch skips raw and live duplicates and dispatches the new rows.
#[tokio::test]
async fn batch_dedupes_and_dispatches_new_rows() {
    let (state, _db, dispatch) = setup().await;
    ask(&state, TRUSTED, MBID_A).await;
    dispatch.take_calls().await;
    let item = |mbid: &str| json!({"musicbrainz_id": mbid, "artist_name": "A", "album_title": "B"});
    let (status, body) = call(
        &state,
        Method::POST,
        "/api/v3/requests/batches",
        Some(TRUSTED),
        Some(json!({"items": [item(MBID_A), item(MBID_B), item(MBID_B), item(MBID_C)]})),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    assert_eq!(body["requested"], 2);
    assert_eq!(body["skipped"], 2);
    assert_eq!(dispatch.take_calls().await.len(), 2);
}

// Strangers may not cancel, listeners detach, owners stop the task.
#[tokio::test]
async fn cancel_matrix() {
    let (state, _db, dispatch) = setup().await;
    ask(&state, TRUSTED, MBID_A).await;
    ask(&state, BOB, MBID_A).await;
    let uri = format!("/api/v3/requests/active/{MBID_A}");
    let (status, _) = call(&state, Method::DELETE, &uri, Some(ADA), None).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (_, detached) = call(&state, Method::DELETE, &uri, Some(BOB), None).await;
    assert_eq!(detached["success"], true);
    assert!(
        dispatch.take_cancels().await.is_empty(),
        "a listener never stops the task"
    );
    let (_, cancelled) = call(&state, Method::DELETE, &uri, Some(TRUSTED), None).await;
    assert_eq!(cancelled["success"], true, "{cancelled}");
    assert_eq!(dispatch.take_cancels().await, vec![dispatch.task(1).await]);
    let (_, history) = call(
        &state,
        Method::GET,
        "/api/v3/requests/history",
        Some(TRUSTED),
        None,
    )
    .await;
    assert_eq!(history["items"][0]["status"], "cancelled");

    // An admin stops someone else's download: stopping the task settles
    // the row, and the cancel answers success.
    ask(&state, TRUSTED, MBID_B).await;
    let uri = format!("/api/v3/requests/active/{MBID_B}");
    let (_, stopped) = call(&state, Method::DELETE, &uri, Some(ADMIN), None).await;
    assert_eq!(stopped["success"], true, "{stopped}");
    assert_eq!(dispatch.take_cancels().await, vec![dispatch.task(2).await]);
    let (_, history) = call(
        &state,
        Method::GET,
        "/api/v3/requests/history",
        Some(TRUSTED),
        None,
    )
    .await;
    let stopped = history["items"]
        .as_array()
        .expect("history items")
        .iter()
        .find(|item| item["musicbrainz_id"] == MBID_B)
        .expect("the stopped row");
    assert_eq!(stopped["status"], "cancelled", "{history}");
}

// The request-count quota counts durable rows, so a restart does not reset
// it, and the refusal writes nothing.
#[tokio::test]
async fn request_quota_survives_a_restart() {
    let (_, db, dispatch) = setup().await;
    let limited = QuotaPolicy {
        request_count: 1,
        ..QuotaPolicy::default()
    };
    let state = rebuilt(&db, limited.clone(), &dispatch);
    assert_eq!(ask(&state, ADA, MBID_A).await.0, StatusCode::ACCEPTED);

    let state = rebuilt(&db, limited, &dispatch);
    let (status, body) = ask(&state, ADA, MBID_B).await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(body["error"]["code"], "QUOTA_EXCEEDED");
    assert_eq!(body["error"]["details"]["used"], 1);
    // Curators are exempt.
    assert_eq!(ask(&state, TRUSTED, MBID_B).await.0, StatusCode::ACCEPTED);
}

/// A settings store in `dir` whose download policy was copied in from a
/// `config.json` unvalidated and does not decode: a quota of one, FLAC
/// only, and a recipe entry with a key the section does not know.
pub(crate) fn unreadable_policy_store(dir: &std::path::Path) -> ConfigStore {
    std::fs::create_dir_all(dir).expect("settings dir");
    let stored = json!({
        "download_policy": {
            "default_request_quota_count": 1,
            "quality_min": "flac",
            "quality_max": "flac",
            "quality_recipe": [{"format": "flac", "quality": "lossless", "preset": "x"}],
        }
    });
    std::fs::write(dir.join("config.json"), stored.to_string()).expect("config.json");
    let store = droppedneedle_acquire::settings::scratch_settings(dir).expect("settings open");
    assert!(store.live::<DownloadPolicy>().get().is_err());
    store
}

// A download policy that does not read is never "unlimited": a request
// answers 500 and leaves no request or task behind. Saving the policy again
// brings the quota back without a restart.
#[tokio::test]
async fn an_unreadable_download_policy_refuses_requests_until_saved() {
    let (_, db, dispatch) = setup().await;
    let dir = db
        .path()
        .parent()
        .expect("scratch dir")
        .join("broken-settings");
    let store = unreadable_policy_store(&dir);
    let quota = Arc::new(QuotaLedger::new(store.live(), db.clone()));
    let state = RequestsState::new(&db, quota, dispatch.dispatch.clone());

    let (status, _) = ask(&state, ADA, MBID_A).await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    let (_, history) = call(
        &state,
        Method::GET,
        "/api/v3/requests/history",
        Some(ADA),
        None,
    )
    .await;
    assert_eq!(history["items"], json!([]));
    assert!(dispatch.take_calls().await.is_empty());

    store
        .save_blocking(DownloadPolicy {
            default_request_quota_count: 1,
            ..DownloadPolicy::default()
        })
        .expect("policy saves");
    assert_eq!(ask(&state, ADA, MBID_A).await.0, StatusCode::ACCEPTED);
    assert_eq!(
        ask(&state, ADA, MBID_B).await.0,
        StatusCode::TOO_MANY_REQUESTS
    );
}

// The library cap reads the real library size.
#[tokio::test]
async fn library_cap_reads_the_library() {
    let (_, db, dispatch) = setup().await;
    db.write("test.seed_library", |tx| {
        tx.execute_batch(
            "INSERT INTO local_artists (id, display_name, folded_name, kind, created_at, updated_at)
               VALUES ('ar1', 'A', 'a', 'person', 0, 0);
             INSERT INTO local_albums (id, root_id, grouping_key, title, title_folded,
               album_artist_id, grouping_source, created_at, updated_at)
               VALUES ('al1', 'r1', 'g', 'T', 't', 'ar1', 'automatic', 0, 0);
             INSERT INTO local_tracks (id, local_album_id, root_id, file_path, relative_path,
               path_hash, file_size_bytes, file_mtime_ns, stat_revision, title, title_folded,
               album_title, album_title_folded, file_format, ingest_source, imported_at,
               membership_source)
               VALUES ('t1', 'al1', 'r1', '/m/a.flac', 'a.flac', 'h', 2147483648, 0, 's',
                 'T', 't', 'T', 't', 'flac', 'scan', 0, 'automatic');",
        )?;
        Ok(())
    })
    .await
    .unwrap();
    let state = rebuilt(
        &db,
        QuotaPolicy {
            max_library_gb: 1,
            ..QuotaPolicy::default()
        },
        &dispatch,
    );
    let (status, body) = ask(&state, ADMIN, MBID_A).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body["error"]["code"], "STORAGE_FULL");
}

// A retry dispatches only with approval provenance; otherwise it queues.
#[tokio::test]
async fn retry_keeps_approval_provenance() {
    let (state, _db, dispatch) = setup().await;
    ask(&state, TRUSTED, MBID_A).await;
    ask(&state, ADA, MBID_B).await;
    call(
        &state,
        Method::POST,
        &format!("/api/v3/requests/approvals/{MBID_B}/reject"),
        Some(ADMIN),
        None,
    )
    .await;
    dispatch.fail(&dispatch.task(1).await).await;
    dispatch.take_calls().await;

    let (_, retried) = call(
        &state,
        Method::POST,
        &format!("/api/v3/requests/retry/{MBID_A}"),
        Some(TRUSTED),
        None,
    )
    .await;
    assert_eq!(retried["success"], true, "{retried}");
    assert_eq!(dispatch.take_calls().await.len(), 1);
    // A rejected row is not retryable at all.
    let (_, rejected) = call(
        &state,
        Method::POST,
        &format!("/api/v3/requests/retry/{MBID_B}"),
        Some(ADA),
        None,
    )
    .await;
    assert_eq!(rejected["success"], false);
    assert!(dispatch.take_calls().await.is_empty());
}

// Wanted actions check ownership; a fulfilled watch refuses; the retrying
// rows come from the auto-retry schedule.
#[tokio::test]
async fn wanted_view_and_actions() {
    let (state, _db, dispatch) = setup().await;
    state.wanted.enrol(watch(MBID_A, "u-ada")).await.unwrap();
    let stop = format!("/api/v3/requests/wanted/{MBID_A}/stop");
    assert_eq!(
        call(&state, Method::POST, &stop, Some(BOB), None).await.0,
        StatusCode::NOT_FOUND
    );
    let (_, stopped) = call(&state, Method::POST, &stop, Some(ADA), None).await;
    assert_eq!(stopped["state"], "stopped");
    let (_, resumed) = call(
        &state,
        Method::POST,
        &format!("/api/v3/requests/wanted/{MBID_A}/resume"),
        Some(ADMIN),
        None,
    )
    .await;
    assert_eq!(resumed["state"], "watching");
    state
        .wanted
        .mark_fulfilled(MBID_A, "in_library", 1_700_000_100)
        .await
        .unwrap();
    assert_eq!(
        call(&state, Method::POST, &stop, Some(ADA), None).await.0,
        StatusCode::BAD_REQUEST
    );

    ask(&state, TRUSTED, MBID_B).await;
    dispatch.fail(&dispatch.task(1).await).await;
    let (_, wanted) = call(
        &state,
        Method::GET,
        "/api/v3/requests/wanted",
        Some(TRUSTED),
        None,
    )
    .await;
    // The default policy retries a failed download six times; none spent.
    assert_eq!(wanted["retrying"][0]["musicbrainz_id"], MBID_B);
    assert_eq!(wanted["retrying"][0]["retry_count"], 0);
    assert_eq!(wanted["retrying"][0]["max_attempts"], 6);
}

// Auto-download approvals: single approve, then a batch reject.
#[tokio::test]
async fn auto_download_approvals() {
    let (state, _db, _dispatch) = setup().await;
    ask_auto_download(&state, ADA, ARTIST).await;
    let imported = state
        .follows
        .import(
            "u-bob",
            [(MBID_A, "One"), (MBID_B, "Two")]
                .into_iter()
                .map(|(mbid, name)| ImportArtist {
                    mbid: mbid.to_owned(),
                    name: name.to_owned(),
                    monitors_new: true,
                })
                .collect(),
            Some("batch-1".to_owned()),
        )
        .await
        .unwrap();
    assert_eq!(imported.approval_batch_id.as_deref(), Some("batch-1"));
    let (_, count) = call(
        &state,
        Method::GET,
        "/api/v3/requests/approvals/count",
        Some(ADMIN),
        None,
    )
    .await;
    assert_eq!(count["count"], 2, "one single plus one batch");
    let approve = format!("/api/v3/requests/auto-download-approvals/u-ada/{ARTIST}/approve");
    assert_eq!(
        call(&state, Method::POST, &approve, Some(ADA), None)
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    let (_, approved) = call(&state, Method::POST, &approve, Some(ADMIN), None).await;
    assert_eq!(approved["success"], true);
    let status = state.follows.status("u-ada", ARTIST).await.unwrap();
    assert!(status.auto_download, "the verdict arms the follow with it");
    assert_eq!(
        serde_json::to_value(status.auto_download_state).unwrap(),
        "approved"
    );
    let (_, again) = call(&state, Method::POST, &approve, Some(ADMIN), None).await;
    assert_eq!(again["success"], false, "only a pending row decides");
    let (_, rejected) = call(
        &state,
        Method::POST,
        "/api/v3/requests/auto-download-approval-batches/batch-1/reject",
        Some(ADMIN),
        None,
    )
    .await;
    assert_eq!(rejected["message"], "Auto-download rejected for 2 artists");
    let status = state.follows.status("u-bob", MBID_A).await.unwrap();
    assert!(
        status.followed && !status.auto_download,
        "a rejected batch keeps the follow and clears its intent"
    );
}

/// ListenBrainz stand-in: one weekly-jams playlist with two tracks on two
/// albums, no similar artists.
struct FakeMix;

const MIX_REC_A: &str = "aaaa0000-0000-4000-8000-000000000001";
const MIX_REC_B: &str = "aaaa0000-0000-4000-8000-000000000002";

impl MixSources for FakeMix {
    fn identity<'a>(&'a self, user_id: &'a str) -> BoxFuture<'a, Option<ListenBrainzCredentials>> {
        Box::pin(async move {
            matches!(user_id, "u-ada" | "u-tris").then(|| ListenBrainzCredentials {
                username: Some("ada".to_owned()),
                user_token: Some("token".to_owned()),
            })
        })
    }
    fn linked_users(&self) -> BoxFuture<'_, Result<Vec<String>, String>> {
        Box::pin(async { Ok(vec!["u-ada".to_owned(), "u-tris".to_owned()]) })
    }
    fn recommendation_playlists<'a>(
        &'a self,
        _creds: &'a ListenBrainzCredentials,
    ) -> BoxFuture<'a, Result<Vec<RecommendationPlaylist>, String>> {
        Box::pin(async {
            Ok(vec![RecommendationPlaylist {
                playlist_id: "jams".to_owned(),
                source_patch: "weekly-jams".to_owned(),
                identifier: "https://listenbrainz.org/playlist/jams".to_owned(),
            }])
        })
    }
    fn playlist_tracks<'a>(
        &'a self,
        _id: &'a str,
        _creds: &'a ListenBrainzCredentials,
    ) -> BoxFuture<'a, Result<Vec<RecommendationTrack>, String>> {
        let track = |title: &str, rec: &str| RecommendationTrack {
            title: title.to_owned(),
            creator: "Band".to_owned(),
            album: format!("{title} LP"),
            recording_mbid: Some(rec.to_owned()),
            artist_mbids: Vec::new(),
            caa_release_mbid: None,
            duration_ms: None,
        };
        Box::pin(async move { Ok(vec![track("One", MIX_REC_A), track("Two", MIX_REC_B)]) })
    }
    fn release_groups<'a>(
        &'a self,
        _recs: &'a [String],
        _creds: &'a ListenBrainzCredentials,
    ) -> BoxFuture<'a, Result<HashMap<String, String>, String>> {
        Box::pin(async {
            Ok(HashMap::from([
                (MIX_REC_A.to_owned(), MBID_A.to_owned()),
                (MIX_REC_B.to_owned(), MBID_B.to_owned()),
            ]))
        })
    }
    fn similar_artists<'a>(
        &'a self,
        _mbid: &'a str,
        _limit: usize,
    ) -> BoxFuture<'a, Result<Vec<SimilarArtist>, String>> {
        Box::pin(async { Ok(Vec::new()) })
    }
    fn top_release_groups<'a>(
        &'a self,
        _mbid: &'a str,
        _count: usize,
    ) -> BoxFuture<'a, Result<Vec<TopReleaseGroup>, String>> {
        Box::pin(async { Ok(Vec::new()) })
    }
    fn top_recording<'a>(
        &'a self,
        _mbid: &'a str,
    ) -> BoxFuture<'a, Result<Option<TopRecording>, String>> {
        Box::pin(async { Ok(None) })
    }
}

// The weekly mix journey: the toggle queues an approval, the admin grants
// it, a build writes the playlist and requests the missing albums without
// a per-request approval wait, a reject turns the toggle off, and the
// refresh route needs a ListenBrainz link.
#[tokio::test]
async fn personal_mix_grant_build_and_refresh() {
    let (mut state, db, dispatch) = setup().await;
    let prefs = ListeningStore::new(
        db.db().clone(),
        Arc::new(droppedneedle_kernel::runtime_config::Crypto::from_key_bytes(&[7u8; 32]).unwrap()),
        Arc::new(droppedneedle_kernel::clock::SystemClock),
    );
    let playlists = PlaylistStore::new(CollectionsDb::new(db.db().clone(), db.clock().clone()));
    let builder = Arc::new(PersonalMixBuilder::new(
        Arc::new(FakeMix),
        prefs.clone(),
        playlists.clone(),
        state.clone(),
        EventHub::new(),
    ));
    state.mixer = Some(builder.clone());
    let hooks = MixGrantHooks::new(state.mixer.clone());

    prefs
        .update_prefs(
            "u-ada",
            &PrefsPatch {
                auto_request_personal_mix: Some(true),
                ..PrefsPatch::default()
            },
        )
        .await;
    hooks.on_toggle("u-ada", Role::User, true).await;
    assert_eq!(hooks.state("u-ada", Role::User, true).await, "pending");
    let approve = "/api/v3/requests/personal-mix-approvals/u-ada/approve";
    assert_eq!(
        call(&state, Method::POST, approve, Some(ADMIN), None)
            .await
            .1["success"],
        true
    );
    assert_eq!(hooks.state("u-ada", Role::User, true).await, "approved");

    let built = builder.build_for_user("u-ada", false).await.unwrap();
    assert_eq!(
        (built.track_count, built.requested_albums, built.skipped),
        (2, 2, None)
    );
    let playlist = built.playlist_id.expect("mix playlist");
    assert_eq!(playlists.tracks(&playlist).await.unwrap().len(), 2);
    assert_eq!(
        dispatch.take_calls().await.len(),
        2,
        "granted asks dispatch straight away"
    );
    let again = builder.build_for_user("u-ada", false).await.unwrap();
    assert_eq!(again.skipped, Some(SkipReason::Fresh));

    // A trusted user holds the grant by role, so the toggle files no approval
    // row and the build requests straight away. Demoted to user, the grant is
    // gone and the build requests nothing.
    let toggle = PrefsPatch {
        auto_request_personal_mix: Some(true),
        ..PrefsPatch::default()
    };
    prefs.update_prefs("u-tris", &toggle).await;
    hooks.on_toggle("u-tris", Role::Trusted, true).await;
    assert_eq!(hooks.state("u-tris", Role::Trusted, true).await, "approved");
    let trusted = builder.build_for_user("u-tris", false).await.unwrap();
    assert_eq!(
        trusted.requested_albums, 2,
        "trusted asks need no approval row"
    );

    db.write("test.demote_tris", |tx| {
        tx.execute(
            "UPDATE auth_users SET role = 'user' WHERE id = 'u-tris'",
            rusqlite::params![],
        )?;
        Ok(())
    })
    .await
    .expect("demotion");
    assert_eq!(hooks.state("u-tris", Role::User, true).await, "none");
    let demoted = builder.build_for_user("u-tris", true).await.unwrap();
    assert_eq!(demoted.requested_albums, 0, "a demotion drops the grant");

    let revoke = "/api/v3/requests/personal-mix-approvals/u-ada/revoke";
    assert_eq!(
        call(&state, Method::POST, revoke, Some(ADMIN), None)
            .await
            .1["success"],
        true
    );
    assert!(
        !prefs.prefs("u-ada").await.auto_request_personal_mix,
        "revoke turns the toggle off"
    );

    let refresh = "/api/v3/requests/personal-mix/refresh";
    assert_eq!(
        call(&state, Method::POST, refresh, Some(ADA), None).await.1["status"],
        "started"
    );
    assert_eq!(
        call(&state, Method::POST, refresh, Some(BOB), None).await.0,
        StatusCode::BAD_REQUEST,
        "no ListenBrainz link"
    );
}

// History pages; a user hides a row from their own view, an admin deletes.
#[tokio::test]
async fn history_pages_and_clears() {
    let (state, _db, dispatch) = setup().await;
    for mbid in [MBID_A, MBID_B, MBID_C] {
        ask(&state, TRUSTED, mbid).await;
    }
    ask(&state, BOB, MBID_A).await;
    let tasks = dispatch.take_calls().await;
    assert_eq!(tasks.len(), 3);
    for task in &tasks {
        dispatch.land(&task.id).await;
    }
    let (_, page) = call(
        &state,
        Method::GET,
        "/api/v3/requests/history?page=2&page_size=2",
        Some(TRUSTED),
        None,
    )
    .await;
    assert_eq!(page["total"], 3);
    assert_eq!(page["total_pages"], 2);
    assert_eq!(page["items"].as_array().unwrap().len(), 1);

    let clear = format!("/api/v3/requests/history/{MBID_A}");
    assert_eq!(
        call(&state, Method::DELETE, &clear, Some(ADA), None)
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        call(&state, Method::DELETE, &clear, Some(BOB), None)
            .await
            .1["success"],
        true
    );
    let (_, bobs) = call(
        &state,
        Method::GET,
        "/api/v3/requests/history",
        Some(BOB),
        None,
    )
    .await;
    assert_eq!(bobs["total"], 0, "hidden from Bob only");
    let (_, owners) = call(
        &state,
        Method::GET,
        "/api/v3/requests/history",
        Some(TRUSTED),
        None,
    )
    .await;
    assert_eq!(owners["total"], 3);
    call(&state, Method::DELETE, &clear, Some(ADMIN), None).await;
    let (_, owners) = call(
        &state,
        Method::GET,
        "/api/v3/requests/history",
        Some(TRUSTED),
        None,
    )
    .await;
    assert_eq!(owners["total"], 2);
}

// Missing principal is 401, a user on an admin queue is 403, a bad body is
// a 400 envelope.
#[tokio::test]
async fn auth_and_envelope_posture() {
    let (state, _db, _dispatch) = setup().await;
    let (status, body) = call(&state, Method::GET, "/api/v3/requests/active", None, None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body["error"]["code"], "UNAUTHORIZED");
    let (status, _) = call(
        &state,
        Method::GET,
        "/api/v3/requests/approvals",
        Some(ADA),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, body) = call(
        &state,
        Method::POST,
        "/api/v3/requests/albums",
        Some(ADA),
        Some(json!({"musicbrainz_id": "not-an-mbid", "artist": "A", "album": "B"})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"]["message"], "Invalid MBID format");
}

// Startup recovery finishes an interrupted cancel and dispatches an
// approved row whose task never got linked.
#[tokio::test]
async fn recovery_finishes_cancels_and_dispatches() {
    let (state, _db, dispatch) = setup().await;
    ask(&state, TRUSTED, MBID_A).await;
    // An owner cancel cut off after the row moved to `cancelling`.
    state
        .store
        .prepare_requester_cancel(RequestKind::Album, MBID_A, "u-tris", 1_700_000_000)
        .await
        .unwrap();
    // An approval whose dispatch never happened.
    ask(&state, ADA, MBID_B).await;
    let row = state
        .store
        .get(RequestKind::Album, MBID_B)
        .await
        .unwrap()
        .unwrap();
    state
        .store
        .claim_approval(
            RequestKind::Album,
            MBID_B,
            None,
            1_700_000_000,
            row.generation,
        )
        .await
        .unwrap();
    dispatch.take_calls().await;

    let report = RequestsService::new(&state).recover().await.unwrap();
    assert_eq!((report.cancelled, report.redispatched), (1, 1));
    let first = dispatch.task(1).await;
    assert_eq!(dispatch.take_cancels().await, vec![first.clone()]);
    assert_eq!(
        dispatch
            .dispatch
            .task(&first)
            .await
            .unwrap()
            .map(|task| task.status),
        Some(TaskStatus::Cancelled)
    );
    let calls = dispatch.take_calls().await;
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].user_id, "u-ada");
    let row = state
        .store
        .get(RequestKind::Album, MBID_B)
        .await
        .unwrap()
        .unwrap();
    assert!(row.task_id.is_some(), "the new task is linked");
}

/// The library's plan for one album, scripted: track 1 kept, track 2
/// missing (held by "t2" when `complete`), track 3 kept and track 4 held
/// only by "t4v", another version of it.
struct PlannedEdition {
    complete: bool,
}

impl droppedneedle_library::operations::port::EditionPlans for PlannedEdition {
    fn group_plan<'a>(
        &'a self,
        _album_id: Option<&'a str>,
        _release_group_mbid: &'a str,
    ) -> droppedneedle_library::operations::port::BoxFuture<
        'a,
        Result<
            Option<droppedneedle_library::operations::plan::EditionPlan>,
            droppedneedle_library::operations::models::OperationError,
        >,
    > {
        use droppedneedle_library::operations::plan::{
            EditionFacts, EditionPlan, FileFate, PlanFile, PlanTrack,
        };
        let track = |position: u32, file: Option<&str>| PlanTrack {
            disc: 1,
            position,
            title: format!("T{position}"),
            recording_mbid: format!("rec-{position}"),
            release_track_mbid: format!("rt-{position}"),
            length_ms: Some(200_000),
            local_track_id: file.map(str::to_owned),
        };
        let kept = |id: &str| PlanFile {
            local_track_id: id.to_owned(),
            title: String::new(),
            disc: 1,
            track: 0,
            length_ms: None,
            fate: FileFate::Keep,
            edition_track: None,
            note: None,
        };
        let two = self.complete.then_some("t2");
        let mut files = vec![kept("t1"), kept("t3")];
        files.extend(two.map(kept));
        files.push(PlanFile {
            fate: FileFate::OtherVersion,
            edition_track: Some((1, 4)),
            ..kept("t4v")
        });
        Box::pin(async move {
            Ok(Some(EditionPlan {
                local_album_id: "al1".into(),
                release_group_mbid: MBID_A.into(),
                release_mbid: "rel-1".into(),
                title: "Album".into(),
                artist_name: "Artist".into(),
                is_current: true,
                same_album: true,
                edition: EditionFacts::default(),
                tracks: vec![
                    track(1, Some("t1")),
                    track(2, two),
                    track(3, Some("t3")),
                    track(4, None),
                ],
                files,
            }))
        })
    }
}

/// The library album "al1" the edition tests plan from: one flac track and
/// one mp3 track, so the plan has a missing track and one below cutoff.
async fn seed_edition_album(db: &AcquireDb) {
    db.write("test.seed_library", |tx| {
        tx.execute_batch(
            "INSERT INTO local_artists (id, display_name, folded_name, kind, created_at, updated_at)
               VALUES ('ar1', 'A', 'a', 'person', 0, 0);
             INSERT INTO local_albums (id, root_id, grouping_key, title, title_folded,
               album_artist_id, grouping_source, created_at, updated_at)
               VALUES ('al1', 'r1', 'g', 'T', 't', 'ar1', 'automatic', 0, 0);
             INSERT INTO local_tracks (id, local_album_id, root_id, file_path, relative_path,
               path_hash, file_size_bytes, file_mtime_ns, stat_revision, title, title_folded,
               album_title, album_title_folded, file_format, bit_rate, ingest_source, imported_at,
               membership_source)
               VALUES ('t1', 'al1', 'r1', '/m/1.flac', '1.flac', 'h1', 1, 0, 's', 'T', 't', 'T',
                 't', 'flac', NULL, 'scan', 0, 'automatic'),
                      ('t3', 'al1', 'r1', '/m/3.mp3', '3.mp3', 'h3', 1, 0, 's', 'T', 't', 'T',
                 't', 'mp3', 128, 'scan', 0, 'automatic');",
        )?;
        Ok(())
    })
    .await
    .unwrap();
}

/// A state whose library plans album "al1" (see [`PlannedEdition`]), with
/// upgrades below lossless on.
async fn edition_setup() -> (RequestsState, AcquireDb, Downloads) {
    let (mut state, db, dispatch) = setup().await;
    seed_edition_album(&db).await;
    state.edition_plans = Some(Arc::new(PlannedEdition { complete: false }));
    state.upgrade_policy = Some(saved_policy(
        &db,
        DownloadPolicy {
            upgrade_allowed: true,
            quality_cutoff: "lossless".into(),
            ..DownloadPolicy::default()
        },
    ));
    (state, db, dispatch)
}

// "Get missing tracks" sends one album download on the chosen edition for
// the missing track plus the owned track below the cutoff, and refuses a
// second while it runs. A track the copy holds in another version is
// asked for with "replace_versions" (on by default), naming the file it
// replaces, and never on a copy the app leaves alone.
#[tokio::test]
async fn edition_acquire_asks_once_for_missing_and_below_cutoff_tracks() {
    let (state, _db, dispatch) = edition_setup().await;
    let asked = format!("/api/v3/albums/{MBID_A}/edition/acquire?album_id=al1");
    let uri = format!("{asked}&replace_versions=false");
    let (status, body) = call(&state, Method::POST, &uri, Some(ADMIN), None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["status"], "started");
    assert_eq!(
        (&body["requested"], &body["upgrades"], &body["versions"]),
        (&json!(1), &json!(1), &json!(0))
    );
    let calls = dispatch.take_calls().await;
    assert_eq!(calls.len(), 1, "one download for the album");
    assert_eq!(calls[0].release_mbid.as_deref(), Some("rel-1"));
    assert_eq!(calls[0].wanted, vec![(1, 2), (1, 3)], "your version stays");
    assert_eq!(
        calls[0].planned_album,
        Some(PlannedAlbum {
            album_id: "al1".into(),
            release_mbid: "rel-1".into(),
            replace: Vec::new(),
        }),
        "the files join the copy the plan was for"
    );

    let (status, body) = call(&state, Method::POST, &uri, Some(ADMIN), None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["status"], "already_in_progress");
    assert!(dispatch.take_calls().await.is_empty(), "never two at once");

    // "replace_versions" is on when not given.
    let replacing = asked;
    let (state, _db, dispatch) = edition_setup().await;
    let (status, body) = call(&state, Method::POST, &replacing, Some(ADMIN), None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        (&body["requested"], &body["upgrades"], &body["versions"]),
        (&json!(1), &json!(1), &json!(1))
    );
    assert_eq!(
        body["message"],
        "Downloading the 1 missing track of this edition and 1 in this edition's version."
    );
    let calls = dispatch.take_calls().await;
    assert_eq!(calls[0].wanted, vec![(1, 2), (1, 3), (1, 4)]);
    assert_eq!(
        calls[0]
            .planned_album
            .as_ref()
            .map(|planned| planned.replace.clone()),
        Some(vec![ReplaceVersion {
            release_track_mbid: "rt-4".into(),
            local_track_id: "t4v".into(),
        }])
    );

    // A person's "leave unmanaged" keeps their version.
    let (state, db, dispatch) = edition_setup().await;
    db.write("test.exclude", |tx| {
        tx.execute_batch(
            "INSERT INTO library_management_exclusions (local_album_id, reason, \
             excluded_by_user_id, excluded_at) VALUES ('al1', 'mine', 'u-root', 0);",
        )?;
        Ok(())
    })
    .await
    .unwrap();
    let (status, body) = call(&state, Method::POST, &replacing, Some(ADMIN), None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["versions"], json!(0));
    assert!(
        body["message"]
            .as_str()
            .is_some_and(|message| message.ends_with(
                "Your own versions stay, because this \
                 album's files are left as they are."
            )),
        "{body}"
    );
    assert_eq!(dispatch.take_calls().await[0].wanted, vec![(1, 2), (1, 3)]);

    // Only a version left, not asked for (upgrades off).
    let (mut state, db, dispatch) = setup().await;
    seed_edition_album(&db).await;
    state.edition_plans = Some(Arc::new(PlannedEdition { complete: true }));
    let (status, body) = call(&state, Method::POST, &uri, Some(ADMIN), None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["status"], "already_complete", "{body}");
    assert_eq!(
        body["message"],
        "Every track of this edition is in your library, 1 of them in another version."
    );
    assert!(dispatch.take_calls().await.is_empty());
}

/// The library's plan, unavailable (MusicBrainz down).
struct NoPlan;

impl droppedneedle_library::operations::port::EditionPlans for NoPlan {
    fn group_plan<'a>(
        &'a self,
        _album_id: Option<&'a str>,
        _release_group_mbid: &'a str,
    ) -> droppedneedle_library::operations::port::BoxFuture<
        'a,
        Result<
            Option<droppedneedle_library::operations::plan::EditionPlan>,
            droppedneedle_library::operations::models::OperationError,
        >,
    > {
        Box::pin(async {
            Err(
                droppedneedle_library::operations::models::OperationError::Unavailable(
                    "MusicBrainz is down".into(),
                ),
            )
        })
    }
}

// With no plan, "Get missing tracks" fetches the whole album for the copy
// asked about, on the release its identity names.
#[tokio::test]
async fn edition_acquire_without_a_plan_fetches_for_the_named_copy() {
    let (mut state, db, dispatch) = setup().await;
    let seed = format!(
        "INSERT INTO local_artists (id, display_name, folded_name, kind, created_at, updated_at)
           VALUES ('ar1', 'A', 'a', 'person', 0, 0);
         INSERT INTO local_albums (id, root_id, grouping_key, title, title_folded,
           album_artist_id, grouping_source, created_at, updated_at)
           VALUES ('al1', 'r1', 'g', 'T', 't', 'ar1', 'automatic', 0, 0);
         UPDATE local_albums SET album_artist_name = 'A' WHERE id = 'al1';
         INSERT INTO local_album_external_identities (local_album_id, provider,
           release_group_mbid, release_mbid, decision_source, selected_at)
           VALUES ('al1', 'musicbrainz', '{MBID_A}', 'rel-x', 'automatic', 0);"
    );
    db.write("test.seed_library", move |tx| {
        tx.execute_batch(&seed)?;
        Ok(())
    })
    .await
    .unwrap();
    state.edition_plans = Some(Arc::new(NoPlan));
    let uri = format!("/api/v3/albums/{MBID_A}/edition/acquire?album_id=al1");
    let (status, body) = call(&state, Method::POST, &uri, Some(ADMIN), None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let calls = dispatch.take_calls().await;
    assert_eq!(calls.len(), 1, "{body}");
    assert_eq!(calls[0].release_mbid.as_deref(), Some("rel-x"));
    assert_eq!(
        calls[0].planned_album,
        Some(PlannedAlbum {
            album_id: "al1".into(),
            release_mbid: "rel-x".into(),
            replace: Vec::new(),
        })
    );
}

// With no download source set up, "Get missing tracks" is held like any
// request: the download is queued for the worker, which waits for a source,
// and the answer says so. It is not refused.
#[tokio::test]
async fn edition_acquire_is_held_while_no_source_is_ready() {
    let (mut state, db, dispatch) = setup().await;
    seed_edition_album(&db).await;
    state.edition_plans = Some(Arc::new(PlannedEdition { complete: false }));
    let staging = crate::common::ScratchDir::new("edition-held");
    state.readiness = Some(AcquireReadiness {
        sources: droppedneedle_acquire::readiness::SourceReadiness::unprobed(
            droppedneedle_acquire::downloads::worker::SourceList::Fixed(Arc::new(Vec::new())),
        ),
        staging_root: staging.to_path_buf(),
    });
    let uri = format!("/api/v3/albums/{MBID_A}/edition/acquire?album_id=al1");
    let (status, body) = call(&state, Method::POST, &uri, Some(ADMIN), None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["status"], "started", "{body}");
    assert!(
        body["message"]
            .as_str()
            .is_some_and(|message| message.contains("waits")),
        "{body}"
    );
    assert_eq!(dispatch.take_calls().await.len(), 1, "queued, not refused");
}

/// The library's plan for album "al1" as its files stand: tracks 1 to 4,
/// each held by file "t{n}" once the library has it.
struct LibraryPlan {
    db: AcquireDb,
}

impl droppedneedle_library::operations::port::EditionPlans for LibraryPlan {
    fn group_plan<'a>(
        &'a self,
        _album_id: Option<&'a str>,
        _release_group_mbid: &'a str,
    ) -> droppedneedle_library::operations::port::BoxFuture<
        'a,
        Result<
            Option<droppedneedle_library::operations::plan::EditionPlan>,
            droppedneedle_library::operations::models::OperationError,
        >,
    > {
        use droppedneedle_library::operations::plan::{
            EditionFacts, EditionPlan, FileFate, PlanFile, PlanTrack,
        };
        Box::pin(async move {
            let files: Vec<String> =
                sqlx::query_scalar("SELECT id FROM local_tracks WHERE local_album_id = 'al1'")
                    .fetch_all(self.db.db().sqlx())
                    .await
                    .expect("library files read");
            let tracks = (1..=4)
                .map(|position: u32| {
                    let file = format!("t{position}");
                    PlanTrack {
                        disc: 1,
                        position,
                        title: format!("T{position}"),
                        recording_mbid: format!("rec-{position}"),
                        release_track_mbid: format!("rt-{position}"),
                        length_ms: Some(200_000),
                        local_track_id: files.contains(&file).then_some(file),
                    }
                })
                .collect();
            let files = files
                .into_iter()
                .map(|id| PlanFile {
                    local_track_id: id,
                    title: String::new(),
                    disc: 1,
                    track: 0,
                    length_ms: None,
                    fate: FileFate::Keep,
                    edition_track: None,
                    note: None,
                })
                .collect();
            Ok(Some(EditionPlan {
                local_album_id: "al1".into(),
                release_group_mbid: MBID_A.into(),
                release_mbid: "rel-1".into(),
                title: "Album".into(),
                artist_name: "Artist".into(),
                is_current: true,
                same_album: true,
                edition: EditionFacts::default(),
                tracks,
                files,
            }))
        })
    }
}

/// A landing of `task_id` as the worker records it: `outcome` with the
/// release positions still `missing`, file `t{landed}` joining album "al1",
/// and the task settled at `status`.
async fn land_edition(
    db: &AcquireDb,
    downloads: &Downloads,
    task_id: &str,
    (outcome, missing): (&str, &str),
    landed: u32,
    status: TaskStatus,
) {
    let row = droppedneedle_acquire::downloads::landing_rows::ImportDecisionRow {
        task_id: task_id.to_owned(),
        outcome: outcome.to_owned(),
        files_total: 1,
        files_imported: 1,
        checks_json: "[]".to_owned(),
        missing_positions: missing.to_owned(),
        decided_at: 1_700_000_000.0,
        ..Default::default()
    };
    downloads
        .dispatch
        .journal()
        .run("test.landing", move |store| {
            store.record_import_decision(&row)
        })
        .await
        .expect("landing recorded");
    let file = format!(
        "INSERT INTO local_tracks (id, local_album_id, root_id, file_path, relative_path,
           path_hash, file_size_bytes, file_mtime_ns, stat_revision, title, title_folded,
           album_title, album_title_folded, file_format, bit_rate, ingest_source, imported_at,
           membership_source)
         VALUES ('t{landed}', 'al1', 'r1', '/m/{landed}.flac', '{landed}.flac', 'h{landed}', 1,
           0, 's', 'T', 't', 'T', 't', 'flac', NULL, 'scan', 0, 'automatic');"
    );
    db.write("test.landed_file", move |tx| {
        tx.execute_batch(&file)?;
        Ok(())
    })
    .await
    .unwrap();
    downloads.settle(task_id, status).await;
}

/// `(disc, position, state)` of each track the edition fetch asked for.
fn fetch_tracks(body: &Value) -> Vec<(u64, u64, String)> {
    body["latest"]["tracks"]
        .as_array()
        .expect("tracks")
        .iter()
        .map(|track| {
            (
                track["disc"].as_u64().unwrap(),
                track["position"].as_u64().unwrap(),
                track["state"].as_str().unwrap().to_owned(),
            )
        })
        .collect()
}

// The edition fetch shows each track it asked for, after the sheet that
// started it is gone: a short landing leaves one track landed and one
// failed, a retry asks only for the failed one, and once that lands the
// album lacks nothing.
#[tokio::test]
async fn an_edition_fetch_shows_its_tracks_and_retries_what_failed() {
    let (mut state, db, downloads) = setup().await;
    seed_edition_album(&db).await;
    state.edition_plans = Some(Arc::new(LibraryPlan { db: db.clone() }));
    let uri = format!("/api/v3/albums/{MBID_A}/edition/acquire");
    let (status, body) = call(&state, Method::GET, &uri, Some(ADMIN), None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["latest"], Value::Null, "no fetch yet: {body}");

    let asked = format!("{uri}?album_id=al1");
    let (status, body) = call(&state, Method::POST, &asked, Some(ADMIN), None).await;
    assert_eq!(
        (status, &body["status"]),
        (StatusCode::OK, &json!("started"))
    );
    let first = downloads.take_calls().await.remove(0);
    assert_eq!(first.wanted, vec![(1, 2), (1, 4)]);

    let (status, body) = call(&state, Method::GET, &uri, Some(TRUSTED), None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["latest"]["task_id"], first.id.as_str());
    assert_eq!(body["latest"]["state"], "waiting");
    assert_eq!(
        fetch_tracks(&body),
        vec![(1, 2, "waiting".into()), (1, 4, "waiting".into())]
    );
    assert_eq!(
        (&body["latest"]["can_stop"], &body["latest"]["can_retry"]),
        (&json!(true), &json!(false))
    );

    // Track 2 lands; no source had track 4.
    land_edition(
        &db,
        &downloads,
        &first.id,
        ("partial", "[[1,4]]"),
        2,
        TaskStatus::Partial,
    )
    .await;
    let (_, body) = call(&state, Method::GET, &uri, Some(ADMIN), None).await;
    assert_eq!(body["latest"]["state"], "failed", "{body}");
    assert_eq!(
        fetch_tracks(&body),
        vec![(1, 2, "landed".into()), (1, 4, "failed".into())]
    );
    assert_eq!(
        (&body["latest"]["can_stop"], &body["latest"]["can_retry"]),
        (&json!(false), &json!(true))
    );

    // Retry is the same acquire again: it asks only for track 4.
    let (status, body) = call(&state, Method::POST, &asked, Some(ADMIN), None).await;
    assert_eq!(
        (status, &body["status"]),
        (StatusCode::OK, &json!("started"))
    );
    let retry = downloads.take_calls().await.remove(0);
    assert_eq!(retry.wanted, vec![(1, 4)], "only what is still missing");
    let (_, body) = call(&state, Method::GET, &uri, Some(ADMIN), None).await;
    assert_eq!(body["latest"]["task_id"], retry.id.as_str());
    assert_eq!(fetch_tracks(&body), vec![(1, 4, "waiting".into())]);

    land_edition(
        &db,
        &downloads,
        &retry.id,
        ("imported", "[]"),
        4,
        TaskStatus::Completed,
    )
    .await;
    let (_, body) = call(&state, Method::GET, &uri, Some(ADMIN), None).await;
    assert_eq!(body["latest"]["state"], "done", "{body}");
    assert_eq!(fetch_tracks(&body), vec![(1, 4, "landed".into())]);
    let (_, body) = call(&state, Method::POST, &asked, Some(ADMIN), None).await;
    assert_eq!(
        body["status"], "already_complete",
        "both tracks landed: {body}"
    );
}

// The badge count reads without writing, even with a row whose task
// finished behind the journal's back. `Db::commits()` moves only when a
// commit changed rows, so an unchanged count proves the read wrote none.
#[tokio::test]
async fn active_count_issues_no_write() {
    let (state, db, dispatch) = setup().await;
    ask(&state, TRUSTED, MBID_A).await;
    let task = dispatch.task(1).await;
    db.write("test.complete", move |tx| {
        tx.execute(
            "UPDATE download_tasks SET status = 'completed' WHERE id = ?1",
            [task],
        )?;
        Ok(())
    })
    .await
    .unwrap();
    let commits = db.db().commits();
    let before = *commits.borrow();
    let (status, body) = call(
        &state,
        Method::GET,
        "/api/v3/requests/active/count",
        Some(TRUSTED),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let after = *commits.borrow();
    assert_eq!(after, before, "the badge read wrote");
}

/// An album ask with `monitor_artist` and `auto_download_artist`.
fn ask_to_follow(album: &str, artist: &str, auto_download: bool) -> Value {
    json!({
        "musicbrainz_id": album, "artist": "Artist", "album": "Album",
        "artist_mbid": artist, "monitor_artist": true, "auto_download_artist": auto_download,
    })
}

// A request that asks to follow its artist follows it in the request's own
// write: a plain user's auto-download waits for an admin, a repeat ask on a
// live row follows too, a batch follows each artist, and a request the
// quota refuses leaves no follow behind.
#[tokio::test]
async fn a_request_that_asks_to_follow_follows_the_artist() {
    let (_, db, dispatch) = setup().await;
    let limited = QuotaPolicy {
        request_count: 1,
        ..QuotaPolicy::default()
    };
    let state = rebuilt(&db, limited, &dispatch);
    let albums = "/api/v3/requests/albums";
    let body = ask_to_follow(MBID_A, ARTIST, true);
    let (status, _) = call(&state, Method::POST, albums, Some(ADA), Some(body)).await;
    assert_eq!(status, StatusCode::ACCEPTED);
    let follow = state.follows.status("u-ada", ARTIST).await.unwrap();
    assert!(follow.followed);
    assert_eq!(
        serde_json::to_value(follow.auto_download_state).unwrap(),
        "pending"
    );

    let refused = ask_to_follow(MBID_B, MBID_C, false);
    let (status, _) = call(&state, Method::POST, albums, Some(ADA), Some(refused)).await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
    assert!(
        !state
            .follows
            .status("u-ada", MBID_C)
            .await
            .unwrap()
            .followed,
        "a refused request follows nobody"
    );

    let again = ask_to_follow(MBID_A, ARTIST, false);
    call(&state, Method::POST, albums, Some(BOB), Some(again)).await;
    assert!(
        state
            .follows
            .status("u-bob", ARTIST)
            .await
            .unwrap()
            .followed
    );

    let batch = json!({
        "items": [{"musicbrainz_id": MBID_B, "artist_name": "Other", "album_title": "Album",
                   "artist_mbid": MBID_C}],
        "monitor_artist": true,
        "auto_download_artist": true,
    });
    let batches = "/api/v3/requests/batches";
    call(&state, Method::POST, batches, Some(TRUSTED), Some(batch)).await;
    let follow = state.follows.status("u-tris", MBID_C).await.unwrap();
    assert!(
        follow.followed && follow.auto_download,
        "a trusted ask arms itself"
    );
}
