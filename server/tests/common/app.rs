//! The whole app for a journey: the production builder on a scratch data
//! directory, served and stopped the way `main` does it.

use std::{
    future::Future,
    net::SocketAddr,
    path::Path,
    time::{Duration, Instant},
};

use axum::{
    Router,
    body::Body,
    extract::ConnectInfo,
    http::{HeaderMap, Request, StatusCode},
};
use droppedneedle::{AppState, bootstrap, middleware::request_scope};
use droppedneedle_kernel::config::AppConfig;
use droppedneedle_library::scan::models::FailureCode;
use droppedneedle_testkit::{
    remotes::{MockServer, serve_plex},
    slskd::MockSlskd,
    usenet::{SabnzbdMock, serve_loopback},
};
use serde_json::{Value, json};
use tokio::{net::TcpListener, sync::oneshot, task::JoinHandle};
use tower::ServiceExt as _;

use super::{ScratchDir, closed_outbound, migrated_db, through_busy};

/// The app's shutdown grace in tests. On a loaded host the database close
/// alone has taken half a minute, so the production ten seconds would fail
/// a shutdown that is slow but finishing. Something that holds the app
/// still waits out the whole grace, which [`TestApp::stop`] refuses.
const SHUTDOWN_GRACE: Duration = Duration::from_secs(60);

const ADMIN_NAME: &str = "admin";
const ADMIN_PASSWORD: &str = "correct horse battery";

/// A signed-in account.
pub struct Login {
    /// The account's id.
    pub user_id: String,
    /// The `authorization` header value that signs its requests.
    pub authorization: String,
}

/// The production app on a fresh data directory.
///
/// Every outside service with an address fixed in code sits on a closed
/// port, and no download client or media server is configured: a journey
/// that wants one saves the address of `slskd`, `sabnzbd` or `plex` in
/// Settings, as an admin would. The data directory is
/// `config.root_app_dir`. The background loops run as they do in
/// production, so a journey waits on something it can observe (a row, a
/// response, a call a mock recorded).
///
/// End a journey with [`TestApp::stop`]. Dropping the app without it still
/// tells the loops to stop and removes the directory.
pub struct TestApp {
    /// Deployment config, with every path under the scratch directory.
    pub config: AppConfig,
    /// The mock slskd. Nothing calls it until Settings point at it.
    // No journey configures a download client yet.
    #[allow(dead_code)]
    pub slskd: MockSlskd,
    /// The mock SABnzbd behind `sabnzbd_url`.
    #[allow(dead_code)]
    pub sabnzbd: SabnzbdMock,
    /// Where the mock SABnzbd listens.
    #[allow(dead_code)]
    pub sabnzbd_url: String,
    /// The mock Plex server, at `plex.base_url`.
    pub plex: MockServer,
    outbound: bootstrap::Outbound,
    router: Option<Router>,
    // Boxed, as is the build below: the state is about 10 KB, and every
    // journey future that holds this app keeps copies of it on the test
    // thread's 2 MiB stack in a debug build.
    state: Option<Box<AppState>>,
    admin: tokio::sync::OnceCell<String>,
    stop: Option<oneshot::Sender<()>>,
    server: Option<JoinHandle<bootstrap::Stopped>>,
    sabnzbd_server: JoinHandle<()>,
    // Last, so the directory goes after everything that writes to it.
    _dir: ScratchDir,
}

impl TestApp {
    /// Build and serve the app in a new scratch directory named after `tag`.
    pub async fn start(tag: &str) -> Self {
        Self::start_with(tag, closed_outbound()).await
    }

    /// As [`TestApp::start`], reaching the outside world as `outbound`
    /// says: a journey that scripts a provider port passes it here.
    pub async fn start_with(tag: &str, outbound: bootstrap::Outbound) -> Self {
        Self::launch(tag, outbound, |config| config, |_| {}).await
    }

    /// As [`TestApp::start_with`], with test hooks on: the jobs table runs
    /// on a manual clock.
    // Only some test binaries use it.
    #[allow(dead_code)]
    pub async fn start_hooked_with(tag: &str, outbound: bootstrap::Outbound) -> Self {
        Self::launch(tag, outbound, AppConfig::with_test_hooks, |_| {}).await
    }

    /// As [`TestApp::start`], with the deployment config passed through
    /// `configure` before the app builds: `AppConfig::with_test_hooks` mounts
    /// the `__test__` failure routes.
    pub async fn start_configured(
        tag: &str,
        configure: impl FnOnce(AppConfig) -> AppConfig,
    ) -> Self {
        Self::launch(tag, closed_outbound(), configure, |_| {}).await
    }

