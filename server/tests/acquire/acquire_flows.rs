//! Acquisition flows over the durable stores: the wanted watcher, status
//! sync, follow poll, upgrade sweep and drop import.
//!
//! Ticks take an explicit `now`; the loop test runs on paused tokio time
//! and bounds its waits in wall-clock time. Stores sit on a scratch
//! database and downloads go into the real journal on it. The wanted
//! watcher searches the sources the saved settings turn on (the slskd
//! mock), the follow poll reads MusicBrainz pages from a scripted wire,
//! and flow notices go to a real event hub.

use std::sync::Arc;
use std::time::Duration;

use crate::common::bundles::quiet_ticks;
use droppedneedle_acquire::clients::LiveClients;
use droppedneedle_acquire::db::AcquireDb;
use droppedneedle_acquire::dispatch::{
    DispatchOrigin, DispatchOutcome, DispatchRequest, DispatchTarget, DownloadDispatch,
};
use droppedneedle_acquire::downloads::journal::Journal;
use droppedneedle_acquire::downloads::state::TaskStatus;
use droppedneedle_acquire::downloads::store::{NewTask, TaskRow};
use droppedneedle_acquire::downloads::worker::SourceList;
use droppedneedle_acquire::downloads::worker::WorkerSettings;
use droppedneedle_acquire::flows::{
    loops::{
        FollowDeps, PruneDeps, SYNC_INITIAL_DELAY, SYNC_INTERVAL, SYNC_JOB, SweepDeps, SyncDeps,
        WantedDeps, follow_tick, interval_seconds, prune_tick, spawn_sync_loop, sweep_tick,
        sync_tick, wanted_tick,
    },
    seams::TickSink,
    stores::{AdminDirectory, LibraryPresence, UpgradeWorklist},
};
use droppedneedle_acquire::follows::Follows;
use droppedneedle_acquire::plugin_events::FlowTicks;
use droppedneedle_acquire::requests::{
    ledger::{BeginOutcome, RequestRecord, RequestStatus, WatchState},
    models::{RequestKind, WaitingReason},
    quota::QuotaLedger,
    service::RequestsService,
    sqlite::{RequestStore, WantedStore},
    state::RequestsState,
};
use droppedneedle_acquire::search_jobs::store::JobStore;
use droppedneedle_acquire::target::Targets;
use droppedneedle_acquire::wiring::ClientLives;
use droppedneedle_auth::{Principal, Role};
use droppedneedle_kernel::db::{DbRuntime, JobState};
use droppedneedle_kernel::events::EventHub;
use droppedneedle_kernel::events::model::names;
use droppedneedle_kernel::ids::UuidGenerator;
use droppedneedle_kernel::jobs::Supervisor;
use droppedneedle_kernel::runtime_config::ConfigStore;
use droppedneedle_kernel::runtime_config::secret_sections::SlskdConnection;
use droppedneedle_kernel::runtime_config::sections::{DownloadPolicy, WantedWatcher};
use droppedneedle_providers::RequestPriority;
use droppedneedle_providers::executor::ScriptedWire;
use droppedneedle_providers::musicbrainz::MbSource;
use droppedneedle_testkit::slskd::{MOCK_API_KEY, MockSlskd};

const NOW: i64 = 1_700_000_000;
/// Scratch directory, removed when the test ends.
fn scratch_dir(name: &str) -> crate::common::ScratchDir {
    crate::common::ScratchDir::new(name)
}

/// A served runtime for the tests that need the job registry.
async fn runtime(name: &str) -> (DbRuntime, AcquireDb, crate::common::ScratchDir) {
    let dir = scratch_dir(name);
    let runtime = crate::common::scratch_runtime(&dir.join("app.db")).await;
    let db = AcquireDb::new(
        runtime.db().clone(),
        std::sync::Arc::new(droppedneedle_kernel::clock::SystemClock),
    );
    (runtime, db, dir)
}

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
/// journal would: its requests are left for the status sweep.
async fn behind(db: &AcquireDb, id: &str, status: TaskStatus) {
    let id = id.to_owned();
    db.write("test.behind", move |tx| {
        tx.execute(
            "UPDATE download_tasks SET status = ?2, completed_at = ?3 WHERE id = ?1",
            rusqlite::params![id, status.as_str(), NOW as f64],
        )?;
        Ok(())
    })
    .await
    .expect("task moves");
}

