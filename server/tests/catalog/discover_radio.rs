//! Radio by sound, over a library whose track sounds are written straight
//! into the analysis table. Each track's vector points along an angle in
//! one plane, so the cosine distance between two tracks is one minus the
//! cosine of the angle between them and every order here can be worked out
//! by hand.

use axum::Router;
use droppedneedle_auth::Role;
use droppedneedle_auth::session::store::SessionKind;
use droppedneedle_kernel::clock::Clock as _;
use droppedneedle_kernel::db::{DbError, Lane};
use droppedneedle_kernel::runtime_config::sections::SoundAnalysisSettings;
use droppedneedle_library::sound::{Closeness, SoundSearch};
use droppedneedle_providers::Providers;
use serde_json::{Value, json};

use crate::common::auth::{AuthRig, test_principal, with_test_principal};
use crate::discover_queue::{call, discover_deps, discover_routes};

/// The seed track, at 0 degrees, by the seed artist.
const SEED: &str = "seed";
/// The eight tracks around it, each by an artist of its own, at these
/// angles: their distances from the seed are 0.0038, 0.0055, 0.0152,
/// 0.0219, 0.0387, 0.0603, 0.0865 and 0.1171.
const AROUND: [(&str, f64); 8] = [
    ("c1", 5.0),
    ("c2", -6.0),
    ("c3", 10.0),
    ("c4", -12.0),
    ("c5", 16.0),
    ("c6", -20.0),
    ("c7", 24.0),
    ("c8", -28.0),
];
/// The walk from the seed, worked out by hand: each next track is the one
/// with the lowest 0.7 x its distance from the track before plus 0.3 x its
/// distance from the seed.
const WALKED: [&str; 8] = ["c1", "c3", "c5", "c7", "c2", "c4", "c6", "c8"];

fn mbid(n: usize) -> String {
    format!("{n:08x}-0000-4000-8000-000000000000")
}

/// One planted library track and its sound.
struct Planted {
    id: &'static str,
    /// Its artist, by number: each has a library row and a MusicBrainz id.
    artist: usize,
    title: &'static str,
    recording: Option<String>,
    vector: Vec<f32>,
}

fn at_angle(degrees: f64) -> Vec<f32> {
    let radians = degrees.to_radians();
    let mut vector = vec![0.0f32; 200];
    vector[0] = radians.cos() as f32;
    vector[1] = radians.sin() as f32;
    vector
}

fn planted(id: &'static str, artist: usize, degrees: f64) -> Planted {
    Planted {
        id,
        artist,
        title: id,
        recording: Some(format!("rec-{id}")),
        vector: at_angle(degrees),
    }
}

/// The seed and the eight tracks around it, each identified.
fn circle() -> Vec<Planted> {
    let mut tracks = vec![planted(SEED, 0, 0.0)];
    for (n, (id, degrees)) in AROUND.iter().enumerate() {
        tracks.push(planted(id, n + 1, *degrees));
    }
    tracks
}

/// Write `tracks` into the library, each on an album of its own artist
/// with its sound analysed at the current version.
async fn plant(rig: &AuthRig, tracks: Vec<Planted>) {
    rig.platform
        .db
        .write(Lane::Foreground, "test.plant", move |tx| {
            tx.execute(
                "INSERT OR IGNORE INTO auth_users (id, display_name, role, created_at) \
                 VALUES ('user-1', 'Ada', 'user', 0)",
                [],
            )?;
            for track in &tracks {
                let artist = format!("artist-{}", track.artist);
                tx.execute(
                    "INSERT OR IGNORE INTO local_artists (id, display_name, folded_name, kind, \
                     created_at, updated_at) VALUES (?1, ?1, ?1, 'group', 1, 1)",
                    [&artist],
                )?;
                tx.execute(
                    "INSERT OR IGNORE INTO local_artist_external_identities (local_artist_id, \
                     provider, provider_artist_id, decision_source, selected_at) \
                     VALUES (?1, 'musicbrainz', ?2, 'manual', 1)",
                    [&artist, &mbid(track.artist)],
                )?;
                let album = format!("album-{}", track.artist);
                tx.execute(
                    "INSERT OR IGNORE INTO local_albums (id, root_id, grouping_key, title, \
                     title_folded, album_artist_id, grouping_source, created_at, updated_at) \
                     VALUES (?1, 'r1', ?1, ?1, ?1, ?2, 'automatic', 1, 1)",
                    [&album, &artist],
                )?;
                tx.execute(
                    "INSERT INTO local_tracks (id, local_album_id, root_id, file_path, \
                     relative_path, path_hash, file_size_bytes, file_mtime_ns, stat_revision, \
                     title, title_folded, artist_name, album_title, album_title_folded, \
                     file_format, availability, ingest_source, imported_at, membership_source, \
                     embedded_recording_mbid, embedded_release_group_mbid) \
                     VALUES (?1, ?2, 'r1', '/music/' || ?1, ?1, ?1, 1, 1, 'stat-' || ?1, ?3, ?3, \
                     ?4, ?2, ?2, 'flac', 'indexed', 'scan', 1, 'automatic', ?5, ?6)",
                    rusqlite::params![
                        track.id,
                        album,
                        track.title,
                        artist,
                        track.recording,
                        mbid(9000 + track.artist)
                    ],
                )?;
                tx.execute(
                    "INSERT INTO local_track_artists (local_track_id, position, local_artist_id, \
                     role) VALUES (?1, 0, ?2, 'main')",
                    [track.id, &artist],
                )?;
                let vector: Vec<u8> = track
                    .vector
                    .iter()
                    .flat_map(|value| value.to_le_bytes())
                    .collect();
                // Every track's strongest five tags are the first five, so
                // no track is from outside the seed's genre.
                let tags: Vec<u8> = (0..50).map(|tag| if tag < 5 { 200 } else { 10 }).collect();
                tx.execute(
                    "INSERT INTO library_sound_analyses (local_track_id, analysis_version, \
                     stat_revision, model, outcome, vector, tags, analysed_seconds, analysed_at) \
                     VALUES (?1, 1, 'stat-' || ?1, 'MSD_musicnn', 'analysed', ?2, ?3, 1.0, 1)",
                    rusqlite::params![track.id, vector, tags],
                )?;
            }
            Ok::<_, DbError>(())
        })
        .await
        .expect("library planted");
}

