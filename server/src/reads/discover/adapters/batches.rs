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
use std::sync::{Arc, OnceLock};

use sqlx::SqlitePool;

use crate::auth::users::{roles::Role, stores::UserStore};
use crate::db::{WriteLane, writer::Lane};
use crate::providers::musicbrainz::is_valid_mbid;
use crate::reads::catalog::library::LocalCatalog;
use crate::reads::discover::models::{
    DiscoveryBatchCreate, DiscoveryBatchDetail, DiscoveryBatchItemStatus,
    DiscoveryBatchRemoveResult, DiscoveryBatchSummary,
};
use crate::reads::discover::ports::{BatchFailure, BatchStore, BoxFuture, ProviderFailure};

/// Outcome of an album the batch filed a request for.
pub const OUTCOME_REQUESTED: &str = "requested";
/// Outcome of an album the library already held.
pub const OUTCOME_IN_LIBRARY: &str = "skipped_in_library";
/// Outcome of an album someone had already requested.
pub const OUTCOME_DUPLICATE: &str = "skipped_duplicate";

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
    /// Cancel the caller's pending request for one album (an admin
    /// cancels it outright). True when something was cancelled.
    fn cancel_album<'a>(
        &'a self,
        caller: &'a BatchCaller,
        release_group_mbid: &'a str,
    ) -> BoxFuture<'a, Result<bool, String>>;
    /// Move one album's files to the recycle bin and clear its download
    /// state. True when the library held it.
    fn remove_album<'a>(
        &'a self,
        release_group_mbid: &'a str,
        actor: &'a str,
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

    /// The albums the library holds, lowercased. A failed read is logged
    /// and treated as "nothing owned", as v2 did.
    async fn owned(&self, mbids: &[String]) -> HashSet<String> {
        let lowered: Vec<String> = mbids.iter().map(|id| id.to_ascii_lowercase()).collect();
        LocalCatalog::new(self.pool.clone())
            .owned_albums(&lowered)
            .await
            .unwrap_or_else(|error| {
                tracing::warn!(%error, "batch library check failed; treating albums as new");
                HashSet::new()
            })
    }

    async fn statuses(&self, items: &[BatchItem]) -> Result<Vec<DiscoveryBatchItemStatus>, String> {
        let ids: Vec<String> = items
            .iter()
            .map(|item| item.release_group_mbid.clone())
            .collect();
        let owned = self.owned(&ids).await;
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

    async fn create_batch(
        &self,
        user_id: &str,
        body: &DiscoveryBatchCreate,
    ) -> Result<DiscoveryBatchDetail, BatchFailure> {
        let requests = self.requests()?;
        let caller = self.caller(user_id).await?;
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
        let owned = self.owned(&ids).await;
        let active: HashSet<String> = LocalCatalog::new(self.pool.clone())
            .requested_albums(&ids)
            .await
            .map_err(|error| failed(format!("batch request check: {error}")))?;

        let mut rows = Vec::new();
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
        requests.request_albums(&caller, to_request).await?;

        let name = body.name.trim();
        let head = BatchHead {
            id: uuid::Uuid::new_v4().to_string(),
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
        let detail = self.detail_of(head).await?;
        let mut result = DiscoveryBatchRemoveResult {
            removed_albums: 0,
            cancelled_requests: 0,
            kept: 0,
        };
        if remove_albums {
            let requests = self.requests()?;
            for status in &detail.items {
                // Only albums this batch caused; skipped ones are never touched.
                if status.outcome != OUTCOME_REQUESTED {
                    result.kept += 1;
                    continue;
                }
                let mbid = status.release_group_mbid.as_str();
                if status.in_library {
                    match requests.remove_album(mbid, &caller.user_id).await {
                        Ok(true) => result.removed_albums += 1,
                        Ok(false) => result.kept += 1,
                        Err(cause) => {
                            tracing::warn!(album = mbid, %cause, "batch removal kept an album");
                            result.kept += 1;
                        }
                    }
                } else if status
                    .request_status
                    .as_deref()
                    .is_some_and(|state| PENDING_STATUSES.contains(&state))
                {
                    match requests.cancel_album(&caller, mbid).await {
                        Ok(true) => result.cancelled_requests += 1,
                        Ok(false) => result.kept += 1,
                        Err(cause) => {
                            tracing::warn!(album = mbid, %cause, "batch removal kept a request");
                            result.kept += 1;
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
                tx.execute(
                    "DELETE FROM discovery_batch_items WHERE batch_id = ?1",
                    rusqlite::params![id],
                )?;
                tx.execute(
                    "DELETE FROM discovery_batches WHERE id = ?1",
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
