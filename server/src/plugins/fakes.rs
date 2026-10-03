//! Test doubles for the plugins slice: scripted modules, loaders,
//! fetchers, unpackers, verifiers, and roles.
//!
//! The memory stores (`MemoryTickStore`, `MemoryTickJobs`,
//! `MemoryScrobblePrefsStore`, `MemoryListenBrainzLinkStore`) are real
//! implementations and double as their own fakes; only the behavior seams
//! need scripting here.

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};

use droppedneedle::auth::users::roles::Role;

use super::host::{ArchiveEntry, ArchiveFetcher, ArchiveUnpacker};
use super::manifest::PluginManifest;
use super::runtime::{
    BoxFuture, EventKind, ModuleLoader, PluginEvent, PluginModule, PluginPurchaseLink,
    PluginRouteBody, PluginRouteResponse, ScrobbleEvent, TickContext,
};
use super::scrobble::{ListenBrainzVerifier, VerifyOutcome};

/// How a fake tick behaves.
#[derive(Debug, Clone, Default)]
pub enum FakeTickBehavior {
    /// Finish cleanly.
    #[default]
    Ok,
    /// Fail with a message.
    Fail(String),
    /// Write one state blob, then finish cleanly.
    WriteState(String),
}

/// How a fake answers `/ext/` calls.
#[derive(Debug, Clone)]
pub enum FakeRouteScript {
    /// Answer 200 with a JSON body.
    Answer(serde_json::Value),
    /// Answer with a chosen status and body.
    Status(i32, serde_json::Value),
    /// Fail with a message (the host answers 502).
    Fail(String),
}

/// Scripted plugin module. Records every call for assertions.
pub struct FakeModule {
    provides: Vec<String>,
    /// Received scrobbles.
    pub scrobbles: Mutex<Vec<ScrobbleEvent>>,
    /// Received fan-out events.
    pub events: Mutex<Vec<PluginEvent>>,
    /// Ticks run.
    pub ticks: Mutex<u32>,
    /// Scripted links.
    pub links: Mutex<Vec<PluginPurchaseLink>>,
    /// Scripted tick behavior.
    pub tick_behavior: Mutex<FakeTickBehavior>,
    /// Scripted route answers, in order (the last repeats).
    pub route_scripts: Mutex<Vec<FakeRouteScript>>,
    /// Received route calls `(method, subpath)`.
    pub route_calls: Mutex<Vec<(String, String)>>,
    /// Whether the module serves routes.
    pub route_handler: bool,
}

impl FakeModule {
    /// Module implementing the given capabilities.
    pub fn providing(caps: &[&str]) -> Self {
        Self {
            provides: caps.iter().map(|cap| cap.to_string()).collect(),
            scrobbles: Mutex::new(Vec::new()),
            events: Mutex::new(Vec::new()),
            ticks: Mutex::new(0),
            links: Mutex::new(Vec::new()),
            tick_behavior: Mutex::new(FakeTickBehavior::Ok),
            route_scripts: Mutex::new(Vec::new()),
            route_calls: Mutex::new(Vec::new()),
            route_handler: caps.contains(&"publisher"),
        }
    }

    /// Ticks run so far.
    pub fn tick_count(&self) -> u32 {
        self.ticks.lock().map(|guard| *guard).unwrap_or(0)
    }

    /// Events received so far.
    pub fn event_count(&self) -> usize {
        self.events.lock().map(|guard| guard.len()).unwrap_or(0)
    }
}

impl PluginModule for FakeModule {
    fn provides(&self, capability: &str) -> bool {
        self.provides.iter().any(|cap| cap == capability)
    }

    fn on_scrobble<'a>(&'a self, event: &'a ScrobbleEvent) -> BoxFuture<'a, Result<(), String>> {
        Box::pin(async move {
            if let Ok(mut guard) = self.scrobbles.lock() {
                guard.push(event.clone());
            }
            Ok(())
        })
    }

