//! The export steps of a person's import from ListenBrainz, one a run, as
//! ListenBrainz's export docs ask.
//!
//! - Asked for (`pending`): list the person's exports first, and take one
//!   that is waiting or being made and covers what is wanted, or one made
//!   in the last twelve hours; else ask for one. ListenBrainz keeps one
//!   pending export an account: while another client's export of a
//!   different range waits, the step waits too, and an "already asked
//!   for" answer to the request is taken up from a second list in the
//!   same step. Any other refusal of the request fails the import.
//! - Being made (`exporting`): ask where it stands a minute after it was
//!   asked for, then waiting twice as long each time, up to 15 minutes,
//!   for at most a day; once made, download it, streamed, to the cache
//!   folder under [`MAX_DOWNLOAD_BYTES`], and hand it to the
//!   month-by-month read. A ten-minute export takes five polls; one that
//!   takes the whole day about a hundred.
//!
//! What is wanted is the whole history for a person with nothing kept,
//! else the listens from where the kept history reaches ([`cursor`]). An
//! export ListenBrainz could not make, or one that is gone, marks the
//! import failed, and nothing asks again by itself: the person can, from
//! their Profile. Every call is made at background priority with the
//! person's own token and counted against the day's ceiling; while
//! ListenBrainz reads are off, or the day has no room, the step waits an
//! hour and sends nothing.

use std::time::Duration;

use droppedneedle_kernel::db::DbError;
use droppedneedle_kernel::jobs::queue::{JobCtx, JobError, Outcome};
use droppedneedle_providers::executor::DownloadError;
use droppedneedle_providers::listenbrainz::{AskedExport, ExportStatus, ListenBrainzExport};
use droppedneedle_providers::{ProviderError, RequestPriority};

use super::archive::Export;
use super::import::MAX_ATTEMPTS;
use super::store::{ExportStep, HistoryStore, ImportRow, Link, WriteError, remove};
use crate::playback::sqlite::off_worker;

/// Every export call is background work.
pub(crate) const PRIORITY: RequestPriority = RequestPriority::BackgroundSync;
/// The largest export downloaded, in bytes.
pub const MAX_DOWNLOAD_BYTES: u64 = 512 * (1 << 20);
/// The longest a download may take.
const DOWNLOAD_DEADLINE: Duration = Duration::from_secs(10 * 60);
/// How long a step waits before first asking about the export, before the
/// jitter. ListenBrainz asks for 30 seconds or more, growing.
const POLL_WAIT_MS: i64 = 60_000;
/// The longest wait between asks.
const POLL_WAIT_MAX_MS: i64 = 15 * 60_000;
/// How long a step waits while another client's export holds the
/// person's one pending export.
const HELD_WAIT_MS: i64 = 10 * 60_000;
/// How long a step waits while ListenBrainz reads are off or the day has
/// no room for them.
pub(crate) const QUIET_WAIT_MS: i64 = 3_600_000;
/// How long an export may be waited on before it is given up on, ms.
const GIVE_UP_MS: i64 = 86_400_000;
/// The kind of export that holds the listens.
const EXPORT_KIND: &str = "export_all_user_data";
/// How recently a made export must have been asked for to be taken
/// instead of asking for another, in seconds: the reads afterwards cover
/// the day before the history was last brought up to date.
const FRESH_SECS: i64 = 12 * 3_600;
/// How far before the time the history was last brought up to date the
/// next read starts, in seconds: listens sent in late, and an export's
/// last hours, are read again rather than missed.
pub(crate) const OVERLAP_SECS: i64 = 86_400;

/// Where the person's kept history reaches, unix seconds: the next read
/// asks for the listens after it. For a history no link has read (an
/// upload), the newest listen kept; else also no earlier than a day before
/// the history was last brought up to date, since a person whose listens
/// here come back from ListenBrainz keeps none of those. `None` with
/// nothing kept or read.
pub(crate) fn cursor(row: &ImportRow) -> Option<i64> {
    let newest = row.newest_listened_at.map(|ms| ms.div_euclid(1000));
    let read = row
        .last_synced_at
        .map(|ms| ms.div_euclid(1000) - OVERLAP_SECS);
    match row.account {
        Some(_) => newest.max(read),
        None => newest.or(read),
    }
}

