//! Compat client journeys through the real Subsonic and Jellyfin routers,
//! with the CORS and rate-limit layers, over the protocol fixture seams.
//!
//! Symfonium (Subsonic) and Finamp (Jellyfin) each run browse, stream,
//! favorite, playlist and scrobble; Jellify fetches `Latest` and plays the
//! stream URL with no auth headers (v2 serves compat audio anonymously).
//! Every step lands in a golden trace under `tests/fixtures/compat/`, next
//! to the pinned reference shapes. `COMPAT_BLESS=1` rewrites both; re-add
//! the `re:` markers for volatile leaves and review the diff before
//! committing.

use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::http::{HeaderMap, Request, StatusCode, header};
use axum::middleware::from_fn_with_state;
use droppedneedle::auth::compat_auth::fakes::FakeCompatPasswords;
use droppedneedle::compat::adapters::principal::{CompatProfile, CompatProfiles, SubsonicVerifier};
use droppedneedle::compat::http::{
    CompatLimits, StoreLabels, SubsonicState, cors_layer, limits_layer, redact_target,
    subsonic_router,
};
use droppedneedle::compat::jellyfin::seams::{
    AlbumView, ArtistView, GenreView, IdMap, JellyfinSettings, MemoryEngine, MemoryIds,
    MemoryLibrary, MemorySessions, SessionCall, TrackView,
};
use droppedneedle::compat::jellyfin::{JellyfinState, router as jellyfin_router};
use droppedneedle::compat::subsonic::Settings as SubsonicSettings;
use droppedneedle::compat::subsonic::fake::{FakeAudio, FakeStore};
use serde_json::{Value, json};
use tower::ServiceExt as _;

const ALICE_SECRET: &str = "alice-app-secret";
const ALICE_ID: &str = "user-alice";

fn seeded_passwords() -> FakeCompatPasswords {
    let store = FakeCompatPasswords::new();
    store.add_user(
        ALICE_ID,
        "alice",
        "Alice",
        "user",
        "alice-account-password-never-verifies",
        &[ALICE_SECRET],
    );
    store
}

/// Profile lookup behind the Subsonic verifier.
#[derive(Debug, Clone)]
struct JourneyProfiles;

impl CompatProfiles for JourneyProfiles {
    async fn profile(&self, user_id: &str) -> Option<CompatProfile> {
        (user_id == ALICE_ID).then(|| CompatProfile {
            user_id: ALICE_ID.to_owned(),
            username: "alice".to_owned(),
            username_display: "Alice".to_owned(),
            display_name: "Alice".to_owned(),
            is_admin: false,
        })
    }
}

fn subsonic_settings() -> SubsonicSettings {
    SubsonicSettings {
        enabled: true,
        server_name: "DroppedNeedle".to_owned(),
        server_version: "3.0.0-test".to_owned(),
        transcoding_enabled: true,
        transcode_default_format: "mp3".to_owned(),
        transcode_max_bitrate_kbps: 320,
        ffmpeg_available: true,
        base_url: String::new(),
    }
}

fn jellyfin_settings() -> JellyfinSettings {
    JellyfinSettings {
        enabled: true,
        server_name: "DroppedNeedle".to_owned(),
        server_version: "10.10.6".to_owned(),
        transcoding_enabled: true,
        transcode_max_bitrate_kbps: 320,
        transcode_default_format: "mp3".to_owned(),
        ffmpeg_available: true,
    }
}

