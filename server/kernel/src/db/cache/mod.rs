//! The provider cache file: answers from outside services, kept in a
//! throwaway SQLite file of their own so a restart keeps them.
//!
//! `metadata-cache.db` sits in the cache folder, beside the cover cache. If
//! that folder is a symlink or on a network filesystem, the file goes
//! beside `library.db` instead, whose folder has passed the same checks. If
//! that fails too, or another server holds the file's lock, the cache is
//! off for the run: every read misses and every write is dropped, with one
//! warning.
//!
//! The file holds nothing that cannot be fetched again, so it has no
//! migrations: its version stamp is a hash of the schema and the release,
//! and a file with another stamp, or one that is not a database, is deleted
//! and made fresh at start. It is never backed up or exported. A file found
//! corrupt while running turns the cache off for the run and is deleted, so
//! the next start makes a new one.
//!
//! One writer thread owns the write connection. Puts are queued and dropped
//! when the queue is full; deletes, the clear and the sweep wait for their
//! commit. A queued put reads back at once: reads look at the rows still
//! queued before the file, so a page read right after a fetch sees it. The
//! writer keeps the rows under [`CAP_BYTES`] by deleting the oldest in the
//! same transaction that took them over. Reads run on a few
//! read-only connections off the async worker threads, and a read that
//! fails is a miss.

pub(crate) mod sql;

use std::collections::{HashMap, HashSet};
use std::panic::AssertUnwindSafe;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::Duration;

use rusqlite::{
    Connection, ErrorCode, OpenFlags, OptionalExtension as _, Transaction, TransactionBehavior,
    params,
};
use tokio::sync::{Semaphore, mpsc, oneshot};

use super::datalock::DataLock;
use super::fs::{reject_remote_filesystem, reject_symlink};

pub use sql::SCHEMA;

/// The file's name, in the cache folder or beside the database.
pub const FILE_NAME: &str = "metadata-cache.db";

/// Most bytes the rows (keys and values) may hold. A library's own page
/// data is a few tens of MB; the rest is room for what people browse. The
/// file holds only what can be fetched again, so this bounds disk use, not
/// what the app can show.
pub const CAP_BYTES: i64 = 512 * 1024 * 1024;

/// Where eviction stops once the cap is passed, so it does not run again on
/// the very next write.
const EVICT_TO: i64 = CAP_BYTES / 10 * 9;
/// Rows deleted per statement when evicting.
const EVICT_CHUNK: i64 = 500;
/// Rows deleted per transaction when sweeping.
const SWEEP_CHUNK: i64 = 2_000;
/// Puts waiting for the writer before new ones are dropped.
const QUEUE: usize = 1_024;
/// Most queued operations committed in one transaction.
const BATCH: usize = 256;
/// Read-only connections.
const READERS: usize = 4;
/// How long a statement waits on a lock before failing.
const BUSY_TIMEOUT: Duration = Duration::from_secs(2);
/// The WAL is cut back to this size after a checkpoint.
const JOURNAL_SIZE_LIMIT: i64 = 64 * 1024 * 1024;

/// What a row records: an answer, or that the provider has nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CacheKind {
    /// The provider answered; the value is the answer.
    Found,
    /// The provider said there is nothing there.
    Missing,
}

impl CacheKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Found => "found",
            Self::Missing => "missing",
        }
    }

    fn parse(text: &str) -> Option<Self> {
        match text {
            "found" => Some(Self::Found),
            "missing" => Some(Self::Missing),
            _ => None,
        }
    }
}

/// One row to store. Times are milliseconds from the kernel clock.
#[derive(Debug, Clone)]
pub struct CacheRow {
    /// The key, which starts with its source.
    pub key: String,
    /// The source's name, for stats.
    pub source: &'static str,
    /// Found or missing.
    pub kind: CacheKind,
    /// When it was fetched.
    pub fetched_at: i64,
    /// Served without asking again until then.
    pub fresh_until: i64,
    /// Served when the provider fails until then, and swept after.
    pub keep_until: i64,
    /// The answer; `None` exactly when `kind` is missing.
    pub value: Option<Vec<u8>>,
}

/// One stored row as read back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredRow {
    /// Found or missing.
    pub kind: CacheKind,
    /// When it was fetched.
    pub fetched_at: i64,
    /// Fresh until then.
    pub fresh_until: i64,
    /// Kept until then.
    pub keep_until: i64,
    /// The answer, for a found row.
    pub value: Option<Vec<u8>>,
}

/// Rows and bytes one source holds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceUsage {
    /// The source's name as stored.
    pub source: String,
    /// Rows.
    pub rows: u64,
    /// Bytes of keys and values.
    pub bytes: u64,
}

