//! The read that keeps a person's kept history current: one [`KIND`] run,
//! keyed by the person, reads their listens since where the history
//! reaches ([`super::export::cursor`]) with their own token, a page of up
//! to a thousand at a time, oldest first.
//!
//! A run reads at most [`MAX_PAGES`] pages, the most ListenBrainz's docs
//! ask a client to read this way; when the last of them is still full, an
//! export of the rest is asked for instead. Each page is kept in its own
//! write, which checks the import is still current and kept from the
//! link's account; the last write notes the account, stamps the time and
//! completes the run. A run is safe to repeat: a listen is kept once. It
//! reads nothing while ListenBrainz reads are off, without the person's
//! link, once the day has no room for background reads, or when a read
//! was tried in the last hour (a second run queued behind one), and every
//! page it asks for is counted. A read that fails for good (a refused
//! token, an account gone, an answer of another shape) stamps the try on
//! the import, so the next waits as [`super::due::next_read_at`] says.

use std::sync::Arc;
use std::time::Duration;

use droppedneedle_kernel::db::DbError;
use droppedneedle_kernel::jobs::BoxFuture;
use droppedneedle_kernel::jobs::queue::{JobCtx, JobError, JobHandler, JobKind, KindSpec, Outcome};
use droppedneedle_providers::listenbrainz::MAX_LISTENS_PAGE;

use super::HistoryImportState;
use super::export::{PRIORITY, counted, cursor};
use super::store::{HistoryStore, SyncFinish, WriteError};

/// The read job kind.
pub const KIND: JobKind = JobKind::new("listens.sync");

/// Tries a run gets.
const MAX_ATTEMPTS: u32 = 3;

/// The most pages one run reads.
pub const MAX_PAGES: usize = 10;

/// A read tried this recently is not tried again, ms.
const TRIED_LATELY_MS: i64 = 3_600_000;

/// How read runs go: ten paced reads at most; done rows kept an hour.
pub const SPEC: KindSpec = KindSpec::new(KIND)
    .max_attempts(MAX_ATTEMPTS)
    .run_timeout(Duration::from_secs(5 * 60))
    .keep_done(Duration::from_secs(3600));

/// Runs [`KIND`] jobs over the one history store.
pub struct SyncJobs {
    store: Arc<HistoryStore>,
}

impl SyncJobs {
    /// The handler over `store`.
    pub fn new(store: Arc<HistoryStore>) -> Self {
        Self { store }
    }
}

impl JobHandler for SyncJobs {
    fn run(&self, job: JobCtx) -> BoxFuture<'static, Outcome> {
        let store = Arc::clone(&self.store);
        Box::pin(async move { run(&store, job).await })
    }
}

async fn run(store: &HistoryStore, job: JobCtx) -> Outcome {
    let user_id = job.key().to_owned();
    let row = match store.row(&user_id).await {
        Ok(Some(row)) if row.state == HistoryImportState::Current => row,
        Ok(_) => return Outcome::Done,
        Err(DbError::Busy { .. }) => return Outcome::Busy,
        Err(error) => {
            tracing::warn!(%error, "listening history not read; trying again");
            return retry(store, None);
        }
    };
    let tried = row.updated_at.max(row.last_synced_at.unwrap_or(0));
    if !store.reads_on() || store.now_ms().saturating_sub(tried) < TRIED_LATELY_MS {
        return Outcome::Done;
    }
    let Some(link) = store.link_of(&user_id, &row).await else {
        return Outcome::Done;
    };
    let Some(mut after) = cursor(&row).filter(|at| *at > 1) else {
        return Outcome::Done;
    };
    for page_number in 1..=MAX_PAGES {
        match store.budget().room().await {
            Ok(0) => return Outcome::Done,
            Ok(_) => {}
            Err(DbError::Busy { .. }) => return Outcome::Busy,
            Err(error) => {
                tracing::warn!(%error, "the day's reads not counted; the read waits");
                return Outcome::Done;
            }
        }
        // From the second before, so listens in the same second as the
        // newest kept are read too; a listen is kept once.
        let read = store
            .listenbrainz()
            .listens_after(
                &link.username,
                after - 1,
                MAX_LISTENS_PAGE,
                &link.creds,
                PRIORITY,
            )
            .await;
        let page = match counted(store, read).await {
            Ok(page) => page,
            Err(error) if error.is_retriable() && job.attempt() < MAX_ATTEMPTS => {
                tracing::info!(%error, "ListenBrainz did not answer the read; trying again");
                return retry(store, error.retry_after());
            }
            Err(error) => {
                tracing::warn!(%error, "listening history not read from ListenBrainz");
                return match store.read_failed(&user_id, &link.username).await {
                    Ok(()) => Outcome::Done,
                    Err(DbError::Busy { .. }) => Outcome::Busy,
                    Err(error) => {
                        tracing::warn!(%error, "a failed listening history read not noted");
                        Outcome::Done
                    }
                };
            }
        };
        if page.skipped > 0 {
            tracing::info!(
                page_number,
                skipped = page.skipped,
                "listens ListenBrainz sent that were not kept: not a listen, or no name"
            );
        }
        let full = page.read >= MAX_LISTENS_PAGE as usize;
        let finish = match (full, page_number == MAX_PAGES) {
            (false, _) => SyncFinish::UpToDate,
            (true, true) => SyncFinish::Export,
            (true, false) => SyncFinish::More,
        };
        let complete = job.clone();
        match store
            .sync_page(&user_id, &link.username, page.listens, finish, move |tx| {
                complete.complete(tx)
            })
            .await
        {
            Ok(added) => tracing::debug!(page_number, added, "a page of listening history kept"),
            // A forget, a new link or an export moved the import on.
            Err(WriteError::MovedOn | WriteError::Job(JobError::LeaseLost)) => {
                return Outcome::Done;
            }
            Err(error) if error.is_busy() => return Outcome::Busy,
            Err(error) => {
                tracing::warn!(%error, "listening history not kept; trying again");
                return retry(store, None);
            }
        }
        match finish {
            SyncFinish::More => {
                let newest = page.newest_at.unwrap_or(after);
                after = if newest > after { newest } else { after + 1 };
            }
            SyncFinish::UpToDate => return Outcome::Done,
            SyncFinish::Export => {
                tracing::info!(
                    pages = MAX_PAGES,
                    "too many listens to read page by page; an export is asked for"
                );
                return Outcome::Done;
            }
        }
    }
    Outcome::Done
}

fn retry(store: &HistoryStore, wait: Option<Duration>) -> Outcome {
    Outcome::Retry {
        code: "HISTORY_READ_FAILED",
        at: wait
            .and_then(|wait| i64::try_from(wait.as_millis()).ok())
            .map(|wait| store.job_now().saturating_add(wait)),
    }
}
