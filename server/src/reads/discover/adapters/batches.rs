//! Discovery batches (v2 `DiscoveryBatchService`): "download this whole
//! shelf" as real album requests, with a removal that only undoes what
//! the batch itself caused.
//!
//! Nothing here is automatic: a batch exists because someone pressed
//! "Download all". The albums are filed through the requests service
//! ([`BatchRequests`]), so quotas, approvals and the chosen edition apply
//! exactly as for a single album request; a refusal files nothing and
//! records no batch. Albums the library holds, or that someone already
//! requested, are listed as skipped and never touched again. Removing a
//! batch cancels its still-pending requests and moves the albums it
//! brought in to the recycle bin.
//!
//! The rows live in `discovery_batches` and `discovery_batch_items`
//! (baseline schema, v2 shape). Request state and ownership are read live
//! on every view, never copied into the batch.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, OnceLock};

use sqlx::SqlitePool;

use crate::auth::users::{roles::Role, stores::UserStore};
use crate::providers::musicbrainz::is_valid_mbid;
use crate::reads::catalog::library::LocalCatalog;
use crate::reads::discover::models::{
    DiscoveryBatchCreate, DiscoveryBatchDetail, DiscoveryBatchItemStatus,
    DiscoveryBatchRemoveResult, DiscoveryBatchSummary,
};
use crate::reads::discover::ports::{BatchFailure, BatchStore, BoxFuture, ProviderFailure};
use droppedneedle_kernel::db::{WriteLane, writer::Lane};

/// Outcome of an album the batch filed a request for.
pub const OUTCOME_REQUESTED: &str = "requested";
/// Outcome of an album the library already held.
pub const OUTCOME_IN_LIBRARY: &str = "skipped_in_library";
/// Outcome of an album someone had already requested.
pub const OUTCOME_DUPLICATE: &str = "skipped_duplicate";
/// Outcome of an album the requests service did not file (no usable
/// names, or someone else's request won a race).
pub const OUTCOME_NOT_REQUESTED: &str = "not_requested";

/// Request states that mean "still cancellable, nothing on disk yet".
const PENDING_STATUSES: [&str; 4] = ["awaiting_approval", "pending", "queued", "downloading"];

/// Who is asking, resolved fresh from the user store.
#[derive(Debug, Clone)]
pub struct BatchCaller {
    /// User id.
    pub user_id: String,
    /// Login name, when the account has one.
    pub username: Option<String>,
    /// Current role.
    pub role: Role,
}

/// One album the batch asks for.
#[derive(Debug, Clone)]
pub struct BatchAlbum {
    /// MusicBrainz release-group id.
    pub release_group_mbid: String,
    /// Artist name.
    pub artist_name: String,
    /// Album title.
    pub album_title: String,
    /// MusicBrainz artist id, when known.
    pub artist_mbid: Option<String>,
}

/// The acquisition side of a batch: filing, cancelling and removing
/// albums. Production wires the requests service and the library behind
/// it once both are built.
pub trait BatchRequests: Send + Sync {
    /// File these albums as requests for the caller, under the normal
    /// quota and approval rules. A refusal files nothing.
    fn request_albums<'a>(
        &'a self,
        caller: &'a BatchCaller,
        albums: Vec<BatchAlbum>,
    ) -> BoxFuture<'a, Result<(), BatchFailure>>;
    /// Withdraw the batch owner's ask for one album: the owner detaches
    /// when others share the request, and it is cancelled when it was
    /// only theirs. Never cancels a request someone else still wants.
    /// True when the owner's ask is gone.
    fn withdraw_album<'a>(
        &'a self,
        owner_id: &'a str,
        release_group_mbid: &'a str,
    ) -> BoxFuture<'a, Result<bool, String>>;
    /// Move one album's files to the recycle bin and clear its download
    /// state. Admins only: anyone else is refused. True when the library
    /// held it.
    fn remove_album<'a>(
        &'a self,
        caller: &'a BatchCaller,
        release_group_mbid: &'a str,
    ) -> BoxFuture<'a, Result<bool, String>>;
}

