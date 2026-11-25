//! Free space on the filesystems the app writes: the one `statvfs` reader,
//! and the watch that reads every folder the app writes every five minutes
//! and keeps the latest reading in memory.

use std::collections::{HashMap, HashSet};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::clock::Clock;
use crate::jobs::{AlreadyRunning, FnTick, Next, Schedule, Supervisor};

/// Bytes an unprivileged process may still write on the filesystem holding
/// `path` (`f_bavail * f_frsize`). Blocking: call it where a blocking call
/// is allowed.
pub fn free_bytes(path: &Path) -> io::Result<u64> {
    space_bytes(path).map(|(free, _)| free)
}

/// Free and total bytes on the filesystem holding `path`, from one
/// `statvfs`. Blocking.
fn space_bytes(path: &Path) -> io::Result<(u64, u64)> {
    use std::os::unix::ffi::OsStrExt as _;

    let c_path = std::ffi::CString::new(path.as_os_str().as_bytes())?;
    // SAFETY: `statvfs` is plain old data, so all-zero is a valid value.
    let mut stat: libc::statvfs = unsafe { std::mem::zeroed() };
    // SAFETY: `c_path` is a NUL-terminated string that outlives the call,
    // and `stat` is a valid, writable `statvfs`.
    let status = unsafe { libc::statvfs(c_path.as_ptr(), &mut stat) };
    if status != 0 {
        return Err(io::Error::last_os_error());
    }
    // The field widths differ by platform (u32 blocks on macOS).
    #[allow(clippy::unnecessary_cast)]
    let free = (stat.f_bavail as u64).saturating_mul(stat.f_frsize as u64);
    #[allow(clippy::unnecessary_cast)]
    let total = (stat.f_blocks as u64).saturating_mul(stat.f_frsize as u64);
    Ok((free, total))
}

/// What a watched folder is to the app.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum FolderRole {
    /// The folder holding the database and the undo copies.
    Data,
    /// The config folder.
    Config,
    /// The temporary folder.
    Temp,
    /// The app's own imports folder (uploads and held files).
    Imports,
    /// A library folder.
    Library,
    /// The configured recycle bin.
    RecycleBin,
    /// A download client's folder.
    Downloads,
    /// The import folder an admin set.
    ImportFolder,
}

/// One folder the watch reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Watched {
    /// What the folder is to the app.
    pub role: FolderRole,
    /// Where it is.
    pub path: PathBuf,
}

/// How much room a filesystem has left, against what the app needs there.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiskLevel {
    /// Enough room.
    Ok,
    /// Room is running out.
    Low,
    /// The data disk cannot take another backup or undo copy.
    Critical,
    /// The read failed or did not answer in time.
    Unknown,
}

/// One filesystem in a sample: the watched folders on it, its free bytes
/// and its level.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FilesystemSpace {
    /// The folders on this filesystem, data first when it holds it.
    pub folders: Vec<Watched>,
    /// Bytes free, unless the read failed or timed out.
    pub free: Option<u64>,
    /// Its size in bytes, when read.
    pub total: Option<u64>,
    /// The filesystem's kind (`ext4`, `nfs`, ...), `other` when not known
    /// and `unknown` when not read.
    pub kind: &'static str,
    /// Whether it is local, not a network share; `false` when not read.
    pub local: bool,
    /// How that compares with what the app needs there.
    pub level: DiskLevel,
}

/// The latest reading of every watched folder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiskSample {
    /// When it was taken, kernel clock milliseconds.
    pub checked_at_ms: i64,
    /// One entry per filesystem; a folder whose read failed has its own.
    pub filesystems: Vec<FilesystemSpace>,
}

impl DiskSample {
    /// The worst level in the sample.
    pub fn worst(&self) -> DiskLevel {
        let rank = |level: DiskLevel| match level {
            DiskLevel::Ok => 0,
            DiskLevel::Unknown => 1,
            DiskLevel::Low => 2,
            DiskLevel::Critical => 3,
        };
        self.filesystems
            .iter()
            .map(|fs| fs.level)
            .max_by_key(|level| rank(*level))
            .unwrap_or(DiskLevel::Ok)
    }
}

