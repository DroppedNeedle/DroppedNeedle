//! Discovery batches: "download this whole shelf" as real album requests,
//! with a removal that only undoes what the batch itself caused.
//!
//! Nothing here is automatic: a batch exists because someone pressed
//! "Download all". Its albums are asked for through intake, so quotas,
//! approvals and the chosen edition apply exactly as for a single album
//! request, and the batch row, its albums and their requests land in one
//! write: a refusal files nothing and records no batch. The batch id is
//! the create's idempotency key, so a retried create answers the batch it
//! made. Albums the library holds, or that someone already requested, are
//! listed as skipped and never touched again. Removing a batch withdraws
//! the owner's still-pending asks and, for an admin, moves the albums it
//! brought in to the recycle bin.
//!
//! Request state and ownership are read live on every view, never copied
//! into the batch.

pub mod http;
pub mod models;
pub(crate) mod store;

use std::collections::HashSet;

use droppedneedle_auth::{Admin, Principal, Role};
use droppedneedle_catalog::reads::catalog::library::LocalCatalog;
use droppedneedle_catalog::reads::library::stores::StoreError;
use droppedneedle_library::mutations::MutationError;
use droppedneedle_providers::musicbrainz::is_valid_mbid;

use self::models::{
    BatchOutcome, DiscoveryBatchCreate, DiscoveryBatchDetail, DiscoveryBatchItemStatus,
    DiscoveryBatchRemoveResult, DiscoveryBatchSummary,
};
use self::store::{BatchHead, BatchItem, BatchStore, NewBatch};
use super::RequestsState;
use super::error::RequestsError;
use super::models::RequestKind;
use super::service::RequestsService;

/// Most albums in one batch.
pub const BATCH_MAX_ITEMS: usize = 30;
/// Most characters in a batch name.
pub const BATCH_NAME_MAX: usize = 200;
/// Most characters in a batch's source section.
pub const BATCH_SECTION_MAX: usize = 100;

/// Request states that mean "still cancellable, nothing on disk yet".
const PENDING_STATUSES: [&str; 4] = ["awaiting_approval", "pending", "queued", "downloading"];

/// What removing a batch did with one album.
enum Step {
    /// Its files went to the recycle bin.
    Removed,
    /// The owner's ask was withdrawn.
    Cancelled,
    /// Left as it was.
    Kept,
}

/// What filing a batch came to.
#[derive(Debug)]
pub(crate) enum BatchFiling {
    /// The batch and its requests were written.
    Filed,
    /// A batch with this id was there already, owned by `owner`.
    Taken { owner: String },
}

fn invalid(message: impl Into<String>) -> RequestsError {
    RequestsError::InvalidInput {
        message: message.into(),
    }
}

fn store_failure(error: StoreError) -> RequestsError {
    match error {
        StoreError::Busy(operation) => RequestsError::Busy { operation },
        StoreError::Internal(cause) => RequestsError::internal(&cause),
    }
}

/// A stored millisecond time as RFC 3339, as the routes answer it.
fn rfc3339(millis: i64) -> String {
    time::OffsetDateTime::from_unix_timestamp_nanos(i128::from(millis) * 1_000_000)
        .unwrap_or(time::OffsetDateTime::UNIX_EPOCH)
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_default()
}

/// Discovery batches over the requests state.
pub struct DiscoveryBatches {
    requests: RequestsState,
    store: BatchStore,
    catalog: LocalCatalog,
}

impl DiscoveryBatches {
    /// Batches over one requests state.
    pub fn new(requests: &RequestsState) -> Self {
        Self {
            store: BatchStore::new(requests.library.clone()),
            catalog: LocalCatalog::new(requests.library.db().clone()),
            requests: requests.clone(),
        }
    }

    /// The batch when the caller may see it: their own, or any for an
    /// admin. Anything else reads as missing.
    async fn visible(
        &self,
        caller: &Principal,
        batch_id: &str,
    ) -> Result<BatchHead, RequestsError> {
        match self.store.head(batch_id).await? {
            Some(head) if head.user_id == caller.user_id() || caller.is_admin() => Ok(head),
            _ => Err(RequestsError::NotFound),
        }
    }

