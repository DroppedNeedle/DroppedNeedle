//! Acquisition flows over the durable stores: the wanted watcher, status
//! sync, follow poll, upgrade sweep and drop import.
//!
//! Ticks take an explicit `now`. Stores sit on a scratch database and
//! downloads go into the real journal on it. The wanted
//! watcher searches the sources the saved settings turn on (the slskd
//! mock), the follow poll reads MusicBrainz pages from a scripted wire,
//! and flow notices go to a real event hub.

use crate::common::Probe;
use std::sync::Arc;
use std::time::Duration;

use crate::common::acquire::FixedAlbums;
use crate::common::bundles::{idle_host_over, quiet_ticks, requests_over};
use droppedneedle_acquire::clients::LiveClients;
use droppedneedle_acquire::db::AcquireDb;
use droppedneedle_acquire::dispatch::{
    DispatchOrigin, DispatchOutcome, DispatchRequest, DispatchTarget, DownloadDispatch,
};
use droppedneedle_acquire::downloads::journal::Journal;
use droppedneedle_acquire::downloads::state::TaskStatus;
use droppedneedle_acquire::downloads::store::{NewTask, TaskRow};
use droppedneedle_acquire::downloads::worker::WorkerSettings;
use droppedneedle_acquire::flows::{
    loops::{
        FollowDeps, PruneDeps, SweepDeps, SyncDeps, WantedDeps, follow_tick, interval_seconds,
        prune_tick, sweep_tick, sync_tick, wanted_tick,
    },
    seams::TickSink,
    stores::{AdminDirectory, LibraryPresence, UpgradeWorklist},
};
use droppedneedle_acquire::follows::Follows;
use droppedneedle_acquire::plugin_events::FlowTicks;
use droppedneedle_acquire::requests::{
    ledger::{BeginOutcome, RequestRecord, RequestStatus, WatchState},
    models::{AlbumIntake, RequestKind, RequestOrigin, WaitingReason},
    quota::QuotaLedger,
    service::RequestsService,
    sqlite::{RequestStore, WantedStore},
    state::RequestsState,
};
use droppedneedle_acquire::search_jobs::store::JobStore;
use droppedneedle_acquire::target::Targets;
use droppedneedle_acquire::wiring::ClientLives;
use droppedneedle_auth::{Principal, Role};
use droppedneedle_kernel::events::EventHub;
use droppedneedle_kernel::events::model::names;
use droppedneedle_kernel::ids::UuidGenerator;
use droppedneedle_kernel::runtime_config::ConfigStore;
use droppedneedle_kernel::runtime_config::secret_sections::SlskdConnection;
use droppedneedle_kernel::runtime_config::sections::{DownloadPolicy, WantedWatcher};
use droppedneedle_providers::RequestPriority;
use droppedneedle_providers::musicbrainz::MbSource;
use droppedneedle_providers::test_support::ScriptedWire;
use droppedneedle_testkit::slskd::{MOCK_API_KEY, MockSlskd};

const NOW: i64 = 1_700_000_000;

/// One request row in `status`, owned by `user-1`.
async fn seed_request(store: &RequestStore, key: &str, status: &str, task: Option<&str>) {
    let record = RequestRecord {
        key: key.to_owned(),
        kind: RequestKind::Album,
        status: RequestStatus::parse(status).expect("a known status"),
        artist_name: "Massive Attack".to_owned(),
        album_title: "Blue Lines".to_owned(),
        artist_mbid: None,
        year: None,
        release_mbid: None,
        track_title: None,
        duration_seconds: None,
        track_release_group_mbid: None,
        user_id: Some("user-1".to_owned()),
        origin: DispatchOrigin::User,
        requested_by_name: None,
        requesters: Vec::new(),
        requested_at: NOW as u64,
        completed_at: None,
        task_id: task.map(str::to_owned),
        generation: 0,
        dispatch_authorized: true,
        monitor_artist: false,
        auto_download_artist: false,
        reviewed_by_name: None,
        reviewed_at: None,
    };
    let outcomes = store
        .begin_batch(vec![record], "user-1", None)
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(outcomes[0], BeginOutcome::Won(_)));
}

async fn status_of(store: &RequestStore, key: &str) -> String {
    store
        .get(RequestKind::Album, key)
        .await
        .unwrap()
        .unwrap()
        .status
        .as_str()
        .to_owned()
}

/// Settings beside `db`, at their defaults until a test saves one.
fn settings(db: &AcquireDb) -> ConfigStore {
    let dir = db.path().parent().expect("scratch dir").join("settings");
    droppedneedle_acquire::settings::scratch_settings(&dir).expect("settings open")
}

/// The production dispatch over the download journal on `db`.
fn dispatch(db: &AcquireDb, settings: &ConfigStore) -> Arc<DownloadDispatch> {
    Arc::new(DownloadDispatch::new(
        Arc::new(Journal::new(db.clone(), quiet_ticks())),
        Arc::new(UuidGenerator),
        settings.live(),
    ))
}

/// One album task for `user-1` in the journal, put in `status` behind the
/// journal's back: the requests linked to it do not follow.
async fn seed_task(db: &AcquireDb, id: &str, rg_mbid: &str, status: TaskStatus) {
    let task = NewTask {
        id: id.to_owned(),
        user_id: "user-1".to_owned(),
        artist_name: "Massive Attack".to_owned(),
        album_title: rg_mbid.to_owned(),
        release_group_mbid: rg_mbid.to_owned(),
        origin: "user".to_owned(),
        retry_count: 0,
    };
    Journal::new(db.clone(), quiet_ticks())
        .run("test.seed_task", move |store| {
            store.insert_task(&task, NOW as f64)
        })
        .await
        .expect("task seeds");
    if status != TaskStatus::Queued {
        behind(db, id, status).await;
    }
}

