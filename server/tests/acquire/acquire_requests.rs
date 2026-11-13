//! Request intake over the durable ledger: the approval gate, co-requesters,
//! quotas, the wanted and approval queues, and restart survival.
//!
//! Each test mounts the requests routes, signed in through the test sign-in
//! layer, over a scratch database. Asks dispatch into the real download
//! journal on the same database; a test settles a task the way the worker
//! would.

use crate::common::Probe;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use droppedneedle_acquire::requests::mix::{
    MixGrantHooks, MixSources, PersonalMixBuilder, SkipReason,
};
use droppedneedle_catalog::collections::db::CollectionsDb;
use droppedneedle_catalog::collections::store::playlists::PlaylistStore;
use droppedneedle_kernel::events::EventHub;
use droppedneedle_kernel::runtime_config::sections::DownloadPolicy;
use droppedneedle_kernel::runtime_config::{ConfigStore, Live};
use droppedneedle_media::playback::listening::{ListeningStore, PrefsPatch};
use droppedneedle_media::playback::listening_routes::MixGrants as _;
use droppedneedle_media::playback::models::MixGrantState;
use droppedneedle_providers::listenbrainz::playlists::{
    RecommendationPlaylist, RecommendationTrack,
};
use droppedneedle_providers::listenbrainz::{ListenBrainzCredentials, SimilarArtist, TopRecording};
use futures_util::future::BoxFuture;

