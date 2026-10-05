//! Test doubles for plugins: scripted connections and launchers,
//! fetchers, unpackers, verifiers, and roles.
//!
//! The real plugin runtime is a subprocess; tests that need one launch the
//! shipped examples (see `tests/it/plugins_examples.rs`). These fakes cover
//! the host logic around it: fan-out, publish guards, route clamps.
//! The memory stores (`MemoryTickStore`, `MemoryScrobblePrefsStore`,
//! `MemoryListenBrainzLinkStore`) are real implementations and double as
//! their own fakes.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::Value;

use crate::auth::users::roles::Role;

use super::install::{ArchiveEntry, ArchiveFetcher, ArchiveUnpacker};
use super::protocol::RpcError;
use super::runtime::{
    BoxFuture, CallError, EventKind, HostServices, LaunchSpec, PluginConnection, PluginEvent,
    PluginLauncher, RuntimeState, RuntimeStatus, ScrobbleEvent,
};
use crate::providers::listenbrainz::{ListenBrainzVerifier, VerifyOutcome};

/// Scripted plugin runtime. Answers each method from a script (default
/// `null`), records every call, and can make host requests as the plugin.
pub struct FakeConnection {
    answers: Mutex<HashMap<String, Result<Value, CallError>>>,
    /// Calls received, `(method, params)`, in order.
    pub calls: Mutex<Vec<(String, Value)>>,
    /// Notifications received, `(method, params)`, in order.
    pub notifications: Mutex<Vec<(String, Value)>>,
    /// How long every call takes.
    pub delay: Mutex<Duration>,
    services: Mutex<Option<(String, Arc<dyn HostServices>)>>,
    stopped: Mutex<bool>,
}

impl FakeConnection {
    /// A runtime answering `null` to everything.
    pub fn new() -> Self {
        Self {
            answers: Mutex::new(HashMap::new()),
            calls: Mutex::new(Vec::new()),
            notifications: Mutex::new(Vec::new()),
            delay: Mutex::new(Duration::ZERO),
            services: Mutex::new(None),
            stopped: Mutex::new(false),
        }
    }

    /// Script one method's answer.
    pub fn answer(&self, method: &str, answer: Result<Value, CallError>) {
        if let Ok(mut answers) = self.answers.lock() {
            answers.insert(method.to_owned(), answer);
        }
    }

    /// Calls made with one method.
    pub fn calls_to(&self, method: &str) -> Vec<Value> {
        self.calls
            .lock()
            .map(|calls| {
                calls
                    .iter()
                    .filter(|(name, _)| name == method)
                    .map(|(_, params)| params.clone())
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Make one plugin-to-host request, as the plugin would.
    pub async fn host_request(&self, method: &str, params: Value) -> Result<Value, RpcError> {
        let bound = self.services.lock().ok().and_then(|slot| slot.clone());
        let Some((name, services)) = bound else {
            return Err(RpcError::new(-1, "not launched"));
        };
        services.handle(&name, method, params).await
    }

    /// Whether the host stopped this runtime.
    pub fn was_stopped(&self) -> bool {
        self.stopped.lock().map(|stopped| *stopped).unwrap_or(false)
    }
}

impl Default for FakeConnection {
    fn default() -> Self {
        Self::new()
    }
}

impl PluginConnection for FakeConnection {
    fn call<'a>(
        &'a self,
        method: &'a str,
        params: Value,
        timeout: Duration,
    ) -> BoxFuture<'a, Result<Value, CallError>> {
        Box::pin(async move {
            if let Ok(mut calls) = self.calls.lock() {
                calls.push((method.to_owned(), params));
            }
            let delay = self.delay.lock().map(|delay| *delay).unwrap_or_default();
            if delay > timeout {
                tokio::time::sleep(timeout).await;
                return Err(CallError::Timeout);
            }
            if !delay.is_zero() {
                tokio::time::sleep(delay).await;
            }
            self.answers
                .lock()
                .ok()
                .and_then(|answers| answers.get(method).cloned())
                .unwrap_or(Ok(Value::Null))
        })
    }

    fn notify(&self, method: &str, params: Value) {
        if let Ok(mut notes) = self.notifications.lock() {
            notes.push((method.to_owned(), params));
        }
    }

    fn status(&self) -> RuntimeStatus {
        RuntimeStatus {
            state: if self.was_stopped() {
                RuntimeState::Stopped
            } else {
                RuntimeState::Running
            },
            restarts: 0,
            last_error: None,
            implemented: Vec::new(),
        }
    }

    fn stop(&self) -> BoxFuture<'_, ()> {
        Box::pin(async move {
            if let Ok(mut stopped) = self.stopped.lock() {
                *stopped = true;
            }
        })
    }
}

