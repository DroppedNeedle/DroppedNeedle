//! The replay: Discover's engine run on a copy of an app database, the way
//! production wires it, with a network that answers nothing. After the
//! kept facts the engine reads only local rows, so a provider call is a
//! bug, and the run fails naming every request it made.
//!
//! Ignored, so the normal suite never runs it; it reads a person's own
//! data, which never enters the repository. Run it by hand:
//!
//! ```text
//! DN_REPLAY_DB=<copy.db> DN_REPLAY_USER=<username> \
//!     cargo test --test catalog discover_replay -- --ignored --nocapture
//! ```
//!
//! - `DN_REPLAY_DB`: a copy of an app database taken with the SQLite
//!   backup API, never a copy of a live file. It is read, never written:
//!   the run works on its own backup of it in the temp dir (`TMPDIR`),
//!   removed when the run ends.
//! - `DN_REPLAY_USER`: the person, by username or id.
//! - `DN_REPLAY_LASTFM`: instead of a person in the copy, one more user
//!   whose only input is this file, a Last.fm `user.getTopArtists` answer
//!   (`{"topartists": {"artist": [{"name", "mbid", "playcount"}]}}`).
//! - `DN_REPLAY_CUTS`: `monthly`, or dates (`YYYY-MM-DD`, comma
//!   separated). Without it the run builds the person's set at the copy's
//!   own time (the latest job it holds) and prints it.
//! - `DN_REPLAY_TIMELINE`: for cuts, a ListenBrainz export ZIP; its
//!   `listens/*/*.jsonl` are the person's history. Without it the listens
//!   the copy imported are.
//! - `DN_REPLAY_WINDOW_WEEKS`: the window after each cut, 8 by default.
//!
//! At each cut the copy is put back to that date as far as it can be: the
//! person's listens, plays, hearts, ratings, follows, playlist entries,
//! feedback and picks after it are removed, as are later shows and samples
//! of the picks before it and anyone's request made after it; an album
//! first played or asked for after it, or scanned in after the first scan,
//! is taken out of the library (its tracks marked missing). An album whose own release was never played is dated by its
//! artist's first play. The taste and pool jobs then run with the clock at
//! the cut, and a set of eight is selected, then once more a day later on
//! the same inputs. The kept facts stay as fetched: similar lists and
//! counts are today's, which a cut cannot undo, and a candidate first
//! released after the cut is dropped and counted. Three baselines are
//! scored beside the engine: the most listened kept albums, a uniform draw
//! from the engine's own pool, and the person's own artists' unheard
//! albums.
//!
//! A settings section holding a value sealed under the copy's key is
//! dropped (the run holds another key), so those sections are at their
//! defaults; the rest are read as the copy holds them. The person's
//! ListenBrainz weekly playlist is not replayed: it is answered "none".
//! Last.fm and Jellyfin are not replayed for a person in the copy. Provider
//! calls made past the listening network, through the providers' own
//! clients, go to a scripted wire that answers nothing, and fail the run
//! the same way.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use droppedneedle_catalog::discover::engine::candidate::{Candidate, PathKind};
use droppedneedle_catalog::discover::engine::pool_store;
use droppedneedle_catalog::discover::engine::select::{self, Exposure, Floors, Share, Shown};
use droppedneedle_catalog::discover::listening::{
    AlbumRow, ArtistFacts, ArtistRow, JellyfinList, LastFmAlbum, LastFmChartAlbum, LastFmFacts,
    ListenSource, ListenedAlbum, ListeningError, ListeningNetwork, LovedTrack, PlayedArtist, Read,
    ReleaseGroupFacts, ScoredArtist, SourceContext, StatsRange, TagAlbum, UserMusic,
    WeeklyPlaylist,
};
use droppedneedle_catalog::discover::ports::BoxFuture;
use droppedneedle_catalog::discover::services::DiscoverDeps;
use droppedneedle_catalog::metadata::artist::ReleaseGroupItem;
use droppedneedle_kernel::clock::Clock;
use droppedneedle_kernel::db::{DbConfig, DbRuntime, LaneLimits, open_runtime};
use droppedneedle_kernel::platform::Platform;
use droppedneedle_kernel::test_support::ManualClock;
use droppedneedle_providers::listenbrainz::{
    ArtistPopularity, ReleaseGroupCounts, ScoredSimilarArtist,
};
use droppedneedle_providers::test_support::ScriptedWire;
use droppedneedle_providers::{ProviderEndpoints, Providers, RequestPriority};
use serde::Deserialize;

use crate::common::auth::AuthRig;
use crate::common::replay::{
    self, ADOPTED, ADOPTED_HEAVILY, DAY_MS, Listen, Request, Scored, SetScore, Timeline, Truth,
    WEEK_MS,
};
use crate::common::{Probe, ScratchDir, TestPlatform};
use crate::discover_queue::discover_deps;

/// How many picks a set holds.
const SET: usize = 8;
/// The id of the Last.fm-only person the run adds.
const LASTFM_PERSON: &str = "replay-lastfm";
/// How long a run's loops and copy get to close.
const CLOSE_GRACE: Duration = Duration::from_secs(5);

// -- the network ------------------------------------------------------------

/// The network the replay runs on: it answers the person's accounts, an
/// empty weekly playlist and, for the Last.fm-only person, their top
/// artists from the file. Every other read is refused and recorded, and so
/// is every request the providers' clients send down `wire`.
struct Replay {
    people: HashMap<String, UserMusic>,
    lastfm_top: HashMap<String, Vec<ArtistRow>>,
    listenbrainz_on: bool,
    asked: Mutex<Vec<String>>,
    answered: Mutex<Vec<String>>,
    wire: Arc<ScriptedWire>,
}

impl Replay {
    fn refuse<T: Send + 'static>(&self, request: String) -> Read<'static, T> {
        self.asked
            .lock()
            .expect("the replay's record")
            .push(request.clone());
        Box::pin(async move {
            Err(ListeningError::MediaServer(format!(
                "the replay asks no provider: {request}"
            )))
        })
    }

    fn answer<T: Send + 'static>(&self, request: String, value: T) -> Read<'static, T> {
        self.answered
            .lock()
            .expect("the replay's record")
            .push(request);
        Box::pin(async move { Ok(value) })
    }

    /// Every request the engine made of a provider: through the listening
    /// network, then down the wire.
    fn requests(&self) -> Vec<String> {
        let mut requests = self.asked.lock().expect("the replay's record").clone();
        requests.extend(
            self.wire
                .sent()
                .into_iter()
                .map(|sent| format!("{:?} {}", sent.method, sent.url)),
        );
        requests
    }

    /// Fail, naming every request, when the engine asked a provider.
    fn check(&self, during: &str) {
        let requests = self.requests();
        assert!(
            requests.is_empty(),
            "the engine asked a provider during {during}, {} request(s):\n  {}",
            requests.len(),
            requests.join("\n  ")
        );
    }

    /// What it answered by script since the last call, each kind once.
    fn take_answered(&self) -> Vec<String> {
        let mut answered = std::mem::take(&mut *self.answered.lock().expect("the replay's record"));
        answered.sort();
        answered.dedup();
        answered
    }
}

