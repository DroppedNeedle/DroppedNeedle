//! Daily Mixes: one mix of tracks for each of a person's tastes, built once
//! a day by one job kind ([`KIND`], keyed by the user id) from their own
//! rows and kept whole ([`store::keep`]).
//!
//! A mix is for one taste group of the person's taste build, up to
//! [`MIXES_MOST`] of them, largest share first: a group they said was a
//! phase gets none, and one they asked for less from goes after the rest.
//! Each holds up to [`MIX_TRACKS`] tracks, about seven in ten the person's
//! own:
//!
//! - the library tracks by the group's artists, most played first (each
//!   play weighs half as much every [`HALF_LIFE_MS`]);
//! - while sound analysis is on, library tracks by other artists that sound
//!   like the group's: nearest the mean sound of the group's library
//!   tracks, weighted the same way with at most [`CENTRE_PER_ALBUM`] an
//!   album, under the similar-tracks cut-off. Only library tracks are ever
//!   found by sound;
//! - while a source an admin set up plays what the library lacks, tracks
//!   from albums the person's pool proposed for the group (none the library
//!   holds or is fetching, nothing they hid or put off), each with its
//!   reason from the pool's evidence, while the pool is as new as the taste
//!   build. Which tracks come from the artist's popular songs on
//!   ListenBrainz, read with the person's own link at background priority
//!   through the cache, for at most [`SONG_ARTISTS`] artists a run; a call
//!   that goes out is counted against the day's background reads and none
//!   is sent once the day has no room. Without a link there are none.
//!
//! No two versions of a song and at most [`PER_ARTIST`] tracks by one
//! artist. The day's shuffle picks among the strongest, so a mix turns over
//! from day to day and one day's rows always give the same mixes. The
//! person's own tracks are ordered by energy (loudness, then the sound
//! tags) along one arc, [`ARC`]; each new track follows a track by the
//! artist its album was proposed from. A mix is named by the group's most
//! played artists on it.
//!
//! A run reads only stored rows besides those songs. It keeps nothing once
//! the person forgot their history after it began. It is safe to repeat.

pub mod build;
pub mod store;

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use droppedneedle_kernel::clock::Clock;
use droppedneedle_kernel::db::Db;
use droppedneedle_kernel::jobs::BoxFuture;
use droppedneedle_kernel::jobs::queue::{JobCtx, JobError, JobHandler, JobKind, KindSpec, Outcome};
use droppedneedle_kernel::runtime_config::Live;
use droppedneedle_kernel::runtime_config::secret_sections::YouTubeConnection;
use droppedneedle_library::sound::{SoundError, SoundIndex, SoundSearch, SoundTrack};
use droppedneedle_library::tags::loudness::BlockSummary;
use rusqlite::Transaction;

use self::build::{BuiltMix, LibraryTrack, NewTrack, TasteInputs};
use self::store::Group;
use crate::discover::cache::ForgetCounts;
use crate::discover::engine::taste_store::Kept;
use crate::discover::engine::{pool_store, reason};
use crate::discover::facts::{KeptFacts, MeteredSource, plan};
use crate::discover::feedback::Feedback;
use crate::discover::sql;
use crate::metadata::Catalog;
use crate::metadata::models::TopSong;

/// The mixes job kind.
pub const KIND: JobKind = JobKind::new("discover.mixes");

/// How mixes jobs run: local reads, a few cached song lists, one write a
/// person; done rows kept a day, since each person runs about once a day.
pub const SPEC: KindSpec = KindSpec::new(KIND)
    .max_attempts(3)
    .run_timeout(Duration::from_secs(5 * 60))
    .keep_done(Duration::from_secs(24 * 3600))
    .keep_failed(Duration::from_secs(7 * 24 * 3600));

