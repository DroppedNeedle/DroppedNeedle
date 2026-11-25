//! The setup guide's look at the folders mounted into the container: which
//! of them could be the library, the import folder or a download folder.
//!
//! It looks at every mount and each folder directly inside one (the README
//! mounts `/data` and keeps the music in `/data/music`), leaving out system
//! and pseudo filesystems, the app's own folders and hidden names, plus the
//! folders already set. Folders are told apart by device and inode, so two
//! mounts of one folder show once. It reads folder listings only: it opens
//! no file, follows no link and writes nothing, and each count stops at its
//! budget, answering "at least". A library folder already set is not walked
//! again: its count comes from the catalog. Each suggestion passes the
//! checks the settings it fills in use: a library folder stays clear of the
//! download folders, the recycle bin, the import folder and the app's
//! folders, and an import folder passes the import folder's own check with
//! the suggested library folders counted in. It only suggests: nothing
//! here saves a setting.

mod walk;

use std::collections::HashMap;
use std::os::unix::fs::MetadataExt as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde::Serialize;
use utoipa::ToSchema;

use crate::folder_overlap::folder_clear;
use crate::import_folder::site::{ImportSite, Neighbours};
use crate::imports::mount::{MountRow, containing_mount};
use crate::landing::ports::LandingLibrary;

/// What a found folder could be.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum SetupFolderRole {
    /// A library folder.
    Library,
    /// The import folder.
    ImportFolder,
    /// A download client's folder.
    Downloads,
}

/// One folder the setup guide found.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct FoundFolder {
    /// The folder, as this container sees it.
    pub path: String,
    /// What it could be.
    pub role: SetupFolderRole,
    /// Audio files counted so far.
    pub audio_files: u64,
    /// False when the count stopped at its budget (or waits for a scan),
    /// so there may be more.
    pub counted_all: bool,
    /// Whether the server can write in it. A library folder it cannot
    /// write can still be scanned and played, but not tagged or renamed.
    pub writable: bool,
    /// It is already a library folder, the import folder or a download
    /// folder.
    pub already_set: bool,
}

/// How long and how far the counts go.
#[derive(Debug, Clone, Copy)]
pub struct WalkBudget {
    /// Time for one folder.
    pub per_folder: Duration,
    /// Names read in one folder.
    pub entries: u64,
    /// Time for every folder together.
    pub total: Duration,
}

impl WalkBudget {
    /// The setup guide's budget.
    pub const SETUP: Self = Self {
        per_folder: Duration::from_secs(3),
        entries: 200_000,
        total: Duration::from_secs(6),
    };
}

/// Folders never looked at, nor anything below them.
pub const SYSTEM_FOLDERS: &[&str] = &[
    "/proc", "/sys", "/dev", "/etc", "/run", "/usr", "/boot", "/var/lib", "/tmp",
];

/// Filesystems that hold no one's music.
const PSEUDO_FILESYSTEMS: &[&str] = &[
    "proc",
    "sysfs",
    "cgroup",
    "cgroup2",
    "devpts",
    "mqueue",
    "tmpfs",
    "devtmpfs",
    "overlay",
    "securityfs",
    "debugfs",
    "tracefs",
    "pstore",
    "bpf",
    "configfs",
    "hugetlbfs",
    "autofs",
    "nsfs",
    "fusectl",
    "binfmt_misc",
];

/// A library holds at least this many audio files.
const LIBRARY_MIN_AUDIO: u64 = 100;
/// At least this share of a library's audio files (in tenths) sits two or
/// more folders down, as in `Artist/Album/track.flac`.
const LIBRARY_DEEP_TENTHS: u64 = 6;
/// Names that say a folder takes new music to import.
const IMPORT_NAMES: &[&str] = &["import", "imports", "incoming", "inbox"];
/// Names that say a folder is a download client's.
const DOWNLOAD_NAMES: &[&str] = &["download", "downloads", "complete", "completed"];