    /// The albums the library holds, lowercased.
    async fn owned(&self, mbids: &[String]) -> Result<HashSet<String>, RequestsError> {
        let lowered: Vec<String> = mbids.iter().map(|id| id.to_ascii_lowercase()).collect();
        self.catalog
            .owned_albums(&lowered)
            .await
            .map_err(store_failure)
    }

    async fn statuses(
        &self,
        items: Vec<BatchItem>,
    ) -> Result<Vec<DiscoveryBatchItemStatus>, RequestsError> {
        let ids: Vec<String> = items
            .iter()
            .map(|item| item.release_group_mbid.clone())
            .collect();
        let owned = self.owned(&ids).await?;
        Ok(items
            .into_iter()
            .map(|item| DiscoveryBatchItemStatus {
                in_library: owned.contains(&item.release_group_mbid.to_ascii_lowercase()),
                musicbrainz_id: item.release_group_mbid,
                artist_id: item.artist_mbid,
                title: item.album_name,
                artist_name: item.artist_name,
                outcome: item.outcome,
                request_status: item.request_status,
            })
            .collect())
    }

    fn summary(head: &BatchHead, statuses: &[DiscoveryBatchItemStatus]) -> DiscoveryBatchSummary {
        let imported = statuses.iter().filter(|status| status.in_library).count();
        let pending = statuses
            .iter()
            .filter(|status| {
                status.outcome == BatchOutcome::Requested
                    && !status.in_library
                    && status
                        .request_status
                        .as_deref()
                        .is_some_and(|state| PENDING_STATUSES.contains(&state))
            })
            .count();
        DiscoveryBatchSummary {
            id: head.id.clone(),
            name: head.name.clone(),
            source_section: head.source_section.clone(),
            created_at: rfc3339(head.created_at),
            item_count: statuses.len() as i64,
            imported_count: imported as i64,
            pending_count: pending as i64,
        }
    }

    async fn detail_of(&self, head: BatchHead) -> Result<DiscoveryBatchDetail, RequestsError> {
        let items = self.store.items(&head.id).await?;
        let statuses = self.statuses(items).await?;
        let summary = Self::summary(&head, &statuses);
        Ok(DiscoveryBatchDetail {
            id: summary.id,
            name: summary.name,
            source_section: summary.source_section,
            created_at: summary.created_at,
            item_count: summary.item_count,
            imported_count: summary.imported_count,
            pending_count: summary.pending_count,
            items: statuses,
        })
    }

    /// The batch made under `key` for its owner; another user's key is
    /// refused.
    async fn made(
        &self,
        caller: &Principal,
        key: &str,
        owner: &str,
    ) -> Result<DiscoveryBatchDetail, RequestsError> {
        if owner != caller.user_id() {
            return Err(invalid("This idempotency key is already in use."));
        }
        let head = self.visible(caller, key).await?;
        self.detail_of(head).await
    }