/// Why a waited-on write did not commit.
#[derive(Debug, Clone, thiserror::Error)]
pub enum CacheError {
    /// The cache is off for this run, or shutting down.
    #[error("the provider cache is off")]
    Off,
    /// The transaction failed.
    #[error("the provider cache write failed: {0}")]
    Failed(String),
}

type Ack = oneshot::Sender<Result<u64, CacheError>>;

enum Op {
    /// A row and its place in the queue, to drop it from the queued rows
    /// once it is written.
    Put(Arc<CacheRow>, u64),
    Delete(Vec<String>, Ack),
    Clear(Ack),
    Sweep(i64, Ack),
}

/// The provider cache file. Cloning shares it.
#[derive(Clone)]
pub struct CacheDb {
    inner: Arc<Inner>,
}

impl std::fmt::Debug for CacheDb {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CacheDb")
            .field("path", &self.inner.shared.path)
            .field("on", &self.is_on())
            .finish_non_exhaustive()
    }
}

struct Inner {
    shared: Arc<Shared>,
    queue: Mutex<Option<mpsc::Sender<Op>>>,
    writer: Mutex<Option<JoinHandle<()>>>,
    readers: Arc<Semaphore>,
    #[cfg(any(test, feature = "test-support"))]
    _scratch: Option<super::scratch::ScratchDir>,
}

impl Drop for Inner {
    fn drop(&mut self) {
        // Close the queue and let the writer finish what it holds, so a
        // test's scratch folder outlives the last write into it.
        drop(lock(&self.queue).take());
        if let Some(writer) = lock(&self.writer).take()
            && writer.join().is_err()
        {
            tracing::warn!("provider cache writer ended with a panic");
        }
    }
}

/// What the writer and the readers share.
struct Shared {
    path: PathBuf,
    off: AtomicBool,
    idle: Mutex<Vec<Connection>>,
    read_errors: Mutex<HashSet<String>>,
    write_failing: AtomicBool,
    /// Puts not yet written, by key, with their place in the queue. Each
    /// entry is a put in the queue or in the writer's batch, so the queue's
    /// bound is this map's too.
    queued: Mutex<HashMap<String, (u64, Arc<CacheRow>)>>,
    /// The next put's place in the queue.
    next: AtomicU64,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

impl Shared {
    fn is_off(&self) -> bool {
        self.off.load(Ordering::Acquire)
    }

    /// Note a read that failed: corruption turns the cache off, anything
    /// else is a miss logged once per kind.
    fn read_failed(&self, error: &rusqlite::Error) {
        if is_corrupt(error) {
            self.corrupt(error);
            return;
        }
        let kind = error
            .sqlite_error_code()
            .map_or_else(|| "other".to_owned(), |code| format!("{code:?}"));
        if lock(&self.read_errors).insert(kind.clone()) {
            tracing::warn!(%error, kind, "provider cache read failed; reading it as a miss");
        }
    }

    /// Turn the cache off for the run and delete the file, so the next
    /// start makes a fresh one.
    fn corrupt(&self, error: &rusqlite::Error) {
        if self.off.swap(true, Ordering::AcqRel) {
            return;
        }
        lock(&self.idle).clear();
        tracing::warn!(%error, "provider cache file is damaged; the cache is off until the next start, which makes a new one");
        if let Err(reason) = remove_regular(&self.path) {
            tracing::warn!(reason, "cannot delete the damaged provider cache file");
        }
    }

    fn reader(&self) -> rusqlite::Result<Connection> {
        if let Some(connection) = lock(&self.idle).pop() {
            return Ok(connection);
        }
        let connection = Connection::open_with_flags(
            &self.path,
            OpenFlags::SQLITE_OPEN_READ_ONLY
                | OpenFlags::SQLITE_OPEN_NO_MUTEX
                | OpenFlags::SQLITE_OPEN_NOFOLLOW,
        )?;
        connection.busy_timeout(BUSY_TIMEOUT)?;
        Ok(connection)
    }

    fn give_back(&self, connection: Connection) {
        if !self.is_off() {
            lock(&self.idle).push(connection);
        }
    }