impl ListeningNetwork for Replay {
    fn source(&self) -> SourceContext {
        SourceContext {
            mode: "official".to_owned(),
            id: "official".to_owned(),
            generation: 0,
        }
    }
    fn accounts<'a>(&'a self, user_id: &'a str) -> BoxFuture<'a, UserMusic> {
        let accounts = self.people.get(user_id).cloned().unwrap_or_default();
        Box::pin(async move { accounts })
    }
    fn listenbrainz_ready(&self) -> bool {
        self.listenbrainz_on
    }
    fn sitewide_artists(
        &self,
        range: StatsRange,
        count: u32,
        offset: u32,
        _: RequestPriority,
    ) -> Read<'_, Vec<ArtistRow>> {
        self.refuse(format!("sitewide_artists {range:?} {count} from {offset}"))
    }
    fn sitewide_albums(
        &self,
        range: StatsRange,
        count: u32,
        offset: u32,
        _: RequestPriority,
    ) -> Read<'_, Vec<ListenedAlbum>> {
        self.refuse(format!("sitewide_albums {range:?} {count} from {offset}"))
    }
    fn user_artists<'a>(
        &'a self,
        username: &'a str,
        range: StatsRange,
        count: u32,
        offset: u32,
        _: RequestPriority,
    ) -> Read<'a, Vec<ArtistRow>> {
        self.refuse(format!(
            "user_artists {username} {range:?} {count} from {offset}"
        ))
    }
    fn user_albums<'a>(
        &'a self,
        username: &'a str,
        range: StatsRange,
        count: u32,
        offset: u32,
        _: RequestPriority,
    ) -> Read<'a, Vec<ListenedAlbum>> {
        self.refuse(format!(
            "user_albums {username} {range:?} {count} from {offset}"
        ))
    }
    fn scored_similar<'a>(
        &'a self,
        artist_mbid: &'a str,
        _: RequestPriority,
    ) -> Read<'a, Vec<ScoredSimilarArtist>> {
        self.refuse(format!("scored_similar {artist_mbid}"))
    }
    fn artist_listeners<'a>(
        &'a self,
        artist_mbids: &'a [String],
        _: RequestPriority,
    ) -> Read<'a, HashMap<String, ArtistPopularity>> {
        self.refuse(format!("artist_listeners {}", artist_mbids.join(",")))
    }
    fn discography<'a>(
        &'a self,
        artist_mbid: &'a str,
        _: RequestPriority,
    ) -> Read<'a, Option<Vec<ReleaseGroupItem>>> {
        self.refuse(format!("discography {artist_mbid}"))
    }
    fn release_group_counts<'a>(
        &'a self,
        release_group_mbids: &'a [String],
        _: RequestPriority,
    ) -> Read<'a, HashMap<String, ReleaseGroupCounts>> {
        self.refuse(format!(
            "release_group_counts {}",
            release_group_mbids.join(",")
        ))
    }
    fn genres<'a>(&'a self, username: &'a str, _: RequestPriority) -> Read<'a, Vec<(String, i64)>> {
        self.refuse(format!("genres {username}"))
    }
    fn fresh_releases<'a>(
        &'a self,
        username: &'a str,
        _: RequestPriority,
    ) -> Read<'a, Vec<AlbumRow>> {
        self.refuse(format!("fresh_releases {username}"))
    }
    fn loved<'a>(
        &'a self,
        username: &'a str,
        count: u32,
        _: RequestPriority,
    ) -> Read<'a, Vec<LovedTrack>> {
        self.refuse(format!("loved {username} {count}"))
    }
    fn similar_users<'a>(&'a self, username: &'a str, _: RequestPriority) -> Read<'a, Vec<String>> {
        self.refuse(format!("similar_users {username}"))
    }
    fn weekly_playlist<'a>(
        &'a self,
        _: &'a str,
        username: &'a str,
        _: RequestPriority,
    ) -> Read<'a, Option<WeeklyPlaylist>> {
        self.answer(
            format!("weekly_playlist {username}: not replayed, answered none"),
            None,
        )
    }
    fn recording_groups<'a>(
        &'a self,
        _: &'a str,
        recording_mbids: &'a [String],
        _: RequestPriority,
    ) -> Read<'a, HashMap<String, String>> {
        self.refuse(format!("recording_groups {}", recording_mbids.join(",")))
    }
    fn listen_counts<'a>(
        &'a self,
        _: &'a str,
        release_group_mbids: &'a [String],
        _: RequestPriority,
    ) -> Read<'a, HashMap<String, i64>> {
        self.refuse(format!("listen_counts {}", release_group_mbids.join(",")))
    }
    fn lastfm_user_artists<'a>(
        &'a self,
        user_id: &'a str,
        username: &'a str,
        period: &'a str,
        limit: u32,
        _: RequestPriority,
    ) -> Read<'a, Vec<ArtistRow>> {
        match self.lastfm_top.get(user_id) {
            Some(rows) => self.answer(
                format!("lastfm_user_artists {username}: from the file"),
                rows.iter().take(limit as usize).cloned().collect(),
            ),
            None => self.refuse(format!("lastfm_user_artists {username} {period} {limit}")),
        }
    }
    fn lastfm_user_albums<'a>(
        &'a self,
        _: &'a str,
        username: &'a str,
        period: &'a str,
        limit: u32,
        _: RequestPriority,
    ) -> Read<'a, Vec<LastFmChartAlbum>> {
        self.refuse(format!("lastfm_user_albums {username} {period} {limit}"))
    }
    fn lastfm_similar<'a>(
        &'a self,
        _: &'a str,
        artist: &'a ArtistRow,
        limit: u32,
        _: RequestPriority,
    ) -> Read<'a, Vec<ScoredArtist>> {
        self.refuse(format!("lastfm_similar {} {limit}", artist.name))
    }
    fn lastfm_artist_albums<'a>(
        &'a self,
        _: &'a str,
        artist: &'a ArtistRow,
        limit: u32,
        _: RequestPriority,
    ) -> Read<'a, Vec<LastFmAlbum>> {
        self.refuse(format!("lastfm_artist_albums {} {limit}", artist.name))
    }
    fn lastfm_chart_artists<'a>(
        &'a self,
        _: &'a str,
        limit: u32,
        _: RequestPriority,
    ) -> Read<'a, Vec<ArtistRow>> {
        self.refuse(format!("lastfm_chart_artists {limit}"))
    }
    fn lastfm_album<'a>(
        &'a self,
        _: &'a str,
        artist: &'a str,
        album: &'a str,
        _: RequestPriority,
    ) -> Read<'a, Option<LastFmFacts>> {
        self.refuse(format!("lastfm_album {artist} - {album}"))
    }
    fn lastfm_artist<'a>(
        &'a self,
        _: &'a str,
        artist: &'a str,
        _: Option<&'a str>,
        _: RequestPriority,
    ) -> Read<'a, Option<LastFmFacts>> {
        self.refuse(format!("lastfm_artist {artist}"))
    }
    fn lastfm_weekly_artists<'a>(
        &'a self,
        _: &'a str,
        username: &'a str,
        _: RequestPriority,
    ) -> Read<'a, Vec<ArtistRow>> {
        self.refuse(format!("lastfm_weekly_artists {username}"))
    }
    fn lastfm_weekly_albums<'a>(
        &'a self,
        _: &'a str,
        username: &'a str,
        _: RequestPriority,
    ) -> Read<'a, Vec<LastFmChartAlbum>> {
        self.refuse(format!("lastfm_weekly_albums {username}"))
    }
    fn lastfm_recent_albums<'a>(
        &'a self,
        _: &'a str,
        username: &'a str,
        limit: u32,
        _: RequestPriority,
    ) -> Read<'a, Vec<LastFmChartAlbum>> {
        self.refuse(format!("lastfm_recent_albums {username} {limit}"))
    }
    fn lastfm_tag_artists<'a>(
        &'a self,
        _: &'a str,
        tag: &'a str,
        limit: u32,
        _: RequestPriority,
    ) -> Read<'a, Vec<ArtistRow>> {
        self.refuse(format!("lastfm_tag_artists {tag} {limit}"))
    }
    fn lastfm_loved<'a>(
        &'a self,
        _: &'a str,
        username: &'a str,
        limit: u32,
        _: RequestPriority,
    ) -> Read<'a, Vec<LovedTrack>> {
        self.refuse(format!("lastfm_loved {username} {limit}"))
    }
    fn tag_albums<'a>(
        &'a self,
        tag: &'a str,
        limit: u32,
        offset: u32,
        _: RequestPriority,
    ) -> Read<'a, Vec<TagAlbum>> {
        self.refuse(format!("tag_albums {tag} {limit} from {offset}"))
    }
    fn tag_artists<'a>(
        &'a self,
        tag: &'a str,
        limit: u32,
        offset: u32,
        _: RequestPriority,
    ) -> Read<'a, Vec<ArtistRow>> {
        self.refuse(format!("tag_artists {tag} {limit} from {offset}"))
    }
    fn release_group_of<'a>(
        &'a self,
        release_mbid: &'a str,
        _: RequestPriority,
    ) -> Read<'a, Option<String>> {
        self.refuse(format!("release_group_of {release_mbid}"))
    }
    fn release_group<'a>(
        &'a self,
        mbid: &'a str,
        _: RequestPriority,
    ) -> Read<'a, Option<ReleaseGroupFacts>> {
        self.refuse(format!("release_group {mbid}"))
    }
    fn artist<'a>(&'a self, mbid: &'a str, _: RequestPriority) -> Read<'a, Option<ArtistFacts>> {
        self.refuse(format!("artist {mbid}"))
    }
    fn release_video<'a>(
        &'a self,
        release_mbid: &'a str,
        _: RequestPriority,
    ) -> Read<'a, Option<String>> {
        self.refuse(format!("release_video {release_mbid}"))
    }
    fn release_recordings<'a>(
        &'a self,
        release_mbid: &'a str,
        limit: usize,
        _: RequestPriority,
    ) -> Read<'a, Vec<String>> {
        self.refuse(format!("release_recordings {release_mbid} {limit}"))
    }
    fn recording_video<'a>(
        &'a self,
        recording_mbid: &'a str,
        _: RequestPriority,
    ) -> Read<'a, Option<String>> {
        self.refuse(format!("recording_video {recording_mbid}"))
    }
    fn wikipedia_extract<'a>(
        &'a self,
        url: &'a str,
        _: RequestPriority,
    ) -> Read<'a, Option<String>> {
        self.refuse(format!("wikipedia_extract {url}"))
    }
    fn jellyfin_artists<'a>(
        &'a self,
        user_id: &'a str,
        list: JellyfinList,
        limit: u32,
    ) -> Read<'a, Vec<ArtistRow>> {
        self.refuse(format!("jellyfin_artists {user_id} {list:?} {limit}"))
    }
    fn jellyfin_plays<'a>(&'a self, user_id: &'a str, limit: u32) -> Read<'a, Vec<PlayedArtist>> {
        self.refuse(format!("jellyfin_plays {user_id} {limit}"))
    }
}

// -- what the run reads -----------------------------------------------------

/// The run's inputs, from the environment.
struct Env {
    db: PathBuf,
    user: Option<String>,
    lastfm: Option<PathBuf>,
    cuts: Option<String>,
    timeline: Option<PathBuf>,
    window: i64,
}