use axum::{
    Router,
    body::Body,
    http::{Method, Request, StatusCode},
};
use droppedneedle_acquire::db::AcquireDb;
use droppedneedle_acquire::dispatch::DownloadDispatch;
use droppedneedle_acquire::downloads::state::{AttemptState, TaskStatus};
use droppedneedle_acquire::downloads::{PlannedAlbum, ReplaceVersion};
use droppedneedle_acquire::follows::ImportArtist;
use droppedneedle_acquire::requests::{
    self, MixRoutes, RequestsState,
    ledger::{WantedWatch, WatchKind, WatchOutcome, WatchState},
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

/// The requests state of a production acquire bundle on a scratch
/// database, with Soulseek on and no quota. Returns the state, the database (to seed users
/// and rows) and the dispatch (to read and settle its tasks).
pub(crate) async fn requests_state() -> (RequestsState, AcquireDb, Arc<DownloadDispatch>) {
    let db = AcquireDb::scratch().expect("scratch db");
    let dir = db.path().parent().expect("scratch dir").to_path_buf();
    let platform = crate::common::TestPlatform::on(db.db(), &dir).build().await;
    // A download source is on, so asks start at once.
    crate::common::acquire::save_slskd(&platform.settings, "http://127.0.0.1:9");
    let (library, _) = crate::common::library::library_at(&platform);
    let acquire = crate::common::bundles::acquire_at(
        &platform,
        crate::common::auth::AuthRig::on(&platform).deps,
        &library,
        &crate::common::bundles::collections_at(&platform).stores,
    );
    let quota = Arc::new(QuotaLedger::unlimited(acquire.db.clone()).expect("quota ledger"));
    let state = RequestsState {
        quota,
        ..acquire.requests.clone()
    };
    (state, db, acquire.dispatch)
}

/// State over a scratch database with every test user present.
async fn setup() -> (RequestsState, AcquireDb, Downloads) {
    let (state, db, dispatch) = requests_state().await;
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
    fn ids(&self) -> Vec<String> {
        self.dispatch.journal().db().db().column(
            "SELECT id FROM download_tasks ORDER BY created_at, rowid",
            [],
        )
    }

    /// The `n`th task dispatched (1-based).
    fn task(&self, n: usize) -> String {
        self.ids()[n - 1].clone()
    }

    /// The tasks dispatched since the last call, oldest first.
    async fn take_calls(&self) -> Vec<Dispatched> {
        let ids = self.ids();
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
    fn take_cancels(&self) -> Vec<String> {
        let cancelled: Vec<String> = self.dispatch.journal().db().db().column(
            "SELECT id FROM download_tasks WHERE status = 'cancelled' \
             ORDER BY updated_at, rowid",
            [],
        );
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
    let store = droppedneedle_acquire::test_support::scratch_settings(&dir).expect("settings open");
    store.save_blocking(policy).expect("policy saves");
    store.live()
}

/// A fresh state over the same database and `state`'s ports: what a
/// restart sees.
fn rebuilt(
    state: &RequestsState,
    db: &AcquireDb,
    quota: QuotaPolicy,
    downloads: &Downloads,
) -> RequestsState {
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
    RequestsState {
        quota: Arc::new(QuotaLedger::new(policy, db.clone())),
        dispatch: downloads.dispatch.clone(),
        ..state.clone()
    }
}

/// Every requests route plus the follows' auto-download approval routes.
fn router(state: &RequestsState) -> Router {
    let follows = droppedneedle_acquire::follows::routes()
        .with_state(state.follows.clone())
        .into_router();
    requests::routes()
        .with_state(state.clone())
        .into_router()
        .merge(follows)
}

async fn call(
    state: &RequestsState,
    method: Method,
    uri: &str,
    identity: Option<Who>,
    body: Option<Value>,
) -> (StatusCode, Value) {
    call_on(router(state), method, uri, identity, body).await
}

/// [`call`] on the Weekly Mix routes over `mix`.
async fn call_mix(
    mix: &MixRoutes,
    method: Method,
    uri: &str,
    identity: Option<Who>,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let app = requests::mix_routes().with_state(mix.clone()).into_router();
    call_on(app, method, uri, identity, body).await
}

async fn call_on(
    mut app: Router,
    method: Method,
    uri: &str,
    identity: Option<Who>,
    body: Option<Value>,
) -> (StatusCode, Value) {
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
        kind: WatchKind::Missing,
        state: WatchState::Watching,
        created_at: 1_700_000_000,
        first_release_date: None,
        check_count: 0,
        quiet_streak: 0,
        next_check_at: 1_700_000_000,
        new_candidate_count: 2,
        last_outcome: None,
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

    let after = rebuilt(&state, &db, QuotaPolicy::default(), &dispatch);
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

// One task per album however it is asked for, owned by the requester: an
// approval (a second one dispatches nothing), a duplicate ask (a listener)
// and a batch (raw and live duplicates skipped).
#[tokio::test]
async fn an_album_dispatches_once_however_it_is_asked() {
    // A user's ask, an admin's approval, dispatch and landing: imported.
    let case = "approval";
    let (state, _db, dispatch) = setup().await;
    ask(&state, ADA, MBID_A).await;
    assert!(dispatch.take_calls().await.is_empty(), "{case}");
    let uri = format!("/api/v3/requests/approvals/{MBID_A}/approve");
    let (status, body) = call(&state, Method::POST, &uri, Some(ADMIN), None).await;
    assert_eq!(status, StatusCode::OK, "{case}: {body}");
    let calls = dispatch.take_calls().await;
    assert_eq!(calls.len(), 1, "{case}");
    assert_eq!(
        calls[0].user_id, "u-ada",
        "{case}: the owner, never the approver"
    );
    // A second approval loses the claim instead of dispatching again.
    let (_, again) = call(&state, Method::POST, &uri, Some(ADMIN), None).await;
    assert_eq!(again["success"], false, "{case}");
    assert!(dispatch.take_calls().await.is_empty(), "{case}");

    dispatch.land(&calls[0].id).await;
    let (_, active) = call(
        &state,
        Method::GET,
        "/api/v3/requests/active",
        Some(ADA),
        None,
    )
    .await;
    assert!(keys(&active).is_empty(), "{case}");
    let (_, history) = call(
        &state,
        Method::GET,
        "/api/v3/requests/history",
        Some(ADA),
        None,
    )
    .await;
    assert_eq!(history["items"][0]["status"], "imported", "{case}");
    assert_eq!(history["items"][0]["reviewed_by_name"], "Root", "{case}");

    // A duplicate ask attaches as a listener: one dispatch, one row.
    let case = "duplicate ask";
    let (state, _db, dispatch) = setup().await;
    ask(&state, TRUSTED, MBID_A).await;
    let (status, body) = ask(&state, BOB, MBID_A).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{case}");
    assert_eq!(body["message"], "Request already in progress", "{case}");
    let calls = dispatch.take_calls().await;
    assert_eq!(calls.len(), 1, "{case}");
    let (_, all) = call(
        &state,
        Method::GET,
        "/api/v3/requests/active",
        Some(ADMIN),
        None,
    )
    .await;
    assert_eq!(all["count"], 1, "{case}");
    assert_eq!(all["items"][0]["task_id"], calls[0].id.as_str(), "{case}");

    // The listener reads the download's own state on the shared row, though
    // the task is the owner's and not in the listener's own task list.
    let active = "/api/v3/requests/active";
    let (_, bobs) = call(&state, Method::GET, active, Some(BOB), None).await;
    assert_eq!(
        bobs["items"][0]["task_state"], "searching",
        "{case}: {bobs}"
    );
    dispatch.settle(&calls[0].id, TaskStatus::Downloading).await;
    let (_, bobs) = call(&state, Method::GET, active, Some(BOB), None).await;
    assert_eq!(
        bobs["items"][0]["task_state"], "downloading",
        "{case}: {bobs}"
    );

    // A batch skips raw and live duplicates and dispatches the new rows.
    let case = "batch";
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
    assert_eq!(status, StatusCode::ACCEPTED, "{case}: {body}");
    assert_eq!(body["requested"], 2, "{case}");
    assert_eq!(body["skipped"], 2, "{case}");
    assert_eq!(dispatch.take_calls().await.len(), 2, "{case}");
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

// A fresh curator's ask is fetched in the edition the page asked for. A
// curator's other edition switches a live request while no download serves
// it; a plain user's ask never does, and once the download has started the
// request keeps its edition.
#[tokio::test]
async fn a_request_switches_edition_only_before_its_download_starts() {
    let (state, _db, dispatch) = setup().await;

    // An album the library does not hold is fetched in the edition the page
    // asked for, not the catalog's preferred one.
    let case = "fresh trusted ask";
    assert_eq!(
        ask_edition(&state, TRUSTED, MBID_B, REL_2).await["status"],
        "pending",
        "{case}"
    );
    let calls = dispatch.take_calls().await;
    assert_eq!(calls.len(), 1, "{case}");
    assert_eq!(calls[0].release_mbid.as_deref(), Some(REL_2), "{case}");
    assert_eq!(requested_edition(&state, MBID_B).await, REL_2, "{case}");

    let case = "switch before the download";
    ask_edition(&state, ADA, MBID_A, REL_1).await;
    ask_edition(&state, BOB, MBID_A, REL_3).await;
    assert_eq!(requested_edition(&state, MBID_A).await, REL_1, "{case}");
    let answer = ask_edition(&state, TRUSTED, MBID_A, REL_2).await;
    assert_eq!(answer["status"], "awaiting_approval", "{case}: {answer}");
    assert_eq!(
        answer["message"], "The request now gets this edition",
        "{case}"
    );
    assert_eq!(requested_edition(&state, MBID_A).await, REL_2, "{case}");

    let uri = format!("/api/v3/requests/approvals/{MBID_A}/approve");
    let (status, body) = call(&state, Method::POST, &uri, Some(ADMIN), None).await;
    assert_eq!(status, StatusCode::OK, "{case}: {body}");
    let calls = dispatch.take_calls().await;
    assert_eq!(calls.len(), 1, "{case}");
    assert_eq!(calls[0].release_mbid.as_deref(), Some(REL_2), "{case}");

    let case = "after the download started";
    let answer = ask_edition(&state, TRUSTED, MBID_A, REL_3).await;
    assert_eq!(answer["status"], "downloading", "{case}: {answer}");
    assert_eq!(
        answer["message"], "Already downloading, so the edition stays as it is",
        "{case}"
    );
    assert_eq!(requested_edition(&state, MBID_A).await, REL_2, "{case}");
    assert!(dispatch.take_calls().await.is_empty(), "{case}");
}

// Listeners detach and owners or an admin stop the task; a cancel outlasts
// the automatic retry; a retry dispatches only with approval provenance.
// Another user's cancel and retry answer 404 in the ownership table.
#[tokio::test]
async fn cancel_and_retry_respect_owner_and_approval() {
    let case = "cancel";
    let (state, _db, dispatch) = setup().await;
    ask(&state, TRUSTED, MBID_A).await;
    ask(&state, BOB, MBID_A).await;
    let uri = format!("/api/v3/requests/active/{MBID_A}");
    let (_, detached) = call(&state, Method::DELETE, &uri, Some(BOB), None).await;
    assert_eq!(detached["success"], true, "{case}");
    assert!(
        dispatch.take_cancels().is_empty(),
        "{case}: a listener never stops the task"
    );
    let (_, cancelled) = call(&state, Method::DELETE, &uri, Some(TRUSTED), None).await;
    assert_eq!(cancelled["success"], true, "{case}: {cancelled}");
    assert_eq!(dispatch.take_cancels(), vec![dispatch.task(1)], "{case}");
    let (_, history) = call(
        &state,
        Method::GET,
        "/api/v3/requests/history",
        Some(TRUSTED),
        None,
    )
    .await;
    assert_eq!(history["items"][0]["status"], "cancelled", "{case}");

    // An admin stops someone else's download: stopping the task settles
    // the row, and the cancel answers success.
    let case = "admin cancel";
    ask(&state, TRUSTED, MBID_B).await;
    let uri = format!("/api/v3/requests/active/{MBID_B}");
    let (_, stopped) = call(&state, Method::DELETE, &uri, Some(ADMIN), None).await;
    assert_eq!(stopped["success"], true, "{case}: {stopped}");
    assert_eq!(dispatch.take_cancels(), vec![dispatch.task(2)], "{case}");
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
    assert_eq!(stopped["status"], "cancelled", "{case}: {history}");

    // A requester's cancel that meets a download which has just failed stops
    // it for good: the automatic retry neither starts the album again nor
    // moves the request back. Retrying the stopped download by hand brings
    // the request back with it.
    let case = "cancel outlasts the automatic retry";
    let (state, db, dispatch) = setup().await;
    ask(&state, TRUSTED, MBID_A).await;
    let first = dispatch.task(1);
    // The download fails as the cancel arrives, before the request follows.
    let failed = first.clone();
    db.write("test.fail_behind", move |tx| {
        tx.execute(
            "UPDATE download_tasks SET status = 'failed', completed_at = 1000 WHERE id = ?1",
            rusqlite::params![failed],
        )?;
        Ok(())
    })
    .await
    .unwrap();
    let uri = format!("/api/v3/requests/active/{MBID_A}");
    let (_, cancelled) = call(&state, Method::DELETE, &uri, Some(TRUSTED), None).await;
    assert_eq!(cancelled["success"], true, "{case}: {cancelled}");

    let journal = dispatch.dispatch.journal().clone();
    let settings_dir = db
        .path()
        .parent()
        .expect("scratch dir")
        .join("worker-settings");
    let settings = droppedneedle_acquire::test_support::scratch_settings(&settings_dir)
        .expect("settings open");
    let (worker, _) = crate::common::acquire::worker_over(
        &db,
        journal.clone(),
        &settings,
        settings_dir.join("slskd"),
        Arc::new(crate::common::acquire::FixedAlbums::default()),
        None,
        droppedneedle_kernel::events::EventHub::new(),
    )
    .await;
    worker.run_once(1).await;
    let row = state.store.get(RequestKind::Album, MBID_A).await.unwrap();
    assert_eq!(row.unwrap().status.as_str(), "cancelled", "{case}");
    assert_eq!(
        dispatch.ids(),
        std::slice::from_ref(&first),
        "{case}: nothing starts again"
    );

    let source = journal.read_task(&first).await.unwrap().unwrap();
    let successor = worker
        .retry_now(&source)
        .await
        .expect("a hand retry starts");
    let row = state.store.get(RequestKind::Album, MBID_A).await.unwrap();
    let row = row.unwrap();
    assert_eq!(row.status.as_str(), "downloading", "{case}");
    assert_eq!(row.task_id.as_deref(), Some(successor.as_str()), "{case}");

    // A retry dispatches only with approval provenance; otherwise it queues.
    let case = "retry provenance";
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
    dispatch.fail(&dispatch.task(1)).await;
    dispatch.take_calls().await;

    let (_, retried) = call(
        &state,
        Method::POST,
        &format!("/api/v3/requests/retry/{MBID_A}"),
        Some(TRUSTED),
        None,
    )
    .await;
    assert_eq!(retried["success"], true, "{case}: {retried}");
    assert_eq!(dispatch.take_calls().await.len(), 1, "{case}");
    // A rejected row is not retryable at all.
    let (_, rejected) = call(
        &state,
        Method::POST,
        &format!("/api/v3/requests/retry/{MBID_B}"),
        Some(ADA),
        None,
    )
    .await;
    assert_eq!(rejected["success"], false, "{case}");
    assert!(dispatch.take_calls().await.is_empty(), "{case}");
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
    let store = droppedneedle_acquire::test_support::scratch_settings(dir).expect("settings open");
    assert!(store.live::<DownloadPolicy>().get().is_err());
    store
}

// Quotas count durable rows and never read as unlimited: the request count
// survives a restart and its refusal writes nothing, a download policy that
// does not read refuses until saved, and the library cap reads the library.
#[tokio::test]
async fn quotas_count_durable_rows_and_never_read_as_unlimited() {
    let case = "restart";
    let (state, db, dispatch) = setup().await;
    let limited = QuotaPolicy {
        request_count: 1,
        ..QuotaPolicy::default()
    };
    let state = rebuilt(&state, &db, limited.clone(), &dispatch);
    assert_eq!(
        ask(&state, ADA, MBID_A).await.0,
        StatusCode::ACCEPTED,
        "{case}"
    );

    let state = rebuilt(&state, &db, limited, &dispatch);
    let (status, body) = ask(&state, ADA, MBID_B).await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS, "{case}");
    assert_eq!(body["error"]["code"], "QUOTA_EXCEEDED", "{case}");
    assert_eq!(body["error"]["details"]["used"], 1, "{case}");
    // Curators are exempt.
    assert_eq!(
        ask(&state, TRUSTED, MBID_B).await.0,
        StatusCode::ACCEPTED,
        "{case}"
    );

    // A download policy that does not read is never "unlimited": a request
    // answers 500 and leaves no request or task behind. Saving the policy
    // again brings the quota back without a restart.
    let case = "unreadable policy";
    let (base, db, dispatch) = setup().await;
    let dir = db
        .path()
        .parent()
        .expect("scratch dir")
        .join("broken-settings");
    let store = unreadable_policy_store(&dir);
    let state = RequestsState {
        quota: Arc::new(QuotaLedger::new(store.live(), db.clone())),
        dispatch: dispatch.dispatch.clone(),
        ..base
    };

    let (status, _) = ask(&state, ADA, MBID_A).await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR, "{case}");
    let (_, history) = call(
        &state,
        Method::GET,
        "/api/v3/requests/history",
        Some(ADA),
        None,
    )
    .await;
    assert_eq!(history["items"], json!([]), "{case}");
    assert!(dispatch.take_calls().await.is_empty(), "{case}");

    store
        .save_blocking(DownloadPolicy {
            default_request_quota_count: 1,
            ..DownloadPolicy::default()
        })
        .expect("policy saves");
    assert_eq!(
        ask(&state, ADA, MBID_A).await.0,
        StatusCode::ACCEPTED,
        "{case}"
    );
    assert_eq!(
        ask(&state, ADA, MBID_B).await.0,
        StatusCode::TOO_MANY_REQUESTS,
        "{case}"
    );

    // The library cap reads the real library size.
    let case = "library cap";
    let (base, db, dispatch) = setup().await;
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
        &base,
        &db,
        QuotaPolicy {
            max_library_gb: 1,
            ..QuotaPolicy::default()
        },
        &dispatch,
    );
    let (status, body) = ask(&state, ADMIN, MBID_A).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{case}");
    assert_eq!(body["error"]["code"], "STORAGE_FULL", "{case}");
}

// A watch stops and resumes, a fulfilled one refuses to stop, the retrying
// rows come from the auto-retry schedule, removing a watch takes its seen
// candidates with it, and clearing takes only the caller's finished watches.
// Another user's stop and remove answer 404 in the ownership table.
#[tokio::test]
async fn wanted_watches_stop_resume_remove_and_clear() {
    const MBID_D: &str = "44444444-4444-4444-8444-444444444444";
    const MBID_E: &str = "55555555-5555-4555-8555-555555555555";

    let case = "stop and resume";
    let (state, _db, dispatch) = setup().await;
    state.wanted.enrol(watch(MBID_A, "u-ada")).await.unwrap();
    let stop = format!("/api/v3/requests/wanted/{MBID_A}/stop");
    let (_, stopped) = call(&state, Method::POST, &stop, Some(ADA), None).await;
    assert_eq!(stopped["state"], "stopped", "{case}");
    let (_, resumed) = call(
        &state,
        Method::POST,
        &format!("/api/v3/requests/wanted/{MBID_A}/resume"),
        Some(ADMIN),
        None,
    )
    .await;
    assert_eq!(resumed["state"], "watching", "{case}");
    state
        .store
        .settle_in_library(MBID_A, WatchOutcome::InLibrary, 1_700_000_100)
        .await
        .unwrap();
    assert_eq!(
        call(&state, Method::POST, &stop, Some(ADA), None).await.0,
        StatusCode::BAD_REQUEST,
        "{case}: a fulfilled watch refuses"
    );

    let case = "retrying";
    ask(&state, TRUSTED, MBID_B).await;
    dispatch.fail(&dispatch.task(1)).await;
    let (_, wanted) = call(
        &state,
        Method::GET,
        "/api/v3/requests/wanted",
        Some(TRUSTED),
        None,
    )
    .await;
    // The default policy retries a failed download six times; none spent.
    assert_eq!(wanted["retrying"][0]["musicbrainz_id"], MBID_B, "{case}");
    assert_eq!(wanted["retrying"][0]["retry_count"], 0, "{case}");
    assert_eq!(wanted["retrying"][0]["max_attempts"], 6, "{case}");

    // Removing a watch takes its seen candidates with it, and an active one
    // goes in one step. Clearing takes only the finished watches the caller
    // can see.
    let case = "remove and clear";
    let (state, _db, _dispatch) = setup().await;
    state.wanted.enrol(watch(MBID_A, "u-ada")).await.unwrap();
    state
        .wanted
        .add_seen(MBID_A, vec![("slskd".into(), "peer".into())], 1_700_000_000)
        .await
        .unwrap();
    let remove = format!("/api/v3/requests/wanted/{MBID_A}");
    let (status, removed) = call(&state, Method::DELETE, &remove, Some(ADA), None).await;
    assert_eq!(status, StatusCode::OK, "{case}");
    assert_eq!(removed["removed"], 1, "{case}");
    assert!(state.wanted.get(MBID_A).await.unwrap().is_none(), "{case}");
    assert!(
        state.wanted.seen(MBID_A).await.unwrap().is_empty(),
        "{case}"
    );
    assert_eq!(
        call(&state, Method::DELETE, &remove, Some(ADA), None)
            .await
            .0,
        StatusCode::NOT_FOUND,
        "{case}"
    );

    for (key, user) in [
        (MBID_B, "u-ada"),
        (MBID_C, "u-ada"),
        (MBID_D, "u-ada"),
        (MBID_E, "u-bob"),
    ] {
        state.wanted.enrol(watch(key, user)).await.unwrap();
    }
    for (key, who) in [(MBID_B, ADA), (MBID_E, BOB)] {
        let stop = format!("/api/v3/requests/wanted/{key}/stop");
        call(&state, Method::POST, &stop, Some(who), None).await;
    }
    state
        .store
        .settle_in_library(MBID_C, WatchOutcome::InLibrary, 1_700_000_100)
        .await
        .unwrap();
    let clear = "/api/v3/requests/wanted/clear-finished";
    let (_, cleared) = call(&state, Method::POST, clear, Some(ADA), None).await;
    assert_eq!(
        cleared["removed"], 2,
        "{case}: stopped and fulfilled, own only"
    );
    let (_, left) = call(
        &state,
        Method::GET,
        "/api/v3/requests/wanted",
        Some(ADMIN),
        None,
    )
    .await;
    assert_eq!(
        keys(&left),
        vec![MBID_D.to_owned(), MBID_E.to_owned()],
        "{case}"
    );
    let (_, cleared) = call(&state, Method::POST, clear, Some(ADMIN), None).await;
    assert_eq!(
        cleared["removed"], 1,
        "{case}: an admin's covers everyone's"
    );
    let (_, left) = call(
        &state,
        Method::GET,
        "/api/v3/requests/wanted",
        Some(ADMIN),
        None,
    )
    .await;
    assert_eq!(keys(&left), vec![MBID_D.to_owned()], "{case}: active stays");
}

// Standing grants: an auto-download ask waits for an admin (a single
// approve and a batch reject), the weekly mix grant builds and refreshes and
// follows the role, and a curator's pending mix ask is neither listed nor
// kept.
#[tokio::test]
async fn standing_grants_wait_for_an_admin_and_follow_the_role() {
    // Auto-download approvals: single approve, then a batch reject.
    let case = "auto-download";
    let (state, db, _dispatch) = setup().await;
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
        )
        .await
        .unwrap();
    assert_eq!(imported.auto_download_enabled, 2, "{case}");
    // A batch of asks arrives only with the upgrade from v2: seed one.
    db.write("seed batch", |tx| {
        tx.execute_batch(&format!(
            "INSERT INTO auto_download_approvals (user_id, artist_mbid, artist_mbid_lower, \
               artist_name, state, requested_at, batch_id, source) VALUES \
               ('u-bob', '{MBID_A}', lower('{MBID_A}'), 'One', 'pending', 1, 'batch-1', 'lidarr_import'), \
               ('u-bob', '{MBID_B}', lower('{MBID_B}'), 'Two', 'pending', 1, 'batch-1', 'lidarr_import');"
        ))?;
        Ok(())
    })
    .await
    .unwrap();
    let (_, count) = call(
        &state,
        Method::GET,
        "/api/v3/requests/approvals/count",
        Some(ADMIN),
        None,
    )
    .await;
    assert_eq!(count["count"], 2, "{case}: one single plus one batch");
    let approve = format!("/api/v3/requests/auto-download-approvals/u-ada/{ARTIST}/approve");
    assert_eq!(
        call(&state, Method::POST, &approve, Some(ADA), None)
            .await
            .0,
        StatusCode::FORBIDDEN,
        "{case}"
    );
    let (_, approved) = call(&state, Method::POST, &approve, Some(ADMIN), None).await;
    assert_eq!(approved["success"], true, "{case}");
    let status = state.follows.status("u-ada", ARTIST).await.unwrap();
    assert!(
        status.auto_download,
        "{case}: the verdict arms the follow with it"
    );
    assert_eq!(
        serde_json::to_value(status.auto_download_state).unwrap(),
        "approved",
        "{case}"
    );
    let (_, again) = call(&state, Method::POST, &approve, Some(ADMIN), None).await;
    assert_eq!(
        again["success"], false,
        "{case}: only a pending row decides"
    );
    let (_, rejected) = call(
        &state,
        Method::POST,
        "/api/v3/requests/auto-download-approval-batches/batch-1/reject",
        Some(ADMIN),
        None,
    )
    .await;
    assert_eq!(
        rejected["message"], "Auto-download rejected for 2 artists",
        "{case}"
    );
    let status = state.follows.status("u-bob", MBID_A).await.unwrap();
    assert!(
        status.followed && !status.auto_download,
        "{case}: a rejected batch keeps the follow and clears its intent"
    );

    // A grant stays listed for an admin until it is revoked.
    let list = "/api/v3/requests/auto-download-approvals";
    let (_, listed) = call(&state, Method::GET, list, Some(ADMIN), None).await;
    assert_eq!(
        listed["granted"][0]["artist_mbid"], ARTIST,
        "{case}: {listed}"
    );
    assert_eq!(listed["count"], 0, "{case}: a grant is not a pending ask");
    let revoke = format!("/api/v3/requests/auto-download-approvals/u-ada/{ARTIST}/revoke");
    let (_, revoked) = call(&state, Method::POST, &revoke, Some(ADMIN), None).await;
    assert_eq!(revoked["success"], true, "{case}");
    let (_, listed) = call(&state, Method::GET, list, Some(ADMIN), None).await;
    assert_eq!(listed["granted"], json!([]), "{case}: {listed}");

    // The weekly mix journey: the toggle queues an approval, the admin
    // grants it, a build writes the playlist and requests the missing albums
    // without a per-request approval wait, a reject turns the toggle off, and
    // the refresh route needs a ListenBrainz link.
    let case = "weekly mix";
    let (state, db, dispatch) = setup().await;
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
    let mix = MixRoutes {
        requests: state.clone(),
        mixer: builder.clone(),
    };
    let hooks = MixGrantHooks::new(builder.clone());

    prefs
        .update_prefs(
            "u-ada",
            &PrefsPatch {
                auto_request_personal_mix: Some(true),
                ..PrefsPatch::default()
            },
        )
        .await
        .unwrap();
    hooks.on_toggle("u-ada", Role::User, true).await;
    assert_eq!(
        hooks.state("u-ada", Role::User, true).await,
        MixGrantState::Pending,
        "{case}"
    );
    let approve = "/api/v3/requests/personal-mix-approvals/u-ada/approve";
    assert_eq!(
        call(&state, Method::POST, approve, Some(ADMIN), None)
            .await
            .1["success"],
        true,
        "{case}"
    );
    assert_eq!(
        hooks.state("u-ada", Role::User, true).await,
        MixGrantState::Approved,
        "{case}"
    );

    let built = builder.build_for_user("u-ada", false).await.unwrap();
    assert_eq!(
        (built.track_count, built.requested_albums, built.skipped),
        (2, 2, None),
        "{case}"
    );
    let playlist = built.playlist_id.expect("mix playlist");
    assert_eq!(
        playlists
            .owned_by("u-ada")
            .tracks(&playlist)
            .await
            .unwrap()
            .len(),
        2,
        "{case}"
    );
    assert_eq!(
        dispatch.take_calls().await.len(),
        2,
        "{case}: granted asks dispatch straight away"
    );
    let again = builder.build_for_user("u-ada", false).await.unwrap();
    assert_eq!(again.skipped, Some(SkipReason::Fresh), "{case}");

    // A trusted user holds the grant by role, so the toggle files no approval
    // row and the build requests straight away. Demoted to user, the grant is
    // gone and the build requests nothing.
    let toggle = PrefsPatch {
        auto_request_personal_mix: Some(true),
        ..PrefsPatch::default()
    };
    prefs.update_prefs("u-tris", &toggle).await.unwrap();
    hooks.on_toggle("u-tris", Role::Trusted, true).await;
    assert_eq!(
        hooks.state("u-tris", Role::Trusted, true).await,
        MixGrantState::Approved,
        "{case}"
    );
    let trusted = builder.build_for_user("u-tris", false).await.unwrap();
    assert_eq!(
        trusted.requested_albums, 2,
        "{case}: trusted asks need no approval row"
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
    assert_eq!(
        hooks.state("u-tris", Role::User, true).await,
        MixGrantState::None,
        "{case}"
    );
    let demoted = builder.build_for_user("u-tris", true).await.unwrap();
    assert_eq!(
        demoted.requested_albums, 0,
        "{case}: a demotion drops the grant"
    );

    let list = "/api/v3/requests/personal-mix-approvals";
    let (_, listed) = call(&state, Method::GET, list, Some(ADMIN), None).await;
    assert_eq!(listed["granted"][0]["user_id"], "u-ada", "{case}: {listed}");
    let revoke = "/api/v3/requests/personal-mix-approvals/u-ada/revoke";
    assert_eq!(
        call_mix(&mix, Method::POST, revoke, Some(ADMIN), None)
            .await
            .1["success"],
        true,
        "{case}"
    );
    let (_, listed) = call(&state, Method::GET, list, Some(ADMIN), None).await;
    assert_eq!(listed["granted"], json!([]), "{case}: {listed}");
    assert!(
        !prefs.prefs("u-ada").await.auto_request_personal_mix,
        "{case}: revoke turns the toggle off"
    );

    let refresh = "/api/v3/requests/personal-mix/refresh";
    assert_eq!(
        call_mix(&mix, Method::POST, refresh, Some(ADA), None)
            .await
            .1["status"],
        "started",
        "{case}"
    );
    assert_eq!(
        call_mix(&mix, Method::POST, refresh, Some(BOB), None)
            .await
            .0,
        StatusCode::BAD_REQUEST,
        "{case}: no ListenBrainz link"
    );

    // An admin's or a trusted user's Weekly Mix grant never waits for review:
    // a pending ask of theirs (filed before their role changed, or brought
    // over from v2) is not in the admin's list, and startup removes it.
    let case = "curator's pending mix ask";
    let (state, _db, _) = setup().await;
    state
        .mixes
        .file_pending("u-tris", 1_700_000_000)
        .await
        .unwrap();
    state
        .mixes
        .file_pending("u-ada", 1_700_000_001)
        .await
        .unwrap();

    let uri = "/api/v3/requests/personal-mix-approvals";
    let (_, listed) = call(&state, Method::GET, uri, Some(ADMIN), None).await;
    let users: Vec<&str> = listed["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item["user_id"].as_str().unwrap())
        .collect();
    assert_eq!(users, ["u-ada"], "{case}");

    let report = RequestsService::new(&state).recover().await.unwrap();
    assert_eq!(report.mix_asks_cleared, 1, "{case}");
    assert_eq!(state.mixes.state("u-tris").await.unwrap(), None, "{case}");
    assert_eq!(
        state.mixes.state("u-ada").await.unwrap(),
        Some(droppedneedle_acquire::requests::ledger::ApprovalState::Pending),
        "{case}"
    );
}

/// ListenBrainz stand-in: one weekly-jams playlist with two tracks on two
/// albums, no similar artists.
struct FakeMix;

const MIX_REC_A: &str = "aaaa0000-0000-4000-8000-000000000001";
const MIX_REC_B: &str = "aaaa0000-0000-4000-8000-000000000002";

impl MixSources for FakeMix {
    fn reads_on(&self) -> bool {
        true
    }
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
        _user_id: &'a str,
        _mbid: &'a str,
        _limit: usize,
    ) -> BoxFuture<'a, Result<Vec<SimilarArtist>, String>> {
        Box::pin(async { Ok(Vec::new()) })
    }
    fn top_recording<'a>(
        &'a self,
        _user_id: &'a str,
        _mbid: &'a str,
    ) -> BoxFuture<'a, Result<Option<TopRecording>, String>> {
        Box::pin(async { Ok(None) })
    }
}

