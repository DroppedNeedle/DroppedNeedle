//! What each library track sounds like, worked out once in the background
//! when an admin turns sound analysis on, so the app can find songs that
//! sound alike.
//!
//! The sound analysis loop reads only the library's own files, beneath
//! their library folder through the no-follow opener, never a preview, a
//! stream or any other music, and never writes a file. Each file is
//! decoded as a stream, prepared as [`front`] says and run through the
//! `MSD_musicnn` model ([`net`]) one patch at a time; the result is a
//! row in `library_sound_analyses`: the track vector (the mean of the
//! patches' 200 values) and the 50 tag scores, at the analysis version.
//!
//! It is opt-in and off by default, and gives way: it rests after each
//! file as long as the file took, never runs while the loudness loop has
//! files due (so the two stay at about half a core together), stops for a
//! running scan and while anyone plays through a transcode, and skips
//! library folders that are not there. Pause stops it and keeps the switch
//! on; what is analysed keeps serving. Off stops it within one file, keeps
//! its rows, and turns the reads off.
//!
//! A row counts only while its stat revision is its track's, so a changed
//! file is analysed again and a moved one is not. A file that does not
//! decode, takes too long, or is shorter than one patch keeps a row saying
//! why and is not tried again until it changes.
//!
//! Every row carries its [`ANALYSIS_VERSION`]. A new version wipes nothing:
//! the rows of the old one keep answering until the new one covers every
//! indexed track, then they are dropped. A read takes one version, never a
//! mix. A vector is never a track's identity: nothing merges, links or
//! de-duplicates tracks by it.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use droppedneedle_kernel::clock::Clock;
use droppedneedle_kernel::db::{CancelFlag, Db, DbError, Lane};
use droppedneedle_kernel::runtime_config::Live;
use droppedneedle_kernel::runtime_config::sections::SoundAnalysisSettings;

use crate::loudness::Loudness;
use crate::scan::{legacy_mtime_eps_seconds, mtime_ns_from_metadata};
use crate::tags::TrackFile;
use crate::wiring::{LibraryCore, ScanCoordinator};

pub mod analyse;
pub mod front;
pub mod net;
mod run;
pub(crate) mod sql;

pub use analyse::{Analyser, TrackSound, Unanalysed, analyse_file};
pub use net::{MODEL, Models, TAGS};
pub use run::{SOUND_JOB, spawn_on};

/// The analysis every new row is written at. Bump it when the model or
/// anything in the audio preparation changes, the resampler's settings for
/// any rate included.
pub const ANALYSIS_VERSION: i64 = 1;

/// Seconds one track takes, rest included, before the loop has timed any
/// file here: 5.28 s of work for a 4-minute track on one core of a fast
/// desktop processor, doubled by the rest after it.
pub const SECONDS_PER_TRACK: f64 = 10.6;

/// What analysing a file came to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum SoundOutcome {
    /// Its sound was worked out.
    Analysed,
    /// It would not decode, or stopped early.
    Undecodable,
    /// It is shorter than one patch, about 3 seconds.
    TooShort,
    /// It took longer than a file may.
    OverTime,
}

droppedneedle_kernel::spelled!(SoundOutcome {
    Analysed => "analysed",
    Undecodable => "undecodable",
    TooShort => "too_short",
    OverTime => "over_time",
});

/// Why sound analysis did not finish.
#[derive(Debug, Clone, thiserror::Error)]
pub enum SoundError {
    /// The database was too busy; nothing is lost and it is tried again.
    #[error("the database is busy: {0}")]
    Busy(String),
    /// A store fault, or a model that would not load or run.
    #[error("sound analysis: {0}")]
    Failed(String),
}

impl From<DbError> for SoundError {
    fn from(error: DbError) -> Self {
        match error {
            DbError::Busy { .. } | DbError::LaneClosed => Self::Busy(error.to_string()),
            other => Self::Failed(other.to_string()),
        }
    }
}

impl From<rusqlite::Error> for SoundError {
    fn from(error: rusqlite::Error) -> Self {
        DbError::from(error).into()
    }
}

/// How hard the loop works: after each file it rests `rest_per_spent`
/// times as long as the file took, and never less than `min_rest`.
#[derive(Debug, Clone, Copy)]
pub struct Pace {
    pub min_rest: Duration,
    pub rest_per_spent: f64,
}

/// About half of one core, the loudness loop's pace.
pub const PACE: Pace = Pace {
    min_rest: Duration::from_millis(500),
    rest_per_spent: 1.0,
};

