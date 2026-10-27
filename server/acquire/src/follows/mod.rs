//! Follows: who follows which artist, the auto-download asks and their
//! verdicts, and the follow poll's cursors.
//!
//! [`Follows`] alone writes `user_followed_artists`,
//! `auto_download_approvals` and `acquire_follow_cursors`, and each action
//! is one writer-lane transaction: the auto-download toggle sets the
//! intent and files or withdraws the ask together, a verdict moves the
//! approval and arms or clears the follow together, and a Lidarr import
//! follows, files its batch and arms in one go. A failure leaves nothing
//! half done.
//!
//! Following is instant. Enabling auto-download needs a follow; trusted
//! and admin accounts approve themselves, a plain user's ask waits for an
//! admin. Unfollowing keeps any approval row, so a grant survives a
//! re-follow. Rows key on the lowercased MBID, so imported follows and
//! follows made in the app are one row.
//!
//! What a user sees of a follow (the artist header, the Following page,
//! the answer to a toggle) is read by the collections reads over the same
//! rows.

pub mod approvals;
pub mod cursors;
pub mod http;
pub mod models;
pub(crate) mod sql;

use std::collections::HashSet;

use rusqlite::types::Value;
use rusqlite::{OptionalExtension, Transaction, params, params_from_iter};

use self::approvals::Reviewer;
use self::models::{
    ApprovalBatchItem, ApprovalBatchListResponse, AutoDownloadApprovalItem,
    AutoDownloadApprovalListResponse,
};
use crate::db::{AcquireDb, epoch_secs, unix_secs_f64};
use crate::requests::error::RequestsError;
use crate::requests::ledger::{ApprovalState, validate_mbid};
use crate::requests::models::ActionResponse;
use crate::requests::sqlite::{lane_error, ms_from_secs, read};
use droppedneedle_auth::{Admin, Principal, Role};
use droppedneedle_catalog::reads::collections::db::CollectionsDb;
use droppedneedle_catalog::reads::collections::error::CollectionsError;
use droppedneedle_catalog::reads::collections::models::FollowStatusResponse;
use droppedneedle_catalog::reads::collections::service::follow_status;
use droppedneedle_catalog::reads::collections::store::FollowStore;
use droppedneedle_kernel::clock::Clock;
use droppedneedle_kernel::db::statement::ListLen;

pub use self::http::routes;

/// Name used when a follow arrives without one.
const UNKNOWN_ARTIST: &str = "Unknown Artist";
/// What a batch card says produced its asks.
const IMPORT_SOURCE: &str = "import";
/// What a Lidarr import batch stores as its source.
const LIDARR_SOURCE: &str = "lidarr_import";
/// How many artist names a batch card previews.
const BATCH_SAMPLE_COUNT: usize = 3;

/// Whether a follow with auto-download on downloads by itself: the
/// follower's role approves itself, or an admin approved the ask.
pub(crate) fn auto_download_live(role: Role, verdict: Option<ApprovalState>) -> bool {
    role.is_curator() || verdict == Some(ApprovalState::Approved)
}

/// One artist a Lidarr import follows.
#[derive(Debug, Clone)]
pub struct ImportArtist {
    /// Artist MBID as Lidarr spells it.
    pub mbid: String,
    /// Artist name from Lidarr.
    pub name: String,
    /// Lidarr monitors the artist's new releases, so a new follow mirrors
    /// that as auto-download.
    pub monitors_new: bool,
}

/// What one import did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportOutcome {
    /// New follows.
    pub imported: usize,
    /// Artists the user already followed (left as they were).
    pub already_following: usize,
    /// New follows with auto-download turned on.
    pub auto_download_enabled: usize,
    /// The approval batch filed for them, when the user needs one.
    pub approval_batch_id: Option<String>,
}