/// A filesystem's device and free bytes, as [`SpaceReader`] answers them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Space {
    /// The filesystem's device number, to tell filesystems apart.
    pub device: u64,
    /// Bytes an unprivileged process may still write there.
    pub free: u64,
    /// Its size in bytes.
    pub total: u64,
    /// Its statfs magic, which names its kind.
    pub magic: i64,
}

/// Reads one folder's filesystem. Blocking: the watch runs it on the
/// blocking pool under its time bound. Tests pass a fake.
pub type SpaceReader = Arc<dyn Fn(&Path) -> io::Result<Space> + Send + Sync>;

/// The folders to read on each pass, worked out again each time (library
/// and download folders change with settings). Blocking.
pub type FolderList = Arc<dyn Fn() -> Vec<Watched> + Send + Sync>;

/// The least free bytes over `paths`; `None` when any read fails.
/// Blocking.
pub fn least_free(paths: &[PathBuf]) -> Option<u64> {
    paths
        .iter()
        .map(|path| free_bytes(path).ok())
        .try_fold(u64::MAX, |least, free| Some(least.min(free?)))
        .filter(|_| !paths.is_empty())
}

/// The real reader: the folder's device and [`free_bytes`].
pub fn read_space(path: &Path) -> io::Result<Space> {
    use std::os::unix::fs::MetadataExt as _;

    let device = std::fs::metadata(path)?.dev();
    let (free, total) = space_bytes(path)?;
    Ok(Space {
        device,
        free,
        total,
        magic: crate::db::filesystem_magic(path)?,
    })
}

/// How long one folder's read may take before it counts as "could not
/// check". A dead network mount never hangs a pass.
pub const READ_BOUND: Duration = Duration::from_secs(2);
/// How often the watch reads every folder.
pub const WATCH_EVERY: Duration = Duration::from_secs(5 * 60);
/// The loop's name on the supervisor.
pub const DISK_WATCH_JOB: &str = "disk-watch";
/// Below this, any folder other than the data folder is low.
pub const LOW_ELSEWHERE: u64 = 2 * GIB;
/// Below this on the data or temp folder, a fault answers `DISK_FULL`.
pub const DISK_FULL_BELOW: u64 = 64 * MIB;
const MIB: u64 = 1024 * 1024;
const GIB: u64 = 1024 * MIB;

/// What one of the watch's blocking reads reads.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum Key {
    /// The folder list.
    FolderList,
    /// The size of the database and its undo copies.
    DataBytes,
    /// One folder's filesystem.
    Folder(PathBuf),
}

/// A read marked in flight; it is unmarked when the read returns or
/// panics.
struct InFlight(Arc<Mutex<HashSet<Key>>>, Key);

impl Drop for InFlight {
    fn drop(&mut self) {
        lock(&self.0).remove(&self.1);
    }
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Watches free space on every folder the app writes, keeps the latest
/// sample in memory and logs once each time a folder's level changes.
#[derive(Clone)]
pub struct DiskWatch {
    pub(crate) inner: Arc<WatchInner>,
}

pub(crate) struct WatchInner {
    reader: SpaceReader,
    folders: FolderList,
    db_path: PathBuf,
    undo_dir: PathBuf,
    clock: Arc<dyn Clock>,
    pub(crate) bound: Duration,
    /// The reads still running, by what they read. A read that runs past
    /// its bound stays here until it returns, and no second read of the
    /// same thing starts meanwhile, so a hung mount holds one blocking
    /// thread, never one per pass.
    in_flight: Arc<Mutex<HashSet<Key>>>,
    latest: Mutex<Option<DiskSample>>,
    levels: Mutex<HashMap<(FolderRole, PathBuf), DiskLevel>>,
}

impl std::fmt::Debug for DiskWatch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DiskWatch")
            .field("db_path", &self.inner.db_path)
            .finish_non_exhaustive()
    }
}

