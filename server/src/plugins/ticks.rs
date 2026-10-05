//! `scheduler`: plugin ticks on the jobs registry, and plugin state on the
//! jobs tick store.
//!
//! Jobs owns the loop mechanics, the registry, and the tick store; this
//! module plugs the plugin host into them. [`HostTickAdapter`] implements
//! the jobs [`TickHost`](jobs_tick_host) seam over [`PluginHost`],
//! resolving the plugin fresh on every tick, and [`PluginTickLoops`]
//! rebuilds loops through the jobs [`sync_ticks`] choke point after every
//! install, update, and uninstall. One loop per plugin, no overlap; a tick
//! that runs past its interval is cancelled. The same
//! [`TickStore`](jobs_tick_store) backs every plugin's `host.state.*`
//! requests, which is what makes plugin state survive restarts and
//! reinstalls.
//!
//! [`jobs_tick_host`]: crate::jobs::plugin_ticks::TickHost
//! [`jobs_tick_store`]: crate::jobs::plugin_ticks::TickStore
//! [`sync_ticks`]: crate::jobs::plugin_ticks::sync_ticks

use std::sync::Arc;
use std::time::Duration;

use crate::jobs::plugin_ticks::{
    MemoryTickStore, SqliteTickStore, TickHost as JobsTickHost, TickPlugin as JobsTickPlugin,
    TickSpec, TickStore as JobsTickStore, TickStoreError, TickSyncState, clamp_interval_minutes,
    sync_ticks,
};
use crate::jobs::registry::{JobRegistry, RegistryStore};

use super::host::{PluginHost, PluginStateStore};
use super::protocol::methods;
use super::runtime::BoxFuture;

/// Default cancel grace for tick loops.
pub const TICK_CANCEL_GRACE: Duration = Duration::from_secs(5);

/// Registration half of a tick store. The jobs store keys state by plugin
/// and refuses unknown plugins; the install path registers each enabled
/// plugin before its loop starts. Both stores implement this over their
/// own `add_plugin`.
pub trait TickStoreAdmin: Send + Sync {
    /// Register one plugin, creating its empty key map when absent.
    fn ensure_plugin(&self, name: &str);
}

impl TickStoreAdmin for MemoryTickStore {
    fn ensure_plugin(&self, name: &str) {
        self.add_plugin(name);
    }
}

impl TickStoreAdmin for SqliteTickStore {
    fn ensure_plugin(&self, name: &str) {
        self.add_plugin(name);
    }
}

/// Tick state behind one concrete type: memory in test states, SQLite in
/// production. The enum keeps [`PluginTickLoops`] (and through it
/// `PluginsSetup`) non-generic so `AppState` holds one field type.
#[derive(Clone, Debug)]
pub enum TickStoreKind {
    /// In-memory state for states without a database.
    Memory(MemoryTickStore),
    /// Durable state over `plugin_tick_state`.
    Sqlite(SqliteTickStore),
}

impl JobsTickStore for TickStoreKind {
    fn read(
        &self,
        plugin: &str,
        key: &str,
    ) -> crate::jobs::registry::BoxFuture<'_, Result<Option<Vec<u8>>, TickStoreError>> {
        match self {
            Self::Memory(store) => store.read(plugin, key),
            Self::Sqlite(store) => store.read(plugin, key),
        }
    }

    fn write(
        &self,
        plugin: &str,
        key: &str,
        bytes: Vec<u8>,
    ) -> crate::jobs::registry::BoxFuture<'_, Result<(), TickStoreError>> {
        match self {
            Self::Memory(store) => store.write(plugin, key, bytes),
            Self::Sqlite(store) => store.write(plugin, key, bytes),
        }
    }
}

impl TickStoreAdmin for TickStoreKind {
    fn ensure_plugin(&self, name: &str) {
        match self {
            Self::Memory(store) => store.ensure_plugin(name),
            Self::Sqlite(store) => store.ensure_plugin(name),
        }
    }
}

impl PluginStateStore for TickStoreKind {
    fn read<'a>(
        &'a self,
        plugin: &'a str,
        key: &'a str,
    ) -> BoxFuture<'a, Result<Option<Vec<u8>>, String>> {
        Box::pin(async move {
            match JobsTickStore::read(self, plugin, key).await {
                Ok(found) => Ok(found),
                Err(TickStoreError::NotFound(_)) | Err(TickStoreError::UnknownPlugin(_)) => {
                    Ok(None)
                }
                Err(error) => Err(error.to_string()),
            }
        })
    }

    fn write<'a>(
        &'a self,
        plugin: &'a str,
        key: &'a str,
        value: Vec<u8>,
    ) -> BoxFuture<'a, Result<(), String>> {
        Box::pin(async move {
            self.ensure_plugin(plugin);
            JobsTickStore::write(self, plugin, key, value)
                .await
                .map_err(|error| error.to_string())
        })
    }
}