/// Tracks read from the database at a time; their results go back in one
/// short write.
const BATCH: usize = 10;
/// Results held while the database is busy before a pass reads no more.
const MOST_HELD: usize = 100;
/// Longest one file may take before it counts as over time.
const FILE_BUDGET: Duration = Duration::from_secs(600);
/// How long a file that could not be read, or a library folder whose files
/// are not there, waits before it is tried again.
const UNREADABLE_WAIT: Duration = Duration::from_secs(3600);
/// Files in a row under one library folder that are not there before the
/// folder is left for [`UNREADABLE_WAIT`].
const MISSING_IN_A_ROW: usize = 5;

/// Why the loop is waiting. It carries on by itself once the reason goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum SoundWaitReason {
    /// A library scan is running.
    Scan,
    /// The loudness loop has files to measure first.
    Loudness,
    /// Someone is playing music through a transcode.
    Streaming,
    /// The tracks left are under a library folder that is not there.
    FolderMissing,
    /// The library is switched off.
    LibraryOff,
    /// The sound analysis settings cannot be read.
    SettingsUnreadable,
}

/// What the loop is doing, as the status shows it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum SoundState {
    /// The switch is off.
    Off,
    /// Switched on, and the loop has not looked yet: it does within a
    /// minute.
    Starting,
    /// Analysing files.
    Running,
    /// Waiting for something; see the reason.
    Waiting,
    /// Paused by an admin.
    Paused,
    /// Every track that can be reached is analysed.
    Done,
}

/// What the loop last reported, kept in memory only.
#[derive(Debug, Clone, Copy, Default)]
struct Activity {
    /// Waiting for this, when it is.
    waiting: Option<SoundWaitReason>,
    /// The last pass found nothing to do.
    done: bool,
    /// A pass has run since the server started.
    started: bool,
    /// The mean time one file took lately, rest included.
    per_track: Option<f64>,
}

/// One result waiting to be kept.
#[derive(Debug, Clone)]
struct Found {
    track_id: String,
    stat_revision: String,
    outcome: SoundOutcome,
    sound: Option<TrackSound>,
    seconds: f64,
    at_ms: i64,
}

/// What the loop keeps between passes: results the database was too busy
/// to take; files waiting out a failed read, by track and stat revision;
/// library folders left alone while their files are not there; and the
/// models, loaded on first use and let go when the switch is turned off.
#[derive(Debug, Default)]
pub struct Sweep {
    held: Vec<Found>,
    waiting: HashMap<(String, String), Instant>,
    roots_waiting: HashMap<String, Instant>,
    missing: HashMap<String, usize>,
    models: Option<Models>,
}

/// What one pass did.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct PassReport {
    /// Files read, analysed or not.
    pub read: usize,
    /// Rows written.
    pub kept: usize,
    /// Seconds of audio analysed.
    pub audio_seconds: f64,
    /// Time spent reading and analysing.
    pub spent: Duration,
    /// Nothing is due now.
    pub finished: bool,
    /// Why the pass stopped short, when something held it back.
    pub held_back: Option<SoundWaitReason>,
}

impl PassReport {
    fn add(&mut self, other: &Self) {
        self.read += other.read;
        self.kept += other.kept;
        self.audio_seconds += other.audio_seconds;
        self.spent += other.spent;
    }

    /// One line on what a batch analysed and how fast, for the heat check.
    fn log(&self) {
        if self.read == 0 {
            return;
        }
        let spent = self.spent.as_secs_f64();
        tracing::info!(
            files = self.read,
            kept = self.kept,
            audio_seconds = self.audio_seconds.round(),
            seconds = (spent * 100.0).round() / 100.0,
            seconds_per_file = (spent / self.read as f64 * 100.0).round() / 100.0,
            "track sound analysed"
        );
    }
}

/// Whether a batch went on to its end.
enum Flow {
    Go,
    Stop(Option<SoundWaitReason>),
}

/// The admin's switch and pause as the loop reads them now.
enum Switch {
    On,
    Off,
    Paused,
    Unreadable,
}

/// The sound analysis service: the loop's passes, its status and the
/// reads the sound features use.
#[derive(Clone)]
pub struct SoundAnalysis {
    db: Db,
    core: LibraryCore,
    coordinator: Arc<ScanCoordinator>,
    loudness: Loudness,
    settings: Live<SoundAnalysisSettings>,
    clock: Arc<dyn Clock>,
    activity: Arc<Mutex<Activity>>,
    /// Moved after every write that kept or dropped rows, for [`SoundMark`].
    changes: Arc<AtomicU64>,
}

/// One track's stored sound.
#[derive(Debug, Clone, PartialEq)]
pub struct StoredSound {
    pub track_id: String,
    /// The track vector: 200 values, not normalised.
    pub vector: Vec<f32>,
    /// The 50 tag scores in [`TAGS`] order, 0 to 255.
    pub tags: Vec<u8>,
}

