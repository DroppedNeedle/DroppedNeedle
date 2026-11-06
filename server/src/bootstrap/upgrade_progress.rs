//! What `droppedneedle-tool upgrade --progress json` writes on stdout: one
//! JSON object a line, each with `"v": 1` and a `type`. The tool writes
//! these lines and the server reads them while it runs the tool, so both
//! build on the types here.
//!
//! An upgrade, a fresh start or an undo writes a `step` line as each step
//! starts and `progress` lines while a step with a total moves along, then
//! ends with one `result` line or one `failed` line. `upgrade --check`
//! writes a single `check` line. The server's upgrade page answers with
//! the same types, so they are in the API document too.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// The version every line carries as `v`.
pub const LINE_VERSION: u32 = 1;

/// One line.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Line {
    /// The line format's version. A line of another version does not parse.
    pub v: Version,
    /// What the line says, tagged by `type`.
    #[serde(flatten)]
    pub event: Event,
}

impl Line {
    /// A line of the current version.
    #[must_use]
    pub fn new(event: Event) -> Self {
        Self { v: Version, event }
    }
}

/// [`LINE_VERSION`], the only version a line parses with.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "u32", into = "u32")]
pub struct Version;

impl TryFrom<u32> for Version {
    type Error = String;

    fn try_from(version: u32) -> Result<Self, Self::Error> {
        if version == LINE_VERSION {
            Ok(Self)
        } else {
            Err(format!("line version {version} is not {LINE_VERSION}"))
        }
    }
}

impl From<Version> for u32 {
    fn from(_: Version) -> Self {
        LINE_VERSION
    }
}

/// The kinds of line.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Event {
    /// A step starts.
    Step(StepLine),
    /// The current step moved along.
    Progress(ProgressLine),
    /// What `upgrade --check` found.
    Check(CheckResult),
    /// The run finished.
    Result(Finished),
    /// The run failed or stopped.
    Failed(FailedLine),
}

/// The steps a run goes through, in the order they can come.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[schema(as = UpgradeStep)]
#[serde(rename_all = "snake_case")]
pub enum StepId {
    /// An earlier run did not finish; its changes are rolled back first.
    Recover,
    /// The v2 files move into the `v2-backup` folders.
    MoveAside,
    /// The v2 data is exported.
    Export,
    /// Follows and approvals of deleted users were removed from the v2
    /// database, so it is exported again.
    ExportAgain,
    /// The import is rehearsed without writing records.
    DryRun,
    /// The v2 data is imported into v3.
    Import,
    /// The v2 files are deleted, as `--no-backup` asked.
    RemoveBackup,
    /// The run failed; the v2 files go back.
    RollBack,
    /// `--undo`: the v3 data is deleted and the v2 files go back.
    Undo,
}

impl StepId {
    /// A short label for the step.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Recover => "Putting back the v2 files of an unfinished upgrade",
            Self::MoveAside => "Moving the v2 files into v2-backup",
            Self::Export => "Exporting from v2",
            Self::ExportAgain => "Exporting again without records of deleted users",
            Self::DryRun => "Checking the import",
            Self::Import => "Importing into v3",
            Self::RemoveBackup => "Deleting the v2 files",
            Self::RollBack => "Putting the v2 files back",
            Self::Undo => "Deleting the v3 data and putting the v2 files back",
        }
    }
}

/// A step starts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StepLine {
    /// Which step.
    pub step: StepId,
    /// [`StepId::label`].
    pub label: String,
    /// How many parts the step has, when it knows; its `progress` lines
    /// count up to it.
    pub total: Option<u64>,
}

/// The current step moved along. Written at most about four times a
/// second for a step.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProgressLine {
    /// The step, the one the last `step` line started.
    pub step: StepId,
    /// Parts done so far, never more than the step's total.
    pub done: u64,
}

