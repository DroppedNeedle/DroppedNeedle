//! One plugin as a supervised subprocess speaking the JSON-RPC protocol
//! over stdin and stdout.
//!
//! [`ProcessLauncher`] starts a supervisor task per enabled plugin. The
//! supervisor spawns the child, runs the `initialize` handshake, serves
//! calls until the child exits or stops answering, and starts it again
//! with a growing backoff (1 s doubling to 5 min, reset after 5 min of
//! healthy running). Stopping asks the plugin to exit, then kills its
//! whole process group.
//!
//! What the child gets, and what that does and does not prevent:
//! - A scrubbed environment: only `PATH`, `HOME`, `TMPDIR`, `LANG`, `TZ`,
//!   proxy and CA variables, `PYTHONPATH` and the plugin's own name and
//!   paths. No server secrets, no config or cache paths.
//! - Its own data directory as the working directory. The config and cache
//!   directories are never passed in.
//! - A memory cap (`RLIMIT_AS`, 1 GiB by default), no core dumps, no new
//!   privileges, its own process group.
//! - A time budget on every call; three timeouts in a row restart it.
//!
//! This is not a sandbox. The child runs as the server's user, so a plugin
//! that goes looking for the config directory on disk can still read it.
//! The limits stop accidents and keep a crash or a hang from taking the
//! server down; they do not make untrusted code safe.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::{Notify, Semaphore, mpsc, oneshot, watch};

use super::manifest::PluginManifest;
use super::protocol::{
    self, HostInfo, Incoming, InitializeParams, InitializeResult, PluginIdentity, RpcError, RpcId,
    codes, methods,
};
use super::runtime::{
    BoxFuture, CallError, HostServices, LaunchSpec, PluginConnection, PluginLauncher, RuntimeState,
    RuntimeStatus,
};

/// Default address-space cap for one plugin process (1 GiB).
pub const DEFAULT_MEMORY_LIMIT_BYTES: u64 = 1024 * 1024 * 1024;
/// Time the plugin has to answer `initialize`.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(15);
/// Time the plugin has to exit after `shutdown` before it is killed.
const SHUTDOWN_GRACE: Duration = Duration::from_secs(2);
/// First restart delay.
const BACKOFF_START: Duration = Duration::from_secs(1);
/// Longest restart delay.
const BACKOFF_MAX: Duration = Duration::from_secs(300);
/// Running this long without a crash resets the backoff.
const HEALTHY_UPTIME: Duration = Duration::from_secs(300);
/// Timeouts in a row that count as a hang and restart the plugin.
const HANG_TIMEOUTS: u32 = 3;
/// Messages queued for the plugin's stdin before calls fail fast.
const INPUT_QUEUE: usize = 256;
/// Plugin-to-host requests one plugin may have in flight.
const HOST_REQUESTS_IN_FLIGHT: usize = 32;
/// Budget for answering one plugin-to-host request.
const HOST_REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
/// Longest stderr line kept in the server log.
const STDERR_LINE_MAX: usize = 2048;

/// How to run plugins as subprocesses.
#[derive(Debug, Clone)]
pub struct ProcessLauncher {
    /// Interpreter for Python entrypoints (`python3` on `PATH` by default).
    pub python: String,
    /// Directory holding the `droppedneedle_plugin` helper module.
    pub sdk_dir: PathBuf,
    /// Address-space cap per plugin process.
    pub memory_limit_bytes: u64,
}

impl ProcessLauncher {
    /// Launcher with the default interpreter and memory cap.
    pub fn new(sdk_dir: PathBuf) -> Self {
        Self {
            python: "python3".to_owned(),
            sdk_dir,
            memory_limit_bytes: DEFAULT_MEMORY_LIMIT_BYTES,
        }
    }
}

/// The program and arguments that run one plugin.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginCommand {
    /// Program path or name looked up on `PATH`.
    pub program: String,
    /// Arguments.
    pub args: Vec<String>,
}