    /// As [`TestApp::start`], with `place` run on the data directory before
    /// the first boot, after the migrated database is copied in: a test
    /// puts its own database, key file or `config.json` there.
    pub async fn start_prepared(tag: &str, place: impl FnOnce(&AppConfig)) -> Self {
        Self::launch(tag, closed_outbound(), |config| config, place).await
    }

    async fn launch(
        tag: &str,
        outbound: bootstrap::Outbound,
        configure: impl FnOnce(AppConfig) -> AppConfig,
        place: impl FnOnce(&AppConfig),
    ) -> Self {
        let dir = ScratchDir::new(tag);
        let mut config = AppConfig::with_root(0, &dir);
        config.shutdown_grace = SHUTDOWN_GRACE;
        let config = configure(config);
        std::fs::create_dir_all(&config.cache_dir).expect("cache dir creates");
        std::fs::create_dir_all(config.config_dir()).expect("config dir creates");
        // A database that holds data comes with its key, as the server's
        // first start leaves them.
        migrated_db(&config.library_db_path);
        droppedneedle_kernel::runtime_config::Crypto::load_or_generate(&config.config_dir())
            .expect("key file writes");
        place(&config);

        let slskd = MockSlskd::start().await.expect("mock slskd starts");
        let sabnzbd = SabnzbdMock::new();
        let (sabnzbd_url, sabnzbd_server) = serve_loopback(sabnzbd.router())
            .await
            .expect("mock SABnzbd starts");
        let plex = serve_plex().await.expect("mock Plex starts");

        let mut app = Self {
            config,
            slskd,
            sabnzbd,
            sabnzbd_url,
            plex,
            outbound,
            router: None,
            state: None,
            admin: tokio::sync::OnceCell::new(),
            stop: None,
            server: None,
            sabnzbd_server,
            _dir: dir,
        };
        app.boot().await;
        app
    }

    /// Build the app on the data directory and serve it.
    async fn boot(&mut self) {
        // `halt` drops the state before the stop signal, so no handle on
        // the app outlives the shutdown.
        let bootstrap::Built {
            router,
            state,
            background,
        } = Box::pin(bootstrap::build_with(
            self.config.clone(),
            self.outbound.clone(),
        ))
        .await
        .expect("the app builds");
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("loopback binds");
        let (stop, signal) = oneshot::channel::<()>();
        self.server = Some(tokio::spawn(bootstrap::serve(
            listener,
            router.clone(),
            background,
            async {
                let _ = signal.await;
            },
        )));
        self.router = Some(router);
        self.state = Some(Box::new(state));
        self.stop = Some(stop);
    }

    /// Stop the app as [`TestApp::stop`] does, then build it again on the
    /// same data directory. Sessions are in the database, so a login from
    /// before still signs requests.
    pub async fn restart(mut self) -> Self {
        self.halt().await;
        self.boot().await;
        self
    }

    /// The running app's state, for a test that reaches a bundle directly.
    // Only some test binaries use it.
    #[allow(dead_code)]
    pub fn state(&self) -> &AppState {
        self.state.as_deref().expect("the app is running")
    }

    /// Stop the app as [`TestApp::restart`] does, run `look` on its stopped
    /// data directory, then build it again on the same directory.
    #[allow(dead_code)]
    pub async fn while_stopped<T>(mut self, look: impl FnOnce(&AppConfig) -> T) -> (Self, T) {
        self.halt().await;
        let seen = look(&self.config);
        self.boot().await;
        (self, seen)
    }

    /// `router` under this app's request wrapper: request ids from its
    /// generator and the 5xx boundary, for a test that serves its own
    /// routes. Drop the router before [`TestApp::stop`].
    pub fn request_scope(&self, router: Router) -> Router {
        let state = self.state.as_deref().cloned().expect("the app is running");
        router.layer(axum::middleware::from_fn_with_state(state, request_scope))
    }

    /// Answer one request as a client on the same host would get it.
    async fn request(&self, mut request: Request<Body>) -> axum::response::Response {
        let peer = SocketAddr::from(([127, 0, 0, 1], 40_000));
        request.extensions_mut().insert(ConnectInfo(peer));
        self.router
            .clone()
            .expect("the app is running")
            .oneshot(request)
            .await
            .expect("the app answers")
    }