/// Every analysed track's sound at the version reads answer from.
#[derive(Debug, Clone, PartialEq)]
pub struct SoundVectors {
    pub analysis_version: i64,
    pub tracks: Vec<StoredSound>,
}

/// How much of the library is analysed at [`ANALYSIS_VERSION`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Coverage {
    /// Indexed tracks under the library folders the settings name.
    pub tracks: u64,
    /// With a sound for their current bytes.
    pub analysed: u64,
    /// Read and found not analysable for their current bytes.
    pub not_analysed: u64,
}

impl Coverage {
    /// Tracks still to read.
    #[must_use]
    pub fn to_go(&self) -> u64 {
        self.tracks
            .saturating_sub(self.analysed + self.not_analysed)
    }
}

/// A cheap answer to "has the analysis changed", read without walking a
/// table. It differs whenever [`SoundAnalysis::vectors`] could answer
/// differently: this process kept or dropped rows (`changes`), a commit
/// changed catalog rows, such as a scan reading a file again (its row stops
/// counting) or a track going (its row goes with it) (`catalog_revision`),
/// or the switch moved (`enabled`). A restart starts `changes` again, so a
/// reader keeps its mark in memory only.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SoundMark {
    pub enabled: bool,
    pub catalog_revision: i64,
    pub changes: u64,
}

/// The last file found not analysable, where it lies below its library
/// folder, and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LastNotAnalysed {
    pub relative_path: String,
    pub outcome: SoundOutcome,
}

/// Everything the settings card shows.
#[derive(Debug, Clone, PartialEq)]
pub struct SoundStatus {
    pub state: SoundState,
    pub waiting_for: Option<SoundWaitReason>,
    pub coverage: Coverage,
    /// Seconds one track takes, rest included: measured here once the loop
    /// has timed files, else [`SECONDS_PER_TRACK`].
    pub seconds_per_track: f64,
    pub last_not_analysed: Option<LastNotAnalysed>,
}

fn coverage_of(row: &sql::Coverage) -> Coverage {
    let count = |value: i64| u64::try_from(value).unwrap_or(0);
    Coverage {
        tracks: count(row.tracks),
        analysed: count(row.analysed),
        not_analysed: count(row.not_analysed),
    }
}

fn decode_vector(bytes: &[u8]) -> Option<Vec<f32>> {
    (bytes.len() == net::EMBEDDING * 4).then(|| {
        bytes
            .chunks_exact(4)
            .map(|chunk| f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]))
            .collect()
    })
}

fn stored(row: sql::Vector) -> Option<StoredSound> {
    let vector = decode_vector(&row.vector);
    if vector.is_none() || row.tags.len() != net::TAG_COUNT {
        tracing::warn!(
            track_id = row.track_id,
            "a stored track sound does not read; it is left out"
        );
    }
    Some(StoredSound {
        vector: vector?,
        tags: (row.tags.len() == net::TAG_COUNT).then_some(row.tags)?,
        track_id: row.track_id,
    })
}

impl SoundAnalysis {
    pub(crate) fn new(
        db: Db,
        core: LibraryCore,
        coordinator: Arc<ScanCoordinator>,
        loudness: Loudness,
        settings: Live<SoundAnalysisSettings>,
        clock: Arc<dyn Clock>,
    ) -> Self {
        Self {
            db,
            core,
            coordinator,
            loudness,
            settings,
            clock,
            activity: Arc::default(),
            changes: Arc::default(),
        }
    }

    fn switch(&self) -> Switch {
        match self.settings.read() {
            None => Switch::Unreadable,
            Some(section) if !section.sound_analysis_enabled => Switch::Off,
            Some(section) if section.sound_analysis_paused => Switch::Paused,
            Some(_) => Switch::On,
        }
    }

    /// Whether the switch is on (paused or not): the sound features answer
    /// only then.
    fn enabled(&self) -> bool {
        matches!(self.switch(), Switch::On | Switch::Paused)
    }

    fn note(&self, change: impl FnOnce(&mut Activity)) {
        change(&mut self.activity.lock().unwrap_or_else(PoisonError::into_inner));
    }

