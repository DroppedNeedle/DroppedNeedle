//! The discover page end to end over a real database: the first visit
//! starts a background build and answers `loading`, the built page carries
//! its shelves and is saved, a restart serves the saved page at once,
//! activity is recorded for the warm cycle, and refresh is accepted.

use std::collections::HashMap;
use std::sync::Arc;

use axum::{
    Router,
    body::Body,
    http::{Request, StatusCode},
};
use droppedneedle_catalog::reads::discover::page::sources::{
    LastFmChartAlbum, PageSources, PlayedArtist, RankedAlbum, ScoredArtist, SourceContext,
    WeeklyPlaylist,
};
use droppedneedle_catalog::reads::discover::ports::BoxFuture;
use droppedneedle_catalog::reads::discover::queue::sources::{
    AlbumRow, ArtistFacts, ArtistRow, JellyfinList, LastFmAlbum, LastFmFacts, MusicSource,
    QueueSources, ReleaseGroupFacts, SourceResult, StatsRange, UserMusic,
};
use droppedneedle_kernel::db::Lane;
use droppedneedle_providers::Providers;
use serde_json::{Value, json};
use tower::ServiceExt as _;

use crate::common::auth::AuthRig;
use crate::discover_queue::{after_commits, discover_deps, discover_routes};

const SEED: &str = "11111111-1111-4111-8111-111111111111";

fn mbid(prefix: char, n: u8) -> String {
    let p = prefix.to_string().repeat(4);
    format!("{n:08x}-{p}-4{}-8{}-{}", &p[..3], &p[..3], p.repeat(3))
}

fn ok<T: Send + 'static>(value: T) -> BoxFuture<'static, SourceResult<T>> {
    Box::pin(async move { Ok(value) })
}

fn ranked(n: u8, artist: &str, listens: i64) -> RankedAlbum {
    RankedAlbum {
        album: AlbumRow {
            release_group_mbid: mbid('a', n),
            title: format!("Album {n}"),
            artist_name: format!("Artist {artist}"),
            artist_mbid: Some(artist.to_owned()),
        },
        listen_count: listens,
    }
}