    /// Drop the queued row under `key` if it is still the put at `place`;
    /// a later put of the same key stays.
    fn unqueue(&self, key: &str, place: u64) {
        let mut queued = lock(&self.queued);
        if queued.get(key).is_some_and(|(held, _)| *held == place) {
            queued.remove(key);
        }
    }
}

fn is_corrupt(error: &rusqlite::Error) -> bool {
    matches!(
        error.sqlite_error_code(),
        Some(ErrorCode::DatabaseCorrupt | ErrorCode::NotADatabase)
    )
}

impl CacheDb {
    /// Open the cache in `cache_dir`, or beside `library_db_path` when the
    /// cache folder is refused. Never fails: a cache that cannot open is
    /// off for the run, with one warning.
    pub async fn open(cache_dir: &Path, library_db_path: &Path) -> Self {
        let (cache_dir, library_db_path) = (cache_dir.to_owned(), library_db_path.to_owned());
        match tokio::task::spawn_blocking(move || Self::open_blocking(&cache_dir, &library_db_path))
            .await
        {
            Ok(cache) => cache,
            Err(error) => {
                tracing::warn!(%error, "provider cache did not open; the cache is off for this run");
                Self::off(PathBuf::new())
            }
        }
    }

    /// [`open`](Self::open), on the calling thread.
    pub fn open_blocking(cache_dir: &Path, library_db_path: &Path) -> Self {
        match Self::try_open(cache_dir, library_db_path) {
            Ok(cache) => cache,
            Err(reason) => {
                tracing::warn!(
                    reason,
                    "provider cache is off for this run; provider answers are fetched each time"
                );
                Self::off(cache_dir.join(FILE_NAME))
            }
        }
    }

    /// A cache in a scratch folder removed when the last handle drops.
    #[cfg(any(test, feature = "test-support"))]
    pub fn scratch() -> Self {
        let dir = match super::scratch::ScratchDir::new("provider-cache") {
            Ok(dir) => dir,
            Err(error) => {
                tracing::warn!(%error, "no scratch folder for the provider cache");
                return Self::off(PathBuf::new());
            }
        };
        let mut cache = Self::open_blocking(dir.path(), &dir.path().join("library.db"));
        if let Some(inner) = Arc::get_mut(&mut cache.inner) {
            inner._scratch = Some(dir);
        }
        cache
    }

    fn off(path: PathBuf) -> Self {
        Self {
            inner: Arc::new(Inner {
                shared: Arc::new(Shared {
                    path,
                    off: AtomicBool::new(true),
                    idle: Mutex::new(Vec::new()),
                    read_errors: Mutex::new(HashSet::new()),
                    write_failing: AtomicBool::new(false),
                    queued: Mutex::new(HashMap::new()),
                    next: AtomicU64::new(0),
                }),
                queue: Mutex::new(None),
                writer: Mutex::new(None),
                readers: Arc::new(Semaphore::new(READERS)),
                #[cfg(any(test, feature = "test-support"))]
                _scratch: None,
            }),
        }
    }

    fn try_open(cache_dir: &Path, library_db_path: &Path) -> Result<Self, String> {
        let preferred = cache_dir.join(FILE_NAME);
        let path = match checked(&preferred) {
            Ok(()) => preferred,
            Err(refused) => {
                let beside = library_db_path
                    .parent()
                    .map_or_else(|| PathBuf::from(FILE_NAME), |dir| dir.join(FILE_NAME));
                checked(&beside)
                    .map_err(|again| format!("{refused}; and beside the database: {again}"))?;
                tracing::warn!(
                    refused,
                    path = %beside.display(),
                    "cache folder refused for the provider cache; keeping it beside the database"
                );
                beside
            }
        };
        if path == library_db_path {
            return Err("the provider cache path is the database's own".to_owned());
        }
        let data_lock = DataLock::server(&path).map_err(|error| error.to_string())?;
        let (connection, total) = prepare_file(&path, library_db_path)?;
        let shared = Arc::new(Shared {
            path,
            off: AtomicBool::new(false),
            idle: Mutex::new(Vec::new()),
            read_errors: Mutex::new(HashSet::new()),
            write_failing: AtomicBool::new(false),
            queued: Mutex::new(HashMap::new()),
            next: AtomicU64::new(0),
        });
        let (sender, receiver) = mpsc::channel(QUEUE);
        let writer_shared = Arc::clone(&shared);
        let writer = std::thread::Builder::new()
            .name("droppedneedle-provider-cache".to_owned())
            .spawn(move || {
                let shared = Arc::clone(&writer_shared);
                let ran = std::panic::catch_unwind(AssertUnwindSafe(move || {
                    Writer {
                        connection,
                        shared: writer_shared,
                        total,
                        _lock: data_lock,
                    }
                    .run(receiver);
                }));
                if ran.is_err() {
                    shared.off.store(true, Ordering::Release);
                    tracing::error!("provider cache writer stopped unexpectedly; the cache is off");
                }
            })
            .map_err(|error| error.to_string())?;
        Ok(Self {
            inner: Arc::new(Inner {
                shared,
                queue: Mutex::new(Some(sender)),
                writer: Mutex::new(Some(writer)),
                readers: Arc::new(Semaphore::new(READERS)),
                #[cfg(any(test, feature = "test-support"))]
                _scratch: None,
            }),
        })
    }

