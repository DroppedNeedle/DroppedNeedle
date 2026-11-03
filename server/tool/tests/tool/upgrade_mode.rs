//! The upgrade run from the upgrade page, with the real tool as the
//! server's child: a server started on v2 data serves upgrade mode, an
//! admin signs in with a v2 password, runs the pre-flight and the upgrade
//! (or a fresh start, or an undo), and Continue opens the app in the same
//! process on the same port.

use std::{
    net::SocketAddr,
    time::{Duration, Instant},
};

use droppedneedle::{bootstrap, upgrade_mode::ToolCommand};
use droppedneedle_kernel::{
    config::AppConfig,
    db::datalock::{DataLock, DataLockError},
};
use droppedneedle_tool::tooling::upgrade::{Fault, UpgradeError};
use serde_json::{Value, json};
use tokio::{
    io::{AsyncReadExt as _, AsyncWriteExt as _},
    net::{TcpListener, TcpStream},
    sync::oneshot,
};
use tracing::instrument::WithSubscriber as _;

use crate::common::{PATIENCE, closed_outbound, logs};
use crate::tooling_cli::{
    backup_trees, clean_v2, data_trees, scratch_dir, tool, upgrade_with_fault,
};

/// Run `journey` against a server started through the production entry
/// point on `config`, the real tool beside it, then stop the server. The
/// server runs on the test's own task, beside the journey, as `main` runs
/// it.
async fn with_server<F: Future<Output = ()>>(config: AppConfig, journey: impl FnOnce(Server) -> F) {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("binds");
    let server = Server {
        address: listener.local_addr().expect("address"),
    };
    let (stop, signal) = oneshot::channel::<()>();
    let running = bootstrap::run(
        listener,
        config,
        closed_outbound(),
        ToolCommand::new(tool(), Vec::new()),
        async {
            let _ = signal.await;
        },
    );
    let driven = async {
        server.health("upgrade").await;
        journey(server).await;
        let _ = stop.send(());
    };
    let (stopped, ()) = tokio::time::timeout(PATIENCE * 2, async { tokio::join!(running, driven) })
        .await
        .expect("the journey and the shutdown end");
    let stopped = stopped.expect("the server ran");
    assert!(stopped.outcome.is_ok(), "{:?}", stopped.outcome);
}

/// A client of the server under test.
#[derive(Clone, Copy)]
struct Server {
    address: SocketAddr,
}

