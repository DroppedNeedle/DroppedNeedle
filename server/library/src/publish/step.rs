//! The steps a publish and its recovery share.
//!
//! Publishing and recovery run one set of steps: [`Run::advance`] reads
//! what a bundle's journal rows and the files on disk show, once, decides
//! the next step for the bundle and each row ([`decide`], which reads
//! nothing), and acts on it. A publish stages its files and then calls it
//! from `Staged`; recovery calls it from wherever a crash left the bundle.
//! The same rows and the same files give the same steps from either.
//!
//! Only an unexpected error (a file operation, the store) is handled by
//! the caller: a publish unwinds and reports it, since its caller was told
//! the write failed; recovery leaves the bundle for its next pass. A
//! refusal (a file in the way, no safe way to place) unwinds from both.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use droppedneedle_kernel::db::Db;
use droppedneedle_kernel::jobs::queue::JobQueue;

use super::PublishError;
use super::journal::{
    FileJournal, JournalKind, JournalState, JournalStore, JournalWriter, MgmtState,
};
use super::paths::{self, FileStat, Sandbox};
use super::place::{
    Original, PlaceError, Seams, SetAsideError, place_from_hidden, place_with, set_aside_with,
};
use super::planner::PlanBundle;
use super::publisher::{
    BundleCommit, CrashPoint, TrackCommit, commit_bundle, durable, enqueue_after_commit, locate,
    long_read, mark_missing, probe_root, read_adopted,
};
use super::recovery::{
    compensate_bundle, flag_attention, flag_rows, placements_first, removals_first, remove_folders,
};

/// Where a commit's imported files say they came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum AdoptionFrom {
    /// The plan item's own adoption: the publish has it in hand.
    Plan,
    /// The provenance the import wrote beside its staged copies, with the
    /// policy the plan recorded: what recovery reads.
    Provenance,
}

/// Test hooks a publish passes. Recovery passes none, and production
/// never arms a crash point.
#[derive(Default)]
pub(super) struct Hooks<'a> {
    pub crash: Option<CrashPoint>,
    pub before_place: Option<&'a dyn Fn(&Path)>,
    pub before_set_aside: Option<&'a dyn Fn(&Path)>,
    /// Past this instant, or once the flag is set, an added file is not
    /// placed.
    pub add_budget: Option<&'a (Instant, Arc<AtomicBool>)>,
}

/// Everything one run of the steps needs. A publish fills it from its own
/// state; recovery from what it can read.
pub(super) struct Run<'a> {
    pub db: &'a Db,
    pub sandbox: &'a Sandbox,
    pub jobs: &'a JobQueue,
    pub seams: Seams,
    /// Seams for a replaced file's move into its bin.
    pub bin_seams: Seams,
    /// Unix milliseconds, read by the caller.
    pub now_ms: i64,
    /// Wall time, for stamps read as each step runs.
    pub clock: &'a dyn droppedneedle_kernel::clock::Clock,
    pub adoption: AdoptionFrom,
    /// Whether the plan's identities of the files a removal takes away
    /// are trusted before they move: a publish read them itself, recovery
    /// cannot know what happened since.
    pub known_removals: bool,
    /// Each put-back temp's identity as this publish wrote it.
    pub temps: &'a BTreeMap<String, FileStat>,
    /// Hashes this publish worked out; empty in recovery.
    pub known: &'a paths::Known,
    pub hooks: Hooks<'a>,
}

/// A row the steps refused to finish: why, as recovery reports it, and
/// the error a publish returns.
#[derive(Debug)]
pub(super) struct Refused {
    pub reason: String,
    pub error: PublishError,
}

/// How a run ended.
#[derive(Debug)]
pub(super) enum Advanced {
    /// Every row was already settled.
    Noop,
    /// Every row was settled, but folders the bundle made wait for their
    /// library folder.
    FoldersWait,
    /// A root the bundle names is not usable now; nothing moved.
    Waiting(String),
    /// The evidence was ambiguous; every live row needs attention.
    Attention(String),
    /// Unwound before its commit, with the refusal that caused it, if any,
    /// and why a file was kept under a hidden name, if one was.
    Unwound {
        refused: Option<Refused>,
        kept: Option<String>,
    },
    /// Its catalog commit was refused, nothing committed, and the bundle
    /// unwound.
    CommitRefused(PublishError),
    /// Committed in this run; `pending` when some cleanup stays pending.
    Committed { pending: bool },
    /// Found committed, verified, and cleaned up.
    CleanupFinished,
}

/// What one pre-commit placement row's staged temp shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Temp {
    /// No regular file under the hidden name.
    Missing,
    /// The staged bytes, a file of its own.
    Staged,
    /// The staged bytes, and the same file as the destination: a
    /// link-then-unlink placement that stopped halfway.
    SameAsDest,
    /// Other bytes.
    Other,
}

/// What one pre-commit placement row's destination holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Dest {
    /// No regular file.
    Free,
    /// The staged bytes, which are also the source's (no tag change).
    StagedAlsoSource,
    /// The staged bytes.
    Staged,
    /// The source's bytes.
    Source,
    /// The bytes of the file the row replaces there.
    Replaced,
    /// A file a sibling removal takes away, not set aside yet.
    Removed,
    /// Anything else.
    Other,
}

/// What one pre-commit placement row's backup shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Backup {
    /// None journaled, not there, or holding the bytes it should.
    Fine,
    /// Other bytes.
    Changed,
    /// Outside the library folders.
    Escapes,
}

/// A pre-commit placement row's evidence.
#[derive(Debug, Clone, Copy)]
pub(super) struct PlaceEvidence {
    pub temp: Temp,
    pub dest: Dest,
    pub backup: Backup,
    /// The row writes its file in place (source is destination, or a
    /// case-only alias of it).
    pub in_place: bool,
    /// A write in place with no tag change, before its original was set
    /// aside: the destination is that original.
    pub unchanged_original: bool,
    pub source_safe: bool,
}

/// A generated file being taken away (a `delete` row).
#[derive(Debug, Clone, Copy)]
pub(super) struct GeneratedEvidence {
    pub in_place: bool,
    pub aside: bool,
    /// One file under both names.
    pub linked: bool,
    pub dest_present: bool,
    pub aside_present: bool,
}

/// What one row shows, read once when a run starts.
pub(super) enum RowEvidence {
    /// Committed or terminal: nothing read for it on this path.
    Settled,
    /// A path that is unsafe or outside the library folders.
    Unsafe(String),
    Place(PlaceEvidence),
    Generated(GeneratedEvidence),
    Removal(RemovalEvidence),
    PutBack(super::put_back::Evidence),
    /// An added file, and whether its temp is gone (placed by rename).
    Add(super::add::Evidence, bool),
}

/// What a removal's pre-commit row shows on disk.
#[derive(Debug)]
pub(super) enum RemovalEvidence {
    /// Not set aside yet and still in place, or set aside with its bytes:
    /// a resume may finish it. A resume never touches the file's own name
    /// once the file is set aside, so whatever sits there now is left.
    Resume,
    /// The file never left (one file under both names, an interrupted
    /// link), it changed or went before it was set aside, or an unwind
    /// already put it home: the bundle unwinds, which touches nothing that
    /// is not the removal's.
    Unwind,
    /// A hidden copy that is not the removed file.
    Ambiguous(String),
}

/// The next step for one row.
pub(super) enum RowStep {
    /// Nothing to do before the commit.
    Skip,
    /// The file is placed; only its row lags.
    RecordPlaced,
    /// Place the staged temp onto the free destination.
    Place,
    /// Set the file at the destination aside under the row's backup, then
    /// place.
    SetAsideThenPlace,
    /// Set the removed file aside under its hidden name.
    SetAsideRemoval,
    /// Put a removed file's copy home.
    PlacePutBack,
    /// Set the generated file aside.
    SetAsideGenerated,
    /// The row cannot finish; the bundle unwinds.
    Unwind,
    /// Another file is in the way; the bundle unwinds without touching it.
    Refuse(Refused),
    /// The evidence is ambiguous.
    Attention(String),
}

