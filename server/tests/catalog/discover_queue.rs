//! The discover queue deck end to end over a real database: a background
//! build from scripted ListenBrainz answers, an ignore that a rebuilt deck
//! honours, and both the ignore and the deck surviving a restart.

use std::collections::HashMap;
use std::future::Future;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use axum::{
    Router,
    body::Body,
    http::{Request, StatusCode},
};
use droppedneedle_auth::session::store::SessionKind;
use droppedneedle_auth::{Admin, Principal, Role};
use droppedneedle_catalog::reads::discover::batches::{BatchAlbum, BatchRequests};
use droppedneedle_catalog::reads::discover::listening::{
    AlbumRow, ArtistFacts, ArtistRow, Ask, JellyfinList, LastFmAlbum, LastFmChartAlbum,
    LastFmFacts, ListenedAlbum, ListeningData, ListeningError, ListeningNetwork, LovedTrack,
    PlayedArtist, RankedAlbum, Read, ReleaseGroupFacts, ScoredArtist, SourceContext, StatsRange,
    TagAlbum, UserMusic, WeeklyPlaylist,
};
use droppedneedle_catalog::reads::discover::ports::{BatchFailure, BoxFuture, ProviderFailure};
use droppedneedle_catalog::reads::discover::services::ReadsDeps;
use droppedneedle_catalog::reads::discover::wiring::{DiscoverInputs, deps_over, live_network};
use droppedneedle_kernel::db::{Db, Lane};
use droppedneedle_kernel::runtime_config::secret_sections::AdvancedSettings;
use droppedneedle_media::playback::listening::MemoryListenBrainzLinkStore;
use droppedneedle_providers::{ProviderError, Providers, RequestPriority};
use serde_json::{Value, json};
use tower::ServiceExt as _;

use crate::common::auth::{AuthRig, test_principal, with_test_principal};
use crate::common::bundles::{NoDownloadSource, remotes_resolver};

const SEED: &str = "11111111-1111-4111-8111-111111111111";
const SIMILAR: &str = "22222222-2222-4222-8222-222222222222";

fn group(n: u8) -> String {
    format!("{n:08x}-aaaa-4aaa-8aaa-aaaaaaaaaaaa")
}

/// Scripted listening answers, the network behind the listening data.
/// Each read answers its field (empty by default). Release-group facts
/// fail while `musicbrainz_down` is set.
#[derive(Default)]
pub(crate) struct Script {
    pub user: UserMusic,
    /// Users who can read Last.fm, besides `user`'s own setting.
    pub lastfm_users: Vec<String>,
    /// The Last.fm chart, answered to users who can read Last.fm.
    pub lastfm_chart: Vec<ArtistRow>,
    pub source: Option<SourceContext>,
    pub user_artists: Vec<ArtistRow>,
    pub user_albums: Vec<ListenedAlbum>,
    pub sitewide_artists: Vec<ArtistRow>,
    pub sitewide_albums: Vec<ListenedAlbum>,
    pub similar: HashMap<String, Vec<ScoredArtist>>,
    pub artist_albums: HashMap<String, Vec<RankedAlbum>>,
    pub genres: Vec<(String, i64)>,
    pub fresh: Vec<AlbumRow>,
    pub loved: Vec<LovedTrack>,
    pub tag_artists: Vec<ArtistRow>,
    pub tag_albums: Vec<TagAlbum>,
    pub release_group: Option<ReleaseGroupFacts>,
    pub musicbrainz_down: AtomicBool,
}

type Answer<'a, T> = Read<'a, T>;

fn ok<T: Send + 'static>(value: T) -> Answer<'static, T> {
    Box::pin(async move { Ok(value) })
}