/// Resolve the command for a manifest. A `command` in the manifest runs
/// as given, with a leading `./` resolved inside the plugin directory.
/// Without one, the `module:Class` entrypoint runs under the Python helper.
pub fn plugin_command(
    manifest: &PluginManifest,
    plugin_dir: &Path,
    python: &str,
) -> Result<PluginCommand, String> {
    if let Some((program, args)) = manifest.command.split_first() {
        let program = match program.strip_prefix("./") {
            Some(relative) => {
                if relative
                    .split('/')
                    .any(|part| part == ".." || part.is_empty())
                {
                    return Err(format!("command {program:?} escapes the plugin folder"));
                }
                plugin_dir.join(relative).to_string_lossy().into_owned()
            }
            None => program.clone(),
        };
        return Ok(PluginCommand {
            program,
            args: args.to_vec(),
        });
    }
    if manifest.entrypoint.is_empty() {
        return Err("manifest has neither a command nor an entrypoint".to_owned());
    }
    Ok(PluginCommand {
        program: python.to_owned(),
        args: vec![
            "-m".to_owned(),
            "droppedneedle_plugin".to_owned(),
            manifest.entrypoint.clone(),
        ],
    })
}

/// The child's whole environment. Nothing from the server's own
/// environment passes except the listed harmless variables.
fn plugin_env(spec: &LaunchSpec, sdk_dir: &Path) -> Vec<(String, String)> {
    let mut env = vec![
        (
            "PATH".to_owned(),
            std::env::var("PATH").unwrap_or_else(|_| "/usr/local/bin:/usr/bin:/bin".to_owned()),
        ),
        ("HOME".to_owned(), path_text(&spec.data_dir)),
        ("TMPDIR".to_owned(), path_text(&spec.data_dir.join("tmp"))),
        ("LANG".to_owned(), "C.UTF-8".to_owned()),
        ("PYTHONUNBUFFERED".to_owned(), "1".to_owned()),
        ("PYTHONDONTWRITEBYTECODE".to_owned(), "1".to_owned()),
        (
            "PYTHONPATH".to_owned(),
            format!("{}:{}", path_text(sdk_dir), path_text(&spec.plugin_dir)),
        ),
        // glibc reserves 64 MiB of address space per thread arena; two
        // arenas keep a threaded plugin well inside the memory cap.
        ("MALLOC_ARENA_MAX".to_owned(), "2".to_owned()),
        (
            "DROPPEDNEEDLE_PLUGIN_NAME".to_owned(),
            spec.manifest.name.clone(),
        ),
        (
            "DROPPEDNEEDLE_PLUGIN_DIR".to_owned(),
            path_text(&spec.plugin_dir),
        ),
        (
            "DROPPEDNEEDLE_PLUGIN_DATA_DIR".to_owned(),
            path_text(&spec.data_dir),
        ),
    ];
    for key in [
        "TZ",
        "HTTP_PROXY",
        "HTTPS_PROXY",
        "NO_PROXY",
        "http_proxy",
        "https_proxy",
        "no_proxy",
        "SSL_CERT_FILE",
        "SSL_CERT_DIR",
        "REQUESTS_CA_BUNDLE",
    ] {
        if let Ok(value) = std::env::var(key) {
            env.push((key.to_owned(), value));
        }
    }
    env
}