impl DiskWatch {
    /// A watch over `folders` (the data folder is added from `db_path`),
    /// reading each through `reader`. The data folder's thresholds come
    /// from the database file and the undo copies in `undo_dir`.
    pub fn new(
        reader: SpaceReader,
        folders: FolderList,
        db_path: PathBuf,
        undo_dir: PathBuf,
        clock: Arc<dyn Clock>,
    ) -> Self {
        Self {
            inner: Arc::new(WatchInner {
                reader,
                folders,
                db_path,
                undo_dir,
                clock,
                bound: READ_BOUND,
                in_flight: Arc::new(Mutex::new(HashSet::new())),
                latest: Mutex::new(None),
                levels: Mutex::new(HashMap::new()),
            }),
        }
    }

    /// The latest sample, if a pass has run.
    pub fn latest(&self) -> Option<DiskSample> {
        self.inner
            .latest
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    /// Read every folder once, keep the sample and log each folder whose
    /// level changed. Answers the number of folders whose level changed.
    pub async fn check(&self) -> usize {
        let data_dir = self
            .inner
            .db_path
            .parent()
            .map_or_else(|| PathBuf::from("."), Path::to_path_buf);
        let mut folders = vec![Watched {
            role: FolderRole::Data,
            path: data_dir,
        }];
        let list = Arc::clone(&self.inner.folders);
        let listed = self
            .bounded(Key::FolderList, move || list())
            .await
            .unwrap_or_default();
        for folder in listed {
            if !folders.iter().any(|seen| seen.path == folder.path) {
                folders.push(folder);
            }
        }
        let used = self
            .bounded(
                Key::DataBytes,
                data_bytes_of(&self.inner.db_path, &self.inner.undo_dir),
            )
            .await;
        let mut filesystems: Vec<(Option<u64>, FilesystemSpace)> = Vec::new();
        for folder in folders {
            let reader = Arc::clone(&self.inner.reader);
            let path = folder.path.clone();
            let read = self
                .bounded(Key::Folder(folder.path.clone()), move || reader(&path).ok())
                .await
                .flatten();
            match read {
                Some(space) => {
                    if let Some((_, fs)) = filesystems
                        .iter_mut()
                        .find(|(device, _)| *device == Some(space.device))
                    {
                        fs.folders.push(folder);
                    } else {
                        filesystems.push((
                            Some(space.device),
                            FilesystemSpace {
                                folders: vec![folder],
                                free: Some(space.free),
                                total: Some(space.total),
                                kind: crate::db::filesystem_name(space.magic),
                                local: crate::db::filesystem_is_local(space.magic),
                                level: DiskLevel::Ok,
                            },
                        ));
                    }
                }
                None => filesystems.push((
                    None,
                    FilesystemSpace {
                        folders: vec![folder],
                        free: None,
                        total: None,
                        kind: "unknown",
                        local: false,
                        level: DiskLevel::Unknown,
                    },
                )),
            }
        }
        let filesystems: Vec<FilesystemSpace> = filesystems
            .into_iter()
            .map(|(_, mut fs)| {
                if let Some(free) = fs.free {
                    let data = fs.folders.iter().any(|f| f.role == FolderRole::Data);
                    fs.level = level_for(free, data, used.flatten());
                }
                fs
            })
            .collect();
        let sample = DiskSample {
            checked_at_ms: self.inner.clock.now_ms(),
            filesystems,
        };
        let changed = self.note_levels(&sample);
        *self
            .inner
            .latest
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(sample);
        changed
    }

    /// Run `work` on the blocking pool; `None` once it runs past the bound,
    /// while an earlier run under the same `key` has not returned, or when
    /// the thread is lost.
    async fn bounded<T: Send + 'static>(
        &self,
        key: Key,
        work: impl FnOnce() -> T + Send + 'static,
    ) -> Option<T> {
        let in_flight = Arc::clone(&self.inner.in_flight);
        if !lock(&in_flight).insert(key.clone()) {
            return None;
        }
        let release = InFlight(in_flight, key);
        let run = tokio::task::spawn_blocking(move || {
            let _release = release;
            work()
        });
        tokio::time::timeout(self.inner.bound, run).await.ok()?.ok()
    }