    /// Whether the cache is working.
    #[must_use]
    pub fn is_on(&self) -> bool {
        !self.inner.shared.is_off()
    }

    /// The row under `key`, or `None` when there is none, the cache is off
    /// or the read failed. A row still queued for the writer is read from
    /// the queue.
    pub async fn get(&self, key: &str) -> Option<StoredRow> {
        if !self.is_on() {
            return None;
        }
        if let Some((_, row)) = lock(&self.inner.shared.queued).get(key) {
            return Some(StoredRow {
                kind: row.kind,
                fetched_at: row.fetched_at,
                fresh_until: row.fresh_until,
                keep_until: row.keep_until,
                value: row.value.clone(),
            });
        }
        let key = key.to_owned();
        self.read(move |connection| {
            connection
                .query_row(sql::GET.text(), [&key], |row| {
                    let kind: String = row.get(0)?;
                    Ok((kind, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?))
                })
                .optional()
        })
        .await
        .flatten()
        .and_then(|(kind, fetched_at, fresh_until, keep_until, value)| {
            Some(StoredRow {
                kind: CacheKind::parse(&kind)?,
                fetched_at,
                fresh_until,
                keep_until,
                value,
            })
        })
    }

    /// Rows and bytes per source; empty when the cache is off.
    pub async fn stats(&self) -> Vec<SourceUsage> {
        self.read(|connection| {
            let mut statement = connection.prepare_cached(sql::BY_SOURCE.text())?;
            let rows = statement.query_map([], |row| {
                Ok(SourceUsage {
                    source: row.get(0)?,
                    rows: row.get::<_, i64>(1)?.try_into().unwrap_or(0),
                    bytes: row.get::<_, i64>(2)?.try_into().unwrap_or(0),
                })
            })?;
            rows.collect::<rusqlite::Result<Vec<_>>>()
        })
        .await
        .unwrap_or_default()
    }

    async fn read<R, F>(&self, op: F) -> Option<R>
    where
        R: Send + 'static,
        F: FnOnce(&Connection) -> rusqlite::Result<R> + Send + 'static,
    {
        if !self.is_on() {
            return None;
        }
        // Waits for a free connection: a read never misses because the
        // others are busy.
        let permit = Arc::clone(&self.inner.readers).acquire_owned().await.ok()?;
        let shared = Arc::clone(&self.inner.shared);
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            let connection = match shared.reader() {
                Ok(connection) => connection,
                Err(error) => {
                    shared.read_failed(&error);
                    return None;
                }
            };
            match op(&connection) {
                Ok(value) => {
                    shared.give_back(connection);
                    Some(value)
                }
                Err(error) => {
                    shared.read_failed(&error);
                    None
                }
            }
        })
        .await
        .ok()
        .flatten()
    }

    fn sender(&self) -> Option<mpsc::Sender<Op>> {
        if !self.is_on() {
            return None;
        }
        lock(&self.inner.queue).clone()
    }

    /// Queue a row for the writer. `false` when it was dropped: the cache
    /// is off or the queue is full.
    pub fn put(&self, row: CacheRow) -> bool {
        let Some(sender) = self.sender() else {
            return false;
        };
        let shared = &self.inner.shared;
        let place = shared.next.fetch_add(1, Ordering::Relaxed);
        let (key, row) = (row.key.clone(), Arc::new(row));
        // Listed before it is sent, so the writer never unlists it first.
        lock(&shared.queued).insert(key.clone(), (place, Arc::clone(&row)));
        match sender.try_send(Op::Put(row, place)) {
            Ok(()) => true,
            Err(_) => {
                shared.unqueue(&key, place);
                tracing::debug!("provider cache queue full; an answer was not stored");
                false
            }
        }
    }

    async fn wait(&self, make: impl FnOnce(Ack) -> Op) -> Result<u64, CacheError> {
        let sender = self.sender().ok_or(CacheError::Off)?;
        let (ack, answer) = oneshot::channel();
        sender.send(make(ack)).await.map_err(|_| CacheError::Off)?;
        answer.await.map_err(|_| CacheError::Off)?
    }

    /// Delete the rows under `keys`, once committed. Answers how many went.
    pub async fn delete(&self, keys: Vec<String>) -> Result<u64, CacheError> {
        self.wait(|ack| Op::Delete(keys, ack)).await
    }

    /// Delete every row, once committed. Answers how many went.
    pub async fn clear(&self) -> Result<u64, CacheError> {
        self.wait(Op::Clear).await
    }

    /// Delete the rows kept past `now_ms`, in chunks, then cut the WAL
    /// back. Answers how many went.
    pub async fn sweep(&self, now_ms: i64) -> Result<u64, CacheError> {
        self.wait(|ack| Op::Sweep(now_ms, ack)).await
    }

    /// Stop at shutdown, after the loops: close the queue, let the writer
    /// commit what it holds within `grace`, join it, and so release the
    /// file's lock.
    pub async fn shutdown_within(&self, grace: Duration) {
        drop(lock(&self.inner.queue).take());
        let writer = lock(&self.inner.writer).take();
        if let Some(writer) = writer {
            match tokio::time::timeout(grace, tokio::task::spawn_blocking(move || writer.join()))
                .await
            {
                Ok(Ok(Ok(()))) => {}
                Ok(_) => tracing::warn!("provider cache writer ended with a panic"),
                Err(_) => tracing::warn!(
                    ?grace,
                    "provider cache writer still busy at the shutdown bound"
                ),
            }
        }
        self.inner.shared.off.store(true, Ordering::Release);
        lock(&self.inner.shared.idle).clear();
    }
}