/// The next step for the bundle.
pub(super) enum BundleStep {
    Attention(String),
    /// Some row committed: verify and clean up.
    Committed,
    /// Unwind, with a reason to log and the refusal, if one caused it.
    Unwind(String, Option<Refused>),
    /// Finish every row in order, then commit.
    Resume(Vec<RowStep>),
}

/// The bundle's rows, read with what they show.
pub(super) struct Gathered {
    pub rows: Vec<RowEvidence>,
    /// An earlier unwind found no safe way to put a file back.
    pub put_back_refused: bool,
}

/// Decide the bundle's next step from its rows, what they show and its
/// recorded plan. Reads nothing.
pub(super) fn decide(
    bundle_id: &str,
    journals: &[FileJournal],
    gathered: Gathered,
    plan: Option<&PlanBundle>,
) -> BundleStep {
    let mut steps = Vec::with_capacity(journals.len());
    for (journal, evidence) in journals.iter().zip(gathered.rows) {
        let step = row_step(journal, evidence);
        if let RowStep::Attention(reason) = step {
            return BundleStep::Attention(reason);
        }
        steps.push(step);
    }
    if journals.iter().any(|journal| {
        matches!(
            journal.state,
            JournalState::Committed | JournalState::CleanupPending
        )
    }) {
        return BundleStep::Committed;
    }
    // A compensated or needs-attention row means an unwind already began
    // and stopped, between two rows or on an error. Such a bundle can
    // never commit, so it is never resumed.
    let unwinding = journals.iter().any(|journal| {
        matches!(
            journal.state,
            JournalState::Compensated | JournalState::NeedsAttention
        )
    });
    if unwinding && gathered.put_back_refused {
        return BundleStep::Attention(format!(
            "bundle {bundle_id} has a file with no safe way back onto its name"
        ));
    }
    if unwinding {
        return BundleStep::Unwind("an unwind began and stopped".into(), None);
    }
    let refused = steps
        .iter()
        .position(|step| matches!(step, RowStep::Refuse(_)))
        .and_then(|index| match steps.swap_remove(index) {
            RowStep::Refuse(refused) => Some(refused),
            RowStep::Skip
            | RowStep::RecordPlaced
            | RowStep::Place
            | RowStep::SetAsideThenPlace
            | RowStep::SetAsideRemoval
            | RowStep::PlacePutBack
            | RowStep::SetAsideGenerated
            | RowStep::Unwind
            | RowStep::Attention(_) => None,
        });
    if let Some(refused) = refused {
        return BundleStep::Unwind("a file is in the way".into(), Some(refused));
    }
    if steps.iter().any(|step| matches!(step, RowStep::Unwind)) {
        return BundleStep::Unwind("a file cannot be finished".into(), None);
    }
    let Some(plan) = plan.filter(|plan| staged_in_full(plan, journals)) else {
        return BundleStep::Unwind("it stopped before every file staged".into(), None);
    };
    if let Some(reason) = commit_shape(journals, plan) {
        return BundleStep::Unwind(
            reason.clone(),
            Some(Refused {
                reason: format!("bundle {bundle_id} cannot commit: {reason}"),
                error: PublishError::Journal(
                    format!("bundle {bundle_id} cannot commit: {reason}").into(),
                ),
            }),
        );
    }
    BundleStep::Resume(steps)
}

/// Whether every row `prepare` writes for the plan exists and is past
/// `Prepared` (its temp written and its before-state kept). A bundle that
/// stopped partway through staging can look complete on disk, so only its
/// recorded plan can tell.
fn staged_in_full(plan: &PlanBundle, journals: &[FileJournal]) -> bool {
    if journals
        .iter()
        .any(|journal| journal.state == JournalState::Prepared)
    {
        return false;
    }
    let present: BTreeSet<&str> = journals.iter().map(|journal| journal.id.as_str()).collect();
    super::publisher::journal_ids(plan)
        .iter()
        .all(|id| present.contains(id.as_str()))
}

/// Why a bundle could never commit, from its rows and plan alone: it has
/// nothing to commit, or a track it moves has no seal to check the catalog
/// against.
fn commit_shape(journals: &[FileJournal], plan: &PlanBundle) -> Option<String> {
    let tracks: Vec<(&str, MgmtState)> = journals
        .iter()
        .filter_map(|journal| journal.track_id.as_deref().zip(journal.commit_as))
        .collect();
    if tracks.is_empty()
        && plan.removal.is_none()
        && plan.put_back.is_none()
        && plan.added.is_none()
    {
        return Some("it has no committable track".to_owned());
    }
    tracks
        .iter()
        .find(|(track_id, mgmt)| {
            *mgmt != MgmtState::Imported
                && plan
                    .items
                    .iter()
                    .find(|item| item.track_id == *track_id)
                    .and_then(|item| item.commit_seal())
                    .is_none()
        })
        .map(|(track_id, _)| format!("track {track_id} has no seal"))
}

/// A file holds the destination of a row that was never placed (its
/// staged temp is still a file of its own). It is not ours: the bundle
/// unwinds, which takes only our own checked temp and leaves that file as
/// it is.
fn unplaced_destination_taken(journal: &FileJournal) -> RowStep {
    RowStep::Refuse(Refused {
        reason: format!(
            "another file holds the destination of journal {}",
            journal.id
        ),
        error: PublishError::Collision(
            format!(
                "late occupier at {}:{}",
                journal.dest_root, journal.dest_rel
            )
            .into(),
        ),
    })
}

/// A removal, put-back or added file that cannot go ahead because its
/// file changed or its name is taken: the bundle unwinds with the
/// collision the caller reads as such.
fn taken_or_changed(journal: &FileJournal, what: &str) -> RowStep {
    RowStep::Refuse(Refused {
        reason: format!(
            "journal {}: {}:{} {what}",
            journal.id, journal.dest_root, journal.dest_rel
        ),
        error: PublishError::Collision(
            format!("{}:{} {what}", journal.dest_root, journal.dest_rel).into(),
        ),
    })
}

fn row_step(journal: &FileJournal, evidence: RowEvidence) -> RowStep {
    let published = journal.state == JournalState::Published;
    let staged = journal.state == JournalState::Staged;
    match evidence {
        RowEvidence::Settled => RowStep::Skip,
        RowEvidence::Unsafe(reason) => RowStep::Attention(reason),
        RowEvidence::Place(evidence) => place_step(journal, evidence),
        RowEvidence::Generated(evidence) => {
            // One file under both names: an unwind linked it home and
            // stopped, or the link test before a set-aside stopped before
            // removing its link. In both the file is in place.
            if evidence.in_place && evidence.linked {
                return RowStep::Unwind;
            }
            if (evidence.in_place && evidence.aside_present)
                || (evidence.aside && evidence.dest_present)
                || (!evidence.in_place && evidence.dest_present)
                || (!evidence.aside && evidence.aside_present)
            {
                return RowStep::Attention(format!(
                    "journal {} generated file changed",
                    journal.id
                ));
            }
            match (evidence.in_place, evidence.aside, published) {
                (false, false, false | true) => RowStep::Unwind,
                (true | false, true | false, true) => RowStep::Skip,
                (true | false, true | false, false) => RowStep::SetAsideGenerated,
            }
        }
        RowEvidence::Removal(evidence) => match (evidence, published) {
            (RemovalEvidence::Ambiguous(reason), false | true) => RowStep::Attention(reason),
            (RemovalEvidence::Unwind, true) => RowStep::Unwind,
            (RemovalEvidence::Unwind, false) => {
                taken_or_changed(journal, "changed while it was being removed")
            }
            (RemovalEvidence::Resume, true) => RowStep::Skip,
            (RemovalEvidence::Resume, false) => RowStep::SetAsideRemoval,
        },
        RowEvidence::PutBack(evidence) => match (evidence, published) {
            (super::put_back::Evidence::Ambiguous(reason), false | true) => {
                RowStep::Attention(reason)
            }
            (super::put_back::Evidence::Unwind, true) => RowStep::Unwind,
            (super::put_back::Evidence::Unwind, false) if staged => {
                taken_or_changed(journal, "is taken by another file")
            }
            (super::put_back::Evidence::Unwind, false) => RowStep::Unwind,
            (super::put_back::Evidence::Resume, true) => RowStep::Skip,
            (super::put_back::Evidence::Resume, false) => RowStep::PlacePutBack,
        },
        RowEvidence::Add(evidence, temp_gone) => match (evidence, published, temp_gone) {
            (super::add::Evidence::Ambiguous(reason), false | true, false | true) => {
                RowStep::Attention(reason)
            }
            (super::add::Evidence::Unwind, true, false | true) => RowStep::Unwind,
            (super::add::Evidence::Unwind, false, false | true) if staged => {
                taken_or_changed(journal, "is taken by another file")
            }
            (super::add::Evidence::Unwind, false, false | true) => RowStep::Unwind,
            (super::add::Evidence::Resume, true, false | true) => RowStep::Skip,
            (super::add::Evidence::Resume, false, true) => RowStep::RecordPlaced,
            (super::add::Evidence::Resume, false, false) => RowStep::Place,
        },
    }
}