/// A ListenBrainz user with one seed artist, three similar artists with
/// two albums each, a worldwide chart and one fresh release.
struct Scripted;

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
        _u: &'a str,
        _r: StatsRange,
        _c: u32,
        _priority: droppedneedle_providers::RequestPriority,
    ) -> BoxFuture<'a, SourceResult<Vec<ArtistRow>>> {
        ok(vec![ArtistRow {
            name: "Seed".to_owned(),
            mbid: Some(SEED.to_owned()),
            listen_count: 50,
        }])
    }
    fn listenbrainz_top_albums<'a>(
        &'a self,
        _u: &'a str,
        _r: StatsRange,
        _c: u32,
        _priority: droppedneedle_providers::RequestPriority,
    ) -> BoxFuture<'a, SourceResult<Vec<AlbumRow>>> {
        ok(Vec::new())
    }
    fn listenbrainz_similar_artists<'a>(
        &'a self,
        _user: &'a str,
        _artist: &'a str,
        _limit: usize,
        _priority: droppedneedle_providers::RequestPriority,
    ) -> BoxFuture<'a, SourceResult<Vec<ArtistRow>>> {
        ok(Vec::new())
    }
    fn listenbrainz_artist_albums<'a>(
        &'a self,
        _user: &'a str,
        _artist: &'a str,
        _count: usize,
        _priority: droppedneedle_providers::RequestPriority,
    ) -> BoxFuture<'a, SourceResult<Vec<AlbumRow>>> {
        ok(Vec::new())
    }
    fn listenbrainz_genres<'a>(
        &'a self,
        _u: &'a str,
        _priority: droppedneedle_providers::RequestPriority,
    ) -> BoxFuture<'a, SourceResult<Vec<String>>> {
        ok(Vec::new())
    }
    fn listenbrainz_fresh_releases<'a>(
        &'a self,
        _u: &'a str,
        _priority: droppedneedle_providers::RequestPriority,
    ) -> BoxFuture<'a, SourceResult<Vec<AlbumRow>>> {
        ok(vec![ranked(90, &mbid('f', 1), 0).album])
    }
    fn listenbrainz_loved_artists<'a>(
        &'a self,
        _u: &'a str,
        _c: u32,
        _priority: droppedneedle_providers::RequestPriority,
    ) -> BoxFuture<'a, SourceResult<Vec<String>>> {
        ok(Vec::new())
    }
    fn listenbrainz_trending(
        &self,
        _count: u32,
        _priority: droppedneedle_providers::RequestPriority,
    ) -> BoxFuture<'_, SourceResult<Vec<AlbumRow>>> {
        ok(Vec::new())
    }
    fn listenbrainz_listen_counts<'a>(
        &'a self,
        _user: &'a str,
        _ids: &'a [String],
        _priority: droppedneedle_providers::RequestPriority,
    ) -> BoxFuture<'a, SourceResult<HashMap<String, i64>>> {
        ok(HashMap::new())
    }
    fn listenbrainz_popularity_down(&self) -> bool {
        false
    }
    fn lastfm_top_artists<'a>(
        &'a self,
        _user: &'a str,
        _u: &'a str,
        _l: u32,
        _priority: droppedneedle_providers::RequestPriority,
    ) -> BoxFuture<'a, SourceResult<Vec<ArtistRow>>> {
        ok(Vec::new())
    }
    fn lastfm_similar_artists<'a>(
        &'a self,
        _user: &'a str,
        _a: &'a ArtistRow,
        _l: u32,
        _priority: droppedneedle_providers::RequestPriority,
    ) -> BoxFuture<'a, SourceResult<Vec<ArtistRow>>> {
        ok(Vec::new())
    }
    fn lastfm_artist_albums<'a>(
        &'a self,
        _user: &'a str,
        _a: &'a ArtistRow,
        _l: u32,
        _priority: droppedneedle_providers::RequestPriority,
    ) -> BoxFuture<'a, SourceResult<Vec<LastFmAlbum>>> {
        ok(Vec::new())
    }
    fn lastfm_chart_artists<'a>(
        &'a self,
        _user: &'a str,
        _l: u32,
        _priority: droppedneedle_providers::RequestPriority,
    ) -> BoxFuture<'a, SourceResult<Vec<ArtistRow>>> {
        ok(Vec::new())
    }
    fn lastfm_album_facts<'a>(
        &'a self,
        _user: &'a str,
        _a: &'a str,
        _b: &'a str,
        _priority: droppedneedle_providers::RequestPriority,
    ) -> BoxFuture<'a, SourceResult<Option<LastFmFacts>>> {
        ok(None)
    }
    fn lastfm_artist_facts<'a>(
        &'a self,
        _user: &'a str,
        _a: &'a str,
        _m: Option<&'a str>,
        _priority: droppedneedle_providers::RequestPriority,
    ) -> BoxFuture<'a, SourceResult<Option<LastFmFacts>>> {
        ok(None)
    }
    fn jellyfin_artists<'a>(
        &'a self,
        _user: &'a str,
        _list: JellyfinList,
        _l: u32,
    ) -> BoxFuture<'a, SourceResult<Vec<ArtistRow>>> {
        ok(Vec::new())
    }
    fn musicbrainz_tag_albums<'a>(
        &'a self,
        _t: &'a str,
        _l: u32,
        _p: droppedneedle_providers::RequestPriority,
    ) -> BoxFuture<'a, SourceResult<Vec<AlbumRow>>> {
        ok(Vec::new())
    }
    fn musicbrainz_release_group_of<'a>(
        &'a self,
        _r: &'a str,
        _p: droppedneedle_providers::RequestPriority,
    ) -> BoxFuture<'a, SourceResult<Option<String>>> {
        ok(None)
    }
    fn musicbrainz_release_group<'a>(
        &'a self,
        _m: &'a str,
        _p: droppedneedle_providers::RequestPriority,
    ) -> BoxFuture<'a, SourceResult<Option<ReleaseGroupFacts>>> {
        ok(None)
    }
    fn musicbrainz_artist<'a>(
        &'a self,
        _m: &'a str,
        _p: droppedneedle_providers::RequestPriority,
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

