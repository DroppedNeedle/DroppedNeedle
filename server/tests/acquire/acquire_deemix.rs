//! Deezer through Deemix in the running app: a saved, turned-on Deemix
//! section makes Deezer a download source for requests, the queue and the
//! status card. The app starts through the production builder against the
//! testkit Deemix mock and recorded MusicBrainz and Deezer answers; the
//! audio is the committed fixtures, put where the recorded queue says
//! Deemix wrote it.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use axum::Router;
use axum::extract::Request;
use axum::http::{StatusCode, Uri, header};
use axum::middleware::{Next, from_fn};
use axum::response::IntoResponse;
use droppedneedle_acquire::db::AcquireDb;
use droppedneedle_acquire::downloads::journal::Journal;
use droppedneedle_acquire::downloads::store::{NewTask, TaskDetails};
use droppedneedle_acquire::landing::LandingService;
use droppedneedle_acquire::landing::library::LibraryLanding;
use droppedneedle_acquire::reason::Reason;
use droppedneedle_acquire::target::Targets;
use droppedneedle_acquire::target::lookup::{AlbumRelease, AlbumTrack};
use droppedneedle_kernel::runtime_config::ConfigStore;
use droppedneedle_kernel::runtime_config::sections::{DeemixConnection, recipe_for_range};
use droppedneedle_kernel::test_support::ManualClock;
use droppedneedle_library::tags::TagField;
use droppedneedle_library::tags::save::{TagEdit, save_tags};
use droppedneedle_testkit::deemix::{DeemixMock, snapshots};
use droppedneedle_testkit::usenet::{ServerGuard, serve_loopback};
use serde_json::{Value, json};

use crate::common::acquire::{FixedAlbums, save_policy, worker_with};
use crate::common::{ScratchDir, TestApp, TestPlatform};

const RADIOHEAD: &str = "a74b1b7f-71a5-4011-9441-d0b5e4122711";
const IN_RAINBOWS_GROUP: &str = "c0ffee00-0000-4000-8000-0000000014b0";
const IN_RAINBOWS_RELEASE: &str = "c0ffee00-0000-4000-8000-0000000014a0";
const IN_RAINBOWS_BARCODE: &str = "634904032463";
const MELLON_COLLIE_RELEASE: &str = "c0ffee00-0000-4000-8000-0000000062a0";

fn fixture(path: &str) -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(path);
    std::fs::read_to_string(path).expect("fixture reads")
}

/// The tracks of a recorded Deemix tracklist, disc separators left out.
fn tracks(name: &str) -> Vec<Value> {
    let tracklist: Value = serde_json::from_str(&fixture(name)).expect("tracklist is JSON");
    tracklist["tracks"]
        .as_array()
        .expect("tracks")
        .iter()
        .filter(|track| track["type"] == "track")
        .cloned()
        .collect()
}

fn recording_mbid(position: u64) -> String {
    format!("c0ffee00-0000-4000-8000-00001400{position:04}")
}

fn track_mbid(position: u64) -> String {
    format!("c0ffee00-0000-4000-8000-00001500{position:04}")
}

fn credit() -> Value {
    json!([{
        "name": "Radiohead",
        "joinphrase": "",
        "artist": {"id": RADIOHEAD, "name": "Radiohead", "sort-name": "Radiohead"},
    }])
}

fn in_rainbows_group() -> Value {
    json!({
        "id": IN_RAINBOWS_GROUP,
        "title": "In Rainbows",
        "primary-type": "Album",
        "secondary-types": [],
        "first-release-date": "2007-10-10",
        "artist-credit": credit(),
    })
}

/// In Rainbows as MusicBrainz lists it, in the shape of a recorded release
/// lookup: the recorded Deezer tracklist's positions, titles and ISRCs, and
/// no lengths, which the short audio fixtures could not match.
fn in_rainbows_release() -> Value {
    let tracks: Vec<Value> = tracks("deemix/tracklist_album.json")
        .iter()
        .map(|track| {
            let position = track["track_position"].as_u64().expect("position");
            json!({
                "id": track_mbid(position),
                "position": position,
                "number": position.to_string(),
                "title": track["title_short"],
                "artist-credit": credit(),
                "recording": {
                    "id": recording_mbid(position),
                    "title": track["title_short"],
                    "isrcs": [track["isrc"]],
                    "artist-credit": credit(),
                },
            })
        })
        .collect();
    json!({
        "id": IN_RAINBOWS_RELEASE,
        "title": "In Rainbows",
        "status": "Official",
        "date": "2007-12-31",
        "country": "XW",
        "barcode": IN_RAINBOWS_BARCODE,
        "artist-credit": credit(),
        "release-group": in_rainbows_group(),
        "media": [{
            "position": 1,
            "format": "Digital Media",
            "track-count": tracks.len(),
            "tracks": tracks,
        }],
    })
}

