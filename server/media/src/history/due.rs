//! Who is due for an import or a read, and a person's own acts on their
//! history. Every import and read job is queued from here, or from the
//! steps of a running one.
//!
//! - [`HistoryStore::visit`]: a person opened their Profile or Discover.
//!   A linked person with no import gets one now, in place of a first
//!   start the tick spread out; an import with no step queued or running
//!   gets one; a history whose read is due ([`next_read_at`]) gets it. A
//!   step already waiting is never brought forward. Handled at most once a
//!   minute a person.
//! - [`HistoryStore::linked_as`]: a person linked ListenBrainz. As a
//!   visit, and an import that failed is asked for again; with their yes,
//!   a history kept from another account is replaced.
//! - [`HistoryStore::tick`]: the demand tick, while Discover's warm cycle
//!   runs. Each linked person seen in the last seven days who has no
//!   import gets a first step at a random time within two days, so a new
//!   version never asks ListenBrainz for every export at once; the step
//!   starts the import when it runs. Each one whose read is due gets it.
//!
//! A read is due six hours after the last one tried, whether it worked or
//! not; after failed reads it waits as long again as they have been
//! failing, up to a week, so a refused token or a renamed account costs a
//! few reads, not one every six hours.
//!
//! Nothing is queued while ListenBrainz reads are off or do not read, and
//! the tick queues nothing once the day has no room for background reads.
//! After a forget nothing is imported until the person asks
//! ([`HistoryStore::start_again`]).

use std::collections::HashSet;

use droppedneedle_kernel::db::DbError;
use droppedneedle_kernel::jobs::queue::{JobError, State};

use super::HistoryImportState;
use super::export::jitter;
use super::store::HistoryStore;

/// How often a person's visits are handled, ms.
const VISIT_EVERY_MS: i64 = 60_000;
/// How far first imports the tick starts are spread, ms.
pub const SPREAD_MS: i64 = 2 * 86_400_000;
/// How often a person's history is brought up to date, ms.
pub const SYNC_EVERY_MS: i64 = 6 * 3_600_000;
/// How recently a person must have been seen for the tick to queue their
/// work, ms.
pub const SEEN_WINDOW_MS: i64 = 7 * 86_400_000;
/// The longest a read waits after failed ones, ms.
pub const MAX_READ_WAIT_MS: i64 = 7 * 86_400_000;
/// How long the tick leaves a read or a first step it queued before
/// queuing it again, ms.
const REQUEUE_MS: i64 = 3_600_000;

/// Whether a newly linked account differs from the one a history is kept
/// from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AccountCheck {
    /// The same account, or none kept.
    Same,
    /// Another account: `kept` is its name, and `holds` says whether
    /// anything kept or on its way would be replaced, which the person
    /// confirms first.
    Differs {
        /// The account the history is kept from.
        kept: String,
        /// Whether listens are kept or an import is running.
        holds: bool,
    },
}

/// What asking to import again did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StartAgain {
    /// An export is asked for.
    Started,
    /// The person has no ListenBrainz link.
    NoLink,
    /// ListenBrainz reads are off on this server.
    ReadsOff,
    /// The history is kept or being imported already.
    Running,
}

/// When a current history's next read is due, unix ms, given when it was
/// last brought up to date and when its import row last changed (a read
/// tried and failed stamps it). Six hours after the last try; after failed
/// tries, as long again as the reads have been failing, up to a week.
pub(crate) fn next_read_at(last_synced_at: Option<i64>, updated_at: i64) -> i64 {
    let synced = last_synced_at.unwrap_or(updated_at);
    let tried = updated_at.max(synced);
    let failing = tried.saturating_sub(synced).min(MAX_READ_WAIT_MS);
    tried.saturating_add(SYNC_EVERY_MS.max(failing))
}

impl HistoryStore {
    /// A person opened their Profile or Discover: start, bring forward or
    /// bring up to date their history, as the module says. A failure is
    /// logged; the next visit or tick tries again.
    pub async fn visit(&self, user_id: &str) {
        self.visit_as(user_id, false).await;
    }

    async fn visit_as(&self, user_id: &str, forced: bool) {
        let now = self.now_ms();
        if !forced {
            let mut marks = self.marks();
            if marks
                .visits
                .get(user_id)
                .is_some_and(|at| now.saturating_sub(*at) < VISIT_EVERY_MS)
            {
                return;
            }
            marks.visits.insert(user_id.to_owned(), now);
        }
        if !self.reads_on() {
            return;
        }
        let Some(username) = self.linked_name(user_id).await else {
            return;
        };
        let row = match self.row(user_id).await {
            Ok(row) => row,
            Err(error) => {
                tracing::warn!(%error, "a visitor's listening history unreadable; nothing queued");
                return;
            }
        };
        let queued = match row {
            None => self
                .start_first(user_id, &username, self.job_now())
                .await
                .map(|started| {
                    if started {
                        tracing::info!("a listening history import started by its person");
                    }
                }),
            Some(row) => match row.state {
                HistoryImportState::Pending
                | HistoryImportState::Exporting
                | HistoryImportState::Importing => self.keep_going(user_id).await,
                HistoryImportState::Current
                    if now >= next_read_at(row.last_synced_at, row.updated_at)
                        && row
                            .account
                            .as_deref()
                            .is_none_or(|kept| kept.eq_ignore_ascii_case(&username)) =>
                {
                    self.queue_sync(user_id).await
                }
                _ => Ok(()),
            },
        };
        if let Err(error) = queued {
            tracing::warn!(%error, "a visitor's listening history work not queued");
        }
    }

    /// Queue the next step of an import when none is queued or running,
    /// as after a step that stopped without queuing one.
    async fn keep_going(&self, user_id: &str) -> Result<(), JobError> {
        match self.import_job(user_id).await? {
            Some(job) if matches!(job.state, State::Queued | State::Running) => Ok(()),
            _ => self.queue_import(user_id).await,
        }
    }

