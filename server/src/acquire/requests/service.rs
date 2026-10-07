//! Request-intake domain logic, ported from v2 `request_service.py` and
//! `requests_page_service.py`.
//!
//! The approval gate: a `user` ask waits for admin review while `trusted`
//! and `admin` asks auto-approve and dispatch through the download seam
//! immediately. Duplicate asks attach as co-requesters to the live row
//! instead of spawning rivals; every mutation names the generation it won.
//! Quirk citations name the v2 behavior each branch preserves.

use std::sync::Arc;

use super::auth::Principal;
use super::bridges::FollowDecisionSink;
use super::dispatch::{
    DispatchError, DispatchOrigin, DispatchOutcome, DispatchRequest, DispatchTaskState,
};
use super::error::RequestsError;
use super::ledger::{
    BeginOutcome, CANCELLABLE_STATUSES, CLEARABLE_STATUSES, EditionMark, RETRYABLE_STATUSES,
    RequestRecord, STATUS_AWAITING_APPROVAL, STATUS_CANCELLED, STATUS_CANCELLING,
    STATUS_DOWNLOADING, STATUS_FAILED, STATUS_IMPORTED, STATUS_INCOMPLETE, STATUS_PENDING,
    STATUS_QUEUED, is_active, meaningful_name, now_epoch, validate_mbid,
};
use super::models::{
    ActionResponse, ActiveCountResponse, ActiveRequestsResponse, AlbumIntake, ApprovalBatchItem,
    ApprovalBatchListResponse, AutoDownloadApprovalItem, AutoDownloadApprovalsResponse,
    BatchCancelResponse, BatchIntake, BatchIntakeResponse, ClearHistoryResponse,
    EditionAcquireResponse, HistoryQuery, HistoryResponse, IntakeResponse, PersonalMixApprovalItem,
    PersonalMixApprovalsResponse, RefreshResponse, RequestItem, RequestKind, SyncResponse,
    TrackIntake, TrackIntakeResponse, WantedActionResponse, WantedItem, WantedResponse,
    WantedRetryingItem,
};
use super::quota::QuotaLedger;
use super::sqlite::{
    EditionStore, FollowApprovalStore, PersonalMixStore, RequestStore, WantedStore, WatchChange,
};
use super::{RequestsState, dispatch::DownloadDispatch};
use crate::acquire::db::AcquireDb;
use crate::acquire::edition::{chosen_edition, local_names};

/// Batch row cap (v2 `BatchAlbumRequest` schema `max_length=500`).
const BATCH_MAX_ITEMS: usize = 500;

/// History page-size cap (v2 `page_size` query `le=100`).
const HISTORY_MAX_PAGE_SIZE: u32 = 100;

/// Domain service over the requests state. Every mutation is one durable
/// write; nothing is held across an await.
pub struct RequestsService {
    /// Request ledger.
    store: RequestStore,
    /// Quota ledger.
    quota: Arc<QuotaLedger>,
    /// Download-dispatch seam.
    dispatch: Arc<dyn DownloadDispatch>,
    /// Wanted watches.
    wanted: WantedStore,
    /// Auto-download approvals.
    follows: FollowApprovalStore,
    /// Personal-mix approvals and refresh guard.
    mixes: Arc<PersonalMixStore>,
    /// In-flight edition acquires.
    editions: EditionStore,
    /// The application database (library edition and names).
    library: AcquireDb,
    /// Verdict sink into the collections follow rows, when wired.
    follow_sink: Option<Arc<dyn FollowDecisionSink>>,
    /// Plugin host for `request_created` events, when attached.
    plugins: crate::acquire::wiring::PluginSlot,
    /// The personal-mix builder, when boot wired one.
    mixer: super::state::MixSlot,
}

