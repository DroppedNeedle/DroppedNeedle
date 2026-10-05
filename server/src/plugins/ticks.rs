//! Plugin ticks on the jobs store design (D11).
//!
//! The jobs slice owns the loop mechanics, the registry, and the tick
//! store; this module is the adapter that plugs the plugin host into it.
//! [`HostTickAdapter`] implements the jobs [`TickHost`](jobs_tick_host)
//! seam over [`PluginHost`], resolving modules fresh every sweep, and
//! [`PluginTickLoops`] rebuilds loops through the jobs [`sync_ticks`]
//! choke point after every install, update, and uninstall. Tick state
//! itself lives on the jobs [`TickStore`](jobs_tick_store): modules reach
//! it through the plugin-scoped [`TickStateAccess`], which is what makes
//! their state survive restarts and reinstalls.
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

use super::host::PluginHost;
use super::runtime::{BoxFuture, TickContext, TickStateAccess};

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

/// [`TickStateAccess`] over one jobs tick store, scoped to one plugin.
/// Missing keys and unknown plugins read as `None`; unsafe keys and
/// over-cap writes fail loudly.
pub struct JobsStateAccess<T> {
    store: T,
    plugin: String,
}

impl<T> JobsStateAccess<T> {
    /// Scope one store to one plugin.
    pub fn new(store: T, plugin: &str) -> Self {
        Self {
            store,
            plugin: plugin.to_owned(),
        }
    }
}

impl<T: JobsTickStore> TickStateAccess for JobsStateAccess<T> {
    fn read<'a>(&'a self, key: &'a str) -> BoxFuture<'a, Option<Vec<u8>>> {
        Box::pin(async move {
            match self.store.read(&self.plugin, key).await {
                Ok(found) => found,
                Err(TickStoreError::NotFound(_)) | Err(TickStoreError::UnknownPlugin(_)) => None,
                Err(error) => {
                    tracing::warn!(
                        plugin = %self.plugin,
                        key = %key,
                        %error,
                        "tick state read failed"
                    );
                    None
                }
            }
        })
    }

    fn write<'a>(&'a self, key: &'a str, bytes: Vec<u8>) -> BoxFuture<'a, Result<(), String>> {
        Box::pin(async move {
            self.store
                .write(&self.plugin, key, bytes)
                .await
                .map_err(|error| error.to_string())
        })
    }
}

/// One scheduler-capable module behind the jobs [`TickPlugin`] seam. The
/// host and name resolve the module fresh on every call, never captured,
/// so a settings-save rebuild applies on the next sweep.
pub struct ModuleTick<T> {
    host: Arc<PluginHost>,
    name: String,
    store: T,
}

impl<T> ModuleTick<T> {}

impl<T: JobsTickStore> JobsTickPlugin for ModuleTick<T> {
    fn name(&self) -> &str {
        &self.name
    }

    fn on_tick(&self) -> crate::jobs::registry::BoxFuture<'_, Result<(), String>> {
        let host = Arc::clone(&self.host);
        let name = self.name.clone();
        let access = JobsStateAccess::new(self.store.clone(), &self.name);
        Box::pin(async move {
            let ctx = TickContext {
                plugin_name: &name,
                state: &access,
            };
            host.fire_module_tick(&name, &ctx).await
        })
    }

    fn tick_enabled(&self) -> bool {
        self.host
            .get(&self.name)
            .map(|plugin| {
                plugin.enabled
                    && plugin.module.is_some()
                    && plugin
                        .active_capabilities
                        .iter()
                        .any(|capability| capability == "scheduler")
            })
            .unwrap_or(false)
    }
}

/// The plugin host behind the jobs [`TickHost`] seam.
#[derive(Clone)]
pub struct HostTickAdapter<T> {
    host: Arc<PluginHost>,
    store: T,
}

impl<T> HostTickAdapter<T> {
    /// Adapt one host plus its tick store.
    pub fn new(host: Arc<PluginHost>, store: T) -> Self {
        Self { host, store }
    }
}

impl<T: JobsTickStore> JobsTickHost for HostTickAdapter<T> {
    type Plugin = ModuleTick<T>;

    fn get(&self, name: &str) -> Option<Self::Plugin> {
        self.host.get(name).map(|_| ModuleTick {
            host: Arc::clone(&self.host),
            name: name.to_owned(),
            store: self.store.clone(),
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
            let adapter = HostTickAdapter::new(Arc::clone(host), self.store.clone());
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adapter_reports_enabled_only_while_the_plugin_wants_its_loop() {
        let dir = std::env::temp_dir().join(format!(
            "dn-tick-adapter-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("plugins").join("toy")).unwrap();
        std::fs::write(
            dir.join("plugins").join("toy").join("plugin.toml"),
            "[plugin]\nname = \"toy\"\napi_version = 1\nentrypoint = \"plugin:Toy\"\ncapabilities = [\"scheduler\"]\n[schedule]\ninterval_minutes = 60\n",
        )
        .unwrap();
        let crypto = crate::runtime_config::crypto::Crypto::from_key_bytes(&[7u8; 32]).unwrap();
        let config = Arc::new(
            crate::runtime_config::ConfigStore::open(&dir.join("config.json"), crypto).unwrap(),
        );
        let loader = Arc::new(super::super::fakes::FakeLoader::new());
        loader.insert(
            "toy",
            Arc::new(super::super::fakes::FakeModule::providing(&["scheduler"])),
        );
        let host = Arc::new(PluginHost::new(dir.join("plugins"), config, loader));
        host.load_all();
        let adapter = HostTickAdapter::new(Arc::clone(&host), MemoryTickStore::new());
        // Disabled: the record exists but the loop is unwanted.
        assert!(!adapter.get("toy").unwrap().tick_enabled());
        host.update_settings("toy", true, std::collections::HashMap::new())
            .unwrap();
        assert!(adapter.get("toy").unwrap().tick_enabled());
        assert!(adapter.get("ghost").is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
