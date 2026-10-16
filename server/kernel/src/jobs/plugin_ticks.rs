//! Plugin scheduler ticks, with durable state on the store.
//!
//! v2 kept tick state in files under each plugin's directory: unregistered
//! durability that vanished with an uninstall and bypassed every backup.
//! v3 moves that state onto [`TickStore`], keyed by plugin name, so it
//! survives restarts and reinstalls and travels with the database. The loop
//! contract itself is unchanged from `host.py`: one task per enabled
//! scheduler plugin under [`TICK_PREFIX`], interval clamped to 5-1440 minutes
//! (default 60), a hung tick cancelled at the interval, no overlap, no
//! backfill, a failed tick logging and continuing, and a disabled or removed
//! plugin exiting its loop. [`sync_ticks`] stays the sole rebuild choke
//! point, called after plugin loads and on settings saves.
//!
//! State keys keep the v2 path rules (`^[a-z0-9][a-z0-9/_-]{0,63}$`, no
//! trailing slash, no empty or dot parts) minus the filesystem they used to
//! guard: there are no symlinks to escape through a key-value store, so that
//! check retires while the shape and the 10 MiB cap stay.

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use super::registry::{BoxFuture, JobCtx, JobExit, JobKind, JobRegistry, RegistryStore};
use super::schedule::sleep_or_stop;
use crate::db::{WriteLane, writer::Lane};

/// Registered-name prefix for tick loops: `plugin-tick:{name}`.
pub const TICK_PREFIX: &str = "plugin-tick:";

/// Smallest tick interval the clamp honors, in minutes.
pub const MIN_INTERVAL_MINUTES: u64 = 5;
/// Largest tick interval the clamp honors, in minutes.
pub const MAX_INTERVAL_MINUTES: u64 = 1440;
/// Interval when the manifest names none or names garbage, in minutes.
pub const DEFAULT_INTERVAL_MINUTES: u64 = 60;

/// Per-key state cap, carried over from the v2 file cap.
pub const STATE_MAX_BYTES: usize = 10 * 1024 * 1024;

/// Most state keys one plugin may hold.
pub const STATE_MAX_KEYS: usize = 1000;

/// Most state bytes one plugin may hold across all its keys (64 MiB).
pub const STATE_MAX_TOTAL_BYTES: usize = 64 * 1024 * 1024;

/// Longest state key the validator accepts.
const STATE_KEY_MAX_LEN: usize = 64;

/// Clamp a manifest `interval_minutes` into range. Missing (and, upstream of
/// here, unparseable) values fall back to the 60-minute default; anything
/// else clamps into 5-1440, exactly the v2 rule.
pub fn clamp_interval_minutes(raw: Option<i64>) -> Duration {
    let minutes = raw
        .unwrap_or(DEFAULT_INTERVAL_MINUTES as i64)
        .clamp(MIN_INTERVAL_MINUTES as i64, MAX_INTERVAL_MINUTES as i64) as u64;
    Duration::from_secs(minutes * 60)
}

/// One plugin's desired schedule, resolved from its manifest and settings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TickSpec {
    /// Plugin name; the loop registers as `plugin-tick:{name}`.
    pub name: String,
    /// Clamped tick cadence.
    pub interval: Duration,
    /// Tick once at startup instead of waiting out the interval.
    pub run_on_load: bool,
}

/// One scheduler-capable plugin instance.
pub trait TickPlugin: Send + Sync + 'static {
    /// Plugin name, matching the [`TickSpec`].
    fn name(&self) -> &str;
    /// Run one tick. `Err` carries the failure for the log; the loop carries on.
    fn on_tick(&self) -> BoxFuture<'_, Result<(), String>>;
    /// True while the plugin still wants its loop: enabled, scheduler
    /// capability active, tick handler present. Re-checked every sweep.
    fn tick_enabled(&self) -> bool;
}

/// The plugin host behind a seam. The loop resolves its plugin fresh every
/// sweep, never captured, so a settings-save rebuild applies on the next tick.
pub trait TickHost: Clone + Send + Sync + 'static {
    /// Instance type the host hands out.
    type Plugin: TickPlugin;
    /// Fresh instance for the name, or nothing when removed.
    fn get(&self, name: &str) -> Option<Self::Plugin>;
}