/// Scripted launcher: plugin names map to shared fake runtimes.
#[derive(Default)]
pub struct FakeLauncher {
    runtimes: Mutex<HashMap<String, Arc<FakeConnection>>>,
    /// Names that fail to launch, with reasons.
    pub failures: Mutex<HashMap<String, String>>,
    /// Launches made, by plugin name, in order.
    pub launched: Mutex<Vec<String>>,
}

impl FakeLauncher {
    /// Empty launcher: unknown names get a fresh runtime answering `null`.
    pub fn new() -> Self {
        Self::default()
    }

    /// Share one runtime under one plugin name.
    pub fn insert(&self, name: &str, runtime: Arc<FakeConnection>) {
        if let Ok(mut runtimes) = self.runtimes.lock() {
            runtimes.insert(name.to_owned(), runtime);
        }
    }

    /// The runtime bound to one name, creating it when absent.
    pub fn runtime(&self, name: &str) -> Arc<FakeConnection> {
        match self.runtimes.lock() {
            Ok(mut runtimes) => Arc::clone(
                runtimes
                    .entry(name.to_owned())
                    .or_insert_with(|| Arc::new(FakeConnection::new())),
            ),
            Err(_) => Arc::new(FakeConnection::new()),
        }
    }
}

impl PluginLauncher for FakeLauncher {
    fn launch(&self, spec: LaunchSpec) -> Result<Arc<dyn PluginConnection>, String> {
        let name = spec.manifest.name.clone();
        if let Ok(failures) = self.failures.lock()
            && let Some(reason) = failures.get(&name)
        {
            return Err(reason.clone());
        }
        if let Ok(mut launched) = self.launched.lock() {
            launched.push(name.clone());
        }
        let runtime = self.runtime(&name);
        if let Ok(mut slot) = runtime.services.lock() {
            *slot = Some((name, spec.services));
        }
        if let Ok(mut stopped) = runtime.stopped.lock() {
            *stopped = false;
        }
        Ok(runtime as Arc<dyn PluginConnection>)
    }
}

/// Scripted archive fetcher: URLs map to bytes (`None` = missing ref).
pub struct FakeFetcher {
    archives: Mutex<HashMap<String, Vec<u8>>>,
    /// URLs that fail, with reasons.
    pub failures: Mutex<HashMap<String, String>>,
    /// Requested URLs, in order.
    pub requested: Mutex<Vec<String>>,
}

impl FakeFetcher {
    /// Empty fetcher: every URL is a missing ref.
    pub fn new() -> Self {
        Self {
            archives: Mutex::new(HashMap::new()),
            failures: Mutex::new(HashMap::new()),
            requested: Mutex::new(Vec::new()),
        }
    }

    /// Serve bytes for one URL.
    pub fn insert(&self, url: &str, bytes: &[u8]) {
        if let Ok(mut guard) = self.archives.lock() {
            guard.insert(url.to_owned(), bytes.to_vec());
        }
    }
}

impl Default for FakeFetcher {
    fn default() -> Self {
        Self::new()
    }
}

impl ArchiveFetcher for FakeFetcher {
    fn fetch<'a>(&'a self, url: &'a str) -> BoxFuture<'a, Result<Option<Vec<u8>>, String>> {
        Box::pin(async move {
            if let Ok(mut guard) = self.requested.lock() {
                guard.push(url.to_owned());
            }
            if let Ok(guard) = self.failures.lock()
                && let Some(reason) = guard.get(url)
            {
                return Err(reason.clone());
            }
            Ok(self
                .archives
                .lock()
                .ok()
                .and_then(|guard| guard.get(url).cloned()))
        })
    }
}