/// Everything one look reads, gathered beforehand.
#[derive(Debug, Clone)]
pub struct FolderInputs {
    /// The mount table.
    pub mounts: Vec<MountRow>,
    /// The folders set now: library folders, recycle bin, download folders
    /// and the app's folders.
    pub neighbours: Neighbours,
    /// The import folder, when one is set.
    pub import_folder: Option<PathBuf>,
    /// Folders never looked at, nor anything below them: the system's and
    /// the app's.
    pub left_out: Vec<PathBuf>,
    /// The catalog's track count for each library folder already set.
    pub library_counts: HashMap<PathBuf, u64>,
    /// A scan is running: folders are not counted until it ends.
    pub scanning: bool,
    /// How far the counts go.
    pub budget: WalkBudget,
}

/// A folder to look at, once per device and inode.
struct Candidate {
    path: PathBuf,
    real: PathBuf,
    /// Device and inode, as checked when it was listed.
    key: (u64, u64),
    set: Option<SetupFolderRole>,
}

/// What one unset folder looked like.
struct Looked {
    candidate: Candidate,
    role: SetupFolderRole,
    audio: u64,
    counted_all: bool,
}

/// Look at the folders. Blocking.
pub fn find_folders(inputs: &FolderInputs) -> Vec<FoundFolder> {
    let until = Instant::now() + inputs.budget.total;
    let candidates = candidates(inputs);
    let canonical_counts: HashMap<PathBuf, u64> = inputs
        .library_counts
        .iter()
        .filter_map(|(path, count)| Some((std::fs::canonicalize(path).ok()?, *count)))
        .collect();
    let mut found = Vec::new();
    let mut looked = Vec::new();
    for candidate in candidates {
        match candidate.set {
            Some(SetupFolderRole::Library) => {
                let count = inputs
                    .library_counts
                    .get(&candidate.path)
                    .or_else(|| canonical_counts.get(&candidate.real))
                    .copied()
                    .unwrap_or(0);
                found.push(folder(&candidate, SetupFolderRole::Library, count, true));
            }
            Some(SetupFolderRole::ImportFolder) => {
                let counted = count(&candidate, inputs, until);
                found.push(folder(
                    &candidate,
                    SetupFolderRole::ImportFolder,
                    counted.audio,
                    counted.complete,
                ));
            }
            Some(SetupFolderRole::Downloads) => {
                found.push(folder(&candidate, SetupFolderRole::Downloads, 0, true));
            }
            None => {
                if let Some(look) = look_at(candidate, inputs, until) {
                    looked.push(look);
                }
            }
        }
    }
    found.extend(suggestions(looked, &found, inputs));
    found.sort_by(|a, b| {
        rank(a.role)
            .cmp(&rank(b.role))
            .then(b.already_set.cmp(&a.already_set))
            .then(b.audio_files.cmp(&a.audio_files))
            .then(a.path.cmp(&b.path))
    });
    found
}

fn rank(role: SetupFolderRole) -> u8 {
    match role {
        SetupFolderRole::Library => 0,
        SetupFolderRole::ImportFolder => 1,
        SetupFolderRole::Downloads => 2,
    }
}

fn folder(
    candidate: &Candidate,
    role: SetupFolderRole,
    audio_files: u64,
    counted_all: bool,
) -> FoundFolder {
    FoundFolder {
        path: candidate.path.display().to_string(),
        role,
        audio_files,
        counted_all,
        writable: crate::library_ready::writable(&candidate.real),
        already_set: candidate.set.is_some(),
    }
}

