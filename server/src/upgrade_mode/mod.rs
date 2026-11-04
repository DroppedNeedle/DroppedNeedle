//! Upgrade mode: what a server started on v2 data serves until the upgrade
//! is done.
//!
//! Boot ([`crate::bootstrap::claim`]) sends a v2 folder, or one an upgrade
//! left halfway, here instead of refusing. Nothing of v3 opens: no
//! database, settings, key, platform or loop, and nothing is written into
//! the data folders. The web UI is stamped into a private folder under the
//! temporary folder and removed when the mode ends.
//!
//! The mode serves the web UI's files, `/health` (status `upgrade`) and the
//! upgrade routes in [`routes`], with their own sign-in ([`sign_in`]). Every
//! other `/api/v3` route answers 503 `UPGRADE_REQUIRED`, and the Subsonic
//! and Jellyfin paths their protocol's failure.
//!
//! The upgrade itself is `droppedneedle-tool upgrade`, run as a child
//! process ([`runner`]), one run at a time. The mode holds only the server
//! lock (`DataLock::server_only`), which keeps a second server off the
//! folder and leaves the tool free to take the folder's exclusive lock.
//! Continue, once a run has left v3 data, takes the shared lock as well
//! and hands both to the normal boot, which then runs in this process on
//! the same listening socket.

pub mod routes;
mod runner;
pub mod sign_in;
pub(crate) mod sql;

use std::{
    net::SocketAddr,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use axum::{
    Router,
    extract::{Request, State},
    http::StatusCode,
    middleware::{self, Next},
    response::{IntoResponse as _, Response},
};
use futures_util::future::{BoxFuture, Shared};
use serde::Serialize;
use tokio::{net::TcpListener, sync::Notify, time::Instant};
use utoipa::ToSchema;

use droppedneedle_auth::session::{
    extract::Transport,
    middleware::effective_origin,
    one_time_code::OneTimeCode,
    origin::{OriginDecision, check_origin},
    rate_limit::{RateLimiter, rate_limit},
    tokens,
};
use droppedneedle_compat::shared::path_case::is_compat_path;
use droppedneedle_kernel::{
    config::{AppConfig, TrustedProxies},
    db::datalock::DataLock,
    error::{ApiError, Unavailable},
    ids::UuidGenerator,
    jobs::CancellationToken,
    mount::{Postures, Routes, routes as route},
};

use crate::{
    app::under_base,
    bootstrap::{
        BootError, ServeError, Stopped,
        data_dirs::{DataDirs, Layout, detect},
        upgrade_progress::{
            CheckResult, Code, Event, FailedLine, Finished, FoundLayout, StepId, V2Files,
        },
    },
    handlers::{self, HealthStatus},
    middleware::{Boundary, request_scope},
    security_headers::{ContentSecurity, content_security},
    web::{WebError, WebUi},
};

pub use runner::ToolCommand;
use runner::{ChildEnv, Ending, Task};
use sign_in::SignIn;

/// Where the upgrade stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum UpgradeLayout {
    /// The folders hold v2 data.
    NeedsUpgrade,
    /// An upgrade started here and did not finish: finish it or go back
    /// to v2.
    Interrupted,
    /// The folders hold v3 data, or none: Continue opens the app.
    Finished,
}

/// What a run in the progress read does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum UpgradeTask {
    /// The upgrade, or finishing an interrupted one.
    Upgrade,
    /// Start fresh: v2's files go into the backup and nothing comes across.
    Fresh,
    /// Back to v2.
    Undo,
}

/// The latest run of the upgrade, a fresh start or an undo.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct UpgradeRun {
    /// What the run does.
    pub task: UpgradeTask,
    /// True until it ends.
    pub running: bool,
    /// The step under way, or the last one.
    pub step: Option<StepId>,
    /// The step's label.
    pub label: Option<String>,
    /// Parts of the step done, when it counts them.
    pub done: Option<u64>,
    /// Parts the step has, when it knows.
    pub total: Option<u64>,
    /// How the run finished, once it has.
    pub result: Option<Finished>,
    /// Why it failed, once it has.
    pub failure: Option<FailedLine>,
}

