//! The listening history store: the imported listens, each person's
//! import row, and the files waiting to be read in the cache folder.
//!
//! One store is built at startup and shared by the upload, status and
//! import routes, the listening settings' connect, the import and read
//! jobs, Discover's visits and tick, and Discover's forget. It holds what
//! those steps need beside the database: the person's link, the
//! ListenBrainz client and its reads switch, the read budget, and the
//! jobs table's clock for due times. A file waiting to be read
//! is `<cache>/listening-history/<person>-<import id>.zip`, where the
//! person's part is a hash of their id, so a name never carries text from
//! a request. A file no `importing` row names is left over (an account
//! deleted, a crash after the last month was read, an upload that never
//! started its import) and is swept at the start of every import run once
//! it is an hour old, so an upload still arriving is never taken.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::UNIX_EPOCH;

use rusqlite::{OptionalExtension, Transaction, params};
use sha2::{Digest as _, Sha256};

use droppedneedle_kernel::clock::Clock;
use droppedneedle_kernel::db::time::Millis;
use droppedneedle_kernel::db::{CancelFlag, ChunkedError, Db, DbError, Lane};
use droppedneedle_kernel::ids::IdGenerator;
use droppedneedle_kernel::jobs::queue::{JobError, JobKind, JobQueue, JobStatus};
use droppedneedle_kernel::platform::Platform;
use droppedneedle_kernel::runtime_config::Live;
use droppedneedle_kernel::runtime_config::sections::{ListenBrainzSettings, listenbrainz_reads_on};
use droppedneedle_providers::listenbrainz::history::HistoryListen;
use droppedneedle_providers::listenbrainz::{ListenBrainzClient, ListenBrainzCredentials};

use super::import::{IMPORT_JOB, KIND};
use super::sync;
use super::{HistoryImportSource, HistoryImportState, ReadBudget, sql};
use crate::playback::listening::ListeningStore;

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

/// One person's import as its steps read it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ImportRow {
    /// Where it stands.
    pub state: HistoryImportState,
    /// The ListenBrainz account it is kept from.
    pub account: Option<String>,
    /// The export ListenBrainz is making for it.
    pub export_id: Option<String>,
    /// The file being read.
    pub import_id: Option<String>,
    /// How many of that file's month files are read.
    pub entries_read: usize,
    /// When the history was last brought up to date, unix ms.
    pub last_synced_at: Option<i64>,
    /// The newest listen kept, unix ms.
    pub newest_listened_at: Option<i64>,
    /// When the row last changed, unix ms: for an export being made, since
    /// when; for a current history, the last read tried.
    pub updated_at: i64,
}

/// A person with a ListenBrainz link, and where their import stands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Linked {
    /// The person.
    pub user_id: String,
    /// Where their import stands; `None` without one.
    pub state: Option<HistoryImportState>,
    /// When their history was last brought up to date, unix ms.
    pub last_synced_at: Option<i64>,
    /// When their import row last changed, unix ms.
    pub updated_at: Option<i64>,
}

impl ImportRow {
    /// Whether anything is kept, or was read before.
    pub fn holds_history(&self) -> bool {
        self.newest_listened_at.is_some() || self.last_synced_at.is_some()
    }
}

/// What the store reaches beside the database, built once by bootstrap.
pub struct HistoryParts {
    /// The jobs table imports and reads are queued on.
    pub jobs: JobQueue,
    /// The clock the jobs table runs on, for due times.
    pub job_clock: Arc<dyn Clock>,
    /// The people's ListenBrainz links.
    pub listening: ListeningStore,
    /// The one ListenBrainz client.
    pub listenbrainz: ListenBrainzClient,
    /// The day's room for background reads.
    pub budget: Arc<dyn ReadBudget>,
}

/// What a person's link lends their history's reads.
pub(crate) struct Link {
    /// Their ListenBrainz username.
    pub username: String,
    /// Their username and token.
    pub creds: ListenBrainzCredentials,
}

/// When this process last queued work for people, so a tick or a visit
/// repeated soon after asks the database nothing.
#[derive(Default)]
pub(crate) struct Marks {
    /// When each person's last visit was handled, unix ms.
    pub visits: HashMap<String, i64>,
    /// When the tick last queued each person's read, unix ms.
    pub syncs: HashMap<String, i64>,
    /// When the tick last queued each person's first import step, unix ms.
    pub firsts: HashMap<String, i64>,
}

