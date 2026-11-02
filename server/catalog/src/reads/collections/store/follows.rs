//! Follow reads over `acquire_followed_artists`, plus the new-release feed.
//!
//! The follows module in acquisition alone writes the follow rows and
//! their approval verdicts in `auto_download_approvals`; this store only
//! reads them to show a follow's state. Rows key on the lowercased MBID.
//!
//! New releases come from `acquire_new_release_feed` joined to the user's follows;
//! albums already in the library drop out of the to-do list (v2 rule).

use droppedneedle_auth::Role;
use rusqlite::types::{Type, Value};
use rusqlite::{OptionalExtension as _, params, params_from_iter};

use super::super::db::{CollectionsDb, StoreError, epoch_from_real};
use super::super::sql;
use super::super::state::AutoDownloadState;

/// One follow row with its approval verdict.
#[derive(Debug, Clone, PartialEq)]
pub struct FollowRow {
    /// Follower user id.
    pub user_id: String,
    /// Follower display name.
    pub user_name: String,
    /// Follower role.
    pub user_role: Role,
    /// Artist MBID as first followed.
    pub artist_mbid: String,
    /// Artist name snapshot.
    pub artist_name: String,
    /// Auto-download intent.
    pub auto_download: bool,
    /// Approval verdict, when an approval row exists.
    pub approval_state: Option<AutoDownloadState>,
    /// When the ask was filed, epoch seconds, when an approval row exists.
    pub requested_at: Option<u64>,
    /// When the follow started, epoch seconds.
    pub followed_at: u64,
    /// Last change, epoch seconds.
    pub updated_at: u64,
}

/// One new-release sighting.
#[derive(Debug, Clone, PartialEq)]
pub struct ReleaseRow {
    /// Release-group MBID.
    pub release_group_mbid: String,
    /// Release title.
    pub title: String,
    /// Artist name.
    pub artist_name: String,
    /// Artist MBID as the user followed it.
    pub artist_mbid: String,
    /// Primary type, when known.
    pub primary_type: Option<String>,
    /// First release date, when known.
    pub first_release_date: Option<String>,
    /// True when the library already holds the album.
    pub in_library: bool,
}

/// Which release sightings to read.
#[derive(Debug, Clone, PartialEq)]
pub struct ReleaseFilter {
    /// Only releases dated on or after this `YYYY-MM-DD`, or, undated,
    /// discovered at or after this epoch time.
    pub since: Option<(String, f64)>,
    /// Keep albums the library already holds (the log view).
    pub owned_too: bool,
    /// Page size.
    pub limit: usize,
    /// Rows skipped first.
    pub offset: usize,
}

/// The follow store.
#[derive(Clone, Debug)]
pub struct FollowStore {
    db: CollectionsDb,
}

impl FollowStore {
    /// Now, as fractional epoch seconds, by the store's clock.
    pub fn now_real(&self) -> f64 {
        self.db.now_real()
    }
}

/// A stored value outside its closed set fails the read.
fn unknown_value(column: usize, value: &str) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(
        column,
        Type::Text,
        format!("unknown stored value {value:?}").into(),
    )
}

fn follow_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<FollowRow> {
    let row = sql::FollowColumns::read(row)?;
    Ok(FollowRow {
        user_role: Role::parse(&row.user_role).ok_or_else(|| unknown_value(2, &row.user_role))?,
        approval_state: row
            .approval_state
            .map(|state| {
                AutoDownloadState::from_approval(&state).ok_or_else(|| unknown_value(6, &state))
            })
            .transpose()?,
        user_id: row.user_id,
        user_name: row.user_name,
        artist_mbid: row.artist_mbid,
        artist_name: row.artist_name,
        auto_download: row.auto_download,
        requested_at: row.requested_at.map(epoch_from_real),
        followed_at: epoch_from_real(row.followed_at),
        updated_at: epoch_from_real(row.updated_at),
    })
}