/// Why upgrade mode ended.
pub enum Ended {
    /// The stop signal came: the process stops, holding `stopped.data_lock`.
    Stopped(Stopped),
    /// Continue was accepted: the normal boot runs on `listener`, under
    /// `lock`, which holds both of a server's locks.
    Continued {
        /// The same listening socket.
        listener: TcpListener,
        /// The server lock and the shared lock.
        lock: DataLock,
        /// The sign-in code not yet used. After a fresh start the app's
        /// first-admin setup asks for it, so the log keeps one code.
        code: OneTimeCode,
    },
}

/// The runs' shared state.
struct Runs {
    layout: UpgradeLayout,
    /// A run holds the one place for runs.
    busy: bool,
    /// Continue was accepted: no run starts again.
    continuing: bool,
    /// The latest check of v2 data, kept for start to hold the
    /// acknowledgements to.
    preflight: Option<CheckResult>,
    /// The latest upgrade, fresh start or undo.
    last: Option<UpgradeRun>,
}

/// Upgrade mode's state, shared by its routes. Cheap to clone.
#[derive(Clone)]
pub struct UpgradeMode(Arc<Inner>);

struct Inner {
    dirs: DataDirs,
    base_path: String,
    trusted_proxies: TrustedProxies,
    limits: Arc<RateLimiter>,
    sign_in: SignIn,
    tool: ToolCommand,
    env: ChildEnv,
    runs: Mutex<Runs>,
    /// The shared lock Continue took, for the normal boot.
    continued: Mutex<Option<DataLock>>,
    continue_now: Notify,
    /// Stops a running child when the mode ends.
    stop: CancellationToken,
}

/// Why a run could not start.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refused {
    /// Another run is under way, or Continue was accepted.
    Busy,
}

impl UpgradeMode {
    /// Open upgrade mode for `config`'s folders: read the v2 admins'
    /// passwords and mint the first code. `interrupted` says an upgrade
    /// left the folders halfway.
    pub async fn open(
        config: &AppConfig,
        interrupted: bool,
        tool: ToolCommand,
    ) -> Result<Self, BootError> {
        let dirs = DataDirs::from_config(config);
        let read = dirs.clone();
        let sign_in = tokio::task::spawn_blocking(move || SignIn::open(&read, interrupted))
            .await
            .map_err(|error| BootError::Stage {
                stage: "upgrade sign-in",
                message: error.to_string(),
            })?
            .map_err(|_| BootError::Stage {
                stage: "upgrade sign-in",
                message: "the system random source failed".to_owned(),
            })?;
        Ok(Self(Arc::new(Inner {
            dirs,
            base_path: config.base_path.clone(),
            trusted_proxies: config.trusted_proxies.clone(),
            // Upgrade mode runs before the platform opens, so its limiter
            // reads the system clock itself.
            limits: Arc::new(
                RateLimiter::new(Arc::new(droppedneedle_kernel::clock::SystemClock))
                    .with_trusted_proxies(config.trusted_proxies.clone()),
            ),
            sign_in,
            tool,
            env: ChildEnv::for_config(config),
            runs: Mutex::new(Runs {
                layout: if interrupted {
                    UpgradeLayout::Interrupted
                } else {
                    UpgradeLayout::NeedsUpgrade
                },
                busy: false,
                continuing: false,
                preflight: None,
                last: None,
            }),
            continued: Mutex::new(None),
            continue_now: Notify::new(),
            stop: CancellationToken::new(),
        })))
    }

    /// The router upgrade mode serves, `web` as the web UI.
    pub fn router(&self, web: Option<WebUi>) -> Router {
        let boundary = Boundary::new(
            Arc::new(UuidGenerator),
            |path, head, query, status, retry_after| {
                droppedneedle_compat::http::server_error_response(
                    path,
                    head,
                    query,
                    status,
                    retry_after,
                    &droppedneedle_compat::subsonic::Settings::default(),
                )
            },
        );
        let security = ContentSecurity::fixed();
        let Postures {
            public,
            upgrade_session,
            ..
        } = routes::routes().with_state(self.clone()).into_postures();
        let api = Router::from(public)
            .merge(Router::from(upgrade_session))
            .route_layer(middleware::from_fn_with_state(self.clone(), same_origin))
            .route_layer(middleware::from_fn_with_state(
                Arc::clone(&self.0.limits),
                rate_limit,
            ));
        let health = Routes::new()
            .public(route!(handlers::health))
            .with_state::<()>(HealthStatus::Upgrade)
            .into_postures()
            .public;
        let app = api
            .merge(Router::from(health))
            .fallback(move |request: Request| fallback(web.clone(), request))
            .method_not_allowed_fallback(handlers::fallback_405)
            .layer(middleware::from_fn_with_state(
                boundary.clone(),
                request_scope,
            ))
            .layer(middleware::from_fn_with_state(
                security.clone(),
                content_security,
            ));
        under_base(&self.0.base_path, app, boundary, security)
    }