/// A follow an album request asks for, written in the request's own
/// transaction.
#[derive(Debug, Clone)]
pub struct FollowAsk {
    /// Artist MBID, validated.
    pub artist_mbid: String,
    /// Artist name, when the ask has one.
    pub artist_name: Option<String>,
    /// Ask for auto-download as well.
    pub auto_download: bool,
    /// The asker's role approves its own auto-download.
    pub self_approves: bool,
}

impl FollowAsk {
    /// Follow the artist for `user_id`, and ask for auto-download the way
    /// the Following page does. An ask is filed at `now_ms`.
    pub(crate) fn apply(
        &self,
        tx: &Transaction,
        user_id: &str,
        now_ms: i64,
    ) -> rusqlite::Result<()> {
        set_follow_tx(
            tx,
            user_id,
            &self.artist_mbid,
            true,
            self.artist_name.as_deref(),
            real_secs(now_ms),
        )?;
        if self.auto_download {
            set_auto_download_tx(
                tx,
                user_id,
                &self.artist_mbid,
                true,
                self.self_approves,
                now_ms,
            )?;
        }
        Ok(())
    }
}

/// The one owner of the follow tables.
#[derive(Clone)]
pub struct Follows {
    db: AcquireDb,
    /// The collections read of one follow's status, over the same rows.
    reads: FollowStore,
}

impl Follows {
    /// Follows over the application database.
    pub fn new(db: AcquireDb) -> Self {
        let reads = FollowStore::new(CollectionsDb::new(db.db().clone(), db.clock().clone()));
        Self { db, reads }
    }

    /// One artist's follow status for `user_id`.
    pub async fn status(
        &self,
        user_id: &str,
        artist_mbid: &str,
    ) -> Result<FollowStatusResponse, RequestsError> {
        follow_status(&self.reads, user_id, artist_mbid)
            .await
            .map_err(|error| match error {
                CollectionsError::Busy { operation } => RequestsError::Busy { operation },
                other => RequestsError::internal(&format_args!("follow status: {other:?}")),
            })
    }

    /// Follow or unfollow one artist. Re-following keeps the intent and
    /// the start date and refreshes the name when one is given.
    pub async fn set_follow(
        &self,
        user_id: &str,
        artist_mbid: &str,
        followed: bool,
        artist_name: Option<String>,
    ) -> Result<FollowStatusResponse, RequestsError> {
        let (user, mbid) = (user_id.to_owned(), artist_mbid.to_owned());
        let name = artist_name.filter(|name| !name.trim().is_empty());
        let now = self.db.now_unix_f64();
        self.db
            .write("follows.set_follow", move |tx| {
                Ok(set_follow_tx(
                    tx,
                    &user,
                    &mbid,
                    followed,
                    name.as_deref(),
                    now,
                )?)
            })
            .await
            .map_err(|error| lane_error("follows.set_follow", error))?;
        self.status(user_id, artist_mbid).await
    }

    /// Turn auto-download on or off for one followed artist, filing the
    /// ask (or withdrawing a pending one) in the same transaction.
    pub async fn set_auto_download(
        &self,
        caller: &Principal,
        artist_mbid: &str,
        enabled: bool,
    ) -> Result<FollowStatusResponse, RequestsError> {
        let (user, mbid) = (caller.user_id().to_owned(), artist_mbid.to_owned());
        let self_approves = caller.role().is_curator();
        let clock = self.db.clock().clone();
        let followed = self
            .db
            .write("follows.set_auto_download", move |tx| {
                Ok(set_auto_download_tx(
                    tx,
                    &user,
                    &mbid,
                    enabled,
                    self_approves,
                    now_ms(clock.as_ref()),
                )?)
            })
            .await
            .map_err(|error| lane_error("follows.set_auto_download", error))?;
        if !followed {
            return Err(if enabled {
                RequestsError::Conflict {
                    message: "Follow the artist before enabling auto-download".to_owned(),
                }
            } else {
                RequestsError::NotFound
            });
        }
        self.status(caller.user_id(), artist_mbid).await
    }