/// Durable tick state. Keys are validated before any read or write; values
/// are opaque bytes up to [`STATE_MAX_BYTES`].
pub trait TickStore: Clone + Send + Sync + 'static {
    /// Read one key. Missing keys and unknown plugins are errors, matching
    /// the v2 read (callers treat `NotFound` as empty state).
    fn read(
        &self,
        plugin: &str,
        key: &str,
    ) -> BoxFuture<'_, Result<Option<Vec<u8>>, TickStoreError>>;
    /// Store one key, creating or overwriting.
    fn write(
        &self,
        plugin: &str,
        key: &str,
        bytes: Vec<u8>,
    ) -> BoxFuture<'_, Result<(), TickStoreError>>;
}

/// Every way a tick-state call can fail.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TickStoreError {
    /// No plugin by that name is known to the store.
    UnknownPlugin(String),
    /// The key breaks the shape rules.
    UnsafePath(String),
    /// The value tops the 10 MiB cap.
    OverCap,
    /// The plugin is known but the key was never written.
    NotFound(String),
    /// The database call failed. The message is log-only.
    Unavailable(String),
}

impl fmt::Display for TickStoreError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TickStoreError::UnknownPlugin(name) => write!(formatter, "unknown plugin {name:?}"),
            TickStoreError::UnsafePath(key) => {
                write!(formatter, "state path {key:?} is not a safe relative path")
            }
            TickStoreError::OverCap => write!(formatter, "state file exceeds the 10 MiB cap"),
            TickStoreError::NotFound(key) => write!(formatter, "state file {key:?} not found"),
            TickStoreError::Unavailable(cause) => {
                write!(formatter, "tick state store is unavailable: {cause}")
            }
        }
    }
}

impl std::error::Error for TickStoreError {}

/// Validate a state key: `^[a-z0-9][a-z0-9/_-]{0,63}$`, no trailing slash,
/// no empty or dot parts. Hand-rolled to match the v2 regex exactly.
pub fn validate_state_key(key: &str) -> Result<(), TickStoreError> {
    if key.is_empty() || key.len() > STATE_KEY_MAX_LEN {
        return Err(TickStoreError::UnsafePath(key.to_owned()));
    }
    if key.ends_with('/') {
        return Err(TickStoreError::UnsafePath(key.to_owned()));
    }
    let mut chars = key.chars();
    let Some(first) = chars.next() else {
        return Err(TickStoreError::UnsafePath(key.to_owned()));
    };
    if !(first.is_ascii_lowercase() || first.is_ascii_digit()) {
        return Err(TickStoreError::UnsafePath(key.to_owned()));
    }
    if !key.chars().all(|cell| {
        cell.is_ascii_lowercase() || cell.is_ascii_digit() || matches!(cell, '/' | '_' | '-')
    }) {
        return Err(TickStoreError::UnsafePath(key.to_owned()));
    }
    if key
        .split('/')
        .any(|part| part.is_empty() || part == "." || part == "..")
    {
        return Err(TickStoreError::UnsafePath(key.to_owned()));
    }
    Ok(())
}

/// In-memory tick state: plugin names to their key maps. State outlives any
/// one runner because the store is shared, not owned, which is what the
/// durability test relies on (drop the loops, keep the store, read it back).
/// Plugin names to their state keys to raw bytes.
type TickStateMap = HashMap<String, HashMap<String, Vec<u8>>>;

#[derive(Debug, Clone, Default)]
pub struct MemoryTickStore {
    plugins: Arc<Mutex<TickStateMap>>,
}

impl MemoryTickStore {
    /// Empty store knowing no plugins yet.
    #[cfg(any(test, feature = "test-support"))]
    pub fn new() -> Self {
        Self::default()
    }

    /// Empty store with the named plugins registered.
    #[cfg(any(test, feature = "test-support"))]
    pub fn with_plugins(names: &[&str]) -> Self {
        let store = Self::new();
        {
            let mut guard = store
                .plugins
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            for name in names {
                guard.entry((*name).to_owned()).or_default();
            }
        }
        store
    }

    /// Register one more plugin (install path).
    pub fn add_plugin(&self, name: &str) {
        self.plugins
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .entry(name.to_owned())
            .or_default();
    }
}