/// The imported listens and import rows, over the one database.
pub struct HistoryStore {
    db: Db,
    clock: Arc<dyn Clock>,
    ids: Arc<dyn IdGenerator>,
    jobs: JobQueue,
    job_clock: Arc<dyn Clock>,
    dir: PathBuf,
    listening: ListeningStore,
    listenbrainz: ListenBrainzClient,
    reads: Live<ListenBrainzSettings>,
    budget: Arc<dyn ReadBudget>,
    marks: Mutex<Marks>,
}

impl HistoryStore {
    /// The store over `platform`'s database, clock, ids and settings, with
    /// uploads and downloads waiting under its cache folder, and reaching
    /// what `parts` names.
    pub fn new(platform: &Platform, parts: HistoryParts) -> Self {
        Self {
            db: platform.db.clone(),
            clock: platform.clock.clone(),
            ids: platform.ids.clone(),
            jobs: parts.jobs,
            job_clock: parts.job_clock,
            dir: platform.config.cache_dir.join(FOLDER),
            listening: parts.listening,
            listenbrainz: parts.listenbrainz,
            reads: platform.settings.live(),
            budget: parts.budget,
            marks: Mutex::new(Marks::default()),
        }
    }

    /// The ListenBrainz client.
    pub(crate) fn listenbrainz(&self) -> &ListenBrainzClient {
        &self.listenbrainz
    }

    /// The day's room for background reads.
    pub(crate) fn budget(&self) -> &dyn ReadBudget {
        &*self.budget
    }

    /// Whether ListenBrainz may be read now. A section that does not read
    /// is off.
    pub(crate) fn reads_on(&self) -> bool {
        listenbrainz_reads_on(&self.reads)
    }

    /// The jobs table's now, unix ms, for due times.
    pub(crate) fn job_now(&self) -> i64 {
        self.job_clock.now_ms()
    }

    /// Now, unix ms, for the rows.
    pub(crate) fn now_ms(&self) -> i64 {
        self.clock.now_ms()
    }

    /// The database, for the activity read.
    pub(crate) fn db(&self) -> &Db {
        &self.db
    }

    /// What this process queued.
    pub(crate) fn marks(&self) -> MutexGuard<'_, Marks> {
        self.marks.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// `user_id`'s ListenBrainz username, when they have a usable link.
    pub(crate) async fn linked_name(&self, user_id: &str) -> Option<String> {
        self.listening
            .listenbrainz_link(user_id)
            .await
            .map(|link| link.username)
            .filter(|name| !name.is_empty())
    }

    /// What `user_id`'s link lends a read: `None` without a link that
    /// carries a username and a token.
    pub(crate) async fn link(&self, user_id: &str) -> Option<Link> {
        let creds = self.listening.listenbrainz_credentials(user_id).await;
        let username = creds.username.clone().filter(|name| !name.is_empty())?;
        creds.user_token.as_ref()?;
        Some(Link { username, creds })
    }

    /// What `user_id`'s link lends their import `row`: `None` without a
    /// usable link, or with a link of another account than the import's.
    pub(crate) async fn link_of(&self, user_id: &str, row: &ImportRow) -> Option<Link> {
        let link = self.link(user_id).await?;
        let same = row
            .account
            .as_deref()
            .is_none_or(|account| account.eq_ignore_ascii_case(&link.username));
        same.then_some(link)
    }

    /// One person's import as its steps read it; `None` when they have
    /// none.
    pub(crate) async fn row(&self, user_id: &str) -> Result<Option<ImportRow>, DbError> {
        let user = user_id.to_owned();
        let row = self
            .db
            .read("history-row", move |conn| {
                Ok::<_, DbError>(
                    conn.query_row(sql::IMPORT_ROW.text(), [user], sql::ImportRow::read)
                        .optional()?,
                )
            })
            .await?;
        Ok(row.map(|row| ImportRow {
            state: row.state,
            account: row.account,
            export_id: row.export_id,
            import_id: row.import_id,
            entries_read: usize::try_from(row.entries_read).unwrap_or(usize::MAX),
            last_synced_at: row.last_synced_at.map(|at| at.0),
            newest_listened_at: row.newest_listened_at.map(|at| at.0),
            updated_at: row.updated_at.0,
        }))
    }

