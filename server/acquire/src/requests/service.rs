//! Request-intake domain logic, ported from v2 `request_service.py` and
//! `requests_page_service.py`.
//!
//! The approval gate: a `user` ask waits for admin review while `trusted`
//! and `admin` asks auto-approve and dispatch through the download seam
//! immediately. Duplicate asks attach as co-requesters to the live row
//! instead of spawning rivals; every mutation names the generation it won.
//! Quirk citations name the v2 behavior each branch preserves.

mod edition;

use std::sync::Arc;

use super::RequestsState;
use super::auth::auto_approves;
use super::error::RequestsError;
use super::ledger::{BeginOutcome, RequestRecord, RequestStatus, meaningful_name, validate_mbid};
use super::models::{
    ActionResponse, ActiveCountResponse, ActiveRequestsResponse, AlbumIntake, BatchCancelResponse,
    BatchIntake, BatchIntakeResponse, ClearHistoryResponse, HistoryQuery, HistoryResponse,
    IntakeResponse, PersonalMixApprovalItem, PersonalMixApprovalsResponse, RefreshResponse,
    RequestItem, RequestKind, TrackIntake, TrackIntakeResponse, WaitingReason,
    WantedActionResponse, WantedItem, WantedResponse, WantedRetryingItem,
};
use super::quota::QuotaLedger;
use super::sqlite::{EditionStore, PersonalMixStore, RequestStore, WantedStore, WatchChange};
use crate::db::AcquireDb;
use crate::dispatch::{
    DispatchError, DispatchOrigin, DispatchOutcome, DispatchRequest, DispatchTarget,
    DownloadDispatch,
};
use crate::downloads::state::TaskStatus;
use crate::edition::chosen_edition;
use crate::follows::cursors::Follower;
use crate::follows::{FollowAsk, Follows};
use droppedneedle_auth::{Admin, Principal};

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

    /// Ask for a followed artist's new release on the follower's behalf.
    /// The follower's auto-download approval stands in for a per-request
    /// one. Quotas apply, but a full one never refuses: the row waits as
    /// queued and [`Self::start_waiting`] starts it once there is room.
    pub async fn request_follow(
        &self,
        release: &FollowRelease,
        now: u64,
    ) -> Result<FollowIntake, RequestsError> {
        let principal = Principal::acting_for(
            release.follower.user_id.clone(),
            release.follower.role,
            release.follower.name.clone(),
        );
        let owner = principal.user_id();
        let key = release.release_group_mbid.to_lowercase();
        if let Some(existing) = self.store.get(RequestKind::Album, &key).await?
            && (existing.status.is_active() || existing.status == RequestStatus::Cancelling)
        {
            return self.join_follow(&existing, &principal, now).await;
        }
        let gate = self.quota.request_gate(owner, principal.role(), 1)?;
        let hold = !self.storage_room(&principal).await?;
        let record = RequestRecord {
            key: key.clone(),
            kind: RequestKind::Album,
            status: RequestStatus::Pending,
            artist_name: release.artist_name.clone(),
            album_title: release.title.clone(),
            artist_mbid: Some(release.artist_mbid.clone()),
            year: None,
            release_mbid: self.chosen_release(&key).await,
            track_title: None,
            duration_seconds: None,
            track_release_group_mbid: None,
            user_id: Some(owner.to_owned()),
            origin: DispatchOrigin::Follow,
            requested_by_name: Some(principal.username_display().to_owned()),
            requesters: Vec::new(),
            requested_at: now,
            completed_at: None,
            task_id: None,
            generation: 0,
            dispatch_authorized: true,
            monitor_artist: false,
            auto_download_artist: false,
            reviewed_by_name: None,
            reviewed_at: None,
        };
        match self.store.begin_or_wait(record, owner, gate, hold).await? {
            BeginOutcome::Existing(_) => Ok(FollowIntake::Joined),
            BeginOutcome::Won(won) => {
                self.announce_created(&won);
                if won.waits_for_quota() {
                    return Ok(FollowIntake::Waiting);
                }
                let started = self
                    .dispatch_album(&won, DispatchOrigin::Follow, owner)
                    .await?;
                Ok(FollowIntake::Started {
                    task_id: started.task_id.unwrap_or_default(),
                })
            }
        }
    }

    /// A follow's new release whose album already has a live row. The
    /// follow settles only on a row that will download: a row waiting for
    /// an admin or being cancelled may never do so, so the release stays
    /// held and is asked for again on a later poll. A row waiting for
    /// another owner's quota starts when the follower has room.
    async fn join_follow(
        &self,
        existing: &RequestRecord,
        principal: &Principal,
        now: u64,
    ) -> Result<FollowIntake, RequestsError> {
        if matches!(
            existing.status,
            RequestStatus::AwaitingApproval | RequestStatus::Cancelling
        ) {
            return Ok(FollowIntake::Held);
        }
        self.store
            .attach_requester(
                RequestKind::Album,
                &existing.key,
                principal.user_id(),
                Some(principal.username_display().to_owned()),
                now,
            )
            .await?;
        if !existing.waits_for_quota() || existing.user_id.as_deref() == Some(principal.user_id()) {
            return Ok(FollowIntake::Joined);
        }
        Ok(match self.start_one(existing, principal, now).await? {
            Some(task_id) => FollowIntake::Started { task_id },
            None => FollowIntake::Held,
        })
    }

    /// Start every row waiting for quota room whose owner has room now.
    /// Answers the rows started with their tasks. A row whose owner no
    /// longer exists is cancelled. One row's failure is logged and leaves
    /// the others to run.
    pub async fn start_waiting(
        &self,
        now: u64,
    ) -> Result<Vec<(RequestRecord, String)>, RequestsError> {
        let mut started = Vec::new();
        for (record, role) in self.store.waiting_for_quota().await? {
            let (Some(owner), Some(role)) = (record.user_id.clone(), role) else {
                tracing::info!(key = %record.key, "waiting request has no owner; cancelling it");
                if let Err(error) = self
                    .store
                    .update_status(
                        RequestKind::Album,
                        &record.key,
                        RequestStatus::Cancelled,
                        Some(now),
                        Some(record.generation),
                    )
                    .await
                {
                    tracing::warn!(key = %record.key, ?error, "ownerless request cancel failed");
                }
                continue;
            };
            let principal = Principal::acting_for(owner, role, record.requested_by_name.clone());
            match self.start_one(&record, &principal, now).await {
                Ok(Some(task_id)) => started.push((record, task_id)),
                Ok(None) => {}
                Err(error) => {
                    tracing::warn!(key = %record.key, ?error, "waiting request could not start");
                }
            }
        }
        Ok(started)
    }

    /// Start one row waiting for quota room when `principal` has room
    /// for one more ask: the owner, or a person whose own ask joined it.
    /// Answers the task, or `None` while it still waits. A download that
    /// cannot be queued (a busy database included) puts the row back to
    /// waiting, never failed.
    async fn start_one(
        &self,
        record: &RequestRecord,
        principal: &Principal,
        now: u64,
    ) -> Result<Option<String>, RequestsError> {
        if self.waiting_reason_for(principal, now).await?.is_some() {
            return Ok(None);
        }
        let gate = self
            .quota
            .request_gate(principal.user_id(), principal.role(), 1)?;
        let Some(admitted) = self
            .store
            .admit_waiting(&record.key, principal.user_id(), gate, now)
            .await?
        else {
            return Ok(None);
        };
        let owner = admitted.user_id.clone().unwrap_or_default();
        let request = stored_dispatch(&admitted, admitted.origin, &owner);
        let outcome = match self.dispatch.dispatch(&request).await {
            Ok(outcome) => outcome,
            Err(cause) => {
                self.store
                    .requeue_waiting(&admitted.key, admitted.generation)
                    .await?;
                return Err(dispatch_fault(cause));
            }
        };
        if !self
            .store
            .link_task(
                RequestKind::Album,
                &admitted.key,
                outcome.task_id(),
                Some(admitted.generation),
            )
            .await?
        {
            cancel_started(&self.dispatch, &outcome).await;
            return Err(RequestsError::internal(&format_args!(
                "waiting request lost its generation for {}",
                admitted.key
            )));
        }
        Ok(Some(outcome.task_id().to_owned()))
    }

    /// What keeps `principal` from one more ask now: the request count
    /// for the window (read first, as it is the cheaper check), then the
    /// storage budget and library cap. `None` when there is room.
    async fn waiting_reason_for(
        &self,
        principal: &Principal,
        now: u64,
    ) -> Result<Option<WaitingReason>, RequestsError> {
        if let Some(gate) = self
            .quota
            .request_gate(principal.user_id(), principal.role(), 1)?
        {
            let used = self
                .quota
                .count_in_window(principal.user_id(), gate.window_days, now)
                .await?;
            if used.saturating_add(gate.new_requests) > gate.limit {
                return Ok(Some(WaitingReason::RequestQuotaFull));
            }
        }
        if !self.storage_room(principal).await? {
            return Ok(Some(WaitingReason::StorageFull));
        }
        Ok(None)
    }

    /// Whether `principal`'s storage budget and the library cap leave
    /// room for one more album.
    async fn storage_room(&self, principal: &Principal) -> Result<bool, RequestsError> {
        match self
            .quota
            .check_storage_admission(
                principal.user_id(),
                principal.role(),
                DispatchOrigin::Follow,
            )
            .await
        {
            Ok(()) => Ok(true),
            Err(RequestsError::StorageFull { .. }) => Ok(false),
            Err(error) => Err(error),
        }
    }

    async fn intake_album(
        &self,
        principal: &Principal,
        body: &AlbumIntake,
        granted: bool,
    ) -> Result<IntakeResponse, RequestsError> {
        let role = principal.role();
        let mbid = validate_mbid(&body.musicbrainz_id)?;
        let now = self.now_epoch();
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
        let follow = follow_ask(
            principal,
            body.artist_mbid.as_deref().filter(|_| body.monitor_artist),
            Some(artist_name.clone()),
            body.auto_download_artist,
        );
        if let Some(existing) = self.store.get(RequestKind::Album, &mbid).await?
            && (existing.status.is_active() || existing.status == RequestStatus::Cancelling)
        {
            return self.attach_album(principal, &existing, follow).await;
        }

        let gate = self.quota.request_gate(principal.user_id(), role, 1)?;
        self.quota
            .check_storage_admission(principal.user_id(), role, DispatchOrigin::User)
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
        let needs_approval = !granted && !auto_approves(role);
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
            .begin(record, principal.user_id(), gate, follow.clone())
            .await?
            .map_err(|refusal| QuotaLedger::refusal(&refusal))?;
        match outcome {
            BeginOutcome::Existing(winner) if winner.status.is_active() => {
                self.attach_album(principal, &winner, follow).await
            }
            BeginOutcome::Existing(winner) if winner.status == RequestStatus::Cancelling => {
                self.attach_album(principal, &winner, follow).await
            }
            BeginOutcome::Existing(_) => Ok(IntakeResponse {
                success: false,
                message: "Request could not be recorded".to_owned(),
                musicbrainz_id: mbid,
                status: RequestStatus::Failed.as_str().to_owned(),
                task_id: None,
            }),
            BeginOutcome::Won(won) => {
                self.announce_created(&won);
                if needs_approval {
                    return Ok(IntakeResponse {
                        success: true,
                        message: "Request submitted, awaiting admin approval".to_owned(),
                        musicbrainz_id: mbid,
                        status: RequestStatus::AwaitingApproval.as_str().to_owned(),
                        task_id: None,
                    });
                }
                self.dispatch_album(&won, DispatchOrigin::User, principal.user_id())
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
        let role = principal.role();
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
        let now = self.now_epoch();

        if let Some(existing) = self.store.get(RequestKind::Track, &mbid).await?
            && (existing.status.is_active() || existing.status == RequestStatus::Cancelling)
        {
            self.store
                .attach_requester(
                    RequestKind::Track,
                    &mbid,
                    principal.user_id(),
                    Some(principal.username_display().to_owned()),
                    now,
                )
                .await?;
            return Ok(track_duplicate(&existing));
        }

        let gate = self.quota.request_gate(principal.user_id(), role, 1)?;
        self.quota
            .check_storage_admission(principal.user_id(), role, DispatchOrigin::User)
            .await?;

        let needs_approval = !auto_approves(role);
        let record = RequestRecord {
            key: mbid.to_lowercase(),
            kind: RequestKind::Track,
            status: status_for_role(needs_approval),
            artist_name,
            album_title,
            artist_mbid: body.artist_mbid.clone(),
            year: None,
            release_mbid: body.release_mbid.clone(),
            track_title: Some(track_title),
            duration_seconds: body.duration_seconds,
            track_release_group_mbid: body.release_group_mbid.clone(),
            user_id: Some(principal.user_id().to_owned()),
            origin: DispatchOrigin::User,
            requested_by_name: Some(principal.username_display().to_owned()),
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
            .begin(record, principal.user_id(), gate, None)
            .await?
            .map_err(|refusal| QuotaLedger::refusal(&refusal))?;
        match outcome {
            BeginOutcome::Existing(winner)
                if winner.status.is_active() || winner.status == RequestStatus::Cancelling =>
            {
                self.store
                    .attach_requester(
                        RequestKind::Track,
                        &mbid,
                        principal.user_id(),
                        Some(principal.username_display().to_owned()),
                        now,
                    )
                    .await?;
                Ok(track_duplicate(&winner))
            }
            BeginOutcome::Existing(_) => Ok(TrackIntakeResponse {
                status: RequestStatus::Queued.as_str().to_owned(),
                task_id: None,
            }),
            BeginOutcome::Won(won) => {
                self.announce_created(&won);
                if needs_approval {
                    return Ok(TrackIntakeResponse {
                        status: RequestStatus::AwaitingApproval.as_str().to_owned(),
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
        let (user_id, role) = (principal.user_id(), principal.role());
        let name = principal.username_display().to_owned();
        if body.items.len() > BATCH_MAX_ITEMS {
            return Err(RequestsError::InvalidInput {
                message: format!("Batch must hold at most {BATCH_MAX_ITEMS} items"),
            });
        }
        let now = self.now_epoch();

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
        // The follow each row's ask names, keyed by the row's lowercased id.
        let follows: std::collections::HashMap<String, FollowAsk> = normalized
            .iter()
            .filter_map(|(mbid, item)| {
                let artist = item
                    .artist_mbid
                    .as_deref()
                    .filter(|artist| body.monitor_artist && validate_mbid(artist).is_ok());
                let name = meaningful_name(item.artist_name.as_deref());
                let ask = follow_ask(principal, artist, name, body.auto_download_artist)?;
                Some((mbid.to_lowercase(), ask))
            })
            .collect();
        let follow_for = |mbid: &str| follows.get(&mbid.to_lowercase()).cloned();

        let needs_approval = !auto_approves(role);
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
                    .attach_following(mbid, user_id, Some(name.clone()), now, follow_for(mbid))
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
            user_id,
            role,
            u32::try_from(new_items.len()).unwrap_or(u32::MAX),
        )?;
        self.quota
            .check_storage_admission(user_id, role, DispatchOrigin::User)
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
                status: RequestStatus::Failed.as_str().to_owned(),
            });
        }

        let mut records = Vec::with_capacity(resolvable.len());
        for (mbid, item, artist_name, album_title) in &resolvable {
            let release_mbid = self.chosen_release(mbid).await;
            let record = RequestRecord {
                key: mbid.to_lowercase(),
                kind: RequestKind::Album,
                status: status_for_role(needs_approval),
                artist_name: artist_name.clone(),
                album_title: album_title.clone(),
                artist_mbid: item.artist_mbid.clone(),
                year: item.year,
                release_mbid,
                track_title: None,
                duration_seconds: None,
                track_release_group_mbid: None,
                user_id: Some(user_id.to_owned()),
                origin: DispatchOrigin::User,
                requested_by_name: Some(name.clone()),
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
            };
            records.push((record, follow_for(mbid)));
        }
        let outcomes = self
            .store
            .begin_following(records, user_id, gate)
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
                    if winner.status.is_active() || winner.status == RequestStatus::Cancelling =>
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
                .attach_following(mbid, user_id, Some(name.clone()), now, follow_for(mbid))
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
                status: RequestStatus::Failed.as_str().to_owned(),
            });
        }
        if needs_approval {
            return Ok(BatchIntakeResponse {
                success: true,
                message: "Batch request submitted, awaiting admin approval".to_owned(),
                requested: created.len() as u32,
                skipped,
                overflow: 0,
                status: RequestStatus::AwaitingApproval.as_str().to_owned(),
            });
        }

        let mut dispatched = 0_u32;
        for won in &created {
            match self
                .dispatch_album(won, DispatchOrigin::User, user_id)
                .await
            {
                Ok(_) => {
                    dispatched = dispatched.saturating_add(1);
                }
                Err(RequestsError::InvalidInput { .. }) => {
                    // Admission fault: the row already reads failed.
                }
                Err(error) => {
                    // One row's dispatch fault never fails its siblings.
                    tracing::warn!(key = %won.key, ?error, "batch dispatch failed");
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
                RequestStatus::Pending.as_str().to_owned()
            } else {
                RequestStatus::Failed.as_str().to_owned()
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
        let is_admin = principal.is_admin();
        let now = self.now_epoch();
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
                self.requester_cancel_one(kind, &mbid, principal.user_id(), now)
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
        admin: &Admin,
        mbid: &str,
        kind: RequestKind,
    ) -> Result<ActionResponse, RequestsError> {
        let mbid = validate_mbid(mbid)?;
        let now = self.now_epoch();
        let Some(record) = self.store.get(kind, &mbid).await? else {
            return Ok(ActionResponse {
                success: false,
                message: "Request not found".to_owned(),
            });
        };
        if record.status != RequestStatus::AwaitingApproval {
            return Ok(not_waiting(record.status.as_str()));
        }
        let claimed = self
            .store
            .claim_approval(
                kind,
                &mbid,
                Some(admin.username_display().to_owned()),
                now,
                record.generation,
            )
            .await?;
        let Some(claimed) = claimed else {
            let status = self
                .store
                .get(kind, &mbid)
                .await?
                .map(|row| row.status.as_str().to_owned())
                .unwrap_or_else(|| "changed".to_owned());
            return Ok(not_waiting(&status));
        };
        // Shared approvals keep the immutable primary owner; only ownerless
        // legacy rows fall back to the acting admin (v2 `_dispatch_record`).
        let owner = claimed
            .user_id
            .clone()
            .unwrap_or_else(|| admin.user_id().to_owned());
        let request = stored_dispatch(&claimed, DispatchOrigin::Approval, &owner);
        match self.dispatch.dispatch(&request).await {
            Err(DispatchError::Busy { operation }) => {
                self.store
                    .restore_status(
                        kind,
                        &mbid,
                        RequestStatus::AwaitingApproval,
                        RequestStatus::Pending,
                        claimed.generation,
                    )
                    .await?;
                Err(RequestsError::Busy { operation })
            }
            Err(DispatchError::Failed(cause)) => {
                tracing::error!(%cause, "approved dispatch failed");
                self.store
                    .update_status(
                        kind,
                        &mbid,
                        RequestStatus::Failed,
                        Some(now),
                        Some(claimed.generation),
                    )
                    .await?;
                Ok(ActionResponse {
                    success: false,
                    message: format!("Approved but failed to start: {}", record_title(&record)),
                })
            }
            Ok(outcome) => {
                if !self
                    .store
                    .link_task(kind, &mbid, outcome.task_id(), Some(claimed.generation))
                    .await?
                {
                    cancel_started(&self.dispatch, &outcome).await;
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
        admin: &Admin,
        mbid: &str,
        kind: RequestKind,
    ) -> Result<ActionResponse, RequestsError> {
        let mbid = validate_mbid(mbid)?;
        let now = self.now_epoch();
        let Some(record) = self.store.get(kind, &mbid).await? else {
            return Ok(ActionResponse {
                success: false,
                message: "Request not found".to_owned(),
            });
        };
        if record.status != RequestStatus::AwaitingApproval {
            return Ok(not_waiting(record.status.as_str()));
        }
        let claimed = self
            .store
            .claim_rejection(
                kind,
                &mbid,
                Some(admin.username_display().to_owned()),
                now,
                record.generation,
            )
            .await?;
        if !claimed {
            let status = self
                .store
                .get(kind, &mbid)
                .await?
                .map(|row| row.status.as_str().to_owned())
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
        let now = self.now_epoch();
        let Some(record) = self.store.get(kind, &mbid).await? else {
            return Ok(ActionResponse {
                success: false,
                message: "Request not found".to_owned(),
            });
        };
        if !principal.is_admin() {
            return self
                .requester_cancel_verbose(kind, &mbid, &record, principal.user_id(), now)
                .await;
        }
        if record.status == RequestStatus::AwaitingApproval {
            if !self
                .store
                .update_status(
                    kind,
                    &mbid,
                    RequestStatus::Cancelled,
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
        if !record.status.is_cancellable() {
            return Ok(ActionResponse {
                success: false,
                message: format!("Cannot cancel request with status '{}'", record.status),
            });
        }
        if let Some(task_id) = record.task_id.as_deref()
            && !cancel_linked(&self.dispatch, task_id).await
        {
            return Ok(ActionResponse {
                success: false,
                message: "Could not stop the download; try again".to_owned(),
            });
        }
        if !self
            .land_admin_cancel(kind, &mbid, record.task_id.as_deref(), now, false)
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

    /// After an admin stopped a row's download, make the row read
    /// cancelled. Stopping the download already settles a linked live row
    /// as cancelled in the same write; a row it did not reach is cancelled
    /// here at its current generation, if it may still be (`waiting` lets
    /// a row awaiting approval through) and still follows the download that
    /// was stopped (`stopped`). Answers whether the row reads cancelled.
    async fn land_admin_cancel(
        &self,
        kind: RequestKind,
        mbid: &str,
        stopped: Option<&str>,
        now: u64,
        waiting: bool,
    ) -> Result<bool, RequestsError> {
        let Some(current) = self.store.get(kind, mbid).await? else {
            return Ok(false);
        };
        if current.status == RequestStatus::Cancelled {
            return Ok(true);
        }
        // Linked to a new download meanwhile: that one keeps running.
        if current.task_id.as_deref() != stopped {
            return Ok(false);
        }
        let open = current.status.is_cancellable()
            || (waiting && current.status == RequestStatus::AwaitingApproval);
        if !open {
            return Ok(false);
        }
        self.store
            .update_status(
                kind,
                mbid,
                RequestStatus::Cancelled,
                Some(now),
                Some(current.generation),
            )
            .await
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
        self.retry_with(principal, &mbid, kind, false).await
    }

    /// Take a single from its album: a failed track request whose download
    /// found only the album's version of the song is retried, and this
    /// time the album's track imports as the song when it is the right
    /// track. The person's click is the choice; the retry keeps the
    /// request's approval, like any retry.
    pub async fn take_album_version(
        &self,
        principal: &Principal,
        mbid: &str,
    ) -> Result<ActionResponse, RequestsError> {
        let mbid = validate_mbid(mbid)?;
        let kind = RequestKind::Track;
        let Some(record) = self.store.get(kind, &mbid).await? else {
            return Err(RequestsError::NotFound);
        };
        if !principal.is_admin()
            && !self
                .store
                .is_requester(kind, &mbid, principal.user_id())
                .await?
        {
            return Err(RequestsError::Forbidden {
                message: "Cannot change another user's request".to_owned(),
            });
        }
        let offered = match record.task_id.as_deref() {
            Some(task_id) if record.status == RequestStatus::Failed => self
                .dispatch
                .album_version_only(task_id)
                .await
                .map_err(dispatch_fault)?,
            _ => false,
        };
        if !offered {
            return Err(RequestsError::Conflict {
                message: "This request has no album version to take".to_owned(),
            });
        }
        // Waiting for approval would drop the choice: the retry would be
        // dispatched later as a plain one.
        if !record.dispatch_authorized && !auto_approves(principal.role()) {
            return Err(RequestsError::Conflict {
                message: "This request needs approval first".to_owned(),
            });
        }
        self.retry_with(principal, &mbid, kind, true).await
    }

    /// Retry one terminal ask; `album_version` takes a single from its
    /// album (see [`Self::take_album_version`]).
    async fn retry_with(
        &self,
        principal: &Principal,
        mbid: &str,
        kind: RequestKind,
        album_version: bool,
    ) -> Result<ActionResponse, RequestsError> {
        let mbid = mbid.to_owned();
        let now = self.now_epoch();
        let Some(record) = self.store.get(kind, &mbid).await? else {
            return Ok(ActionResponse {
                success: false,
                message: "Request not found".to_owned(),
            });
        };
        if !record.status.is_retryable() {
            return Ok(ActionResponse {
                success: false,
                message: format!("Cannot retry request with status '{}'", record.status),
            });
        }
        let prior_status = record.status;
        let can_dispatch = record.dispatch_authorized || auto_approves(principal.role());
        let target = if can_dispatch {
            RequestStatus::Pending
        } else {
            RequestStatus::AwaitingApproval
        };
        let claim = self
            .store
            .claim_retry(
                kind,
                &mbid,
                principal.user_id(),
                target,
                can_dispatch,
                !principal.is_admin(),
                now,
                record.generation,
            )
            .await?;
        let generation = match claim {
            super::ledger::RetryClaim::Claimed { generation, .. } => generation,
            super::ledger::RetryClaim::Lost => {
                let current = self.store.get(kind, &mbid).await?;
                if !principal.is_admin()
                    && current
                        .as_ref()
                        .is_some_and(|row| row.status.is_retryable())
                    && !self
                        .store
                        .is_requester(kind, &mbid, principal.user_id())
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
        if target == RequestStatus::AwaitingApproval {
            return Ok(ActionResponse {
                success: true,
                message: "Retry submitted, awaiting admin approval".to_owned(),
            });
        }
        let owner = record
            .user_id
            .clone()
            .unwrap_or_else(|| principal.user_id().to_owned());
        let mut request = stored_dispatch(&record, DispatchOrigin::Retry, &owner);
        if let DispatchTarget::Track {
            album_version: from_album,
            ..
        } = &mut request.target
        {
            *from_album = album_version;
        }
        match self.dispatch.dispatch(&request).await {
            Err(DispatchError::Busy { operation }) => {
                self.store
                    .update_status(kind, &mbid, prior_status, None, Some(generation))
                    .await?;
                Err(RequestsError::Busy { operation })
            }
            Err(DispatchError::Failed(cause)) => {
                tracing::error!(%cause, "retry dispatch failed");
                self.store
                    .update_status(kind, &mbid, prior_status, None, Some(generation))
                    .await?;
                Ok(ActionResponse {
                    success: false,
                    message: "Retry failed to start download".to_owned(),
                })
            }
            Ok(outcome) => {
                if !self
                    .store
                    .link_task(kind, &mbid, outcome.task_id(), Some(generation))
                    .await?
                {
                    cancel_started(&self.dispatch, &outcome).await;
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
        if !principal.is_admin()
            && !self
                .store
                .is_requester(kind, &mbid, principal.user_id())
                .await?
        {
            return Err(RequestsError::Forbidden {
                message: "Cannot clear another user's request".to_owned(),
            });
        }
        if !record.status.is_clearable() {
            return Ok(ClearHistoryResponse { success: false });
        }
        let cleared = if principal.is_admin() {
            self.store.delete(kind, &mbid).await?
        } else {
            self.store
                .dismiss(kind, &mbid, principal.user_id(), self.now_epoch())
                .await?
        };
        Ok(ClearHistoryResponse { success: cleared })
    }

    /// Live rows. Admins see every row; anyone else sees their own (v2
    /// `user_id=None`-for-admin quirk). A read never writes: rows follow
    /// their tasks when the tasks settle.
    pub async fn active(
        &self,
        principal: &Principal,
    ) -> Result<ActiveRequestsResponse, RequestsError> {
        let owner = (!principal.is_admin()).then_some(principal.user_id());
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
        let owner = (!principal.is_admin()).then_some(principal.user_id());
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
        let owner = (!principal.is_admin()).then_some(principal.user_id());
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
        _admin: &Admin,
    ) -> Result<ActiveRequestsResponse, RequestsError> {
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
        _admin: &Admin,
    ) -> Result<ActiveCountResponse, RequestsError> {
        let asks = self.store.pending_approvals().await?.len() as u32;
        let follows = self.follows.pending_units().await?;
        let mixes = self.mixes.pending_count().await?;
        Ok(ActiveCountResponse {
            count: asks.saturating_add(follows).saturating_add(mixes),
        })
    }

    /// Wanted watches plus the still-retrying set. Admins see every row.
    pub async fn wanted(&self, principal: &Principal) -> Result<WantedResponse, RequestsError> {
        let is_admin = principal.is_admin();
        let watches = self
            .wanted
            .watches_for(principal.user_id(), is_admin)
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
        let is_admin = principal.is_admin();
        let mut out = Vec::new();
        for status in [RequestStatus::Failed, RequestStatus::Incomplete] {
            for record in self.store.with_status(RequestKind::Album, status).await? {
                if !is_admin && !record.has_requester(principal.user_id()) {
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
            .stop(&mbid, principal.user_id(), principal.is_admin())
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
                principal.user_id(),
                principal.is_admin(),
                self.now_epoch(),
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
            .mark_seen(&mbid, principal.user_id(), principal.is_admin())
            .await?;
        watch_action(change)
    }

    /// Pending personal-mix approvals (admin).
    pub async fn mix_approvals(
        &self,
        _admin: &Admin,
    ) -> Result<PersonalMixApprovalsResponse, RequestsError> {
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
        admin: &Admin,
        user_id: &str,
    ) -> Result<ActionResponse, RequestsError> {
        if !self
            .mixes
            .decide(user_id, "approved", reviewer(admin), self.now_epoch())
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
        admin: &Admin,
        user_id: &str,
    ) -> Result<ActionResponse, RequestsError> {
        if !self
            .mixes
            .decide(user_id, "rejected", reviewer(admin), self.now_epoch())
            .await?
        {
            return Ok(no_match());
        }
        // A rejection also turns the user's toggle off (v2).
        if let Some(mixer) = &self.mixer {
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
        admin: &Admin,
        user_id: &str,
    ) -> Result<ActionResponse, RequestsError> {
        if !self
            .mixes
            .revoke(user_id, reviewer(admin), self.now_epoch())
            .await?
        {
            return Ok(no_match());
        }
        if let Some(mixer) = &self.mixer {
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
        let Some(mixer) = &self.mixer else {
            return Err(RequestsError::Conflict {
                message: "Weekly Mix is not available on this server.".to_owned(),
            });
        };
        if !mixer.is_linked(principal.user_id()).await {
            return Err(RequestsError::InvalidInput {
                message: "Connect ListenBrainz first to build Your Weekly Mix".to_owned(),
            });
        }
        if !self.mixes.refresh_start(principal.user_id())? {
            return Ok(RefreshResponse {
                status: "already_running".to_owned(),
            });
        }
        mixer.spawn_refresh(principal.user_id().to_owned());
        Ok(RefreshResponse {
            status: "started".to_owned(),
        })
    }

    /// Startup recovery for the ledger. A row left `cancelling` finishes
    /// its cancel (the owner asked to stop; cancelling a task twice is
    /// harmless). An authorized `pending` row with no task had its dispatch
    /// or its link cut off: it relinks to the task created for it, or
    /// dispatches under the same idempotency key, so nothing runs twice.
    pub async fn recover(&self) -> Result<RequestRecovery, RequestsError> {
        let now = self.now_epoch();
        let mut report = RequestRecovery::default();
        for kind in [RequestKind::Album, RequestKind::Track] {
            for record in self
                .store
                .with_status(kind, RequestStatus::Cancelling)
                .await?
            {
                if let Some(task_id) = record.task_id.as_deref()
                    && !cancel_linked(&self.dispatch, task_id).await
                {
                    continue;
                }
                if self
                    .store
                    .update_status(
                        kind,
                        &record.key,
                        RequestStatus::Cancelled,
                        Some(now),
                        Some(record.generation),
                    )
                    .await?
                {
                    report.cancelled += 1;
                }
            }
            for record in self.store.with_status(kind, RequestStatus::Pending).await? {
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
                        let request = stored_dispatch(&record, record.origin, &owner);
                        match self.dispatch.dispatch(&request).await {
                            Ok(DispatchOutcome::Dispatched { task_id }) => {
                                report.redispatched += 1;
                                task_id
                            }
                            Ok(DispatchOutcome::Active { task_id }) => {
                                report.relinked += 1;
                                task_id
                            }
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

    /// Attach a duplicate album ask to its live row, with the follow it
    /// names, and report the row's own status (v2: the duplicate reply
    /// mirrors the winner, including the awaiting-approval message).
    async fn attach_album(
        &self,
        principal: &Principal,
        existing: &RequestRecord,
        follow: Option<FollowAsk>,
    ) -> Result<IntakeResponse, RequestsError> {
        self.store
            .attach_following(
                &existing.key,
                principal.user_id(),
                Some(principal.username_display().to_owned()),
                self.now_epoch(),
                follow,
            )
            .await?;
        // A row waiting for its owner's quota starts on the ask of someone
        // who needs no approval and has room of their own.
        if existing.waits_for_quota()
            && auto_approves(principal.role())
            && let Some(task_id) = self
                .start_one(existing, principal, self.now_epoch())
                .await?
        {
            return Ok(IntakeResponse {
                success: true,
                message: "Request accepted".to_owned(),
                musicbrainz_id: existing.key.clone(),
                status: RequestStatus::Pending.as_str().to_owned(),
                task_id: Some(task_id),
            });
        }
        let message = if existing.status == RequestStatus::AwaitingApproval {
            "Request is awaiting admin approval"
        } else if existing.status == RequestStatus::Cancelling {
            "Request is being cancelled"
        } else {
            "Request already in progress"
        };
        Ok(IntakeResponse {
            success: true,
            message: message.to_owned(),
            musicbrainz_id: existing.key.clone(),
            status: existing.status.as_str().to_owned(),
            task_id: existing.task_id.clone(),
        })
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
            status: status_for_role(needs_approval),
            artist_name: artist_name.to_owned(),
            album_title: album_title.to_owned(),
            artist_mbid: body.artist_mbid.clone(),
            year: body.year,
            release_mbid,
            track_title: None,
            duration_seconds: None,
            track_release_group_mbid: None,
            user_id: Some(principal.user_id().to_owned()),
            origin: DispatchOrigin::User,
            requested_by_name: Some(principal.username_display().to_owned()),
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
        let now = self.now_epoch();
        match self.dispatch.dispatch(&request).await {
            Err(cause) => {
                self.store
                    .update_status(
                        RequestKind::Album,
                        &won.key,
                        RequestStatus::Failed,
                        Some(now),
                        Some(won.generation),
                    )
                    .await?;
                Err(RequestsError::internal(&format_args!(
                    "album dispatch failed: {cause}"
                )))
            }
            Ok(outcome) => {
                if !self
                    .store
                    .link_task(
                        RequestKind::Album,
                        &won.key,
                        outcome.task_id(),
                        Some(won.generation),
                    )
                    .await?
                {
                    cancel_started(&self.dispatch, &outcome).await;
                    return Err(RequestsError::internal(&format_args!(
                        "album link lost its generation for {}",
                        won.key
                    )));
                }
                Ok(IntakeResponse {
                    success: true,
                    message: "Request accepted".to_owned(),
                    musicbrainz_id: won.key.clone(),
                    status: RequestStatus::Pending.as_str().to_owned(),
                    task_id: Some(outcome.task_id().to_owned()),
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
        let now = self.now_epoch();
        match self.dispatch.dispatch(&request).await {
            Err(cause) => {
                self.store
                    .update_status(
                        RequestKind::Track,
                        &won.key,
                        RequestStatus::Failed,
                        Some(now),
                        Some(won.generation),
                    )
                    .await?;
                Err(RequestsError::internal(&format_args!(
                    "track dispatch failed: {cause}"
                )))
            }
            Ok(outcome) => {
                if !self
                    .store
                    .link_task(
                        RequestKind::Track,
                        &won.key,
                        outcome.task_id(),
                        Some(won.generation),
                    )
                    .await?
                {
                    cancel_started(&self.dispatch, &outcome).await;
                    return Err(RequestsError::internal(&format_args!(
                        "track link lost its generation for {}",
                        won.key
                    )));
                }
                Ok(TrackIntakeResponse {
                    status: RequestStatus::Queued.as_str().to_owned(),
                    task_id: Some(outcome.task_id().to_owned()),
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
        if !record.status.is_active() {
            return Ok(false);
        }
        if let Some(task_id) = record.task_id.as_deref()
            && !cancel_linked(&self.dispatch, task_id).await
        {
            return Ok(false);
        }
        if !self
            .land_admin_cancel(kind, mbid, record.task_id.as_deref(), now, true)
            .await?
        {
            return Ok(false);
        }
        if record.status == RequestStatus::AwaitingApproval {
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
                    && !cancel_linked(&self.dispatch, task_id).await
                {
                    self.store
                        .restore_status(
                            kind,
                            mbid,
                            prior_status,
                            RequestStatus::Cancelling,
                            generation,
                        )
                        .await?;
                    return Ok(false);
                }
                if !self
                    .store
                    .update_status(
                        kind,
                        mbid,
                        RequestStatus::Cancelled,
                        Some(now),
                        Some(generation),
                    )
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
                if prior_status.is_cancellable()
                    || prior_status == RequestStatus::AwaitingApproval
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
                    && !cancel_linked(&self.dispatch, task_id).await
                {
                    self.store
                        .restore_status(kind, mbid, prior_status, RequestStatus::Cancelling, generation)
                        .await?;
                    return Ok(ActionResponse {
                        success: false,
                        message: "Could not stop the download; try again".to_owned(),
                    });
                }
                if !self.store.update_status(
                    kind,
                    mbid,
                    RequestStatus::Cancelled,
                    Some(now),
                    Some(generation),
                ).await? {
                    self.store.restore_status(
                        kind,
                        mbid,
                        prior_status,
                        RequestStatus::Cancelling,
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
    /// Why a row waiting for quota room still waits, read from its
    /// owner's quota now. A read failure only loses the reason.
    async fn waiting_reason(&self, record: &RequestRecord) -> Option<WaitingReason> {
        let owner = record.user_id.as_deref()?;
        let reason = match self.store.role_of(owner).await {
            Ok(Some(role)) => {
                let principal = Principal::acting_for(owner, role, None);
                self.waiting_reason_for(&principal, self.now_epoch()).await
            }
            Ok(None) => Ok(None),
            Err(error) => Err(error),
        };
        reason.unwrap_or_else(|error| {
            tracing::warn!(key = %record.key, ?error, "waiting reason unreadable");
            None
        })
    }

    async fn to_item(&self, record: &RequestRecord) -> RequestItem {
        let mut item = RequestItem::from(record);
        if record.waits_for_quota() {
            item.waiting_reason = self.waiting_reason(record).await;
        }
        let Some(task_id) = record.task_id.as_deref() else {
            return item;
        };
        match self.dispatch.task(task_id).await {
            Ok(Some(snapshot)) => {
                item.progress = Some(snapshot.progress_percent as f64);
                item.size = snapshot.total_size_bytes.map(|total| total as f64);
                item.size_remaining = snapshot
                    .total_size_bytes
                    .map(|total| (total - snapshot.downloaded_bytes).max(0) as f64);
                if snapshot.status == TaskStatus::Queued
                    && let Some(readiness) = &self.readiness
                    && !readiness.sources.can_acquire()
                {
                    item.waiting_reason = Some(WaitingReason::NoDownloadSource);
                }
                item.error_message = snapshot.error_message;
                item.quality = snapshot.quality;
                item.protocol = Some(snapshot.protocol);
            }
            Ok(None) => {}
            Err(error) => tracing::warn!(task_id, ?error, "task progress read failed"),
        }
        // v2 history quirk: only failed rows offer the admin reimport, and
        // only when the task still has its candidate linked.
        if record.status == RequestStatus::Failed {
            match self.dispatch.reimportable(task_id).await {
                Ok(reimportable) => item.can_reimport = Some(reimportable),
                Err(error) => tracing::warn!(task_id, ?error, "reimport check failed"),
            }
            if record.kind == RequestKind::Track {
                match self.dispatch.album_version_only(task_id).await {
                    Ok(found) => item.album_version_only = Some(found),
                    Err(error) => tracing::warn!(task_id, ?error, "hold reason read failed"),
                }
            }
        }
        item
    }
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

/// Reviewer stamp for approval decisions: acting id plus display name.
fn reviewer(principal: &Principal) -> (&str, Option<String>) {
    (
        principal.user_id(),
        Some(principal.username_display().to_owned()),
    )
}

/// Map a guarded watch change to the action reply (v2 `_owned_watch`).
fn watch_action(change: WatchChange) -> Result<WantedActionResponse, RequestsError> {
    match change {
        WatchChange::Done(watch) => Ok(WantedActionResponse {
            success: true,
            state: watch.state.as_str().to_owned(),
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

/// The follow an album ask names: its artist, when the ask says to follow
/// and names one, and auto-download the way the Following page asks for
/// it (trusted and admin callers approve themselves, a plain user's ask
/// waits for an admin).
fn follow_ask(
    principal: &Principal,
    artist_mbid: Option<&str>,
    artist_name: Option<String>,
    auto_download: bool,
) -> Option<FollowAsk> {
    Some(FollowAsk {
        artist_mbid: artist_mbid?.to_owned(),
        artist_name,
        auto_download,
        self_approves: principal.role().is_curator(),
    })
}

/// A followed artist's new release, asked for on a follower's behalf.
#[derive(Debug, Clone)]
pub struct FollowRelease {
    /// The follower the download is for.
    pub follower: Follower,
    /// Followed artist MBID.
    pub artist_mbid: String,
    /// Followed artist name.
    pub artist_name: String,
    /// The new release group.
    pub release_group_mbid: String,
    /// Its title.
    pub title: String,
}

/// How a follow's ask ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FollowIntake {
    /// A download started for the new row.
    Started {
        /// The download task.
        task_id: String,
    },
    /// The row waits for room in the follower's quota.
    Waiting,
    /// A live row for the album already existed and will download; the
    /// follower joined it.
    Joined,
    /// The album's row may never download (it waits for an admin, is
    /// being cancelled, or waits on a quota the follower cannot fill):
    /// keep the release held and ask again later.
    Held,
}

/// Initial status for one role's fresh ask.
fn status_for_role(needs_approval: bool) -> RequestStatus {
    if needs_approval {
        RequestStatus::AwaitingApproval
    } else {
        RequestStatus::Pending
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
    let status = if existing.status == RequestStatus::AwaitingApproval {
        RequestStatus::AwaitingApproval
    } else {
        RequestStatus::Queued
    };
    TrackIntakeResponse {
        status: status.as_str().to_owned(),
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
            status: record.status.as_str().to_owned(),
            progress: None,
            eta: None,
            size: None,
            size_remaining: None,
            status_messages: None,
            waiting_reason: None,
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
            album_version_only: None,
            request_kind: record.kind,
            origin: record.origin,
            track_title: record.track_title.clone(),
            duration_seconds: record.duration_seconds,
            track_release_group_mbid: record.track_release_group_mbid.clone(),
            requester_count: record.requesters.len() as u32,
        }
    }
}
