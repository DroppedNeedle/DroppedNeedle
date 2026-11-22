//! Deemix client contract tests against the in-repo mock, which answers
//! from the recordings of deemix-webui 4.8.0 in `tests/fixtures/deemix/`.
//! Transport is real HTTP over `127.0.0.1` only.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use droppedneedle_acquire::deemix::client::{
    Bitrate, DeemixClient, DeemixError, ItemKind, QueueTarget, QueueUuid, Route,
};
use droppedneedle_acquire::deemix::models::{
    AddOutcome, Arl, DeezerAvailable, ErrorCause, ErrorId, ErrorKind, ItemStatus, LoginStatus,
    Overwrite, Queue,
};
use droppedneedle_acquire::deemix::session::{DeemixSession, NotNow, SignIn, sign_in_if_needed};
use droppedneedle_kernel::http_client::HttpClientFactory;
use droppedneedle_kernel::test_support::ManualClock;
use droppedneedle_testkit::deemix::{DeemixMock, snapshots};
use droppedneedle_testkit::usenet::serve_loopback;
use serde_json::{Value, json};

const TIMEOUT: Duration = Duration::from_secs(10);

/// The fake ARL the recordings were scrubbed to.
fn recorded_arl() -> String {
    "deadbeef".repeat(24)
}

fn fixtures() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/deemix")
}

fn fixture(name: &str) -> Value {
    let text = std::fs::read_to_string(fixtures().join(name)).expect("fixture reads");
    serde_json::from_str(&text).expect("fixture is JSON")
}

fn uuid(text: &str) -> QueueUuid {
    QueueUuid::parse(text).expect("a uuid of our shape")
}

async fn queue_of(client: &DeemixClient, mock: &DeemixMock, answer: Value) -> Queue {
    let mut state = mock.state();
    state.queue_walk.clear();
    state.queue = answer;
    drop(state);
    client.queue().await.expect("the queue decodes")
}

/// A made-up ARL of the real length.
fn fake_arl() -> String {
    "0123456789abcdef".repeat(12)
}

fn client_for(base: &str, session: &Arc<DeemixSession>) -> DeemixClient {
    let factory = HttpClientFactory::new().expect("http client factory");
    DeemixClient::new(
        factory.no_redirect().clone(),
        base,
        Arc::clone(session),
        TIMEOUT,
    )
    .expect("a usable base URL")
}

fn body(text: &str) -> Value {
    serde_json::from_str(text).expect("the request body is JSON")
}

/// A base URL that misses Deemix's root reaches its web UI, which answers
/// 200 with a page: that is "not Deemix", never a success. A JSON body
/// under another content type is not taken either.
#[tokio::test]
async fn an_html_answer_is_not_deemix() {
    let mock = DeemixMock::new();
    let (base, _server) = serve_loopback(mock.router()).await.expect("mock serves");
    let session = Arc::new(DeemixSession::new());

    let wrong = client_for(&format!("{base}/deemix/"), &session);
    assert_eq!(
        wrong.connect().await.unwrap_err(),
        DeemixError::NotDeemix {
            route: Route::Connect
        }
    );
    assert_eq!(mock.calls_to("/deemix/api/connect").len(), 1);

    mock.script(
        "/api/getQueue",
        200,
        "text/html; charset=UTF-8",
        r#"{"queue":{},"queueOrder":[]}"#,
    );
    let right = client_for(&base, &session);
    assert_eq!(
        right.queue().await.unwrap_err(),
        DeemixError::NotDeemix {
            route: Route::GetQueue
        }
    );
}

