//! `scheduler`: plugin ticks on the loop supervisor, and plugin state on the
//! jobs tick store.
//!
//! Jobs owns the loop mechanics, the supervisor, and the tick store; this
//! module plugs the plugin host into them. [`HostTickAdapter`] implements
//! the jobs [`TickHost`](jobs_tick_host) seam over [`PluginHost`],
//! resolving the plugin fresh on every tick, and [`PluginTickLoops`]
//! rebuilds loops through the jobs [`sync_ticks`] choke point after every
//! install, update, and uninstall. One loop per plugin, no overlap; a tick
//! that runs past its interval is cancelled. The same [`SqliteTickStore`]
//! backs every plugin's `host.state.*` requests, which is what makes plugin
//! state survive restarts and reinstalls.
//!
//! [`jobs_tick_host`]: droppedneedle_kernel::jobs::plugin_ticks::TickHost
//! [`sync_ticks`]: droppedneedle_kernel::jobs::plugin_ticks::sync_ticks

use std::sync::Arc;
use std::time::Duration;

use droppedneedle_kernel::jobs::Supervisor;
use droppedneedle_kernel::jobs::plugin_ticks::{
    SqliteTickStore, TickHost as JobsTickHost, TickPlugin as JobsTickPlugin, TickSpec,
    TickSyncState, clamp_interval_minutes, sync_ticks,
};

use super::host::PluginHost;
use super::manifest::Capability;
use super::protocol::methods;

/// Default cancel grace for tick loops.
pub const TICK_CANCEL_GRACE: Duration = Duration::from_secs(5);

impl PluginHost {
    /// Run one scheduler tick, waiting at most `budget`. Resolves the
    /// plugin fresh every call, so a settings save applies on the next tick.
    pub async fn fire_tick(&self, name: &str, budget: Duration) -> Result<(), String> {
        let plugin = self
            .get(name)
            .filter(|plugin| plugin.serves(Capability::Scheduler))
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

    fn on_tick(&self) -> droppedneedle_kernel::jobs::BoxFuture<'_, Result<(), String>> {
        let host = Arc::clone(&self.host);
        let name = self.name.clone();
        let budget = self.budget;
        Box::pin(async move { host.fire_tick(&name, budget).await })
    }

    fn tick_enabled(&self) -> bool {
        self.host
            .get(&self.name)
            .is_some_and(|plugin| plugin.serves(Capability::Scheduler))
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

/// Tick loops on the supervisor over the tick store. Each sync registers
/// the desired plugins with the store, then rebuilds through the jobs
/// choke point.
pub struct PluginTickLoops {
    supervisor: Supervisor,
    sync: TickSyncState,
    store: SqliteTickStore,
    grace: Duration,
}

impl PluginTickLoops {
    /// Wire loops on the supervisor over one tick store.
    pub fn new(supervisor: Supervisor, store: SqliteTickStore, grace: Duration) -> Self {
        Self {
            supervisor,
            sync: TickSyncState::new(),
            store,
            grace,
        }
    }

    /// Rebuild loops to match the host's enabled scheduler plugins.
    pub async fn sync_host(&self, host: &Arc<PluginHost>) {
        let specs = desired_specs(host);
        for spec in &specs {
            self.store.add_plugin(&spec.name);
        }
        let adapter = HostTickAdapter::new(Arc::clone(host));
        sync_ticks(&self.supervisor, &adapter, &self.sync, &specs, self.grace).await;
    }
}
