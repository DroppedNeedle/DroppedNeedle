//! Finding the library tracks that sound most like one track.
//!
//! Every analysed track's vector at the serving analysis version sits in
//! one contiguous array, normalised, loaded once and loaded again only when
//! the analysis changed ([`SoundMark`]): never on the database's commit
//! counter, since a load is up to 40 MB for a large library. A search scans
//! the whole array: there is no index. Cosine distance on the stored
//! vectors, 0 for the same sound and 2 for opposite ones.
//!
//! A list keeps one version of each song and spreads over artists: other
//! versions of the seed (the same MusicBrainz recording, or the same title
//! by the same artist) go first, then a track that sounds the same as the
//! one kept before it, unless the two are different identified pieces.
//! At most [`PER_ARTIST`] a list, and nothing past [`CUT_OFF`]: a thin
//! library gives a short list, never one topped up with far tracks. A
//! path from one track to another ([`SoundIndex::path`]) keeps the same
//! rules at each step along the arc between their sounds.
//!
//! The search knows only the library. It knows nothing of a person.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Instant;

use droppedneedle_kernel::db::{CancelFlag, Db};
use droppedneedle_kernel::runtime_config::Live;
use droppedneedle_kernel::runtime_config::sections::SoundAnalysisSettings;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use super::net::{EMBEDDING, TAG_COUNT, TAGS};
use super::{ANALYSIS_VERSION, SoundError, SoundMark, sql};
use crate::scan::naming::artist_name_key;

/// The furthest a track may sound from the seed and still be listed.
/// Measured on dev's library (S4-01): the tenth neighbour's median is
/// 0.085, and 91 per cent of tracks have ten neighbours within it.
pub const CUT_OFF: f32 = 0.15;
/// Below this a track sounds very close: a third of the cut-off.
pub const VERY_CLOSE_UNDER: f32 = CUT_OFF / 3.0;
/// Below this a track sounds close: two thirds of the cut-off.
pub const CLOSE_UNDER: f32 = CUT_OFF * 2.0 / 3.0;
/// Two tracks this near sound the same: the second is another copy.
const SAME_SOUND: f32 = 0.01;
/// Most tracks by one artist in one list.
pub const PER_ARTIST: usize = 3;
/// The nearest tracks each waypoint of a path chooses among.
pub const PATH_CHOICE: usize = 25;
/// A track's strongest tags, the ones two tracks can share.
pub const TOP_TAGS: usize = 5;

/// How close a track sounds to the seed, in words.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum Closeness {
    /// Under a third of the cut-off.
    VeryClose,
    /// Under two thirds of the cut-off.
    Close,
    /// Up to the cut-off.
    Further,
}

impl Closeness {
    /// The word for `distance`.
    #[must_use]
    pub fn of(distance: f32) -> Self {
        if distance < VERY_CLOSE_UNDER {
            Self::VeryClose
        } else if distance < CLOSE_UNDER {
            Self::Close
        } else {
            Self::Further
        }
    }
}

/// The index loaded for search, with the mark it was loaded at.
type Loaded = (SoundMark, Arc<SoundIndex>);

/// What the sound bench is first tuned to.
#[derive(Debug, Clone, PartialEq)]
pub struct BenchSeed {
    /// The analysis version serving now.
    pub analysis_version: i64,
    /// Whether any track has a sound at that version.
    pub analysed: bool,
    /// The most played analysed track, when there is one.
    pub seed: Option<BenchSeedTrack>,
}

/// The library track the sound bench is first tuned to.
#[derive(Debug, Clone, PartialEq)]
pub struct BenchSeedTrack {
    /// The local track id.
    pub track_id: String,
    pub title: String,
    pub artist_name: String,
    /// The local album id.
    pub album_id: String,
    pub album_title: Option<String>,
    /// Its length in seconds, when known.
    pub duration_seconds: Option<f64>,
}

/// One track a search found.
#[derive(Debug, Clone, PartialEq)]
pub struct Neighbour {
    /// The library track.
    pub track_id: String,
    /// Its cosine distance from the seed.
    pub distance: f32,
    /// That distance in words.
    pub closeness: Closeness,
    /// The tags among both tracks' strongest, in the seed's order.
    pub shared_tags: Vec<&'static str>,
}