/// What the data folders hold, as the check found them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[schema(as = UpgradeFoundLayout)]
#[serde(rename_all = "snake_case")]
pub enum FoundLayout {
    /// A v2 instance: the checks ran.
    V2,
    /// An upgrade stopped halfway. The upgrade first puts the v2 files
    /// back, so the checks run only after that.
    Interrupted,
    /// Already v3: nothing to do.
    V3,
    /// Nothing there: nothing to do.
    Empty,
    /// Neither v2 nor v3: a blocker says what.
    Unknown,
}

/// What `upgrade --check` found. It runs every check the upgrade runs
/// before it moves anything, and changes nothing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[schema(as = UpgradeCheck)]
pub struct CheckResult {
    /// What the folders hold.
    pub layout: FoundLayout,
    /// What the export carries, by the names `upgrade-report.json` counts
    /// under: what the upgrade would move, at most. The import can still
    /// leave a record out, and its report then lists it. None when the
    /// export could not be tried.
    pub counts: Option<BTreeMap<String, u64>>,
    /// Follows and approvals of deleted users the upgrade would remove
    /// from the v2 database first. They are not in `counts`.
    pub orphans: u64,
    /// What stops the upgrade until it is fixed.
    pub blockers: Vec<Blocker>,
    /// Addresses that change with the upgrade, which `--callbacks-changed`
    /// acknowledges.
    pub warnings: Vec<Warning>,
}

/// Something that stops the upgrade until it is fixed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[schema(as = UpgradeBlocker)]
pub struct Blocker {
    /// What it is.
    pub code: Code,
    /// What is wrong, as a sentence.
    pub message: String,
    /// What to do about it, as a sentence.
    pub fix: String,
}

/// A feature whose callback address changes with the upgrade.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[schema(as = UpgradeCallbackFeature)]
#[serde(rename_all = "snake_case")]
pub enum CallbackFeature {
    /// Signing in through an OpenID Connect provider.
    SingleSignOn,
    /// Spotify playlist import.
    Spotify,
    /// Edits open in MusicBrainz's release editor.
    MusicbrainzContributions,
}

/// A change at an outside service to acknowledge before the upgrade.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[schema(as = UpgradeWarning)]
pub struct Warning {
    /// The feature it concerns.
    pub feature: CallbackFeature,
    /// What to do, as sentences.
    pub message: String,
    /// The address v2 registered; none for MusicBrainz contributions.
    pub old_address: Option<Address>,
    /// The address v3 uses; none for MusicBrainz contributions.
    pub new_address: Option<Address>,
}

/// An address, whole when v2's settings name the host, else the path to
/// put after the scheme and host DroppedNeedle is opened at.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[schema(as = UpgradeAddress)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum Address {
    /// A full address.
    Full(String),
    /// A path, the base path included.
    Path(String),
}

/// How a run ended well.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[schema(as = UpgradeOutcome)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    /// The v2 data is now v3 data.
    Upgraded,
    /// The v2 files are in the backup and v3 starts empty.
    StartedFresh,
    /// The v2 files are back and the v3 data is gone.
    Undone,
    /// The folders are already on v3.
    AlreadyV3,
    /// The folders hold no v2 data.
    NoV2Data,
}

/// The run finished.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[schema(as = UpgradeResult)]
pub struct Finished {
    /// How it ended.
    pub outcome: Outcome,
    /// Where the v2 files are kept; none when there are none to keep.
    pub backup: Option<BackupFolders>,
    /// What an upgrade moved; only after an upgrade.
    pub summary: Option<Summary>,
}

/// The two folders the v2 files are kept in.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[schema(as = UpgradeBackupFolders)]
pub struct BackupFolders {
    /// The config folder's backup.
    pub config: String,
    /// The cache folder's backup.
    pub cache: String,
}