    /// Where the upgrade stands.
    pub fn layout(&self) -> UpgradeLayout {
        lock(&self.0.runs).layout
    }

    /// The latest upgrade, fresh start or undo.
    pub fn last_run(&self) -> Option<UpgradeRun> {
        lock(&self.0.runs).last.clone()
    }

    /// The sign-in side.
    pub fn sign_in(&self) -> &SignIn {
        &self.0.sign_in
    }

    pub(crate) fn limits(&self) -> &RateLimiter {
        &self.0.limits
    }

    pub(crate) fn base_path(&self) -> &str {
        &self.0.base_path
    }

    pub(crate) fn trusted_proxies(&self) -> &TrustedProxies {
        &self.0.trusted_proxies
    }

    /// Run the check and wait for it. The latest one that found v2 data is
    /// kept for start.
    pub(crate) async fn check(&self) -> Result<Ending, Refused> {
        self.claim(None)?;
        let (sent, ended) = tokio::sync::oneshot::channel();
        let mode = self.clone();
        // On its own task, so a caller that stops waiting neither kills the
        // check nor leaves the place for runs taken.
        tokio::spawn(async move {
            let held = Held::check(&mode);
            let ending = mode.run(Task::Check).await;
            let mut runs = lock(&mode.0.runs);
            runs.busy = false;
            runs.preflight = match &ending {
                Ending::Checked(check) if check.layout == FoundLayout::V2 => Some(check.clone()),
                _ => None,
            };
            drop(runs);
            held.ended();
            if sent.send(ending).is_err() {
                tracing::debug!("the pre-flight's caller stopped waiting");
            }
        });
        Ok(ended
            .await
            .unwrap_or_else(|_| runner::stopped_without_line("the check was lost")))
    }

    /// The check start holds its acknowledgements to, if any.
    pub(crate) fn preflight(&self) -> Option<CheckResult> {
        lock(&self.0.runs).preflight.clone()
    }

    /// Start `task` in the background. Its progress shows in
    /// [`UpgradeMode::last_run`].
    pub(crate) fn start(&self, task: Task) -> Result<UpgradeRun, Refused> {
        let kind = match task {
            Task::Upgrade { .. } => UpgradeTask::Upgrade,
            Task::Fresh => UpgradeTask::Fresh,
            Task::Undo => UpgradeTask::Undo,
            Task::Check => return Err(Refused::Busy),
        };
        let run = UpgradeRun {
            task: kind,
            running: true,
            step: None,
            label: None,
            done: None,
            total: None,
            result: None,
            failure: None,
        };
        self.claim(Some(run.clone()))?;
        let mode = self.clone();
        // The run outlives the request that started it; it ends on its own,
        // or when the mode stops, and records how either way.
        tokio::spawn(async move {
            let held = Held::run(&mode);
            let ending = mode.run(task).await;
            mode.finish(ending).await;
            held.ended();
        });
        Ok(run)
    }

    /// Take the one place for runs, recording `run` as the latest.
    fn claim(&self, run: Option<UpgradeRun>) -> Result<(), Refused> {
        let mut runs = lock(&self.0.runs);
        if runs.busy || runs.continuing {
            return Err(Refused::Busy);
        }
        runs.busy = true;
        if run.is_some() {
            runs.last = run;
        }
        Ok(())
    }

    async fn run(&self, task: Task) -> Ending {
        let mode = self.clone();
        let progress = move |event: Event| mode.moved(event);
        runner::run(&self.0.tool, &self.0.env, task, &progress, &self.0.stop).await
    }