/// An add Deemix cannot read crashes it and drops every session, so ids,
/// bitrates and uuids are checked before anything is sent. An add sends
/// one link built from the id; a search term goes out cut to 200
/// characters with control characters taken out.
#[tokio::test]
async fn requests_are_checked_before_they_are_sent() {
    let mock = DeemixMock::new();
    let (base, _server) = serve_loopback(mock.router()).await.expect("mock serves");
    let client = client_for(&base, &Arc::new(DeemixSession::new()));

    for id in [0, 1 << 53] {
        assert!(matches!(
            QueueTarget::album(id),
            Err(DeemixError::InvalidRequest(_))
        ));
        assert!(QueueTarget::track(id).is_err());
    }
    for code in [0, 2, 4, 8, 10, 128, 255] {
        assert!(Bitrate::try_from(code).is_err(), "bitrate {code}");
    }
    let album = QueueTarget::album(14880659).expect("an album id");
    assert_eq!(album.uuid(Bitrate::Flac).as_str(), "album_14880659_9");
    assert!(QueueUuid::parse("track_138546803_1").is_some());
    for bad in [
        "",
        "album_1_5",
        "playlist_1_9",
        "album__9",
        "album_x1_1",
        "track_1",
        "album_1_9;x",
        "album_1_9&uuid=x",
        " album_1_9",
        "album_1_9 ",
        "album_1_1_9",
    ] {
        assert!(QueueUuid::parse(bad).is_none(), "uuid {bad:?}");
        assert!(
            matches!(
                client.remove_from_queue(bad).await,
                Err(DeemixError::InvalidRequest(_))
            ),
            "uuid {bad:?}"
        );
    }
    assert!(client.album_tracklist(0).await.is_err());
    assert!(client.search_albums("\u{7}\n \t", 10).await.is_err());
    assert!(mock.state().calls.is_empty(), "nothing reached Deemix");

    assert_eq!(
        client.add_to_queue(album, Bitrate::Flac).await,
        Err(DeemixError::NotSignedIn)
    );
    let added = mock.calls_to("/api/addToQueue");
    assert_eq!(added.len(), 1);
    assert_eq!(
        body(&added[0].body),
        json!({"url": "https://www.deezer.com/album/14880659", "bitrate": 9})
    );

    let long = format!("Radiohead\u{0}\tIn Rainbows {}", "x".repeat(300));
    client.search_albums(&long, 10).await.expect("searched");
    let searched = mock.calls_to("/api/search");
    let term = &searched[0].query["term"];
    assert_eq!(term.chars().count(), 200);
    assert!(term.starts_with("RadioheadIn Rainbows x"), "{term}");
}

/// The ARL `/connect` offers is Deemix's own Deezer sign-in. It reads only
/// as lowercase hex, never shows in debug output or an error, and goes
/// only to the Deemix it came from, in the sign-in body.
#[tokio::test]
async fn the_arl_never_shows() {
    let arl = fake_arl();
    for bad in [
        String::new(),
        arl.to_uppercase(),
        format!("{arl} "),
        format!("0x{arl}"),
        "a".repeat(257),
    ] {
        assert!(Arl::parse(&bad).is_none(), "{bad:?}");
    }
    let mock = DeemixMock::new();
    mock.state().connect["singleUser"]["arl"] = json!(arl);
    mock.script(
        "/api/loginArl",
        200,
        "application/json; charset=utf-8",
        &json!({"status": arl, "arl": arl}).to_string(),
    );
    let (base, _server) = serve_loopback(mock.router()).await.expect("mock serves");
    let client = client_for(&base, &Arc::new(DeemixSession::new()));

    let mut connect = client.connect().await.expect("connect decodes");
    assert!(!format!("{connect:?}").contains(&arl));
    let offered = connect.take_arl().expect("the ARL is offered");
    assert!(!format!("{offered:?}").contains(&arl));

    let error = client.login_arl(offered).await.unwrap_err();
    assert!(matches!(
        error,
        DeemixError::Shape {
            route: Route::LoginArl,
            ..
        }
    ));
    assert!(!error.to_string().contains(&arl));
    assert!(!format!("{error:?}").contains(&arl));
    let login = mock.calls_to("/api/loginArl");
    assert_eq!(login.len(), 1);
    assert_eq!(login[0].method, "POST");
    assert!(login[0].query.is_empty());
    assert_eq!(body(&login[0].body), json!({ "arl": arl }));

    mock.state().connect["singleUser"]["arl"] = json!(arl.to_uppercase());
    let mut connect = client.connect().await.expect("connect decodes");
    assert!(connect.take_arl().is_none());
}