    /// Log each folder whose level differs from the last pass; answers
    /// how many did.
    fn note_levels(&self, sample: &DiskSample) -> usize {
        let mut levels = self
            .inner
            .levels
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut changes = Vec::new();
        for fs in &sample.filesystems {
            for folder in &fs.folders {
                let key = (folder.role, folder.path.clone());
                let before = levels.insert(key, fs.level);
                // The first pass logs only what is wrong.
                if before.unwrap_or(DiskLevel::Ok) != fs.level {
                    changes.push((folder.clone(), fs.level, fs.free));
                }
            }
        }
        drop(levels);
        for (folder, level, free) in &changes {
            let free_mb = free.map(|bytes| bytes / MIB);
            let folder = folder.path.display();
            match level {
                DiskLevel::Ok => tracing::info!(%folder, ?free_mb, "free space is back to normal"),
                DiskLevel::Low => tracing::warn!(%folder, ?free_mb, "free space is running low"),
                DiskLevel::Critical => tracing::error!(
                    %folder,
                    ?free_mb,
                    "the data disk is nearly full: backups and uploads are refused until space is freed"
                ),
                DiskLevel::Unknown => {
                    tracing::warn!(%folder, "free space could not be checked");
                }
            }
        }
        changes.len()
    }

    /// Start the watch's loop: a pass every five minutes.
    pub fn spawn_on(&self, supervisor: &Supervisor) -> Result<(), AlreadyRunning> {
        let watch = self.clone();
        supervisor.periodic(
            DISK_WATCH_JOB,
            Schedule::new(WATCH_EVERY).with_jitter(Duration::from_secs(30)),
            FnTick(move |_ctx| {
                let watch = watch.clone();
                async move {
                    watch.check().await;
                    Ok(Next::Continue)
                }
            }),
        )
    }
}

/// The level for `free` bytes. The data disk is low below twice what the
/// database and undo copies hold plus 1 GiB, critical below what they hold
/// plus 256 MiB; with `used` unknown it counts them as nothing. Any other
/// folder is low below 2 GiB.
fn level_for(free: u64, data: bool, used: Option<u64>) -> DiskLevel {
    if data {
        let used = used.unwrap_or(0);
        if free < used.saturating_add(256 * MIB) {
            return DiskLevel::Critical;
        }
        if free < used.saturating_mul(2).saturating_add(GIB) {
            return DiskLevel::Low;
        }
        return DiskLevel::Ok;
    }
    if free < LOW_ELSEWHERE {
        DiskLevel::Low
    } else {
        DiskLevel::Ok
    }
}

/// A closure summing the database file, its WAL and the undo copies.
fn data_bytes_of(db_path: &Path, undo_dir: &Path) -> impl FnOnce() -> Option<u64> + Send + 'static {
    let db_path = db_path.to_path_buf();
    let undo_dir = undo_dir.to_path_buf();
    move || {
        let mut total = std::fs::metadata(&db_path).ok()?.len();
        let wal = crate::db::backup::wal_path(&db_path);
        total += std::fs::metadata(wal).map_or(0, |meta| meta.len());
        Some(total.saturating_add(folder_bytes(&undo_dir)))
    }
}