/// Radiohead, Karma Police and OK Computer: the artist, recording and
/// album the recorded ListenBrainz answers are for.
const RADIOHEAD: &str = "a74b1b7f-71a5-4011-9441-d0b5e4122711";
const KARMA_POLICE: &str = "9e2ad5bc-c6f9-40d2-a36f-3122ee2072a3";
const OK_COMPUTER: &str = "b1392450-e666-3926-a536-22c65f834433";
/// The two artists the recorded LB Radio answer finds like Radiohead, each
/// with a top song of its own, the album ListenBrainz places it on and that
/// album's name.
const SIMILAR_TOPS: [(&str, &str, &str, &str); 2] = [
    (
        "ada7a83c-e3e1-40f1-93f9-3e73dbc9298a",
        "5e1a0000-0000-4000-8000-00000000000a",
        "a1b00000-0000-4000-8000-00000000000a",
        "Album A",
    ),
    (
        "f181961b-20f7-459e-89de-920ef03c7ed0",
        "5e1a0000-0000-4000-8000-00000000000b",
        "a1b00000-0000-4000-8000-00000000000b",
        "Album B",
    ),
];

/// A linked person's Weekly Mix is topped up with the top song of each
/// artist similar to the ones in it, on the album ListenBrainz places that
/// song on, and no song twice. ListenBrainz gives an artist's top songs only
/// to a linked account, so those reads and the album lookup carry the
/// person's own token; an artist's top albums are never asked for. An
/// unlinked person gets no mix and nothing is sent.
#[tokio::test]
async fn a_weekly_mix_tops_up_with_the_persons_own_link() {
    use droppedneedle_providers::listenbrainz::DEFAULT_BASE_URL;
    use droppedneedle_providers::test_support::{Scripted, ScriptedWire};

    let (state, db, _) = setup().await;
    let lb = |path: &str| format!("{DEFAULT_BASE_URL}/1/{path}");
    let wire = Arc::new(ScriptedWire::new());
    // The recording account has no recommended playlist, so these two
    // answers are written to the models' shape.
    let playlists = json!({"playlists": [{"playlist": {
        "identifier": "https://listenbrainz.org/playlist/jams",
        "extension": {"https://musicbrainz.org/doc/jspf#playlist": {"additional_metadata":
            {"algorithm_metadata": {"source_patch": "weekly-jams"}}}}}}]});
    let jams = json!({"playlist": {"title": "Weekly Jams", "track": [{
        "title": "Karma Police", "creator": "Radiohead", "album": "OK Computer",
        "identifier": [format!("https://musicbrainz.org/recording/{KARMA_POLICE}")],
        "extension": {"https://musicbrainz.org/doc/jspf#track": {"additional_metadata":
            {"artists": [{"artist_mbid": RADIOHEAD}]}}}}]}});
    wire.answer(
        &lb("user/ada-lb/playlists/recommendations"),
        200,
        &[],
        playlists.to_string(),
    );
    wire.answer(&lb("playlist/jams"), 200, &[], jams.to_string());
    // The recorded album lookup for Karma Police, with the similar artists'
    // songs added in its shape.
    let mut lookup: Value = serde_json::from_str(include_str!(
        "../fixtures/listenbrainz/recording_metadata_post.json"
    ))
    .unwrap();
    for (_, song, album, _) in SIMILAR_TOPS {
        let mut entry = lookup[KARMA_POLICE].clone();
        entry["release"]["release_group_mbid"] = json!(album);
        lookup[song] = entry;
    }
    wire.on(
        droppedneedle_providers::executor::Method::Post,
        &lb("metadata/recording/"),
        Scripted::Answer {
            status: 200,
            headers: Vec::new(),
            body: lookup.to_string().into_bytes(),
        },
    );
    let radio = include_str!("../fixtures/listenbrainz/lb_radio_artist.json");
    wire.answer(
        &lb(&format!("lb-radio/artist/{RADIOHEAD}")),
        200,
        &[],
        radio,
    );
    for (artist, song, _, release) in SIMILAR_TOPS {
        let top = json!([{"recording_name": format!("Song on {release}"),
            "artist_name": "Someone", "total_listen_count": 900, "recording_mbid": song,
            "release_name": release}]);
        wire.answer(
            &lb(&format!("popularity/top-recordings-for-artist/{artist}")),
            200,
            &[],
            top.to_string(),
        );
        // ListenBrainz would answer the artist's top albums too; the mix
        // never asks for them.
        wire.answer(
            &lb(&format!(
                "popularity/top-release-groups-for-artist/{artist}"
            )),
            200,
            &[],
            include_str!("../fixtures/listenbrainz/top_release_groups_radiohead.json"),
        );
    }
    let providers = droppedneedle_providers::Providers::unpaced_at(
        wire.clone(),
        droppedneedle_providers::ProviderEndpoints {
            listenbrainz: DEFAULT_BASE_URL.to_owned(),
            ..droppedneedle_providers::ProviderEndpoints::closed()
        },
    );
    let listening = ListeningStore::new(
        db.db().clone(),
        Arc::new(droppedneedle_kernel::runtime_config::Crypto::from_key_bytes(&[7u8; 32]).unwrap()),
        Arc::new(droppedneedle_kernel::clock::SystemClock),
    );
    listening
        .save_listenbrainz("u-ada", "ada-lb", "ada-token")
        .await
        .expect("link");
    let dir = db.path().parent().expect("scratch dir").join("lb-settings");
    let settings = droppedneedle_acquire::test_support::scratch_settings(&dir).expect("settings");
    let playlists = PlaylistStore::new(CollectionsDb::new(db.db().clone(), db.clock().clone()));
    let builder = droppedneedle_acquire::requests::mix_sources::live_builder(
        &state,
        &providers,
        listening,
        playlists.clone(),
        EventHub::new(),
        settings.live(),
    );

    let built = builder.build_for_user("u-ada", true).await.unwrap();
    assert_eq!(built.skipped, None);
    let entries = playlists
        .owned_by("u-ada")
        .tracks(&built.playlist_id.expect("mix playlist"))
        .await
        .unwrap();
    let mut placed: Vec<(Option<String>, Option<String>, String)> = entries
        .into_iter()
        .map(|entry| (entry.track_source_id, entry.album_id, entry.album_name))
        .collect();
    placed.sort();
    let mut expected = vec![(
        Some(KARMA_POLICE.to_owned()),
        Some(OK_COMPUTER.to_owned()),
        "OK Computer".to_owned(),
    )];
    for (_, song, album, release) in SIMILAR_TOPS {
        expected.push((
            Some(song.to_owned()),
            Some(album.to_owned()),
            release.to_owned(),
        ));
    }
    expected.sort();
    assert_eq!(
        placed, expected,
        "each song once, on the album its own lookup names"
    );

    let sent = wire.sent();
    let asked = |path: &str| {
        sent.iter()
            .filter(|sent| sent.url.path().contains(path))
            .collect::<Vec<_>>()
    };
    assert!(
        asked("top-release-groups-for-artist").is_empty(),
        "an artist's top albums are never asked for"
    );
    for read in ["top-recordings-for-artist", "metadata/recording"] {
        assert!(!asked(read).is_empty(), "{read} was asked");
        for sent in asked(read) {
            let token = sent
                .headers
                .iter()
                .find(|(name, _)| name.eq_ignore_ascii_case("authorization"))
                .map(|(_, value)| value.as_str());
            assert_eq!(token, Some("Token ada-token"), "{}", sent.url);
        }
    }

    let unlinked = builder.build_for_user("u-bob", true).await.unwrap();
    assert_eq!(unlinked.skipped, Some(SkipReason::NotLinked));
    assert_eq!(wire.sent().len(), sent.len(), "nothing was sent for Bob");
}

