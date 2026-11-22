//! Each library track's loudness, measured once in the background so the
//! web player can play every song at a similar level.
//!
//! The loudness loop reads files beneath their library folder through the
//! no-follow opener, decodes them with the ReplayGain meter and keeps the
//! result in `library_loudness_measurements`. It never writes a file. It
//! measures one file at a time and rests as long as the file took (at
//! least half a second), so it uses about half of one core; it gives way
//! to a running scan and skips library folders that are not there. A row
//! counts only while its stat revision is its track's, so a changed file
//! is never served an old number and is measured again; a move keeps the
//! track and its revision, so nothing is measured twice.
//!
//! [`Loudness::levels`] answers the web player's levelling numbers:
//! measured first, the file's ReplayGain tags second. An album's loudness
//! is worked out from its tracks' rows when asked, never stored.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use droppedneedle_kernel::clock::Clock;
use droppedneedle_kernel::db::{CancelFlag, Db, DbError, Lane};

use crate::scan::{legacy_mtime_eps_seconds, mtime_ns_from_metadata};
use crate::tags::TrackFile;
use crate::tags::loudness::{self, BlockSummary, Unanalysed};
use crate::wiring::{LibraryCore, ScanCoordinator};

pub(crate) mod sql;

/// The wakeup the loudness loop listens on: a scan run that changed the
/// catalog, or a publish that rewrote files.
pub const WAKEUP: &str = "library-loudness";

/// What a measurement found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoudnessOutcome {
    /// The whole file was decoded and measured.
    Measured,
    /// The file could not be decoded, reads short, or took longer than a
    /// file may: it is not tried again until it changes.
    Unmeasurable,
}

droppedneedle_kernel::spelled!(LoudnessOutcome {
    Measured => "measured",
    Unmeasurable => "unmeasurable",
});

/// Why loudness work did not finish.
#[derive(Debug, Clone, thiserror::Error)]
pub enum LoudnessError {
    /// The database was too busy; nothing is lost and it is tried again.
    #[error("the database is busy: {0}")]
    Busy(String),
    /// A store fault.
    #[error("loudness store: {0}")]
    Failed(String),
}

impl From<DbError> for LoudnessError {
    fn from(error: DbError) -> Self {
        match error {
            DbError::Busy { .. } | DbError::LaneClosed => Self::Busy(error.to_string()),
            other => Self::Failed(other.to_string()),
        }
    }
}

impl From<rusqlite::Error> for LoudnessError {
    fn from(error: rusqlite::Error) -> Self {
        DbError::from(error).into()
    }
}

/// One track's levelling numbers, as ReplayGain 2.0 gives them: a gain in
/// dB to the -18 LUFS reference and a linear sample peak.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct TrackLevels {
    pub track_gain_db: Option<f64>,
    pub track_peak: Option<f64>,
    pub album_gain_db: Option<f64>,
    pub album_peak: Option<f64>,
}

/// How hard the loop works: after each file it rests `rest_per_spent`
/// times as long as the file took, and never less than `min_rest`.
#[derive(Debug, Clone, Copy)]
pub struct Pace {
    pub min_rest: Duration,
    pub rest_per_spent: f64,
}

/// About half of one core.
pub const PACE: Pace = Pace {
    min_rest: Duration::from_millis(500),
    rest_per_spent: 1.0,
};

/// Tracks read from the database at a time; their results go back in one
/// short write.
const BATCH: usize = 20;
/// Results held while the database is busy before a pass decodes no more.
const MOST_HELD: usize = 200;
/// Longest one file's decode may take before it counts as unmeasurable.
const FILE_BUDGET: Duration = Duration::from_secs(600);
/// How long a file that could not be read, or a library folder whose files
/// are not there, waits before it is tried again.
const UNREADABLE_WAIT: Duration = Duration::from_secs(3600);
/// Files in a row under one library folder that are not there before the
/// folder is left for [`UNREADABLE_WAIT`]: a share that is not mounted
/// leaves an empty folder behind.
const MISSING_IN_A_ROW: usize = 5;

/// One result waiting to be kept.
#[derive(Debug, Clone)]
struct Found {
    track_id: String,
    stat_revision: String,
    outcome: LoudnessOutcome,
    peak: Option<f64>,
    blocks: Option<Vec<u8>>,
    at_ms: i64,
}