    /// One request, answered as it came: status, headers and body bytes.
    pub async fn raw(
        &self,
        method: &str,
        uri: &str,
        headers: &[(&str, &str)],
        body: Option<Value>,
    ) -> (StatusCode, HeaderMap, Vec<u8>) {
        let mut builder = Request::builder()
            .method(method)
            .uri(uri)
            .header("host", "test.local");
        for (name, value) in headers {
            builder = builder.header(*name, *value);
        }
        let body = match body {
            Some(value) => {
                builder = builder.header("content-type", "application/json");
                Body::from(value.to_string())
            }
            None => Body::empty(),
        };
        let response = self
            .request(builder.body(body).expect("request builds"))
            .await;
        let status = response.status();
        let headers = response.headers().clone();
        let bytes = axum::body::to_bytes(response.into_body(), 64 * 1024 * 1024)
            .await
            .expect("body reads");
        (status, headers, bytes.to_vec())
    }

    /// One request with `body` sent as it is, under whatever content type
    /// `headers` names. Answered as [`TestApp::raw`] answers.
    pub async fn raw_bytes(
        &self,
        method: &str,
        uri: &str,
        headers: &[(&str, &str)],
        body: Vec<u8>,
    ) -> (StatusCode, HeaderMap, Vec<u8>) {
        let mut builder = Request::builder()
            .method(method)
            .uri(uri)
            .header("host", "test.local");
        for (name, value) in headers {
            builder = builder.header(*name, *value);
        }
        let response = self
            .request(builder.body(Body::from(body)).expect("request builds"))
            .await;
        let status = response.status();
        let headers = response.headers().clone();
        let bytes = axum::body::to_bytes(response.into_body(), 64 * 1024 * 1024)
            .await
            .expect("body reads");
        (status, headers, bytes.to_vec())
    }

