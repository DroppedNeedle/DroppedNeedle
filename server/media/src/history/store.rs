//! The listening history store: the imported listens, each person's
//! import row, and the files waiting to be read in the cache folder.
//!
//! One store is built at startup and shared by the upload and status
//! routes, the import job and Discover's forget. A file waiting to be read
//! is `<cache>/listening-history/<person>-<import id>.zip`, where the
//! person's part is a hash of their id, so a name never carries text from
//! a request. A file no `importing` row names is left over (an account
//! deleted, a crash after the last month was read, an upload that never
//! started its import) and is swept at the start of every import run once
//! it is an hour old, so an upload still arriving is never taken.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::UNIX_EPOCH;

use rusqlite::{OptionalExtension, Transaction, params};
use sha2::{Digest as _, Sha256};

use droppedneedle_kernel::clock::Clock;
use droppedneedle_kernel::db::time::Millis;
use droppedneedle_kernel::db::{CancelFlag, ChunkedError, Db, DbError, Lane};
use droppedneedle_kernel::ids::IdGenerator;
use droppedneedle_kernel::jobs::queue::{JobError, JobQueue};
use droppedneedle_kernel::platform::Platform;
use droppedneedle_providers::listenbrainz::history::HistoryListen;

use super::import::{IMPORT_JOB, KIND};
use super::{HistoryImportSource, HistoryImportState, sql};

/// The folder under the cache folder where uploads wait to be read.
const FOLDER: &str = "listening-history";
/// Listens written per transaction.
const CHUNK_LISTENS: usize = 1000;
/// How long a file no import names is kept: an upload still arriving
/// writes to its file all along, and starts its import right after.
const LEFTOVER_AFTER_MS: i64 = 3_600_000;

/// Where one person's history stands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistoryStatus {
    /// Their import's state; `None` while they never imported.
    pub state: Option<HistoryImportState>,
    /// Where the last import's file came from.
    pub source: Option<HistoryImportSource>,
    /// The ListenBrainz account the history is from; `None` for an upload.
    pub account: Option<String>,
    /// Listens kept.
    pub listens: i64,
    /// Plays here, in the web player and in apps.
    pub plays: i64,
    /// The oldest listen kept, unix ms.
    pub oldest_listened_at: Option<i64>,
    /// The newest listen kept, unix ms.
    pub newest_listened_at: Option<i64>,
    /// When the last import finished, unix ms.
    pub last_synced_at: Option<i64>,
}

/// The file an import reads and how far it got.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reading {
    /// The file's import id.
    pub import_id: String,
    /// How many of its month files are read.
    pub entries_read: usize,
}