impl PluginHost {
    /// Run one scheduler tick, waiting at most `budget`. Resolves the
    /// plugin fresh every call, so a settings save applies on the next tick.
    pub async fn fire_tick(&self, name: &str, budget: Duration) -> Result<(), String> {
        let plugin = self
            .get(name)
            .filter(|plugin| plugin.serves("scheduler"))
            .ok_or_else(|| "unknown or disabled scheduler".to_owned())?;
        super::capabilities::call(&plugin, methods::TICK, serde_json::Value::Null, budget)
            .await
            .map(|_| ())
            .map_err(|error| error.to_string())
    }
}

/// One scheduler plugin behind the jobs [`TickPlugin`] seam. The host
/// resolves the plugin fresh on every call, never captured.
pub struct PluginTick {
    host: Arc<PluginHost>,
    name: String,
    budget: Duration,
}

impl JobsTickPlugin for PluginTick {
    fn name(&self) -> &str {
        &self.name
    }

    fn on_tick(&self) -> crate::jobs::registry::BoxFuture<'_, Result<(), String>> {
        let host = Arc::clone(&self.host);
        let name = self.name.clone();
        let budget = self.budget;
        Box::pin(async move { host.fire_tick(&name, budget).await })
    }

    fn tick_enabled(&self) -> bool {
        self.host
            .get(&self.name)
            .is_some_and(|plugin| plugin.serves("scheduler"))
    }
}

/// The plugin host behind the jobs [`TickHost`] seam.
#[derive(Clone)]
pub struct HostTickAdapter {
    host: Arc<PluginHost>,
}

impl HostTickAdapter {
    /// Adapt one host.
    pub fn new(host: Arc<PluginHost>) -> Self {
        Self { host }
    }
}

impl JobsTickHost for HostTickAdapter {
    type Plugin = PluginTick;

    fn get(&self, name: &str) -> Option<Self::Plugin> {
        let plugin = self.host.get(name)?;
        let minutes = plugin
            .manifest
            .schedule
            .as_ref()
            .map(|schedule| schedule.interval_minutes)
            .unwrap_or(60);
        Some(PluginTick {
            host: Arc::clone(&self.host),
            name: name.to_owned(),
            budget: clamp_interval_minutes(Some(minutes)),
        })
    }
}

/// Desired tick loops, resolved from the host's enabled scheduler plugins
/// with the jobs interval clamp.
pub fn desired_specs(host: &PluginHost) -> Vec<TickSpec> {
    let mut specs: Vec<TickSpec> = host
        .desired_ticks()
        .into_iter()
        .map(|tick| TickSpec {
            name: tick.name,
            interval: clamp_interval_minutes(Some(tick.interval_minutes)),
            run_on_load: tick.run_on_load,
        })
        .collect();
    specs.sort_by(|left, right| left.name.cmp(&right.name));
    specs
}

/// Tick-loop rebuilds behind an object-safe seam, so handlers never name
/// the concrete registry or tick stores.
pub trait TickLoopSync: Send + Sync {
    /// Rebuild loops to match the host's enabled scheduler plugins.
    fn sync_host<'a>(&'a self, host: &'a Arc<PluginHost>) -> BoxFuture<'a, ()>;
}

/// Tick loops over one jobs registry plus one jobs tick store. Each sync
/// registers the desired plugins with the store, then rebuilds through
/// the jobs choke point. Tests run this over the memory pair, production
/// over the durable pair, both behind the same seams.
pub struct PluginTickLoops<S, T> {
    registry: JobRegistry<S>,
    sync: TickSyncState,
    store: T,
    grace: Duration,
}

impl<S, T> PluginTickLoops<S, T> {
    /// Wire loops over one registry and one tick store.
    pub fn new(registry: JobRegistry<S>, store: T, grace: Duration) -> Self {
        Self {
            registry,
            sync: TickSyncState::new(),
            store,
            grace,
        }
    }
}

impl<S: RegistryStore, T: JobsTickStore + TickStoreAdmin> TickLoopSync for PluginTickLoops<S, T> {
    fn sync_host<'a>(&'a self, host: &'a Arc<PluginHost>) -> BoxFuture<'a, ()> {
        Box::pin(async move {
            let specs = desired_specs(host);
            for spec in &specs {
                self.store.ensure_plugin(&spec.name);
            }
            let adapter = HostTickAdapter::new(Arc::clone(host));
            sync_ticks(&self.registry, &adapter, &self.sync, &specs, self.grace).await;
        })
    }
}

/// No-op sync for handlers under test that never assert loops.
#[cfg(any(test, feature = "test-support"))]
#[derive(Debug, Default)]
pub struct NoopTickSync;

#[cfg(any(test, feature = "test-support"))]
impl TickLoopSync for NoopTickSync {
    fn sync_host<'a>(&'a self, _host: &'a Arc<PluginHost>) -> BoxFuture<'a, ()> {
        Box::pin(async {})
    }
}