/// What the upgrade did, as its text summary says it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[schema(as = UpgradeSummary)]
pub struct Summary {
    /// Records moved to v3, by the names the report counts under; kinds
    /// with none moved are left out.
    pub moved: BTreeMap<String, u64>,
    /// Records left out or changed on the way, each listed in the report.
    pub needs_attention: u64,
    /// Follows and approvals of deleted users removed from the v2 database.
    pub orphans_removed: u64,
    /// What stayed behind in v2, one line each.
    pub left_behind: Vec<String>,
    /// What is worth a look, one line each.
    pub notes: Vec<String>,
    /// Settings v2 had nothing saved for, now at v3's defaults.
    pub settings_defaulted: Vec<String>,
    /// Where the full report, `upgrade-report.json`, was saved.
    pub report: String,
}

/// The run failed or stopped.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[schema(as = UpgradeFailure)]
pub struct FailedLine {
    /// What went wrong.
    pub code: Code,
    /// The error, as the tool prints it.
    pub message: String,
    /// Where the v2 files are now.
    pub v2_files: V2Files,
}

/// Where the v2 files are after a failed run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[schema(as = UpgradeV2Files)]
#[serde(rename_all = "snake_case")]
pub enum V2Files {
    /// The run stopped before it moved any.
    Untouched,
    /// The run moved them and put them back: the folders hold v2 as before.
    PutBack,
    /// Some or all of them are in the `v2-backup` folders, so the folders
    /// are not v2 as before. `--undo --yes` puts them back; on a folder an
    /// upgrade left unfinished, running the upgrade again also does first.
    InBackup,
}

/// Why the upgrade cannot start, or why a run failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[schema(as = UpgradeCode)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum Code {
    /// The folders hold something that is neither v2 nor v3.
    UnknownLayout,
    /// The folders are laid out in a way the in-place upgrade does not handle.
    UnsupportedPaths,
    /// A `v2-backup` folder is already there.
    BackupExists,
    /// v2's database `-wal` file cannot be read.
    V2WalUnreadable,
    /// Too little free disk in the cache folder.
    NotEnoughDisk,
    /// v2's key file is missing.
    V2KeyNotFound,
    /// v2's key file holds no usable key.
    V2KeyInvalid,
    /// v2's stored secrets do not open with its key file.
    V2KeyMismatch,
    /// v2's `config.json` cannot be read.
    V2ConfigUnreadable,
    /// v2's `config.json` is not a settings file.
    V2ConfigInvalid,
    /// v2's `config.json` names no instance id.
    MissingInstanceId,
    /// v2's database cannot be read.
    V2DatabaseError,
    /// The export failed for another reason.
    ExportFailed,
    /// The export did not pass validation.
    ValidationFailed,
    /// Callback addresses change and `--callbacks-changed` was not given.
    CallbacksToChange,
    /// Single sign-on is on and no admin could get back in without it.
    NoAdminCanSignIn,
    /// A server or another tool run is using the folders.
    Locked,
    /// An upgrade or import is running on the folders.
    ToolRunning,
    /// A file or folder operation failed.
    FileError,
    /// A v2 file cannot go back because something else is in its place.
    Occupied,
    /// A backup path is not a folder.
    NotAFolder,
    /// The system random source failed.
    RandomFailed,
    /// Removing orphan rows from the v2 database failed.
    OrphanRepairFailed,
    /// The import could not start.
    ImportSetupFailed,
    /// The import or its dry run failed.
    ImportFailed,
    /// The run failed and putting the v2 files back failed too.
    RollbackFailed,
    /// The upgrade finished but the v2 files could not be deleted.
    BackupNotRemoved,
    /// There is no complete v2 backup to go back to.
    NoBackup,
    /// Undo was asked on folders that already hold v2 data.
    AlreadyV2,
    /// The v2 backup may be the only complete copy, so it was kept.
    BackupStillNeeded,
    /// The command deletes data and was not given `--yes`.
    ConfirmationNeeded,
    /// The command was called wrongly.
    Usage,
    /// The deployment settings in the environment do not read.
    EnvironmentInvalid,
    /// Something failed that should not.
    Internal,
}