impl TickStore for MemoryTickStore {
    fn read(
        &self,
        plugin: &str,
        key: &str,
    ) -> BoxFuture<'_, Result<Option<Vec<u8>>, TickStoreError>> {
        let plugins = Arc::clone(&self.plugins);
        let plugin = plugin.to_owned();
        let key = key.to_owned();
        Box::pin(async move {
            validate_state_key(&key)?;
            let guard = plugins
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let Some(keys) = guard.get(&plugin) else {
                return Err(TickStoreError::UnknownPlugin(plugin));
            };
            match keys.get(&key) {
                Some(bytes) => {
                    if bytes.len() > STATE_MAX_BYTES {
                        return Err(TickStoreError::OverCap);
                    }
                    Ok(Some(bytes.clone()))
                }
                None => Err(TickStoreError::NotFound(key)),
            }
        })
    }

    fn write(
        &self,
        plugin: &str,
        key: &str,
        bytes: Vec<u8>,
    ) -> BoxFuture<'_, Result<(), TickStoreError>> {
        let plugins = Arc::clone(&self.plugins);
        let plugin = plugin.to_owned();
        let key = key.to_owned();
        Box::pin(async move {
            if bytes.len() > STATE_MAX_BYTES {
                tracing::warn!(
                    plugin = %plugin,
                    path = %key,
                    reason = "over-cap",
                    "plugin tick state write rejected"
                );
                return Err(TickStoreError::OverCap);
            }
            validate_state_key(&key)?;
            let mut guard = plugins
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let Some(keys) = guard.get_mut(&plugin) else {
                return Err(TickStoreError::UnknownPlugin(plugin));
            };
            let (count, total) = keys
                .iter()
                .filter(|(name, _)| **name != key)
                .fold((0usize, 0usize), |(count, total), (_, value)| {
                    (count + 1, total + value.len())
                });
            if count + 1 > STATE_MAX_KEYS || total + bytes.len() > STATE_MAX_TOTAL_BYTES {
                tracing::warn!(plugin = %plugin, "plugin state write rejected: plugin over its total cap");
                return Err(TickStoreError::OverCap);
            }
            keys.insert(key, bytes);
            Ok(())
        })
    }
}

/// SQLite-backed tick state over `plugin_tick_state`: the production
/// [`TickStore`]. Reads travel the reader pool, writes travel the writer
/// lane, so state survives process restarts with the database.
///
/// Plugin registration stays in memory: boot re-registers every desired
/// plugin through `sync_ticks` before any loop reads, so only the key bytes
/// need the disk. Unknown plugins still error like the memory store, and
/// uninstalls leave rows behind, so a reinstall reads its old state back.
#[derive(Clone, Debug)]
pub struct SqliteTickStore {
    pool: sqlx::SqlitePool,
    lane: WriteLane,
    plugins: Arc<Mutex<HashSet<String>>>,
}

impl SqliteTickStore {
    /// Bind the store over a migrated pool plus the writer lane.
    pub fn new(pool: sqlx::SqlitePool, lane: WriteLane) -> Self {
        Self {
            pool,
            lane,
            plugins: Arc::new(Mutex::new(HashSet::new())),
        }
    }

    /// Register one more plugin (install path).
    pub fn add_plugin(&self, name: &str) {
        self.plugins
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(name.to_owned());
    }

    fn is_known(&self, plugin: &str) -> bool {
        self.plugins
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .contains(plugin)
    }
}