/// What the loop keeps between passes: results the database was too busy
/// to take; files waiting out a failed read, by track and the stat
/// revision they failed at (a file the scan reads again is due at once);
/// and library folders left alone while their files are not there.
#[derive(Debug, Default)]
pub struct Sweep {
    held: Vec<Found>,
    waiting: HashMap<(String, String), Instant>,
    roots_waiting: HashMap<String, Instant>,
    missing: HashMap<String, usize>,
}

/// What one pass did.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct PassReport {
    /// Files decoded, whole or not.
    pub decoded: usize,
    /// Rows written.
    pub kept: usize,
    /// Seconds of audio measured.
    pub audio_seconds: f64,
    /// Time spent decoding.
    pub decoding: Duration,
    /// Nothing is due now.
    pub finished: bool,
}

impl PassReport {
    fn add(&mut self, other: &Self) {
        self.decoded += other.decoded;
        self.kept += other.kept;
        self.audio_seconds += other.audio_seconds;
        self.decoding += other.decoding;
    }

    /// One line on what a batch measured and how fast, for the heat check.
    fn log(&self) {
        if self.decoded == 0 {
            return;
        }
        let decoding = self.decoding.as_secs_f64();
        tracing::info!(
            files = self.decoded,
            kept = self.kept,
            audio_seconds = self.audio_seconds.round(),
            decoding_seconds = (decoding * 100.0).round() / 100.0,
            times_real_time = (decoding > 0.0).then(|| (self.audio_seconds / decoding).round()),
            "track loudness measured"
        );
    }
}

/// Whether a batch went on to its end.
enum Flow {
    Go,
    Stop,
}

/// The loudness service: the loop's passes and the levelling read.
#[derive(Clone)]
pub struct Loudness {
    db: Db,
    core: LibraryCore,
    coordinator: Arc<ScanCoordinator>,
    clock: Arc<dyn Clock>,
}

impl Loudness {
    pub(crate) fn new(
        db: Db,
        core: LibraryCore,
        coordinator: Arc<ScanCoordinator>,
        clock: Arc<dyn Clock>,
    ) -> Self {
        Self {
            db,
            core,
            coordinator,
            clock,
        }
    }

    /// Measure every due file, newest first, until none is left, a scan
    /// runs, the library is switched off or `stop` answers true. After
    /// each file it calls `rest` with how long to rest, which answers false
    /// to stop. Results are kept after every [`BATCH`] files; a busy
    /// database holds them in `sweep` for a later write. A pass that finds
    /// [`MOST_HELD`] results still held and the database still busy decodes
    /// nothing and answers `Busy`. Blocking.
    pub fn pass(
        &self,
        sweep: &mut Sweep,
        pace: &Pace,
        stop: &dyn Fn() -> bool,
        rest: &dyn Fn(Duration) -> bool,
    ) -> Result<PassReport, LoudnessError> {
        let mut report = PassReport::default();
        match self.keep_held(sweep) {
            Ok(kept) => report.kept += kept,
            Err(LoudnessError::Busy(cause)) if sweep.held.len() >= MOST_HELD => {
                return Err(LoudnessError::Busy(cause));
            }
            Err(LoudnessError::Busy(_)) => {}
            Err(error) => return Err(error),
        }
        loop {
            if self.give_way(stop) {
                return Ok(report);
            }
            let due = self.next_batch(sweep)?;
            if due.is_empty() {
                report.finished = true;
                return Ok(report);
            }
            let mut batch = PassReport::default();
            let flow = self.measure_batch(due, sweep, pace, stop, rest, &mut batch);
            let kept = self.keep_held(sweep);
            if let Ok(count) = kept {
                batch.kept += count;
            }
            batch.log();
            report.add(&batch);
            match kept {
                Ok(_) => {}
                Err(LoudnessError::Busy(cause)) if sweep.held.len() < MOST_HELD => {
                    tracing::info!(%cause, held = sweep.held.len(), "loudness results wait: the database is busy");
                }
                Err(error) => return Err(error),
            }
            if matches!(flow, Flow::Stop) {
                return Ok(report);
            }
        }
    }

