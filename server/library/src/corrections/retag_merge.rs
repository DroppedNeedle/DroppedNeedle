//! Joining an album retagged outside the app to the row its files sit on
//! now: across two scans, or to the album that replaced it on the same
//! edition. The old row is merged into the new one, which keeps its id,
//! name and work. The old id stays as an alias. No file is touched.

use rusqlite::{Connection, Transaction, params};

use super::edition::drop_edition;
use super::membership::retire;
use crate::catalog_revision::bump_catalog;
use crate::identify::sqlite::{
    chosen_matches_differ, match_chosen, merge_moves_match, offer_album, release_chosen,
    settle_merged_match,
};
use crate::management::{automatic, takeover};
use crate::operations::decisions::{CatalogAction, record_action};
use crate::scan::sqlite_store::retag_pairs::RetagPairReason;
use droppedneedle_kernel::db::savepoint;

/// Why two rows are joined.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum JoinCause {
    /// The files left the old row in an earlier scan and sit on the new
    /// one, or a person joined a pair. The new row is offered for
    /// identification.
    AcrossScans,
    /// The new row's files replaced the old row's on the same edition
    /// ([`join_replaced`](super::release_join::join_replaced)). The new row
    /// was identified for these files already and is not offered again.
    SameRelease,
}

impl JoinCause {
    /// Reason code of the audit action the merge records.
    fn reason(self) -> &'static str {
        match self {
            Self::AcrossScans => "RETAG_JOINED_ACROSS_SCANS",
            Self::SameRelease => "RETAG_JOINED_SAME_RELEASE",
        }
    }
}

/// What became of a merge.
#[derive(Debug)]
pub(crate) enum MergeOutcome {
    /// The old row is retired into the new one.
    Merged {
        /// The new row was offered for identification.
        offered: bool,
    },
    /// Both rows are as they were.
    Refused {
        reason: RetagPairReason,
        /// The store's error, for a refusal on one.
        error: Option<rusqlite::Error>,
    },
}

