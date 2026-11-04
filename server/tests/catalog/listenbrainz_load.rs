//! How much one active user asks of ListenBrainz in a day.
//!
//! MetaBrainz counts every install, so the warm day's total is pinned. The
//! journey runs the production discover wiring, search counts, artist pages,
//! radio and the Weekly Mix against a mock of the providers, on a manual
//! clock: a cold day, a warm day, the Monday the Daily Mix seeds rotate, and
//! day 15, past the 14-day rows, with a Weekly Mix after it. Each day has
//! four visits, four searches, two artist pages, two album pages, one radio
//! shelf and the warm cycle ticking hourly. One artist in five has nothing
//! on ListenBrainz. Run it with `--nocapture` for the calls per endpoint.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use axum::Router;
use axum::body::{Body, Bytes};
use axum::http::{Method, Request, Uri};
use axum::response::{IntoResponse, Response};
use droppedneedle_acquire::requests::{RequestsState, mix_sources};
use droppedneedle_auth::users::models::LastFmConnection;
use droppedneedle_catalog::collections::store::playlists::PlaylistStore;
use droppedneedle_catalog::discover::scheduler::Feature;
use droppedneedle_catalog::discover::scheduler::activity::{Activity, ActivityDb};
use droppedneedle_catalog::discover::wiring::{deps_over, live_network};
use droppedneedle_catalog::metadata::Catalog;
use droppedneedle_catalog::metadata::library::LocalCatalog;
use droppedneedle_catalog::metadata::upstream::{LiveCatalogSettings, Upstream};
use droppedneedle_catalog::search::enrichment::{SearchCounts, enrich_with_degradation};
use droppedneedle_catalog::search::models::{
    AlbumEnrichmentRequest, ArtistEnrichmentRequest, EnrichmentBatchRequest,
};
use droppedneedle_kernel::clock::Clock as _;
use droppedneedle_kernel::db::Lane;
use droppedneedle_kernel::events::EventHub;
use droppedneedle_kernel::runtime_config::Secret;
use droppedneedle_kernel::runtime_config::secret_sections::ListenBrainzConnection;
use droppedneedle_media::playback::listening::PrefsPatch;
use droppedneedle_providers::{
    CallOutcome, ProviderEndpoints, Providers, RequestPriority, ReqwestWire,
};
use serde_json::{Value, json};
use tower::ServiceExt as _;

use crate::common::ScratchDir;
use crate::common::auth::AuthRig;
use crate::discover_queue::{after_commits, discover_inputs, discover_routes};

/// The most ListenBrainz calls each day of this journey may make. The old
/// code made 296, 294, 276 and 279.
const LISTENBRAINZ_BOUNDS: [(&str, u64); 4] = [
    ("cold", 275),
    ("warm", 105),
    ("monday", 195),
    ("day 15", 235),
];
/// The most ListenBrainz plus MusicBrainz calls for a user who follows
/// Last.fm. The old code made 320, 283, 262 and 260.
const LASTFM_BOUNDS: [(&str, u64); 4] = [
    ("cold", 300),
    ("warm", 165),
    ("monday", 110),
    ("day 15", 90),
];

const USER: &str = "user-1";
const HOUR_MS: i64 = 3_600_000;
const GENRES: [&str; 4] = ["rock", "jazz", "folk", "metal"];
const SIMILAR_USERS_BODY: &str =
    include_str!("../fixtures/listenbrainz/similar_users_account.json");
const FRESH_RELEASES_BODY: &str =
    include_str!("../fixtures/listenbrainz/fresh_releases_account.json");
const FEEDBACK_BODY: &str = include_str!("../fixtures/listenbrainz/feedback_account.json");
const RECOMMENDATION_PLAYLISTS_BODY: &str =
    include_str!("../fixtures/listenbrainz/recommendation_playlists_account.json");
const LIBRARY_ARTISTS: u32 = 20;
const SIMILAR_POOL: u32 = 5_000;
const DAYS: [(&str, u32); 4] = [("cold", 0), ("warm", 1), ("monday", 7), ("day 15", 15)];

fn mbid(kind: char, n: u32) -> String {
    let p = kind.to_string().repeat(4);
    format!("{n:08x}-{p}-4{}-8{}-{}", &p[..3], &p[..3], p.repeat(3))
}

