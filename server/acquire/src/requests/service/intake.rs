//! Intake: album, track, batch and follow asks. Winners record and
//! maybe dispatch; duplicate asks attach to the live row.

use super::recovery::Revived;
use super::{Dispatched, RequestsService, dispatch_fault, fresh_record, stored_dispatch};
use crate::dispatch::{DispatchError, DispatchOrigin};
use crate::follows::FollowAsk;
use crate::follows::cursors::Follower;
use crate::requests::auth::auto_approves;
use crate::requests::batches::BatchFiling;
use crate::requests::batches::models::BatchOutcome;
use crate::requests::batches::store::{BatchWrite, NewBatch};
use crate::requests::error::RequestsError;
use crate::requests::ledger::{
    BeginOutcome, RequestRecord, RequestStatus, meaningful_name, validate_mbid,
};
use crate::requests::models::{
    AlbumIntake, BatchIntake, BatchIntakeResponse, IntakeResponse, RequestKind, TrackIntake,
    TrackIntakeResponse, WaitingReason,
};
use crate::requests::quota::QuotaLedger;
use droppedneedle_auth::Principal;
use droppedneedle_kernel::events::{AutoDownloadEnqueued, UserNotice};

/// Batch row cap.
const BATCH_MAX_ITEMS: usize = 500;

/// How long a pending row may go without a task before the waiting pass
/// takes it for one that lost its dispatch.
const STALLED_START_SECS: u64 = 300;

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
/// required up front.
fn missing_names(mbid: &str) -> RequestsError {
    RequestsError::InvalidInput {
        message: format!(
            "Could not resolve artist and album for MBID {mbid}: artist and album are required"
        ),
    }
}

/// Duplicate-track reply mirroring the winner.
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