/// Deemix keeps a Deezer sign-in per session cookie, so every call sends
/// the cookie held for that Deemix and keeps the one an answer sets. A
/// cookie held for another address, or dropped, is not sent.
#[tokio::test]
async fn the_client_sends_the_cookie_it_was_given() {
    let mock = DeemixMock::new();
    let (base, _server) = serve_loopback(mock.router()).await.expect("mock serves");
    let session = Arc::new(DeemixSession::new());
    let client = client_for(&base, &session);

    client.connect().await.expect("connect");
    client.queue().await.expect("queue");
    client
        .search_albums("Radiohead In Rainbows", 3)
        .await
        .expect("search");
    let opened = mock.state().sessions.clone();
    assert_eq!(opened.len(), 1);
    let sent: Vec<Option<String>> = mock
        .state()
        .calls
        .iter()
        .map(|call| call.cookie.clone())
        .collect();
    assert_eq!(
        sent,
        [None, Some(opened[0].clone()), Some(opened[0].clone())]
    );

    let given = Arc::new(DeemixSession::new());
    given.replace(client.base_url(), "s%3Agiven.cookie");
    client_for(&base, &given).queue().await.expect("queue");
    assert_eq!(
        mock.state()
            .calls
            .last()
            .and_then(|call| call.cookie.clone()),
        Some("s%3Agiven.cookie".to_owned())
    );

    let elsewhere = Arc::new(DeemixSession::new());
    let other = client_for("http://192.0.2.1:6595", &elsewhere);
    elsewhere.replace(other.base_url(), "s%3Aelsewhere.cookie");
    client_for(&base, &elsewhere).queue().await.expect("queue");
    assert_eq!(
        mock.state()
            .calls
            .last()
            .and_then(|call| call.cookie.clone()),
        None
    );

    session.drop_cookie();
    client.queue().await.expect("queue");
    assert_eq!(
        mock.state()
            .calls
            .last()
            .and_then(|call| call.cookie.clone()),
        None
    );
}

/// A signed-out `/connect` decodes from the recording, and the `queue`
/// Deemix adds beside it while anything is queued is never read, whatever
/// its shape. A command set to run after downloads reads as set.
#[tokio::test]
async fn a_signed_out_connect_decodes_without_reading_its_queue() {
    let mock = DeemixMock::new();
    mock.state().connect_queue = Some(json!({
        "album_1_9": {"status": 7, "files": "not a list"},
        "playlist_2_3": [null, {"uuid": 4}],
    }));
    let (base, _server) = serve_loopback(mock.router()).await.expect("mock serves");
    let client = client_for(&base, &Arc::new(DeemixSession::new()));

    let mut connect = client.connect().await.expect("connect decodes");
    assert_eq!(connect.update.webui_version, "4.8.0");
    assert_eq!(connect.update.deemix_version, "3.14.1");
    assert!(connect.autologin);
    assert_eq!(connect.deezer_available, DeezerAvailable::Yes);
    assert_eq!(connect.can_stream_hq(), None);
    assert_eq!(connect.can_stream_lossless(), None);
    let settings = connect.settings();
    assert_eq!(settings.overwrite_file, Overwrite::No);
    assert!(!settings.fallback_bitrate);
    assert!(!settings.fallback_search);
    assert!(!settings.fallback_isrc);
    assert!(!settings.execute_command);
    assert_eq!(settings.download_location, "/downloads/");
    assert!(connect.take_arl().is_none());

    mock.state().connect["settingsData"]["settings"]["executeCommand"] =
        json!("chmod -R 777 %folder%");
    let connect = client.connect().await.expect("connect decodes");
    assert!(connect.settings().execute_command);
}