fn number_of(text: &str) -> u32 {
    text.bytes().fold(7u32, |sum, byte| {
        sum.wrapping_mul(31).wrapping_add(u32::from(byte))
    })
}

type Tally = Arc<Mutex<BTreeMap<String, u64>>>;

/// The name a call counts under.
fn endpoint(method: &Method, uri: &Uri) -> String {
    let parts: Vec<&str> = uri
        .path()
        .split('/')
        .filter(|part| !part.is_empty())
        .collect();
    match parts.as_slice() {
        ["listenbrainz", "1", "popularity", "release-group", ..] => {
            "lb release-group popularity".to_owned()
        }
        ["listenbrainz", "1", "popularity", name, ..] => format!("lb {name}"),
        ["listenbrainz", "1", "lb-radio", ..] => "lb lb-radio/artist".to_owned(),
        ["listenbrainz", "1", "metadata", "recording", ..] => "lb metadata/recording".to_owned(),
        ["listenbrainz", "1", "stats", "sitewide", name] => format!("lb stats/sitewide/{name}"),
        ["listenbrainz", "1", "stats", "user", _, name] => format!("lb stats/user/{name}"),
        ["listenbrainz", "1", "feedback", ..] => "lb feedback".to_owned(),
        ["listenbrainz", "1", "user", _, name, ..] => format!("lb user/{name}"),
        ["listenbrainz", "1", "playlist", ..] => "lb playlist".to_owned(),
        ["listenbrainz", ..] => format!("lb other {method} {}", uri.path()),
        ["musicbrainz", kind, rest @ ..] => {
            format!(
                "musicbrainz {kind}{}",
                if rest.is_empty() { " search" } else { "" }
            )
        }
        ["musicbrainz", ..] => "musicbrainz other".to_owned(),
        ["lastfm", ..] => "lastfm".to_owned(),
        [first, ..] => format!("other {first}"),
        [] => "other".to_owned(),
    }
}

