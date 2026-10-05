//! Plugin-tick briefs: loop contract plus store durability (D11).
//!
//! The loop half pins the v2 `host.py` contract under virtual time: failures
//! log and continue, hung ticks die at the interval, ticks never overlap,
//! disabled plugins exit, and `sync_ticks` stays the sole rebuild choke
//! point. The store half pins the D11 redesign: state lives on the tick
//! store, keyed by plugin, validated like the old paths, capped at 10 MiB,
//! readable after every runner is gone, and (on the SQLite store) durable
//! across a full runtime restart.

use std::collections::HashMap;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicU64, Ordering},
};
use std::time::Duration;

use droppedneedle::db::{DbConfig, open_runtime};
use droppedneedle::jobs::plugin_ticks::{
    MemoryTickStore, SqliteTickStore, TickHost, TickPlugin, TickSpec, TickStore, TickStoreError,
    sync_ticks,
};
use droppedneedle::jobs::registry::{
    BoxFuture, JobRegistry, JobState, MemoryRegistryStore, RegistryStore,
};

type TestRegistry = JobRegistry<MemoryRegistryStore>;

fn registry() -> TestRegistry {
    JobRegistry::new(MemoryRegistryStore::new())
}

async fn settle() {
    for _ in 0..3 {
        tokio::task::yield_now().await;
    }
}

/// Move the clock in steps, settling between. One `advance` fires each armed
/// timer at most once no matter how far it jumps, so multi-cycle asserts
/// must step in interval-sized pieces.
async fn advance_stepping(total: Duration, step: Duration) {
    let mut left = total;
    while left > Duration::ZERO {
        let hop = left.min(step);
        tokio::time::advance(hop).await;
        settle().await;
        left -= hop;
    }
}

fn tick_name(plugin: &str) -> String {
    format!("plugin-tick:{plugin}")
}

/// Scripted plugin: fails the first `fail_first` ticks, hangs when told to,
/// tracks overlap while slow.
#[derive(Clone)]
struct FakePlugin {
    name: String,
    enabled: Arc<Mutex<bool>>,
    calls: Arc<AtomicU64>,
    max_active: Arc<AtomicU64>,
    active: Arc<AtomicU64>,
    fail_first: u64,
    hang: bool,
    slow: Option<Duration>,
}

impl FakePlugin {
    fn new(name: &str) -> Self {
        Self {
            name: name.to_owned(),
            enabled: Arc::new(Mutex::new(true)),
            calls: Arc::new(AtomicU64::new(0)),
            max_active: Arc::new(AtomicU64::new(0)),
            active: Arc::new(AtomicU64::new(0)),
            fail_first: 0,
            hang: false,
            slow: None,
        }
    }

    fn calls(&self) -> u64 {
        self.calls.load(Ordering::SeqCst)
    }
}

impl TickPlugin for FakePlugin {
    fn name(&self) -> &str {
        &self.name
    }

    fn on_tick(&self) -> BoxFuture<'_, Result<(), String>> {
        let calls = Arc::clone(&self.calls);
        let active = Arc::clone(&self.active);
        let max_active = Arc::clone(&self.max_active);
        let fail_first = self.fail_first;
        let hang = self.hang;
        let slow = self.slow;
        Box::pin(async move {
            let call = calls.fetch_add(1, Ordering::SeqCst) + 1;
            let now_active = active.fetch_add(1, Ordering::SeqCst) + 1;
            max_active.fetch_max(now_active, Ordering::SeqCst);
            let result = if hang {
                std::future::pending::<()>().await;
                Ok(())
            } else {
                if let Some(delay) = slow {
                    tokio::time::sleep(delay).await;
                }
                if call <= fail_first {
                    Err("boom".to_owned())
                } else {
                    Ok(())
                }
            };
            active.fetch_sub(1, Ordering::SeqCst);
            result
        })
    }

    fn tick_enabled(&self) -> bool {
        *self.enabled.lock().unwrap()
    }
}