/// What a search knows of one analysed track besides its sound.
#[derive(Debug, Clone)]
pub struct SoundTrack {
    /// The library track.
    pub track_id: String,
    /// Its title.
    pub title: String,
    /// Its artist's name.
    pub artist_name: String,
    /// Its first credited artist's MusicBrainz id, lowercased, when known.
    pub artist_mbid: Option<String>,
    /// Its album, the library's id.
    pub album_id: String,
    /// The album's title.
    pub album_title: Option<String>,
    /// Its length in seconds, when known.
    pub duration_seconds: Option<f64>,
    /// Its recording's MusicBrainz id, lowercased, when known.
    recording: Option<String>,
    /// Its title folded for comparing versions.
    title_key: String,
    /// Its first credited artist: the library artist, else the name key.
    artist_key: String,
    /// Its strongest tags, strongest first.
    top_tags: Vec<u8>,
}

impl SoundTrack {
    /// Its first credited artist, as the per-artist rules count it.
    #[must_use]
    pub fn artist_key(&self) -> &str {
        &self.artist_key
    }

    /// The names of its strongest tags, strongest first.
    #[must_use]
    pub fn top_tag_names(&self) -> Vec<&'static str> {
        self.top_tags
            .iter()
            .filter_map(|tag| TAGS.get(usize::from(*tag)).copied())
            .collect()
    }

    /// Whether this and `other` are versions of one song: the same
    /// recording, or the same title by the same artist.
    fn same_song(&self, other: &Self) -> bool {
        match (&self.recording, &other.recording) {
            (Some(a), Some(b)) if a == b => return true,
            _ => {}
        }
        !self.title_key.is_empty()
            && self.title_key == other.title_key
            && self.artist_key == other.artist_key
    }

    /// The names of the tags among both tracks' strongest, in this one's
    /// order.
    fn shared_tags(&self, other: &Self) -> Vec<&'static str> {
        self.top_tags
            .iter()
            .filter(|tag| other.top_tags.contains(tag))
            .filter_map(|tag| TAGS.get(usize::from(*tag)).copied())
            .collect()
    }
}

