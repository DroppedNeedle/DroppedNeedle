//! Compat journeys (stage 9): three client-shaped lifecycles through the
//! REAL compat routers, plus the golden-trace corpus and the
//! streaming-semantics briefs shared with stage 6.
//!
//! Symfonium over Subsonic and Finamp over Jellyfin each run browse →
//! stream → favorite → playlist → scrobble-report, while Jellify fetches
//! `Latest` and plays the stream URL with no auth headers at all (pinned v2
//! behavior: anonymous audio). Both apps mount through the CORS + limits
//! layers (NOT the full production edge: case-insensitive paths live in the
//! `create_app` fallbacks, pinned by `compat_wiring.rs` instead); only the
//! data seams are fixtures (the protocol slices' own fakes plus the stage-3
//! fake password store).
//!
//! What compat is allowed to mutate (fixture-scoped only): favorites,
//! playlists, queues, bookmarks, and presence. The library fixtures are
//! static, so there is no catalog to pollute.
//!
//! Golden traces live in `tests/fixtures/compat/`. `COMPAT_BLESS=1`
//! regenerates self-golden traces AND pinned references; inspect the diff
//! before committing.

use std::collections::BTreeMap;
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
use droppedneedle::stream::routes::{
    AudioSource, OpenMedia, StreamEngine, StreamFault, StreamOpen, StreamParams,
    content_type_for_extension, parse_range,
};
use serde_json::{Value, json};
use tower::ServiceExt as _;

// ---------------------------------------------------------------------------
// Fixture seams: stage-3 fake passwords, slice fakes, one test profile lookup
// ---------------------------------------------------------------------------

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

/// Test profile lookup behind the Subsonic verifier.
#[derive(Debug, Clone)]
struct JourneyProfiles;

impl CompatProfiles for JourneyProfiles {
    async fn profile(&self, user_id: &str) -> Option<CompatProfile> {
        if user_id == ALICE_ID {
            Some(CompatProfile {
                user_id: ALICE_ID.to_owned(),
                username: "alice".to_owned(),
                username_display: "Alice".to_owned(),
                display_name: "Alice".to_owned(),
                is_admin: false,
            })
        } else {
            None
        }
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

/// One wired app per protocol, sharing nothing across tests (fresh limits,
/// fresh fixtures). Returns the routers plus the seam handles journeys
/// assert against.
struct Apps {
    subsonic: Router,
    jellyfin: Router,
    sessions: MemorySessions,
    ids: MemoryIds,
}

fn seed_jellyfin(library: &MemoryLibrary, engine: &MemoryEngine) {
    library.add_artist(
        ArtistView {
            artist_mbid: "mb-a1".to_owned(),
            name: "Blue Giant".to_owned(),
            album_count: 2,
            date_added: Some(900.0),
            starred: false,
        },
        true,
    );
    library.add_artist(
        ArtistView {
            artist_mbid: "mb-a2".to_owned(),
            name: "Guest Star".to_owned(),
            album_count: 0,
            date_added: Some(950.0),
            starred: false,
        },
        false,
    );
    library.add_album(AlbumView {
        rg_mbid: "rg-1".to_owned(),
        title: "First Light".to_owned(),
        artist_name: Some("Blue Giant".to_owned()),
        artist_mbid: Some("mb-a1".to_owned()),
        year: Some(2021),
        genre: Some("Rock".to_owned()),
        track_count: 2,
        total_duration_seconds: Some(380.0),
        date_added: Some(1000.0),
        starred: false,
        play_count: 5,
        last_played: Some(5000.0),
    });
    library.add_album(AlbumView {
        rg_mbid: "rg-2".to_owned(),
        title: "Second Dawn".to_owned(),
        artist_name: Some("Blue Giant".to_owned()),
        artist_mbid: Some("mb-a1".to_owned()),
        year: Some(2023),
        genre: Some("Rock".to_owned()),
        track_count: 1,
        total_duration_seconds: Some(240.0),
        date_added: Some(2000.0),
        starred: false,
        play_count: 0,
        last_played: None,
    });
    let mut t1 = TrackView {
        file_id: "f1".to_owned(),
        title: "Opener".to_owned(),
        rg_mbid: Some("rg-1".to_owned()),
        artist_mbid: Some("mb-a1".to_owned()),
        album_artist_mbid: Some("mb-a1".to_owned()),
        ..TrackView::default()
    };
    t1.album_title = Some("First Light".to_owned());
    t1.artist_name = Some("Blue Giant".to_owned());
    t1.album_artist_name = Some("Blue Giant".to_owned());
    t1.duration_seconds = Some(180.0);
    t1.year = Some(2021);
    t1.track_number = Some(1);
    t1.disc_number = Some(1);
    t1.genre = Some("Rock".to_owned());
    t1.file_format = Some("mp3".to_owned());
    t1.bitrate = Some(320);
    t1.channels = Some(2);
    t1.sample_rate = Some(44100);
    t1.file_size_bytes = Some(1024);
    t1.created_at = Some(1000.0);
    t1.recording_mbid = Some("rec-1".to_owned());
    t1.play_count = 5;
    t1.last_played = Some(5000.0);
    library.add_track(t1);
    let mut t2 = TrackView {
        file_id: "f2".to_owned(),
        title: "Deep Cut".to_owned(),
        rg_mbid: Some("rg-1".to_owned()),
        artist_mbid: Some("mb-a1".to_owned()),
        album_artist_mbid: Some("mb-a1".to_owned()),
        ..TrackView::default()
    };
    t2.album_title = Some("First Light".to_owned());
    t2.artist_name = Some("Blue Giant".to_owned());
    t2.album_artist_name = Some("Blue Giant".to_owned());
    t2.duration_seconds = Some(200.0);
    t2.year = Some(2021);
    t2.track_number = Some(2);
    t2.disc_number = Some(1);
    t2.genre = Some("Rock".to_owned());
    t2.file_format = Some("flac".to_owned());
    t2.bitrate = Some(900);
    t2.file_size_bytes = Some(512);
    t2.created_at = Some(1100.0);
    library.add_track(t2);
    let mut t3 = TrackView {
        file_id: "f3".to_owned(),
        title: "Late Bloomer".to_owned(),
        rg_mbid: Some("rg-2".to_owned()),
        artist_mbid: Some("mb-a2".to_owned()),
        album_artist_mbid: Some("mb-a1".to_owned()),
        ..TrackView::default()
    };
    t3.album_title = Some("Second Dawn".to_owned());
    t3.artist_name = Some("Guest Star".to_owned());
    t3.album_artist_name = Some("Blue Giant".to_owned());
    t3.duration_seconds = Some(240.0);
    t3.year = Some(2023);
    t3.track_number = Some(1);
    t3.disc_number = Some(1);
    t3.genre = Some("Rock".to_owned());
    t3.file_format = Some("mp3".to_owned());
    t3.bitrate = Some(128);
    t3.file_size_bytes = Some(256);
    t3.created_at = Some(2000.0);
    library.add_track(t3);
    library.add_genre(GenreView {
        name: "Rock".to_owned(),
        song_count: 3,
    });
    library.add_cover("rg-1", "500", b"cover-500-bytes".to_vec(), "image/jpeg");
    library.add_artist_image("mb-a1", b"artist-bytes".to_vec(), "image/jpeg");
    let bytes1: Vec<u8> = (0..1024).map(|i| (i % 251) as u8).collect();
    engine.add_file("f1", bytes1, Some("mp3"));
    engine.add_file("f2", vec![7u8; 512], Some("flac"));
    engine.add_file("f3", vec![9u8; 256], Some("mp3"));
}

fn apps() -> Apps {
    let passwords = seeded_passwords();
    // One bucket set shared by the layer and the dispatch state, exactly
    // like production `CompatSetup::assemble`: denials recorded
    // post-verify must feed the pre-check the layer reads.
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
    // The limits settings only shape Subsonic limit envelopes (Jellyfin
    // rejects are empty 429s that never consult them), so the Jellyfin
    // app reuses the shared Subsonic settings deliberately.
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

// ---------------------------------------------------------------------------
// Trace recorder: every journey step becomes golden JSON
// ---------------------------------------------------------------------------

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

    /// One request through a cloned app router (state is shared, so
    /// mutations persist across steps).
    async fn call(
        &mut self,
        app: &Router,
        method: &str,
        uri: &str,
        headers: &[(&str, &str)],
        json_body: Option<Value>,
    ) -> (StatusCode, Value) {
        let mut builder = Request::builder().method(method).uri(uri);
        for (name, value) in headers {
            builder = builder.header(*name, *value);
        }
        let request = if let Some(payload) = json_body {
            builder
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(payload.to_string()))
        } else {
            builder.body(Body::empty())
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
        // Header sidecar: the wire framing every golden step pins
        // alongside status and body (absent headers stay absent).
        let mut headers_out = serde_json::Map::new();
        for name in [
            "content-type",
            "content-length",
            "content-range",
            "accept-ranges",
        ] {
            if let Some(value) = header_map
                .get(name)
                .and_then(|v| v.to_str().ok())
                .map(str::to_owned)
            {
                headers_out.insert(name.to_owned(), Value::String(value));
            }
        }
        let mut body = if is_json && !raw.is_empty() {
            serde_json::from_slice(&raw).expect("json parses")
        } else if raw.is_empty() {
            Value::Null
        } else {
            json!({
                "byte_len": raw.len(),
                "head": raw.iter().take(8).collect::<Vec<_>>(),
            })
        };
        normalize_volatile(&mut body);
        redact_body_secrets(&mut body);
        // Recorded URIs route through the production redactor, and
        // body-embedded tokens (PlaybackInfo's `DirectStreamUrl`) mask
        // the same way: secrets must never land in the blessed fixtures
        // (this is the wiring proof for `redact_target`). The returned
        // body is the recorded one, so journeys replay the masked URL;
        // audio is anonymous, so the masked token still fetches.
        self.steps.push(json!({
            "request": format!("{method} {}", redact_target(uri)),
            "status": status.as_u16(),
            "headers": headers_out,
            "body": body,
        }));
        let recorded = self.steps.last().expect("just pushed").clone();
        (status, recorded["body"].clone())
    }
}

/// Replace live-clock leaves with a placeholder: `minutesAgo` derives from
/// the wall clock in the HTTP adapter (the fixed-clock golden pins the math
/// in the protocol suite instead). Fresh-UUID leaves (`PlaySessionId`) keep
/// `re:` markers in the blessed files instead (see `assert_pinned`).
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
        Value::Array(items) => {
            for item in items {
                normalize_volatile(item);
            }
        }
        _ => {}
    }
}