/// Refuse a cache path that is a symlink or on a network filesystem.
fn checked(path: &Path) -> Result<(), String> {
    if let Some(parent) = path.parent().filter(|dir| !dir.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    reject_symlink(path).map_err(|error| error.to_string())?;
    reject_remote_filesystem(path).map_err(|error| error.to_string())
}

fn companions(path: &Path) -> [PathBuf; 3] {
    let with = |suffix: &str| {
        let mut name = path.as_os_str().to_owned();
        name.push(suffix);
        PathBuf::from(name)
    };
    [path.to_owned(), with("-wal"), with("-shm")]
}

/// Delete `path` when it is a regular file; a missing one is fine, and
/// anything else is refused rather than touched.
fn remove_regular(path: &Path) -> Result<(), String> {
    match std::fs::symlink_metadata(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.to_string()),
        Ok(meta) if meta.file_type().is_file() => {
            std::fs::remove_file(path).map_err(|error| error.to_string())
        }
        Ok(_) => Err(format!("{} is not a regular file", path.display())),
    }
}

/// The file's version stamp: the schema and the release, so either change
/// starts a new file. Never 0, which a new or half-made file reads.
fn version() -> i32 {
    let mut hash: u32 = 0x811c_9dc5;
    for byte in SCHEMA
        .bytes()
        .chain(":".bytes())
        .chain(env!("CARGO_PKG_VERSION").bytes())
    {
        hash ^= u32::from(byte);
        hash = hash.wrapping_mul(0x0100_0193);
    }
    i32::from_ne_bytes((hash | 1).to_ne_bytes())
}

/// Open the file as the writer, making it fresh when it is missing, of
/// another version or not a database.
fn prepare_file(path: &Path, library_db_path: &Path) -> Result<(Connection, i64), String> {
    refuse_shared_inode(path, library_db_path)?;
    let files = companions(path);
    for file in &files {
        match std::fs::symlink_metadata(file) {
            Ok(meta) if !meta.file_type().is_file() => {
                return Err(format!("{} is not a regular file", file.display()));
            }
            _ => {}
        }
    }
    if !path.exists() {
        // A journal with no database beside it would be replayed into
        // the new file.
        for file in &files[1..] {
            remove_regular(file)?;
        }
    }
    if path.exists() {
        match open_current(path) {
            Ok(Some(opened)) => return Ok(opened),
            Ok(None) => {
                tracing::info!("provider cache file is from another version; starting it fresh")
            }
            Err(error) => {
                tracing::info!(%error, "provider cache file does not open; starting it fresh");
            }
        }
        for file in &files {
            remove_regular(file)?;
        }
    }
    create(path)
        .map(|connection| (connection, 0))
        .map_err(|error| error.to_string())
}

/// Refuse a cache file that is a hard link, or the database's own inode:
/// opening it would write into a file that is not the cache's alone.
fn refuse_shared_inode(path: &Path, library_db_path: &Path) -> Result<(), String> {
    use std::os::unix::fs::MetadataExt as _;

    let Ok(cache) = std::fs::symlink_metadata(path) else {
        return Ok(());
    };
    if cache.nlink() > 1 {
        return Err(format!("{} has other hard links", path.display()));
    }
    if let Ok(database) = std::fs::metadata(library_db_path)
        && database.dev() == cache.dev()
        && database.ino() == cache.ino()
    {
        return Err(format!("{} is the database file", path.display()));
    }
    Ok(())
}