/// The MusicBrainz answer for one request below `/ws/2`, if one is made
/// here for it.
fn musicbrainz(path: &str, query: &str) -> Option<Value> {
    if path == format!("/release/{IN_RAINBOWS_RELEASE}") {
        return Some(in_rainbows_release());
    }
    if path == format!("/release-group/{IN_RAINBOWS_GROUP}") {
        let mut group = in_rainbows_group();
        group["releases"] = json!([{
            "id": IN_RAINBOWS_RELEASE, "title": "In Rainbows", "status": "Official",
            "date": "2007-12-31", "country": "XW",
        }]);
        return Some(group);
    }
    if path == "/release" && query.contains(&format!("release-group={IN_RAINBOWS_GROUP}")) {
        return Some(json!({
            "release-count": 1, "release-offset": 0, "releases": [in_rainbows_release()],
        }));
    }
    let recording = path.strip_prefix("/recording/")?;
    let release = in_rainbows_release();
    let track = release["media"][0]["tracks"]
        .as_array()?
        .iter()
        .find(|track| track["recording"]["id"] == recording)?
        .clone();
    let mut answer = track["recording"].clone();
    answer["releases"] = json!([{
        "id": IN_RAINBOWS_RELEASE, "title": "In Rainbows", "status": "Official",
        "release-group": in_rainbows_group(),
    }]);
    Some(answer)
}

/// MusicBrainz (and its mirror) and Deezer's API at one origin, as the
/// app's outbound addresses place them. Every request is recorded.
fn providers(calls: Arc<Mutex<Vec<String>>>) -> Router {
    Router::new().fallback(move |uri: Uri| {
        let calls = calls.clone();
        async move {
            calls.lock().expect("calls").push(
                uri.path_and_query()
                    .map(|p| p.to_string())
                    .unwrap_or_default(),
            );
            let path = uri.path();
            let body = if path == format!("/deezer/album/upc:{IN_RAINBOWS_BARCODE}") {
                Some(fixture("deezer/album_upc_12.json"))
            } else if path.starts_with("/deezer/") {
                Some(fixture("deezer/album_upc_unknown.json"))
            } else {
                path.strip_prefix("/musicbrainz")
                    .or_else(|| path.strip_prefix("/brainzmash"))
                    .and_then(|path| musicbrainz(path, uri.query().unwrap_or_default()))
                    .map(|answer| answer.to_string())
            };
            let (status, body) = match body {
                Some(body) => (StatusCode::OK, body),
                None => (
                    StatusCode::NOT_FOUND,
                    fixture("musicbrainz/artist_merged_missing.json"),
                ),
            };
            (status, [(header::CONTENT_TYPE, "application/json")], body).into_response()
        }
    })
}

/// The Deemix mock, whose queue answers `walk` once an add arrives, as
/// Deemix lists an item from its add on.
fn deemix(mock: &DeemixMock, walk: &'static [&'static str]) -> Router {
    let watched = mock.clone();
    mock.router()
        .layer(from_fn(move |request: Request, next: Next| {
            let mock = watched.clone();
            async move {
                let add = request.uri().path() == "/api/addToQueue";
                let response = next.run(request).await;
                if add && mock.state().queue_walk.is_empty() {
                    mock.walk_queue(walk);
                }
                response
            }
        }))
}

/// A Deemix that offers its saved Deezer sign-in (a free account, MP3 128
/// only) and overwrites files.
fn signed_out_deemix() -> DeemixMock {
    let mock = DeemixMock::new();
    mock.offer_saved_login();
    let mut state = mock.state();
    state.connect["settingsData"]["settings"]["overwriteFile"] = json!("y");
    state.connect_signed_in["settingsData"]["settings"]["overwriteFile"] = json!("y");
    drop(state);
    mock
}