fn lb_answer(uri: &Uri, body: &[u8]) -> Value {
    let parts: Vec<&str> = uri
        .path()
        .split('/')
        .filter(|part| !part.is_empty())
        .collect();
    let asked: Value = serde_json::from_slice(body).unwrap_or(Value::Null);
    let recorded =
        |text: &str| -> Value { serde_json::from_str(text).expect("a recorded answer parses") };
    let metadata_read = uri
        .query()
        .is_some_and(|query| query.contains("metadata=true"));
    let artist_stat = |n: u32| {
        json!({"artist_name": format!("Artist {n}"), "listen_count": 100 - n,
               "artist_mbids": [mbid('a', n)]})
    };
    let group_stat = |n: u32| {
        json!({"release_group_name": format!("Group {n}"), "artist_name": format!("Artist {n}"),
               "listen_count": 100 - n, "release_group_mbid": mbid('c', 1000 + n),
               "artist_mbids": [mbid('a', 1 + n % 10)]})
    };
    match &parts[2..] {
        ["stats", "sitewide", "artists"] => json!({"payload": {"artists": (1..=10)
            .map(|n| json!({"artist_name": format!("Artist {n}"), "listen_count": 100 - n,
                            "artist_mbid": mbid('a', n)}))
            .collect::<Vec<_>>()}}),
        ["stats", "user", _, "artists"] => {
            json!({"payload": {"artists": (1..=10).map(artist_stat).collect::<Vec<_>>()}})
        }
        ["stats", _, .., "release-groups"] => {
            json!({"payload": {"release_groups": (1..=25).map(group_stat).collect::<Vec<_>>()}})
        }
        ["stats", "user", _, "genre-activity"] => json!({"result": GENRES
            .iter()
            .map(|genre| json!({"genre": genre, "listen_count": 40}))
            .collect::<Vec<_>>()}),
        ["user", _, "similar-users"] => recorded(SIMILAR_USERS_BODY),
        ["user", _, "fresh_releases"] => recorded(FRESH_RELEASES_BODY),
        // `metadata=true` is not recorded yet, so those rows stay generated.
        ["feedback", ..] if metadata_read => {
            json!({"feedback": (1..=6)
                .map(|n| json!({"recording_mbid": mbid('f', n), "track_metadata": {
                    "track_name": format!("Loved {n}"), "artist_name": format!("Artist {n}"),
                    "mbid_mapping": {"artist_mbids": [mbid('a', n + 10)]}}}))
                .collect::<Vec<_>>()})
        }
        ["feedback", ..] => recorded(FEEDBACK_BODY),
        ["user", _, "playlists", "recommendations"] => recorded(RECOMMENDATION_PLAYLISTS_BODY),
        ["playlist", id] => {
            let base = number_of(id) % 1000;
            json!({"playlist": {"title": "Weekly", "date": "2024-01-01", "track": (0..50)
                .map(|n| json!({"title": format!("Track {n}"), "creator": format!("Artist {n}"),
                    "album": "Album",
                    "identifier": [format!("https://musicbrainz.org/recording/{}", mbid('7', base + n))],
                    "extension": {"https://musicbrainz.org/doc/jspf#track": {"additional_metadata":
                        {"artists": [{"artist_mbid": mbid('a', 1 + n % 8)}]}}}}))
                .collect::<Vec<_>>()}})
        }
        ["popularity", "top-release-groups-for-artist", artist] if number_of(artist) % 5 == 0 => {
            json!([])
        }
        ["lb-radio", "artist", artist] if number_of(artist) % 5 == 0 => json!({}),
        ["popularity", "top-release-groups-for-artist", artist] => {
            let base = number_of(artist) % 100_000;
            Value::Array(
                (0..25)
                    .map(|n| {
                        json!({"release_group_mbid": mbid('d', base * 30 + n),
                    "total_listen_count": 500 - n,
                    "release_group": {"name": format!("Album {base}-{n}")},
                    "artist": {"name": format!("Name of {artist}")}})
                    })
                    .collect(),
            )
        }
        ["popularity", "top-recordings-for-artist", artist] => {
            let base = number_of(artist) % 100_000;
            Value::Array(
                (0..10)
                    .map(|n| {
                        json!({"recording_name": format!("Song {n}"), "artist_name": "Someone",
                    "total_listen_count": 300 - n, "recording_mbid": mbid('8', base * 30 + n),
                    "release_name": "Album", "release_mbid": mbid('6', base)})
                    })
                    .collect(),
            )
        }
        ["popularity", "release-group", ..] => Value::Array(
            asked["release_group_mbids"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|id| json!({"release_group_mbid": id, "total_listen_count": 90}))
                .collect(),
        ),
        ["lb-radio", "artist", artist] => {
            let base = number_of(artist) % SIMILAR_POOL;
            Value::Object(
                (0..25)
                    .map(|n| {
                        let id = mbid('b', 1 + (base + n * 7) % SIMILAR_POOL);
                        let rows = json!([{"similar_artist_name": format!("Similar {id}"),
                                       "total_listen_count": 1000 - n}]);
                        (id, rows)
                    })
                    .collect(),
            )
        }
        ["metadata", "recording", ..] => Value::Object(
            asked["recording_mbids"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .map(|id| {
                    (
                        id.to_owned(),
                        json!({"release":
                    {"release_group_mbid": mbid('d', number_of(id) % 100_000)}}),
                    )
                })
                .collect(),
        ),
        _ => json!({}),
    }
}

fn lastfm_answer(uri: &Uri) -> Value {
    let query = uri.query().unwrap_or_default();
    let param = |name: &str| {
        query.split('&').find_map(|pair| {
            let (key, value) = pair.split_once('=')?;
            (key == name).then(|| value.to_owned())
        })
    };
    let asked = number_of(&param("mbid").unwrap_or_default()) % SIMILAR_POOL;
    let artists = |count: u32, kind: char| {
        (1..=count)
            .map(|n| n + if kind == 'b' { asked } else { 0 })
            .map(|n| {
                json!({"name": format!("Artist {kind}{n}"), "mbid": mbid(kind, n),
                            "playcount": "50", "match": "0.5"})
            })
            .collect::<Vec<_>>()
    };
    match param("method").as_deref().unwrap_or_default() {
        "user.getTopArtists" => json!({"topartists": {"artist": artists(10, 'a')}}),
        "chart.getTopArtists" => json!({"artists": {"artist": artists(20, 'b')}}),
        "artist.getSimilar" => json!({"similarartists": {"artist": artists(25, 'b')}}),
        "artist.getTopAlbums" => {
            let base = number_of(&param("mbid").unwrap_or_default()) % 100_000;
            json!({"topalbums": {"album": (0..10)
                .map(|n| json!({"name": format!("Lastfm Album {base}-{n}"),
                    "mbid": mbid('5', base * 30 + n), "playcount": "9",
                    "artist": {"name": "Someone"}}))
                .collect::<Vec<_>>()}})
        }
        "artist.getTopTags" | "album.getTopTags" => json!({"toptags": {"tag": [{"name": "rock"}]}}),
        "tag.getTopArtists" => json!({"topartists": {"artist": artists(5, 'b')}}),
        "user.getWeeklyArtistChart" => json!({"weeklyartistchart": {"artist": artists(5, 'a')}}),
        "user.getWeeklyAlbumChart" => json!({"weeklyalbumchart": {"album": []}}),
        "user.getTopAlbums" => json!({"topalbums": {"album": []}}),
        "user.getRecentTracks" => json!({"recenttracks": {"track": []}}),
        "user.getLovedTracks" => json!({"lovedtracks": {"track": []}}),
        _ => json!({"error": 6, "message": "not found"}),
    }
}

fn musicbrainz_answer(uri: &Uri) -> Value {
    let parts: Vec<&str> = uri
        .path()
        .split('/')
        .filter(|part| !part.is_empty())
        .collect();
    match parts.as_slice() {
        ["musicbrainz", "release", id] => json!({"id": id, "title": "Release",
            "release-group": {"id": mbid('d', number_of(id) % 100_000), "title": "Group"}}),
        ["musicbrainz", "release-group"] => {
            json!({"release-groups": [], "count": 0, "offset": 0})
        }
        ["musicbrainz", "artist"] => json!({"artists": [], "count": 0, "offset": 0}),
        _ => json!({"error": "not found"}),
    }
}

async fn mock(tally: Tally) -> String {
    let app = Router::new().fallback(move |method: Method, uri: Uri, body: Bytes| {
        let tally = tally.clone();
        async move {
            *tally
                .lock()
                .unwrap()
                .entry(endpoint(&method, &uri))
                .or_default() += 1;
            let answer = if uri.path().starts_with("/listenbrainz/") {
                lb_answer(&uri, &body)
            } else if uri.path().starts_with("/lastfm") {
                lastfm_answer(&uri)
            } else if uri.path().starts_with("/musicbrainz") {
                let answer = musicbrainz_answer(&uri);
                if answer.get("error").is_some() {
                    return axum::http::StatusCode::NOT_FOUND.into_response();
                }
                answer
            } else {
                return axum::http::StatusCode::NOT_FOUND.into_response();
            };
            axum::Json(answer).into_response() as Response
        }
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    origin
}

/// A library of 20 identified artists in four genres, three albums each.
async fn seed_library(rig: &AuthRig) {
    rig.platform
        .db
        .write(Lane::Foreground, "test.seed", |tx| {
            tx.execute(
                "INSERT INTO auth_users (id, display_name, role, created_at) \
                 VALUES ('user-1', 'Ada', 'user', 0)",
                [],
            )?;
            for artist in 1..=LIBRARY_ARTISTS {
                let (id, name) = (format!("a{artist}"), format!("Library Artist {artist}"));
                tx.execute(
                    "INSERT INTO local_artists (id, display_name, folded_name, kind, created_at, \
                     updated_at) VALUES (?1, ?2, lower(?2), 'group', 1, 1)",
                    rusqlite::params![id, name],
                )?;
                tx.execute(
                    "INSERT INTO local_artist_external_identities (local_artist_id, provider, \
                     provider_artist_id, decision_source, selected_at) \
                     VALUES (?1, 'musicbrainz', ?2, 'manual', 1)",
                    rusqlite::params![id, mbid('a', artist)],
                )?;
                for album in 0..3 {
                    let number = artist * 10 + album;
                    let (al, track) = (format!("al{number}"), format!("t{number}"));
                    let title = format!("Owned {number}");
                    tx.execute(
                        "INSERT INTO local_albums (id, root_id, grouping_key, title, title_folded, \
                         album_artist_name, album_artist_name_folded, album_artist_id, \
                         grouping_source, created_at, updated_at, year) VALUES (?1, 'r1', ?1, ?2, \
                         lower(?2), ?3, lower(?3), ?4, 'automatic', 1, 1, 1990)",
                        rusqlite::params![al, title, name, id],
                    )?;
                    tx.execute(
                        "INSERT INTO local_album_artists (local_album_id, position, \
                         local_artist_id, role) VALUES (?1, 0, ?2, 'main')",
                        rusqlite::params![al, id],
                    )?;
                    tx.execute(
                        "INSERT INTO local_album_external_identities (local_album_id, provider, \
                         release_group_mbid, decision_source, selected_at) \
                         VALUES (?1, 'musicbrainz', ?2, 'manual', 1)",
                        rusqlite::params![al, mbid('c', number)],
                    )?;
                    tx.execute(
                        "INSERT INTO local_tracks (id, local_album_id, root_id, file_path, \
                         relative_path, path_hash, file_size_bytes, file_mtime_ns, stat_revision, \
                         title, title_folded, album_title, album_title_folded, file_format, \
                         availability, ingest_source, imported_at, membership_source) VALUES \
                         (?1, ?2, 'r1', ?1, ?1, ?1, 1, 1, 's1', 'Song', 'song', ?3, lower(?3), \
                         'flac', 'indexed', 'scan', 1, 'automatic')",
                        rusqlite::params![track, al, title],
                    )?;
                    let genre = GENRES[(artist % 4) as usize];
                    tx.execute(
                        "INSERT INTO local_track_genres (local_track_id, position, name, \
                         folded_name, source) VALUES (?1, 0, ?2, ?2, 'local')",
                        rusqlite::params![track, genre],
                    )?;
                }
            }
            Ok::<_, droppedneedle_kernel::db::DbError>(())
        })
        .await
        .expect("library rows");
}

async fn get(app: &Router, uri: &str) -> Value {
    call(app, Method::GET, uri, None).await
}

async fn call(app: &Router, method: Method, uri: &str, body: Option<Value>) -> Value {
    let mut request = Request::builder().method(method).uri(uri);
    let body = match body {
        Some(body) => {
            request = request.header("content-type", "application/json");
            Body::from(body.to_string())
        }
        None => Body::empty(),
    };
    let response = app
        .clone()
        .oneshot(request.body(body).unwrap())
        .await
        .unwrap();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    serde_json::from_slice(&bytes).unwrap_or(Value::Null)
}

fn search_batch(day: u32, search: u32) -> EnrichmentBatchRequest {
    let start = day * 5 + search * 5;
    EnrichmentBatchRequest {
        artists: (start..start + 10)
            .map(|n| ArtistEnrichmentRequest {
                musicbrainz_id: mbid('4', n),
                name: format!("Searched {n}"),
            })
            .collect(),
        albums: (start..start + 10)
            .map(|n| AlbumEnrichmentRequest {
                musicbrainz_id: mbid('2', n),
                artist_name: format!("Searched {n}"),
                album_name: format!("Album {n}"),
            })
            .collect(),
    }
}

/// Calls per endpoint in each part of the journey, and what the executor
/// counted on the ListenBrainz row over all of them.
struct Measured {
    days: BTreeMap<&'static str, BTreeMap<String, u64>>,
    executor_listenbrainz: u64,
    failed_listenbrainz: u64,
}

fn since(now: &BTreeMap<String, u64>, then: &BTreeMap<String, u64>) -> BTreeMap<String, u64> {
    now.iter()
        .filter_map(|(name, count)| {
            let grew = count - then.get(name).copied().unwrap_or_default();
            (grew > 0).then(|| (name.clone(), grew))
        })
        .collect()
}

fn listenbrainz_total(calls: &BTreeMap<String, u64>) -> u64 {
    calls
        .iter()
        .filter(|(name, _)| name.starts_with("lb "))
        .map(|(_, count)| *count)
        .sum()
}

fn musicbrainz_total(calls: &BTreeMap<String, u64>) -> u64 {
    calls
        .iter()
        .filter(|(name, _)| name.starts_with("musicbrainz"))
        .map(|(_, count)| *count)
        .sum()
}

/// The journey for a user who follows `lastfm` or, otherwise, ListenBrainz.
async fn journey(tag: &str, lastfm: bool) -> Measured {
    let dir = ScratchDir::new(tag);
    let rig = AuthRig::new(tag).await;
    seed_library(&rig).await;
    let tally: Tally = Arc::default();
    let origin = mock(tally.clone()).await;
    let snapshot = || tally.lock().unwrap().clone();

    let http = droppedneedle_kernel::http_client::HttpClientFactory::new().unwrap();
    let providers = Providers::unpaced_on(
        Arc::new(ReqwestWire::new(&http)),
        rig.platform.clock.clone(),
        ProviderEndpoints {
            listenbrainz: format!("{origin}/listenbrainz"),
            lastfm: format!("{origin}/lastfm/"),
            musicbrainz: format!("{origin}/musicbrainz"),
            brainzmash: format!("{origin}/musicbrainz"),
            ..ProviderEndpoints::closed()
        },
    );
    rig.config
        .save_secret_blocking(ListenBrainzConnection {
            username: String::new(),
            user_token: Secret::new(""),
            enabled: true,
        })
        .expect("ListenBrainz on");
    let listening = crate::common::listening_store(&rig.platform);
    if lastfm {
        rig.set_lastfm_instance_key("instance-key");
        rig.deps
            .lastfm
            .upsert(
                USER,
                LastFmConnection {
                    username: Some("scrobbler".to_owned()),
                    ..LastFmConnection::default()
                },
            )
            .await
            .expect("Last.fm link");
        listening
            .update_prefs(
                USER,
                &PrefsPatch {
                    primary_music_source: Some(
                        droppedneedle_kernel::runtime_config::sections::ListenSource::Lastfm,
                    ),
                    ..PrefsPatch::default()
                },
            )
            .await
            .expect("prefs");
    } else {
        listening
            .save_listenbrainz(USER, "listener", "token")
            .await
            .expect("ListenBrainz link");
    }

    let mut inputs = discover_inputs(&rig, &dir, providers);
    inputs.discover_warmer = true;
    let providers = inputs.providers.clone();
    let network = live_network(&inputs, &rig.config);
    let namespace = network.source().namespace();
    let deps = deps_over(inputs, &rig.config, network);
    let content = deps.content.clone();
    let app = discover_routes(deps);
    let catalog = Catalog::new(
        Upstream::new(
            providers.clone(),
            Arc::new(LiveCatalogSettings::new(&rig.config)),
            rig.deps.clone(),
        ),
        LocalCatalog::new(rig.platform.db.clone()),
        crate::common::bundles::quiet_page_sources(crate::common::bundles::cover_warm_at(
            &rig.platform.db,
            rig.platform.clock.clone(),
        )),
    );
    let counts = SearchCounts::new(
        &providers,
        rig.config.live(),
        rig.config.live(),
        listening.clone(),
        rig.deps.clone(),
    );
    let collections = crate::common::bundles::collections_at(&rig.platform);
    let (library, _) = crate::common::library::library_at(&rig.platform);
    let acquire = crate::common::bundles::acquire_at(
        &rig.platform,
        rig.deps.clone(),
        &library,
        &collections.stores,
    );
    let requests = RequestsState {
        quota: Arc::new(
            droppedneedle_acquire::requests::quota::QuotaLedger::unlimited(acquire.db.clone()),
        ),
        ..acquire.requests.clone()
    };
    let mixes = mix_sources::live_builder(
        &requests,
        &providers,
        listening.clone(),
        PlaylistStore::new(collections.db.clone()),
        EventHub::new(),
    );

    let activity = ActivityDb::new(rig.platform.db.clone());
    let start = rig.clock().now_ms();
    let source = if lastfm { "lastfm" } else { "listenbrainz" };
    let mut days = BTreeMap::new();
    for (name, day) in DAYS {
        let before = snapshot();
        for hour in 0..24 {
            rig.clock().set(start + i64::from(day * 24 + hour) * HOUR_MS);
            if matches!(hour, 0 | 5 | 12 | 19) {
                let visit = hour / 5;
                for feature in [Feature::Home, Feature::Discover, Feature::Queue] {
                    activity
                        .record(
                            Activity {
                                user_id: USER.to_owned(),
                                feature,
                                artist_mbid: String::new(),
                                section: String::new(),
                                provider: String::new(),
                                source: namespace.clone(),
                            },
                            rig.clock().now_ms(),
                        )
                        .await
                        .expect("activity row");
                }
                for page in ["/api/v3/discover", "/api/v3/home"] {
                    after_commits(&rig.platform.db, page, || async {
                        (get(&app, page).await["refreshing"] == false).then_some(())
                    })
                    .await;
                }
                get(&app, "/api/v3/discover/queue?count=10").await;
                let found = enrich_with_degradation(&counts, USER, search_batch(day, visit)).await;
                assert!(
                    found.degradations.is_empty(),
                    "the mock answers every count"
                );
                if hour == 0 {
                    let (asked, similar_to) = (mbid('b', day + 1), mbid('a', day + 1));
                    let priority = RequestPriority::UserInitiated;
                    let wanted = Some(if lastfm {
                        droppedneedle_kernel::runtime_config::sections::ListenSource::Lastfm
                    } else {
                        droppedneedle_kernel::runtime_config::sections::ListenSource::Listenbrainz
                    });
                    for artist in [&asked, &similar_to] {
                        catalog
                            .similar_artists(USER, artist, 15, wanted, priority)
                            .await
                            .expect("similar artists");
                        catalog
                            .top_songs(USER, artist, 10, wanted, priority)
                            .await
                            .expect("top songs");
                        catalog
                            .top_albums(USER, artist, 10, wanted, priority)
                            .await
                            .expect("top albums");
                    }
                    for (album, artist) in [
                        (mbid('c', 2000 + day), &asked),
                        (mbid('c', 3000 + day), &similar_to),
                    ] {
                        catalog
                            .similar_albums(USER, &album, artist, 10)
                            .await
                            .expect("similar albums");
                    }
                    call(
                        &app,
                        Method::POST,
                        "/api/v3/discover/radio",
                        Some(json!({"seed_type": "artist", "seed_id": similar_to,
                                    "count": 10, "source": source})),
                    )
                    .await;
                }
            }
            content.run_due_tick().await;
        }
        if !lastfm && day != 1 {
            mixes.build_for_user(USER, true).await.expect("Weekly Mix");
        }
        days.insert(name, since(&snapshot(), &before));
    }
    let limiter = providers.limiter("listenbrainz").expect("the row");
    let row = limiter.calls();
    let measured = Measured {
        days,
        executor_listenbrainz: row.iter().map(|count| count.calls).sum(),
        failed_listenbrainz: row
            .iter()
            .filter(|count| count.outcome != CallOutcome::Ok)
            .map(|count| count.calls)
            .sum(),
    };
    for (part, calls) in &measured.days {
        for (name, count) in calls {
            println!("{tag}: {part},{name},{count}");
        }
        println!(
            "{tag}: {part},listenbrainz total,{}",
            listenbrainz_total(calls)
        );
        println!(
            "{tag}: {part},musicbrainz total,{}",
            musicbrainz_total(calls)
        );
    }
    measured
}

/// One active user's day stays under its bound, whichever service they
/// follow, and the executor sees what the mock saw. A user who follows
/// Last.fm costs MetaBrainz less in all than the old code's did on any day
/// after the first week.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn one_active_users_days_stay_under_their_bounds() {
    let listenbrainz = journey("lb-load", false).await;
    let lastfm = journey("lastfm-load", true).await;

    let counted: u64 = listenbrainz.days.values().map(listenbrainz_total).sum();
    assert_eq!(listenbrainz.executor_listenbrainz, counted);
    assert_eq!(listenbrainz.failed_listenbrainz, 0);
    for (name, bound) in LISTENBRAINZ_BOUNDS {
        let calls = listenbrainz_total(&listenbrainz.days[name]);
        assert!(calls <= bound, "{name}: {calls} calls, bound {bound}");
    }
    for (name, bound) in LASTFM_BOUNDS {
        let day = &lastfm.days[name];
        let calls = listenbrainz_total(day) + musicbrainz_total(day);
        assert!(
            calls <= bound,
            "Last.fm user, {name}: {calls} calls, bound {bound}"
        );
    }
}