/// The step for a pre-commit placement row. Every combination of state,
/// temp and destination is named: one not covered by a rule needs
/// attention.
fn place_step(journal: &FileJournal, evidence: PlaceEvidence) -> RowStep {
    let foreign = || {
        RowStep::Attention(format!(
            "journal {} destination holds foreign bytes",
            journal.id
        ))
    };
    let duplicate = || {
        RowStep::Attention(format!(
            "journal {} has duplicate staged locations",
            journal.id
        ))
    };
    let impossible = || {
        RowStep::Attention(format!(
            "journal {} shows one file with two contents",
            journal.id
        ))
    };
    let users_copy = || unplaced_destination_taken(journal);
    use Dest as D;
    use JournalState as S;
    use Temp as T;
    let step = match (journal.state, evidence.temp, evidence.dest) {
        // Still being staged, its temp part written (a tag write's row
        // learns its hash only once staged): it is our own part copy, and
        // the unwind takes it.
        (S::Prepared, T::Other, D::Other) => foreign(),
        (
            S::Prepared,
            T::Other,
            D::Free | D::Staged | D::StagedAlsoSource | D::Source | D::Replaced | D::Removed,
        ) => RowStep::Unwind,
        (
            S::Staged
            | S::Published
            | S::Committed
            | S::CleanupPending
            | S::Cleaned
            | S::Compensated
            | S::NeedsAttention,
            T::Other,
            D::Free
            | D::Staged
            | D::StagedAlsoSource
            | D::Source
            | D::Replaced
            | D::Removed
            | D::Other,
        ) => {
            return RowStep::Attention(format!("journal {} staged bytes changed", journal.id));
        }
        // Still being staged: the bundle unwinds.
        (S::Prepared, T::Missing | T::SameAsDest, D::Other) => foreign(),
        (S::Prepared, T::Staged, D::Other) => foreign(),
        // An unchanged write in place: the destination is the original.
        (S::Prepared, T::Staged, D::StagedAlsoSource) if evidence.unchanged_original => {
            RowStep::Unwind
        }
        (S::Prepared, T::Staged, D::Staged | D::StagedAlsoSource) => duplicate(),
        (
            S::Prepared,
            T::Missing,
            D::Free | D::Staged | D::StagedAlsoSource | D::Source | D::Replaced | D::Removed,
        ) => RowStep::Unwind,
        (
            S::Prepared,
            T::SameAsDest,
            D::Free | D::Staged | D::StagedAlsoSource | D::Source | D::Replaced | D::Removed,
        ) => RowStep::Unwind,
        (S::Prepared, T::Staged, D::Free | D::Source | D::Replaced | D::Removed) => RowStep::Unwind,
        // Staged, not placed yet.
        (S::Staged, T::Missing, D::Staged | D::StagedAlsoSource) => RowStep::RecordPlaced,
        (S::Staged, T::Missing, D::Free | D::Source | D::Replaced | D::Removed) => RowStep::Unwind,
        (S::Staged, T::Missing, D::Other) => foreign(),
        (S::Staged, T::SameAsDest, D::Staged | D::StagedAlsoSource) => RowStep::Place,
        (S::Staged, T::SameAsDest, D::Free | D::Source | D::Replaced | D::Removed | D::Other) => {
            impossible()
        }
        (S::Staged, T::Staged, D::Free | D::Removed) => RowStep::Place,
        (S::Staged, T::Staged, D::Replaced) => RowStep::SetAsideThenPlace,
        (S::Staged, T::Staged, D::Source) if evidence.in_place => RowStep::SetAsideThenPlace,
        (S::Staged, T::Staged, D::Source) => users_copy(),
        (S::Staged, T::Staged, D::StagedAlsoSource) if evidence.unchanged_original => {
            RowStep::SetAsideThenPlace
        }
        (S::Staged, T::Staged, D::StagedAlsoSource | D::Staged | D::Other) => {
            unplaced_destination_taken(journal)
        }
        // Placed.
        (S::Published, T::Missing | T::SameAsDest, D::Staged | D::StagedAlsoSource) => {
            RowStep::Skip
        }
        (S::Published, T::Missing, D::Free | D::Source | D::Replaced | D::Removed) => {
            RowStep::Unwind
        }
        (S::Published, T::Missing | T::SameAsDest | T::Staged, D::Other) => foreign(),
        (S::Published, T::SameAsDest, D::Free | D::Source | D::Replaced | D::Removed) => {
            impossible()
        }
        (S::Published, T::Staged, D::Staged | D::StagedAlsoSource) => duplicate(),
        // An unwind took the placed copy off and stopped before the temp
        // went.
        (S::Published, T::Staged, D::Free | D::Source | D::Replaced | D::Removed) => {
            RowStep::Unwind
        }
        (
            S::Committed | S::CleanupPending | S::Cleaned | S::Compensated | S::NeedsAttention,
            T::Missing | T::Staged | T::SameAsDest,
            D::Free
            | D::Staged
            | D::StagedAlsoSource
            | D::Source
            | D::Replaced
            | D::Removed
            | D::Other,
        ) => RowStep::Skip,
    };
    if matches!(step, RowStep::Attention(_)) {
        return step;
    }
    match evidence.backup {
        Backup::Escapes => {
            return RowStep::Attention(format!("journal {} backup escapes sandbox", journal.id));
        }
        Backup::Changed => {
            return RowStep::Attention(format!("journal {} backup bytes changed", journal.id));
        }
        Backup::Fine => {}
    }
    if !evidence.source_safe {
        return RowStep::Attention(format!("journal {} source unsafe", journal.id));
    }
    step
}