/// A fixture copy at `path`, tagged as Radiohead's track `title`, number
/// `track` of `album`, with no MusicBrainz ids.
fn tagged(path: &Path, album: &str, title: &str, track: u64) {
    std::fs::create_dir_all(path.parent().expect("folder")).expect("folder");
    std::fs::copy(
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/library/mp3_full_01.mp3"),
        path,
    )
    .expect("fixture copies");
    let mut edits = vec![
        TagEdit::new(TagField::Title, vec![title.to_owned()]),
        TagEdit::new(TagField::Artist, vec!["Radiohead".to_owned()]),
        TagEdit::new(TagField::AlbumArtist, vec!["Radiohead".to_owned()]),
        TagEdit::new(TagField::Album, vec![album.to_owned()]),
        TagEdit::new(TagField::TrackNumber, vec![track.to_string()]),
        TagEdit::new(TagField::DiscNumber, vec!["1".to_owned()]),
    ];
    for field in [
        TagField::MusicBrainzRecordingId,
        TagField::MusicBrainzReleaseTrackId,
        TagField::MusicBrainzReleaseId,
        TagField::MusicBrainzReleaseGroupId,
    ] {
        edits.push(TagEdit::new(field, Vec::new()));
    }
    save_tags(path, &edits).expect("tags save");
}

/// The audio files under `dir`.
fn music_under(dir: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut pending = vec![dir.to_path_buf()];
    while let Some(dir) = pending.pop() {
        for entry in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
            let path = entry.path();
            if path.is_dir() {
                pending.push(path);
            } else if path.extension().is_some_and(|ext| ext == "mp3") {
                found.push(path);
            }
        }
    }
    found.sort();
    found
}

/// The app with Deemix and the providers' answers serving.
struct Journey {
    app: TestApp,
    admin: String,
    mock: DeemixMock,
    deemix_url: String,
    providers: Arc<Mutex<Vec<String>>>,
    music: PathBuf,
    mount: PathBuf,
    _servers: (ServerGuard, ServerGuard),
}

impl Journey {
    /// The app with a library folder, quality settings that take the free
    /// account's MP3 128, and no download source yet. Deemix's queue answers
    /// `walk` once an add arrives.
    async fn start(tag: &str, walk: &'static [&'static str]) -> Self {
        let mock = signed_out_deemix();
        let (deemix_url, deemix_server) = serve_loopback(deemix(&mock, walk))
            .await
            .expect("Deemix mock");
        let calls: Arc<Mutex<Vec<String>>> = Arc::default();
        let (origin, provider_server) = serve_loopback(providers(calls.clone()))
            .await
            .expect("providers mock");
        let app = TestApp::start_with(tag, crate::common::outbound_at(&origin)).await;
        let admin = app.login_admin().await;
        let dir = app.config.root_app_dir.clone();
        let music = dir.join("music");
        let mount = dir.join("deemix-downloads");
        std::fs::create_dir_all(&music).expect("music folder");
        std::fs::create_dir_all(&mount).expect("Deemix folder");
        app.add_library_folder("music", &music).await;
        let journey = Self {
            app,
            admin,
            mock,
            deemix_url,
            providers: calls,
            music,
            mount,
            _servers: (deemix_server, provider_server),
        };
        let policy = "/api/v3/settings/download-clients/policy";
        let (status, mut saved) = journey.call("GET", policy, None).await;
        assert_eq!(status, StatusCode::OK, "{saved}");
        saved["quality_min"] = json!("low");
        saved["quality_recipe"] =
            serde_json::to_value(recipe_for_range("low", "lossless", &[])).expect("recipe");
        saved["search_review"] = json!("off");
        let (status, saved) = journey.call("PUT", policy, Some(saved)).await;
        assert_eq!(status, StatusCode::OK, "{saved}");
        journey
    }

    async fn call(&self, method: &str, uri: &str, body: Option<Value>) -> (StatusCode, Value) {
        self.app
            .call_through_busy(method, uri, &[("authorization", self.admin.as_str())], body)
            .await
    }

    /// Turn Deemix on in Settings, as the admin does.
    async fn save_deemix(&self) {
        self.save_deemix_at(&self.deemix_url, &self.mount).await;
    }

    /// Save Deemix's settings with its address `url` and its downloads
    /// folder at `mount`.
    async fn save_deemix_at(&self, url: &str, mount: &Path) {
        let (status, body) = self
            .call(
                "PUT",
                "/api/v3/settings/download-clients/deemix",
                Some(json!({
                    "enabled": true,
                    "url": url,
                    "downloads_mount": mount,
                })),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{body}");
    }

    /// The JSON bodies of the adds Deemix was sent.
    fn adds(&self) -> Vec<Value> {
        self.mock
            .calls_to("/api/addToQueue")
            .iter()
            .map(|call| serde_json::from_str(&call.body).expect("add body is JSON"))
            .collect()
    }

    /// Wait, waking the download worker each time round, until `check`
    /// answers.
    async fn until<T>(&self, what: &str, mut check: impl FnMut(&Self) -> Option<T>) -> T {
        self.app
            .until(what, || {
                self.app.state().acquire.worker.wake();
                std::future::ready(check(self))
            })
            .await
    }

    /// Wait until the one download task completes.
    async fn completed(&self) {
        self.until("the download completes", |journey| {
            let status: String = journey
                .app
                .db()
                .query_row("SELECT status FROM download_tasks", [], |row| row.get(0))
                .ok()?;
            assert_ne!(status, "failed", "the download failed");
            (status == "completed").then_some(())
        })
        .await;
    }

    /// The files the Deezer attempt stored for its landing.
    fn stored_paths(&self) -> Vec<PathBuf> {
        let stored: String = self
            .app
            .db()
            .query_row(
                "SELECT materialized_paths_json FROM download_attempts WHERE source = 'deezer'",
                [],
                |row| row.get(0),
            )
            .expect("the Deezer attempt");
        let mut paths: Vec<PathBuf> = serde_json::from_str(&stored).expect("paths are JSON");
        paths.sort();
        paths
    }

    /// Wait until the Deemix item `uuid` is taken out of Deemix's queue.
    async fn removed(&self, uuid: &str) {
        self.until("the Deemix item is removed", |journey| {
            journey
                .mock
                .calls_to("/api/removeFromQueue")
                .iter()
                .any(|call| call.query.get("uuid").map(String::as_str) == Some(uuid))
                .then_some(())
        })
        .await;
    }

    async fn stop(self) {
        self.app.stop().await;
    }
}