/// The most mixes a person has: one for each of their largest tastes.
pub const MIXES_MOST: usize = 6;
/// The most tracks in a mix.
pub const MIX_TRACKS: usize = 40;
/// About how much of a mix is new: three in ten.
pub const NEW_SHARE: f64 = 0.3;
/// The most new tracks in a mix: three in ten of a full one.
pub const NEW_MOST: usize = 12;
/// The most tracks found by sound in a mix.
pub const SOUND_MOST: usize = 12;
/// The most tracks by one artist in a mix, as in a similar-tracks list.
pub const PER_ARTIST: usize = droppedneedle_library::sound::search::PER_ARTIST;
/// A mix needs at least this many of the person's own tracks.
pub const MIN_YOURS: usize = 8;
/// The most new tracks by one artist in a mix.
pub const NEW_PER_ARTIST: usize = 2;
/// The most new tracks from one album in a mix.
pub const NEW_PER_ALBUM: usize = 2;
/// The pool's albums whose songs are read for one mix.
pub const NEW_ALBUMS: usize = 6;
/// A kind's strongest this many times what it gives are shuffled for the
/// day.
pub const WINDOW: usize = 2;
/// How long until a play weighs half as much, ms.
pub const HALF_LIFE_MS: i64 = 30 * 86_400_000;
/// A day, ms.
const DAY_MS: i64 = 86_400_000;
/// How far back plays are read, ms: a play older weighs next to nothing.
pub const PLAYS_SINCE_MS: i64 = 365 * 86_400_000;
/// What a library track the person never played weighs.
pub const UNPLAYED_WEIGHT: f64 = 0.1;
/// The most tracks of one album in a taste's sound.
pub const CENTRE_PER_ALBUM: usize = 3;
/// The artist's popular songs read for a new album: as many as the artist
/// page asks for, so the two share the cache.
pub const TOP_SONGS: u32 = 10;
/// The ListenBrainz calls one read of an artist's popular songs makes
/// when it is not held: the list, and one for its recordings' albums.
pub const SONG_READ_CALLS: u64 = 2;
/// The most artists whose popular songs one run reads.
pub const SONG_ARTISTS: usize = 20;
/// Sound tags that mark a calm track, a second key after loudness.
pub const CALM_TAGS: [&str; 4] = ["Mellow", "chill", "chillout", "ambient"];
/// Sound tags that mark a lively track.
pub const LIVELY_TAGS: [&str; 4] = ["party", "dance", "House", "metal"];

/// The energy a mix runs along, from 0 (its quietest) to 1 (its loudest).
#[derive(Debug, Clone, Copy)]
pub struct EnergyArc {
    /// Where it starts.
    pub start: f64,
    /// Its top.
    pub peak: f64,
    /// How far through the mix the top comes, from 0 to 1.
    pub peak_at: f64,
    /// Where it settles at the end.
    pub end: f64,
}

/// The one arc: from the middle up to the top two thirds of the way, then
/// down to settle.
pub const ARC: EnergyArc = EnergyArc {
    start: 0.5,
    peak: 1.0,
    peak_at: 2.0 / 3.0,
    end: 0.2,
};

/// What the mixes job is built from.
pub struct MixInputs {
    /// The database.
    pub db: Db,
    /// The kernel clock.
    pub clock: Arc<dyn Clock>,
    /// The forget counts every page cache shares.
    pub forgets: ForgetCounts,
    /// How the library's tracks sound.
    pub sound: SoundSearch,
    /// The catalog, for an artist's popular songs.
    pub catalog: Catalog,
    /// The day's background read counter.
    pub facts: KeptFacts,
    /// The YouTube settings: whether a source plays what the library lacks.
    pub youtube: Live<YouTubeConnection>,
    /// Where the app playlists' refresh a new day's mixes make due is
    /// queued.
    pub due: Arc<crate::discover::due::Due>,
}

/// Runs [`KIND`] jobs.
#[derive(Clone)]
pub struct MixJobs {
    inner: Arc<MixInputs>,
}

