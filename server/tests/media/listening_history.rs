//! A person's ListenBrainz history, uploaded as an export file, through
//! the production builder: each listen lands once, a second upload of the
//! same file adds none, a listen in the same second as one of the person's
//! plays here is that play and is not kept twice, no play or play count
//! changes, and the file is gone once read. A hostile archive gives only
//! its real month files, and a file that is not an export is refused.

use std::collections::VecDeque;
use std::io::Write as _;
use std::sync::{Arc, Mutex};

use droppedneedle_kernel::clock::Clock as _;

use axum::http::StatusCode;
use serde_json::Value;

use crate::common::TestApp;

/// The hand-made export: five listens over two months.
const EXPORT: &[u8] = include_bytes!("../fixtures/listenbrainz/history_export.zip");
/// The second of the export's fourth listen.
const PLAYED_HERE: i64 = 1_770_000_000;

async fn upload(app: &TestApp, auth: &str, file: &[u8]) -> (StatusCode, Value) {
    let boundary = "history-boundary";
    let mut form = format!(
        "--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; \
         filename=\"listenbrainz.zip\"\r\nContent-Type: application/zip\r\n\r\n"
    )
    .into_bytes();
    form.extend_from_slice(file);
    form.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
    let content_type = format!("multipart/form-data; boundary={boundary}");
    let (status, _, bytes) = app
        .raw_bytes(
            "POST",
            "/api/v3/me/listening-history/upload",
            &[
                ("authorization", auth),
                ("content-type", content_type.as_str()),
            ],
            form,
        )
        .await;
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

/// The caller's history once its import is current.
async fn imported(app: &TestApp, auth: &str) -> Value {
    app.until("the import is current", || async {
        let (status, body) = app
            .call(
                "GET",
                "/api/v3/me/listening-history",
                &[("authorization", auth)],
                None,
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_ne!(body["state"], "failed", "{body}");
        (body["state"] == "current").then_some(body)
    })
    .await
}

/// Every row of `sql`, one text per row.
fn rows(app: &TestApp, sql: &str) -> Vec<String> {
    let db = app.db();
    let mut statement = db.prepare(sql).expect("statement");
    statement
        .query_map([], |row| row.get::<_, String>(0))
        .expect("query")
        .collect::<Result<_, _>>()
        .expect("rows")
}

/// The files waiting to be read.
fn waiting(app: &TestApp) -> usize {
    std::fs::read_dir(app.state().media.history.dir()).map_or(0, |dir| dir.count())
}

/// A ZIP of `entries`, deflated, as an upload sends it.
fn archive(entries: &[(&str, &str)]) -> Vec<u8> {
    let mut writer = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    for (name, text) in entries {
        writer
            .start_file(*name, zip::write::SimpleFileOptions::default())
            .expect("entry starts");
        writer.write_all(text.as_bytes()).expect("entry writes");
    }
    writer.finish().expect("archive ends").into_inner()
}

/// Make the entry `name` of `zip` say it inflates to `size` bytes, in its
/// local header and its central directory record.
fn declare_size(zip: &mut [u8], name: &str, size: u32) {
    let mut patched = 0;
    for at in 0..zip.len().saturating_sub(46) {
        let (field, name_at) = match &zip[at..at + 4] {
            b"PK\x03\x04" => (at + 22, at + 30),
            b"PK\x01\x02" => (at + 24, at + 46),
            _ => continue,
        };
        if zip.get(name_at..name_at + name.len()) == Some(name.as_bytes()) {
            zip[field..field + 4].copy_from_slice(&size.to_le_bytes());
            patched += 1;
        }
    }
    assert_eq!(patched, 2, "{name} has a header and a directory record");
}

/// One listen line in the export's shape.
fn listen(at: i64, track: &str) -> String {
    format!(
        r#"{{"inserted_at": {at}, "listened_at": {at}.0, "track_metadata": {{"track_name": "{track}", "artist_name": "Someone", "release_name": "Somewhere", "additional_info": {{}}, "mbid_mapping": null}}}}"#
    )
}

#[tokio::test]
async fn an_uploaded_export_lands_each_listen_once_and_never_as_a_play() {
    let app = TestApp::start("listening-history").await;
    let ada = app.login_user("ada").await;
    let auth = ada.authorization.as_str();
    let (status, body) = app
        .call(
            "GET",
            "/api/v3/me/listening-history",
            &[("authorization", auth)],
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["state"], Value::Null, "nothing imported yet: {body}");
    assert_eq!(body["imported_listens"], 0);

    // One of the export's listens is a play here, half a second into it.
    app.seed(format!(
        "INSERT INTO library_play_history (id, user_id, track_name, artist_name, played_at) \
         VALUES ('play-1', '{id}', 'Glass Weather', 'Mara Vell & The Static', {at})",
        id = ada.user_id,
        at = PLAYED_HERE * 1000 + 500
    ))
    .await;
    let plays = |app: &TestApp| {
        (
            rows(
                app,
                "SELECT id || played_at FROM library_play_history ORDER BY id",
            ),
            rows(
                app,
                "SELECT user_id || local_album_id || plays FROM library_user_album_plays \
                 ORDER BY 1",
            ),
        )
    };
    let plays_before = plays(&app);
    let listens = |app: &TestApp| {
        rows(
            app,
            "SELECT listened_at || '|' || track_name || '|' || COALESCE(artist_mbid, '-') \
             || '|' || COALESCE(recording_mbid, '-') FROM media_imported_listens \
             ORDER BY listened_at",
        )
    };

    let (status, body) = upload(&app, auth, EXPORT).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    let history = imported(&app, auth).await;
    assert_eq!(history["imported_listens"], 4, "{history}");
    assert_eq!(history["plays"], 1, "{history}");
    assert_eq!(history["source"], "upload", "{history}");
    assert_eq!(history["oldest_listened_at"], 1_767_312_000, "{history}");
    assert_eq!(history["newest_listened_at"], 1_770_086_400, "{history}");
    let kept = listens(&app);
    assert_eq!(
        kept,
        [
            "1767312000000|Paper Lanterns|1f6d3a52-8c0b-4e7a-9a43-6b2f0d1e5c01|\
             5dab7e96-c04f-4cbe-9e87-af6d4b5c9a05",
            "1767398400000|Paper Lanterns|1f6d3a52-8c0b-4e7a-9a43-6b2f0d1e5c01|\
             5dab7e96-c04f-4cbe-9e87-af6d4b5c9a05",
            "1767484800000|Northbound|2a7e4b63-9d1c-4f8b-8b54-7c3a1e2f6d02|\
             a2f0cdeb-b59d-4b03-83dc-f4bc9aabef10",
            "1770086400000|Low Tide|1f6d3a52-8c0b-4e7a-9a43-6b2f0d1e5c01|\
             e6d40123-f9d1-4f47-871a-d8f0d4ef2d14",
        ],
        "each listen once, the one played here left out"
    );
    assert_eq!(plays(&app), plays_before, "no play or play count changes");
    assert_eq!(waiting(&app), 0, "the file is deleted once read");

    // A file no import names, left by a deleted account or a crash, goes at
    // the next import run once it is an hour old; a younger one, as an
    // upload still arriving would be, stays.
    let dir = app.state().media.history.dir().to_owned();
    let (old, young) = (dir.join("left-over.zip"), dir.join("arriving.zip"));
    for file in [&old, &young] {
        std::fs::write(file, b"PK").expect("file written");
    }
    std::fs::File::options()
        .write(true)
        .open(&old)
        .and_then(|file| {
            file.set_modified(std::time::SystemTime::now() - std::time::Duration::from_secs(7200))
        })
        .expect("dated two hours back");

    // The same file again adds nothing.
    let (status, body) = upload(&app, auth, EXPORT).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    let again = imported(&app, auth).await;
    assert_eq!(again["imported_listens"], 4, "{again}");
    assert_eq!(listens(&app), kept);
    assert_eq!(plays(&app), plays_before);
    assert!(!old.exists(), "the old file no import names is swept");
    assert!(young.exists(), "a young one stays");
    std::fs::remove_file(&young).expect("removed");
    assert_eq!(waiting(&app), 0);

    // A hostile archive: a traversal name, an entry that is not a month
    // file and one that says it inflates past the limit give nothing; its
    // one real month file is read.
    let mut hostile = archive(&[
        (
            "../listens/2026/3.jsonl",
            &listen(1_772_000_000, "Climbed Out"),
        ),
        (
            "listens/2026/../../4.jsonl",
            &listen(1_772_100_000, "Walked Up"),
        ),
        ("notes.txt", &listen(1_772_200_000, "Not A Month")),
        ("listens/2026/4.jsonl", &listen(1_772_300_000, "Too Big")),
        (
            "listens/2026/5.jsonl",
            &listen(1_772_400_000, "The Real One"),
        ),
    ]);
    declare_size(&mut hostile, "listens/2026/4.jsonl", 100 << 20);
    let (status, body) = upload(&app, auth, &hostile).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    let after = imported(&app, auth).await;
    assert_eq!(after["imported_listens"], 5, "{after}");
    assert_eq!(
        listens(&app).last().map(String::as_str),
        Some("1772400000000|The Real One|-|-")
    );
    assert_eq!(waiting(&app), 0);

    // A file that is not an export is refused and not kept.
    for (file, what) in [
        (b"not a zip at all".to_vec(), "not a ZIP"),
        (
            archive(&[("notes.txt", "hello")]),
            "a ZIP with no month file",
        ),
    ] {
        let (status, body) = upload(&app, auth, &file).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{what}: {body}");
        assert_eq!(
            body["error"]["message"], "That file is not a ListenBrainz export.",
            "{what}"
        );
        assert_eq!(waiting(&app), 0, "{what} leaves nothing behind");
    }
    let (_, last) = app
        .call(
            "GET",
            "/api/v3/me/listening-history",
            &[("authorization", auth)],
            None,
        )
        .await;
    assert_eq!(
        last["state"], "current",
        "a refused file changes nothing: {last}"
    );
    assert_eq!(plays(&app), plays_before);
}

// ---------------------------------------------------------------------------
// From ListenBrainz, with the person's own link
// ---------------------------------------------------------------------------

/// An export being made, ListenBrainz's own example.
const EXPORT_WAITING: &str = include_str!("../fixtures/listenbrainz/export_waiting.json");
/// That export, made.
const EXPORT_COMPLETED: &str = include_str!("../fixtures/listenbrainz/export_completed.json");
/// The 400 while an export is pending.
const ALREADY_ASKED: &str = include_str!("../fixtures/listenbrainz/export_already_requested.json");
/// Three listens, newest first, all after the export's.
const LISTENS: &str = include_str!("../fixtures/listenbrainz/listens_account.json");
/// The newest listen of the export, unix seconds.
const EXPORT_NEWEST: i64 = 1_770_086_400;

/// One request the mock ListenBrainz saw.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Seen {
    method: String,
    path: String,
    query: String,
    token: String,
}

/// What the mock ListenBrainz answers, each list in turn; once a list is
/// down to one answer, that one is kept.
#[derive(Default)]
struct Script {
    lists: VecDeque<String>,
    creates: VecDeque<(u16, String)>,
    statuses: VecDeque<String>,
    pages: VecDeque<(u16, String)>,
    seen: Vec<Seen>,
}

fn next<T: Clone>(queue: &mut VecDeque<T>, otherwise: T) -> T {
    match queue.len() {
        0 => otherwise,
        1 => queue[0].clone(),
        _ => queue.pop_front().unwrap_or(otherwise),
    }
}

/// ListenBrainz on loopback: its export, listens and token routes.
#[derive(Clone, Default)]
struct ListenBrainz(Arc<Mutex<Script>>);

impl ListenBrainz {
    fn script(&self) -> std::sync::MutexGuard<'_, Script> {
        self.0.lock().expect("lock")
    }

    /// Every request but the token checks, as `METHOD path`.
    fn calls(&self) -> Vec<String> {
        self.script()
            .seen
            .iter()
            .filter(|seen| seen.path != "/1/validate-token")
            .map(|seen| format!("{} {}", seen.method, seen.path))
            .collect()
    }

    fn seen(&self) -> Vec<Seen> {
        self.script().seen.clone()
    }

    async fn outbound(&self) -> droppedneedle::bootstrap::Outbound {
        let router = axum::Router::new()
            .fallback(answer)
            .with_state(self.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("binds");
        let origin = format!("http://{}", listener.local_addr().expect("addr"));
        tokio::spawn(async move {
            let _ = axum::serve(listener, router).await;
        });
        droppedneedle::bootstrap::Outbound {
            listenbrainz: origin,
            ..crate::common::closed_outbound()
        }
    }
}

async fn answer(
    axum::extract::State(lb): axum::extract::State<ListenBrainz>,
    request: axum::http::Request<axum::body::Body>,
) -> axum::response::Response {
    let token = request
        .headers()
        .get("authorization")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Token "))
        .unwrap_or("")
        .to_owned();
    let (method, path) = (
        request.method().to_string(),
        request.uri().path().to_owned(),
    );
    let query = request.uri().query().unwrap_or("").to_owned();
    let mut script = lb.script();
    script.seen.push(Seen {
        method: method.clone(),
        path: path.clone(),
        query,
        token: token.clone(),
    });
    let json = |status: u16, body: String| {
        axum::response::Response::builder()
            .status(status)
            .header("content-type", "application/json")
            .body(axum::body::Body::from(body))
            .expect("answer builds")
    };
    match (method.as_str(), path.as_str()) {
        ("GET", "/1/validate-token") => json(
            200,
            format!(
                r#"{{"code": 200, "message": "Token valid.", "valid": true, "user_name": "{token}"}}"#
            ),
        ),
        ("GET", "/1/export/list") => json(200, next(&mut script.lists, "[]".to_owned())),
        ("POST", "/1/export/") => {
            let (status, body) = next(&mut script.creates, (200, EXPORT_WAITING.to_owned()));
            json(status, body)
        }
        ("GET", path) if path.starts_with("/1/export/") && path.ends_with("/download") => {
            axum::response::Response::builder()
                .status(200)
                .header("content-type", "application/zip")
                .body(axum::body::Body::from(EXPORT))
                .expect("answer builds")
        }
        ("GET", path) if path.starts_with("/1/export/") => {
            json(200, next(&mut script.statuses, EXPORT_COMPLETED.to_owned()))
        }
        ("GET", path) if path.starts_with("/1/user/") && path.ends_with("/listens") => {
            let (status, body) = next(
                &mut script.pages,
                (
                    200,
                    r#"{"payload": {"count": 0, "listens": []}}"#.to_owned(),
                ),
            );
            json(status, body)
        }
        _ => json(404, r#"{"code": 404, "error": "Not found"}"#.to_owned()),
    }
}

/// The app over `lb`, with Discover's warm cycle off. Its pass, on a timer
/// of its own, queues taste builds that read a linked person's top artists
/// from ListenBrainz while their import is not current, which the exact
/// request lists here would catch at random.
async fn start(tag: &str, lb: &ListenBrainz) -> TestApp {
    TestApp::start_hooked_configured(tag, lb.outbound().await, |mut config| {
        config.discover_warmer = false;
        config
    })
    .await
}

/// `name`, signed in, with ListenBrainz linked as `name` with the token
/// `name`, as the link route stores it.
async fn linked(app: &TestApp, name: &str) -> crate::common::Login {
    let login = app.login_user(name).await;
    link(app, &login.user_id, name).await;
    login
}

/// Link `user_id`'s ListenBrainz as `name`, with the token `name`.
async fn link(app: &TestApp, user_id: &str, name: &str) {
    let (crypto, _) =
        droppedneedle_kernel::runtime_config::Crypto::load_or_generate(&app.config.config_dir())
            .expect("the app's key reads");
    let sealed = crypto
        .encrypt(&serde_json::json!({"user_token": name, "username": name}).to_string())
        .expect("link seals");
    app.seed_with(
        "INSERT INTO user_connections (user_id, service, connection_data, enabled, created_at, \
         updated_at) VALUES (?1, 'listenbrainz', ?2, 1, 0, 0)",
        [user_id, sealed.as_str()],
    )
    .await;
}

/// `name`, who uses only a music app (an app password used just now),
/// with ListenBrainz linked as `name`. Answers their id.
async fn app_listener(app: &TestApp, admin: &str, name: &str) -> String {
    let (status, body) = app
        .call(
            "POST",
            "/api/v3/admin/users",
            &[("authorization", admin)],
            Some(
                serde_json::json!({"username": name, "password": format!("{name}-password-1234")}),
            ),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let user_id = body["id"].as_str().expect("user id").to_owned();
    link(app, &user_id, name).await;
    let now = droppedneedle_kernel::clock::SystemClock.now_ms();
    app.seed(format!(
        "INSERT INTO auth_app_passwords (id, user_id, name, secret_sha256, secret_encrypted, \
         created_at, last_used_at) VALUES ('app-{name}', '{user_id}', 'Phone', 'sha-{name}', \
         'sealed', {now}, {now})"
    ))
    .await;
    user_id
}

/// `json` asked for just now, in place of the fixture's date.
fn fresh(json: &str) -> String {
    let now = time::OffsetDateTime::now_utc()
        .format(&time::format_description::well_known::Rfc3339)
        .expect("now formats");
    json.replace("2026-09-17T12:00:00+00:00", &now)
}

/// The caller's history now.
async fn history(app: &TestApp, auth: &str) -> Value {
    let (status, body) = app
        .call(
            "GET",
            "/api/v3/me/listening-history",
            &[("authorization", auth)],
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    body
}

/// The jobs table and its manual clock.
fn job_table(
    app: &TestApp,
) -> (
    droppedneedle_kernel::jobs::queue::JobQueue,
    droppedneedle::bootstrap::jobs::scripted::ScriptedJobs,
) {
    let jobs = &app.state().jobs;
    (
        jobs.queue().clone(),
        jobs.scripted().expect("test hooks are on").clone(),
    )
}

/// Wait until the person's next import step waits for its time, then let
/// that time come.
async fn next_step(app: &TestApp, user_id: &str, what: &str) {
    use droppedneedle_kernel::jobs::queue::State;
    let (queue, kit) = job_table(app);
    let waiting = app
        .until(what, || async {
            queue
                .latest(IMPORT, user_id)
                .await
                .expect("the jobs table reads")
                .filter(|row| {
                    row.state == State::Queued && row.not_before_at > kit.clock().now_ms()
                })
        })
        .await;
    kit.clock().set(waiting.not_before_at);
    queue.wake();
}

/// The person's import, as the row reads.
fn state(app: &TestApp, user_id: &str) -> String {
    app.db()
        .query_row(
            "SELECT state || '|' || COALESCE(account, '-') || '|' || COALESCE(export_id, '-') \
             FROM media_history_imports WHERE user_id = ?1",
            [user_id],
            |row| row.get(0),
        )
        .unwrap_or_else(|_| "none".to_owned())
}

/// The background reads counted today.
fn reads_counted(app: &TestApp) -> i64 {
    app.db()
        .query_row(
            "SELECT COALESCE(SUM(reads), 0) FROM catalog_background_read_days",
            [],
            |row| row.get(0),
        )
        .expect("the read count reads")
}

const IMPORT: droppedneedle_kernel::jobs::queue::JobKind =
    droppedneedle_media::history::import::KIND;
const SYNC: droppedneedle_kernel::jobs::queue::JobKind = droppedneedle_media::history::sync::KIND;

/// A person who links ListenBrainz gets their history at once through the
/// export: asked for, waited on, downloaded and read, each listen kept
/// once, every call with their own token and counted. Asked again after a
/// failure, while an export is already pending at ListenBrainz, that one
/// is taken up rather than asked for twice, and the same listens add none.
#[tokio::test]
async fn a_link_imports_the_history_through_an_export() {
    let lb = ListenBrainz::default();
    lb.script().statuses = [fresh(EXPORT_WAITING), fresh(EXPORT_COMPLETED)].into();
    let app = start("history-export", &lb).await;
    let ada = app.login_user("ada").await;
    let auth = ada.authorization.as_str();

    let (status, body) = app
        .call(
            "PUT",
            "/api/v3/me/connections/listenbrainz",
            &[("authorization", auth)],
            Some(serde_json::json!({"username": "ada", "user_token": "ada"})),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    next_step(&app, &ada.user_id, "the export is asked for").await;
    assert_eq!(lb.calls(), ["GET /1/export/list", "POST /1/export/"]);
    assert_eq!(state(&app, &ada.user_id), "exporting|ada|123");
    next_step(&app, &ada.user_id, "the export is still being made").await;
    let body = imported(&app, auth).await;
    assert_eq!(body["imported_listens"], 5, "{body}");
    assert_eq!(body["source"], "export", "{body}");
    assert_eq!(body["account"], "ada", "{body}");
    assert_eq!(body["newest_listened_at"], EXPORT_NEWEST, "{body}");
    assert_eq!(
        lb.calls(),
        [
            "GET /1/export/list",
            "POST /1/export/",
            "GET /1/export/123",
            "GET /1/export/123",
            "GET /1/export/123/download",
        ]
    );
    assert!(
        lb.seen().iter().all(|seen| seen.token == "ada"),
        "every call carries the person's own token"
    );
    assert_eq!(reads_counted(&app), 5, "each call counted against the day");
    assert_eq!(waiting(&app), 0, "the download is deleted once read");

    // Asked again after a failure, with an export already pending.
    app.seed(format!(
        "UPDATE media_history_imports SET state = 'failed' WHERE user_id = '{}'",
        ada.user_id
    ))
    .await;
    {
        let mut script = lb.script();
        script.seen.clear();
        script.lists = [
            "[]".to_owned(),
            format!("[{}]", fresh(EXPORT_WAITING).replace("123", "124")),
        ]
        .into();
        script.creates = [(400, ALREADY_ASKED.to_owned())].into();
        script.statuses = [fresh(EXPORT_COMPLETED).replace("123", "124")].into();
    }
    let (status, body) = app
        .call(
            "POST",
            "/api/v3/me/listening-history/import",
            &[("authorization", auth)],
            None,
        )
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    assert_eq!(body["imported_listens"], 5, "what is kept stays: {body}");
    next_step(&app, &ada.user_id, "the pending export is taken up").await;
    let body = imported(&app, auth).await;
    assert_eq!(
        body["imported_listens"], 5,
        "the same listens add none: {body}"
    );
    assert_eq!(
        lb.calls(),
        [
            "GET /1/export/list",
            "POST /1/export/",
            "GET /1/export/list",
            "GET /1/export/124",
            "GET /1/export/124/download",
        ],
        "one request, then the pending export is taken up"
    );
    let (status, _) = app
        .call(
            "POST",
            "/api/v3/me/listening-history/import",
            &[("authorization", auth)],
            None,
        )
        .await;
    assert_eq!(
        status,
        StatusCode::CONFLICT,
        "a kept history is not asked for again"
    );
    app.stop().await;
}

/// The six-hourly read asks for the listens since the newest kept, from
/// the second before it, and keeps them. With ListenBrainz reads off,
/// without a link, after a forget, or for a person unseen for a week,
/// nothing is queued and no request leaves.
#[tokio::test]
async fn the_read_starts_at_the_newest_listen_and_stays_quiet_when_it_should() {
    let lb = ListenBrainz::default();
    lb.script().pages = [(200, LISTENS.to_owned())].into();
    let app = start("history-sync", &lb).await;
    let history_store = app.state().media.history.clone();
    let (queue, _) = job_table(&app);
    let bo = linked(&app, "bowie").await;
    let (status, body) = upload(&app, bo.authorization.as_str(), EXPORT).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    imported(&app, bo.authorization.as_str()).await;
    let stale = |user: &str| {
        format!(
            "UPDATE media_history_imports SET last_synced_at = 1000, updated_at = 1000 \
             WHERE user_id = '{user}'"
        )
    };
    app.seed(stale(&bo.user_id)).await;
    lb.script().seen.clear();

    history_store.tick().await;
    let body = app
        .until("the read lands", || async {
            let body = history(&app, bo.authorization.as_str()).await;
            (body["imported_listens"] == 8).then_some(body)
        })
        .await;
    assert_eq!(
        body["account"], "bowie",
        "the read notes the account: {body}"
    );
    let reads: Vec<Seen> = lb.seen();
    assert_eq!(reads.len(), 1, "{reads:?}");
    assert_eq!(reads[0].path, "/1/user/bowie/listens");
    assert_eq!(
        reads[0].query,
        format!("min_ts={}&count=1000", EXPORT_NEWEST - 1),
        "from the newest listen kept"
    );
    assert_eq!(reads[0].token, "bowie");

    // None of these queues a read or sends anything.
    let quiet = |user: String| {
        let queue = queue.clone();
        async move {
            queue
                .latest(SYNC, &user)
                .await
                .expect("jobs read")
                .is_none()
        }
    };
    lb.script().seen.clear();
    // No link.
    let cy = app.login_user("cyril").await;
    history(&app, cy.authorization.as_str()).await;
    // Unseen for eight days.
    let di = linked(&app, "diane").await;
    app.seed(format!(
        "INSERT INTO media_history_imports (user_id, state, source, account, entries_read, \
         last_synced_at, updated_at) VALUES ('{}', 'current', 'export', 'diane', 0, 1000, 1000)",
        di.user_id
    ))
    .await;
    app.seed(format!(
        "UPDATE auth_tokens SET last_seen_at = {} WHERE user_id = '{}'",
        droppedneedle_kernel::clock::SystemClock.now_ms() - 8 * 86_400_000,
        di.user_id
    ))
    .await;
    // Forgotten.
    let ed = linked(&app, "edgar").await;
    app.seed(format!(
        "INSERT INTO media_history_imports (user_id, state, entries_read, updated_at) \
         VALUES ('{}', 'forgotten', 0, 0)",
        ed.user_id
    ))
    .await;
    history_store.tick().await;
    history(&app, ed.authorization.as_str()).await;
    for user in [&cy.user_id, &di.user_id, &ed.user_id] {
        assert!(quiet(user.clone()).await, "no read queued");
    }
    assert_eq!(state(&app, &ed.user_id), "forgotten|-|-", "a forget stays");

    // ListenBrainz reads off: bo's next read is due, and nothing goes.
    let admin = app.login_admin().await;
    let (status, body) = app
        .call(
            "PUT",
            "/api/v3/settings/listenbrainz",
            &[("authorization", admin.as_str())],
            Some(serde_json::json!({"reads_enabled": false})),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    app.seed(stale(&bo.user_id)).await;
    let before = queue.latest(SYNC, &bo.user_id).await.expect("jobs read");
    history_store.tick().await;
    history(&app, bo.authorization.as_str()).await;
    assert_eq!(
        queue.latest(SYNC, &bo.user_id).await.expect("jobs read"),
        before,
        "nothing queued while reads are off"
    );
    assert!(lb.seen().is_empty(), "no request left: {:?}", lb.seen());
    app.stop().await;
}

/// Ten linked people seen after an upgrade, none of whom opened Profile or
/// Discover, each get a first import step at a random time within two
/// days; none is asked for yet. One who opens their Profile starts at once,
/// in place of their waiting step.
#[tokio::test]
async fn first_imports_are_spread_and_a_visit_starts_one_at_once() {
    let lb = ListenBrainz::default();
    let app = start("history-spread", &lb).await;
    let (queue, kit) = job_table(&app);
    let admin = app.login_admin().await;
    let first = linked(&app, "listener0").await;
    let mut people = vec![(first.user_id.clone(), "listener0".to_owned())];
    for n in 1..10 {
        let name = format!("listener{n}");
        people.push((app_listener(&app, &admin, &name).await, name));
    }
    let now = kit.clock().now_ms();
    app.state().media.history.tick().await;
    let mut starts = Vec::new();
    for (user_id, name) in &people {
        assert_eq!(
            state(&app, user_id),
            "none",
            "{name}'s import starts when its step runs"
        );
        let job = queue
            .latest(IMPORT, user_id)
            .await
            .expect("jobs read")
            .expect("an import is queued");
        starts.push(job.not_before_at);
    }
    let two_days = 2 * 86_400_000;
    assert!(
        starts.iter().all(|at| (now..=now + two_days).contains(at)),
        "within two days: {starts:?}"
    );
    starts.sort_unstable();
    starts.dedup();
    assert_eq!(starts.len(), 10, "ten different times");
    assert!(
        starts[9] - starts[0] > 6 * 3_600_000,
        "spread out: {starts:?}"
    );
    assert!(lb.calls().is_empty(), "nothing asked for yet");

    // One opens their Profile: theirs starts now, the others still wait.
    history(&app, first.authorization.as_str()).await;
    app.until("the visitor's export is asked for", || async {
        (lb.calls().len() >= 2).then_some(())
    })
    .await;
    assert!(
        lb.seen().iter().all(|seen| seen.token == "listener0"),
        "only the visitor's: {:?}",
        lb.seen()
    );

    // The first waiting step's time comes: it starts that person's import
    // from their link and asks for their export.
    let mut waiting = Vec::new();
    for (user_id, name) in &people[1..] {
        let job = queue
            .latest(IMPORT, user_id)
            .await
            .expect("jobs read")
            .expect("a step waits");
        waiting.push((job.not_before_at, user_id.clone(), name.clone()));
    }
    waiting.sort();
    let (due, user_id, name) = waiting[0].clone();
    kit.clock().set(due);
    queue.wake();
    app.until("the step asks for that person's export", || async {
        lb.seen()
            .iter()
            .any(|seen| seen.method == "POST" && seen.token == name)
            .then_some(())
    })
    .await;
    app.until("the import waits on its export", || async {
        (state(&app, &user_id) == format!("exporting|{name}|123")).then_some(())
    })
    .await;
    app.stop().await;
}

/// A forget before any import starts is kept: the tick starts none, and
/// no step asks ListenBrainz for anything.
#[tokio::test]
async fn a_forget_before_the_first_import_keeps_it_from_starting() {
    let lb = ListenBrainz::default();
    let app = start("history-forget-first", &lb).await;
    let (queue, kit) = job_table(&app);
    let kim = linked(&app, "kimber").await;
    let (status, body) = app
        .call(
            "DELETE",
            "/api/v3/discover/history",
            &[("authorization", kim.authorization.as_str())],
            None,
        )
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
    assert_eq!(state(&app, &kim.user_id), "forgotten|-|-");

    let steps = || async { queue.latest(IMPORT, &kim.user_id).await.expect("jobs read") };
    app.state().media.history.tick().await;
    assert_eq!(steps().await, None, "the tick queues no first step");
    kit.clock().advance(2 * 86_400_000 + 60_000);
    queue.wake();
    app.state().media.history.tick().await;
    assert_eq!(steps().await, None, "nor two days later");
    assert!(
        !lb.seen().iter().any(|seen| seen.token == "kimber"),
        "nothing sent with the person's link: {:?}",
        lb.seen()
    );
    assert_eq!(state(&app, &kim.user_id), "forgotten|-|-");
    app.stop().await;
}

/// Linking another account than the one a history is kept from asks
/// first and stores nothing; on yes, the kept history goes and the new
/// account's export is asked for.
#[tokio::test]
async fn linking_another_account_asks_first_and_replaces_on_yes() {
    let lb = ListenBrainz::default();
    let app = start("history-replace", &lb).await;
    let fay = linked(&app, "fay").await;
    let auth = fay.authorization.as_str();
    let (status, body) = upload(&app, auth, EXPORT).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    imported(&app, auth).await;
    app.seed(format!(
        "UPDATE media_history_imports SET account = 'fay' WHERE user_id = '{}'",
        fay.user_id
    ))
    .await;
    lb.script().seen.clear();

    let link = |replace: bool| serde_json::json!({"username": "gus", "user_token": "gus", "replace_history": replace});
    let (status, body) = app
        .call(
            "PUT",
            "/api/v3/me/connections/listenbrainz",
            &[("authorization", auth)],
            Some(link(false)),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["error"]["code"], "HISTORY_ACCOUNT_DIFFERS");
    assert_eq!(body["error"]["details"]["kept_account"], "fay");
    assert!(lb.seen().is_empty(), "nothing sent before the yes");
    assert_eq!(
        history(&app, auth).await["imported_listens"],
        5,
        "nothing replaced"
    );

    let (status, body) = app
        .call(
            "PUT",
            "/api/v3/me/connections/listenbrainz",
            &[("authorization", auth)],
            Some(link(true)),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let body = history(&app, auth).await;
    assert_eq!(body["imported_listens"], 0, "the kept history goes: {body}");
    assert_eq!(body["account"], "gus", "{body}");
    app.until("the new account's export is asked for", || async {
        lb.calls()
            .contains(&"POST /1/export/".to_owned())
            .then_some(())
    })
    .await;
    assert!(
        lb.seen().iter().all(|seen| seen.token == "gus"),
        "with the new account's token: {:?}",
        lb.seen()
    );
    app.stop().await;
}

/// A read ListenBrainz refuses (a token revoked) is noted on the import:
/// neither the tick nor a visit asks again within six hours.
#[tokio::test]
async fn a_refused_read_is_not_asked_again_within_six_hours() {
    let lb = ListenBrainz::default();
    lb.script().pages = [(
        401,
        r#"{"code": 401, "error": "Invalid authorization token."}"#.to_owned(),
    )]
    .into();
    let app = start("history-refused", &lb).await;
    let gil = linked(&app, "gilda").await;
    // Brought up to date seven hours ago: its read is due.
    let synced = droppedneedle_kernel::clock::SystemClock.now_ms() - 7 * 3_600_000;
    app.seed(format!(
        "INSERT INTO media_history_imports (user_id, state, source, account, entries_read, \
         last_synced_at, updated_at) VALUES ('{}', 'current', 'export', 'gilda', 0, {synced}, \
         {synced})",
        gil.user_id
    ))
    .await;
    let reads = || {
        lb.seen()
            .iter()
            .filter(|seen| seen.path.ends_with("/listens"))
            .count()
    };
    app.state().media.history.tick().await;
    let (queue, _) = job_table(&app);
    app.until("the refused read is done", || async {
        queue
            .latest(SYNC, &gil.user_id)
            .await
            .expect("jobs read")
            .filter(|row| row.state == droppedneedle_kernel::jobs::queue::State::Done)
    })
    .await;
    assert_eq!(reads(), 1);
    let tried: i64 = app
        .db()
        .query_row(
            "SELECT updated_at FROM media_history_imports WHERE user_id = ?1",
            [&gil.user_id],
            |row| row.get(0),
        )
        .expect("the import reads");
    assert!(tried > synced, "the try is noted");
    // The first visit of this person, and the tick, queue nothing.
    let done = queue.latest(SYNC, &gil.user_id).await.expect("jobs read");
    assert_eq!(
        history(&app, gil.authorization.as_str()).await["last_synced_at"],
        synced.div_euclid(1000),
        "not marked up to date"
    );
    app.state().media.history.tick().await;
    assert_eq!(
        queue.latest(SYNC, &gil.user_id).await.expect("jobs read"),
        done
    );
    assert_eq!(reads(), 1, "asked once");
    app.stop().await;
}

/// An export request ListenBrainz refuses ends the import failed at once,
/// visits or not; so does one it says is already pending when its list
/// shows none. Neither is asked again by itself.
#[tokio::test]
async fn a_refused_export_request_fails_and_is_not_asked_again() {
    let lb = ListenBrainz::default();
    lb.script().creates = [(
        400,
        r#"{"code": 400, "error": "start_time must be an integer UNIX timestamp in seconds."}"#
            .to_owned(),
    )]
    .into();
    let app = start("history-refused-export", &lb).await;
    let (queue, kit) = job_table(&app);
    let creates = || {
        lb.seen()
            .iter()
            .filter(|seen| seen.method == "POST")
            .count()
    };

    let hal = linked(&app, "halle").await;
    history(&app, hal.authorization.as_str()).await;
    until_failed(&app, &hal.user_id).await;
    for _ in 0..3 {
        history(&app, hal.authorization.as_str()).await;
        kit.clock().advance(10 * 60_000);
        queue.wake();
    }
    assert_eq!(creates(), 1, "one request, refused: {:?}", lb.calls());

    // "Already pending", with nothing pending in the list.
    {
        let mut script = lb.script();
        script.seen.clear();
        script.creates = [(400, ALREADY_ASKED.to_owned())].into();
    }
    let ivy = linked(&app, "ivy").await;
    history(&app, ivy.authorization.as_str()).await;
    until_failed(&app, &ivy.user_id).await;
    history(&app, ivy.authorization.as_str()).await;
    assert_eq!(
        lb.calls(),
        [
            "GET /1/export/list",
            "POST /1/export/",
            "GET /1/export/list"
        ],
        "listed again once, then failed"
    );
    app.stop().await;
}

/// Wait until `user_id`'s import has failed.
async fn until_failed(app: &TestApp, user_id: &str) {
    app.until("the import failed", || async {
        state(app, user_id).starts_with("failed").then_some(())
    })
    .await;
}

/// Opening Discover starts a linked person's import at once, as their
/// Profile does.
#[tokio::test]
async fn a_discover_visit_starts_the_import() {
    let lb = ListenBrainz::default();
    let app = start("history-discover", &lb).await;
    let jo = linked(&app, "jolene").await;
    let (status, body) = app
        .call(
            "POST",
            "/api/v3/discover/activity",
            &[("authorization", jo.authorization.as_str())],
            Some(serde_json::json!({ "feature": "discover" })),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    app.until("the export is asked for", || async {
        lb.calls()
            .contains(&"POST /1/export/".to_owned())
            .then_some(())
    })
    .await;
    assert_eq!(state(&app, &jo.user_id), "exporting|jolene|123");
    app.stop().await;
}
