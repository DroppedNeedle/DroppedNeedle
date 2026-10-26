//! One way into the library for every file, whichever door it came
//! through: a finished download (the landing), an upload (drop import) or
//! a held file a person imports.
//!
//! A door finds and matches its files, then builds an [`IntakeBatch`] and
//! a [`Pairing`] and calls [`place`]. `place` owns everything after the
//! match: version replacement, the upgrade comparison, the copy the files
//! join, the fingerprint gate, the one library import and the per-file
//! outcome. The same pairing and gate give the same outcome whichever door
//! built them. The door keeps what is its own: how files are found and
//! matched, where held files are written, removing what was consumed
//! ([`dispose`]) and settling its task, job or request.

pub mod dispose;
mod place;

use std::path::PathBuf;

pub use self::place::place;
use crate::downloads::plan::PlannedAlbum;
use crate::landing::probe::LandedFile;
use crate::landing::specs::Check;
use crate::reason::Reason;
use droppedneedle_library::matching::Release;

/// Who decided the files go in, which decides the checks `place` runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Gate {
    /// The app decided (a landing, an upload it matched by itself). Every
    /// file about to import is fingerprinted; one AcoustID hears as
    /// another recording is held.
    Automatic,
    /// A person picked the album and the app paired the files. A file
    /// that would replace a library copy needs AcoustID to name its
    /// recording: no answer keeps it held, another recording leaves the
    /// library's copy as it is.
    PersonChoseAlbum,
    /// A person imported held files they have seen, each paired to its
    /// track. The check that held a file is skipped, the comparisons with
    /// the library's copy still run, and a file is never dropped as
    /// already owned: one no better than the library's copy stays held.
    PersonChoseFile,
    /// The held list retried, by itself, files our side could not take:
    /// as [`Gate::PersonChoseFile`], except that a track the library
    /// already holds, on another copy of the album or now on this one, is
    /// held for a person: never placed beside it, and never replaced unless
    /// the file was held because the recycle bin refused the old copy.
    HeldRetry,
}

impl Gate {
    /// A held file imported as it is, by a person or by the held list's
    /// retry.
    pub fn imports_held(self) -> bool {
        matches!(self, Self::PersonChoseFile | Self::HeldRetry)
    }
}

/// Where the files came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Origin {
    Download {
        task_id: String,
        /// The task asked for a better copy of what the library holds.
        upgrade: bool,
        /// Name of the import's staging folder when it must differ from
        /// the task id (a held file imported while its task may land
        /// again).
        staging: Option<String>,
        /// The client's downloads folders (real paths) the files are read
        /// beneath, never through a link below them. `None` when they sit
        /// in the app's own held folder.
        downloads_folders: Option<Vec<PathBuf>>,
    },
    Upload {
        drop_item_id: i64,
    },
}

/// One file of a batch.
#[derive(Debug, Clone)]
pub struct IntakeFile {
    pub file: LandedFile,
    /// The hold a person is importing the file from, under
    /// [`Gate::PersonChoseFile`].
    pub held_for: Option<Reason>,
}

/// What the files are for.
#[derive(Debug, Clone)]
pub struct Target {
    /// The release the files were paired to; they are tagged from it.
    pub release: Release,
    /// The library copy the files were fetched for, which they join, with
    /// the other versions on it they replace.
    pub join: Option<PlannedAlbum>,
    /// `(disc, position)` of owned tracks the download asked for again
    /// ("Get missing tracks" below the cutoff): compared for an upgrade.
    pub asked: Vec<(u32, u32)>,
}

/// Files to place, all from one origin and so one plan.
#[derive(Debug, Clone)]
pub struct IntakeBatch {
    pub files: Vec<IntakeFile>,
    pub target: Target,
    pub origin: Origin,
    pub gate: Gate,
    /// The user the work is done for; replaced tracks are recorded as
    /// theirs.
    pub owner: String,
}

/// A file the door's own checks held, with the track it was paired to.
#[derive(Debug, Clone)]
pub struct Hold {
    pub file: usize,
    pub track: Option<usize>,
    pub reason: Reason,
    pub detail: String,
}

/// The door's match: batch files (by index) against release tracks (by
/// index). A file in none of the lists is left as it is.
#[derive(Debug, Clone, Default)]
pub struct Pairing {
    /// Files on tracks the library lacks.
    pub import: Vec<(usize, usize)>,
    /// Files on tracks the library already holds.
    pub owned: Vec<(usize, usize)>,
    /// Files the door's checks held.
    pub held: Vec<Hold>,
    /// Files the release does not list, filed beside the album.
    pub extras: Vec<usize>,
}

/// What became of one batch file.
#[derive(Debug, Clone, PartialEq)]
pub enum Outcome {
    /// Not in the pairing; nothing was done with it.
    Untouched,
    /// In the library now. `track` is `None` for an extra file;
    /// `upgraded` when it replaced a library copy.
    Imported {
        track: Option<usize>,
        upgraded: bool,
    },
    /// The library already holds the track as well or better.
    AlreadyOwned { track: usize },
    /// Kept out of the library for a person.
    Held {
        track: Option<usize>,
        reason: Reason,
        detail: String,
    },
}

/// What `place` did with a batch.
#[derive(Debug, Clone)]
pub struct Placement {
    /// One outcome per batch file, by index.
    pub outcomes: Vec<Outcome>,
    /// The held files, in the order they were held.
    pub held: Vec<usize>,
    /// Where the imported files now live.
    pub paths: Vec<PathBuf>,
    /// Of the imported files, those placed in a folder that keeps its own
    /// tags.
    pub tags_kept: usize,
    /// The AcoustID check, when AcoustID answered for an automatic batch.
    pub fingerprint: Option<Check>,
}

impl Placement {
    /// Batch files with `outcome` matching.
    pub fn files(&self, keep: impl Fn(&Outcome) -> bool) -> Vec<usize> {
        self.outcomes
            .iter()
            .enumerate()
            .filter(|(_, outcome)| keep(outcome))
            .map(|(index, _)| index)
            .collect()
    }
}

/// Nothing was placed: the whole batch waits for `reason`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Deferred {
    /// `library_changed` (the library changed under the import twice),
    /// `naming_script_failed` or `local_fault`.
    pub reason: Reason,
    pub detail: String,
}