/// Album search and tracklists decode from the recordings: a two-disc
/// album's separator rows are dropped, and an id Deezer does not know
/// reads as no album.
#[tokio::test]
async fn searches_and_tracklists_decode_from_the_recordings() {
    let mock = DeemixMock::new();
    let (base, _server) = serve_loopback(mock.router()).await.expect("mock serves");
    let client = client_for(&base, &Arc::new(DeemixSession::new()));

    let hits = client
        .search_albums("Radiohead In Rainbows", 3)
        .await
        .expect("search");
    assert_eq!(hits.len(), 3);
    assert_eq!(
        (
            hits[0].id,
            hits[0].title.as_str(),
            hits[0].artist.name.as_str()
        ),
        (14880659, "In Rainbows", "Radiohead")
    );
    let searched = &mock.calls_to("/api/search")[0].query;
    assert_eq!(searched["type"], "album");
    assert_eq!(searched["nb"], "3");

    let album = client
        .album_tracklist(6158273)
        .await
        .expect("tracklist")
        .expect("a known album");
    assert_eq!(album.id, 6158273);
    assert_eq!(album.artist.name, "The Smashing Pumpkins");
    assert_eq!(album.explicit_lyrics, Some(true));
    assert_eq!(album.tracks.len(), 28);
    for disc in [1, 2] {
        let tracks = album
            .tracks
            .iter()
            .filter(|track| track.disk_number == disc);
        assert_eq!(tracks.count(), 14, "disc {disc}");
    }
    let first = &album.tracks[0];
    assert_eq!(
        (first.id, first.disk_number, first.track_position),
        (62738609, 1, 1)
    );
    assert_eq!(first.title_short, "Mellon Collie And The Infinite Sadness");
    assert_eq!(first.title_version, "(Remastered 2012)");
    assert_eq!(first.isrc.as_deref(), Some("USVI21200892"));
    assert_eq!(first.duration, Some(171));
    assert!(first.readable);
    let tracklist = &mock.calls_to("/api/getTracklist")[0].query;
    assert_eq!(tracklist["type"], "album");

    assert_eq!(client.album_tracklist(1).await.expect("tracklist"), None);
}

/// Every recorded queue snapshot decodes through the client: a track and
/// two albums from waiting to done, a restart, a kill and a cancel. A key
/// of another shape in `queue` is noted as present and never read, however
/// it looks; an item under a key of ours is read strictly.
#[tokio::test]
async fn every_recorded_queue_snapshot_decodes() {
    let mock = DeemixMock::new();
    let (base, _server) = serve_loopback(mock.router()).await.expect("mock serves");
    let client = client_for(&base, &Arc::new(DeemixSession::new()));

    let mut names: Vec<String> = std::fs::read_dir(fixtures())
        .expect("fixtures list")
        .map(|entry| entry.expect("fixture entry").file_name())
        .map(|name| name.to_string_lossy().into_owned())
        .filter(|name| name.starts_with("getQueue_"))
        .collect();
    names.sort();
    assert_eq!(names.len(), 21, "{names:?}");
    for name in &names {
        mock.state().queue = fixture(name);
        if let Err(error) = client.queue().await {
            panic!("{name}: {error}");
        }
    }

    let mut after_kill = fixture("getQueue_after_kill.json");
    after_kill["queue"]["playlist_908622995_9"] = json!({"status": 3, "files": "not a list"});
    after_kill["queue"]["spotify_6tbjWDEIzxoDsBA1FuhfPW"] = json!([null]);
    after_kill["queue"]["album_1_5"] = json!("not an item");
    let queue = queue_of(&client, &mock, after_kill).await;
    assert!(queue.holds("playlist_908622995_9"));
    assert!(queue.holds("album_1_5"));
    assert_eq!(queue.items().count(), 3);
    for waiting in ["album_14880659_1", "album_6158273_1"] {
        assert_eq!(
            queue.item(&uuid(waiting)).map(|item| item.status),
            Some(ItemStatus::InQueue)
        );
    }
    assert_eq!(queue.queue_order, ["album_14880659_1"]);
    assert!(queue.current().is_none());

    let mut bad = fixture("getQueue_track_3_completed.json");
    bad["queue"]["track_138546807_1"]["files"] = json!("not a list");
    mock.state().queue = bad;
    assert!(matches!(
        client.queue().await,
        Err(DeemixError::Shape {
            route: Route::GetQueue,
            ..
        })
    ));
    let mut misfiled = fixture("getQueue_track_3_completed.json");
    misfiled["queue"]["track_1_1"] = misfiled["queue"]["track_138546807_1"].clone();
    mock.state().queue = misfiled;
    assert!(client.queue().await.is_err());
}