/// Move one task to `status` with a plain write, as something outside the
/// journal would: its requests are left for the status sweep. Only a
/// settled task gets a completion time.
async fn behind(db: &AcquireDb, id: &str, status: TaskStatus) {
    let id = id.to_owned();
    let completed_at = status.is_terminal().then_some(NOW as f64);
    db.write("test.behind", move |tx| {
        tx.execute(
            "UPDATE download_tasks SET status = ?2, completed_at = ?3 WHERE id = ?1",
            rusqlite::params![id, status.as_str(), completed_at],
        )?;
        Ok(())
    })
    .await
    .expect("task moves");
}

/// Every task in the journal, oldest first.
async fn tasks(db: &AcquireDb) -> Vec<TaskRow> {
    let ids: Vec<String> = db.db().column(
        "SELECT id FROM download_tasks ORDER BY created_at, rowid",
        [],
    );
    let journal = Journal::new(db.clone(), quiet_ticks());
    let mut rows = Vec::new();
    for id in ids {
        rows.push(journal.read_task(&id).await.unwrap().expect("task row"));
    }
    rows
}

/// The notices `user` would be shown on connecting now, by name.
async fn notices(events: &EventHub, user: &str) -> Vec<&'static str> {
    let mut stream = events.subscribe(user);
    let mut names = Vec::new();
    while let Ok(Some(frame)) = tokio::time::timeout(Duration::ZERO, stream.next()).await {
        names.push(frame.name());
    }
    names
}

/// The production tick sink over `events`, with a plugin host that
/// launches nothing.
async fn ticks(db: &AcquireDb, events: &EventHub) -> Arc<dyn TickSink> {
    Arc::new(FlowTicks::new(idle_host_over(db).await, events.clone()))
}

struct Wanted {
    deps: WantedDeps,
    library: Arc<LibraryPresence>,
    events: EventHub,
    slskd: MockSlskd,
}

/// The sources the saved settings turn on, over the slskd mock, with
/// slskd turned on or off.
async fn sources_over(
    db: &AcquireDb,
    settings: &ConfigStore,
    enabled: bool,
) -> (Arc<WorkerSettings>, MockSlskd) {
    let slskd = MockSlskd::start().await.expect("mock slskd starts");
    settings
        .save_secret_blocking(SlskdConnection {
            enabled,
            url: slskd.base_url(),
            api_key: MOCK_API_KEY.into(),
            ..SlskdConnection::default()
        })
        .expect("slskd connection saves");
    let dir = db.path().parent().expect("scratch dir").to_path_buf();
    let journal = Arc::new(Journal::new(db.clone(), quiet_ticks()));
    let http = droppedneedle_kernel::http_client::HttpClientFactory::new()
        .expect("http client factory")
        .shared()
        .clone();
    let clients = Arc::new(LiveClients::new(
        http,
        droppedneedle_providers::Providers::closed().newznab,
        ClientLives::new(settings),
        dir.join("slskd"),
        journal.clone(),
        Arc::new(Targets::new(journal, Arc::new(FixedAlbums::default()))),
        idle_host_over(db).await,
    ));
    let sources = Arc::new(WorkerSettings::new(settings, clients));
    (sources, slskd)
}

async fn wanted(db: &AcquireDb) -> Wanted {
    wanted_with_slskd(db, true).await
}

async fn wanted_with_slskd(db: &AcquireDb, slskd_on: bool) -> Wanted {
    db.add_user("user-1", "Owner", "user").await.unwrap();
    let settings = settings(db);
    settings
        .save_blocking(WantedWatcher {
            enabled: true,
            watch_partial_albums: false,
            max_checks_per_sweep: 20,
            auto_download_on_find: true,
        })
        .expect("watcher settings save");
    let (sources, slskd) = sources_over(db, &settings, slskd_on).await;
    let library = Arc::new(LibraryPresence::new());
    let events = EventHub::new();
    Wanted {
        deps: WantedDeps {
            settings: settings.live(),
            watches: WantedStore::new(db.clone()),
            ledger: RequestStore::new(db.clone()),
            sources,
            downloads: dispatch(db, &settings),
            library: library.clone(),
            targets: Arc::new(Targets::new(
                Arc::new(Journal::new(db.clone(), quiet_ticks())),
                Arc::new(FixedAlbums::default()),
            )),
            ticks: ticks(db, &events).await,
        },
        library,
        events,
        slskd,
    }
}

