//! How much one active user asks of ListenBrainz in a day.
//!
//! MetaBrainz counts every install, so the warm day's total is pinned. The
//! journey runs the production discover wiring, search counts, artist pages,
//! radio and the Weekly Mix against a mock of the providers, on a manual
//! clock: a cold day, a warm day, the Monday the Daily Mix seeds rotate, and
//! day 15, past the 14-day rows, with a Weekly Mix after it. Each day has
//! four visits, four searches, two artist pages, two album pages, one radio
//! shelf and the warm cycle ticking hourly, the kept facts' refresh running
//! after each tick. One artist in five has nothing on ListenBrainz or
//! MusicBrainz. Run it with `--nocapture` for the calls per endpoint.
//!
//! Beside it: what an artist's albums read offers from those answers.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use axum::Router;
use axum::body::{Body, Bytes};
use axum::http::{Method, Request, Uri};
use axum::response::{IntoResponse, Response};
use droppedneedle_acquire::requests::{RequestsState, mix_sources};
use droppedneedle_auth::users::models::LastFmConnection;
use droppedneedle_catalog::collections::store::playlists::PlaylistStore;
use droppedneedle_catalog::discover::listening::shared;
use droppedneedle_catalog::discover::listening::{AlbumFacts, AlbumRow, RankedAlbum, gate};
use droppedneedle_catalog::discover::models::{AlbumRef, SectionItem};
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
use droppedneedle_media::playback::listening::PrefsPatch;
use droppedneedle_providers::executor::{Body as WireBody, Method as WireMethod};
use droppedneedle_providers::test_support::{Scripted, ScriptedWire};
use droppedneedle_providers::{
    CallOutcome, ProviderEndpoints, Providers, RequestPriority, ReqwestWire, listenbrainz,
};
use serde_json::{Value, json};
use tower::ServiceExt as _;

use crate::common::ScratchDir;
use crate::common::auth::AuthRig;
use crate::discover_queue::{after_commits, discover_inputs, discover_routes};

/// The most ListenBrainz calls each day of this journey may make. The code
/// before similar artists, counts and album lists were kept facts made 197,
/// 52, 128 and 155; this code makes 129, 52, 81 and 87, the album counts
/// asked across artists in one read by the kept facts' refresh.
const LISTENBRAINZ_BOUNDS: [(&str, u64); 4] =
    [("cold", 140), ("warm", 60), ("monday", 90), ("day 15", 95)];