/// Seeds the Jellyfin fixture library: two artists, two albums, three
/// tracks (mp3 1024 bytes, flac 512 bytes, mp3 256 bytes), one genre, art.
/// Shared with `compat_jellyfin`.
pub(crate) fn seed_jellyfin(library: &MemoryLibrary, engine: &MemoryEngine) {
    for (mbid, name, albums, added, album_artist) in [
        ("mb-a1", "Blue Giant", 2, 900.0, true),
        ("mb-a2", "Guest Star", 0, 950.0, false),
    ] {
        library.add_artist(
            ArtistView {
                artist_mbid: mbid.to_owned(),
                name: name.to_owned(),
                album_count: albums,
                date_added: Some(added),
                starred: false,
            },
            album_artist,
        );
    }
    for (rg, title, year, tracks, seconds, added, plays, last_played) in [
        (
            "rg-1",
            "First Light",
            2021,
            2,
            380.0,
            1000.0,
            5,
            Some(5000.0),
        ),
        ("rg-2", "Second Dawn", 2023, 1, 240.0, 2000.0, 0, None),
    ] {
        library.add_album(AlbumView {
            rg_mbid: rg.to_owned(),
            title: title.to_owned(),
            artist_name: Some("Blue Giant".to_owned()),
            artist_mbid: Some("mb-a1".to_owned()),
            year: Some(year),
            genre: Some("Rock".to_owned()),
            track_count: tracks,
            total_duration_seconds: Some(seconds),
            date_added: Some(added),
            starred: false,
            play_count: plays,
            last_played,
        });
    }
    let track = |id: &str, title: &str, rg: &str, album: &str, artist: (&str, &str)| TrackView {
        file_id: id.to_owned(),
        title: title.to_owned(),
        rg_mbid: Some(rg.to_owned()),
        album_title: Some(album.to_owned()),
        artist_mbid: Some(artist.0.to_owned()),
        artist_name: Some(artist.1.to_owned()),
        album_artist_mbid: Some("mb-a1".to_owned()),
        album_artist_name: Some("Blue Giant".to_owned()),
        disc_number: Some(1),
        genre: Some("Rock".to_owned()),
        ..TrackView::default()
    };
    let blue = ("mb-a1", "Blue Giant");
    library.add_track(TrackView {
        duration_seconds: Some(180.0),
        year: Some(2021),
        track_number: Some(1),
        file_format: Some("mp3".to_owned()),
        bitrate: Some(320),
        channels: Some(2),
        sample_rate: Some(44100),
        file_size_bytes: Some(1024),
        created_at: Some(1000.0),
        recording_mbid: Some("rec-1".to_owned()),
        play_count: 5,
        last_played: Some(5000.0),
        ..track("f1", "Opener", "rg-1", "First Light", blue)
    });
    library.add_track(TrackView {
        duration_seconds: Some(200.0),
        year: Some(2021),
        track_number: Some(2),
        file_format: Some("flac".to_owned()),
        bitrate: Some(900),
        file_size_bytes: Some(512),
        created_at: Some(1100.0),
        ..track("f2", "Deep Cut", "rg-1", "First Light", blue)
    });
    library.add_track(TrackView {
        duration_seconds: Some(240.0),
        year: Some(2023),
        track_number: Some(1),
        file_format: Some("mp3".to_owned()),
        bitrate: Some(128),
        file_size_bytes: Some(256),
        created_at: Some(2000.0),
        ..track(
            "f3",
            "Late Bloomer",
            "rg-2",
            "Second Dawn",
            ("mb-a2", "Guest Star"),
        )
    });
    library.add_genre(GenreView {
        name: "Rock".to_owned(),
        song_count: 3,
    });
    library.add_cover("rg-1", "500", b"cover-500-bytes".to_vec(), "image/jpeg");
    library.add_cover("rg-1", "250", b"cover-250-bytes".to_vec(), "image/jpeg");
    library.add_artist_image("mb-a1", b"artist-bytes".to_vec(), "image/jpeg");
    engine.add_file(
        "f1",
        (0..1024).map(|i| (i % 251) as u8).collect(),
        Some("mp3"),
    );
    engine.add_file("f2", vec![7u8; 512], Some("flac"));
    engine.add_file("f3", vec![9u8; 256], Some("mp3"));
}

/// One layered app per protocol with fresh limits and fixtures.
struct Apps {
    subsonic: Router,
    jellyfin: Router,
    sessions: MemorySessions,
    ids: MemoryIds,
}