fn sound_on(rig: &AuthRig, on: bool) {
    rig.config
        .save_blocking(SoundAnalysisSettings {
            sound_analysis_enabled: on,
            sound_analysis_paused: false,
        })
        .expect("sound analysis saved");
}

async fn radio(rig: &AuthRig, tag: &str) -> (crate::common::ScratchDir, Router) {
    let dir = crate::common::ScratchDir::new(tag);
    let app = discover_routes(discover_deps(rig, &dir, Providers::closed(), |_| {}));
    (dir, app)
}

/// The next chunk of a station: its tracks' ids and their reasons.
async fn plan(app: &Router, body: Value) -> (Vec<String>, Vec<Value>, Value) {
    let (status, plan) = call(app, "POST", "/api/v3/discover/radio/plan", Some(body)).await;
    assert_eq!(status, 200, "{plan}");
    let tracks = plan["tracks"].as_array().expect("tracks").clone();
    let ids = tracks
        .iter()
        .map(|track| {
            track["local_file_id"]
                .as_str()
                .expect("library track")
                .to_owned()
        })
        .collect();
    let reasons = tracks.iter().map(|track| track["reason"].clone()).collect();
    (ids, reasons, plan)
}

fn search(rig: &AuthRig) -> SoundSearch {
    SoundSearch::new(rig.platform.db.clone(), rig.config.live())
}

/// Another version of the seed never comes, nor a second copy of a track
/// kept just before it; two different identified pieces that sound almost
/// alike both stay. Nothing past the cut-off is listed, so a thin library
/// gives a short list.
#[tokio::test]
async fn similar_tracks_keep_one_version_and_nothing_far() {
    let rig = AuthRig::new("radio-versions").await;
    let mut tracks = vec![planted(SEED, 0, 0.0)];
    tracks.extend([
        // The seed's recording again, on another album.
        Planted {
            recording: tracks[0].recording.clone(),
            ..planted("copy", 1, 1.0)
        },
        // The seed's title and artist, live.
        Planted {
            title: "seed (Live)",
            recording: None,
            ..planted("live", 0, 2.0)
        },
        // Two identified pieces 0.0002 apart.
        planted("piece-one", 2, 12.0),
        planted("piece-two", 2, 13.0),
        // Two unidentified tracks 0.0002 apart: the second is a copy.
        Planted {
            recording: None,
            ..planted("tape-one", 3, -20.0)
        },
        Planted {
            recording: None,
            ..planted("tape-two", 3, -21.0)
        },
        // Past the cut-off, at 0.16.
        planted("far", 4, 33.0),
    ]);
    plant(&rig, tracks).await;
    sound_on(&rig, true);
    let index = search(&rig).index().expect("reads").expect("on");
    let found = index
        .similar(SEED, 10, &|_| false)
        .expect("the seed is analysed");
    let ids: Vec<&str> = found.iter().map(|near| near.track_id.as_str()).collect();
    assert_eq!(ids, ["piece-one", "piece-two", "tape-one"]);
    assert!(found.iter().all(|near| near.distance <= 0.15));
}