fn path_text(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

impl PluginLauncher for ProcessLauncher {
    fn launch(&self, spec: LaunchSpec) -> Result<Arc<dyn PluginConnection>, String> {
        let command = plugin_command(&spec.manifest, &spec.plugin_dir, &self.python)?;
        std::fs::create_dir_all(spec.data_dir.join("tmp"))
            .map_err(|error| format!("cannot create the plugin data folder: {error}"))?;
        let runtime = tokio::runtime::Handle::try_current()
            .map_err(|_| "plugins start only inside the server runtime".to_owned())?;
        let env = plugin_env(&spec, &self.sdk_dir);
        let (state_tx, _) = watch::channel(0u64);
        let shared = Arc::new(Shared {
            name: spec.manifest.name.clone(),
            status: Mutex::new(RuntimeStatus {
                state: RuntimeState::Starting,
                restarts: 0,
                last_error: None,
                implemented: Vec::new(),
            }),
            session: Mutex::new(None),
            changed: state_tx,
            stopping: AtomicBool::new(false),
            stop_now: Notify::new(),
            restart_now: Notify::new(),
            timeouts_in_row: AtomicU32::new(0),
            settings: Mutex::new(spec.settings.clone()),
        });
        let child = ChildSpec {
            command,
            env,
            data_dir: spec.data_dir.clone(),
            plugin_dir: spec.plugin_dir.clone(),
            memory_limit_bytes: self.memory_limit_bytes,
            manifest: spec.manifest,
            services: spec.services,
        };
        runtime.spawn(supervise(Arc::clone(&shared), child));
        Ok(Arc::new(ProcessConnection { shared }))
    }
}

/// What the supervisor needs to (re)start the child.
struct ChildSpec {
    command: PluginCommand,
    env: Vec<(String, String)>,
    data_dir: PathBuf,
    plugin_dir: PathBuf,
    memory_limit_bytes: u64,
    manifest: PluginManifest,
    services: Arc<dyn HostServices>,
}

/// State shared by the connection handle and its supervisor.
struct Shared {
    name: String,
    status: Mutex<RuntimeStatus>,
    session: Mutex<Option<Arc<Session>>>,
    /// Bumped on every state change so waiting calls re-check.
    changed: watch::Sender<u64>,
    stopping: AtomicBool,
    stop_now: Notify,
    restart_now: Notify,
    timeouts_in_row: AtomicU32,
    /// Latest settings, sent again in `initialize` after a restart.
    settings: Mutex<HashMap<String, String>>,
}

impl Shared {
    fn set_state(&self, state: RuntimeState, error: Option<String>) {
        if let Ok(mut status) = self.status.lock() {
            status.state = state;
            if error.is_some() {
                status.last_error = error;
            }
            if state != RuntimeState::Running {
                status.implemented.clear();
            }
        }
        self.changed.send_modify(|generation| *generation += 1);
    }

    fn state(&self) -> RuntimeState {
        self.status
            .lock()
            .map(|status| status.state)
            .unwrap_or(RuntimeState::Failed)
    }

    fn current_session(&self) -> Option<Arc<Session>> {
        self.session.lock().ok().and_then(|guard| guard.clone())
    }
}

/// One live child: its input queue and the calls waiting on it.
struct Session {
    input: mpsc::Sender<String>,
    pending: Mutex<HashMap<i64, oneshot::Sender<Result<Value, RpcError>>>>,
    next_id: AtomicI64,
}

impl Session {
    /// Fail every waiting call at once (the child is gone).
    fn fail_pending(&self) {
        if let Ok(mut pending) = self.pending.lock() {
            pending.clear();
        }
    }

    fn send(&self, line: String) -> bool {
        self.input.try_send(line).is_ok()
    }
}

/// Removes a call's pending entry if the caller gives up or is dropped,
/// and tells the plugin to stop working on it.
struct PendingGuard {
    session: Arc<Session>,
    id: i64,
    done: bool,
}

impl Drop for PendingGuard {
    fn drop(&mut self) {
        if self.done {
            return;
        }
        let removed = self
            .session
            .pending
            .lock()
            .map(|mut pending| pending.remove(&self.id).is_some())
            .unwrap_or(false);
        if removed {
            self.session.send(protocol::notification_line(
                methods::CANCEL,
                &serde_json::json!({ "id": self.id }),
            ));
        }
    }
}

/// The handle the host keeps for one plugin process.
pub struct ProcessConnection {
    shared: Arc<Shared>,
}

impl ProcessConnection {
    /// Wait until the plugin is running, failed, or the deadline passes.
    async fn running_session(
        &self,
        deadline: tokio::time::Instant,
    ) -> Result<Arc<Session>, CallError> {
        let mut changes = self.shared.changed.subscribe();
        loop {
            match self.shared.state() {
                RuntimeState::Running => {
                    if let Some(session) = self.shared.current_session() {
                        return Ok(session);
                    }
                }
                RuntimeState::Stopped => {
                    return Err(CallError::NotRunning("plugin is stopped".to_owned()));
                }
                RuntimeState::Failed => {
                    let reason = self
                        .shared
                        .status
                        .lock()
                        .ok()
                        .and_then(|status| status.last_error.clone())
                        .unwrap_or_else(|| "plugin failed to start".to_owned());
                    return Err(CallError::NotRunning(reason));
                }
                RuntimeState::Starting | RuntimeState::Restarting => {}
            }
            match tokio::time::timeout_at(deadline, changes.changed()).await {
                Ok(Ok(())) => continue,
                Ok(Err(_)) => {
                    return Err(CallError::NotRunning("plugin supervisor ended".to_owned()));
                }
                Err(_) => {
                    return Err(CallError::NotRunning("plugin is still starting".to_owned()));
                }
            }
        }
    }
}

impl Drop for ProcessConnection {
    fn drop(&mut self) {
        // The host always stops a connection it replaces; this covers a
        // handle dropped any other way so no supervisor outlives it.
        self.shared.stopping.store(true, Ordering::Relaxed);
        self.shared.stop_now.notify_one();
    }
}

impl PluginConnection for ProcessConnection {
    fn call<'a>(
        &'a self,
        method: &'a str,
        params: Value,
        timeout: Duration,
    ) -> BoxFuture<'a, Result<Value, CallError>> {
        Box::pin(async move {
            let deadline = tokio::time::Instant::now() + timeout;
            let session = self.running_session(deadline).await?;
            let id = session.next_id.fetch_add(1, Ordering::Relaxed);
            let (tx, rx) = oneshot::channel();
            if let Ok(mut pending) = session.pending.lock() {
                pending.insert(id, tx);
            }
            let mut guard = PendingGuard {
                session: Arc::clone(&session),
                id,
                done: false,
            };
            if !session.send(protocol::request_line(id, method, &params)) {
                return Err(CallError::NotRunning(
                    "plugin is not reading its input".to_owned(),
                ));
            }
            let outcome = tokio::time::timeout_at(deadline, rx).await;
            match outcome {
                Ok(Ok(answer)) => {
                    guard.done = true;
                    self.shared.timeouts_in_row.store(0, Ordering::Relaxed);
                    answer.map_err(|error| match error.code {
                        codes::METHOD_NOT_FOUND => CallError::Unsupported(method.to_owned()),
                        _ => CallError::Failed(error.message),
                    })
                }
                Ok(Err(_)) => {
                    guard.done = true;
                    Err(CallError::NotRunning("plugin exited".to_owned()))
                }
                Err(_) => {
                    // The guard cancels the call on drop.
                    let in_row = self.shared.timeouts_in_row.fetch_add(1, Ordering::Relaxed) + 1;
                    if in_row >= HANG_TIMEOUTS {
                        tracing::warn!(
                            plugin = %self.shared.name,
                            timeouts = in_row,
                            "plugin stopped answering; restarting it"
                        );
                        self.shared.timeouts_in_row.store(0, Ordering::Relaxed);
                        self.shared.restart_now.notify_one();
                    }
                    Err(CallError::Timeout)
                }
            }
        })
    }

    fn notify(&self, method: &str, params: Value) {
        if method == methods::SETTINGS_UPDATE
            && let Some(settings) = params.get("settings")
            && let Ok(map) = serde_json::from_value::<HashMap<String, String>>(settings.clone())
            && let Ok(mut stored) = self.shared.settings.lock()
        {
            *stored = map;
        }
        if let Some(session) = self.shared.current_session() {
            session.send(protocol::notification_line(method, &params));
        }
    }

    fn status(&self) -> RuntimeStatus {
        self.shared
            .status
            .lock()
            .map(|status| status.clone())
            .unwrap_or(RuntimeStatus {
                state: RuntimeState::Failed,
                restarts: 0,
                last_error: Some("status unreadable".to_owned()),
                implemented: Vec::new(),
            })
    }

    fn stop(&self) -> BoxFuture<'_, ()> {
        Box::pin(async move {
            self.shared.stopping.store(true, Ordering::Relaxed);
            self.shared.stop_now.notify_one();
            let mut changes = self.shared.changed.subscribe();
            let wait = async {
                while self.shared.state() != RuntimeState::Stopped
                    && self.shared.state() != RuntimeState::Failed
                {
                    if changes.changed().await.is_err() {
                        break;
                    }
                }
            };
            let _ = tokio::time::timeout(SHUTDOWN_GRACE * 3, wait).await;
        })
    }
}