    /// An admin's verdict on one ask: approve or reject a pending one, or
    /// revoke an approved one. Approving arms the follow (following the
    /// artist when the ask came without a follow); the others clear its
    /// intent and keep the follow.
    pub async fn decide(
        &self,
        admin: &Admin,
        user_id: &str,
        artist_mbid: &str,
        verdict: ApprovalState,
    ) -> Result<ActionResponse, RequestsError> {
        validate_mbid(artist_mbid)?;
        let from = match verdict {
            ApprovalState::Approved | ApprovalState::Rejected => ApprovalState::Pending,
            ApprovalState::Revoked => ApprovalState::Approved,
            ApprovalState::Pending => {
                return Err(RequestsError::InvalidInput {
                    message: "A verdict approves, rejects or revokes".to_owned(),
                });
            }
        };
        let (user, lower, reviewer) = (
            user_id.to_owned(),
            artist_mbid.to_lowercase(),
            reviewer(admin),
        );
        let clock = self.db.clock().clone();
        let decided = self
            .db
            .write("follows.decide", move |tx| {
                let now = now_ms(clock.as_ref());
                let Some(row) =
                    approvals::transition(tx, &user, &lower, (from, verdict), &reviewer, now)?
                else {
                    return Ok(false);
                };
                apply_verdict(
                    tx,
                    &user,
                    (&row.artist_mbid, &row.artist_name),
                    verdict,
                    unix_secs_f64(clock.as_ref()),
                )?;
                Ok(true)
            })
            .await
            .map_err(|error| lane_error("follows.decide", error))?;
        Ok(if decided {
            ActionResponse {
                success: true,
                message: format!("Auto-download {}", verdict.as_str()),
            }
        } else {
            ActionResponse {
                success: false,
                message: "No matching approval found".to_owned(),
            }
        })
    }

    /// Approve or reject every pending ask of one batch, arming or
    /// clearing each follow in the same transaction.
    pub async fn decide_batch(
        &self,
        admin: &Admin,
        batch_id: &str,
        verdict: ApprovalState,
    ) -> Result<ActionResponse, RequestsError> {
        let (batch, reviewer) = (batch_id.to_owned(), reviewer(admin));
        let clock = self.db.clock().clone();
        let rows = self
            .db
            .write("follows.decide_batch", move |tx| {
                let rows = approvals::decide_batch_rows(
                    tx,
                    &batch,
                    verdict,
                    &reviewer,
                    now_ms(clock.as_ref()),
                )?;
                for (user, mbid, name) in &rows {
                    apply_verdict(
                        tx,
                        user,
                        (mbid, name),
                        verdict,
                        unix_secs_f64(clock.as_ref()),
                    )?;
                }
                Ok(rows.len())
            })
            .await
            .map_err(|error| lane_error("follows.decide_batch", error))?;
        Ok(if rows == 0 {
            ActionResponse {
                success: false,
                message: "No matching batch found".to_owned(),
            }
        } else {
            ActionResponse {
                success: true,
                message: format!("Auto-download {} for {rows} artists", verdict.as_str()),
            }
        })
    }

    /// Pending single asks, oldest first, as the admin list shows them.
    pub async fn approval_list(&self) -> Result<AutoDownloadApprovalListResponse, RequestsError> {
        let items = self
            .pending()
            .await?
            .into_iter()
            .map(|row| AutoDownloadApprovalItem {
                user_id: row.user_id,
                user_name: Some(row.user_name),
                artist_mbid: row.artist_mbid,
                artist_name: row.artist_name,
                requested_at: row.requested_at,
            })
            .collect::<Vec<_>>();
        Ok(AutoDownloadApprovalListResponse {
            count: items.len(),
            items,
        })
    }