impl RequestsService {
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
            follow_sink: state.follow_sink.clone(),
            plugins: state.plugins.clone(),
            mixer: state.mixer.clone(),
        }
    }

    /// Tell `subscriber` plugins about one new request.
    fn announce_created(&self, record: &super::ledger::RequestRecord) {
        use crate::plugins::runtime::{EventKind, EventPayload, RequestEvent};

        let release_group_mbid = match record.kind {
            RequestKind::Album => record.key.clone(),
            _ => record.track_release_group_mbid.clone().unwrap_or_default(),
        };
        crate::acquire::plugin_events::announce(
            &self.plugins,
            EventKind::RequestCreated,
            EventPayload::Request(RequestEvent {
                request_id: record.key.clone(),
                user_id: record.user_id.clone().unwrap_or_default(),
                release_group_mbid,
                status: record.status.clone(),
            }),
        );
    }

    /// Ask for one album. Winners record and maybe dispatch; duplicate asks
    /// attach to the live row and report its status (v2 `request_album`).
    pub async fn request_album(
        &self,
        principal: &Principal,
        body: &AlbumIntake,
    ) -> Result<IntakeResponse, RequestsError> {
        self.intake_album(principal, body, false).await
    }

    /// Ask for one album under a standing grant (the personal mix's
    /// approved auto-request): no per-request approval wait, quotas still
    /// apply.
    pub async fn request_album_granted(
        &self,
        principal: &Principal,
        body: &AlbumIntake,
    ) -> Result<IntakeResponse, RequestsError> {
        self.intake_album(principal, body, true).await
    }

    async fn intake_album(
        &self,
        principal: &Principal,
        body: &AlbumIntake,
        granted: bool,
    ) -> Result<IntakeResponse, RequestsError> {
        let role = principal.role;
        let mbid = validate_mbid(&body.musicbrainz_id)?;
        let now = now_epoch();
        // Seam note: v2 resolves provider ids and release aliases through
        // the ownership and MBID stores, then fills missing names from the
        // album service. Intake treats the submitted id as canonical and
        // requires real names up front.
        let Some(artist_name) = meaningful_name(body.artist.as_deref()) else {
            return Err(missing_names(&mbid));
        };
        let Some(album_title) = meaningful_name(body.album.as_deref()) else {
            return Err(missing_names(&mbid));
        };
        if body
            .artist_mbid
            .as_deref()
            .is_some_and(|id| validate_mbid(id).is_err())
        {
            return Err(RequestsError::InvalidInput {
                message: "Invalid artist MBID format".to_owned(),
            });
        }
        if let Some(existing) = self.store.get(RequestKind::Album, &mbid).await?
            && (is_active(&existing.status) || existing.status == STATUS_CANCELLING)
        {
            return self.attach_album(principal, &existing, body).await;
        }

        let gate = self.quota.request_gate(&principal.user_id, role, 1)?;
        self.quota
            .check_storage_admission(&principal.user_id, role, DispatchOrigin::User)
            .await?;

        // The library's chosen edition wins for an album it holds; the
        // edition the page showed fills in for one it does not.
        let asked = body
            .release_mbid
            .as_deref()
            .map(validate_mbid)
            .transpose()?
            .map(|release| release.to_lowercase());
        let release_mbid = self.chosen_release(&mbid).await.or(asked);
        let needs_approval = !granted && !role.auto_approves();
        let record = self.new_album_record(
            principal,
            &mbid,
            body,
            &artist_name,
            &album_title,
            release_mbid,
            needs_approval,
            now,
        );
        let outcome = self
            .store
            .begin(record, &principal.user_id, gate)
            .await?
            .map_err(|refusal| QuotaLedger::refusal(&refusal))?;
        match outcome {
            BeginOutcome::Existing(winner) if is_active(&winner.status) => {
                self.attach_album(principal, &winner, body).await
            }
            BeginOutcome::Existing(winner) if winner.status == STATUS_CANCELLING => {
                self.attach_album(principal, &winner, body).await
            }
            BeginOutcome::Existing(_) => Ok(IntakeResponse {
                success: false,
                message: "Request could not be recorded".to_owned(),
                musicbrainz_id: mbid,
                status: STATUS_FAILED.to_owned(),
                task_id: None,
            }),
            BeginOutcome::Won(won) => {
                self.announce_created(&won);
                if needs_approval {
                    return Ok(IntakeResponse {
                        success: true,
                        message: "Request submitted, awaiting admin approval".to_owned(),
                        musicbrainz_id: mbid,
                        status: STATUS_AWAITING_APPROVAL.to_owned(),
                        task_id: None,
                    });
                }
                self.dispatch_album(&won, DispatchOrigin::User, &principal.user_id)
                    .await
            }
        }
    }

    /// Ask for one exact recording (v2 `request_track`).
    pub async fn request_track(
        &self,
        principal: &Principal,
        body: &TrackIntake,
    ) -> Result<TrackIntakeResponse, RequestsError> {
        let role = principal.role;
        let mbid = validate_mbid(&body.recording_mbid)?;
        if let Some(rg) = body.release_group_mbid.as_deref() {
            validate_mbid(rg)?;
        }
        if let Some(artist) = body.artist_mbid.as_deref() {
            validate_mbid(artist)?;
        }
        if let Some(release) = body.release_mbid.as_deref() {
            validate_mbid(release)?;
        }
        let artist_name = meaningful_name(Some(body.artist_name.as_str()))
            .unwrap_or_else(|| "Unknown".to_owned());
        let track_title = meaningful_name(Some(body.track_title.as_str())).ok_or_else(|| {
            RequestsError::InvalidInput {
                message: "Track title is required".to_owned(),
            }
        })?;
        let album_title = meaningful_name(body.album_title.as_deref())
            .unwrap_or_else(|| "Single track".to_owned());
        let now = now_epoch();

        if let Some(existing) = self.store.get(RequestKind::Track, &mbid).await?
            && (is_active(&existing.status) || existing.status == STATUS_CANCELLING)
        {
            self.store
                .attach_requester(
                    RequestKind::Track,
                    &mbid,
                    &principal.user_id,
                    Some(principal.display_name()),
                    now,
                )
                .await?;
            return Ok(track_duplicate(&existing));
        }

        let gate = self.quota.request_gate(&principal.user_id, role, 1)?;
        self.quota
            .check_storage_admission(&principal.user_id, role, DispatchOrigin::User)
            .await?;

        let needs_approval = !role.auto_approves();
        let record = RequestRecord {
            key: mbid.to_lowercase(),
            kind: RequestKind::Track,
            status: status_for_role(needs_approval).to_owned(),
            artist_name,
            album_title,
            artist_mbid: body.artist_mbid.clone(),
            year: None,
            release_mbid: body.release_mbid.clone(),
            track_title: Some(track_title),
            duration_seconds: body.duration_seconds,
            track_release_group_mbid: body.release_group_mbid.clone(),
            user_id: Some(principal.user_id.clone()),
            requested_by_name: Some(principal.display_name()),
            requesters: Vec::new(),
            requested_at: now,
            completed_at: None,
            task_id: None,
            generation: 0,
            dispatch_authorized: !needs_approval,
            monitor_artist: false,
            auto_download_artist: false,
            reviewed_by_name: None,
            reviewed_at: None,
        };
        let outcome = self
            .store
            .begin(record, &principal.user_id, gate)
            .await?
            .map_err(|refusal| QuotaLedger::refusal(&refusal))?;
        match outcome {
            BeginOutcome::Existing(winner)
                if is_active(&winner.status) || winner.status == STATUS_CANCELLING =>
            {
                self.store
                    .attach_requester(
                        RequestKind::Track,
                        &mbid,
                        &principal.user_id,
                        Some(principal.display_name()),
                        now,
                    )
                    .await?;
                Ok(track_duplicate(&winner))
            }
            BeginOutcome::Existing(_) => Ok(TrackIntakeResponse {
                status: STATUS_QUEUED.to_owned(),
                task_id: None,
            }),
            BeginOutcome::Won(won) => {
                self.announce_created(&won);
                if needs_approval {
                    return Ok(TrackIntakeResponse {
                        status: STATUS_AWAITING_APPROVAL.to_owned(),
                        task_id: None,
                    });
                }
                self.dispatch_track(&won, DispatchOrigin::User).await
            }
        }
    }

    /// Ask for a batch of albums (v2 `request_batch`): raw dupes and
    /// canonical dupes both skip, live rows attach, unresolvable rows skip,
    /// and each created row dispatches on its own (one failure never fails
    /// the batch).
    pub async fn request_batch(
        &self,
        principal: &Principal,
        body: &BatchIntake,
    ) -> Result<BatchIntakeResponse, RequestsError> {
        if body.items.len() > BATCH_MAX_ITEMS {
            return Err(RequestsError::InvalidInput {
                message: format!("Batch must hold at most {BATCH_MAX_ITEMS} items"),
            });
        }
        let role = principal.role;
        let now = now_epoch();

        // Dedupe pass one: raw submitted ids, case-insensitive.
        let mut seen_raw = std::collections::HashSet::new();
        let mut duplicate_count = 0_u32;
        let mut raw_items = Vec::new();
        for item in &body.items {
            if seen_raw.insert(item.musicbrainz_id.to_lowercase()) {
                raw_items.push(item);
            } else {
                duplicate_count = duplicate_count.saturating_add(1);
            }
        }
        // Dedupe pass two: canonical ids. Intake has no MBID-alias
        // store, so canon equals the validated id; invalid ids skip the way
        // unresolvable rows do in v2.
        let mut seen_canonical = std::collections::HashSet::new();
        let mut normalized = Vec::new();
        for item in raw_items {
            let Ok(mbid) = validate_mbid(&item.musicbrainz_id) else {
                duplicate_count = duplicate_count.saturating_add(1);
                continue;
            };
            if seen_canonical.insert(mbid.to_lowercase()) {
                normalized.push((mbid, item));
            } else {
                duplicate_count = duplicate_count.saturating_add(1);
            }
        }

        let needs_approval = !role.auto_approves();
        let live = self.store.active_mbids(RequestKind::Album).await?;
        let mut new_items = Vec::new();
        let mut existing_items = Vec::new();
        for (mbid, item) in normalized {
            if live.contains(&mbid.to_lowercase()) {
                existing_items.push((mbid, item));
            } else {
                new_items.push((mbid, item));
            }
        }
        let mut skipped = duplicate_count.saturating_add(existing_items.len() as u32);

        if new_items.is_empty() {
            for (mbid, _) in &existing_items {
                self.store
                    .attach_requester(
                        RequestKind::Album,
                        mbid,
                        &principal.user_id,
                        Some(principal.display_name()),
                        now,
                    )
                    .await?;
            }
            return Ok(BatchIntakeResponse {
                success: true,
                message: "All albums already requested".to_owned(),
                requested: 0,
                skipped,
                overflow: 0,
                status: "already_requested".to_owned(),
            });
        }

        let gate = self.quota.request_gate(
            &principal.user_id,
            role,
            u32::try_from(new_items.len()).unwrap_or(u32::MAX),
        )?;
        self.quota
            .check_storage_admission(&principal.user_id, role, DispatchOrigin::User)
            .await?;

        // Name resolution pass: unresolvable rows skip, never fail the batch.
        let mut resolvable = Vec::new();
        for (mbid, item) in new_items {
            let artist = meaningful_name(item.artist_name.as_deref());
            let album = meaningful_name(item.album_title.as_deref());
            match (artist, album) {
                (Some(artist_name), Some(album_title)) => {
                    resolvable.push((mbid, item, artist_name, album_title));
                }
                _ => {
                    skipped = skipped.saturating_add(1);
                }
            }
        }
        if resolvable.is_empty() {
            return Ok(BatchIntakeResponse {
                success: false,
                message: "Batch request could not be recorded".to_owned(),
                requested: 0,
                skipped,
                overflow: 0,
                status: STATUS_FAILED.to_owned(),
            });
        }

        let mut records = Vec::with_capacity(resolvable.len());
        for (mbid, item, artist_name, album_title) in &resolvable {
            let release_mbid = self.chosen_release(mbid).await;
            records.push(RequestRecord {
                key: mbid.to_lowercase(),
                kind: RequestKind::Album,
                status: status_for_role(needs_approval).to_owned(),
                artist_name: artist_name.clone(),
                album_title: album_title.clone(),
                artist_mbid: item.artist_mbid.clone(),
                year: item.year,
                release_mbid,
                track_title: None,
                duration_seconds: None,
                track_release_group_mbid: None,
                user_id: Some(principal.user_id.clone()),
                requested_by_name: Some(principal.display_name()),
                requesters: Vec::new(),
                requested_at: now,
                completed_at: None,
                task_id: None,
                generation: 0,
                dispatch_authorized: !needs_approval,
                monitor_artist: body.monitor_artist,
                auto_download_artist: body.auto_download_artist,
                reviewed_by_name: None,
                reviewed_at: None,
            });
        }
        let outcomes = self
            .store
            .begin_batch(records, &principal.user_id, gate)
            .await?
            .map_err(|refusal| QuotaLedger::refusal(&refusal))?;
        let mut created = Vec::new();
        let mut raced = Vec::new();
        for outcome in outcomes {
            match outcome {
                BeginOutcome::Won(won) => {
                    self.announce_created(&won);
                    created.push(won);
                }
                BeginOutcome::Existing(winner)
                    if is_active(&winner.status) || winner.status == STATUS_CANCELLING =>
                {
                    // Won by someone else between the live read and the
                    // bulk claim: a listener attachment, never a dispatch
                    // candidate (v2 raced-items quirk).
                    raced.push(winner.key);
                    skipped = skipped.saturating_add(1);
                }
                BeginOutcome::Existing(_) => {
                    skipped = skipped.saturating_add(1);
                }
            }
        }
        for mbid in raced
            .iter()
            .chain(existing_items.iter().map(|(mbid, _)| mbid))
        {
            self.store
                .attach_requester(
                    RequestKind::Album,
                    mbid,
                    &principal.user_id,
                    Some(principal.display_name()),
                    now,
                )
                .await?;
        }

        if created.is_empty() {
            if skipped > 0 {
                return Ok(BatchIntakeResponse {
                    success: true,
                    message: "All albums already requested".to_owned(),
                    requested: 0,
                    skipped,
                    overflow: 0,
                    status: "already_requested".to_owned(),
                });
            }
            return Ok(BatchIntakeResponse {
                success: false,
                message: "Batch request could not be recorded".to_owned(),
                requested: 0,
                skipped,
                overflow: 0,
                status: STATUS_FAILED.to_owned(),
            });
        }
        if needs_approval {
            return Ok(BatchIntakeResponse {
                success: true,
                message: "Batch request submitted, awaiting admin approval".to_owned(),
                requested: created.len() as u32,
                skipped,
                overflow: 0,
                status: STATUS_AWAITING_APPROVAL.to_owned(),
            });
        }

        let mut dispatched = 0_u32;
        for won in &created {
            match self
                .dispatch_album(won, DispatchOrigin::User, &principal.user_id)
                .await
            {
                Ok(_) => {
                    dispatched = dispatched.saturating_add(1);
                }
                Err(RequestsError::InvalidInput { .. }) => {
                    // Admission fault: the row already reads failed.
                }
                Err(_) => {
                    // One row's dispatch fault never fails its siblings.
                }
            }
        }
        Ok(BatchIntakeResponse {
            success: true,
            message: format!("Batch request accepted: {dispatched} started"),
            requested: dispatched,
            skipped,
            overflow: 0,
            status: if dispatched > 0 {
                STATUS_PENDING.to_owned()
            } else {
                STATUS_FAILED.to_owned()
            },
        })
    }

    /// Cancel a batch of asks. Non-admins detach from shared rows and
    /// cancel their own; only an explicit admin cancels anything (v2
    /// `cancel_batch`: a missing id must never mint admin rights).
    pub async fn cancel_batch(
        &self,
        principal: &Principal,
        ids: &[String],
        kind: RequestKind,
    ) -> Result<BatchCancelResponse, RequestsError> {
        let is_admin = principal.role.is_admin();
        let now = now_epoch();
        let mut cancelled = 0_u32;
        let mut failed = 0_u32;
        let mut seen = std::collections::HashSet::new();
        for raw in ids {
            if !seen.insert(raw.clone()) {
                continue;
            }
            let Ok(mbid) = validate_mbid(raw) else {
                failed = failed.saturating_add(1);
                continue;
            };
            let outcome = if is_admin {
                self.admin_cancel_one(kind, &mbid, now).await?
            } else {
                self.requester_cancel_one(kind, &mbid, &principal.user_id, now)
                    .await?
            };
            if outcome {
                cancelled = cancelled.saturating_add(1);
            } else {
                failed = failed.saturating_add(1);
            }
        }
        let mut message = format!("Cancelled {cancelled} requests");
        if failed > 0 {
            message.push_str(&format!(", {failed} failed"));
        }
        Ok(BatchCancelResponse {
            success: cancelled > 0,
            cancelled,
            failed,
            message,
        })
    }

    /// Approve one waiting ask and dispatch it (v2 `approve_request`). The
    /// claim moves the row out of the queue before dispatch so a second
    /// approval loses loudly instead of double-dispatching.
    pub async fn approve_request(
        &self,
        principal: &Principal,
        mbid: &str,
        kind: RequestKind,
    ) -> Result<ActionResponse, RequestsError> {
        principal.require_admin()?;
        let mbid = validate_mbid(mbid)?;
        let now = now_epoch();
        let Some(record) = self.store.get(kind, &mbid).await? else {
            return Ok(ActionResponse {
                success: false,
                message: "Request not found".to_owned(),
            });
        };
        if record.status != STATUS_AWAITING_APPROVAL {
            return Ok(not_waiting(&record.status));
        }
        let claimed = self
            .store
            .claim_approval(
                kind,
                &mbid,
                Some(principal.display_name()),
                now,
                record.generation,
            )
            .await?;
        let Some(claimed) = claimed else {
            let status = self
                .store
                .get(kind, &mbid)
                .await?
                .map(|row| row.status)
                .unwrap_or_else(|| "changed".to_owned());
            return Ok(not_waiting(&status));
        };
        // Shared approvals keep the immutable primary owner; only ownerless
        // legacy rows fall back to the acting admin (v2 `_dispatch_record`).
        let owner = claimed
            .user_id
            .clone()
            .unwrap_or_else(|| principal.user_id.clone());
        let request = stored_dispatch(&claimed, DispatchOrigin::Approval, &owner);
        match self.dispatch.dispatch(&request).await {
            Err(DispatchError::Validation(message)) => {
                self.store
                    .restore_status(
                        kind,
                        &mbid,
                        STATUS_AWAITING_APPROVAL,
                        STATUS_PENDING,
                        claimed.generation,
                    )
                    .await?;
                Ok(ActionResponse {
                    success: false,
                    message,
                })
            }
            Err(DispatchError::Failed(cause)) => {
                tracing::error!(%cause, "approved dispatch failed");
                self.store
                    .update_status(
                        kind,
                        &mbid,
                        STATUS_FAILED,
                        Some(now),
                        Some(claimed.generation),
                    )
                    .await?;
                Ok(ActionResponse {
                    success: false,
                    message: format!("Approved but failed to start: {}", record_title(&record)),
                })
            }
            Ok(DispatchOutcome::AlreadyInLibrary) => {
                self.store
                    .update_status(
                        kind,
                        &mbid,
                        STATUS_IMPORTED,
                        Some(now),
                        Some(claimed.generation),
                    )
                    .await?;
                Ok(ActionResponse {
                    success: true,
                    message: format!("Approved: {}", record_title(&record)),
                })
            }
            Ok(DispatchOutcome::Dispatched { task_id }) => {
                if !self
                    .store
                    .link_task(kind, &mbid, &task_id, Some(claimed.generation))
                    .await?
                {
                    cancel_linked(self.dispatch.as_ref(), &task_id).await;
                    return Ok(ActionResponse {
                        success: false,
                        message: "Approved request became stale".to_owned(),
                    });
                }
                Ok(ActionResponse {
                    success: true,
                    message: format!("Approved: {}", record_title(&record)),
                })
            }
        }
    }

    /// Reject one waiting ask (v2 `reject_request`).
    pub async fn reject_request(
        &self,
        principal: &Principal,
        mbid: &str,
        kind: RequestKind,
    ) -> Result<ActionResponse, RequestsError> {
        principal.require_admin()?;
        let mbid = validate_mbid(mbid)?;
        let now = now_epoch();
        let Some(record) = self.store.get(kind, &mbid).await? else {
            return Ok(ActionResponse {
                success: false,
                message: "Request not found".to_owned(),
            });
        };
        if record.status != STATUS_AWAITING_APPROVAL {
            return Ok(not_waiting(&record.status));
        }
        let claimed = self
            .store
            .claim_rejection(
                kind,
                &mbid,
                Some(principal.display_name()),
                now,
                record.generation,
            )
            .await?;
        if !claimed {
            let status = self
                .store
                .get(kind, &mbid)
                .await?
                .map(|row| row.status)
                .unwrap_or_else(|| "changed".to_owned());
            return Ok(not_waiting(&status));
        }
        Ok(ActionResponse {
            success: true,
            message: format!("Rejected: {}", record_title(&record)),
        })
    }

    /// Cancel one ask (v2 `cancel_request`).
    pub async fn cancel_one(
        &self,
        principal: &Principal,
        mbid: &str,
        kind: RequestKind,
    ) -> Result<ActionResponse, RequestsError> {
        let mbid = validate_mbid(mbid)?;
        let now = now_epoch();
        let Some(record) = self.store.get(kind, &mbid).await? else {
            return Ok(ActionResponse {
                success: false,
                message: "Request not found".to_owned(),
            });
        };
        if !principal.role.is_admin() {
            return self
                .requester_cancel_verbose(kind, &mbid, &record, &principal.user_id, now)
                .await;
        }
        if record.status == STATUS_AWAITING_APPROVAL {
            if !self
                .store
                .update_status(
                    kind,
                    &mbid,
                    STATUS_CANCELLED,
                    Some(now),
                    Some(record.generation),
                )
                .await?
            {
                return Ok(ActionResponse {
                    success: false,
                    message: "Request changed while cancelling".to_owned(),
                });
            }
            // Approval cancellation revokes the persisted capability after
            // winning the generation CAS (v2 quirk).
            self.store
                .set_dispatch_authorized(kind, &mbid, false)
                .await?;
            return Ok(ActionResponse {
                success: true,
                message: format!("Cancelled request for {}", record_title(&record)),
            });
        }
        if !CANCELLABLE_STATUSES.contains(&record.status.as_str()) {
            return Ok(ActionResponse {
                success: false,
                message: format!("Cannot cancel request with status '{}'", record.status),
            });
        }
        if let Some(task_id) = record.task_id.as_deref()
            && !cancel_linked(self.dispatch.as_ref(), task_id).await
        {
            return Ok(ActionResponse {
                success: false,
                message: "Could not stop the download; try again".to_owned(),
            });
        }
        if !self
            .store
            .update_status(
                kind,
                &mbid,
                STATUS_CANCELLED,
                Some(now),
                Some(record.generation),
            )
            .await?
        {
            return Ok(ActionResponse {
                success: false,
                message: "Request changed while cancelling".to_owned(),
            });
        }
        Ok(ActionResponse {
            success: true,
            message: format!("Cancelled download of {}", record_title(&record)),
        })
    }

    /// Retry one terminal ask (v2 `retry_request`). An ordinary user may only
    /// dispatch a generation that already carries approval provenance;
    /// anything else rejoins the queue.
    pub async fn retry_request(
        &self,
        principal: &Principal,
        mbid: &str,
        kind: RequestKind,
    ) -> Result<ActionResponse, RequestsError> {
        let mbid = validate_mbid(mbid)?;
        let now = now_epoch();
        let Some(record) = self.store.get(kind, &mbid).await? else {
            return Ok(ActionResponse {
                success: false,
                message: "Request not found".to_owned(),
            });
        };
        if !RETRYABLE_STATUSES.contains(&record.status.as_str()) {
            return Ok(ActionResponse {
                success: false,
                message: format!("Cannot retry request with status '{}'", record.status),
            });
        }
        let prior_status = record.status.clone();
        let can_dispatch = record.dispatch_authorized || principal.role.is_curator();
        let target = if can_dispatch {
            STATUS_PENDING
        } else {
            STATUS_AWAITING_APPROVAL
        };
        let claim = self
            .store
            .claim_retry(
                kind,
                &mbid,
                &principal.user_id,
                target,
                can_dispatch,
                !principal.role.is_admin(),
                now,
                record.generation,
            )
            .await?;
        let generation = match claim {
            super::ledger::RetryClaim::Claimed { generation, .. } => generation,
            super::ledger::RetryClaim::Lost => {
                let current = self.store.get(kind, &mbid).await?;
                let current_status = current.as_ref().map(|row| row.status.as_str());
                if !principal.role.is_admin()
                    && current_status.is_some_and(|s| RETRYABLE_STATUSES.contains(&s))
                    && !self
                        .store
                        .is_requester(kind, &mbid, &principal.user_id)
                        .await?
                {
                    return Err(RequestsError::Forbidden {
                        message: "Cannot retry another user's request".to_owned(),
                    });
                }
                return Ok(ActionResponse {
                    success: false,
                    message: match current {
                        None => "Request not found".to_owned(),
                        Some(row) => format!("Cannot retry request with status '{}'", row.status),
                    },
                });
            }
        };
        if target == STATUS_AWAITING_APPROVAL {
            return Ok(ActionResponse {
                success: true,
                message: "Retry submitted, awaiting admin approval".to_owned(),
            });
        }
        let owner = record
            .user_id
            .clone()
            .unwrap_or_else(|| principal.user_id.clone());
        let request = stored_dispatch(&record, DispatchOrigin::Retry, &owner);
        match self.dispatch.dispatch(&request).await {
            Err(DispatchError::Validation(message)) => {
                self.store
                    .update_status(kind, &mbid, &prior_status, None, Some(generation))
                    .await?;
                Ok(ActionResponse {
                    success: false,
                    message,
                })
            }
            Err(DispatchError::Failed(cause)) => {
                tracing::error!(%cause, "retry dispatch failed");
                self.store
                    .update_status(kind, &mbid, &prior_status, None, Some(generation))
                    .await?;
                Ok(ActionResponse {
                    success: false,
                    message: "Retry failed to start download".to_owned(),
                })
            }
            Ok(DispatchOutcome::AlreadyInLibrary) => {
                self.store
                    .update_status(kind, &mbid, STATUS_IMPORTED, Some(now), Some(generation))
                    .await?;
                Ok(ActionResponse {
                    success: true,
                    message: format!("Re-requested {}", record_title(&record)),
                })
            }
            Ok(DispatchOutcome::Dispatched { task_id }) => {
                if !self
                    .store
                    .link_task(kind, &mbid, &task_id, Some(generation))
                    .await?
                {
                    cancel_linked(self.dispatch.as_ref(), &task_id).await;
                    return Ok(ActionResponse {
                        success: false,
                        message: "Retry request became stale".to_owned(),
                    });
                }
                Ok(ActionResponse {
                    success: true,
                    message: format!("Re-requested {}", record_title(&record)),
                })
            }
        }
    }

    /// Clear one history row. Ownership reads before clearability so a
    /// non-owner gets 403 rather than a misleading miss (v2 quirk); admins
    /// delete the row while users dismiss it from their own view.
    pub async fn clear_history_item(
        &self,
        principal: &Principal,
        mbid: &str,
        kind: RequestKind,
    ) -> Result<ClearHistoryResponse, RequestsError> {
        let mbid = validate_mbid(mbid)?;
        let Some(record) = self.store.get(kind, &mbid).await? else {
            return Ok(ClearHistoryResponse { success: false });
        };
        if !principal.role.is_admin()
            && !self
                .store
                .is_requester(kind, &mbid, &principal.user_id)
                .await?
        {
            return Err(RequestsError::Forbidden {
                message: "Cannot clear another user's request".to_owned(),
            });
        }
        if !CLEARABLE_STATUSES.contains(&record.status.as_str()) {
            return Ok(ClearHistoryResponse { success: false });
        }
        let cleared = if principal.role.is_admin() {
            self.store.delete(kind, &mbid).await?
        } else {
            self.store.dismiss(kind, &mbid, &principal.user_id).await?
        };
        Ok(ClearHistoryResponse { success: cleared })
    }

    /// Live rows. Admins see every row; anyone else sees their own (v2
    /// `user_id=None`-for-admin quirk). Rows reconcile against their linked
    /// tasks on the way out (v2 checks completion inside the read).
    pub async fn active(
        &self,
        principal: &Principal,
    ) -> Result<ActiveRequestsResponse, RequestsError> {
        self.sync_request_statuses().await?;
        let owner = (!principal.role.is_admin()).then_some(principal.user_id.as_str());
        let rows = self.store.active(owner, None).await?;
        let items = self.to_items(&rows).await;
        Ok(ActiveRequestsResponse {
            count: items.len() as u32,
            items,
        })
    }

    /// Live-row count for badges.
    pub async fn active_count(
        &self,
        principal: &Principal,
    ) -> Result<ActiveCountResponse, RequestsError> {
        self.sync_request_statuses().await?;
        let owner = (!principal.role.is_admin()).then_some(principal.user_id.as_str());
        Ok(ActiveCountResponse {
            count: self.store.active(owner, None).await?.len() as u32,
        })
    }

    /// Paged history. Admins see every row; anyone else sees their own minus
    /// dismissed rows.
    pub async fn history(
        &self,
        principal: &Principal,
        query: &HistoryQuery,
    ) -> Result<HistoryResponse, RequestsError> {
        self.sync_request_statuses().await?;
        let page = query.page.unwrap_or(1).max(1);
        let page_size = query.page_size.unwrap_or(20);
        if page_size == 0 || page_size > HISTORY_MAX_PAGE_SIZE {
            return Err(RequestsError::InvalidInput {
                message: format!("page_size must be between 1 and {HISTORY_MAX_PAGE_SIZE}"),
            });
        }
        let sort = query.sort.as_deref().unwrap_or("newest");
        if !matches!(sort, "newest" | "oldest" | "status") {
            return Err(RequestsError::InvalidInput {
                message: "sort must be newest, oldest, or status".to_owned(),
            });
        }
        let kind = match query.kind.as_deref() {
            None => None,
            Some(value) => {
                Some(
                    RequestKind::parse(value).ok_or_else(|| RequestsError::InvalidInput {
                        message: "kind must be album or track".to_owned(),
                    })?,
                )
            }
        };
        let owner = (!principal.role.is_admin()).then_some(principal.user_id.as_str());
        let offset = page.saturating_sub(1).saturating_mul(page_size);
        let (rows, total) = self
            .store
            .history(
                owner,
                kind,
                query.status.as_deref(),
                sort,
                offset,
                page_size,
            )
            .await?;
        let items = self.to_items(&rows).await;
        let total_pages = total.div_ceil(page_size).max(1);
        Ok(HistoryResponse {
            items,
            total,
            page,
            page_size,
            total_pages,
        })
    }

    /// Rows waiting for review (admin only in v2).
    pub async fn pending_approvals(
        &self,
        principal: &Principal,
    ) -> Result<ActiveRequestsResponse, RequestsError> {
        principal.require_admin()?;
        let rows = self.store.pending_approvals().await?;
        let items = self.to_items(&rows).await;
        Ok(ActiveRequestsResponse {
            count: items.len() as u32,
            items,
        })
    }

    /// Pending-review badge count: album/track rows plus auto-download units
    /// plus personal-mix rows (v2 sums all three sources).
    pub async fn pending_approval_count(
        &self,
        principal: &Principal,
    ) -> Result<ActiveCountResponse, RequestsError> {
        principal.require_admin()?;
        let asks = self.store.pending_approvals().await?.len() as u32;
        let follows = self.follows.pending_units().await?;
        let mixes = self.mixes.pending_count().await?;
        Ok(ActiveCountResponse {
            count: asks.saturating_add(follows).saturating_add(mixes),
        })
    }

    /// Wanted watches plus the still-retrying set. Admins see every row.
    pub async fn wanted(&self, principal: &Principal) -> Result<WantedResponse, RequestsError> {
        let is_admin = principal.role.is_admin();
        let watches = self
            .wanted
            .watches_for(&principal.user_id, is_admin)
            .await?;
        let items = watches
            .into_iter()
            .map(|watch| WantedItem {
                musicbrainz_id: watch.key,
                artist_name: watch.artist_name,
                album_title: watch.album_title,
                kind: watch.kind,
                state: watch.state,
                check_count: watch.check_count,
                next_check_at: Some(watch.next_check_at),
                new_candidate_count: watch.new_candidate_count,
                created_at: watch.created_at,
                artist_mbid: watch.artist_mbid,
                year: watch.year,
                cover_url: watch.cover_url,
                user_id: is_admin.then_some(watch.user_id),
                user_name: if is_admin { watch.user_name } else { None },
            })
            .collect::<Vec<_>>();
        let retrying = self.retrying(principal).await?;
        Ok(WantedResponse {
            count: items.len() as u32,
            items,
            retrying,
        })
    }

    /// Failed or short album asks still inside their auto-retry ladder:
    /// the wanted view's read-only "still hunting" rows (v2
    /// `list_retrying_for`). A row graduates into a real watch the sweep
    /// after its ladder runs out.
    async fn retrying(
        &self,
        principal: &Principal,
    ) -> Result<Vec<WantedRetryingItem>, RequestsError> {
        let is_admin = principal.role.is_admin();
        let mut out = Vec::new();
        for status in [STATUS_FAILED, STATUS_INCOMPLETE] {
            for record in self.store.with_status(RequestKind::Album, status).await? {
                if !is_admin && !record.has_requester(&principal.user_id) {
                    continue;
                }
                let Some(task_id) = record.task_id.as_deref() else {
                    continue;
                };
                let schedule = match self.dispatch.retry_schedule(task_id).await {
                    Ok(Some(schedule)) => schedule,
                    Ok(None) => continue,
                    Err(error) => {
                        tracing::warn!(task_id, ?error, "retry schedule read failed; row skipped");
                        continue;
                    }
                };
                out.push(WantedRetryingItem {
                    musicbrainz_id: record.key.clone(),
                    artist_name: record.artist_name.clone(),
                    album_title: record.album_title.clone(),
                    retry_count: schedule.retry_count,
                    max_attempts: schedule.max_attempts,
                    next_retry_at: Some(schedule.next_retry_at),
                    artist_mbid: record.artist_mbid.clone(),
                    year: record.year,
                    cover_url: None,
                    user_id: if is_admin {
                        record.user_id.clone()
                    } else {
                        None
                    },
                    user_name: if is_admin {
                        record.requested_by_name.clone()
                    } else {
                        None
                    },
                });
            }
        }
        out.sort_by_key(|item| item.next_retry_at);
        Ok(out)
    }

    /// Stop one wanted watch.
    pub async fn wanted_stop(
        &self,
        principal: &Principal,
        mbid: &str,
    ) -> Result<WantedActionResponse, RequestsError> {
        let mbid = validate_mbid(mbid)?;
        let change = self
            .wanted
            .stop(&mbid, &principal.user_id, principal.role.is_admin())
            .await?;
        watch_action(change)
    }

    /// Resume one wanted watch (doubles as "check now" on a watching row).
    pub async fn wanted_resume(
        &self,
        principal: &Principal,
        mbid: &str,
    ) -> Result<WantedActionResponse, RequestsError> {
        let mbid = validate_mbid(mbid)?;
        let change = self
            .wanted
            .resume(
                &mbid,
                &principal.user_id,
                principal.role.is_admin(),
                now_epoch(),
            )
            .await?;
        watch_action(change)
    }

    /// Clear one watch's unseen candidates.
    pub async fn wanted_seen(
        &self,
        principal: &Principal,
        mbid: &str,
    ) -> Result<WantedActionResponse, RequestsError> {
        let mbid = validate_mbid(mbid)?;
        let change = self
            .wanted
            .mark_seen(&mbid, &principal.user_id, principal.role.is_admin())
            .await?;
        watch_action(change)
    }

    /// Pending auto-download approvals (admin).
    pub async fn auto_download_approvals(
        &self,
        principal: &Principal,
    ) -> Result<AutoDownloadApprovalsResponse, RequestsError> {
        principal.require_admin()?;
        let items = self
            .follows
            .pending()
            .await?
            .into_iter()
            .map(|row| AutoDownloadApprovalItem {
                user_id: row.user_id,
                user_name: row.user_name,
                artist_mbid: row.artist_mbid,
                artist_name: row.artist_name,
                requested_at: row.requested_at,
            })
            .collect::<Vec<_>>();
        Ok(AutoDownloadApprovalsResponse {
            count: items.len() as u32,
            items,
        })
    }

    /// Approve one auto-download grant (admin).
    pub async fn approve_auto_download(
        &self,
        principal: &Principal,
        user_id: &str,
        artist_mbid: &str,
    ) -> Result<ActionResponse, RequestsError> {
        principal.require_admin()?;
        validate_mbid(artist_mbid)?;
        let Some(row) = self
            .follows
            .decide(
                user_id,
                artist_mbid,
                "approved",
                reviewer(principal),
                now_epoch(),
            )
            .await?
        else {
            return Ok(no_match());
        };
        if let Some(sink) = &self.follow_sink {
            sink.arm_auto_download(user_id, &row.artist_mbid, &row.artist_name)
                .await
                .map_err(|cause| RequestsError::internal(&cause))?;
        }
        Ok(ActionResponse {
            success: true,
            message: "Auto-download approved".to_owned(),
        })
    }

    /// Reject one auto-download ask, keeping the follow (admin; as v2 does).
    pub async fn reject_auto_download(
        &self,
        principal: &Principal,
        user_id: &str,
        artist_mbid: &str,
    ) -> Result<ActionResponse, RequestsError> {
        principal.require_admin()?;
        validate_mbid(artist_mbid)?;
        if self
            .follows
            .decide(
                user_id,
                artist_mbid,
                "rejected",
                reviewer(principal),
                now_epoch(),
            )
            .await?
            .is_none()
        {
            return Ok(no_match());
        }
        if let Some(sink) = &self.follow_sink {
            sink.clear_auto_download(user_id, artist_mbid)
                .await
                .map_err(|cause| RequestsError::internal(&cause))?;
        }
        Ok(ActionResponse {
            success: true,
            message: "Auto-download rejected".to_owned(),
        })
    }

    /// Revoke one auto-download grant, keeping the follow (admin; as v2 does).
    pub async fn revoke_auto_download(
        &self,
        principal: &Principal,
        user_id: &str,
        artist_mbid: &str,
    ) -> Result<ActionResponse, RequestsError> {
        principal.require_admin()?;
        validate_mbid(artist_mbid)?;
        if self
            .follows
            .revoke(user_id, artist_mbid, reviewer(principal), now_epoch())
            .await?
            .is_none()
        {
            return Ok(no_match());
        }
        if let Some(sink) = &self.follow_sink {
            sink.clear_auto_download(user_id, artist_mbid)
                .await
                .map_err(|cause| RequestsError::internal(&cause))?;
        }
        Ok(ActionResponse {
            success: true,
            message: "Auto-download revoked".to_owned(),
        })
    }

    /// Pending bulk-approval batches (admin).
    pub async fn auto_download_batches(
        &self,
        principal: &Principal,
    ) -> Result<ApprovalBatchListResponse, RequestsError> {
        principal.require_admin()?;
        let batches = self
            .follows
            .pending_batches()
            .await?
            .iter()
            .map(|batch| ApprovalBatchItem {
                batch_id: batch.batch_id.clone(),
                user_id: batch.user_id.clone(),
                user_name: batch.user_name.clone(),
                artist_count: batch.artists.len() as u32,
                sample_names: batch
                    .artists
                    .iter()
                    .take(3)
                    .map(|(_, name)| name.clone())
                    .collect(),
                requested_at: batch.requested_at,
            })
            .collect::<Vec<_>>();
        Ok(ApprovalBatchListResponse {
            count: batches.len() as u32,
            batches,
        })
    }

    /// Approve one bulk batch (admin).
    pub async fn approve_auto_download_batch(
        &self,
        principal: &Principal,
        batch_id: &str,
    ) -> Result<ActionResponse, RequestsError> {
        self.decide_batch(principal, batch_id, "approved").await
    }

    /// Reject one bulk batch (admin).
    pub async fn reject_auto_download_batch(
        &self,
        principal: &Principal,
        batch_id: &str,
    ) -> Result<ActionResponse, RequestsError> {
        self.decide_batch(principal, batch_id, "rejected").await
    }

    /// Decide every pending row of one batch and carry the verdict into the
    /// follow rows.
    async fn decide_batch(
        &self,
        principal: &Principal,
        batch_id: &str,
        state: &str,
    ) -> Result<ActionResponse, RequestsError> {
        principal.require_admin()?;
        let rows = self
            .follows
            .decide_batch(batch_id, state, reviewer(principal), now_epoch())
            .await?;
        if rows.is_empty() {
            return Ok(ActionResponse {
                success: false,
                message: "No matching batch found".to_owned(),
            });
        }
        if let Some(sink) = &self.follow_sink {
            for (user_id, artist_mbid, artist_name) in &rows {
                let applied = if state == "approved" {
                    sink.arm_auto_download(user_id, artist_mbid, artist_name)
                        .await
                } else {
                    sink.clear_auto_download(user_id, artist_mbid).await
                };
                applied.map_err(|cause| RequestsError::internal(&cause))?;
            }
        }
        Ok(ActionResponse {
            success: true,
            message: format!("Auto-download {state} for {} artists", rows.len()),
        })
    }

    /// Pending personal-mix approvals (admin).
    pub async fn mix_approvals(
        &self,
        principal: &Principal,
    ) -> Result<PersonalMixApprovalsResponse, RequestsError> {
        principal.require_admin()?;
        let items = self
            .mixes
            .pending()
            .await?
            .into_iter()
            .map(|row| PersonalMixApprovalItem {
                user_id: row.user_id,
                user_name: row.user_name,
                requested_at: row.requested_at,
            })
            .collect::<Vec<_>>();
        Ok(PersonalMixApprovalsResponse {
            count: items.len() as u32,
            items,
        })
    }

    /// Approve one mix auto-request grant (admin).
    pub async fn approve_mix(
        &self,
        principal: &Principal,
        user_id: &str,
    ) -> Result<ActionResponse, RequestsError> {
        principal.require_admin()?;
        if !self
            .mixes
            .decide(user_id, "approved", reviewer(principal), now_epoch())
            .await?
        {
            return Ok(no_match());
        }
        Ok(ActionResponse {
            success: true,
            message: "Weekly Mix auto-request approved".to_owned(),
        })
    }

    /// Reject one mix auto-request ask (admin).
    pub async fn reject_mix(
        &self,
        principal: &Principal,
        user_id: &str,
    ) -> Result<ActionResponse, RequestsError> {
        principal.require_admin()?;
        if !self
            .mixes
            .decide(user_id, "rejected", reviewer(principal), now_epoch())
            .await?
        {
            return Ok(no_match());
        }
        // A rejection also turns the user's toggle off (v2).
        if let Some(mixer) = self.mixer.get() {
            mixer.clear_intent(user_id).await;
        }
        Ok(ActionResponse {
            success: true,
            message: "Weekly Mix auto-request rejected".to_owned(),
        })
    }

    /// Revoke one mix auto-request grant (admin).
    pub async fn revoke_mix(
        &self,
        principal: &Principal,
        user_id: &str,
    ) -> Result<ActionResponse, RequestsError> {
        principal.require_admin()?;
        if !self
            .mixes
            .revoke(user_id, reviewer(principal), now_epoch())
            .await?
        {
            return Ok(no_match());
        }
        if let Some(mixer) = self.mixer.get() {
            mixer.clear_intent(user_id).await;
        }
        Ok(ActionResponse {
            success: true,
            message: "Weekly Mix auto-request revoked".to_owned(),
        })
    }

    /// Refresh one user's personal mix. The build runs in the background
    /// (a cold build waits on ListenBrainz pacing for minutes) and lands as
    /// a `personal_mix_refreshed` event. While it runs, repeat calls answer
    /// `already_running` instead of stacking builds (v2).
    pub async fn refresh_personal_mix(
        &self,
        principal: &Principal,
    ) -> Result<RefreshResponse, RequestsError> {
        let Some(mixer) = self.mixer.get() else {
            return Err(RequestsError::Conflict {
                message: "Weekly Mix is not available on this server.".to_owned(),
            });
        };
        if !mixer.is_linked(&principal.user_id).await {
            return Err(RequestsError::InvalidInput {
                message: "Connect ListenBrainz first to build Your Weekly Mix".to_owned(),
            });
        }
        if !self.mixes.refresh_start(&principal.user_id)? {
            return Ok(RefreshResponse {
                status: "already_running".to_owned(),
            });
        }
        mixer.spawn_refresh(principal.user_id.clone());
        Ok(RefreshResponse {
            status: "started".to_owned(),
        })
    }

    /// Fill the selected edition's missing tracks and upgrade its
    /// below-cutoff owned tracks (curator only, as in v2). Never retags
    /// existing files: the seam only fetches.
    pub async fn acquire_edition(
        &self,
        principal: &Principal,
        album_id: &str,
    ) -> Result<EditionAcquireResponse, RequestsError> {
        principal.require_curator()?;
        let mbid = validate_mbid(album_id)?;
        let now = now_epoch();
        if let Some(mark) = self.editions.get(&mbid).await? {
            let state = self
                .dispatch
                .task_state(&mark.task_id)
                .await
                .map_err(dispatch_fault)?;
            if state == DispatchTaskState::Active {
                return Ok(EditionAcquireResponse {
                    status: "already_in_progress".to_owned(),
                    message: "Edition acquire already in progress".to_owned(),
                    task_id: Some(mark.task_id),
                });
            }
            self.editions.clear(&mbid).await?;
        }
        self.quota
            .check_storage_admission(&principal.user_id, principal.role, DispatchOrigin::Edition)
            .await?;
        // The search needs the album's names, and the fetch its chosen
        // edition: both come from the library's copy of the album.
        let Some((artist_name, title)) = local_names(self.library.pool(), &mbid)
            .await
            .map_err(|error| RequestsError::internal(&error))?
        else {
            return Err(RequestsError::InvalidInput {
                message: "This album is not in your library yet. Request the album instead."
                    .to_owned(),
            });
        };
        let request = DispatchRequest {
            user_id: principal.user_id.clone(),
            kind: "edition".to_owned(),
            key: mbid.clone(),
            artist_name,
            title,
            origin: DispatchOrigin::Edition,
            release_mbid: self.chosen_release(&mbid).await,
            // No stable per-decision key exists (marks clear on terminal,
            // so a key would pin re-acquires to the old task); the
            // in-progress mark above owns double-submit dedup.
            idempotency_key: None,
        };
        match self.dispatch.dispatch(&request).await {
            Err(DispatchError::Validation(message)) => Err(RequestsError::InvalidInput { message }),
            Err(DispatchError::Failed(cause)) => Err(RequestsError::internal(&format_args!(
                "edition dispatch failed: {cause}"
            ))),
            Ok(DispatchOutcome::AlreadyInLibrary) => Ok(EditionAcquireResponse {
                status: "already_complete".to_owned(),
                message: "Edition is already complete".to_owned(),
                task_id: None,
            }),
            Ok(DispatchOutcome::Dispatched { task_id }) => {
                self.editions
                    .set(EditionMark {
                        key: mbid,
                        task_id: task_id.clone(),
                        started_at: now,
                    })
                    .await?;
                Ok(EditionAcquireResponse {
                    status: "started".to_owned(),
                    message: "Edition acquire started".to_owned(),
                    task_id: Some(task_id),
                })
            }
        }
    }

    /// Startup recovery for the ledger. A row left `cancelling` finishes
    /// its cancel (the owner asked to stop; cancelling a task twice is
    /// harmless). An authorized `pending` row with no task had its dispatch
    /// or its link cut off: it relinks to the task created for it, or
    /// dispatches under the same idempotency key, so nothing runs twice.
    pub async fn recover(&self) -> Result<RequestRecovery, RequestsError> {
        let now = now_epoch();
        let mut report = RequestRecovery::default();
        for kind in [RequestKind::Album, RequestKind::Track] {
            for record in self.store.with_status(kind, STATUS_CANCELLING).await? {
                if let Some(task_id) = record.task_id.as_deref()
                    && !cancel_linked(self.dispatch.as_ref(), task_id).await
                {
                    continue;
                }
                if self
                    .store
                    .update_status(
                        kind,
                        &record.key,
                        STATUS_CANCELLED,
                        Some(now),
                        Some(record.generation),
                    )
                    .await?
                {
                    report.cancelled += 1;
                }
            }
            for record in self.store.with_status(kind, STATUS_PENDING).await? {
                if record.task_id.is_some() || !record.dispatch_authorized {
                    continue;
                }
                let owner = record.user_id.clone().unwrap_or_default();
                let found = self
                    .dispatch
                    .find_task_since(&owner, kind.as_str(), &record.key, record.requested_at)
                    .await;
                let task_id = match found {
                    Ok(Some(task_id)) => {
                        report.relinked += 1;
                        task_id
                    }
                    Ok(None) => {
                        let request = stored_dispatch(&record, DispatchOrigin::User, &owner);
                        match self.dispatch.dispatch(&request).await {
                            Ok(DispatchOutcome::Dispatched { task_id }) => {
                                report.redispatched += 1;
                                task_id
                            }
                            Ok(DispatchOutcome::AlreadyInLibrary) => continue,
                            Err(error) => {
                                tracing::warn!(key = %record.key, ?error, "recovery dispatch failed");
                                continue;
                            }
                        }
                    }
                    Err(error) => {
                        tracing::warn!(key = %record.key, ?error, "recovery task lookup failed");
                        continue;
                    }
                };
                self.store
                    .link_task(kind, &record.key, &task_id, Some(record.generation))
                    .await?;
            }
        }
        Ok(report)
    }

    /// Reconcile live rows with their linked tasks (v2
    /// `sync_request_statuses`). One bad row never stops the sweep.
    pub async fn sync_request_statuses(&self) -> Result<SyncResponse, RequestsError> {
        let rows = self.store.active(None, None).await?;
        let now = now_epoch();
        let mut reconciled = 0_u32;
        for record in rows {
            match self.reconcile_one(&record, now).await {
                Ok(true) => reconciled = reconciled.saturating_add(1),
                Ok(false) => {}
                Err(error) => {
                    tracing::warn!(key = %record.key, ?error, "request reconcile failed; row skipped");
                }
            }
        }
        Ok(SyncResponse { reconciled })
    }

    /// Attach a duplicate album ask to its live row and report the row's own
    /// status (v2: the duplicate reply mirrors the winner, including the
    /// awaiting-approval message).
    async fn attach_album(
        &self,
        principal: &Principal,
        existing: &RequestRecord,
        body: &AlbumIntake,
    ) -> Result<IntakeResponse, RequestsError> {
        self.store
            .attach_requester(
                RequestKind::Album,
                &existing.key,
                &principal.user_id,
                Some(principal.display_name()),
                now_epoch(),
            )
            .await?;
        if body.monitor_artist {
            self.store
                .widen_monitoring(RequestKind::Album, &existing.key, body.auto_download_artist)
                .await?;
        }
        let message = if existing.status == STATUS_AWAITING_APPROVAL {
            "Request is awaiting admin approval"
        } else if existing.status == STATUS_CANCELLING {
            "Request is being cancelled"
        } else {
            "Request already in progress"
        };
        Ok(IntakeResponse {
            success: true,
            message: message.to_owned(),
            musicbrainz_id: existing.key.clone(),
            status: existing.status.clone(),
            task_id: existing.task_id.clone(),
        })
    }

    /// The edition the library has chosen for one album, when it holds
    /// the album. A read failure only loses the edition, never the ask.
    async fn chosen_release(&self, release_group_mbid: &str) -> Option<String> {
        match chosen_edition(self.library.pool(), release_group_mbid).await {
            Ok(chosen) => chosen.map(|edition| edition.release_mbid),
            Err(error) => {
                tracing::warn!(release_group_mbid, %error, "chosen edition unreadable");
                None
            }
        }
    }

    /// Build a fresh album row for the claim.
    #[allow(clippy::too_many_arguments)]
    fn new_album_record(
        &self,
        principal: &Principal,
        mbid: &str,
        body: &AlbumIntake,
        artist_name: &str,
        album_title: &str,
        release_mbid: Option<String>,
        needs_approval: bool,
        now: u64,
    ) -> RequestRecord {
        RequestRecord {
            key: mbid.to_lowercase(),
            kind: RequestKind::Album,
            status: status_for_role(needs_approval).to_owned(),
            artist_name: artist_name.to_owned(),
            album_title: album_title.to_owned(),
            artist_mbid: body.artist_mbid.clone(),
            year: body.year,
            release_mbid,
            track_title: None,
            duration_seconds: None,
            track_release_group_mbid: None,
            user_id: Some(principal.user_id.clone()),
            requested_by_name: Some(principal.display_name()),
            requesters: Vec::new(),
            requested_at: now,
            completed_at: None,
            task_id: None,
            generation: 0,
            dispatch_authorized: !needs_approval,
            monitor_artist: body.monitor_artist,
            auto_download_artist: body.auto_download_artist,
            reviewed_by_name: None,
            reviewed_at: None,
        }
    }

    /// Dispatch one won album row and link its task (v2 dispatch-and-link
    /// with orphan-cancel when the generation moved on).
    async fn dispatch_album(
        &self,
        won: &RequestRecord,
        origin: DispatchOrigin,
        actor_id: &str,
    ) -> Result<IntakeResponse, RequestsError> {
        let owner = won.user_id.clone().unwrap_or_else(|| actor_id.to_owned());
        let request = stored_dispatch(won, origin, &owner);
        let now = now_epoch();
        match self.dispatch.dispatch(&request).await {
            Err(DispatchError::Validation(message)) => {
                self.store
                    .update_status(
                        RequestKind::Album,
                        &won.key,
                        STATUS_FAILED,
                        Some(now),
                        Some(won.generation),
                    )
                    .await?;
                Err(RequestsError::InvalidInput { message })
            }
            Err(DispatchError::Failed(cause)) => {
                self.store
                    .update_status(
                        RequestKind::Album,
                        &won.key,
                        STATUS_FAILED,
                        Some(now),
                        Some(won.generation),
                    )
                    .await?;
                Err(RequestsError::internal(&format_args!(
                    "album dispatch failed: {cause}"
                )))
            }
            Ok(DispatchOutcome::AlreadyInLibrary) => {
                self.store
                    .update_status(
                        RequestKind::Album,
                        &won.key,
                        STATUS_IMPORTED,
                        Some(now),
                        Some(won.generation),
                    )
                    .await?;
                // Quirk, kept: v2 marks the row imported yet answers
                // `status: "pending"` on this path.
                Ok(IntakeResponse {
                    success: true,
                    message: "Album is already in the library".to_owned(),
                    musicbrainz_id: won.key.clone(),
                    status: STATUS_PENDING.to_owned(),
                    task_id: None,
                })
            }
            Ok(DispatchOutcome::Dispatched { task_id }) => {
                if !self
                    .store
                    .link_task(RequestKind::Album, &won.key, &task_id, Some(won.generation))
                    .await?
                {
                    cancel_linked(self.dispatch.as_ref(), &task_id).await;
                    return Err(RequestsError::internal(&format_args!(
                        "album link lost its generation for {}",
                        won.key
                    )));
                }
                Ok(IntakeResponse {
                    success: true,
                    message: "Request accepted".to_owned(),
                    musicbrainz_id: won.key.clone(),
                    status: STATUS_PENDING.to_owned(),
                    task_id: Some(task_id),
                })
            }
        }
    }

    /// Dispatch one won track row and link its task.
    async fn dispatch_track(
        &self,
        won: &RequestRecord,
        origin: DispatchOrigin,
    ) -> Result<TrackIntakeResponse, RequestsError> {
        let owner = won.user_id.clone().unwrap_or_default();
        let request = stored_dispatch(won, origin, &owner);
        let now = now_epoch();
        match self.dispatch.dispatch(&request).await {
            Err(DispatchError::Validation(message)) => {
                self.store
                    .update_status(
                        RequestKind::Track,
                        &won.key,
                        STATUS_FAILED,
                        Some(now),
                        Some(won.generation),
                    )
                    .await?;
                Err(RequestsError::InvalidInput { message })
            }
            Err(DispatchError::Failed(cause)) => {
                self.store
                    .update_status(
                        RequestKind::Track,
                        &won.key,
                        STATUS_FAILED,
                        Some(now),
                        Some(won.generation),
                    )
                    .await?;
                Err(RequestsError::internal(&format_args!(
                    "track dispatch failed: {cause}"
                )))
            }
            Ok(DispatchOutcome::AlreadyInLibrary) => {
                self.store
                    .update_status(
                        RequestKind::Track,
                        &won.key,
                        STATUS_IMPORTED,
                        Some(now),
                        Some(won.generation),
                    )
                    .await?;
                Ok(TrackIntakeResponse {
                    status: "already_in_library".to_owned(),
                    task_id: None,
                })
            }
            Ok(DispatchOutcome::Dispatched { task_id }) => {
                if !self
                    .store
                    .link_task(RequestKind::Track, &won.key, &task_id, Some(won.generation))
                    .await?
                {
                    cancel_linked(self.dispatch.as_ref(), &task_id).await;
                    return Err(RequestsError::internal(&format_args!(
                        "track link lost its generation for {}",
                        won.key
                    )));
                }
                Ok(TrackIntakeResponse {
                    status: STATUS_QUEUED.to_owned(),
                    task_id: Some(task_id),
                })
            }
        }
    }

    /// Quiet admin cancel of one row for batch-cancel: only live rows
    /// cancel, and approval cancels revoke the persisted capability.
    async fn admin_cancel_one(
        &self,
        kind: RequestKind,
        mbid: &str,
        now: u64,
    ) -> Result<bool, RequestsError> {
        let Some(record) = self.store.get(kind, mbid).await? else {
            return Ok(false);
        };
        if !matches!(
            record.status.as_str(),
            STATUS_AWAITING_APPROVAL | STATUS_PENDING | STATUS_QUEUED | STATUS_DOWNLOADING
        ) {
            return Ok(false);
        }
        if let Some(task_id) = record.task_id.as_deref()
            && !cancel_linked(self.dispatch.as_ref(), task_id).await
        {
            return Ok(false);
        }
        if !self
            .store
            .update_status(
                kind,
                mbid,
                STATUS_CANCELLED,
                Some(now),
                Some(record.generation),
            )
            .await?
        {
            return Ok(false);
        }
        if record.status == STATUS_AWAITING_APPROVAL {
            self.store
                .set_dispatch_authorized(kind, mbid, false)
                .await?;
        }
        Ok(true)
    }

    /// Quiet requester cancel of one row for batch-cancel. The task
    /// cancels under the immutable primary owner, never the actor (v2
    /// quirk: a co-requester can never replace the attribution).
    async fn requester_cancel_one(
        &self,
        kind: RequestKind,
        mbid: &str,
        user_id: &str,
        now: u64,
    ) -> Result<bool, RequestsError> {
        if self.store.get(kind, mbid).await?.is_none() {
            return Ok(false);
        }
        let decision = self
            .store
            .prepare_requester_cancel(kind, mbid, user_id, now)
            .await?;
        match decision {
            None => Ok(false),
            Some(super::ledger::CancelDecision::Denied { .. }) => Ok(false),
            Some(super::ledger::CancelDecision::Detached) => Ok(true),
            Some(super::ledger::CancelDecision::CancelledDirect) => Ok(true),
            Some(super::ledger::CancelDecision::CancelTask {
                prior_status,
                task_id,
                generation,
                ..
            }) => {
                if let Some(task_id) = task_id.as_deref()
                    && !cancel_linked(self.dispatch.as_ref(), task_id).await
                {
                    self.store
                        .restore_status(kind, mbid, &prior_status, STATUS_CANCELLING, generation)
                        .await?;
                    return Ok(false);
                }
                if !self
                    .store
                    .update_status(kind, mbid, STATUS_CANCELLED, Some(now), Some(generation))
                    .await?
                {
                    return Ok(false);
                }
                Ok(true)
            }
        }
    }

    /// Verbose requester cancel of one row with v2's messages.
    async fn requester_cancel_verbose(
        &self,
        kind: RequestKind,
        mbid: &str,
        record: &RequestRecord,
        user_id: &str,
        now: u64,
    ) -> Result<ActionResponse, RequestsError> {
        let decision = self
            .store
            .prepare_requester_cancel(kind, mbid, user_id, now)
            .await?;
        match decision {
            None => Ok(ActionResponse {
                success: false,
                message: "Request not found".to_owned(),
            }),
            Some(super::ledger::CancelDecision::Denied { prior_status }) => {
                if CANCELLABLE_STATUSES.contains(&prior_status.as_str())
                    || prior_status == STATUS_AWAITING_APPROVAL
                {
                    return Err(RequestsError::Forbidden {
                        message: "Cannot cancel another user's request".to_owned(),
                    });
                }
                Ok(ActionResponse {
                    success: false,
                    message: format!("Cannot cancel request with status '{prior_status}'"),
                })
            }
            Some(super::ledger::CancelDecision::Detached) => Ok(ActionResponse {
                success: true,
                message: "Removed from your requests. The shared server request continues for another listener."
                    .to_owned(),
            }),
            Some(super::ledger::CancelDecision::CancelledDirect) => Ok(ActionResponse {
                success: true,
                message: format!("Cancelled request for {}", record_title(record)),
            }),
            Some(super::ledger::CancelDecision::CancelTask {
                prior_status,
                task_id,
                generation,
                ..
            }) => {
                if let Some(task_id) = task_id.as_deref()
                    && !cancel_linked(self.dispatch.as_ref(), task_id).await
                {
                    self.store
                        .restore_status(kind, mbid, &prior_status, STATUS_CANCELLING, generation)
                        .await?;
                    return Ok(ActionResponse {
                        success: false,
                        message: "Could not stop the download; try again".to_owned(),
                    });
                }
                if !self.store.update_status(
                    kind,
                    mbid,
                    STATUS_CANCELLED,
                    Some(now),
                    Some(generation),
                ).await? {
                    self.store.restore_status(
                        kind,
                        mbid,
                        &prior_status,
                        STATUS_CANCELLING,
                        generation,
                    ).await?;
                    return Ok(ActionResponse {
                        success: false,
                        message: "Request changed while cancelling".to_owned(),
                    });
                }
                Ok(ActionResponse {
                    success: true,
                    message: format!("Cancelled download of {}", record_title(record)),
                })
            }
        }
    }

    /// Rows plus their linked tasks' progress, in order.
    async fn to_items(&self, records: &[RequestRecord]) -> Vec<RequestItem> {
        let mut items = Vec::with_capacity(records.len());
        for record in records {
            items.push(self.to_item(record).await);
        }
        items
    }

    /// One row plus its linked task's progress snapshot. Rows without a
    /// task (waiting for approval) read exactly as the record maps; a task
    /// read failure leaves the progress fields empty and logs why.
    async fn to_item(&self, record: &RequestRecord) -> RequestItem {
        let mut item = RequestItem::from(record);
        let Some(task_id) = record.task_id.as_deref() else {
            return item;
        };
        match self.dispatch.task_progress(task_id).await {
            Ok(Some(snapshot)) => {
                item.progress = Some(snapshot.progress_percent as f64);
                item.size = snapshot.total_size_bytes.map(|total| total as f64);
                item.size_remaining = snapshot
                    .total_size_bytes
                    .map(|total| (total - snapshot.downloaded_bytes).max(0) as f64);
                item.error_message = snapshot.error_message;
                item.quality = snapshot.quality;
                item.protocol = Some(snapshot.protocol);
            }
            Ok(None) => {}
            Err(error) => tracing::warn!(task_id, ?error, "task progress read failed"),
        }
        // v2 history quirk: only failed rows offer the admin reimport, and
        // only when the task still has its candidate linked.
        if record.status == STATUS_FAILED {
            match self.dispatch.reimportable(task_id).await {
                Ok(reimportable) => item.can_reimport = Some(reimportable),
                Err(error) => tracing::warn!(task_id, ?error, "reimport check failed"),
            }
        }
        item
    }

    /// Reconcile one live row against its linked task. Waiting rows have no
    /// task and never move here; taskless dispatch rows keep their status
    /// (v2 falls back to library presence, which intake cannot see).
    async fn reconcile_one(&self, record: &RequestRecord, now: u64) -> Result<bool, RequestsError> {
        let Some(task_id) = record.task_id.as_deref() else {
            return Ok(false);
        };
        let state = self
            .dispatch
            .task_state(task_id)
            .await
            .map_err(dispatch_fault)?;
        let mapped = match state {
            DispatchTaskState::Active => {
                if record.status == STATUS_PENDING || record.status == STATUS_QUEUED {
                    Some(STATUS_DOWNLOADING)
                } else {
                    None
                }
            }
            DispatchTaskState::Imported => Some(STATUS_IMPORTED),
            DispatchTaskState::Incomplete => Some(STATUS_INCOMPLETE),
            DispatchTaskState::Failed => Some(STATUS_FAILED),
            DispatchTaskState::Cancelled => Some(STATUS_CANCELLED),
            DispatchTaskState::Missing => None,
        };
        let Some(next) = mapped else {
            return Ok(false);
        };
        if next == record.status {
            return Ok(false);
        }
        let completed =
            matches!(next, STATUS_IMPORTED | STATUS_FAILED | STATUS_CANCELLED).then_some(now);
        self.store
            .update_status(
                record.kind,
                &record.key,
                next,
                completed,
                Some(record.generation),
            )
            .await
    }
}