/// A title folded for comparing versions: lowercased, cut before a
/// bracket or a " - " (a remaster, a live take), letters and digits only.
#[must_use]
pub fn title_key(title: &str) -> String {
    let lower = title.to_lowercase();
    let cut = [lower.find('('), lower.find('['), lower.find(" - ")]
        .into_iter()
        .flatten()
        .min()
        .unwrap_or(lower.len());
    lower[..cut]
        .split(|ch: char| !ch.is_alphanumeric())
        .filter(|word| !word.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

/// The indexes of the [`TOP_TAGS`] highest scores, highest first.
fn top_tags(scores: &[u8]) -> Vec<u8> {
    let mut order: Vec<u8> = (0..TAG_COUNT as u8).collect();
    order.sort_by_key(|tag| (std::cmp::Reverse(scores[usize::from(*tag)]), *tag));
    order.truncate(TOP_TAGS);
    order
}

/// Every analysed track's sound at one version, normalised in one array.
pub struct SoundIndex {
    analysis_version: i64,
    vectors: Vec<f32>,
    tracks: Vec<SoundTrack>,
    at: HashMap<String, usize>,
}

impl std::fmt::Debug for SoundIndex {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SoundIndex")
            .field("analysis_version", &self.analysis_version)
            .field("tracks", &self.tracks.len())
            .finish()
    }
}

impl SoundIndex {
    fn build(analysis_version: i64, rows: Vec<sql::SearchTrack>) -> Self {
        let mut vectors = Vec::with_capacity(rows.len() * EMBEDDING);
        let mut tracks = Vec::with_capacity(rows.len());
        let mut at = HashMap::with_capacity(rows.len());
        for row in rows {
            let Some(vector) = super::decode_vector(&row.vector) else {
                tracing::warn!(
                    track_id = row.track_id,
                    "a stored track sound does not read; it is left out"
                );
                continue;
            };
            let norm = vector.iter().map(|value| value * value).sum::<f32>().sqrt();
            if row.tags.len() != TAG_COUNT || !norm.is_normal() {
                tracing::warn!(
                    track_id = row.track_id,
                    "a stored track sound cannot be compared; it is left out"
                );
                continue;
            }
            vectors.extend(vector.iter().map(|value| value / norm));
            at.insert(row.track_id.clone(), tracks.len());
            let artist_key = match &row.artist_id {
                Some(id) => format!("artist:{id}"),
                None => format!("name:{}", artist_name_key(&row.artist_name)),
            };
            tracks.push(SoundTrack {
                title_key: title_key(&row.title),
                artist_key,
                recording: row
                    .recording_mbid
                    .map(|id| id.trim().to_ascii_lowercase())
                    .filter(|id| !id.is_empty()),
                artist_mbid: row
                    .artist_mbid
                    .map(|id| id.trim().to_ascii_lowercase())
                    .filter(|id| !id.is_empty()),
                top_tags: top_tags(&row.tags),
                track_id: row.track_id,
                title: row.title,
                artist_name: row.artist_name,
                album_id: row.album_id,
                album_title: row.album_title,
                duration_seconds: row.duration_seconds,
            });
        }
        Self {
            analysis_version,
            vectors,
            tracks,
            at,
        }
    }

    /// The analysis version it was read at.
    #[must_use]
    pub fn analysis_version(&self) -> i64 {
        self.analysis_version
    }

    /// How many tracks it holds.
    #[must_use]
    pub fn len(&self) -> usize {
        self.tracks.len()
    }

    /// Whether it holds no track.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.tracks.is_empty()
    }

    /// One analysed track, when it is in the index.
    #[must_use]
    pub fn track(&self, track_id: &str) -> Option<&SoundTrack> {
        self.at.get(track_id).map(|at| &self.tracks[*at])
    }

    fn vector(&self, at: usize) -> &[f32] {
        &self.vectors[at * EMBEDDING..(at + 1) * EMBEDDING]
    }

    fn between(&self, a: usize, b: usize) -> f32 {
        let dot: f32 = self
            .vector(a)
            .iter()
            .zip(self.vector(b))
            .map(|(x, y)| x * y)
            .sum();
        (1.0 - dot).max(0.0)
    }

    /// The distance between two analysed tracks.
    #[must_use]
    pub fn distance(&self, a: &str, b: &str) -> Option<f32> {
        Some(self.between(*self.at.get(a)?, *self.at.get(b)?))
    }

    /// The tags among both tracks' strongest, in `a`'s order.
    #[must_use]
    pub fn shared_tags(&self, a: &str, b: &str) -> Vec<&'static str> {
        match (self.track(a), self.track(b)) {
            (Some(a), Some(b)) => a.shared_tags(b),
            _ => Vec::new(),
        }
    }

    /// Up to `n` tracks that sound like `seed`, nearest first, by the
    /// rules at the top of this file. A track `skip` answers true for (one
    /// the station has already queued) is never listed and counts toward
    /// neither `n` nor the artist cap, but still keeps its other copies and
    /// near copies out. `None` when `seed` is not analysed.
    #[must_use]
    pub fn similar(
        &self,
        seed: &str,
        n: usize,
        skip: &dyn Fn(&SoundTrack) -> bool,
    ) -> Option<Vec<Neighbour>> {
        let seed_at = *self.at.get(seed)?;
        let nearest = (0..self.tracks.len())
            .filter(|at| *at != seed_at)
            .map(|at| (self.between(seed_at, at), at))
            .collect();
        Some(self.listed(nearest, Some(seed_at), n, skip))
    }

    /// Up to `n` tracks that sound like the weighted mean of `members`
    /// (track ids and their weights), nearest first, by the same rules as
    /// [`Self::similar`]: one sound for a set of tracks, such as a person's
    /// taste. Members are listed like any other track unless `skip` says
    /// not. A member that is not analysed or weighs nothing counts for
    /// nothing; `None` when none counts.
    #[must_use]
    pub fn near(
        &self,
        members: &[(String, f64)],
        n: usize,
        skip: &dyn Fn(&SoundTrack) -> bool,
    ) -> Option<Vec<Neighbour>> {
        let mut centre = vec![0.0f64; EMBEDDING];
        let mut counted = false;
        for (track_id, weight) in members {
            let Some(at) = self.at.get(track_id) else {
                continue;
            };
            if !weight.is_normal() || *weight < 0.0 {
                continue;
            }
            counted = true;
            for (sum, value) in centre.iter_mut().zip(self.vector(*at)) {
                *sum += weight * f64::from(*value);
            }
        }
        let norm = centre.iter().map(|value| value * value).sum::<f64>().sqrt();
        if !counted || !norm.is_normal() {
            return None;
        }
        let centre: Vec<f32> = centre.iter().map(|value| (value / norm) as f32).collect();
        let nearest = (0..self.tracks.len())
            .map(|at| {
                let dot: f32 = centre.iter().zip(self.vector(at)).map(|(x, y)| x * y).sum();
                ((1.0 - dot).max(0.0), at)
            })
            .collect();
        Some(self.listed(nearest, None, n, skip))
    }

    /// The tracks of `nearest` (distances and places) a list keeps, by the
    /// rules at the top of this file; `seed` is the seed track's place when
    /// the list grows from one track.
    fn listed(
        &self,
        mut nearest: Vec<(f32, usize)>,
        seed: Option<usize>,
        n: usize,
        skip: &dyn Fn(&SoundTrack) -> bool,
    ) -> Vec<Neighbour> {
        let seed_track = seed.map(|at| &self.tracks[at]);
        // Enough that the artist cap and the versions dropped still leave
        // `n` (S4-01: the cap dropped up to 28 of 41 for one seed).
        let skipped: Vec<bool> = self.tracks.iter().map(skip).collect();
        let ask = n + (3 * n).max(20) + 1 + skipped.iter().filter(|skip| **skip).count();
        let by_distance = |a: &(f32, usize), b: &(f32, usize)| {
            a.0.total_cmp(&b.0)
                .then_with(|| self.tracks[a.1].track_id.cmp(&self.tracks[b.1].track_id))
        };
        if nearest.len() > ask {
            nearest.select_nth_unstable_by(ask, by_distance);
            nearest.truncate(ask);
        }
        nearest.sort_by(by_distance);
        let mut kept: Vec<usize> = Vec::new();
        let mut per_artist: HashMap<&str, usize> = HashMap::new();
        let mut found = Vec::new();
        for (distance, at) in nearest {
            if found.len() >= n || distance > CUT_OFF {
                break;
            }
            let track = &self.tracks[at];
            if seed_track.is_some_and(|seed| track.same_song(seed))
                || kept
                    .iter()
                    .any(|other| track.same_song(&self.tracks[*other]))
            {
                continue;
            }
            if let Some(previous) = kept.last().copied().or(seed) {
                let other = &self.tracks[previous];
                let different_pieces = track.recording.is_some() && other.recording.is_some();
                if self.between(at, previous) < SAME_SOUND && !different_pieces {
                    continue;
                }
            }
            if skipped[at] {
                // Kept, not listed: its copies stay out.
                kept.push(at);
                continue;
            }
            let count = per_artist.entry(track.artist_key.as_str()).or_default();
            if *count >= PER_ARTIST {
                continue;
            }
            *count += 1;
            kept.push(at);
            found.push(Neighbour {
                track_id: track.track_id.clone(),
                distance,
                closeness: Closeness::of(distance),
                shared_tags: seed_track.map_or_else(Vec::new, |seed| seed.shared_tags(track)),
            });
        }
        found
    }

    /// Up to `count` tracks leading from `start` to `end`, both included,
    /// `start` first and `end` last. Between them each waypoint is a step
    /// along the arc from one sound to the other (spherical interpolation
    /// of the two vectors), and takes the nearest of its [`PATH_CHOICE`]
    /// nearest tracks that keeps the rules at the top of this file: one
    /// version of a song, no near copy of the track before, at most
    /// [`PER_ARTIST`] by one artist between the ends, nothing past
    /// [`CUT_OFF`] from its waypoint. A waypoint with no such track is
    /// passed, so a thin library gives a shorter path. A track `skip`
    /// answers true for is never taken. `None` when either end is not
    /// analysed.
    #[must_use]
    pub fn path(
        &self,
        start: &str,
        end: &str,
        count: usize,
        skip: &dyn Fn(&SoundTrack) -> bool,
    ) -> Option<Vec<String>> {
        let (start_at, end_at) = (*self.at.get(start)?, *self.at.get(end)?);
        if start_at == end_at {
            return Some(vec![start.to_owned()]);
        }
        let waypoints = count.saturating_sub(2);
        let from = self.vector(start_at);
        let to = self.vector(end_at);
        let dot: f32 = from.iter().zip(to).map(|(a, b)| a * b).sum();
        let angle = dot.clamp(-1.0, 1.0).acos();
        let mut picked: Vec<usize> = vec![start_at];
        let mut taken = vec![false; self.tracks.len()];
        taken[start_at] = true;
        let mut per_artist: HashMap<&str, usize> = HashMap::new();
        for step in 1..=waypoints {
            let t = step as f32 / (waypoints + 1) as f32;
            let (a, b) = if angle.sin().abs() < 1e-6 {
                (1.0 - t, t)
            } else {
                (
                    ((1.0 - t) * angle).sin() / angle.sin(),
                    (t * angle).sin() / angle.sin(),
                )
            };
            let mut point: Vec<f32> = from.iter().zip(to).map(|(x, y)| a * x + b * y).collect();
            let norm = point.iter().map(|value| value * value).sum::<f32>().sqrt();
            if !norm.is_normal() {
                continue;
            }
            point.iter_mut().for_each(|value| *value /= norm);
            let mut nearest: Vec<(f32, usize)> = (0..self.tracks.len())
                .filter(|at| *at != end_at && !taken[*at])
                .map(|at| {
                    let dot: f32 = point.iter().zip(self.vector(at)).map(|(x, y)| x * y).sum();
                    ((1.0 - dot).max(0.0), at)
                })
                .collect();
            let by_distance = |a: &(f32, usize), b: &(f32, usize)| {
                a.0.total_cmp(&b.0)
                    .then_with(|| self.tracks[a.1].track_id.cmp(&self.tracks[b.1].track_id))
            };
            if nearest.len() > PATH_CHOICE {
                nearest.select_nth_unstable_by(PATH_CHOICE, by_distance);
                nearest.truncate(PATH_CHOICE);
            }
            nearest.sort_by(by_distance);
            let previous = picked.last().copied().unwrap_or(start_at);
            let chosen = nearest.into_iter().find(|(distance, at)| {
                let track = &self.tracks[*at];
                let other = &self.tracks[previous];
                let different_pieces = track.recording.is_some() && other.recording.is_some();
                *distance <= CUT_OFF
                    && !skip(track)
                    && !picked
                        .iter()
                        .chain([&end_at])
                        .any(|kept| track.same_song(&self.tracks[*kept]))
                    && (self.between(*at, previous) >= SAME_SOUND || different_pieces)
                    && per_artist
                        .get(track.artist_key.as_str())
                        .is_none_or(|count| *count < PER_ARTIST)
            });
            if let Some((_, at)) = chosen {
                *per_artist
                    .entry(self.tracks[at].artist_key.as_str())
                    .or_default() += 1;
                taken[at] = true;
                picked.push(at);
            }
        }
        picked.push(end_at);
        Some(
            picked
                .into_iter()
                .map(|at| self.tracks[at].track_id.clone())
                .collect(),
        )
    }
}