/// A request made while no source is on waits for one; saving a turned-on
/// Deemix starts it by itself. The edition's barcode finds the Deezer album
/// through Deezer's API, its tracklist fits, and the album is added at the
/// free account's MP3 128 on the one session the status probe signed in.
/// Deemix's files are read where its folder is mounted, land in the
/// library, and the item leaves Deemix's queue. The status card shows the
/// account from the probe cache.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_held_album_request_lands_from_deezer_once_deemix_is_on() {
    let journey = Journey::start(
        "deemix-album",
        &[snapshots::TWO_ALBUMS[0], snapshots::TWO_ALBUMS[6]],
    )
    .await;
    let album = journey.mount.join("Radiohead - In Rainbows");
    for track in tracks("deemix/tracklist_album.json") {
        let position = track["track_position"].as_u64().expect("position");
        let title = track["title_short"].as_str().expect("title");
        let name = format!("{position:02} - {}.mp3", title.replace('/', "_"));
        tagged(&album.join(name), "In Rainbows", title, position);
    }

    let (status, body) = journey
        .call(
            "POST",
            "/api/v3/requests/albums",
            Some(json!({
                "musicbrainz_id": IN_RAINBOWS_GROUP,
                "artist": "Radiohead",
                "album": "In Rainbows",
                "release_mbid": IN_RAINBOWS_RELEASE,
            })),
        )
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    let (status, body) = journey.call("GET", "/api/v3/requests/active", None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        body["items"][0]["waiting_reason"], "no_download_source",
        "{body}"
    );

    journey.save_deemix().await;
    journey
        .app
        .until("saving Deemix starts the held request", || {
            std::future::ready((!journey.adds().is_empty()).then_some(()))
        })
        .await;
    assert_eq!(
        journey.adds(),
        [json!({"url": "https://www.deezer.com/album/14880659", "bitrate": 1})]
    );
    let deezer_api: Vec<String> = journey
        .providers
        .lock()
        .expect("calls")
        .iter()
        .filter(|call| call.starts_with("/deezer/"))
        .cloned()
        .collect();
    assert_eq!(
        deezer_api,
        [format!("/deezer/album/upc:{IN_RAINBOWS_BARCODE}")]
    );
    let tracklists = journey.mock.calls_to("/api/getTracklist");
    assert_eq!(tracklists.len(), 1);
    assert_eq!(
        tracklists[0].query.get("id").map(String::as_str),
        Some("14880659")
    );
    assert!(journey.mock.calls_to("/api/search").is_empty());
    assert_eq!(
        journey.mock.calls_to("/api/loginArl").len(),
        1,
        "the probe and the source share one session and its sign-in"
    );

    journey.completed().await;
    let expected: Vec<PathBuf> = {
        let mut names: Vec<PathBuf> = tracks("deemix/tracklist_album.json")
            .iter()
            .map(|track| {
                let position = track["track_position"].as_u64().expect("position");
                let title = track["title_short"].as_str().expect("title");
                album.join(format!("{position:02} - {}.mp3", title.replace('/', "_")))
            })
            .collect();
        names.sort();
        names
    };
    assert_eq!(journey.stored_paths(), expected);
    assert_eq!(
        music_under(&journey.music).len(),
        10,
        "the album is in the library"
    );
    assert!(
        music_under(&journey.mount).is_empty(),
        "the imported files left Deemix's folder"
    );
    journey.removed("album_14880659_1").await;

    let card = journey
        .app
        .until("the status card shows the account", || async {
            let (code, card) = journey
                .call("GET", "/api/v3/acquire/deemix/status", None)
                .await;
            assert_eq!(code, StatusCode::OK, "{card}");
            (card["signed_in"] == true).then_some(card)
        })
        .await;
    assert_eq!(card["reachable"], true, "{card}");
    assert_eq!(card["account_quality"], "mp3_128", "{card}");
    assert_eq!(card["best_quality"], "mp3_128", "{card}");

    // Another section's save keeps the signed-in session, and so does a
    // Deemix save that moves only its folder. A save that changes nothing
    // is not stored, so each save here changes a value.
    let seen = journey.mock.calls_to("/api/connect").len();
    let policy = "/api/v3/settings/download-clients/policy";
    let (status, mut saved) = journey.call("GET", policy, None).await;
    assert_eq!(status, StatusCode::OK, "{saved}");
    saved["max_concurrent_downloads"] = json!(
        saved["max_concurrent_downloads"]
            .as_i64()
            .expect("a number")
            + 1
    );
    let (status, saved) = journey.call("PUT", policy, Some(saved)).await;
    assert_eq!(status, StatusCode::OK, "{saved}");
    let after = journey
        .until("Deemix is read after the policy save", |journey| {
            let calls = journey.mock.calls_to("/api/connect");
            (calls.len() > seen).then(|| calls[seen..].to_vec())
        })
        .await;
    let signed_in = journey.mock.state().signed_in.clone();
    assert!(
        after.iter().all(|call| call
            .cookie
            .as_ref()
            .is_some_and(|cookie| signed_in.contains(cookie))),
        "a policy save kept the session"
    );
    assert_eq!(journey.mock.calls_to("/api/loginArl").len(), 1);

    let seen = journey.mock.calls_to("/api/connect").len();
    let moved = journey.mount.with_file_name("deemix-downloads-2");
    std::fs::create_dir_all(&moved).expect("second Deemix folder");
    journey.save_deemix_at(&journey.deemix_url, &moved).await;
    let after = journey
        .until("Deemix is read after its folder save", |journey| {
            let calls = journey.mock.calls_to("/api/connect");
            (calls.len() > seen).then(|| calls[seen..].to_vec())
        })
        .await;
    assert!(
        after.iter().all(|call| call
            .cookie
            .as_ref()
            .is_some_and(|cookie| signed_in.contains(cookie))),
        "a folder save kept the session"
    );
    assert_eq!(journey.mock.calls_to("/api/loginArl").len(), 1);

    // A new address drops it: the next read opens a new session.
    let seen = journey.mock.calls_to("/api/connect").len();
    let other = journey.deemix_url.replace("127.0.0.1", "localhost");
    journey.save_deemix_at(&other, &moved).await;
    journey
        .until(
            "Deemix is read on a new session after its address changed",
            |journey| {
                journey.mock.calls_to("/api/connect")[seen..]
                    .iter()
                    .any(|call| call.cookie.is_none())
                    .then_some(())
            },
        )
        .await;
    journey.stop().await;
}