/// Whether `export` holds what is wanted: listens from `from` on, or the
/// whole history.
fn covers(export: &ListenBrainzExport, from: Option<i64>) -> bool {
    export
        .kind
        .as_deref()
        .is_none_or(|kind| kind == EXPORT_KIND)
        && export.end_time.is_none()
        && match from {
            None => export.start_time.is_none(),
            Some(from) => export.start_time.is_none_or(|start| start <= from),
        }
}

/// Whether ListenBrainz is still making `export`.
fn being_made(export: &ListenBrainzExport) -> bool {
    matches!(
        export.status,
        ExportStatus::Waiting | ExportStatus::InProgress | ExportStatus::Unknown
    )
}

/// The wait before the next ask about an export waited on for
/// `waited_ms`: as long again as it has been waited on, from a minute to
/// 15 minutes, with up to a quarter more at random.
fn poll_wait(waited_ms: i64) -> i64 {
    let wait = waited_ms.clamp(POLL_WAIT_MS, POLL_WAIT_MAX_MS);
    wait + jitter(wait / 4)
}

/// A random wait up to `most` ms.
pub(crate) fn jitter(most: i64) -> i64 {
    let mut bytes = [0u8; 8];
    if getrandom::fill(&mut bytes).is_err() || most <= 0 {
        return most / 2;
    }
    i64::try_from(u64::from_le_bytes(bytes) % most.unsigned_abs()).unwrap_or(0)
}

/// Whether a failed call never left the server, so nothing is counted.
pub(crate) fn unsent(error: &ProviderError) -> bool {
    matches!(
        error,
        ProviderError::Invalid { .. } | ProviderError::NotConfigured { .. }
    )
}

/// Count one call that went out.
pub(crate) async fn counted<T>(
    store: &HistoryStore,
    result: Result<T, ProviderError>,
) -> Result<T, ProviderError> {
    if !result.as_ref().is_err_and(unsent) {
        store.budget().count(1).await;
    }
    result
}

/// What a step may do now.
enum Ready {
    /// Go ahead with this link; the day has this much room.
    Go(Link, u64),
    /// Make this move instead.
    Stop(ExportStep, i64),
    /// Answer this.
    Answer(Outcome),
}

/// Whether a step may call ListenBrainz now: the person still has a link
/// of the import's account, ListenBrainz reads are on and the day has room.
async fn ready(store: &HistoryStore, user_id: &str, row: &ImportRow) -> Ready {
    let quiet = store.job_now() + QUIET_WAIT_MS;
    let Some(link) = store.link_of(user_id, row).await else {
        tracing::info!("a listening history export has no link to read it with; the import failed");
        return Ready::Stop(ExportStep::Failed, 0);
    };
    if !store.reads_on() {
        return Ready::Stop(ExportStep::Same, quiet);
    }
    match store.budget().room().await {
        Ok(0) => Ready::Stop(ExportStep::Same, quiet),
        Ok(room) => Ready::Go(link, room),
        Err(DbError::Busy { .. }) => Ready::Answer(Outcome::Busy),
        Err(error) => {
            tracing::warn!(%error, "the day's reads not counted; the export waits");
            Ready::Stop(ExportStep::Same, quiet)
        }
    }
}