/// A dispatch read failure as a server fault (the cause goes to the log).
fn dispatch_fault(error: DispatchError) -> RequestsError {
    match error {
        DispatchError::Validation(message) | DispatchError::Failed(message) => {
            RequestsError::internal(&format_args!("dispatch read failed: {message}"))
        }
    }
}

/// Cancel one linked task, logging a failure. Callers that must not leave
/// a row cancelled over a live transfer check the result.
async fn cancel_linked(dispatch: &dyn DownloadDispatch, task_id: &str) -> bool {
    match dispatch.cancel_task(task_id).await {
        Ok(()) => true,
        Err(error) => {
            tracing::warn!(task_id, ?error, "task cancel failed");
            false
        }
    }
}

/// Reviewer stamp for approval decisions: acting id plus display name.
fn reviewer(principal: &Principal) -> (&str, Option<String>) {
    (principal.user_id.as_str(), Some(principal.display_name()))
}

/// Map a guarded watch change to the action reply (v2 `_owned_watch`).
fn watch_action(change: WatchChange) -> Result<WantedActionResponse, RequestsError> {
    match change {
        WatchChange::Done(watch) => Ok(WantedActionResponse {
            success: true,
            state: watch.state,
        }),
        WatchChange::NotFound => Err(RequestsError::NotFound),
        WatchChange::Fulfilled => Err(RequestsError::InvalidInput {
            message: "This watch is already fulfilled - re-request the album to watch it again"
                .to_owned(),
        }),
    }
}