impl Env {
    fn read() -> Self {
        let path = |name: &str| std::env::var_os(name).map(PathBuf::from);
        let db = path("DN_REPLAY_DB").unwrap_or_else(|| {
            panic!("DN_REPLAY_DB names the database copy to replay; see the module docs")
        });
        let window = std::env::var("DN_REPLAY_WINDOW_WEEKS")
            .ok()
            .map(|weeks| {
                weeks
                    .parse::<i64>()
                    .expect("DN_REPLAY_WINDOW_WEEKS is a number")
            })
            .unwrap_or(8);
        Self {
            db,
            user: std::env::var("DN_REPLAY_USER").ok(),
            lastfm: path("DN_REPLAY_LASTFM"),
            cuts: std::env::var("DN_REPLAY_CUTS").ok(),
            timeline: path("DN_REPLAY_TIMELINE"),
            window: window * WEEK_MS,
        }
    }
}

/// A Last.fm `user.getTopArtists` answer.
#[derive(Deserialize)]
struct LastFmTop {
    topartists: LastFmArtists,
}

#[derive(Deserialize)]
struct LastFmArtists {
    artist: Vec<LastFmArtist>,
}

#[derive(Deserialize)]
struct LastFmArtist {
    name: String,
    #[serde(default)]
    mbid: Option<String>,
    playcount: LastFmCount,
}

/// Last.fm sends counts as text.
#[derive(Deserialize)]
#[serde(untagged)]
enum LastFmCount {
    Text(String),
    Number(i64),
}

/// The artists of the Last.fm top-artists file at `path`, as the provider
/// rows read them: a blank id is none.
fn lastfm_rows(path: &Path) -> Vec<ArtistRow> {
    let text = std::fs::read_to_string(path).expect("the Last.fm file reads");
    let top: LastFmTop = serde_json::from_str(&text).expect("a Last.fm top-artists answer");
    top.topartists
        .artist
        .into_iter()
        .map(|artist| ArtistRow {
            name: artist.name,
            mbid: artist
                .mbid
                .map(|mbid| mbid.trim().to_lowercase())
                .filter(|mbid| !mbid.is_empty()),
            listen_count: match artist.playcount {
                LastFmCount::Text(text) => text.trim().parse().ok(),
                LastFmCount::Number(count) => Some(count),
            },
        })
        .collect()
}

/// The person replayed.
struct Person {
    id: String,
    name: String,
    accounts: UserMusic,
}

impl Person {
    /// `user` (a username or id) in the copy at `db`, with the accounts the
    /// copy shows in plain rows: their ListenBrainz import's account and
    /// their primary source. Last.fm and Jellyfin are not replayed.
    fn in_copy(db: &Path, user: &str) -> Self {
        let (id, name): (String, String) = db
            .maybe_row(
                "SELECT id, COALESCE(username, display_name, id) FROM auth_users \
                 WHERE id = ?1 OR lower(username) = lower(?1)",
                [user],
            )
            .unwrap_or_else(|| panic!("no user {user} in the copy"));
        let listenbrainz: Option<String> = db
            .maybe::<Option<String>>(
                "SELECT account FROM media_history_imports WHERE user_id = ?1 \
                 AND state <> 'forgotten'",
                [&id],
            )
            .flatten();
        let primary: Option<String> = db.maybe(
            "SELECT primary_music_source FROM user_listening_prefs WHERE user_id = ?1",
            [&id],
        );
        Self {
            accounts: UserMusic {
                listenbrainz,
                primary: match primary.as_deref() {
                    Some("lastfm") => ListenSource::Lastfm,
                    _ => ListenSource::Listenbrainz,
                },
                ..UserMusic::default()
            },
            id,
            name,
        }
    }

    /// The Last.fm-only person, added to the copy at `db`.
    fn lastfm_only(db: &Path) -> Self {
        db.exec(
            "INSERT INTO auth_users (id, display_name, role, created_at) \
             VALUES (?1, 'Last.fm only', 'user', 0)",
            [LASTFM_PERSON],
        );
        Self {
            id: LASTFM_PERSON.to_owned(),
            name: "a Last.fm-only person".to_owned(),
            accounts: UserMusic {
                lastfm_username: Some(LASTFM_PERSON.to_owned()),
                lastfm: true,
                primary: ListenSource::Lastfm,
                ..UserMusic::default()
            },
        }
    }

    fn describe(&self) -> String {
        let listenbrainz = match &self.accounts.listenbrainz {
            Some(account) => format!("ListenBrainz {account}"),
            None => "no ListenBrainz import".to_owned(),
        };
        let lastfm = if self.accounts.lastfm {
            "Last.fm from the file"
        } else {
            "Last.fm not replayed"
        };
        format!("{listenbrainz}, {lastfm}, Jellyfin not replayed")
    }
}

// -- the database -----------------------------------------------------------