/// A queue item reads as recorded: a waiting item has no counters, files or
/// errors; a finished one lists its files with Deemix's own paths, `CD1/`
/// and `CD2/` on a two-disc album. `current` is read only while its item is
/// still in `queue`. A state or error this version does not know reads as
/// unknown, never as a real one.
#[tokio::test]
async fn queue_items_read_as_recorded() {
    let mock = DeemixMock::new();
    let (base, _server) = serve_loopback(mock.router()).await.expect("mock serves");
    let client = client_for(&base, &Arc::new(DeemixSession::new()));

    mock.walk_queue(&snapshots::TRACK);
    let downloading = client.queue().await.expect("queue");
    let track = uuid("track_138546807_1");
    let current = downloading.current().expect("the track is current");
    assert_eq!(current.uuid, track.as_str());
    assert_eq!(current.title.as_deref(), Some("Nude"));
    let progress = current.progress.as_ref().expect("ours, so read");
    assert_eq!((progress.downloaded, progress.failed), (0, 0));
    assert!(progress.files.is_empty());
    client.queue().await.expect("queue");
    let done = client.queue().await.expect("queue");
    assert!(done.current().is_none());
    let item = done.item(&track).expect("the track");
    assert_eq!(
        (item.kind, item.id, item.bitrate, item.status, item.size),
        (
            ItemKind::Track,
            138546807,
            Bitrate::Mp3At128,
            ItemStatus::Completed,
            1
        )
    );
    assert_eq!((item.downloaded, item.failed), (Some(1), Some(0)));
    assert_eq!(item.extras_path, "/downloads/");
    assert_eq!(item.errors.as_deref(), Some(&[][..]));
    let files = item.files.as_ref().expect("files");
    assert_eq!(files.len(), 1);
    assert_eq!(files[0].path, "/downloads//Radiohead - Nude.mp3");
    assert_eq!(files[0].filename, "Radiohead - Nude.mp3");
    assert_eq!(files[0].data.id, 138546807);
    assert_eq!(client.queue().await.expect("queue").items().count(), 1);

    mock.walk_queue(&snapshots::TWO_ALBUMS);
    let first = client.queue().await.expect("queue");
    let waiting = first.item(&uuid("album_6158273_1")).expect("waiting");
    assert_eq!(waiting.status, ItemStatus::InQueue);
    assert_eq!(waiting.size, 28);
    assert_eq!((waiting.downloaded, waiting.failed), (None, None));
    assert!(waiting.files.is_none() && waiting.errors.is_none());
    assert_eq!(waiting.extras_path, "");
    assert_eq!(first.queue_order, ["album_6158273_1"]);
    let current = first.current().expect("current");
    assert_eq!(current.uuid, "album_14880659_1");
    let progress = current.progress.as_ref().expect("progress");
    assert_eq!((progress.downloaded, progress.files.len()), (8, 8));
    assert_eq!(progress.progress, 78.0);
    for _ in 0..5 {
        client.queue().await.expect("queue");
    }
    let last = client.queue().await.expect("queue");
    let album = last.item(&uuid("album_6158273_1")).expect("album");
    assert_eq!(album.status, ItemStatus::Completed);
    assert_eq!(
        album.extras_path,
        "/downloads//The Smashing Pumpkins - Mellon Collie And The Infinite Sadness (Remastered)"
    );
    let files = album.files.as_ref().expect("files");
    assert_eq!(files.len(), 28);
    for disc in ["CD1/", "CD2/"] {
        let on_disc = files.iter().filter(|file| file.filename.starts_with(disc));
        assert_eq!(on_disc.count(), 14, "{disc}");
    }

    mock.walk_queue(&[snapshots::AFTER_CANCEL]);
    let cancelled = client.queue().await.expect("queue");
    assert!(!cancelled.holds("album_6158273_1"));
    assert!(
        cancelled.current().is_none(),
        "the removed item is not current"
    );

    let mut foreign = fixture("getQueue_track_1_downloading.json");
    foreign["queue"]["playlist_908622995_9"] = json!({"title": "A mix", "status": "downloading"});
    foreign["current"] = json!({"uuid": "playlist_908622995_9", "title": "A mix", "files": 7});
    let queue = queue_of(&client, &mock, foreign).await;
    let current = queue.current().expect("a foreign item is current");
    assert_eq!(current.title.as_deref(), Some("A mix"));
    assert!(current.progress.is_none());

    let mut failed = fixture("getQueue_track_3_completed.json");
    let item = &mut failed["queue"]["track_138546807_1"];
    item["status"] = json!("paused");
    item["errors"] = json!([
        {"message": "Track not found at desired bitrate and no alternative found!",
         "errid": "wrongBitrateNoAlternative", "data": {"id": "138546807"}, "type": "track"},
        {"message": "You need to login to download tracks.", "errid": "notLoggedIn",
         "data": {}, "type": "track"},
        {"message": "a new one", "errid": "someNewError", "data": {}, "type": "track"},
        {"message": "ENOENT", "stack": "Error: ENOENT", "data": {"position": 1}, "type": "post"},
    ]);
    let queue = queue_of(&client, &mock, failed).await;
    let item = queue.item(&track).expect("the track");
    assert_eq!(item.status, ItemStatus::Unknown);
    let errors: Vec<(Option<ErrorId>, ErrorKind)> = item
        .errors
        .as_ref()
        .expect("errors")
        .iter()
        .map(|error| (error.errid, error.kind))
        .collect();
    let id = |cause, no_alternative| {
        Some(ErrorId {
            cause,
            no_alternative,
        })
    };
    assert_eq!(
        errors,
        [
            (id(ErrorCause::WrongBitrate, true), ErrorKind::Track),
            (id(ErrorCause::NotLoggedIn, false), ErrorKind::Track),
            (id(ErrorCause::Other, false), ErrorKind::Track),
            (None, ErrorKind::Post),
        ]
    );
}