fn apps() -> Apps {
    let passwords = seeded_passwords();
    // One bucket set shared by the layer and the dispatch state, as in
    // production: denials recorded after verify feed the layer's pre-check.
    let subsonic_limits = CompatLimits::new(
        Arc::new(StoreLabels::new(passwords.clone())),
        subsonic_settings(),
    );
    let subsonic_state = SubsonicState::new(
        SubsonicVerifier::new(passwords.clone(), JourneyProfiles),
        FakeStore::loaded(),
        FakeAudio,
        subsonic_settings(),
        subsonic_limits.limits.clone(),
        subsonic_limits.started.clone(),
    );
    let subsonic = Router::new()
        .merge(subsonic_router(subsonic_state))
        .layer(from_fn_with_state(subsonic_limits, limits_layer))
        .layer(axum::middleware::from_fn(cors_layer));

    let library = MemoryLibrary::new();
    let engine = MemoryEngine::new();
    seed_jellyfin(&library, &engine);
    let sessions = MemorySessions::new();
    let ids = MemoryIds::new();
    let jellyfin_state = JellyfinState::new(
        passwords.clone(),
        library,
        engine,
        sessions.clone(),
        ids.clone(),
        jellyfin_settings(),
    );
    // Jellyfin rejects are empty 429s that never read the limit settings,
    // so the Subsonic settings serve here too.
    let jellyfin = Router::new()
        .nest("/jellyfin", jellyfin_router(jellyfin_state))
        .layer(from_fn_with_state(
            CompatLimits::new(Arc::new(StoreLabels::new(passwords)), subsonic_settings()),
            limits_layer,
        ))
        .layer(axum::middleware::from_fn(cors_layer));

    Apps {
        subsonic,
        jellyfin,
        sessions,
        ids,
    }
}

// --- Trace recorder and golden comparison ---

struct Trace {
    steps: Vec<Value>,
}

impl Trace {
    fn new() -> Self {
        Self { steps: Vec::new() }
    }

    fn body(&self) -> Value {
        json!({ "steps": self.steps })
    }

    async fn get(&mut self, app: &Router, uri: &str, headers: &[(&str, &str)]) -> Value {
        self.call(app, "GET", uri, headers, None).await
    }

    /// Sends one request and records status, framing headers and body.
    /// Returns the recorded (masked) body.
    async fn call(
        &mut self,
        app: &Router,
        method: &str,
        uri: &str,
        headers: &[(&str, &str)],
        json_body: Option<Value>,
    ) -> Value {
        let mut builder = Request::builder().method(method).uri(uri);
        for (name, value) in headers {
            builder = builder.header(*name, *value);
        }
        let request = match json_body {
            Some(payload) => builder
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(payload.to_string())),
            None => builder.body(Body::empty()),
        }
        .expect("request builds");
        let response = app.clone().oneshot(request).await.expect("router responds");
        let status = response.status();
        let header_map = response.headers().clone();
        let is_json = header_map
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|ct| ct.contains("json"));
        let raw = axum::body::to_bytes(response.into_body(), 8 * 1024 * 1024)
            .await
            .expect("body reads");
        let mut headers_out = serde_json::Map::new();
        for name in [
            "content-type",
            "content-length",
            "content-range",
            "accept-ranges",
        ] {
            if let Some(value) = header_map.get(name).and_then(|v| v.to_str().ok()) {
                headers_out.insert(name.to_owned(), Value::String(value.to_owned()));
            }
        }
        let mut body = if raw.is_empty() {
            Value::Null
        } else if is_json {
            serde_json::from_slice(&raw).expect("json parses")
        } else {
            json!({ "byte_len": raw.len(), "head": raw.iter().take(8).collect::<Vec<_>>() })
        };
        normalize_volatile(&mut body);
        mask_api_keys(&mut body);
        // The URI goes through the production redactor so blessed traces
        // never carry the fixture credential.
        self.steps.push(json!({
            "request": format!("{method} {}", redact_target(uri)),
            "status": status.as_u16(),
            "headers": headers_out,
            "body": body,
        }));
        self.steps
            .last()
            .map_or(Value::Null, |step| step["body"].clone())
    }
}

/// `minutesAgo` comes from the wall clock; the trace pins its presence only.
fn normalize_volatile(body: &mut Value) {
    match body {
        Value::Object(map) => {
            for (key, value) in map.iter_mut() {
                if key == "minutesAgo" {
                    *value = Value::String("__MINUTES_AGO__".to_owned());
                } else {
                    normalize_volatile(value);
                }
            }
        }
        Value::Array(items) => items.iter_mut().for_each(normalize_volatile),
        _ => {}
    }
}