/// Scripted unpacker: every archive unpacks to the same entries.
pub struct FakeUnpacker {
    entries: Vec<ArchiveEntry>,
    /// When set, every unpack fails with this reason.
    pub failure: Mutex<Option<String>>,
    /// Archives seen, in order.
    pub seen: Mutex<Vec<Vec<u8>>>,
}

impl FakeUnpacker {
    /// Unpacker serving fixed entries.
    pub fn new(entries: Vec<ArchiveEntry>) -> Self {
        Self {
            entries,
            failure: Mutex::new(None),
            seen: Mutex::new(Vec::new()),
        }
    }

    /// One file entry.
    pub fn file(path: &str, data: &[u8]) -> ArchiveEntry {
        ArchiveEntry {
            path: path.to_owned(),
            is_symlink: false,
            data: data.to_vec(),
        }
    }
}

impl ArchiveUnpacker for FakeUnpacker {
    fn unpack(&self, bytes: &[u8]) -> Result<Vec<ArchiveEntry>, String> {
        if let Ok(mut guard) = self.seen.lock() {
            guard.push(bytes.to_vec());
        }
        if let Ok(guard) = self.failure.lock()
            && let Some(reason) = guard.clone()
        {
            return Err(reason);
        }
        Ok(self.entries.clone())
    }
}

/// Scripted ListenBrainz verifier.
pub struct FakeVerifier {
    /// Outcome per (username, token) pair.
    pub outcomes: Mutex<HashMap<(String, String), VerifyOutcome>>,
    /// Fallback outcome.
    pub fallback: Mutex<VerifyOutcome>,
}

impl FakeVerifier {
    /// Verifier answering the fallback for every pair.
    pub fn new(fallback: VerifyOutcome) -> Self {
        Self {
            outcomes: Mutex::new(HashMap::new()),
            fallback: Mutex::new(fallback),
        }
    }

    /// Always-valid verifier.
    pub fn valid() -> Self {
        Self::new(VerifyOutcome {
            valid: true,
            message: "Successfully connected".to_owned(),
            rate_limited: false,
        })
    }
}

impl ListenBrainzVerifier for FakeVerifier {
    fn verify<'a>(&'a self, username: &'a str, token: &'a str) -> BoxFuture<'a, VerifyOutcome> {
        Box::pin(async move {
            if let Ok(guard) = self.outcomes.lock()
                && let Some(outcome) = guard.get(&(username.to_owned(), token.to_owned()))
            {
                return outcome.clone();
            }
            self.fallback
                .lock()
                .map(|guard| guard.clone())
                .unwrap_or(VerifyOutcome {
                    valid: false,
                    message: "ListenBrainz is temporarily unavailable. Try again shortly."
                        .to_owned(),
                    rate_limited: false,
                })
        })
    }
}

/// Fixed role map.
pub struct FakeRoles {
    roles: Mutex<HashMap<String, Role>>,
}

impl FakeRoles {
    /// Empty map: every lookup is a stale session.
    pub fn new() -> Self {
        Self {
            roles: Mutex::new(HashMap::new()),
        }
    }

    /// Pin one user's role.
    pub fn insert(&self, user_id: &str, role: Role) {
        if let Ok(mut guard) = self.roles.lock() {
            guard.insert(user_id.to_owned(), role);
        }
    }
}

impl Default for FakeRoles {
    fn default() -> Self {
        Self::new()
    }
}

impl super::handlers::UserRoles for FakeRoles {
    fn role_of(&self, user_id: &str) -> Option<Role> {
        self.roles
            .lock()
            .ok()
            .and_then(|guard| guard.get(user_id).copied())
    }
}

/// One scrobble-kind fan-out event with a fresh causation id.
pub fn scrobble_plugin_event(event: ScrobbleEvent) -> PluginEvent {
    PluginEvent {
        kind: EventKind::Scrobble,
        payload: super::runtime::EventPayload::Scrobble(event),
        causation_id: uuid::Uuid::new_v4().simple().to_string(),
    }
}