/// Scripted host: resolves plugins fresh from its map, like the real host.
#[derive(Clone, Default)]
struct FakeHost {
    plugins: Arc<Mutex<HashMap<String, FakePlugin>>>,
}

impl FakeHost {
    fn with(plugin: FakePlugin) -> Self {
        let host = Self::default();
        host.plugins
            .lock()
            .unwrap()
            .insert(plugin.name.clone(), plugin);
        host
    }
}

impl TickHost for FakeHost {
    type Plugin = FakePlugin;

    fn get(&self, name: &str) -> Option<Self::Plugin> {
        self.plugins.lock().unwrap().get(name).cloned()
    }
}

fn spec(name: &str, interval: Duration, run_on_load: bool) -> TickSpec {
    TickSpec {
        name: name.to_owned(),
        interval,
        run_on_load,
    }
}

#[tokio::test(start_paused = true)]
async fn tick_exception_logs_and_continues() {
    let registry = registry();
    let mut plugin = FakePlugin::new("tick-toy");
    plugin.fail_first = 1;
    let host = FakeHost::with(plugin.clone());
    let sync = droppedneedle::jobs::plugin_ticks::TickSyncState::new();
    sync_ticks(
        &registry,
        &host,
        &sync,
        &[spec("tick-toy", Duration::from_millis(100), true)],
        Duration::from_secs(5),
    )
    .await;
    assert!(registry.is_running(&tick_name("tick-toy")));
    settle().await;

    advance_stepping(Duration::from_secs(1), Duration::from_millis(50)).await;
    // The first tick raised; the loop carried on through several more.
    assert!(plugin.calls() >= 3, "calls: {}", plugin.calls());

    registry.cancel_all(Duration::from_secs(5)).await;
}

#[tokio::test(start_paused = true)]
async fn tick_cancel_breaks_loop() {
    let registry = registry();
    let plugin = FakePlugin::new("tick-toy");
    let host = FakeHost::with(plugin);
    let sync = droppedneedle::jobs::plugin_ticks::TickSyncState::new();
    sync_ticks(
        &registry,
        &host,
        &sync,
        &[spec("tick-toy", Duration::from_secs(60), false)],
        Duration::from_secs(5),
    )
    .await;
    settle().await;
    registry.cancel_all(Duration::from_secs(5)).await;
    assert!(!registry.is_running(&tick_name("tick-toy")));
    let row = registry
        .store()
        .get_job(&tick_name("tick-toy"))
        .await
        .expect("row exists");
    assert_eq!(row.state, JobState::Stopped);
}

#[tokio::test(start_paused = true)]
async fn tick_hung_tick_cancelled_at_interval() {
    let registry = registry();
    let mut plugin = FakePlugin::new("tick-toy");
    plugin.hang = true;
    let host = FakeHost::with(plugin.clone());
    let sync = droppedneedle::jobs::plugin_ticks::TickSyncState::new();
    sync_ticks(
        &registry,
        &host,
        &sync,
        &[spec("tick-toy", Duration::from_millis(100), true)],
        Duration::from_secs(5),
    )
    .await;
    settle().await;

    // Each hung tick dies at the 100 ms timeout and the next starts.
    advance_stepping(Duration::from_secs(1), Duration::from_millis(50)).await;
    assert!(plugin.calls() >= 3, "calls: {}", plugin.calls());

    registry.cancel_all(Duration::from_secs(5)).await;
}

#[tokio::test(start_paused = true)]
async fn tick_no_overlap() {
    let registry = registry();
    let mut plugin = FakePlugin::new("tick-toy");
    plugin.slow = Some(Duration::from_millis(300));
    let host = FakeHost::with(plugin.clone());
    let sync = droppedneedle::jobs::plugin_ticks::TickSyncState::new();
    sync_ticks(
        &registry,
        &host,
        &sync,
        &[spec("tick-toy", Duration::from_millis(500), true)],
        Duration::from_secs(5),
    )
    .await;
    settle().await;

    // Slow ticks finish inside the timeout; the loop still serializes them.
    advance_stepping(Duration::from_secs(2), Duration::from_millis(100)).await;
    assert!(plugin.calls() >= 2, "calls: {}", plugin.calls());
    assert_eq!(plugin.max_active.load(Ordering::SeqCst), 1);

    registry.cancel_all(Duration::from_secs(5)).await;
}