// History pages; a user hides a row from their own view while an admin
// deletes it, and `status=reimportable` lists only the failed requests whose
// download kept its files, and only for an admin.
#[tokio::test]
async fn history_pages_hides_and_lists_reimportable() {
    let case = "pages and clears";
    let (state, _db, dispatch) = setup().await;
    for mbid in [MBID_A, MBID_B, MBID_C] {
        ask(&state, TRUSTED, mbid).await;
    }
    ask(&state, BOB, MBID_A).await;
    let tasks = dispatch.take_calls().await;
    assert_eq!(tasks.len(), 3, "{case}");
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
    assert_eq!(page["total"], 3, "{case}");
    assert_eq!(page["total_pages"], 2, "{case}");
    assert_eq!(page["items"].as_array().unwrap().len(), 1, "{case}");

    let clear = format!("/api/v3/requests/history/{MBID_A}");
    assert_eq!(
        call(&state, Method::DELETE, &clear, Some(ADA), None)
            .await
            .0,
        StatusCode::NOT_FOUND,
        "{case}"
    );
    assert_eq!(
        call(&state, Method::DELETE, &clear, Some(BOB), None)
            .await
            .1["success"],
        true,
        "{case}"
    );
    let (_, bobs) = call(
        &state,
        Method::GET,
        "/api/v3/requests/history",
        Some(BOB),
        None,
    )
    .await;
    assert_eq!(bobs["total"], 0, "{case}: hidden from Bob only");
    let (_, owners) = call(
        &state,
        Method::GET,
        "/api/v3/requests/history",
        Some(TRUSTED),
        None,
    )
    .await;
    assert_eq!(owners["total"], 3, "{case}");
    call(&state, Method::DELETE, &clear, Some(ADMIN), None).await;
    let (_, owners) = call(
        &state,
        Method::GET,
        "/api/v3/requests/history",
        Some(TRUSTED),
        None,
    )
    .await;
    assert_eq!(owners["total"], 2, "{case}");

    let case = "reimportable";
    let (state, _db, dispatch) = setup().await;
    for mbid in [MBID_A, MBID_B] {
        ask(&state, TRUSTED, mbid).await;
    }
    let kept = dispatch.task(1);
    let bare = dispatch.task(2);
    let attempt_task = kept.clone();
    dispatch
        .dispatch
        .journal()
        .run("test.attempt", move |store| {
            store.insert_attempt(
                "kept-a0",
                &attempt_task,
                "soulseek",
                0,
                "",
                r#"{"source":"soulseek","username":"peer","filenames":["a.flac"],"job_name":""}"#,
                AttemptState::Preserved,
                1_700_000_000.0,
            )
        })
        .await
        .expect("attempt recorded");
    dispatch.fail(&kept).await;
    dispatch.fail(&bare).await;

    let uri = "/api/v3/requests/history?status=reimportable";
    let (status, body) = call(&state, Method::GET, uri, Some(ADMIN), None).await;
    assert_eq!(status, StatusCode::OK, "{case}: {body}");
    assert_eq!(body["total"], 1, "{case}: {body}");
    assert_eq!(body["items"][0]["musicbrainz_id"], MBID_A, "{case}");
    assert_eq!(body["items"][0]["can_reimport"], true, "{case}");

    let (status, body) = call(&state, Method::GET, uri, Some(TRUSTED), None).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{case}: {body}");
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
    let first = dispatch.task(1);
    assert_eq!(dispatch.take_cancels(), vec![first.clone()]);
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

/// An album or track ask whose download cannot be queued because the
/// database is busy is never failed: the row waits, and the next pass of
/// waiting requests starts it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_busy_database_leaves_an_ask_waiting_until_the_next_pass() {
    use droppedneedle_acquire::downloads::journal::Journal;
    use droppedneedle_kernel::db::{DbConfig, DbError, Lane, LaneLimits, open_runtime};

    let (state, _db, dispatch) = setup().await;
    // A download journal whose writer is held, so a dispatch hears busy.
    let dir = crate::common::ScratchDir::new("requests-busy-dispatch");
    let path = dir.join("journal.db");
    crate::common::migrated_db(&path);
    let runtime = open_runtime(
        &DbConfig::new(
            &path,
            std::sync::Arc::new(droppedneedle_kernel::clock::SystemClock),
        )
        .with_lane_limits(LaneLimits {
            foreground_wait: std::time::Duration::from_millis(200),
            watch_tick: std::time::Duration::from_millis(10),
            ..LaneLimits::default()
        }),
    )
    .await
    .expect("runtime opens");
    let held = AcquireDb::new(
        runtime.db().clone(),
        Arc::new(droppedneedle_kernel::clock::SystemClock),
    );
    let settings = droppedneedle_acquire::test_support::scratch_settings(&dir.join("settings"))
        .expect("settings open");
    let busy = Arc::new(DownloadDispatch::new(
        Arc::new(Journal::new(held, crate::common::bundles::quiet_ticks())),
        Arc::new(droppedneedle_kernel::ids::UuidGenerator),
        settings.live(),
    ));
    let busy_state = RequestsState {
        dispatch: busy,
        ..state.clone()
    };
    let (entered, inside) = std::sync::mpsc::channel();
    let (let_go, release) = std::sync::mpsc::channel::<()>();
    let writer = {
        let db = runtime.db().clone();
        std::thread::spawn(move || {
            let _ = db.write_blocking(Lane::Foreground, "test.hold", |_| {
                let _ = entered.send(());
                let _ = release.recv();
                Ok::<_, DbError>(())
            });
        })
    };
    inside
        .recv_timeout(std::time::Duration::from_secs(10))
        .expect("the writer is held");

    let (status, body) = ask(&busy_state, TRUSTED, MBID_A).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    assert_eq!(body["status"], "queued");
    let (status, body) = call(
        &busy_state,
        Method::POST,
        "/api/v3/requests/tracks",
        Some(TRUSTED),
        Some(json!({
            "recording_mbid": MBID_B,
            "artist_name": "Artist",
            "track_title": "Song",
        })),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    assert_eq!(body["status"], "queued");
    drop(let_go);
    writer.join().expect("writer joins");
    for (kind, key) in [(RequestKind::Album, MBID_A), (RequestKind::Track, MBID_B)] {
        let row = state.store.get(kind, key).await.unwrap().unwrap();
        assert!(row.waits_for_quota(), "{kind:?} waits: {:?}", row.status);
    }

    let started = RequestsService::new(&state)
        .start_waiting(2_000_000_000)
        .await
        .unwrap();
    assert!(started.is_empty(), "no follow notice for a person's ask");
    assert_eq!(dispatch.take_calls().await.len(), 2);
    for (kind, key) in [(RequestKind::Album, MBID_A), (RequestKind::Track, MBID_B)] {
        let row = state.store.get(kind, key).await.unwrap().unwrap();
        assert_eq!(row.status.as_str(), "downloading", "{kind:?} started");
        assert!(row.task_id.is_some(), "{kind:?} is linked");
    }
    runtime.shutdown().await;
}

/// An album ask that names the edition its page showed is still fetched
/// in the library's chosen edition. While that edition does not read (a
/// busy database), the ask answers busy and stores nothing, so the named
/// edition never stands in for the chosen one.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_busy_edition_read_never_lets_the_asked_edition_stand_in() {
    use droppedneedle_kernel::db::{DbConfig, DbError, LaneLimits, open_runtime};

    const ASKED: &str = "22222222-2222-4222-8222-222222222222";
    const CHOSEN: &str = "33333333-3333-4333-8333-333333333333";
    let (mut state, _db, dispatch) = setup().await;
    // The library's database, with one reader, holding the chosen edition.
    let dir = crate::common::ScratchDir::new("requests-busy-edition");
    let path = dir.join("library.db");
    crate::common::migrated_db(&path);
    let mut config = DbConfig::new(
        &path,
        std::sync::Arc::new(droppedneedle_kernel::clock::SystemClock),
    )
    .with_read_wait(std::time::Duration::from_millis(200))
    .with_lane_limits(LaneLimits {
        watch_tick: std::time::Duration::from_millis(10),
        ..LaneLimits::default()
    });
    config.readers = 1;
    let runtime = open_runtime(&config).await.expect("runtime opens");
    let library = AcquireDb::new(
        runtime.db().clone(),
        Arc::new(droppedneedle_kernel::clock::SystemClock),
    );
    library
        .write("test.seed_edition", |tx| {
            tx.execute_batch(&format!(
                "INSERT INTO local_artists (id, display_name, folded_name, kind, created_at,
                   updated_at) VALUES ('ar1', 'A', 'a', 'person', 0, 0);
                 INSERT INTO local_albums (id, root_id, grouping_key, title, title_folded,
                   album_artist_id, grouping_source, created_at, updated_at)
                   VALUES ('al1', 'r1', 'g', 'T', 't', 'ar1', 'automatic', 0, 0);
                 INSERT INTO local_album_external_identities (local_album_id, provider,
                   release_group_mbid, release_mbid, decision_source, selected_at, row_revision)
                   VALUES ('al1', 'musicbrainz', '{MBID_A}', '{CHOSEN}', 'manual', 5, 1);"
            ))?;
            Ok(())
        })
        .await
        .unwrap();
    state.library = library;
    let body = json!({
        "musicbrainz_id": MBID_A,
        "artist": "Artist",
        "album": "Album",
        "release_mbid": ASKED,
    });

    let (entered, inside) = std::sync::mpsc::channel();
    let (let_go, release) = std::sync::mpsc::channel::<()>();
    let reader = {
        let db = runtime.db().clone();
        std::thread::spawn(move || {
            db.read_blocking("test.hold", move |_| {
                let _ = entered.send(());
                let _ = release.recv();
                Ok::<_, DbError>(())
            })
        })
    };
    inside
        .recv_timeout(std::time::Duration::from_secs(10))
        .expect("the reader is held");
    let (status, answer) = call(
        &state,
        Method::POST,
        "/api/v3/requests/albums",
        Some(TRUSTED),
        Some(body.clone()),
    )
    .await;
    drop(let_go);
    reader
        .join()
        .expect("reader joins")
        .expect("held read ends");
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{answer}");
    assert!(
        state
            .store
            .get(RequestKind::Album, MBID_A)
            .await
            .unwrap()
            .is_none(),
        "nothing is stored"
    );
    assert!(dispatch.take_calls().await.is_empty(), "nothing is queued");

    let (status, answer) = call(
        &state,
        Method::POST,
        "/api/v3/requests/albums",
        Some(TRUSTED),
        Some(body),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{answer}");
    let calls = dispatch.take_calls().await;
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].release_mbid.as_deref(), Some(CHOSEN));
    runtime.shutdown().await;
}