/// Why one child run ended.
enum RunEnd {
    /// Stop requested; do not restart.
    Stopped,
    /// Something went wrong; restart after the backoff.
    Crashed(String),
    /// Restarting cannot help (protocol mismatch).
    Fatal(String),
}

/// The supervisor loop: start, serve, restart with backoff, until stopped.
async fn supervise(shared: Arc<Shared>, spec: ChildSpec) {
    let mut backoff = BACKOFF_START;
    loop {
        if shared.stopping.load(Ordering::Relaxed) {
            break;
        }
        shared.set_state(RuntimeState::Starting, None);
        let started = Instant::now();
        let end = run_child(&shared, &spec).await;
        if let Some(session) = shared
            .session
            .lock()
            .ok()
            .and_then(|mut guard| guard.take())
        {
            session.fail_pending();
        }
        match end {
            RunEnd::Stopped => break,
            RunEnd::Fatal(reason) => {
                tracing::error!(plugin = %shared.name, %reason, "plugin cannot run");
                shared.set_state(RuntimeState::Failed, Some(reason));
                return;
            }
            RunEnd::Crashed(reason) => {
                if shared.stopping.load(Ordering::Relaxed) {
                    break;
                }
                if started.elapsed() >= HEALTHY_UPTIME {
                    backoff = BACKOFF_START;
                }
                tracing::warn!(
                    plugin = %shared.name,
                    %reason,
                    retry_in_seconds = backoff.as_secs(),
                    "plugin stopped; restarting"
                );
                if let Ok(mut status) = shared.status.lock() {
                    status.restarts += 1;
                }
                shared.set_state(RuntimeState::Restarting, Some(reason));
                tokio::select! {
                    _ = tokio::time::sleep(backoff) => {}
                    _ = shared.stop_now.notified() => {}
                }
                backoff = (backoff * 2).min(BACKOFF_MAX);
            }
        }
    }
    shared.set_state(RuntimeState::Stopped, None);
}