/// A signed-in add answers the uuid of the item it queued. An add that
/// queued nothing (recorded when Deemix's own lookup failed) answers
/// "nothing added", and a bitrate the account cannot stream is refused.
#[tokio::test]
async fn an_add_answers_the_uuid_it_queued() {
    let mock = DeemixMock::new();
    mock.offer_saved_login();
    let (base, _server) = serve_loopback(mock.router()).await.expect("mock serves");
    let client = client_for(&base, &Arc::new(DeemixSession::new()));
    let clock = ManualClock::at(1_700_000_000_000);
    let signed_in = sign_in_if_needed(&client, &clock).await.expect("sign-in");
    assert!(matches!(signed_in, SignIn::SignedIn(_)), "{signed_in:?}");

    let track = QueueTarget::track(138546807).expect("track");
    assert_eq!(
        client.add_to_queue(track, Bitrate::Mp3At128).await,
        Ok(AddOutcome::Queued(uuid("track_138546807_1")))
    );
    let album = QueueTarget::album(6158273).expect("album");
    assert_eq!(
        client.add_to_queue(album, Bitrate::Mp3At128).await,
        Ok(AddOutcome::Queued(uuid("album_6158273_1")))
    );
    assert_eq!(
        client.add_to_queue(album, Bitrate::Flac).await,
        Err(DeemixError::CantStream)
    );
    let added = mock.calls_to("/api/addToQueue");
    assert_eq!(
        body(&added[1].body),
        json!({"url": "https://www.deezer.com/album/6158273", "bitrate": 1})
    );

    mock.state().add_nothing = true;
    assert_eq!(
        client.add_to_queue(album, Bitrate::Mp3At128).await,
        Ok(AddOutcome::NothingAdded)
    );

    let mut foreign = fixture("addToQueue_album.json");
    foreign["data"]["obj"][0]["uuid"] = json!("playlist_1_1");
    mock.script(
        "/api/addToQueue",
        200,
        "application/json; charset=utf-8",
        &foreign.to_string(),
    );
    assert!(matches!(
        client.add_to_queue(album, Bitrate::Mp3At128).await,
        Err(DeemixError::Shape {
            route: Route::AddToQueue,
            ..
        })
    ));
}