    /// File the batch's albums as requests and record the batch, in one
    /// write.
    pub async fn create(
        &self,
        caller: &Principal,
        body: &DiscoveryBatchCreate,
    ) -> Result<DiscoveryBatchDetail, RequestsError> {
        if body.items.is_empty() {
            return Err(invalid("A discovery batch needs at least one album"));
        }
        if body.items.len() > BATCH_MAX_ITEMS {
            return Err(invalid(format!(
                "A discovery batch is capped at {BATCH_MAX_ITEMS} albums"
            )));
        }
        if body.name.chars().count() > BATCH_NAME_MAX {
            return Err(invalid(format!(
                "A batch name is at most {BATCH_NAME_MAX} characters"
            )));
        }
        if body.source_section.chars().count() > BATCH_SECTION_MAX {
            return Err(invalid(format!(
                "source_section is at most {BATCH_SECTION_MAX} characters"
            )));
        }
        // A retried create with the same key answers with the batch it
        // already made instead of filing again.
        let key = match body.idempotency_key.as_deref().map(str::trim) {
            Some(key) if !key.is_empty() => {
                let key = key.to_ascii_lowercase();
                if uuid::Uuid::parse_str(&key).is_err() {
                    return Err(invalid("idempotency_key must be a UUID"));
                }
                if let Some(head) = self.store.head(&key).await? {
                    return self.made(caller, &key, &head.user_id).await;
                }
                Some(key)
            }
            _ => None,
        };
        let mut seen = HashSet::new();
        let mut wanted = Vec::new();
        for item in &body.items {
            let id = item.musicbrainz_id.trim().to_ascii_lowercase();
            if !is_valid_mbid(&id) {
                return Err(invalid(format!(
                    "'{}' is not a MusicBrainz release-group id",
                    item.musicbrainz_id
                )));
            }
            if seen.insert(id.clone()) {
                wanted.push((id, item));
            }
        }
        let ids: Vec<String> = wanted.iter().map(|(id, _)| id.clone()).collect();
        // Fail closed: a batch must never request (and later recycle) an
        // album the library already holds because the check failed.
        let owned = self.owned(&ids).await?;
        let active = self
            .catalog
            .requested_albums(&ids)
            .await
            .map_err(store_failure)?;
        let items: Vec<BatchItem> = wanted
            .into_iter()
            .map(|(id, item)| BatchItem {
                outcome: if owned.contains(&id) {
                    BatchOutcome::SkippedInLibrary
                } else if active.contains(&id) {
                    BatchOutcome::SkippedDuplicate
                } else {
                    BatchOutcome::Requested
                },
                release_group_mbid: id,
                artist_mbid: item.artist_id.clone(),
                album_name: item.title.clone(),
                artist_name: item.artist_name.clone(),
                request_status: None,
            })
            .collect();
        if !items
            .iter()
            .any(|item| item.outcome == BatchOutcome::Requested)
        {
            return Err(invalid(
                "Every album in this batch is already in your library or already requested",
            ));
        }
        let name = body.name.trim();
        let head = BatchHead {
            id: key.unwrap_or_else(|| uuid::Uuid::new_v4().to_string()),
            user_id: caller.user_id().to_owned(),
            name: if name.is_empty() {
                "Discovery batch".to_owned()
            } else {
                name.to_owned()
            },
            source_section: body.source_section.clone(),
            created_at: self.requests.library.clock().now_ms(),
        };
        let batch = NewBatch {
            head: head.clone(),
            items,
        };
        match RequestsService::new(&self.requests)
            .file_discovery_batch(caller, batch)
            .await?
        {
            BatchFiling::Filed => self.detail_of(head).await,
            BatchFiling::Taken { owner } => self.made(caller, &head.id, &owner).await,
        }
    }

    /// The caller's batches, newest first.
    pub async fn list(&self, user_id: &str) -> Result<Vec<DiscoveryBatchSummary>, RequestsError> {
        let heads = self.store.heads_of(user_id).await?;
        let mut out = Vec::with_capacity(heads.len());
        for head in &heads {
            let items = self.store.items(&head.id).await?;
            let statuses = self.statuses(items).await?;
            out.push(Self::summary(head, &statuses));
        }
        Ok(out)
    }

    /// One batch with each album's current state.
    pub async fn detail(
        &self,
        caller: &Principal,
        batch_id: &str,
    ) -> Result<DiscoveryBatchDetail, RequestsError> {
        let head = self.visible(caller, batch_id).await?;
        self.detail_of(head).await
    }

    /// Withdraw the batch owner's ask for one album: the owner detaches
    /// when others share the request, and it is cancelled when it was
    /// only theirs. True when the owner's ask is gone.
    async fn withdraw(&self, owner_id: &str, mbid: &str) -> Result<bool, RequestsError> {
        // Act as the owner with plain rights, whoever removes the batch.
        let owner = Principal::acting_for(owner_id, Role::User, None);
        let ids = [mbid.to_owned()];
        RequestsService::new(&self.requests)
            .cancel_batch(&owner, &ids, RequestKind::Album)
            .await
            .map(|outcome| outcome.cancelled > 0)
    }