/// A station from a track plays the tracks around it in the walk's order,
/// worked out by hand; the next chunk, sent what was played, plays none of
/// it and walks on from the last track played.
#[tokio::test]
async fn a_station_walks_and_goes_on_without_repeats() {
    let rig = AuthRig::new("radio-walk").await;
    plant(&rig, circle()).await;
    sound_on(&rig, true);
    let (_dir, app) = radio(&rig, "radio-walk").await;
    let (all, _, _) = plan(&app, from_the_seed(json!({"count": 8}))).await;
    assert_eq!(all, WALKED);

    let (first, _, _) = plan(&app, from_the_seed(json!({"count": 5}))).await;
    assert_eq!(first, WALKED[..5]);
    let (next, _, _) = plan(
        &app,
        from_the_seed(json!({"count": 5, "station_tracks": queued(&first)})),
    )
    .await;
    assert_eq!(next, ["c4", "c6", "c8"], "none of the first chunk again");
}

/// "Not this" takes the track off the station and pushes the tracks that
/// sound very close to it down: worked out by hand, the walk now starts
/// from the far side.
#[tokio::test]
async fn not_this_removes_a_track_and_lowers_its_neighbours() {
    let rig = AuthRig::new("radio-not-this").await;
    plant(&rig, circle()).await;
    sound_on(&rig, true);
    let (_dir, app) = radio(&rig, "radio-not-this").await;
    let (ids, _, _) = plan(&app, from_the_seed(json!({"count": 8, "not_this": ["c3"]}))).await;
    assert_eq!(ids, ["c4", "c6", "c8", "c2", "c1", "c5", "c7"]);
}

/// Each track's reason names the seed track that found it, one the station
/// really grew from, and its closeness word is the one its distance from
/// that seed gives. A track found from one the person asked for more like
/// says so.
#[tokio::test]
async fn each_reason_names_a_seed_that_was_used() {
    let rig = AuthRig::new("radio-reasons").await;
    plant(&rig, circle()).await;
    sound_on(&rig, true);
    let (_dir, app) = radio(&rig, "radio-reasons").await;
    let (ids, reasons, _) = plan(
        &app,
        from_the_seed(json!({"count": 8, "more_like": ["c8"], "sound_only": true})),
    )
    .await;
    assert!(!ids.is_empty());
    let angle = |id: &str| {
        std::iter::once((SEED, 0.0))
            .chain(AROUND)
            .find(|(track, _)| *track == id)
            .map(|(_, degrees)| degrees)
            .expect("a planted track")
    };
    let mut kinds = std::collections::HashSet::new();
    for (id, reason) in ids.iter().zip(&reasons) {
        let seed = reason["seed_track_id"].as_str().expect("a seed track");
        let kind = reason["kind"].as_str().expect("a kind");
        kinds.insert(kind.to_owned());
        match kind {
            "sounds_like" => {
                assert_eq!(seed, SEED, "{reason}");
                let distance = 1.0 - (angle(id) - angle(SEED)).to_radians().cos();
                let word = serde_json::to_value(Closeness::of(distance as f32)).unwrap();
                assert_eq!(reason["closeness"], word, "{id}: {reason}");
                assert!(
                    reason["line"]
                        .as_str()
                        .unwrap()
                        .contains("seed by artist-0")
                );
            }
            "more_like" => {
                assert_eq!(seed, "c8", "{reason}");
                assert_eq!(reason["line"], "You asked for more like c8.");
            }
            other => panic!("{id} has a {other} reason: {reason}"),
        }
    }
    assert_eq!(kinds.len(), 2, "both seeds found tracks: {reasons:?}");
}

/// The vectors are read once and again only when the analysis changed: a
/// write elsewhere in the database reads nothing, a new analysis row reads
/// them again.
#[tokio::test]
async fn the_vectors_load_again_only_when_the_analysis_changes() {
    let rig = AuthRig::new("radio-reload").await;
    plant(&rig, vec![planted(SEED, 0, 0.0)]).await;
    sound_on(&rig, true);
    let search = search(&rig);
    let held = search.clone();
    let index = move || held.index().expect("reads").expect("on");
    assert_eq!(index().len(), 1);
    assert_eq!(search.loads(), 1);
    rig.platform
        .db
        .write(Lane::Foreground, "test.elsewhere", |tx| {
            tx.execute(
                "INSERT INTO catalog_playlists (id, name, created_at, updated_at, user_id) \
                 VALUES ('pl1', 'Mine', 1, 1, 'user-1')",
                [],
            )?;
            Ok::<_, DbError>(())
        })
        .await
        .expect("written");
    assert_eq!(index().len(), 1);
    assert_eq!(search.loads(), 1, "a write elsewhere reads nothing");
    plant(&rig, vec![planted("c1", 1, 5.0)]).await;
    assert_eq!(index().len(), 2);
    assert_eq!(search.loads(), 2, "a new analysis row is read");
}