/// The folders set now and the ones the mounts offer, once per device and
/// inode.
fn candidates(inputs: &FolderInputs) -> Vec<Candidate> {
    let left_out = |path: &Path| inputs.left_out.iter().any(|out| path.starts_with(out));
    let mut set: Vec<(PathBuf, SetupFolderRole)> = Vec::new();
    for library in &inputs.neighbours.library {
        set.push((library.clone(), SetupFolderRole::Library));
    }
    if let Some(import) = &inputs.import_folder {
        set.push((import.clone(), SetupFolderRole::ImportFolder));
    }
    for downloads in &inputs.neighbours.downloads {
        let real = std::fs::canonicalize(downloads).unwrap_or_else(|_| downloads.clone());
        let app = inputs
            .neighbours
            .app
            .iter()
            .any(|app| real.starts_with(app));
        if !app && !left_out(downloads) && !left_out(&real) {
            set.push((downloads.clone(), SetupFolderRole::Downloads));
        }
    }
    let mut offered: Vec<PathBuf> = Vec::new();
    for mount in &inputs.mounts {
        let point = &mount.point;
        if point == Path::new("/")
            || PSEUDO_FILESYSTEMS.contains(&mount.fs_type.as_str())
            || left_out(point)
            || !plain_dir(point)
        {
            continue;
        }
        offered.push(point.clone());
        let Ok(entries) = std::fs::read_dir(point) else {
            continue;
        };
        for entry in entries.flatten() {
            let hidden = entry.file_name().as_encoded_bytes().starts_with(b".");
            let dir = entry.file_type().is_ok_and(|kind| kind.is_dir());
            let path = entry.path();
            if !hidden && dir && !left_out(&path) {
                offered.push(path);
            }
        }
    }

    let download_mounts: Vec<i64> = inputs
        .neighbours
        .downloads
        .iter()
        .filter_map(|path| containing_mount(path, &inputs.mounts))
        .collect();
    let mut by_inode: HashMap<(u64, u64), Candidate> = HashMap::new();
    let mut order: Vec<(u64, u64)> = Vec::new();
    let entries = set
        .into_iter()
        .map(|(path, role)| (path, Some(role)))
        .chain(offered.into_iter().map(|path| (path, None)));
    for (path, role) in entries {
        // A folder an admin set may be reached through a link; one the
        // mounts offer was listed as a plain folder.
        let metadata = match role {
            Some(_) => std::fs::metadata(&path),
            None => std::fs::symlink_metadata(&path),
        };
        let Ok(metadata) = metadata else { continue };
        let Ok(real) = std::fs::canonicalize(&path) else {
            continue;
        };
        if !metadata.is_dir() {
            continue;
        }
        let key = (metadata.dev(), metadata.ino());
        let candidate = Candidate {
            path,
            real,
            key,
            set: role,
        };
        match by_inode.get_mut(&key) {
            None => {
                order.push(key);
                by_inode.insert(key, candidate);
            }
            Some(kept) => {
                if prefer(&candidate, kept, &download_mounts, &inputs.mounts) {
                    *kept = candidate;
                }
            }
        }
    }
    order
        .into_iter()
        .filter_map(|key| by_inode.remove(&key))
        .collect()
}

/// A folder that is not a link.
fn plain_dir(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok_and(|metadata| metadata.is_dir())
}

/// Whether `new` names a folder better than `kept` does: a folder already
/// set first, then one on a download folder's mount (so imports move
/// rather than copy), then the shorter path.
fn prefer(new: &Candidate, kept: &Candidate, download_mounts: &[i64], mounts: &[MountRow]) -> bool {
    if new.set.is_some() != kept.set.is_some() {
        return new.set.is_some();
    }
    if kept.set.is_some() {
        return false;
    }
    let near = |candidate: &Candidate| {
        containing_mount(&candidate.path, mounts).is_some_and(|id| download_mounts.contains(&id))
    };
    if near(new) != near(kept) {
        return near(new);
    }
    let length = |candidate: &Candidate| candidate.path.as_os_str().len();
    (length(new), &new.path) < (length(kept), &kept.path)
}

fn named(path: &Path, names: &[&str]) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| names.contains(&name.to_lowercase().as_str()))
}

/// Count one folder within its budget and what is left of the whole one.
/// While a scan runs nothing is counted.
fn count(candidate: &Candidate, inputs: &FolderInputs, until: Instant) -> walk::Count {
    if inputs.scanning {
        return walk::Count::default();
    }
    let limit = walk::Limit {
        until: until.min(Instant::now() + inputs.budget.per_folder),
        entries: inputs.budget.entries,
    };
    let real = &candidate.real;
    walk::count_audio(real, candidate.key, limit).unwrap_or_else(|error| {
        tracing::debug!(folder = %real.display(), %error, "setup: folder not counted");
        walk::Count::default()
    })
}