/// PlaybackInfo advertises `DirectStreamUrl` with `api_key=<token>`; every
/// such value is masked as `***` before it is recorded.
fn mask_api_keys(body: &mut Value) {
    match body {
        Value::String(text) => {
            let mut out = String::with_capacity(text.len());
            let mut rest = text.as_str();
            while let Some(start) = rest.find("api_key=") {
                out.push_str(&rest[..start + "api_key=".len()]);
                out.push_str("***");
                rest = &rest[start + "api_key=".len()..];
                rest = rest.find('&').map_or("", |end| &rest[end..]);
            }
            out.push_str(rest);
            *text = out;
        }
        Value::Array(items) => items.iter_mut().for_each(mask_api_keys),
        Value::Object(map) => map.values_mut().for_each(mask_api_keys),
        _ => {}
    }
}

fn sub_auth(client: &str) -> String {
    format!("u=alice&p={ALICE_SECRET}&v=1.16.1&c={client}&f=json")
}

fn jf_auth_value() -> String {
    format!("MediaBrowser Client=\"Journey\", Token=\"{ALICE_SECRET}\"")
}

/// Subsonic URL for `call` (`endpoint` or `endpoint?query`) plus credentials.
fn rest(call: &str, auth: &str) -> String {
    let sep = if call.contains('?') { '&' } else { '?' };
    format!("/subsonic/rest/{call}{sep}{auth}")
}

fn fixtures_dir() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/compat")
}

/// Compares actual against expected. Leaves must match exactly, except a
/// string starting with `re:`, which is a regex the whole actual string
/// must match. Returns the drifted paths.
fn diff_golden(expected: &Value, actual: &Value, path: &str, out: &mut Vec<String>) {
    match (expected, actual) {
        (Value::Object(exp), Value::Object(got)) => {
            let mut keys: Vec<&String> = exp.keys().chain(got.keys()).collect();
            keys.sort();
            keys.dedup();
            for key in keys {
                let at = format!("{path}.{key}");
                match (exp.get(key), got.get(key)) {
                    (Some(e), Some(a)) => diff_golden(e, a, &at, out),
                    (Some(_), None) => out.push(format!("{at}: missing in actual")),
                    _ => out.push(format!("{at}: unexpected in actual")),
                }
            }
        }
        (Value::Array(exp), Value::Array(got)) if exp.len() == got.len() => {
            for (i, (e, a)) in exp.iter().zip(got).enumerate() {
                diff_golden(e, a, &format!("{path}[{i}]"), out);
            }
        }
        (Value::String(pattern), Value::String(text)) if pattern.starts_with("re:") => {
            let regex = regex::Regex::new(&format!("^(?:{})$", &pattern[3..])).expect("regex");
            if !regex.is_match(text) {
                out.push(format!("{path}: {text:?} does not match {pattern:?}"));
            }
        }
        (exp, got) if exp != got => out.push(format!("{path}: {got} != {exp}")),
        _ => {}
    }
}

/// Asserts `actual` matches the named fixture, or rewrites it under
/// `COMPAT_BLESS=1`.
fn assert_golden(name: &str, actual: &Value) {
    let path = fixtures_dir().join(name);
    if std::env::var("COMPAT_BLESS").is_ok() {
        let text = serde_json::to_string_pretty(actual).expect("serializes");
        std::fs::write(path, format!("{text}\n")).expect("bless writes");
        return;
    }
    let raw = std::fs::read_to_string(path).expect("golden reads");
    let expected: Value = serde_json::from_str(&raw).expect("golden parses");
    let mut drift = Vec::new();
    diff_golden(&expected, actual, "$", &mut drift);
    assert!(drift.is_empty(), "{name} drifted:\n{}", drift.join("\n"));
}

// --- Journeys ---