/// A failed request enrols a durable watch; once due with a candidate it
/// auto-dispatches, links the task, and reads imported once that download
/// completes, announced once. A watch the library now holds is fulfilled
/// without a download.
#[tokio::test]
async fn wanted_watch_lifecycle() {
    let db = AcquireDb::scratch().unwrap();
    let rig = wanted(&db).await;
    seed_request(&rig.deps.ledger, "rg-blue", "failed", None).await;

    let first = wanted_tick(NOW, &rig.deps).await;
    assert_eq!(first.enrolled, 1);
    let watch = WantedStore::new(db.clone())
        .get("rg-blue")
        .await
        .unwrap()
        .expect("watch persisted");
    assert_eq!(watch.state, WatchState::Watching);
    // A second sweep never enrols the same request twice, and the stored
    // due time is not reached yet.
    let again = wanted_tick(NOW, &rig.deps).await;
    assert_eq!((again.enrolled, again.checked), (0, 0));

    let due = watch.next_check_at as i64;
    let checked = wanted_tick(due, &rig.deps).await;
    assert_eq!((checked.checked, checked.dispatched), (1, 1));
    let queued = tasks(&db).await;
    assert_eq!(queued.len(), 1);
    assert_eq!(
        queued[0].origin,
        DispatchOrigin::Wanted,
        "a wanted find says so"
    );
    assert_eq!(queued[0].release_group_mbid, "rg-blue");
    let row = rig
        .deps
        .ledger
        .get(RequestKind::Album, "rg-blue")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(row.task_id.as_deref(), Some(queued[0].id.as_str()));
    assert_eq!(
        notices(&rig.events, "user-1").await,
        [names::WANTED_AUTO_DISPATCHED]
    );

    // The download completes (reported twice): the request reads imported,
    // its watch is fulfilled, and it is announced once.
    assert_eq!(status_of(&rig.deps.ledger, "rg-blue").await, "downloading");
    let task = tasks(&db).await.remove(0);
    let recorded = Arc::new(crate::common::bundles::FulfilledTicks::default());
    let journal = Journal::new(db.clone(), recorded.clone());
    for _ in 0..2 {
        let id = task.id.clone();
        let _ = journal
            .run("test.complete", move |store| {
                store.transition_task(&id, TaskStatus::Completed, NOW as f64, None)
            })
            .await;
    }
    assert_eq!(status_of(&rig.deps.ledger, "rg-blue").await, "imported");
    let watch = rig.deps.watches.get("rg-blue").await.unwrap().unwrap();
    assert_eq!(watch.state, WatchState::Fulfilled);
    assert_eq!(recorded.keys(), ["rg-blue"], "announced once");

    // Case "held by the library", on a rig of its own so its notices and
    // tasks are its own.
    let db = AcquireDb::scratch().unwrap();
    let rig = wanted(&db).await;
    seed_request(&rig.deps.ledger, "rg-blue", "failed", None).await;
    wanted_tick(NOW, &rig.deps).await;
    rig.library.add(&["rg-blue"]);
    let due = NOW + interval_seconds(None, 0, NOW);
    let summary = wanted_tick(due, &rig.deps).await;
    assert_eq!(summary.fulfilled, 1);
    assert_eq!(status_of(&rig.deps.ledger, "rg-blue").await, "imported");
    let watch = rig.deps.watches.get("rg-blue").await.unwrap().unwrap();
    assert_eq!(watch.state, WatchState::Fulfilled);
    assert_eq!(
        notices(&rig.events, "user-1").await,
        [names::WANTED_FULFILLED]
    );
    assert!(tasks(&db).await.is_empty(), "nothing is downloaded");
}

// The watcher asks only the sources that are turned on. With slskd set up
// but turned off there is no source to ask: the watch waits, still due,
// neither rescheduled nor counted quiet, and nothing starts.
#[tokio::test]
async fn wanted_waits_while_no_source_is_turned_on() {
    let db = AcquireDb::scratch().unwrap();
    let mut rig = wanted_with_slskd(&db, false).await;
    seed_request(&rig.deps.ledger, "rg-blue", "failed", None).await;
    wanted_tick(NOW, &rig.deps).await;
    let before = rig.deps.watches.get("rg-blue").await.unwrap().unwrap();
    let due = before.next_check_at as i64;
    let checked = wanted_tick(due, &rig.deps).await;
    assert_eq!((checked.checked, checked.dispatched), (1, 0));
    let after = rig.deps.watches.get("rg-blue").await.unwrap().unwrap();
    assert_eq!(after.next_check_at, before.next_check_at, "still due");
    assert_eq!(after.quiet_streak, 0, "not counted quiet");
    assert_eq!(after.check_count, before.check_count, "no check recorded");
    assert!(tasks(&db).await.is_empty(), "nothing is downloaded");

    // Download settings that do not read hold the watch the same way,
    // even with slskd turned on.
    let dir = db.path().parent().unwrap().join("broken-settings");
    let broken = crate::acquire_requests::unreadable_policy_store(&dir);
    let (sources, _slskd) = sources_over(&db, &broken, true).await;
    rig.deps.sources = sources;
    wanted_tick(due, &rig.deps).await;
    let held = rig.deps.watches.get("rg-blue").await.unwrap().unwrap();
    assert_eq!(held.next_check_at, before.next_check_at, "still due");
    assert_eq!(held.quiet_streak, 0, "not counted quiet");
    assert_eq!(held.check_count, before.check_count, "no check recorded");
    assert!(tasks(&db).await.is_empty(), "nothing is downloaded");
}

// A removed watch does not come back from the request's failed row. It comes
// back only once that request is written again and fails again.
#[tokio::test]
async fn a_removed_watch_stays_gone_until_the_request_fails_again() {
    let db = AcquireDb::scratch().unwrap();
    let rig = wanted(&db).await;
    seed_request(&rig.deps.ledger, "rg-blue", "failed", None).await;
    assert_eq!(wanted_tick(NOW, &rig.deps).await.enrolled, 1);
    assert!(
        rig.deps
            .watches
            .remove("rg-blue", "user-1", false)
            .await
            .unwrap()
    );
    assert_eq!(wanted_tick(NOW, &rig.deps).await.enrolled, 0);
    let row = rig
        .deps
        .ledger
        .get(RequestKind::Album, "rg-blue")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        row.status,
        RequestStatus::Failed,
        "the request is untouched"
    );

    assert!(
        rig.deps
            .ledger
            .update_status(
                RequestKind::Album,
                "rg-blue",
                RequestStatus::Failed,
                None,
                None
            )
            .await
            .unwrap()
    );
    assert_eq!(wanted_tick(NOW, &rig.deps).await.enrolled, 1);
}