/// Spawn one child, handshake, serve until it ends.
async fn run_child(shared: &Arc<Shared>, spec: &ChildSpec) -> RunEnd {
    let mut command = tokio::process::Command::new(&spec.command.program);
    command
        .args(&spec.command.args)
        .current_dir(&spec.data_dir)
        .env_clear()
        .envs(
            spec.env
                .iter()
                .map(|(key, value)| (key.as_str(), value.as_str())),
        )
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    let memory_limit = spec.memory_limit_bytes;
    // SAFETY: the closure runs in the forked child before exec and only
    // makes async-signal-safe system calls (setsid, setrlimit, prctl). It
    // allocates nothing and touches no locks.
    unsafe {
        command.pre_exec(move || {
            if libc::setsid() == -1 {
                return Err(std::io::Error::last_os_error());
            }
            let memory = libc::rlimit {
                rlim_cur: memory_limit as libc::rlim_t,
                rlim_max: memory_limit as libc::rlim_t,
            };
            if libc::setrlimit(libc::RLIMIT_AS, &memory) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            let no_core = libc::rlimit {
                rlim_cur: 0,
                rlim_max: 0,
            };
            libc::setrlimit(libc::RLIMIT_CORE, &no_core);
            libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0);
            Ok(())
        });
    }
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) => {
            return RunEnd::Crashed(format!("could not start {}: {error}", spec.command.program));
        }
    };
    let (Some(stdin), Some(stdout), Some(stderr)) =
        (child.stdin.take(), child.stdout.take(), child.stderr.take())
    else {
        kill_group(&mut child).await;
        return RunEnd::Crashed("child pipes were not available".to_owned());
    };
    tokio::spawn(pump_stderr(shared.name.clone(), stderr));

    let (input_tx, input_rx) = mpsc::channel::<String>(INPUT_QUEUE);
    tokio::spawn(write_input(stdin, input_rx));
    let session = Arc::new(Session {
        input: input_tx,
        pending: Mutex::new(HashMap::new()),
        next_id: AtomicI64::new(1),
    });
    let mut reader = tokio::spawn(read_output(
        shared.name.clone(),
        stdout,
        Arc::clone(&session),
        Arc::clone(&spec.services),
    ));

    let handshake = tokio::select! {
        answer = handshake(shared, spec, &session) => answer,
        ended = &mut reader => Err(RunEnd::Crashed(format!(
            "plugin exited during the handshake ({})",
            ended.unwrap_or_else(|error| error.to_string())
        ))),
        _ = shared.stop_now.notified() => Err(RunEnd::Stopped),
    };
    let implemented = match handshake {
        Ok(implemented) => implemented,
        Err(end) => {
            reader.abort();
            kill_group(&mut child).await;
            return end;
        }
    };
    let undeclared: Vec<&String> = implemented
        .iter()
        .filter(|capability| !spec.manifest.capabilities.contains(capability))
        .collect();
    if !undeclared.is_empty() {
        tracing::info!(
            plugin = %shared.name,
            ?undeclared,
            "plugin implements capabilities its manifest does not declare; they stay off"
        );
    }
    for declared in &spec.manifest.capabilities {
        if !implemented.contains(declared) {
            tracing::warn!(
                plugin = %shared.name,
                capability = %declared,
                "capability declared but not implemented"
            );
        }
    }
    if let Ok(mut slot) = shared.session.lock() {
        *slot = Some(Arc::clone(&session));
    }
    if let Ok(mut status) = shared.status.lock() {
        status.implemented = implemented;
    }
    shared.set_state(RuntimeState::Running, None);
    tracing::info!(plugin = %shared.name, pid = child.id(), "plugin running");

    let end = tokio::select! {
        ended = &mut reader => {
            let reason = ended.unwrap_or_else(|error| error.to_string());
            let status = child.wait().await.map(|status| status.to_string()).unwrap_or_default();
            RunEnd::Crashed(format!("{reason} {status}").trim().to_owned())
        }
        _ = shared.restart_now.notified() => RunEnd::Crashed("plugin stopped answering".to_owned()),
        _ = shared.stop_now.notified() => {
            session.send(protocol::request_line(0, methods::SHUTDOWN, &Value::Null));
            let _ = tokio::time::timeout(SHUTDOWN_GRACE, child.wait()).await;
            RunEnd::Stopped
        }
    };
    reader.abort();
    kill_group(&mut child).await;
    end
}