/// A song request queues only its track, by the track's address, from the
/// album that fits; Deemix writes the single loose in its folder, and it
/// lands.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_song_request_lands_its_one_track_from_deezer() {
    let journey = Journey::start("deemix-song", &snapshots::TRACK).await;
    tagged(
        &journey.mount.join("Radiohead - Nude.mp3"),
        "In Rainbows",
        "Nude",
        3,
    );
    journey.save_deemix().await;

    let (status, body) = journey
        .call(
            "POST",
            "/api/v3/requests/tracks",
            Some(json!({
                "recording_mbid": recording_mbid(3),
                "artist_name": "Radiohead",
                "track_title": "Nude",
                "album_title": "In Rainbows",
                "release_group_mbid": IN_RAINBOWS_GROUP,
                "release_mbid": IN_RAINBOWS_RELEASE,
            })),
        )
        .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    journey.completed().await;
    assert_eq!(
        journey.adds(),
        [json!({"url": "https://www.deezer.com/track/138546807", "bitrate": 1})]
    );
    assert_eq!(
        journey.stored_paths(),
        [journey.mount.join("Radiohead - Nude.mp3")]
    );
    assert_eq!(
        music_under(&journey.music).len(),
        1,
        "the song is in the library"
    );
    journey.removed("track_138546807_1").await;
    journey.stop().await;
}