/// Copy `from` to `to` with the SQLite backup API.
fn backup(from: &Path, to: &Path) {
    let source =
        rusqlite::Connection::open_with_flags(from, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .unwrap_or_else(|error| panic!("{} opens: {error}", from.display()));
    source
        .backup(
            rusqlite::MAIN_DB,
            to,
            None::<fn(rusqlite::backup::Progress)>,
        )
        .unwrap_or_else(|error| panic!("{} backs up: {error}", from.display()));
}

/// Ready the run's own copy: settings sealed under the copy's key go (the
/// run cannot open them), so do the fact wants already recorded, so what a
/// build wants is its own. Answers whether ListenBrainz reads were on.
fn prepare(db: &Path) -> bool {
    let reads: Option<String> = db.maybe(
        "SELECT value_json FROM config_sections WHERE key = 'listenbrainz_settings'",
        [],
    );
    db.exec_batch(
        "DELETE FROM config_sections WHERE key = 'key_check' OR value_json LIKE '%\"v3:%'; \
         DELETE FROM catalog_fact_fetches WHERE fetched_at IS NULL; \
         UPDATE catalog_fact_fetches SET wanted_at = NULL, want_order = NULL;",
    );
    reads
        .and_then(|json| serde_json::from_str::<serde_json::Value>(&json).ok())
        .and_then(|value| value.get("reads_enabled")?.as_bool())
        .unwrap_or(true)
}

/// The copy's own time: its latest job, else its latest listen.
fn copy_time(db: &Path) -> i64 {
    let job: Option<i64> = db.one("SELECT MAX(updated_at) FROM durable_jobs", []);
    let listen: Option<i64> = db.one("SELECT MAX(listened_at) FROM media_imported_listens", []);
    job.or(listen).expect("the copy holds a job or a listen")
}

/// The person's listens the copy imported.
fn imported_listens(db: &Path, user: &str) -> Vec<Listen> {
    /// When, the artist's id and name, the track, the recording's id, the
    /// release's name and id.
    type Row = (
        i64,
        Option<String>,
        String,
        String,
        Option<String>,
        Option<String>,
        Option<String>,
    );
    let rows: Vec<Row> = db.rows(
        "SELECT listened_at, lower(artist_mbid), artist_name, track_name, \
             lower(recording_mbid), release_name, lower(release_mbid) \
             FROM media_imported_listens WHERE user_id = ?1",
        [user],
    );
    rows.into_iter()
        .map(
            |(
                at_ms,
                artist_mbid,
                artist_name,
                track_name,
                recording_mbid,
                release_name,
                release_mbid,
            )| Listen {
                at_ms,
                artist_mbid,
                artist_name,
                track_name,
                recording_mbid,
                release_name,
                release_mbid,
                in_app: false,
            },
        )
        .collect()
}

/// The person's plays in the app, their artist by its MusicBrainz id.
fn app_plays(db: &Path, user: &str) -> Vec<Listen> {
    let rows: Vec<(i64, Option<String>, String, String)> = db.rows(
        "SELECT h.played_at, (SELECT lower(ai.provider_artist_id) \
         FROM local_artist_external_identities ai WHERE ai.local_artist_id = h.local_artist_id \
         AND ai.provider = 'musicbrainz' LIMIT 1), h.artist_name, h.track_name \
         FROM library_play_history h WHERE h.user_id = ?1",
        [user],
    );
    rows.into_iter()
        .map(|(at_ms, artist_mbid, artist_name, track_name)| Listen {
            at_ms,
            artist_mbid,
            artist_name,
            track_name,
            recording_mbid: None,
            release_name: None,
            release_mbid: None,
            in_app: true,
        })
        .collect()
}

/// The person's requests, each by its album's artist.
fn requests(db: &Path, user: &str) -> Vec<Request> {
    let rows: Vec<(Option<String>, i64)> = db.rows(
        "SELECT COALESCE(lower(h.artist_mbid), (SELECT lower(a.artist_mbid) \
         FROM catalog_artist_albums a WHERE lower(a.release_group_mbid) = h.musicbrainz_id_lower \
         LIMIT 1)), q.requested_at FROM request_history_requesters q \
         JOIN request_history h ON h.musicbrainz_id_lower = q.musicbrainz_id_lower \
         WHERE q.user_id = ?1",
        [user],
    );
    rows.into_iter()
        .filter_map(|(artist, at_ms)| {
            Some(Request {
                artist_mbid: artist?,
                at_ms,
            })
        })
        .collect()
}

/// One album the library holds, and when it is known to have been there.
struct LibraryAlbum {
    id: String,
    /// The latest moment it is known not to be in the library yet: its
    /// first play or request, or its scan when that came after the first.
    after: Option<i64>,
}

/// Every album the copy's library holds, with when it came: its first play
/// in the app (anyone's), the person's first listen of its release, else of
/// its album artist (an edition they never played is dated by when they
/// first played the artist), its first request, and its scan when that came
/// after the first one.
fn library(db: &Path, timeline: &Timeline) -> Vec<LibraryAlbum> {
    type Row = (
        String,
        Option<String>,
        Option<String>,
        i64,
        Option<i64>,
        Option<i64>,
    );
    let rows: Vec<Row> = db.rows(
        "SELECT a.id, lower(e.release_mbid), (SELECT lower(ai.provider_artist_id) \
         FROM local_artist_external_identities ai WHERE ai.local_artist_id = a.album_artist_id \
         AND ai.provider = 'musicbrainz' LIMIT 1), a.created_at, \
         (SELECT MIN(p.played_at) FROM library_play_history p WHERE p.local_album_id = a.id), \
         (SELECT MIN(h.requested_at) FROM request_history h WHERE h.request_kind = 'album' \
          AND h.musicbrainz_id_lower = lower(e.release_group_mbid)) \
         FROM local_albums a LEFT JOIN local_album_external_identities e \
         ON e.local_album_id = a.id AND e.provider = 'musicbrainz' \
         WHERE a.retired_into_album_id IS NULL AND a.has_indexed_track = 1",
        [],
    );
    let first_scan = rows.iter().map(|row| row.3).min().unwrap_or(0);
    let mut by_release: HashMap<&str, i64> = HashMap::new();
    let mut by_artist: HashMap<&str, i64> = HashMap::new();
    for listen in &timeline.listens {
        if let Some(release) = listen.release_mbid.as_deref() {
            by_release.entry(release).or_insert(listen.at_ms);
        }
        if let Some(artist) = listen.artist_mbid.as_deref() {
            by_artist.entry(artist).or_insert(listen.at_ms);
        }
    }
    rows.iter()
        .map(|(id, release, artist, created_at, played, requested)| {
            let listened = release
                .as_deref()
                .and_then(|release| by_release.get(release).copied())
                .or_else(|| {
                    artist
                        .as_deref()
                        .and_then(|artist| by_artist.get(artist).copied())
                });
            let scanned = (*created_at > first_scan + DAY_MS).then_some(*created_at);
            let first = [*played, listened, *requested].into_iter().flatten().min();
            LibraryAlbum {
                id: id.clone(),
                after: [first, scanned].into_iter().flatten().max(),
            }
        })
        .collect()
}

/// Put the copy at `db` back to `cut` for `user`, as the module docs say;
/// `listens` replace the person's imported listens when given. Answers how
/// many albums left the library.
fn put_back(
    db: &Path,
    user: &str,
    cut: i64,
    listens: Option<&[&Listen]>,
    albums: &[LibraryAlbum],
) -> usize {
    // Foreign keys stay off, as a plain connection opens: every row that
    // goes is named here, and nothing that stays is checked against it.
    let mut connection = rusqlite::Connection::open(db).expect("the cut copy opens");
    let tx = connection.transaction().expect("a transaction");
    for table in [
        "catalog_discover_candidate_paths",
        "catalog_discover_candidates",
        "catalog_discover_pool_builds",
        "catalog_known_release_groups",
        "catalog_taste_artists",
        "catalog_taste_groups",
        "catalog_taste_builds",
    ] {
        tx.execute(&format!("DELETE FROM {table} WHERE user_id = ?1"), [user])
            .unwrap_or_else(|error| panic!("{table}: {error}"));
    }
    for statement in [
        "DELETE FROM library_play_history WHERE user_id = ?1 AND played_at > ?2",
        "DELETE FROM catalog_user_favorites WHERE user_id = ?1 AND created_at > ?2",
        "DELETE FROM catalog_user_ratings WHERE user_id = ?1 AND rated_at > ?2",
        "DELETE FROM acquire_followed_artists WHERE user_id = ?1 AND followed_at > ?2",
        "DELETE FROM catalog_playlist_tracks WHERE created_at > ?2 \
         AND playlist_id IN (SELECT id FROM catalog_playlists WHERE user_id = ?1)",
        "DELETE FROM catalog_discover_feedback WHERE user_id = ?1 AND created_at > ?2",
        "DELETE FROM catalog_discover_picks WHERE user_id = ?1 AND first_offered_at > ?2",
        // A pick offered by the cut keeps no show or sample from after it:
        // its last show at the cut is the first one known.
        "UPDATE catalog_discover_picks SET first_shown_at = NULL, last_shown_at = NULL, \
         shown_days = 0 WHERE user_id = ?1 AND first_shown_at > ?2",
        "UPDATE catalog_discover_picks SET last_shown_at = first_shown_at \
         WHERE user_id = ?1 AND last_shown_at > ?2",
        "UPDATE catalog_discover_picks SET sampled_at = NULL, sampled_seconds = NULL \
         WHERE user_id = ?1 AND sampled_at > ?2",
    ] {
        tx.execute(statement, rusqlite::params![user, cut])
            .unwrap_or_else(|error| panic!("{statement}: {error}"));
    }
    for statement in [
        "DELETE FROM request_history_requesters WHERE requested_at > ?1",
        "DELETE FROM request_history WHERE requested_at > ?1",
    ] {
        tx.execute(statement, [cut])
            .unwrap_or_else(|error| panic!("{statement}: {error}"));
    }
    match listens {
        None => {
            tx.execute(
                "DELETE FROM media_imported_listens WHERE user_id = ?1 AND listened_at > ?2",
                rusqlite::params![user, cut],
            )
            .expect("later listens go");
        }
        Some(listens) => {
            tx.execute(
                "DELETE FROM media_imported_listens WHERE user_id = ?1",
                [user],
            )
            .expect("the import goes");
            let mut insert = tx
                .prepare(
                    "INSERT OR IGNORE INTO media_imported_listens (user_id, listened_at, \
                     recording_mbid, artist_mbid, artist_name, track_name, release_name, \
                     release_mbid, imported_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                )
                .expect("the insert prepares");
            for listen in listens {
                insert
                    .execute(rusqlite::params![
                        user,
                        listen.at_ms,
                        listen.recording_mbid,
                        listen.artist_mbid,
                        listen.artist_name,
                        listen.track_name,
                        listen.release_name,
                        listen.release_mbid,
                        cut,
                    ])
                    .expect("a listen goes in");
            }
        }
    }
    let gone: Vec<&str> = albums
        .iter()
        .filter(|album| album.after.is_some_and(|after| after > cut))
        .map(|album| album.id.as_str())
        .collect();
    let ids = serde_json::to_string(&gone).expect("ids");
    tx.execute(
        "UPDATE local_tracks SET availability = 'missing' WHERE availability = 'indexed' \
         AND local_album_id IN (SELECT value FROM json_each(?1))",
        [ids],
    )
    .expect("later albums leave the library");
    tx.commit().expect("the cut commits");
    gone.len()
}

// -- the engine -------------------------------------------------------------

/// The production wiring over one copy, on a clock the run sets.
struct Run {
    deps: DiscoverDeps,
    network: Arc<Replay>,
    clock: Arc<ManualClock>,
    platform: Platform,
    rig: AuthRig,
    runtime: DbRuntime,
}

impl Run {
    async fn open(dir: &Path, db: &Path, at: i64, network: Arc<Replay>) -> Self {
        std::fs::create_dir_all(dir).expect("the app folder creates");
        let runtime = open_runtime(
            &DbConfig::new(db, Arc::new(droppedneedle_kernel::clock::SystemClock))
                .with_lane_limits(LaneLimits::patient()),
        )
        .await
        .expect("the copy opens");
        let clock = Arc::new(ManualClock::at(at));
        let platform = TestPlatform::on(runtime.db(), dir)
            .clock(Arc::clone(&clock) as Arc<dyn Clock>)
            .build()
            .await;
        let rig = AuthRig::on(&platform);
        let providers = Providers::over_wire(
            Arc::clone(&network.wire) as Arc<dyn droppedneedle_providers::Wire>,
            Arc::clone(&clock) as Arc<dyn Clock>,
            ProviderEndpoints::closed(),
        );
        let listening = Arc::clone(&network) as Arc<dyn ListeningNetwork>;
        let deps = discover_deps(&rig, dir, providers, move |slot| *slot = listening);
        Self {
            deps,
            network,
            clock,
            platform,
            rig,
            runtime,
        }
    }

    /// Stop the loops and close the copy, so the next one starts clean.
    async fn close(self) {
        let Self {
            deps,
            platform,
            rig,
            runtime,
            ..
        } = self;
        platform.supervisor.shutdown(CLOSE_GRACE).await;
        drop((deps, rig, platform));
        runtime.shutdown_within(CLOSE_GRACE).await;
    }

    fn db(&self) -> &droppedneedle_kernel::db::Db {
        &self.platform.db
    }

    /// Run the taste and pool jobs for `user` now; `during` names the
    /// build when it asked a provider. A job's failure names the requests
    /// made before it too.
    async fn build(&self, user: &str, during: &str) {
        if let Err(error) = self.deps.taste.run_once(user, |_| Ok(())).await {
            self.network.check(during);
            panic!("the taste job failed during {during}: {error:?}");
        }
        if let Err(error) = self.deps.pool.run_once(user, |_| Ok(())).await {
            self.network.check(during);
            panic!("the pool job failed during {during}: {error:?}");
        }
        self.network.check(during);
    }

    /// `user`'s pool now, gates applied, and what rests for them.
    async fn pool(&self, user: &str) -> (Vec<Candidate>, Exposure) {
        let now = self.clock.now_ms();
        let pool = pool_store::ranked(self.db(), user, now)
            .await
            .expect("the pool reads");
        let shown: Vec<(String, String, i64, bool, bool)> = self.db().rows(
            "SELECT p.release_group_mbid_lower, COALESCE(lower(p.artist_mbid), ''), \
             p.last_shown_at, p.sampled_at IS NOT NULL, EXISTS (SELECT 1 \
             FROM request_history_requesters r WHERE r.user_id = p.user_id \
             AND r.musicbrainz_id_lower = p.release_group_mbid_lower) \
             FROM catalog_discover_picks p WHERE p.user_id = ?1 AND p.last_shown_at IS NOT NULL",
            [user],
        );
        let shown: Vec<Shown> = shown
            .into_iter()
            .map(
                |(release_group, artist, last_shown_at, sampled, requested)| Shown {
                    release_group,
                    artist,
                    last_shown_at,
                    sampled,
                    requested,
                },
            )
            .collect();
        (pool, Exposure::from_shown(&shown, now))
    }
}

/// What a person's taste build holds.
struct Taste {
    row: Option<(i64, i64, i64, i64, bool)>,
    groups: Vec<(i64, String, f64)>,
    /// The seeds, heaviest first: id, name, weight.
    seeds: Vec<(String, String, f64)>,
}

impl Taste {
    fn read(db: &impl Probe, user: &str) -> Self {
        Self {
            row: db.maybe_row(
                "SELECT plays_counted, imported_listens_counted, artists_played, seeds, \
                 incomplete FROM catalog_taste_builds WHERE user_id = ?1",
                [user],
            ),
            groups: db.rows(
                "SELECT group_no, anchor_name, share FROM catalog_taste_groups \
                 WHERE user_id = ?1 ORDER BY group_no",
                [user],
            ),
            seeds: db.rows(
                "SELECT artist_mbid_lower, name, weight FROM catalog_taste_artists \
                 WHERE user_id = ?1 AND is_seed = 1 ORDER BY weight DESC, artist_mbid_lower",
                [user],
            ),
        }
    }

    fn shares(&self) -> Vec<Share> {
        self.groups
            .iter()
            .map(|(group, _, share)| Share {
                group: *group,
                share: *share,
            })
            .collect()
    }

    fn anchor(&self, group: Option<i64>) -> String {
        group
            .and_then(|group| self.groups.iter().find(|row| row.0 == group))
            .map_or_else(|| "further out".to_owned(), |row| row.1.clone())
    }

    fn top_seed(&self) -> Option<&str> {
        self.seeds.first().map(|seed| seed.0.as_str())
    }

    fn print(&self) {
        match self.row {
            Some((plays, listens, artists, seeds, incomplete)) => println!(
                "taste: {plays} plays, {listens} imported listens, {artists} artists, \
                 {seeds} seeds{}",
                if incomplete { ", incomplete" } else { "" }
            ),
            None => println!("taste: no build"),
        }
        for (group, anchor, share) in &self.groups {
            println!("  group {group} {anchor}: {:.0}%", share * 100.0);
        }
    }
}

/// The pool build's counts, as one line.
fn pool_line(db: &impl Probe, user: &str) -> String {
    type Row = (bool, i64, i64, i64, i64, Option<i64>, i64, i64, i64, i64);
    let row: Option<Row> = db.maybe_row(
        "SELECT incomplete, json_array_length(waiting_for), seeds_used, neighbours_found, \
         known_left_out, hubs_left_out, artists_ranked, artists_without_album, \
         new_artist_candidates, source_pick_candidates FROM catalog_discover_pool_builds \
         WHERE user_id = ?1",
        [user],
    );
    match row {
        None => "pool: no build".to_owned(),
        Some((incomplete, waiting, seeds, found, known, hubs, ranked, without, new, picks)) => {
            format!(
                "pool: {seeds} seeds, {found} neighbours, {known} known left out, {} hubs left \
                 out, {ranked} ranked, {without} without an album, {new} new-artist and {picks} \
                 playlist candidates, waiting for {waiting} artists{}",
                hubs.map_or_else(|| "-".to_owned(), |hubs| hubs.to_string()),
                if incomplete { ", incomplete" } else { "" }
            )
        }
    }
}

/// The facts the builds wanted that the copy does not keep, by kind.
fn wanted(db: &impl Probe) -> Vec<String> {
    let rows: Vec<(String, String, Option<String>)> = db.rows(
        "SELECT f.kind, f.artist_mbid, COALESCE((SELECT t.name FROM catalog_taste_artists t \
         WHERE t.artist_mbid_lower = f.artist_mbid LIMIT 1), (SELECT n.neighbour_name \
         FROM catalog_artist_neighbours n WHERE n.neighbour_mbid = f.artist_mbid LIMIT 1)) \
         FROM catalog_fact_fetches f WHERE f.wanted_at IS NOT NULL ORDER BY f.kind, f.artist_mbid",
        [],
    );
    let mut by_kind: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for (kind, artist, name) in rows {
        by_kind
            .entry(kind)
            .or_default()
            .push(name.unwrap_or(artist));
    }
    by_kind
        .into_iter()
        .map(|(kind, names)| {
            let shown: Vec<&str> = names.iter().take(12).map(String::as_str).collect();
            let more = names.len().saturating_sub(shown.len());
            format!(
                "{} {kind}: {}{}",
                names.len(),
                shown.join(", "),
                if more > 0 {
                    format!(" and {more} more")
                } else {
                    String::new()
                }
            )
        })
        .collect()
}

// -- now --------------------------------------------------------------------

/// One pick's lines in a printed set.
fn print_pick(number: usize, pick: &Candidate, taste: &Taste) {
    let year = pick
        .first_release_date
        .as_deref()
        .and_then(|date| date.get(..4))
        .unwrap_or("----");
    println!("  {number}. {} - {} ({year})", pick.artist_name, pick.title);
    let count = |count: Option<i64>| count.map_or_else(|| "-".to_owned(), |n| n.to_string());
    println!(
        "     taste group: {}; listeners: artist {}, album {}",
        taste.anchor(pick.group),
        count(pick.artist_listeners),
        count(pick.album_listeners)
    );
    let seeds: Vec<String> = pick
        .paths
        .iter()
        .filter(|path| path.kind == PathKind::Seed)
        .filter_map(|path| {
            let artist = path.artist.as_ref()?;
            let link = path.link.as_ref()?;
            Some(format!(
                "{} ({} plays, #{} of {}{})",
                artist.name,
                artist.plays,
                link.rank,
                link.of,
                if link.lastfm { ", Last.fm too" } else { "" }
            ))
        })
        .collect();
    if !seeds.is_empty() {
        println!("     seeds: {}", seeds.join("; "));
    }
    if pick.paths.iter().any(|path| path.kind == PathKind::Source) {
        println!("     named by their ListenBrainz playlist");
    }
}

/// Build the person's set at the copy's own time and print it.
async fn now(dir: &Path, db: &Path, person: &Person, network: Arc<Replay>) {
    let at = copy_time(db);
    let picks_before: Option<i64> = db.maybe(
        "SELECT source_pick_candidates FROM catalog_discover_pool_builds WHERE user_id = ?1",
        [&person.id],
    );
    let run = Run::open(dir, db, at, Arc::clone(&network)).await;
    run.build(
        &person.id,
        &format!("the build for {} at {}", person.name, replay::day(at)),
    )
    .await;
    let (pool, exposure) = run.pool(&person.id).await;
    let taste = Taste::read(run.db(), &person.id);
    let selection = select::select(&pool, SET, &taste.shares(), &exposure, Floors::PROPOSED);

    println!(
        "== replay now: {} at {} (the copy's latest job)",
        person.name,
        replay::day(at)
    );
    println!(
        "accounts: {}; settings sections holding a sealed value at their defaults",
        person.describe()
    );
    for answer in network.take_answered() {
        println!("answered by the replay: {answer}");
    }
    taste.print();
    println!("{}", pool_line(run.db(), &person.id));
    match picks_before {
        Some(picks) => println!(
            "the copy's own pool, before this build: {picks} playlist candidates (not replayed)"
        ),
        None => println!("the copy held no pool for them before this build"),
    }
    println!(
        "live pool after the gates that move: {} candidates",
        pool.len()
    );
    for line in wanted(run.db()) {
        println!("wanted, not in the copy: {line}");
    }
    println!(
        "set of {}{}:",
        selection.picks.len(),
        selection
            .short
            .map_or_else(String::new, |short| format!(" (short: {short:?})"))
    );
    for (at, pick) in selection.picks.iter().enumerate() {
        print_pick(at + 1, pick, &taste);
    }
    run.close().await;
}

// -- cuts -------------------------------------------------------------------

/// The engines a cut scores.
const ENGINES: [&str; 4] = ["engine", "most popular", "uniform from pool", "own artists"];

/// One kept album row, as the baselines read it.
struct KeptAlbum {
    release_group: String,
    artist: String,
    date: Option<String>,
    artist_listeners: Option<i64>,
}

/// Every kept album a pick may be (album or EP, not live, a compilation or
/// a demo), most listened first.
fn kept_albums(db: &(impl Probe + ?Sized)) -> Vec<KeptAlbum> {
    let rows: Vec<(String, String, Option<String>, Option<i64>)> = db.rows(
        "SELECT lower(a.release_group_mbid), lower(a.artist_mbid), a.first_release_date, \
         l.listeners FROM catalog_artist_albums a \
         LEFT JOIN catalog_artist_listeners l ON l.artist_mbid = a.artist_mbid \
         WHERE a.primary_type IN ('Album', 'EP') AND NOT EXISTS (SELECT 1 \
         FROM json_each(COALESCE(a.secondary_types, '[]')) s \
         WHERE s.value IN ('Live', 'Compilation', 'Demo')) \
         ORDER BY a.listeners IS NULL, a.listeners DESC, a.release_group_mbid",
        [],
    );
    rows.into_iter()
        .map(
            |(release_group, artist, date, artist_listeners)| KeptAlbum {
                release_group,
                artist,
                date,
                artist_listeners,
            },
        )
        .collect()
}

/// Up to [`SET`] of `albums` in their order, one an artist, each passing
/// `keep`.
fn take_set<'a>(
    albums: impl Iterator<Item = &'a KeptAlbum>,
    mut keep: impl FnMut(&KeptAlbum) -> bool,
) -> Vec<Scored> {
    let mut artists = HashSet::new();
    let mut groups = HashSet::new();
    let mut set = Vec::new();
    for album in albums {
        if set.len() >= SET {
            break;
        }
        if !keep(album) || artists.contains(&album.artist) || groups.contains(&album.release_group)
        {
            continue;
        }
        artists.insert(album.artist.clone());
        groups.insert(album.release_group.clone());
        set.push(Scored {
            release_group: album.release_group.clone(),
            artist: album.artist.clone(),
            artist_listeners: album.artist_listeners,
            strongest_seed: None,
        });
    }
    set
}

fn scored(candidate: &Candidate) -> Scored {
    Scored {
        release_group: candidate.release_group_mbid.clone(),
        artist: candidate.artist_mbid.clone(),
        artist_listeners: candidate.artist_listeners,
        strongest_seed: candidate.strongest_seed().map(str::to_owned),
    }
}

/// A uniform draw of [`SET`] from `pool`, one an artist, seeded by `seed`.
fn uniform(pool: &[&Candidate], seed: i64) -> Vec<Scored> {
    let mut state = seed as u64 ^ 0x9e37_79b9_7f4a_7c15;
    let mut next = move || {
        state = state.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = state;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    };
    let mut order: Vec<&Candidate> = pool.to_vec();
    for at in (1..order.len()).rev() {
        let other = (next() % (at as u64 + 1)) as usize;
        order.swap(at, other);
    }
    let mut artists = HashSet::new();
    order
        .into_iter()
        .filter(|candidate| artists.insert(candidate.artist_mbid.clone()))
        .take(SET)
        .map(scored)
        .collect()
}

/// What the baselines read at a cut from the copy as it stood then.
struct AtCut {
    owned: HashSet<String>,
    played_groups: HashSet<String>,
    /// The person's artists, heaviest first.
    own_artists: Vec<String>,
    known: HashSet<String>,
    owned_artists: HashSet<String>,
    named: HashSet<String>,
    lists_kept: HashSet<String>,
    seeds_without_list: usize,
}

impl AtCut {
    fn read(db: &impl Probe, user: &str) -> Self {
        let set = |sql: &str, params: &[&str]| -> HashSet<String> {
            db.column::<String>(sql, rusqlite::params_from_iter(params))
                .into_iter()
                .collect()
        };
        let own_artists = db.column(
            "SELECT artist_mbid_lower FROM catalog_taste_artists WHERE user_id = ?1 \
             ORDER BY weight DESC, artist_mbid_lower",
            [user],
        );
        Self {
            owned: set(
                "SELECT DISTINCT lower(e.release_group_mbid) FROM local_album_external_identities e \
                 JOIN local_albums a ON a.id = e.local_album_id WHERE e.provider = 'musicbrainz' \
                 AND e.release_group_mbid IS NOT NULL AND a.retired_into_album_id IS NULL \
                 AND a.has_indexed_track = 1",
                &[],
            ),
            played_groups: set(
                "SELECT release_group_mbid_lower FROM catalog_known_release_groups \
                 WHERE user_id = ?1",
                &[user],
            ),
            known: own_artists.iter().cloned().collect(),
            own_artists,
            owned_artists: set(
                "SELECT DISTINCT lower(ai.provider_artist_id) \
                 FROM local_artist_external_identities ai \
                 JOIN local_album_artists oc ON oc.local_artist_id = ai.local_artist_id \
                 JOIN local_albums ob ON ob.id = oc.local_album_id \
                 WHERE ai.provider = 'musicbrainz' AND ob.retired_into_album_id IS NULL \
                 AND ob.has_indexed_track = 1",
                &[],
            ),
            named: set(
                "SELECT DISTINCT lower(n.neighbour_mbid) FROM catalog_artist_neighbours n \
                 JOIN catalog_taste_artists s ON s.artist_mbid_lower = n.artist_mbid \
                 AND s.user_id = ?1 AND s.is_seed = 1",
                &[user],
            ),
            lists_kept: set(
                "SELECT artist_mbid FROM catalog_fact_fetches WHERE kind = 'albums' \
                 AND outcome IS NOT NULL",
                &[],
            ),
            seeds_without_list: db.one(
                "SELECT COUNT(*) FROM catalog_taste_artists s WHERE s.user_id = ?1 \
                 AND s.is_seed = 1 AND NOT EXISTS (SELECT 1 FROM catalog_fact_fetches f \
                 WHERE f.artist_mbid = s.artist_mbid_lower AND f.kind = 'neighbours' \
                 AND f.outcome IS NOT NULL)",
                [user],
            ),
        }
    }
}

/// Where one adopted artist was lost, or found: `pool_all` is the live
/// pool, `dated` the part of it out by the cut.
fn stage(
    artist: &str,
    picks: &[Scored],
    pool_all: &[Candidate],
    dated: &[Candidate],
    at: &AtCut,
) -> &'static str {
    if picks.iter().any(|pick| pick.artist == artist) {
        return "in the eight";
    }
    let theirs: Vec<&Candidate> = dated.iter().filter(|c| c.artist_mbid == artist).collect();
    if !theirs.is_empty() {
        return if theirs.iter().all(|c| !Floors::PROPOSED.clears(c)) {
            "in the pool, under a floor"
        } else {
            "in the pool, ranked out"
        };
    }
    if pool_all.iter().any(|c| c.artist_mbid == artist) {
        return "in the pool only with albums out after the cut";
    }
    if at.known.contains(artist) {
        return "known to the engine at the cut";
    }
    if at.owned_artists.contains(artist) {
        return "the library held them at the cut";
    }
    if at.named.contains(artist) {
        return if at.lists_kept.contains(artist) {
            "named by a seed's list, left out by the walk, a hub or the album gates"
        } else {
            "named by a seed's list, no album list kept"
        };
    }
    "in no seed's kept list"
}

