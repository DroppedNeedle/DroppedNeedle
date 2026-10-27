//! Plugin tick loops: a hung tick is cancelled at its interval and the loop
//! carries on, tick state refuses unsafe paths, unknown plugins and
//! oversize writes, and SQLite tick state survives a restart.

use crate::common::{ScratchDir, scratch_supervisor};
use std::collections::HashMap;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicU64, Ordering},
};
use std::time::Duration;

use droppedneedle_kernel::db::{DbConfig, open_runtime};
use droppedneedle_kernel::jobs::BoxFuture;
use droppedneedle_kernel::jobs::plugin_ticks::{
    SqliteTickStore, TickHost, TickPlugin, TickSpec, TickStoreError, TickSyncState, sync_ticks,
};

/// Scripted plugin whose every tick hangs.
#[derive(Clone)]
struct HangingPlugin {
    name: String,
    calls: Arc<AtomicU64>,
}

impl TickPlugin for HangingPlugin {
    fn name(&self) -> &str {
        &self.name
    }

    fn on_tick(&self) -> BoxFuture<'_, Result<(), String>> {
        let calls = Arc::clone(&self.calls);
        Box::pin(async move {
            calls.fetch_add(1, Ordering::SeqCst);
            std::future::pending::<Result<(), String>>().await
        })
    }

    fn tick_enabled(&self) -> bool {
        true
    }
}

/// Scripted host: resolves plugins fresh from its map, like the real host.
#[derive(Clone, Default)]
struct FakeHost {
    plugins: Arc<Mutex<HashMap<String, HangingPlugin>>>,
}

impl TickHost for FakeHost {
    type Plugin = HangingPlugin;

    fn get(&self, name: &str) -> Option<Self::Plugin> {
        self.plugins.lock().unwrap().get(name).cloned()
    }
}

#[tokio::test]
async fn tick_hung_tick_cancelled_at_interval() {
    let (supervisor, runtime, _dir) = scratch_supervisor("tick-hung").await;
    let plugin = HangingPlugin {
        name: "tick-toy".to_owned(),
        calls: Arc::new(AtomicU64::new(0)),
    };
    let host = FakeHost::default();
    host.plugins
        .lock()
        .unwrap()
        .insert(plugin.name.clone(), plugin.clone());
    tokio::time::pause();
    sync_ticks(
        &supervisor,
        &host,
        &TickSyncState::new(),
        &[TickSpec {
            name: "tick-toy".to_owned(),
            interval: Duration::from_millis(100),
            run_on_load: true,
        }],
        Duration::from_secs(5),
    )
    .await;
    assert!(supervisor.is_running("plugin-tick:tick-toy"));

    // Each hung tick dies at the 100 ms timeout and, after its backoff, the
    // next starts.
    tokio::time::sleep(Duration::from_secs(10)).await;
    let calls = plugin.calls.load(Ordering::SeqCst);
    assert!(calls >= 3, "calls: {calls}");

    tokio::time::resume();
    supervisor.shutdown(Duration::from_secs(5)).await;
    runtime.shutdown().await;
}

// ---------------------------------------------------------------------------
// Tick state on the store
// ---------------------------------------------------------------------------

#[tokio::test]
async fn tick_state_rejects_unsafe_paths_unknown_plugins_and_over_cap() {
    let dir = ScratchDir::new("tick-refusals");
    let runtime = crate::common::scratch_runtime(&dir.join("app.db")).await;
    let store = SqliteTickStore::new(
        runtime.db().clone(),
        Arc::new(droppedneedle_kernel::clock::SystemClock),
    );
    store.add_plugin("tick-toy");
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
    runtime.shutdown().await;
}

#[tokio::test]
async fn sqlite_tick_state_survives_a_full_restart() {
    let dir = ScratchDir::new("tick-store");
    let db_path = dir.join("app.db");

    // First boot: register, write, shut everything down.
    let runtime = crate::common::scratch_runtime(&db_path).await;
    let store = SqliteTickStore::new(
        runtime.db().clone(),
        Arc::new(droppedneedle_kernel::clock::SystemClock),
    );
    store.add_plugin("tick-toy");
    store
        .write("tick-toy", "cache/seen", b"tick-1".to_vec())
        .await
        .expect("write wins");
    // Unknown plugins and unsafe keys are refused before the disk.
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
    let store = SqliteTickStore::new(
        runtime.db().clone(),
        Arc::new(droppedneedle_kernel::clock::SystemClock),
    );
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