/// Send `initialize` and check the answer. Returns the implemented
/// capabilities.
async fn handshake(
    shared: &Arc<Shared>,
    spec: &ChildSpec,
    session: &Arc<Session>,
) -> Result<Vec<String>, RunEnd> {
    let settings = shared
        .settings
        .lock()
        .map(|settings| settings.clone())
        .unwrap_or_default();
    let params = InitializeParams {
        protocol_version: protocol::PROTOCOL_VERSION,
        host: HostInfo {
            name: "droppedneedle".to_owned(),
            version: env!("CARGO_PKG_VERSION").to_owned(),
        },
        plugin: PluginIdentity {
            name: spec.manifest.name.clone(),
            version: spec.manifest.version.clone(),
            api_version: spec.manifest.api_version,
            entrypoint: spec.manifest.entrypoint.clone(),
            capabilities: spec.manifest.capabilities.clone(),
        },
        settings,
        plugin_dir: path_text(&spec.plugin_dir),
        data_dir: path_text(&spec.data_dir),
    };
    let params = serde_json::to_value(&params)
        .map_err(|error| RunEnd::Fatal(format!("cannot encode initialize: {error}")))?;
    let id = session.next_id.fetch_add(1, Ordering::Relaxed);
    let (tx, rx) = oneshot::channel();
    if let Ok(mut pending) = session.pending.lock() {
        pending.insert(id, tx);
    }
    if !session.send(protocol::request_line(id, methods::INITIALIZE, &params)) {
        return Err(RunEnd::Crashed("plugin input closed".to_owned()));
    }
    let answer = match tokio::time::timeout(HANDSHAKE_TIMEOUT, rx).await {
        Ok(Ok(Ok(value))) => value,
        Ok(Ok(Err(error))) => {
            return Err(RunEnd::Fatal(format!(
                "plugin refused initialize: {}",
                error.message
            )));
        }
        Ok(Err(_)) => {
            return Err(RunEnd::Crashed(
                "plugin exited during the handshake".to_owned(),
            ));
        }
        Err(_) => {
            return Err(RunEnd::Crashed(
                "plugin did not answer initialize".to_owned(),
            ));
        }
    };
    let result: InitializeResult = serde_json::from_value(answer)
        .map_err(|error| RunEnd::Fatal(format!("bad initialize answer: {error}")))?;
    if result.protocol_version != protocol::PROTOCOL_VERSION {
        return Err(RunEnd::Fatal(format!(
            "plugin speaks protocol {}; this server speaks {}",
            result.protocol_version,
            protocol::PROTOCOL_VERSION
        )));
    }
    Ok(result.capabilities)
}