// The status sweep settles what the journal did not: rows whose task
// finished behind its back (a failed row whose retry completed included),
// and album rows with no task to follow whose album the library holds. A
// row a person decided, or a short one whose task is gone, stays. A second
// pass finds nothing.
#[tokio::test]
async fn status_sweep_settles_what_the_journal_missed() {
    let db = AcquireDb::scratch().unwrap();
    db.add_user("user-1", "Owner", "user").await.unwrap();
    let ledger = RequestStore::new(db.clone());
    seed_request(&ledger, "rg-done", "downloading", Some("task-done")).await;
    seed_request(&ledger, "rg-short", "downloading", Some("task-short")).await;
    seed_request(&ledger, "rg-retried", "failed", Some("task-retry")).await;
    seed_request(&ledger, "rg-held", "pending", None).await;
    seed_request(&ledger, "rg-gone", "failed", Some("task-cleared")).await;
    seed_request(&ledger, "rg-partial", "incomplete", Some("task-cleared-2")).await;
    seed_request(&ledger, "rg-cancelled", "cancelled", None).await;
    seed_task(&db, "task-done", "rg-done", TaskStatus::Completed).await;
    seed_task(&db, "task-short", "rg-short", TaskStatus::Partial).await;
    seed_task(&db, "task-retry", "rg-retried", TaskStatus::Completed).await;
    let library = Arc::new(LibraryPresence::new());
    library.add(&["rg-held", "rg-gone", "rg-partial", "rg-cancelled"]);
    let recorded = Arc::new(crate::common::bundles::FulfilledTicks::default());
    let deps = SyncDeps {
        ledger: ledger.clone(),
        journal: Arc::new(Journal::new(db.clone(), recorded.clone())),
        library,
        ticks: recorded.clone(),
    };
    let summary = sync_tick(NOW, &deps).await;
    assert_eq!(
        (summary.settled, summary.imported, summary.errors),
        (3, 2, 0)
    );
    assert_eq!(status_of(&ledger, "rg-done").await, "imported");
    assert_eq!(status_of(&ledger, "rg-short").await, "incomplete");
    assert_eq!(status_of(&ledger, "rg-retried").await, "imported");
    assert_eq!(status_of(&ledger, "rg-held").await, "imported");
    assert_eq!(status_of(&ledger, "rg-gone").await, "imported");
    assert_eq!(status_of(&ledger, "rg-partial").await, "incomplete");
    assert_eq!(status_of(&ledger, "rg-cancelled").await, "cancelled");
    let done = ledger
        .get(RequestKind::Album, "rg-done")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(done.completed_at, Some(NOW as u64));
    let mut keys = recorded.keys();
    keys.sort();
    assert_eq!(keys, ["rg-done", "rg-gone", "rg-held", "rg-retried"]);

    let again = sync_tick(NOW, &deps).await;
    assert_eq!((again.settled, again.imported, again.errors), (0, 0, 0));
    assert_eq!(recorded.keys().len(), 4, "nothing is announced twice");
}

/// One page of an artist's release groups, as the MusicBrainz browse
/// endpoint answers it.
fn browse_page(releases: &[(&str, &str, &str, &[&str])]) -> String {
    let groups: Vec<serde_json::Value> = releases
        .iter()
        .map(|(id, date, kind, secondary)| {
            serde_json::json!({
                "id": id,
                "title": id,
                "first-release-date": date,
                "primary-type": kind,
                "secondary-types": secondary,
            })
        })
        .collect();
    serde_json::json!({
        "release-group-count": groups.len(),
        "release-group-offset": 0,
        "release-groups": groups,
    })
    .to_string()
}

// The follow poll baselines first, then adds new releases to the feed and
// queues one download for the first approved auto-download follower;
// future releases are held, owned and filtered ones never appear.
#[tokio::test]
async fn follow_poll_baselines_then_emits() {
    let db = AcquireDb::scratch().unwrap();
    db.write("seed follows", |tx| {
        tx.execute_batch(
            "INSERT INTO auth_users (id, display_name, role, created_at) VALUES \
               ('user-1', 'Admin', 'admin', 0), \
               ('user-2', 'Pending', 'user', 0); \
             INSERT INTO acquire_followed_artists (user_id, artist_mbid, artist_mbid_lower, \
               artist_name, auto_download, followed_at, updated_at) VALUES \
               ('user-2', 'Artist-1', 'artist-1', 'The Band', 1, 1, 1), \
               ('user-1', 'Artist-1', 'artist-1', 'The Band', 1, 1, 1);",
        )?;
        Ok(())
    })
    .await
    .unwrap();
    let follows = Follows::new(db.clone());
    // The first poll reads the old page; every later poll the new one.
    const MB: &str = "http://musicbrainz.test/ws/2";
    let wire = Arc::new(ScriptedWire::new());
    let json = [("content-type", "application/json")];
    let url = format!("{MB}/release-group");
    wire.answer(
        &url,
        200,
        &json,
        browse_page(&[("rg-old", "2020-01-01", "Album", &[])]),
    );
    wire.answer(
        &url,
        200,
        &json,
        browse_page(&[
            ("rg-old", "2020-01-01", "Album", &[]),
            ("rg-new", "2024-06-01", "Album", &[]),
            ("rg-future", "2025-01-01", "Single", &[]),
            ("rg-sloppy", "2024-06", "Album", &[]),
            ("rg-live", "2024-06-01", "Album", &["Live"]),
            ("rg-owned", "2024-06-01", "Album", &[]),
        ]),
    );
    let providers = droppedneedle_providers::Providers::unpaced(wire);
    let poll =
        droppedneedle_acquire::search::MusicBrainzReleasePoll::new(providers.musicbrainz.fixed(
            MbSource::Mirror {
                base_url: MB.to_owned(),
                rate_per_sec: 0.0,
            },
            RequestPriority::BackgroundSync,
        ));
    let library = Arc::new(LibraryPresence::new());
    library.add(&["rg-owned"]);
    let settings = settings(&db);
    let downloads = dispatch(&db, &settings);
    let requests = Arc::new(RequestsService::new(&RequestsState {
        quota: Arc::new(QuotaLedger::new(settings.live(), db.clone())),
        dispatch: downloads.clone(),
        ..requests_over(&db).await
    }));
    let deps = FollowDeps {
        follows: follows.clone(),
        poll: Arc::new(poll),
        downloads,
        requests: requests.clone(),
        library,
        ticks: ticks(&db, &EventHub::new()).await,
        preferences: settings.live(),
        today: Arc::new(|| "2024-06-01".to_owned()),
    };
    let first = follow_tick(NOW, &deps).await;
    assert_eq!(first.baselined, 1);
    assert!(tasks(&db).await.is_empty());
    let again = follow_tick(NOW + 3_600, &deps).await;
    assert_eq!(again.artists_polled, 0, "a polled artist waits a day");

    let second = follow_tick(NOW + 86_400, &deps).await;
    assert_eq!(
        second.new_releases, 2,
        "rg-new and rg-future reach the feed"
    );
    assert_eq!(second.enqueued, 1, "one task per release");
    let queued = tasks(&db).await;
    assert_eq!(queued.len(), 1);
    assert_eq!(queued[0].user_id, "user-1", "only the approved follower");
    assert_eq!(queued[0].release_group_mbid, "rg-new");
    assert_eq!(queued[0].artist_name, "The Band");
    let row = RequestStore::new(db.clone())
        .get(RequestKind::Album, "rg-new")
        .await
        .unwrap()
        .expect("the follow download has a request row");
    assert_eq!(row.user_id.as_deref(), Some("user-1"));
    assert_eq!(row.task_id.as_deref(), Some(queued[0].id.as_str()));
    let active = requests
        .active(&Principal::acting_for("user-1", Role::Admin, None))
        .await
        .unwrap();
    assert_eq!(active.items.len(), 1, "the Requests page lists it");
    assert_eq!(active.items[0].musicbrainz_id, "rg-new");
    assert_eq!(active.items[0].origin, RequestOrigin::Follow);
    let cursor = follows.cursor("artist-1").await.unwrap().unwrap();
    assert_eq!(cursor.pending.len(), 1);
    assert_eq!(cursor.pending[0].rg_mbid, "rg-future");
    let feed: Vec<String> = db.db().column(
        "SELECT release_group_mbid FROM acquire_new_release_feed ORDER BY release_group_mbid",
        [],
    );
    assert_eq!(feed, ["rg-future", "rg-new"]);
}