impl ListeningNetwork for Script {
    fn source(&self) -> SourceContext {
        self.source.clone().unwrap_or_else(|| SourceContext {
            mode: "official".to_owned(),
            id: "official".to_owned(),
            generation: 0,
        })
    }
    fn accounts<'a>(&'a self, user_id: &'a str) -> BoxFuture<'a, UserMusic> {
        let mut user = self.user.clone();
        user.lastfm |= self.lastfm_users.iter().any(|id| id == user_id);
        Box::pin(async move { user })
    }
    fn sitewide_artists(
        &self,
        _r: StatsRange,
        _c: u32,
        _o: u32,
        _p: RequestPriority,
    ) -> Answer<'_, Vec<ArtistRow>> {
        ok(self.sitewide_artists.clone())
    }
    fn sitewide_albums(
        &self,
        _r: StatsRange,
        _c: u32,
        _o: u32,
        _p: RequestPriority,
    ) -> Answer<'_, Vec<ListenedAlbum>> {
        ok(self.sitewide_albums.clone())
    }
    fn user_artists<'a>(
        &'a self,
        _u: &'a str,
        _r: StatsRange,
        _c: u32,
        _o: u32,
        _p: RequestPriority,
    ) -> Answer<'a, Vec<ArtistRow>> {
        ok(self.user_artists.clone())
    }
    fn user_albums<'a>(
        &'a self,
        _u: &'a str,
        _r: StatsRange,
        _c: u32,
        _o: u32,
        _p: RequestPriority,
    ) -> Answer<'a, Vec<ListenedAlbum>> {
        ok(self.user_albums.clone())
    }
    fn similar_artists<'a>(
        &'a self,
        _u: &'a str,
        artist: &'a str,
        _l: usize,
        _p: RequestPriority,
    ) -> Answer<'a, Vec<ScoredArtist>> {
        ok(self.similar.get(artist).cloned().unwrap_or_default())
    }
    fn artist_albums<'a>(
        &'a self,
        _u: &'a str,
        artist: &'a str,
        _c: usize,
        _p: RequestPriority,
    ) -> Answer<'a, Vec<RankedAlbum>> {
        ok(self.artist_albums.get(artist).cloned().unwrap_or_default())
    }
    fn genres<'a>(&'a self, _u: &'a str, _p: RequestPriority) -> Answer<'a, Vec<(String, i64)>> {
        ok(self.genres.clone())
    }
    fn fresh_releases<'a>(&'a self, _u: &'a str, _p: RequestPriority) -> Answer<'a, Vec<AlbumRow>> {
        ok(self.fresh.clone())
    }
    fn loved<'a>(
        &'a self,
        _u: &'a str,
        _c: u32,
        _p: RequestPriority,
    ) -> Answer<'a, Vec<LovedTrack>> {
        ok(self.loved.clone())
    }
    fn similar_users<'a>(&'a self, _u: &'a str, _p: RequestPriority) -> Answer<'a, Vec<String>> {
        ok(Vec::new())
    }
    fn weekly_playlist<'a>(
        &'a self,
        _id: &'a str,
        _u: &'a str,
        _p: RequestPriority,
    ) -> Answer<'a, Option<WeeklyPlaylist>> {
        ok(None)
    }
    fn recording_groups<'a>(
        &'a self,
        _id: &'a str,
        _r: &'a [String],
        _p: RequestPriority,
    ) -> Answer<'a, HashMap<String, String>> {
        ok(HashMap::new())
    }
    fn listen_counts<'a>(
        &'a self,
        _id: &'a str,
        _g: &'a [String],
        _p: RequestPriority,
    ) -> Answer<'a, HashMap<String, i64>> {
        ok(HashMap::new())
    }
    fn lastfm_user_artists<'a>(
        &'a self,
        _id: &'a str,
        _u: &'a str,
        _period: &'a str,
        _l: u32,
        _p: RequestPriority,
    ) -> Answer<'a, Vec<ArtistRow>> {
        ok(Vec::new())
    }
    fn lastfm_user_albums<'a>(
        &'a self,
        _id: &'a str,
        _u: &'a str,
        _period: &'a str,
        _l: u32,
        _p: RequestPriority,
    ) -> Answer<'a, Vec<LastFmChartAlbum>> {
        ok(Vec::new())
    }
    fn lastfm_similar<'a>(
        &'a self,
        _id: &'a str,
        _a: &'a ArtistRow,
        _l: u32,
        _p: RequestPriority,
    ) -> Answer<'a, Vec<ScoredArtist>> {
        ok(Vec::new())
    }
    fn lastfm_artist_albums<'a>(
        &'a self,
        _id: &'a str,
        _a: &'a ArtistRow,
        _l: u32,
        _p: RequestPriority,
    ) -> Answer<'a, Vec<LastFmAlbum>> {
        ok(Vec::new())
    }
    fn lastfm_chart_artists<'a>(
        &'a self,
        id: &'a str,
        _l: u32,
        _p: RequestPriority,
    ) -> Answer<'a, Vec<ArtistRow>> {
        // Like the live network: no Last.fm for this user, no rows.
        let reads = self.user.lastfm || self.lastfm_users.iter().any(|user| user == id);
        ok(if reads {
            self.lastfm_chart.clone()
        } else {
            Vec::new()
        })
    }
    fn lastfm_album<'a>(
        &'a self,
        _id: &'a str,
        _a: &'a str,
        _b: &'a str,
        _p: RequestPriority,
    ) -> Answer<'a, Option<LastFmFacts>> {
        ok(None)
    }
    fn lastfm_artist<'a>(
        &'a self,
        _id: &'a str,
        _a: &'a str,
        _m: Option<&'a str>,
        _p: RequestPriority,
    ) -> Answer<'a, Option<LastFmFacts>> {
        ok(None)
    }
    fn lastfm_weekly_artists<'a>(
        &'a self,
        _id: &'a str,
        _u: &'a str,
        _p: RequestPriority,
    ) -> Answer<'a, Vec<ArtistRow>> {
        ok(Vec::new())
    }
    fn lastfm_weekly_albums<'a>(
        &'a self,
        _id: &'a str,
        _u: &'a str,
        _p: RequestPriority,
    ) -> Answer<'a, Vec<LastFmChartAlbum>> {
        ok(Vec::new())
    }
    fn lastfm_recent_albums<'a>(
        &'a self,
        _id: &'a str,
        _u: &'a str,
        _l: u32,
        _p: RequestPriority,
    ) -> Answer<'a, Vec<LastFmChartAlbum>> {
        ok(Vec::new())
    }
    fn lastfm_tag_artists<'a>(
        &'a self,
        _id: &'a str,
        _t: &'a str,
        _l: u32,
        _p: RequestPriority,
    ) -> Answer<'a, Vec<ArtistRow>> {
        ok(Vec::new())
    }
    fn lastfm_loved<'a>(
        &'a self,
        _id: &'a str,
        _u: &'a str,
        _l: u32,
        _p: RequestPriority,
    ) -> Answer<'a, Vec<LovedTrack>> {
        ok(Vec::new())
    }
    fn tag_albums<'a>(
        &'a self,
        _t: &'a str,
        _l: u32,
        _o: u32,
        _p: RequestPriority,
    ) -> Answer<'a, Vec<TagAlbum>> {
        ok(self.tag_albums.clone())
    }
    fn tag_artists<'a>(
        &'a self,
        _t: &'a str,
        _l: u32,
        _o: u32,
        _p: RequestPriority,
    ) -> Answer<'a, Vec<ArtistRow>> {
        ok(self.tag_artists.clone())
    }
    fn release_group_of<'a>(
        &'a self,
        _r: &'a str,
        _p: RequestPriority,
    ) -> Answer<'a, Option<String>> {
        ok(None)
    }
    fn release_group<'a>(
        &'a self,
        _m: &'a str,
        _p: RequestPriority,
    ) -> Answer<'a, Option<ReleaseGroupFacts>> {
        if self.musicbrainz_down.load(Ordering::SeqCst) {
            return Box::pin(async {
                Err(ListeningError::Provider(ProviderError::Unavailable {
                    provider: "musicbrainz",
                    retry_after: None,
                }))
            });
        }
        ok(self.release_group.clone())
    }
    fn artist<'a>(&'a self, _m: &'a str, _p: RequestPriority) -> Answer<'a, Option<ArtistFacts>> {
        ok(None)
    }
    fn release_video<'a>(&'a self, _r: &'a str, _p: RequestPriority) -> Answer<'a, Option<String>> {
        ok(None)
    }
    fn release_recordings<'a>(
        &'a self,
        _r: &'a str,
        _l: usize,
        _p: RequestPriority,
    ) -> Answer<'a, Vec<String>> {
        ok(Vec::new())
    }
    fn recording_video<'a>(
        &'a self,
        _r: &'a str,
        _p: RequestPriority,
    ) -> Answer<'a, Option<String>> {
        ok(None)
    }
    fn wikipedia_extract<'a>(
        &'a self,
        _u: &'a str,
        _p: RequestPriority,
    ) -> Answer<'a, Option<String>> {
        ok(None)
    }
    fn jellyfin_artists<'a>(
        &'a self,
        _id: &'a str,
        _list: JellyfinList,
        _l: u32,
    ) -> Answer<'a, Vec<ArtistRow>> {
        ok(Vec::new())
    }
    fn jellyfin_plays<'a>(&'a self, _id: &'a str, _l: u32) -> Answer<'a, Vec<PlayedArtist>> {
        ok(Vec::new())
    }
}