    /// A `step` or `progress` line of the running upgrade, fresh start or
    /// undo. A check's lines are not shown.
    fn moved(&self, event: Event) {
        let mut runs = lock(&self.0.runs);
        let Some(run) = runs.last.as_mut().filter(|run| run.running) else {
            return;
        };
        match event {
            Event::Step(step) => {
                run.step = Some(step.step);
                run.label = Some(step.label);
                run.total = step.total;
                run.done = step.total.map(|_| 0);
            }
            Event::Progress(moved) if run.step == Some(moved.step) => run.done = Some(moved.done),
            _ => {}
        }
    }

    /// Record how a run ended and look at the folders again.
    async fn finish(&self, ending: Ending) {
        let dirs = self.0.dirs.clone();
        let (layout, backup) =
            tokio::task::spawn_blocking(move || (detect(&dirs), dirs.holds_backup()))
                .await
                .unwrap_or((Layout::Interrupted, true));
        let mut runs = lock(&self.0.runs);
        runs.busy = false;
        runs.preflight = None;
        runs.layout = match layout {
            Layout::V2 => UpgradeLayout::NeedsUpgrade,
            Layout::V3 | Layout::Empty => UpgradeLayout::Finished,
            Layout::Interrupted => UpgradeLayout::Interrupted,
            Layout::Unknown(reason) => {
                tracing::error!(%reason, "after the upgrade tool ran, the data folders read as neither v2 nor v3");
                UpgradeLayout::Interrupted
            }
        };
        let in_backup = runs.layout != UpgradeLayout::NeedsUpgrade && backup;
        let Some(run) = runs.last.as_mut() else {
            return;
        };
        run.running = false;
        match ending {
            Ending::Finished(result) => run.result = Some(result),
            Ending::Failed(mut failure) => {
                if failure.code == Code::Internal {
                    failure.v2_files = if in_backup {
                        V2Files::InBackup
                    } else {
                        V2Files::Untouched
                    };
                }
                tracing::warn!(code = ?failure.code, "the upgrade tool's run failed");
                run.failure = Some(failure);
            }
            Ending::Checked(_) => {}
        }
    }

    /// Accept Continue: no run is under way, the folders hold v3 data (or
    /// none), and the shared lock is free. Its lock is kept for the boot.
    pub(crate) async fn accept_continue(&self) -> Result<(), ContinueRefused> {
        {
            let mut runs = lock(&self.0.runs);
            if runs.busy || runs.continuing {
                return Err(ContinueRefused::Busy);
            }
            runs.continuing = true;
        }
        let accepted = self.try_continue().await;
        if accepted.is_err() {
            lock(&self.0.runs).continuing = false;
        }
        accepted
    }

    async fn try_continue(&self) -> Result<(), ContinueRefused> {
        let dirs = self.0.dirs.clone();
        let layout = tokio::task::spawn_blocking(move || detect(&dirs))
            .await
            .map_err(|_| ContinueRefused::NotV3)?;
        if !matches!(layout, Layout::V3 | Layout::Empty) {
            return Err(ContinueRefused::NotV3);
        }
        let shared = DataLock::shared(&self.0.dirs.db_path).map_err(|error| {
            tracing::warn!(%error, "Continue: the data folders are in use");
            ContinueRefused::Locked
        })?;
        *lock(&self.0.continued) = Some(shared);
        self.0.continue_now.notify_one();
        Ok(())
    }
}

/// The place for runs, held by a run's task. A task that ends without
/// recording how (a panic) gives the place back when this drops, so a
/// later run can start, and marks its run failed.
struct Held {
    mode: Option<UpgradeMode>,
    /// The run shows in the progress read (not a check).
    shown: bool,
}

impl Held {
    fn check(mode: &UpgradeMode) -> Self {
        Self {
            mode: Some(mode.clone()),
            shown: false,
        }
    }

    fn run(mode: &UpgradeMode) -> Self {
        Self {
            mode: Some(mode.clone()),
            shown: true,
        }
    }

    /// The run recorded how it ended.
    fn ended(mut self) {
        self.mode = None;
    }
}

impl Drop for Held {
    fn drop(&mut self) {
        let Some(mode) = self.mode.take() else {
            return;
        };
        tracing::error!("an upgrade tool run ended without recording how");
        let mut runs = lock(&mode.0.runs);
        runs.busy = false;
        if self.shown
            && let Some(run) = runs.last.as_mut().filter(|run| run.running)
        {
            run.running = false;
            if let Ending::Failed(failure) = runner::stopped_without_line("its task stopped") {
                run.failure = Some(failure);
            }
        }
    }
}

