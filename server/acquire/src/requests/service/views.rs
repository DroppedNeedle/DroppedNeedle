//! Reads: the active list, history, the approval queue and wanted
//! watches, each row with its linked task's progress.

use super::RequestsService;
use crate::downloads::state::TaskStatus;
use crate::requests::error::RequestsError;
use crate::requests::ledger::{RequestRecord, RequestStatus, validate_mbid};
use crate::requests::models::{
    ActiveCountResponse, ActiveRequestsResponse, HistoryQuery, HistoryResponse, RequestItem,
    RequestKind, WaitingReason, WantedActionResponse, WantedItem, WantedResponse,
    WantedRetryingItem,
};
use crate::requests::sqlite::WatchChange;
use droppedneedle_auth::{Admin, Principal};

/// History page-size cap.
const HISTORY_MAX_PAGE_SIZE: u32 = 100;

/// Map a guarded watch change to the action reply.
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
            release_mbid: record.release_mbid.clone(),
            requester_count: record.requesters.len() as u32,
        }
    }
}

impl RequestsService {
    /// Live rows. Admins see every row; anyone else sees their own.
    /// A read never writes: rows follow
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

    /// Rows waiting for review.
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
    /// plus personal-mix rows.
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
    /// the wanted view's read-only "still hunting" rows. A row
    /// graduates into a real watch the sweep
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

    /// Rows plus their linked tasks' progress, in order.
    async fn to_items(&self, records: &[RequestRecord]) -> Vec<RequestItem> {
        let mut items = Vec::with_capacity(records.len());
        for record in records {
            items.push(self.to_item(record).await);
        }
        items
    }

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

    /// One row plus its linked task's progress snapshot. Rows without a
    /// task (waiting for approval) read exactly as the record maps; a task
    /// read failure leaves the progress fields empty and logs why.
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
        // Only failed rows offer the admin reimport, and
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