fn open_flags(create: bool) -> OpenFlags {
    let flags = OpenFlags::SQLITE_OPEN_READ_WRITE
        | OpenFlags::SQLITE_OPEN_NO_MUTEX
        | OpenFlags::SQLITE_OPEN_NOFOLLOW;
    if create {
        flags | OpenFlags::SQLITE_OPEN_CREATE
    } else {
        flags
    }
}

/// Times the writer has waited for the file lock, in test builds, so a
/// test can tell the writer is waiting without a sleep.
#[cfg(test)]
static LOCK_WAITS: AtomicU64 = AtomicU64::new(0);

/// The test builds' busy handler: counts the wait, then waits as
/// [`BUSY_TIMEOUT`] would, a millisecond a turn.
#[cfg(test)]
fn counted_wait(turn: i32) -> bool {
    LOCK_WAITS.fetch_add(1, Ordering::AcqRel);
    std::thread::sleep(Duration::from_millis(1));
    u128::try_from(turn).is_ok_and(|turn| turn < BUSY_TIMEOUT.as_millis())
}

fn writer_pragmas(connection: &Connection) -> rusqlite::Result<()> {
    connection.busy_timeout(BUSY_TIMEOUT)?;
    #[cfg(test)]
    connection.busy_handler(Some(counted_wait))?;
    let _mode: String = connection.query_row("PRAGMA journal_mode = WAL", [], |row| row.get(0))?;
    connection.execute_batch(&format!(
        "PRAGMA synchronous = NORMAL; PRAGMA journal_size_limit = {JOURNAL_SIZE_LIMIT};"
    ))
}

/// The open file and the bytes its rows hold, or `None` when its stamp
/// is not this build's. The total is read from every table page, so a
/// damaged table fails here and the file is made fresh.
fn open_current(path: &Path) -> rusqlite::Result<Option<(Connection, i64)>> {
    let connection = Connection::open_with_flags(path, open_flags(false))?;
    let found: i32 = connection.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    if found != version() {
        return Ok(None);
    }
    writer_pragmas(&connection)?;
    let total = connection.query_row(sql::TOTAL_SIZE.text(), [], |row| row.get(0))?;
    Ok(Some((connection, total)))
}

fn create(path: &Path) -> rusqlite::Result<Connection> {
    let mut connection = Connection::open_with_flags(path, open_flags(true))?;
    writer_pragmas(&connection)?;
    let transaction = connection.transaction()?;
    transaction.execute_batch(SCHEMA)?;
    transaction.execute_batch(&format!("PRAGMA user_version = {}", version()))?;
    transaction.commit()?;
    Ok(connection)
}

/// The writer thread's state.
struct Writer {
    connection: Connection,
    shared: Arc<Shared>,
    /// Bytes every row holds, read once at open and kept in step.
    total: i64,
    /// Held while the writer runs; released when it ends.
    _lock: DataLock,
}

impl Writer {
    fn run(mut self, mut receiver: mpsc::Receiver<Op>) {
        while let Some(first) = receiver.blocking_recv() {
            let mut ops = vec![first];
            while ops.len() < BATCH {
                match receiver.try_recv() {
                    Ok(op) => ops.push(op),
                    Err(_) => break,
                }
            }
            if self.shared.is_off() {
                break;
            }
            let mut batch = Vec::new();
            for op in ops {
                match op {
                    Op::Sweep(now_ms, ack) => {
                        self.commit(std::mem::take(&mut batch));
                        let swept = self.sweep(now_ms);
                        let _ = ack.send(swept);
                    }
                    other => batch.push(other),
                }
            }
            self.commit(batch);
            if self.shared.is_off() {
                break;
            }
        }
    }