    fn purchase_links<'a>(
        &'a self,
        _artist: &'a str,
        _album: &'a str,
        _release_group_mbid: &'a str,
    ) -> BoxFuture<'a, Result<Vec<PluginPurchaseLink>, String>> {
        Box::pin(async move {
            Ok(self
                .links
                .lock()
                .map(|guard| guard.clone())
                .unwrap_or_default())
        })
    }

    fn on_event<'a>(&'a self, event: &'a PluginEvent) -> BoxFuture<'a, Result<(), String>> {
        Box::pin(async move {
            if let Ok(mut guard) = self.events.lock() {
                guard.push(event.clone());
            }
            Ok(())
        })
    }

    fn on_tick<'a>(&'a self, ctx: &'a TickContext<'a>) -> BoxFuture<'a, Result<(), String>> {
        Box::pin(async move {
            if let Ok(mut guard) = self.ticks.lock() {
                *guard += 1;
            }
            let behavior = self
                .tick_behavior
                .lock()
                .map(|guard| guard.clone())
                .unwrap_or(FakeTickBehavior::Ok);
            match behavior {
                FakeTickBehavior::Ok => Ok(()),
                FakeTickBehavior::Fail(reason) => Err(reason),
                FakeTickBehavior::WriteState(blob) => {
                    ctx.state.write("state", blob.into_bytes()).await
                }
            }
        })
    }

    fn has_route_handler(&self) -> bool {
        self.route_handler
    }

    fn handle_route<'a>(
        &'a self,
        method: &'a str,
        subpath: &'a str,
        _query: &'a HashMap<String, String>,
        _body: &'a PluginRouteBody,
    ) -> BoxFuture<'a, Result<PluginRouteResponse, String>> {
        Box::pin(async move {
            if let Ok(mut guard) = self.route_calls.lock() {
                guard.push((method.to_owned(), subpath.to_owned()));
            }
            let script = self
                .route_scripts
                .lock()
                .ok()
                .and_then(|guard| guard.last().cloned());
            match script {
                Some(FakeRouteScript::Answer(body)) => Ok(PluginRouteResponse::ok(body)),
                Some(FakeRouteScript::Status(status, body)) => {
                    Ok(PluginRouteResponse { status, body })
                }
                Some(FakeRouteScript::Fail(reason)) => Err(reason),
                None => Ok(PluginRouteResponse::ok(serde_json::json!({"ok": true}))),
            }
        })
    }
}

/// Scripted module loader: names map to shared fake modules.
pub struct FakeLoader {
    modules: Mutex<HashMap<String, Arc<FakeModule>>>,
    /// Names that fail to load, with reasons.
    pub failures: Mutex<HashMap<String, String>>,
}

impl FakeLoader {
    /// Empty loader.
    pub fn new() -> Self {
        Self {
            modules: Mutex::new(HashMap::new()),
            failures: Mutex::new(HashMap::new()),
        }
    }

    /// Share one fake module under one plugin name.
    pub fn insert(&self, name: &str, module: Arc<FakeModule>) {
        if let Ok(mut guard) = self.modules.lock() {
            guard.insert(name.to_owned(), module);
        }
    }

    /// Fail one plugin's load with a reason.
    pub fn fail(&self, name: &str, reason: &str) {
        if let Ok(mut guard) = self.failures.lock() {
            guard.insert(name.to_owned(), reason.to_owned());
        }
    }
}

impl Default for FakeLoader {
    fn default() -> Self {
        Self::new()
    }
}

impl ModuleLoader for FakeLoader {
    fn load(
        &self,
        _dir: &Path,
        manifest: &PluginManifest,
    ) -> Result<Arc<dyn PluginModule>, String> {
        if let Ok(guard) = self.failures.lock()
            && let Some(reason) = guard.get(&manifest.name)
        {
            return Err(reason.clone());
        }
        self.modules
            .lock()
            .ok()
            .and_then(|guard| guard.get(&manifest.name).cloned())
            .map(|module| module as Arc<dyn PluginModule>)
            .ok_or_else(|| format!("no module for '{}'", manifest.name))
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

    /// Script one pair's outcome.
    pub fn insert(&self, username: &str, token: &str, outcome: VerifyOutcome) {
        if let Ok(mut guard) = self.outcomes.lock() {
            guard.insert((username.to_owned(), token.to_owned()), outcome);
        }
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