/// Why a run kept nothing.
#[derive(Debug, thiserror::Error)]
pub enum MixError {
    /// Settings it reads could not be read: the build waits for them.
    #[error("the {0} settings cannot be read")]
    Settings(&'static str),
    /// The sound search failed.
    #[error(transparent)]
    Sound(#[from] SoundError),
    /// The database or the job's lease.
    #[error(transparent)]
    Job(#[from] JobError),
}

impl From<droppedneedle_kernel::db::DbError> for MixError {
    fn from(error: droppedneedle_kernel::db::DbError) -> Self {
        Self::Job(JobError::from(error))
    }
}

/// The UTC day of `now_ms`, `YYYY-MM-DD`.
fn day_of(now_ms: i64) -> String {
    let format = time::macros::format_description!("[year]-[month]-[day]");
    time::OffsetDateTime::from_unix_timestamp(now_ms.div_euclid(1000))
        .ok()
        .and_then(|at| at.date().format(&format).ok())
        .unwrap_or_default()
}

/// How loud a library track is, in LUFS: as measured for its current
/// bytes, else from its ReplayGain track gain.
fn loudness(row: &sql::MixLibraryTrack) -> Option<f64> {
    row.measured_blocks
        .as_deref()
        .and_then(|blocks| BlockSummary::decode(blocks).ok())
        .and_then(|summary| summary.loudness())
        .or_else(|| row.track_gain.map(|gain| -18.0 - gain))
}

/// Which way a track's strongest sound tags lean.
fn lean(index: Option<&SoundIndex>, track_id: &str) -> i8 {
    let Some(track) = index.and_then(|index| index.track(track_id)) else {
        return 0;
    };
    let names = track.top_tag_names();
    let calm = names.iter().filter(|tag| CALM_TAGS.contains(tag)).count();
    let lively = names.iter().filter(|tag| LIVELY_TAGS.contains(tag)).count();
    match calm.cmp(&lively) {
        std::cmp::Ordering::Greater => -1,
        std::cmp::Ordering::Less => 1,
        std::cmp::Ordering::Equal => 0,
    }
}

fn library_track(
    row: &sql::MixLibraryTrack,
    weights: &HashMap<String, f64>,
    index: Option<&SoundIndex>,
) -> LibraryTrack {
    let loudness = loudness(row);
    let lean = lean(index, &row.id);
    let artist_mbid = [&row.artist_mbid, &row.album_artist_mbid]
        .into_iter()
        .find(|mbid| !mbid.is_empty())
        .cloned();
    LibraryTrack {
        weight: weights.get(&row.id).copied().unwrap_or(UNPLAYED_WEIGHT),
        id: row.id.clone(),
        title: row.title.clone(),
        artist_name: row.artist_name.clone(),
        artist_mbid,
        album_id: row.album_id.clone(),
        album_title: row.album_title.clone(),
        release_group_mbid: row.release_group_mbid.clone(),
        recording_mbid: row
            .recording_mbid
            .as_deref()
            .map(|id| id.trim().to_ascii_lowercase())
            .filter(|id| !id.is_empty()),
        loudness,
        lean,
    }
}

/// Each track's weight from `plays` at the start of the UTC day
/// `day_start_ms`: [`UNPLAYED_WEIGHT`] plus one for each play before it,
/// halved every [`HALF_LIFE_MS`] since, so every build of one day weighs
/// the same.
fn weights(plays: &[(String, i64)], day_start_ms: i64) -> HashMap<String, f64> {
    let mut weights: HashMap<String, f64> = HashMap::new();
    for (track, at) in plays.iter().filter(|(_, at)| *at < day_start_ms) {
        let age = (day_start_ms - at) as f64;
        *weights.entry(track.clone()).or_insert(UNPLAYED_WEIGHT) +=
            0.5f64.powf(age / HALF_LIFE_MS as f64);
    }
    weights
}

/// Tracks by an artist the person said was a phase weigh as if never
/// played.
fn unweigh_phases(lists: &mut [Vec<LibraryTrack>], phase_artists: &HashSet<String>) {
    for track in lists.iter_mut().flatten() {
        if track
            .artist_mbid
            .as_ref()
            .is_some_and(|artist| phase_artists.contains(artist))
        {
            track.weight = UNPLAYED_WEIGHT;
        }
    }
}

/// The groups that get a mix, in page order: a phase gets none, the
/// [`MIXES_MOST`] largest shares get one, and among those a group the
/// person asked for less from goes after the rest, never out.
fn chosen(groups: Vec<Group>, phase: &HashSet<String>, less: &HashSet<String>) -> Vec<Group> {
    let mut groups: Vec<Group> = groups
        .into_iter()
        .filter(|group| !phase.contains(&group.anchor))
        .collect();
    groups.sort_by(|a, b| {
        b.share
            .total_cmp(&a.share)
            .then_with(|| a.group_no.cmp(&b.group_no))
    });
    groups.truncate(MIXES_MOST);
    // A stable sort keeps the share order within each side.
    groups.sort_by_key(|group| less.contains(&group.anchor));
    groups
}

/// The group's sound centre: its library tracks, at most
/// [`CENTRE_PER_ALBUM`] an album (the heaviest), each with its weight.
fn centre(own: &[LibraryTrack]) -> Vec<(String, f64)> {
    let mut sorted: Vec<&LibraryTrack> = own.iter().collect();
    sorted.sort_by(|a, b| b.weight.total_cmp(&a.weight).then_with(|| a.id.cmp(&b.id)));
    let mut per_album: HashMap<&str, usize> = HashMap::new();
    sorted
        .into_iter()
        .filter(|track| {
            let count = per_album.entry(track.album_id.as_str()).or_default();
            *count += 1;
            *count <= CENTRE_PER_ALBUM
        })
        .map(|track| (track.id.clone(), track.weight))
        .collect()
}

/// For each group, the library tracks by other artists nearest its sound,
/// nearest first, leaving out artists the person hid.
fn by_sound(
    groups: &[Group],
    own: &[Vec<LibraryTrack>],
    index: &SoundIndex,
    feedback: &Feedback,
) -> Vec<Vec<String>> {
    groups
        .iter()
        .zip(own)
        .map(|(group, own)| {
            let artists: HashSet<&str> = group
                .artists
                .iter()
                .map(|artist| artist.artist_mbid.as_str())
                .collect();
            let own_ids: HashSet<&str> = own.iter().map(|track| track.id.as_str()).collect();
            let skip = |track: &SoundTrack| {
                own_ids.contains(track.track_id.as_str())
                    || track
                        .artist_mbid
                        .as_deref()
                        .is_some_and(|artist| artists.contains(artist))
                    || feedback.hides_artist(track.artist_mbid.as_deref())
            };
            index
                .near(&centre(own), SOUND_MOST * WINDOW, &skip)
                .unwrap_or_default()
                .into_iter()
                .map(|found| found.track_id)
                .collect()
        })
        .collect()
}

impl MixJobs {
    /// The handler over `inputs`.
    pub fn new(inputs: MixInputs) -> Self {
        Self {
            inner: Arc::new(inputs),
        }
    }

    /// Whether a source an admin set up plays tracks the library lacks:
    /// the YouTube player with its search key.
    fn plays_unowned(&self) -> Result<bool, MixError> {
        let youtube = self
            .inner
            .youtube
            .read()
            .ok_or(MixError::Settings("YouTube"))?;
        Ok(youtube.enabled && youtube.api_enabled && !youtube.api_key.expose().trim().is_empty())
    }

    /// The sound index while analysis is on.
    async fn index(&self) -> Result<Option<Arc<SoundIndex>>, MixError> {
        match self.inner.sound.switch() {
            Some(true) => {}
            Some(false) => return Ok(None),
            None => return Err(MixError::Settings("sound analysis")),
        }
        let search = self.inner.sound.clone();
        // Loading the vectors is blocking work.
        match tokio::task::spawn_blocking(move || search.index()).await {
            Ok(index) => Ok(index?),
            Err(error) => Err(MixError::Sound(SoundError::Failed(error.to_string()))),
        }
    }

    /// Build `user_id`'s mixes for today and keep them; `complete` runs
    /// inside the write.
    ///
    /// # Errors
    ///
    /// Unreadable settings, the sound search's or the database's failure.
    pub async fn run_once(
        &self,
        user_id: &str,
        complete: impl FnOnce(&Transaction) -> Result<(), JobError> + Send + 'static,
    ) -> Result<(), MixError> {
        self.run_from(user_id, self.forgets_now(user_id), complete)
            .await
    }

    /// `user_id`'s forget count now, which a run begins from.
    pub(crate) fn forgets_now(&self, user_id: &str) -> u64 {
        self.inner.forgets.of(user_id)
    }

    /// [`Self::run_once`] for a run that began when `user_id`'s forget
    /// count was `forgets_at`.
    pub(crate) async fn run_from(
        &self,
        user_id: &str,
        forgets_at: u64,
        complete: impl FnOnce(&Transaction) -> Result<(), JobError> + Send + 'static,
    ) -> Result<(), MixError> {
        let inner = &self.inner;
        let now_ms = inner.clock.now_ms();
        let day = day_of(now_ms);
        let plays_unowned = self.plays_unowned()?;
        let index = self.index().await?;
        let (groups, said) = store::groups(&inner.db, user_id).await?;
        let groups = chosen(groups, &said.phase_groups, &said.less_groups);
        let mut mixes = Vec::new();
        if !groups.is_empty() {
            mixes = self
                .build(
                    (user_id, &day, now_ms),
                    groups,
                    &said.phase_artists,
                    index.as_deref(),
                    plays_unowned,
                )
                .await?;
        }
        // The day's mixes are what their app playlists play.
        let (due, user) = (Arc::clone(&inner.due), user_id.to_owned());
        let complete = move |tx: &Transaction| {
            due.queue_now(tx, crate::discover::engine::app_lists::KIND, &user)?;
            complete(tx)
        };
        let kept = store::keep(
            &inner.db,
            &inner.forgets,
            (user_id, forgets_at),
            (&day, now_ms),
            mixes,
            complete,
        )
        .await?;
        if kept == Kept::Dropped {
            tracing::debug!("a mixes build outlived a forget or its account; nothing kept");
        }
        Ok(())
    }

    /// The mixes of `groups` for `user_id` on `day`; a phase artist's
    /// plays weigh nothing.
    async fn build(
        &self,
        (user_id, day, now_ms): (&str, &str, i64),
        groups: Vec<Group>,
        phase_artists: &HashSet<String>,
        index: Option<&SoundIndex>,
        plays_unowned: bool,
    ) -> Result<Vec<BuiltMix>, MixError> {
        let db = &self.inner.db;
        let feedback = Feedback::load(db, user_id, now_ms).await?;
        let day_start_ms = now_ms - now_ms.rem_euclid(DAY_MS);
        let weights = weights(
            &store::plays(db, user_id, day_start_ms - PLAYS_SINCE_MS).await?,
            day_start_ms,
        );
        let all_artists: Vec<String> = groups
            .iter()
            .flat_map(|group| {
                group
                    .artists
                    .iter()
                    .map(|artist| artist.artist_mbid.clone())
            })
            .collect();
        let rows = store::artist_tracks(db, &all_artists).await?;
        // A track goes to the group of its own artist, else its album
        // artist's.
        let group_of: HashMap<&str, usize> = groups
            .iter()
            .enumerate()
            .flat_map(|(at, group)| {
                group
                    .artists
                    .iter()
                    .map(move |artist| (artist.artist_mbid.as_str(), at))
            })
            .collect();
        let mut own: Vec<Vec<LibraryTrack>> = vec![Vec::new(); groups.len()];
        for row in rows {
            let at = group_of
                .get(row.artist_mbid.as_str())
                .or_else(|| group_of.get(row.album_artist_mbid.as_str()))
                .copied();
            if let Some(at) = at {
                own[at].push(library_track(&row, &weights, index));
            }
        }
        unweigh_phases(&mut own, phase_artists);
        let sound = match index {
            Some(index) => by_sound(&groups, &own, index, &feedback),
            None => vec![Vec::new(); groups.len()],
        };
        let mut sound = self.sound_tracks(sound, &weights, index).await?;
        unweigh_phases(&mut sound, phase_artists);
        let mut new = vec![Vec::new(); groups.len()];
        if plays_unowned {
            new = self.new_tracks(user_id, now_ms, &groups).await?;
        }
        Ok(groups
            .into_iter()
            .zip(own)
            .zip(sound)
            .zip(new)
            .filter_map(|(((group, own), sound), new)| {
                build::mix(
                    user_id,
                    day,
                    TasteInputs {
                        anchor: group.anchor,
                        artists: group.artists,
                        own,
                        sound,
                        new,
                    },
                )
            })
            .collect())
    }

    /// The library rows of each group's sound finds, in their order.
    async fn sound_tracks(
        &self,
        found: Vec<Vec<String>>,
        weights: &HashMap<String, f64>,
        index: Option<&SoundIndex>,
    ) -> Result<Vec<Vec<LibraryTrack>>, MixError> {
        let ids: Vec<String> = found.iter().flatten().cloned().collect();
        let rows: HashMap<String, sql::MixLibraryTrack> = store::tracks_by_id(&self.inner.db, &ids)
            .await?
            .into_iter()
            .map(|row| (row.id.clone(), row))
            .collect();
        Ok(found
            .into_iter()
            .map(|ids| {
                ids.iter()
                    .filter_map(|id| rows.get(id))
                    .map(|row| library_track(row, weights, index))
                    .collect()
            })
            .collect())
    }

    /// For each group, songs from the albums the person's pool proposed
    /// for it, in pool order, each with its album's reason. None while the
    /// pool is missing or older than the taste build, and songs are read
    /// for at most [`SONG_ARTISTS`] artists a run.
    async fn new_tracks(
        &self,
        user_id: &str,
        now_ms: i64,
        groups: &[Group],
    ) -> Result<Vec<Vec<NewTrack>>, MixError> {
        let db = &self.inner.db;
        let mut new = vec![Vec::new(); groups.len()];
        if !store::pool_current(db, user_id).await? {
            return Ok(new);
        }
        let pool = pool_store::ranked(db, user_id, now_ms).await?;
        let seeds = pool_store::seed_ids(db, user_id).await?;
        let mut songs: HashMap<String, Vec<TopSong>> = HashMap::new();
        for (group, tracks) in groups.iter().zip(new.iter_mut()) {
            let albums = pool
                .iter()
                .filter(|candidate| candidate.group == Some(group.group_no))
                .take(NEW_ALBUMS);
            for candidate in albums {
                let Some(reason) = reason::reason(candidate, &seeds) else {
                    continue;
                };
                let artist = candidate.artist_mbid.clone();
                if !songs.contains_key(&artist) {
                    if songs.len() >= SONG_ARTISTS {
                        continue;
                    }
                    let read = self.songs_of(user_id, &artist).await;
                    songs.insert(artist.clone(), read);
                }
                let seed = candidate.strongest_seed().map(str::to_owned);
                for song in songs.get(&artist).into_iter().flatten() {
                    let on_album = song.release_group_mbid.as_deref().is_some_and(|group| {
                        group.eq_ignore_ascii_case(&candidate.release_group_mbid)
                    });
                    if !on_album {
                        continue;
                    }
                    tracks.push(NewTrack {
                        title: song.title.clone(),
                        artist_name: candidate.artist_name.clone(),
                        artist_mbid: candidate.artist_mbid.clone(),
                        recording_mbid: song.recording_mbid.clone(),
                        release_group_mbid: candidate.release_group_mbid.clone(),
                        album_title: candidate.title.clone(),
                        reason: reason.clone(),
                        seed: seed.clone(),
                    });
                }
            }
        }
        Ok(new)
    }

    /// The artist's popular songs on ListenBrainz with the person's own
    /// link, at background priority through the cache. A read that is not
    /// held fresh goes out only while the day's background reads have room
    /// for it, and the calls it sent are counted.
    async fn songs_of(&self, user_id: &str, artist: &str) -> Vec<TopSong> {
        let inner = &self.inner;
        let may_send = match plan::room(&inner.facts).await {
            Ok(room) => room >= SONG_READ_CALLS,
            Err(error) => {
                tracing::warn!(%error, "the day's reads unreadable; only held songs for a mix");
                false
            }
        };
        let found = inner
            .catalog
            .background_top_songs(user_id, artist, TOP_SONGS, may_send)
            .await;
        match found {
            Ok((songs, sent)) => {
                if sent > 0 {
                    inner
                        .facts
                        .count_read(MeteredSource::ListenBrainz, sent)
                        .await;
                }
                songs
            }
            Err(error) => {
                tracing::debug!(%error, "an artist's songs unavailable for a mix");
                Vec::new()
            }
        }
    }
}

impl JobHandler for MixJobs {
    fn run(&self, job: JobCtx) -> BoxFuture<'static, Outcome> {
        let runner = self.clone();
        Box::pin(async move {
            let user_id = job.key().to_owned();
            match runner.run_once(&user_id, move |tx| job.complete(tx)).await {
                Ok(()) => Outcome::Done,
                Err(MixError::Job(error)) if error.is_busy() => Outcome::Busy,
                Err(MixError::Sound(SoundError::Busy(_))) => Outcome::Busy,
                // Another worker holds the job: its run keeps the rows.
                Err(MixError::Job(JobError::LeaseLost)) => Outcome::Done,
                Err(MixError::Settings(_)) => Outcome::Retry {
                    code: "DISCOVER_SETTINGS_UNREADABLE",
                    at: None,
                },
                Err(error) => {
                    tracing::warn!(%error, "a person's mixes not kept; trying again");
                    Outcome::Retry {
                        code: "DISCOVER_MIXES_FAILED",
                        at: None,
                    }
                }
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn group(group_no: i64, share: f64) -> Group {
        Group {
            group_no,
            anchor: format!("anchor-{group_no}"),
            share,
            artists: Vec::new(),
        }
    }

    /// Of seven groups the six largest get a mix; one the person asked for
    /// less from comes after the others but keeps its place among the
    /// six, and a phase gets none.
    #[test]
    fn less_moves_a_group_down_and_never_out() {
        let groups: Vec<Group> = (0..7).map(|n| group(n, 1.0 - n as f64 / 10.0)).collect();
        let less: HashSet<String> = ["anchor-0".to_owned()].into();
        let order = |groups: Vec<Group>| -> Vec<i64> {
            groups.iter().map(|group| group.group_no).collect()
        };
        assert_eq!(
            order(chosen(groups.clone(), &HashSet::new(), &less)),
            [1, 2, 3, 4, 5, 0]
        );
        let phase: HashSet<String> = ["anchor-2".to_owned()].into();
        assert_eq!(order(chosen(groups, &phase, &less)), [1, 3, 4, 5, 6, 0]);
    }
}