/// Why Continue was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContinueRefused {
    /// A run is under way.
    Busy,
    /// The folders do not hold v3 data yet.
    NotV3,
    /// Another program holds the data lock.
    Locked,
}

/// The origin check on every unsafe request: the upgrade cookie is only
/// ever sent by the page this server serves.
async fn same_origin(State(mode): State<UpgradeMode>, request: Request, next: Next) -> Response {
    let target = effective_origin(&request, mode.trusted_proxies());
    let decision = check_origin(
        request.headers(),
        target.as_ref(),
        &[],
        request.method(),
        Transport::Cookie,
    );
    if decision == OriginDecision::Deny {
        return ApiError::forbidden("Origin not allowed").into_response();
    }
    next.run(request).await
}

/// Everything no route takes: the web UI for a page, 503 for the app's API
/// and the compat APIs, else the 404 envelope.
async fn fallback(web: Option<WebUi>, request: Request) -> Response {
    let path = request.uri().path();
    if is_compat_path(path) {
        // The boundary turns it into the protocol's failure.
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    }
    if path == "/api/v3" || path.starts_with("/api/v3/") {
        return ApiError::from(Unavailable::UpgradeRequired).into_response();
    }
    if let Some(web) = &web
        && let Some(page) = web.respond(request.method(), path, request.headers()).await
    {
        return page;
    }
    handlers::fallback_404().await.into_response()
}