/// Everything one cut scored.
struct CutScore {
    cut: i64,
    truth: Truth,
    sets: Vec<SetScore>,
    repeats: Vec<Option<f64>>,
    reachable: usize,
    stages: Vec<(String, &'static str)>,
}

/// The four sets at one moment: the engine's, then the baselines'.
struct Sets {
    sets: [Vec<Scored>; 4],
    /// The live pool.
    all: Vec<Candidate>,
    /// The part of it out by the date.
    dated: Vec<Candidate>,
}

/// What every cut shares: the copy, the person, the timeline and what the
/// baselines read.
struct Replayed<'a> {
    base: &'a Path,
    person: &'a Person,
    network: &'a Arc<Replay>,
    timeline: &'a Timeline,
    /// Whether the timeline is an export, which then replaces the person's
    /// imported listens at each cut.
    from_export: bool,
    albums: &'a [LibraryAlbum],
    kept: &'a [KeptAlbum],
    requests: &'a [Request],
    window: i64,
}

impl Replayed<'_> {
    /// The engine's set and the baselines' at `now` (the cut, or a day
    /// after it), for a cut at `cut`.
    async fn sets(&self, run: &Run, now: i64, at: &AtCut, heard: &HashSet<String>) -> Sets {
        let user = &self.person.id;
        let (pool_all, exposure) = run.pool(user).await;
        let dated: Vec<Candidate> = pool_all
            .iter()
            .filter(|c| !replay::released_after(c.first_release_date.as_deref(), now))
            .cloned()
            .collect();
        let taste = Taste::read(run.db(), user);
        let engine: Vec<Scored> =
            select::select(&dated, SET, &taste.shares(), &exposure, Floors::PROPOSED)
                .picks
                .into_iter()
                .map(scored)
                .collect();
        let eligible: Vec<&Candidate> = dated
            .iter()
            .filter(|c| Floors::PROPOSED.clears(c) && !exposure.rests(c))
            .collect();
        let popular = take_set(self.kept.iter(), |album| {
            !at.owned.contains(&album.release_group)
                && !heard.contains(&album.artist)
                && !replay::released_after(album.date.as_deref(), now)
        });
        let rank: HashMap<&str, usize> = at
            .own_artists
            .iter()
            .enumerate()
            .map(|(place, artist)| (artist.as_str(), place))
            .collect();
        let mut theirs: Vec<&KeptAlbum> = self
            .kept
            .iter()
            .filter(|album| rank.contains_key(album.artist.as_str()))
            .collect();
        theirs.sort_by_key(|album| rank[album.artist.as_str()]);
        let own = take_set(theirs.into_iter(), |album| {
            !at.owned.contains(&album.release_group)
                && !at.played_groups.contains(&album.release_group)
                && !replay::released_after(album.date.as_deref(), now)
        });
        let drawn = uniform(&eligible, now / DAY_MS);
        Sets {
            sets: [engine, popular, drawn, own],
            all: pool_all,
            dated,
        }
    }

    /// Run the engine and the baselines at `cut`, and print the cut.
    async fn cut(&self, cut: i64) -> CutScore {
        let user = &self.person.id;
        let dir = ScratchDir::new("discover-replay-cut");
        let db = dir.join("cut.db");
        backup(self.base, &db);
        let inputs: Vec<&Listen> = self
            .timeline
            .until(cut)
            .filter(|listen| !listen.in_app)
            .collect();
        let gone = put_back(
            &db,
            user,
            cut,
            self.from_export.then_some(inputs.as_slice()),
            self.albums,
        );
        let run = Run::open(&dir.join("app"), &db, cut, Arc::clone(self.network)).await;
        run.build(user, &format!("the build at {}", replay::day(cut)))
            .await;
        let taste = Taste::read(run.db(), user);
        let at = AtCut::read(run.db(), user);
        let wants = wanted(run.db());
        let pool_line = pool_line(run.db(), user);
        let heard = self.timeline.heard_by(cut);
        let now = self.sets(&run, cut, &at, &heard).await;
        let pool_all = &now.all;

        // The same inputs a day later.
        run.clock.advance(DAY_MS);
        run.build(user, &format!("the build a day after {}", replay::day(cut)))
            .await;
        let later = self.sets(&run, cut + DAY_MS, &at, &heard).await;
        let repeats = now
            .sets
            .iter()
            .zip(&later.sets)
            .map(|(set, later)| replay::repeat(set, later))
            .collect();

        let truth = replay::truth(self.timeline, cut, self.window, self.requests);
        let scores: Vec<SetScore> = now
            .sets
            .iter()
            .map(|set| replay::score(set, &truth, &heard, &at.owned, taste.top_seed()))
            .collect();
        let adopted = truth.ids(ADOPTED);
        let pool_artists: HashSet<&str> =
            now.dated.iter().map(|c| c.artist_mbid.as_str()).collect();
        let reachable = adopted
            .iter()
            .filter(|artist| pool_artists.contains(artist.as_str()))
            .count();
        let stages: Vec<(String, &'static str)> = adopted
            .iter()
            .map(|artist| {
                let found = stage(artist, &now.sets[0], pool_all, &now.dated, &at);
                (artist.clone(), found)
            })
            .collect();

        let names: HashMap<&str, &str> = truth
            .new
            .iter()
            .map(|found| (found.artist.as_str(), found.name.as_str()))
            .collect();
        let name = |artist: &str| names.get(artist).copied().unwrap_or(artist).to_owned();
        println!("-- cut {}", replay::day(cut));
        println!(
            "inputs: {} listens and plays by the cut; {gone} albums out of the library",
            self.timeline.until(cut).count()
        );
        println!(
            "truth: {} new artists played, {} adopted ({ADOPTED}+), {} at {ADOPTED_HEAVILY}+; \
             {} adopted through the app",
            truth.new.len(),
            adopted.len(),
            truth.ids(ADOPTED_HEAVILY).len(),
            truth
                .at_least(ADOPTED)
                .filter(|found| found.through_app)
                .count()
        );
        if !adopted.is_empty() {
            let list: Vec<String> = truth
                .at_least(ADOPTED)
                .map(|found| format!("{} ({})", found.name, found.plays))
                .collect();
            println!("  adopted: {}", list.join(", "));
        }
        taste.print();
        println!("{pool_line}");
        let coarse = now
            .dated
            .iter()
            .filter(|c| replay::coarse_date(c.first_release_date.as_deref()))
            .count();
        println!(
            "live pool {}; {} first released after the cut dropped; {coarse} dated only to a \
             year or month kept",
            pool_all.len(),
            pool_all.len() - now.dated.len()
        );
        if at.seeds_without_list > 0 {
            println!("{} seeds have no kept similar list", at.seeds_without_list);
        }
        for line in wants {
            println!("wanted, not in the copy: {line}");
        }
        for (engine, (set, score)) in ENGINES.iter().zip(now.sets.iter().zip(&scores)) {
            let found: Vec<String> = score.adopted.iter().map(|artist| name(artist)).collect();
            println!(
                "{engine:>18}: {} picks, {} new artists played, adopted found {}{}",
                set.len(),
                score.new_played.len(),
                score.adopted.len(),
                if found.is_empty() {
                    String::new()
                } else {
                    format!(" ({})", found.join(", "))
                }
            );
        }
        for (artist, stage) in &stages {
            println!("  {}: {stage}", name(artist));
        }
        run.close().await;
        CutScore {
            cut,
            truth,
            sets: scores,
            repeats,
            reachable,
            stages,
        }
    }
}