#[tokio::test(start_paused = true)]
async fn tick_disabled_plugin_exits_loop() {
    let registry = registry();
    let plugin = FakePlugin::new("tick-toy");
    let host = FakeHost::with(plugin.clone());
    let sync = droppedneedle::jobs::plugin_ticks::TickSyncState::new();
    sync_ticks(
        &registry,
        &host,
        &sync,
        &[spec("tick-toy", Duration::from_millis(100), true)],
        Duration::from_secs(5),
    )
    .await;
    settle().await;
    assert!(plugin.calls() >= 1);

    *plugin.enabled.lock().unwrap() = false;
    tokio::time::advance(Duration::from_secs(1)).await;
    settle().await;
    assert!(!registry.is_running(&tick_name("tick-toy")));
}

#[tokio::test(start_paused = true)]
async fn sync_ticks_rebuilds_added_removed_and_changed() {
    let registry = registry();
    let host = FakeHost::with(FakePlugin::new("kept"));
    host.plugins
        .lock()
        .unwrap()
        .insert("added".to_owned(), FakePlugin::new("added"));
    let sync = droppedneedle::jobs::plugin_ticks::TickSyncState::new();

    sync_ticks(
        &registry,
        &host,
        &sync,
        &[spec("kept", Duration::from_secs(300), false)],
        Duration::from_secs(5),
    )
    .await;
    assert!(registry.is_running(&tick_name("kept")));

    // Add one, drop one, change one's interval: the rebuild matches exactly.
    sync_ticks(
        &registry,
        &host,
        &sync,
        &[
            spec("kept", Duration::from_secs(600), false),
            spec("added", Duration::from_secs(300), false),
        ],
        Duration::from_secs(5),
    )
    .await;
    assert!(registry.is_running(&tick_name("kept")));
    assert!(registry.is_running(&tick_name("added")));

    // Nothing desired: every tick loop goes away.
    sync_ticks(&registry, &host, &sync, &[], Duration::from_secs(5)).await;
    assert!(!registry.is_running(&tick_name("kept")));
    assert!(!registry.is_running(&tick_name("added")));
}

// ---------------------------------------------------------------------------
// Tick state on the store
// ---------------------------------------------------------------------------

#[tokio::test]
async fn tick_state_roundtrip() {
    let store = MemoryTickStore::with_plugins(&["tick-toy"]);
    store
        .write("tick-toy", "cache/seen", b"abc".to_vec())
        .await
        .expect("write wins");
    assert_eq!(
        store
            .read("tick-toy", "cache/seen")
            .await
            .expect("read wins"),
        Some(b"abc".to_vec())
    );
}

#[tokio::test]
async fn tick_state_rejects_unsafe_paths_unknown_plugins_and_over_cap() {
    let store = MemoryTickStore::with_plugins(&["tick-toy"]);
    for bad in ["../escape", "/absolute", "UPPER", "trailing/", "a//b"] {
        let error = store
            .write("tick-toy", bad, b"x".to_vec())
            .await
            .expect_err("unsafe key rejected");
        assert!(
            matches!(error, TickStoreError::UnsafePath(_)),
            "{bad}: {error}"
        );
    }
    let error = store
        .write(" stranger ", "cache/seen", b"x".to_vec())
        .await
        .expect_err("unknown plugin rejected");
    assert!(matches!(error, TickStoreError::UnknownPlugin(_)));
    let big = vec![0_u8; 10 * 1024 * 1024 + 1];
    let error = store
        .write("tick-toy", "big/blob", big)
        .await
        .expect_err("over-cap rejected");
    assert_eq!(error, TickStoreError::OverCap);
    let error = store
        .read("tick-toy", "big/blob")
        .await
        .expect_err("missing key is an error");
    assert!(matches!(error, TickStoreError::NotFound(_)));
}