/// Where boot puts the acquisition side once it exists.
pub type RequestsSlot = Arc<OnceLock<Arc<dyn BatchRequests>>>;

/// One stored batch row.
struct BatchHead {
    id: String,
    user_id: String,
    name: String,
    source_section: String,
    created_at: String,
}

/// One stored album row.
struct BatchItem {
    release_group_mbid: String,
    artist_mbid: String,
    album_name: String,
    artist_name: String,
    outcome: String,
}

/// Batches over the catalog database.
pub struct LiveBatches {
    pool: SqlitePool,
    lane: WriteLane,
    users: Arc<dyn UserStore>,
    requests: RequestsSlot,
    /// Idempotency keys of creates in progress, so a retried "Download
    /// all" never files the same batch twice at once.
    creating: Mutex<HashSet<String>>,
}

/// Clears an in-progress idempotency key however the create ends.
struct CreatingGuard<'a> {
    keys: &'a Mutex<HashSet<String>>,
    key: String,
}

impl Drop for CreatingGuard<'_> {
    fn drop(&mut self) {
        if let Ok(mut keys) = self.keys.lock() {
            keys.remove(&self.key);
        }
    }
}

fn failed(cause: String) -> BatchFailure {
    BatchFailure::Provider(ProviderFailure::failed(cause))
}

fn now_rfc3339() -> String {
    time::OffsetDateTime::now_utc()
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_default()
}

fn placeholders(count: usize) -> String {
    vec!["?"; count].join(", ")
}

impl LiveBatches {
    /// Batches over this database, filing through `requests` once boot
    /// fills the slot.
    pub fn new(
        pool: SqlitePool,
        lane: WriteLane,
        users: Arc<dyn UserStore>,
        requests: RequestsSlot,
    ) -> Self {
        Self {
            pool,
            lane,
            users,
            requests,
            creating: Mutex::new(HashSet::new()),
        }
    }

    fn requests(&self) -> Result<&Arc<dyn BatchRequests>, BatchFailure> {
        self.requests.get().ok_or_else(|| {
            BatchFailure::Provider(ProviderFailure::not_built(
                "Discovery batches need the download system, which is not running.",
            ))
        })
    }

    async fn caller(&self, user_id: &str) -> Result<BatchCaller, BatchFailure> {
        match self.users.get_by_id(user_id).await {
            Ok(Some(user)) => Ok(BatchCaller {
                user_id: user.id,
                username: user.username,
                role: user.role,
            }),
            // The session outlived its account: nothing is theirs to see.
            Ok(None) => Err(BatchFailure::NotFound),
            Err(error) => Err(failed(format!("batch caller read: {error:?}"))),
        }
    }