fn share(part: usize, whole: usize) -> String {
    if whole == 0 {
        "-".to_owned()
    } else {
        format!("{:.0}%", 100.0 * part as f64 / whole as f64)
    }
}

/// The table over every cut, the paired counts and where adopted artists
/// were lost.
fn print_table(cuts: &[CutScore]) {
    let adopted: usize = cuts.iter().map(|cut| cut.truth.ids(ADOPTED).len()).sum();
    let heavily: usize = cuts
        .iter()
        .map(|cut| cut.truth.ids(ADOPTED_HEAVILY).len())
        .sum();
    let new: usize = cuts.iter().map(|cut| cut.truth.new.len()).sum();
    let reachable: usize = cuts.iter().map(|cut| cut.reachable).sum();
    println!("== {} cuts", cuts.len());
    println!(
        "truth over all cuts: {new} new artists played, {adopted} adopted ({ADOPTED}+), \
         {heavily} at {ADOPTED_HEAVILY}+"
    );
    println!(
        "{:>18}  {:>9}  {:>8}  {:>9}  {:>6}  {:>5}  {:>5}  {:>9}  {:>8}  {:>6}",
        "engine",
        "reachable",
        "new in 8",
        "adopted",
        "20+",
        "owned",
        "heard",
        "listeners",
        "top seed",
        "repeat"
    );
    for (at, engine) in ENGINES.iter().enumerate() {
        let sets: Vec<&SetScore> = cuts.iter().map(|cut| &cut.sets[at]).collect();
        let picks: usize = sets.iter().map(|set| set.picks).sum();
        let listeners: Vec<i64> = sets.iter().flat_map(|set| set.listeners.clone()).collect();
        let top_seed: Vec<usize> = sets.iter().filter_map(|set| set.top_seed).collect();
        let seeded: usize = sets
            .iter()
            .filter(|set| set.top_seed.is_some())
            .map(|set| set.picks)
            .sum();
        let repeats: Vec<f64> = cuts.iter().filter_map(|cut| cut.repeats[at]).collect();
        let pooled = at == 0 || at == 2;
        println!(
            "{engine:>18}  {:>9}  {:>8}  {:>9}  {:>6}  {:>5}  {:>5}  {:>9}  {:>8}  {:>6}",
            if pooled {
                share(reachable, adopted)
            } else {
                "-".to_owned()
            },
            sets.iter().map(|set| set.new_played.len()).sum::<usize>(),
            format!(
                "{} of {adopted}",
                sets.iter().map(|set| set.adopted.len()).sum::<usize>()
            ),
            sets.iter().map(|set| set.adopted_heavily).sum::<usize>(),
            share(sets.iter().map(|set| set.owned).sum(), picks),
            share(sets.iter().map(|set| set.heard).sum(), picks),
            replay::median(&listeners).map_or_else(|| "-".to_owned(), |n| n.to_string()),
            if top_seed.is_empty() {
                "-".to_owned()
            } else {
                share(top_seed.iter().sum(), seeded)
            },
            if repeats.is_empty() {
                "-".to_owned()
            } else {
                format!(
                    "{:.0}%",
                    100.0 * repeats.iter().sum::<f64>() / repeats.len() as f64
                )
            },
        );
    }
    println!("columns: reachable is adopted artists in the pool; new in 8 counts picks whose");
    println!("artist was new and played in the window; heard is the artist played by the cut;");
    println!("listeners is the picks' median artist listeners; repeat is the set a day later");
    for (at, baseline) in ENGINES.iter().enumerate().skip(1) {
        for (label, ids) in [
            (format!("adopted ({ADOPTED}+)"), true),
            ("new and played".to_owned(), false),
        ] {
            let (mut only_engine, mut only_baseline) = (0, 0);
            for cut in cuts {
                let (engine, other) = if ids {
                    (&cut.sets[0].adopted, &cut.sets[at].adopted)
                } else {
                    (&cut.sets[0].new_played, &cut.sets[at].new_played)
                };
                only_engine += engine.difference(other).count();
                only_baseline += other.difference(engine).count();
            }
            println!(
                "engine against {baseline}, {label}: only the engine {only_engine}, only the \
                 baseline {only_baseline}, sign test p = {:.3}",
                replay::sign_test(only_engine, only_baseline)
            );
        }
    }
    let through_app: usize = cuts
        .iter()
        .map(|cut| {
            cut.truth
                .at_least(ADOPTED)
                .filter(|found| found.through_app && cut.sets[0].adopted.contains(&found.artist))
                .count()
        })
        .sum();
    println!(
        "adopted artists the engine found: {} through the app, the rest elsewhere",
        through_app
    );
    let mut lost: BTreeMap<&str, usize> = BTreeMap::new();
    for cut in cuts {
        for (_, stage) in &cut.stages {
            *lost.entry(stage).or_default() += 1;
        }
    }
    println!("where the {adopted} adopted artists were:");
    for (stage, count) in lost {
        println!("  {count:>3}  {stage}");
    }
    let windows: Vec<String> = cuts.iter().map(|cut| replay::day(cut.cut)).collect();
    println!("cuts: {}", windows.join(", "));
}