fn album(n: u8, artist: &str) -> AlbumRow {
    AlbumRow {
        release_group_mbid: group(n),
        title: format!("Album {n}"),
        artist_name: "Someone".to_owned(),
        artist_mbid: Some(artist.to_owned()),
    }
}

/// ListenBrainz answers for one linked user: one seed artist, one similar
/// artist with five albums, and five trending albums. Everything else is
/// empty.
fn queue_script() -> Script {
    Script {
        user: UserMusic {
            listenbrainz: Some("listener".to_owned()),
            ..UserMusic::default()
        },
        user_artists: vec![ArtistRow {
            name: "Seed".to_owned(),
            mbid: Some(SEED.to_owned()),
            listen_count: 10,
        }],
        similar: HashMap::from([(
            SEED.to_owned(),
            vec![ScoredArtist {
                artist: ArtistRow {
                    name: "Similar".to_owned(),
                    mbid: Some(SIMILAR.to_owned()),
                    listen_count: 5,
                },
                score: 5.0,
            }],
        )]),
        artist_albums: HashMap::from([(
            SIMILAR.to_owned(),
            (1..=5)
                .map(|n| RankedAlbum {
                    album: album(n, SIMILAR),
                    listen_count: 0,
                })
                .collect(),
        )]),
        // Distinct artists each, so the per-artist cap never bites.
        sitewide_albums: (11..=15)
            .map(|n| {
                let row = album(n, &format!("{n:08x}-bbbb-4bbb-8bbb-bbbbbbbbbbbb"));
                ListenedAlbum {
                    release_group_mbid: Some(row.release_group_mbid),
                    title: row.title,
                    artist_name: row.artist_name,
                    artist_mbid: row.artist_mbid,
                    listen_count: 0,
                }
            })
            .collect(),
        release_group: Some(ReleaseGroupFacts {
            title: "Album".to_owned(),
            artist_mbid: Some(SIMILAR.to_owned()),
            artist_name: Some("Similar".to_owned()),
            tags: vec!["shoegaze".to_owned()],
            youtube_url: Some("https://youtu.be/dQw4w9WgXcQ".to_owned()),
            first_release_id: None,
            first_release_date: Some("1991-11-04".to_owned()),
        }),
        ..Script::default()
    }
}

