//! Follows and new-release reads.
//!
//! Following an artist is instant. Enabling auto-download files a request:
//! trusted and admin accounts auto-approve, regular users wait in `pending`
//! for the admin approval reads in the approvals module. Auto-download
//! needs a follow first; unfollowing withdraws any pending request.
//! New-release sightings are fixture-seeded; no provider feeds them yet.

use axum::{
    Json,
    extract::{Path, State},
};

use super::{
    auth::Principal,
    error::{CollectionsError, ValidJson},
    models::{
        AutoDownloadBody, FollowBody, FollowStatusResponse, FollowedArtist,
        FollowedArtistListResponse, NewReleaseItem, NewReleaseListResponse, UnseenCountResponse,
    },
    state::{AutoDownloadState, CollectionsState, FollowRow, now_epoch, read_store, write_store},
};

/// Recent window for the recent-releases view: 30 days.
const RECENT_WINDOW_SECS: u64 = 30 * 24 * 3600;

/// Status answer for one follow row, or the unfollowed default.
fn status_for(artist_mbid: &str, row: Option<&FollowRow>) -> FollowStatusResponse {
    match row {
        Some(row) => FollowStatusResponse {
            artist_mbid: artist_mbid.to_owned(),
            followed: true,
            auto_download: row.auto_download,
            auto_download_state: row.auto_download_state.as_str().to_owned(),
        },
        None => FollowStatusResponse {
            artist_mbid: artist_mbid.to_owned(),
            followed: false,
            auto_download: false,
            auto_download_state: AutoDownloadState::Off.as_str().to_owned(),
        },
    }
}

/// Read one artist's follow status for the caller.
pub fn get_follow_status(
    state: &CollectionsState,
    caller: &Principal,
    artist_mbid: &str,
) -> Result<FollowStatusResponse, CollectionsError> {
    state.check_injection()?;
    let rows = read_store(&state.follows.follows, "follow")?;
    let key = (caller.user_id.clone(), artist_mbid.to_owned());
    Ok(status_for(artist_mbid, rows.get(&key)))
}

/// Follow or unfollow one artist. Unfollowing drops auto-download with the row.
pub fn set_follow(
    state: &CollectionsState,
    caller: &Principal,
    artist_mbid: &str,
    body: &FollowBody,
) -> Result<FollowStatusResponse, CollectionsError> {
    state.check_injection()?;
    let key = (caller.user_id.clone(), artist_mbid.to_owned());
    let mut rows = write_store(&state.follows.follows, "follow")?;
    if body.followed {
        let name = body.artist_name.clone().unwrap_or_else(|| {
            rows.get(&key)
                .map(|existing| existing.artist_name.clone())
                .unwrap_or_default()
        });
        let now = now_epoch();
        rows.entry(key.clone()).or_insert_with(|| FollowRow {
            user_id: caller.user_id.clone(),
            user_name: caller.display_name(),
            artist_mbid: artist_mbid.to_owned(),
            artist_name: name,
            auto_download: false,
            auto_download_state: AutoDownloadState::Off,
            followed_at: now,
            requested_at: None,
        });
    } else {
        rows.remove(&key);
    }
    Ok(status_for(artist_mbid, rows.get(&key)))
}

/// Turn auto-download on or off. Enabling needs a follow; the approval state
/// is active at once for trusted/admin, pending for regular users.
pub async fn set_auto_download(
    state: &CollectionsState,
    caller: &Principal,
    artist_mbid: &str,
    body: &AutoDownloadBody,
) -> Result<FollowStatusResponse, CollectionsError> {
    state.check_injection()?;
    let key = (caller.user_id.clone(), artist_mbid.to_owned());
    // The store guard lives only in this block: it must not be held
    // across the approval-store await below.
    let (pending_now, artist_name, response) = {
        let mut rows = write_store(&state.follows.follows, "follow")?;
        let row = rows.get_mut(&key).ok_or_else(|| {
            if body.enabled {
                CollectionsError::Conflict {
                    message: "Follow the artist before enabling auto-download".to_owned(),
                }
            } else {
                CollectionsError::NotFound
            }
        })?;
        if body.enabled {
            row.auto_download = true;
            row.requested_at = Some(now_epoch());
            row.auto_download_state = if caller.role.is_curator() {
                AutoDownloadState::Active
            } else {
                AutoDownloadState::Pending
            };
        } else {
            row.auto_download = false;
            row.auto_download_state = AutoDownloadState::Off;
            row.requested_at = None;
        }
        let pending_now = row.auto_download_state == AutoDownloadState::Pending;
        let artist_name = row.artist_name.clone();
        let response = status_for(artist_mbid, Some(row));
        (pending_now, artist_name, response)
    };
    // Mirror the verdict into the acquire approval store, when wired.
    if pending_now && let Some(sink) = &state.approval_seeds {
        sink.seed_approval(&caller.user_id, artist_mbid, &artist_name)
            .await
            .map_err(|cause| CollectionsError::internal(&cause))?;
    } else if !body.enabled
        && let Some(sink) = &state.approval_seeds
    {
        sink.withdraw_approval(&caller.user_id, artist_mbid)
            .await
            .map_err(|cause| CollectionsError::internal(&cause))?;
    }
    Ok(response)
}