/// Why an import write did not land.
#[derive(Debug, thiserror::Error)]
pub enum WriteError {
    /// A forget, a new upload or another run moved the import on: this
    /// run writes nothing more.
    #[error("the import moved on")]
    MovedOn,
    /// The jobs table refused the write.
    #[error(transparent)]
    Job(#[from] JobError),
}

impl From<DbError> for WriteError {
    fn from(error: DbError) -> Self {
        Self::Job(JobError::Db(error))
    }
}

impl From<rusqlite::Error> for WriteError {
    fn from(error: rusqlite::Error) -> Self {
        Self::from(DbError::from(error))
    }
}

impl WriteError {
    /// True when the database was busy: try again later.
    pub fn is_busy(&self) -> bool {
        matches!(self, Self::Job(error) if error.is_busy())
    }
}

/// Why a forget of the imported history did not land.
#[derive(Debug, thiserror::Error)]
pub enum ForgetError {
    /// The database refused the write.
    #[error(transparent)]
    Db(#[from] DbError),
    /// The jobs table refused to cancel the person's imports.
    #[error(transparent)]
    Jobs(#[from] JobError),
}

impl From<rusqlite::Error> for ForgetError {
    fn from(error: rusqlite::Error) -> Self {
        Self::Db(DbError::from(error))
    }
}

/// The imported listens and import rows, over the one database.
pub struct HistoryStore {
    db: Db,
    clock: Arc<dyn Clock>,
    ids: Arc<dyn IdGenerator>,
    jobs: JobQueue,
    dir: PathBuf,
}

impl HistoryStore {
    /// The store over `platform`'s database, clock and ids, with uploads
    /// waiting under its cache folder and import jobs queued on `jobs`.
    pub fn new(platform: &Platform, jobs: JobQueue) -> Self {
        Self {
            db: platform.db.clone(),
            clock: platform.clock.clone(),
            ids: platform.ids.clone(),
            jobs,
            dir: platform.config.cache_dir.join(FOLDER),
        }
    }

    /// The folder uploads wait in.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// A fresh import id and the file an upload for `user_id` lands in.
    pub(crate) fn new_upload(&self, user_id: &str) -> (String, PathBuf) {
        let import_id: String = self
            .ids
            .new_id()
            .chars()
            .filter(char::is_ascii_alphanumeric)
            .collect();
        let path = self.archive(user_id, &import_id);
        (import_id, path)
    }

    /// The file `import_id` of `user_id` reads.
    pub(crate) fn archive(&self, user_id: &str, import_id: &str) -> PathBuf {
        self.dir.join(file_name(user_id, import_id))
    }

    /// Start reading the uploaded `import_id` for `user_id`: the person's
    /// earlier import jobs are cancelled and one new one queued, in one
    /// write. The import owns the file once this answers.
    pub(crate) async fn start_upload(
        &self,
        user_id: &str,
        import_id: &str,
    ) -> Result<(), JobError> {
        let (user, import, now) = (user_id.to_owned(), import_id.to_owned(), self.now());
        let jobs = self.jobs.clone();
        self.db
            .write(Lane::Foreground, "history-upload", move |tx| {
                tx.execute(
                    sql::START_UPLOAD.text(),
                    params![
                        user,
                        HistoryImportState::Importing,
                        HistoryImportSource::Upload,
                        import,
                        now
                    ],
                )?;
                jobs.cancel(tx, KIND, &user)?;
                jobs.enqueue(tx, KIND, &user, &IMPORT_JOB)?;
                Ok::<_, JobError>(())
            })
            .await?;
        self.jobs.wake();
        Ok(())
    }

    /// Where `user_id`'s history stands, in one statement.
    pub async fn status(&self, user_id: &str) -> Result<HistoryStatus, DbError> {
        let user = user_id.to_owned();
        let row = self
            .db
            .read("history-status", move |conn| {
                Ok::<_, DbError>(conn.query_row(
                    sql::STATUS.text(),
                    [user],
                    sql::StatusRow::read,
                )?)
            })
            .await?;
        Ok(HistoryStatus {
            state: row.state,
            source: row.source,
            account: row.account,
            listens: row.listens,
            plays: row.plays,
            oldest_listened_at: row.oldest_listened_at.map(|at| at.0),
            newest_listened_at: row.newest_listened_at.map(|at| at.0),
            last_synced_at: row.last_synced_at.map(|at| at.0),
        })
    }

    /// The file `user_id`'s import reads and how far it got; `None` when
    /// nothing is being read.
    pub async fn reading(&self, user_id: &str) -> Result<Option<Reading>, DbError> {
        let user = user_id.to_owned();
        let row = self
            .db
            .read("history-reading", move |conn| {
                Ok::<_, DbError>(
                    conn.query_row(
                        sql::READING.text(),
                        params![user, HistoryImportState::Importing],
                        sql::ReadingRow::read,
                    )
                    .optional()?,
                )
            })
            .await?;
        Ok(row.map(|row| Reading {
            import_id: row.import_id,
            entries_read: usize::try_from(row.entries_read).unwrap_or(usize::MAX),
        }))
    }

    /// Keep one month file's `listens` for `user_id`, in chunks, then count
    /// the month read and, in that same write, queue the next run (or
    /// finish when `last`) and complete this one through `complete`. Every
    /// chunk first checks the import still reads `at`, so a forget or a
    /// new upload in between stops it. Answers the listens added.
    pub async fn write_month(
        &self,
        user_id: &str,
        at: &Reading,
        listens: Vec<HistoryListen>,
        last: bool,
        complete: impl FnOnce(&Transaction) -> Result<(), JobError> + Send + 'static,
    ) -> Result<usize, WriteError> {
        let now = self.now();
        let rows: Vec<HistoryListen> = listens
            .into_iter()
            .filter(|listen| listened_ms(listen, now).is_some())
            .collect();
        let (user, import, read) = (user_id.to_owned(), at.import_id.clone(), at.entries_read);
        let read_at = i64::try_from(read).unwrap_or(i64::MAX);
        let added = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        if !rows.is_empty() {
            let (user, import, added) = (user.clone(), import.clone(), added.clone());
            let written = self
                .db
                .write_chunked(
                    "history-listens",
                    rows,
                    CHUNK_LISTENS,
                    &CancelFlag::never(),
                    move |chunk: &[HistoryListen], tx: &Transaction| {
                        still_reading(tx, &user, &import, read_at)?;
                        let mut insert = tx.prepare_cached(sql::INSERT_LISTEN.text())?;
                        for listen in chunk {
                            let Some(at) = listened_ms(listen, now) else {
                                continue;
                            };
                            let kept = insert.execute(params![
                                user,
                                at,
                                listen.recording_mbid,
                                listen.artist_mbid,
                                listen.artist_name,
                                listen.track_name,
                                listen.release_name,
                                listen.release_mbid,
                                Millis(now),
                            ])?;
                            added.fetch_add(kept, std::sync::atomic::Ordering::Relaxed);
                        }
                        Ok::<_, WriteError>(())
                    },
                )
                .await;
            match written {
                Ok(_) => {}
                Err(ChunkedError::Failed { error, .. }) => return Err(error),
                Err(ChunkedError::Cancelled { .. }) => return Err(WriteError::MovedOn),
            }
        }
        let jobs = self.jobs.clone();
        self.db
            .write(Lane::Background, "history-month", move |tx| {
                let changed = if last {
                    tx.execute(
                        sql::FINISHED.text(),
                        params![
                            user,
                            HistoryImportState::Importing,
                            import,
                            read_at,
                            HistoryImportState::Current,
                            Millis(now)
                        ],
                    )?
                } else {
                    tx.execute(
                        sql::MONTH_READ.text(),
                        params![
                            user,
                            HistoryImportState::Importing,
                            import,
                            read_at,
                            Millis(now)
                        ],
                    )?
                };
                if changed == 0 {
                    return Err(WriteError::MovedOn);
                }
                if !last {
                    jobs.enqueue(tx, KIND, &user, &IMPORT_JOB)?;
                }
                complete(tx)?;
                Ok(())
            })
            .await?;
        Ok(added.load(std::sync::atomic::Ordering::Relaxed))
    }

    /// Mark `user_id`'s import of `import_id` failed (whichever file it
    /// reads, with none named), completing the run through `complete` in
    /// the same write. A forget or new upload in between leaves the row as
    /// it is.
    pub(crate) async fn fail(
        &self,
        user_id: &str,
        import_id: Option<&str>,
        complete: impl FnOnce(&Transaction) -> Result<(), JobError> + Send + 'static,
    ) -> Result<(), WriteError> {
        let (user, import, now) = (user_id.to_owned(), import_id.map(str::to_owned), self.now());
        self.db
            .write(Lane::Background, "history-failed", move |tx| {
                tx.execute(
                    sql::FAILED.text(),
                    params![
                        user,
                        HistoryImportState::Importing,
                        import,
                        HistoryImportState::Failed,
                        Millis(now)
                    ],
                )?;
                complete(tx)?;
                Ok::<_, WriteError>(())
            })
            .await
    }

    /// Forget `user_id`'s imported history in one write (see
    /// [`super::forget`]), then delete their files waiting to be read.
    pub(crate) async fn forget(&self, user_id: &str) -> Result<usize, ForgetError> {
        let (user, now) = (user_id.to_owned(), self.now());
        let jobs = self.jobs.clone();
        let removed = self
            .db
            .write(Lane::Foreground, "history-forget", move |tx| {
                let removed = tx.execute(sql::FORGET_LISTENS.text(), [&user])?;
                tx.execute(
                    sql::FORGET_IMPORT.text(),
                    params![user, HistoryImportState::Forgotten, Millis(now)],
                )?;
                jobs.cancel(tx, KIND, &user)?;
                Ok::<_, ForgetError>(removed)
            })
            .await?;
        self.remove_unread(user_id).await;
        Ok(removed)
    }

    /// Delete `user_id`'s file of `import_id` once it is read.
    pub(crate) async fn remove_file(&self, user_id: &str, import_id: &str) {
        remove(&self.archive(user_id, import_id)).await;
    }

    /// Delete `user_id`'s files no `importing` row names: the ones a
    /// newer upload or a forget left unread.
    pub(crate) async fn remove_unread(&self, user_id: &str) {
        let prefix = format!("{}-", person(user_id));
        self.remove_unnamed(|name, _| name.starts_with(&prefix))
            .await;
    }

    /// Delete every file no `importing` row names that is an hour old or
    /// more, whoever's it was.
    pub(crate) async fn sweep(&self) {
        let now = self.now();
        self.remove_unnamed(|_, modified_ms| {
            modified_ms.is_some_and(|at| now.saturating_sub(at) >= LEFTOVER_AFTER_MS)
        })
        .await;
    }

    /// The file names an `importing` row names.
    async fn named(&self) -> Result<HashSet<String>, DbError> {
        let rows = self
            .db
            .read("history-files", move |conn| {
                conn.prepare_cached(sql::FILES_READ.text())?
                    .query_map(params![HistoryImportState::Importing], sql::FileRow::read)?
                    .collect::<rusqlite::Result<Vec<_>>>()
                    .map_err(DbError::from)
            })
            .await?;
        Ok(rows
            .into_iter()
            .map(|row| file_name(&row.user_id, &row.import_id))
            .collect())
    }

    /// Delete each file in the folder that no `importing` row names and
    /// `take` accepts, given its name and when it last changed (unix ms).
    async fn remove_unnamed(&self, take: impl Fn(&str, Option<i64>) -> bool) {
        let mut entries = match tokio::fs::read_dir(&self.dir).await {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return,
            Err(error) => {
                tracing::warn!(%error, "listening history uploads not listed");
                return;
            }
        };
        let named = match self.named().await {
            Ok(named) => named,
            Err(error) => {
                tracing::warn!(%error, "listening history uploads not swept; trying later");
                return;
            }
        };
        loop {
            match entries.next_entry().await {
                Ok(Some(entry)) => {
                    let name = entry.file_name();
                    let Some(name) = name.to_str() else { continue };
                    if named.contains(name) {
                        continue;
                    }
                    let modified_ms = entry
                        .metadata()
                        .await
                        .ok()
                        .and_then(|meta| meta.modified().ok())
                        .and_then(|at| at.duration_since(UNIX_EPOCH).ok())
                        .and_then(|since| i64::try_from(since.as_millis()).ok());
                    if take(name, modified_ms) {
                        remove(&entry.path()).await;
                    }
                }
                Ok(None) => return,
                Err(error) => {
                    tracing::warn!(%error, "listening history uploads not listed");
                    return;
                }
            }
        }
    }

    fn now(&self) -> i64 {
        self.clock.now_ms()
    }
}

/// Delete one file; one already gone is fine.
pub(crate) async fn remove(path: &Path) {
    if let Err(error) = tokio::fs::remove_file(path).await
        && error.kind() != std::io::ErrorKind::NotFound
    {
        tracing::warn!(%error, "a listening history upload was not deleted");
    }
}

/// Refuse the write unless `user_id`'s import still reads `import_id` at
/// `entries_read`.
fn still_reading(
    tx: &Transaction,
    user_id: &str,
    import_id: &str,
    entries_read: i64,
) -> Result<(), WriteError> {
    let reading: bool = tx.query_row(
        sql::STILL_READING.text(),
        params![
            user_id,
            HistoryImportState::Importing,
            import_id,
            entries_read
        ],
        |row| row.get(0),
    )?;
    if reading {
        Ok(())
    } else {
        Err(WriteError::MovedOn)
    }
}

/// When a listen was, unix ms: `None` for a time before 1970 or more than
/// a day after `now_ms`.
fn listened_ms(listen: &HistoryListen, now_ms: i64) -> Option<Millis> {
    let at = listen.listened_at.checked_mul(1000)?;
    (at > 0 && at <= now_ms.saturating_add(86_400_000)).then_some(Millis(at))
}

/// The name of the file `import_id` of `user_id` reads.
fn file_name(user_id: &str, import_id: &str) -> String {
    format!("{}-{import_id}.zip", person(user_id))
}

/// The part of an upload's file name that says whose it is: a hash of the
/// person's id.
fn person(user_id: &str) -> String {
    Sha256::digest(user_id.as_bytes())
        .iter()
        .take(16)
        .map(|byte| format!("{byte:02x}"))
        .collect()
}