    /// Start a first import from ListenBrainz for `user_id`, kept from
    /// `account`, with its first step due at `due_at` (the jobs table's
    /// clock), when they have no import; their earlier import jobs are
    /// cancelled in the same write. False when they had one.
    pub(crate) async fn start_first(
        &self,
        user_id: &str,
        account: &str,
        due_at: i64,
    ) -> Result<bool, JobError> {
        let (user, account, now) = (user_id.to_owned(), account.to_owned(), self.now());
        let jobs = self.jobs.clone();
        let started = self
            .db
            .write(Lane::Foreground, "history-first", move |tx| {
                let started = first_export(tx, &user, &account, now)?;
                if started {
                    jobs.cancel(tx, KIND, &user)?;
                    jobs.enqueue_at(tx, KIND, &user, &IMPORT_JOB, due_at)?;
                }
                Ok::<_, JobError>(started)
            })
            .await?;
        self.jobs.wake();
        Ok(started)
    }

    /// Queue the first import step of the people in `first`, each due at
    /// its time (the jobs table's clock), joining a step already queued,
    /// and queue the reads of the people in `syncs`, in one write. Nothing
    /// is written to their import rows: the step starts the import when it
    /// runs. Answers who got a new step.
    pub(crate) async fn queue_due(
        &self,
        first: Vec<(String, i64)>,
        syncs: Vec<String>,
    ) -> Result<Vec<String>, JobError> {
        let jobs = self.jobs.clone();
        let started = self
            .db
            .write(Lane::Background, "history-due", move |tx| {
                let mut started = Vec::new();
                for (user, due_at) in first {
                    if !jobs
                        .enqueue_at(tx, KIND, &user, &IMPORT_JOB, due_at)?
                        .joined
                    {
                        started.push(user);
                    }
                }
                for user in syncs {
                    jobs.enqueue(tx, sync::KIND, &user, &IMPORT_JOB)?;
                }
                Ok::<_, JobError>(started)
            })
            .await?;
        self.jobs.wake();
        Ok(started)
    }

    /// Ask for an export again for `user_id`, kept from `account`, when
    /// their import is in `from`: what is kept stays, the person's import
    /// jobs are cancelled and one is queued now. False when the import was
    /// not in `from`.
    pub(crate) async fn export_again(
        &self,
        user_id: &str,
        account: &str,
        from: HistoryImportState,
    ) -> Result<bool, JobError> {
        let (user, account, now) = (user_id.to_owned(), account.to_owned(), self.now());
        let jobs = self.jobs.clone();
        let started = self
            .db
            .write(Lane::Foreground, "history-again", move |tx| {
                let changed = tx.execute(
                    sql::EXPORT_AGAIN.text(),
                    params![
                        user,
                        HistoryImportState::Pending,
                        HistoryImportSource::Export,
                        account,
                        Millis(now),
                        from
                    ],
                )?;
                if changed > 0 {
                    jobs.cancel(tx, KIND, &user)?;
                    jobs.enqueue(tx, KIND, &user, &IMPORT_JOB)?;
                }
                Ok::<_, JobError>(changed > 0)
            })
            .await?;
        self.jobs.wake();
        Ok(started)
    }

    /// Replace `user_id`'s history with one from `account`'s: the listens
    /// kept go, their import and read jobs are cancelled and an export is
    /// asked for, in one write; then their files waiting to be read are
    /// deleted.
    pub(crate) async fn replace_account(
        &self,
        user_id: &str,
        account: &str,
    ) -> Result<(), JobError> {
        let (user, account, now) = (user_id.to_owned(), account.to_owned(), self.now());
        let jobs = self.jobs.clone();
        self.db
            .write(Lane::Foreground, "history-replace", move |tx| {
                tx.execute(sql::FORGET_LISTENS.text(), [&user])?;
                if !first_export(tx, &user, &account, now)? {
                    tx.execute(
                        sql::REPLACE_ACCOUNT.text(),
                        params![
                            user,
                            HistoryImportState::Pending,
                            HistoryImportSource::Export,
                            account,
                            Millis(now)
                        ],
                    )?;
                }
                jobs.cancel(tx, KIND, &user)?;
                jobs.cancel(tx, sync::KIND, &user)?;
                jobs.enqueue(tx, KIND, &user, &IMPORT_JOB)?;
                Ok::<_, JobError>(())
            })
            .await?;
        self.jobs.wake();
        self.remove_unread(user_id).await;
        Ok(())
    }

    /// Queue `user_id`'s import step now, joining one already queued.
    pub(crate) async fn queue_import(&self, user_id: &str) -> Result<(), JobError> {
        self.queue_now(user_id, KIND).await
    }

    /// Queue `user_id`'s read now, joining one already queued.
    pub(crate) async fn queue_sync(&self, user_id: &str) -> Result<(), JobError> {
        self.queue_now(user_id, sync::KIND).await
    }