impl TickStore for SqliteTickStore {
    fn read(
        &self,
        plugin: &str,
        key: &str,
    ) -> BoxFuture<'_, Result<Option<Vec<u8>>, TickStoreError>> {
        let store = self.clone();
        let plugin = plugin.to_owned();
        let key = key.to_owned();
        Box::pin(async move {
            validate_state_key(&key)?;
            if !store.is_known(&plugin) {
                return Err(TickStoreError::UnknownPlugin(plugin));
            }
            let row: Option<Vec<u8>> = sqlx::query_scalar(
                "SELECT value FROM plugin_tick_state WHERE plugin = ?1 AND key = ?2",
            )
            .bind(&plugin)
            .bind(&key)
            .fetch_optional(&store.pool)
            .await
            .map_err(|error| {
                tracing::warn!(%plugin, key = %key, %error, "tick state read failed");
                TickStoreError::Unavailable(error.to_string())
            })?;
            match row {
                Some(bytes) => {
                    if bytes.len() > STATE_MAX_BYTES {
                        return Err(TickStoreError::OverCap);
                    }
                    Ok(Some(bytes))
                }
                None => Err(TickStoreError::NotFound(key)),
            }
        })
    }

    fn write(
        &self,
        plugin: &str,
        key: &str,
        bytes: Vec<u8>,
    ) -> BoxFuture<'_, Result<(), TickStoreError>> {
        let store = self.clone();
        let plugin = plugin.to_owned();
        let key = key.to_owned();
        Box::pin(async move {
            if bytes.len() > STATE_MAX_BYTES {
                tracing::warn!(
                    plugin = %plugin,
                    path = %key,
                    reason = "over-cap",
                    "plugin tick state write rejected"
                );
                return Err(TickStoreError::OverCap);
            }
            validate_state_key(&key)?;
            if !store.is_known(&plugin) {
                return Err(TickStoreError::UnknownPlugin(plugin));
            }
            let refused_plugin = plugin.clone();
            let stored = store
                .lane
                .write(Lane::Background, "tick-state-write", move |tx| {
                    // Totals for the plugin's other keys, so a plugin cannot
                    // grow without bound through many small keys.
                    let (count, total): (i64, i64) = tx.query_row(
                        "SELECT COUNT(*), COALESCE(SUM(length(value)), 0)
                         FROM plugin_tick_state WHERE plugin = ?1 AND key <> ?2",
                        rusqlite::params![plugin, key],
                        |row| Ok((row.get(0)?, row.get(1)?)),
                    )?;
                    if count.max(0) as usize + 1 > STATE_MAX_KEYS
                        || total.max(0) as usize + bytes.len() > STATE_MAX_TOTAL_BYTES
                    {
                        return Ok(false);
                    }
                    tx.execute(
                        "INSERT INTO plugin_tick_state (plugin, key, value, updated_at)
                     VALUES (?1, ?2, ?3, ?4)
                     ON CONFLICT (plugin, key) DO UPDATE SET
                         value = excluded.value,
                         updated_at = excluded.updated_at",
                        rusqlite::params![plugin, key, bytes, now_unix()],
                    )?;
                    Ok(true)
                })
                .await
                .map_err(|error| {
                    tracing::warn!(%error, "tick state write failed");
                    TickStoreError::Unavailable(error.to_string())
                })?;
            if !stored {
                tracing::warn!(plugin = %refused_plugin, "plugin state write rejected: plugin over its total cap");
                return Err(TickStoreError::OverCap);
            }
            Ok(())
        })
    }
}

/// Wall-clock now as unix seconds for the `updated_at` stamp.
fn now_unix() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|span| span.as_secs_f64())
        .unwrap_or(0.0)
}

/// Live tick intervals, the half `sync_ticks` compares against. Lives next to
/// the host (the registry tracks liveness, not cadence).
#[derive(Debug, Clone, Default)]
pub struct TickSyncState {
    intervals: Arc<Mutex<HashMap<String, Duration>>>,
}

impl TickSyncState {
    /// No live loops recorded.
    pub fn new() -> Self {
        Self::default()
    }
}

/// Rebuild loops to match the desired set: cancel removed names and changed
/// intervals, start added ones. The sole rebuild choke point.
///
/// Cancel-then-spawn races a replacement against the winding-down incumbent:
/// `cancel` drops the registry entry at once, so the new loop can spawn
/// while the old task still runs out its grace. Ticks are idempotent (one
/// state write per sweep, keyed, last-writer-wins), so a short overlap is
/// harmless and no fencing is needed.
pub async fn sync_ticks<S, H>(
    registry: &JobRegistry<S>,
    host: &H,
    sync: &TickSyncState,
    desired: &[TickSpec],
    grace: Duration,
) where
    S: RegistryStore,
    H: TickHost,
{
    let wanted: HashMap<&str, &TickSpec> = desired
        .iter()
        .map(|spec| (spec.name.as_str(), spec))
        .collect();
    let live: Vec<String> = registry
        .running_names()
        .into_iter()
        .filter_map(|name| name.strip_prefix(TICK_PREFIX).map(str::to_owned))
        .collect();
    for name in &live {
        let stale = match wanted.get(name.as_str()) {
            None => true,
            Some(spec) => sync
                .intervals
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .get(name.as_str())
                .is_none_or(|interval| *interval != spec.interval),
        };
        if stale {
            registry
                .cancel(&format!("{TICK_PREFIX}{name}"), grace)
                .await;
            sync.intervals
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .remove(name.as_str());
        }
    }
    let mut specs: Vec<&TickSpec> = wanted.values().copied().collect();
    specs.sort_by(|left, right| left.name.cmp(&right.name));
    for spec in specs {
        if registry.is_running(&format!("{TICK_PREFIX}{}", spec.name)) {
            continue;
        }
        if spawn_tick(registry, host.clone(), spec.clone())
            .await
            .is_ok()
        {
            sync.intervals
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .insert(spec.name.clone(), spec.interval);
        }
    }
}

