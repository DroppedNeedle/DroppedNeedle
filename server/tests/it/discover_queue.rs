//! The discover queue deck end to end over a real database: a background
//! build from scripted ListenBrainz answers, an ignore that a rebuilt deck
//! honours, and both the ignore and the deck surviving a restart.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use axum::{
    Router,
    body::Body,
    http::{Request, StatusCode},
};
use droppedneedle::auth::session::extract::Transport;
use droppedneedle::auth::session::middleware::CurrentSession;
use droppedneedle::auth::session::store::SessionKind;
use droppedneedle::auth::users::memory::with_test_principal;
use droppedneedle::db::{DbConfig, DbRuntime, open_runtime};
use droppedneedle::ids::UuidGenerator;
use droppedneedle::providers::Providers;
use droppedneedle::reads::discover::adapters::queue::sources::{
    AlbumRow, ArtistFacts, ArtistRow, JellyfinList, LastFmAlbum, LastFmFacts, MusicSource,
    QueueSources, ReleaseGroupFacts, SourceResult, StatsRange, UserMusic,
};
use droppedneedle::reads::discover::adapters::queue::store::QueueDb;
use droppedneedle::reads::discover::adapters::queue::{LiveQueue, QueueSettings};
use droppedneedle::reads::discover::fakes::{
    FakeBatches, FakeCharts, FakeContent, FakeNowPlaying, FakePreviews, FakeRadio, FakeYouTube,
    ManualClock,
};
use droppedneedle::reads::discover::ports::{BoxFuture, SystemClock};
use droppedneedle::reads::discover::services::ReadsDeps;
use serde_json::{Value, json};
use tower::ServiceExt as _;

const SEED: &str = "11111111-1111-4111-8111-111111111111";
const SIMILAR: &str = "22222222-2222-4222-8222-222222222222";

fn group(n: u8) -> String {
    format!("{n:08x}-aaaa-4aaa-8aaa-aaaaaaaaaaaa")
}

/// ListenBrainz answers for one linked user: one seed artist, one similar
/// artist with five albums, and five trending albums. Everything else is
/// empty.
struct Scripted;

fn ok<T: Send + 'static>(value: T) -> BoxFuture<'static, SourceResult<T>> {
    Box::pin(async move { Ok(value) })
}

fn album(n: u8, artist: &str) -> AlbumRow {
    AlbumRow {
        release_group_mbid: group(n),
        title: format!("Album {n}"),
        artist_name: "Someone".to_owned(),
        artist_mbid: Some(artist.to_owned()),
    }
}