/// Kill the child's whole process group, then reap it.
async fn kill_group(child: &mut tokio::process::Child) {
    if let Some(pid) = child.id()
        && let Ok(pid) = i32::try_from(pid)
    {
        // SAFETY: killpg only sends a signal. The group id is the child's
        // pid because the child called setsid before exec.
        unsafe {
            libc::killpg(pid, libc::SIGKILL);
        }
    }
    let _ = child.kill().await;
}

/// Feed queued lines to the child's stdin until the queue or pipe closes.
async fn write_input(mut stdin: tokio::process::ChildStdin, mut lines: mpsc::Receiver<String>) {
    while let Some(line) = lines.recv().await {
        if stdin.write_all(line.as_bytes()).await.is_err() || stdin.flush().await.is_err() {
            break;
        }
    }
}

/// Copy the child's stderr into the server log, one line per entry, with
/// the plugin's name. A leading `ERROR`/`WARNING`/`DEBUG` level word (the
/// Python helper's log format) picks the log level.
async fn pump_stderr(name: String, stderr: tokio::process::ChildStderr) {
    let mut lines = BufReader::new(stderr).lines();
    loop {
        let line = match lines.next_line().await {
            Ok(Some(line)) => line,
            Ok(None) | Err(_) => break,
        };
        let line = truncate(&line, STDERR_LINE_MAX);
        let (level, text) = match line.split_once(' ') {
            Some(("ERROR" | "CRITICAL", rest)) => (tracing::Level::ERROR, rest),
            Some(("WARNING", rest)) => (tracing::Level::WARN, rest),
            Some(("DEBUG", rest)) => (tracing::Level::DEBUG, rest),
            Some(("INFO", rest)) => (tracing::Level::INFO, rest),
            _ => (tracing::Level::INFO, line),
        };
        log_line(&name, level, text);
    }
}

fn log_line(name: &str, level: tracing::Level, text: &str) {
    match level {
        tracing::Level::ERROR => tracing::error!(plugin = %name, "{text}"),
        tracing::Level::WARN => tracing::warn!(plugin = %name, "{text}"),
        tracing::Level::DEBUG | tracing::Level::TRACE => tracing::debug!(plugin = %name, "{text}"),
        tracing::Level::INFO => tracing::info!(plugin = %name, "{text}"),
    }
}