    /// Analyse every due file until none is left, something holds the
    /// loop back, or `stop` answers true; `busy` answers true while
    /// someone plays through a transcode. After each file it calls `rest`
    /// with how long to rest, which answers false to stop. Results are kept
    /// after every [`BATCH`] files; a busy database holds them in `sweep`
    /// for a later write. Blocking.
    pub fn pass(
        &self,
        sweep: &mut Sweep,
        pace: &Pace,
        stop: &dyn Fn() -> bool,
        busy: &dyn Fn() -> bool,
        rest: &dyn Fn(Duration) -> bool,
    ) -> Result<PassReport, SoundError> {
        let mut report = PassReport::default();
        self.note(|activity| activity.started = true);
        match self.keep_held(sweep) {
            Ok(kept) => report.kept += kept,
            Err(SoundError::Busy(cause)) if sweep.held.len() >= MOST_HELD => {
                return Err(SoundError::Busy(cause));
            }
            Err(SoundError::Busy(_)) => {}
            Err(error) => return Err(error),
        }
        loop {
            if let Flow::Stop(reason) = self.holds_back(stop, busy, sweep)? {
                report.held_back = reason;
                return Ok(report);
            }
            let due = self.next_batch(sweep)?;
            if due.is_empty() && !sweep.held.is_empty() {
                // The last results wait for a busy database: not done yet.
                return Err(SoundError::Busy(format!(
                    "{} sound results wait to be kept",
                    sweep.held.len()
                )));
            }
            if due.is_empty() {
                self.finish_pass(&mut report, sweep)?;
                return Ok(report);
            }
            if sweep.models.is_none() {
                sweep.models =
                    Some(Models::load().map_err(|error| SoundError::Failed(error.to_string()))?);
            }
            self.note(|activity| {
                activity.waiting = None;
                activity.done = false;
            });
            let mut batch = PassReport::default();
            let flow = self.analyse_batch(due, sweep, pace, stop, busy, rest, &mut batch)?;
            let kept = self.keep_held(sweep);
            if let Ok(count) = kept {
                batch.kept += count;
            }
            batch.log();
            report.add(&batch);
            match kept {
                Ok(_) => {}
                Err(SoundError::Busy(cause)) if sweep.held.len() < MOST_HELD => {
                    tracing::info!(%cause, held = sweep.held.len(), "sound results wait: the database is busy");
                }
                Err(error) => return Err(error),
            }
            if let Flow::Stop(reason) = flow {
                report.held_back = reason;
                return Ok(report);
            }
        }
    }

    /// Nothing is due under the folders that are there: drop the rows of
    /// other versions once this one covers the library, and say whether
    /// tracks are left under a folder that is not there (or is left alone
    /// while its files are not). Files waiting out a failed read do not
    /// count: they are tried again by themselves.
    fn finish_pass(&self, report: &mut PassReport, sweep: &Sweep) -> Result<(), SoundError> {
        report.finished = true;
        let serving = self.db.read_blocking("library.sound_serving", |conn| {
            conn.prepare_cached(sql::SERVING_VERSION.text())?
                .query_row([ANALYSIS_VERSION], |row| row.get::<_, i64>(0))
                .map_err(SoundError::from)
        })?;
        let library_reads = self.core.library.read().is_some();
        let named = self.core.live_registry().root_paths();
        // No folder named, or a library section that does not read (the
        // registry then keeps what it had, empty at boot), says nothing
        // about coverage: the old rows stay.
        let dropped = if serving == ANALYSIS_VERSION || !library_reads || named.is_empty() {
            0
        } else {
            let roots = self.configured_roots()?;
            self.db
                .write_blocking(Lane::Background, "library.sound_drop_old", |tx| {
                    Ok::<_, SoundError>(
                        tx.prepare_cached(sql::DROP_OLD_VERSIONS.text())?
                            .execute(rusqlite::params![ANALYSIS_VERSION, roots])?,
                    )
                })?
        };
        if dropped > 0 {
            self.changes.fetch_add(1, Ordering::AcqRel);
            tracing::info!(
                dropped,
                version = ANALYSIS_VERSION,
                "the new sound analysis covers the library; the old rows are dropped"
            );
        }
        let present: HashSet<String> = self
            .present_roots()
            .into_iter()
            .filter(|root| !sweep.roots_waiting.contains_key(root))
            .collect();
        let away: Vec<String> = self
            .core
            .live_registry()
            .root_paths()
            .into_keys()
            .filter(|root| !present.contains(root))
            .collect();
        let left = !away.is_empty() && {
            let away = serde_json::to_string(&away)
                .map_err(|error| SoundError::Failed(error.to_string()))?;
            self.db
                .read_long("library.sound_any_due", &CancelFlag::never(), |conn| {
                    conn.prepare_cached(sql::ANY_DUE.text())?
                        .query_row(rusqlite::params![ANALYSIS_VERSION, away], |row| {
                            row.get::<_, bool>(0)
                        })
                        .map_err(SoundError::from)
                })?
        };
        let reason = left.then_some(SoundWaitReason::FolderMissing);
        report.held_back = reason;
        self.note(|activity| {
            activity.waiting = reason;
            activity.done = !left;
        });
        Ok(())
    }