impl Server {
    /// Wait until `/health` says `status`.
    async fn health(&self, status: &str) {
        let deadline = Instant::now() + PATIENCE;
        loop {
            if let Ok((200, _, body)) = self.try_call("GET", "/health", None, None).await
                && body["status"] == status
            {
                return;
            }
            assert!(Instant::now() < deadline, "/health never said {status}");
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    async fn call(
        &self,
        method: &str,
        path: &str,
        cookie: Option<&str>,
        body: Option<Value>,
    ) -> (u16, Vec<String>, Value) {
        self.try_call(method, path, cookie, body)
            .await
            .expect("the server answers")
    }

    /// One request over TCP from the page itself, read to the end.
    async fn try_call(
        &self,
        method: &str,
        path: &str,
        cookie: Option<&str>,
        body: Option<Value>,
    ) -> std::io::Result<(u16, Vec<String>, Value)> {
        let mut stream = TcpStream::connect(self.address).await?;
        let body = body.map(|body| body.to_string()).unwrap_or_default();
        let mut request = format!(
            "{method} {path} HTTP/1.1\r\nHost: {}\r\nConnection: close\r\n\
             Sec-Fetch-Site: same-origin\r\nContent-Type: application/json\r\n\
             Content-Length: {}\r\n",
            self.address,
            body.len()
        );
        if let Some(cookie) = cookie {
            request.push_str(&format!("Cookie: {cookie}\r\n"));
        }
        request.push_str("\r\n");
        request.push_str(&body);
        stream.write_all(request.as_bytes()).await?;
        let mut answer = Vec::new();
        stream.read_to_end(&mut answer).await?;
        let text = String::from_utf8_lossy(&answer).into_owned();
        let (head, payload) = text
            .split_once("\r\n\r\n")
            .ok_or_else(|| std::io::Error::other(format!("no header end: {text:?}")))?;
        let mut lines = head.lines();
        let status = lines
            .next()
            .and_then(|line| line.split_whitespace().nth(1))
            .and_then(|code| code.parse().ok())
            .ok_or_else(|| std::io::Error::other(format!("no status: {head:?}")))?;
        let headers: Vec<String> = lines.map(str::to_owned).collect();
        let chunked = headers
            .iter()
            .any(|line| line.eq_ignore_ascii_case("transfer-encoding: chunked"));
        let payload = if chunked {
            dechunk(payload)
        } else {
            payload.to_owned()
        };
        let value = serde_json::from_str(&payload).unwrap_or(Value::String(payload));
        Ok((status, headers, value))
    }

    async fn sign_in(&self) -> String {
        let (status, headers, body) = self
            .call(
                "POST",
                "/api/v3/upgrade/sign-in",
                None,
                Some(
                    json!({"method": "password", "username": "alice", "password": "alice-v2-pass"}),
                ),
            )
            .await;
        assert_eq!(status, 200, "{body}");
        headers
            .iter()
            .filter_map(|line| line.split_once(": "))
            .filter(|(name, _)| name.eq_ignore_ascii_case("set-cookie"))
            .map(|(_, value)| value)
            .find(|value| value.starts_with("droppedneedle_upgrade="))
            .and_then(|value| value.split(';').next())
            .expect("the upgrade cookie")
            .to_owned()
    }

    /// Read the progress until the latest run has ended.
    async fn ended_run(&self, cookie: &str) -> Value {
        let deadline = Instant::now() + PATIENCE;
        loop {
            let (status, _, body) = self
                .call("GET", "/api/v3/upgrade/progress", Some(cookie), None)
                .await;
            assert_eq!(status, 200, "{body}");
            if body["run"]["running"] == json!(false) {
                return body;
            }
            assert!(Instant::now() < deadline, "the run never ended: {body}");
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    /// Continue, and wait for the app to answer in upgrade mode's place.
    async fn continue_to_the_app(&self, cookie: &str) {
        let (status, _, body) = self
            .call("POST", "/api/v3/upgrade/continue", Some(cookie), None)
            .await;
        assert_eq!(status, 202, "{body}");
        self.health("ok").await;
    }
}

/// A chunked body, joined.
fn dechunk(mut rest: &str) -> String {
    let mut body = String::new();
    while let Some((size, after)) = rest.split_once("\r\n") {
        let Ok(size) = usize::from_str_radix(size.trim(), 16) else {
            break;
        };
        if size == 0 || after.len() < size {
            break;
        }
        body.push_str(&after[..size]);
        rest = after[size..].trim_start_matches("\r\n");
    }
    body
}

/// The whole journey on a copy of the v2 fixture: the app's API answers
/// 503 `UPGRADE_REQUIRED`, Alice signs in with her v2 password, the
/// pre-flight leaves the v2 folders as they were, the upgrade runs to its
/// result, and Continue opens the app, where Alice signs in to v3 with the
/// same password.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_upgrade_page_upgrades_and_continues_into_the_app() {
    let scratch = scratch_dir("upgrade-page");
    let root = scratch.join("data");
    clean_v2(&root);
    let before = data_trees(&root);
    with_server(AppConfig::with_root(0, &root), |server| async move {
        let (status, _, body) = server
            .call("GET", "/api/v3/library/albums", None, None)
            .await;
        assert_eq!(status, 503, "{body}");
        assert_eq!(body["error"]["code"], "UPGRADE_REQUIRED");
        let cookie = server.sign_in().await;
        let (status, _, body) = server
            .call("POST", "/api/v3/upgrade/preflight", Some(&cookie), None)
            .await;
        assert_eq!(status, 200, "{body}");
        assert_eq!(body["check"]["layout"], "v2", "{body}");
        assert_eq!(body["check"]["blockers"], json!([]), "{body}");
        assert_eq!(
            data_trees(&root),
            before,
            "upgrade mode and its pre-flight leave the v2 folders as they were"
        );

        let (status, _, body) = server
            .call(
                "POST",
                "/api/v3/upgrade/start",
                Some(&cookie),
                Some(json!({"acknowledged": []})),
            )
            .await;
        assert_eq!(status, 202, "{body}");
        let ended = server.ended_run(&cookie).await;
        assert_eq!(ended["run"]["result"]["outcome"], "upgraded", "{ended}");
        assert_eq!(ended["layout"], "finished", "{ended}");

        let config = AppConfig::with_root(0, &root);
        let tool_run = DataLock::exclusive(&config.library_db_path).expect("the tool can run");
        let (status, _, body) = server
            .call("POST", "/api/v3/upgrade/continue", Some(&cookie), None)
            .await;
        assert_eq!(status, 409, "Continue waits while the tool holds the folder: {body}");
        drop(tool_run);
        server.continue_to_the_app(&cookie).await;
        assert!(
            DataLock::exclusive(&config.library_db_path).is_err(),
            "the app holds the shared lock"
        );
        assert!(
            matches!(
                bootstrap::claim(&config).await,
                Err(bootstrap::BootError::DataLock(DataLockError::Held { .. }))
            ),
            "a second server is refused"
        );
        let (status, _, body) = server
            .call(
                "POST",
                "/api/v3/auth/login",
                None,
                Some(json!({"username": "alice", "password": "alice-v2-pass", "transport": "bearer"})),
            )
            .await;
        assert_eq!(status, 200, "Alice signs in to v3: {body}");
        let (status, _, _) = server
            .call("GET", "/api/v3/upgrade/state", Some(&cookie), None)
            .await;
        assert_eq!(status, 404, "the app does not serve the upgrade routes");
    })
    .await;
}

/// A folder an upgrade left halfway opens the page as interrupted, and
/// going back puts v2 back exactly as it was.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_interrupted_upgrade_opens_the_page_and_undo_puts_v2_back() {
    let scratch = scratch_dir("upgrade-page-interrupted");
    let root = scratch.join("data");
    clean_v2(&root);
    let before = data_trees(&root);
    let crashed = upgrade_with_fault(&root, Fault::DiesWhileMoving).await;
    assert!(
        matches!(crashed, Err(UpgradeError::InjectedCrash)),
        "{crashed:?}"
    );
    with_server(AppConfig::with_root(0, &root), |server| async move {
        let (_, _, state) = server
            .call("GET", "/api/v3/upgrade/state", None, None)
            .await;
        assert_eq!(state, json!({"layout": "interrupted", "signed_in": false}));
        let cookie = server.sign_in().await;
        let (status, _, body) = server
            .call("POST", "/api/v3/upgrade/undo", Some(&cookie), None)
            .await;
        assert_eq!(status, 202, "{body}");
        let ended = server.ended_run(&cookie).await;
        assert_eq!(ended["run"]["result"]["outcome"], "undone", "{ended}");
        assert_eq!(ended["layout"], "needs_upgrade", "{ended}");
        assert_eq!(data_trees(&root), before, "v2 is back as it was");
    })
    .await;
}

/// The code a log line beginning with `what` gives, from the last such
/// line.
fn logged_code(text: &[u8], what: &str) -> String {
    String::from_utf8_lossy(text)
        .lines()
        .rev()
        .filter(|line| line.contains(what))
        .find_map(|line| line.split("code=").nth(1))
        .and_then(|code| code.split_whitespace().next())
        .unwrap_or_else(|| panic!("no {what} is logged"))
        .to_owned()
}

/// Start fresh moves v2's files into the backup and imports nothing;
/// Continue then opens an empty app that asks for its first admin. Nothing
/// came across, so setup asks for the code upgrade mode logged, the one
/// the app logs again: without it, or with a wrong one, it is refused
/// alike, and with it the first admin is made.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn start_fresh_then_continue_opens_an_empty_app() {
    let scratch = scratch_dir("upgrade-page-fresh");
    let root = scratch.join("data");
    clean_v2(&root);
    let before = data_trees(&root);
    let (dispatch, logged) = logs::warnings();
    let read = logged.clone();
    with_server(AppConfig::with_root(0, &root), |server| async move {
        let cookie = server.sign_in().await;
        let (status, _, body) = server
            .call(
                "POST",
                "/api/v3/upgrade/fresh",
                Some(&cookie),
                Some(json!({"confirm": true})),
            )
            .await;
        assert_eq!(status, 202, "{body}");
        let ended = server.ended_run(&cookie).await;
        assert_eq!(
            ended["run"]["result"]["outcome"], "started_fresh",
            "{ended}"
        );
        assert_eq!(backup_trees(&root), before, "v2's files are in v2-backup");

        server.continue_to_the_app(&cookie).await;
        let (status, _, body) = server
            .call("GET", "/api/v3/auth/setup/status", None, None)
            .await;
        assert_eq!(status, 200, "{body}");
        assert_eq!(
            body,
            json!({"setup_required": true, "code_required": true}),
            "{body}"
        );

        let code = logged_code(&read.lock().unwrap(), "upgrade sign-in code");
        assert_eq!(
            logged_code(&read.lock().unwrap(), "first-admin setup code"),
            code,
            "the app logs the code upgrade mode logged"
        );
        let wrong = format!(
            "{}{}",
            &code[..code.len() - 1],
            if code.ends_with('X') { 'Y' } else { 'X' }
        );
        let setup = |code: Option<&str>| {
            let mut body = json!({
                "username": "owner",
                "password": "a long fresh password",
                "display_name": "Owner",
            });
            if let Some(code) = code {
                body["code"] = json!(code);
            }
            body
        };
        let (status, _, missing) = server
            .call("POST", "/api/v3/auth/setup", None, Some(setup(None)))
            .await;
        assert_eq!(status, 401, "{missing}");
        let (status, _, refused) = server
            .call(
                "POST",
                "/api/v3/auth/setup",
                None,
                Some(setup(Some(&wrong))),
            )
            .await;
        assert_eq!(status, 401, "{refused}");
        assert_eq!(
            missing["error"], refused["error"],
            "a missing code and a wrong one are refused alike"
        );
        let (status, _, body) = server
            .call("POST", "/api/v3/auth/setup", None, Some(setup(Some(&code))))
            .await;
        assert_eq!(status, 201, "{body}");
    })
    .with_subscriber(dispatch)
    .await;
}