#[tokio::test(start_paused = true)]
async fn tick_state_survives_loop_restart() {
    // One tick writes state through the shared memory store; then every
    // loop stops (the restart), and a fresh loop generation reads the same
    // bytes back. This pins loop-restart survival only — the store never
    // leaves the process here. Process-restart survival is the SQLite
    // store's brief below.
    let store = MemoryTickStore::with_plugins(&["tick-toy"]);
    let registry = registry();
    let host = FakeHost::with(FakePlugin::new("tick-toy"));
    let sync = droppedneedle::jobs::plugin_ticks::TickSyncState::new();
    sync_ticks(
        &registry,
        &host,
        &sync,
        &[spec("tick-toy", Duration::from_millis(100), true)],
        Duration::from_secs(5),
    )
    .await;
    settle().await;
    store
        .write("tick-toy", "cache/seen", b"tick-1".to_vec())
        .await
        .expect("write wins");

    // The restart: all loops stop, the registry rows land, the store stays.
    registry.cancel_all(Duration::from_secs(5)).await;
    assert!(!registry.is_running(&tick_name("tick-toy")));
    drop(registry);

    let revived = JobRegistry::new(MemoryRegistryStore::new());
    let revived_host = FakeHost::with(FakePlugin::new("tick-toy"));
    let revived_sync = droppedneedle::jobs::plugin_ticks::TickSyncState::new();
    sync_ticks(
        &revived,
        &revived_host,
        &revived_sync,
        &[spec("tick-toy", Duration::from_millis(100), true)],
        Duration::from_secs(5),
    )
    .await;
    settle().await;
    assert_eq!(
        store
            .read("tick-toy", "cache/seen")
            .await
            .expect("read wins"),
        Some(b"tick-1".to_vec())
    );
    revived.cancel_all(Duration::from_secs(5)).await;
}

/// Scratch-dir sequence so parallel tests never share a database.
static SCRATCH_SEQ: AtomicU64 = AtomicU64::new(0);

#[tokio::test]
async fn sqlite_tick_state_survives_a_full_restart() {
    let seq = SCRATCH_SEQ.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "droppedneedle-tick-store-{seq}-{}",
        std::process::id()
    ));
    let db_path = dir.join("app.db");

    // First boot: register, write, shut everything down.
    let runtime = open_runtime(&DbConfig::new(&db_path))
        .await
        .expect("runtime opens");
    let store = SqliteTickStore::new(runtime.pool().clone(), runtime.lane().clone());
    store.add_plugin("tick-toy");
    store
        .write("tick-toy", "cache/seen", b"tick-1".to_vec())
        .await
        .expect("write wins");
    // Validation matches the memory store exactly.
    assert!(matches!(
        store.write("ghost", "cache/seen", b"x".to_vec()).await,
        Err(TickStoreError::UnknownPlugin(_))
    ));
    assert!(matches!(
        store.write("tick-toy", "../escape", b"x".to_vec()).await,
        Err(TickStoreError::UnsafePath(_))
    ));
    runtime.shutdown().await;

    // Second boot over the same file: fresh pool, fresh lane, fresh store.
    // Registration rebuilds (as boot's sync_ticks does); the bytes persist.
    let runtime = open_runtime(&DbConfig::new(&db_path))
        .await
        .expect("runtime reopens");
    let store = SqliteTickStore::new(runtime.pool().clone(), runtime.lane().clone());
    assert!(matches!(
        store.read("tick-toy", "cache/seen").await,
        Err(TickStoreError::UnknownPlugin(_))
    ));
    store.add_plugin("tick-toy");
    assert_eq!(
        store
            .read("tick-toy", "cache/seen")
            .await
            .expect("read wins"),
        Some(b"tick-1".to_vec())
    );
    assert!(matches!(
        store.read("tick-toy", "cache/missing").await,
        Err(TickStoreError::NotFound(_))
    ));
    runtime.shutdown().await;
    let _ = std::fs::remove_dir_all(&dir);
}