/// What an unset folder could be, when anything.
fn look_at(candidate: Candidate, inputs: &FolderInputs, until: Instant) -> Option<Looked> {
    let neighbours = &inputs.neighbours;
    let import: Vec<PathBuf> = inputs.import_folder.iter().cloned().collect();
    if named(&candidate.path, DOWNLOAD_NAMES) {
        let outside = [
            neighbours.library.clone(),
            neighbours.recycle_bin.clone(),
            neighbours.app.clone(),
            import,
        ];
        return folder_clear(&candidate.real, outside.map(|dirs| ((), dirs)))
            .is_ok()
            .then_some(Looked {
                candidate,
                role: SetupFolderRole::Downloads,
                audio: 0,
                counted_all: true,
            });
    }
    if neighbours.clear(&candidate.real).is_err()
        || folder_clear(&candidate.real, [((), import)]).is_err()
    {
        return None;
    }
    let walk::Count {
        audio,
        deep,
        complete: counted_all,
        ..
    } = count(&candidate, inputs, until);
    let role = if named(&candidate.path, IMPORT_NAMES) {
        SetupFolderRole::ImportFolder
    } else if inputs.scanning {
        // Nothing is counted while a scan runs, so nothing tells a library.
        return None;
    } else if audio >= LIBRARY_MIN_AUDIO && deep * 10 >= audio * LIBRARY_DEEP_TENTHS {
        SetupFolderRole::Library
    } else if !counted_all && audio < LIBRARY_MIN_AUDIO {
        // The budget ran out before this folder said much: offered as "at
        // least N" for the admin to judge.
        SetupFolderRole::Library
    } else if audio > 0 && audio < LIBRARY_MIN_AUDIO {
        SetupFolderRole::ImportFolder
    } else {
        return None;
    };
    Some(Looked {
        candidate,
        role,
        audio,
        counted_all,
    })
}

/// The unset folders worth suggesting: library folders that hold no other
/// suggested library folder, then download and import folders outside every
/// library folder, each import folder passing the import folder's check
/// with the suggested library folders counted in. No import folder is
/// suggested while one is set.
fn suggestions(
    looked: Vec<Looked>,
    set: &[FoundFolder],
    inputs: &FolderInputs,
) -> Vec<FoundFolder> {
    let libraries: Vec<PathBuf> = looked
        .iter()
        .filter(|look| look.role == SetupFolderRole::Library)
        .map(|look| look.candidate.real.clone())
        .collect();
    let proposed: Vec<PathBuf> = libraries
        .iter()
        .filter(|library| {
            !libraries
                .iter()
                .any(|other| other != *library && other.starts_with(library))
        })
        .cloned()
        .collect();
    let set_libraries: Vec<PathBuf> = set
        .iter()
        .filter(|folder| folder.role == SetupFolderRole::Library)
        .filter_map(|folder| std::fs::canonicalize(&folder.path).ok())
        .collect();
    let inside_a_library = |real: &Path| {
        proposed
            .iter()
            .chain(&set_libraries)
            .any(|library| real != library && real.starts_with(library))
    };
    looked
        .into_iter()
        .filter(|look| !inside_a_library(&look.candidate.real))
        .filter(|look| match look.role {
            SetupFolderRole::Library => proposed.contains(&look.candidate.real),
            SetupFolderRole::ImportFolder => {
                inputs.import_folder.is_none()
                    && inputs
                        .neighbours
                        .import_candidate(&look.candidate.real, &proposed)
                        .is_ok()
            }
            SetupFolderRole::Downloads => true,
        })
        .map(|look| folder(&look.candidate, look.role, look.audio, look.counted_all))
        .collect()
}

/// How a folder look went.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum FolderLookState {
    /// Every folder was looked at, within its budget.
    Complete,
    /// A scan is running: folders not set are counted after it.
    ScanRunning,
    /// The folders could not be read: a setting does not read, a folder
    /// did not answer in time, or an earlier look is still stuck on one.
    NotRead,
}

/// What a folder look found, and how it went.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FolderLook {
    pub folders: Vec<FoundFolder>,
    pub state: FolderLookState,
}

impl FolderLook {
    fn not_read() -> Self {
        Self {
            folders: Vec::new(),
            state: FolderLookState::NotRead,
        }
    }
}

/// How long past its budget a look is waited for. A folder on a network
/// share that stops answering can hold a listing forever.
const LOOK_SLACK: Duration = Duration::from_secs(1);