/// The step for an export to be asked for.
pub(crate) async fn request(store: &HistoryStore, row: &ImportRow, job: JobCtx) -> Outcome {
    let (link, room) = match ready(store, job.key(), row).await {
        Ready::Go(link, room) => (link, room),
        Ready::Stop(to, due) => return step(store, row, to, due, job).await,
        Ready::Answer(outcome) => return outcome,
    };
    let client = store.listenbrainz();
    let wanted = if row.holds_history() {
        cursor(row)
    } else {
        None
    };
    let exports = match counted(store, client.exports(&link.creds, PRIORITY).await).await {
        Ok(exports) => exports,
        Err(error) => return unanswered(store, row, error, job).await,
    };
    let now = store.now_ms().div_euclid(1000);
    let taken = exports.iter().find(|export| {
        covers(export, wanted)
            && (being_made(export)
                || (export.status == ExportStatus::Completed
                    && export.created_at.is_some_and(|at| now - at <= FRESH_SECS)
                    && export.available_until.is_some_and(|until| until > now)))
    });
    if let Some(export) = taken {
        let due = if being_made(export) {
            store.job_now() + poll_wait(0)
        } else {
            store.job_now()
        };
        tracing::info!("a listening history export already asked for is taken up");
        return step(
            store,
            row,
            ExportStep::Waiting(export.id.to_string()),
            due,
            job,
        )
        .await;
    }
    if exports.iter().any(being_made) {
        tracing::info!("another export of this account is being made; the import waits");
        let due = store.job_now() + HELD_WAIT_MS;
        return step(store, row, ExportStep::Same, due, job).await;
    }
    if room < 3 {
        let due = store.job_now() + QUIET_WAIT_MS;
        return step(store, row, ExportStep::Same, due, job).await;
    }
    // Asking changes the account at ListenBrainz: not after a forget, a
    // new link or a stop that came while the list was read.
    if job.token().is_cancelled() {
        return Outcome::Done;
    }
    match store.still_at(job.key(), row).await {
        Ok(true) => {}
        Ok(false) => return Outcome::Done,
        Err(DbError::Busy { .. }) => return Outcome::Busy,
        Err(error) => {
            tracing::warn!(%error, "a listening history import not read; trying again");
            return retry();
        }
    }
    match counted(
        store,
        client.create_export(wanted, &link.creds, PRIORITY).await,
    )
    .await
    {
        Ok(AskedExport::Asked(export)) => {
            tracing::info!(
                whole = wanted.is_none(),
                "a listening history export asked for"
            );
            let due = store.job_now() + poll_wait(0);
            step(
                store,
                row,
                ExportStep::Waiting(export.id.to_string()),
                due,
                job,
            )
            .await
        }
        Ok(AskedExport::AlreadyPending) => pending_elsewhere(store, row, &link, job).await,
        Err(error) => unanswered(store, row, error, job).await,
    }
}

/// ListenBrainz said an export of the account is already pending, which
/// the list did not show: list once more and take it up, or, when it is
/// still not there, fail rather than ask again.
async fn pending_elsewhere(
    store: &HistoryStore,
    row: &ImportRow,
    link: &Link,
    job: JobCtx,
) -> Outcome {
    let listed = store.listenbrainz().exports(&link.creds, PRIORITY).await;
    let exports = match counted(store, listed).await {
        Ok(exports) => exports,
        Err(error) => return unanswered(store, row, error, job).await,
    };
    let Some(export) = exports.iter().find(|export| being_made(export)) else {
        tracing::warn!("ListenBrainz says an export is pending but lists none; the import failed");
        return step(store, row, ExportStep::Failed, 0, job).await;
    };
    tracing::info!("a listening history export already asked for is taken up");
    let due = store.job_now() + poll_wait(0);
    step(
        store,
        row,
        ExportStep::Waiting(export.id.to_string()),
        due,
        job,
    )
    .await
}

