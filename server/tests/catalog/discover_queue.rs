//! What a person tells Discover, and the seeds their shelves grow from,
//! end to end over a real database. The scripted listening answers here
//! also stand behind the page, radio, pool and week tests of other files.

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
use droppedneedle_catalog::discover::facts::KeptFacts;
use droppedneedle_catalog::discover::listening::{
    AlbumRow, ArtistFacts, ArtistRow, Ask, FreshRow, JellyfinList, LastFmAlbum, LastFmChartAlbum,
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
use droppedneedle_providers::listenbrainz::{
    ArtistPopularity, ReleaseGroupCounts, ScoredSimilarArtist,
};
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
/// fail while `musicbrainz_down` is set, the sitewide fresh releases
/// (counted) while `fresh_down` is, the weekly playlist while
/// `weekly_down` is, similar artists while `similar_down` is and an
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
    /// The user's Last.fm weekly artist chart.
    pub lastfm_weekly_artists: Vec<ArtistRow>,
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
    /// Top-artist reads of every other range made so far.
    pub user_artist_reads: AtomicUsize,
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
    /// ListenBrainz's sitewide fresh releases.
    pub fresh: Vec<FreshRow>,
    /// Sitewide fresh-release reads made so far.
    pub fresh_reads: AtomicUsize,
    pub loved: Vec<LovedTrack>,
    pub tag_artists: Vec<ArtistRow>,
    pub tag_albums: Vec<TagAlbum>,
    pub release_group: Option<ReleaseGroupFacts>,
    /// The user's ListenBrainz weekly exploration playlist.
    pub weekly: Option<WeeklyPlaylist>,
    /// Release groups of recordings, by recording MBID.
    pub recording_groups: HashMap<String, String>,
    /// The user's Last.fm top albums.
    pub lastfm_top_albums: Vec<LastFmChartAlbum>,
    /// When set, MusicBrainz release lookups never answer.
    pub release_lookups_hang: bool,
    pub musicbrainz_down: AtomicBool,
    pub fresh_down: AtomicBool,
    /// When set, the sitewide fresh releases answer an empty list.
    pub fresh_empty: AtomicBool,
    pub weekly_down: AtomicBool,
    pub similar_down: AtomicBool,
    pub albums_down: AtomicBool,
    /// When set, the user's Jellyfin most played fails.
    pub jellyfin_down: AtomicBool,
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
            self.user_artist_reads.fetch_add(1, Ordering::SeqCst);
            self.user_artists.clone()
        };
        ok(rows)
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
    fn sitewide_fresh_releases(&self, _d: u32, _p: RequestPriority) -> Answer<'_, Vec<FreshRow>> {
        self.fresh_reads.fetch_add(1, Ordering::SeqCst);
        if self.fresh_down.load(Ordering::SeqCst) {
            return Box::pin(async {
                Err(ListeningError::MediaServer(
                    "fresh releases down".to_owned(),
                ))
            });
        }
        if self.fresh_empty.load(Ordering::SeqCst) {
            return ok(Vec::new());
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
    fn weekly_playlist<'a>(
        &'a self,
        _id: &'a str,
        _u: &'a str,
        _p: RequestPriority,
    ) -> Answer<'a, Option<WeeklyPlaylist>> {
        if self.weekly_down.load(Ordering::SeqCst) {
            return down("weekly playlist down");
        }
        ok(self.weekly.clone())
    }
    fn recording_groups<'a>(
        &'a self,
        _id: &'a str,
        recordings: &'a [String],
        _p: RequestPriority,
    ) -> Answer<'a, HashMap<String, String>> {
        ok(recordings
            .iter()
            .filter_map(|recording| {
                let group = self.recording_groups.get(recording)?;
                Some((recording.clone(), group.clone()))
            })
            .collect())
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
        ok(self.lastfm_weekly_artists.clone())
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
        if self.jellyfin_down.load(Ordering::SeqCst) {
            return down("jellyfin down");
        }
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
        jobs: droppedneedle_kernel::jobs::queue::JobQueue::new(
            platform.db.clone(),
            platform.clock.clone(),
        ),
        download_readiness: Arc::new(NoDownloadSource),
        remotes: Arc::new(remotes_resolver(platform)),
        playback: crate::common::playback_store(platform),
        history: crate::common::history_store(platform),
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