/// The search over the analysed tracks: one index, loaded when first asked
/// for and again only after the analysis changed.
#[derive(Clone)]
pub struct SoundSearch {
    db: Db,
    settings: Live<SoundAnalysisSettings>,
    /// Moved by the analysis loop after every write that kept or dropped
    /// rows.
    changes: Arc<AtomicU64>,
    loaded: Arc<Mutex<Option<Loaded>>>,
    loads: Arc<AtomicU64>,
}

impl SoundSearch {
    /// A search over `db`, answering while `settings` has the switch on.
    #[must_use]
    pub fn new(db: Db, settings: Live<SoundAnalysisSettings>) -> Self {
        Self::sharing(db, settings, Arc::default())
    }

    /// A search that hears the analysis loop's writes through `changes`.
    pub(crate) fn sharing(
        db: Db,
        settings: Live<SoundAnalysisSettings>,
        changes: Arc<AtomicU64>,
    ) -> Self {
        Self {
            db,
            settings,
            changes,
            loaded: Arc::default(),
            loads: Arc::default(),
        }
    }

    /// Whether the switch is on (paused or not): the sound features
    /// answer only then. `None` while the settings cannot be read, when
    /// they answer an honest error rather than act as off.
    #[must_use]
    pub fn switch(&self) -> Option<bool> {
        self.settings
            .read()
            .map(|section| section.sound_analysis_enabled)
    }