/// With sound analysis off, a station plays today's library pool: the
/// station artist's owned tracks, in hybrid mode with no source for the
/// rest saying it holds the library only. Both bench reads answer their
/// off code.
#[tokio::test]
async fn with_analysis_off_radio_plays_the_library_pool() {
    let rig = AuthRig::new("radio-off").await;
    plant(
        &rig,
        vec![
            planted(SEED, 0, 0.0),
            planted("also-by-seed", 0, 40.0),
            planted("c1", 1, 5.0),
        ],
    )
    .await;
    sound_on(&rig, false);
    let (_dir, app) = radio(&rig, "radio-off").await;
    let (mut ids, reasons, body) = plan(
        &app,
        json!({"seed_type": "artist", "seed_id": mbid(0), "mode": "hybrid", "count": 10}),
    )
    .await;
    ids.sort();
    assert_eq!(ids, ["also-by-seed", SEED]);
    assert!(
        reasons.iter().all(|reason| reason["kind"] == "seed"),
        "{reasons:?}"
    );
    assert_eq!(body["library_only"], true);

    let (_library, routes) = library_routes(&rig);
    for path in ["similar", "sound"] {
        let (status, error) = call(
            &routes,
            "GET",
            &format!("/api/v3/library/tracks/{SEED}/{path}"),
            None,
        )
        .await;
        assert_eq!(status, 409, "{path}: {error}");
        assert_eq!(error["error"]["code"], "SOUND_ANALYSIS_OFF");
    }
}

/// The sound route answers a track's 200 stored numbers exactly.
#[tokio::test]
async fn the_sound_route_answers_the_stored_numbers() {
    let rig = AuthRig::new("radio-sound-route").await;
    let measures: Vec<f32> = (0..200).map(|n| n as f32 * 0.37 - 5.1).collect();
    plant(
        &rig,
        vec![Planted {
            vector: measures.clone(),
            ..planted(SEED, 0, 0.0)
        }],
    )
    .await;
    sound_on(&rig, true);
    let (_library, routes) = library_routes(&rig);
    let (status, body) = call(
        &routes,
        "GET",
        &format!("/api/v3/library/tracks/{SEED}/sound"),
        None,
    )
    .await;
    assert_eq!(status, 200, "{body}");
    let answered: Vec<f32> = serde_json::from_value(body["measures"].clone()).expect("numbers");
    assert_eq!(answered, measures);
}

/// The library's routes over `rig`'s platform, signed in as a user.
fn library_routes(rig: &AuthRig) -> (droppedneedle_library::wiring::LibrarySetup, Router) {
    let (library, _) = crate::common::library::library_at(&rig.platform);
    let router = droppedneedle_library::http::routes::library_routes()
        .with_state(crate::common::library::services(&library))
        .into_router();
    let router = with_test_principal(
        router,
        test_principal("user-1", Role::User, "sess-test", SessionKind::Standard),
    );
    (library, router)
}

/// A library station from the seed track, with the fields of `extra`.
fn from_the_seed(extra: Value) -> Value {
    let mut body = json!({"seed_type": "track", "seed_id": SEED, "mode": "library"});
    if let (Some(body), Value::Object(extra)) = (body.as_object_mut(), extra) {
        body.extend(extra);
    }
    body
}

/// A long station keeps finding tracks: what it already queued is left out
/// before the search counts, so the second chunk is ten new tracks, not
/// what is left of the first ten.
#[tokio::test]
async fn a_long_station_does_not_run_dry() {
    const AROUND_MANY: [&str; 20] = [
        "t01", "t02", "t03", "t04", "t05", "t06", "t07", "t08", "t09", "t10", "t11", "t12", "t13",
        "t14", "t15", "t16", "t17", "t18", "t19", "t20",
    ];
    let rig = AuthRig::new("radio-long").await;
    let mut tracks = vec![planted(SEED, 0, 0.0)];
    for (n, name) in AROUND_MANY.iter().enumerate() {
        let side = if n % 2 == 0 { 1.0 } else { -1.0 };
        tracks.push(planted(name, n + 1, side * 1.5 * (n / 2 + 1) as f64));
    }
    plant(&rig, tracks).await;
    sound_on(&rig, true);
    let (_dir, app) = radio(&rig, "radio-long").await;
    let (first, _, _) = plan(&app, from_the_seed(json!({"count": 10}))).await;
    assert_eq!(first.len(), 10);
    let (second, _, _) = plan(
        &app,
        from_the_seed(json!({"count": 10, "station_tracks": queued(&first)})),
    )
    .await;
    assert_eq!(second.len(), 10, "{second:?}");
    assert!(second.iter().all(|id| !first.contains(id)));
}