/// `user` was seen now: a session of theirs was used.
pub(crate) async fn seen_now(rig: &AuthRig, user: &str) {
    let (user, now) = (user.to_owned(), rig.clock().now_ms());
    rig.platform
        .db
        .write(Lane::Foreground, "test.seen", move |tx| {
            tx.execute(
                "INSERT INTO auth_tokens (id, user_id, token_hash, issued_at, expires_at, \
                 last_seen_at, revoked) VALUES (?1, ?1, ?1, 0, ?2, ?2, 0) \
                 ON CONFLICT (id) DO UPDATE SET last_seen_at = excluded.last_seen_at",
                rusqlite::params![user, now],
            )?;
            Ok::<_, droppedneedle_kernel::db::DbError>(())
        })
        .await
        .expect("seen");
}

/// The fresh releases' reads over a network that has none, for a `Due`
/// that queues no feed read worth making.
pub(crate) fn fresh_feed(
    platform: &droppedneedle_kernel::platform::Platform,
) -> droppedneedle_catalog::discover::listening::fresh::FreshFeedReads {
    droppedneedle_catalog::discover::listening::fresh::FreshFeedReads::new(
        Arc::new(Script::default()),
        Providers::closed().cache,
        KeptFacts::new(platform.db.clone(), platform.clock.clone()),
    )
}