    /// Whether the switch is on; settings that cannot be read are a
    /// failure, never off.
    pub fn enabled(&self) -> Result<bool, SoundError> {
        self.switch().ok_or_else(|| {
            SoundError::Failed("the sound analysis settings cannot be read".to_owned())
        })
    }

    /// The sound bench's first tuning for `user_id`: the analysis version
    /// serving now, whether any track has a sound at it, and the library
    /// track with a sound at that version for its current bytes they played
    /// most since `since_ms`. Reads the rows only; the index stays as it is.
    ///
    /// # Errors
    ///
    /// The read's failure.
    pub async fn bench_seed(&self, user_id: &str, since_ms: i64) -> Result<BenchSeed, SoundError> {
        let user_id = user_id.to_owned();
        let row = self
            .db
            .read("library.sound_bench_seed", move |conn| {
                conn.prepare_cached(sql::BENCH_SEED.text())?
                    .query_row(
                        rusqlite::params![user_id, since_ms, ANALYSIS_VERSION],
                        sql::BenchSeedRow::read,
                    )
                    .map_err(SoundError::from)
            })
            .await?;
        let seed = match (row.track_id, row.title, row.album_id) {
            (Some(track_id), Some(title), Some(album_id)) => Some(BenchSeedTrack {
                track_id,
                title,
                artist_name: row.artist_name.unwrap_or_default(),
                album_id,
                album_title: row.album_title,
                duration_seconds: row.duration_seconds,
            }),
            _ => None,
        };
        Ok(BenchSeed {
            analysis_version: row.analysis_version,
            analysed: row.analysed,
            seed,
        })
    }