/// Every task in the journal, oldest first.
async fn tasks(db: &AcquireDb) -> Vec<TaskRow> {
    let ids: Vec<String> =
        sqlx::query_scalar("SELECT id FROM download_tasks ORDER BY created_at, rowid")
            .fetch_all(db.db().sqlx())
            .await
            .expect("task ids read");
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

/// The production tick sink over `events`, with no plugin host.
fn ticks(events: &EventHub) -> Arc<dyn TickSink> {
    Arc::new(FlowTicks::new(None, events.clone()))
}

struct Wanted {
    deps: WantedDeps,
    library: Arc<LibraryPresence>,
    events: EventHub,
    _slskd: MockSlskd,
}

/// The sources the saved settings turn on, over the slskd mock, with
/// slskd turned on or off.
async fn sources_over(
    db: &AcquireDb,
    settings: &ConfigStore,
    enabled: bool,
) -> (SourceList, MockSlskd) {
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
        ClientLives::new(settings),
        dir.join("slskd"),
        journal.clone(),
        Arc::new(Targets::new(journal, None)),
        None,
    ));
    let sources = SourceList::Live(Arc::new(WorkerSettings::new(settings, clients)));
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
                None,
            )),
            ticks: ticks(&events),
        },
        library,
        events,
        _slskd: slskd,
    }
}

// A failed request enrols a durable watch; once due with a candidate it
// auto-dispatches and links the task to the request.
#[tokio::test]
async fn wanted_enrols_then_auto_downloads() {
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
    assert_eq!(queued[0].origin, "wanted", "a wanted find says so");
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

// A watch the library now holds is fulfilled and its request imported.
#[tokio::test]
async fn wanted_satisfied_from_the_library() {
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
               ('user-1', 'Admin', 'admin', '2024-01-01'), \
               ('user-2', 'Pending', 'user', '2024-01-01'); \
             INSERT INTO user_followed_artists (user_id, artist_mbid, artist_mbid_lower, \
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
    let requests = Arc::new(RequestsService::new(&RequestsState::new(
        &db,
        Arc::new(QuotaLedger::new(settings.live(), db.clone())),
        downloads.clone(),
    )));
    let deps = FollowDeps {
        follows: follows.clone(),
        poll: Arc::new(poll),
        downloads,
        requests: requests.clone(),
        library,
        ticks: ticks(&EventHub::new()),
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
    assert_eq!(active.items[0].origin, DispatchOrigin::Follow);
    let cursor = follows.cursor("artist-1").await.unwrap().unwrap();
    assert_eq!(cursor.pending.len(), 1);
    assert_eq!(cursor.pending[0].rg_mbid, "rg-future");
    let feed: Vec<String> = sqlx::query_scalar(
        "SELECT release_group_mbid FROM new_release_feed ORDER BY release_group_mbid",
    )
    .fetch_all(db.db().sqlx())
    .await
    .unwrap();
    assert_eq!(feed, ["rg-future", "rg-new"]);
}

// The upgrade sweep needs both gates and an admin, and honours its cap.
#[tokio::test]
async fn upgrade_sweep_gates_and_cap() {
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
                    format!("rg-{n}"),
                ],
            )?;
        }
        Ok(())
    })
    .await
    .unwrap();
    db.add_user("user-1", "Owner", "user").await.unwrap();
    // rg-0 is already downloading, so the sweep queues nothing for it.
    seed_task(&db, "task-0", "rg-0", TaskStatus::Downloading).await;
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
        ticks: ticks(&EventHub::new()),
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
        .filter(|task| task.origin == "upgrade")
        .collect();
    assert_eq!(grabs.len(), 2);
    assert_eq!(grabs[0].release_group_mbid, "rg-1");
    assert_eq!(grabs[0].user_id, "admin-1");
}