/// Mask body-embedded secrets in a recorded step: PlaybackInfo advertises
/// `DirectStreamUrl` with `?api_key=<live token>` for headerless players,
/// so the raw body would commit the fixture credential. Every `api_key`
/// value becomes `***`, matching the URI redactor's mask.
fn redact_body_secrets(body: &mut Value) {
    match body {
        Value::String(text) => {
            *text = mask_api_key(text);
        }
        Value::Array(items) => {
            for item in items {
                redact_body_secrets(item);
            }
        }
        Value::Object(map) => {
            for value in map.values_mut() {
                redact_body_secrets(value);
            }
        }
        _ => {}
    }
}

/// Replace every `api_key=<value>` query value with `api_key=***`.
fn mask_api_key(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find("api_key=") {
        out.push_str(&rest[..start + "api_key=".len()]);
        out.push_str("***");
        rest = &rest[start + "api_key=".len()..];
        rest = match rest.find('&') {
            Some(end) => &rest[end..],
            None => "",
        };
    }
    out.push_str(rest);
    out
}

/// Subsonic credential tail over a stage-3 app password.
fn sub_auth(client: &str) -> String {
    format!("u=alice&p={ALICE_SECRET}&v=1.16.1&c={client}&f=json")
}

fn jf_auth_value() -> String {
    format!("MediaBrowser Client=\"Journey\", Token=\"{ALICE_SECRET}\"")
}

// ---------------------------------------------------------------------------
// Golden corpus: traces plus pinned reference shapes
// ---------------------------------------------------------------------------

fn fixtures_dir() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("compat")
}

fn load_golden(name: &str) -> Value {
    let raw = std::fs::read_to_string(fixtures_dir().join(name)).expect("golden reads");
    serde_json::from_str(&raw).expect("golden parses")
}