#[tokio::test]
async fn symfonium_browse_stream_favorite_playlist_scrobble() {
    let apps = apps();
    let app = &apps.subsonic;
    let mut trace = Trace::new();
    let auth = sub_auth("symfonium");

    trace.get(app, &rest("ping", &auth), &[]).await;
    trace
        .get(app, &rest("getAlbumList2?type=newest", &auth), &[])
        .await;
    let album = trace
        .get(app, &rest("getAlbum?id=al-rg-1", &auth), &[])
        .await;
    assert_eq!(album["subsonic-response"]["album"]["songCount"], json!(2));
    let full = trace.get(app, &rest("stream?id=tr-f1", &auth), &[]).await;
    assert_eq!(full["byte_len"], json!(100));
    let slice = trace
        .get(
            app,
            &rest("stream?id=tr-f1", &auth),
            &[("Range", "bytes=0-49")],
        )
        .await;
    assert_eq!(slice["byte_len"], json!(50));
    trace.get(app, &rest("star?id=tr-f1", &auth), &[]).await;
    let starred = trace.get(app, &rest("getStarred2", &auth), &[]).await;
    assert_eq!(
        starred["subsonic-response"]["starred2"]["song"][0]["id"],
        json!("tr-f1")
    );
    let created = trace
        .get(
            app,
            &rest("createPlaylist?name=Journey+Mix&songId=tr-f1", &auth),
            &[],
        )
        .await;
    let playlist_id = created["subsonic-response"]["playlist"]["id"]
        .as_str()
        .expect("playlist id")
        .to_owned();
    trace
        .get(
            app,
            &rest(&format!("getPlaylist?id={playlist_id}"), &auth),
            &[],
        )
        .await;
    for call in [
        "scrobble?id=tr-f1&submission=false",
        "getNowPlaying",
        "scrobble?id=tr-f1&submission=true",
    ] {
        trace.get(app, &rest(call, &auth), &[]).await;
    }

    assert_golden("self_golden_symfonium.trace.json", &trace.body());
}

#[tokio::test]
async fn finamp_browse_stream_favorite_playlist_progress() {
    let apps = apps();
    let app = &apps.jellyfin;
    let mut trace = Trace::new();
    let auth_value = jf_auth_value();
    let auth = [("X-Emby-Authorization", auth_value.as_str())];
    let user = ALICE_ID;

    trace.get(app, "/jellyfin/System/Info/Public", &[]).await;
    trace
        .get(app, &format!("/jellyfin/Users/{user}/Views"), &auth)
        .await;
    let items = trace
        .get(
            app,
            &format!("/jellyfin/Users/{user}/Items?includeItemTypes=Audio"),
            &auth,
        )
        .await;
    let track_id = items["Items"][0]["Id"]
        .as_str()
        .expect("track id")
        .to_owned();
    let info = trace
        .get(
            app,
            &format!("/jellyfin/Items/{track_id}/PlaybackInfo"),
            &auth,
        )
        .await;
    let stream_url = info["MediaSources"][0]["DirectStreamUrl"]
        .as_str()
        .and_then(|url| url.strip_prefix("http://localhost"))
        .expect("direct stream url")
        .to_owned();
    let full = trace.get(app, &stream_url, &auth).await;
    assert_eq!(full["byte_len"], json!(1024));
    let slice = trace
        .get(app, &stream_url, &[("Range", "bytes=0-99")])
        .await;
    assert_eq!(slice["byte_len"], json!(100));

    let favorite = format!("/jellyfin/Users/{user}/FavoriteItems/{track_id}");
    trace.call(app, "POST", &favorite, &auth, None).await;
    let favorites = trace
        .get(
            app,
            &format!("/jellyfin/Users/{user}/Items?includeItemTypes=Audio&IsFavorite=true"),
            &auth,
        )
        .await;
    assert_eq!(favorites["Items"][0]["Id"], json!(track_id));

    let created = trace
        .call(
            app,
            "POST",
            "/jellyfin/Playlists",
            &auth,
            Some(json!({ "Name": "Journey Mix", "Ids": [track_id] })),
        )
        .await;
    let playlist_id = created["Id"].as_str().expect("playlist id").to_owned();
    let entries = trace
        .get(
            app,
            &format!("/jellyfin/Playlists/{playlist_id}/Items"),
            &auth,
        )
        .await;
    assert_eq!(entries["Items"][0]["Id"], json!(track_id));

    for endpoint in ["Playing", "Playing/Progress"] {
        let uri = format!("/jellyfin/Sessions/{endpoint}");
        let body = json!({ "ItemId": track_id });
        trace.call(app, "POST", &uri, &auth, Some(body)).await;
    }
    let stopped = json!({
        "ItemId": track_id,
        "PositionTicks": 1_700_000_000_i64,
        "RunTimeTicks": 1_800_000_000_i64,
    });
    trace
        .call(
            app,
            "POST",
            "/jellyfin/Sessions/Playing/Stopped",
            &auth,
            Some(stopped),
        )
        .await;
    assert!(
        apps.sessions.calls().iter().any(|call| matches!(
            call,
            SessionCall::Scrobble { user_id, file_id } if user_id == ALICE_ID && file_id == "f1"
        )),
        "stopped past the threshold scrobbles"
    );

    assert_golden("self_golden_finamp.trace.json", &trace.body());
}