    /// How many times the vectors were read since the server started.
    #[must_use]
    pub fn loads(&self) -> u64 {
        self.loads.load(Ordering::Acquire)
    }

    /// A cheap mark that changes whenever the analysis can answer
    /// differently: this process kept or dropped rows, a row was added
    /// (its rowid), a commit changed catalog rows (a scan reading a file
    /// again stops its row counting), or the switch moved. Blocking.
    pub fn mark(&self) -> Result<SoundMark, SoundError> {
        let changes = self.changes.load(Ordering::Acquire);
        let mark = self.db.read_blocking("library.sound_mark", |conn| {
            conn.prepare_cached(sql::MARK.text())?
                .query_row([], sql::Mark::read)
                .map_err(SoundError::from)
        })?;
        Ok(SoundMark {
            enabled: self.switch() == Some(true),
            catalog_revision: mark.catalog_revision,
            newest_row: mark.newest_row,
            changes,
        })
    }

    /// Let the loaded index go, as when the switch is turned off; the next
    /// search with it on loads it again.
    pub fn release(&self) {
        *self.loaded.lock().unwrap_or_else(PoisonError::into_inner) = None;
    }

    /// The index now, loaded again only when the mark moved. `None` while
    /// the switch is off, when the index is let go; settings that cannot be
    /// read are a failure. Blocking.
    pub fn index(&self) -> Result<Option<Arc<SoundIndex>>, SoundError> {
        let mut loaded = self.loaded.lock().unwrap_or_else(PoisonError::into_inner);
        if !self.enabled()? {
            *loaded = None;
            return Ok(None);
        }
        // The mark is taken before the read, so a change between the two
        // shows as a new mark next time rather than being missed.
        let mark = self.mark()?;
        if let Some((at, index)) = loaded.as_ref()
            && *at == mark
        {
            return Ok(Some(Arc::clone(index)));
        }
        // Let the old array go before the new one is read; a search still
        // holding it keeps it until it ends.
        *loaded = None;
        let started = Instant::now();
        let (version, rows) = self.db.read_long(
            "library.sound_search_load",
            &CancelFlag::never(),
            // The version and its rows must agree, as in `vectors`.
            |conn| {
                let version: i64 = conn
                    .prepare_cached(sql::SERVING_VERSION.text())?
                    .query_row([ANALYSIS_VERSION], |row| row.get(0))?;
                let rows = conn
                    .prepare_cached(sql::SEARCH_TRACKS.text())?
                    .query_map([version], sql::SearchTrack::read)?
                    .collect::<Result<Vec<_>, _>>()?;
                Ok::<_, SoundError>((version, rows))
            },
        )?;
        let index = Arc::new(SoundIndex::build(version, rows));
        self.loads.fetch_add(1, Ordering::AcqRel);
        tracing::info!(
            tracks = index.len(),
            version,
            millis = started.elapsed().as_millis() as u64,
            "track sounds loaded for search"
        );
        *loaded = Some((mark, Arc::clone(&index)));
        Ok(Some(index))
    }
}