/// Compare actual against expected under the golden vocabulary, shared
/// by traces and pinned references alike:
/// - exact: every other leaf must equal byte-for-byte;
/// - `re:`: a string leaf starting with `re:` is a regex the whole actual
///   string must match (volatile ids/dates keep `re:` markers in the
///   blessed files; re-add them after every `COMPAT_BLESS=1` run);
/// - ignore: paths in `ignore` (dot-joined, e.g. `steps[3].body.at`) are
///   skipped entirely.
///
/// Returns human-readable drift paths (empty means match).
/// `minutesAgo` is NOT routed through `ignore`: the wall-clock leaf is
/// normalized to `__MINUTES_AGO__` before comparison instead, so the
/// golden still pins the field's presence while the fixed-clock protocol
/// suite pins the math.
fn diff_golden(expected: &Value, actual: &Value, ignore: &[&str]) -> Vec<String> {
    fn walk(expected: &Value, actual: &Value, path: &str, ignore: &[&str], out: &mut Vec<String>) {
        if ignore.contains(&path) {
            return;
        }
        match (expected, actual) {
            (Value::Object(exp), Value::Object(got)) => {
                let mut keys: BTreeMap<&String, ()> = BTreeMap::new();
                for key in exp.keys().chain(got.keys()) {
                    keys.insert(key, ());
                }
                for key in keys.keys() {
                    let at = if path.is_empty() {
                        (*key).clone()
                    } else {
                        format!("{path}.{key}")
                    };
                    match (exp.get(*key), got.get(*key)) {
                        (Some(e), Some(a)) => walk(e, a, &at, ignore, out),
                        (Some(_), None) => out.push(format!("{at}: missing in actual")),
                        (None, Some(_)) => out.push(format!("{at}: unexpected in actual")),
                        (None, None) => {}
                    }
                }
            }
            (Value::Array(exp), Value::Array(got)) => {
                if exp.len() != got.len() {
                    out.push(format!("{path}: len {} != {}", exp.len(), got.len()));
                    return;
                }
                for (i, (e, a)) in exp.iter().zip(got.iter()).enumerate() {
                    walk(e, a, &format!("{path}[{i}]"), ignore, out);
                }
            }
            (Value::String(pattern), Value::String(text)) => {
                if let Some(pattern) = pattern.strip_prefix("re:") {
                    let regex =
                        regex::Regex::new(&format!("^(?:{pattern})$")).expect("regex compiles");
                    if !regex.is_match(text) {
                        out.push(format!("{path}: {text:?} does not match {pattern:?}"));
                    }
                } else if pattern != text {
                    out.push(format!("{path}: {text:?} != {pattern:?}"));
                }
            }
            (exp, got) => {
                if exp != got {
                    out.push(format!("{path}: {got} != {exp}"));
                }
            }
        }
    }
    let mut out = Vec::new();
    walk(expected, actual, "", ignore, &mut out);
    out
}

/// Assert a trace matches its golden, or bless it under `COMPAT_BLESS=1`.
fn assert_golden(name: &str, actual: &Value, ignore: &[&str]) {
    if std::env::var("COMPAT_BLESS").is_ok() {
        let text = serde_json::to_string_pretty(actual).expect("trace serializes");
        std::fs::write(fixtures_dir().join(name), format!("{text}\n")).expect("bless writes");
        return;
    }
    let drift = diff_golden(&load_golden(name), actual, ignore);
    assert!(drift.is_empty(), "{name} drifted:\n{}", drift.join("\n"));
}

/// Assert a pinned reference matches, or bless it under `COMPAT_BLESS=1`.
/// Blessing writes raw values: re-add `re:` markers for volatile leaves
/// afterwards (see the blessed diff).
fn assert_pinned(name: &str, actual: &Value) {
    if std::env::var("COMPAT_BLESS").is_ok() {
        let text = serde_json::to_string_pretty(actual).expect("pinned serializes");
        std::fs::write(fixtures_dir().join(name), format!("{text}\n")).expect("bless writes");
        return;
    }
    let drift = diff_golden(&load_golden(name), actual, &[]);
    assert!(drift.is_empty(), "{name} drifted:\n{}", drift.join("\n"));
}

// ---------------------------------------------------------------------------
// Journey 1: Symfonium shape via Subsonic
// ---------------------------------------------------------------------------