#[tokio::test]
async fn jellify_latest_then_headerless_play() {
    let apps = apps();
    let app = &apps.jellyfin;
    let mut trace = Trace::new();
    let auth_value = jf_auth_value();
    let auth = [("X-Emby-Authorization", auth_value.as_str())];

    let latest = trace.get(app, "/jellyfin/UserItems/Latest", &auth).await;
    assert_eq!(latest[0]["Type"], json!("MusicAlbum"));
    let album_id = latest[0]["Id"].as_str().expect("album id");
    let tracks = trace
        .get(
            app,
            &format!("/jellyfin/Users/{ALICE_ID}/Items?parentId={album_id}&includeItemTypes=Audio"),
            &auth,
        )
        .await;
    let track_id = tracks["Items"][0]["Id"].as_str().expect("track id");
    let stream_url = format!("/jellyfin/Audio/{track_id}/stream.mp3?static=true");

    // No auth headers from here on.
    let full = trace.get(app, &stream_url, &[]).await;
    assert_eq!(full["byte_len"], json!(256));
    let slice = trace
        .get(app, &stream_url, &[("Range", "bytes=100-199")])
        .await;
    assert_eq!(slice["byte_len"], json!(100));
    let head = trace.call(app, "HEAD", &stream_url, &[], None).await;
    assert!(head.is_null(), "HEAD has no body");
    let universal = stream_url.replace("/stream.mp3", "/universal");
    trace.get(app, &universal, &[]).await;

    assert_golden("self_golden_jellify.trace.json", &trace.body());
}

#[tokio::test]
async fn subsonic_pinned_reference_shapes() {
    let apps = apps();
    let app = &apps.subsonic;
    let mut trace = Trace::new();
    let auth = sub_auth("pin-check");

    for (call, pinned) in [
        ("ping", "pinned_subsonic_ping.json"),
        (
            "getAlbumList2?type=newest",
            "pinned_subsonic_album_list2.json",
        ),
        ("getSong?id=tr-f1", "pinned_subsonic_song.json"),
    ] {
        assert_golden(pinned, &trace.get(app, &rest(call, &auth), &[]).await);
    }
    trace.get(app, &rest("star?id=tr-f1", &auth), &[]).await;
    let starred = trace.get(app, &rest("getStarred2", &auth), &[]).await;
    assert_golden("pinned_subsonic_starred2.json", &starred);
    let created = trace
        .get(
            app,
            &rest("createPlaylist?name=Journey+Mix&songId=tr-f1", &auth),
            &[],
        )
        .await;
    let playlist_id = created["subsonic-response"]["playlist"]["id"]
        .as_str()
        .expect("playlist id")
        .to_owned();
    let playlist = trace
        .get(
            app,
            &rest(&format!("getPlaylist?id={playlist_id}"), &auth),
            &[],
        )
        .await;
    assert_golden("pinned_subsonic_playlist.json", &playlist);
}

