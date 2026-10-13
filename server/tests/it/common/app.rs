//! The whole app for a journey: the production builder on a scratch data
//! directory, served and stopped the way `main` does it.

use std::{
    net::SocketAddr,
    time::{Duration, Instant},
};

use axum::{
    Router,
    body::Body,
    extract::ConnectInfo,
    http::{Request, StatusCode},
};
use droppedneedle::{
    AppConfig,
    acquire::{
        slskd::mock::MockSlskd,
        usenet::mocks::{SabnzbdMock, serve_loopback},
    },
    bootstrap,
};
use serde_json::{Value, json};
use tokio::{net::TcpListener, sync::oneshot, task::JoinHandle};
use tower::ServiceExt as _;

use super::{ScratchDir, closed_outbound, migrated_db};

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
/// port, and no download client is configured: a journey that wants one
/// saves the address of `slskd` or `sabnzbd` in Settings, as an admin
/// would. The data directory is `config.root_app_dir`. The
/// background loops run as they do in production, so a journey waits on
/// something it can observe (a row, a response, a call a mock recorded).
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
    router: Option<Router>,
    admin: tokio::sync::OnceCell<String>,
    stop: Option<oneshot::Sender<()>>,
    server: Option<JoinHandle<Result<(), bootstrap::ServeError>>>,
    sabnzbd_server: JoinHandle<()>,
    // Last, so the directory goes after everything that writes to it.
    _dir: ScratchDir,
}

impl TestApp {
    /// Build and serve the app in a new scratch directory named after `tag`.
    pub async fn start(tag: &str) -> Self {
        let dir = ScratchDir::new(tag);
        let config = AppConfig::with_root(0, &dir);
        std::fs::create_dir_all(&config.cache_dir).expect("cache dir creates");
        migrated_db(&config.library_db_path);

        let slskd = MockSlskd::start().await.expect("mock slskd starts");
        let sabnzbd = SabnzbdMock::new();
        let (sabnzbd_url, sabnzbd_server) = serve_loopback(sabnzbd.router())
            .await
            .expect("mock SABnzbd starts");

        // The state goes at once: workers that stop when their queue's last
        // handle drops would otherwise sit out the shutdown grace.
        let bootstrap::Built {
            router, background, ..
        } = bootstrap::build_with(config.clone(), closed_outbound())
            .await
            .expect("the app builds");
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("loopback binds");
        let (stop, signal) = oneshot::channel::<()>();
        let server = tokio::spawn(bootstrap::serve(
            listener,
            router.clone(),
            background,
            async {
                let _ = signal.await;
            },
        ));
        Self {
            config,
            slskd,
            sabnzbd,
            sabnzbd_url,
            router: Some(router),
            admin: tokio::sync::OnceCell::new(),
            stop: Some(stop),
            server: Some(server),
            sabnzbd_server,
            _dir: dir,
        }
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

    /// One JSON request. An empty answer reads as `Value::Null`.
    pub async fn call(
        &self,
        method: &str,
        uri: &str,
        headers: &[(&str, &str)],
        body: Option<Value>,
    ) -> (StatusCode, Value) {
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
        let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
            .await
            .expect("body reads");
        let json = if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes).expect("body is json")
        };
        (status, json)
    }

    /// The owner's `authorization` header value. The first call runs
    /// first-time setup.
    pub async fn login_admin(&self) -> String {
        self.admin
            .get_or_init(|| async {
                let (status, body) = self
                    .call(
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
            .call(
                "POST",
                "/api/v3/admin/users",
                &[("authorization", admin.as_str())],
                Some(json!({"username": username, "password": password})),
            )
            .await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
        let user_id = body["id"].as_str().expect("user id").to_owned();
        let (status, body) = self
            .call(
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

    /// Shut down as production does and wait for it: loops joined, the
    /// database closed, the listener gone. Fails if shutdown had to wait
    /// out its grace period.
    pub async fn stop(mut self) {
        let grace = self.config.shutdown_grace;
        self.router = None;
        let started = Instant::now();
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        let server = self.server.take().expect("the app is running");
        let outcome = tokio::time::timeout(grace + Duration::from_secs(20), server)
            .await
            .expect("shutdown is bounded")
            .expect("the server task joins");
        assert!(outcome.is_ok(), "{outcome:?}");
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