    /// Take the write lock at `BEGIN`, where a busy file waits out
    /// [`BUSY_TIMEOUT`]. A deferred transaction that reads first gets no
    /// wait: SQLite fails its upgrade to a write at once while another
    /// connection holds the lock, and the batch's rows would be dropped.
    fn begin(&mut self) -> rusqlite::Result<Transaction<'_>> {
        self.connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
    }

    fn failed(&self, error: &rusqlite::Error) -> CacheError {
        if is_corrupt(error) {
            self.shared.corrupt(error);
        } else if !self.shared.write_failing.swap(true, Ordering::AcqRel) {
            tracing::warn!(%error, "provider cache write failed; answers are not being stored");
        }
        CacheError::Failed(error.to_string())
    }

    fn succeeded(&self) {
        if self.shared.write_failing.swap(false, Ordering::AcqRel) {
            tracing::info!("provider cache writes work again");
        }
    }

    /// One transaction for a batch of puts, deletes and clears; each
    /// waiter is answered after the commit.
    fn commit(&mut self, ops: Vec<Op>) {
        if ops.is_empty() {
            return;
        }
        let mut total = self.total;
        let mut answers = Vec::new();
        let queued: Vec<(String, u64)> = ops
            .iter()
            .filter_map(|op| match op {
                Op::Put(row, place) => Some((row.key.clone(), *place)),
                _ => None,
            })
            .collect();
        let outcome = (|| {
            let transaction = self.begin()?;
            for op in ops {
                match op {
                    Op::Put(row, _) => put(&transaction, &row, &mut total)?,
                    Op::Delete(keys, ack) => {
                        let mut gone = 0;
                        for key in keys {
                            gone += take_sizes(
                                &transaction,
                                sql::DELETE_KEY.text(),
                                params![key],
                                &mut total,
                            )?;
                        }
                        answers.push((ack, gone));
                    }
                    Op::Clear(ack) => {
                        let gone = take_sizes(&transaction, sql::CLEAR.text(), [], &mut total)?;
                        answers.push((ack, gone));
                    }
                    Op::Sweep(..) => {}
                }
            }
            if total > CAP_BYTES {
                while total > EVICT_TO
                    && take_sizes(
                        &transaction,
                        sql::DELETE_OLDEST.text(),
                        [EVICT_CHUNK],
                        &mut total,
                    )? > 0
                {}
            }
            transaction.commit()
        })();
        // Committed, the rows read from the file; failed, they are gone.
        for (key, place) in queued {
            self.shared.unqueue(&key, place);
        }
        match outcome {
            Ok(()) => {
                self.total = total;
                self.succeeded();
                for (ack, gone) in answers {
                    let _ = ack.send(Ok(gone));
                }
            }
            Err(error) => {
                let failure = self.failed(&error);
                for (ack, _) in answers {
                    let _ = ack.send(Err(failure.clone()));
                }
            }
        }
    }

    fn sweep(&mut self, now_ms: i64) -> Result<u64, CacheError> {
        let mut swept = 0;
        loop {
            let mut total = self.total;
            let chunk = (|| {
                let transaction = self.begin()?;
                let gone = take_sizes(
                    &transaction,
                    sql::DELETE_EXPIRED.text(),
                    params![now_ms, SWEEP_CHUNK],
                    &mut total,
                )?;
                transaction.commit()?;
                Ok(gone)
            })();
            match chunk {
                Ok(gone) => {
                    self.total = total;
                    swept += gone;
                    if gone < SWEEP_CHUNK.unsigned_abs() {
                        break;
                    }
                }
                Err(error) => return Err(self.failed(&error)),
            }
        }
        let truncated: rusqlite::Result<(i64, i64, i64)> =
            self.connection
                .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |row| {
                    Ok((row.get(0)?, row.get(1)?, row.get(2)?))
                });
        if let Err(error) = truncated {
            return Err(self.failed(&error));
        }
        self.succeeded();
        Ok(swept)
    }
}

fn put(transaction: &Transaction<'_>, row: &CacheRow, total: &mut i64) -> rusqlite::Result<()> {
    let value_len = row.value.as_ref().map_or(0, Vec::len);
    let size = i64::try_from(row.key.len() + value_len).unwrap_or(i64::MAX);
    let old: Option<i64> = transaction
        .query_row(sql::SIZE_OF.text(), [&row.key], |found| found.get(0))
        .optional()?;
    transaction.execute(
        sql::PUT.text(),
        params![
            row.key,
            row.source,
            row.kind.as_str(),
            size,
            row.fetched_at,
            row.fresh_until,
            row.keep_until,
            row.value,
        ],
    )?;
    *total += size - old.unwrap_or(0);
    Ok(())
}