    /// What holds the loop back now, noted for the status. The loudness
    /// loop is asked only when everything cheaper lets the pass go on.
    fn holds_back(
        &self,
        stop: &dyn Fn() -> bool,
        busy: &dyn Fn() -> bool,
        sweep: &mut Sweep,
    ) -> Result<Flow, SoundError> {
        if stop() {
            return Ok(Flow::Stop(None));
        }
        let reason = match self.switch() {
            Switch::Unreadable => Some(SoundWaitReason::SettingsUnreadable),
            switch @ (Switch::Off | Switch::Paused) => {
                let off = matches!(switch, Switch::Off);
                if off {
                    sweep.models = None;
                }
                self.note(|activity| {
                    activity.waiting = None;
                    activity.done = false;
                    // Switched on again, the card says Starting until a
                    // pass runs.
                    activity.started &= !off;
                });
                return Ok(Flow::Stop(None));
            }
            Switch::On if self.core.switched_off() => Some(SoundWaitReason::LibraryOff),
            Switch::On if !self.coordinator.current().is_empty() => Some(SoundWaitReason::Scan),
            Switch::On if busy() => Some(SoundWaitReason::Streaming),
            Switch::On => match self.loudness.files_due() {
                Ok(true) => Some(SoundWaitReason::Loudness),
                Ok(false) => None,
                Err(crate::loudness::LoudnessError::Busy(cause)) => {
                    return Err(SoundError::Busy(cause));
                }
                Err(crate::loudness::LoudnessError::Failed(cause)) => {
                    return Err(SoundError::Failed(cause));
                }
            },
        };
        self.note(|activity| activity.waiting = reason);
        Ok(match reason {
            Some(reason) => Flow::Stop(Some(reason)),
            None => Flow::Go,
        })
    }

    /// The next due tracks, leaving out those held, waiting, or under a
    /// library folder that is not there or is left alone for now.
    fn next_batch(&self, sweep: &mut Sweep) -> Result<Vec<sql::DueTrack>, SoundError> {
        let now = Instant::now();
        sweep.waiting.retain(|_, until| *until > now);
        sweep.roots_waiting.retain(|_, until| *until > now);
        let roots: Vec<String> = self
            .present_roots()
            .into_iter()
            .filter(|root| !sweep.roots_waiting.contains_key(root))
            .collect();
        if roots.is_empty() {
            return Ok(Vec::new());
        }
        let held: HashSet<&str> = sweep
            .held
            .iter()
            .map(|found| found.track_id.as_str())
            .collect();
        Ok(self
            .due(&roots, BATCH + held.len() + sweep.waiting.len())?
            .into_iter()
            .filter(|track| {
                !held.contains(track.id.as_str())
                    && !sweep
                        .waiting
                        .contains_key(&(track.id.clone(), track.stat_revision.clone()))
            })
            .take(BATCH)
            .collect())
    }