    /// One JSON request. An empty answer reads as `Value::Null`.
    pub async fn call(
        &self,
        method: &str,
        uri: &str,
        headers: &[(&str, &str)],
        body: Option<Value>,
    ) -> (StatusCode, Value) {
        let (status, _, bytes) = self.raw(method, uri, headers, body).await;
        let json = if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes).expect("body is json")
        };
        (status, json)
    }

    /// [`TestApp::call`], asked again while the app answers busy: see
    /// [`super::through_busy`].
    pub async fn call_through_busy(
        &self,
        method: &str,
        uri: &str,
        headers: &[(&str, &str)],
        body: Option<Value>,
    ) -> (StatusCode, Value) {
        let (status, _, bytes) =
            through_busy(|| self.raw(method, uri, headers, body.clone())).await;
        (
            status,
            serde_json::from_slice(&bytes).expect("body is json"),
        )
    }

    /// Wait for something the app does in its own time. `check` is asked
    /// again and again until it answers; `what` names the wait if it never
    /// does.
    pub async fn until<T, F>(&self, what: &str, mut check: impl FnMut() -> F) -> T
    where
        F: Future<Output = Option<T>>,
    {
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            if let Some(found) = check().await {
                return found;
            }
            assert!(Instant::now() < deadline, "never happened: {what}");
            // Slower than the request limiter refills, so a check may ask
            // the app each time round.
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    /// Add a library folder as the owner and wait for the scan the app gives
    /// every new folder to end and settle. Returns that scan's run id.
    pub async fn add_library_folder(&self, id: &str, path: &Path) -> String {
        let admin = self.login_admin().await;
        let headers = [("authorization", admin.as_str())];
        let before = self.finished_scans(&headers).await;
        let (status, body) = self
            .call_through_busy(
                "POST",
                "/api/v3/library/roots",
                &headers,
                Some(json!({"id": id, "path": path.to_string_lossy()})),
            )
            .await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
        let run = self
            .until("the new folder's first scan", || async {
                let (status, body) = self
                    .call("GET", "/api/v3/library/scan/runs", &headers, None)
                    .await;
                assert_eq!(status, StatusCode::OK, "{body}");
                if body["current"]
                    .as_array()
                    .is_none_or(|runs| !runs.is_empty())
                {
                    return None;
                }
                body["history"]
                    .as_array()?
                    .iter()
                    .filter_map(|run| run["id"].as_str())
                    .find(|run| !before.iter().any(|seen| seen == run))
                    .map(str::to_owned)
            })
            .await;
        self.scan_ended(&run).await;
        run
    }

    /// Ask for a scan of every folder as the owner and wait for it to end.
    /// Returns the run id.
    pub async fn scan_library(&self) -> String {
        let admin = self.login_admin().await;
        let headers = [("authorization", admin.as_str())];
        let (status, body) = self
            .call("POST", "/api/v3/library/scan", &headers, Some(json!({})))
            .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let run = body["run_id"].as_str().expect("run id").to_owned();
        self.scan_ended(&run).await;
        run
    }

    /// Wait for one scan run to end and to settle what it found. A run
    /// ends before its end passes (track continuation, artist merges and
    /// the rest) run in later turns, and the run's note that it still owes
    /// them is the only sign of that, so the wait reads it.
    pub async fn scan_ended(&self, run: &str) {
        let admin = self.login_admin().await;
        let headers = [("authorization", admin.as_str())];
        let uri = format!("/api/v3/library/scan/runs/{run}");
        self.until("the scan ends and settles", || async {
            let (status, body) = self.call("GET", &uri, &headers, None).await;
            assert_eq!(status, StatusCode::OK, "{body}");
            let state = body["run"]["state"].as_str()?;
            let ended = matches!(
                state,
                "completed" | "failed" | "cancelled" | "superseded_policy_changed"
            );
            let owing: bool = self
                .db()
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM library_scan_failures \
                     WHERE run_id = ?1 AND failure_code = ?2)",
                    [run, FailureCode::RunEndPending.as_str()],
                    |row| row.get(0),
                )
                .expect("end passes read");
            (ended && !owing).then_some(())
        })
        .await;
    }

    async fn finished_scans(&self, headers: &[(&str, &str)]) -> Vec<String> {
        let (status, body) = self
            .call("GET", "/api/v3/library/scan/runs", headers, None)
            .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        body["history"]
            .as_array()
            .expect("scan history")
            .iter()
            .filter_map(|run| run["id"].as_str().map(str::to_owned))
            .collect()
    }

    /// A connection to the app's database, for seeding rows and reading
    /// them back.
    pub fn db(&self) -> rusqlite::Connection {
        rusqlite::Connection::open(&self.config.library_db_path).expect("database opens")
    }

    /// The owner's `authorization` header value. The first call runs
    /// first-time setup.
    pub async fn login_admin(&self) -> String {
        self.admin
            .get_or_init(|| async {
                let (status, body) = self
                    .call_through_busy(
                        "POST",
                        "/api/v3/auth/setup",
                        &[],
                        Some(json!({
                            "username": ADMIN_NAME,
                            "password": ADMIN_PASSWORD,
                            "transport": "bearer",
                        })),
                    )
                    .await;
                assert_eq!(status, StatusCode::CREATED, "{body}");
                bearer(&body)
            })
            .await
            .clone()
    }

    /// Create a plain user as the owner and sign them in.
    pub async fn login_user(&self, username: &str) -> Login {
        let admin = self.login_admin().await;
        let password = format!("{username}-password-1234");
        let (status, body) = self
            .call_through_busy(
                "POST",
                "/api/v3/admin/users",
                &[("authorization", admin.as_str())],
                Some(json!({"username": username, "password": password})),
            )
            .await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
        let user_id = body["id"].as_str().expect("user id").to_owned();
        let (status, body) = self
            .call_through_busy(
                "POST",
                "/api/v3/auth/login",
                &[],
                Some(json!({
                    "username": username,
                    "password": password,
                    "transport": "bearer",
                })),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        Login {
            user_id,
            authorization: bearer(&body),
        }
    }

    /// Turn the Subsonic and Jellyfin APIs on or off in Settings, as the
    /// admin does.
    pub async fn set_compat(&self, subsonic: bool, jellyfin: bool) {
        let admin = self.login_admin().await;
        let admin = [("authorization", admin.as_str())];
        let (status, mut apps) = self
            .call("GET", "/api/v3/settings/connect-apps", &admin, None)
            .await;
        assert_eq!(status, StatusCode::OK, "{apps}");
        apps["subsonic_enabled"] = json!(subsonic);
        apps["jellyfin_enabled"] = json!(jellyfin);
        let (status, saved) = self
            .call("PUT", "/api/v3/settings/connect-apps", &admin, Some(apps))
            .await;
        assert_eq!(status, StatusCode::OK, "{saved}");
    }

    /// Shut down as production does and wait for it: loops joined, the
    /// database closed, the listener gone. Fails if shutdown had to wait
    /// out its grace period.
    pub async fn stop(mut self) {
        self.halt().await;
    }

    async fn halt(&mut self) {
        let grace = self.config.shutdown_grace;
        self.router = None;
        self.state = None;
        let started = Instant::now();
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        let server = self.server.take().expect("the app is running");
        let stopped = tokio::time::timeout(grace + Duration::from_secs(20), server)
            .await
            .expect("shutdown is bounded")
            .expect("the server task joins");
        assert!(stopped.outcome.is_ok(), "{:?}", stopped.outcome);
        assert!(
            started.elapsed() < grace,
            "shutdown waited out its grace: something still held the app"
        );
    }
}

impl Drop for TestApp {
    fn drop(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        self.sabnzbd_server.abort();
    }
}

fn bearer(body: &Value) -> String {
    format!("Bearer {}", body["token"].as_str().expect("session token"))
}