/// Discovery batches in a rig with no download system: nothing in these
/// journeys files one.
struct NoBatchRequests;

impl BatchRequests for NoBatchRequests {
    fn request_albums<'a>(
        &'a self,
        _caller: &'a Principal,
        _albums: Vec<BatchAlbum>,
    ) -> BoxFuture<'a, Result<(), BatchFailure>> {
        Box::pin(async {
            Err(BatchFailure::Provider(ProviderFailure::not_built(
                "no download system in this rig",
            )))
        })
    }

    fn withdraw_album<'a>(
        &'a self,
        _owner_id: &'a str,
        _release_group_mbid: &'a str,
    ) -> BoxFuture<'a, Result<bool, String>> {
        Box::pin(async { Ok(false) })
    }

    fn remove_album<'a>(
        &'a self,
        _admin: &'a Admin,
        _release_group_mbid: &'a str,
    ) -> BoxFuture<'a, Result<bool, String>> {
        Box::pin(async { Ok(false) })
    }
}

/// What production discover is built from, over `rig`'s platform,
/// `providers` and the users' ListenBrainz `links`.
pub(crate) fn discover_inputs(
    rig: &AuthRig,
    dir: &Path,
    providers: Providers,
    links: MemoryListenBrainzLinkStore,
) -> DiscoverInputs {
    let platform = &rig.platform;
    DiscoverInputs {
        db: platform.db.clone(),
        http: platform.http.shared().clone(),
        providers: Arc::new(providers),
        listenbrainz_links: Arc::new(links),
        youtube_quota_path: dir.join("youtube_quota.json"),
        users: rig.deps.clone(),
        discover_warmer: false,
        clock: platform.clock.clone(),
        warm: None,
        batch_requests: Arc::new(NoBatchRequests),
        download_readiness: Arc::new(NoDownloadSource),
        remotes: Arc::new(remotes_resolver(platform)),
    }
}

/// Discover's services as the production builder wires them over `rig`'s
/// platform and `providers`, with the listening network `script` swaps
/// in. Calling it again over the same rig is a restart.
pub(crate) fn discover_deps(
    rig: &AuthRig,
    dir: &Path,
    providers: Providers,
    script: impl FnOnce(&mut Arc<dyn ListeningNetwork>),
) -> ReadsDeps {
    let links = MemoryListenBrainzLinkStore::new(rig.platform.crypto.clone());
    let inputs = discover_inputs(rig, dir, providers, links);
    let mut network = live_network(&inputs, &rig.config);
    script(&mut network);
    deps_over(inputs, &rig.config, network)
}

/// Discover's routes over `deps`, signed in as `user-1`.
pub(crate) fn discover_routes(deps: ReadsDeps) -> Router {
    let router = droppedneedle_catalog::reads::discover::routes()
        .with_state(deps)
        .into_router();
    with_test_principal(
        router,
        test_principal("user-1", Role::User, "sess-test", SessionKind::Standard),
    )
}