fn release_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<ReleaseRow> {
    Ok(ReleaseRow {
        release_group_mbid: row.get(0)?,
        title: row.get(1)?,
        artist_name: row.get(2)?,
        artist_mbid: row.get(3)?,
        primary_type: row.get(4)?,
        first_release_date: row.get(5)?,
        in_library: row.get(6)?,
    })
}

impl FollowStore {
    /// Store over one database.
    pub fn new(db: CollectionsDb) -> Self {
        Self { db }
    }

    /// One follow, or None when the user does not follow the artist.
    pub async fn get(
        &self,
        user_id: &str,
        artist_mbid: &str,
    ) -> Result<Option<FollowRow>, StoreError> {
        let user_id = user_id.to_owned();
        let artist_mbid = artist_mbid.to_lowercase();
        self.db
            .read("follows.get", move |conn| {
                let row = conn
                    .query_row(
                        sql::FOLLOW.text(),
                        params![user_id, artist_mbid],
                        follow_from_row,
                    )
                    .optional()?;
                Ok(row)
            })
            .await
    }

    /// A user's follows, newest first.
    pub async fn list(&self, user_id: &str) -> Result<Vec<FollowRow>, StoreError> {
        let user_id = user_id.to_owned();
        self.db
            .read("follows.list", move |conn| {
                let rows = conn
                    .prepare(sql::FOLLOWS.text())?
                    .query_map(params![user_id], follow_from_row)?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                Ok(rows)
            })
            .await
    }

    /// One page of release sightings for the user's follows, newest
    /// first, with the total the filter matches.
    pub async fn releases(
        &self,
        user_id: &str,
        filter: &ReleaseFilter,
    ) -> Result<(Vec<ReleaseRow>, usize), StoreError> {
        let mut binds = vec![Value::Text(user_id.to_owned())];
        if let Some((date, discovered)) = &filter.since {
            binds.push(Value::Text(date.clone()));
            binds.push(Value::Integer((*discovered * 1000.0).round() as i64));
        }
        let count_text = sql::release_count(filter.owned_too, filter.since.is_some());
        let count_binds = binds.clone();
        let total: i64 = self
            .db
            .read("follows.releases", move |conn| {
                let total =
                    conn.query_row(&count_text, params_from_iter(count_binds), |row| row.get(0))?;
                Ok(total)
            })
            .await?;
        binds.push(Value::Integer(
            i64::try_from(filter.limit).unwrap_or(i64::MAX),
        ));
        binds.push(Value::Integer(
            i64::try_from(filter.offset).unwrap_or(i64::MAX),
        ));
        let page_text = sql::release_page(filter.owned_too, filter.since.is_some());
        let rows = self
            .db
            .read("follows.releases", move |conn| {
                let rows = conn
                    .prepare(&page_text)?
                    .query_map(params_from_iter(binds), release_from_row)?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                Ok(rows)
            })
            .await?;
        Ok((rows, total.max(0) as usize))
    }

    /// Releases not yet owned and discovered after the user's seen marker.
    pub async fn unseen_count(&self, user_id: &str) -> Result<usize, StoreError> {
        let user_id = user_id.to_owned();
        let count: i64 = self
            .db
            .read("follows.unseen_count", move |conn| {
                let count =
                    conn.query_row(sql::UNSEEN_RELEASES.text(), params![user_id], |row| {
                        row.get(0)
                    })?;
                Ok(count)
            })
            .await?;
        Ok(count.max(0) as usize)
    }

    /// Move the user's seen marker to now.
    pub async fn mark_seen(&self, user_id: &str) -> Result<(), StoreError> {
        let user_id = user_id.to_owned();
        let now = self.db.now_ms();
        self.db
            .write("follows.mark_seen", move |tx| {
                tx.execute(sql::MARK_SEEN.text(), params![user_id, now])?;
                Ok(())
            })
            .await
    }
}