/// "Not this artist" from Discover holds on a station by sound, except for
/// the artist the station grows from.
#[tokio::test]
async fn a_station_leaves_out_artists_the_person_hid() {
    let rig = AuthRig::new("radio-hidden").await;
    let mut tracks = circle();
    tracks.push(planted("seed-two", 0, 3.0));
    plant(&rig, tracks).await;
    rig.platform
        .db
        .write(Lane::Foreground, "test.hide", |tx| {
            for artist in [mbid(0), mbid(1)] {
                tx.execute(
                    "INSERT INTO catalog_discover_feedback (user_id, target, \
                     target_mbid_lower, kind, created_at) \
                     VALUES ('user-1', 'artist', ?1, 'not_artist', 0)",
                    [artist],
                )?;
            }
            Ok::<_, DbError>(())
        })
        .await
        .expect("hidden");
    sound_on(&rig, true);
    let (_dir, app) = radio(&rig, "radio-hidden").await;
    let (ids, _, _) = plan(&app, from_the_seed(json!({"count": 9}))).await;
    assert!(!ids.contains(&"c1".to_owned()), "{ids:?}");
    assert!(ids.contains(&"seed-two".to_owned()), "{ids:?}");
}

/// In hybrid mode with YouTube set up, the seed artist's popular tracks the
/// library lacks join a station by sound, each with its reason; with no
/// source for them it holds the library only and reads nothing for them.
#[tokio::test]
async fn hybrid_stations_add_unowned_tracks_only_with_a_source() {
    use droppedneedle_kernel::runtime_config::Secret;
    use droppedneedle_kernel::runtime_config::secret_sections::YouTubeConnection;
    use droppedneedle_providers::test_support::ScriptedWire;
    use std::sync::Arc;

    let rig = AuthRig::new("radio-hybrid").await;
    plant(&rig, circle()).await;
    sound_on(&rig, true);
    let wire = Arc::new(ScriptedWire::new());
    let hits: Vec<Value> = ["Not Owned One", "Not Owned Two"]
        .iter()
        .enumerate()
        .map(|(n, title)| json!({"id": n + 1, "title": title, "artist": {"name": "artist-0"}}))
        .collect();
    wire.answer(
        "http://127.0.0.1:9/deezer/search",
        200,
        &[("content-type", "application/json")],
        json!({ "data": hits }).to_string(),
    );
    let dir = crate::common::ScratchDir::new("radio-hybrid");
    let app = discover_routes(discover_deps(
        &rig,
        &dir,
        Providers::unpaced(wire.clone()),
        |_| {},
    ));
    let hybrid = || from_the_seed(json!({"mode": "hybrid", "count": 10}));
    let deezer_reads = || {
        wire.sent()
            .iter()
            .filter(|sent| sent.url.path().ends_with("/deezer/search"))
            .count()
    };

    let (status, alone) = call(&app, "POST", "/api/v3/discover/radio/plan", Some(hybrid())).await;
    assert_eq!(status, 200, "{alone}");
    assert_eq!(alone["library_only"], true);
    let tracks = alone["tracks"].as_array().expect("tracks");
    assert!(tracks.iter().all(|track| track["in_library"] == true));
    assert_eq!(
        deezer_reads(),
        0,
        "nothing is read for tracks that cannot play"
    );

    rig.config
        .save_secret_blocking(YouTubeConnection {
            api_key: Secret::new("youtube-key"),
            enabled: true,
            api_enabled: true,
            daily_quota_limit: 10_000,
        })
        .expect("youtube settings save");
    let (status, mixed) = call(&app, "POST", "/api/v3/discover/radio/plan", Some(hybrid())).await;
    assert_eq!(status, 200, "{mixed}");
    assert_eq!(mixed["library_only"], false);
    let unowned: Vec<&Value> = mixed["tracks"]
        .as_array()
        .expect("tracks")
        .iter()
        .filter(|track| track["in_library"] == false)
        .collect();
    let titles: Vec<&str> = unowned
        .iter()
        .filter_map(|track| track["track_name"].as_str())
        .collect();
    assert_eq!(titles, ["Not Owned One", "Not Owned Two"], "{mixed}");
    for track in &unowned {
        assert_eq!(track["reason"]["kind"], "seed", "{track}");
        assert_eq!(
            track["reason"]["line"],
            "By artist-0, where this station started."
        );
    }

    // The next chunk, sent the station's tracks as the player sends them:
    // the unowned ones carry no recording, only their artist and title.
    let station: Vec<Value> = mixed["tracks"]
        .as_array()
        .expect("tracks")
        .iter()
        .map(|track| {
            json!({
                "local_track_id": track.get("local_file_id"),
                "artist_mbid": track["artist_mbid"],
                "artist_name": track["artist_name"],
                "track_name": track["track_name"],
            })
        })
        .collect();
    let mut next = hybrid();
    next["station_tracks"] = Value::Array(station);
    let (status, again) = call(&app, "POST", "/api/v3/discover/radio/plan", Some(next)).await;
    assert_eq!(status, 200, "{again}");
    let repeated: Vec<&Value> = again["tracks"]
        .as_array()
        .expect("tracks")
        .iter()
        .filter(|track| titles.contains(&track["track_name"].as_str().unwrap_or("")))
        .collect();
    assert!(repeated.is_empty(), "{repeated:?}");
}