/// Score the engine and the baselines at every cut `spec` names.
async fn cuts(env: &Env, base: &Path, person: &Person, network: Arc<Replay>, spec: &str) {
    let (mut timeline, source) = match &env.timeline {
        Some(path) => (
            Timeline::from_export(path).expect("the export reads"),
            "the export",
        ),
        None => (
            Timeline::from_listens(imported_listens(base, &person.id)),
            "the copy's imported listens",
        ),
    };
    let imported = timeline.listens.len();
    timeline.add_app_plays(app_plays(base, &person.id));
    let Some((first, last)) = timeline.span() else {
        panic!("{} has no listens to cut", person.name);
    };
    let cut_days: Vec<i64> = if spec.trim() == "monthly" {
        timeline.monthly_cuts(env.window)
    } else {
        spec.split(',')
            .map(|day| replay::parse_day(day).unwrap_or_else(|| panic!("{day} is not YYYY-MM-DD")))
            .collect()
    };
    let with_ids = timeline
        .listens
        .iter()
        .filter(|listen| listen.artist_mbid.is_some())
        .count();
    println!("== replay cuts: {}", person.name);
    println!(
        "timeline: {source}, {imported} listens ({} lines unread), {} to {}; {} in-app plays \
         added, {} more the same as a listen; {with_ids} of {} name an artist id",
        timeline.skipped,
        replay::day(first),
        replay::day(last),
        timeline.listens.len() - imported,
        timeline.same_as_a_listen,
        timeline.listens.len(),
    );
    println!(
        "window {} weeks; the last cut a whole window allows is {}",
        env.window / WEEK_MS,
        replay::day(last - env.window)
    );
    println!(
        "accounts: {}; settings sections holding a sealed value at their defaults",
        person.describe()
    );
    let albums = library(base, &timeline);
    let kept = kept_albums(base);
    let requests = requests(base, &person.id);
    let replayed = Replayed {
        base,
        person,
        network: &network,
        timeline: &timeline,
        from_export: env.timeline.is_some(),
        albums: &albums,
        kept: &kept,
        requests: &requests,
        window: env.window,
    };
    let mut scores = Vec::new();
    for cut in cut_days {
        scores.push(replayed.cut(cut).await);
    }
    for answer in network.take_answered() {
        println!("answered by the replay: {answer}");
    }
    print_table(&scores);
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "a replay of a person's own data, run by hand: cargo test --test catalog discover_replay -- --ignored --nocapture, with DN_REPLAY_DB and DN_REPLAY_USER set (see the module docs)"]
async fn discover_replay_on_a_database_copy() {
    let env = Env::read();
    let scratch = ScratchDir::new("discover-replay");
    let base = scratch.join("base.db");
    backup(&env.db, &base);
    let listenbrainz_on = prepare(&base);
    let (person, lastfm_top) = match &env.lastfm {
        Some(file) => (Person::lastfm_only(&base), Some(lastfm_rows(file))),
        None => {
            let user = env
                .user
                .as_deref()
                .expect("DN_REPLAY_USER names the person, or DN_REPLAY_LASTFM a file");
            (Person::in_copy(&base, user), None)
        }
    };
    let network = Arc::new(Replay {
        people: HashMap::from([(person.id.clone(), person.accounts.clone())]),
        lastfm_top: lastfm_top
            .map(|rows| HashMap::from([(person.id.clone(), rows)]))
            .unwrap_or_default(),
        listenbrainz_on,
        asked: Mutex::default(),
        answered: Mutex::default(),
        wire: Arc::new(ScriptedWire::new()),
    });
    match env.cuts.as_deref() {
        None => now(&scratch.join("app"), &base, &person, network).await,
        Some(_) if env.lastfm.is_some() => {
            panic!("the Last.fm-only person has no history to cut")
        }
        Some(spec) => cuts(&env, &base, &person, network, spec).await,
    }
}