    #[allow(clippy::too_many_arguments)]
    fn analyse_batch(
        &self,
        due: Vec<sql::DueTrack>,
        sweep: &mut Sweep,
        pace: &Pace,
        stop: &dyn Fn() -> bool,
        busy: &dyn Fn() -> bool,
        rest: &dyn Fn(Duration) -> bool,
        batch: &mut PassReport,
    ) -> Result<Flow, SoundError> {
        for track in due {
            if sweep.roots_waiting.contains_key(&track.root_id) {
                continue;
            }
            if let Flow::Stop(reason) = self.holds_back(stop, busy, sweep)? {
                return Ok(Flow::Stop(reason));
            }
            let Some(models) = sweep.models.as_ref() else {
                return Ok(Flow::Stop(None));
            };
            let started = Instant::now();
            let polls = std::cell::Cell::new(0u32);
            let read = read_one(&track, models, &|| {
                // The scan check reads the run list, so it is asked less
                // often: about every 2 seconds of audio.
                polls.set(polls.get().wrapping_add(1));
                stop()
                    || !matches!(self.switch(), Switch::On)
                    || self.core.switched_off()
                    || busy()
                    || (polls.get().is_multiple_of(4) && !self.coordinator.current().is_empty())
            });
            let took = started.elapsed();
            let wait_out = |sweep: &mut Sweep| {
                sweep.waiting.insert(
                    (track.id.clone(), track.stat_revision.clone()),
                    Instant::now() + UNREADABLE_WAIT,
                );
            };
            match read {
                Read::Stopped => return Ok(Flow::Stop(None)),
                Read::Failed(cause) => return Err(SoundError::Failed(cause)),
                Read::Wait { cause, missing } => {
                    tracing::info!(track_id = track.id, %cause, "a file's sound analysis waits an hour: it could not be read");
                    wait_out(sweep);
                    let in_a_row = sweep.missing.entry(track.root_id.clone()).or_default();
                    *in_a_row = if missing { *in_a_row + 1 } else { 0 };
                    if *in_a_row >= MISSING_IN_A_ROW {
                        tracing::warn!(
                            root_id = track.root_id,
                            "a library folder's files are not there; its sound analysis waits an hour"
                        );
                        sweep.missing.remove(&track.root_id);
                        sweep
                            .roots_waiting
                            .insert(track.root_id.clone(), Instant::now() + UNREADABLE_WAIT);
                    }
                }
                Read::Changed => {
                    tracing::info!(
                        track_id = track.id,
                        "a file changed while its sound was analysed; it is analysed once the scan reads it again"
                    );
                    batch.read += 1;
                    batch.spent += took;
                    wait_out(sweep);
                    sweep.missing.remove(&track.root_id);
                }
                Read::Done(outcome, sound) => {
                    batch.read += 1;
                    batch.spent += took;
                    let seconds = sound.as_ref().map_or(0.0, |sound| sound.seconds);
                    batch.audio_seconds += seconds;
                    sweep.missing.remove(&track.root_id);
                    sweep.held.push(Found {
                        track_id: track.id,
                        stat_revision: track.stat_revision,
                        outcome,
                        sound,
                        seconds,
                        at_ms: self.clock.now_ms(),
                    });
                }
            }
            let wait = took.mul_f64(pace.rest_per_spent).max(pace.min_rest);
            let per_file = (took + wait).as_secs_f64();
            self.note(|activity| {
                activity.per_track = Some(
                    activity
                        .per_track
                        .map_or(per_file, |mean| mean * 0.9 + per_file * 0.1),
                );
            });
            if !rest(wait) {
                return Ok(Flow::Stop(None));
            }
        }
        Ok(Flow::Go)
    }

    /// The library folders the settings name, as the JSON array the
    /// statements take: the library is the indexed tracks under them. A
    /// removed folder's tracks stay indexed until an admin drops them, and
    /// are left out.
    fn configured_roots(&self) -> Result<String, SoundError> {
        let mut roots: Vec<String> = self.core.live_registry().root_paths().into_keys().collect();
        roots.sort();
        serde_json::to_string(&roots).map_err(|error| SoundError::Failed(error.to_string()))
    }

    /// The library folders that are there now.
    fn present_roots(&self) -> Vec<String> {
        let mut roots: Vec<String> = self
            .core
            .live_registry()
            .root_paths()
            .into_iter()
            .filter(|(_, path)| std::fs::metadata(path).is_ok_and(|meta| meta.is_dir()))
            .map(|(id, _)| id)
            .collect();
        roots.sort();
        roots
    }

    /// Up to `limit` due tracks. It walks and sorts the track table, so it
    /// reads on the long pool.
    fn due(&self, roots: &[String], limit: usize) -> Result<Vec<sql::DueTrack>, SoundError> {
        let roots =
            serde_json::to_string(roots).map_err(|error| SoundError::Failed(error.to_string()))?;
        let limit = i64::try_from(limit).unwrap_or(i64::MAX);
        self.db
            .read_long("library.sound_due", &CancelFlag::never(), |conn| {
                conn.prepare_cached(sql::DUE.text())?
                    .query_map(rusqlite::params![ANALYSIS_VERSION, roots, limit], |row| {
                        sql::DueTrack::read(row)
                    })?
                    .collect::<Result<Vec<_>, _>>()
                    .map_err(SoundError::from)
            })
    }

    /// Write the held results in one short transaction; each lands only
    /// while its track is at the revision it was read at. A busy database
    /// keeps them held; any other failure drops them, and their files are
    /// due again. Answers the rows written.
    fn keep_held(&self, sweep: &mut Sweep) -> Result<usize, SoundError> {
        if sweep.held.is_empty() {
            return Ok(0);
        }
        let held = &sweep.held;
        let kept = self
            .db
            .write_blocking(Lane::Background, "library.sound_keep", |tx| {
                let mut keep = tx.prepare_cached(sql::KEEP.text())?;
                let mut kept = 0;
                for found in held {
                    kept += keep.execute(rusqlite::params![
                        found.track_id,
                        found.stat_revision,
                        ANALYSIS_VERSION,
                        MODEL,
                        found.outcome,
                        found.sound.as_ref().map(TrackSound::vector_bytes),
                        found.sound.as_ref().map(|sound| sound.tags.clone()),
                        found.seconds,
                        found.at_ms,
                    ])?;
                }
                Ok::<_, SoundError>(kept)
            });
        if !matches!(kept, Err(SoundError::Busy(_))) {
            sweep.held.clear();
        }
        if matches!(kept, Ok(count) if count > 0) {
            self.changes.fetch_add(1, Ordering::AcqRel);
        }
        kept
    }