/// A first import step the tick queued for a person with no import: the
/// import starts now, from their link, and asks for its export.
pub(crate) async fn first(store: &HistoryStore, job: JobCtx) -> Outcome {
    if !store.reads_on() {
        return Outcome::Done;
    }
    let Some(username) = store.linked_name(job.key()).await else {
        return Outcome::Done;
    };
    // Up to two days may have passed since the tick saw the person: the
    // import starts only while they are still seen.
    let since = store.now_ms().saturating_sub(super::due::SEEN_WINDOW_MS);
    match droppedneedle_auth::sqlite::seen_since(store.db(), since).await {
        Ok(seen) if seen.iter().any(|user| user == job.key()) => {}
        Ok(_) => return Outcome::Done,
        Err(DbError::Busy { .. }) => return Outcome::Busy,
        Err(error) => {
            tracing::warn!(%error, "who was seen unreadable; the import waits");
            return retry();
        }
    }
    match store.begin_first(job.key(), &username).await {
        Ok(_) => {}
        Err(DbError::Busy { .. }) => return Outcome::Busy,
        Err(error) => {
            tracing::warn!(%error, "a listening history import not started; trying again");
            return retry();
        }
    }
    match store.row(job.key()).await {
        Ok(Some(row)) if row.state == super::HistoryImportState::Pending => {
            tracing::info!("a listening history import started by the tick");
            request(store, &row, job).await
        }
        Ok(_) => Outcome::Done,
        Err(DbError::Busy { .. }) => Outcome::Busy,
        Err(error) => {
            tracing::warn!(%error, "a listening history import not read; trying again");
            retry()
        }
    }
}

/// The step for an export ListenBrainz is making.
pub(crate) async fn poll(store: &HistoryStore, row: &ImportRow, job: JobCtx) -> Outcome {
    let Some(export_id) = row
        .export_id
        .as_deref()
        .and_then(|id| id.parse::<i64>().ok())
    else {
        tracing::warn!("a listening history export has no usable id; the import failed");
        return step(store, row, ExportStep::Failed, 0, job).await;
    };
    let link = match ready(store, job.key(), row).await {
        Ready::Go(link, _) => link,
        Ready::Stop(to, due) => return step(store, row, to, due, job).await,
        Ready::Answer(outcome) => return outcome,
    };
    let client = store.listenbrainz();
    let export = match counted(store, client.export(export_id, &link.creds, PRIORITY).await).await {
        Ok(export) => export,
        Err(ProviderError::NotFound { .. }) => {
            tracing::info!("the listening history export is gone; the import failed");
            return step(store, row, ExportStep::Failed, 0, job).await;
        }
        Err(error) => return unanswered(store, row, error, job).await,
    };
    // Since the export was asked for or taken up.
    let waited = store.now_ms().saturating_sub(row.updated_at);
    match export.status {
        ExportStatus::Completed => download(store, row, export_id, &link, job).await,
        ExportStatus::Failed => {
            tracing::info!("ListenBrainz could not make the export; the import failed");
            step(store, row, ExportStep::Failed, 0, job).await
        }
        _ if waited > GIVE_UP_MS => {
            tracing::info!("the listening history export took too long; the import failed");
            step(store, row, ExportStep::Failed, 0, job).await
        }
        _ => {
            let due = store.job_now() + poll_wait(waited);
            step(store, row, ExportStep::Same, due, job).await
        }
    }
}

