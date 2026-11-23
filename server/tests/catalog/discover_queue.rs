//! The discover queue deck end to end over a real database: a background
//! build from scripted ListenBrainz answers, an ignore that a rebuilt deck
//! honours, and both the ignore and the deck surviving a restart.

use std::collections::HashMap;
use std::future::Future;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::{
    Router,
    body::Body,
    http::{Request, StatusCode},
};
use droppedneedle_auth::Role;
use droppedneedle_auth::session::store::SessionKind;
use droppedneedle_catalog::discover::listening::{
    AlbumRow, ArtistFacts, ArtistRow, Ask, JellyfinList, LastFmAlbum, LastFmChartAlbum,
    LastFmFacts, ListenedAlbum, ListeningData, ListeningError, ListeningNetwork, LovedTrack,
    PlayedArtist, RankedAlbum, Read, ReleaseGroupFacts, ScoredArtist, SourceContext, StatsRange,
    TagAlbum, UserMusic, WeeklyPlaylist,
};
use droppedneedle_catalog::discover::ports::BoxFuture;
use droppedneedle_catalog::discover::services::DiscoverDeps;
use droppedneedle_catalog::discover::wiring::{DiscoverInputs, deps_over, live_network};
use droppedneedle_catalog::metadata::artist::ReleaseGroupItem;
use droppedneedle_kernel::clock::Clock as _;
use droppedneedle_kernel::db::{Db, Lane};
use droppedneedle_kernel::runtime_config::secret_sections::AdvancedSettings;
use droppedneedle_providers::listenbrainz::{
    ArtistPopularity, ReleaseGroupCounts, ScoredSimilarArtist,
};
use droppedneedle_providers::{ProviderError, Providers, RequestPriority};
use serde_json::{Value, json};
use tower::ServiceExt as _;

use crate::common::Probe;
use crate::common::auth::{AuthRig, test_principal, with_test_principal};
use crate::common::bundles::{NoDownloadSource, remotes_resolver};

const SEED: &str = "11111111-1111-4111-8111-111111111111";
const SIMILAR: &str = "22222222-2222-4222-8222-222222222222";

fn group(n: u8) -> String {
    format!("{n:08x}-aaaa-4aaa-8aaa-aaaaaaaaaaaa")
}

/// Scripted listening answers, the network behind the listening data.
/// Each read answers its field (empty by default). Release-group facts
/// fail while `musicbrainz_down` is set, fresh releases while
/// `fresh_down` is, similar artists while `similar_down` is and an
/// artist's discography while `albums_down` is. ListenBrainz's similar list
/// answers `similar`, each row's score as its co-listening score, and
/// `listeners` its counts; both reads are counted, and the artists whose
/// list was asked for are recorded in order. An artist's
/// discography is its rows in `artist_albums`, each counted with its listen
/// count as both its listens and, unless its facts name some, its
/// listeners; those reads are recorded too.
#[derive(Default)]
pub(crate) struct Script {
    pub user: UserMusic,
    /// Users who can read Last.fm, besides `user`'s own setting.
    pub lastfm_users: Vec<String>,
    /// The Last.fm chart, answered to users who can read Last.fm.
    pub lastfm_chart: Vec<ArtistRow>,
    /// The user's Last.fm top artists.
    pub lastfm_user_artists: Vec<ArtistRow>,
    /// Last.fm's similar artists, by the asked artist's MBID.
    pub lastfm_similar: HashMap<String, Vec<ScoredArtist>>,
    /// Last.fm's top albums of an artist, by the artist's MBID.
    pub lastfm_albums: HashMap<String, Vec<LastFmAlbum>>,
    pub source: Option<SourceContext>,
    /// The user's ListenBrainz top artists of every range but all time.
    pub user_artists: Vec<ArtistRow>,
    /// The user's ListenBrainz top artists of all time.
    pub user_artists_all_time: Vec<ArtistRow>,
    /// All-time top-artist reads made so far.
    pub all_time_reads: AtomicUsize,
    pub user_albums: Vec<ListenedAlbum>,
    pub sitewide_artists: Vec<ArtistRow>,
    pub sitewide_albums: Vec<ListenedAlbum>,
    pub similar: HashMap<String, Vec<ScoredArtist>>,
    /// ListenBrainz's listener counts, by artist MBID.
    pub listeners: HashMap<String, ArtistPopularity>,
    /// Similar-list reads made so far.
    pub similar_reads: AtomicUsize,
    /// The artists whose similar list was read, in the order asked.
    pub similar_asked: Mutex<Vec<String>>,
    /// The user's top-album reads made so far.
    pub user_album_reads: AtomicUsize,
    /// Listener-count reads made so far.
    pub listener_reads: AtomicUsize,
    pub artist_albums: HashMap<String, Vec<RankedAlbum>>,
    /// The artists whose discography was read, one entry a read.
    pub discography_reads: Mutex<Vec<String>>,
    /// The release groups of each album count read.
    pub count_reads: Mutex<Vec<Vec<String>>>,
    /// The user's Jellyfin artists.
    pub jellyfin_artists: Vec<ArtistRow>,
    pub genres: Vec<(String, i64)>,
    pub fresh: Vec<AlbumRow>,
    pub loved: Vec<LovedTrack>,
    pub tag_artists: Vec<ArtistRow>,
    pub tag_albums: Vec<TagAlbum>,
    pub release_group: Option<ReleaseGroupFacts>,
    /// The user's ListenBrainz weekly exploration playlist.
    pub weekly: Option<WeeklyPlaylist>,
    /// The user's Last.fm top albums.
    pub lastfm_top_albums: Vec<LastFmChartAlbum>,
    /// When set, MusicBrainz release lookups never answer.
    pub release_lookups_hang: bool,
    pub musicbrainz_down: AtomicBool,
    pub fresh_down: AtomicBool,
    pub similar_down: AtomicBool,
    pub albums_down: AtomicBool,
    /// When set, ListenBrainz top-artist reads wait here until released.
    pub hold: Option<Arc<Hold>>,
}