/// Ask `check` after every commit to `db` until it answers. The work it
/// waits on ends in a commit, so no answer after the last one fails.
pub(crate) async fn after_commits<T, F, Fut>(db: &Db, what: &str, mut check: F) -> T
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Option<T>>,
{
    let mut commits = db.commits();
    loop {
        commits.borrow_and_update();
        if let Some(found) = check().await {
            return found;
        }
        tokio::time::timeout(Duration::from_secs(30), commits.changed())
            .await
            .unwrap_or_else(|_| panic!("never happened: {what}"))
            .expect("the database is open");
    }
}

fn queue_app(rig: &AuthRig, dir: &Path) -> Router {
    rig.config
        .save_secret_blocking(AdvancedSettings {
            discover_queue_size: 5,
            discover_queue_wildcard_slots: 1,
            ..AdvancedSettings::default()
        })
        .expect("queue settings save");
    discover_routes(discover_deps(rig, dir, Providers::closed(), |network| {
        *network = Arc::new(queue_script());
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
    let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .expect("body reads");
    let json = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, json)
}

/// The build's status once it is no longer running. A finished build is
/// marked ready before its deck is saved, so the save's commit wakes the
/// last check.
async fn settled(app: &Router, db: &Db) -> Value {
    after_commits(db, "the queue build finished", || async {
        let (status, body) = call(app, "GET", "/api/v3/discover/queue/status", None).await;
        assert_eq!(status, StatusCode::OK);
        (body["status"] != "building").then_some(body)
    })
    .await
}

fn deck_ids(body: &Value) -> Vec<String> {
    body["items"]
        .as_array()
        .expect("items array")
        .iter()
        .map(|item| item["musicbrainz_id"].as_str().unwrap_or("").to_owned())
        .collect()
}

#[tokio::test]
async fn queue_builds_honours_ignores_and_survives_restart() {
    let dir = crate::common::ScratchDir::new("discover-queue");
    let rig = AuthRig::new("discover-queue").await;
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
    let app = queue_app(&rig, &dir);

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
    let ready = settled(&app, &db).await;
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
    let rebuilt = settled(&app, &db).await;
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
    let restarted = queue_app(&rig, &dir);
    let (_, ledger) = call(&restarted, "GET", "/api/v3/discover/queue/ignored", None).await;
    assert_eq!(ledger["items"][0]["release_group_mbid"], ignored.as_str());
    let (_, reloaded) = call(&restarted, "GET", "/api/v3/discover/queue/status", None).await;
    assert_eq!(reloaded["status"], "ready");
    assert_eq!(
        reloaded["queue_id"], rebuilt["queue_id"],
        "the saved deck reloads"
    );
}

/// A card whose MusicBrainz read failed is not cached for the day: once
/// MusicBrainz answers, the next read fills the card in.
#[tokio::test]
async fn a_card_built_during_an_outage_is_not_kept() {
    let dir = crate::common::ScratchDir::new("discover-queue-outage");
    let rig = AuthRig::new("discover-queue-outage").await;
    let script = Arc::new(queue_script());
    let queue = discover_deps(&rig, &dir, Providers::closed(), |network| {
        *network = script.clone();
    })
    .queues;

    script.musicbrainz_down.store(true, Ordering::SeqCst);
    let card = queue.enrich("user-1", &group(1)).await.unwrap();
    assert!(card.tags.is_empty(), "MusicBrainz is down");

    script.musicbrainz_down.store(false, Ordering::SeqCst);
    let card = queue.enrich("user-1", &group(1)).await.unwrap();
    assert_eq!(card.tags, ["shoegaze"], "the card fills in once it answers");
}

/// Last.fm answers are kept under keys every user shares, so a user who
/// cannot read Last.fm never leaves an empty answer behind for the others.
#[tokio::test]
async fn a_user_without_lastfm_leaves_no_empty_answer_for_others() {
    let script = Script {
        lastfm_users: vec!["with-lastfm".to_owned()],
        lastfm_chart: vec![ArtistRow {
            name: "Charted".to_owned(),
            mbid: Some(SIMILAR.to_owned()),
            listen_count: 7,
        }],
        ..Script::default()
    };
    let listening = ListeningData::new(Arc::new(script), Providers::closed().cache);
    let ask = Ask::person();

    let without = listening
        .lastfm_chart_artists("without-lastfm", 10, &ask)
        .await
        .unwrap();
    assert!(without.is_empty());
    let with = listening
        .lastfm_chart_artists("with-lastfm", 10, &ask)
        .await
        .unwrap();
    assert_eq!(with.len(), 1, "the chart is not an empty kept answer");
}