/// The library's plan for one album, scripted: track 1 kept, track 2
/// missing (held by "t2" when `complete`), track 3 kept and track 4 held
/// only by "t4v", another version of it.
struct PlannedEdition {
    complete: bool,
}

impl droppedneedle_library::edition::port::EditionPlans for PlannedEdition {
    fn group_plan<'a>(
        &'a self,
        _album_id: Option<&'a str>,
        _release_group_mbid: &'a str,
    ) -> droppedneedle_library::edition::port::BoxFuture<
        'a,
        Result<
            Option<droppedneedle_library::edition::plan::EditionPlan>,
            droppedneedle_library::jobs::models::OperationError,
        >,
    > {
        use droppedneedle_library::edition::plan::EditionFacts;
        use droppedneedle_library::edition::plan::EditionPlan;
        use droppedneedle_library::edition::plan::FileFate;
        use droppedneedle_library::edition::plan::PlanFile;
        use droppedneedle_library::edition::plan::PlanTrack;
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
            suggested: None,
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
    state.edition_plans = Arc::new(PlannedEdition { complete: false });
    state.upgrade_policy = saved_policy(
        &db,
        DownloadPolicy {
            upgrade_allowed: true,
            quality_cutoff: "lossless".into(),
            ..DownloadPolicy::default()
        },
    );
    (state, db, dispatch)
}