    /// Everything the settings card shows: the state, why it waits, the
    /// counts at [`ANALYSIS_VERSION`] and the last file it could not read.
    /// Blocking.
    pub fn status(&self) -> Result<SoundStatus, SoundError> {
        let activity = *self.activity.lock().unwrap_or_else(PoisonError::into_inner);
        let (state, waiting_for) = match self.switch() {
            Switch::Unreadable => (
                SoundState::Waiting,
                Some(SoundWaitReason::SettingsUnreadable),
            ),
            Switch::Off => (SoundState::Off, None),
            Switch::Paused => (SoundState::Paused, None),
            Switch::On if !activity.started => (SoundState::Starting, None),
            Switch::On => match activity.waiting {
                Some(reason) => (SoundState::Waiting, Some(reason)),
                None if activity.done => (SoundState::Done, None),
                None => (SoundState::Running, None),
            },
        };
        let roots = self.configured_roots()?;
        let (coverage, last) = self.db.read_long(
            "library.sound_status",
            &CancelFlag::never(),
            // The counts and the last file not analysed must agree.
            |conn| {
                let coverage = conn.prepare_cached(sql::COVERAGE.text())?.query_row(
                    rusqlite::params![ANALYSIS_VERSION, roots],
                    sql::Coverage::read,
                )?;
                let last = conn
                    .prepare_cached(sql::LAST_NOT_ANALYSED.text())?
                    .query_map(
                        rusqlite::params![ANALYSIS_VERSION, roots],
                        sql::NotAnalysed::read,
                    )?
                    .next()
                    .transpose()?;
                Ok::<_, SoundError>((coverage, last))
            },
        )?;
        Ok(SoundStatus {
            state,
            waiting_for,
            coverage: coverage_of(&coverage),
            seconds_per_track: activity.per_track.unwrap_or(SECONDS_PER_TRACK),
            last_not_analysed: last.map(|row| LastNotAnalysed {
                relative_path: row.relative_path,
                outcome: row.outcome,
            }),
        })
    }

    /// Every analysed track's sound, at one version: the newest older one
    /// while [`ANALYSIS_VERSION`] does not cover the library yet, else the
    /// current one. `None` while the switch is off or its settings cannot
    /// be read. Reads on the long pool. Blocking.
    pub fn vectors(&self) -> Result<Option<SoundVectors>, SoundError> {
        if !self.enabled() {
            return Ok(None);
        }
        let (version, rows) = self.db.read_long(
            "library.sound_vectors",
            &CancelFlag::never(),
            // The version and its rows must agree: a drop of the old rows
            // between two reads would leave the second empty.
            |conn| {
                let version: i64 = conn
                    .prepare_cached(sql::SERVING_VERSION.text())?
                    .query_row([ANALYSIS_VERSION], |row| row.get(0))?;
                let rows = conn
                    .prepare_cached(sql::VECTORS.text())?
                    .query_map([version], sql::Vector::read)?
                    .collect::<Result<Vec<_>, _>>()?;
                Ok::<_, SoundError>((version, rows))
            },
        )?;
        Ok(Some(SoundVectors {
            analysis_version: version,
            tracks: rows.into_iter().filter_map(stored).collect(),
        }))
    }

    /// One indexed track's sound at the version [`Self::vectors`] answers
    /// from, for its current bytes. `None` when it has none, or while the
    /// switch is off. Blocking.
    pub fn track(&self, track_id: &str) -> Result<Option<StoredSound>, SoundError> {
        if !self.enabled() {
            return Ok(None);
        }
        let row = self.db.read_blocking("library.sound_track", |conn| {
            // The version and the row must agree, as in `vectors`.
            let version: i64 = conn
                .prepare_cached(sql::SERVING_VERSION.text())?
                .query_row([ANALYSIS_VERSION], |row| row.get(0))?;
            conn.prepare_cached(sql::TRACK_VECTOR.text())?
                .query_map(rusqlite::params![version, track_id], sql::Vector::read)?
                .next()
                .transpose()
                .map_err(SoundError::from)
        })?;
        Ok(row.and_then(stored))
    }