impl QueueSources for Scripted {
    fn source_key(&self) -> String {
        "official:g0".to_owned()
    }
    fn user_music<'a>(&'a self, _user_id: &'a str) -> BoxFuture<'a, UserMusic> {
        Box::pin(async {
            UserMusic {
                listenbrainz: Some("listener".to_owned()),
                lastfm_username: None,
                lastfm: false,
                jellyfin: false,
                primary: MusicSource::ListenBrainz,
            }
        })
    }
    fn listenbrainz_top_artists<'a>(
        &'a self,
        _username: &'a str,
        _range: StatsRange,
        _count: u32,
    ) -> BoxFuture<'a, SourceResult<Vec<ArtistRow>>> {
        ok(vec![ArtistRow {
            name: "Seed".to_owned(),
            mbid: Some(SEED.to_owned()),
            listen_count: 10,
        }])
    }
    fn listenbrainz_top_albums<'a>(
        &'a self,
        _username: &'a str,
        _range: StatsRange,
        _count: u32,
    ) -> BoxFuture<'a, SourceResult<Vec<AlbumRow>>> {
        ok(Vec::new())
    }
    fn listenbrainz_similar_artists<'a>(
        &'a self,
        _user_id: &'a str,
        artist_mbid: &'a str,
        _limit: usize,
    ) -> BoxFuture<'a, SourceResult<Vec<ArtistRow>>> {
        let rows = if artist_mbid == SEED {
            vec![ArtistRow {
                name: "Similar".to_owned(),
                mbid: Some(SIMILAR.to_owned()),
                listen_count: 5,
            }]
        } else {
            Vec::new()
        };
        ok(rows)
    }
    fn listenbrainz_artist_albums<'a>(
        &'a self,
        _user_id: &'a str,
        artist_mbid: &'a str,
        _count: usize,
    ) -> BoxFuture<'a, SourceResult<Vec<AlbumRow>>> {
        let rows = if artist_mbid == SIMILAR {
            (1..=5).map(|n| album(n, SIMILAR)).collect()
        } else {
            Vec::new()
        };
        ok(rows)
    }
    fn listenbrainz_genres<'a>(&'a self, _u: &'a str) -> BoxFuture<'a, SourceResult<Vec<String>>> {
        ok(Vec::new())
    }
    fn listenbrainz_fresh_releases<'a>(
        &'a self,
        _u: &'a str,
    ) -> BoxFuture<'a, SourceResult<Vec<AlbumRow>>> {
        ok(Vec::new())
    }
    fn listenbrainz_loved_artists<'a>(
        &'a self,
        _u: &'a str,
        _c: u32,
    ) -> BoxFuture<'a, SourceResult<Vec<String>>> {
        ok(Vec::new())
    }
    fn listenbrainz_trending(&self, _count: u32) -> BoxFuture<'_, SourceResult<Vec<AlbumRow>>> {
        // Distinct artists each, so the per-artist cap never bites.
        ok((11..=15)
            .map(|n| album(n, &format!("{n:08x}-bbbb-4bbb-8bbb-bbbbbbbbbbbb")))
            .collect())
    }
    fn listenbrainz_listen_counts<'a>(
        &'a self,
        _user_id: &'a str,
        _ids: &'a [String],
    ) -> BoxFuture<'a, SourceResult<HashMap<String, i64>>> {
        ok(HashMap::new())
    }
    fn listenbrainz_popularity_down(&self) -> bool {
        false
    }
    fn lastfm_top_artists<'a>(
        &'a self,
        _u: &'a str,
        _n: &'a str,
        _l: u32,
    ) -> BoxFuture<'a, SourceResult<Vec<ArtistRow>>> {
        ok(Vec::new())
    }
    fn lastfm_similar_artists<'a>(
        &'a self,
        _u: &'a str,
        _a: &'a ArtistRow,
        _l: u32,
    ) -> BoxFuture<'a, SourceResult<Vec<ArtistRow>>> {
        ok(Vec::new())
    }
    fn lastfm_artist_albums<'a>(
        &'a self,
        _u: &'a str,
        _a: &'a ArtistRow,
        _l: u32,
    ) -> BoxFuture<'a, SourceResult<Vec<LastFmAlbum>>> {
        ok(Vec::new())
    }
    fn lastfm_chart_artists<'a>(
        &'a self,
        _u: &'a str,
        _l: u32,
    ) -> BoxFuture<'a, SourceResult<Vec<ArtistRow>>> {
        ok(Vec::new())
    }
    fn lastfm_album_facts<'a>(
        &'a self,
        _u: &'a str,
        _a: &'a str,
        _b: &'a str,
    ) -> BoxFuture<'a, SourceResult<Option<LastFmFacts>>> {
        ok(None)
    }
    fn lastfm_artist_facts<'a>(
        &'a self,
        _u: &'a str,
        _a: &'a str,
        _m: Option<&'a str>,
    ) -> BoxFuture<'a, SourceResult<Option<LastFmFacts>>> {
        ok(None)
    }
    fn jellyfin_artists<'a>(
        &'a self,
        _u: &'a str,
        _list: JellyfinList,
        _l: u32,
    ) -> BoxFuture<'a, SourceResult<Vec<ArtistRow>>> {
        ok(Vec::new())
    }
    fn musicbrainz_tag_albums<'a>(
        &'a self,
        _t: &'a str,
        _l: u32,
    ) -> BoxFuture<'a, SourceResult<Vec<AlbumRow>>> {
        ok(Vec::new())
    }
    fn musicbrainz_release_group_of<'a>(
        &'a self,
        _r: &'a str,
    ) -> BoxFuture<'a, SourceResult<Option<String>>> {
        ok(None)
    }
    fn musicbrainz_release_group<'a>(
        &'a self,
        mbid: &'a str,
    ) -> BoxFuture<'a, SourceResult<Option<ReleaseGroupFacts>>> {
        ok(Some(ReleaseGroupFacts {
            title: format!("Album {mbid}"),
            artist_mbid: Some(SIMILAR.to_owned()),
            artist_name: Some("Similar".to_owned()),
            tags: vec!["shoegaze".to_owned()],
            youtube_url: Some("https://youtu.be/dQw4w9WgXcQ".to_owned()),
            first_release_id: None,
            first_release_date: Some("1991-11-04".to_owned()),
        }))
    }
    fn musicbrainz_artist<'a>(
        &'a self,
        _m: &'a str,
    ) -> BoxFuture<'a, SourceResult<Option<ArtistFacts>>> {
        ok(None)
    }
    fn musicbrainz_release_video<'a>(
        &'a self,
        _r: &'a str,
    ) -> BoxFuture<'a, SourceResult<Option<String>>> {
        ok(None)
    }
    fn musicbrainz_release_recordings<'a>(
        &'a self,
        _r: &'a str,
        _l: usize,
    ) -> BoxFuture<'a, SourceResult<Vec<String>>> {
        ok(Vec::new())
    }
    fn musicbrainz_recording_video<'a>(
        &'a self,
        _r: &'a str,
    ) -> BoxFuture<'a, SourceResult<Option<String>>> {
        ok(None)
    }
    fn wikipedia_extract<'a>(&'a self, _u: &'a str) -> BoxFuture<'a, SourceResult<Option<String>>> {
        ok(None)
    }
}