// -- the scoring, on a made-up timeline --------------------------------------

/// The replay's made-up fixtures.
fn fixture_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/catalog/replay")
}

/// The made-up timeline's export, zipped from its month files.
fn fixture_export(dir: &Path) -> PathBuf {
    use std::io::Write as _;
    let root = fixture_root();
    let path = dir.join("export.zip");
    let mut zip = zip::ZipWriter::new(std::fs::File::create(&path).expect("the zip creates"));
    for month in ["1", "2", "3"] {
        let name = format!("listens/2026/{month}.jsonl");
        zip.start_file(name.as_str(), zip::write::SimpleFileOptions::default())
            .expect("an entry starts");
        zip.write_all(&std::fs::read(root.join(&name)).expect("the month file reads"))
            .expect("the entry writes");
    }
    zip.finish().expect("the zip finishes");
    path
}

/// The scoring on the made-up timeline, cut at 2026-02-01 with an
/// eight-week window. Before the cut the person played Aldous; after it
/// Aldous again, Bryn six times, Cato four times (and once in the app, the
/// same play as a listen), Dace five times and Esk five times, Esk after a
/// request for one of its albums. So four new artists were played, three
/// adopted (Bryn, Dace, Esk; Cato's app play is not a fifth), one of them
/// through the app. A set naming Bryn and Cato finds one of them; and an
/// engine that alone found 3 against a baseline that alone found 8 is
/// p = 2 x 232/2048. The made-up Last.fm file reads as the provider rows
/// do, a blank id as none.
#[test]
fn discover_replay_scoring_counts_what_the_person_went_on_to_play() {
    let dir = ScratchDir::new("discover-replay-scoring");
    let mut timeline = Timeline::from_export(&fixture_export(&dir)).expect("the export reads");
    let cato = timeline
        .listens
        .iter()
        .find(|listen| listen.artist_name == "Cato")
        .cloned()
        .expect("Cato is in the timeline");
    timeline.add_app_plays(vec![Listen {
        at_ms: cato.at_ms + 60_000,
        ..cato
    }]);
    let cut = replay::parse_day("2026-02-01").expect("a day");
    let esk = "00000000-0000-4000-8000-00000000000e";
    let asked = [Request {
        artist_mbid: esk.to_owned(),
        at_ms: replay::parse_day("2026-02-10").expect("a day"),
    }];
    let truth = replay::truth(&timeline, cut, 8 * WEEK_MS, &asked);

    assert_eq!(timeline.same_as_a_listen, 1, "the app play is the listen");
    assert_eq!(truth.ids(1).len(), 4, "new artists played");
    assert_eq!(truth.ids(ADOPTED).len(), 3, "adopted at five plays");
    let through_app: Vec<&str> = truth
        .at_least(ADOPTED)
        .filter(|found| found.through_app)
        .map(|found| found.name.as_str())
        .collect();
    assert_eq!(through_app, ["Esk"], "found through the app");

    let pick = |artist: &str| Scored {
        release_group: format!("rg-{artist}"),
        artist: artist.to_owned(),
        artist_listeners: Some(1_000),
        strongest_seed: None,
    };
    let set = [
        pick("00000000-0000-4000-8000-00000000000b"),
        pick("00000000-0000-4000-8000-00000000000c"),
    ];
    let heard = timeline.heard_by(cut);
    let score = replay::score(&set, &truth, &heard, &HashSet::new(), None);
    assert_eq!(score.adopted.len(), 1, "Bryn, adopted; Cato, played only");
    assert_eq!(score.new_played.len(), 2);

    assert!((replay::sign_test(3, 8) - 0.226_562_5).abs() < 1e-9);

    let lastfm = lastfm_rows(&fixture_root().join("lastfm_top_artists.json"));
    let ids: Vec<Option<&str>> = lastfm.iter().map(|row| row.mbid.as_deref()).collect();
    assert_eq!(ids[2], None, "a blank id is none");
    assert_eq!(lastfm[0].listen_count, Some(120));
}