/// Download a made export and hand it to the month-by-month read.
async fn download(
    store: &HistoryStore,
    row: &ImportRow,
    export_id: i64,
    link: &Link,
    job: JobCtx,
) -> Outcome {
    match store.budget().room().await {
        Ok(0) => {
            let due = store.job_now() + QUIET_WAIT_MS;
            return step(store, row, ExportStep::Same, due, job).await;
        }
        Ok(_) => {}
        Err(DbError::Busy { .. }) => return Outcome::Busy,
        Err(error) => {
            tracing::warn!(%error, "the day's reads not counted; the download waits");
            let due = store.job_now() + QUIET_WAIT_MS;
            return step(store, row, ExportStep::Same, due, job).await;
        }
    }
    if let Err(error) = tokio::fs::create_dir_all(store.dir()).await {
        tracing::warn!(%error, "the listening history folder could not be made; trying again");
        return retry_or_fail(store, row, job).await;
    }
    let (import_id, path) = store.new_upload(job.key());
    let downloaded = store
        .listenbrainz()
        .download_export(
            export_id,
            &link.creds,
            PRIORITY,
            &path,
            (MAX_DOWNLOAD_BYTES, DOWNLOAD_DEADLINE),
        )
        .await;
    if !matches!(&downloaded, Err(DownloadError::Provider(error)) if unsent(error)) {
        store.budget().count(1).await;
    }
    match downloaded {
        Ok(bytes) => tracing::info!(bytes, "a listening history export downloaded"),
        Err(error) => {
            remove(&path).await;
            return match error {
                DownloadError::Provider(ProviderError::NotFound { .. }) => {
                    tracing::info!("the listening history export is gone; the import failed");
                    step(store, row, ExportStep::Failed, 0, job).await
                }
                DownloadError::Provider(error) => unanswered(store, row, error, job).await,
                DownloadError::Write => {
                    tracing::warn!("a listening history export could not be written; trying again");
                    retry_or_fail(store, row, job).await
                }
            };
        }
    }
    let readable = {
        let path = path.clone();
        off_worker(move || Export::open(&path).is_ok())
    };
    if !readable {
        remove(&path).await;
        tracing::warn!(
            "a downloaded listening history export is not a readable ZIP; the import failed"
        );
        return step(store, row, ExportStep::Failed, 0, job).await;
    }
    let due = store.job_now();
    let (outcome, landed) = moved(store, row, ExportStep::Downloaded(import_id), due, job).await;
    if !landed {
        remove(&path).await;
    }
    outcome
}

/// Move the import on as `to` says, with the next step due at `due` (the
/// jobs table's clock), completing the run in the same write.
async fn step(
    store: &HistoryStore,
    row: &ImportRow,
    to: ExportStep,
    due: i64,
    job: JobCtx,
) -> Outcome {
    moved(store, row, to, due, job).await.0
}

/// [`step`], with whether the move landed.
async fn moved(
    store: &HistoryStore,
    row: &ImportRow,
    to: ExportStep,
    due: i64,
    job: JobCtx,
) -> (Outcome, bool) {
    let user_id = job.key().to_owned();
    let finish = job.clone();
    match store
        .export_step(&user_id, row, to, due, move |tx| finish.complete(tx))
        .await
    {
        Ok(()) => (Outcome::Done, true),
        // A forget, a new link or another run moved the import on.
        Err(WriteError::MovedOn | WriteError::Job(JobError::LeaseLost)) => (Outcome::Done, false),
        Err(error) if error.is_busy() => (Outcome::Busy, false),
        Err(error) => {
            tracing::warn!(%error, "a listening history export step not kept; trying again");
            (retry(), false)
        }
    }
}

/// A call ListenBrainz did not answer as asked: tried again later while
/// it may yet answer and attempts are left, else the import failed.
async fn unanswered(
    store: &HistoryStore,
    row: &ImportRow,
    error: ProviderError,
    job: JobCtx,
) -> Outcome {
    if error.is_retriable() && job.attempt() < MAX_ATTEMPTS {
        tracing::info!(%error, "ListenBrainz did not answer the export step; trying again");
        return Outcome::Retry {
            code: "HISTORY_EXPORT_UNANSWERED",
            at: error
                .retry_after()
                .and_then(|wait| i64::try_from(wait.as_millis()).ok())
                .map(|wait| store.job_now().saturating_add(wait)),
        };
    }
    tracing::warn!(%error, "ListenBrainz refused the export step; the import failed");
    step(store, row, ExportStep::Failed, 0, job).await
}

/// Try again later while attempts are left, else the import failed.
async fn retry_or_fail(store: &HistoryStore, row: &ImportRow, job: JobCtx) -> Outcome {
    if job.attempt() < MAX_ATTEMPTS {
        return retry();
    }
    step(store, row, ExportStep::Failed, 0, job).await
}

fn retry() -> Outcome {
    Outcome::Retry {
        code: "HISTORY_EXPORT_FAILED",
        at: None,
    }
}