    /// Move one album's files to the recycle bin and clear its download
    /// state, as `admin`. True when the library held it.
    async fn recycle(&self, admin: &Admin, mbid: &str) -> Result<bool, RequestsError> {
        let Some(library) = self.requests.albums.clone() else {
            return Ok(false);
        };
        let (album, actor_id) = (mbid.to_owned(), admin.user_id().to_owned());
        let removed = tokio::task::spawn_blocking(move || {
            library.remove_album(&album, true, true, &actor_id)
        })
        .await
        .map_err(|error| RequestsError::internal(&format_args!("batch album removal: {error}")))?;
        match removed {
            Ok(_) => Ok(true),
            Err(MutationError::NotFound(_)) => Ok(false),
            Err(error) => Err(RequestsError::internal(&format_args!(
                "batch album removal: {error:?}"
            ))),
        }
    }

    /// Remove one batch, and with `remove_albums` the albums it caused:
    /// pending requests are withdrawn, landed albums go to the recycle bin
    /// when an admin removes it. Items whose step failed stay in the batch
    /// so the user can retry.
    pub async fn remove(
        &self,
        caller: &Principal,
        batch_id: &str,
        remove_albums: bool,
    ) -> Result<DiscoveryBatchRemoveResult, RequestsError> {
        let head = self.visible(caller, batch_id).await?;
        let owner = head.user_id.clone();
        let detail = self.detail_of(head).await?;
        let mut result = DiscoveryBatchRemoveResult {
            removed_albums: 0,
            cancelled_requests: 0,
            kept: 0,
        };
        let mut failed_items: Vec<String> = Vec::new();
        if remove_albums {
            // Recycling is for admins: anyone else's landed albums stay.
            let admin = Admin::try_from(caller.clone()).ok();
            for status in &detail.items {
                let mbid = status.musicbrainz_id.as_str();
                // Only albums this batch caused; skipped ones are never touched.
                let step = if status.outcome != BatchOutcome::Requested {
                    Ok(Step::Kept)
                } else if status.in_library {
                    match &admin {
                        // Never takes an album someone else asked for,
                        // favourited, rated, marked or played.
                        Some(admin) => match self.store.held_by_others(mbid, &owner).await {
                            Ok(false) => self
                                .recycle(admin, mbid)
                                .await
                                .map(|removed| if removed { Step::Removed } else { Step::Kept }),
                            Ok(true) => Ok(Step::Kept),
                            Err(error) => Err(error),
                        },
                        None => Ok(Step::Kept),
                    }
                } else if status
                    .request_status
                    .as_deref()
                    .is_some_and(|state| PENDING_STATUSES.contains(&state))
                {
                    // Withdraw the owner's ask only, even when an admin
                    // removes the batch: others may share the request.
                    self.withdraw(&owner, mbid).await.map(|withdrawn| {
                        if withdrawn {
                            Step::Cancelled
                        } else {
                            Step::Kept
                        }
                    })
                } else {
                    Ok(Step::Kept)
                };
                match step {
                    Ok(Step::Removed) => result.removed_albums += 1,
                    Ok(Step::Cancelled) => result.cancelled_requests += 1,
                    Ok(Step::Kept) => result.kept += 1,
                    Err(error) => {
                        tracing::warn!(album = mbid, ?error, "batch removal kept an album");
                        result.kept += 1;
                        failed_items.push(mbid.to_owned());
                    }
                }
            }
        } else {
            result.kept = detail.item_count;
        }
        let items = detail
            .items
            .into_iter()
            .map(|status| status.musicbrainz_id)
            .collect();
        self.store.forget(&detail.id, items, failed_items).await?;
        Ok(result)
    }
}