/// List the caller's followed artists.
pub fn list_followed_artists(
    state: &CollectionsState,
    caller: &Principal,
) -> Result<FollowedArtistListResponse, CollectionsError> {
    state.check_injection()?;
    let rows = read_store(&state.follows.follows, "follow")?;
    let mut artists = rows
        .values()
        .filter(|row| row.user_id == caller.user_id)
        .map(|row| FollowedArtist {
            artist_mbid: row.artist_mbid.clone(),
            name: row.artist_name.clone(),
            auto_download: row.auto_download,
            auto_download_state: row.auto_download_state.as_str().to_owned(),
            followed_at: row.followed_at,
        })
        .collect::<Vec<_>>();
    artists.sort_by(|a, b| a.name.cmp(&b.name).then(a.artist_mbid.cmp(&b.artist_mbid)));
    Ok(FollowedArtistListResponse { artists })
}

/// Sightings for the caller's followed artists, newest first.
fn sightings_for_caller(
    state: &CollectionsState,
    caller: &Principal,
) -> Result<Vec<NewReleaseItem>, CollectionsError> {
    let follows = read_store(&state.follows.follows, "follow")?;
    let releases = read_store(&state.new_releases.releases, "new-release")?;
    let mut matching = releases
        .iter()
        .filter(|release| {
            follows.contains_key(&(caller.user_id.clone(), release.artist_mbid.clone()))
        })
        .collect::<Vec<_>>();
    matching.sort_by(|a, b| b.detected_at.cmp(&a.detected_at));
    let items = matching
        .into_iter()
        .map(|release| NewReleaseItem {
            release_group_mbid: release.release_group_mbid.clone(),
            title: release.title.clone(),
            artist_name: release.artist_name.clone(),
            artist_mbid: release.artist_mbid.clone(),
            primary_type: release.primary_type.clone(),
            first_release_date: release.first_release_date.clone(),
        })
        .collect::<Vec<_>>();
    Ok(items)
}

/// List new-release sightings for followed artists.
pub fn list_new_releases(
    state: &CollectionsState,
    caller: &Principal,
) -> Result<NewReleaseListResponse, CollectionsError> {
    state.check_injection()?;
    let items = sightings_for_caller(state, caller)?;
    let total = items.len();
    Ok(NewReleaseListResponse { items, total })
}

/// List sightings from the recent window for followed artists.
pub fn list_recent_releases(
    state: &CollectionsState,
    caller: &Principal,
) -> Result<NewReleaseListResponse, CollectionsError> {
    state.check_injection()?;
    let follows = read_store(&state.follows.follows, "follow")?;
    let releases = read_store(&state.new_releases.releases, "new-release")?;
    let cutoff = now_epoch().saturating_sub(RECENT_WINDOW_SECS);
    let mut matching = releases
        .iter()
        .filter(|release| {
            release.detected_at >= cutoff
                && follows.contains_key(&(caller.user_id.clone(), release.artist_mbid.clone()))
        })
        .collect::<Vec<_>>();
    matching.sort_by(|a, b| b.detected_at.cmp(&a.detected_at));
    let items = matching
        .into_iter()
        .map(|release| NewReleaseItem {
            release_group_mbid: release.release_group_mbid.clone(),
            title: release.title.clone(),
            artist_name: release.artist_name.clone(),
            artist_mbid: release.artist_mbid.clone(),
            primary_type: release.primary_type.clone(),
            first_release_date: release.first_release_date.clone(),
        })
        .collect::<Vec<_>>();
    let total = items.len();
    Ok(NewReleaseListResponse { items, total })
}

/// Count sightings newer than the caller's seen watermark.
pub fn unseen_count(
    state: &CollectionsState,
    caller: &Principal,
) -> Result<UnseenCountResponse, CollectionsError> {
    state.check_injection()?;
    let follows = read_store(&state.follows.follows, "follow")?;
    let releases = read_store(&state.new_releases.releases, "new-release")?;
    let seen = read_store(&state.new_releases.seen_at, "new-release")?;
    let watermark = seen.get(&caller.user_id).copied().unwrap_or(0);
    let count = releases
        .iter()
        .filter(|release| {
            release.detected_at > watermark
                && follows.contains_key(&(caller.user_id.clone(), release.artist_mbid.clone()))
        })
        .count();
    Ok(UnseenCountResponse { count })
}

/// Mark the caller's sightings seen. The watermark moves to now.
pub fn mark_seen(
    state: &CollectionsState,
    caller: &Principal,
) -> Result<UnseenCountResponse, CollectionsError> {
    state.check_injection()?;
    write_store(&state.new_releases.seen_at, "new-release")?
        .insert(caller.user_id.clone(), now_epoch());
    Ok(UnseenCountResponse { count: 0 })
}