    /// A person linked ListenBrainz as `username`: as a visit, and an
    /// import that failed is asked for again. With `replace`, the person
    /// said yes to replacing the history kept from another account (see
    /// [`HistoryStore::account_check`]): it is forgotten and an export of
    /// the new account's asked for.
    ///
    /// # Errors
    ///
    /// The write's failure.
    pub async fn linked_as(
        &self,
        user_id: &str,
        username: &str,
        replace: bool,
    ) -> Result<(), JobError> {
        if replace {
            tracing::info!("a listening history replaced by another account's");
            return self.replace_account(user_id, username).await;
        }
        let row = self.row(user_id).await?;
        if let Some(row) = row
            && row.state == HistoryImportState::Failed
            && self.reads_on()
            && row
                .account
                .as_deref()
                .is_none_or(|kept| kept.eq_ignore_ascii_case(username))
        {
            self.export_again(user_id, username, HistoryImportState::Failed)
                .await?;
            return Ok(());
        }
        self.visit_as(user_id, true).await;
        Ok(())
    }

    /// Whether linking `username` would replace a history kept from
    /// another account.
    ///
    /// # Errors
    ///
    /// The read's failure.
    pub async fn account_check(
        &self,
        user_id: &str,
        username: &str,
    ) -> Result<AccountCheck, DbError> {
        let Some(row) = self.row(user_id).await? else {
            return Ok(AccountCheck::Same);
        };
        match row.account {
            Some(kept)
                if row.state != HistoryImportState::Forgotten
                    && !kept.eq_ignore_ascii_case(username) =>
            {
                // An export not asked for yet holds nothing.
                let holds = row.newest_listened_at.is_some()
                    || matches!(
                        row.state,
                        HistoryImportState::Exporting | HistoryImportState::Importing
                    );
                Ok(AccountCheck::Differs { kept, holds })
            }
            _ => Ok(AccountCheck::Same),
        }
    }

    /// The person asked to import their history again, after a forget or
    /// a failure: an export is asked for now, what is kept staying.
    ///
    /// # Errors
    ///
    /// A read's or the write's failure.
    pub async fn start_again(&self, user_id: &str) -> Result<StartAgain, JobError> {
        if !self.reads_on() {
            return Ok(StartAgain::ReadsOff);
        }
        let Some(username) = self.linked_name(user_id).await else {
            return Ok(StartAgain::NoLink);
        };
        let started = match self.row(user_id).await? {
            None => self.start_first(user_id, &username, self.job_now()).await?,
            Some(row)
                if matches!(
                    row.state,
                    HistoryImportState::Failed | HistoryImportState::Forgotten
                ) =>
            {
                self.export_again(user_id, &username, row.state).await?
            }
            Some(_) => false,
        };
        if started {
            tracing::info!("a listening history import asked for again by its person");
            Ok(StartAgain::Started)
        } else {
            Ok(StartAgain::Running)
        }
    }

    /// The demand tick: first imports and reads for the linked people seen
    /// in the last seven days, as the module says. A failure is logged;
    /// the next tick tries again.
    pub async fn tick(&self) {
        if !self.reads_on() {
            return;
        }
        match self.budget().room().await {
            Ok(0) => return,
            Ok(_) => {}
            Err(error) => {
                tracing::debug!(%error, "the day's reads not counted; no history work queued");
                return;
            }
        }
        let now = self.now_ms();
        let since = now.saturating_sub(SEEN_WINDOW_MS);
        let seen: HashSet<String> =
            match droppedneedle_auth::sqlite::seen_since(self.db(), since).await {
                Ok(seen) => seen.into_iter().collect(),
                Err(error) => {
                    tracing::warn!(%error, "who was seen unreadable; no history work queued");
                    return;
                }
            };
        if seen.is_empty() {
            return;
        }
        let linked = match self.linked().await {
            Ok(linked) => linked,
            Err(error) => {
                tracing::warn!(%error, "the linked people unreadable; no history work queued");
                return;
            }
        };
        let mut first = Vec::new();
        let mut syncs = Vec::new();
        let lately = |queued: &std::collections::HashMap<String, i64>, user: &str| {
            queued
                .get(user)
                .is_some_and(|at| now.saturating_sub(*at) < REQUEUE_MS)
        };
        for person in linked {
            if !seen.contains(&person.user_id) {
                continue;
            }
            let marks = self.marks();
            match person.state {
                None if !lately(&marks.firsts, &person.user_id) => {
                    first.push((person.user_id, self.job_now() + jitter(SPREAD_MS)));
                }
                Some(HistoryImportState::Current)
                    if now
                        >= next_read_at(
                            person.last_synced_at,
                            person.updated_at.unwrap_or(now),
                        )
                        && !lately(&marks.syncs, &person.user_id) =>
                {
                    syncs.push(person.user_id);
                }
                _ => {}
            }
        }
        if first.is_empty() && syncs.is_empty() {
            return;
        }
        let (queued_firsts, queued_syncs): (Vec<String>, Vec<String>) = (
            first.iter().map(|(user, _)| user.clone()).collect(),
            syncs.clone(),
        );
        match self.queue_due(first, syncs).await {
            Ok(started) => {
                let mut marks = self.marks();
                for user in queued_firsts {
                    marks.firsts.insert(user, now);
                }
                for user in queued_syncs {
                    marks.syncs.insert(user, now);
                }
                if !started.is_empty() {
                    tracing::info!(
                        people = started.len(),
                        "listening history imports queued, spread over two days"
                    );
                }
            }
            Err(error) => tracing::warn!(%error, "listening history work not queued"),
        }
    }
}
