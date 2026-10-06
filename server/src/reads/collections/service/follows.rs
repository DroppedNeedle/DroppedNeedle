//! Follows and new-release reads.
//!
//! Following an artist is instant. Enabling auto-download files an ask:
//! trusted and admin accounts approve themselves, regular users wait in
//! `pending` for an admin verdict (`acquire::requests` decides it).
//! Auto-download needs a follow first. Unfollowing keeps any approval row,
//! so a grant survives a re-follow (v2 rule).

use super::CollectionsService;
use crate::reads::collections::auth::{Principal, Role};
use crate::reads::collections::error::CollectionsError;
use crate::reads::collections::models::{
    FollowStatusResponse, FollowedArtist, FollowedArtistListResponse, NewReleaseItem,
    NewReleaseListResponse, NewReleasePageQuery, RecentReleasesQuery, UnseenCountResponse,
};
use crate::reads::collections::state::AutoDownloadState;
use crate::reads::collections::store::follows::{FollowRow, ReleaseFilter, ReleaseRow};

/// To-do list page size: default and largest (v2 `/new-releases`).
const PAGE_DEFAULT: usize = 50;
const PAGE_MAX: usize = 100;
/// Release log window in days: default and largest (v2 `/new-releases/recent`).
const RECENT_DAYS_DEFAULT: u32 = 30;
const RECENT_DAYS_MAX: u32 = 365;
/// Release log size: default and largest. The page grows by 48 per "load
/// more" and the client stops at 480.
const RECENT_LIMIT_DEFAULT: usize = 8;
const RECENT_LIMIT_MAX: usize = 500;

/// A page size within `1..=max`, or the default when absent.
fn page_size(value: Option<usize>, default: usize, max: usize) -> Result<usize, CollectionsError> {
    match value {
        None => Ok(default),
        Some(size) if (1..=max).contains(&size) => Ok(size),
        Some(_) => Err(CollectionsError::invalid(&format!(
            "limit must be between 1 and {max}"
        ))),
    }
}

/// A follow's state: intent off is off; on is active once approved or when
/// the follower's role approves itself, pending otherwise.
pub(crate) fn derive_state(row: &FollowRow) -> AutoDownloadState {
    let verdict = row
        .approval_state
        .as_deref()
        .map_or(AutoDownloadState::None, AutoDownloadState::from_approval);
    if row.auto_download {
        // Curators approve themselves and carry no approval row (v2 DD3).
        if Role::parse(&row.user_role).is_some_and(Role::is_curator) {
            AutoDownloadState::Approved
        } else {
            verdict
        }
    } else if matches!(
        verdict,
        AutoDownloadState::Rejected | AutoDownloadState::Revoked
    ) {
        verdict
    } else {
        AutoDownloadState::None
    }
}

fn status(artist_mbid: &str, row: Option<&FollowRow>) -> FollowStatusResponse {
    match row {
        Some(row) => FollowStatusResponse {
            artist_mbid: artist_mbid.to_owned(),
            followed: true,
            auto_download: row.auto_download,
            auto_download_state: derive_state(row).as_str().to_owned(),
        },
        None => FollowStatusResponse {
            artist_mbid: artist_mbid.to_owned(),
            followed: false,
            auto_download: false,
            auto_download_state: AutoDownloadState::None.as_str().to_owned(),
        },
    }
}

fn release_items(rows: Vec<ReleaseRow>, total: usize) -> NewReleaseListResponse {
    let items = rows
        .into_iter()
        .map(|row| NewReleaseItem {
            release_group_mbid: row.release_group_mbid,
            title: row.title,
            artist_name: row.artist_name,
            artist_mbid: row.artist_mbid,
            primary_type: row.primary_type,
            first_release_date: row.first_release_date,
            in_library: row.in_library,
        })
        .collect::<Vec<_>>();
    NewReleaseListResponse { items, total }
}

/// `YYYY-MM-DD` of the day `days` before now, UTC.
fn date_days_ago(days: i64) -> String {
    let now = crate::reads::collections::db::now_epoch() as i64;
    let day = (now - days * 86_400).div_euclid(86_400);
    let (year, month, date) = civil_from_days(day);
    format!("{year:04}-{month:02}-{date:02}")
}

/// Days since the epoch to a civil date (Howard Hinnant's algorithm).
fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    (yoe + era * 400 + i64::from(month <= 2), month, day)
}