#[tokio::test]
async fn jellyfin_pinned_reference_shapes() {
    let apps = apps();
    let app = &apps.jellyfin;
    let mut trace = Trace::new();
    let auth_value = jf_auth_value();
    let auth = [("X-Emby-Authorization", auth_value.as_str())];
    let user = ALICE_ID;

    let public = trace.get(app, "/jellyfin/System/Info/Public", &[]).await;
    assert_golden("pinned_jellyfin_public_info.json", &public);
    for (uri, pinned) in [
        (
            format!("/jellyfin/Users/{user}/Views"),
            "pinned_jellyfin_views.json",
        ),
        (
            format!("/jellyfin/Users/{user}/Items"),
            "pinned_jellyfin_items.json",
        ),
    ] {
        assert_golden(pinned, &trace.get(app, &uri, &auth).await);
    }
    let latest = trace.get(app, "/jellyfin/UserItems/Latest", &auth).await;
    assert_golden("pinned_jellyfin_latest.json", &latest);

    let album_id = latest[0]["Id"].as_str().expect("album id");
    let tracks = trace
        .get(
            app,
            &format!("/jellyfin/Users/{user}/Items?parentId={album_id}&includeItemTypes=Audio"),
            &auth,
        )
        .await;
    let track_id = tracks["Items"][0]["Id"]
        .as_str()
        .expect("track id")
        .to_owned();
    let info = trace
        .get(
            app,
            &format!("/jellyfin/Items/{track_id}/PlaybackInfo"),
            &auth,
        )
        .await;
    assert_golden("pinned_jellyfin_playback_info.json", &info);

    let created = trace
        .call(
            app,
            "POST",
            "/jellyfin/Playlists",
            &auth,
            Some(json!({ "Name": "Journey Mix", "Ids": [track_id] })),
        )
        .await;
    let playlist_id = created["Id"].as_str().expect("playlist id");
    let entries = trace
        .get(
            app,
            &format!("/jellyfin/Playlists/{playlist_id}/Items"),
            &auth,
        )
        .await;
    assert_golden("pinned_jellyfin_playlist_items.json", &entries);
}

// --- Streaming semantics ---

async fn raw_call(
    app: &Router,
    method: &str,
    uri: &str,
    headers: &[(&str, &str)],
) -> (StatusCode, HeaderMap, Vec<u8>) {
    let mut builder = Request::builder().method(method).uri(uri);
    for (name, value) in headers {
        builder = builder.header(*name, *value);
    }
    let request = builder.body(Body::empty()).expect("request builds");
    let response = app.clone().oneshot(request).await.expect("router responds");
    let status = response.status();
    let map = response.headers().clone();
    let bytes = axum::body::to_bytes(response.into_body(), 8 * 1024 * 1024)
        .await
        .expect("body reads")
        .to_vec();
    (status, map, bytes)
}

fn header_str(map: &HeaderMap, name: &str) -> String {
    map.get(name)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_owned()
}