impl Run<'_> {
    /// Take a bundle as far as its rows and files allow: resume it, unwind
    /// it, finish its cleanup, or flag it. `plan` is the bundle the rows
    /// were written for, when known.
    pub(super) fn advance(
        &self,
        bundle_id: &str,
        plan: Option<&PlanBundle>,
    ) -> Result<Advanced, PublishError> {
        let journals = self.rows(bundle_id)?;
        // A bundle with no rows (a removal whose files are all gone, a
        // put-back whose files are all home) is not settled: it still has
        // its commit to make.
        if !journals.is_empty() && journals.iter().all(|journal| journal.state.is_terminal()) {
            // An unwind, or a flag for attention, that stopped before the
            // folders the bundle made were removed.
            return Ok(
                if super::folders::remove(self.db, self.sandbox, bundle_id)? {
                    Advanced::Noop
                } else {
                    Advanced::FoldersWait
                },
            );
        }
        if let Some(root) = unusable_root(self.sandbox, &journals) {
            return Ok(Advanced::Waiting(root));
        }
        let gathered = self.gather(&journals, plan)?;
        let step = decide(bundle_id, &journals, gathered, plan);
        if let BundleStep::Attention(reason) = step {
            flag_attention(self.db, &journals)?;
            remove_folders(self.db, self.sandbox, bundle_id);
            return Ok(Advanced::Attention(reason));
        }
        // Whatever comes next may place a file: find out how each root the
        // bundle targets can do that, if nothing has yet.
        let mut probed: BTreeSet<&str> = BTreeSet::new();
        for journal in journals
            .iter()
            .filter(|journal| !journal.state.is_terminal())
        {
            if probed.insert(journal.dest_root.as_str()) {
                probe_root(self.sandbox, journal)?;
            }
        }
        match step {
            BundleStep::Attention(reason) => Ok(Advanced::Attention(reason)),
            BundleStep::Committed => self.committed(bundle_id, &journals, plan),
            BundleStep::Unwind(why, refused) => {
                tracing::warn!(bundle_id, why, "publish bundle unwinds");
                let kept = self.unwind(bundle_id)?;
                Ok(Advanced::Unwound { refused, kept })
            }
            BundleStep::Resume(steps) => {
                let Some(plan) = plan else {
                    return Err(PublishError::Journal(
                        format!("bundle {bundle_id} has no recorded plan").into(),
                    ));
                };
                self.resume(plan, &journals, steps)
            }
        }
    }

    fn rows(&self, bundle_id: &str) -> Result<Vec<FileJournal>, PublishError> {
        long_read(self.db, "publish.bundle_journals", |conn| {
            JournalStore::new(conn).bundle(bundle_id)
        })
    }

    /// The device of a root's folder, when it can be read.
    fn root_dev(&self, root: &str) -> Option<u64> {
        use std::os::unix::fs::MetadataExt as _;
        let dir = self.sandbox.removal_root_dir(root).ok()?;
        std::fs::metadata(dir).ok().map(|meta| meta.dev())
    }

    fn crash(&self, point: CrashPoint) -> Result<(), PublishError> {
        if self.hooks.crash == Some(point) {
            return Err(PublishError::InjectedCrash(point));
        }
        Ok(())
    }

    /// Unwind a pre-commit bundle, then remove the folders it made. The
    /// reason a file was kept under a hidden name, if one was.
    pub(super) fn unwind(&self, bundle_id: &str) -> Result<Option<String>, PublishError> {
        let journals = self.rows(bundle_id)?;
        let journals: Vec<FileJournal> = placements_first(&journals).into_iter().cloned().collect();
        let rows = if self.hooks.crash == Some(CrashPoint::AfterFirstUnwind) {
            &journals[..journals.len().min(1)]
        } else {
            &journals[..]
        };
        let kept = compensate_bundle(self.db, self.sandbox, rows, self.seams)?;
        self.crash(CrashPoint::AfterFirstUnwind)?;
        self.crash(CrashPoint::BeforeFolderRemoval)?;
        remove_folders(self.db, self.sandbox, bundle_id);
        Ok(kept)
    }

    /// Read what every row shows.
    fn gather(
        &self,
        journals: &[FileJournal],
        plan: Option<&PlanBundle>,
    ) -> Result<Gathered, PublishError> {
        let sandbox = self.sandbox;
        // A file the bundle takes away that is still at its path, not yet
        // set aside (or under both names, a set-aside stopped between its
        // link and its unlink): an upgrade's new file may be bound for that
        // same path.
        let not_yet_removed: Vec<(&str, &str, &str)> = journals
            .iter()
            .filter(|journal| {
                journal.kind == JournalKind::Remove
                    && journal.state == JournalState::Staged
                    && (std::fs::symlink_metadata(&journal.staged).is_err()
                        || sandbox
                            .resolve_removal(&journal.dest_root, &journal.dest_rel)
                            .is_ok_and(|dest| paths::same_file(Path::new(&journal.staged), &dest)))
            })
            .map(|journal| {
                (
                    journal.dest_root.as_str(),
                    journal.dest_rel.as_str(),
                    journal.staged_sha256.as_str(),
                )
            })
            .collect();
        let mut rows = Vec::with_capacity(journals.len());
        for journal in journals {
            if journal.state.is_terminal()
                || matches!(
                    journal.state,
                    JournalState::Committed | JournalState::CleanupPending
                )
            {
                rows.push(RowEvidence::Settled);
                continue;
            }
            rows.push(match journal.kind {
                JournalKind::Remove => {
                    let known = self
                        .known_removals
                        .then(|| removal_stat(plan, journal))
                        .flatten();
                    RowEvidence::Removal(removal_evidence(sandbox, journal, known))
                }
                JournalKind::PutBack => RowEvidence::PutBack(super::put_back::evidence(
                    sandbox,
                    journal,
                    self.temps.get(&journal.id).copied(),
                )),
                JournalKind::Add => RowEvidence::Add(
                    super::add::evidence(sandbox, journal),
                    std::fs::symlink_metadata(&journal.staged).is_err(),
                ),
                JournalKind::Audio
                | JournalKind::Sidecar
                | JournalKind::Artwork
                | JournalKind::Output
                | JournalKind::Delete => self.place_evidence(journal, &not_yet_removed)?,
            });
        }
        Ok(Gathered {
            rows,
            put_back_refused: put_back_refused(sandbox, journals),
        })
    }

    fn place_evidence(
        &self,
        journal: &FileJournal,
        not_yet_removed: &[(&str, &str, &str)],
    ) -> Result<RowEvidence, PublishError> {
        let sandbox = self.sandbox;
        let Ok(dest) = sandbox.resolve_no_symlink(&journal.dest_root, &journal.dest_rel) else {
            return Ok(RowEvidence::Unsafe(format!(
                "journal {} destination unsafe",
                journal.id
            )));
        };
        sandbox.ensure_under_roots(&dest).map_err(|_| {
            PublishError::Journal(format!("journal {} escapes sandbox", journal.id).into())
        })?;
        let staged = Path::new(&journal.staged);
        if sandbox.ensure_under_roots(staged).is_err() {
            return Ok(RowEvidence::Unsafe(format!(
                "journal {} staged path escapes sandbox",
                journal.id
            )));
        }
        let holds = |path: &Path| self.known.holds(path, &journal.staged_sha256);
        if journal.kind == JournalKind::Delete {
            // A generated file being taken away: it is either still in
            // place or set aside, never both, and never anything else.
            let in_place = holds(&dest);
            return Ok(RowEvidence::Generated(GeneratedEvidence {
                in_place,
                aside: holds(staged),
                linked: in_place && paths::same_file(staged, &dest),
                dest_present: std::fs::symlink_metadata(&dest).is_ok(),
                aside_present: std::fs::symlink_metadata(staged).is_ok(),
            }));
        }
        let temp = match self.known.hash(staged) {
            None => Temp::Missing,
            Some(sha) if sha != journal.staged_sha256 => Temp::Other,
            Some(_) if paths::same_file(staged, &dest) => Temp::SameAsDest,
            Some(_) => Temp::Staged,
        };
        let dest_state = match self.known.hash(&dest) {
            None => Dest::Free,
            Some(sha) => {
                let source = journal.source_sha256.as_deref() == Some(sha.as_str());
                if sha == journal.staged_sha256 && source {
                    Dest::StagedAlsoSource
                } else if sha == journal.staged_sha256 {
                    Dest::Staged
                } else if source {
                    Dest::Source
                } else if journal
                    .replaces
                    .as_ref()
                    .is_some_and(|replaced| replaced.sha256 == sha)
                {
                    Dest::Replaced
                } else if not_yet_removed.iter().any(|(root, rel, removed)| {
                    *root == journal.dest_root && *rel == journal.dest_rel && *removed == sha
                }) {
                    Dest::Removed
                } else {
                    Dest::Other
                }
            }
        };
        let backup = match journal.backup.as_deref().map(Path::new) {
            None => Backup::Fine,
            Some(backup) if sandbox.ensure_under_roots(backup).is_err() => Backup::Escapes,
            Some(backup) => match self.known.hash(backup) {
                Some(sha) => {
                    let expected = journal.backup_sha256().unwrap_or_default();
                    if !expected.is_empty() && sha != expected {
                        Backup::Changed
                    } else {
                        Backup::Fine
                    }
                }
                None => Backup::Fine,
            },
        };
        Ok(RowEvidence::Place(PlaceEvidence {
            temp,
            dest: dest_state,
            backup,
            in_place: in_place(sandbox, journal, &dest),
            unchanged_original: unchanged_original(sandbox, journal, &dest),
            source_safe: journal
                .source
                .as_ref()
                .is_none_or(|(root, rel)| sandbox.resolve_no_symlink(root, rel).is_ok()),
        }))
    }

    /// Finish every row, in `removals_first` order, then commit and clean
    /// up. A refused row unwinds the bundle.
    fn resume(
        &self,
        plan: &PlanBundle,
        journals: &[FileJournal],
        steps: Vec<RowStep>,
    ) -> Result<Advanced, PublishError> {
        let bundle_id = plan.id.as_str();
        // The commit's rows, from the journal and the plan, before any
        // file moves.
        let tracks = self.track_commits(plan, journals)?;
        let mut by_id: BTreeMap<&str, RowStep> = journals
            .iter()
            .map(|journal| journal.id.as_str())
            .zip(steps)
            .collect();
        let mut first = true;
        for journal in removals_first(journals) {
            let Some(step) = by_id.remove(journal.id.as_str()) else {
                continue;
            };
            if journal.state.is_terminal()
                || matches!(
                    journal.state,
                    JournalState::Committed | JournalState::CleanupPending
                )
            {
                continue;
            }
            if let Some(refused) = self.step(plan, journal, step)? {
                tracing::warn!(
                    bundle_id,
                    reason = refused.reason,
                    "publish bundle unwound after a refusal"
                );
                let kept = self.unwind(bundle_id)?;
                return Ok(Advanced::Unwound {
                    refused: Some(refused),
                    kept,
                });
            }
            if first {
                first = false;
                self.crash(CrashPoint::AfterFirstRename)?;
            }
        }
        self.crash(CrashPoint::BeforeCatalogCommit)?;
        if let Err(error) = self.commit(plan, tracks) {
            // Nothing committed. A database that refused the call would
            // refuse the unwind's rows too, so the bundle waits.
            if error.is_interrupted() {
                return Err(error);
            }
            if let Some(reason) = self.unwind(bundle_id)? {
                tracing::warn!(
                    bundle_id,
                    reason,
                    "publish bundle needs attention after its unwind"
                );
            }
            return Ok(Advanced::CommitRefused(error));
        }
        self.crash(CrashPoint::AfterCatalogCommit)?;
        // Committed: nothing below fails the publish. What is left is
        // pending, and recovery finishes it.
        let pending = match self.cleanup(bundle_id, Some(plan)) {
            Ok(pending) => pending,
            Err(error @ PublishError::InjectedCrash(_)) => return Err(error),
            Err(error) => {
                tracing::warn!(bundle_id, %error, "publish cleanup stopped; recovery finishes it");
                true
            }
        };
        Ok(Advanced::Committed { pending })
    }

    /// Run one row's step. `Some` when the row was refused.
    fn step(
        &self,
        plan: &PlanBundle,
        journal: &FileJournal,
        step: RowStep,
    ) -> Result<Option<Refused>, PublishError> {
        let (root, rel) = (journal.dest_root.as_str(), journal.dest_rel.as_str());
        match step {
            RowStep::Skip => return Ok(None),
            RowStep::RecordPlaced => {}
            RowStep::Place => {
                let dest = self.sandbox.resolve_no_symlink(root, rel)?;
                if let Some(refused) = self.place(journal, &dest)? {
                    return Ok(Some(refused));
                }
            }
            RowStep::SetAsideThenPlace => {
                let dest = self.sandbox.resolve_no_symlink(root, rel)?;
                if let Some(refused) = self.set_aside_original(journal, &dest)? {
                    return Ok(Some(refused));
                }
                if let Some(refused) = self.place(journal, &dest)? {
                    return Ok(Some(refused));
                }
            }
            RowStep::SetAsideRemoval => {
                let dest = self.sandbox.resolve_removal(root, rel)?;
                let aside = Path::new(&journal.staged);
                self.sandbox.ensure_under_removal_roots(aside)?;
                let known = self
                    .known_removals
                    .then(|| removal_stat(Some(plan), journal))
                    .flatten();
                let set_aside = super::remove::set_aside(
                    self.sandbox.placement_mode(root),
                    self.root_dev(root),
                    (root, rel),
                    &dest,
                    aside,
                    &journal.staged_sha256,
                    known,
                    self.seams,
                    self.hooks.before_set_aside,
                );
                if let Some(refused) = refusal(set_aside, || {
                    format!("journal {} could not set its file aside", journal.id)
                })? {
                    return Ok(Some(refused));
                }
            }
            RowStep::PlacePutBack => {
                let placed = super::put_back::place(
                    self.sandbox,
                    journal,
                    self.temps.get(&journal.id).copied(),
                    self.seams,
                    self.hooks.before_place,
                );
                if let Some(refused) = refusal(placed, || {
                    format!("journal {} could not put its file back", journal.id)
                })? {
                    return Ok(Some(refused));
                }
            }
            RowStep::SetAsideGenerated => {
                let dest = self.sandbox.resolve_no_symlink(root, rel)?;
                self.sandbox.ensure_under_roots(&dest)?;
                let aside = Path::new(&journal.staged);
                self.sandbox.ensure_under_roots(aside)?;
                let set_aside = super::generated::set_aside(
                    self.sandbox.placement_mode(root),
                    self.root_dev(root),
                    (root, rel),
                    &dest,
                    aside,
                    &journal.staged_sha256,
                    self.seams,
                );
                if let Some(refused) = refusal(set_aside, || {
                    format!(
                        "journal {} could not set its generated file aside",
                        journal.id
                    )
                })? {
                    return Ok(Some(refused));
                }
            }
            RowStep::Unwind | RowStep::Refuse(_) | RowStep::Attention(_) => {
                return Err(PublishError::Journal(
                    format!("journal {} has no step to finish it", journal.id).into(),
                ));
            }
        }
        durable(self.db, "publish.journal_published", |tx| {
            JournalWriter::new(tx).advance(&journal.id, journal.state, JournalState::Published)
        })?;
        Ok(None)
    }

    /// Set the file at a row's destination aside under its backup name,
    /// journaled first. A name an earlier attempt journaled is kept: a link
    /// an interrupted put-back left there is then found and removed.
    fn set_aside_original(
        &self,
        journal: &FileJournal,
        dest: &Path,
    ) -> Result<Option<Refused>, PublishError> {
        let (root, rel) = (journal.dest_root.as_str(), journal.dest_rel.as_str());
        let backup = match &journal.backup {
            Some(journaled) => PathBuf::from(journaled),
            None => self.sandbox.backup_path_for(dest, &journal.id)?,
        };
        self.sandbox.ensure_under_roots(&backup)?;
        let backup_text = backup.to_string_lossy();
        durable(self.db, "publish.journal_backup", |tx| {
            JournalWriter::new(tx).set_backup(&journal.id, &backup_text)
        })?;
        self.crash(CrashPoint::AfterBackupJournaled)?;
        let home = paths::backup_home(self.sandbox, journal.source.as_ref(), root, rel)?;
        let original = Original {
            visible: dest,
            hidden: &backup,
            home: &home,
            sha256: journal.backup_sha256(),
            root_dev: self.root_dev(root),
            known: journal
                .backup_sha256()
                .and_then(|sha256| self.known.vouch(dest, sha256)),
        };
        let mode = self.sandbox.placement_mode(root);
        match set_aside_with(mode, original, self.seams, self.hooks.before_set_aside) {
            Ok(()) => Ok(None),
            Err(SetAsideError::Io(error)) => Err(PublishError::from(error)),
            Err(error) => Ok(Some(Refused {
                reason: format!(
                    "journal {} could not set its original aside: {error}",
                    journal.id
                ),
                error: error.into_publish(root, rel),
            })),
        }
    }

    /// Place a row's staged temp on its destination. The placing call
    /// itself refuses a name that is taken by then.
    fn place(&self, journal: &FileJournal, dest: &Path) -> Result<Option<Refused>, PublishError> {
        let (root, rel) = (journal.dest_root.as_str(), journal.dest_rel.as_str());
        if let Some(hook) = self.hooks.before_place {
            hook(dest);
        }
        if let Some((deadline, cancelled)) = self.hooks.add_budget
            && (cancelled.load(Ordering::SeqCst) || Instant::now() >= *deadline)
        {
            return Err(PublishError::TimedOut(
                "the file was not placed in time and was not added".into(),
            ));
        }
        let mode = super::add::mode_for(self.sandbox, journal);
        match place_with(mode, Path::new(&journal.staged), dest, self.seams) {
            Ok(_) => Ok(None),
            Err(PlaceError::Collision) => Ok(Some(Refused {
                reason: format!(
                    "another file holds the destination of journal {}",
                    journal.id
                ),
                error: PlaceError::Collision.into_publish(root, rel),
            })),
            Err(error @ (PlaceError::Unavailable(_) | PlaceError::CrossDevice)) => {
                Ok(Some(Refused {
                    reason: format!(
                        "journal {} has no safe way onto its destination",
                        journal.id
                    ),
                    error: error.into_publish(root, rel),
                }))
            }
            Err(error) => Err(error.into_publish(root, rel)),
        }
    }

    /// The catalog commit's track rows, from the journal rows and the plan.
    fn track_commits(
        &self,
        plan: &PlanBundle,
        journals: &[FileJournal],
    ) -> Result<Vec<TrackCommit>, PublishError> {
        let bundle_id = plan.id.as_str();
        let mut tracks = Vec::new();
        for journal in journals {
            let (Some(track_id), Some(mgmt)) = (journal.track_id.as_deref(), journal.commit_as)
            else {
                continue;
            };
            let item = plan.items.iter().find(|item| item.track_id == track_id);
            let adopted = mgmt == MgmtState::Imported;
            let dest = self
                .sandbox
                .resolve_no_symlink(&journal.dest_root, &journal.dest_rel)?;
            let adopt = match (adopted, self.adoption) {
                (false, AdoptionFrom::Plan | AdoptionFrom::Provenance) => None,
                (true, AdoptionFrom::Plan) => item.and_then(|item| item.adopt.clone()),
                (true, AdoptionFrom::Provenance) => {
                    let provenance = journal
                        .source
                        .as_ref()
                        .and_then(|(root, rel)| self.sandbox.resolve_no_symlink(root, rel).ok())
                        .map(|staged| {
                            crate::import::recovered_provenance(&staged, bundle_id, track_id)
                        })
                        .unwrap_or_default();
                    Some(super::planner::Adoption::recovered(
                        item.and_then(|item| item.adopt.as_ref()),
                        provenance.task_id,
                        provenance.source_path,
                        provenance.ingest_source,
                        provenance.join,
                        provenance.track,
                    ))
                }
            };
            tracks.push(TrackCommit {
                track_id: track_id.to_string(),
                root_id: journal.dest_root.clone(),
                rel_path: journal.dest_rel.clone(),
                file_path: dest.to_string_lossy().into_owned(),
                fingerprint: journal.staged_sha256.clone(),
                source: journal.source.clone(),
                source_sha256: journal.source_sha256.clone(),
                mgmt_state: mgmt,
                adopt,
                replaces: journal.replaces.as_ref().and_then(|replaced| {
                    replaced
                        .track_id
                        .clone()
                        .map(|track| (track, replaced.recycle_to.clone()))
                }),
                indexed: None,
                sealed: item.and_then(|item| item.commit_seal()),
                identity: item.filter(|_| adopted).map(|item| item.identity.clone()),
            });
        }
        Ok(tracks)
    }

    /// The catalog commit, checked against the plan's seals, and every row
    /// `published -> committed`, in one durable transaction. The adopted
    /// files are read before it.
    fn commit(&self, plan: &PlanBundle, mut tracks: Vec<TrackCommit>) -> Result<(), PublishError> {
        read_adopted(&mut tracks, self.clock)?;
        let put_back = plan
            .put_back
            .as_ref()
            .map(|put_back| super::put_back::read_placed(self.sandbox, put_back, self.clock))
            .transpose()?;
        let commit = BundleCommit {
            bundle_id: plan.id.clone(),
            tracks,
            removal: plan.removal.clone(),
            put_back,
        };
        durable(self.db, "publish.bundle_commit", |tx| {
            commit_bundle(tx, &commit, self.clock)?;
            enqueue_after_commit(tx, self.jobs, &commit, self.clock)?;
            let writer = JournalWriter::new(tx);
            for journal in JournalStore::new(tx).bundle(&commit.bundle_id)? {
                if journal.state != JournalState::Published {
                    return Err(PublishError::Journal(
                        format!(
                            "journal {} is {} at commit",
                            journal.id,
                            journal.state.as_str()
                        )
                        .into(),
                    ));
                }
                writer.advance(
                    &journal.id,
                    JournalState::Published,
                    JournalState::Committed,
                )?;
            }
            Ok(())
        })
    }

    /// A bundle found committed: never roll back. Verify the catalog still
    /// names each journaled destination with matching bytes, restore an
    /// exact remaining staged copy when the destination went missing, and
    /// retry cleanup. The staged copy only ever goes onto a free name: a
    /// destination holding anything else keeps it, and the staged copy
    /// stays beside it as evidence.
    fn committed(
        &self,
        bundle_id: &str,
        journals: &[FileJournal],
        plan: Option<&PlanBundle>,
    ) -> Result<Advanced, PublishError> {
        let (db, sandbox) = (self.db, self.sandbox);
        for journal in journals {
            if journal.state.is_terminal() {
                continue;
            }
            if matches!(journal.state, JournalState::Prepared | JournalState::Staged) {
                flag_attention(db, journals)?;
                return Ok(Advanced::Attention(format!(
                    "bundle {bundle_id} has mixed commit states"
                )));
            }
            if journal.state == JournalState::Published {
                flag_attention(db, journals)?;
                return Ok(Advanced::Attention(format!(
                    "bundle {bundle_id} disagrees with its catalog commit"
                )));
            }
            match journal.kind {
                // Its destination is meant to be gone: cleanup deletes the
                // set-aside copy of a generated file, and moves a removed
                // file's copy to its bin. Neither ever goes back.
                JournalKind::Delete | JournalKind::Remove => continue,
                // The file put back must be home; its copy is kept until it
                // is.
                JournalKind::PutBack => {
                    let home = sandbox.resolve_removal(&journal.dest_root, &journal.dest_rel)?;
                    if super::remove::holds(&home, &journal.staged_sha256, None) {
                        continue;
                    }
                    let staged = Path::new(&journal.staged);
                    let free = matches!(
                        std::fs::symlink_metadata(&home),
                        Err(error) if error.kind() == std::io::ErrorKind::NotFound
                    );
                    if free
                        && sandbox.ensure_under_removal_roots(staged).is_ok()
                        && super::remove::holds(staged, &journal.staged_sha256, None)
                        && place_from_hidden(
                            sandbox.placement_mode(&journal.dest_root),
                            staged,
                            &home,
                        )
                        .is_ok()
                    {
                        continue;
                    }
                    durable(db, "publish.recover_put_back_lost", |tx| {
                        if let Some(track_id) = journal.track_id.as_deref() {
                            mark_missing(tx, track_id, self.clock)?;
                        }
                        flag_rows(tx, journals)
                    })?;
                    return Ok(Advanced::Attention(format!(
                        "the file put back by journal {} is not home",
                        journal.id
                    )));
                }
                JournalKind::Audio
                | JournalKind::Sidecar
                | JournalKind::Artwork
                | JournalKind::Output
                | JournalKind::Add => {}
            }
            let dest = sandbox.resolve_no_symlink(&journal.dest_root, &journal.dest_rel)?;
            let staged = Path::new(&journal.staged);
            let holds = |path: &Path| self.known.holds(path, &journal.staged_sha256);
            let staged_holds = sandbox.ensure_under_roots(staged).is_ok() && holds(staged);
            let mode = super::add::mode_for(sandbox, journal);
            let dest_free = matches!(
                std::fs::symlink_metadata(&dest),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound
            );
            if holds(&dest) {
                // Still under its hidden name too, as the same file: a
                // placement that linked and stopped. The hidden name goes.
                if staged_holds && paths::same_file(staged, &dest) {
                    place_from_hidden(mode, staged, &dest).map_err(|error| {
                        error.into_publish(&journal.dest_root, &journal.dest_rel)
                    })?;
                }
            } else if !staged_holds {
                durable(db, "publish.recover_output_lost", |tx| {
                    if let Some(track_id) = journal.track_id.as_deref() {
                        mark_missing(tx, track_id, self.clock)?;
                    }
                    flag_rows(tx, journals)
                })?;
                return Ok(Advanced::Attention(format!(
                    "committed output missing for journal {}",
                    journal.id
                )));
            } else if !dest_free {
                flag_attention(db, journals)?;
                return Ok(Advanced::Attention(format!(
                    "another file holds the committed destination of journal {}",
                    journal.id
                )));
            } else {
                match place_from_hidden(mode, staged, &dest) {
                    Ok(_) => {}
                    Err(
                        PlaceError::Collision
                        | PlaceError::Unavailable(_)
                        | PlaceError::CrossDevice,
                    ) => {
                        flag_attention(db, journals)?;
                        return Ok(Advanced::Attention(format!(
                            "committed output for journal {} could not be restored without \
                             risking an overwrite",
                            journal.id
                        )));
                    }
                    Err(error) => {
                        return Err(error.into_publish(&journal.dest_root, &journal.dest_rel));
                    }
                }
            }
            if let Some(track_id) = journal.track_id.as_deref() {
                match long_read(db, "publish.locate", |conn| locate(conn, track_id))? {
                    Some(located) => {
                        if located.root_id != journal.dest_root
                            || located.rel_path != journal.dest_rel
                            || located.published_sha256 != journal.staged_sha256
                        {
                            flag_attention(db, journals)?;
                            return Ok(Advanced::Attention(format!(
                                "catalog disagrees with journal {}",
                                journal.id
                            )));
                        }
                    }
                    None => {
                        flag_attention(db, journals)?;
                        return Ok(Advanced::Attention(format!(
                            "catalog lost track {track_id}"
                        )));
                    }
                }
            }
        }
        self.cleanup(bundle_id, plan)?;
        Ok(Advanced::CleanupFinished)
    }

    /// Post-commit cleanup, row by row: `Cleaned`, or `CleanupPending` for
    /// a later pass when a row fails. Never rolls back the commit. True
    /// when some cleanup stays pending. A database that refused a call ends
    /// it there.
    fn cleanup(&self, bundle_id: &str, plan: Option<&PlanBundle>) -> Result<bool, PublishError> {
        let mut pending = false;
        let mut cleaned_any = false;
        for journal in self.rows(bundle_id)? {
            if !matches!(
                journal.state,
                JournalState::Committed | JournalState::CleanupPending
            ) {
                continue;
            }
            let failed = match self.cleanup_row(&journal, plan) {
                Ok(()) => false,
                Err(error) if error.wrote_nothing() => return Err(error),
                // While its bin is unknown the bins' source has logged why,
                // once; each retry stays quiet.
                Err(error) if self.sandbox.bin_for(&journal.dest_root).is_err() => {
                    tracing::debug!(journal_id = %journal.id, %error, "file waits for a recycle bin");
                    true
                }
                Err(error) => {
                    tracing::warn!(journal_id = %journal.id, %error, "publish cleanup failed; left pending");
                    true
                }
            };
            let cleaned = durable(self.db, "publish.journal_cleaned", |tx| {
                let writer = JournalWriter::new(tx);
                let cleaned = !failed
                    && writer.transition(&journal.id, journal.state, JournalState::Cleaned)?;
                if !cleaned
                    && journal.state != JournalState::CleanupPending
                    && !writer.transition(
                        &journal.id,
                        journal.state,
                        JournalState::CleanupPending,
                    )?
                {
                    tracing::warn!(journal_id = %journal.id, "cleanup-pending not recorded: state moved");
                }
                Ok(cleaned)
            })?;
            if !cleaned {
                pending = true;
            } else if !cleaned_any {
                cleaned_any = true;
                self.crash(CrashPoint::DuringCleanup)?;
            }
        }
        Ok(pending)
    }

    /// Remove one committed row's verified source or backup, or send the
    /// file it replaced or removed to its recycle bin. Unknown files are
    /// never deleted; a file that is already gone counts as done.
    fn cleanup_row(
        &self,
        journal: &FileJournal,
        plan: Option<&PlanBundle>,
    ) -> Result<(), PublishError> {
        let sandbox = self.sandbox;
        let now = std::time::UNIX_EPOCH
            + std::time::Duration::from_millis(u64::try_from(self.now_ms).unwrap_or(0));
        match journal.kind {
            JournalKind::Delete => {
                let aside = Path::new(&journal.staged);
                sandbox.ensure_under_roots(aside)?;
                return super::generated::discard(aside, &journal.staged_sha256);
            }
            // A removed file goes to its bin, never away.
            JournalKind::Remove => {
                let replaced = journal.replaces.as_ref().ok_or_else(|| {
                    PublishError::Journal(format!("journal {} has no bin entry", journal.id).into())
                })?;
                let moved = super::bin::recycle_replaced(
                    self.db,
                    sandbox,
                    journal,
                    replaced,
                    Path::new(&journal.staged),
                    self.bin_seams,
                    now,
                    removal_stat(plan, journal),
                )?;
                if !moved {
                    tracing::warn!(journal_id = %journal.id, "the file to recycle is gone; nothing was moved");
                }
                return Ok(());
            }
            JournalKind::PutBack => {
                let item = plan
                    .and_then(|plan| plan.put_back.as_ref())
                    .and_then(|put_back| super::put_back::item_of(put_back, journal));
                return super::put_back::drop_copy(
                    self.db,
                    sandbox,
                    journal,
                    item,
                    self.temps.get(&journal.id).copied(),
                    self.now_ms,
                );
            }
            JournalKind::Audio
            | JournalKind::Sidecar
            | JournalKind::Artwork
            | JournalKind::Output
            | JournalKind::Add => {}
        }
        if let (Some(replaced), Some(backup_text)) =
            (journal.replaces.as_ref(), journal.backup.as_deref())
        {
            let moved = super::bin::recycle_replaced(
                self.db,
                sandbox,
                journal,
                replaced,
                Path::new(backup_text),
                self.bin_seams,
                now,
                self.known.vouch(Path::new(backup_text), &replaced.sha256),
            )?;
            if !moved {
                tracing::warn!(journal_id = %journal.id, "the file to recycle is gone; nothing was moved");
            }
        } else if let Some(backup_text) = journal.backup.as_deref() {
            let backup = Path::new(backup_text);
            if !gone(backup)? {
                sandbox.ensure_under_roots(backup)?;
                let expected = journal.source_sha256.as_deref().unwrap_or_default();
                // A file is deleted for good only once it is read: an edit
                // that kept its size and modified time is not deleted.
                let held = paths::hash_regular_file(backup)?.sha256 == expected;
                if expected.is_empty() || held {
                    std::fs::remove_file(backup).map_err(PublishError::from)?;
                    if let Some(parent) = backup.parent()
                        && let Err(error) = super::journal::fsync_dir(parent)
                    {
                        tracing::warn!(dir = %parent.display(), %error, "directory sync failed");
                    }
                } else {
                    tracing::warn!(journal_id = %journal.id, "a backup with other bytes is kept");
                }
            }
        }
        if let Some((root, rel)) = journal.source.as_ref() {
            let source = sandbox.resolve_no_symlink(root, rel)?;
            let dest = sandbox.resolve_no_symlink(&journal.dest_root, &journal.dest_rel)?;
            // After a case-only rename on a case-folding filesystem the old
            // name reaches the published file: never delete it.
            if source != dest
                && !paths::same_file(&source, &dest)
                && !gone(&source)?
                && FileStat::read(&source).is_ok()
            {
                let expected = journal.source_sha256.as_deref().unwrap_or_default();
                if expected.is_empty() || paths::holds(&source, expected) {
                    std::fs::remove_file(&source).map_err(PublishError::from)?;
                    super::publisher::prune_empty_parents(sandbox, root, &source);
                }
            }
        }
        Ok(())
    }
}

