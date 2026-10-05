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
    NewReleaseListResponse, UnseenCountResponse,
};
use crate::reads::collections::state::AutoDownloadState;
use crate::reads::collections::store::follows::{FollowRow, ReleaseRow};

/// Recent window for the recent-releases view: 30 days.
const RECENT_WINDOW_DAYS: i64 = 30;

/// A follow's state: intent off is off; on is active once approved or when
/// the follower's role approves itself, pending otherwise.
pub(crate) fn derive_state(row: &FollowRow) -> AutoDownloadState {
    if !row.auto_download {
        return AutoDownloadState::Off;
    }
    let self_approving = Role::parse(&row.user_role).is_some_and(Role::is_curator);
    if self_approving || row.approval_state.as_deref() == Some("approved") {
        AutoDownloadState::Active
    } else {
        AutoDownloadState::Pending
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
            auto_download_state: AutoDownloadState::Off.as_str().to_owned(),
        },
    }
}

fn release_items(rows: Vec<ReleaseRow>) -> NewReleaseListResponse {
    let items = rows
        .into_iter()
        .map(|row| NewReleaseItem {
            release_group_mbid: row.release_group_mbid,
            title: row.title,
            artist_name: row.artist_name,
            artist_mbid: row.artist_mbid,
            primary_type: row.primary_type,
            first_release_date: row.first_release_date,
        })
        .collect::<Vec<_>>();
    let total = items.len();
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

    /// New releases from followed artists the library does not hold yet.
    pub async fn new_releases(
        &self,
        user_id: &str,
    ) -> Result<NewReleaseListResponse, CollectionsError> {
        let rows = self
            .state
            .stores
            .follows
            .releases(user_id, None, false)
            .await?;
        Ok(release_items(rows))
    }

    /// Everything followed artists released in the recent window, owned
    /// albums included (the log view).
    pub async fn recent_releases(
        &self,
        user_id: &str,
    ) -> Result<NewReleaseListResponse, CollectionsError> {
        let cutoff_ts =
            crate::reads::collections::db::now_real() - (RECENT_WINDOW_DAYS * 86_400) as f64;
        let rows = self
            .state
            .stores
            .follows
            .releases(
                user_id,
                Some((date_days_ago(RECENT_WINDOW_DAYS), cutoff_ts)),
                true,
            )
            .await?;
        Ok(release_items(rows))
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