impl PageSources for Scripted {
    fn source_context(&self) -> SourceContext {
        SourceContext {
            mode: "official".to_owned(),
            id: "source-1".to_owned(),
            generation: 3,
        }
    }
    fn listenbrainz_sitewide_artists(
        &self,
        _count: u32,
    ) -> BoxFuture<'_, SourceResult<Vec<ArtistRow>>> {
        ok(vec![ArtistRow {
            name: "Everyone's Favourite".to_owned(),
            mbid: Some(mbid('c', 1)),
            listen_count: 9000,
        }])
    }
    fn listenbrainz_similar_scored<'a>(
        &'a self,
        _user: &'a str,
        _artist: &'a str,
        _limit: usize,
    ) -> BoxFuture<'a, SourceResult<Vec<ScoredArtist>>> {
        ok((1..=3)
            .map(|n| ScoredArtist {
                artist: ArtistRow {
                    name: format!("Similar {n}"),
                    mbid: Some(mbid('b', n)),
                    listen_count: i64::from(n) * 100,
                },
                score: f64::from(n) * 100.0,
            })
            .collect())
    }
    fn listenbrainz_artist_ranked<'a>(
        &'a self,
        _user: &'a str,
        artist: &'a str,
        _count: usize,
    ) -> BoxFuture<'a, SourceResult<Vec<RankedAlbum>>> {
        let n = artist.as_bytes()[7];
        ok(vec![ranked(n, artist, 500), ranked(n + 20, artist, 300)])
    }
    fn listenbrainz_trending_ranked(
        &self,
        _count: u32,
    ) -> BoxFuture<'_, SourceResult<Vec<RankedAlbum>>> {
        ok(vec![ranked(70, &mbid('d', 1), 10_000)])
    }
    fn listenbrainz_user_ranked<'a>(
        &'a self,
        _u: &'a str,
        _r: StatsRange,
        _c: u32,
    ) -> BoxFuture<'a, SourceResult<Vec<RankedAlbum>>> {
        ok(Vec::new())
    }
    fn listenbrainz_genre_counts<'a>(
        &'a self,
        _u: &'a str,
    ) -> BoxFuture<'a, SourceResult<Vec<(String, i64)>>> {
        ok(vec![("shoegaze".to_owned(), 40)])
    }
    fn listenbrainz_similar_users<'a>(
        &'a self,
        _u: &'a str,
    ) -> BoxFuture<'a, SourceResult<Vec<String>>> {
        ok(Vec::new())
    }
    fn listenbrainz_weekly_playlist<'a>(
        &'a self,
        _user: &'a str,
        _u: &'a str,
    ) -> BoxFuture<'a, SourceResult<Option<WeeklyPlaylist>>> {
        ok(None)
    }
    fn listenbrainz_recording_groups<'a>(
        &'a self,
        _user: &'a str,
        _r: &'a [String],
    ) -> BoxFuture<'a, SourceResult<HashMap<String, String>>> {
        ok(HashMap::new())
    }
    fn lastfm_similar_scored<'a>(
        &'a self,
        _user: &'a str,
        _a: &'a ArtistRow,
        _l: u32,
    ) -> BoxFuture<'a, SourceResult<Vec<ScoredArtist>>> {
        ok(Vec::new())
    }
    fn lastfm_weekly_artists<'a>(
        &'a self,
        _user: &'a str,
        _u: &'a str,
    ) -> BoxFuture<'a, SourceResult<Vec<ArtistRow>>> {
        ok(Vec::new())
    }
    fn lastfm_weekly_albums<'a>(
        &'a self,
        _user: &'a str,
        _u: &'a str,
    ) -> BoxFuture<'a, SourceResult<Vec<LastFmChartAlbum>>> {
        ok(Vec::new())
    }
    fn lastfm_recent_albums<'a>(
        &'a self,
        _user: &'a str,
        _u: &'a str,
        _l: u32,
    ) -> BoxFuture<'a, SourceResult<Vec<LastFmChartAlbum>>> {
        ok(Vec::new())
    }
    fn lastfm_artist_tags<'a>(
        &'a self,
        _user: &'a str,
        _a: &'a ArtistRow,
    ) -> BoxFuture<'a, SourceResult<Vec<String>>> {
        ok(Vec::new())
    }
    fn lastfm_tag_artists<'a>(
        &'a self,
        _user: &'a str,
        _t: &'a str,
        _l: u32,
        _priority: droppedneedle_providers::RequestPriority,
    ) -> BoxFuture<'a, SourceResult<Vec<ArtistRow>>> {
        ok(Vec::new())
    }
    fn musicbrainz_tag_artists<'a>(
        &'a self,
        _t: &'a str,
        _l: u32,
    ) -> BoxFuture<'a, SourceResult<Vec<ArtistRow>>> {
        ok(Vec::new())
    }
    fn jellyfin_artist_plays<'a>(
        &'a self,
        _user: &'a str,
        _l: u32,
    ) -> BoxFuture<'a, SourceResult<Vec<PlayedArtist>>> {
        ok(Vec::new())
    }
}