/// Whether nothing is at `path`, its folder included.
fn gone(path: &Path) -> Result<bool, PublishError> {
    match std::fs::symlink_metadata(path) {
        Ok(_) => Ok(false),
        Err(error)
            if error.kind() == std::io::ErrorKind::NotFound
                || error.raw_os_error() == Some(libc::ENOTDIR) =>
        {
            Ok(true)
        }
        Err(error) => Err(PublishError::from(error)),
    }
}

/// A step's error as a refusal (the file was not touched), or the error
/// to stop on.
fn refusal(
    result: Result<(), PublishError>,
    what: impl FnOnce() -> String,
) -> Result<Option<Refused>, PublishError> {
    match result {
        Ok(()) => Ok(None),
        Err(
            error @ (PublishError::NotLinkable(_)
            | PublishError::NoSafePlacement(_)
            | PublishError::Collision(_)
            | PublishError::Validation(_)),
        ) => Ok(Some(Refused {
            reason: format!("{}: {error}", what()),
            error,
        })),
        Err(error) => Err(error),
    }
}

/// The identity the plan read for the file a removal row takes away.
fn removal_stat(plan: Option<&PlanBundle>, journal: &FileJournal) -> Option<FileStat> {
    plan.and_then(|plan| plan.removal.as_ref())
        .and_then(|removal| super::remove::file_of(removal, journal))
        .map(|file| file.stat)
}