fn truncate(text: &str, max: usize) -> &str {
    if text.len() <= max {
        return text;
    }
    let mut end = max;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

/// Read one `\n`-terminated line of at most `max` bytes. `Ok(false)` at
/// end of stream; an over-long line is an error.
async fn read_capped_line<R: tokio::io::AsyncBufRead + Unpin>(
    reader: &mut R,
    buf: &mut Vec<u8>,
    max: usize,
) -> std::io::Result<bool> {
    buf.clear();
    loop {
        let available = reader.fill_buf().await?;
        if available.is_empty() {
            return Ok(!buf.is_empty());
        }
        let (chunk, found) = match available.iter().position(|byte| *byte == b'\n') {
            Some(index) => (&available[..index], Some(index + 1)),
            None => (available, None),
        };
        if buf.len() + chunk.len() > max {
            return Err(std::io::Error::other(format!(
                "plugin sent a message over {max} bytes"
            )));
        }
        buf.extend_from_slice(chunk);
        let used = found.unwrap_or(available.len());
        reader.consume(used);
        if found.is_some() {
            return Ok(true);
        }
    }
}

/// Read the child's stdout: route responses to waiting calls, answer the
/// plugin's own requests. Returns why reading stopped.
async fn read_output(
    name: String,
    stdout: tokio::process::ChildStdout,
    session: Arc<Session>,
    services: Arc<dyn HostServices>,
) -> String {
    let mut reader = BufReader::new(stdout);
    let mut buf = Vec::new();
    let limit = Arc::new(Semaphore::new(HOST_REQUESTS_IN_FLIGHT));
    loop {
        match read_capped_line(&mut reader, &mut buf, protocol::MAX_MESSAGE_BYTES).await {
            Ok(true) => {}
            Ok(false) => return "plugin closed its output".to_owned(),
            Err(error) => return error.to_string(),
        }
        if buf.iter().all(u8::is_ascii_whitespace) {
            continue;
        }
        let message = match protocol::parse_line(&buf) {
            Ok(message) => message,
            Err(reason) => {
                // Usually a stray print() to stdout. Say so plainly; the
                // line itself goes to the log, cut short.
                let text = String::from_utf8_lossy(&buf);
                tracing::warn!(
                    plugin = %name,
                    %reason,
                    line = %truncate(&text, 200),
                    "plugin wrote something that is not a protocol message to stdout"
                );
                continue;
            }
        };
        match message {
            Incoming::Response { id, outcome } => {
                let RpcId::Num(id) = id else {
                    continue;
                };
                let waiter = session
                    .pending
                    .lock()
                    .ok()
                    .and_then(|mut pending| pending.remove(&id));
                if let Some(waiter) = waiter {
                    let _ = waiter.send(outcome);
                }
            }
            Incoming::Notification { method, params } => {
                if method == methods::HOST_LOG {
                    let level = match params.get("level").and_then(Value::as_str) {
                        Some("error") => tracing::Level::ERROR,
                        Some("warning") => tracing::Level::WARN,
                        Some("debug") => tracing::Level::DEBUG,
                        _ => tracing::Level::INFO,
                    };
                    let text = params.get("message").and_then(Value::as_str).unwrap_or("");
                    log_line(&name, level, truncate(text, STDERR_LINE_MAX));
                }
            }
            Incoming::Request { id, method, params } => {
                let Ok(permit) = Arc::clone(&limit).try_acquire_owned() else {
                    session.send(protocol::response_line(
                        &id,
                        &Err(RpcError::new(
                            codes::HOST_REFUSED,
                            "too many host requests in flight",
                        )),
                    ));
                    continue;
                };
                let session = Arc::clone(&session);
                let services = Arc::clone(&services);
                let name = name.clone();
                tokio::spawn(async move {
                    let outcome = match tokio::time::timeout(
                        HOST_REQUEST_TIMEOUT,
                        services.handle(&name, &method, params),
                    )
                    .await
                    {
                        Ok(outcome) => outcome,
                        Err(_) => Err(RpcError::new(
                            codes::INTERNAL_ERROR,
                            "host request timed out",
                        )),
                    };
                    session.send(protocol::response_line(&id, &outcome));
                    drop(permit);
                });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn capped_reader_splits_lines_and_refuses_floods() {
        let input: &[u8] = b"{\"a\":1}\n{\"b\":2}\npartial";
        let mut reader = BufReader::new(input);
        let mut buf = Vec::new();
        assert!(read_capped_line(&mut reader, &mut buf, 64).await.unwrap());
        assert_eq!(buf, b"{\"a\":1}");
        assert!(read_capped_line(&mut reader, &mut buf, 64).await.unwrap());
        assert_eq!(buf, b"{\"b\":2}");
        assert!(read_capped_line(&mut reader, &mut buf, 64).await.unwrap());
        assert_eq!(buf, b"partial");
        assert!(!read_capped_line(&mut reader, &mut buf, 64).await.unwrap());

        let flood = vec![b'x'; 100];
        let mut reader = BufReader::new(flood.as_slice());
        assert!(read_capped_line(&mut reader, &mut buf, 64).await.is_err());
    }

    #[test]
    fn command_defaults_to_the_python_helper_and_keeps_custom_ones_inside() {
        let mut manifest = PluginManifest {
            entrypoint: "plugin:Toy".to_owned(),
            ..PluginManifest::default()
        };
        let dir = Path::new("/plugins/toy");
        let python = plugin_command(&manifest, dir, "python3").unwrap();
        assert_eq!(python.program, "python3");
        assert_eq!(python.args, ["-m", "droppedneedle_plugin", "plugin:Toy"]);

        manifest.command = vec!["./bin/toy".to_owned(), "--serve".to_owned()];
        let custom = plugin_command(&manifest, dir, "python3").unwrap();
        assert_eq!(custom.program, "/plugins/toy/bin/toy");
        manifest.command = vec!["./../escape".to_owned()];
        assert!(plugin_command(&manifest, dir, "python3").is_err());
    }
}