/// Keep, as the refresh job would have, every similar-artists list and
/// count `script` answers and the album list of every artist it names, so
/// a build reads them. They are read as a person waits, so nothing is
/// counted against the day's ceiling.
pub(crate) async fn keep_facts_of(rig: &AuthRig, script: Arc<Script>) {
    let seeds: Vec<String> = script.similar.keys().cloned().collect();
    let mut artists: Vec<String> = script
        .similar
        .values()
        .flatten()
        .chain(script.lastfm_similar.values().flatten())
        .filter_map(|row| row.artist.mbid.clone())
        .chain(script.artist_albums.keys().cloned())
        .chain(seeds.iter().cloned())
        .chain(
            script
                .user_artists
                .iter()
                .filter_map(|row| row.mbid.clone()),
        )
        .collect();
    artists.sort();
    artists.dedup();
    let listening = ListeningData::new(
        script,
        Providers::closed().cache,
        KeptFacts::new(rig.platform.db.clone(), rig.platform.clock.clone()),
    );
    let ask = Ask::person();
    listening
        .similar_artists_each("user-1", &seeds, 100, &ask)
        .await;
    for artist in &artists {
        let _ = listening.artist_albums("user-1", artist, 100, &ask).await;
    }
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

fn feedback_app(rig: &AuthRig, dir: &Path, script: Script) -> Router {
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

pub(crate) async fn call(
    app: &Router,
    method: &str,
    uri: &str,
    body: Option<Value>,
) -> (StatusCode, Value) {
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

/// Tell Discover `kind` about an album by the similar artist; answers the
/// row as stored.
async fn tell(app: &Router, kind: &str, release_group_mbid: &str) -> Value {
    let (status, body) = call(
        app,
        "POST",
        "/api/v3/discover/feedback",
        Some(json!({
            "kind": kind,
            "release_group_mbid": release_group_mbid,
            "artist_mbid": SIMILAR,
            "release_name": "Album",
            "artist_name": "Similar",
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    body
}

const SEED_TWO: &str = "33333333-3333-4333-8333-333333333333";
const OTHER: &str = "44444444-4444-4444-8444-444444444444";

/// One album of a scripted pool: its release group, its artist and the
/// seed whose labs list leads to it.
pub(crate) type Pooled<'a> = (String, String, &'a str);

/// `user`'s taste build of `seeds` (id, name) in one group, and their pool
/// of `albums` in rank order, each found through its seed's labs list, as
/// the taste and pool jobs leave them.
pub(crate) fn pool_of(user: &str, seeds: &[(&str, &str)], albums: &[Pooled<'_>]) -> String {
    let (n, found) = (seeds.len(), albums.len());
    let anchor = seeds.first().map_or("", |seed| seed.0);
    let mut rows = format!(
        "INSERT INTO catalog_taste_builds (user_id, built_at, plays_counted, \
         imported_listens_counted, artists_played, seeds, incomplete) \
         VALUES ('{user}', 1, 10, 0, {n}, {n}, 0); \
         INSERT INTO catalog_taste_groups (user_id, group_no, anchor_artist_mbid, anchor_name, \
         share, recent_share, trend) \
         VALUES ('{user}', 0, '{anchor}', 'Anchor', 1.0, 1.0, 'steady'); \
         INSERT INTO catalog_discover_pool_builds (user_id, built_at, incomplete, waiting_for, \
         seeds_used, neighbours_found, known_left_out, hubs_left_out, artists_ranked, \
         artists_without_album, new_artist_candidates, source_pick_candidates, gap_candidates, \
         shelf_candidates, fresh_candidates, thread_candidates) \
         VALUES ('{user}', 1, 0, '[]', {n}, {found}, 0, NULL, {found}, 0, {found}, 0, 0, 0, 0, 0);"
    );
    for (seed, name) in seeds {
        rows += &format!(
            "INSERT INTO catalog_taste_artists (user_id, artist_mbid_lower, artist_mbid, name, \
             plays, imported_listens, months_active, weight, is_seed, group_no) \
             VALUES ('{user}', '{seed}', '{seed}', '{name}', 5, 0, 1, 1.0, 1, 0);"
        );
    }
    for (rank, (group, by, seed)) in albums.iter().enumerate() {
        let name = seeds
            .iter()
            .find(|(id, _)| id == seed)
            .map_or("", |(_, name)| *name);
        rows += &format!(
            "INSERT INTO catalog_discover_candidates (user_id, release_group_mbid_lower, rank, \
             kind, title, artist_mbid, artist_name, credited_artist_mbids, support, group_no, \
             group_support) VALUES ('{user}', '{group}', {rank}, 'new_artist', 'Album {rank}', \
             '{by}', 'Artist {rank}', '[\"{by}\"]', 1.0, 0, 1.0); \
             INSERT INTO catalog_discover_candidate_paths (user_id, release_group_mbid_lower, \
             path_no, kind, source, artist_mbid, artist_name, artist_plays, artist_weight, \
             artist_albums_owned, link_strength, link_rank, link_of, named_by_labs, \
             named_by_lastfm) VALUES ('{user}', '{group}', 0, 'seed', 'listenbrainz_labs', \
             '{seed}', '{name}', 5, 1.0, 0, 1.0, 1, 10, 1, 0);"
        );
    }
    rows
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

/// An answer is listed, and once undone it is off the list. An undo that
/// names no album or artist is refused.
#[tokio::test]
async fn an_answer_is_listed_until_it_is_undone() {
    let dir = crate::common::ScratchDir::new("discover-answers-undo");
    let rig = AuthRig::new("discover-answers-undo").await;
    seed_user(&rig.platform.db).await;
    let app = feedback_app(&rig, &dir, queue_script());
    let hidden = group(2);

    tell(&app, "no", &hidden).await;
    let (_, ledger) = call(&app, "GET", "/api/v3/discover/feedback", None).await;
    assert_eq!(ledger["items"][0]["target_mbid"], hidden.as_str());

    let (status, body) = call(
        &app,
        "DELETE",
        &format!("/api/v3/discover/feedback/album/{hidden}"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
    let (_, ledger) = call(&app, "GET", "/api/v3/discover/feedback", None).await;
    assert_eq!(ledger["items"], json!([]));

    for bad in ["album/not-an-mbid", &format!("label/{hidden}")] {
        let (status, body) = call(
            &app,
            "DELETE",
            &format!("/api/v3/discover/feedback/{bad}"),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{bad}: {body}");
    }
}

/// "Not now" hides an album for three months and then leaves the list;
/// "not this artist" takes their albums off radio shelves and playlist
/// suggestions at once, and must name the artist.
#[tokio::test]
async fn not_now_lapses_and_not_this_artist_clears_radio() {
    let dir = crate::common::ScratchDir::new("discover-answers-typed");
    let rig = AuthRig::new("discover-answers-typed").await;
    let db = rig.platform.db.clone();
    seed_user(&db).await;
    insert(
        &db,
        pool_of(
            "user-1",
            &[(SEED, "Seed"), (SEED_TWO, "Seed Two")],
            &[
                (group(1), SIMILAR.to_owned(), SEED),
                (group(11), OTHER.to_owned(), SEED_TWO),
            ],
        ),
    )
    .await;
    let app = feedback_app(&rig, &dir, queue_script());
    db.write(Lane::Foreground, "test.playlist", |tx| {
        tx.execute_batch(&format!(
            "INSERT INTO catalog_playlists (id, name, created_at, updated_at, user_id) \
             VALUES ('pl1', 'Mine', 1, 1, 'user-1'); \
             INSERT INTO catalog_playlist_tracks (id, playlist_id, position, track_name, \
             artist_name, album_name, artist_id, source_type, created_at) \
             VALUES ('e1', 'pl1', 0, 'Song', 'Seed', 'Album', '{SEED}', 'jellyfin', 1);"
        ))?;
        Ok::<_, droppedneedle_kernel::db::DbError>(())
    })
    .await
    .expect("a playlist of the seed");
    // The similar artist's albums on a radio shelf of the seed and in the
    // playlist's suggestions.
    let by_similar = |app: Router| async move {
        let (status, radio) = call(
            &app,
            "POST",
            "/api/v3/discover/radio",
            Some(json!({"seed_type": "artist", "seed_id": SEED})),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{radio}");
        let (status, suggested) = call(
            &app,
            "POST",
            "/api/v3/discover/playlist-suggestions",
            Some(json!({"playlist_id": "pl1"})),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{suggested}");
        [&radio, &suggested["suggestions"]].map(|shelf| {
            shelf["items"]
                .as_array()
                .into_iter()
                .flatten()
                .filter(|item| item["artist_id"] == SIMILAR)
                .count()
        })
    };
    let shown = by_similar(app.clone()).await;
    assert!(shown.iter().all(|count| *count > 0), "{shown:?}");

    let now = rig.platform.clock.now_ms() / 1000;
    let told = tell(&app, "not_now", &group(11)).await;
    assert_eq!(told["target"], "album");
    let expires = told["expires_at"].as_i64().expect("not now lapses");
    let days = (expires - now) as f64 / 86_400.0;
    assert!((89.0..=92.0).contains(&days), "three months: {days} days");
    rig.clock().advance((expires - now + 60) * 1000);
    let (_, ledger) = call(&app, "GET", "/api/v3/discover/feedback", None).await;
    assert_eq!(ledger["items"], json!([]), "a lapsed answer is not listed");

    let (status, body) = call(
        &app,
        "POST",
        "/api/v3/discover/feedback",
        Some(json!({"kind": "not_artist", "artist_mbid": SIMILAR, "artist_name": "Similar"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["target"], "artist");
    assert_eq!(body["expires_at"], Value::Null);
    assert_eq!(
        by_similar(app.clone()).await,
        [0, 0],
        "nor on the radio shelf or among the suggestions"
    );

    let (status, body) = call(
        &app,
        "POST",
        "/api/v3/discover/feedback",
        Some(json!({"kind": "not_artist", "release_group_mbid": group(11)})),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "an artist answer names the artist: {body}"
    );
}

/// Last.fm answers are kept under keys every user shares, so a user who
/// cannot read Last.fm never leaves an empty answer behind for the others.
#[tokio::test]
async fn a_user_without_lastfm_leaves_no_empty_answer_for_others() {
    let rig = AuthRig::new("discover-lastfm-empty").await;
    let script = Script {
        lastfm_users: vec!["with-lastfm".to_owned()],
        lastfm_chart: vec![ArtistRow {
            name: "Charted".to_owned(),
            mbid: Some(SIMILAR.to_owned()),
            listen_count: Some(7),
        }],
        ..Script::default()
    };
    let listening = ListeningData::new(
        Arc::new(script),
        Providers::closed().cache,
        KeptFacts::new(rig.platform.db.clone(), rig.platform.clock.clone()),
    );
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

/// user-1's seed artists now, heaviest first, as a page build weighs them
/// from `script` and the database.
async fn seeds_of(rig: &AuthRig, script: Arc<Script>) -> Vec<String> {
    let listening = ListeningData::new(
        script,
        Providers::closed().cache,
        KeptFacts::new(rig.platform.db.clone(), rig.platform.clock.clone()),
    );
    droppedneedle_catalog::test_support::seed_artists(
        &listening,
        &rig.platform.db,
        "user-1",
        rig.platform.clock.now_ms(),
    )
    .await
}

/// A person with nothing linked is seeded from what they did in the app,
/// heaviest first: a heart and two plays outweigh a follow, which
/// outweighs three plays, which outweigh eight songs of a library artist
/// on their own playlist (filed under the artist's MusicBrainz id, not its
/// library id). One play is not enough, a one-star rating takes an artist
/// back below the line, Various Artists never counts however often it is
/// played or listed, and the Weekly Mix and the lists on rotation the app
/// made for them are not their taste.
#[tokio::test]
async fn local_seeds_come_heaviest_first() {
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
         ('mine', 'Mine', 1, 1, NULL, 'user-1'), ('rot', 'On repeat', 1, 1, NULL, 'user-1'); \
         INSERT INTO catalog_rotation_lists (user_id, idea, playlist_id) \
         VALUES ('user-1', 'on_repeat', 'rot');"
    ));
    let rotated = mbid_of(8);
    let entry = |id: &str, list: &str, at: usize, artist: &str| {
        format!(
            "INSERT INTO catalog_playlist_tracks (id, playlist_id, position, track_name, \
             artist_name, album_name, artist_id, source_type, created_at) \
             VALUES ('{id}', '{list}', {at}, 'Song', 'Someone', 'Album', '{artist}', 'local', 1);"
        )
    };
    for n in 0..10 {
        rows.push_str(&entry(&format!("mix-{n}"), "mix", n, &mixed));
        rows.push_str(&entry(&format!("rot-{n}"), "rot", n, &rotated));
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
    assert_eq!(
        seeds_of(&rig, script).await,
        [hearted, followed, played, listed],
        "the seeds, heaviest first"
    );
}

/// One person's rows never seed another (R1): another user's plays,
/// hearts, ratings, follows, playlists and requests leave this person's
/// seeds as they were. The same rows, made this person's own, are seeds.
#[tokio::test]
async fn another_persons_rows_never_seed_this_one() {
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
    assert_eq!(
        seeds_of(&rig, Arc::clone(&script)).await,
        std::slice::from_ref(&own)
    );

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
    let mut asked_for = seeds_of(&rig, script).await;
    asked_for.sort();
    let mut expected = vec![own, played, rated, followed, listed, asked];
    expected.sort();
    assert_eq!(asked_for, expected, "made their own, the rows are seeds");
}

/// Plays fade with time: thirty plays two years ago weigh less than an
/// artist the person's ListenBrainz names this year, so the heaviest seed
/// is the current one, and this year's listening, three artists, is enough
/// without reading all time.
#[tokio::test]
async fn years_old_plays_do_not_outweigh_current_listening() {
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
    let this_year = |name: &str, n: u8, listens: i64| ArtistRow {
        name: name.to_owned(),
        mbid: Some(mbid_of(n)),
        listen_count: Some(listens),
    };
    let script = Arc::new(Script {
        user_artists: vec![
            this_year("Current", 2, 20),
            this_year("Also", 3, 15),
            this_year("Third", 4, 10),
        ],
        ..queue_script()
    });

    let seeds = seeds_of(&rig, Arc::clone(&script)).await;
    assert_eq!(seeds.first(), Some(&current), "{seeds:?}");
    assert_eq!(script.all_time_reads.load(Ordering::SeqCst), 0);
}

/// Early in a year, when this year's ListenBrainz listening names too few
/// artists, the person's top artists of all time seed them.
#[tokio::test]
async fn too_little_listening_this_year_reads_all_time() {
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
    assert_eq!(seeds_of(&rig, Arc::clone(&script)).await, [longtime]);
    assert_eq!(script.all_time_reads.load(Ordering::SeqCst), 1);
}