// A file's release-group tag stands in for the album's identity only
// while the album has none: an album identified as another group whose
// files still carry the old tag does not make that group held.
#[tokio::test]
async fn presence_trusts_a_tag_only_for_an_unidentified_album() {
    let db = AcquireDb::scratch().unwrap();
    db.write("test.seed_library", |tx| {
        tx.execute_batch(
            "INSERT INTO local_artists (id, display_name, folded_name, kind, created_at, updated_at)
               VALUES ('ar1', 'A', 'a', 'person', 0, 0);",
        )?;
        for (album, tag) in [("identified", "rg-x"), ("unidentified", "rg-u")] {
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
        !library.contains("rg-x").await,
        "an album identified as another group never holds its files' tag"
    );
    assert!(
        library.contains("rg-y").await,
        "the album's own group is held"
    );
    assert!(
        library.contains("rg-u").await,
        "an unidentified album's tag stands in for its identity"
    );
}

// The sync loop waits its startup delay, runs a pass, waits the honest
// interval and parks the job stopped on shutdown; a stop during the delay
// still parks it stopped without running a pass.
#[tokio::test]
async fn loop_cadence_and_shutdown() {
    let (runtime, db, _dir) = runtime("loops").await;
    db.add_user("user-1", "Owner", "user").await.unwrap();
    let ledger = RequestStore::new(db.clone());
    seed_request(&ledger, "rg-1", "downloading", Some("task-9")).await;
    seed_request(&ledger, "rg-2", "downloading", Some("task-10")).await;
    seed_task(&db, "task-9", "rg-1", TaskStatus::Completed).await;
    seed_task(&db, "task-10", "rg-2", TaskStatus::Downloading).await;
    // A held album with no task: settling it is the pass's last step.
    seed_request(&ledger, "rg-held", "pending", None).await;
    let library = Arc::new(LibraryPresence::new());
    library.add(&["rg-held"]);
    let announced = Arc::new(crate::common::bundles::FulfilledTicks::default());
    let deps = Arc::new(SyncDeps {
        ledger: ledger.clone(),
        journal: Arc::new(Journal::new(db.clone(), quiet_ticks())),
        library,
        ticks: announced.clone(),
    });
    let supervisor = Supervisor::start(
        runtime.db().clone(),
        Arc::new(droppedneedle_kernel::clock::SystemClock),
        droppedneedle_kernel::jobs::CancellationToken::new(),
    )
    .await
    .unwrap();
    tokio::time::pause();

    spawn_sync_loop(&supervisor, deps.clone()).unwrap();
    settle_loop().await;
    tokio::time::advance(SYNC_INITIAL_DELAY - Duration::from_secs(1)).await;
    settle_loop().await;
    supervisor.cancel(SYNC_JOB, GRACE).await;
    assert_eq!(
        unpaused_status(&ledger, "rg-1").await,
        "downloading",
        "a pass ran inside the startup delay"
    );
    let row =
        crate::common::loop_row(runtime.db(), SYNC_JOB, |row| row.state != JobState::Running).await;
    assert_eq!(row.state, JobState::Stopped);

    spawn_sync_loop(&supervisor, deps).unwrap();
    settle_loop().await;
    tokio::time::advance(SYNC_INITIAL_DELAY).await;
    // The announce is the pass's last act, and the loop arms its interval
    // in the same poll: advancing before then would start the interval
    // late.
    until("the first pass", || {
        let done = announced.keys() == ["rg-held"];
        async move { done }
    })
    .await;
    assert_eq!(unpaused_status(&ledger, "rg-1").await, "imported");
    // Written with the clock running: an idle runtime on a paused clock
    // would jump straight to the loop's next pass while the write waits.
    tokio::time::resume();
    behind(&db, "task-10", TaskStatus::Completed).await;
    tokio::time::pause();
    tokio::time::advance(SYNC_INTERVAL - Duration::from_secs(1)).await;
    settle_loop().await;
    assert_eq!(
        unpaused_status(&ledger, "rg-2").await,
        "downloading",
        "a pass ran inside the interval"
    );
    tokio::time::advance(Duration::from_secs(1)).await;
    until("the second pass", || async {
        status_of(&ledger, "rg-2").await == "imported"
    })
    .await;

    supervisor.shutdown(GRACE).await;
    // Store reads wait for a pooled connection on a tokio timer, which the
    // paused clock would run out at once while a blocking read holds one.
    tokio::time::resume();
    let row =
        crate::common::loop_row(runtime.db(), SYNC_JOB, |row| row.state != JobState::Running).await;
    assert_eq!(row.state, JobState::Stopped);
    assert_eq!(status_of(&ledger, "rg-1").await, "imported");
    assert_eq!(status_of(&ledger, "rg-2").await, "imported");
    runtime.shutdown().await;
}

const GRACE: Duration = Duration::from_secs(5);

/// Let the loop run what is due now, or start its wait. Yields instead of
/// sleeping, so the paused clock stays put.
async fn settle_loop() {
    for _ in 0..50 {
        tokio::task::yield_now().await;
        std::thread::sleep(Duration::from_millis(2));
    }
}

/// A request's status, read with the clock running: the read waits for a
/// pooled connection on a tokio timer, which the paused clock could run
/// out at once. The loop's next wait is a minute away, so it stays put.
async fn unpaused_status(ledger: &RequestStore, key: &str) -> String {
    tokio::time::resume();
    let status = status_of(ledger, key).await;
    tokio::time::pause();
    status
}

/// Wait until `done` answers true, bounded in wall-clock time. The pass's
/// database calls wait on tokio timers too, so each turn moves the paused
/// clock by a millisecond.
async fn until<F: Future<Output = bool>>(what: &str, mut done: impl FnMut() -> F) {
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    loop {
        // Checked with the clock running, as in `unpaused_status`.
        tokio::time::resume();
        let done = done().await;
        tokio::time::pause();
        if done {
            return;
        }
        assert!(std::time::Instant::now() < deadline, "{what} never came");
        tokio::time::advance(Duration::from_millis(1)).await;
        std::thread::sleep(Duration::from_millis(2));
    }
}

// The wanted recheck table's age bands.
#[test]
fn recheck_bands() {
    let now = 1_716_720_000; // 2024-05-26 UTC
    assert_eq!(interval_seconds(Some("2024-05-20"), 0, now), 2 * 86_400);
    assert_eq!(interval_seconds(Some("2024-04-01"), 0, now), 4 * 86_400);
    assert_eq!(interval_seconds(Some("2024-01-15"), 0, now), 7 * 86_400);
    assert_eq!(interval_seconds(Some("2020-01-01"), 0, now), 14 * 86_400);
    assert_eq!(interval_seconds(Some("2020-01-01"), 10, now), 28 * 86_400);
    assert_eq!(interval_seconds(None, 0, now), 14 * 86_400);
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

// An upgrade asked for an album that is already downloading answers that
// task as active and queues nothing.
#[tokio::test]
async fn upgrade_of_a_downloading_album_answers_active() {
    let db = AcquireDb::scratch().unwrap();
    db.add_user("user-1", "Owner", "user").await.unwrap();
    db.add_user("admin-1", "Admin", "admin").await.unwrap();
    seed_task(&db, "task-0", "rg-0", TaskStatus::Downloading).await;
    let outcome = dispatch(&db, &settings(&db))
        .dispatch(&DispatchRequest {
            user_id: "admin-1".to_owned(),
            target: DispatchTarget::Album {
                release_group_mbid: "rg-0".to_owned(),
            },
            artist_name: "Massive Attack".to_owned(),
            title: "rg-0".to_owned(),
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
    assert_eq!(tasks(&db).await.len(), 1);
}

/// `user-2`, a plain user, follows `artist-1` with auto-download approved.
async fn seed_approved_follower(db: &AcquireDb) {
    db.write("seed follows", |tx| {
        tx.execute_batch(
            "INSERT INTO auth_users (id, display_name, role, created_at) VALUES \
               ('user-2', 'Listener', 'user', '2024-01-01'); \
             INSERT INTO user_followed_artists (user_id, artist_mbid, artist_mbid_lower, \
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
fn follow_rig(db: &AcquireDb, settings: &ConfigStore) -> (FollowDeps, Arc<RequestsService>) {
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
    let requests = Arc::new(RequestsService::new(&RequestsState::new(
        db,
        Arc::new(QuotaLedger::new(settings.live(), db.clone())),
        downloads.clone(),
    )));
    let deps = FollowDeps {
        follows: Follows::new(db.clone()),
        poll: Arc::new(poll),
        downloads,
        requests: requests.clone(),
        library: Arc::new(LibraryPresence::new()),
        ticks: ticks(&EventHub::new()),
        preferences: settings.live(),
        today: Arc::new(|| "2024-06-01".to_owned()),
    };
    (deps, requests)
}

// A plain user's auto-download of a followed artist's new release, made
// while their request quota is full, is a request that waits on the
// Requests page and starts by itself once the window has room. It is never
// dropped, and a start that cannot reach the download queue leaves it
// waiting, never failed.
#[tokio::test]
async fn follow_download_waits_while_the_follower_is_over_quota() {
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
        status: RequestStatus::Pending,
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
    let (deps, requests) = follow_rig(&db, &settings);
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
    assert_eq!(waiting.origin, DispatchOrigin::Follow);
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

// A follow download that meets another user's ask still waiting for an
// admin keeps the release held; when that ask is rejected, the follower's
// own download goes ahead.
#[tokio::test]
async fn follow_download_outlives_a_rejected_ask_it_met() {
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
    let (deps, _requests) = follow_rig(&db, &settings);
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

// A failed request the wanted watcher downloads again reads downloading,
// then imported once that download completes, with its watch fulfilled and
// the request announced once.
#[tokio::test]
async fn wanted_redispatch_imports_the_failed_request() {
    let db = AcquireDb::scratch().unwrap();
    let rig = wanted(&db).await;
    seed_request(&rig.deps.ledger, "rg-blue", "failed", None).await;
    wanted_tick(NOW, &rig.deps).await;
    let watch = rig.deps.watches.get("rg-blue").await.unwrap().unwrap();
    let checked = wanted_tick(watch.next_check_at as i64, &rig.deps).await;
    assert_eq!(checked.dispatched, 1);
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
}