// Which copy and release an edition fetch joins: the copy the album page
// shows, a copy a person picked, and with no plan the copy asked about.
#[tokio::test]
async fn a_fetch_joins_the_copy_the_album_page_shows() {
    // With two copies and no chosen release, a fetch with no album named
    // joins the copy the album page shows, on its release: here the newer
    // copy, the only one with files. A shown copy that names no release
    // gives no edition.
    let case = "shown copy";
    let (state, db, dispatch) = setup().await;
    seed_edition_album(&db).await;
    let group = MBID_A.to_owned();
    let seed_group = group.clone();
    db.write("test.seed_copies", move |tx| {
        tx.execute_batch(&format!(
            "UPDATE local_albums SET created_at = 5, title = 'Shown', album_artist_name = 'A' WHERE id = 'al1';
             INSERT INTO local_albums (id, root_id, grouping_key, title, title_folded,
               album_artist_id, grouping_source, created_at, updated_at)
               VALUES ('al0', 'r1', 'g0', 'Empty', 'empty', 'ar1', 'automatic', 0, 0);
             INSERT INTO local_album_external_identities (local_album_id, provider,
               release_group_mbid, release_mbid, decision_source, selected_at, row_revision)
               VALUES ('al0', 'musicbrainz', '{seed_group}', 'rel-empty', 'automatic', 0, 1),
                      ('al1', 'musicbrainz', '{seed_group}', 'rel-shown', 'automatic', 0, 1);"
        ))?;
        Ok(())
    })
    .await
    .unwrap();
    let page = droppedneedle_catalog::metadata::library::LocalCatalog::new(db.db().clone())
        .album(&group)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(page.title, "Shown", "{case}");
    let edition = droppedneedle_acquire::edition::chosen_edition(db.db(), &group)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(edition.release_mbid, "rel-shown", "{case}");
    let uri = format!("/api/v3/albums/{MBID_A}/edition/acquire");
    let (status, body) = call(&state, Method::POST, &uri, Some(ADMIN), None).await;
    assert_eq!(status, StatusCode::OK, "{case}: {body}");
    let planned = dispatch.take_calls().await[0]
        .planned_album
        .clone()
        .expect("a planned copy");
    assert_eq!(
        (planned.album_id.as_str(), planned.release_mbid.as_str()),
        ("al1", "rel-shown"),
        "{case}"
    );

    db.write("test.unmanaged", |tx| {
        tx.execute_batch(
            "UPDATE local_album_external_identities SET release_mbid = NULL \
             WHERE local_album_id = 'al1'",
        )?;
        Ok(())
    })
    .await
    .unwrap();
    assert_eq!(
        droppedneedle_acquire::edition::chosen_edition(db.db(), &group)
            .await
            .unwrap(),
        None,
        "{case}: another copy's release does not stand in"
    );

    // Two copies with files: the newer one a person picked is the one the
    // album page shows and a fetch joins.
    let case = "picked copy";
    let (state, db, dispatch) = setup().await;
    seed_edition_album(&db).await;
    let group = MBID_A.to_owned();
    let seed_group = group.clone();
    db.write("test.seed_copies", move |tx| {
        tx.execute_batch(&format!(
            "UPDATE local_albums SET created_at = 5, title = 'Picked', album_artist_name = 'A'
               WHERE id = 'al1';
             INSERT INTO local_albums (id, root_id, grouping_key, title, title_folded,
               album_artist_id, grouping_source, album_artist_name, created_at, updated_at)
               VALUES ('al0', 'r1', 'g0', 'Older', 'older', 'ar1', 'automatic', 'A', 0, 0);
             INSERT INTO local_tracks (id, local_album_id, root_id, file_path, relative_path,
               path_hash, file_size_bytes, file_mtime_ns, stat_revision, title, title_folded,
               album_title, album_title_folded, file_format, bit_rate, ingest_source, imported_at,
               membership_source)
               VALUES ('t0', 'al0', 'r1', '/m/0.flac', '0.flac', 'h0', 1, 0, 's', 'T', 't', 'T',
                 't', 'flac', NULL, 'scan', 0, 'automatic');
             INSERT INTO local_album_external_identities (local_album_id, provider,
               release_group_mbid, release_mbid, decision_source, selected_at, row_revision)
               VALUES ('al0', 'musicbrainz', '{seed_group}', 'rel-older', 'automatic', 0, 1),
                      ('al1', 'musicbrainz', '{seed_group}', 'rel-picked', 'manual', 0, 1);"
        ))?;
        Ok(())
    })
    .await
    .unwrap();
    let page = droppedneedle_catalog::metadata::library::LocalCatalog::new(db.db().clone())
        .album(&group)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(page.title, "Picked", "{case}");
    let uri = format!("/api/v3/albums/{MBID_A}/edition/acquire");
    let (status, body) = call(&state, Method::POST, &uri, Some(ADMIN), None).await;
    assert_eq!(status, StatusCode::OK, "{case}: {body}");
    let planned = dispatch.take_calls().await[0]
        .planned_album
        .clone()
        .expect("a planned copy");
    assert_eq!(planned.album_id, "al1", "{case}");

    // With no plan, "Get missing tracks" fetches the whole album for the copy
    // asked about, on the release its identity names.
    let case = "no plan";
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
    state.edition_plans = Arc::new(NoPlan);
    let uri = format!("/api/v3/albums/{MBID_A}/edition/acquire?album_id=al1");
    let (status, body) = call(&state, Method::POST, &uri, Some(ADMIN), None).await;
    assert_eq!(status, StatusCode::OK, "{case}: {body}");
    let calls = dispatch.take_calls().await;
    assert_eq!(calls.len(), 1, "{case}: {body}");
    assert_eq!(calls[0].release_mbid.as_deref(), Some("rel-x"), "{case}");
    assert_eq!(
        calls[0].planned_album,
        Some(PlannedAlbum {
            album_id: "al1".into(),
            release_mbid: "rel-x".into(),
            replace: Vec::new(),
        }),
        "{case}"
    );
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
    state.edition_plans = Arc::new(PlannedEdition { complete: true });
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

impl droppedneedle_library::edition::port::EditionPlans for NoPlan {
    fn group_plan<'a>(
        &'a self,
        _album_id: Option<&'a str>,
        _release_group_mbid: &'a str,
    ) -> droppedneedle_library::edition::port::BoxFuture<
        'a,
        Result<
            Option<droppedneedle_library::edition::plan::EditionPlan>,
            droppedneedle_library::jobs::models::OperationError,
        >,
    > {
        Box::pin(async {
            Err(
                droppedneedle_library::jobs::models::OperationError::Unavailable(
                    "MusicBrainz is down".into(),
                ),
            )
        })
    }
}