    /// The batch when the caller may see it: their own, or any for an
    /// admin. Anything else reads as missing.
    async fn visible(
        &self,
        caller: &BatchCaller,
        batch_id: &str,
    ) -> Result<BatchHead, BatchFailure> {
        let row: Option<(String, String, String, String, String)> = sqlx::query_as(
            "SELECT id, user_id, name, source_section, created_at FROM discovery_batches \
             WHERE id = ?1",
        )
        .bind(batch_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(|error| failed(format!("batch read: {error}")))?;
        let Some((id, user_id, name, source_section, created_at)) = row else {
            return Err(BatchFailure::NotFound);
        };
        if user_id != caller.user_id && caller.role != Role::Admin {
            return Err(BatchFailure::NotFound);
        }
        Ok(BatchHead {
            id,
            user_id,
            name,
            source_section,
            created_at,
        })
    }

    async fn items(&self, batch_ids: &[String]) -> Result<HashMap<String, Vec<BatchItem>>, String> {
        let mut grouped: HashMap<String, Vec<BatchItem>> = HashMap::new();
        for chunk in batch_ids.chunks(400) {
            let sql = format!(
                "SELECT batch_id, release_group_mbid, artist_mbid, album_name, artist_name, \
                 outcome FROM discovery_batch_items WHERE batch_id IN ({}) \
                 ORDER BY added_at, album_name",
                placeholders(chunk.len())
            );
            let mut query =
                sqlx::query_as::<_, (String, String, String, String, String, String)>(&sql);
            for id in chunk {
                query = query.bind(id);
            }
            let rows = query
                .fetch_all(&self.pool)
                .await
                .map_err(|error| format!("batch items read: {error}"))?;
            for (batch_id, release_group_mbid, artist_mbid, album_name, artist_name, outcome) in
                rows
            {
                grouped.entry(batch_id).or_default().push(BatchItem {
                    release_group_mbid,
                    artist_mbid,
                    album_name,
                    artist_name,
                    outcome,
                });
            }
        }
        Ok(grouped)
    }

    /// The live request state of each album, keyed by lowercased id.
    async fn request_states(&self, mbids: &[String]) -> Result<HashMap<String, String>, String> {
        let mut states = HashMap::new();
        for chunk in mbids.chunks(400) {
            let sql = format!(
                "SELECT musicbrainz_id_lower, status FROM request_history \
                 WHERE request_kind = 'album' AND musicbrainz_id_lower IN ({})",
                placeholders(chunk.len())
            );
            let mut query = sqlx::query_as::<_, (String, String)>(&sql);
            for mbid in chunk {
                query = query.bind(mbid.to_ascii_lowercase());
            }
            let rows = query
                .fetch_all(&self.pool)
                .await
                .map_err(|error| format!("batch request states read: {error}"))?;
            states.extend(rows);
        }
        Ok(states)
    }

    /// The albums the library holds, lowercased.
    async fn owned(&self, mbids: &[String]) -> Result<HashSet<String>, String> {
        let lowered: Vec<String> = mbids.iter().map(|id| id.to_ascii_lowercase()).collect();
        LocalCatalog::new(self.pool.clone())
            .owned_albums(&lowered)
            .await
            .map_err(|error| format!("batch library check: {error}"))
    }

    /// The albums whose live request belongs to `owner` or that `owner`
    /// joined: what filing actually did for them.
    async fn filed_for(&self, owner: &str, mbids: &[String]) -> Result<HashSet<String>, String> {
        let mut filed = HashSet::new();
        for chunk in mbids.chunks(400) {
            let sql = format!(
                "SELECT h.musicbrainz_id_lower FROM request_history h \
                 WHERE h.request_kind = 'album' \
                 AND h.status IN ('pending','downloading','queued','awaiting_approval') \
                 AND h.musicbrainz_id_lower IN ({}) \
                 AND (h.user_id = ? OR EXISTS (SELECT 1 FROM request_history_requesters r \
                 WHERE r.musicbrainz_id_lower = h.musicbrainz_id_lower AND r.user_id = ?))",
                placeholders(chunk.len())
            );
            let mut query = sqlx::query_scalar::<_, String>(&sql);
            for mbid in chunk {
                query = query.bind(mbid.to_ascii_lowercase());
            }
            let rows = query
                .bind(owner)
                .bind(owner)
                .fetch_all(&self.pool)
                .await
                .map_err(|error| format!("batch filing read: {error}"))?;
            filed.extend(rows);
        }
        Ok(filed)
    }

    /// Whether anyone but `owner` requested, favourited or played this
    /// album. Such an album is never recycled with a batch.
    async fn held_by_others(&self, mbid: &str, owner: &str) -> Result<bool, String> {
        sqlx::query_scalar::<_, bool>(
            "SELECT EXISTS (SELECT 1 FROM request_history WHERE request_kind = 'album' \
                 AND musicbrainz_id_lower = ?1 AND user_id IS NOT NULL AND user_id != ?2) \
             OR EXISTS (SELECT 1 FROM request_history_requesters \
                 WHERE musicbrainz_id_lower = ?1 AND user_id != ?2) \
             OR EXISTS (SELECT 1 FROM library_user_favorites f WHERE f.user_id != ?2 AND ( \
                 (f.item_kind = 'album' AND f.item_id IN (SELECT e.local_album_id \
                     FROM local_album_external_identities e \
                     WHERE lower(e.release_group_mbid) = ?1)) \
                 OR (f.item_kind = 'track' AND f.item_id IN (SELECT t.id FROM local_tracks t \
                     JOIN local_album_external_identities e ON e.local_album_id = t.local_album_id \
                     WHERE lower(e.release_group_mbid) = ?1)))) \
             OR EXISTS (SELECT 1 FROM library_play_history p WHERE p.user_id != ?2 AND ( \
                 lower(p.release_group_mbid) = ?1 OR p.local_album_id IN (SELECT e.local_album_id \
                     FROM local_album_external_identities e \
                     WHERE lower(e.release_group_mbid) = ?1)))",
        )
        .bind(mbid.to_ascii_lowercase())
        .bind(owner)
        .fetch_one(&self.pool)
        .await
        .map_err(|error| format!("batch sharing check: {error}"))
    }

    async fn statuses(&self, items: &[BatchItem]) -> Result<Vec<DiscoveryBatchItemStatus>, String> {
        let ids: Vec<String> = items
            .iter()
            .map(|item| item.release_group_mbid.clone())
            .collect();
        let owned = self.owned(&ids).await?;
        let states = self.request_states(&ids).await?;
        Ok(items
            .iter()
            .map(|item| {
                let key = item.release_group_mbid.to_ascii_lowercase();
                DiscoveryBatchItemStatus {
                    release_group_mbid: item.release_group_mbid.clone(),
                    artist_mbid: item.artist_mbid.clone(),
                    album_name: item.album_name.clone(),
                    artist_name: item.artist_name.clone(),
                    outcome: item.outcome.clone(),
                    request_status: states.get(&key).cloned(),
                    in_library: owned.contains(&key),
                }
            })
            .collect())
    }

    fn summary(head: &BatchHead, statuses: &[DiscoveryBatchItemStatus]) -> DiscoveryBatchSummary {
        let imported = statuses.iter().filter(|status| status.in_library).count();
        let pending = statuses
            .iter()
            .filter(|status| {
                status.outcome == OUTCOME_REQUESTED
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
            created_at: head.created_at.clone(),
            item_count: statuses.len() as i64,
            imported_count: imported as i64,
            pending_count: pending as i64,
        }
    }

    async fn detail_of(&self, head: BatchHead) -> Result<DiscoveryBatchDetail, BatchFailure> {
        let mut items = self
            .items(std::slice::from_ref(&head.id))
            .await
            .map_err(failed)?;
        let items = items.remove(&head.id).unwrap_or_default();
        let statuses = self.statuses(&items).await.map_err(failed)?;
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

    async fn record(&self, head: &BatchHead, items: Vec<BatchItem>) -> Result<(), String> {
        let (id, user_id, name, section, created_at) = (
            head.id.clone(),
            head.user_id.clone(),
            head.name.clone(),
            head.source_section.clone(),
            head.created_at.clone(),
        );
        self.lane
            .write(Lane::Foreground, "discover.batches.create", move |tx| {
                tx.execute(
                    "INSERT INTO discovery_batches (id, user_id, name, source_section, \
                     created_at) VALUES (?1, ?2, ?3, ?4, ?5)",
                    rusqlite::params![id, user_id, name, section, created_at],
                )?;
                for item in &items {
                    tx.execute(
                        "INSERT OR REPLACE INTO discovery_batch_items (batch_id, \
                         release_group_mbid, artist_mbid, album_name, artist_name, outcome, \
                         added_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                        rusqlite::params![
                            id,
                            item.release_group_mbid,
                            item.artist_mbid,
                            item.album_name,
                            item.artist_name,
                            item.outcome,
                            created_at,
                        ],
                    )?;
                }
                Ok(())
            })
            .await
            .map_err(|error| format!("batch write: {error}"))
    }

    /// The caller's batch already made under this idempotency key.
    async fn existing(
        &self,
        caller: &BatchCaller,
        key: &str,
    ) -> Result<Option<DiscoveryBatchDetail>, BatchFailure> {
        let owner: Option<String> =
            sqlx::query_scalar("SELECT user_id FROM discovery_batches WHERE id = ?1")
                .bind(key)
                .fetch_optional(&self.pool)
                .await
                .map_err(|error| failed(format!("batch key read: {error}")))?;
        match owner {
            None => Ok(None),
            Some(owner) if owner == caller.user_id => {
                let head = self.visible(caller, key).await?;
                self.detail_of(head).await.map(Some)
            }
            Some(_) => Err(BatchFailure::Invalid(
                "This idempotency key is already in use.".to_owned(),
            )),
        }
    }

    async fn create_batch(
        &self,
        user_id: &str,
        body: &DiscoveryBatchCreate,
    ) -> Result<DiscoveryBatchDetail, BatchFailure> {
        let requests = self.requests()?;
        let caller = self.caller(user_id).await?;
        // A retried create with the same key answers with the batch it
        // already made instead of filing again.
        let key = match body.idempotency_key.as_deref().map(str::trim) {
            Some(key) if !key.is_empty() => {
                let key = key.to_ascii_lowercase();
                if uuid::Uuid::parse_str(&key).is_err() {
                    return Err(BatchFailure::Invalid(
                        "idempotency_key must be a UUID".to_owned(),
                    ));
                }
                Some(key)
            }
            _ => None,
        };
        let _guard = match &key {
            Some(key) => {
                if let Some(existing) = self.existing(&caller, key).await? {
                    return Ok(existing);
                }
                let mut creating = self
                    .creating
                    .lock()
                    .map_err(|_| failed("batch create guard poisoned".to_owned()))?;
                if !creating.insert(key.clone()) {
                    return Err(BatchFailure::Invalid(
                        "This batch is already being created; try again in a moment.".to_owned(),
                    ));
                }
                Some(CreatingGuard {
                    keys: &self.creating,
                    key: key.clone(),
                })
            }
            None => None,
        };
        let mut seen = HashSet::new();
        let mut wanted = Vec::new();
        for item in &body.items {
            let id = item.release_group_mbid.trim().to_ascii_lowercase();
            if !is_valid_mbid(&id) {
                return Err(BatchFailure::Invalid(format!(
                    "'{}' is not a MusicBrainz release-group id",
                    item.release_group_mbid
                )));
            }
            if seen.insert(id.clone()) {
                wanted.push((id, item));
            }
        }
        let ids: Vec<String> = wanted.iter().map(|(id, _)| id.clone()).collect();
        // Fail closed: a batch must never request (and later recycle) an
        // album the library already holds because the check failed.
        let owned = self.owned(&ids).await.map_err(failed)?;
        let active: HashSet<String> = LocalCatalog::new(self.pool.clone())
            .requested_albums(&ids)
            .await
            .map_err(|error| failed(format!("batch request check: {error}")))?;

        let mut rows: Vec<BatchItem> = Vec::new();
        let mut to_request = Vec::new();
        for (id, item) in wanted {
            let outcome = if owned.contains(&id) {
                OUTCOME_IN_LIBRARY
            } else if active.contains(&id) {
                OUTCOME_DUPLICATE
            } else {
                to_request.push(BatchAlbum {
                    release_group_mbid: id.clone(),
                    artist_name: item.artist_name.trim().to_owned(),
                    album_title: item.album_name.trim().to_owned(),
                    artist_mbid: Some(item.artist_mbid.trim().to_owned())
                        .filter(|mbid| !mbid.is_empty()),
                });
                OUTCOME_REQUESTED
            };
            rows.push(BatchItem {
                release_group_mbid: id,
                artist_mbid: item.artist_mbid.clone(),
                album_name: item.album_name.clone(),
                artist_name: item.artist_name.clone(),
                outcome: outcome.to_owned(),
            });
        }
        if to_request.is_empty() {
            return Err(BatchFailure::Invalid(
                "Every album in this batch is already in your library or already requested"
                    .to_owned(),
            ));
        }
        // Quota and role rules live in the requests service; a refusal
        // propagates and no batch row is written.
        let asked: Vec<String> = to_request
            .iter()
            .map(|album| album.release_group_mbid.clone())
            .collect();
        requests.request_albums(&caller, to_request).await?;
        // Record what filing actually did for each album, not what the
        // check above expected.
        let filed = self
            .filed_for(&caller.user_id, &asked)
            .await
            .map_err(failed)?;
        for row in &mut rows {
            if row.outcome == OUTCOME_REQUESTED && !filed.contains(&row.release_group_mbid) {
                row.outcome = OUTCOME_NOT_REQUESTED.to_owned();
            }
        }
        if !rows.iter().any(|row| row.outcome == OUTCOME_REQUESTED) {
            return Err(BatchFailure::Invalid(
                "None of these albums could be requested.".to_owned(),
            ));
        }

        let name = body.name.trim();
        let head = BatchHead {
            id: key.unwrap_or_else(|| uuid::Uuid::new_v4().to_string()),
            user_id: caller.user_id.clone(),
            name: if name.is_empty() {
                "Discovery batch".to_owned()
            } else {
                name.to_owned()
            },
            source_section: body.source_section.clone(),
            created_at: now_rfc3339(),
        };
        self.record(&head, rows).await.map_err(failed)?;
        self.detail_of(head).await
    }

    async fn list_batches(
        &self,
        user_id: &str,
    ) -> Result<Vec<DiscoveryBatchSummary>, BatchFailure> {
        let rows: Vec<(String, String, String, String, String)> = sqlx::query_as(
            "SELECT id, user_id, name, source_section, created_at FROM discovery_batches \
             WHERE user_id = ?1 ORDER BY created_at DESC",
        )
        .bind(user_id)
        .fetch_all(&self.pool)
        .await
        .map_err(|error| failed(format!("batch list read: {error}")))?;
        let heads: Vec<BatchHead> = rows
            .into_iter()
            .map(
                |(id, user_id, name, source_section, created_at)| BatchHead {
                    id,
                    user_id,
                    name,
                    source_section,
                    created_at,
                },
            )
            .collect();
        let ids: Vec<String> = heads.iter().map(|head| head.id.clone()).collect();
        let mut items = self.items(&ids).await.map_err(failed)?;
        let mut out = Vec::with_capacity(heads.len());
        for head in &heads {
            let batch_items = items.remove(&head.id).unwrap_or_default();
            let statuses = self.statuses(&batch_items).await.map_err(failed)?;
            out.push(Self::summary(head, &statuses));
        }
        Ok(out)
    }

    async fn remove_batch(
        &self,
        user_id: &str,
        batch_id: &str,
        remove_albums: bool,
    ) -> Result<DiscoveryBatchRemoveResult, BatchFailure> {
        let caller = self.caller(user_id).await?;
        let head = self.visible(&caller, batch_id).await?;
        let owner = head.user_id.clone();
        let detail = self.detail_of(head).await?;
        let mut result = DiscoveryBatchRemoveResult {
            removed_albums: 0,
            cancelled_requests: 0,
            kept: 0,
        };
        // Items whose step failed stay in the batch so the user can retry.
        let mut failed_items: Vec<String> = Vec::new();
        if remove_albums {
            let requests = self.requests()?;
            for status in &detail.items {
                let mbid = status.release_group_mbid.as_str();
                // Only albums this batch caused; skipped ones are never touched.
                if status.outcome != OUTCOME_REQUESTED {
                    result.kept += 1;
                } else if status.in_library {
                    // Recycling is for admins, and never takes an album
                    // someone else asked for, favourited or played.
                    if caller.role != Role::Admin {
                        result.kept += 1;
                        continue;
                    }
                    match self.held_by_others(mbid, &owner).await {
                        Ok(false) => {}
                        Ok(true) => {
                            result.kept += 1;
                            continue;
                        }
                        Err(cause) => {
                            tracing::warn!(album = mbid, %cause, "batch removal kept an album");
                            result.kept += 1;
                            failed_items.push(mbid.to_owned());
                            continue;
                        }
                    }
                    match requests.remove_album(&caller, mbid).await {
                        Ok(true) => result.removed_albums += 1,
                        Ok(false) => result.kept += 1,
                        Err(cause) => {
                            tracing::warn!(album = mbid, %cause, "batch removal kept an album");
                            result.kept += 1;
                            failed_items.push(mbid.to_owned());
                        }
                    }
                } else if status
                    .request_status
                    .as_deref()
                    .is_some_and(|state| PENDING_STATUSES.contains(&state))
                {
                    // Withdraw the owner's ask only, even when an admin
                    // removes the batch: others may share the request.
                    match requests.withdraw_album(&owner, mbid).await {
                        Ok(true) => result.cancelled_requests += 1,
                        Ok(false) => result.kept += 1,
                        Err(cause) => {
                            tracing::warn!(album = mbid, %cause, "batch removal kept a request");
                            result.kept += 1;
                            failed_items.push(mbid.to_owned());
                        }
                    }
                } else {
                    result.kept += 1;
                }
            }
        } else {
            result.kept = detail.item_count;
        }
        let id = detail.id.clone();
        self.lane
            .write(Lane::Foreground, "discover.batches.delete", move |tx| {
                let mut keep = String::new();
                for (index, _) in failed_items.iter().enumerate() {
                    keep.push_str(if index == 0 { "?" } else { ", ?" });
                }
                let sql = if failed_items.is_empty() {
                    "DELETE FROM discovery_batch_items WHERE batch_id = ?".to_owned()
                } else {
                    format!(
                        "DELETE FROM discovery_batch_items WHERE batch_id = ? \
                         AND release_group_mbid NOT IN ({keep})"
                    )
                };
                let binds: Vec<&dyn rusqlite::ToSql> = std::iter::once(&id as &dyn rusqlite::ToSql)
                    .chain(failed_items.iter().map(|mbid| mbid as &dyn rusqlite::ToSql))
                    .collect();
                tx.execute(&sql, binds.as_slice())?;
                tx.execute(
                    "DELETE FROM discovery_batches WHERE id = ?1 AND NOT EXISTS \
                     (SELECT 1 FROM discovery_batch_items WHERE batch_id = ?1)",
                    rusqlite::params![id],
                )?;
                Ok(())
            })
            .await
            .map_err(|error| failed(format!("batch delete: {error}")))?;
        Ok(result)
    }
}

impl BatchStore for LiveBatches {
    fn create<'a>(
        &'a self,
        user_id: &'a str,
        body: &'a DiscoveryBatchCreate,
    ) -> BoxFuture<'a, Result<DiscoveryBatchDetail, BatchFailure>> {
        Box::pin(self.create_batch(user_id, body))
    }

    fn list<'a>(
        &'a self,
        user_id: &'a str,
    ) -> BoxFuture<'a, Result<Vec<DiscoveryBatchSummary>, BatchFailure>> {
        Box::pin(self.list_batches(user_id))
    }

    fn detail<'a>(
        &'a self,
        user_id: &'a str,
        batch_id: &'a str,
    ) -> BoxFuture<'a, Result<DiscoveryBatchDetail, BatchFailure>> {
        Box::pin(async move {
            let caller = self.caller(user_id).await?;
            let head = self.visible(&caller, batch_id).await?;
            self.detail_of(head).await
        })
    }

    fn remove<'a>(
        &'a self,
        user_id: &'a str,
        batch_id: &'a str,
        remove_albums: bool,
    ) -> BoxFuture<'a, Result<DiscoveryBatchRemoveResult, BatchFailure>> {
        Box::pin(self.remove_batch(user_id, batch_id, remove_albums))
    }
}