/// The upgrade sweep needs both gates and an admin, and honours its cap. An
/// upgrade asked for an album already downloading answers that task as
/// active and queues nothing.
#[tokio::test]
async fn upgrade_sweep_gates_cap_and_active() {
    let db = AcquireDb::scratch().unwrap();
    // Four albums below the cutoff, one 128 kbps file each.
    db.write("test.seed_library", |tx| {
        tx.execute_batch(
            "INSERT INTO local_artists (id, display_name, folded_name, kind, created_at, updated_at)
               VALUES ('ar1', 'A', 'a', 'person', 0, 0);",
        )?;
        for n in 0..4 {
            tx.execute(
                "INSERT INTO local_albums (id, root_id, grouping_key, title, title_folded,
                   album_artist_id, album_artist_name, grouping_source, created_at, updated_at)
                   VALUES (?1, 'r1', ?1, 'T', 't', 'ar1', 'A', 'automatic', 0, 0)",
                [format!("al{n}")],
            )?;
            tx.execute(
                "INSERT INTO local_tracks (id, local_album_id, root_id, file_path, relative_path,
                   path_hash, file_size_bytes, file_mtime_ns, stat_revision, title, title_folded,
                   album_title, album_title_folded, file_format, bit_rate, ingest_source,
                   imported_at, membership_source, availability, embedded_release_group_mbid)
                   VALUES (?1, ?2, 'r1', ?3, ?3, ?1, 1, 0, 's', 'T', 't', 'T', 't', 'mp3', 128,
                     'scan', 0, 'automatic', 'indexed', ?4)",
                [
                    format!("t{n}"),
                    format!("al{n}"),
                    format!("/m/{n}.mp3"),
                    format!("0a0b0c0d-0000-4000-8000-{n:012}"),
                ],
            )?;
        }
        Ok(())
    })
    .await
    .unwrap();
    db.add_user("user-1", "Owner", "user").await.unwrap();
    // The first album is already downloading, so the sweep queues nothing
    // for it.
    seed_task(
        &db,
        "task-0",
        "0a0b0c0d-0000-4000-8000-000000000000",
        TaskStatus::Downloading,
    )
    .await;
    let admins = AdminDirectory::over_users(db.clone());
    let settings = settings(&db);
    settings
        .save_blocking(DownloadPolicy {
            upgrade_allowed: true,
            quality_cutoff: "lossless".to_owned(),
            background_upgrade_scan_enabled: true,
            background_upgrade_max_per_run: 2,
            background_upgrade_scan_interval_hours: 12,
            ..DownloadPolicy::default()
        })
        .expect("download policy saves");
    let deps = SweepDeps {
        policy: settings.live(),
        worklist: Arc::new(UpgradeWorklist::new(db.clone())),
        admins,
        downloads: dispatch(&db, &settings),
        ticks: ticks(&db, &EventHub::new()).await,
        editions: settings.live(),
    };
    let idle = sweep_tick(NOW, &deps).await;
    assert!(idle.skipped_no_admin);
    db.add_user("admin-1", "Admin", "admin").await.unwrap();
    let swept = sweep_tick(NOW, &deps).await;
    assert_eq!(
        swept.enqueued, 2,
        "an album already downloading never counts"
    );
    let grabs: Vec<TaskRow> = tasks(&db)
        .await
        .into_iter()
        .filter(|task| task.origin == DispatchOrigin::Upgrade)
        .collect();
    assert_eq!(grabs.len(), 2);
    assert_eq!(
        grabs[0].release_group_mbid,
        "0a0b0c0d-0000-4000-8000-000000000001"
    );
    assert_eq!(grabs[0].user_id, "admin-1");

    let before = tasks(&db).await.len();
    let outcome = deps
        .downloads
        .dispatch(&DispatchRequest {
            user_id: "admin-1".to_owned(),
            target: DispatchTarget::Album {
                release_group_mbid: "0a0b0c0d-0000-4000-8000-000000000000".to_owned(),
            },
            artist_name: "Massive Attack".to_owned(),
            title: "T".to_owned(),
            origin: DispatchOrigin::Upgrade,
            release_mbid: None,
            wanted: Vec::new(),
            planned_album: None,
            idempotency_key: None,
        })
        .await
        .unwrap();
    assert_eq!(
        outcome,
        DispatchOutcome::Active {
            task_id: "task-0".to_owned()
        }
    );
    assert_eq!(
        tasks(&db).await.len(),
        before,
        "an active upgrade queues nothing"
    );
}