/// Run a `DELETE ... RETURNING size`, taking the sizes off `total`.
/// Answers the rows deleted.
fn take_sizes(
    transaction: &Transaction<'_>,
    statement: &str,
    values: impl rusqlite::Params,
    total: &mut i64,
) -> rusqlite::Result<u64> {
    let mut prepared = transaction.prepare_cached(statement)?;
    let mut rows = prepared.query(values)?;
    let mut gone = 0;
    while let Some(row) = rows.next()? {
        *total -= row.get::<_, i64>(0)?;
        gone += 1;
    }
    Ok(gone)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(key: &str, value: &[u8]) -> CacheRow {
        CacheRow {
            key: key.to_owned(),
            source: "musicbrainz",
            kind: CacheKind::Found,
            fetched_at: 1,
            fresh_until: 10,
            keep_until: 100,
            value: Some(value.to_vec()),
        }
    }

    async fn settled(cache: &CacheDb, key: &str) -> Option<StoredRow> {
        // A delete of no keys commits after every put queued before it.
        cache.delete(Vec::new()).await.expect("queue drains");
        cache.get(key).await
    }

    /// A row reads back as soon as it is queued, before the writer commits
    /// it: a page that reads right after a fetch sees that fetch's answer.
    #[tokio::test]
    async fn a_queued_row_reads_before_it_commits() {
        let cache = CacheDb::scratch();
        assert!(cache.is_on());
        // Another connection holds the write lock, so the writer cannot
        // commit until it lets go.
        let holder = Connection::open(&cache.inner.shared.path).unwrap();
        holder.execute_batch("BEGIN IMMEDIATE").unwrap();
        assert!(cache.put(row("musicbrainz:queued", b"answer")));
        let read = cache.get("musicbrainz:queued").await;
        holder.execute_batch("ROLLBACK").unwrap();
        assert_eq!(read.and_then(|row| row.value), Some(b"answer".to_vec()));
    }

    /// A put made while another connection holds the write lock waits for
    /// it: the row reads back while the writer waits, and lands once the
    /// lock is let go instead of being dropped.
    #[tokio::test]
    async fn a_put_waits_for_the_write_lock() {
        let cache = CacheDb::scratch();
        assert!(cache.is_on());
        let shared = Arc::clone(&cache.inner.shared);
        let holder = Connection::open(&shared.path).unwrap();
        holder.execute_batch("BEGIN IMMEDIATE").unwrap();
        let waits = LOCK_WAITS.load(Ordering::Acquire);
        assert!(cache.put(row("musicbrainz:held", b"answer")));
        // Until the writer is waiting on the lock, or its batch failed.
        while LOCK_WAITS.load(Ordering::Acquire) == waits
            && !shared.write_failing.load(Ordering::Acquire)
        {
            tokio::task::yield_now().await;
        }
        let read = cache.get("musicbrainz:held").await;
        assert_eq!(read.and_then(|row| row.value), Some(b"answer".to_vec()));
        holder.execute_batch("ROLLBACK").unwrap();
        let landed = settled(&cache, "musicbrainz:held").await;
        assert_eq!(landed.and_then(|row| row.value), Some(b"answer".to_vec()));
    }

    /// A file of another version, one that is not a database, and one
    /// with damaged table pages are replaced at open and the cache works.
    #[tokio::test]
    async fn foreign_files_are_replaced() {
        let dir = crate::db::scratch::ScratchDir::new("cache-foreign").unwrap();
        let path = dir.path().join(FILE_NAME);
        std::fs::write(&path, b"not a database at all, just bytes").unwrap();
        let cache = CacheDb::open(dir.path(), &dir.path().join("library.db")).await;
        assert!(cache.is_on());
        assert!(cache.put(row("musicbrainz:a", b"1")));
        assert!(settled(&cache, "musicbrainz:a").await.is_some());
        cache.shutdown_within(Duration::from_secs(5)).await;
        drop(cache);

        let old = Connection::open(&path).unwrap();
        old.execute_batch("PRAGMA user_version = 7").unwrap();
        drop(old);
        let cache = CacheDb::open(dir.path(), &dir.path().join("library.db")).await;
        assert!(cache.is_on());
        assert!(
            cache.get("musicbrainz:a").await.is_none(),
            "the old file's rows are gone"
        );
        let value = vec![7_u8; 2_000];
        for at in 0..100 {
            assert!(cache.put(row(&format!("musicbrainz:{at}"), &value)));
        }
        assert!(settled(&cache, "musicbrainz:0").await.is_some());
        cache.shutdown_within(Duration::from_secs(5)).await;
        drop(cache);

        // Fold the journal in, then keep this build's stamp on page one and
        // put garbage on every page after it.
        let fold = Connection::open(&path).unwrap();
        fold.execute_batch("PRAGMA wal_checkpoint(TRUNCATE)")
            .unwrap();
        drop(fold);
        let mut bytes = std::fs::read(&path).unwrap();
        assert!(bytes.len() > 8_192, "the rows span several pages");
        bytes[4_096..].fill(0xA5);
        std::fs::write(&path, &bytes).unwrap();
        for file in &companions(&path)[1..] {
            remove_regular(file).unwrap();
        }
        let cache = CacheDb::open(dir.path(), &dir.path().join("library.db")).await;
        assert!(cache.is_on(), "a damaged file is made fresh");
        assert!(cache.put(row("musicbrainz:b", b"2")));
        assert!(settled(&cache, "musicbrainz:b").await.is_some());
        cache.shutdown_within(Duration::from_secs(5)).await;
    }
}