/// Spawn one plugin's loop. A live loop owns its name; spawning over it keeps
/// the incumbent and reports the collision.
pub async fn spawn_tick<S, H>(
    registry: &JobRegistry<S>,
    host: H,
    spec: TickSpec,
) -> Result<(), super::registry::AlreadyRunning>
where
    S: RegistryStore,
    H: TickHost,
{
    let name = format!("{}{}", TICK_PREFIX, spec.name);
    registry
        .spawn(&name, JobKind::Ephemeral, None, move |ctx| async move {
            run(ctx, &host, &spec).await
        })
        .await
}

/// One plugin's schedule: no overlap (an overrun delays itself plus one full
/// interval), a hung tick cancelled at the interval, an exception logged and
/// continued, a missed tick skipped. The plugin resolves fresh every sweep;
/// a disabled or removed plugin exits its loop.
pub async fn run<S, H>(ctx: JobCtx<S>, host: &H, spec: &TickSpec) -> JobExit
where
    S: RegistryStore,
    H: TickHost,
{
    if !spec.run_on_load && sleep_or_stop(spec.interval, ctx.stop()).await {
        return JobExit::Stopped;
    }
    loop {
        let Some(plugin) = host.get(&spec.name) else {
            return JobExit::Stopped;
        };
        if !plugin.tick_enabled() {
            return JobExit::Stopped;
        }
        tokio::select! {
            biased;
            _ = ctx.stop().notified() => return JobExit::Stopped,
            tick = tokio::time::timeout(spec.interval, plugin.on_tick()) => {
                match tick {
                    Ok(Ok(())) => {}
                    Ok(Err(cause)) => {
                        tracing::warn!(
                            plugin = %spec.name,
                            %cause,
                            "plugin tick failed; the loop carries on"
                        );
                    }
                    Err(_) => {
                        tracing::warn!(
                            plugin = %spec.name,
                            interval_s = spec.interval.as_secs(),
                            "plugin tick timed out and was cancelled"
                        );
                    }
                }
            }
        }
        ctx.heartbeat().await;
        if sleep_or_stop(spec.interval, ctx.stop()).await {
            return JobExit::Stopped;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clamp_defaults_garbage_and_clamps_edges() {
        let minute = 60;
        assert_eq!(
            clamp_interval_minutes(None),
            Duration::from_secs(DEFAULT_INTERVAL_MINUTES * minute)
        );
        assert_eq!(
            clamp_interval_minutes(Some(0)),
            Duration::from_secs(MIN_INTERVAL_MINUTES * minute)
        );
        assert_eq!(
            clamp_interval_minutes(Some(-30)),
            Duration::from_secs(MIN_INTERVAL_MINUTES * minute)
        );
        assert_eq!(
            clamp_interval_minutes(Some(1)),
            Duration::from_secs(MIN_INTERVAL_MINUTES * minute)
        );
        assert_eq!(
            clamp_interval_minutes(Some(5)),
            Duration::from_secs(MIN_INTERVAL_MINUTES * minute)
        );
        assert_eq!(
            clamp_interval_minutes(Some(30)),
            Duration::from_secs(30 * minute)
        );
        assert_eq!(
            clamp_interval_minutes(Some(2000)),
            Duration::from_secs(MAX_INTERVAL_MINUTES * minute)
        );
    }

    #[test]
    fn state_keys_accept_v2_shapes() {
        for key in ["cache/seen", "a", "x-1_y/2", "tick-toy"] {
            assert!(validate_state_key(key).is_ok(), "{key}");
        }
    }

    #[test]
    fn state_keys_reject_unsafe_shapes() {
        for key in [
            "",
            "../escape",
            "/absolute",
            "UPPER",
            "trailing/",
            "a//b",
            "a/./b",
            "with space",
            "sneaky/../../x",
        ] {
            assert!(validate_state_key(key).is_err(), "{key}");
        }
        let long = "a".repeat(STATE_KEY_MAX_LEN + 1);
        assert!(validate_state_key(&long).is_err());
    }
}