impl CollectionsService<'_> {
    /// One artist's follow status for the caller.
    pub async fn follow_status(
        &self,
        user_id: &str,
        artist_mbid: &str,
    ) -> Result<FollowStatusResponse, CollectionsError> {
        let row = self.state.stores.follows.get(user_id, artist_mbid).await?;
        Ok(status(artist_mbid, row.as_ref()))
    }

    /// Follow or unfollow one artist.
    pub async fn set_follow(
        &self,
        user_id: &str,
        artist_mbid: &str,
        followed: bool,
        artist_name: Option<String>,
    ) -> Result<FollowStatusResponse, CollectionsError> {
        let follows = &self.state.stores.follows;
        if followed {
            let name = artist_name.filter(|name| !name.trim().is_empty());
            follows
                .follow(user_id, &[(artist_mbid.to_owned(), name)])
                .await?;
        } else {
            follows.unfollow(user_id, artist_mbid).await?;
        }
        self.follow_status(user_id, artist_mbid).await
    }

    /// Turn auto-download on or off. Enabling needs a follow.
    pub async fn set_auto_download(
        &self,
        caller: &Principal,
        artist_mbid: &str,
        enabled: bool,
    ) -> Result<FollowStatusResponse, CollectionsError> {
        let follows = &self.state.stores.follows;
        let Some(row) = follows.get(&caller.user_id, artist_mbid).await? else {
            return Err(if enabled {
                CollectionsError::Conflict {
                    message: "Follow the artist before enabling auto-download".to_owned(),
                }
            } else {
                CollectionsError::NotFound
            });
        };
        follows
            .set_intent(&caller.user_id, &[artist_mbid.to_owned()], enabled)
            .await?;
        if let Some(sink) = &self.state.approval_seeds {
            if enabled && !caller.role.is_curator() {
                sink.seed_approval(&caller.user_id, artist_mbid, &row.artist_name)
                    .await
                    .map_err(|cause| CollectionsError::internal(&cause))?;
            } else if !enabled {
                sink.withdraw_approval(&caller.user_id, artist_mbid)
                    .await
                    .map_err(|cause| CollectionsError::internal(&cause))?;
            }
        }
        self.follow_status(&caller.user_id, artist_mbid).await
    }

    /// The caller's followed artists, newest first.
    pub async fn list_followed(
        &self,
        user_id: &str,
    ) -> Result<FollowedArtistListResponse, CollectionsError> {
        let artists = self
            .state
            .stores
            .follows
            .list(user_id)
            .await?
            .into_iter()
            .map(|row| FollowedArtist {
                auto_download_state: derive_state(&row).as_str().to_owned(),
                artist_mbid: row.artist_mbid,
                name: row.artist_name,
                auto_download: row.auto_download,
                followed_at: row.followed_at,
            })
            .collect();
        Ok(FollowedArtistListResponse { artists })
    }

    /// New releases from followed artists the library does not hold yet
    /// (the to-do list), one page.
    pub async fn new_releases(
        &self,
        user_id: &str,
        query: &NewReleasePageQuery,
    ) -> Result<NewReleaseListResponse, CollectionsError> {
        let filter = ReleaseFilter {
            since: None,
            owned_too: false,
            limit: page_size(query.limit, PAGE_DEFAULT, PAGE_MAX)?,
            offset: query.offset.unwrap_or(0),
        };
        let (rows, total) = self.state.stores.follows.releases(user_id, &filter).await?;
        Ok(release_items(rows, total))
    }

    /// The release log: everything followed artists released in the last
    /// `days` days, albums already in the library flagged `in_library`
    /// unless the caller hides them. Undated rows fall back to when they
    /// were discovered.
    pub async fn recent_releases(
        &self,
        user_id: &str,
        query: &RecentReleasesQuery,
    ) -> Result<NewReleaseListResponse, CollectionsError> {
        let days = query.days.unwrap_or(RECENT_DAYS_DEFAULT);
        if !(1..=RECENT_DAYS_MAX).contains(&days) {
            return Err(CollectionsError::invalid(&format!(
                "days must be between 1 and {RECENT_DAYS_MAX}"
            )));
        }
        let days = i64::from(days);
        let cutoff_ts = crate::reads::collections::db::now_real() - (days * 86_400) as f64;
        let filter = ReleaseFilter {
            since: Some((date_days_ago(days), cutoff_ts)),
            owned_too: query.include_owned.unwrap_or(true),
            limit: page_size(query.limit, RECENT_LIMIT_DEFAULT, RECENT_LIMIT_MAX)?,
            offset: 0,
        };
        let (rows, total) = self.state.stores.follows.releases(user_id, &filter).await?;
        Ok(release_items(rows, total))
    }

    /// Releases discovered since the caller last looked.
    pub async fn unseen_count(
        &self,
        user_id: &str,
    ) -> Result<UnseenCountResponse, CollectionsError> {
        let count = self.state.stores.follows.unseen_count(user_id).await?;
        Ok(UnseenCountResponse { count })
    }

    /// Mark everything seen.
    pub async fn mark_seen(&self, user_id: &str) -> Result<UnseenCountResponse, CollectionsError> {
        self.state.stores.follows.mark_seen(user_id).await?;
        Ok(UnseenCountResponse { count: 0 })
    }
}