/// The station's library tracks as a plan request names them, by id only.
fn queued(ids: &[String]) -> Value {
    ids.iter()
        .map(|id| json!({"local_track_id": id, "artist_name": "", "track_name": ""}))
        .collect()
}

/// A track the station has queued keeps its other copies out of the next
/// chunk: the same recording on another album, and the same title by the
/// same artist with no identity.
#[tokio::test]
async fn a_queued_track_keeps_its_copies_out() {
    let rig = AuthRig::new("radio-copies").await;
    let mut tracks = vec![
        planted(SEED, 0, 0.0),
        Planted {
            title: "Anthem",
            ..planted("anthem", 1, 5.0)
        },
        Planted {
            title: "Anthem",
            recording: Some("rec-anthem".to_owned()),
            ..planted("anthem-copy", 1, 5.3)
        },
        Planted {
            title: "Anthem (Live)",
            recording: None,
            ..planted("anthem-tape", 1, 6.0)
        },
    ];
    for (n, degrees) in [-8.0, -12.0, 15.0, -20.0].into_iter().enumerate() {
        tracks.push(planted(["d1", "d2", "d3", "d4"][n], n + 2, degrees));
    }
    plant(&rig, tracks).await;
    sound_on(&rig, true);
    let (_dir, app) = radio(&rig, "radio-copies").await;
    let (ids, _, _) = plan(
        &app,
        from_the_seed(json!({"count": 8, "station_tracks": queued(&["anthem".to_owned()])})),
    )
    .await;
    assert!(!ids.is_empty());
    for copy in ["anthem", "anthem-copy", "anthem-tape"] {
        assert!(!ids.contains(&copy.to_owned()), "{copy} came back: {ids:?}");
    }
}

/// A station from a playlist grows from the playlist's library tracks:
/// every track names one of them as its seed, and none of them plays.
#[tokio::test]
async fn a_playlist_station_grows_from_its_tracks() {
    let rig = AuthRig::new("radio-playlist").await;
    plant(&rig, circle()).await;
    rig.platform
        .db
        .write(Lane::Foreground, "test.playlist", |tx| {
            tx.execute(
                "INSERT INTO catalog_playlists (id, name, created_at, updated_at, user_id) \
                 VALUES ('pl1', 'Mine', 1, 1, 'user-1')",
                [],
            )?;
            for (position, track) in ["c1", "c8"].into_iter().enumerate() {
                tx.execute(
                    "INSERT INTO catalog_playlist_tracks (id, playlist_id, position, track_name, \
                     artist_name, album_name, source_type, created_at, library_file_id) \
                     VALUES (?1, 'pl1', ?2, ?1, 'someone', 'album', 'local', 1, ?1)",
                    rusqlite::params![track, position as i64],
                )?;
            }
            Ok::<_, DbError>(())
        })
        .await
        .expect("playlist");
    sound_on(&rig, true);
    let (_dir, app) = radio(&rig, "radio-playlist").await;
    let (ids, reasons, body) = plan(
        &app,
        json!({"seed_type": "playlist", "seed_id": "pl1", "mode": "library", "count": 6,
               "sound_only": true}),
    )
    .await;
    assert_eq!(body["title"], "Radio: Mine");
    assert!(!ids.is_empty());
    assert!(!ids.iter().any(|id| id == "c1" || id == "c8"), "{ids:?}");
    let seeds: std::collections::HashSet<&str> = reasons
        .iter()
        .map(|reason| reason["seed_track_id"].as_str().expect("a seed track"))
        .collect();
    assert!(
        seeds.iter().all(|seed| ["c1", "c8"].contains(seed)),
        "{reasons:?}"
    );
}