/// Read one artist's follow status.
#[utoipa::path(
    get,
    path = "/api/v3/artists/{artist_mbid}/follow-status",
    params(("artist_mbid" = String, Path, description = "Artist MBID")),
    responses(
        (status = 200, description = "Follow status", body = FollowStatusResponse),
        (status = 401, description = "Not authenticated"),
    )
)]
pub async fn get_follow_status_handler(
    State(state): State<CollectionsState>,
    caller: Principal,
    Path(artist_mbid): Path<String>,
) -> Result<Json<FollowStatusResponse>, CollectionsError> {
    get_follow_status(&state, &caller, &artist_mbid).map(Json)
}

/// Follow or unfollow one artist.
#[utoipa::path(
    put,
    path = "/api/v3/artists/{artist_mbid}/follow",
    params(("artist_mbid" = String, Path, description = "Artist MBID")),
    request_body = FollowBody,
    responses(
        (status = 200, description = "Follow status", body = FollowStatusResponse),
        (status = 400, description = "Bad request body"),
        (status = 401, description = "Not authenticated"),
    )
)]
pub async fn set_follow_handler(
    State(state): State<CollectionsState>,
    caller: Principal,
    Path(artist_mbid): Path<String>,
    ValidJson(body): ValidJson<FollowBody>,
) -> Result<Json<FollowStatusResponse>, CollectionsError> {
    set_follow(&state, &caller, &artist_mbid, &body).map(Json)
}

/// Turn auto-download on or off for one artist.
#[utoipa::path(
    put,
    path = "/api/v3/artists/{artist_mbid}/auto-download",
    params(("artist_mbid" = String, Path, description = "Artist MBID")),
    request_body = AutoDownloadBody,
    responses(
        (status = 200, description = "Follow status", body = FollowStatusResponse),
        (status = 400, description = "Bad request body"),
        (status = 401, description = "Not authenticated"),
        (status = 404, description = "Artist not followed"),
        (status = 409, description = "Follow before enabling auto-download"),
    )
)]
pub async fn set_auto_download_handler(
    State(state): State<CollectionsState>,
    caller: Principal,
    Path(artist_mbid): Path<String>,
    ValidJson(body): ValidJson<AutoDownloadBody>,
) -> Result<Json<FollowStatusResponse>, CollectionsError> {
    set_auto_download(&state, &caller, &artist_mbid, &body)
        .await
        .map(Json)
}

/// List the caller's followed artists.
#[utoipa::path(
    get,
    path = "/api/v3/following/artists",
    responses(
        (status = 200, description = "Followed artists", body = FollowedArtistListResponse),
        (status = 401, description = "Not authenticated"),
    )
)]
pub async fn list_followed_artists_handler(
    State(state): State<CollectionsState>,
    caller: Principal,
) -> Result<Json<FollowedArtistListResponse>, CollectionsError> {
    list_followed_artists(&state, &caller).map(Json)
}

/// List new-release sightings for followed artists.
#[utoipa::path(
    get,
    path = "/api/v3/following/new-releases",
    responses(
        (status = 200, description = "New releases", body = NewReleaseListResponse),
        (status = 401, description = "Not authenticated"),
    )
)]
pub async fn list_new_releases_handler(
    State(state): State<CollectionsState>,
    caller: Principal,
) -> Result<Json<NewReleaseListResponse>, CollectionsError> {
    list_new_releases(&state, &caller).map(Json)
}

/// List recent-window sightings for followed artists.
#[utoipa::path(
    get,
    path = "/api/v3/following/new-releases/recent",
    responses(
        (status = 200, description = "Recent releases", body = NewReleaseListResponse),
        (status = 401, description = "Not authenticated"),
    )
)]
pub async fn list_recent_releases_handler(
    State(state): State<CollectionsState>,
    caller: Principal,
) -> Result<Json<NewReleaseListResponse>, CollectionsError> {
    list_recent_releases(&state, &caller).map(Json)
}

/// Count unseen sightings for the caller.
#[utoipa::path(
    get,
    path = "/api/v3/following/new-releases/unseen-count",
    responses(
        (status = 200, description = "Unseen count", body = UnseenCountResponse),
        (status = 401, description = "Not authenticated"),
    )
)]
pub async fn unseen_count_handler(
    State(state): State<CollectionsState>,
    caller: Principal,
) -> Result<Json<UnseenCountResponse>, CollectionsError> {
    unseen_count(&state, &caller).map(Json)
}

/// Mark the caller's sightings seen.
#[utoipa::path(
    post,
    path = "/api/v3/following/new-releases/seen",
    responses(
        (status = 200, description = "Zeroed count", body = UnseenCountResponse),
        (status = 401, description = "Not authenticated"),
    )
)]
pub async fn mark_seen_handler(
    State(state): State<CollectionsState>,
    caller: Principal,
) -> Result<Json<UnseenCountResponse>, CollectionsError> {
    mark_seen(&state, &caller).map(Json)
}
