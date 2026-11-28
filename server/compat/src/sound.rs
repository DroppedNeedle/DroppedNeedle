//! What the apps' similar songs, instant mixes and the OpenSubsonic
//! `sonicSimilarity` extension read: the library's similar-tracks search
//! (`droppedneedle_library::sound`), and a station's walk over it
//! (`droppedneedle_catalog::discover::radio::sound`).
//!
//! The engine answers apps only while sound analysis is switched on and
//! has a sound for nine in ten of the library's tracks
//! ([`CompatSound::serves`]); until then, and for a track with no sound
//! yet, each protocol answers as it did before analysis. Everything is read
//! from memory or the database: no provider is asked, and nothing is
//! recorded for later. A person's "Not this artist" holds, except for the
//! artists of the tracks asked about.

use std::collections::HashSet;
use std::sync::{Arc, Mutex, PoisonError};

use droppedneedle_catalog::discover::feedback::Feedback;
use droppedneedle_catalog::discover::radio::sound::{Seed, Steering, station};
use droppedneedle_kernel::clock::Clock;
use droppedneedle_kernel::db::Db;
use droppedneedle_kernel::runtime_config::Live;
use droppedneedle_kernel::runtime_config::sections::SoundAnalysisSettings;
use droppedneedle_library::sound::{SoundAnalysis, SoundError, SoundIndex, SoundTrack};
use tokio::sync::watch;

use crate::library::CompatError;

/// How long a reading of the analysis' coverage is trusted, ms. The switch
/// itself is read on every call.
const COVERAGE_FOR_MS: i64 = 60_000;
/// How long a coverage read that failed answers "not yet", ms.
const FAILED_FOR_MS: i64 = 10_000;
/// The share of indexed tracks, in per cent, that must have a sound before
/// the engine answers apps.
pub const SERVES_FROM_PERCENT: u64 = 90;

/// One track the engine found, with how alike it sounds to the track it
/// was asked about: `1 - cosine distance`, held to 0..1.
#[derive(Debug, Clone, PartialEq)]
pub struct Match {
    /// The library track.
    pub track_id: String,
    /// 1.0 for the same sound, lower further away.
    pub similarity: f64,
}

/// The number Navidrome passes on for its AudioMuse-AI plugin's similar
/// tracks reads the same: the plugin gives `clamp(1 - distance, 0, 1)` over
/// AudioMuse's cosine distance, `1 - cos`, as ours is (read from the
/// plugin's `normalizeSimilarity` and AudioMuse's index, 2026-11). For a
/// path Navidrome sends -1 ("not supported") on every entry; the spec asks
/// for the similarity to the start song, which this gives.
fn similarity(distance: f32) -> f64 {
    (1.0 - f64::from(distance)).clamp(0.0, 1.0)
}

fn failed(error: SoundError) -> CompatError {
    match error {
        SoundError::Busy(cause) => CompatError::Busy(cause),
        SoundError::Failed(cause) => CompatError::Internal(cause),
    }
}

/// The sound engine as both protocols read it.
#[derive(Clone)]
pub struct CompatSound {
    analysis: SoundAnalysis,
    db: Db,
    clock: Arc<dyn Clock>,
    /// The sound analysis settings' revision, which every save moves.
    revision: watch::Receiver<i64>,
    /// The coverage last read.
    covered: Arc<Mutex<Option<Covered>>>,
    /// Held while the coverage is read, so one read runs at a time.
    reading: Arc<tokio::sync::Mutex<()>>,
}

/// One reading of the analysis' coverage.
#[derive(Debug, Clone, Copy)]
struct Covered {
    /// The settings revision it was read at.
    revision: i64,
    /// When, ms.
    at: i64,
    /// Whether the engine answers apps.
    covers: bool,
    /// The read failed.
    failed: bool,
}

impl CompatSound {
    /// The engine over `analysis` and its `settings`, reading people's
    /// feedback from `db`.
    pub fn new(
        analysis: SoundAnalysis,
        settings: Live<SoundAnalysisSettings>,
        db: Db,
        clock: Arc<dyn Clock>,
    ) -> Self {
        Self {
            analysis,
            db,
            clock,
            revision: settings.subscribe(),
            covered: Arc::default(),
            reading: Arc::default(),
        }
    }

    /// Whether the engine answers apps: the switch is on (paused or not)
    /// and at least [`SERVES_FROM_PERCENT`] per cent of the indexed tracks
    /// have a sound. A track asked about with no sound yet still answers as
    /// before analysis. The switch is read each call; the coverage at most
    /// once every [`COVERAGE_FOR_MS`] while the settings are not saved
    /// again ([`FAILED_FOR_MS`] after a read that failed), one read at a
    /// time, so the public extensions list stays cheap. Settings or counts
    /// that cannot be read answer false, which leaves apps with the answers
    /// they had before analysis.
    pub async fn serves(&self) -> bool {
        if self.analysis.search().switch() != Some(true) {
            return false;
        }
        if let Some(covers) = self.cached() {
            return covers;
        }
        let _reading = self.reading.lock().await;
        // Another call may have read it while this one waited.
        if let Some(covers) = self.cached() {
            return covers;
        }
        let revision = *self.revision.borrow();
        let analysis = self.analysis.clone();
        let read = match tokio::task::spawn_blocking(move || analysis.coverage()).await {
            Ok(Ok(coverage)) => Some(
                coverage.tracks > 0
                    && coverage.analysed * 100 >= coverage.tracks * SERVES_FROM_PERCENT,
            ),
            Ok(Err(error)) => {
                tracing::warn!(%error, "sound coverage unreadable; apps get the answers they had");
                None
            }
            Err(error) => {
                tracing::warn!(%error, "sound coverage read did not finish");
                None
            }
        };
        *self.covered.lock().unwrap_or_else(PoisonError::into_inner) = Some(Covered {
            revision,
            at: self.clock.now_ms(),
            covers: read.unwrap_or(false),
            failed: read.is_none(),
        });
        read.unwrap_or(false)
    }