#[tokio::test]
async fn symfonium_browse_stream_favorite_playlist_scrobble() {
    let apps = apps();
    let mut trace = Trace::new();
    let auth = sub_auth("symfonium");
    let rest = |endpoint: &str| format!("/subsonic/rest/{endpoint}?{auth}");

    // Browse: ping, album list, one album, one song.
    let (status, _) = trace
        .call(&apps.subsonic, "GET", &rest("ping"), &[], None)
        .await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = trace
        .call(
            &apps.subsonic,
            "GET",
            &format!("/subsonic/rest/getAlbumList2?type=newest&{auth}"),
            &[],
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let (status, album) = trace
        .call(
            &apps.subsonic,
            "GET",
            &format!("/subsonic/rest/getAlbum?id=al-rg-1&{auth}"),
            &[],
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        album["subsonic-response"]["album"]["songCount"],
        json!(2),
        "rg-1 carries two fixture songs"
    );

    // Stream: whole object (100 bytes), then a seek slice. Symfonium
    // authenticates media reads; the headerless variant belongs to
    // journey 3.
    let (status, full) = trace
        .call(
            &apps.subsonic,
            "GET",
            &format!("/subsonic/rest/stream?id=tr-f1&{auth}"),
            &[],
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(full["byte_len"], json!(100));
    let (status, slice) = trace
        .call(
            &apps.subsonic,
            "GET",
            &format!("/subsonic/rest/stream?id=tr-f1&{auth}"),
            &[("Range", "bytes=0-49")],
            None,
        )
        .await;
    assert_eq!(status, StatusCode::PARTIAL_CONTENT);
    assert_eq!(slice["byte_len"], json!(50));

    // Favorite, then prove it stuck.
    let (status, _) = trace
        .call(
            &apps.subsonic,
            "GET",
            &format!("/subsonic/rest/star?id=tr-f1&{auth}"),
            &[],
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let (status, starred) = trace
        .call(&apps.subsonic, "GET", &rest("getStarred2"), &[], None)
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        starred["subsonic-response"]["starred2"]["song"][0]["id"],
        json!("tr-f1")
    );

    // Playlist: create with the song, read it back.
    let (status, created) = trace
        .call(
            &apps.subsonic,
            "GET",
            &format!("/subsonic/rest/createPlaylist?name=Journey+Mix&songId=tr-f1&{auth}"),
            &[],
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let playlist_id = created["subsonic-response"]["playlist"]["id"]
        .as_str()
        .expect("id")
        .to_owned();
    let (status, _) = trace
        .call(
            &apps.subsonic,
            "GET",
            &format!("/subsonic/rest/getPlaylist?id={playlist_id}&{auth}"),
            &[],
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK);

    // Scrobble-report: now-playing first, then the submission.
    let (status, _) = trace
        .call(
            &apps.subsonic,
            "GET",
            &format!("/subsonic/rest/scrobble?id=tr-f1&submission=false&{auth}"),
            &[],
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = trace
        .call(
            &apps.subsonic,
            "GET",
            &format!("/subsonic/rest/getNowPlaying?{auth}"),
            &[],
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = trace
        .call(
            &apps.subsonic,
            "GET",
            &format!("/subsonic/rest/scrobble?id=tr-f1&submission=true&{auth}"),
            &[],
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK);

    assert_golden("self_golden_symfonium.trace.json", &trace.body(), &[]);
}

// ---------------------------------------------------------------------------
// Journey 2: Finamp shape via Jellyfin
// ---------------------------------------------------------------------------

#[tokio::test]
async fn finamp_browse_stream_favorite_playlist_progress() {
    let apps = apps();
    let mut trace = Trace::new();
    let auth_value = jf_auth_value();
    let auth_header = [("X-Emby-Authorization", auth_value.as_str())];
    let user = ALICE_ID;

    // Browse: public info needs no token; the rest rides the header.
    let (status, _) = trace
        .call(
            &apps.jellyfin,
            "GET",
            "/jellyfin/System/Info/Public",
            &[],
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = trace
        .call(
            &apps.jellyfin,
            "GET",
            &format!("/jellyfin/Users/{user}/Views"),
            &auth_header,
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let (status, items) = trace
        .call(
            &apps.jellyfin,
            "GET",
            &format!("/jellyfin/Users/{user}/Items?includeItemTypes=Audio"),
            &auth_header,
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let track_id = items["Items"][0]["Id"]
        .as_str()
        .expect("track id")
        .to_owned();
    let (status, info) = trace
        .call(
            &apps.jellyfin,
            "GET",
            &format!("/jellyfin/Items/{track_id}/PlaybackInfo"),
            &auth_header,
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let stream_url = info["MediaSources"][0]["DirectStreamUrl"]
        .as_str()
        .expect("direct url")
        .strip_prefix("http://localhost")
        .expect("origin strips")
        .to_owned();

    // Stream: whole object authed, seek slice headerless. Both must agree.
    let (status, full) = trace
        .call(&apps.jellyfin, "GET", &stream_url, &auth_header, None)
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(full["byte_len"], json!(1024));
    let (status, slice) = trace
        .call(
            &apps.jellyfin,
            "GET",
            &stream_url,
            &[("Range", "bytes=0-99")],
            None,
        )
        .await;
    assert_eq!(status, StatusCode::PARTIAL_CONTENT);
    assert_eq!(slice["byte_len"], json!(100));

    // Favorite, then prove it stuck through the favorites filter.
    let (status, _) = trace
        .call(
            &apps.jellyfin,
            "POST",
            &format!("/jellyfin/Users/{user}/FavoriteItems/{track_id}"),
            &auth_header,
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let (status, favorites) = trace
        .call(
            &apps.jellyfin,
            "GET",
            &format!("/jellyfin/Users/{user}/Items?includeItemTypes=Audio&IsFavorite=true"),
            &auth_header,
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(favorites["Items"][0]["Id"], json!(track_id));

    // Playlist: create with the song, read its items back.
    let (status, created) = trace
        .call(
            &apps.jellyfin,
            "POST",
            "/jellyfin/Playlists",
            &auth_header,
            Some(json!({ "Name": "Journey Mix", "Ids": [track_id] })),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let playlist_id = created["Id"].as_str().expect("id").to_owned();
    let (status, items) = trace
        .call(
            &apps.jellyfin,
            "GET",
            &format!("/jellyfin/Playlists/{playlist_id}/Items"),
            &auth_header,
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(items["Items"][0]["Id"], json!(track_id));

    // Playback report: start, progress, stop lands a scrobble in state.
    for endpoint in ["Playing", "Playing/Progress"] {
        let (status, _) = trace
            .call(
                &apps.jellyfin,
                "POST",
                &format!("/jellyfin/Sessions/{endpoint}"),
                &auth_header,
                Some(json!({ "ItemId": track_id })),
            )
            .await;
        assert_eq!(status, StatusCode::NO_CONTENT);
    }
    let (status, _) = trace
        .call(
            &apps.jellyfin,
            "POST",
            "/jellyfin/Sessions/Playing/Stopped",
            &auth_header,
            Some(json!({
                "ItemId": track_id,
                "PositionTicks": 1_700_000_000_i64,
                "RunTimeTicks": 1_800_000_000_i64,
            })),
        )
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert!(
        apps.sessions.calls().iter().any(|call| matches!(
            call,
            SessionCall::Scrobble { user_id, file_id }
                if user_id == ALICE_ID && file_id == "f1"
        )),
        "stopped lands a submission scrobble"
    );

    assert_golden("self_golden_finamp.trace.json", &trace.body(), &[]);
}

// ---------------------------------------------------------------------------
// Journey 3: Jellify Latest, then headerless play
// ---------------------------------------------------------------------------

#[tokio::test]
async fn jellify_latest_then_headerless_play() {
    let apps = apps();
    let mut trace = Trace::new();
    let auth_value = jf_auth_value();
    let auth_header = [("X-Emby-Authorization", auth_value.as_str())];

    // Latest carries album DTOs, newest first; open the album for its
    // tracks, then play the first one headerless.
    let (status, latest) = trace
        .call(
            &apps.jellyfin,
            "GET",
            "/jellyfin/UserItems/Latest",
            &auth_header,
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(latest[0]["Type"], json!("MusicAlbum"));
    let album_id = latest[0]["Id"].as_str().expect("latest id").to_owned();
    let (status, tracks) = trace
        .call(
            &apps.jellyfin,
            "GET",
            &format!("/jellyfin/Users/{ALICE_ID}/Items?parentId={album_id}&includeItemTypes=Audio"),
            &auth_header,
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let track_id = tracks["Items"][0]["Id"]
        .as_str()
        .expect("track id")
        .to_owned();
    let stream_url = format!("/jellyfin/Audio/{track_id}/stream.mp3?static=true");

    // Jellify plays that URL with no auth headers at all: full fetch, a
    // seek slice, a HEAD probe, and the universal landing.
    let (status, full) = trace
        .call(&apps.jellyfin, "GET", &stream_url, &[], None)
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(full["byte_len"], json!(256));
    let (status, slice) = trace
        .call(
            &apps.jellyfin,
            "GET",
            &stream_url,
            &[("Range", "bytes=100-199")],
            None,
        )
        .await;
    assert_eq!(status, StatusCode::PARTIAL_CONTENT);
    assert_eq!(slice["byte_len"], json!(100));
    let (status, head) = trace
        .call(&apps.jellyfin, "HEAD", &stream_url, &[], None)
        .await;
    assert_eq!(status, StatusCode::OK);
    assert!(head.is_null(), "HEAD answers headers with an empty body");
    let universal = stream_url.replace("/stream.mp3", "/universal");
    let (status, _) = trace
        .call(&apps.jellyfin, "GET", &universal, &[], None)
        .await;
    assert_eq!(status, StatusCode::OK);

    assert_golden("self_golden_jellify.trace.json", &trace.body(), &[]);
}

// ---------------------------------------------------------------------------
// Pinned reference shapes: committed fixtures, regex for volatile fields
// ---------------------------------------------------------------------------

#[tokio::test]
async fn subsonic_pinned_reference_shapes() {
    let apps = apps();
    let mut trace = Trace::new();
    let auth = sub_auth("pin-check");

    let (_, ping) = trace
        .call(
            &apps.subsonic,
            "GET",
            &format!("/subsonic/rest/ping?{auth}"),
            &[],
            None,
        )
        .await;
    assert_pinned("pinned_subsonic_ping.json", &ping);

    let (_, list) = trace
        .call(
            &apps.subsonic,
            "GET",
            &format!("/subsonic/rest/getAlbumList2?type=newest&{auth}"),
            &[],
            None,
        )
        .await;
    assert_pinned("pinned_subsonic_album_list2.json", &list);

    let (_, song) = trace
        .call(
            &apps.subsonic,
            "GET",
            &format!("/subsonic/rest/getSong?id=tr-f1&{auth}"),
            &[],
            None,
        )
        .await;
    assert_pinned("pinned_subsonic_song.json", &song);

    trace
        .call(
            &apps.subsonic,
            "GET",
            &format!("/subsonic/rest/star?id=tr-f1&{auth}"),
            &[],
            None,
        )
        .await;
    let (_, starred) = trace
        .call(
            &apps.subsonic,
            "GET",
            &format!("/subsonic/rest/getStarred2?{auth}"),
            &[],
            None,
        )
        .await;
    assert_pinned("pinned_subsonic_starred2.json", &starred);

    let (_, created) = trace
        .call(
            &apps.subsonic,
            "GET",
            &format!("/subsonic/rest/createPlaylist?name=Journey+Mix&songId=tr-f1&{auth}"),
            &[],
            None,
        )
        .await;
    let playlist_id = created["subsonic-response"]["playlist"]["id"]
        .as_str()
        .expect("id")
        .to_owned();
    let (_, playlist) = trace
        .call(
            &apps.subsonic,
            "GET",
            &format!("/subsonic/rest/getPlaylist?id={playlist_id}&{auth}"),
            &[],
            None,
        )
        .await;
    assert_pinned("pinned_subsonic_playlist.json", &playlist);
}

#[tokio::test]
async fn jellyfin_pinned_reference_shapes() {
    let apps = apps();
    let mut trace = Trace::new();
    let auth_value = jf_auth_value();
    let auth_header = [("X-Emby-Authorization", auth_value.as_str())];
    let user = ALICE_ID;

    let (_, public) = trace
        .call(
            &apps.jellyfin,
            "GET",
            "/jellyfin/System/Info/Public",
            &[],
            None,
        )
        .await;
    assert_pinned("pinned_jellyfin_public_info.json", &public);

    let (_, views) = trace
        .call(
            &apps.jellyfin,
            "GET",
            &format!("/jellyfin/Users/{user}/Views"),
            &auth_header,
            None,
        )
        .await;
    assert_pinned("pinned_jellyfin_views.json", &views);

    let (_, items) = trace
        .call(
            &apps.jellyfin,
            "GET",
            &format!("/jellyfin/Users/{user}/Items"),
            &auth_header,
            None,
        )
        .await;
    assert_pinned("pinned_jellyfin_items.json", &items);

    let (_, latest) = trace
        .call(
            &apps.jellyfin,
            "GET",
            "/jellyfin/UserItems/Latest",
            &auth_header,
            None,
        )
        .await;
    assert_pinned("pinned_jellyfin_latest.json", &latest);

    let album_id = latest[0]["Id"].as_str().expect("latest id").to_owned();
    let (_, album_tracks) = trace
        .call(
            &apps.jellyfin,
            "GET",
            &format!("/jellyfin/Users/{user}/Items?parentId={album_id}&includeItemTypes=Audio"),
            &auth_header,
            None,
        )
        .await;
    let track_id = album_tracks["Items"][0]["Id"]
        .as_str()
        .expect("track id")
        .to_owned();
    let (_, info) = trace
        .call(
            &apps.jellyfin,
            "GET",
            &format!("/jellyfin/Items/{track_id}/PlaybackInfo"),
            &auth_header,
            None,
        )
        .await;
    assert_pinned("pinned_jellyfin_playback_info.json", &info);

    let (_, created) = trace
        .call(
            &apps.jellyfin,
            "POST",
            "/jellyfin/Playlists",
            &auth_header,
            Some(json!({ "Name": "Journey Mix", "Ids": [track_id] })),
        )
        .await;
    let playlist_id = created["Id"].as_str().expect("id").to_owned();
    let (_, playlist_items) = trace
        .call(
            &apps.jellyfin,
            "GET",
            &format!("/jellyfin/Playlists/{playlist_id}/Items"),
            &auth_header,
            None,
        )
        .await;
    assert_pinned("pinned_jellyfin_playlist_items.json", &playlist_items);
}

// ---------------------------------------------------------------------------
// Streaming matrices: 206 / 416 / HEAD / identity per protocol
// ---------------------------------------------------------------------------

/// Raw single call returning status, headers, and bytes for wire assertions.
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

#[tokio::test]
async fn subsonic_streaming_matrix() {
    let apps = apps();
    let auth = sub_auth("matrix");
    let url = format!("/subsonic/rest/stream?id=tr-f1&{auth}");

    // Identity: full object, engine-resolved content type.
    let (status, headers, bytes) = raw_call(&apps.subsonic, "GET", &url, &[]).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(header_str(&headers, "content-type"), "audio/mpeg");
    assert_eq!(header_str(&headers, "accept-ranges"), "bytes");
    assert_eq!(header_str(&headers, "content-length"), "100");
    assert_eq!(bytes.len(), 100);

    // Seek: exact slice with a content range.
    let (status, headers, bytes) =
        raw_call(&apps.subsonic, "GET", &url, &[("Range", "bytes=10-19")]).await;
    assert_eq!(status, StatusCode::PARTIAL_CONTENT);
    assert_eq!(header_str(&headers, "content-range"), "bytes 10-19/100");
    assert_eq!(bytes.len(), 10);
    assert_eq!(bytes[0], 10);

    // Suffix and open ranges follow the engine rules.
    let (status, _, bytes) = raw_call(&apps.subsonic, "GET", &url, &[("Range", "bytes=-4")]).await;
    assert_eq!(status, StatusCode::PARTIAL_CONTENT);
    assert_eq!(bytes.len(), 4);
    let (status, headers, _) =
        raw_call(&apps.subsonic, "GET", &url, &[("Range", "bytes=90-")]).await;
    assert_eq!(status, StatusCode::PARTIAL_CONTENT);
    assert_eq!(header_str(&headers, "content-range"), "bytes 90-99/100");

    // Unsatisfiable: 416, empty body, total advertised.
    let (status, headers, bytes) =
        raw_call(&apps.subsonic, "GET", &url, &[("Range", "bytes=100-")]).await;
    assert_eq!(status, StatusCode::RANGE_NOT_SATISFIABLE);
    assert_eq!(header_str(&headers, "content-range"), "bytes */100");
    assert!(bytes.is_empty());

    // HEAD mirrors GET headers with no body.
    let (status, headers, bytes) = raw_call(&apps.subsonic, "HEAD", &url, &[]).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(header_str(&headers, "content-length"), "100");
    assert!(bytes.is_empty());

    // HEAD honors Range exactly like GET: 206 + Content-Range on a
    // satisfiable slice, 416 on an unsatisfiable one, never a body.
    let (status, headers, bytes) =
        raw_call(&apps.subsonic, "HEAD", &url, &[("Range", "bytes=10-19")]).await;
    assert_eq!(status, StatusCode::PARTIAL_CONTENT);
    assert_eq!(header_str(&headers, "content-range"), "bytes 10-19/100");
    assert_eq!(header_str(&headers, "content-length"), "10");
    assert!(bytes.is_empty());
    let (status, headers, bytes) =
        raw_call(&apps.subsonic, "HEAD", &url, &[("Range", "bytes=100-")]).await;
    assert_eq!(status, StatusCode::RANGE_NOT_SATISFIABLE);
    assert_eq!(header_str(&headers, "content-range"), "bytes */100");
    assert!(bytes.is_empty());

    // Unknown id: binary-path 404 as text, never a stack trace.
    let missing = format!("/subsonic/rest/stream?id=tr-nope&{auth}");
    let (status, headers, _) = raw_call(&apps.subsonic, "GET", &missing, &[]).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(header_str(&headers, "content-type"), "text/plain");

    // Metadata without credentials is refused with a failed envelope.
    let (status, _, bytes) =
        raw_call(&apps.subsonic, "GET", "/subsonic/rest/ping?f=json", &[]).await;
    assert_eq!(status, StatusCode::OK);
    let error: Value = serde_json::from_slice(&bytes).expect("error json");
    assert_eq!(error["subsonic-response"]["status"], json!("failed"));
    assert_eq!(error["subsonic-response"]["error"]["code"], json!(10));
}

#[tokio::test]
async fn media_path_auth_lockout_blocks_brute_force() {
    // M3: media paths skip the token buckets but NOT the auth-failure
    // lockout pre-check, so password-guessing over stream/download still
    // trips backoff.
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
    let auth = sub_auth("probe");
    let (status, headers, bytes) = raw_call(
        &apps.subsonic,
        "GET",
        &format!("/subsonic/rest/stream?id=tr-f1&{auth}"),
        &[],
    )
    .await;
    assert_eq!(status, StatusCode::OK, "Subsonic rejects stay HTTP 200");
    assert!(
        !header_str(&headers, "retry-after").is_empty(),
        "locked-out media carries Retry-After"
    );
    let reject: Value = serde_json::from_slice(&bytes).expect("reject json");
    assert_eq!(
        reject["subsonic-response"]["error"]["code"],
        json!(0),
        "locked-out media renders the code-0 limit envelope"
    );
}

#[tokio::test]
async fn jellyfin_streaming_matrix() {
    let apps = apps();
    let track2 = apps.ids.to_jf("track", "f2").await;
    let url = format!("/jellyfin/Audio/{track2}/stream");

    // Identity over the flac fixture, headerless.
    let (status, headers, bytes) = raw_call(&apps.jellyfin, "GET", &url, &[]).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(header_str(&headers, "content-type"), "audio/flac");
    assert_eq!(bytes.len(), 512);

    // Container-suffixed and universal URLs serve the same bytes.
    let (status, _, suffixed) = raw_call(
        &apps.jellyfin,
        "GET",
        &format!("/jellyfin/Audio/{track2}/stream.mp3"),
        &[],
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(suffixed.len(), 512);
    let (status, _, universal) = raw_call(
        &apps.jellyfin,
        "GET",
        &format!("/jellyfin/Audio/{track2}/universal"),
        &[],
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(universal, bytes);

    // Seek slice and 416 behave exactly like the Subsonic side.
    let (status, headers, slice) =
        raw_call(&apps.jellyfin, "GET", &url, &[("Range", "bytes=0-9")]).await;
    assert_eq!(status, StatusCode::PARTIAL_CONTENT);
    assert_eq!(header_str(&headers, "content-range"), "bytes 0-9/512");
    assert_eq!(slice.len(), 10);
    let (status, headers, empty) =
        raw_call(&apps.jellyfin, "GET", &url, &[("Range", "bytes=9999-")]).await;
    assert_eq!(status, StatusCode::RANGE_NOT_SATISFIABLE);
    assert_eq!(header_str(&headers, "content-range"), "bytes */512");
    assert!(empty.is_empty());

    // HEAD without Range mirrors GET's full-object headers, no body.
    let (status, headers, empty) = raw_call(&apps.jellyfin, "HEAD", &url, &[]).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(header_str(&headers, "content-length"), "512");
    assert!(empty.is_empty());

    // HEAD honors Range exactly like GET (Brief 4): 206 + Content-Range
    // on a satisfiable slice, 416 past the end, never a body.
    let (status, headers, empty) =
        raw_call(&apps.jellyfin, "HEAD", &url, &[("Range", "bytes=0-9")]).await;
    assert_eq!(status, StatusCode::PARTIAL_CONTENT);
    assert_eq!(header_str(&headers, "content-range"), "bytes 0-9/512");
    assert_eq!(header_str(&headers, "content-length"), "10");
    assert!(empty.is_empty());
    let (status, headers, empty) =
        raw_call(&apps.jellyfin, "HEAD", &url, &[("Range", "bytes=9999-")]).await;
    assert_eq!(status, StatusCode::RANGE_NOT_SATISFIABLE);
    assert_eq!(header_str(&headers, "content-range"), "bytes */512");
    assert!(empty.is_empty());

    // Unknown id: plain 404, never a stack trace.
    let (status, _, _) = raw_call(&apps.jellyfin, "GET", "/jellyfin/Audio/nope/stream", &[]).await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // Metadata without credentials is refused.
    let (status, _, _) = raw_call(
        &apps.jellyfin,
        "GET",
        &format!("/jellyfin/Users/{ALICE_ID}/Views"),
        &[],
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

// ---------------------------------------------------------------------------
// Shared engine seam: the brief stage 6 and compat both execute
// ---------------------------------------------------------------------------

/// The real engine behind `StreamEngine` is stage 6 (`stream::gateway`);
/// this fake resolves fixed keys so the brief exercises the trait exactly
/// as the compat engine adapters call it.
struct FixtureEngine;

impl StreamEngine for FixtureEngine {
    async fn open(&self, request: StreamOpen) -> Result<OpenMedia, StreamFault> {
        let total_len = match request.key.as_str() {
            "so-1" => 4096u64,
            "so-2" => 2048u64,
            _ => return Err(StreamFault::NotFound),
        };
        Ok(OpenMedia {
            content_type: "audio/mpeg".to_owned(),
            total_len,
            transcoded: false,
            estimated_len: None,
            bytes: vec![0u8; total_len as usize],
        })
    }
}

#[tokio::test]
async fn streaming_briefs_shared_engine_seam() {
    // Range parsing is the engine's own function, so both protocols and
    // native playback seek identically.
    let slice = parse_range("bytes=0-99", 4096).expect("bounded range parses");
    assert_eq!((slice.start, slice.end), (0, 99));
    let open = parse_range("bytes=100-", 4096).expect("open range parses");
    assert_eq!((open.start, open.end), (100, 4095));
    let suffix = parse_range("bytes=-50", 4096).expect("suffix range parses");
    assert_eq!((suffix.start, suffix.end), (4046, 4095));
    assert!(
        parse_range("bytes=4096-", 4096).is_none(),
        "past-the-end is 416"
    );
    assert!(
        parse_range("bytes=90-80", 4096).is_none(),
        "inverted range is 416"
    );
    assert!(parse_range("bytes=abc", 4096).is_none(), "garbage is 416");

    // Content types come from the engine table, including the WMA refusal.
    assert_eq!(content_type_for_extension("mp3"), Some("audio/mpeg"));
    assert_eq!(content_type_for_extension(".FLAC"), Some("audio/flac"));
    assert_eq!(content_type_for_extension("wma"), None);

    // The trait both engine adapters call: open resolves or fails closed.
    let engine = FixtureEngine;
    let open = |key: &str| {
        engine.open(StreamOpen {
            source: AudioSource::Local,
            key: key.to_owned(),
            user_id: "fixture-user".to_owned(),
            params: StreamParams::default(),
        })
    };
    let media = open("so-1").await.expect("fixture opens");
    assert_eq!(media.total_len, 4096);
    assert!(
        !media.transcoded,
        "identity fixture; transcode owns landings"
    );
    assert_eq!(open("missing").await, Err(StreamFault::NotFound));
}

// ---------------------------------------------------------------------------
// Mutation scope: favorites, playlists, presence — nothing else
// ---------------------------------------------------------------------------

/// Strip the per-user overlays (favorites, play state) from a digest
/// row: those are the ALLOWED mutations, so the digest compares catalog
/// identity (ids, names, counts, bytes) rather than overlay flags.
fn strip_user_data(value: &mut Value) {
    match value {
        Value::Object(map) => {
            map.remove("UserData");
            map.remove("starred");
            for value in map.values_mut() {
                strip_user_data(value);
            }
        }
        Value::Array(items) => {
            for item in items {
                strip_user_data(item);
            }
        }
        _ => {}
    }
}

/// One catalog read per browse surface on both protocols: the digest
/// [`compat_mutates_only_favorites_playlists_presence`] snapshots before
/// and after its mutations.
async fn browse_digest(
    trace: &mut Trace,
    apps: &Apps,
    auth: &str,
    auth_header: &[(&str, &str)],
    user: &str,
) -> Vec<Value> {
    let mut rows = Vec::new();
    for uri in [
        format!("/subsonic/rest/getAlbumList2?type=newest&{auth}"),
        format!("/subsonic/rest/getArtists?{auth}"),
        format!("/subsonic/rest/getGenres?{auth}"),
    ] {
        rows.push(trace.call(&apps.subsonic, "GET", &uri, &[], None).await.1);
    }
    for uri in [
        format!("/jellyfin/Users/{user}/Views"),
        format!("/jellyfin/Users/{user}/Items"),
        "/jellyfin/UserItems/Latest".to_owned(),
    ] {
        rows.push(
            trace
                .call(&apps.jellyfin, "GET", &uri, auth_header, None)
                .await
                .1,
        );
    }
    rows
}

#[tokio::test]
async fn compat_mutates_only_favorites_playlists_presence() {
    let apps = apps();
    let mut trace = Trace::new();
    let auth = sub_auth("scope-check");
    let auth_value = jf_auth_value();
    let auth_header = [("X-Emby-Authorization", auth_value.as_str())];
    let user = ALICE_ID;
    let track2 = apps.ids.to_jf("track", "f2").await;
    let album1 = apps.ids.to_jf("album", "rg-1").await;

    // Browse digest before: catalog reads that must be byte-identical
    // after every allowed mutation below (Brief 6).
    let before = browse_digest(&mut trace, &apps, &auth, &auth_header, user).await;

    // Hit the mutating endpoints on both protocols: favorites,
    // playlists, queues, bookmarks, and presence.
    trace
        .call(
            &apps.subsonic,
            "GET",
            &format!("/subsonic/rest/star?id=tr-f1&{auth}"),
            &[],
            None,
        )
        .await;
    trace
        .call(
            &apps.subsonic,
            "GET",
            &format!("/subsonic/rest/createPlaylist?name=X&songId=tr-f1&{auth}"),
            &[],
            None,
        )
        .await;
    trace
        .call(
            &apps.subsonic,
            "GET",
            &format!("/subsonic/rest/savePlayQueue?id=tr-f1&id=tr-f2&current=tr-f1&{auth}"),
            &[],
            None,
        )
        .await;
    trace
        .call(
            &apps.subsonic,
            "GET",
            &format!("/subsonic/rest/createBookmark?id=tr-f1&position=1000&{auth}"),
            &[],
            None,
        )
        .await;
    trace
        .call(
            &apps.subsonic,
            "GET",
            &format!("/subsonic/rest/scrobble?id=tr-f1&{auth}"),
            &[],
            None,
        )
        .await;
    trace
        .call(
            &apps.jellyfin,
            "POST",
            &format!("/jellyfin/Users/{user}/FavoriteItems/{album1}"),
            &auth_header,
            None,
        )
        .await;
    trace
        .call(
            &apps.jellyfin,
            "POST",
            "/jellyfin/Playlists",
            &auth_header,
            Some(json!({ "Name": "Y", "Ids": [track2] })),
        )
        .await;
    trace
        .call(
            &apps.jellyfin,
            "POST",
            "/jellyfin/Sessions/Playing",
            &auth_header,
            Some(json!({ "ItemId": track2 })),
        )
        .await;

    // The allowed mutations all landed (read back through the APIs).
    let (_, starred) = trace
        .call(
            &apps.subsonic,
            "GET",
            &format!("/subsonic/rest/getStarred2?{auth}"),
            &[],
            None,
        )
        .await;
    assert_eq!(
        starred["subsonic-response"]["starred2"]["song"][0]["id"],
        json!("tr-f1")
    );
    let (_, favorites) = trace
        .call(
            &apps.jellyfin,
            "GET",
            &format!("/jellyfin/Users/{user}/Items?includeItemTypes=MusicAlbum&IsFavorite=true"),
            &auth_header,
            None,
        )
        .await;
    assert_eq!(favorites["Items"][0]["Id"], json!(album1));
    let (_, playlists) = trace
        .call(
            &apps.subsonic,
            "GET",
            &format!("/subsonic/rest/getPlaylists?{auth}"),
            &[],
            None,
        )
        .await;
    assert_eq!(
        playlists["subsonic-response"]["playlists"]["playlist"]
            .as_array()
            .map(Vec::len)
            .unwrap_or(0),
        2,
        "seeded mix plus the created list"
    );
    assert!(
        apps.sessions
            .calls()
            .iter()
            .any(|call| matches!(call, SessionCall::NowPlaying { file_id, .. } if file_id == "f2")),
        "playing lands presence"
    );

    // The catalog digest is byte-identical after (modulo the allowed
    // favorite overlays): playlists and presence live outside the
    // catalog reads, so anything else drifting would be a leak.
    let after = browse_digest(&mut trace, &apps, &auth, &auth_header, user).await;
    let mut before_stripped = before.clone();
    let mut after_stripped = after;
    for rows in [&mut before_stripped, &mut after_stripped] {
        for row in rows {
            strip_user_data(row);
        }
    }
    assert_eq!(
        after_stripped, before_stripped,
        "catalog digest drifted across mutations"
    );
}

// ---------------------------------------------------------------------------
// Harness self-test: the three golden modes do what they claim
// ---------------------------------------------------------------------------

#[test]
fn golden_harness_modes() {
    // Exact: equal passes, any drift fails loudly.
    assert!(diff_golden(&json!({"a": 1}), &json!({"a": 1}), &[]).is_empty());
    assert_eq!(
        diff_golden(&json!({"a": 1}), &json!({"a": 2}), &[]).len(),
        1
    );
    assert_eq!(
        diff_golden(&json!({"a": 1}), &json!({"a": 1, "b": 2}), &[]).len(),
        1
    );

    // Regex: volatile leaves match patterns, wrong shapes fail.
    let pattern = json!({ "at": "re:\\d{4}-\\d{2}-\\d{2}T\\d{2}:\\d{2}:\\d{2}" });
    assert!(diff_golden(&pattern, &json!({ "at": "2026-09-30T12:00:00" }), &[]).is_empty());
    assert_eq!(
        diff_golden(&pattern, &json!({ "at": "yesterday" }), &[]).len(),
        1
    );
    let uuid = json!({ "id": "re:[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}" });
    assert!(
        diff_golden(
            &uuid,
            &json!({ "id": "123e4567-e89b-12d3-a456-426614174000" }),
            &[]
        )
        .is_empty()
    );

    // Ignore: listed paths are skipped, everything else still compares.
    let expected = json!({ "stable": 1, "volatile": "anything" });
    let actual = json!({ "stable": 1, "volatile": "changed" });
    assert!(!diff_golden(&expected, &actual, &[]).is_empty());
    assert!(diff_golden(&expected, &actual, &["volatile"]).is_empty());
}

#[tokio::test]
async fn trace_redacts_credentials_from_recorded_uris() {
    // T-M6/t-m7 wiring: every recorded request routes through the
    // production `redact_target`, so blessed fixtures can never commit
    // a credential.
    let apps = apps();
    let mut trace = Trace::new();
    let auth = sub_auth("redact-check");
    trace
        .call(
            &apps.subsonic,
            "GET",
            &format!("/subsonic/rest/ping?{auth}"),
            &[],
            None,
        )
        .await;
    assert_eq!(
        redact_target(&format!("/subsonic/rest/ping?{auth}")),
        format!("/subsonic/rest/ping?u=alice&p=***&v=1.16.1&c=redact-check&f=json"),
    );
    let recorded = trace.body()["steps"][0]["request"]
        .as_str()
        .expect("request records")
        .to_owned();
    assert!(
        recorded.contains("p=***"),
        "secret masked in recorded request: {recorded}"
    );
    assert!(
        !recorded.contains(ALICE_SECRET),
        "no credential in recorded request: {recorded}"
    );
}

#[test]
fn body_redaction_masks_embedded_tokens() {
    assert_eq!(
        mask_api_key("http://x/Audio/1/stream.mp3?static=true&api_key=secret"),
        "http://x/Audio/1/stream.mp3?static=true&api_key=***"
    );
    assert_eq!(
        mask_api_key("/Audio/1/stream?api_key=secret&static=true"),
        "/Audio/1/stream?api_key=***&static=true"
    );
    assert_eq!(mask_api_key("no token here"), "no token here");
}

#[test]
fn blessed_fixtures_carry_no_credentials() {
    // t-m7 tripwire: neither recorded URIs nor recorded bodies may commit
    // the fixture credential. Bless runs that regress the redactors fail
    // here instead of landing a secret.
    for entry in std::fs::read_dir(fixtures_dir()).expect("fixtures list") {
        let path = entry.expect("entry").path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
            continue;
        }
        let text = std::fs::read_to_string(&path).expect("fixture reads");
        assert!(
            !text.contains(ALICE_SECRET),
            "{} commits the fixture credential",
            path.display()
        );
    }
}

#[test]
fn trace_uses_one_golden_spelling() {
    // The corpus is JSON traces plus pinned JSON references: no stray
    // sidecar layouts, no binary goldens. See `compat` module docs for the
    // exact/regex/ignore mode vocabulary shared by every suite.
    let mut traces = 0;
    let mut pinned = 0;
    for entry in std::fs::read_dir(fixtures_dir()).expect("fixtures list") {
        let name = entry
            .expect("entry")
            .file_name()
            .to_string_lossy()
            .into_owned();
        if name.ends_with(".trace.json") {
            traces += 1;
        } else if name.starts_with("pinned_") && name.ends_with(".json") {
            pinned += 1;
        } else if name == "streaming_briefs.md" {
            // Design note, not a golden.
        } else {
            panic!("unexpected corpus file: {name}");
        }
    }
    assert_eq!(traces, 3, "one trace per journey");
    assert_eq!(pinned, 11, "five subsonic plus six jellyfin pins");
}