fn page_app(rig: &AuthRig, dir: &std::path::Path) -> Router {
    discover_routes(discover_deps(rig, dir, Providers::closed(), |sources| {
        sources.page = Arc::new(Scripted);
    }))
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
    let bytes = axum::body::to_bytes(response.into_body(), 4 * 1024 * 1024)
        .await
        .expect("body reads");
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

#[tokio::test]
async fn page_builds_in_the_background_is_saved_and_records_activity() {
    let dir = crate::common::ScratchDir::new("discover-page");
    let rig = AuthRig::new("discover-page").await;
    let db = rig.platform.db.clone();
    db.write(Lane::Foreground, "test.seed", |tx| {
        tx.execute(
            "INSERT INTO auth_users (id, display_name, role, created_at) \
                 VALUES ('user-1', 'Ada', 'user', '2026-01-01T00:00:00Z')",
            [],
        )?;
        Ok::<_, droppedneedle_kernel::db::DbError>(())
    })
    .await
    .expect("user row");
    let app = page_app(&rig, &dir);

    let (status, first) = call(&app, "GET", "/api/v3/discover", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(first["refreshing"], true);
    assert_eq!(first["section_status"]["picks"], "loading");

    let saved = || async {
        sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM discovery_snapshots \
             WHERE snapshot_key = 'discover_response:user-1:True:False'",
        )
        .fetch_one(db.sqlx())
        .await
        .expect("snapshot count")
    };
    after_commits(&db, "the built page was saved", || async {
        (saved().await == 1).then_some(())
    })
    .await;
    let (_, page) = call(&app, "GET", "/api/v3/discover", None).await;
    assert_eq!(
        page["because_you_listen_to"][0]["section"]["title"],
        "Because You Listen To Seed"
    );
    assert_eq!(
        page["globally_trending"]["items"][0]["name"],
        "Everyone's Favourite"
    );
    assert_eq!(
        page["fresh_releases"]["items"].as_array().map(Vec::len),
        Some(1)
    );
    let picks = page["top_picks"]["items"].as_array().expect("top picks");
    assert!(!picks.is_empty());
    assert_eq!(picks[0]["reasons"][0], "Because you listen to Seed");
    assert_eq!(page["genre_list"]["items"][0]["name"], "shoegaze");

    // A restart serves the saved page straight away, built and ready.
    let restarted = page_app(&rig, &dir);
    let (_, again) = call(&restarted, "GET", "/api/v3/discover", None).await;
    assert_eq!(again["refreshing"], false, "the build finished");
    assert_eq!(again["section_status"]["picks"], "ready");
    assert_eq!(
        again["because_you_listen_to"][0]["seed_artist"], "Seed",
        "served from the snapshot"
    );

    let (status, cursor) = call(
        &restarted,
        "POST",
        "/api/v3/discover/activity",
        Some(json!({ "feature": "discover" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(cursor["source_id"], "source-1");
    assert_eq!(cursor["generation"], 3);
    let recorded: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM discovery_activity WHERE user_id = 'user-1' AND feature = 'discover'",
    )
    .fetch_one(db.sqlx())
    .await
    .expect("activity count");
    assert_eq!(recorded, 1);
    let (status, _) = call(
        &restarted,
        "POST",
        "/api/v3/discover/activity",
        Some(json!({ "feature": "artist", "section": "similar" })),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "artist activity needs an artist"
    );

    let (status, _) = call(&restarted, "POST", "/api/v3/discover/refresh", None).await;
    assert_eq!(status, StatusCode::ACCEPTED);
}