/// What startup recovery did to the request ledger.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RequestRecovery {
    /// Interrupted cancels finished.
    pub cancelled: usize,
    /// Rows relinked to the task created for them.
    pub relinked: usize,
    /// Rows dispatched again under their original key.
    pub redispatched: usize,
}

/// Initial status for one role's fresh ask.
fn status_for_role(needs_approval: bool) -> &'static str {
    if needs_approval {
        STATUS_AWAITING_APPROVAL
    } else {
        STATUS_PENDING
    }
}

/// Missing-names rejection. Intake has no catalog lookup, so names are
/// required up front; v2 fills them from the album service instead.
fn missing_names(mbid: &str) -> RequestsError {
    RequestsError::InvalidInput {
        message: format!(
            "Could not resolve artist and album for MBID {mbid}: artist and album are required"
        ),
    }
}

/// Duplicate-track reply mirroring the winner (v2 `request_track`).
fn track_duplicate(existing: &RequestRecord) -> TrackIntakeResponse {
    let status = if existing.status == STATUS_AWAITING_APPROVAL {
        STATUS_AWAITING_APPROVAL
    } else {
        STATUS_QUEUED
    };
    TrackIntakeResponse {
        status: status.to_owned(),
        task_id: existing.task_id.clone(),
    }
}