// A file's release-group tag stands in for the album's identity only
// while the album has none: an album identified as another group whose
// files still carry the old tag does not make that group held.
#[tokio::test]
async fn presence_trusts_a_tag_only_for_an_unidentified_album() {
    const RG_X: &str = "0a0b0c0d-0000-4000-8000-00000000000a";
    const RG_U: &str = "0a0b0c0d-0000-4000-8000-00000000000b";
    let db = AcquireDb::scratch().unwrap();
    db.write("test.seed_library", |tx| {
        tx.execute_batch(
            "INSERT INTO local_artists (id, display_name, folded_name, kind, created_at, updated_at)
               VALUES ('ar1', 'A', 'a', 'person', 0, 0);",
        )?;
        for (album, tag) in [("identified", RG_X), ("unidentified", RG_U)] {
            tx.execute(
                "INSERT INTO local_albums (id, root_id, grouping_key, title, title_folded,
                   album_artist_id, album_artist_name, grouping_source, created_at, updated_at)
                   VALUES (?1, 'r1', ?1, 'T', 't', 'ar1', 'A', 'automatic', 0, 0)",
                [album],
            )?;
            tx.execute(
                "INSERT INTO local_tracks (id, local_album_id, root_id, file_path, relative_path,
                   path_hash, file_size_bytes, file_mtime_ns, stat_revision, title, title_folded,
                   album_title, album_title_folded, file_format, bit_rate, ingest_source,
                   imported_at, membership_source, availability, embedded_release_group_mbid)
                   VALUES (?1, ?2, 'r1', ?1, ?1, ?1, 1, 0, 's', 'T', 't', 'T', 't', 'mp3', 320,
                     'scan', 0, 'automatic', 'indexed', ?3)",
                [format!("t-{album}"), album.to_owned(), tag.to_owned()],
            )?;
        }
        tx.execute_batch(
            "INSERT INTO local_album_external_identities
               (local_album_id, release_group_mbid, decision_source, selected_at)
               VALUES ('identified', 'rg-y', 'automatic', 0);",
        )?;
        Ok(())
    })
    .await
    .unwrap();
    let library = LibraryPresence::over_catalog(db.db().clone());
    assert!(
        !library.contains(RG_X).await,
        "an album identified as another group never holds its files' tag"
    );
    assert!(
        library.contains("rg-y").await,
        "the album's own group is held"
    );
    assert!(
        library.contains(RG_U).await,
        "an unidentified album's tag stands in for its identity"
    );
}