/// An album as MusicBrainz lists it, from a recorded Deemix tracklist.
fn album_release(id: &str, title: &str, artist: &str, tracklist: &str) -> AlbumRelease {
    AlbumRelease {
        id: id.to_owned(),
        title: title.to_owned(),
        release_group_mbid: format!("{id}-group"),
        artist: artist.to_owned(),
        various_artists: false,
        year: None,
        barcode: None,
        tracks: tracks(tracklist)
            .iter()
            .enumerate()
            .map(|(index, track)| AlbumTrack {
                disc: track["disk_number"].as_u64().expect("disc") as u32,
                position: track["track_position"].as_u64().expect("position") as u32,
                title: track["title_short"].as_str().expect("title").to_owned(),
                duration_seconds: track["duration"].as_f64(),
                recording_mbid: recording_mbid(index as u64 + 100),
                release_track_mbid: track_mbid(index as u64 + 100),
                isrcs: vec![track["isrc"].as_str().expect("isrc").to_owned()],
            })
            .collect(),
    }
}

/// Deezer's own queue keeps a download waiting, or holds it current while
/// no track finishes: both fail over with no blocklist entry, and the item
/// leaves Deemix's queue. The worker runs on a manual clock past the
/// shortest queue and stall timeouts.
#[tokio::test]
async fn a_deezer_queue_wait_and_stall_fail_over_without_blame() {
    // (album, its release, its artist, its tracklist, the verdict, its Deemix item)
    let cases = [
        (
            "In Rainbows",
            IN_RAINBOWS_RELEASE,
            "Radiohead",
            "deemix/tracklist_album.json",
            Reason::Stalled,
            "album_14880659_1",
        ),
        (
            "Mellon Collie and the Infinite Sadness",
            MELLON_COLLIE_RELEASE,
            "The Smashing Pumpkins",
            "deemix/tracklist_album_multidisc.json",
            Reason::QueueTimeout,
            "album_6158273_1",
        ),
    ];
    for (album, release, artist, tracklist, reason, uuid) in cases {
        let mock = signed_out_deemix();
        let search = mock.state().searches["Mellon Collie and the Infinite Sadness"].clone();
        mock.state().searches.insert(
            "The Smashing Pumpkins Mellon Collie and the Infinite Sadness".to_owned(),
            search,
        );
        let (url, _server) = serve_loopback(deemix(&mock, &[snapshots::TWO_ALBUMS[0]]))
            .await
            .expect("Deemix mock");
        let clock = ManualClock::at(1_800_000_000_000);
        let scratch = AcquireDb::scratch().expect("scratch db");
        let db = AcquireDb::new(scratch.db().clone(), Arc::new(clock.clone()));
        db.add_user("u1", "U", "user").await.expect("user seeds");
        let dir = db.path().parent().expect("scratch dir").to_path_buf();
        let platform = TestPlatform::on(db.db(), &dir)
            .clock(Arc::new(clock.clone()))
            .build()
            .await;
        let settings: &ConfigStore = &platform.settings;
        let mount = ScratchDir::new("deemix-watchdog");
        save_policy(settings, |policy| {
            policy.quality_min = "low".to_owned();
            policy.quality_recipe = recipe_for_range("low", "lossless", &[]);
            policy.search_review = "off".to_owned();
            policy.download_stall_timeout_minutes = 2;
            policy.download_queued_timeout_minutes = 5;
        });
        settings
            .save_blocking(DeemixConnection {
                enabled: true,
                url,
                downloads_mount: mount.display().to_string(),
            })
            .expect("Deemix saves");

        let journal = Arc::new(Journal::new(
            db.clone(),
            crate::common::bundles::quiet_ticks(),
        ));
        let mut albums = FixedAlbums::default();
        albums.releases.insert(
            release.to_owned(),
            album_release(release, album, artist, tracklist),
        );
        let targets = Arc::new(Targets::new(
            journal.clone(),
            Arc::new(albums),
            crate::common::bundles::closed_catalog(db.db()).await,
        ));
        let (library, _) = crate::common::library::library_at(&platform);
        let landing = Arc::new(LandingService::new(
            journal.clone(),
            Arc::new(LibraryLanding::new(
                &library,
                library.imports.releases.clone(),
            )),
            settings.live(),
            dir.join("held"),
            settings.live(),
            settings.live(),
        ));
        let worker = worker_with(
            &platform,
            crate::common::bundles::idle_host(&platform, library.core.clone()),
            journal.clone(),
            settings,
            dir.join("slskd"),
            targets,
            landing,
            Default::default(),
            droppedneedle_kernel::events::EventHub::new(),
        );
        let task_id = "0123456789abcdef0123456789abcdef";
        let (album_title, artist_name, release_mbid) =
            (album.to_owned(), artist.to_owned(), release.to_owned());
        journal
            .run("test.task", move |store| {
                store.insert_task(
                    &NewTask {
                        id: task_id.to_owned(),
                        user_id: "u1".to_owned(),
                        artist_name: artist_name.clone(),
                        album_title: album_title.clone(),
                        release_group_mbid: format!("{release_mbid}-group"),
                        origin: "user".to_owned(),
                        retry_count: 0,
                    },
                    1_000.0,
                )?;
                store.set_task_details(
                    task_id,
                    &TaskDetails {
                        release_mbid: Some(release_mbid.clone()),
                        ..TaskDetails::default()
                    },
                    1_000.0,
                )
            })
            .await
            .expect("task seeds");

        worker.run_once(1).await;
        assert_eq!(
            mock.calls_to("/api/addToQueue").len(),
            1,
            "{album} is added"
        );
        clock.advance(6 * 60 * 1000);
        worker.run_once(2).await;
        worker.wait_for_landings().await;

        let conn = rusqlite::Connection::open(db.path()).expect("database opens");
        let attempts: Vec<(String, String)> = conn
            .prepare("SELECT source, disposition FROM download_attempts")
            .expect("attempts query")
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .expect("attempts read")
            .collect::<Result<_, _>>()
            .expect("attempt rows");
        assert_eq!(
            attempts,
            [("deezer".to_owned(), "discard".to_owned())],
            "{album}: the attempt was given up"
        );
        let task: (String, Option<String>) = conn
            .query_row(
                "SELECT status, outcome_code FROM download_tasks",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .expect("task reads");
        assert_eq!(
            task,
            ("queued".to_owned(), Some(reason.code().to_owned())),
            "{album}: failed over"
        );
        let blocklisted: i64 = conn
            .query_row("SELECT COUNT(*) FROM download_quarantine", [], |row| {
                row.get(0)
            })
            .expect("quarantine counts");
        assert_eq!(blocklisted, 0, "{album}: nothing is blocklisted");
        assert!(
            mock.calls_to("/api/removeFromQueue").iter().any(|call| call
                .query
                .get("uuid")
                .map(String::as_str)
                == Some(uuid)),
            "{album} leaves Deemix's queue"
        );
        drop(scratch);
    }
}

/// A failed queue item of the shape Deemix answers, every track failing in
/// Deemix itself (an error naming no cause).
fn failed_in_deemix(uuid: &str) -> Value {
    json!({
        "queue": {uuid: {
            "uuid": uuid, "type": "album", "id": "14880659", "bitrate": 1,
            "status": "failed", "size": 10, "downloaded": 0, "failed": 1,
            "extrasPath": "/downloads//Radiohead - In Rainbows", "files": [], "errors": [{"message": "x", "type": "track", "data": {}}],
        }},
        "queueOrder": [],
    })
}

/// What the Deezer source remembers outlives a rebuild of the clients, as
/// another section's save makes one: an item removed to be added again,
/// whose add queued nothing, is added back after the rebuild rather than
/// read as dropped, and its one re-add is not given again.
#[tokio::test]
async fn a_rebuild_of_the_clients_keeps_what_the_deezer_source_remembers() {
    use droppedneedle_acquire::clients::LiveClients;
    use droppedneedle_acquire::downloads::state::AttemptState;
    use droppedneedle_acquire::downloads::{SourceError, SourceTag};
    use droppedneedle_acquire::wiring::ClientLives;
    use droppedneedle_providers::Providers;

    let mock = signed_out_deemix();
    let (url, _server) = serve_loopback(mock.router()).await.expect("Deemix mock");
    let clock = ManualClock::at(1_800_000_000_000);
    let scratch = AcquireDb::scratch().expect("scratch db");
    let db = AcquireDb::new(scratch.db().clone(), Arc::new(clock.clone()));
    db.add_user("u1", "U", "user").await.expect("user seeds");
    let dir = db.path().parent().expect("scratch dir").to_path_buf();
    let settings = droppedneedle_acquire::test_support::scratch_settings(&dir.join("settings"))
        .expect("settings");
    let mount = ScratchDir::new("deemix-memory");
    save_policy(&settings, |policy| {
        policy.quality_min = "low".to_owned();
        policy.quality_recipe = recipe_for_range("low", "lossless", &[]);
    });
    settings
        .save_blocking(DeemixConnection {
            enabled: true,
            url,
            downloads_mount: mount.display().to_string(),
        })
        .expect("Deemix saves");
    let journal = Arc::new(Journal::new(
        db.clone(),
        crate::common::bundles::quiet_ticks(),
    ));
    let mut albums = FixedAlbums::default();
    albums.releases.insert(
        IN_RAINBOWS_RELEASE.to_owned(),
        album_release(
            IN_RAINBOWS_RELEASE,
            "In Rainbows",
            "Radiohead",
            "deemix/tracklist_album.json",
        ),
    );
    let targets = Arc::new(Targets::new(
        journal.clone(),
        Arc::new(albums),
        crate::common::bundles::closed_catalog(db.db()).await,
    ));
    let http = droppedneedle_kernel::http_client::HttpClientFactory::new().expect("http");
    let clients = LiveClients::new(
        http.no_redirect().clone(),
        http.same_host_redirects().clone(),
        Providers::closed().newznab,
        ClientLives::new(&settings),
        dir.join("slskd"),
        journal.clone(),
        targets,
        crate::common::bundles::idle_host_over(&db).await,
        droppedneedle_acquire::test_support::deemix_wiring(
            &settings,
            Arc::new(clock.clone()),
            &Providers::closed(),
        )
        .expect("Deemix wiring"),
    );
    let deezer = || {
        clients
            .current()
            .sources
            .iter()
            .find(|source| source.tag() == SourceTag::Deezer)
            .cloned()
            .expect("a Deezer source")
    };
    let rebuild = |concurrent: i64| {
        save_policy(&settings, |policy| {
            policy.max_concurrent_downloads = concurrent;
        });
    };
    let queue = |answer: Value| {
        let mut state = mock.state();
        state.queue_walk.clear();
        state.queue = answer;
        drop(state);
        clock.advance(2_000);
    };
    let task_id = "0123456789abcdef0123456789abcdef";
    journal
        .run("test.task", move |store| {
            store.insert_task(
                &NewTask {
                    id: task_id.to_owned(),
                    user_id: "u1".to_owned(),
                    artist_name: "Radiohead".to_owned(),
                    album_title: "In Rainbows".to_owned(),
                    release_group_mbid: format!("{IN_RAINBOWS_RELEASE}-group"),
                    origin: "user".to_owned(),
                    retry_count: 0,
                },
                1_000.0,
            )?;
            store.set_task_details(
                task_id,
                &TaskDetails {
                    release_mbid: Some(IN_RAINBOWS_RELEASE.to_owned()),
                    ..TaskDetails::default()
                },
                1_000.0,
            )
        })
        .await
        .expect("task seeds");
    let source = deezer();
    let handle = source.enqueue(task_id, 0).await.expect("an add");
    let (job, encoded) = (
        handle.job_name.clone(),
        serde_json::to_string(&handle).expect("handle encodes"),
    );
    journal
        .run("test.attempt", move |store| {
            store.insert_attempt(
                "a1",
                task_id,
                "deezer",
                0,
                &job,
                &encoded,
                AttemptState::Acquiring,
                1.0,
            )
        })
        .await
        .expect("attempt journals");

    // Removed to be added again; the add queues nothing.
    mock.state().add_nothing = true;
    queue(failed_in_deemix("album_14880659_1"));
    assert!(matches!(
        source.poll(&handle).await,
        Err(SourceError::Unavailable(_))
    ));
    assert_eq!(mock.calls_to("/api/removeFromQueue").len(), 1);

    rebuild(2);
    let rebuilt = deezer();
    assert!(
        !Arc::ptr_eq(&source, &rebuilt),
        "the save rebuilt the clients"
    );
    mock.state().add_nothing = false;
    queue(serde_json::from_str(&fixture("deemix/getQueue_empty.json")).expect("queue"));
    let progress = rebuilt.poll(&handle).await.expect("added back");
    assert!(progress.queued_at_client);
    assert_eq!(mock.calls_to("/api/addToQueue").len(), 3);

    // Failing the same way after another rebuild, it is not added again.
    rebuild(3);
    queue(failed_in_deemix("album_14880659_1"));
    match deezer().poll(&handle).await {
        Err(SourceError::Rejected { reason, .. }) => assert_eq!(reason, Reason::LocalFault),
        other => panic!("expected a local fault, got {other:?}"),
    }
    assert_eq!(mock.calls_to("/api/removeFromQueue").len(), 1);
    assert_eq!(mock.calls_to("/api/addToQueue").len(), 3);
    drop(scratch);
}