/// Display title for approval messages: the track title for exact-track
/// rows, else the album title (v2 `_record_title`).
fn record_title(record: &RequestRecord) -> String {
    if record.kind == RequestKind::Track
        && let Some(title) = record.track_title.as_deref()
    {
        return title.to_owned();
    }
    record.album_title.clone()
}

/// Build one stored row's dispatch call without widening exact-track
/// identity (v2 `_dispatch_record`).
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
    DispatchRequest {
        user_id: owner_id.to_owned(),
        kind: record.kind.as_str().to_owned(),
        key: record.key.clone(),
        artist_name: record.artist_name.clone(),
        title,
        origin,
        release_mbid: record.release_mbid.clone(),
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

/// Not-waiting rejection for approve/reject.
fn not_waiting(status: &str) -> ActionResponse {
    ActionResponse {
        success: false,
        message: format!("Request is not awaiting approval (status: {status})"),
    }
}

/// No-match reply for approval queues.
fn no_match() -> ActionResponse {
    ActionResponse {
        success: false,
        message: "No matching approval found".to_owned(),
    }
}

impl From<&RequestRecord> for RequestItem {
    fn from(record: &RequestRecord) -> Self {
        Self {
            musicbrainz_id: record.key.clone(),
            artist_name: record.artist_name.clone(),
            album_title: record.album_title.clone(),
            artist_mbid: record.artist_mbid.clone(),
            year: record.year,
            cover_url: None,
            requested_at: record.requested_at,
            completed_at: record.completed_at,
            status: record.status.clone(),
            progress: None,
            eta: None,
            size: None,
            size_remaining: None,
            status_messages: None,
            error_message: None,
            quality: None,
            protocol: None,
            user_id: record.user_id.clone(),
            requested_by_name: record.requested_by_name.clone(),
            reviewed_by_name: record.reviewed_by_name.clone(),
            reviewed_at: record.reviewed_at,
            in_library: None,
            task_id: record.task_id.clone(),
            can_reimport: None,
            request_kind: record.kind.as_str().to_owned(),
            track_title: record.track_title.clone(),
            duration_seconds: record.duration_seconds,
            track_release_group_mbid: record.track_release_group_mbid.clone(),
            requester_count: record.requesters.len() as u32,
        }
    }
}