fn removal_evidence(
    sandbox: &Sandbox,
    journal: &FileJournal,
    known: Option<FileStat>,
) -> RemovalEvidence {
    let Ok(dest) = sandbox.resolve_removal(&journal.dest_root, &journal.dest_rel) else {
        return RemovalEvidence::Ambiguous(format!("journal {} destination unsafe", journal.id));
    };
    let aside = Path::new(&journal.staged);
    if sandbox.ensure_under_removal_roots(aside).is_err() {
        return RemovalEvidence::Ambiguous(format!(
            "journal {} staged path escapes sandbox",
            journal.id
        ));
    }
    let holds = |path: &Path| super::remove::holds(path, &journal.staged_sha256, known);
    if std::fs::symlink_metadata(aside).is_ok() {
        if paths::same_file(aside, &dest) {
            return RemovalEvidence::Unwind;
        }
        if holds(aside) {
            return RemovalEvidence::Resume;
        }
        return RemovalEvidence::Ambiguous(format!(
            "journal {} hidden copy is not the removed file",
            journal.id
        ));
    }
    // Only a row that never set its file aside may resume from it. A
    // published row with no hidden copy was put home by an unwind that
    // stopped before it recorded so.
    if journal.state == JournalState::Staged && holds(&dest) {
        RemovalEvidence::Resume
    } else {
        RemovalEvidence::Unwind
    }
}