    async fn queue_now(&self, user_id: &str, kind: JobKind) -> Result<(), JobError> {
        let user = user_id.to_owned();
        let jobs = self.jobs.clone();
        self.db
            .write(Lane::Background, "history-queue", move |tx| {
                jobs.enqueue(tx, kind, &user, &IMPORT_JOB)?;
                Ok::<_, JobError>(())
            })
            .await?;
        self.jobs.wake();
        Ok(())
    }

    /// Start a first import from ListenBrainz for `user_id`, kept from
    /// `account`, when they have none, without touching their jobs: the
    /// step that runs it is already running. False when they had one.
    pub(crate) async fn begin_first(&self, user_id: &str, account: &str) -> Result<bool, DbError> {
        let (user, account, now) = (user_id.to_owned(), account.to_owned(), self.now());
        self.db
            .write(Lane::Background, "history-begin", move |tx| {
                Ok::<_, DbError>(first_export(tx, &user, &account, now)?)
            })
            .await
    }

    /// Whether `user_id`'s import is still at the step `at` read: no
    /// forget, new link or other run moved it on.
    pub(crate) async fn still_at(&self, user_id: &str, at: &ImportRow) -> Result<bool, DbError> {
        let user = user_id.to_owned();
        let (state, account, export) = (at.state, at.account.clone(), at.export_id.clone());
        self.db
            .read("history-still", move |conn| {
                Ok::<_, DbError>(conn.query_row(
                    sql::STILL_AT.text(),
                    params![user, state, account, export],
                    |row| row.get(0),
                )?)
            })
            .await
    }

    /// Note that a read of `user_id`'s new listens from `account` failed:
    /// the try is stamped on their import, so the next one waits as
    /// [`super::due`] says, and the history is not marked up to date.
    pub(crate) async fn read_failed(&self, user_id: &str, account: &str) -> Result<(), DbError> {
        let (user, account, now) = (user_id.to_owned(), account.to_owned(), self.now());
        self.db
            .write(Lane::Background, "history-read-failed", move |tx| {
                tx.execute(
                    sql::READ_TRIED.text(),
                    params![user, HistoryImportState::Current, account, Millis(now)],
                )?;
                Ok::<_, DbError>(())
            })
            .await
    }

    /// The newest row of `user_id`'s import jobs.
    pub(crate) async fn import_job(&self, user_id: &str) -> Result<Option<JobStatus>, DbError> {
        self.jobs.latest(KIND, user_id).await
    }

    /// Move `user_id`'s import on from the step `at` read: to `state`,
    /// waiting on `export_id`, with the next step due at `due_at` (the jobs
    /// table's clock); `same` leaves the row as it is and only queues that
    /// step. The run completes through `complete` in the same write.
    pub(crate) async fn export_step(
        &self,
        user_id: &str,
        at: &ImportRow,
        to: ExportStep,
        due_at: i64,
        complete: impl FnOnce(&Transaction) -> Result<(), JobError> + Send + 'static,
    ) -> Result<(), WriteError> {
        let (user, now) = (user_id.to_owned(), self.now());
        let (state, account, export) = (at.state, at.account.clone(), at.export_id.clone());
        let jobs = self.jobs.clone();
        self.db
            .write(Lane::Background, "history-export", move |tx| {
                let still = |tx: &Transaction| -> Result<bool, WriteError> {
                    Ok(tx.query_row(
                        sql::STILL_AT.text(),
                        params![user, state, account, export],
                        |row| row.get(0),
                    )?)
                };
                let moved = match &to {
                    ExportStep::Same => still(tx)?,
                    ExportStep::Waiting(export_id) => {
                        tx.execute(
                            sql::EXPORT_MOVED.text(),
                            params![
                                user,
                                state,
                                account,
                                export,
                                HistoryImportState::Exporting,
                                export_id,
                                Millis(now)
                            ],
                        )? > 0
                    }
                    ExportStep::Downloaded(import_id) => {
                        tx.execute(
                            sql::EXPORT_DOWNLOADED.text(),
                            params![
                                user,
                                state,
                                account,
                                export,
                                HistoryImportState::Importing,
                                import_id,
                                Millis(now)
                            ],
                        )? > 0
                    }
                    ExportStep::Failed => {
                        tx.execute(
                            sql::EXPORT_FAILED.text(),
                            params![
                                user,
                                state,
                                account,
                                export,
                                HistoryImportState::Failed,
                                Millis(now)
                            ],
                        )? > 0
                    }
                };
                if !moved {
                    return Err(WriteError::MovedOn);
                }
                if !matches!(to, ExportStep::Failed) {
                    jobs.enqueue_at(tx, KIND, &user, &IMPORT_JOB, due_at)?;
                }
                complete(tx)?;
                Ok(())
            })
            .await?;
        self.jobs.wake();
        Ok(())
    }