// The store prune drops settled requests past retention, never an active
// one.
#[tokio::test]
async fn prune_drops_only_settled_rows_past_retention() {
    let db = AcquireDb::scratch().unwrap();
    db.add_user("user-1", "Owner", "user").await.unwrap();
    let ledger = RequestStore::new(db.clone());
    seed_request(&ledger, "rg-done", "imported", None).await;
    seed_request(&ledger, "rg-live", "pending", None).await;
    let deps = PruneDeps {
        settings: settings(&db).live(),
        ledger: ledger.clone(),
        watches: WantedStore::new(db.clone()),
        search_jobs: JobStore::new(db.clone()),
    };

    let summary = prune_tick(NOW + 200 * 86_400, &deps).await.unwrap();
    assert_eq!(summary.requests, 1);
    assert!(
        ledger
            .get(RequestKind::Album, "rg-done")
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(status_of(&ledger, "rg-live").await, "pending");
}

/// `user-2`, a plain user, follows `artist-1` with auto-download approved.
async fn seed_approved_follower(db: &AcquireDb) {
    db.write("seed follows", |tx| {
        tx.execute_batch(
            "INSERT INTO auth_users (id, display_name, role, created_at) VALUES \
               ('user-2', 'Listener', 'user', 0); \
             INSERT INTO acquire_followed_artists (user_id, artist_mbid, artist_mbid_lower, \
               artist_name, auto_download, followed_at, updated_at) VALUES \
               ('user-2', 'Artist-1', 'artist-1', 'The Band', 1, 1, 1); \
             INSERT INTO auto_download_approvals (user_id, artist_mbid, artist_mbid_lower, \
               artist_name, state, requested_at) VALUES \
               ('user-2', 'Artist-1', 'artist-1', 'The Band', 'approved', 1);",
        )?;
        Ok(())
    })
    .await
    .unwrap();
}

/// A follow poll over `db` whose MusicBrainz page lists only `rg-old` on
/// the first poll and adds `rg-new` (2024-06-01) from the second, with the
/// request intake it files follow downloads through.
async fn follow_rig(db: &AcquireDb, settings: &ConfigStore) -> (FollowDeps, Arc<RequestsService>) {
    const MB: &str = "http://musicbrainz.test/ws/2";
    let wire = Arc::new(ScriptedWire::new());
    let json = [("content-type", "application/json")];
    let url = format!("{MB}/release-group");
    wire.answer(
        &url,
        200,
        &json,
        browse_page(&[("rg-old", "2020-01-01", "Album", &[])]),
    );
    wire.answer(
        &url,
        200,
        &json,
        browse_page(&[
            ("rg-old", "2020-01-01", "Album", &[]),
            ("rg-new", "2024-06-01", "Album", &[]),
        ]),
    );
    let providers = droppedneedle_providers::Providers::unpaced(wire);
    let poll =
        droppedneedle_acquire::search::MusicBrainzReleasePoll::new(providers.musicbrainz.fixed(
            MbSource::Mirror {
                base_url: MB.to_owned(),
                rate_per_sec: 0.0,
            },
            RequestPriority::BackgroundSync,
        ));
    let downloads = dispatch(db, settings);
    let requests = Arc::new(RequestsService::new(&RequestsState {
        quota: Arc::new(QuotaLedger::new(settings.live(), db.clone())),
        dispatch: downloads.clone(),
        ..requests_over(db).await
    }));
    let deps = FollowDeps {
        follows: Follows::new(db.clone()),
        poll: Arc::new(poll),
        downloads,
        requests: requests.clone(),
        library: Arc::new(LibraryPresence::new()),
        ticks: ticks(db, &EventHub::new()).await,
        preferences: settings.live(),
        today: Arc::new(|| "2024-06-01".to_owned()),
    };
    (deps, requests)
}

/// A follow download waits, is never dropped, and starts by itself: past
/// a full request quota, and past another user's ask that an admin
/// rejects.
#[tokio::test]
async fn follow_downloads_wait_and_start_by_themselves() {
    {
        // Over quota: a plain user's auto-download of a followed artist's
        // new release, made while their request quota is full, waits on the
        // Requests page and starts by itself once the window has room. A
        // start that cannot reach the download queue leaves it waiting,
        // never failed.
        let db = AcquireDb::scratch().unwrap();
        seed_approved_follower(&db).await;
        // The Requests page reads the quota at the real time, so the poll runs
        // on it too.
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;
        let settings = settings(&db);
        settings
            .save_blocking(DownloadPolicy {
                default_request_quota_count: 1,
                default_request_quota_days: 7,
                ..DownloadPolicy::default()
            })
            .expect("download policy saves");
        // One ask already in the window uses up the quota.
        let ledger = RequestStore::new(db.clone());
        let record = RequestRecord {
            key: "rg-asked".to_owned(),
            kind: RequestKind::Album,
            status: RequestStatus::Imported,
            artist_name: "The Band".to_owned(),
            album_title: "Asked".to_owned(),
            artist_mbid: None,
            year: None,
            release_mbid: None,
            track_title: None,
            duration_seconds: None,
            track_release_group_mbid: None,
            user_id: Some("user-2".to_owned()),
            origin: DispatchOrigin::User,
            requested_by_name: None,
            requesters: Vec::new(),
            requested_at: now as u64,
            completed_at: None,
            task_id: None,
            generation: 0,
            dispatch_authorized: true,
            monitor_artist: false,
            auto_download_artist: false,
            reviewed_by_name: None,
            reviewed_at: None,
        };
        ledger
            .begin_batch(vec![record], "user-2", None)
            .await
            .unwrap()
            .unwrap();
        let (deps, requests) = follow_rig(&db, &settings).await;
        follow_tick(now, &deps).await;
        let polled = follow_tick(now + 86_400, &deps).await;
        assert_eq!(polled.enqueued, 0);
        assert!(tasks(&db).await.is_empty(), "nothing is downloaded");
        let follower = Principal::acting_for("user-2", Role::User, None);
        let active = requests.active(&follower).await.unwrap();
        let waiting = active
            .items
            .iter()
            .find(|item| item.musicbrainz_id == "rg-new")
            .expect("the follow download is on the Requests page");
        assert_eq!(waiting.origin, RequestOrigin::Follow);
        assert_eq!(
            waiting.waiting_reason,
            Some(WaitingReason::RequestQuotaFull)
        );

        let still = follow_tick(now + 86_400 + 60, &deps).await;
        assert_eq!(still.enqueued, 0, "the window is still full");
        assert!(tasks(&db).await.is_empty());

        // The earlier ask leaves the seven-day window, but the download queue
        // refuses the start: the request goes back to waiting.
        db.write("refuse downloads", |tx| {
            tx.execute_batch(
                "CREATE TRIGGER refuse_downloads BEFORE INSERT ON download_tasks \
                 BEGIN SELECT RAISE(ABORT, 'refused'); END;",
            )?;
            Ok(())
        })
        .await
        .unwrap();
        let refused = follow_tick(now + 8 * 86_400, &deps).await;
        assert_eq!(refused.enqueued, 0);
        let row = ledger
            .get(RequestKind::Album, "rg-new")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(row.status, RequestStatus::Queued, "never failed");
        assert_eq!(row.task_id, None);
        db.write("accept downloads", |tx| {
            tx.execute_batch("DROP TRIGGER refuse_downloads;")?;
            Ok(())
        })
        .await
        .unwrap();

        let freed = follow_tick(now + 8 * 86_400 + 60, &deps).await;
        assert_eq!(freed.enqueued, 1, "the waiting request starts by itself");
        let started = tasks(&db).await;
        assert_eq!(started.len(), 1);
        assert_eq!(started[0].release_group_mbid, "rg-new");
        assert_eq!(started[0].user_id, "user-2");
        let row = ledger
            .get(RequestKind::Album, "rg-new")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(row.task_id.as_deref(), Some(started[0].id.as_str()));
        assert_eq!(row.status, RequestStatus::Downloading);
    }
    {
        // Past a rejected ask: a follow download that meets another user's
        // ask still waiting for an admin keeps the release held; when that
        // ask is rejected, the follower's own download goes ahead.
        let db = AcquireDb::scratch().unwrap();
        seed_approved_follower(&db).await;
        db.add_user("user-3", "Asker", "user").await.unwrap();
        let ledger = RequestStore::new(db.clone());
        let asked = RequestRecord {
            key: "rg-new".to_owned(),
            kind: RequestKind::Album,
            status: RequestStatus::AwaitingApproval,
            artist_name: "The Band".to_owned(),
            album_title: "rg-new".to_owned(),
            artist_mbid: None,
            year: None,
            release_mbid: None,
            track_title: None,
            duration_seconds: None,
            track_release_group_mbid: None,
            user_id: Some("user-3".to_owned()),
            origin: DispatchOrigin::User,
            requested_by_name: None,
            requesters: Vec::new(),
            requested_at: NOW as u64,
            completed_at: None,
            task_id: None,
            generation: 0,
            dispatch_authorized: false,
            monitor_artist: false,
            auto_download_artist: false,
            reviewed_by_name: None,
            reviewed_at: None,
        };
        ledger
            .begin_batch(vec![asked], "user-3", None)
            .await
            .unwrap()
            .unwrap();
        let settings = settings(&db);
        let (deps, _requests) = follow_rig(&db, &settings).await;
        follow_tick(NOW, &deps).await;
        let met = follow_tick(NOW + 86_400, &deps).await;
        assert_eq!(met.enqueued, 0);
        assert!(tasks(&db).await.is_empty(), "the ask waits for an admin");

        assert!(
            ledger
                .update_status(
                    RequestKind::Album,
                    "rg-new",
                    RequestStatus::Rejected,
                    Some(NOW as u64 + 90_000),
                    None,
                )
                .await
                .unwrap()
        );
        let later = follow_tick(NOW + 2 * 86_400, &deps).await;
        assert_eq!(later.enqueued, 1, "the follower's download goes ahead");
        let started = tasks(&db).await;
        assert_eq!(started.len(), 1);
        assert_eq!(started[0].user_id, "user-2");
        assert_eq!(started[0].release_group_mbid, "rg-new");
    }
}

/// A release group key the request routes accept.
const RG_WANTED: &str = "44444444-4444-4444-8444-444444444444";

// Cancelling a request the wanted watcher downloaded again stops its watch,
// even while the watcher is searching for the album: the search that
// answers after the cancel starts nothing, and the request stays cancelled.
#[tokio::test]
async fn cancelling_a_wanted_download_stops_its_watch() {
    let db = AcquireDb::scratch().unwrap();
    let rig = wanted(&db).await;
    seed_request(&rig.deps.ledger, RG_WANTED, "failed", None).await;
    wanted_tick(NOW, &rig.deps).await;
    let watch = rig.deps.watches.get(RG_WANTED).await.unwrap().unwrap();
    let checked = wanted_tick(watch.next_check_at as i64, &rig.deps).await;
    assert_eq!(checked.dispatched, 1);

    let requests = RequestsService::new(&RequestsState {
        quota: Arc::new(QuotaLedger::unlimited(db.clone())),
        dispatch: rig.deps.downloads.clone(),
        ..requests_over(&db).await
    });
    let owner = Principal::acting_for("user-1", Role::User, None);
    let watch = rig.deps.watches.get(RG_WANTED).await.unwrap().unwrap();
    rig.slskd.reset();
    rig.slskd.set_handler_delay_ms(1_000);
    let cancel = async {
        let deadline = std::time::Instant::now() + Duration::from_secs(30);
        while rig.slskd.max_in_flight() == 0 {
            assert!(
                std::time::Instant::now() < deadline,
                "the watcher's search never reached slskd"
            );
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        requests
            .cancel_one(&owner, RG_WANTED, RequestKind::Album)
            .await
            .unwrap();
    };
    let (checked, ()) = tokio::join!(wanted_tick(watch.next_check_at as i64, &rig.deps), cancel);
    assert_eq!(checked.dispatched, 0);
    assert_eq!(status_of(&rig.deps.ledger, RG_WANTED).await, "cancelled");
    let watch = rig.deps.watches.get(RG_WANTED).await.unwrap().unwrap();
    assert_eq!(watch.state, WatchState::Stopped);
    assert_eq!(tasks(&db).await.len(), 1, "no second download");
}

// A re-ask an admin rejects stops its album's watch the way a cancel does:
// with auto-download on, the watcher downloads nothing for it. Asking for
// the album again, whoever asks, arms the watch again for them.
#[tokio::test]
async fn a_rejected_re_ask_stops_its_watch() {
    let db = AcquireDb::scratch().unwrap();
    let rig = wanted(&db).await;
    seed_request(&rig.deps.ledger, RG_WANTED, "failed", None).await;
    wanted_tick(NOW, &rig.deps).await;

    let requests = RequestsService::new(&RequestsState {
        quota: Arc::new(QuotaLedger::unlimited(db.clone())),
        dispatch: rig.deps.downloads.clone(),
        ..requests_over(&db).await
    });
    let owner = Principal::acting_for("user-1", Role::User, None);
    let ask = AlbumIntake {
        musicbrainz_id: RG_WANTED.to_owned(),
        artist: Some("Massive Attack".to_owned()),
        album: Some("Blue Lines".to_owned()),
        year: None,
        artist_mbid: None,
        release_mbid: None,
        monitor_artist: false,
        auto_download_artist: false,
    };
    requests.request_album(&owner, &ask).await.unwrap();
    requests
        .reject_request(
            &crate::common::auth::test_admin("root"),
            RG_WANTED,
            RequestKind::Album,
        )
        .await
        .unwrap();
    assert_eq!(status_of(&rig.deps.ledger, RG_WANTED).await, "rejected");

    let watch = rig.deps.watches.get(RG_WANTED).await.unwrap().unwrap();
    let checked = wanted_tick(watch.next_check_at as i64, &rig.deps).await;
    assert_eq!(checked.dispatched, 0);
    assert!(tasks(&db).await.is_empty(), "nothing downloads");
    let watch = rig.deps.watches.get(RG_WANTED).await.unwrap().unwrap();
    assert_eq!(watch.state, WatchState::Stopped);

    db.add_user("user-2", "Other", "user").await.unwrap();
    let other = Principal::acting_for("user-2", Role::User, None);
    requests.request_album(&other, &ask).await.unwrap();
    let watch = rig.deps.watches.get(RG_WANTED).await.unwrap().unwrap();
    assert_eq!(watch.state, WatchState::Watching, "a new ask watches again");
    assert_eq!(watch.user_id, "user-2", "for the person who asked");
    assert!(
        watch.next_check_at > watch.created_at,
        "not while the new ask's own download starts"
    );
}