/// The bench's list: nearest first, each with its distance and closeness
/// word and the tags it shares with the track; the count is clamped, and a
/// track not analysed is not found.
#[tokio::test]
async fn similar_tracks_answer_in_order_with_their_closeness() {
    let rig = AuthRig::new("radio-similar-route").await;
    let mut tracks = circle();
    tracks.push(planted("unanalysed", 20, 40.0));
    plant(&rig, tracks).await;
    rig.platform
        .db
        .write(Lane::Foreground, "test.unanalysed", |tx| {
            tx.execute(
                "DELETE FROM library_sound_analyses WHERE local_track_id = 'unanalysed'",
                [],
            )?;
            Ok::<_, DbError>(())
        })
        .await
        .expect("deleted");
    sound_on(&rig, true);
    let (_library, routes) = library_routes(&rig);
    let similar = |path: String| {
        let routes = routes.clone();
        async move { call(&routes, "GET", &path, None).await }
    };

    let (status, body) = similar(format!("/api/v3/library/tracks/{SEED}/similar?limit=8")).await;
    assert_eq!(status, 200, "{body}");
    let listed = body["tracks"].as_array().expect("tracks");
    let ids: Vec<&str> = listed
        .iter()
        .map(|track| track["track_id"].as_str().expect("id"))
        .collect();
    assert_eq!(ids, ["c1", "c2", "c3", "c4", "c5", "c6", "c7", "c8"]);
    for (track, (_, degrees)) in listed.iter().zip(AROUND) {
        let expected = 1.0 - degrees.to_radians().cos();
        let distance = track["distance"].as_f64().expect("distance");
        assert!((distance - expected).abs() < 1e-4, "{track}");
        let word = serde_json::to_value(Closeness::of(expected as f32)).unwrap();
        assert_eq!(track["closeness"], word, "{track}");
        assert!(!track["shared_tags"].as_array().expect("tags").is_empty());
    }
    assert_eq!(listed[7]["closeness"], "further");

    let (_, one) = similar(format!("/api/v3/library/tracks/{SEED}/similar?limit=0")).await;
    assert_eq!(one["tracks"].as_array().map(Vec::len), Some(1), "{one}");
    let (_, many) = similar(format!("/api/v3/library/tracks/{SEED}/similar?limit=1000")).await;
    assert_eq!(many["tracks"].as_array().map(Vec::len), Some(8), "{many}");
    let (status, _) = similar("/api/v3/library/tracks/unanalysed/similar".to_owned()).await;
    assert_eq!(status, 404);
}

