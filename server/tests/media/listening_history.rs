//! A person's ListenBrainz history, uploaded as an export file, through
//! the production builder: each listen lands once, a second upload of the
//! same file adds none, a listen in the same second as one of the person's
//! plays here is that play and is not kept twice, no play or play count
//! changes, and the file is gone once read. A hostile archive gives only
//! its real month files, and a file that is not an export is refused.

use std::io::Write as _;

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