    /// Pending batches as cards, oldest first.
    pub async fn batch_list(&self) -> Result<ApprovalBatchListResponse, RequestsError> {
        let mut batches = self
            .pending_batches()
            .await?
            .into_iter()
            .map(|batch| ApprovalBatchItem {
                artist_count: batch.artists.len(),
                sample_names: batch
                    .artists
                    .iter()
                    .take(BATCH_SAMPLE_COUNT)
                    .map(|(_, name)| name.clone())
                    .collect(),
                batch_id: batch.batch_id,
                user_id: batch.user_id,
                user_name: Some(batch.user_name),
                requested_at: batch.requested_at,
                source: IMPORT_SOURCE.to_owned(),
            })
            .collect::<Vec<_>>();
        batches.sort_by(|a, b| {
            a.requested_at
                .cmp(&b.requested_at)
                .then(a.user_id.cmp(&b.user_id))
        });
        Ok(ApprovalBatchListResponse {
            count: batches.len(),
            batches,
        })
    }

    /// Lowercased MBIDs among `candidates` the user already follows.
    pub async fn followed_among(
        &self,
        user_id: &str,
        candidates: &[String],
    ) -> Result<HashSet<String>, RequestsError> {
        let mut out = HashSet::new();
        for chunk in candidates.chunks(500) {
            let text = sql::followed_among(ListLen::new(chunk.len()));
            let mut values = vec![Value::Text(user_id.to_owned())];
            values.extend(chunk.iter().map(|mbid| Value::Text(mbid.to_lowercase())));
            let found: Vec<String> = read(&self.db, "follows.followed_among", move |conn| {
                let mut statement = conn.prepare(&text)?;
                let rows = statement.query_map(params_from_iter(values), |row| row.get(0))?;
                rows.collect()
            })
            .await?;
            out.extend(found);
        }
        Ok(out)
    }

    /// Follow every artist of an import for `user_id` in one transaction.
    /// Artists already followed are left as they are. A new follow of an
    /// artist Lidarr monitors gets auto-download, behind one approval
    /// batch (`batch_id`) when the user does not approve themselves.
    pub async fn import(
        &self,
        user_id: &str,
        artists: Vec<ImportArtist>,
        batch_id: Option<String>,
    ) -> Result<ImportOutcome, RequestsError> {
        let user = user_id.to_owned();
        let clock = self.db.clock().clone();
        self.db
            .write("follows.import", move |tx| {
                let (now, now_ms) = (unix_secs_f64(clock.as_ref()), now_ms(clock.as_ref()));
                let mut already_following = 0;
                let mut armed = Vec::new();
                for artist in &artists {
                    let lower = artist.mbid.to_lowercase();
                    let known = tx
                        .query_row(sql::IS_FOLLOWED.text(), params![user, lower], |_| Ok(()))
                        .optional()?
                        .is_some();
                    if known {
                        already_following += 1;
                    } else if artist.monitors_new {
                        armed.push(artist);
                    }
                    upsert_follow(tx, &user, &artist.mbid, Some(&artist.name), now)?;
                }
                let approval_batch_id = batch_id.filter(|_| !armed.is_empty());
                for artist in &armed {
                    if let Some(batch) = &approval_batch_id {
                        approvals::file_batch_member(
                            tx,
                            batch,
                            &user,
                            (&artist.mbid, &artist.name),
                            LIDARR_SOURCE,
                            now_ms,
                        )?;
                    }
                    set_intent(tx, &user, &artist.mbid.to_lowercase(), true, now)?;
                }
                Ok(ImportOutcome {
                    imported: artists.len() - already_following,
                    already_following,
                    auto_download_enabled: armed.len(),
                    approval_batch_id,
                })
            })
            .await
            .map_err(|error| lane_error("follows.import", error))
    }
}

/// `clock`'s time as an approval row stores it: whole seconds, in integer
/// milliseconds.
pub(crate) fn now_ms(clock: &dyn Clock) -> i64 {
    ms_from_secs(epoch_secs(clock))
}