    /// The coverage last read, while it still holds.
    fn cached(&self) -> Option<bool> {
        let revision = *self.revision.borrow();
        let now = self.clock.now_ms();
        let covered = (*self.covered.lock().unwrap_or_else(PoisonError::into_inner))?;
        let lasts = if covered.failed {
            FAILED_FOR_MS
        } else {
            COVERAGE_FOR_MS
        };
        (covered.revision == revision && now - covered.at < lasts).then_some(covered.covers)
    }

    /// Run `work` over the index and `user_id`'s "Not this artist", off the
    /// async threads: loading the index and scanning it are blocking.
    async fn run_on_index<T: Send + 'static>(
        &self,
        user_id: &str,
        work: impl FnOnce(&SoundIndex, &(dyn Fn(&str) -> bool + Sync)) -> T + Send + 'static,
    ) -> Result<Option<T>, CompatError> {
        let feedback =
            Feedback::load_or_none(&self.db, user_id, self.clock.now_ms(), "compat").await;
        let search = self.analysis.search().clone();
        let run = move || -> Result<Option<T>, SoundError> {
            let Some(index) = search.index()? else {
                return Ok(None);
            };
            let hides = |artist: &str| feedback.hides_artist(Some(artist));
            Ok(Some(work(&index, &hides)))
        };
        match tokio::task::spawn_blocking(run).await {
            Ok(result) => result.map_err(failed),
            Err(error) => Err(CompatError::Internal(error.to_string())),
        }
    }

    /// Up to `count` library tracks that sound like `seeds`, in a
    /// station's walking order, none of the seeds among them. Empty while
    /// the engine does not answer, when it holds none of the seeds, or
    /// when nothing is near enough.
    ///
    /// # Errors
    ///
    /// The index's load failed.
    pub async fn similar(
        &self,
        user_id: &str,
        seeds: &[String],
        count: usize,
    ) -> Result<Vec<String>, CompatError> {
        if seeds.is_empty() || count == 0 || !self.serves().await {
            return Ok(Vec::new());
        }
        let seeds: Vec<Seed> = seeds
            .iter()
            .map(|track_id| Seed {
                track_id: track_id.clone(),
                more_like: false,
            })
            .collect();
        let found = self
            .run_on_index(user_id, move |index, hides| {
                let own: HashSet<String> = seeds
                    .iter()
                    .filter_map(|seed| index.track(&seed.track_id)?.artist_mbid.clone())
                    .collect();
                let hidden_artist = |artist: &str| !own.contains(artist) && hides(artist);
                let excluded = HashSet::new();
                let steering = Steering {
                    excluded: &excluded,
                    hidden_artist: &hidden_artist,
                    not_this: &[],
                    played: &[],
                    groups: &Default::default(),
                    sound_only: true,
                };
                station(index, &seeds, &steering, count)
                    .into_iter()
                    .map(|found| found.track_id)
                    .collect::<Vec<_>>()
            })
            .await?;
        Ok(found.unwrap_or_default())
    }

    /// Up to `count` library tracks that sound like `track_id`, nearest
    /// first. `None` while the engine does not answer or the track has no
    /// sound; an empty list when nothing is near enough.
    ///
    /// # Errors
    ///
    /// The index's load failed.
    pub async fn nearest(
        &self,
        user_id: &str,
        track_id: &str,
        count: usize,
    ) -> Result<Option<Vec<Match>>, CompatError> {
        if !self.serves().await {
            return Ok(None);
        }
        let seed = track_id.to_owned();
        let found = self
            .run_on_index(user_id, move |index, hides| {
                let own = index.track(&seed)?.artist_mbid.clone();
                let skip = |track: &SoundTrack| {
                    track
                        .artist_mbid
                        .as_deref()
                        .is_some_and(|artist| own.as_deref() != Some(artist) && hides(artist))
                };
                let near = index.similar(&seed, count, &skip)?;
                Some(
                    near.into_iter()
                        .map(|near| Match {
                            similarity: similarity(near.distance),
                            track_id: near.track_id,
                        })
                        .collect::<Vec<_>>(),
                )
            })
            .await?;
        Ok(found.flatten())
    }

    /// Up to `count` library tracks leading from `start` to `end` by how
    /// they sound, `start` first and `end` last, each with how alike it
    /// sounds to `start`. `None` while the engine does not answer or either
    /// end has no sound.
    ///
    /// # Errors
    ///
    /// The index's load failed.
    pub async fn path(
        &self,
        user_id: &str,
        start: &str,
        end: &str,
        count: usize,
    ) -> Result<Option<Vec<Match>>, CompatError> {
        if !self.serves().await {
            return Ok(None);
        }
        let (start, end) = (start.to_owned(), end.to_owned());
        let found = self
            .run_on_index(user_id, move |index, hides| {
                let ends: HashSet<String> = [&start, &end]
                    .iter()
                    .filter_map(|id| index.track(id)?.artist_mbid.clone())
                    .collect();
                let skip = |track: &SoundTrack| {
                    track
                        .artist_mbid
                        .as_deref()
                        .is_some_and(|artist| !ends.contains(artist) && hides(artist))
                };
                let path = index.path(&start, &end, count, &skip)?;
                Some(
                    path.into_iter()
                        .map(|track_id| Match {
                            similarity: index.distance(&start, &track_id).map_or(0.0, similarity),
                            track_id,
                        })
                        .collect::<Vec<_>>(),
                )
            })
            .await?;
        Ok(found.flatten())
    }
}