/// Whether an earlier unwind found no safe way to put a file back: a
/// needs-attention row whose set-aside file is still under its hidden
/// name, with the bytes the journal recorded, while its own name is free.
/// That unwind moved nothing more, since each further row would lose its
/// placed copy and not get its original back, and recovery does not
/// either. A hidden file with other bytes was kept for that reason alone,
/// and the unwind went on past it.
fn put_back_refused(sandbox: &Sandbox, journals: &[FileJournal]) -> bool {
    journals
        .iter()
        .filter(|journal| journal.state == JournalState::NeedsAttention)
        .any(|journal| {
            let (hidden, sha256, home) = match journal.kind {
                JournalKind::Delete => (
                    Some(journal.staged.as_str()),
                    Some(journal.staged_sha256.as_str()),
                    sandbox.resolve_no_symlink(&journal.dest_root, &journal.dest_rel),
                ),
                JournalKind::Remove => (
                    Some(journal.staged.as_str()),
                    Some(journal.staged_sha256.as_str()),
                    sandbox.resolve_removal(&journal.dest_root, &journal.dest_rel),
                ),
                JournalKind::Audio
                | JournalKind::Sidecar
                | JournalKind::Artwork
                | JournalKind::Output
                | JournalKind::Add
                | JournalKind::PutBack => (
                    journal.backup.as_deref(),
                    journal.backup_sha256(),
                    paths::backup_home(
                        sandbox,
                        journal.source.as_ref(),
                        &journal.dest_root,
                        &journal.dest_rel,
                    ),
                ),
            };
            let (Some(hidden), Ok(home)) = (hidden, home) else {
                return false;
            };
            let expected = sha256.unwrap_or_default();
            let intact = if expected.is_empty() {
                FileStat::read(Path::new(hidden)).is_ok()
            } else {
                paths::holds(Path::new(hidden), expected)
            };
            intact && std::fs::symlink_metadata(&home).is_err()
        })
}

