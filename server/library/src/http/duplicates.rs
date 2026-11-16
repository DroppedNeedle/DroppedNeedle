//! The duplicates page's route: albums the library holds more than once,
//! with the copies and the evidence that groups them. Read-only.

use axum::{Json, extract::State};
use droppedneedle_auth::Curator;
use droppedneedle_kernel::error::ApiError;
use droppedneedle_kernel::extract::ValidQuery;
use droppedneedle_kernel::mount::{Routes, routes};
use droppedneedle_kernel::page::{Page, PageRequest};
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};

use super::blocking;
use crate::duplicates::{
    DuplicateClass, DuplicateCopy, DuplicateEvidence, DuplicateGroup, Duplicates,
};
use crate::identity::DecisionSource;

/// Largest page of groups.
const PAGE_MAX: usize = 100;
const PAGE_DEFAULT: usize = 50;

pub fn routes() -> Routes<crate::wiring::LibraryServices> {
    Routes::new().curator(routes!(list_duplicate_albums))
}

/// Paging for the duplicates list.
#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct DuplicatesQuery {
    /// The previous page's `next_cursor`.
    pub cursor: Option<String>,
    /// Groups per page, 1 to 100 (50 by default); more is read as 100.
    pub limit: Option<u32>,
}

/// One album in a duplicate group.
#[derive(Debug, Serialize, ToSchema)]
pub struct DuplicateCopyView {
    /// The local album id; its page opens on it.
    pub album_id: String,
    pub title: String,
    pub artist: String,
    /// Indexed tracks the album holds.
    pub track_count: u64,
    /// Their bytes.
    pub total_size_bytes: u64,
    /// The distinct file formats of its indexed tracks, sorted.
    pub formats: Vec<String>,
    /// It has cover art of its own; otherwise the group's cover stands in.
    pub cover_available: bool,
    /// The release its match row names, when the row names this group.
    pub release_mbid: Option<String>,
    /// Who set that match.
    pub decision_source: Option<DecisionSource>,
    /// How the copy names its group: its match row or its files' tags.
    pub evidence: DuplicateEvidence,
    /// Its match is a guess its files contradict.
    pub doubted: bool,
}

impl From<&DuplicateCopy> for DuplicateCopyView {
    fn from(copy: &DuplicateCopy) -> Self {
        Self {
            album_id: copy.album_id.clone(),
            title: copy.title.clone(),
            artist: copy.artist.clone(),
            track_count: copy.track_count,
            total_size_bytes: copy.total_size_bytes,
            formats: copy.formats.clone(),
            cover_available: copy.cover_available,
            release_mbid: copy.release_mbid.clone(),
            decision_source: copy.decision_source,
            evidence: copy.evidence,
            doubted: copy.doubted,
        }
    }
}

/// Albums the library lists more than once.
#[derive(Debug, Serialize, ToSchema)]
pub struct DuplicateGroupView {
    /// The release group the copies share.
    pub id: String,
    /// The first copy's title, to name the group by.
    pub title: String,
    /// The first copy's artist.
    pub artist: String,
    /// What the copies show: a shared release, different releases, a copy
    /// with fewer tracks. Empty when none applies.
    pub classes: Vec<DuplicateClass>,
    pub copies: Vec<DuplicateCopyView>,
}

impl From<&DuplicateGroup> for DuplicateGroupView {
    fn from(group: &DuplicateGroup) -> Self {
        Self {
            id: group.id.clone(),
            title: group.title.clone(),
            artist: group.artist.clone(),
            classes: group.classes.clone(),
            copies: group.copies.iter().map(Into::into).collect(),
        }
    }
}

/// Albums the library holds more than once, a page of groups at a time.
#[utoipa::path(
    get,
    tag = "library management",
    path = "/api/v3/library/duplicates",
    params(DuplicatesQuery),
    responses(
        (status = 200, description = "One page of duplicate groups", body = Page<DuplicateGroupView>),
        (status = 400, description = "Bad cursor"),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Curator role required"),
    )
)]
pub async fn list_duplicate_albums(
    State(duplicates): State<Duplicates>,
    _caller: Curator,
    ValidQuery(query): ValidQuery<DuplicatesQuery>,
) -> Result<Json<Page<DuplicateGroupView>>, ApiError> {
    let paging =
        PageRequest::<String>::read(query.limit, query.cursor.as_deref(), PAGE_DEFAULT, PAGE_MAX)?;
    let page = blocking(move || duplicates.page(paging.limit, paging.after)).await?;
    Ok(Json(Page::new(
        page.groups.iter().map(Into::into).collect(),
        page.next,
    )))
}