/// The most MusicBrainz calls each day for the same user. Artists' album
/// lists read their discographies, about one browse an artist every 14
/// days, an artist with nothing to offer included; the old code made 12 a
/// day, and this code 173, 16, 90 and 130. On a default install these go to
/// BrainzMash, not MusicBrainz itself.
const MUSICBRAINZ_BOUNDS: [(&str, u64); 4] = [
    ("cold", 185),
    ("warm", 25),
    ("monday", 100),
    ("day 15", 140),
];
/// The most ListenBrainz plus MusicBrainz calls for a user who follows
/// Last.fm. Similar artists come from ListenBrainz for everyone, which adds
/// its list and count reads: 5 to 18 a day here. The rest are MusicBrainz
/// lookups matching Last.fm albums, at most 30 a build: with the similar
/// lists kept, Monday's builds reach more albums to match than before
/// (150 lookups, 90 before), on BrainzMash on a default install.
const LASTFM_BOUNDS: [(&str, u64); 4] = [
    ("cold", 315),
    ("warm", 165),
    ("monday", 175),
    ("day 15", 100),
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

/// The value of `name` in `uri`'s query.
fn query_value(uri: &Uri, name: &str) -> Option<String> {
    uri.query()?.split('&').find_map(|pair| {
        let (key, value) = pair.split_once('=')?;
        (key == name).then(|| value.to_owned())
    })
}

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
        ["labs", "similar-artists", ..] => "lb labs similar-artists".to_owned(),
        ["listenbrainz", "1", "metadata", "recording", ..] => "lb metadata/recording".to_owned(),
        ["listenbrainz", "1", "stats", "sitewide", name] => format!("lb stats/sitewide/{name}"),
        ["listenbrainz", "1", "stats", "user", _, name] => format!("lb stats/user/{name}"),
        ["listenbrainz", "1", "feedback", ..] => "lb feedback".to_owned(),
        ["listenbrainz", "1", "user", _, name, ..] => format!("lb user/{name}"),
        ["listenbrainz", "1", "playlist", ..] => "lb playlist".to_owned(),
        ["listenbrainz", ..] => format!("lb other {method} {}", uri.path()),
        ["musicbrainz", kind, rest @ ..] => {
            let how = if !rest.is_empty() {
                ""
            } else if query_value(uri, "artist").is_some() {
                " browse"
            } else {
                " search"
            };
            format!("musicbrainz {kind}{how}")
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
        ["stats", "user", _, "genre-activity"] => json!({"payload": {"genre_activity": GENRES
            .iter()
            .map(|genre| json!({"genre": genre, "listen_count": 40}))
            .collect::<Vec<_>>()}}),
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
        ["popularity", "top-release-groups-for-artist", artist]
            if number_of(artist).is_multiple_of(5) =>
        {
            json!([])
        }
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
                .filter_map(Value::as_str)
                .map(|id| {
                    let listeners = 1 + number_of(id) % 5_000;
                    json!({"release_group_mbid": id, "total_listen_count": listeners * 9,
                           "total_user_count": listeners})
                })
                .collect(),
        ),
        ["popularity", "artist"] => Value::Array(
            asked["artist_mbids"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .map(|id| {
                    let listeners = 1 + number_of(id) % 50_000;
                    json!({"artist_mbid": id, "total_listen_count": listeners * 10,
                           "total_user_count": listeners})
                })
                .collect(),
        ),
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

/// Labs' similar artists for the asked artist: 25 of the pool, none for one
/// artist in five.
fn labs_answer(uri: &Uri) -> Value {
    let artist = uri
        .query()
        .unwrap_or_default()
        .split('&')
        .find_map(|pair| pair.strip_prefix("artist_mbids="))
        .unwrap_or_default()
        .to_owned();
    if number_of(&artist).is_multiple_of(5) {
        return json!([]);
    }
    let base = number_of(&artist) % SIMILAR_POOL;
    Value::Array(
        (0..25)
            .map(|n| {
                let id = mbid('b', 1 + (base + n * 7) % SIMILAR_POOL);
                json!({"artist_mbid": id, "name": format!("Similar {id}"),
                       "score": 1000 - n, "reference_mbid": artist})
            })
            .collect(),
    )
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
        ["musicbrainz", "release-group"] => match query_value(uri, "artist") {
            // An artist's discography: none for one artist in five, else
            // twelve groups, a single in every four and one live album.
            Some(artist) if !number_of(&artist).is_multiple_of(5) => {
                let base = number_of(&artist) % 100_000;
                let groups: Vec<Value> = (0..12)
                    .map(|n| {
                        json!({"id": mbid('d', base * 30 + n), "title": format!("Album {base}-{n}"),
                            "primary-type": if n % 4 == 3 { "Single" } else { "Album" },
                            "secondary-types": if n == 5 { vec!["Live"] } else { Vec::new() },
                            "first-release-date": format!("{}", 1990 + n),
                            "artist-credit": [{"name": format!("Name of {artist}"),
                                "joinphrase": "", "artist": {"id": artist, "name": "Someone"}}]})
                    })
                    .collect();
                json!({"release-groups": groups, "release-group-count": 12,
                       "release-group-offset": 0})
            }
            _ => json!({"release-groups": [], "release-group-count": 0, "release-group-offset": 0}),
        },
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
            } else if uri.path().starts_with("/labs/") {
                labs_answer(&uri)
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

/// The user's session is used now: they are seen.
async fn seen_now(rig: &AuthRig) {
    let now = rig.clock().now_ms();
    rig.platform
        .db
        .write(Lane::Foreground, "test.seen", move |tx| {
            tx.execute(
                "INSERT INTO auth_tokens (id, user_id, token_hash, issued_at, expires_at, \
                 last_seen_at, revoked) VALUES ('s1', 'user-1', 'h1', 0, ?1, ?1, 0) \
                 ON CONFLICT (id) DO UPDATE SET last_seen_at = excluded.last_seen_at",
                [now],
            )?;
            Ok::<_, droppedneedle_kernel::db::DbError>(())
        })
        .await
        .expect("seen");
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
            listenbrainz_labs: format!("{origin}/labs"),
            lastfm: format!("{origin}/lastfm/"),
            musicbrainz: format!("{origin}/musicbrainz"),
            brainzmash: format!("{origin}/musicbrainz"),
            ..ProviderEndpoints::closed()
        },
    );
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
    let refresh = deps.facts.clone();
    let app = discover_routes(deps);
    let catalog = Catalog::new(
        Upstream::new(
            providers.clone(),
            Arc::new(LiveCatalogSettings::new(&rig.config)),
            rig.deps.clone(),
            crate::common::listening_store(&rig.platform),
        ),
        LocalCatalog::new(rig.platform.db.clone()),
        crate::common::bundles::quiet_page_sources(&rig.platform.db, rig.platform.clock.clone()),
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
            droppedneedle_acquire::requests::quota::QuotaLedger::unlimited(acquire.db.clone())
                .expect("quota ledger"),
        ),
        ..acquire.requests.clone()
    };
    let mixes = mix_sources::live_builder(
        &requests,
        &providers,
        listening.clone(),
        PlaylistStore::new(collections.db.clone()),
        EventHub::new(),
        rig.config.live(),
    );

    let activity = ActivityDb::new(rig.platform.db.clone());
    let start = rig.clock().now_ms();
    let source = if lastfm { "lastfm" } else { "listenbrainz" };
    let mut days = BTreeMap::new();
    for (name, day) in DAYS {
        let before = snapshot();
        for hour in 0..24 {
            rig.clock()
                .set(start + i64::from(day * 24 + hour) * HOUR_MS);
            if matches!(hour, 0 | 5 | 12 | 19) {
                let visit = hour / 5;
                seen_now(&rig).await;
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
            refresh
                .run_once(&|| false)
                .await
                .expect("the kept facts' refresh runs");
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
    for (name, bound) in MUSICBRAINZ_BOUNDS {
        let calls = musicbrainz_total(&listenbrainz.days[name]);
        assert!(
            calls <= bound,
            "{name}: {calls} MusicBrainz calls, bound {bound}"
        );
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

const RADIOHEAD: &str = "a74b1b7f-71a5-4011-9441-d0b5e4122711";
const RADIOHEAD_GROUPS: &str =
    include_str!("../fixtures/listenbrainz/top_release_groups_radiohead.json");

/// An album row with these facts, credited to `credited` when given.
fn faceted(
    primary: Option<&str>,
    secondary: Option<&[&str]>,
    credited: Option<&[&str]>,
) -> AlbumRow {
    let owned = |list: &[&str]| list.iter().map(|text| (*text).to_owned()).collect();
    AlbumRow {
        release_group_mbid: mbid('e', 1),
        title: "Album".to_owned(),
        artist_name: "Radiohead".to_owned(),
        artist_mbid: Some(RADIOHEAD.to_owned()),
        facts: AlbumFacts {
            primary_type: primary.map(str::to_owned),
            secondary_types: secondary.map(owned),
            credited_artist_mbids: credited.map(owned),
            ..AlbumFacts::default()
        },
    }
}

const MUSICBRAINZ: &str = "https://musicbrainz.test/ws/2";
const RADIOHEAD_COUNTS: &str =
    include_str!("../fixtures/listenbrainz/popularity_release_group_radiohead_post.json");
/// Various Artists, credited alone on one recorded group.
const VARIOUS_ARTISTS: &str = "89ad4ac3-39f7-470e-963a-56509c546377";

/// Radiohead's release groups of these titles in MusicBrainz's browse
/// shape, `count` listed in all: the ids, titles and dates of the recorded
/// top release groups, with the types and credit given here.
fn discography(groups: &[Group], count: usize) -> String {
    let recorded: Vec<Value> = serde_json::from_str(RADIOHEAD_GROUPS).expect("fixture parses");
    let rows: Vec<Value> = groups
        .iter()
        .map(|(title, primary, secondary, credited)| {
            let row = recorded
                .iter()
                .find(|row| row["release_group"]["name"] == *title)
                .expect("a recorded group");
            json!({
                "id": row["release_group_mbid"],
                "title": title,
                "primary-type": primary,
                "secondary-types": secondary,
                "first-release-date": row["release_group"]["date"],
                "artist-credit": [{"name": "Someone", "joinphrase": "",
                    "artist": {"id": credited, "name": "Someone"}}],
            })
        })
        .collect();
    json!({"release-group-count": count, "release-group-offset": 0, "release-groups": rows})
        .to_string()
}

/// A title, primary type, secondary types and the one credited artist.
type Group = (
    &'static str,
    &'static str,
    &'static [&'static str],
    &'static str,
);

/// Four albums, an EP and two albums ListenBrainz has no count for, which
/// may be offered; a live album, a demo set, a compilation, a single and
/// an album credited only to someone else, which may not.
const GROUPS: [Group; 11] = [
    ("High & Dry", "Single", &[], RADIOHEAD),
    ("Painkiller", "Album", &[], RADIOHEAD),
    ("The Best Of", "Album", &["Compilation"], RADIOHEAD),
    ("Kid A", "Album", &[], RADIOHEAD),
    ("On a Friday Demos 1", "Album", &["Demo"], RADIOHEAD),
    ("Com Lag: 2plus2isfive", "EP", &[], RADIOHEAD),
    ("Acoustic Album", "Album", &[], VARIOUS_ARTISTS),
    ("OK Computer", "Album", &[], RADIOHEAD),
    (
        "I Might Be Wrong: Live Recordings",
        "Album",
        &["Live"],
        RADIOHEAD,
    ),
    ("Oxford Angels: The Rarities", "Album", &[], RADIOHEAD),
    ("In Rainbows", "Album", &[], RADIOHEAD),
];

fn popularity_url() -> String {
    format!(
        "{}/1/popularity/release-group",
        listenbrainz::DEFAULT_BASE_URL
    )
}

/// The catalog and providers over a wire that answers the album list's two
/// reads: `discography_body` for the MusicBrainz browse and `counts` for
/// ListenBrainz's popularity POST.
fn album_reads(
    rig: &AuthRig,
    discography_body: String,
    counts: Scripted,
) -> (Catalog, Arc<Providers>, Arc<ScriptedWire>) {
    let wire = Arc::new(ScriptedWire::new());
    wire.answer(
        &format!("{MUSICBRAINZ}/release-group"),
        200,
        &[],
        discography_body,
    );
    wire.on(WireMethod::Post, &popularity_url(), counts);
    let providers = Arc::new(Providers::unpaced_on(
        wire.clone(),
        rig.platform.clock.clone(),
        ProviderEndpoints {
            listenbrainz: listenbrainz::DEFAULT_BASE_URL.to_owned(),
            musicbrainz: MUSICBRAINZ.to_owned(),
            brainzmash: MUSICBRAINZ.to_owned(),
            ..ProviderEndpoints::closed()
        },
    ));
    let catalog = Catalog::new(
        Upstream::new(
            providers.clone(),
            Arc::new(LiveCatalogSettings::new(&rig.config)),
            rig.deps.clone(),
            crate::common::listening_store(&rig.platform),
        ),
        LocalCatalog::new(rig.platform.db.clone()),
        crate::common::bundles::quiet_page_sources(&rig.platform.db, rig.platform.clock.clone()),
    );
    (catalog, providers, wire)
}

fn recorded_counts() -> Scripted {
    Scripted::Answer {
        status: 200,
        headers: Vec::new(),
        body: RADIOHEAD_COUNTS.as_bytes().to_vec(),
    }
}

/// The titles offered for Radiohead, `None` while its list is not read yet.
async fn offered(catalog: &Catalog, providers: &Providers) -> Option<Vec<String>> {
    let rows = shared::artist_albums(
        catalog,
        &providers.listenbrainz,
        RADIOHEAD,
        100,
        RequestPriority::UserInitiated,
    )
    .await
    .expect("the albums read")?;
    Some(rows.into_iter().map(|row| row.album.title).collect())
}

async fn offered_titles(catalog: &Catalog, providers: &Providers) -> Vec<String> {
    offered(catalog, providers).await.expect("a whole list")
}

/// An artist's albums come from its discography, counted with no token:
/// its own albums and EPs, most listeners first, then the ones ListenBrainz
/// has no count for, newest first. A live album, a demo set, a compilation,
/// a single and an album credited to someone else are not offered. One
/// discography read and one count read of the offered groups make the
/// list, and it is kept.
#[tokio::test]
async fn an_artists_albums_are_ordered_by_listeners() {
    let rig = AuthRig::new("lb-albums-ranked").await;
    let (catalog, providers, wire) =
        album_reads(&rig, discography(&GROUPS, GROUPS.len()), recorded_counts());

    let titles = offered_titles(&catalog, &providers).await;
    assert_eq!(
        titles,
        [
            "OK Computer",
            "In Rainbows",
            "Kid A",
            "Com Lag: 2plus2isfive",
            "Oxford Angels: The Rarities",
            "Painkiller",
        ]
    );
    assert_eq!(offered_titles(&catalog, &providers).await, titles);
    let sent = wire.sent();
    assert_eq!(sent.len(), 2, "one discography read and one count read");
    let counted = sent
        .iter()
        .find(|sent| sent.url.as_str() == popularity_url())
        .expect("the count read");
    let WireBody::Json(body) = &counted.body else {
        panic!("the count read sends JSON");
    };
    let asked: Value = serde_json::from_slice(body).expect("its body");
    assert_eq!(
        asked["release_group_mbids"].as_array().map(Vec::len),
        Some(titles.len()),
        "only the groups that may be offered are counted"
    );
}

/// While ListenBrainz will not count, the list still answers, newest first
/// with the undated last, and is not kept: once it counts again the next
/// read is ranked.
#[tokio::test]
async fn with_popularity_down_the_list_is_in_date_order() {
    let rig = AuthRig::new("lb-albums-dated").await;
    let (catalog, providers, wire) = album_reads(
        &rig,
        discography(&GROUPS, GROUPS.len()),
        Scripted::Answer {
            status: 500,
            headers: Vec::new(),
            body: b"This endpoint is currently disabled".to_vec(),
        },
    );
    // Answered after the refusal.
    wire.on(WireMethod::Post, &popularity_url(), recorded_counts());

    assert_eq!(
        offered_titles(&catalog, &providers).await,
        [
            "Oxford Angels: The Rarities",
            "In Rainbows",
            "Com Lag: 2plus2isfive",
            "Kid A",
            "OK Computer",
            "Painkiller",
        ]
    );
    assert_eq!(
        offered_titles(&catalog, &providers).await[0],
        "OK Computer",
        "the dated list was not kept"
    );
}

/// A discography over 100 groups answers its first page at once and the
/// rest later: until it is whole the artist's list is not read yet, and
/// nothing is counted for it.
#[tokio::test]
async fn a_partial_discography_offers_nothing() {
    let rig = AuthRig::new("lb-albums-partial").await;
    let (catalog, providers, wire) =
        album_reads(&rig, discography(&GROUPS, 150), recorded_counts());

    assert_eq!(offered(&catalog, &providers).await, None);
    assert!(
        wire.sent()
            .iter()
            .all(|sent| sent.url.as_str() != popularity_url()),
        "nothing is counted for a list that is not whole"
    );
}

/// An artist with no album or EP to offer, read without a failure, is not
/// asked again the next day.
#[tokio::test]
async fn an_artist_with_nothing_to_offer_is_not_asked_again_the_next_day() {
    let rig = AuthRig::new("lb-albums-none").await;
    let singles: [Group; 1] = [("High & Dry", "Single", &[], RADIOHEAD)];
    let (catalog, providers, wire) = album_reads(&rig, discography(&singles, 1), recorded_counts());

    assert!(offered_titles(&catalog, &providers).await.is_empty());
    let asked = wire.sent().len();
    rig.clock().advance(2 * 86_400_000);
    assert!(offered_titles(&catalog, &providers).await.is_empty());
    assert_eq!(wire.sent().len(), asked, "nothing was asked again");
}

/// A release group whose secondary types name a live set, a demo, a
/// compilation, a soundtrack, a remix album, a DJ mix or a spoken record
/// (in any case) is never offered; one with none, or other types, is, and
/// so is a row nothing is known about. A row whose credit leaves the artist
/// out is dropped.
#[test]
fn left_out_release_kinds_are_never_offered() {
    let credited = Some(&[RADIOHEAD][..]);
    for kind in [
        "Live",
        "Demo",
        "Compilation",
        "Soundtrack",
        "Remix",
        "DJ-mix",
        "Spokenword",
        "Interview",
        "Audiobook",
        "Audio drama",
        "audio DRAMA",
    ] {
        let row = faceted(Some("Album"), Some(&["Mixtape/Street", kind]), credited);
        assert!(!gate::may_offer(&row, RADIOHEAD), "{kind}");
    }
    for secondary in [
        Some(&[][..]),
        Some(&["Mixtape/Street"][..]),
        Some(&["Field recording"][..]),
        None,
    ] {
        let row = faceted(Some("EP"), secondary, credited);
        assert!(gate::may_offer(&row, RADIOHEAD), "{secondary:?}");
    }
    assert!(gate::may_offer(&faceted(None, None, None), RADIOHEAD));
    let other = mbid('f', 1);
    let elsewhere = faceted(Some("Album"), None, Some(&[other.as_str()]));
    assert!(!gate::may_offer(&elsewhere, RADIOHEAD));
}

/// An album row kept in the provider cache before its facts were read
/// still decodes, with none known, and is offered.
#[test]
fn an_album_row_kept_before_its_facts_still_reads() {
    let kept: Vec<RankedAlbum> = serde_json::from_value(json!([{
        "album": {
            "release_group_mbid": mbid('e', 2),
            "title": "In Rainbows",
            "artist_name": "Radiohead",
            "artist_mbid": RADIOHEAD,
        },
        "listen_count": 12,
    }]))
    .expect("an old row decodes");
    assert_eq!(kept[0].album.facts, AlbumFacts::default());
    assert_eq!(gate::offered(kept, RADIOHEAD).len(), 1);
}

/// An album shown on a shelf carries its date, year and type, and keeps
/// the `kind` that tags it as an album row: the type has its own field.
#[test]
fn a_shown_album_keeps_its_row_tag() {
    let mut row = faceted(Some("EP"), None, None);
    row.facts.first_release_date = Some("2004-03-24".to_owned());
    let item = SectionItem::Album(row.shown(AlbumRef::titled("Com Lag")));
    let wire = serde_json::to_value(&item).expect("the row encodes");
    assert_eq!(wire["kind"], "album", "{wire}");
    assert_eq!(wire["release_type"], "ep", "{wire}");
    assert_eq!(wire["year"], 2004, "{wire}");
    assert_eq!(wire["release_date"], "2004-03-24", "{wire}");
    let back: SectionItem = serde_json::from_value(wire).expect("the row decodes");
    assert_eq!(back, item);
}