    /// The next due tracks, leaving out those held, waiting, or under a
    /// library folder that is not there or is left alone for now.
    fn next_batch(&self, sweep: &mut Sweep) -> Result<Vec<sql::DueTrack>, LoudnessError> {
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

    fn measure_batch(
        &self,
        due: Vec<sql::DueTrack>,
        sweep: &mut Sweep,
        pace: &Pace,
        stop: &dyn Fn() -> bool,
        rest: &dyn Fn(Duration) -> bool,
        batch: &mut PassReport,
    ) -> Flow {
        for track in due {
            if sweep.roots_waiting.contains_key(&track.root_id) {
                continue;
            }
            if self.give_way(stop) {
                return Flow::Stop;
            }
            let started = Instant::now();
            let measured = measure(&track, stop);
            let took = started.elapsed();
            let wait_out = |sweep: &mut Sweep| {
                sweep.waiting.insert(
                    (track.id.clone(), track.stat_revision.clone()),
                    Instant::now() + UNREADABLE_WAIT,
                );
            };
            match measured {
                Measure::Stopped => return Flow::Stop,
                Measure::Wait { cause, missing } => {
                    tracing::info!(track_id = track.id, %cause, "a file's loudness waits an hour: it could not be read");
                    wait_out(sweep);
                    let in_a_row = sweep.missing.entry(track.root_id.clone()).or_default();
                    *in_a_row = if missing { *in_a_row + 1 } else { 0 };
                    if *in_a_row >= MISSING_IN_A_ROW {
                        tracing::warn!(
                            root_id = track.root_id,
                            "a library folder's files are not there; its loudness waits an hour"
                        );
                        sweep.missing.remove(&track.root_id);
                        sweep
                            .roots_waiting
                            .insert(track.root_id.clone(), Instant::now() + UNREADABLE_WAIT);
                    }
                }
                Measure::Changed => {
                    tracing::info!(
                        track_id = track.id,
                        "a file changed while its loudness was measured; it is measured once the scan reads it again"
                    );
                    batch.decoded += 1;
                    batch.decoding += took;
                    wait_out(sweep);
                    sweep.missing.remove(&track.root_id);
                }
                Measure::Done(outcome, measured) => {
                    batch.decoded += 1;
                    batch.decoding += took;
                    batch.audio_seconds += measured.as_ref().map_or(0.0, |m| m.seconds());
                    sweep.missing.remove(&track.root_id);
                    sweep.held.push(Found {
                        track_id: track.id,
                        stat_revision: track.stat_revision,
                        outcome,
                        peak: measured.as_ref().map(|m| m.peak),
                        blocks: measured.as_ref().map(|m| m.summary().encode()),
                        at_ms: self.clock.now_ms(),
                    });
                }
            }
            let wait = took.mul_f64(pace.rest_per_spent).max(pace.min_rest);
            if !rest(wait) {
                return Flow::Stop;
            }
        }
        Flow::Go
    }

    /// A stop, a scan that runs or waits, or a library switched off.
    fn give_way(&self, stop: &dyn Fn() -> bool) -> bool {
        stop() || !self.coordinator.current().is_empty() || self.core.switched_off()
    }

    /// The library folders that are there now. A folder that is not (a
    /// share not mounted) has its tracks left until it is back.
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
    fn due(&self, roots: &[String], limit: usize) -> Result<Vec<sql::DueTrack>, LoudnessError> {
        let roots = serde_json::to_string(roots)
            .map_err(|error| LoudnessError::Failed(error.to_string()))?;
        let limit = i64::try_from(limit).unwrap_or(i64::MAX);
        self.db
            .read_long("library.loudness_due", &CancelFlag::never(), |conn| {
                conn.prepare_cached(sql::DUE.text())?
                    .query_map(rusqlite::params![roots, limit], |row| {
                        sql::DueTrack::read(row)
                    })?
                    .collect::<Result<Vec<_>, _>>()
                    .map_err(LoudnessError::from)
            })
    }

    /// Write the held results in one short transaction; each lands only
    /// while its track is at the revision it was measured at. A busy
    /// database keeps them held; any other failure drops them, and their
    /// files are due again. Answers the rows written.
    fn keep_held(&self, sweep: &mut Sweep) -> Result<usize, LoudnessError> {
        if sweep.held.is_empty() {
            return Ok(0);
        }
        let held = &sweep.held;
        let kept = self
            .db
            .write_blocking(Lane::Background, "library.loudness_keep", |tx| {
                let mut keep = tx.prepare_cached(sql::KEEP.text())?;
                let mut kept = 0;
                for found in held {
                    kept += keep.execute(rusqlite::params![
                        found.track_id,
                        found.stat_revision,
                        found.outcome,
                        found.peak,
                        found.blocks,
                        found.at_ms,
                    ])?;
                }
                Ok::<_, LoudnessError>(kept)
            });
        if !matches!(kept, Err(LoudnessError::Busy(_))) {
            sweep.held.clear();
        }
        kept
    }

    /// The levelling numbers for indexed track `track_id`: the measured
    /// ones where its current bytes were measured, else its ReplayGain
    /// tags. The album's are worked out from every indexed track of the
    /// album once each has a row for its current bytes (unmeasurable ones
    /// left out), else they are the track's album tags. `None` when the
    /// track is not indexed. Blocking.
    pub fn levels(&self, track_id: &str) -> Result<Option<TrackLevels>, LoudnessError> {
        let rows = self.db.read_blocking("library.loudness_levels", |conn| {
            conn.prepare_cached(sql::ALBUM_LEVELS.text())?
                .query_map([track_id], sql::AlbumLevel::read)?
                .collect::<Result<Vec<_>, _>>()
                .map_err(LoudnessError::from)
        })?;
        Ok(levels_of(track_id, &rows))
    }
}

/// A measurement as its row holds it, when it reads.
fn measured_of(row: &sql::AlbumLevel) -> Option<Result<(f64, BlockSummary), ()>> {
    match row.outcome? {
        LoudnessOutcome::Unmeasurable => Some(Err(())),
        LoudnessOutcome::Measured => {
            let summary = row.blocks.as_deref().map(BlockSummary::decode);
            match (row.peak, summary) {
                (Some(peak), Some(Ok(summary))) => Some(Ok((peak, summary))),
                _ => {
                    tracing::warn!(
                        track_id = row.id,
                        "a stored loudness row does not read; the file's tags are used"
                    );
                    None
                }
            }
        }
    }
}

fn levels_of(track_id: &str, rows: &[sql::AlbumLevel]) -> Option<TrackLevels> {
    let asked = rows.iter().find(|row| row.id == track_id)?;
    let rounded = |value: f64| (value * 100.0).round() / 100.0;
    let mut levels = match measured_of(asked) {
        Some(Ok((peak, summary))) => TrackLevels {
            track_gain_db: summary
                .loudness()
                .map(|value| rounded(loudness::gain(value))),
            track_peak: Some(peak),
            ..TrackLevels::default()
        },
        _ => TrackLevels {
            track_gain_db: asked.tag_track_gain,
            track_peak: asked.tag_track_peak,
            ..TrackLevels::default()
        },
    };
    let mut album: Option<(f64, BlockSummary)> = None;
    let mut whole = true;
    for row in rows {
        match measured_of(row) {
            None => whole = false,
            Some(Err(())) => {}
            Some(Ok((peak, summary))) => match album.as_mut() {
                Some((top, all)) => {
                    *top = top.max(peak);
                    all.merge(&summary);
                }
                None => album = Some((peak, summary)),
            },
        }
    }
    match album.filter(|_| whole) {
        Some((peak, summary)) => {
            levels.album_gain_db = summary
                .loudness()
                .map(|value| rounded(loudness::gain(value)));
            levels.album_peak = Some(peak);
        }
        None => {
            levels.album_gain_db = asked.tag_album_gain;
            levels.album_peak = asked.tag_album_peak;
        }
    }
    Some(levels)
}

/// What measuring one file came to.
enum Measure {
    /// Measured, or unmeasurable until the file changes (no numbers).
    Done(LoudnessOutcome, Option<loudness::TrackLoudness>),
    /// The file could not be read now: try again later. `missing` when it
    /// is not there at all.
    Wait { cause: String, missing: bool },
    /// The file changed while it was read.
    Changed,
    /// The pass was asked to stop.
    Stopped,
}

/// Measure one due track's file, read beneath its library folder. The
/// file's size and modification time are read before and after and must
/// be the ones the scan read; a file that changed on the way gives no
/// result. A file that reads short with its stat unchanged is cut short
/// for good, not mid-copy, and is unmeasurable until it changes.
fn measure(track: &sql::DueTrack, stop: &dyn Fn() -> bool) -> Measure {
    let Some(file) = TrackFile::in_library(Path::new(&track.file_path), &track.relative_path)
    else {
        return Measure::Wait {
            cause: "its path lies outside its library folder".to_owned(),
            missing: false,
        };
    };
    let before = match stat(file) {
        Ok(before) => before,
        Err(error) => {
            return Measure::Wait {
                missing: error.kind() == std::io::ErrorKind::NotFound,
                cause: error.to_string(),
            };
        }
    };
    if !as_scanned(track, before) {
        return Measure::Wait {
            cause: "its size or modification time is not what the scan read".to_owned(),
            missing: false,
        };
    }
    let found = loudness::analyse_until(file, Instant::now() + FILE_BUDGET, stop);
    if !matches!(found, Err(Unanalysed::Stopped)) && stat(file).ok() != Some(before) {
        return Measure::Changed;
    }
    match found {
        Ok(found) => Measure::Done(LoudnessOutcome::Measured, Some(found)),
        Err(Unanalysed::Undecodable | Unanalysed::OutOfTime | Unanalysed::Unreadable) => {
            Measure::Done(LoudnessOutcome::Unmeasurable, None)
        }
        Err(Unanalysed::Stopped) => Measure::Stopped,
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

#[cfg(test)]
mod tests {
    use super::*;

    /// A summary of `blocks` 400 ms blocks at `lufs`, as a row stores it.
    fn blocks_at(lufs: f64, blocks: u32) -> Vec<u8> {
        let energy = 10f64.powf((lufs + 0.691) / 10.0);
        let mut bytes = vec![1u8, (lufs + 70.0).floor() as u8];
        bytes.extend_from_slice(&blocks.to_le_bytes());
        bytes.extend_from_slice(&(energy * f64::from(blocks)).to_le_bytes());
        bytes
    }

    fn row(id: &str, outcome: Option<LoudnessOutcome>, lufs: f64, peak: f64) -> sql::AlbumLevel {
        let measured = outcome == Some(LoudnessOutcome::Measured);
        sql::AlbumLevel {
            id: id.to_owned(),
            tag_track_gain: Some(-7.0),
            tag_track_peak: Some(0.9),
            tag_album_gain: Some(-6.0),
            tag_album_peak: Some(0.95),
            outcome,
            peak: measured.then_some(peak),
            blocks: measured.then(|| blocks_at(lufs, 100)),
        }
    }

    /// The album part is worked out from the tracks' buckets only once
    /// every track has a row for its current bytes; until then it is the
    /// album tags. An unmeasurable track is left out of the album figure.
    #[test]
    fn the_album_figure_waits_for_every_track_and_leaves_out_the_unmeasurable() {
        let measured = Some(LoudnessOutcome::Measured);
        let waiting = [row("a", measured, -20.0, 0.5), row("b", None, 0.0, 0.0)];
        assert_eq!(
            levels_of("a", &waiting),
            Some(TrackLevels {
                track_gain_db: Some(2.0),
                track_peak: Some(0.5),
                album_gain_db: Some(-6.0),
                album_peak: Some(0.95),
            })
        );
        assert_eq!(
            levels_of("b", &waiting),
            Some(TrackLevels {
                track_gain_db: Some(-7.0),
                track_peak: Some(0.9),
                album_gain_db: Some(-6.0),
                album_peak: Some(0.95),
            })
        );

        let one_unmeasurable = [
            row("a", measured, -20.0, 0.5),
            row("b", Some(LoudnessOutcome::Unmeasurable), 0.0, 0.0),
        ];
        let levels = levels_of("a", &one_unmeasurable).expect("indexed");
        assert_eq!(
            (levels.album_gain_db, levels.album_peak),
            (Some(2.0), Some(0.5))
        );

        let both = [
            row("a", measured, -20.0, 0.5),
            row("b", measured, -14.0, 0.8),
        ];
        let levels = levels_of("a", &both).expect("indexed");
        let mean = (10f64.powf((-20.0 + 0.691) / 10.0) + 10f64.powf((-14.0 + 0.691) / 10.0)) / 2.0;
        let album = -18.0 - (-0.691 + 10.0 * mean.log10());
        assert_eq!(levels.album_gain_db, Some((album * 100.0).round() / 100.0));
        assert_eq!(levels.album_peak, Some(0.8));
        assert_eq!(levels_of("c", &both), None);
    }
}