    /// How much of the library is analysed at [`ANALYSIS_VERSION`].
    /// Blocking.
    pub fn coverage(&self) -> Result<Coverage, SoundError> {
        let roots = self.configured_roots()?;
        let row = self
            .db
            .read_long("library.sound_coverage", &CancelFlag::never(), |conn| {
                conn.prepare_cached(sql::COVERAGE.text())?
                    .query_row(
                        rusqlite::params![ANALYSIS_VERSION, roots],
                        sql::Coverage::read,
                    )
                    .map_err(SoundError::from)
            })?;
        Ok(coverage_of(&row))
    }

    /// A cheap mark that changes whenever the analysis does, so a reader
    /// loads [`Self::vectors`] again only then. A reader takes the mark
    /// before it calls `vectors`, so a change between the two shows as a
    /// new mark next time rather than being missed. Blocking.
    pub fn mark(&self) -> Result<SoundMark, SoundError> {
        let changes = self.changes.load(Ordering::Acquire);
        let catalog_revision = self.db.read_blocking("library.sound_mark", |conn| {
            conn.prepare_cached(sql::CATALOG_REVISION.text())?
                .query_row([], |row| row.get::<_, i64>(0))
                .map_err(SoundError::from)
        })?;
        Ok(SoundMark {
            enabled: self.enabled(),
            catalog_revision,
            changes,
        })
    }
}

/// What reading one file came to.
enum Read {
    /// Analysed, or not analysable until the file changes.
    Done(SoundOutcome, Option<TrackSound>),
    /// The file could not be read now: try again later. `missing` when it
    /// is not there at all.
    Wait { cause: String, missing: bool },
    /// The file changed while it was read.
    Changed,
    /// The pass was asked to stop.
    Stopped,
    /// The model failed, whatever the file.
    Failed(String),
}

/// Analyse one due track's file, read beneath its library folder. Its
/// size and modification time are read before and after and must be the
/// ones the scan read; a file that changed on the way gives no result.
fn read_one(track: &sql::DueTrack, models: &Models, stop: &dyn Fn() -> bool) -> Read {
    let Some(file) = TrackFile::in_library(Path::new(&track.file_path), &track.relative_path)
    else {
        return Read::Wait {
            cause: "its path lies outside its library folder".to_owned(),
            missing: false,
        };
    };
    let before = match stat(file) {
        Ok(before) => before,
        Err(error) => {
            return Read::Wait {
                missing: error.kind() == std::io::ErrorKind::NotFound,
                cause: error.to_string(),
            };
        }
    };
    if !as_scanned(track, before) {
        return Read::Wait {
            cause: "its size or modification time is not what the scan read".to_owned(),
            missing: false,
        };
    }
    let found = analyse_file(file, models, Instant::now() + FILE_BUDGET, stop);
    if matches!(found, Err(Unanalysed::Stopped)) {
        return Read::Stopped;
    }
    if stat(file).ok() != Some(before) {
        return Read::Changed;
    }
    match found {
        Ok(sound) => Read::Done(SoundOutcome::Analysed, Some(sound)),
        Err(Unanalysed::Undecodable) => Read::Done(SoundOutcome::Undecodable, None),
        Err(Unanalysed::TooShort) => Read::Done(SoundOutcome::TooShort, None),
        Err(Unanalysed::OverTime) => Read::Done(SoundOutcome::OverTime, None),
        Err(Unanalysed::Stopped) => Read::Stopped,
        // The resampler or the FFT refused what this file gave it (a rate
        // in its header no resampler takes): the file's fault, not the
        // loop's.
        Err(Unanalysed::Front(error)) => {
            tracing::warn!(track_id = track.id, %error, "a file's audio could not be prepared; it is kept as undecodable");
            Read::Done(SoundOutcome::Undecodable, None)
        }
        Err(error @ Unanalysed::Model(_)) => Read::Failed(error.to_string()),
    }
}

/// Whether a file's size and modification time are the ones the scan
/// stored, the time within the band the scan allows a v2 row.
fn as_scanned(track: &sql::DueTrack, (size, mtime_ns): (u64, i64)) -> bool {
    let band_ns = legacy_mtime_eps_seconds(track.file_mtime_ns as f64 / 1e9) * 1e9;
    u64::try_from(track.file_size_bytes).ok() == Some(size)
        && (mtime_ns.abs_diff(track.file_mtime_ns) as f64) <= band_ns
}

/// A file's size and modification time, read through the no-follow opener.
fn stat(file: TrackFile<'_>) -> std::io::Result<(u64, i64)> {
    let meta = file.open()?.metadata()?;
    Ok((meta.len(), mtime_ns_from_metadata(&meta)))
}
