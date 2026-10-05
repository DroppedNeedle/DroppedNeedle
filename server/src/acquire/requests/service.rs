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
use super::bridges::{FollowDecisionSink, WatchView};
use super::dispatch::{
    DispatchError, DispatchOrigin, DispatchOutcome, DispatchRequest, DispatchTaskState,
};
use super::error::RequestsError;
use super::ledger::{
    BeginOutcome, CANCELLABLE_STATUSES, CLEARABLE_STATUSES, EditionMark, EditionStore,
    FollowApprovalStore, PersonalMixStore, RETRYABLE_STATUSES, RequestRecord, RequestStore,
    STATUS_AWAITING_APPROVAL, STATUS_CANCELLED, STATUS_CANCELLING, STATUS_DOWNLOADING,
    STATUS_FAILED, STATUS_IMPORTED, STATUS_INCOMPLETE, STATUS_PENDING, STATUS_QUEUED, WantedStore,
    is_active, meaningful_name, now_epoch, validate_mbid,
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
use super::{RequestsState, dispatch::DownloadDispatch};

/// Batch row cap (v2 `BatchAlbumRequest` schema `max_length=500`).
const BATCH_MAX_ITEMS: usize = 500;

/// History page-size cap (v2 `page_size` query `le=100`).
const HISTORY_MAX_PAGE_SIZE: u32 = 100;

/// Domain service over the requests state. Every method is sync; handlers wrap
/// the calls without holding anything across awaits.
pub struct RequestsService {
    /// Request ledger.
    store: Arc<RequestStore>,
    /// Quota ledger.
    quota: Arc<QuotaLedger>,
    /// Download-dispatch seam.
    dispatch: Arc<dyn DownloadDispatch>,
    /// Wanted rows.
    wanted: Arc<WantedStore>,
    /// Auto-download approvals.
    follows: Arc<FollowApprovalStore>,
    /// Personal-mix approvals and refresh guard.
    mixes: Arc<PersonalMixStore>,
    /// In-flight edition acquires.
    editions: Arc<EditionStore>,
    /// Verdict sink into the collections follow rows, when wired.
    follow_sink: Option<Arc<dyn FollowDecisionSink>>,
    /// Watches owned by the flows watcher loop, when wired.
    watch_view: Option<Arc<dyn WatchView>>,
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
            follow_sink: state.follow_sink.clone(),
            watch_view: state.watch_view.clone(),
        }
    }

    /// Ask for one album. Winners record and maybe dispatch; duplicate asks
    /// attach to the live row and report its status (v2 `request_album`).
    pub fn request_album(
        &self,
        principal: &Principal,
        body: &AlbumIntake,
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
        if let Some(existing) = self.store.get(RequestKind::Album, &mbid)?
            && (is_active(&existing.status) || existing.status == STATUS_CANCELLING)
        {
            return self.attach_album(principal, &existing, body);
        }

        self.quota
            .check_request_quota(&principal.user_id, role, 1, now)?;
        self.quota
            .check_storage_admission(&principal.user_id, role, DispatchOrigin::User)?;

        let needs_approval = !role.auto_approves();
        let record = self.new_album_record(
            principal,
            &mbid,
            body,
            &artist_name,
            &album_title,
            needs_approval,
            now,
        );
        match self.store.begin(record)? {
            BeginOutcome::Existing(winner) if is_active(&winner.status) => {
                self.attach_album(principal, &winner, body)
            }
            BeginOutcome::Existing(winner) if winner.status == STATUS_CANCELLING => {
                self.attach_album(principal, &winner, body)
            }
            BeginOutcome::Existing(_) => Ok(IntakeResponse {
                success: false,
                message: "Request could not be recorded".to_owned(),
                musicbrainz_id: mbid,
                status: STATUS_FAILED.to_owned(),
                task_id: None,
            }),
            BeginOutcome::Won(won) => {
                self.quota.record_ask(&principal.user_id, now)?;
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
            }
        }
    }

    /// Ask for one exact recording (v2 `request_track`).
    pub fn request_track(
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

        if let Some(existing) = self.store.get(RequestKind::Track, &mbid)?
            && (is_active(&existing.status) || existing.status == STATUS_CANCELLING)
        {
            self.store.attach_requester(
                RequestKind::Track,
                &mbid,
                &principal.user_id,
                Some(principal.display_name()),
            )?;
            return Ok(track_duplicate(&existing));
        }

        self.quota
            .check_request_quota(&principal.user_id, role, 1, now)?;
        self.quota
            .check_storage_admission(&principal.user_id, role, DispatchOrigin::User)?;

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
            dismissed_by: std::collections::HashSet::new(),
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
        match self.store.begin(record)? {
            BeginOutcome::Existing(winner)
                if is_active(&winner.status) || winner.status == STATUS_CANCELLING =>
            {
                self.store.attach_requester(
                    RequestKind::Track,
                    &mbid,
                    &principal.user_id,
                    Some(principal.display_name()),
                )?;
                Ok(track_duplicate(&winner))
            }
            BeginOutcome::Existing(_) => Ok(TrackIntakeResponse {
                status: STATUS_QUEUED.to_owned(),
                task_id: None,
            }),
            BeginOutcome::Won(won) => {
                self.quota.record_ask(&principal.user_id, now)?;
                if needs_approval {
                    return Ok(TrackIntakeResponse {
                        status: STATUS_AWAITING_APPROVAL.to_owned(),
                        task_id: None,
                    });
                }
                self.dispatch_track(&won, DispatchOrigin::User)
            }
        }
    }

    /// Ask for a batch of albums (v2 `request_batch`): raw dupes and
    /// canonical dupes both skip, live rows attach, unresolvable rows skip,
    /// and each created row dispatches on its own (one failure never fails
    /// the batch).
    pub fn request_batch(
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
        let live = self.store.active_mbids(RequestKind::Album)?;
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
                self.store.attach_requester(
                    RequestKind::Album,
                    mbid,
                    &principal.user_id,
                    Some(principal.display_name()),
                )?;
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

        self.quota
            .check_request_quota(&principal.user_id, role, new_items.len() as u32, now)?;
        self.quota
            .check_storage_admission(&principal.user_id, role, DispatchOrigin::User)?;

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

        let mut created = Vec::new();
        let mut raced = Vec::new();
        for (mbid, item, artist_name, album_title) in &resolvable {
            let record = RequestRecord {
                key: mbid.to_lowercase(),
                kind: RequestKind::Album,
                status: status_for_role(needs_approval).to_owned(),
                artist_name: artist_name.clone(),
                album_title: album_title.clone(),
                artist_mbid: item.artist_mbid.clone(),
                year: item.year,
                release_mbid: None,
                track_title: None,
                duration_seconds: None,
                track_release_group_mbid: None,
                user_id: Some(principal.user_id.clone()),
                requested_by_name: Some(principal.display_name()),
                requesters: Vec::new(),
                dismissed_by: std::collections::HashSet::new(),
                requested_at: now,
                completed_at: None,
                task_id: None,
                generation: 0,
                dispatch_authorized: !needs_approval,
                monitor_artist: body.monitor_artist,
                auto_download_artist: body.auto_download_artist,
                reviewed_by_name: None,
                reviewed_at: None,
            };
            match self.store.begin(record)? {
                BeginOutcome::Won(won) => {
                    self.quota.record_ask(&principal.user_id, now)?;
                    created.push(won);
                }
                BeginOutcome::Existing(winner)
                    if is_active(&winner.status) || winner.status == STATUS_CANCELLING =>
                {
                    // Won by someone else between the live read and the
                    // bulk claim: a listener attachment, never a dispatch
                    // candidate (v2 raced-items quirk).
                    raced.push(mbid.clone());
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
            self.store.attach_requester(
                RequestKind::Album,
                mbid,
                &principal.user_id,
                Some(principal.display_name()),
            )?;
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
            match self.dispatch_album(won, DispatchOrigin::User, &principal.user_id) {
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
    pub fn cancel_batch(
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
                self.admin_cancel_one(kind, &mbid, now)?
            } else {
                self.requester_cancel_one(kind, &mbid, &principal.user_id, now)?
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
    pub fn approve_request(
        &self,
        principal: &Principal,
        mbid: &str,
        kind: RequestKind,
    ) -> Result<ActionResponse, RequestsError> {
        principal.require_admin()?;
        let mbid = validate_mbid(mbid)?;
        let now = now_epoch();
        let Some(record) = self.store.get(kind, &mbid)? else {
            return Ok(ActionResponse {
                success: false,
                message: "Request not found".to_owned(),
            });
        };
        if record.status != STATUS_AWAITING_APPROVAL {
            return Ok(not_waiting(&record.status));
        }
        let claimed = self.store.claim_approval(
            kind,
            &mbid,
            Some(principal.display_name()),
            now,
            record.generation,
        )?;
        let Some(claimed) = claimed else {
            let status = self
                .store
                .get(kind, &mbid)?
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
        match self.dispatch.dispatch(&request) {
            Err(DispatchError::Validation(message)) => {
                self.store.restore_status(
                    kind,
                    &mbid,
                    STATUS_AWAITING_APPROVAL,
                    STATUS_PENDING,
                    claimed.generation,
                )?;
                Ok(ActionResponse {
                    success: false,
                    message,
                })
            }
            Err(DispatchError::Failed(cause)) => {
                tracing::error!(%cause, "approved dispatch failed");
                self.store.update_status(
                    kind,
                    &mbid,
                    STATUS_FAILED,
                    Some(now),
                    claimed.generation,
                )?;
                Ok(ActionResponse {
                    success: false,
                    message: format!("Approved but failed to start: {}", record_title(&record)),
                })
            }
            Ok(DispatchOutcome::AlreadyInLibrary) => {
                self.store.update_status(
                    kind,
                    &mbid,
                    STATUS_IMPORTED,
                    Some(now),
                    claimed.generation,
                )?;
                Ok(ActionResponse {
                    success: true,
                    message: format!("Approved: {}", record_title(&record)),
                })
            }
            Ok(DispatchOutcome::Dispatched { task_id }) => {
                if !self
                    .store
                    .link_task(kind, &mbid, &task_id, claimed.generation)?
                {
                    self.dispatch.cancel_task(&task_id);
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
    pub fn reject_request(
        &self,
        principal: &Principal,
        mbid: &str,
        kind: RequestKind,
    ) -> Result<ActionResponse, RequestsError> {
        principal.require_admin()?;
        let mbid = validate_mbid(mbid)?;
        let now = now_epoch();
        let Some(record) = self.store.get(kind, &mbid)? else {
            return Ok(ActionResponse {
                success: false,
                message: "Request not found".to_owned(),
            });
        };
        if record.status != STATUS_AWAITING_APPROVAL {
            return Ok(not_waiting(&record.status));
        }
        let claimed = self.store.claim_rejection(
            kind,
            &mbid,
            Some(principal.display_name()),
            now,
            now,
            record.generation,
        )?;
        if claimed.is_none() {
            let status = self
                .store
                .get(kind, &mbid)?
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
    pub fn cancel_one(
        &self,
        principal: &Principal,
        mbid: &str,
        kind: RequestKind,
    ) -> Result<ActionResponse, RequestsError> {
        let mbid = validate_mbid(mbid)?;
        let now = now_epoch();
        let Some(record) = self.store.get(kind, &mbid)? else {
            return Ok(ActionResponse {
                success: false,
                message: "Request not found".to_owned(),
            });
        };
        if !principal.role.is_admin() {
            return self.requester_cancel_verbose(kind, &mbid, &record, &principal.user_id, now);
        }
        if record.status == STATUS_AWAITING_APPROVAL {
            if !self.store.update_status(
                kind,
                &mbid,
                STATUS_CANCELLED,
                Some(now),
                record.generation,
            )? {
                return Ok(ActionResponse {
                    success: false,
                    message: "Request changed while cancelling".to_owned(),
                });
            }
            // Approval cancellation revokes the persisted capability after
            // winning the generation CAS (v2 quirk).
            self.store.set_dispatch_authorized(kind, &mbid, false)?;
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
        if let Some(task_id) = record.task_id.as_deref() {
            self.dispatch.cancel_task(task_id);
        }
        if !self
            .store
            .update_status(kind, &mbid, STATUS_CANCELLED, Some(now), record.generation)?
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
    pub fn retry_request(
        &self,
        principal: &Principal,
        mbid: &str,
        kind: RequestKind,
    ) -> Result<ActionResponse, RequestsError> {
        let mbid = validate_mbid(mbid)?;
        let now = now_epoch();
        let Some(record) = self.store.get(kind, &mbid)? else {
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
        let claim = self.store.claim_retry(
            kind,
            &mbid,
            &principal.user_id,
            target,
            can_dispatch,
            !principal.role.is_admin(),
            now,
            record.generation,
        )?;
        let generation = match claim {
            super::ledger::RetryClaim::Claimed { generation, .. } => generation,
            super::ledger::RetryClaim::Lost => {
                let current = self.store.get(kind, &mbid)?;
                let current_status = current.as_ref().map(|row| row.status.as_str());
                if !principal.role.is_admin()
                    && current_status.is_some_and(|s| RETRYABLE_STATUSES.contains(&s))
                    && !self.store.is_requester(kind, &mbid, &principal.user_id)?
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
        match self.dispatch.dispatch(&request) {
            Err(DispatchError::Validation(message)) => {
                self.store
                    .update_status(kind, &mbid, &prior_status, None, generation)?;
                Ok(ActionResponse {
                    success: false,
                    message,
                })
            }
            Err(DispatchError::Failed(cause)) => {
                tracing::error!(%cause, "retry dispatch failed");
                self.store
                    .update_status(kind, &mbid, &prior_status, None, generation)?;
                Ok(ActionResponse {
                    success: false,
                    message: "Retry failed to start download".to_owned(),
                })
            }
            Ok(DispatchOutcome::AlreadyInLibrary) => {
                self.store
                    .update_status(kind, &mbid, STATUS_IMPORTED, Some(now), generation)?;
                Ok(ActionResponse {
                    success: true,
                    message: format!("Re-requested {}", record_title(&record)),
                })
            }
            Ok(DispatchOutcome::Dispatched { task_id }) => {
                if !self.store.link_task(kind, &mbid, &task_id, generation)? {
                    self.dispatch.cancel_task(&task_id);
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
    pub fn clear_history_item(
        &self,
        principal: &Principal,
        mbid: &str,
        kind: RequestKind,
    ) -> Result<ClearHistoryResponse, RequestsError> {
        let mbid = validate_mbid(mbid)?;
        let Some(record) = self.store.get(kind, &mbid)? else {
            return Ok(ClearHistoryResponse { success: false });
        };
        if !principal.role.is_admin()
            && !self.store.is_requester(kind, &mbid, &principal.user_id)?
        {
            return Err(RequestsError::Forbidden {
                message: "Cannot clear another user's request".to_owned(),
            });
        }
        if !CLEARABLE_STATUSES.contains(&record.status.as_str()) {
            return Ok(ClearHistoryResponse { success: false });
        }
        let cleared = if principal.role.is_admin() {
            self.store.delete(kind, &mbid)?
        } else {
            self.store.dismiss(kind, &mbid, &principal.user_id)?
        };
        Ok(ClearHistoryResponse { success: cleared })
    }

    /// Live rows. Admins see every row; anyone else sees their own (v2
    /// `user_id=None`-for-admin quirk). Rows reconcile against their linked
    /// tasks on the way out (v2 checks completion inside the read).
    pub fn active(&self, principal: &Principal) -> Result<ActiveRequestsResponse, RequestsError> {
        self.sync_request_statuses()?;
        let owner = (!principal.role.is_admin()).then_some(principal.user_id.as_str());
        let rows = self.store.active(owner, None)?;
        let items = rows.iter().map(|row| self.to_item(row)).collect::<Vec<_>>();
        Ok(ActiveRequestsResponse {
            count: items.len() as u32,
            items,
        })
    }

    /// Live-row count for badges.
    pub fn active_count(
        &self,
        principal: &Principal,
    ) -> Result<ActiveCountResponse, RequestsError> {
        self.sync_request_statuses()?;
        let owner = (!principal.role.is_admin()).then_some(principal.user_id.as_str());
        Ok(ActiveCountResponse {
            count: self.store.active(owner, None)?.len() as u32,
        })
    }

    /// Paged history. Admins see every row; anyone else sees their own minus
    /// dismissed rows.
    pub fn history(
        &self,
        principal: &Principal,
        query: &HistoryQuery,
    ) -> Result<HistoryResponse, RequestsError> {
        self.sync_request_statuses()?;
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
        let rows = self
            .store
            .history(owner, kind, query.status.as_deref(), sort)?;
        let total = rows.len() as u32;
        let start = (page.saturating_sub(1)).saturating_mul(page_size) as usize;
        let items = rows
            .iter()
            .skip(start)
            .take(page_size as usize)
            .map(|row| self.to_item(row))
            .collect::<Vec<_>>();
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
    pub fn pending_approvals(
        &self,
        principal: &Principal,
    ) -> Result<ActiveRequestsResponse, RequestsError> {
        principal.require_admin()?;
        let rows = self.store.pending_approvals(None)?;
        let items = rows.iter().map(|row| self.to_item(row)).collect::<Vec<_>>();
        Ok(ActiveRequestsResponse {
            count: items.len() as u32,
            items,
        })
    }

    /// Pending-review badge count: album/track rows plus auto-download units
    /// plus personal-mix rows (v2 sums all three sources).
    pub fn pending_approval_count(
        &self,
        principal: &Principal,
    ) -> Result<ActiveCountResponse, RequestsError> {
        principal.require_admin()?;
        let asks = self.store.pending_approvals(None)?.len() as u32;
        let follows = self.follows.pending_units()?;
        let mixes = self.mixes.pending_count()?;
        Ok(ActiveCountResponse {
            count: asks.saturating_add(follows).saturating_add(mixes),
        })
    }

    /// Wanted watches plus the still-retrying set. Admins see every row.
    pub fn wanted(&self, principal: &Principal) -> Result<WantedResponse, RequestsError> {
        let is_admin = principal.role.is_admin();
        let watches = self.wanted.watches_for(&principal.user_id, is_admin)?;
        let retrying = self.wanted.retrying_for(&principal.user_id, is_admin)?;
        let mut items = watches
            .iter()
            .map(|watch| WantedItem {
                musicbrainz_id: watch.key.clone(),
                artist_name: watch.artist_name.clone(),
                album_title: watch.album_title.clone(),
                kind: watch.kind.clone(),
                state: watch.state.clone(),
                check_count: watch.check_count,
                next_check_at: watch.next_check_at,
                new_candidate_count: watch.new_candidate_count,
                created_at: watch.created_at,
                artist_mbid: watch.artist_mbid.clone(),
                year: watch.year,
                cover_url: watch.cover_url.clone(),
                user_id: is_admin.then(|| watch.user_id.clone()),
                user_name: if is_admin {
                    watch.user_name.clone()
                } else {
                    None
                },
            })
            .collect::<Vec<_>>();
        // Watches the flows loop owns render beside the requests rows.
        // Loop watches have no local check counts, so they read zero; the
        // loop tracks album-level availability, so they read `missing`.
        if let Some(view) = &self.watch_view {
            for watch in view.watching() {
                if !is_admin && watch.user_id != principal.user_id {
                    continue;
                }
                if items.iter().any(|item| item.musicbrainz_id == watch.key) {
                    continue;
                }
                items.push(WantedItem {
                    musicbrainz_id: watch.key,
                    artist_name: watch.artist_name,
                    album_title: watch.album_title,
                    kind: "missing".to_owned(),
                    state: "watching".to_owned(),
                    check_count: 0,
                    next_check_at: Some(watch.next_check_at),
                    new_candidate_count: 0,
                    created_at: watch.created_at,
                    artist_mbid: None,
                    year: None,
                    cover_url: None,
                    user_id: is_admin.then_some(watch.user_id),
                    user_name: None,
                });
            }
            items.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        }
        Ok(WantedResponse {
            count: items.len() as u32,
            items,
            retrying: retrying
                .iter()
                .map(|entry| WantedRetryingItem {
                    musicbrainz_id: entry.key.clone(),
                    artist_name: entry.artist_name.clone(),
                    album_title: entry.album_title.clone(),
                    retry_count: entry.retry_count,
                    max_attempts: entry.max_attempts,
                    next_retry_at: entry.next_retry_at,
                    artist_mbid: entry.artist_mbid.clone(),
                    year: entry.year,
                    cover_url: entry.cover_url.clone(),
                    user_id: is_admin.then(|| entry.user_id.clone()),
                    user_name: if is_admin {
                        entry.user_name.clone()
                    } else {
                        None
                    },
                })
                .collect(),
        })
    }

    /// Pause one wanted watch.
    pub fn wanted_stop(
        &self,
        principal: &Principal,
        mbid: &str,
    ) -> Result<WantedActionResponse, RequestsError> {
        let mbid = validate_mbid(mbid)?;
        let watch = self
            .wanted
            .stop(&mbid, &principal.user_id, principal.role.is_admin())?
            .ok_or(RequestsError::NotFound)?;
        Ok(WantedActionResponse {
            success: true,
            state: watch.state,
        })
    }

    /// Resume one wanted watch.
    pub fn wanted_resume(
        &self,
        principal: &Principal,
        mbid: &str,
    ) -> Result<WantedActionResponse, RequestsError> {
        let mbid = validate_mbid(mbid)?;
        let watch = self
            .wanted
            .resume(&mbid, &principal.user_id, principal.role.is_admin())?
            .ok_or(RequestsError::NotFound)?;
        Ok(WantedActionResponse {
            success: true,
            state: watch.state,
        })
    }

    /// Clear one watch's unseen candidates.
    pub fn wanted_seen(
        &self,
        principal: &Principal,
        mbid: &str,
    ) -> Result<WantedActionResponse, RequestsError> {
        let mbid = validate_mbid(mbid)?;
        let watch = self
            .wanted
            .mark_seen(&mbid, &principal.user_id, principal.role.is_admin())?
            .ok_or(RequestsError::NotFound)?;
        Ok(WantedActionResponse {
            success: true,
            state: watch.state,
        })
    }

    /// Pending auto-download approvals (admin).
    pub fn auto_download_approvals(
        &self,
        principal: &Principal,
    ) -> Result<AutoDownloadApprovalsResponse, RequestsError> {
        principal.require_admin()?;
        let items = self
            .follows
            .pending()?
            .iter()
            .map(|row| AutoDownloadApprovalItem {
                user_id: row.user_id.clone(),
                user_name: row.user_name.clone(),
                artist_mbid: row.artist_mbid.clone(),
                artist_name: row.artist_name.clone(),
                requested_at: row.requested_at,
            })
            .collect::<Vec<_>>();
        Ok(AutoDownloadApprovalsResponse {
            count: items.len() as u32,
            items,
        })
    }

    /// Approve one auto-download grant (admin).
    pub fn approve_auto_download(
        &self,
        principal: &Principal,
        user_id: &str,
        artist_mbid: &str,
    ) -> Result<ActionResponse, RequestsError> {
        principal.require_admin()?;
        validate_mbid(artist_mbid)?;
        let names = self
            .follows
            .pending()?
            .into_iter()
            .find(|row| row.user_id == user_id && row.artist_mbid == artist_mbid.to_lowercase())
            .map(|row| (row.user_name, row.artist_name));
        if !self.follows.decide(user_id, artist_mbid, "approved")? {
            return Ok(no_match());
        }
        if let (Some(sink), Some((user_name, artist_name))) = (&self.follow_sink, names) {
            sink.arm_auto_download(user_id, &user_name, artist_mbid, &artist_name);
        }
        Ok(ActionResponse {
            success: true,
            message: "Auto-download approved".to_owned(),
        })
    }

    /// Reject one auto-download ask, keeping the follow (admin; as v2 does).
    pub fn reject_auto_download(
        &self,
        principal: &Principal,
        user_id: &str,
        artist_mbid: &str,
    ) -> Result<ActionResponse, RequestsError> {
        principal.require_admin()?;
        validate_mbid(artist_mbid)?;
        if !self.follows.decide(user_id, artist_mbid, "rejected")? {
            return Ok(no_match());
        }
        if let Some(sink) = &self.follow_sink {
            sink.clear_auto_download(user_id, artist_mbid);
        }
        Ok(ActionResponse {
            success: true,
            message: "Auto-download rejected".to_owned(),
        })
    }

    /// Revoke one auto-download grant, keeping the follow (admin; as v2 does).
    pub fn revoke_auto_download(
        &self,
        principal: &Principal,
        user_id: &str,
        artist_mbid: &str,
    ) -> Result<ActionResponse, RequestsError> {
        principal.require_admin()?;
        validate_mbid(artist_mbid)?;
        if !self.follows.revoke(user_id, artist_mbid)? {
            return Ok(no_match());
        }
        if let Some(sink) = &self.follow_sink {
            sink.clear_auto_download(user_id, artist_mbid);
        }
        Ok(ActionResponse {
            success: true,
            message: "Auto-download revoked".to_owned(),
        })
    }

    /// Pending bulk-approval batches (admin).
    pub fn auto_download_batches(
        &self,
        principal: &Principal,
    ) -> Result<ApprovalBatchListResponse, RequestsError> {
        principal.require_admin()?;
        let batches = self
            .follows
            .pending_batches()?
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
    pub fn approve_auto_download_batch(
        &self,
        principal: &Principal,
        batch_id: &str,
    ) -> Result<ActionResponse, RequestsError> {
        principal.require_admin()?;
        let batch = self
            .follows
            .pending_batches()?
            .into_iter()
            .find(|batch| batch.batch_id == batch_id);
        let affected = self.follows.decide_batch(batch_id, "approved")?;
        if affected == 0 {
            return Ok(ActionResponse {
                success: false,
                message: "No matching batch found".to_owned(),
            });
        }
        if let (Some(sink), Some(batch)) = (&self.follow_sink, batch) {
            for (artist_mbid, artist_name) in &batch.artists {
                sink.arm_auto_download(&batch.user_id, &batch.user_name, artist_mbid, artist_name);
            }
        }
        Ok(ActionResponse {
            success: true,
            message: format!("Auto-download approved for {affected} artists"),
        })
    }

    /// Reject one bulk batch (admin).
    pub fn reject_auto_download_batch(
        &self,
        principal: &Principal,
        batch_id: &str,
    ) -> Result<ActionResponse, RequestsError> {
        principal.require_admin()?;
        let batch = self
            .follows
            .pending_batches()?
            .into_iter()
            .find(|batch| batch.batch_id == batch_id);
        let affected = self.follows.decide_batch(batch_id, "rejected")?;
        if affected == 0 {
            return Ok(ActionResponse {
                success: false,
                message: "No matching batch found".to_owned(),
            });
        }
        if let (Some(sink), Some(batch)) = (&self.follow_sink, batch) {
            for (artist_mbid, _) in &batch.artists {
                sink.clear_auto_download(&batch.user_id, artist_mbid);
            }
        }
        Ok(ActionResponse {
            success: true,
            message: format!("Auto-download rejected for {affected} artists"),
        })
    }

    /// Pending personal-mix approvals (admin).
    pub fn mix_approvals(
        &self,
        principal: &Principal,
    ) -> Result<PersonalMixApprovalsResponse, RequestsError> {
        principal.require_admin()?;
        let items = self
            .mixes
            .pending()?
            .iter()
            .map(|row| PersonalMixApprovalItem {
                user_id: row.user_id.clone(),
                user_name: row.user_name.clone(),
                requested_at: row.requested_at,
            })
            .collect::<Vec<_>>();
        Ok(PersonalMixApprovalsResponse {
            count: items.len() as u32,
            items,
        })
    }

    /// Approve one mix auto-request grant (admin).
    pub fn approve_mix(
        &self,
        principal: &Principal,
        user_id: &str,
    ) -> Result<ActionResponse, RequestsError> {
        principal.require_admin()?;
        if !self.mixes.decide(user_id, "approved")? {
            return Ok(no_match());
        }
        Ok(ActionResponse {
            success: true,
            message: "Weekly Mix auto-request approved".to_owned(),
        })
    }

    /// Reject one mix auto-request ask (admin).
    pub fn reject_mix(
        &self,
        principal: &Principal,
        user_id: &str,
    ) -> Result<ActionResponse, RequestsError> {
        principal.require_admin()?;
        if !self.mixes.decide(user_id, "rejected")? {
            return Ok(no_match());
        }
        Ok(ActionResponse {
            success: true,
            message: "Weekly Mix auto-request rejected".to_owned(),
        })
    }

    /// Revoke one mix auto-request grant (admin).
    pub fn revoke_mix(
        &self,
        principal: &Principal,
        user_id: &str,
    ) -> Result<ActionResponse, RequestsError> {
        principal.require_admin()?;
        if !self.mixes.revoke(user_id)? {
            return Ok(no_match());
        }
        Ok(ActionResponse {
            success: true,
            message: "Weekly Mix auto-request revoked".to_owned(),
        })
    }

    /// Refresh one user's personal mix. The build runs behind a key the mix
    /// builder owns; while it runs, repeat calls answer
    /// `already_running` instead of stacking builds (v2
    /// `me_connections.refresh_personal_mix` quirk, including the
    /// lost-the-race reply).
    pub fn refresh_personal_mix(
        &self,
        principal: &Principal,
    ) -> Result<RefreshResponse, RequestsError> {
        if !self.mixes.is_linked(&principal.user_id)? {
            return Err(RequestsError::InvalidInput {
                message: "Connect ListenBrainz first to build Your Weekly Mix".to_owned(),
            });
        }
        if !self.mixes.refresh_start(&principal.user_id)? {
            return Ok(RefreshResponse {
                status: "already_running".to_owned(),
            });
        }
        Ok(RefreshResponse {
            status: "started".to_owned(),
        })
    }

    /// Fill the selected edition's missing tracks and upgrade its
    /// below-cutoff owned tracks (curator only, as in v2). Never retags
    /// existing files: the seam only fetches.
    pub fn acquire_edition(
        &self,
        principal: &Principal,
        album_id: &str,
    ) -> Result<EditionAcquireResponse, RequestsError> {
        principal.require_curator()?;
        let mbid = validate_mbid(album_id)?;
        let now = now_epoch();
        if let Some(mark) = self.editions.get(&mbid)? {
            match self.dispatch.task_state(&mark.task_id) {
                DispatchTaskState::Active => {
                    return Ok(EditionAcquireResponse {
                        status: "already_in_progress".to_owned(),
                        message: "Edition acquire already in progress".to_owned(),
                        task_id: Some(mark.task_id),
                    });
                }
                _ => {
                    self.editions.clear(&mbid)?;
                }
            }
        }
        self.quota.check_storage_admission(
            &principal.user_id,
            principal.role,
            DispatchOrigin::Edition,
        )?;
        let request = DispatchRequest {
            user_id: principal.user_id.clone(),
            kind: "edition".to_owned(),
            key: mbid.clone(),
            artist_name: String::new(),
            title: String::new(),
            origin: DispatchOrigin::Edition,
            release_mbid: None,
            // No stable per-decision key exists (marks clear on terminal,
            // so a key would pin re-acquires to the old task); the
            // in-progress mark above owns double-submit dedup.
            idempotency_key: None,
        };
        match self.dispatch.dispatch(&request) {
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
                self.editions.set(EditionMark {
                    key: mbid,
                    task_id: task_id.clone(),
                    started_at: now,
                })?;
                Ok(EditionAcquireResponse {
                    status: "started".to_owned(),
                    message: "Edition acquire started".to_owned(),
                    task_id: Some(task_id),
                })
            }
        }
    }

    /// Reconcile live rows with their linked tasks (v2
    /// `sync_request_statuses`). One bad row never stops the sweep.
    pub fn sync_request_statuses(&self) -> Result<SyncResponse, RequestsError> {
        let rows = self.store.active(None, None)?;
        let now = now_epoch();
        let mut reconciled = 0_u32;
        for record in rows {
            if self.reconcile_one(&record, now)? {
                reconciled = reconciled.saturating_add(1);
            }
        }
        Ok(SyncResponse { reconciled })
    }

    /// Attach a duplicate album ask to its live row and report the row's own
    /// status (v2: the duplicate reply mirrors the winner, including the
    /// awaiting-approval message).
    fn attach_album(
        &self,
        principal: &Principal,
        existing: &RequestRecord,
        body: &AlbumIntake,
    ) -> Result<IntakeResponse, RequestsError> {
        self.store.attach_requester(
            RequestKind::Album,
            &existing.key,
            &principal.user_id,
            Some(principal.display_name()),
        )?;
        if body.monitor_artist {
            self.store.widen_monitoring(
                RequestKind::Album,
                &existing.key,
                true,
                body.auto_download_artist,
            )?;
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

    /// Build a fresh album row for the claim.
    #[allow(clippy::too_many_arguments)]
    fn new_album_record(
        &self,
        principal: &Principal,
        mbid: &str,
        body: &AlbumIntake,
        artist_name: &str,
        album_title: &str,
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
            release_mbid: None,
            track_title: None,
            duration_seconds: None,
            track_release_group_mbid: None,
            user_id: Some(principal.user_id.clone()),
            requested_by_name: Some(principal.display_name()),
            requesters: Vec::new(),
            dismissed_by: std::collections::HashSet::new(),
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
    fn dispatch_album(
        &self,
        won: &RequestRecord,
        origin: DispatchOrigin,
        actor_id: &str,
    ) -> Result<IntakeResponse, RequestsError> {
        let owner = won.user_id.clone().unwrap_or_else(|| actor_id.to_owned());
        let request = stored_dispatch(won, origin, &owner);
        let now = now_epoch();
        match self.dispatch.dispatch(&request) {
            Err(DispatchError::Validation(message)) => {
                self.store.update_status(
                    RequestKind::Album,
                    &won.key,
                    STATUS_FAILED,
                    Some(now),
                    won.generation,
                )?;
                Err(RequestsError::InvalidInput { message })
            }
            Err(DispatchError::Failed(cause)) => {
                self.store.update_status(
                    RequestKind::Album,
                    &won.key,
                    STATUS_FAILED,
                    Some(now),
                    won.generation,
                )?;
                Err(RequestsError::internal(&format_args!(
                    "album dispatch failed: {cause}"
                )))
            }
            Ok(DispatchOutcome::AlreadyInLibrary) => {
                self.store.update_status(
                    RequestKind::Album,
                    &won.key,
                    STATUS_IMPORTED,
                    Some(now),
                    won.generation,
                )?;
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
                    .link_task(RequestKind::Album, &won.key, &task_id, won.generation)?
                {
                    self.dispatch.cancel_task(&task_id);
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
    fn dispatch_track(
        &self,
        won: &RequestRecord,
        origin: DispatchOrigin,
    ) -> Result<TrackIntakeResponse, RequestsError> {
        let owner = won.user_id.clone().unwrap_or_default();
        let request = stored_dispatch(won, origin, &owner);
        let now = now_epoch();
        match self.dispatch.dispatch(&request) {
            Err(DispatchError::Validation(message)) => {
                self.store.update_status(
                    RequestKind::Track,
                    &won.key,
                    STATUS_FAILED,
                    Some(now),
                    won.generation,
                )?;
                Err(RequestsError::InvalidInput { message })
            }
            Err(DispatchError::Failed(cause)) => {
                self.store.update_status(
                    RequestKind::Track,
                    &won.key,
                    STATUS_FAILED,
                    Some(now),
                    won.generation,
                )?;
                Err(RequestsError::internal(&format_args!(
                    "track dispatch failed: {cause}"
                )))
            }
            Ok(DispatchOutcome::AlreadyInLibrary) => {
                self.store.update_status(
                    RequestKind::Track,
                    &won.key,
                    STATUS_IMPORTED,
                    Some(now),
                    won.generation,
                )?;
                Ok(TrackIntakeResponse {
                    status: "already_in_library".to_owned(),
                    task_id: None,
                })
            }
            Ok(DispatchOutcome::Dispatched { task_id }) => {
                if !self
                    .store
                    .link_task(RequestKind::Track, &won.key, &task_id, won.generation)?
                {
                    self.dispatch.cancel_task(&task_id);
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
    fn admin_cancel_one(
        &self,
        kind: RequestKind,
        mbid: &str,
        now: u64,
    ) -> Result<bool, RequestsError> {
        let Some(record) = self.store.get(kind, mbid)? else {
            return Ok(false);
        };
        if !matches!(
            record.status.as_str(),
            STATUS_AWAITING_APPROVAL | STATUS_PENDING | STATUS_QUEUED | STATUS_DOWNLOADING
        ) {
            return Ok(false);
        }
        if let Some(task_id) = record.task_id.as_deref() {
            self.dispatch.cancel_task(task_id);
        }
        if !self
            .store
            .update_status(kind, mbid, STATUS_CANCELLED, Some(now), record.generation)?
        {
            return Ok(false);
        }
        if record.status == STATUS_AWAITING_APPROVAL {
            self.store.set_dispatch_authorized(kind, mbid, false)?;
        }
        Ok(true)
    }

    /// Quiet requester cancel of one row for batch-cancel. The task
    /// cancels under the immutable primary owner, never the actor (v2
    /// quirk: a co-requester can never replace the attribution).
    fn requester_cancel_one(
        &self,
        kind: RequestKind,
        mbid: &str,
        user_id: &str,
        now: u64,
    ) -> Result<bool, RequestsError> {
        if self.store.get(kind, mbid)?.is_none() {
            return Ok(false);
        }
        let decision = self
            .store
            .prepare_requester_cancel(kind, mbid, user_id, now)?;
        match decision {
            None => Ok(false),
            Some(super::ledger::CancelDecision::Denied { .. }) => Ok(false),
            Some(super::ledger::CancelDecision::Detached) => Ok(true),
            Some(super::ledger::CancelDecision::CancelledDirect) => Ok(true),
            Some(super::ledger::CancelDecision::CancelTask {
                task_id,
                generation,
                ..
            }) => {
                if let Some(task_id) = task_id.as_deref() {
                    self.dispatch.cancel_task(task_id);
                }
                if !self
                    .store
                    .update_status(kind, mbid, STATUS_CANCELLED, Some(now), generation)?
                {
                    return Ok(false);
                }
                Ok(true)
            }
        }
    }

    /// Verbose requester cancel of one row with v2's messages.
    fn requester_cancel_verbose(
        &self,
        kind: RequestKind,
        mbid: &str,
        record: &RequestRecord,
        user_id: &str,
        now: u64,
    ) -> Result<ActionResponse, RequestsError> {
        let decision = self
            .store
            .prepare_requester_cancel(kind, mbid, user_id, now)?;
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
                if let Some(task_id) = task_id.as_deref() {
                    self.dispatch.cancel_task(task_id);
                }
                if !self.store.update_status(
                    kind,
                    mbid,
                    STATUS_CANCELLED,
                    Some(now),
                    generation,
                )? {
                    self.store.restore_status(
                        kind,
                        mbid,
                        &prior_status,
                        STATUS_CANCELLING,
                        generation,
                    )?;
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

    /// Reconcile one live row against its linked task. Waiting rows have no
    /// task and never move here; taskless dispatch rows keep their status
    /// (v2 falls back to library presence, which intake cannot see).
    /// One row plus its linked task's progress snapshot. Rows without a
    /// task (waiting for approval) read exactly as the record maps.
    fn to_item(&self, record: &RequestRecord) -> RequestItem {
        let mut item = RequestItem::from(record);
        let Some(task_id) = record.task_id.as_deref() else {
            return item;
        };
        if let Some(snapshot) = self.dispatch.task_progress(task_id) {
            item.progress = Some(snapshot.progress_percent as f64);
            item.size = snapshot.total_size_bytes.map(|total| total as f64);
            item.size_remaining = snapshot
                .total_size_bytes
                .map(|total| (total - snapshot.downloaded_bytes).max(0) as f64);
            item.error_message = snapshot.error_message;
            item.quality = snapshot.quality;
            item.protocol = Some(snapshot.protocol);
        }
        // v2 history quirk: only failed rows offer the admin reimport, and
        // only when the task still has its candidate linked.
        if record.status == STATUS_FAILED {
            item.can_reimport = Some(self.dispatch.reimportable(task_id));
        }
        item
    }

    fn reconcile_one(&self, record: &RequestRecord, now: u64) -> Result<bool, RequestsError> {
        let Some(task_id) = record.task_id.as_deref() else {
            return Ok(false);
        };
        let mapped = match self.dispatch.task_state(task_id) {
            DispatchTaskState::Active => {
                if record.status == STATUS_PENDING || record.status == STATUS_QUEUED {
                    Some(STATUS_DOWNLOADING)
                } else {
                    None
                }
            }
            DispatchTaskState::Imported => Some(STATUS_IMPORTED),
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
        let completed = matches!(
            next,
            STATUS_IMPORTED | STATUS_FAILED | STATUS_CANCELLED | STATUS_INCOMPLETE
        )
        .then_some(now);
        self.store
            .update_status(record.kind, &record.key, next, completed, record.generation)
    }
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