    /// Keep one page of listens read from `account`'s ListenBrainz for
    /// `user_id`, while their import is current and kept from that account
    /// (or from an upload no link has read since). `finish` says what the
    /// page ends: more pages to read, the history up to date (the account
    /// is noted and the time stamped), or too much to read this way (an
    /// export is asked for from where the pages reached). The run
    /// completes through `complete` in the write that ends it. Answers the
    /// listens added.
    pub(crate) async fn sync_page(
        &self,
        user_id: &str,
        account: &str,
        listens: Vec<HistoryListen>,
        finish: SyncFinish,
        complete: impl FnOnce(&Transaction) -> Result<(), JobError> + Send + 'static,
    ) -> Result<usize, WriteError> {
        let now = self.now();
        let (user, account) = (user_id.to_owned(), account.to_owned());
        let jobs = self.jobs.clone();
        let added = self
            .db
            .write(Lane::Background, "history-sync", move |tx| {
                let current: bool = tx.query_row(
                    sql::STILL_CURRENT.text(),
                    params![user, HistoryImportState::Current, account],
                    |row| row.get(0),
                )?;
                if !current {
                    return Err(WriteError::MovedOn);
                }
                let mut added = 0;
                let mut insert = tx.prepare_cached(sql::INSERT_LISTEN.text())?;
                for listen in &listens {
                    let Some(at) = listened_ms(listen, now) else {
                        continue;
                    };
                    added += insert.execute(params![
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
                }
                drop(insert);
                match finish {
                    SyncFinish::More => return Ok(added),
                    SyncFinish::UpToDate => {
                        tx.execute(
                            sql::SYNCED.text(),
                            params![user, HistoryImportState::Current, account, Millis(now)],
                        )?;
                    }
                    SyncFinish::Export => {
                        tx.execute(
                            sql::EXPORT_AGAIN.text(),
                            params![
                                user,
                                HistoryImportState::Pending,
                                HistoryImportSource::Export,
                                account,
                                Millis(now),
                                HistoryImportState::Current
                            ],
                        )?;
                        jobs.enqueue(tx, KIND, &user, &IMPORT_JOB)?;
                    }
                }
                complete(tx)?;
                Ok::<_, WriteError>(added)
            })
            .await?;
        self.jobs.wake();
        Ok(added)
    }

    /// The people with a ListenBrainz link, with where their import
    /// stands.
    pub(crate) async fn linked(&self) -> Result<Vec<Linked>, DbError> {
        self.db
            .read("history-linked", |conn| {
                conn.prepare_cached(sql::LINKED.text())?
                    .query_map([], sql::LinkedRow::read)?
                    .map(|row| {
                        row.map(|row| Linked {
                            user_id: row.user_id,
                            state: row.state,
                            last_synced_at: row.last_synced_at.map(|at| at.0),
                            updated_at: row.updated_at.map(|at| at.0),
                        })
                    })
                    .collect::<rusqlite::Result<Vec<_>>>()
                    .map_err(DbError::from)
            })
            .await
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
                jobs.cancel(tx, sync::KIND, &user)?;
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

/// Where an export step moves a person's import.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ExportStep {
    /// Nowhere: the next step only waits.
    Same,
    /// ListenBrainz is making this export.
    Waiting(String),
    /// The export is downloaded to the file of this import id.
    Downloaded(String),
    /// The export cannot be had: the import failed.
    Failed,
}

/// What a page of listens read for a person ends.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SyncFinish {
    /// More pages follow.
    More,
    /// The history is up to date.
    UpToDate,
    /// Too much is left to read page by page: an export is asked for.
    Export,
}

/// Start a first import from ListenBrainz for `user_id`, kept from
/// `account`, when they have none. False when they had one.
fn first_export(
    tx: &Transaction,
    user_id: &str,
    account: &str,
    now: i64,
) -> rusqlite::Result<bool> {
    Ok(tx.execute(
        sql::FIRST_EXPORT.text(),
        params![
            user_id,
            HistoryImportState::Pending,
            HistoryImportSource::Export,
            account,
            Millis(now)
        ],
    )? > 0)
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