/// Serve upgrade mode on `listener` until the stop signal or an accepted
/// Continue. `lock` is the server lock alone.
pub async fn serve(
    listener: TcpListener,
    config: &AppConfig,
    lock: DataLock,
    interrupted: bool,
    tool: ToolCommand,
    signal: Shared<BoxFuture<'static, ()>>,
) -> Result<Ended, BootError> {
    let stage = |error: std::io::Error| BootError::Stage {
        stage: "upgrade listener",
        message: error.to_string(),
    };
    // A second handle on the socket outlives this server, so the app takes
    // over the same port and connections arriving meanwhile wait in its
    // backlog.
    let listening = listener.into_std().map_err(stage)?;
    let spare = listening.try_clone().map_err(stage)?;
    let listener = TcpListener::from_std(listening).map_err(stage)?;
    let mode = UpgradeMode::open(config, interrupted, tool).await?;
    let web = PrivateWeb::prepare(config).await?;
    tracing::warn!(
        "this data folder holds DroppedNeedle v2 data: serving only the upgrade page until it \
         is upgraded"
    );
    mode.sign_in().log_code();
    let router = mode.router(web.as_ref().map(|web| web.ui.clone()));
    let stop = CancellationToken::new();
    let server = axum::serve(
        listener,
        router.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(stop.clone().cancelled_owned());
    let mut server = tokio::spawn(std::future::IntoFuture::into_future(server));
    let (continued, ended_early) = tokio::select! {
        () = signal => (false, None),
        () = mode.0.continue_now.notified() => (true, None),
        outcome = &mut server => (false, Some(outcome)),
    };
    let deadline = Instant::now() + config.shutdown_grace;
    stop.cancel();
    mode.0.stop.cancel();
    let served = match ended_early {
        Some(outcome) => outcome,
        None => match tokio::time::timeout_at(deadline, &mut server).await {
            Ok(outcome) => outcome,
            Err(_) => {
                tracing::warn!("connections still open at the shutdown bound; closing");
                server.abort();
                Ok(Ok(()))
            }
        },
    };
    if let Some(web) = web {
        web.remove().await;
    }
    let outcome = match served {
        Ok(Ok(())) => Ok(()),
        Ok(Err(error)) => Err(ServeError::Io(error)),
        Err(error) => Err(ServeError::Task(error.to_string())),
    };
    let shared = lock_take(&mode.0.continued);
    match (continued, shared, outcome) {
        (true, Some(shared), Ok(())) => {
            tracing::info!("the upgrade is done; starting the app");
            spare.set_nonblocking(true).map_err(stage)?;
            Ok(Ended::Continued {
                listener: TcpListener::from_std(spare).map_err(stage)?,
                lock: lock.joined(shared),
                code: mode.sign_in().current_code(),
            })
        }
        (_, _, outcome) => Ok(Ended::Stopped(Stopped {
            outcome,
            left: deadline.saturating_duration_since(Instant::now()),
            data_lock: lock,
        })),
    }
}

/// The web UI, stamped into a folder of upgrade mode's own under the
/// temporary folder, never into the data folders. The folder is new, named
/// at random and open to this user alone, so nobody else on the host can
/// place a page in it.
struct PrivateWeb {
    folder: PathBuf,
    ui: WebUi,
}

impl PrivateWeb {
    /// `None`, creating nothing, when no web UI build is installed. A
    /// folder left half-prepared is removed.
    async fn prepare(config: &AppConfig) -> Result<Option<Self>, BootError> {
        let template = config.static_dir.clone();
        if !template.join("index.html").is_file() {
            return Ok(None);
        }
        let temp_dir = config.temp_dir.clone();
        let base_path = config.base_path.clone();
        let failed = |message: String| BootError::Stage {
            stage: "web UI",
            message,
        };
        let suffix = tokens::mint_token()
            .map(|token| tokens::hash_token(&token))
            .map_err(|error| failed(error.to_string()))?;
        let name = format!("droppedneedle-upgrade-ui-{}", &suffix[..16]);
        let prepared = tokio::task::spawn_blocking(move || {
            let folder = private_folder(&temp_dir, &name).map_err(|source| WebError::Io {
                path: temp_dir.join(&name),
                source,
            })?;
            match WebUi::prepare(&template, &folder.join("static"), &base_path) {
                Ok(Some(ui)) => Ok(Some(Self { folder, ui })),
                other => {
                    if let Err(error) = std::fs::remove_dir_all(&folder) {
                        tracing::warn!(folder = %folder.display(), %error, "cannot remove the upgrade page's files");
                    }
                    other.map(|_| None)
                }
            }
        })
        .await
        .map_err(|error| failed(error.to_string()))??;
        Ok(prepared)
    }

    async fn remove(self) {
        let folder = self.folder;
        let removed = tokio::task::spawn_blocking(move || {
            std::fs::remove_dir_all(&folder).map_err(|error| (folder, error))
        })
        .await;
        if let Ok(Err((folder, error))) = removed {
            tracing::warn!(folder = %folder.display(), %error, "cannot remove the upgrade page's files");
        }
    }
}

/// A new folder `name` in `parent`, open to this user alone. One already
/// there (a folder, a file or a link) is refused, never used.
fn private_folder(parent: &Path, name: &str) -> std::io::Result<PathBuf> {
    let folder = parent.join(name);
    let mut builder = std::fs::DirBuilder::new();
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
    builder.create(&folder)?;
    Ok(folder)
}

/// How long a session cookie lasts, in seconds.
pub(crate) fn session_max_age() -> i64 {
    i64::try_from(sign_in::SESSION_LIFETIME.as_secs()).unwrap_or(i64::MAX)
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn lock_take<T>(mutex: &Mutex<Option<T>>) -> Option<T> {
    lock(mutex).take()
}

#[cfg(test)]
mod tests {
    use super::*;
    use droppedneedle_kernel::test_support::ScratchDir;

    /// The upgrade page is never served from a folder someone placed
    /// first: one already there under the name, or a link, is refused, and
    /// the folder made is open to this user alone.
    #[test]
    fn a_folder_already_there_is_never_used() {
        let scratch = ScratchDir::new("upgrade-ui-folder").unwrap();
        std::fs::create_dir(scratch.join("taken")).unwrap();
        std::os::unix::fs::symlink(scratch.join("elsewhere"), scratch.join("linked")).unwrap();
        for name in ["taken", "linked"] {
            let refused = private_folder(&scratch, name).unwrap_err();
            assert_eq!(refused.kind(), std::io::ErrorKind::AlreadyExists, "{name}");
        }
        assert!(!scratch.join("elsewhere").exists());
        let made = private_folder(&scratch, "new").unwrap();
        let mode = std::os::unix::fs::PermissionsExt::mode(&made.metadata().unwrap().permissions());
        assert_eq!(mode & 0o077, 0, "{mode:o}");
    }
}