impl RequestsService {
    /// Ask for one album. Winners record and maybe dispatch; duplicate asks
    /// attach to the live row and report its status.
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
    /// queued and [`Self::start_waiting`] starts it once there is room. A
    /// busy database leaves it waiting the same way.
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
            artist_name: release.artist_name.clone(),
            album_title: release.title.clone(),
            artist_mbid: Some(release.artist_mbid.clone()),
            release_mbid: self.chosen_release_or_none(&key).await,
            ..fresh_record(
                RequestKind::Album,
                key.clone(),
                &principal,
                DispatchOrigin::Follow,
                RequestStatus::Pending,
                now,
            )
        };
        match self.store.begin_or_wait(record, owner, gate, hold).await? {
            BeginOutcome::Existing(existing) => self.join_follow(&existing, &principal, now).await,
            BeginOutcome::Won(won) => {
                self.announce_created(&won);
                if won.waits_for_quota() {
                    return Ok(FollowIntake::Waiting);
                }
                Ok(
                    match self
                        .dispatch_won(&won, DispatchOrigin::Follow, owner)
                        .await?
                    {
                        Some(task_id) => FollowIntake::Started { task_id },
                        None => FollowIntake::Waiting,
                    },
                )
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
        Ok(match self.start_joined(existing, principal, now).await? {
            Some(task_id) => FollowIntake::Started { task_id },
            None => FollowIntake::Held,
        })
    }

    /// Start every waiting row (one waiting for quota room, or one a busy
    /// database kept from starting) whose owner has room now. Answers the
    /// follow rows started, with their tasks, for the follower's notice. A
    /// row whose owner no longer exists is cancelled. One row's failure is
    /// logged and leaves the others to run.
    pub async fn start_waiting(
        &self,
        now: u64,
    ) -> Result<Vec<(RequestRecord, String)>, RequestsError> {
        let mut started = Vec::new();
        // A busy database can leave a row pending with no task. Once it is
        // older than any dispatch would take, it gets the task it lost.
        let stalled = self
            .store
            .stalled_starts(now.saturating_sub(STALLED_START_SECS))
            .await?;
        for record in stalled {
            match self.revive(&record).await {
                Ok(Some(Revived::Relinked(task_id) | Revived::Redispatched(task_id)))
                    if record.origin == DispatchOrigin::Follow =>
                {
                    started.push((record, task_id));
                }
                Ok(_) => {}
                Err(error) => {
                    tracing::warn!(key = %record.key, ?error, "stalled request could not start");
                }
            }
        }
        for (record, role) in self.store.waiting_for_quota().await? {
            let (Some(owner), Some(role)) = (record.user_id.clone(), role) else {
                tracing::info!(key = %record.key, "waiting request has no owner; cancelling it");
                if let Err(error) = self
                    .store
                    .update_status(
                        record.kind,
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
                Ok(Some(task_id)) if record.origin == DispatchOrigin::Follow => {
                    started.push((record, task_id));
                }
                Ok(_) => {}
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
            .admit_waiting(record.kind, &record.key, principal.user_id(), gate, now)
            .await?
        else {
            return Ok(None);
        };
        let owner = admitted.user_id.clone().unwrap_or_default();
        let request = stored_dispatch(&admitted, admitted.origin, &owner);
        match self
            .dispatch_and_link(admitted.kind, &admitted.key, &request, admitted.generation)
            .await?
        {
            Dispatched::Linked(task_id) => Ok(Some(task_id)),
            Dispatched::Stale => Err(RequestsError::internal(&format_args!(
                "waiting request lost its generation for {}",
                admitted.key
            ))),
            Dispatched::Failed(cause) => {
                self.wait_to_start(&admitted).await?;
                Err(dispatch_fault(cause))
            }
        }
    }

    /// Put a row whose download could not be queued back to waiting. When
    /// the database is too busy for that too, the row stays pending with
    /// no task: the waiting pass and startup recovery both start it.
    async fn wait_to_start(&self, record: &RequestRecord) -> Result<(), RequestsError> {
        match self
            .store
            .requeue_waiting(record.kind, &record.key, record.generation)
            .await
        {
            Ok(_) => Ok(()),
            Err(RequestsError::Busy { operation }) => {
                tracing::info!(key = %record.key, %operation, "database busy; the request stays pending");
                Ok(())
            }
            Err(error) => Err(error),
        }
    }

    /// Start a waiting row `principal` joined, as [`Self::start_one`]. A
    /// followed artist's release started by someone else's ask tells its
    /// follower, as the follow poll would.
    async fn start_joined(
        &self,
        record: &RequestRecord,
        principal: &Principal,
        now: u64,
    ) -> Result<Option<String>, RequestsError> {
        let started = self.start_one(record, principal, now).await?;
        if let (Some(task_id), Some(owner), Some(events)) =
            (&started, record.user_id.as_deref(), &self.events)
            && record.origin == DispatchOrigin::Follow
            && owner != principal.user_id()
        {
            events.notify(
                owner,
                UserNotice::AutoDownloadEnqueued(AutoDownloadEnqueued {
                    artist_mbid: record.artist_mbid.clone().unwrap_or_default(),
                    artist_name: record.artist_name.clone(),
                    release_group_mbid: record.key.clone(),
                    title: record.album_title.clone(),
                    task_id: task_id.clone(),
                }),
            );
        }
        Ok(started)
    }

    /// Start the waiting album rows of `keys` a batch ask by `principal`
    /// joined, when `principal` needs no approval. One row's failure is
    /// logged and leaves the others to run.
    async fn start_joined_keys(&self, principal: &Principal, keys: &[String]) {
        if !auto_approves(principal.role()) {
            return;
        }
        let now = self.now_epoch();
        for key in keys {
            let started = match self.store.get(RequestKind::Album, key).await {
                Ok(Some(row)) if row.waits_for_quota() => {
                    self.start_joined(&row, principal, now).await
                }
                Ok(_) => Ok(None),
                Err(error) => Err(error),
            };
            if let Err(error) = started {
                tracing::warn!(%key, ?error, "joined waiting request could not start");
            }
        }
    }

    /// What keeps `principal` from one more ask now: the request count
    /// for the window (read first, as it is the cheaper check), then the
    /// storage budget and library cap. `None` when there is room.
    pub(super) async fn waiting_reason_for(
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
        // Intake treats the submitted id as canonical and
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
        if body.monitor_artist && body.artist_mbid.is_none() {
            return Err(RequestsError::InvalidInput {
                message: "Following the artist needs its artist MBID".to_owned(),
            });
        }
        let follow = follow_ask(
            principal,
            body.artist_mbid.as_deref().filter(|_| body.monitor_artist),
            Some(artist_name.clone()),
            body.auto_download_artist,
        );
        // The library's chosen edition wins for an album it holds; the
        // edition the page showed fills in for one it does not.
        let asked = body
            .release_mbid
            .as_deref()
            .map(validate_mbid)
            .transpose()?
            .map(|release| release.to_lowercase());
        if let Some(existing) = self.store.get(RequestKind::Album, &mbid).await?
            && (existing.status.is_active() || existing.status == RequestStatus::Cancelling)
        {
            return self
                .attach_album(principal, &existing, follow, asked.as_deref())
                .await;
        }

        let gate = self.quota.request_gate(principal.user_id(), role, 1)?;
        self.quota
            .check_storage_admission(principal.user_id(), role, DispatchOrigin::User)
            .await?;

        let release_mbid = self.chosen_release(&mbid).await?.or(asked.clone());
        let needs_approval = !granted && !auto_approves(role);
        let record = RequestRecord {
            artist_name,
            album_title,
            artist_mbid: body.artist_mbid.clone(),
            year: body.year,
            release_mbid,
            monitor_artist: body.monitor_artist,
            auto_download_artist: body.auto_download_artist,
            ..fresh_record(
                RequestKind::Album,
                mbid.to_lowercase(),
                principal,
                DispatchOrigin::User,
                status_for_role(needs_approval),
                now,
            )
        };
        let outcome = self
            .store
            .begin(record, principal.user_id(), gate, follow.clone())
            .await?
            .map_err(|refusal| QuotaLedger::refusal(&refusal))?;
        match outcome {
            BeginOutcome::Existing(winner)
                if winner.status.is_active() || winner.status == RequestStatus::Cancelling =>
            {
                self.attach_album(principal, &winner, follow, asked.as_deref())
                    .await
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
                let task_id = self
                    .dispatch_won(&won, DispatchOrigin::User, principal.user_id())
                    .await?;
                Ok(accepted(&won.key, task_id))
            }
        }
    }

    /// Ask for one exact recording.
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
            artist_name,
            album_title,
            artist_mbid: body.artist_mbid.clone(),
            release_mbid: body.release_mbid.clone(),
            track_title: Some(track_title),
            duration_seconds: body.duration_seconds,
            track_release_group_mbid: body.release_group_mbid.clone(),
            ..fresh_record(
                RequestKind::Track,
                mbid.to_lowercase(),
                principal,
                DispatchOrigin::User,
                status_for_role(needs_approval),
                now,
            )
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
                // A track row always has its owner, so no actor stands in.
                let task_id = self.dispatch_won(&won, DispatchOrigin::User, "").await?;
                Ok(TrackIntakeResponse {
                    status: RequestStatus::Queued.as_str().to_owned(),
                    task_id,
                })
            }
        }
    }

    /// Ask for a batch of albums: raw dupes and
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
        // store, so canon equals the validated id; invalid ids are
        // skipped.
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
            let joined: Vec<String> = existing_items
                .iter()
                .map(|(mbid, _)| mbid.clone())
                .collect();
            self.start_joined_keys(principal, &joined).await;
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
            let release_mbid = self.chosen_release_or_none(mbid).await;
            let record = RequestRecord {
                artist_name: artist_name.clone(),
                album_title: album_title.clone(),
                artist_mbid: item.artist_mbid.clone(),
                year: item.year,
                release_mbid,
                monitor_artist: body.monitor_artist,
                auto_download_artist: body.auto_download_artist,
                ..fresh_record(
                    RequestKind::Album,
                    mbid.to_lowercase(),
                    principal,
                    DispatchOrigin::User,
                    status_for_role(needs_approval),
                    now,
                )
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
                    // candidate.
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
        let joined: Vec<String> = raced
            .iter()
            .cloned()
            .chain(existing_items.iter().map(|(mbid, _)| mbid.clone()))
            .collect();
        self.start_joined_keys(principal, &joined).await;

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

        let dispatched = self.dispatch_created(&created, user_id).await;
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

    /// File a discovery batch: its albums are asked for under the rules
    /// any album ask follows (quota, storage, approval, the chosen
    /// edition), albums someone already asked for are joined, and the
    /// batch is recorded with what each album came to, all in one write.
    /// Downloads start after it, as for any album ask; a held one waits
    /// for a source like any other.
    pub(crate) async fn file_discovery_batch(
        &self,
        principal: &Principal,
        batch: NewBatch,
    ) -> Result<BatchFiling, RequestsError> {
        let (user_id, role) = (principal.user_id(), principal.role());
        let needs_approval = !auto_approves(role);
        let now = self.now_epoch();
        let live = self.store.active_mbids(RequestKind::Album).await?;
        let mut joins = Vec::new();
        let mut asks = Vec::new();
        for item in &batch.items {
            if item.outcome != BatchOutcome::Requested {
                continue;
            }
            if live.contains(&item.release_group_mbid) {
                joins.push(item.release_group_mbid.clone());
            } else {
                asks.push(item);
            }
        }
        let gate = if asks.is_empty() {
            None
        } else {
            let gate = self.quota.request_gate(
                user_id,
                role,
                u32::try_from(asks.len()).unwrap_or(u32::MAX),
            )?;
            self.quota
                .check_storage_admission(user_id, role, DispatchOrigin::User)
                .await?;
            gate
        };
        let mut records = Vec::with_capacity(asks.len());
        for item in asks {
            let (Some(artist_name), Some(album_title)) = (
                meaningful_name(Some(&item.artist_name)),
                meaningful_name(Some(&item.album_name)),
            ) else {
                continue;
            };
            let release_mbid = self.chosen_release_or_none(&item.release_group_mbid).await;
            let artist_mbid = Some(item.artist_mbid.trim().to_owned()).filter(|id| !id.is_empty());
            records.push(RequestRecord {
                artist_name,
                album_title,
                artist_mbid,
                release_mbid,
                ..fresh_record(
                    RequestKind::Album,
                    item.release_group_mbid.clone(),
                    principal,
                    DispatchOrigin::User,
                    status_for_role(needs_approval),
                    now,
                )
            });
        }
        if records.is_empty() && joins.is_empty() {
            return Err(RequestsError::InvalidInput {
                message: "None of these albums could be requested: each one needs an artist and \
                          an album name."
                    .to_owned(),
            });
        }
        let asker = (user_id.to_owned(), principal.username_display().to_owned());
        let joined = joins.clone();
        match self
            .store
            .file_batch(records, joins, asker, gate, batch)
            .await?
        {
            BatchWrite::Taken { owner } => Ok(BatchFiling::Taken { owner }),
            BatchWrite::Refused(refusal) => Err(QuotaLedger::refusal(&refusal)),
            BatchWrite::NothingFiled => Err(RequestsError::InvalidInput {
                message: "None of these albums could be requested.".to_owned(),
            }),
            BatchWrite::Filed { won } => {
                for record in &won {
                    self.announce_created(record);
                }
                if !needs_approval {
                    self.dispatch_created(&won, user_id).await;
                }
                self.start_joined_keys(principal, &joined).await;
                Ok(BatchFiling::Filed)
            }
        }
    }

    /// Start the downloads of rows a batch ask won. One row's fault never
    /// fails its siblings. Answers how many started or wait to start.
    async fn dispatch_created(&self, created: &[RequestRecord], actor_id: &str) -> u32 {
        let mut dispatched = 0_u32;
        for won in created {
            match self.dispatch_won(won, DispatchOrigin::User, actor_id).await {
                Ok(_) => {
                    dispatched = dispatched.saturating_add(1);
                }
                Err(RequestsError::InvalidInput { .. }) => {
                    // Admission fault: the row already reads failed.
                }
                Err(error) => {
                    tracing::warn!(key = %won.key, ?error, "batch dispatch failed");
                }
            }
        }
        dispatched
    }

    /// Attach a duplicate album ask to its live row, with the follow it
    /// names, and report the row's own status (the duplicate reply
    /// mirrors the winner, including the awaiting-approval message). A
    /// curator asking for another edition switches the row to it while no
    /// download serves it; once one does, the row keeps its edition.
    async fn attach_album(
        &self,
        principal: &Principal,
        existing: &RequestRecord,
        follow: Option<FollowAsk>,
        asked: Option<&str>,
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
        let switch = self.edition_switch(principal, existing, asked).await?;
        if let Some(release) = switch
            && !self
                .store
                .switch_release(&existing.key, release, existing.generation)
                .await?
        {
            return Ok(IntakeResponse {
                success: true,
                message: "Already downloading, so the edition stays as it is".to_owned(),
                musicbrainz_id: existing.key.clone(),
                status: RequestStatus::Downloading.as_str().to_owned(),
                task_id: existing.task_id.clone(),
            });
        }
        // A row waiting for its owner's quota starts on the ask of someone
        // who needs no approval and has room of their own.
        if existing.waits_for_quota()
            && auto_approves(principal.role())
            && let Some(task_id) = self
                .start_joined(existing, principal, self.now_epoch())
                .await?
        {
            return Ok(accepted(&existing.key, Some(task_id)));
        }
        let message = if switch.is_some() {
            "The request now gets this edition"
        } else if existing.status == RequestStatus::AwaitingApproval {
            "Request is awaiting admin approval"
        } else if existing.status == RequestStatus::Cancelling {
            "Request is being cancelled"
        } else if existing.waits_for_quota() {
            "Request is waiting to start"
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

    /// The edition a curator's ask would switch a live row to: the asked
    /// one, when it differs from the row's and the library has not chosen
    /// one of its own (that edition always wins). A chosen edition that
    /// does not read switches nothing and answers the error.
    async fn edition_switch<'a>(
        &self,
        principal: &Principal,
        existing: &RequestRecord,
        asked: Option<&'a str>,
    ) -> Result<Option<&'a str>, RequestsError> {
        let Some(release) = asked else {
            return Ok(None);
        };
        if !principal.role().is_curator()
            || existing.kind != RequestKind::Album
            || existing.status == RequestStatus::Cancelling
            || existing.release_mbid.as_deref() == Some(release)
        {
            return Ok(None);
        }
        Ok(match self.chosen_release(&existing.key).await? {
            Some(_) => None,
            None => Some(release),
        })
    }

    /// Dispatch one won row and link its task, cancelling the
    /// orphan when the generation moved on. A busy database puts the row
    /// back to waiting, and [`Self::start_waiting`] starts it on its next
    /// pass; any other failure marks the row failed. Answers the task, or
    /// `None` while the row waits.
    async fn dispatch_won(
        &self,
        won: &RequestRecord,
        origin: DispatchOrigin,
        actor_id: &str,
    ) -> Result<Option<String>, RequestsError> {
        let owner = won.user_id.clone().unwrap_or_else(|| actor_id.to_owned());
        let request = stored_dispatch(won, origin, &owner);
        let now = self.now_epoch();
        let kind = won.kind.as_str();
        match self
            .dispatch_and_link(won.kind, &won.key, &request, won.generation)
            .await?
        {
            Dispatched::Linked(task_id) => Ok(Some(task_id)),
            Dispatched::Stale => Err(RequestsError::internal(&format_args!(
                "{kind} link lost its generation for {}",
                won.key
            ))),
            Dispatched::Failed(DispatchError::Busy { operation }) => {
                tracing::info!(key = %won.key, %operation, "database busy; the request waits to start");
                self.wait_to_start(won).await?;
                Ok(None)
            }
            Dispatched::Failed(cause) => {
                self.store
                    .update_status(
                        won.kind,
                        &won.key,
                        RequestStatus::Failed,
                        Some(now),
                        Some(won.generation),
                    )
                    .await?;
                Err(RequestsError::internal(&format_args!(
                    "{kind} dispatch failed: {cause}"
                )))
            }
        }
    }
}

/// The reply to an album ask whose download started, or waits to start
/// while the database is busy.
fn accepted(key: &str, task_id: Option<String>) -> IntakeResponse {
    let status = if task_id.is_some() {
        RequestStatus::Pending
    } else {
        RequestStatus::Queued
    };
    IntakeResponse {
        success: true,
        message: "Request accepted".to_owned(),
        musicbrainz_id: key.to_owned(),
        status: status.as_str().to_owned(),
        task_id,
    }
}