fn queue_app(runtime: &DbRuntime) -> Router {
    let settings = QueueSettings {
        queue_size: 5,
        wildcard_slots: 1,
        ..QueueSettings::default()
    };
    let queue = LiveQueue::new(
        Arc::new(Scripted),
        QueueDb::new(runtime.pool().clone(), runtime.lane().clone()),
        Arc::new(FakeYouTube::unconfigured()),
        Providers::with_memory_cache().cache.clone(),
        Arc::new(move || settings.clone()),
    );
    let clock = ManualClock::new(1_790_000_000);
    let deps = ReadsDeps {
        content: Arc::new(FakeContent::new(clock.clone())),
        queues: Arc::new(queue),
        batches: Arc::new(FakeBatches::new(clock)),
        charts: Arc::new(FakeCharts::new()),
        previews: Arc::new(FakePreviews),
        youtube: Arc::new(FakeYouTube::unconfigured()),
        radio: Arc::new(FakeRadio),
        now_playing: Arc::new(FakeNowPlaying),
        ids: Arc::new(UuidGenerator),
        clock: Arc::new(SystemClock),
    };
    let router = Router::new().nest(
        "/api/v3",
        droppedneedle::reads::discover::reads_router(deps),
    );
    with_test_principal(
        router,
        CurrentSession {
            user_id: "user-1".to_owned(),
            session_id: "sess-test".to_owned(),
            kind: SessionKind::Standard,
            transport: Transport::Bearer,
        },
    )
}

async fn call(app: &Router, method: &str, uri: &str, body: Option<Value>) -> (StatusCode, Value) {
    let mut builder = Request::builder().method(method).uri(uri);
    let body = match body {
        Some(json) => {
            builder = builder.header("content-type", "application/json");
            Body::from(json.to_string())
        }
        None => Body::empty(),
    };
    let response = app
        .clone()
        .oneshot(builder.body(body).expect("request builds"))
        .await
        .expect("router answers");
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .expect("body reads");
    let json = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, json)
}