/// A gate a scripted read stops at: it says it arrived, then waits until
/// the gate is opened.
pub(crate) struct Hold {
    arrived: tokio::sync::Notify,
    open: tokio::sync::Semaphore,
}

impl Hold {
    fn new() -> Self {
        Self {
            arrived: tokio::sync::Notify::new(),
            open: tokio::sync::Semaphore::new(0),
        }
    }

    async fn pass(&self) {
        self.arrived.notify_one();
        // Opening closes the semaphore, which lets every waiter through.
        let _ = self.open.acquire().await;
    }

    async fn reached(&self) {
        self.arrived.notified().await;
    }

    fn release(&self) {
        self.open.close();
    }
}

type Answer<'a, T> = Read<'a, T>;

fn ok<T: Send + 'static>(value: T) -> Answer<'static, T> {
    Box::pin(async move { Ok(value) })
}

fn down<T: Send + 'static>(cause: &'static str) -> Answer<'static, T> {
    Box::pin(async move { Err(ListeningError::MediaServer(cause.to_owned())) })
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
    fn listenbrainz_ready(&self) -> bool {
        true
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
        range: StatsRange,
        _c: u32,
        _o: u32,
        _p: RequestPriority,
    ) -> Answer<'a, Vec<ArtistRow>> {
        let rows = if range == StatsRange::AllTime {
            self.all_time_reads.fetch_add(1, Ordering::SeqCst);
            self.user_artists_all_time.clone()
        } else {
            self.user_artists.clone()
        };
        let hold = self.hold.clone();
        Box::pin(async move {
            if let Some(hold) = hold {
                hold.pass().await;
            }
            Ok(rows)
        })
    }
    fn user_albums<'a>(
        &'a self,
        _u: &'a str,
        _r: StatsRange,
        _c: u32,
        _o: u32,
        _p: RequestPriority,
    ) -> Answer<'a, Vec<ListenedAlbum>> {
        self.user_album_reads.fetch_add(1, Ordering::SeqCst);
        ok(self.user_albums.clone())
    }
    fn scored_similar<'a>(
        &'a self,
        artist: &'a str,
        _p: RequestPriority,
    ) -> Answer<'a, Vec<ScoredSimilarArtist>> {
        self.similar_reads.fetch_add(1, Ordering::SeqCst);
        self.similar_asked.lock().unwrap().push(artist.to_owned());
        if self.similar_down.load(Ordering::SeqCst) {
            return down("similar artists down");
        }
        let rows = self
            .similar
            .get(artist)
            .into_iter()
            .flatten()
            .filter_map(|row| {
                Some(ScoredSimilarArtist {
                    artist_mbid: row.artist.mbid.clone()?,
                    name: Some(row.artist.name.clone()),
                    score: row.score as i64,
                    reference_mbid: Some(artist.to_owned()),
                })
            })
            .collect();
        ok(rows)
    }
    fn artist_listeners<'a>(
        &'a self,
        _u: &'a str,
        artists: &'a [String],
        _p: RequestPriority,
    ) -> Answer<'a, HashMap<String, ArtistPopularity>> {
        self.listener_reads.fetch_add(1, Ordering::SeqCst);
        ok(artists
            .iter()
            .filter_map(|id| Some((id.clone(), *self.listeners.get(id)?)))
            .collect())
    }
    fn discography<'a>(
        &'a self,
        artist: &'a str,
        _p: RequestPriority,
    ) -> Answer<'a, Option<Vec<ReleaseGroupItem>>> {
        self.discography_reads
            .lock()
            .unwrap()
            .push(artist.to_owned());
        if self.albums_down.load(Ordering::SeqCst) {
            return down("artist albums down");
        }
        let groups = self
            .artist_albums
            .get(artist)
            .into_iter()
            .flatten()
            .map(|row| {
                let album = row.album.clone();
                ReleaseGroupItem {
                    id: album.release_group_mbid,
                    title: Some(album.title),
                    primary_type: album.facts.primary_type,
                    secondary_types: album.facts.secondary_types.unwrap_or_default(),
                    first_release_date: album.facts.first_release_date,
                    artist_name: Some(album.artist_name),
                    credited_artist_mbids: album.facts.credited_artist_mbids.unwrap_or_default(),
                }
            })
            .collect();
        ok(Some(groups))
    }
    fn release_group_counts<'a>(
        &'a self,
        groups: &'a [String],
        _p: RequestPriority,
    ) -> Answer<'a, HashMap<String, ReleaseGroupCounts>> {
        self.count_reads.lock().unwrap().push(groups.to_vec());
        ok(self
            .artist_albums
            .values()
            .flatten()
            .filter(|row| groups.contains(&row.album.release_group_mbid))
            .map(|row| {
                let counts = ReleaseGroupCounts {
                    listen_count: Some(row.listen_count),
                    listeners: row.album.facts.listeners.or(Some(row.listen_count)),
                };
                (row.album.release_group_mbid.clone(), counts)
            })
            .collect())
    }
    fn genres<'a>(&'a self, _u: &'a str, _p: RequestPriority) -> Answer<'a, Vec<(String, i64)>> {
        ok(self.genres.clone())
    }
    fn fresh_releases<'a>(&'a self, _u: &'a str, _p: RequestPriority) -> Answer<'a, Vec<AlbumRow>> {
        if self.fresh_down.load(Ordering::SeqCst) {
            return Box::pin(async {
                Err(ListeningError::MediaServer(
                    "fresh releases down".to_owned(),
                ))
            });
        }
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
        ok(self.weekly.clone())
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
        ok(self.lastfm_user_artists.clone())
    }
    fn lastfm_user_albums<'a>(
        &'a self,
        _id: &'a str,
        _u: &'a str,
        _period: &'a str,
        _l: u32,
        _p: RequestPriority,
    ) -> Answer<'a, Vec<LastFmChartAlbum>> {
        ok(self.lastfm_top_albums.clone())
    }
    fn lastfm_similar<'a>(
        &'a self,
        _id: &'a str,
        artist: &'a ArtistRow,
        _l: u32,
        _p: RequestPriority,
    ) -> Answer<'a, Vec<ScoredArtist>> {
        let rows = artist
            .mbid
            .as_ref()
            .and_then(|mbid| self.lastfm_similar.get(mbid))
            .cloned()
            .unwrap_or_default();
        ok(rows)
    }
    fn lastfm_artist_albums<'a>(
        &'a self,
        _id: &'a str,
        artist: &'a ArtistRow,
        _l: u32,
        _p: RequestPriority,
    ) -> Answer<'a, Vec<LastFmAlbum>> {
        let rows = artist
            .mbid
            .as_ref()
            .and_then(|mbid| self.lastfm_albums.get(mbid))
            .cloned()
            .unwrap_or_default();
        ok(rows)
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
        if self.release_lookups_hang {
            return Box::pin(std::future::pending());
        }
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
        ok(self.jellyfin_artists.clone())
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
        facts: Default::default(),
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
            listen_count: Some(10),
        }],
        similar: HashMap::from([(
            SEED.to_owned(),
            vec![ScoredArtist {
                artist: ArtistRow {
                    name: "Similar".to_owned(),
                    mbid: Some(SIMILAR.to_owned()),
                    listen_count: Some(5),
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

/// What production discover is built from, over `rig`'s platform and
/// `providers`, with the listening store on the rig's database.
pub(crate) fn discover_inputs(rig: &AuthRig, dir: &Path, providers: Providers) -> DiscoverInputs {
    let platform = &rig.platform;
    DiscoverInputs {
        db: platform.db.clone(),
        http: platform.http.shared().clone(),
        providers: Arc::new(providers),
        listening: crate::common::listening_store(platform),
        youtube_quota_path: dir.join("youtube_quota.json"),
        users: rig.deps.clone(),
        discover_warmer: false,
        clock: platform.clock.clone(),
        warm: crate::common::bundles::cover_warm_at(&platform.db, platform.clock.clone()),
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
) -> DiscoverDeps {
    let inputs = discover_inputs(rig, dir, providers);
    let mut network = live_network(&inputs, &rig.config);
    script(&mut network);
    deps_over(inputs, &rig.config, network)
}

/// Discover's routes over `deps`, signed in as `user-1`.
pub(crate) fn discover_routes(deps: DiscoverDeps) -> Router {
    let router = droppedneedle_catalog::discover::routes()
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

fn queue_app(rig: &AuthRig, dir: &Path, script: Script) -> Router {
    rig.config
        .save_secret_blocking(AdvancedSettings {
            discover_queue_size: 5,
            discover_queue_wildcard_slots: 1,
            ..AdvancedSettings::default()
        })
        .expect("queue settings save");
    discover_routes(discover_deps(rig, dir, Providers::closed(), |network| {
        *network = Arc::new(script);
    }))
}

async fn seed_user(db: &Db) {
    db.write(Lane::Foreground, "test.seed", |tx| {
        tx.execute(
            "INSERT INTO auth_users (id, display_name, role, created_at) \
                 VALUES ('user-1', 'Ada', 'user', 0)",
            [],
        )?;
        Ok::<_, droppedneedle_kernel::db::DbError>(())
    })
    .await
    .expect("user row");
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

async fn ignore(app: &Router, release_group_mbid: &str) {
    let (status, body) = call(
        app,
        "POST",
        "/api/v3/discover/queue/ignore",
        Some(json!({
            "release_group_mbid": release_group_mbid,
            "artist_mbid": SIMILAR,
            "release_name": "Album",
            "artist_name": "Similar",
        })),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
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
    seed_user(&db).await;
    let app = queue_app(&rig, &dir, queue_script());

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
    // Two cards from the one similar artist and the one chart slot: a
    // deck short of picks stays short.
    assert_eq!(ready["item_count"], 3);
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
    assert_eq!(ids.len(), 3);
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
    let wildcards = deck["items"]
        .as_array()
        .expect("items")
        .iter()
        .filter(|item| item["is_wildcard"] == true)
        .count();
    assert_eq!(wildcards, 1, "one chart card for the one wildcard slot");
    let row = after_commits(&db, "the deck was saved", || async {
        db.maybe::<String>(
            "SELECT CAST(payload AS TEXT) FROM catalog_cached_pages \
             WHERE user_id = 'user-1' AND page = 'queue'",
            [],
        )
        .and_then(|payload| serde_json::from_str::<Value>(&payload).ok())
    })
    .await;
    assert_eq!(
        row["health"],
        json!({
            "cards": 3,
            "chart_cards": wildcards,
            "distinct_artists": 1 + wildcards,
            "max_cards_one_artist": 2,
        }),
        "the saved deck carries its health"
    );

    let ignored = ids[0].clone();
    ignore(&app, &ignored).await;
    // The ignore's build replaces the deck: wait for its new id.
    let rebuilt = after_commits(&db, "the ignore rebuilt the deck", || async {
        let (_, body) = call(&app, "GET", "/api/v3/discover/queue/status", None).await;
        (body["status"] == "ready" && body["queue_id"] != ready["queue_id"]).then_some(body)
    })
    .await;
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
    let restarted = queue_app(&rig, &dir, queue_script());
    let (_, ledger) = call(&restarted, "GET", "/api/v3/discover/queue/ignored", None).await;
    assert_eq!(ledger["items"][0]["release_group_mbid"], ignored.as_str());
    // The deck reads as ready a moment before its row is written.
    after_commits(&db, "the saved deck reloads", || async {
        let (_, reloaded) = call(&restarted, "GET", "/api/v3/discover/queue/status", None).await;
        (reloaded["status"] == "ready" && reloaded["queue_id"] == rebuilt["queue_id"]).then_some(())
    })
    .await;
}

/// The library holds `groups`: one album with a file for each.
pub(crate) async fn own(db: &Db, groups: &[String]) {
    let mut seed = String::from(
        "INSERT INTO local_artists (id, display_name, folded_name, kind, created_at, \
         updated_at) VALUES ('a1', 'Someone', 'someone', 'group', 1, 1);",
    );
    for (n, group) in groups.iter().enumerate() {
        seed.push_str(&format!(
            "INSERT INTO local_albums (id, root_id, grouping_key, title, title_folded, \
             album_artist_id, grouping_source, created_at, updated_at) \
             VALUES ('al{n}', 'r1', 'g{n}', 'Owned', 'owned', 'a1', 'automatic', 1, 1); \
             INSERT INTO local_album_external_identities (local_album_id, provider, \
             release_group_mbid, decision_source, selected_at) \
             VALUES ('al{n}', 'musicbrainz', '{group}', 'manual', 1); \
             INSERT INTO local_tracks (id, local_album_id, root_id, file_path, relative_path, \
             path_hash, file_size_bytes, file_mtime_ns, stat_revision, title, title_folded, \
             album_title, album_title_folded, file_format, availability, ingest_source, \
             imported_at, membership_source) VALUES ('t{n}', 'al{n}', 'r1', '/music/t{n}.flac', \
             't{n}.flac', 'h{n}', 1, 1, 's{n}', 'Song', 'song', 'Owned', 'owned', 'flac', \
             'indexed', 'scan', 1, 'automatic');"
        ));
    }
    db.write(Lane::Foreground, "test.own", move |tx| {
        tx.execute_batch(&seed)?;
        Ok::<_, droppedneedle_kernel::db::DbError>(())
    })
    .await
    .expect("owned albums");
}

/// Owned albums are dropped before the deal, never after it: with a
/// similar artist's album and a chart album owned, the deck holds neither,
/// one chart card for its one wildcard slot, and stays short of the five
/// it asks for rather than filling up from the chart.
#[tokio::test]
async fn a_seeded_deck_holds_nothing_owned_and_is_never_refilled() {
    let dir = crate::common::ScratchDir::new("discover-queue-owned");
    let rig = AuthRig::new("discover-queue-owned").await;
    let db = rig.platform.db.clone();
    seed_user(&db).await;
    let owned = [group(1), group(11)];
    own(&db, &owned).await;
    let app = queue_app(&rig, &dir, queue_script());

    call(
        &app,
        "POST",
        "/api/v3/discover/queue/generate",
        Some(json!({})),
    )
    .await;
    assert_eq!(settled(&app, &db).await["status"], "ready");
    let (_, deck) = call(&app, "GET", "/api/v3/discover/queue", None).await;
    let ids = deck_ids(&deck);
    assert!(
        ids.iter().all(|id| !owned.contains(id)),
        "no owned card: {ids:?}"
    );
    let charts = deck["items"]
        .as_array()
        .expect("items")
        .iter()
        .filter(|item| item["is_wildcard"] == true)
        .count();
    assert_eq!(charts, 1, "the one wildcard slot: {ids:?}");
    assert_eq!(ids.len(), 3, "two picks and one chart card: {ids:?}");
}

/// With all but one of the similar artist's albums and the whole chart
/// owned, the deck holds that one album and nothing the library has.
#[tokio::test]
async fn a_deck_never_holds_an_album_the_library_has() {
    let dir = crate::common::ScratchDir::new("discover-queue-owned-all");
    let rig = AuthRig::new("discover-queue-owned-all").await;
    let db = rig.platform.db.clone();
    seed_user(&db).await;
    let owned: Vec<String> = (1..=4).chain(11..=15).map(group).collect();
    own(&db, &owned).await;
    let app = queue_app(&rig, &dir, queue_script());

    call(
        &app,
        "POST",
        "/api/v3/discover/queue/generate",
        Some(json!({})),
    )
    .await;
    assert_eq!(settled(&app, &db).await["status"], "ready");
    let (_, deck) = call(&app, "GET", "/api/v3/discover/queue", None).await;
    assert_eq!(deck_ids(&deck), vec![group(5)], "no owned card");
}

/// An ignore that lands while a build is running, after that build read
/// the ignores: the deck it leaves is served without the ignored albums.
#[tokio::test]
async fn an_ignore_during_a_build_never_reaches_the_deck() {
    let dir = crate::common::ScratchDir::new("discover-queue-midbuild");
    let rig = AuthRig::new("discover-queue-midbuild").await;
    let db = rig.platform.db.clone();
    seed_user(&db).await;
    let hold = Arc::new(Hold::new());
    let app = queue_app(
        &rig,
        &dir,
        Script {
            hold: Some(hold.clone()),
            ..queue_script()
        },
    );

    let (_, started) = call(
        &app,
        "POST",
        "/api/v3/discover/queue/generate",
        Some(json!({})),
    )
    .await;
    assert_eq!(started["action"], "started");
    // The build has read the ignores and now waits on its seed artists.
    hold.reached().await;

    // Every deck holds at least one trending album, so ignoring all of
    // them always catches a card this build picks.
    let trending: Vec<String> = (11..=15).map(group).collect();
    for mbid in &trending {
        ignore(&app, mbid).await;
    }
    hold.release();

    let deck = after_commits(&db, "the running build finished", || async {
        let (_, status) = call(&app, "GET", "/api/v3/discover/queue/status", None).await;
        if status["status"] != "ready" {
            return None;
        }
        let (_, deck) = call(&app, "GET", "/api/v3/discover/queue", None).await;
        Some(deck)
    })
    .await;
    let ids = deck_ids(&deck);
    assert!(!ids.is_empty(), "the similar artist's cards remain");
    assert!(
        ids.iter().all(|id| !trending.contains(id)),
        "no ignored album is served: {ids:?}"
    );
}

/// A hidden album leaves the deck and the ledger shows it. Once un-hidden
/// it is off the ledger and the next deck can hold it again. The pool is
/// smaller than the deck, so every album still allowed is always dealt.
#[tokio::test]
async fn an_unhidden_album_returns_to_the_queue() {
    let dir = crate::common::ScratchDir::new("discover-queue-unhide");
    let rig = AuthRig::new("discover-queue-unhide").await;
    let db = rig.platform.db.clone();
    seed_user(&db).await;
    let full = queue_script();
    let script = Script {
        artist_albums: HashMap::from([(
            SIMILAR.to_owned(),
            vec![RankedAlbum {
                album: album(1, SIMILAR),
                listen_count: 0,
            }],
        )]),
        sitewide_albums: full.sitewide_albums.into_iter().take(1).collect(),
        ..queue_script()
    };
    let app = queue_app(&rig, &dir, script);
    let hidden = group(11);
    let deck_once = |wanted: bool| {
        let (app, db, hidden) = (&app, &db, &hidden);
        async move {
            after_commits(db, "the deck settled", || async {
                let (_, status) = call(app, "GET", "/api/v3/discover/queue/status", None).await;
                if status["status"] != "ready" {
                    return None;
                }
                let (_, deck) = call(app, "GET", "/api/v3/discover/queue", None).await;
                (deck_ids(&deck).contains(hidden) == wanted).then_some(())
            })
            .await;
        }
    };

    call(
        &app,
        "POST",
        "/api/v3/discover/queue/generate",
        Some(json!({})),
    )
    .await;
    deck_once(true).await;

    ignore(&app, &hidden).await;
    deck_once(false).await;
    let (_, ledger) = call(&app, "GET", "/api/v3/discover/queue/ignored", None).await;
    assert_eq!(ledger["items"][0]["release_group_mbid"], hidden.as_str());
    settled(&app, &db).await;

    let (status, body) = call(
        &app,
        "DELETE",
        &format!("/api/v3/discover/queue/ignored/{hidden}"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
    let (_, ledger) = call(&app, "GET", "/api/v3/discover/queue/ignored", None).await;
    assert_eq!(ledger["items"], json!([]));

    let (_, started) = call(
        &app,
        "POST",
        "/api/v3/discover/queue/generate",
        Some(json!({"force": true})),
    )
    .await;
    assert_eq!(started["action"], "started");
    deck_once(true).await;

    let (status, body) = call(
        &app,
        "DELETE",
        "/api/v3/discover/queue/ignored/not-an-mbid",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
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

/// The status once no build is running. A build that keeps the deck
/// before it writes nothing, so this asks until it has settled.
async fn settled_status(app: &Router) -> Value {
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let (_, body) = call(app, "GET", "/api/v3/discover/queue/status", None).await;
            if body["status"] != "building" {
                return body;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the queue build settled")
}

/// A build that met failed reads and would deal fewer cards of its own
/// than the deck it replaces keeps that deck and says why. The next build
/// may start five minutes later.
#[tokio::test]
async fn a_build_whose_pools_failed_keeps_the_deck_before_it() {
    let dir = crate::common::ScratchDir::new("discover-queue-kept");
    let rig = AuthRig::new("discover-queue-kept").await;
    let db = rig.platform.db.clone();
    seed_user(&db).await;
    rig.config
        .save_secret_blocking(AdvancedSettings {
            discover_queue_size: 5,
            discover_queue_wildcard_slots: 1,
            ..AdvancedSettings::default()
        })
        .expect("queue settings save");
    let script = Arc::new(queue_script());
    let network = Arc::clone(&script);
    let providers = Providers::closed();
    let answers = providers.cache.clone();
    let app = discover_routes(discover_deps(&rig, &dir, providers, |slot| {
        *slot = network;
    }));
    let generate = |force: bool| {
        let app = app.clone();
        async move {
            let (_, body) = call(
                &app,
                "POST",
                "/api/v3/discover/queue/generate",
                Some(json!({ "force": force })),
            )
            .await;
            body
        }
    };

    assert_eq!(generate(false).await["action"], "started");
    let first = settled(&app, &db).await;
    assert_eq!(first["item_count"], 3);
    assert_eq!(first["reason"], Value::Null, "three of five is not short");

    script.similar_down.store(true, Ordering::SeqCst);
    answers.clear().await.expect("provider answers cleared");
    assert_eq!(generate(true).await["action"], "started");
    let kept = settled_status(&app).await;
    assert_eq!(kept["status"], "ready");
    assert_eq!(
        kept["queue_id"], first["queue_id"],
        "the deck before it stays"
    );
    assert_eq!(kept["item_count"], 3);
    assert_eq!(kept["reason"], "source_down");
    let again = generate(false).await;
    assert_eq!(again["action"], "already_ready");
    assert_eq!(again["reason"], "source_down", "the answer says why too");

    rig.clock().advance(301_000);
    assert_eq!(
        generate(false).await["action"],
        "started",
        "tried again five minutes on"
    );
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
            listen_count: Some(7),
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

/// Run `sql` as one batch.
pub(crate) async fn insert(db: &Db, sql: String) {
    db.write(Lane::Foreground, "test.rows", move |tx| {
        tx.execute_batch(&sql)?;
        Ok::<_, droppedneedle_kernel::db::DbError>(())
    })
    .await
    .expect("rows inserted");
}

/// A library artist `id` named `name` with MusicBrainz id `mbid`.
pub(crate) fn library_artist(id: &str, name: &str, mbid: &str, kind: &str) -> String {
    format!(
        "INSERT INTO local_artists (id, display_name, folded_name, kind, created_at, \
         updated_at) VALUES ('{id}', '{name}', lower('{name}'), '{kind}', 1, 1); \
         INSERT INTO local_artist_external_identities (local_artist_id, provider, \
         provider_artist_id, decision_source, selected_at) \
         VALUES ('{id}', 'musicbrainz', '{mbid}', 'manual', 1);"
    )
}

/// `count` plays by `user` of the library artist `artist`, the latest at
/// `at` (unix ms), a minute apart.
pub(crate) fn plays(user: &str, artist: &str, count: usize, at: i64) -> String {
    (0..count)
        .map(|n| {
            format!(
                "INSERT INTO library_play_history (id, user_id, local_artist_id, track_name, \
                 artist_name, played_at) VALUES ('{user}-{artist}-{n}', '{user}', '{artist}', \
                 'Song', 'Someone', {});",
                at - 60_000 * n as i64
            )
        })
        .collect()
}

fn mbid_of(n: u8) -> String {
    format!("{n:08x}-cccc-4ccc-8ccc-cccccccccccc")
}

/// The queue over `script`, asking for `seeds` seed artists.
fn seeded_queue(rig: &AuthRig, dir: &Path, script: Arc<Script>, seeds: i64) -> Router {
    rig.config
        .save_secret_blocking(AdvancedSettings {
            discover_queue_size: 5,
            discover_queue_wildcard_slots: 1,
            discover_queue_seed_artists: seeds,
            ..AdvancedSettings::default()
        })
        .expect("queue settings save");
    discover_routes(discover_deps(rig, dir, Providers::closed(), |network| {
        *network = script;
    }))
}

/// Build a deck now and wait until it is ready.
async fn build_deck(app: &Router, db: &Db) {
    call(
        app,
        "POST",
        "/api/v3/discover/queue/generate",
        Some(json!({ "force": true })),
    )
    .await;
    assert_eq!(settled(app, db).await["status"], "ready");
}

/// A person with nothing linked is seeded from what they did in the app,
/// heaviest first: a heart and two plays outweigh a follow, which
/// outweighs three plays, which outweigh eight songs of a library artist
/// on their own playlist (filed under the artist's MusicBrainz id, not its
/// library id). One play is not enough, a one-star rating takes an artist
/// back below the line, Various Artists never counts however often it is
/// played or listed, and the Weekly Mix the app made for them is not their
/// taste.
#[tokio::test]
async fn local_seeds_come_heaviest_first() {
    let dir = crate::common::ScratchDir::new("discover-queue-local-seeds");
    let rig = AuthRig::new("discover-queue-local-seeds").await;
    let db = rig.platform.db.clone();
    seed_user(&db).await;
    let (hearted, followed, played, once, disliked, mixed) = (
        mbid_of(1),
        mbid_of(2),
        mbid_of(3),
        mbid_of(4),
        mbid_of(5),
        mbid_of(6),
    );
    let listed = mbid_of(7);
    let listed_id = "aaaaaaaa-0000-4000-8000-00000000000a";
    let at = rig.platform.clock.now_ms();
    let mut rows = [
        library_artist(listed_id, "Listed", &listed, "group"),
        library_artist("h", "Hearted", &hearted, "group"),
        library_artist("p", "Played", &played, "group"),
        library_artist("o", "Once", &once, "group"),
        library_artist("d", "Disliked", &disliked, "group"),
        library_artist(
            "va",
            "Various Artists",
            "89ad4ac3-39f7-470e-963a-56509c546377",
            "various_artists",
        ),
        plays("user-1", "h", 2, at),
        plays("user-1", "p", 3, at),
        plays("user-1", "o", 1, at),
        plays("user-1", "d", 2, at),
        plays("user-1", "va", 10, at),
    ]
    .concat();
    rows.push_str(&format!(
        "INSERT INTO catalog_user_favorites (user_id, item_kind, item_id, created_at) \
         VALUES ('user-1', 'artist', 'h', 1); \
         INSERT INTO catalog_user_ratings (user_id, item_kind, item_id, rating, rated_at) \
         VALUES ('user-1', 'artist', 'd', 1, 1); \
         INSERT INTO acquire_followed_artists (user_id, artist_mbid, artist_mbid_lower, \
         artist_name, followed_at, updated_at) \
         VALUES ('user-1', '{followed}', '{followed}', 'Followed', 1, 1); \
         INSERT INTO catalog_playlists (id, name, created_at, updated_at, source_ref, user_id) \
         VALUES ('mix', 'Your Weekly Mix', 1, 1, 'personal-mix:user-1', 'user-1'), \
         ('mine', 'Mine', 1, 1, NULL, 'user-1');"
    ));
    let entry = |id: &str, list: &str, at: usize, artist: &str| {
        format!(
            "INSERT INTO catalog_playlist_tracks (id, playlist_id, position, track_name, \
             artist_name, album_name, artist_id, source_type, created_at) \
             VALUES ('{id}', '{list}', {at}, 'Song', 'Someone', 'Album', '{artist}', 'local', 1);"
        )
    };
    for n in 0..10 {
        rows.push_str(&entry(&format!("mix-{n}"), "mix", n, &mixed));
        // The library's own Various Artists row, which has no MusicBrainz id.
        rows.push_str(&entry(
            &format!("va-{n}"),
            "mine",
            n,
            "00000000-0000-4000-8000-000000000001",
        ));
    }
    for n in 0..8 {
        rows.push_str(&entry(&format!("mine-{n}"), "mine", 10 + n, listed_id));
    }
    insert(&db, rows).await;
    let script = Arc::new(Script {
        user: UserMusic::default(),
        ..queue_script()
    });
    let app = seeded_queue(&rig, &dir, Arc::clone(&script), 10);

    build_deck(&app, &db).await;
    assert_eq!(
        *script.similar_asked.lock().unwrap(),
        [hearted, followed, played, listed],
        "the seeds, heaviest first"
    );
}

/// One person's rows never seed another (R1): another user's plays,
/// hearts, ratings, follows, playlists and requests leave this person's
/// seeds as they were. The same rows, made this person's own, are seeds.
#[tokio::test]
async fn another_persons_rows_never_seed_this_one() {
    let dir = crate::common::ScratchDir::new("discover-queue-own-seeds");
    let rig = AuthRig::new("discover-queue-own-seeds").await;
    let db = rig.platform.db.clone();
    seed_user(&db).await;
    let (own, played, rated, followed, listed, asked) = (
        mbid_of(1),
        mbid_of(2),
        mbid_of(3),
        mbid_of(4),
        mbid_of(5),
        mbid_of(6),
    );
    let mut rows = [
        "INSERT INTO auth_users (id, display_name, role, created_at) \
         VALUES ('user-2', 'Bea', 'user', 0);"
            .to_owned(),
        library_artist("p", "Played", &played, "group"),
        library_artist("r", "Rated", &rated, "group"),
        plays("user-2", "p", 5, 1_790_000_000_000),
    ]
    .concat();
    rows.push_str(&format!(
        "INSERT INTO acquire_followed_artists (user_id, artist_mbid, artist_mbid_lower, \
         artist_name, followed_at, updated_at) \
         VALUES ('user-1', '{own}', '{own}', 'Own', 1, 1), \
         ('user-2', '{followed}', '{followed}', 'Followed', 1, 1); \
         INSERT INTO catalog_user_favorites (user_id, item_kind, item_id, created_at) \
         VALUES ('user-2', 'artist', 'p', 1); \
         INSERT INTO catalog_user_ratings (user_id, item_kind, item_id, rating, rated_at) \
         VALUES ('user-2', 'artist', 'r', 5, 1); \
         INSERT INTO catalog_playlists (id, name, created_at, updated_at, user_id) \
         VALUES ('list', 'Mine', 1, 1, 'user-2'); \
         INSERT INTO request_history (musicbrainz_id_lower, musicbrainz_id, artist_name, \
         album_title, artist_mbid, requested_at, status, user_id) \
         VALUES ('{album}', '{album}', 'Asked', 'Album', '{asked}', 1, 'imported', 'user-2'); \
         INSERT INTO request_history_requesters (user_id, musicbrainz_id_lower, requested_at) \
         VALUES ('user-2', '{album}', 1);",
        album = group(9),
    ));
    for n in 0..8 {
        rows.push_str(&format!(
            "INSERT INTO catalog_playlist_tracks (id, playlist_id, position, track_name, \
             artist_name, album_name, artist_id, source_type, created_at) \
             VALUES ('list-{n}', 'list', {n}, 'Song', 'Listed', 'Album', '{listed}', \
             'youtube', 1);"
        ));
    }
    insert(&db, rows).await;
    let script = Arc::new(Script {
        user: UserMusic::default(),
        ..queue_script()
    });
    let app = seeded_queue(&rig, &dir, Arc::clone(&script), 10);

    build_deck(&app, &db).await;
    assert_eq!(*script.similar_asked.lock().unwrap(), [own.clone()]);

    insert(
        &db,
        "UPDATE library_play_history SET user_id = 'user-1' WHERE user_id = 'user-2'; \
         UPDATE acquire_followed_artists SET user_id = 'user-1' WHERE user_id = 'user-2'; \
         UPDATE catalog_user_favorites SET user_id = 'user-1' WHERE user_id = 'user-2'; \
         UPDATE catalog_user_ratings SET user_id = 'user-1' WHERE user_id = 'user-2'; \
         UPDATE catalog_playlists SET user_id = 'user-1' WHERE user_id = 'user-2'; \
         UPDATE request_history_requesters SET user_id = 'user-1' WHERE user_id = 'user-2';"
            .to_owned(),
    )
    .await;
    build_deck(&app, &db).await;
    let mut asked_for = script.similar_asked.lock().unwrap().clone();
    asked_for.sort();
    let mut expected = vec![own, played, rated, followed, listed, asked];
    expected.sort();
    assert_eq!(asked_for, expected, "made their own, the rows are seeds");
}

/// A person with no listening account, seeded from their own plays in the
/// app, gets album cards of the artists like their seed: the album step
/// needs no ListenBrainz link, theirs or anyone's.
#[tokio::test]
async fn a_person_with_no_listening_account_gets_album_cards() {
    let dir = crate::common::ScratchDir::new("discover-queue-unlinked");
    let rig = AuthRig::new("discover-queue-unlinked").await;
    let db = rig.platform.db.clone();
    seed_user(&db).await;
    insert(
        &db,
        [
            library_artist("s", "Seed", SEED, "group"),
            plays("user-1", "s", 2, rig.platform.clock.now_ms()),
        ]
        .concat(),
    )
    .await;
    let script = Script {
        user: UserMusic::default(),
        ..queue_script()
    };
    let app = queue_app(&rig, &dir, script);

    call(
        &app,
        "POST",
        "/api/v3/discover/queue/generate",
        Some(json!({})),
    )
    .await;
    assert_eq!(settled(&app, &db).await["status"], "ready");
    let (_, deck) = call(&app, "GET", "/api/v3/discover/queue", None).await;
    let similar = deck["items"]
        .as_array()
        .expect("items")
        .iter()
        .filter(|item| item["recommendation_reason"] == "Similar to Seed")
        .count();
    assert!(similar > 0, "{deck}");
}

/// A seeded person whose seeds turn up nothing like them, with every read
/// answered, gets the deck of a person with no seeds: a full chart deck,
/// not one chart card.
#[tokio::test]
async fn seeds_with_nothing_like_them_get_the_chart_deck() {
    let dir = crate::common::ScratchDir::new("discover-queue-empty-pools");
    let rig = AuthRig::new("discover-queue-empty-pools").await;
    let db = rig.platform.db.clone();
    seed_user(&db).await;
    insert(
        &db,
        [
            library_artist("l", "Lonely", &mbid_of(1), "group"),
            plays("user-1", "l", 3, rig.platform.clock.now_ms()),
        ]
        .concat(),
    )
    .await;
    let script = Arc::new(Script {
        user: UserMusic::default(),
        ..queue_script()
    });
    let app = seeded_queue(&rig, &dir, Arc::clone(&script), 3);

    build_deck(&app, &db).await;
    assert_eq!(*script.similar_asked.lock().unwrap(), [mbid_of(1)]);
    let (_, deck) = call(&app, "GET", "/api/v3/discover/queue", None).await;
    let items = deck["items"].as_array().expect("items");
    assert_eq!(items.len(), 5, "{deck}");
    assert!(
        items.iter().all(|item| item["is_wildcard"] == true),
        "{deck}"
    );
}

/// Plays fade with time: thirty plays two years ago weigh less than an
/// artist the person's ListenBrainz names this year, so the one seed asked
/// for is the current one, and this year's listening is enough without
/// reading all time.
#[tokio::test]
async fn years_old_plays_do_not_outweigh_current_listening() {
    let dir = crate::common::ScratchDir::new("discover-queue-old-plays");
    let rig = AuthRig::new("discover-queue-old-plays").await;
    let db = rig.platform.db.clone();
    seed_user(&db).await;
    let two_years = 730 * 86_400_000;
    insert(
        &db,
        [
            library_artist("old", "Old", &mbid_of(1), "group"),
            plays("user-1", "old", 30, rig.platform.clock.now_ms() - two_years),
        ]
        .concat(),
    )
    .await;
    let current = mbid_of(2);
    let script = Arc::new(Script {
        user_artists: vec![ArtistRow {
            name: "Current".to_owned(),
            mbid: Some(current.clone()),
            listen_count: Some(20),
        }],
        ..queue_script()
    });
    let app = seeded_queue(&rig, &dir, Arc::clone(&script), 1);

    build_deck(&app, &db).await;
    assert_eq!(*script.similar_asked.lock().unwrap(), [current]);
    assert_eq!(script.all_time_reads.load(Ordering::SeqCst), 0);
}

/// Early in a year, when this year's ListenBrainz listening names too few
/// artists, the person's top artists of all time seed them.
#[tokio::test]
async fn too_little_listening_this_year_reads_all_time() {
    let dir = crate::common::ScratchDir::new("discover-queue-all-time");
    let rig = AuthRig::new("discover-queue-all-time").await;
    let db = rig.platform.db.clone();
    seed_user(&db).await;
    let longtime = mbid_of(3);
    let script = Arc::new(Script {
        user_artists: Vec::new(),
        user_artists_all_time: vec![ArtistRow {
            name: "Longtime".to_owned(),
            mbid: Some(longtime.clone()),
            listen_count: Some(500),
        }],
        ..queue_script()
    });
    let app = seeded_queue(&rig, &dir, Arc::clone(&script), 3);

    build_deck(&app, &db).await;
    assert_eq!(*script.similar_asked.lock().unwrap(), [longtime]);
    assert_eq!(script.all_time_reads.load(Ordering::SeqCst), 1);
}
