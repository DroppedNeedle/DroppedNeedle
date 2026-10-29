//! Fakes of what plugins reach outside the process: the plugin's own
//! child process and GitHub. The real subprocess runtime is tested with
//! the shipped examples in `plugins_examples`.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use droppedneedle_plugins::protocol::RpcError;
use droppedneedle_plugins::runtime::{
    BoxFuture, CallError, EventKind, EventPayload, HostServices, LaunchSpec, PluginConnection,
    PluginEvent, PluginLauncher, RuntimeState, RuntimeStatus, ScrobbleEvent,
};
use droppedneedle_providers::executor::{Prepared, WireAnswer, WireError};
use droppedneedle_providers::github::{API_BASE, CODELOAD_BASE, GitHubRepos};
use droppedneedle_providers::{ProviderEndpoints, Providers, Wire};
use serde_json::Value;

/// Stands in for a plugin's child process: answers each method from a
/// script (default `null`), records every call, and can make host requests
/// as the plugin.
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

/// Stands in for starting a plugin's child process: plugin names map to
/// shared fake runtimes.
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

/// One scripted GitHub answer: status, headers, body.
type GitHubAnswer = (u16, Vec<(String, String)>, Vec<u8>);

/// Stands in for GitHub on the providers' wire: each URL gives the answer
/// last set for it, and a URL with none is a 404.
pub struct FakeGitHub {
    answers: Mutex<HashMap<String, GitHubAnswer>>,
    /// Requested URLs, in order.
    pub requested: Mutex<Vec<String>>,
}

impl FakeGitHub {
    /// Empty fake: every URL is a 404.
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            answers: Mutex::new(HashMap::new()),
            requested: Mutex::new(Vec::new()),
        })
    }

    /// Serve bytes for one URL.
    pub fn insert(&self, url: &str, bytes: &[u8]) {
        self.set(url, 200, &[], bytes.to_vec());
    }

    /// Answer one URL with a bodiless status and these headers.
    pub fn answer(&self, url: &str, status: u16, headers: &[(&str, &str)]) {
        self.set(url, status, headers, Vec::new());
    }

    fn set(&self, url: &str, status: u16, headers: &[(&str, &str)], body: Vec<u8>) {
        let headers = headers
            .iter()
            .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
            .collect();
        if let Ok(mut guard) = self.answers.lock() {
            guard.insert(url.to_owned(), (status, headers, body));
        }
    }

    /// The installer's GitHub client over this fake, with GitHub at its real
    /// addresses, on a fresh `github` row that holds cooldowns but does not
    /// pace.
    pub fn client(self: &Arc<Self>) -> GitHubRepos {
        let endpoints = ProviderEndpoints {
            github_api: API_BASE.to_owned(),
            github_codeload: CODELOAD_BASE.to_owned(),
            ..ProviderEndpoints::closed()
        };
        Providers::unpaced_at(Arc::clone(self) as Arc<dyn Wire>, endpoints)
            .github_repos
            .clone()
    }
}

impl FakeGitHub {
    /// The providers over this fake with the real `github` row, paced as
    /// in production.
    pub fn paced(self: &Arc<Self>) -> Providers {
        Providers::over_wire(
            Arc::clone(self) as Arc<dyn Wire>,
            Arc::new(droppedneedle_kernel::clock::SystemClock),
            ProviderEndpoints {
                github_api: API_BASE.to_owned(),
                github_codeload: CODELOAD_BASE.to_owned(),
                ..ProviderEndpoints::closed()
            },
        )
    }
}

impl Wire for FakeGitHub {
    fn send<'a>(&'a self, prepared: Prepared<'a>) -> BoxFuture<'a, Result<WireAnswer, WireError>> {
        Box::pin(async move {
            let url = prepared.url.to_string();
            if let Ok(mut guard) = self.requested.lock() {
                guard.push(url.clone());
            }
            let (status, headers, body) = self
                .answers
                .lock()
                .ok()
                .and_then(|guard| guard.get(&url).cloned())
                .unwrap_or((404, Vec::new(), Vec::new()));
            if body.len() > prepared.max_body {
                return Err(WireError::TooLarge);
            }
            Ok(WireAnswer {
                status,
                headers,
                body,
            })
        })
    }
}

/// One scrobble-kind fan-out event with a fresh causation id.
pub fn scrobble_plugin_event(event: ScrobbleEvent) -> PluginEvent {
    PluginEvent {
        kind: EventKind::Scrobble,
        payload: EventPayload::Scrobble(event),
        causation_id: uuid::Uuid::new_v4().simple().to_string(),
    }
}