/// Poll the status route until the build is no longer running.
async fn settled(app: &Router) -> Value {
    for _ in 0..250 {
        let (status, body) = call(app, "GET", "/api/v3/discover/queue/status", None).await;
        assert_eq!(status, StatusCode::OK);
        if body["status"] != "building" {
            return body;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("queue build never finished");
}

fn deck_ids(body: &Value) -> Vec<String> {
    body["items"]
        .as_array()
        .expect("items array")
        .iter()
        .map(|item| item["release_group_mbid"].as_str().unwrap_or("").to_owned())
        .collect()
}

#[tokio::test]
async fn queue_builds_honours_ignores_and_survives_restart() {
    let dir = crate::common::ScratchDir::new("discover-queue");
    let runtime = open_runtime(&DbConfig::new(&dir.join("app.db")))
        .await
        .expect("runtime opens");
    let app = queue_app(&runtime);

    let (_, idle) = call(&app, "GET", "/api/v3/discover/queue/status", None).await;
    assert_eq!(idle["status"], "idle");
    let (status, started) = call(
        &app,
        "POST",
        "/api/v3/discover/queue/generate",
        Some(json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(started["action"], "started");
    let ready = settled(&app).await;
    assert_eq!(ready["status"], "ready");
    assert_eq!(ready["item_count"], 5);
    assert_eq!(ready["stale"], false);

    let (_, again) = call(
        &app,
        "POST",
        "/api/v3/discover/queue/generate",
        Some(json!({})),
    )
    .await;
    assert_eq!(again["action"], "already_ready");

    let (_, deck) = call(&app, "GET", "/api/v3/discover/queue", None).await;
    let ids = deck_ids(&deck);
    assert_eq!(ids.len(), 5);
    let similar_cards = deck["items"]
        .as_array()
        .expect("items")
        .iter()
        .filter(|item| item["recommendation_reason"] == "Similar to Seed")
        .count();
    assert_eq!(
        similar_cards, 2,
        "one similar artist holds at most two cards"
    );
    assert!(
        deck["items"]
            .as_array()
            .expect("items")
            .iter()
            .any(|item| item["is_wildcard"] == true),
        "trending wildcards fill the rest"
    );

    let ignored = ids[0].clone();
    let (status, _) = call(
        &app,
        "POST",
        "/api/v3/discover/queue/ignore",
        Some(json!({
            "release_group_mbid": ignored,
            "artist_mbid": SIMILAR,
            "release_name": "Album",
            "artist_name": "Similar",
        })),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let rebuilt = settled(&app).await;
    assert_eq!(rebuilt["status"], "ready");
    assert_ne!(
        rebuilt["queue_id"], ready["queue_id"],
        "the ignore rebuilt the deck"
    );
    let (_, deck) = call(&app, "GET", "/api/v3/discover/queue", None).await;
    assert!(
        !deck_ids(&deck).contains(&ignored),
        "an ignored album never returns"
    );

    let (status, bad) = call(
        &app,
        "GET",
        "/api/v3/discover/queue/enrich/not-an-mbid",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{bad}");
    let (status, details) = call(
        &app,
        "GET",
        &format!("/api/v3/discover/queue/enrich/{}", group(1)),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(details["tags"], json!(["shoegaze"]));
    assert_eq!(
        details["youtube_url"],
        "https://www.youtube.com/embed/dQw4w9WgXcQ"
    );

    // A restart: a new queue over the same database.
    let restarted = queue_app(&runtime);
    let (_, ledger) = call(&restarted, "GET", "/api/v3/discover/queue/ignored", None).await;
    assert_eq!(ledger["items"][0]["release_group_mbid"], ignored.as_str());
    let (_, reloaded) = call(&restarted, "GET", "/api/v3/discover/queue/status", None).await;
    assert_eq!(reloaded["status"], "ready");
    assert_eq!(
        reloaded["queue_id"], rebuilt["queue_id"],
        "the saved deck reloads"
    );
}