// With no download source set up, "Get missing tracks" is held like any
// request: the download is queued for the worker, which waits for a source,
// and the answer says so. It is not refused.
#[tokio::test]
async fn edition_acquire_is_held_while_no_source_is_ready() {
    let (mut state, db, dispatch) = setup().await;
    seed_edition_album(&db).await;
    state.edition_plans = Arc::new(PlannedEdition { complete: false });
    let staging = crate::common::ScratchDir::new("edition-held");
    // Sources read from settings where none is set up.
    let settings = droppedneedle_acquire::test_support::scratch_settings(&staging.join("settings"))
        .expect("settings open");
    let (worker, _) = crate::common::acquire::worker_over(
        &db,
        dispatch.dispatch.journal().clone(),
        &settings,
        staging.join("slskd"),
        Arc::new(crate::common::acquire::FixedAlbums::default()),
        None,
        droppedneedle_kernel::events::EventHub::new(),
    )
    .await;
    state.readiness = AcquireReadiness {
        sources: worker.readiness().clone(),
        staging_root: staging.to_path_buf(),
    };
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

impl droppedneedle_library::edition::port::EditionPlans for LibraryPlan {
    fn group_plan<'a>(
        &'a self,
        _album_id: Option<&'a str>,
        _release_group_mbid: &'a str,
    ) -> droppedneedle_library::edition::port::BoxFuture<
        'a,
        Result<
            Option<droppedneedle_library::edition::plan::EditionPlan>,
            droppedneedle_library::jobs::models::OperationError,
        >,
    > {
        use droppedneedle_library::edition::plan::EditionFacts;
        use droppedneedle_library::edition::plan::EditionPlan;
        use droppedneedle_library::edition::plan::FileFate;
        use droppedneedle_library::edition::plan::PlanFile;
        use droppedneedle_library::edition::plan::PlanTrack;
        Box::pin(async move {
            let files: Vec<String> = self.db.db().column(
                "SELECT id FROM local_tracks WHERE local_album_id = 'al1'",
                [],
            );
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
                    suggested: None,
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
        outcome: droppedneedle_acquire::downloads::landing_rows::ImportOutcome::parse(outcome)
            .unwrap(),
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
    state.edition_plans = Arc::new(LibraryPlan { db: db.clone() });
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
    let task = dispatch.task(1);
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
// write: one that names no artist is refused, a plain user's auto-download
// waits for an admin, a repeat ask on a
// live row follows too, a batch follows each artist, and a request the
// quota refuses leaves no follow behind.
#[tokio::test]
async fn a_request_that_asks_to_follow_follows_the_artist() {
    let (state, db, dispatch) = setup().await;
    let limited = QuotaPolicy {
        request_count: 1,
        ..QuotaPolicy::default()
    };
    let state = rebuilt(&state, &db, limited, &dispatch);
    let albums = "/api/v3/requests/albums";
    let no_artist = json!({
        "musicbrainz_id": MBID_C, "artist": "Artist", "album": "Album", "monitor_artist": true,
    });
    let (status, _) = call(&state, Method::POST, albums, Some(ADA), Some(no_artist)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "a follow needs its artist");
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

// A dispatch whose link loses the row's generation to another linker of the
// same task (a waiting pass, a recovery) keeps that task: the ask answers as
// started, the row follows the task, and nothing is cancelled.
#[tokio::test]
async fn a_link_won_by_another_linker_of_the_same_task_stands() {
    let (state, db, dispatch) = setup().await;
    db.write("test.steal_link", |tx| {
        tx.execute_batch(
            "CREATE TRIGGER steal_link AFTER INSERT ON download_tasks BEGIN \
             UPDATE request_history SET download_task_id = NEW.id, status = 'downloading', \
             generation = generation + 1 \
             WHERE musicbrainz_id_lower = lower(NEW.release_group_mbid); END;",
        )?;
        Ok(())
    })
    .await
    .unwrap();

    let (status, body) = ask(&state, TRUSTED, MBID_A).await;
    assert!(status.is_success(), "{status} {body}");
    let task = dispatch.task(1);
    assert!(dispatch.take_cancels().is_empty(), "nothing is cancelled");
    let row = state.store.get(RequestKind::Album, MBID_A).await.unwrap();
    assert_eq!(row.unwrap().task_id.as_deref(), Some(task.as_str()));
}