/// Run `work` on a blocking thread, holding `turn` there until it returns,
/// and wait at most `wait` for it. `None` when a look already holds the
/// turn (one stuck on a hung folder keeps it until that folder answers) or
/// when `work` did not finish in time; the thread is left to finish by
/// itself.
pub async fn bounded_look<T: Send + 'static>(
    turn: &Arc<tokio::sync::Mutex<()>>,
    wait: Duration,
    work: impl FnOnce() -> T + Send + 'static,
) -> Option<T> {
    let held = turn.clone().try_lock_owned().ok()?;
    let task = tokio::task::spawn_blocking(move || {
        let _held = held;
        work()
    });
    match tokio::time::timeout(wait, task).await {
        Ok(Ok(done)) => Some(done),
        Ok(Err(error)) => {
            tracing::warn!(%error, "setup: the folder look stopped");
            None
        }
        Err(_) => {
            tracing::warn!("setup: the folder look did not finish in time");
            None
        }
    }
}

/// The setup guide's folder look over the live settings: one look at a
/// time, on a blocking thread.
pub struct SetupFolders {
    site: Arc<ImportSite>,
    library: Arc<dyn LandingLibrary>,
    left_out: Vec<PathBuf>,
    turn: Arc<tokio::sync::Mutex<()>>,
}

impl SetupFolders {
    /// The look over the import folder's site (which reads every folder it
    /// stays clear of), the library (its folders and whether a scan runs),
    /// and the app's folders besides the site's (`app_extra`), which are
    /// never looked at.
    pub fn new(
        site: Arc<ImportSite>,
        library: Arc<dyn LandingLibrary>,
        app_extra: Vec<PathBuf>,
    ) -> Self {
        let left_out = SYSTEM_FOLDERS
            .iter()
            .map(PathBuf::from)
            .chain(app_extra)
            .collect();
        Self {
            site,
            library,
            left_out,
            turn: Arc::new(tokio::sync::Mutex::new(())),
        }
    }

    /// Look at the folders `mounts` lists. `library_counts` is the
    /// catalog's track count for each library folder already set. Answers
    /// within the budget and a second: a look that cannot finish, or that
    /// finds another still running, answers no folders.
    pub async fn find(
        &self,
        mounts: Vec<MountRow>,
        library_counts: HashMap<PathBuf, u64>,
    ) -> FolderLook {
        let site = self.site.clone();
        let library = self.library.clone();
        let mut left_out = self.left_out.clone();
        let budget = WalkBudget::SETUP;
        let look = bounded_look(&self.turn, budget.total + LOOK_SLACK, move || {
            let neighbours = match site.neighbours() {
                Ok(neighbours) => neighbours,
                Err(why) => {
                    tracing::warn!(?why, "setup: the folder settings do not read");
                    return FolderLook::not_read();
                }
            };
            left_out.extend(neighbours.app.iter().cloned());
            let scanning = library.scanning();
            let folders = find_folders(&FolderInputs {
                mounts,
                import_folder: site.configured(),
                neighbours,
                left_out,
                library_counts,
                scanning,
                budget,
            });
            FolderLook {
                folders,
                state: if scanning {
                    FolderLookState::ScanRunning
                } else {
                    FolderLookState::Complete
                },
            }
        })
        .await;
        look.unwrap_or_else(FolderLook::not_read)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    use super::bounded_look;

    /// A look stuck on a folder that never answers gives up in time, and
    /// the next one answers at once instead of starting a second walk.
    #[tokio::test]
    async fn a_stuck_look_answers_in_time_and_blocks_no_second_walk() {
        let turn = Arc::new(tokio::sync::Mutex::new(()));
        let (release, stuck) = std::sync::mpsc::channel::<()>();
        let started = Instant::now();
        let first = bounded_look(&turn, Duration::from_millis(200), move || {
            let _ = stuck.recv();
        })
        .await;
        assert!(first.is_none());
        assert!(started.elapsed() < Duration::from_secs(2));

        let second = bounded_look(&turn, Duration::from_secs(5), || 7).await;
        assert_eq!(second, None, "the stuck walk still holds its turn");

        release.send(()).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(answer) = bounded_look(&turn, Duration::from_secs(5), || 7).await {
                assert_eq!(answer, 7);
                break;
            }
            assert!(Instant::now() < deadline, "the turn was never given back");
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }
}
