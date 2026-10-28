//! Request-intake domain logic.
//!
//! The approval gate: a `user` ask waits for admin review while `trusted`
//! and `admin` asks auto-approve and dispatch through the download seam
//! immediately. Duplicate asks attach as co-requesters to the live row
//! instead of spawning rivals; every mutation names the generation it won.
//!
//! The service is one type over several files: `intake` records asks,
//! `decisions` approves, rejects, cancels and retries them, `views` reads
//! them, `mixes` decides personal-mix grants, `recovery` finishes what a
//! restart cut off, and `edition` fetches what an album's edition lacks.

mod decisions;
mod edition;
mod intake;
mod mixes;
mod recovery;
mod views;

pub use decisions::{BatchCancelled, Decided};
pub use intake::{FollowIntake, FollowRelease};
pub use mixes::MixDecided;
pub use recovery::RequestRecovery;

use std::sync::Arc;

use super::RequestsState;
use super::error::RequestsError;
use super::ledger::{RequestRecord, RequestStatus};
use super::models::RequestKind;
use super::quota::QuotaLedger;
use super::sqlite::{EditionStore, PersonalMixStore, RequestStore, WantedStore};
use crate::db::AcquireDb;
use crate::dispatch::{
    DispatchError, DispatchOrigin, DispatchOutcome, DispatchRequest, DispatchTarget,
    DownloadDispatch,
};
use crate::edition::chosen_edition;
use crate::follows::Follows;
use droppedneedle_auth::Principal;

/// Domain service over the requests state. Every mutation is one durable
/// write; nothing is held across an await.
pub struct RequestsService {
    /// Request ledger.
    store: RequestStore,
    /// Quota ledger.
    quota: Arc<QuotaLedger>,
    /// Download dispatch.
    dispatch: Arc<DownloadDispatch>,
    /// Wanted watches.
    wanted: WantedStore,
    /// Follows, for the auto-download asks on the approval badge.
    follows: Follows,
    /// Personal-mix approvals and refresh guard.
    mixes: Arc<PersonalMixStore>,
    /// In-flight edition acquires.
    editions: EditionStore,
    /// The application database (library edition and names).
    library: AcquireDb,
    /// Plugin host for `request_created` events, when there is one.
    plugins: Option<Arc<droppedneedle_plugins::host::PluginHost>>,
    /// The personal-mix builder, when there is one.
    mixer: Option<Arc<super::mix::PersonalMixBuilder>>,
    /// The library's edition plans, when wired.
    edition_plans: Option<Arc<dyn droppedneedle_library::operations::port::EditionPlans>>,
    /// The upgrade policy reader, when wired.
    upgrade_policy: Option<
        droppedneedle_kernel::runtime_config::Live<
            droppedneedle_kernel::runtime_config::sections::DownloadPolicy,
        >,
    >,
    /// Source and disk checks, when wired.
    readiness: Option<super::state::AcquireReadiness>,
}

/// A dispatch read failure as a server fault (the cause goes to the log).
fn dispatch_fault(error: DispatchError) -> RequestsError {
    match error {
        DispatchError::Busy { operation } => RequestsError::Busy { operation },
        DispatchError::Failed(message) => {
            RequestsError::internal(&format_args!("dispatch read failed: {message}"))
        }
    }
}

/// Cancel one linked task, logging a failure. Callers that must not leave
/// a row cancelled over a live transfer check the result.
async fn cancel_linked(dispatch: &DownloadDispatch, task_id: &str) -> bool {
    match dispatch.cancel(task_id).await {
        Ok(()) => true,
        Err(error) => {
            tracing::warn!(task_id, ?error, "task cancel failed");
            false
        }
    }
}

/// Cancel a task this ask just started and could not link. A live task
/// it found belongs to another ask and keeps running.
async fn cancel_started(dispatch: &DownloadDispatch, outcome: &DispatchOutcome) {
    if outcome.started() {
        cancel_linked(dispatch, outcome.task_id()).await;
    }
}

/// What dispatching a claimed row and linking its task came to.
enum Dispatched {
    /// The row follows the task now.
    Linked(String),
    /// The row moved on before the link landed. A task this call started
    /// was cancelled; one it found belongs to another ask and keeps running.
    Stale,
    /// The dispatch failed; the caller settles what the row reads.
    Failed(DispatchError),
}