/// Identity, Range and HEAD semantics, identical on both protocols: HEAD
/// answers exactly what GET would, minus the body.
#[tokio::test]
async fn streaming_ranges_and_head_on_both_protocols() {
    let apps = apps();
    let auth = sub_auth("matrix");
    let flac = apps.ids.to_jf("track", "f2").await;
    let cases = [
        (
            &apps.subsonic,
            format!("/subsonic/rest/stream?id=tr-f1&{auth}"),
            100u64,
            "audio/mpeg",
        ),
        (
            &apps.jellyfin,
            format!("/jellyfin/Audio/{flac}/stream"),
            512,
            "audio/flac",
        ),
    ];
    for (app, url, total, content_type) in cases {
        let (status, headers, bytes) = raw_call(app, "GET", &url, &[]).await;
        assert_eq!(status, StatusCode::OK, "{url}");
        assert_eq!(header_str(&headers, "content-type"), content_type);
        assert_eq!(header_str(&headers, "accept-ranges"), "bytes");
        assert_eq!(header_str(&headers, "content-length"), total.to_string());
        assert_eq!(bytes.len() as u64, total);
        let (status, headers, bytes) = raw_call(app, "HEAD", &url, &[]).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(header_str(&headers, "content-length"), total.to_string());
        assert!(bytes.is_empty());

        let last = total - 1;
        let unsatisfiable = format!("bytes */{total}");
        let past_end = format!("bytes={total}-");
        let ranges: [(&str, u16, String, u64); 9] = [
            ("bytes=10-19", 206, format!("bytes 10-19/{total}"), 10),
            ("bytes=0-1 ", 206, format!("bytes 0-1/{total}"), 2),
            (
                "bytes=-4",
                206,
                format!("bytes {}-{last}/{total}", total - 4),
                4,
            ),
            (
                "bytes=90-",
                206,
                format!("bytes 90-{last}/{total}"),
                total - 90,
            ),
            (&past_end, 416, unsatisfiable.clone(), 0),
            ("bytes=0-0,2-3", 416, unsatisfiable.clone(), 0),
            ("bytes=-0", 416, unsatisfiable.clone(), 0),
            ("bytes=abc", 416, unsatisfiable.clone(), 0),
            ("items=0-1", 416, unsatisfiable.clone(), 0),
        ];
        for method in ["GET", "HEAD"] {
            for (range, want, content_range, len) in &ranges {
                let (status, headers, bytes) =
                    raw_call(app, method, &url, &[("Range", range)]).await;
                let case = format!("{method} {url} {range:?}");
                assert_eq!(status.as_u16(), *want, "{case}");
                assert_eq!(
                    header_str(&headers, "content-range"),
                    *content_range,
                    "{case}"
                );
                if *want == 206 {
                    assert_eq!(
                        header_str(&headers, "content-length"),
                        len.to_string(),
                        "{case}"
                    );
                }
                let body_len = if method == "GET" { *len } else { 0 };
                assert_eq!(bytes.len() as u64, body_len, "{case}");
            }
        }
    }

    // Unknown ids are plain 404s; Subsonic's binary path answers as text.
    let missing = format!("/subsonic/rest/stream?id=tr-nope&{auth}");
    let (status, headers, _) = raw_call(&apps.subsonic, "GET", &missing, &[]).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(header_str(&headers, "content-type"), "text/plain");
    let (status, _, _) = raw_call(&apps.jellyfin, "GET", "/jellyfin/Audio/nope/stream", &[]).await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // The container suffix is cosmetic and `universal` serves the same bytes.
    let (_, _, plain) = raw_call(
        &apps.jellyfin,
        "GET",
        &format!("/jellyfin/Audio/{flac}/stream"),
        &[],
    )
    .await;
    for tail in ["stream.mp3", "universal"] {
        let uri = format!("/jellyfin/Audio/{flac}/{tail}");
        let (status, _, bytes) = raw_call(&apps.jellyfin, "GET", &uri, &[]).await;
        assert_eq!(status, StatusCode::OK, "{tail}");
        assert_eq!(bytes, plain, "{tail}");
    }
}

/// Media paths skip the token buckets but not the auth-failure lockout, so
/// guessing passwords over `stream` still trips the backoff.
#[tokio::test]
async fn media_path_auth_lockout_blocks_brute_force() {
    let apps = apps();
    for _ in 0..5 {
        raw_call(
            &apps.subsonic,
            "GET",
            "/subsonic/rest/stream?id=tr-f1&u=alice&p=wrong&v=1.16.1&c=probe&f=json",
            &[],
        )
        .await;
    }
    let url = rest("stream?id=tr-f1", &sub_auth("probe"));
    let (status, headers, bytes) = raw_call(&apps.subsonic, "GET", &url, &[]).await;
    assert_eq!(status, StatusCode::OK, "Subsonic rejects stay HTTP 200");
    assert!(!header_str(&headers, "retry-after").is_empty());
    let reject: Value = serde_json::from_slice(&bytes).expect("reject json");
    assert_eq!(reject["subsonic-response"]["error"]["code"], json!(0));
}

/// Tripwire: no committed fixture may contain the fixture credential.
#[test]
fn blessed_fixtures_carry_no_credentials() {
    for entry in std::fs::read_dir(fixtures_dir()).expect("fixtures list") {
        let path = entry.expect("entry").path();
        let text = std::fs::read_to_string(&path).expect("fixture reads");
        assert!(
            !text.contains(ALICE_SECRET),
            "{} commits the fixture credential",
            path.display()
        );
    }
}
