//! Request intake over the durable ledger: the approval gate, co-requesters,
//! quotas, the wanted and approval queues, and restart survival.
//!
//! Each test mounts the requests router with the test principal header over
//! a scratch database. The scripted dispatch stands in for the downloads
//! side; every row it touches is a real SQLite row.

use std::collections::HashMap;
use std::sync::Arc;

use droppedneedle::acquire::requests::mix::{
    MixGrantHooks, MixSources, PersonalMixBuilder, SkipReason,
};
use droppedneedle::plugins::scrobble::{
    MemoryScrobblePrefsStore, MixApprovalHook as _, MixStateReader as _, ScrobblePrefsPatch,
    ScrobblePrefsStore as _,
};
use droppedneedle::providers::listenbrainz::playlists::{
    RecommendationPlaylist, RecommendationTrack,
};
use droppedneedle::providers::listenbrainz::{
    ListenBrainzCredentials, SimilarArtist, TopRecording, TopReleaseGroup,
};
use droppedneedle::reads::collections::db::CollectionsDb;
use droppedneedle::reads::collections::store::playlists::PlaylistStore;
use droppedneedle_kernel::events::EventSink;
use futures_util::future::BoxFuture;

use axum::{
    Router,
    body::Body,
    http::{Method, Request, StatusCode},
};
use droppedneedle::acquire::db::AcquireDb;
use droppedneedle::acquire::downloads::manifest::{PlannedAlbum, ReplaceVersion};
use droppedneedle::acquire::requests::{
    self, RequestsState,
    dispatch::{DispatchTaskState, RetrySchedule, ScriptedDispatch},
    ledger::{WATCH_WATCHING, WantedWatch},
    models::RequestKind,
    quota::{QuotaLedger, QuotaPolicy},
    service::RequestsService,
    state::AcquireReadiness,
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
const ARTIST: &str = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";

/// State over a scratch database with every test user present.
async fn setup() -> (RequestsState, AcquireDb, Arc<ScriptedDispatch>) {
    let (state, db, dispatch) = RequestsState::for_tests().expect("requests state builds");
    for (id, name, role) in [
        ("u-ada", "Ada", "user"),
        ("u-bob", "Bob", "user"),
        ("u-tris", "Tris", "trusted"),
        ("u-root", "Root", "admin"),
    ] {
        db.add_user(id, name, role).await.expect("user row");
    }
    (state, db, dispatch)
}

/// A fresh state over the same database: what a restart sees.
fn rebuilt(db: &AcquireDb, quota: QuotaPolicy, dispatch: Arc<ScriptedDispatch>) -> RequestsState {
    let quota = Arc::new(QuotaLedger::new(
        Arc::new(move || quota.clone()),
        db.clone(),
    ));
    RequestsState::new(db, quota, dispatch)
}

async fn call(
    state: &RequestsState,
    method: Method,
    uri: &str,
    identity: Option<&str>,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let app: Router = requests::requests_router(state.clone());
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

async fn ask(state: &RequestsState, who: &str, mbid: &str) -> (StatusCode, Value) {
    call(
        state,
        Method::POST,
        "/requests/albums",
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
        state: WATCH_WATCHING.to_owned(),
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
    state
        .follows
        .file_pending("u-ada", ARTIST, "Artist", 1_700_000_000)
        .await
        .unwrap();
    state.wanted.enrol(watch(MBID_C, "u-ada")).await.unwrap();

    let after = rebuilt(&db, QuotaPolicy::default(), dispatch);
    let (_, approvals) = call(
        &after,
        Method::GET,
        "/requests/approvals",
        Some(ADMIN),
        None,
    )
    .await;
    assert_eq!(keys(&approvals), vec![MBID_A.to_owned()]);
    let (_, bobs) = call(&after, Method::GET, "/requests/active", Some(BOB), None).await;
    assert_eq!(keys(&bobs), vec![MBID_B.to_owned()]);
    assert_eq!(bobs["items"][0]["requester_count"], 1);
    let (_, count) = call(
        &after,
        Method::GET,
        "/requests/approvals/count",
        Some(ADMIN),
        None,
    )
    .await;
    assert_eq!(count["count"], 2, "one album ask plus one follow approval");
    let (_, wanted) = call(&after, Method::GET, "/requests/wanted", Some(ADA), None).await;
    assert_eq!(keys(&wanted), vec![MBID_C.to_owned()]);
}

// User ask, admin approval, dispatch, landing: the row reads imported.
#[tokio::test]
async fn approval_dispatches_and_the_landing_imports() {
    let (state, _db, dispatch) = setup().await;
    ask(&state, ADA, MBID_A).await;
    assert!(dispatch.take_calls().is_empty());
    let uri = format!("/requests/approvals/{MBID_A}/approve");
    let (status, body) = call(&state, Method::POST, &uri, Some(ADMIN), None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let calls = dispatch.take_calls();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].user_id, "u-ada", "the owner, never the approver");
    // A second approval loses the claim instead of dispatching again.
    let (_, again) = call(&state, Method::POST, &uri, Some(ADMIN), None).await;
    assert_eq!(again["success"], false);
    assert!(dispatch.take_calls().is_empty());

    dispatch.land("task-1");
    let (_, active) = call(&state, Method::GET, "/requests/active", Some(ADA), None).await;
    assert!(keys(&active).is_empty());
    let (_, history) = call(&state, Method::GET, "/requests/history", Some(ADA), None).await;
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
    assert_eq!(dispatch.take_calls().len(), 1);
    let (_, all) = call(&state, Method::GET, "/requests/active", Some(ADMIN), None).await;
    assert_eq!(all["count"], 1);
    assert_eq!(all["items"][0]["task_id"], "task-1");
}

// A batch skips raw and live duplicates and dispatches the new rows.
#[tokio::test]
async fn batch_dedupes_and_dispatches_new_rows() {
    let (state, _db, dispatch) = setup().await;
    ask(&state, TRUSTED, MBID_A).await;
    dispatch.take_calls();
    let item = |mbid: &str| json!({"musicbrainz_id": mbid, "artist_name": "A", "album_title": "B"});
    let (status, body) = call(
        &state,
        Method::POST,
        "/requests/batches",
        Some(TRUSTED),
        Some(json!({"items": [item(MBID_A), item(MBID_B), item(MBID_B), item(MBID_C)]})),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    assert_eq!(body["requested"], 2);
    assert_eq!(body["skipped"], 2);
    assert_eq!(dispatch.take_calls().len(), 2);
}

// Strangers may not cancel, listeners detach, owners stop the task.
#[tokio::test]
async fn cancel_matrix() {
    let (state, _db, dispatch) = setup().await;
    ask(&state, TRUSTED, MBID_A).await;
    ask(&state, BOB, MBID_A).await;
    let uri = format!("/requests/active/{MBID_A}");
    let (status, _) = call(&state, Method::DELETE, &uri, Some(ADA), None).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (_, detached) = call(&state, Method::DELETE, &uri, Some(BOB), None).await;
    assert_eq!(detached["success"], true);
    assert!(
        dispatch.take_cancels().is_empty(),
        "a listener never stops the task"
    );
    let (_, cancelled) = call(&state, Method::DELETE, &uri, Some(TRUSTED), None).await;
    assert_eq!(cancelled["success"], true, "{cancelled}");
    assert_eq!(dispatch.take_cancels(), vec!["task-1".to_owned()]);
    let (_, history) = call(
        &state,
        Method::GET,
        "/requests/history",
        Some(TRUSTED),
        None,
    )
    .await;
    assert_eq!(history["items"][0]["status"], "cancelled");
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
    let state = rebuilt(&db, limited.clone(), dispatch.clone());
    assert_eq!(ask(&state, ADA, MBID_A).await.0, StatusCode::ACCEPTED);

    let state = rebuilt(&db, limited, dispatch);
    let (status, body) = ask(&state, ADA, MBID_B).await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(body["error"]["code"], "QUOTA_EXCEEDED");
    assert_eq!(body["error"]["details"]["used"], 1);
    // Curators are exempt.
    assert_eq!(ask(&state, TRUSTED, MBID_B).await.0, StatusCode::ACCEPTED);
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
        dispatch,
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
        &format!("/requests/approvals/{MBID_B}/reject"),
        Some(ADMIN),
        None,
    )
    .await;
    dispatch.fail("task-1");
    call(&state, Method::POST, "/requests/sync", Some(ADMIN), None).await;
    dispatch.take_calls();

    let (_, retried) = call(
        &state,
        Method::POST,
        &format!("/requests/retry/{MBID_A}"),
        Some(TRUSTED),
        None,
    )
    .await;
    assert_eq!(retried["success"], true, "{retried}");
    assert_eq!(dispatch.take_calls().len(), 1);
    // A rejected row is not retryable at all.
    let (_, rejected) = call(
        &state,
        Method::POST,
        &format!("/requests/retry/{MBID_B}"),
        Some(ADA),
        None,
    )
    .await;
    assert_eq!(rejected["success"], false);
    assert!(dispatch.take_calls().is_empty());
}

// Wanted actions check ownership; a fulfilled watch refuses; the retrying
// rows come from the auto-retry schedule.
#[tokio::test]
async fn wanted_view_and_actions() {
    let (state, _db, dispatch) = setup().await;
    state.wanted.enrol(watch(MBID_A, "u-ada")).await.unwrap();
    let stop = format!("/requests/wanted/{MBID_A}/stop");
    assert_eq!(
        call(&state, Method::POST, &stop, Some(BOB), None).await.0,
        StatusCode::NOT_FOUND
    );
    let (_, stopped) = call(&state, Method::POST, &stop, Some(ADA), None).await;
    assert_eq!(stopped["state"], "stopped");
    let (_, resumed) = call(
        &state,
        Method::POST,
        &format!("/requests/wanted/{MBID_A}/resume"),
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
    dispatch.fail("task-1");
    call(&state, Method::POST, "/requests/sync", Some(ADMIN), None).await;
    dispatch.set_retry(
        "task-1",
        RetrySchedule {
            retry_count: 1,
            max_attempts: 6,
            next_retry_at: 1_700_000_900,
        },
    );
    let (_, wanted) = call(&state, Method::GET, "/requests/wanted", Some(TRUSTED), None).await;
    assert_eq!(wanted["retrying"][0]["musicbrainz_id"], MBID_B);
    assert_eq!(wanted["retrying"][0]["retry_count"], 1);
}

// Auto-download approvals: single approve, then a batch reject.
#[tokio::test]
async fn auto_download_approvals() {
    let (state, _db, _dispatch) = setup().await;
    state
        .follows
        .file_pending("u-ada", ARTIST, "Artist", 1_700_000_000)
        .await
        .unwrap();
    state
        .follows
        .create_batch(
            "batch-1",
            "u-bob",
            &[
                (MBID_A.to_owned(), "One".to_owned()),
                (MBID_B.to_owned(), "Two".to_owned()),
            ],
            "lidarr_import",
            1_700_000_000,
        )
        .await
        .unwrap();
    let (_, count) = call(
        &state,
        Method::GET,
        "/requests/approvals/count",
        Some(ADMIN),
        None,
    )
    .await;
    assert_eq!(count["count"], 2, "one single plus one batch");
    let approve = format!("/requests/auto-download-approvals/u-ada/{ARTIST}/approve");
    assert_eq!(
        call(&state, Method::POST, &approve, Some(ADA), None)
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    let (_, approved) = call(&state, Method::POST, &approve, Some(ADMIN), None).await;
    assert_eq!(approved["success"], true);
    let (_, again) = call(&state, Method::POST, &approve, Some(ADMIN), None).await;
    assert_eq!(again["success"], false, "only a pending row decides");
    let (_, rejected) = call(
        &state,
        Method::POST,
        "/requests/auto-download-approval-batches/batch-1/reject",
        Some(ADMIN),
        None,
    )
    .await;
    assert_eq!(rejected["message"], "Auto-download rejected for 2 artists");
}

/// ListenBrainz stand-in: one weekly-jams playlist with two tracks on two
/// albums, no similar artists.
struct FakeMix;

const MIX_REC_A: &str = "aaaa0000-0000-4000-8000-000000000001";
const MIX_REC_B: &str = "aaaa0000-0000-4000-8000-000000000002";

impl MixSources for FakeMix {
    fn identity<'a>(&'a self, user_id: &'a str) -> BoxFuture<'a, Option<ListenBrainzCredentials>> {
        Box::pin(async move {
            (user_id == "u-ada").then(|| ListenBrainzCredentials {
                username: Some("ada".to_owned()),
                user_token: Some("token".to_owned()),
            })
        })
    }
    fn linked_users(&self) -> BoxFuture<'_, Result<Vec<String>, String>> {
        Box::pin(async { Ok(vec!["u-ada".to_owned()]) })
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
    let (state, db, dispatch) = setup().await;
    let prefs = Arc::new(MemoryScrobblePrefsStore::new());
    let playlists = PlaylistStore::new(CollectionsDb::new(db.pool().clone(), db.lane().clone()));
    let builder = Arc::new(PersonalMixBuilder::new(
        Arc::new(FakeMix),
        prefs.clone(),
        playlists.clone(),
        state.clone(),
        EventSink::default(),
    ));
    assert!(state.mixer.set(builder.clone()).is_ok());
    let hooks = MixGrantHooks::new(state.mixer.clone());

    prefs
        .upsert(
            "u-ada",
            &ScrobblePrefsPatch {
                auto_request_personal_mix: Some(true),
                ..ScrobblePrefsPatch::default()
            },
        )
        .await;
    hooks.on_auto_request_toggled("u-ada", "user", true).await;
    assert_eq!(
        hooks.auto_request_state("u-ada", "user", true).await,
        "pending"
    );
    let approve = "/requests/personal-mix-approvals/u-ada/approve";
    assert_eq!(
        call(&state, Method::POST, approve, Some(ADMIN), None)
            .await
            .1["success"],
        true
    );
    assert_eq!(
        hooks.auto_request_state("u-ada", "user", true).await,
        "approved"
    );

    let built = builder.build_for_user("u-ada", false).await.unwrap();
    assert_eq!(
        (built.track_count, built.requested_albums, built.skipped),
        (2, 2, None)
    );
    let playlist = built.playlist_id.expect("mix playlist");
    assert_eq!(playlists.tracks(&playlist).await.unwrap().len(), 2);
    assert_eq!(
        dispatch.take_calls().len(),
        2,
        "granted asks dispatch straight away"
    );
    let again = builder.build_for_user("u-ada", false).await.unwrap();
    assert_eq!(again.skipped, Some(SkipReason::Fresh));

    let revoke = "/requests/personal-mix-approvals/u-ada/revoke";
    assert_eq!(
        call(&state, Method::POST, revoke, Some(ADMIN), None)
            .await
            .1["success"],
        true
    );
    assert!(
        !prefs.get("u-ada").await.auto_request_personal_mix,
        "revoke turns the toggle off"
    );

    let refresh = "/requests/personal-mix/refresh";
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
    for task in ["task-1", "task-2", "task-3"] {
        dispatch.land(task);
    }
    let (_, page) = call(
        &state,
        Method::GET,
        "/requests/history?page=2&page_size=2",
        Some(TRUSTED),
        None,
    )
    .await;
    assert_eq!(page["total"], 3);
    assert_eq!(page["total_pages"], 2);
    assert_eq!(page["items"].as_array().unwrap().len(), 1);

    let clear = format!("/requests/history/{MBID_A}");
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
    let (_, bobs) = call(&state, Method::GET, "/requests/history", Some(BOB), None).await;
    assert_eq!(bobs["total"], 0, "hidden from Bob only");
    let (_, owners) = call(
        &state,
        Method::GET,
        "/requests/history",
        Some(TRUSTED),
        None,
    )
    .await;
    assert_eq!(owners["total"], 3);
    call(&state, Method::DELETE, &clear, Some(ADMIN), None).await;
    let (_, owners) = call(
        &state,
        Method::GET,
        "/requests/history",
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
    let (status, body) = call(&state, Method::GET, "/requests/active", None, None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body["error"]["code"], "UNAUTHORIZED");
    let (status, _) = call(&state, Method::GET, "/requests/approvals", Some(ADA), None).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, body) = call(
        &state,
        Method::POST,
        "/requests/albums",
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
    dispatch.take_calls();

    let report = RequestsService::new(&state).recover().await.unwrap();
    assert_eq!((report.cancelled, report.redispatched), (1, 1));
    assert_eq!(dispatch.take_cancels(), vec!["task-1".to_owned()]);
    assert_eq!(dispatch.state_of("task-1"), DispatchTaskState::Cancelled);
    let calls = dispatch.take_calls();
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

impl droppedneedle::library::operations::port::EditionPlans for PlannedEdition {
    fn group_plan<'a>(
        &'a self,
        _album_id: Option<&'a str>,
        _release_group_mbid: &'a str,
    ) -> droppedneedle::library::operations::port::BoxFuture<
        'a,
        Result<
            Option<droppedneedle::library::operations::plan::EditionPlan>,
            droppedneedle::library::operations::models::OperationError,
        >,
    > {
        use droppedneedle::library::operations::plan::{
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
async fn edition_setup() -> (RequestsState, AcquireDb, Arc<ScriptedDispatch>) {
    let (state, db, dispatch) = setup().await;
    seed_edition_album(&db).await;
    assert!(
        state
            .edition_plans
            .set(Arc::new(PlannedEdition { complete: false }))
            .is_ok()
    );
    assert!(
        state
            .upgrade_policy
            .set(Arc::new(|| {
                droppedneedle::acquire::flows::stores::UpgradePolicy {
                    upgrade_allowed: true,
                    cutoff: "lossless".into(),
                    ..Default::default()
                }
            }))
            .is_ok()
    );
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
    let asked = format!("/albums/{MBID_A}/edition/acquire?album_id=al1");
    let uri = format!("{asked}&replace_versions=false");
    let (status, body) = call(&state, Method::POST, &uri, Some(ADMIN), None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["status"], "started");
    assert_eq!(
        (&body["requested"], &body["upgrades"], &body["versions"]),
        (&json!(1), &json!(1), &json!(0))
    );
    let calls = dispatch.take_calls();
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
    assert!(dispatch.take_calls().is_empty(), "never two at once");

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
    let calls = dispatch.take_calls();
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
    assert_eq!(dispatch.take_calls()[0].wanted, vec![(1, 2), (1, 3)]);

    // Only a version left, not asked for (upgrades off).
    let (state, db, dispatch) = setup().await;
    seed_edition_album(&db).await;
    assert!(
        state
            .edition_plans
            .set(Arc::new(PlannedEdition { complete: true }))
            .is_ok()
    );
    let (status, body) = call(&state, Method::POST, &uri, Some(ADMIN), None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["status"], "already_complete", "{body}");
    assert_eq!(
        body["message"],
        "Every track of this edition is in your library, 1 of them in another version."
    );
    assert!(dispatch.take_calls().is_empty());
}

/// The library's plan, unavailable (MusicBrainz down).
struct NoPlan;

impl droppedneedle::library::operations::port::EditionPlans for NoPlan {
    fn group_plan<'a>(
        &'a self,
        _album_id: Option<&'a str>,
        _release_group_mbid: &'a str,
    ) -> droppedneedle::library::operations::port::BoxFuture<
        'a,
        Result<
            Option<droppedneedle::library::operations::plan::EditionPlan>,
            droppedneedle::library::operations::models::OperationError,
        >,
    > {
        Box::pin(async {
            Err(
                droppedneedle::library::operations::models::OperationError::Unavailable(
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
    let (state, db, dispatch) = setup().await;
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
    assert!(state.edition_plans.set(Arc::new(NoPlan)).is_ok());
    let uri = format!("/albums/{MBID_A}/edition/acquire?album_id=al1");
    let (status, body) = call(&state, Method::POST, &uri, Some(ADMIN), None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let calls = dispatch.take_calls();
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
    let (state, db, dispatch) = setup().await;
    seed_edition_album(&db).await;
    assert!(
        state
            .edition_plans
            .set(Arc::new(PlannedEdition { complete: false }))
            .is_ok()
    );
    let staging = crate::common::ScratchDir::new("edition-held");
    assert!(
        state
            .readiness
            .set(AcquireReadiness {
                sources_ready: Arc::new(|| false),
                staging_root: staging.to_path_buf(),
            })
            .is_ok()
    );
    let uri = format!("/albums/{MBID_A}/edition/acquire?album_id=al1");
    let (status, body) = call(&state, Method::POST, &uri, Some(ADMIN), None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["status"], "started", "{body}");
    assert!(
        body["message"]
            .as_str()
            .is_some_and(|message| message.contains("waits")),
        "{body}"
    );
    assert_eq!(dispatch.take_calls().len(), 1, "queued, not refused");
}