/// A new row for `principal`'s ask before its claim: no task, no
/// co-requesters, generation zero, not reviewed, and authorized to
/// dispatch unless it waits for approval. Callers fill in what the ask
/// names.
fn fresh_record(
    kind: RequestKind,
    key: String,
    principal: &Principal,
    origin: DispatchOrigin,
    status: RequestStatus,
    now: u64,
) -> RequestRecord {
    RequestRecord {
        key,
        kind,
        status,
        artist_name: String::new(),
        album_title: String::new(),
        artist_mbid: None,
        year: None,
        release_mbid: None,
        track_title: None,
        duration_seconds: None,
        track_release_group_mbid: None,
        user_id: Some(principal.user_id().to_owned()),
        origin,
        requested_by_name: Some(principal.username_display().to_owned()),
        requesters: Vec::new(),
        requested_at: now,
        completed_at: None,
        task_id: None,
        generation: 0,
        dispatch_authorized: status != RequestStatus::AwaitingApproval,
        monitor_artist: false,
        auto_download_artist: false,
        reviewed_by_name: None,
        reviewed_at: None,
    }
}

/// Display title for approval messages: the track title for exact-track
/// rows, else the album title.
fn record_title(record: &RequestRecord) -> String {
    if record.kind == RequestKind::Track
        && let Some(title) = record.track_title.as_deref()
    {
        return title.to_owned();
    }
    record.album_title.clone()
}

/// Build one stored row's dispatch call without widening exact-track
/// identity.
fn stored_dispatch(
    record: &RequestRecord,
    origin: DispatchOrigin,
    owner_id: &str,
) -> DispatchRequest {
    let title = if record.kind == RequestKind::Track {
        record.track_title.clone().unwrap_or_default()
    } else {
        record.album_title.clone()
    };
    let target = match record.kind {
        RequestKind::Album => DispatchTarget::Album {
            release_group_mbid: record.key.clone(),
        },
        RequestKind::Track => DispatchTarget::Track {
            recording_mbid: record.key.clone(),
            album_version: false,
        },
    };
    DispatchRequest {
        user_id: owner_id.to_owned(),
        target,
        artist_name: record.artist_name.clone(),
        title,
        origin,
        release_mbid: record.release_mbid.clone(),
        wanted: Vec::new(),
        planned_album: None,
        // Owner + key + origin + generation: repeating the same decision
        // (double approval, crash before the task link lands) answers the
        // original task; any intervening write bumps the generation and
        // mints fresh.
        idempotency_key: Some(format!(
            "request:{owner_id}:{}:{}:{}",
            record.key,
            origin.as_str(),
            record.generation
        )),
    }
}

impl RequestsService {
    /// Now, as whole unix seconds.
    fn now_epoch(&self) -> u64 {
        self.library.now_epoch()
    }

    /// Service over one requests state.
    pub fn new(state: &RequestsState) -> Self {
        Self {
            store: state.store.clone(),
            quota: state.quota.clone(),
            dispatch: state.dispatch.clone(),
            wanted: state.wanted.clone(),
            follows: state.follows.clone(),
            mixes: state.mixes.clone(),
            editions: state.editions.clone(),
            library: state.library.clone(),
            plugins: state.plugins.clone(),
            mixer: state.mixer.clone(),
            edition_plans: state.edition_plans.clone(),
            upgrade_policy: state.upgrade_policy.clone(),
            readiness: state.readiness.clone(),
        }
    }

    /// Tell `subscriber` plugins about one new request.
    fn announce_created(&self, record: &super::ledger::RequestRecord) {
        use droppedneedle_plugins::runtime::{EventKind, EventPayload, RequestEvent};

        let release_group_mbid = match record.kind {
            RequestKind::Album => record.key.clone(),
            _ => record.track_release_group_mbid.clone().unwrap_or_default(),
        };
        crate::plugin_events::announce(
            &self.plugins,
            EventKind::RequestCreated,
            EventPayload::Request(RequestEvent {
                request_id: record.key.clone(),
                user_id: record.user_id.clone().unwrap_or_default(),
                release_group_mbid,
                status: record.status.as_str().to_owned(),
            }),
        );
    }

    /// The edition the library has chosen for one album, when it holds
    /// the album. A read failure only loses the edition, never the ask.
    async fn chosen_release(&self, release_group_mbid: &str) -> Option<String> {
        match chosen_edition(self.library.db(), release_group_mbid).await {
            Ok(chosen) => chosen.map(|edition| edition.release_mbid),
            Err(error) => {
                tracing::warn!(release_group_mbid, %error, "chosen edition unreadable");
                None
            }
        }
    }

    /// Dispatch `request` for the row `key` claimed at `generation`, and
    /// link the task it gets behind the generation check.
    async fn dispatch_and_link(
        &self,
        kind: RequestKind,
        key: &str,
        request: &DispatchRequest,
        generation: u64,
    ) -> Result<Dispatched, RequestsError> {
        let outcome = match self.dispatch.dispatch(request).await {
            Ok(outcome) => outcome,
            Err(cause) => return Ok(Dispatched::Failed(cause)),
        };
        if !self
            .store
            .link_task(kind, key, outcome.task_id(), Some(generation))
            .await?
        {
            cancel_started(&self.dispatch, &outcome).await;
            return Ok(Dispatched::Stale);
        }
        Ok(Dispatched::Linked(outcome.task_id().to_owned()))
    }
}