/// `loginArl` reads its status only: the recorded success and "already
/// signed in", the two that erase Deemix's saved sign-in, and any other
/// number as unknown. The answer echoes the ARL, which shows in no result,
/// error or debug output.
#[tokio::test]
async fn sign_in_statuses_decode_without_the_arl() {
    let arl = recorded_arl();
    let mock = DeemixMock::new();
    mock.offer_saved_login();
    let (base, _server) = serve_loopback(mock.router()).await.expect("mock serves");
    let session = Arc::new(DeemixSession::new());
    let client = client_for(&base, &session);

    let mut connect = client.connect().await.expect("connect");
    assert!(!connect.signed_in());
    let offered = connect.take_arl().expect("the saved sign-in is offered");
    assert_eq!(client.login_arl(offered).await, Ok(LoginStatus::Success));
    assert_eq!(
        client.login_arl(Arl::parse(&arl).expect("arl")).await,
        Ok(LoginStatus::AlreadyLogged)
    );
    let login = mock.calls_to("/api/loginArl");
    assert_eq!(body(&login[0].body), json!({ "arl": arl }));

    for (code, status) in [
        (-1, LoginStatus::NotAvailable),
        (0, LoginStatus::Failed),
        (3, LoginStatus::Unknown),
        (42, LoginStatus::Unknown),
    ] {
        let answer = json!({"status": code, "arl": arl, "user": {}});
        mock.script(
            "/api/loginArl",
            200,
            "application/json; charset=utf-8",
            &answer.to_string(),
        );
        let read = client.login_arl(Arl::parse(&arl).expect("arl")).await;
        assert_eq!(read, Ok(status), "status {code}");
    }
    mock.script(
        "/api/loginArl",
        200,
        "application/json; charset=utf-8",
        &json!({"status": arl, "arl": arl}).to_string(),
    );
    let error = client
        .login_arl(Arl::parse(&arl).expect("arl"))
        .await
        .unwrap_err();
    assert!(!error.to_string().contains(&arl));
    assert!(!format!("{error:?}").contains(&arl));
    assert!(!format!("{session:?}").contains(&arl));
}

/// The signed-in `/connect` answers decode from the recordings: the
/// account's flags, no ARL offered, and a `queue` beside them that is
/// never read.
#[tokio::test]
async fn signed_in_connects_decode_from_the_recordings() {
    let mock = DeemixMock::new();
    let (base, _server) = serve_loopback(mock.router()).await.expect("mock serves");
    let client = client_for(&base, &Arc::new(DeemixSession::new()));

    for name in ["connect_signed_in.json", "connect_with_queue.json"] {
        mock.state().connect = fixture(name);
        let mut connect = client.connect().await.expect(name);
        assert!(connect.signed_in(), "{name}");
        assert_eq!(connect.can_stream_hq(), Some(false), "{name}");
        assert_eq!(connect.can_stream_lossless(), Some(false), "{name}");
        assert!(connect.take_arl().is_none(), "{name}");
    }
    mock.state().connect = fixture("connect_saved_login.json");
    let mut connect = client.connect().await.expect("saved login");
    assert!(!connect.signed_in());
    assert_eq!(connect.can_stream_hq(), None);
    assert!(connect.take_arl().is_some());
}