/// The Discover page once a build for the signed-in person has settled.
async fn settled_page(app: &Router) -> Value {
    tokio::time::timeout(crate::common::PATIENCE, async {
        loop {
            let (_, page) = call(app, "GET", "/api/v3/discover", None).await;
            if page["refreshing"] == false && page["generated_at"].is_number() {
                return page;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the page was built")
}

/// The bench's section on `page`, and whether it has a state line.
fn bench(page: &Value) -> (Option<&Value>, bool) {
    let find = |list: &str| {
        page[list]
            .as_array()
            .and_then(|rows| rows.iter().find(|row| row["key"] == "sound_bench"))
    };
    (
        find("sections").map(|section| &section["content"]["sound_bench"]),
        find("section_states").is_some(),
    )
}

/// By ear is on Discover only while sound analysis is on, tuned to the
/// person's most played track of the last 30 days that has been analysed:
/// not one played more but longer ago, nor one not analysed. Someone with
/// no such plays gets the bench with no seed. Turning analysis off takes
/// it off the page at once, with no state line.
#[tokio::test]
async fn the_bench_is_tuned_to_the_most_played_analysed_track() {
    let rig = AuthRig::new("radio-bench-seed").await;
    let mut tracks = circle();
    tracks.push(planted("unanalysed", 20, 40.0));
    plant(&rig, tracks).await;
    let now = rig.clock().now_ms();
    let day = 86_400_000_i64;
    rig.platform
        .db
        .write(Lane::Foreground, "test.bench", move |tx| {
            tx.execute(
                "DELETE FROM library_sound_analyses WHERE local_track_id = 'unanalysed'",
                [],
            )?;
            tx.execute(
                "INSERT INTO auth_users (id, display_name, role, created_at) \
                 VALUES ('user-2', 'Bea', 'user', 0)",
                [],
            )?;
            for (track, count, at) in [
                ("unanalysed", 9, now - 2 * day),
                ("c1", 6, now - 40 * day),
                ("c3", 3, now - 2 * day),
                ("c5", 2, now - day),
            ] {
                for n in 0..count {
                    tx.execute(
                        "INSERT INTO library_play_history (id, user_id, local_track_id, \
                         track_name, artist_name, played_at) \
                         VALUES (?1, 'user-1', ?2, ?2, 'Someone', ?3)",
                        rusqlite::params![format!("{track}-{n}"), track, at - n * 60_000],
                    )?;
                }
            }
            Ok::<_, DbError>(())
        })
        .await
        .expect("plays written");
    sound_on(&rig, true);
    crate::discover_queue::seen_now(&rig, "user-1").await;
    crate::discover_queue::seen_now(&rig, "user-2").await;
    let dir = crate::common::ScratchDir::new("radio-bench-seed");
    let deps = discover_deps(&rig, &dir, Providers::closed(), |_| {});
    let app = discover_routes(deps.clone());

    let page = settled_page(&app).await;
    let (section, stated) = bench(&page);
    let section = section.expect("the bench is on the page");
    assert_eq!(section["seed"]["track_id"], "c3", "{page}");
    assert_eq!(section["seed"]["album_id"], "album-3");
    assert_eq!(section["analysis_version"], 1);
    assert!(!stated, "{page}");

    let bea = with_test_principal(
        droppedneedle_catalog::discover::routes()
            .with_state(deps)
            .into_router(),
        test_principal("user-2", Role::User, "sess-bea", SessionKind::Standard),
    );
    let page = settled_page(&bea).await;
    let section = bench(&page).0.expect("the bench is on Bea's page too");
    assert!(section.get("seed").is_none(), "{page}");

    sound_on(&rig, false);
    let (_, page) = call(&app, "GET", "/api/v3/discover", None).await;
    assert_eq!(bench(&page), (None, false), "{page}");
}

/// A page built while sound analysis was off is built again once it is
/// turned on, and the next page has the bench; so is a page saved before
/// the bench existed.
#[tokio::test]
async fn turning_sound_analysis_on_brings_the_bench_to_a_built_page() {
    let rig = AuthRig::new("radio-bench-on").await;
    plant(&rig, circle()).await;
    // Nine plays of an album long ago put From your shelves on the page, so
    // a build with analysis off has something to keep.
    let long_ago = rig.clock().now_ms() - 100 * 86_400_000;
    rig.platform
        .db
        .write(Lane::Foreground, "test.shelves", move |tx| {
            for n in 0..9 {
                tx.execute(
                    "INSERT INTO library_play_history (id, user_id, local_track_id, \
                     local_album_id, track_name, artist_name, played_at) \
                     VALUES (?1, 'user-1', 'c1', 'album-1', 'c1', 'Someone', ?2)",
                    rusqlite::params![format!("old-{n}"), long_ago - n * 60_000],
                )?;
            }
            Ok::<_, DbError>(())
        })
        .await
        .expect("plays written");
    sound_on(&rig, false);
    crate::discover_queue::seen_now(&rig, "user-1").await;
    let dir = crate::common::ScratchDir::new("radio-bench-on");
    let app = discover_routes(discover_deps(&rig, &dir, Providers::closed(), |_| {}));

    let page = settled_page(&app).await;
    assert_eq!(bench(&page), (None, false), "{page}");

    sound_on(&rig, true);
    let (_, asked) = call(&app, "GET", "/api/v3/discover", None).await;
    assert_eq!(
        asked["refreshing"], true,
        "the old page is built again: {asked}"
    );
    let page = settled_page(&app).await;
    let section = bench(&page).0.expect("the bench is on the new page");
    assert_eq!(section["analysis_version"], 1, "{page}");
    assert_eq!(section["model"], "MSD_musicnn");
    // A page saved before the bench existed names it as neither built nor
    // skipped: it is built again too.
    rig.platform
        .db
        .write(Lane::Foreground, "test.older_page", |tx| {
            let payload: Vec<u8> = tx.query_row(
                "SELECT payload FROM catalog_cached_pages WHERE user_id = 'user-1' \
                 AND page = 'discover'",
                [],
                |row| row.get(0),
            )?;
            let mut saved: Value = serde_json::from_slice(&payload).expect("a saved page");
            let without = |list: &mut Value| {
                if let Some(rows) = list.as_array_mut() {
                    rows.retain(|row| row != "sound_bench" && row["key"] != "sound_bench");
                }
            };
            without(&mut saved["built"]);
            without(&mut saved["page"]["sections"]);
            tx.execute(
                "UPDATE catalog_cached_pages SET payload = ?1 WHERE user_id = 'user-1' \
                 AND page = 'discover'",
                [serde_json::to_vec(&saved).expect("encodes")],
            )?;
            Ok::<_, DbError>(())
        })
        .await
        .expect("page made older");
    let (_, asked) = call(&app, "GET", "/api/v3/discover", None).await;
    assert_eq!(bench(&asked).0, None, "{asked}");
    assert_eq!(
        asked["refreshing"], true,
        "a page that never built the bench is built again: {asked}"
    );
    let page = settled_page(&app).await;
    assert!(bench(&page).0.is_some(), "{page}");
}