/// Bytes of the plain files below `dir`, without following links.
fn folder_bytes(dir: &Path) -> u64 {
    let mut total = 0u64;
    let mut pending = vec![dir.to_path_buf()];
    while let Some(next) = pending.pop() {
        let Ok(entries) = std::fs::read_dir(&next) else {
            continue;
        };
        for entry in entries.flatten() {
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            if kind.is_dir() {
                pending.push(entry.path());
            } else if kind.is_file() {
                total = total.saturating_add(entry.metadata().map_or(0, |meta| meta.len()));
            }
        }
    }
    total
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::*;
    use crate::test_support::ManualClock;

    const DATA_DEVICE: u64 = 1;
    const LIBRARY_DEVICE: u64 = 2;

    fn watch(reader: SpaceReader) -> DiskWatch {
        let library: FolderList = Arc::new(|| {
            vec![
                Watched {
                    role: FolderRole::Library,
                    path: PathBuf::from("/music"),
                },
                Watched {
                    role: FolderRole::Temp,
                    path: PathBuf::from("/data/tmp"),
                },
            ]
        });
        DiskWatch::new(
            reader,
            library,
            PathBuf::from("/data/library.db"),
            PathBuf::from("/data/undo"),
            Arc::new(ManualClock::at(1_000)),
        )
    }

    #[tokio::test]
    async fn crossing_low_and_critical_changes_the_sample_once_each() {
        let data_free = Arc::new(AtomicU64::new(50 * GIB));
        let seen = Arc::clone(&data_free);
        let reader: SpaceReader = Arc::new(move |path: &Path| {
            Ok(if path.starts_with("/music") {
                Space {
                    device: LIBRARY_DEVICE,
                    free: 500 * GIB,
                    ..Space::default()
                }
            } else {
                Space {
                    device: DATA_DEVICE,
                    free: seen.load(Ordering::SeqCst),
                    ..Space::default()
                }
            })
        });
        let watch = watch(reader);
        let level = |watch: &DiskWatch| {
            let sample = watch.latest().unwrap();
            let data = sample
                .filesystems
                .iter()
                .find(|fs| fs.folders.iter().any(|f| f.role == FolderRole::Data))
                .unwrap();
            // Temp shares the data disk, so it is one entry.
            assert_eq!(data.folders.len(), 2, "{data:?}");
            data.level
        };

        assert_eq!(watch.check().await, 0);
        assert_eq!(level(&watch), DiskLevel::Ok);
        assert_eq!(watch.latest().unwrap().filesystems.len(), 2);

        data_free.store(GIB / 2, Ordering::SeqCst);
        assert_eq!(watch.check().await, 2, "data and temp went low");
        assert_eq!(level(&watch), DiskLevel::Low);
        assert_eq!(watch.check().await, 0, "no change, no second line");

        data_free.store(100 * MIB, Ordering::SeqCst);
        assert_eq!(watch.check().await, 2);
        assert_eq!(level(&watch), DiskLevel::Critical);
        assert_eq!(watch.latest().unwrap().worst(), DiskLevel::Critical);

        data_free.store(50 * GIB, Ordering::SeqCst);
        assert_eq!(watch.check().await, 2);
        assert_eq!(level(&watch), DiskLevel::Ok);
    }

    #[tokio::test]
    async fn a_hung_read_reports_could_not_check_and_is_never_started_twice() {
        let (release, released) = std::sync::mpsc::channel::<()>();
        let released = Arc::new(Mutex::new(released));
        let started = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let (gate, count) = (Arc::clone(&released), Arc::clone(&started));
        let reader: SpaceReader = Arc::new(move |path: &Path| {
            if path.starts_with("/music") {
                count.fetch_add(1, Ordering::SeqCst);
                // Blocks until the test lets go, as a dead mount would.
                let _ = lock(&gate).recv();
            }
            Ok(Space {
                device: DATA_DEVICE,
                free: 50 * GIB,
                ..Space::default()
            })
        });
        let watch = watch(reader).with_bound(Duration::from_millis(50));
        let music = |watch: &DiskWatch| {
            let sample = watch.latest().unwrap();
            let fs = sample
                .filesystems
                .into_iter()
                .find(|fs| fs.folders.iter().any(|f| f.role == FolderRole::Library))
                .unwrap();
            (fs.level, fs.free)
        };

        watch.check().await;
        assert_eq!(music(&watch), (DiskLevel::Unknown, None));
        watch.check().await;
        assert_eq!(music(&watch), (DiskLevel::Unknown, None));
        assert_eq!(
            started.load(Ordering::SeqCst),
            1,
            "a second read of a hung folder started"
        );

        release.send(()).unwrap();
        // Once the first read returned, the next pass reads the folder again.
        let deadline = std::time::Instant::now() + Duration::from_secs(30);
        while lock(&watch.inner.in_flight).contains(&Key::Folder(PathBuf::from("/music"))) {
            assert!(
                std::time::Instant::now() < deadline,
                "the hung read never returned"
            );
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        release.send(()).unwrap();
        watch.check().await;
        assert_eq!(started.load(Ordering::SeqCst), 2);
    }
}