/// Merge `old` into `new`, in a savepoint of the caller's transaction.
///
/// Refused, with both rows left as they are, when the two rows hold
/// different chosen matches, when the old row holds a custom edition, when
/// a draft or a plan of a live job is open on the old row (or on the new
/// row, when the merge would change its match), and on a store error.
///
/// Otherwise, in order:
/// - the match is settled ([`settle_merged_match`]): a person's match is
///   never replaced or dropped except by an equal one;
/// - what was made for the old row's match is cleared from it, with its
///   place in the automatic management queue;
/// - the track rows left on the old row (missing or excluded files) move
///   to the new one;
/// - the old row's "leave unmanaged" goes first when the new row holds a
///   person's match of its own; otherwise it is carried, like the rest
///   [`retire`] carries (the alias, favorites, plays, the taken-over mark,
///   album overrides the new row has none of);
/// - playlist entries naming the old row name the new one;
/// - when the old row's match was a release someone chose, the new row
///   holds no match a person chose and the app has no record of any file
///   now on it, the new row is stamped so automatic management leaves it
///   unmanaged ([`automatic::stamp_retag_followed`]): the old edition must
///   not be written over a retag done by hand. A match a person chose on
///   the new row, equal or not, was chosen for the retagged files;
/// - an audit action with the reason `cause` gives, the catalog revision,
///   and, for a join across scans, the offer for identification where the
///   files are managed automatically.
///
/// `actor` is the person who asked, recorded on the audit action; `None`
/// is the app.
pub(crate) fn merge_retagged(
    tx: &Transaction<'_>,
    old: &str,
    new: &str,
    actor: Option<&str>,
    now: f64,
    cause: JoinCause,
) -> rusqlite::Result<MergeOutcome> {
    let refused = |reason| {
        Ok(MergeOutcome::Refused {
            reason,
            error: None,
        })
    };
    if chosen_matches_differ(tx, old, new)? {
        return refused(RetagPairReason::ChosenMatchesDiffer);
    }
    let custom: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM library_custom_edition_active WHERE local_album_id = ?1)",
        params![old],
        |row| row.get(0),
    )?;
    if custom {
        return refused(RetagPairReason::CustomEdition);
    }
    if live_work(tx, old, now)? || (merge_moves_match(tx, old, new)? && live_work(tx, new, now)?) {
        return refused(RetagPairReason::LiveWork);
    }
    let chosen_release = release_chosen(tx, old)?;
    let new_chosen = match_chosen(tx, new)?;
    let merged = savepoint(tx, "merge_album", || {
        settle_merged_match(tx, old, new, now)?;
        drop_edition(tx, old)?;
        tx.execute(
            "DELETE FROM library_management_automatic_queue WHERE local_album_id = ?1",
            params![old],
        )?;
        tx.execute(
            "UPDATE local_tracks SET local_album_id = ?2 WHERE local_album_id = ?1",
            params![old, new],
        )?;
        if new_chosen {
            tx.execute(
                "DELETE FROM library_management_exclusions WHERE local_album_id = ?1",
                params![old],
            )?;
        }
        retire(tx, old, new, now)?;
        tx.execute(
            "UPDATE playlist_tracks SET album_id = ?2 WHERE album_id = ?1",
            params![old, new],
        )?;
        let stamped = chosen_release && !new_chosen && !album_has_record(tx, new)?;
        if stamped {
            automatic::stamp_retag_followed(tx, new, now)?;
        }
        record_action(
            tx,
            CatalogAction {
                actor: actor.unwrap_or("system"),
                kind: "merge_album",
                album_id: new,
                job_id: None,
                before: serde_json::json!({ "retired_album_id": old }),
                after: serde_json::json!({
                    "kind": "join_retagged_album",
                    "surviving_album_id": new,
                    "retired_album_ids": [old],
                    "stamped_unmanaged": stamped,
                    "filesystem_writes": 0,
                }),
                reason: cause.reason(),
            },
            now,
        )?;
        bump_catalog(tx)?;
        if cause == JoinCause::SameRelease {
            return Ok(MergeOutcome::Merged { offered: false });
        }
        let automatic: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM local_tracks WHERE local_album_id = ?1 \
             AND availability = 'indexed' AND applied_policy = 'automatic')",
            params![new],
            |row| row.get(0),
        )?;
        let offered = automatic && offer_album(tx, new, (now * 1000.0) as i64)?;
        Ok(MergeOutcome::Merged { offered })
    })?;
    match merged {
        Ok(outcome) => Ok(outcome),
        Err(error) => Ok(MergeOutcome::Refused {
            reason: RetagPairReason::StoreError,
            error: Some(error),
        }),
    }
}

/// Work a person or a job is doing on the album: an open draft, or a plan
/// item of a job that is queued, running, paused, or ready and not past
/// its expiry. An album field override is not.
fn live_work(conn: &Connection, album_id: &str, now: f64) -> rusqlite::Result<bool> {
    conn.query_row(
        &format!(
            "SELECT EXISTS(SELECT 1 FROM library_contribution_drafts WHERE local_album_id = ?1 \
             AND state IN ('draft','ready','seeded','verifying','needs_review')) \
             OR EXISTS(SELECT 1 FROM library_management_plan_items item \
             JOIN library_operation_jobs job ON job.id = item.job_id \
             WHERE item.local_album_id = ?1 AND {})",
            crate::management::store::live_job_sql("?2")
        ),
        params![album_id, now],
        |row| row.get(0),
    )
}

/// Whether the app has a record of the bytes of any file on the album,
/// whatever its availability: one it wrote, or one v2 wrote.
pub(crate) fn album_has_record(conn: &Connection, album_id: &str) -> rusqlite::Result<bool> {
    conn.prepare_cached(&format!(
        "SELECT EXISTS(SELECT 1 FROM local_tracks t \
         JOIN library_track_management_state s ON s.local_track_id = t.id \
         WHERE t.local_album_id = ?1 AND {})",
        takeover::record_sql()
    ))?
    .query_row(params![album_id], |row| row.get(0))
}