/// A sign-in replays the offered ARL only when the session is signed out,
/// Deezer is reachable and the limiter is open: at most one per 15 minutes
/// plus up to 5 per Deemix. After a failed one, which erases Deemix's saved
/// sign-in, none is tried until an ARL is offered again.
#[tokio::test]
async fn sign_ins_keep_to_their_limits() {
    let minute = 60_000;
    let clock = ManualClock::at(1_700_000_000_000);

    let mock = DeemixMock::new();
    let (base, _server) = serve_loopback(mock.router()).await.expect("mock serves");
    let client = client_for(&base, &Arc::new(DeemixSession::new()));
    let none = sign_in_if_needed(&client, &clock).await.expect("sign-in");
    assert!(
        matches!(none, SignIn::NotNow(NotNow::NoArlOffered)),
        "{none:?}"
    );
    mock.offer_saved_login();
    mock.state().connect["deezerAvailable"] = json!("no-network");
    let down = sign_in_if_needed(&client, &clock).await.expect("sign-in");
    assert!(
        matches!(down, SignIn::NotNow(NotNow::DeezerUnavailable)),
        "{down:?}"
    );
    assert!(mock.calls_to("/api/loginArl").is_empty());

    mock.offer_saved_login();
    let SignIn::SignedIn(connect) = sign_in_if_needed(&client, &clock).await.expect("sign-in")
    else {
        panic!("signed in");
    };
    assert_eq!(connect.can_stream_hq(), Some(false));
    let again = sign_in_if_needed(&client, &clock).await.expect("sign-in");
    assert!(matches!(again, SignIn::SignedIn(_)), "{again:?}");
    assert_eq!(mock.calls_to("/api/loginArl").len(), 1);

    mock.restart();
    clock.advance(15 * minute - 1);
    let early = sign_in_if_needed(&client, &clock).await.expect("sign-in");
    assert!(
        matches!(early, SignIn::NotNow(NotNow::Waiting)),
        "{early:?}"
    );
    assert_eq!(mock.calls_to("/api/loginArl").len(), 1);
    clock.advance(5 * minute + 1_001);
    let late = sign_in_if_needed(&client, &clock).await.expect("sign-in");
    assert!(matches!(late, SignIn::SignedIn(_)), "{late:?}");
    assert_eq!(mock.calls_to("/api/loginArl").len(), 2);

    let mock = DeemixMock::new();
    mock.offer_saved_login();
    mock.state().login_fails = true;
    let (base, _server) = serve_loopback(mock.router()).await.expect("mock serves");
    let session = Arc::new(DeemixSession::new());
    let client = client_for(&base, &session);
    let lost = sign_in_if_needed(&client, &clock).await.expect("sign-in");
    assert!(matches!(lost, SignIn::Lost), "{lost:?}");
    assert!(session.sign_in_lost(client.base_url()));
    clock.advance(60 * minute);
    let still = sign_in_if_needed(&client, &clock).await.expect("sign-in");
    assert!(matches!(still, SignIn::Lost), "{still:?}");
    assert_eq!(mock.calls_to("/api/loginArl").len(), 1);

    mock.state().login_fails = false;
    mock.offer_saved_login();
    let back = sign_in_if_needed(&client, &clock).await.expect("sign-in");
    assert!(matches!(back, SignIn::SignedIn(_)), "{back:?}");
    assert!(!session.sign_in_lost(client.base_url()));
    assert_eq!(mock.calls_to("/api/loginArl").len(), 2);

    let mock = DeemixMock::new();
    mock.offer_saved_login();
    mock.script("/api/loginArl", 500, "text/plain", "");
    let (base, _server) = serve_loopback(mock.router()).await.expect("mock serves");
    let client = client_for(&base, &Arc::new(DeemixSession::new()));
    assert_eq!(
        sign_in_if_needed(&client, &clock).await.unwrap_err(),
        DeemixError::Status {
            route: Route::LoginArl,
            status: 500
        }
    );
    mock.state().scripted.clear();
    let retry = sign_in_if_needed(&client, &clock).await.expect("sign-in");
    assert!(
        matches!(retry, SignIn::NotNow(NotNow::Waiting)),
        "a sign-in that met an error still spends its turn: {retry:?}"
    );
    assert_eq!(mock.calls_to("/api/loginArl").len(), 1);
}

/// The recordings were scrubbed before they were committed: no hex run of
/// 40 or more is left but the fake ARL, and every `license_token` is all
/// `x`.
#[test]
fn the_fixtures_hold_no_sign_in() {
    fn tokens(value: &Value, found: &mut Vec<String>) {
        match value {
            Value::Object(map) => {
                for (key, value) in map {
                    if key == "license_token" {
                        found.push(value.as_str().unwrap_or("not text").to_owned());
                    }
                    tokens(value, found);
                }
            }
            Value::Array(items) => items.iter().for_each(|item| tokens(item, found)),
            _ => {}
        }
    }
    let hex_run = regex::Regex::new("[0-9a-fA-F]{40,}").expect("regex");
    let fake = regex::Regex::new("^(?:deadbeef)+$").expect("regex");
    let mut license_tokens = Vec::new();
    for entry in std::fs::read_dir(fixtures()).expect("fixtures list") {
        let path = entry.expect("fixture entry").path();
        let text = std::fs::read_to_string(&path).expect("fixture reads");
        for run in hex_run.find_iter(&text) {
            assert!(
                fake.is_match(run.as_str()),
                "{}: {}",
                path.display(),
                run.as_str()
            );
        }
        if path.extension().is_some_and(|ext| ext == "json") {
            let value: Value = serde_json::from_str(&text).expect("fixture is JSON");
            tokens(&value, &mut license_tokens);
        }
    }
    assert!(!license_tokens.is_empty());
    for token in license_tokens {
        assert!(
            !token.is_empty() && token.bytes().all(|byte| byte == b'x'),
            "{token}"
        );
    }
}