/// Float unix seconds, for the REAL follow columns, from unix milliseconds.
fn real_secs(now_ms: i64) -> f64 {
    now_ms.max(0) as f64 / 1000.0
}

/// The reviewer stamp: acting id plus display name.
fn reviewer(admin: &Admin) -> Reviewer {
    (
        admin.user_id().to_owned(),
        Some(admin.username_display().to_owned()),
    )
}

/// Follow or unfollow one artist inside the caller's transaction.
/// Re-following keeps the intent and the start date and refreshes the
/// name when one is given.
pub(crate) fn set_follow_tx(
    tx: &Transaction,
    user_id: &str,
    artist_mbid: &str,
    followed: bool,
    artist_name: Option<&str>,
    now: f64,
) -> rusqlite::Result<()> {
    if followed {
        upsert_follow(tx, user_id, artist_mbid, artist_name, now)
    } else {
        tx.execute(
            sql::UNFOLLOW.text(),
            params![user_id, artist_mbid.to_lowercase()],
        )?;
        Ok(())
    }
}

/// Turn auto-download on or off for one followed artist inside the
/// caller's transaction, filing the ask when the user does not approve
/// themselves (or withdrawing a pending one). Answers whether the artist
/// is followed; an unfollowed artist is left alone.
pub(crate) fn set_auto_download_tx(
    tx: &Transaction,
    user_id: &str,
    artist_mbid: &str,
    enabled: bool,
    self_approves: bool,
    now_ms: i64,
) -> rusqlite::Result<bool> {
    let lower = artist_mbid.to_lowercase();
    let name: Option<(String, String)> = tx
        .query_row(
            sql::FOLLOWED_ARTIST.text(),
            params![user_id, lower],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    let Some((mbid, name)) = name else {
        return Ok(false);
    };
    set_intent(tx, user_id, &lower, enabled, real_secs(now_ms))?;
    if !enabled {
        approvals::withdraw_pending(tx, user_id, &lower)?;
    } else if !self_approves {
        approvals::file_pending(tx, user_id, &mbid, &name, now_ms)?;
    }
    Ok(true)
}

/// Follow one artist. A conflict keeps the intent and the start date and
/// refreshes the name when one is given.
fn upsert_follow(
    tx: &Transaction,
    user_id: &str,
    artist_mbid: &str,
    artist_name: Option<&str>,
    now: f64,
) -> rusqlite::Result<()> {
    tx.execute(
        sql::UPSERT_FOLLOW.text(),
        params![
            user_id,
            artist_mbid,
            artist_mbid.to_lowercase(),
            artist_name,
            UNKNOWN_ARTIST,
            now
        ],
    )?;
    Ok(())
}

/// Set the auto-download intent on one followed row; an unfollowed artist
/// is left alone.
fn set_intent(
    tx: &Transaction,
    user_id: &str,
    mbid_lower: &str,
    enabled: bool,
    now: f64,
) -> rusqlite::Result<()> {
    tx.execute(
        sql::SET_INTENT.text(),
        params![i64::from(enabled), now, user_id, mbid_lower],
    )?;
    Ok(())
}

/// Carry a verdict into the follow row: an approval turns auto-download
/// on, following the artist first when the ask came without a follow;
/// anything else turns it off and keeps the follow.
fn apply_verdict(
    tx: &Transaction,
    user_id: &str,
    (artist_mbid, artist_name): (&str, &str),
    verdict: ApprovalState,
    now: f64,
) -> rusqlite::Result<()> {
    if verdict == ApprovalState::Approved {
        tx.execute(
            sql::FOLLOW_ARMED.text(),
            params![
                user_id,
                artist_mbid,
                artist_mbid.to_lowercase(),
                artist_name,
                now
            ],
        )?;
        Ok(())
    } else {
        set_intent(tx, user_id, &artist_mbid.to_lowercase(), false, now)
    }
}