/// The first root a bundle's active rows name that the sandbox does not
/// hold for them, or whose folder is not there now (a share not yet
/// mounted, a link to nothing). A removal's row may name a remove-only
/// root.
fn unusable_root(sandbox: &Sandbox, journals: &[FileJournal]) -> Option<String> {
    journals
        .iter()
        .filter(|journal| !journal.state.is_terminal())
        .flat_map(|journal| {
            std::iter::once((journal.kind, journal.dest_root.as_str())).chain(
                journal
                    .source
                    .as_ref()
                    .map(|(root, _)| (journal.kind, root.as_str())),
            )
        })
        .find(|(kind, root)| {
            let dir = match kind {
                JournalKind::Remove | JournalKind::PutBack => sandbox.removal_root_dir(root),
                JournalKind::Audio
                | JournalKind::Sidecar
                | JournalKind::Artwork
                | JournalKind::Output
                | JournalKind::Add
                | JournalKind::Delete => sandbox.root_dir(root),
            };
            dir.and_then(paths::ensure_library_dir).is_err()
        })
        .map(|(_, root)| root.to_owned())
}

/// Whether a row writes its file in place: its source is its destination,
/// or the destination reaches the source by case alone.
fn in_place(sandbox: &Sandbox, journal: &FileJournal, dest: &Path) -> bool {
    journal.source.as_ref().is_some_and(|(root, rel)| {
        sandbox
            .resolve_no_symlink(root, rel)
            .is_ok_and(|source| source == dest)
            || paths::case_alias(
                sandbox,
                (root, rel),
                (&journal.dest_root, &journal.dest_rel),
            )
    })
}

/// A row written in place with no tag change stages the original's own
/// bytes. Until the original is set aside, the file at the destination is
/// that original, not a second staged copy: its temp is still a file of its
/// own, and no backup holds the original yet (or only a link a stopped link
/// test left, which is the original itself).
fn unchanged_original(sandbox: &Sandbox, journal: &FileJournal, dest: &Path) -> bool {
    let staged = Path::new(&journal.staged);
    matches!(journal.state, JournalState::Prepared | JournalState::Staged)
        && journal.source_sha256.as_deref() == Some(journal.staged_sha256.as_str())
        && std::fs::symlink_metadata(staged).is_ok()
        && !paths::same_file(staged, dest)
        && in_place(sandbox, journal, dest)
        && journal.backup.as_deref().is_none_or(|backup| {
            let backup = Path::new(backup);
            std::fs::symlink_metadata(backup).is_err() || paths::same_file(backup, dest)
        })
}
