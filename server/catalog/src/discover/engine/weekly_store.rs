//! Each person's weekly sets and set day: what a set is built from, the one
//! writer of the sets, the sets read back with what became of each pick,
//! and forgetting them.
//!
//! A set is computed once by the weekly set job and is a record from then
//! on, the one computed result that is never rebuilt: [`keep`] is the one
//! writer of its two tables, it never replaces a week's set, and nothing
//! else writes them but the forget. A pick's state is read from the
//! request, library, Later and feedback rows each time, never stored here.

use droppedneedle_kernel::db::time::Millis;
use droppedneedle_kernel::db::{Db, DbError, writer::Lane};
use droppedneedle_kernel::jobs::queue::JobError;
use droppedneedle_media::history::HistoryImportState;
use rusqlite::{Transaction, params};
use serde::{Deserialize, Serialize};

use super::candidate::Candidate;
use super::select::Shown;
use super::sql;
use super::taste_store::Kept;
use super::weekly::{Depth, SetDay, ShortReason};
use crate::discover::cache::ForgetCounts;
use crate::discover::facts::{FactSource, Outcome};
use crate::discover::feedback::FeedbackTarget;
use crate::discover::models::{PickEvidence, PickReason, SetCounts, SetTaste};
use crate::discover::record::{MadeFrom, Offer};

fn json(ids: &[String]) -> String {
    serde_json::to_string(ids).unwrap_or_else(|_| "[]".to_owned())
}

/// The taste build a set copies its counts from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TasteRow {
    /// When it was built, unix ms.
    pub built_at: i64,
    /// Plays in the app and imported listens counted.
    pub plays_counted: i64,
    /// The first of them, unix ms.
    pub history_from_at: Option<i64>,
    /// The latest of them, unix ms.
    pub history_to_at: Option<i64>,
    /// Its seeds.
    pub seeds: i64,
}

/// The pool build a set is dealt from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PoolRow {
    /// When it was built, unix ms.
    pub built_at: i64,
    /// Whether facts it needed were still to come.
    pub incomplete: bool,
    /// The artists whose album facts it waits on.
    pub waiting_for: Vec<String>,
    /// Artists the seeds point at.
    pub neighbours_found: i64,
    /// Of those, the ones the person knows or the library holds.
    pub known_left_out: i64,
    /// The hubs left out; `None` when the rule did not run.
    pub hubs_left_out: Option<i64>,
    /// The depth it was ordered at; next door for a build before it was kept.
    pub depth: Depth,
}

/// What a person's set is built from and when.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Context {
    /// Their set day.
    pub set_day: SetDay,
    /// Their depth.
    pub depth: Depth,
    /// When their first set began waiting for their history, unix ms.
    pub wait_from_at: Option<i64>,
    /// When their newest set's week starts, unix ms.
    pub last_week_at: Option<i64>,
    /// Their taste build.
    pub taste: Option<TasteRow>,
    /// Their pool build.
    pub pool: Option<PoolRow>,
    /// Their history import's state.
    pub import_state: Option<HistoryImportState>,
}

/// `user_id`'s context; `None` when there is no such account.
///
/// # Errors
///
/// The read's failure.
pub(crate) async fn context(db: &Db, user_id: &str) -> Result<Option<Context>, DbError> {
    let user = user_id.to_owned();
    let row = db
        .read("discover.weekly.context", move |conn| {
            conn.prepare_cached(sql::WEEK_CONTEXT.text())?
                .query_map([&user], sql::WeekContext::read)?
                .next()
                .transpose()
                .map_err(DbError::from)
        })
        .await?;
    Ok(row.map(|row| Context {
        set_day: row.set_day.unwrap_or(SetDay::DEFAULT),
        depth: row.depth.unwrap_or(Depth::DEFAULT),
        wait_from_at: row.wait_from_at.map(|at| at.0),
        last_week_at: row.last_week_at.map(|at| at.0),
        taste: row.taste_built_at.map(|built| TasteRow {
            built_at: built.0,
            plays_counted: row.plays_counted.unwrap_or(0),
            history_from_at: row.history_from_at.map(|at| at.0),
            history_to_at: row.history_to_at.map(|at| at.0),
            seeds: row.seeds.unwrap_or(0),
        }),
        pool: row.pool_built_at.map(|built| PoolRow {
            built_at: built.0,
            incomplete: row.pool_incomplete.unwrap_or(false),
            waiting_for: row
                .waiting_for
                .as_deref()
                .and_then(|text| serde_json::from_str(text).ok())
                .unwrap_or_default(),
            neighbours_found: row.neighbours_found.unwrap_or(0),
            known_left_out: row.known_left_out.unwrap_or(0),
            hubs_left_out: row.hubs_left_out,
            depth: row.pool_depth.unwrap_or(Depth::DEFAULT),
        }),
        import_state: row.import_state,
    }))
}

/// The picks of `user_id`'s sets built at `since` or later, as memory
/// reads them: each counts as shown when its set was built.
///
/// # Errors
///
/// The read's failure.
pub(crate) async fn earlier_picks(
    db: &Db,
    user_id: &str,
    since: i64,
) -> Result<Vec<Shown>, DbError> {
    let user = user_id.to_owned();
    db.read("discover.weekly.earlier", move |conn| {
        conn.prepare_cached(sql::SET_EARLIER_PICKS.text())?
            .query_map(params![user, Millis(since)], sql::SetEarlierPick::read)?
            .map(|row| {
                row.map(|row| Shown {
                    release_group: row.release_group,
                    artist: row.artist_mbid,
                    last_shown_at: row.built_at.0,
                    sampled: row.sampled,
                    requested: row.requested,
                })
            })
            .collect::<rusqlite::Result<_>>()
            .map_err(DbError::from)
    })
    .await
}

/// `user_id`'s taste groups: number, anchor id, anchor name, share.
///
/// # Errors
///
/// The read's failure.
pub(crate) async fn taste_groups(
    db: &Db,
    user_id: &str,
) -> Result<Vec<(i64, String, String, f64)>, DbError> {
    let user = user_id.to_owned();
    db.read("discover.weekly.groups", move |conn| {
        conn.prepare_cached(sql::SET_TASTE_GROUPS.text())?
            .query_map([user], sql::SetTasteGroup::read)?
            .map(|row| {
                row.map(|row| {
                    (
                        row.group_no,
                        row.anchor_artist_mbid,
                        row.anchor_name,
                        row.share,
                    )
                })
            })
            .collect::<rusqlite::Result<_>>()
            .map_err(DbError::from)
    })
    .await
}

/// How many albums the library holds, and artists are credited on them.
///
/// # Errors
///
/// The read's failure.
pub(in crate::discover) async fn shelves(db: &Db) -> Result<sql::SetShelves, DbError> {
    db.read("discover.weekly.shelves", |conn| {
        conn.query_row(sql::SET_SHELVES.text(), [], sql::SetShelves::read)
            .map_err(DbError::from)
    })
    .await
}

/// The first failing fact among `artists`' fetch rows: its service and
/// when it first failed, unix ms.
///
/// # Errors
///
/// The read's failure.
pub(crate) async fn failing(
    db: &Db,
    artists: &[String],
) -> Result<Option<(FactSource, i64)>, DbError> {
    if artists.is_empty() {
        return Ok(None);
    }
    let asked = json(artists);
    db.read("discover.weekly.failing", move |conn| {
        conn.prepare_cached(sql::SET_FAILING.text())?
            .query_map(params![asked, Outcome::Failed], sql::SetFailing::read)?
            .next()
            .transpose()
            .map(|row| row.map(|row| (row.source, row.since.0)))
            .map_err(DbError::from)
    })
    .await
}

/// The taste list as stored: its shape's version, then the tastes.
#[derive(Debug, Serialize, Deserialize)]
struct Tastes {
    version: u32,
    tastes: Vec<SetTaste>,
}

/// The version the taste list is written at.
const TASTES_VERSION: u32 = 1;

/// One pick of a set about to be kept.
pub(crate) struct NewPick {
    /// Its place, from 1.
    pub pick_no: i64,
    /// The entry of the set's tastes it is filed under.
    pub taste_no: Option<i64>,
    /// The candidate it was dealt as.
    pub candidate: Candidate,
    /// Its reason.
    pub reason: PickReason,
    /// Its evidence as built now.
    pub evidence: PickEvidence,
    /// What the pick record keeps of it.
    pub offer: Offer,
}

/// A set about to be kept.
pub(crate) struct NewSet {
    /// When its week starts, unix ms.
    pub week_start_at: i64,
    /// When it was built, unix ms.
    pub built_at: i64,
    /// Why it is short, when it is.
    pub short_reason: Option<ShortReason>,
    /// The depth it was dealt at.
    pub depth: Depth,
    /// What it was made from.
    pub counts: SetCounts,
    /// The tastes its picks are filed under.
    pub tastes: Vec<SetTaste>,
    /// Its picks, in order.
    pub picks: Vec<NewPick>,
}

/// Keep `set` as `user_id`'s set for its week, with each pick offered on
/// the weekly set in the pick record, in one write that `complete` also
/// runs in. A week that has a set keeps it. Nothing is written once a
/// forget of theirs came after `forgets_at` (their forget count when the
/// build began), or once their account is gone.
///
/// A set is computed once, by the weekly set job, and from then on is a
/// record of what was shown: this is the one writer of its two tables, it
/// never replaces a set, and nothing rebuilds one.
///
/// # Errors
///
/// The write's or `complete`'s failure.
pub(crate) async fn keep(
    db: &Db,
    forgets: &ForgetCounts,
    user_id: &str,
    forgets_at: u64,
    set: NewSet,
    complete: impl FnOnce(&Transaction) -> Result<(), JobError> + Send + 'static,
) -> Result<Kept, JobError> {
    let tastes = serde_json::to_string(&Tastes {
        version: TASTES_VERSION,
        tastes: set.tastes,
    })
    .unwrap_or_else(|_| "{\"version\":1,\"tastes\":[]}".to_owned());
    let picks: Vec<(NewPick, String, String)> = set
        .picks
        .into_iter()
        .filter_map(|pick| {
            let evidence = serde_json::to_string(&pick.evidence).ok()?;
            let offered = serde_json::to_string(&pick.offer.evidence).ok()?;
            Some((pick, evidence, offered))
        })
        .collect();
    let (forgets, user) = (forgets.clone(), user_id.to_owned());
    let (week, built, short, depth, counts) = (
        Millis(set.week_start_at),
        Millis(set.built_at),
        set.short_reason,
        set.depth,
        set.counts,
    );
    db.write(Lane::Background, "discover.weekly.keep", move |tx| {
        let exists: bool = tx.query_row(sql::USER_EXISTS.text(), [&user], |row| row.get(0))?;
        if forgets.of(&user) != forgets_at || !exists {
            complete(tx)?;
            return Ok(Kept::Dropped);
        }
        let added = tx.execute(
            sql::SET_INSERT.text(),
            params![
                user,
                week,
                built,
                short,
                counts.plays_counted,
                counts.history_from_at.map(Millis),
                counts.history_to_at.map(Millis),
                counts.seeds,
                counts.neighbours_found,
                counts.known_left_out,
                counts.hubs_left_out,
                counts.shelf_artists,
                counts.shelf_albums,
                counts.linked,
                tastes,
                depth,
            ],
        )?;
        if added == 1 {
            let mut kept = tx.prepare_cached(sql::SET_PICK_INSERT.text())?;
            let mut offered = tx.prepare_cached(crate::discover::sql::OFFER_PICK.text())?;
            for (pick, evidence, offer_evidence) in &picks {
                let candidate = &pick.candidate;
                kept.execute(params![
                    user,
                    week,
                    pick.pick_no,
                    candidate.release_group_mbid,
                    candidate.release_group_mbid,
                    candidate.title,
                    candidate.artist_mbid,
                    candidate.artist_name,
                    candidate.primary_type,
                    candidate.first_release_date,
                    pick.taste_no,
                    pick.reason.line,
                    evidence,
                ])?;
                let offer = &pick.offer;
                offered.execute(params![
                    user,
                    offer.release_group_mbid.trim().to_ascii_lowercase(),
                    offer.release_group_mbid.trim(),
                    offer.artist_mbid.trim(),
                    offer.album_name,
                    offer.artist_name,
                    MadeFrom::WeeklySet,
                    built,
                    offer_evidence,
                ])?;
            }
        }
        complete(tx)?;
        Ok(Kept::Written)
    })
    .await
}

/// One set as stored.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct StoredSet {
    /// When its week starts, unix ms.
    pub week_start_at: i64,
    /// When it was built, unix ms.
    pub built_at: i64,
    /// Why it is short, when it is.
    pub short_reason: Option<ShortReason>,
    /// What it was made from.
    pub counts: SetCounts,
    /// The tastes its picks are filed under.
    pub tastes: Vec<SetTaste>,
}

/// `user_id`'s newest set; `None` before their first.
///
/// # Errors
///
/// The read's failure.
pub(crate) async fn latest(db: &Db, user_id: &str) -> Result<Option<StoredSet>, DbError> {
    let user = user_id.to_owned();
    let row = db
        .read("discover.weekly.latest", move |conn| {
            conn.prepare_cached(sql::SET_LATEST.text())?
                .query_map([user], sql::SetRow::read)?
                .next()
                .transpose()
                .map_err(DbError::from)
        })
        .await?;
    Ok(row.map(|row| {
        let tastes = match serde_json::from_str::<Tastes>(&row.tastes) {
            Ok(list) => list.tastes,
            Err(error) => {
                tracing::warn!(%error, "a kept set's tastes do not read");
                Vec::new()
            }
        };
        StoredSet {
            week_start_at: row.week_start_at.0,
            built_at: row.built_at.0,
            short_reason: row.short_reason,
            counts: SetCounts {
                plays_counted: row.plays_counted,
                history_from_at: row.history_from_at.map(|at| at.0),
                history_to_at: row.history_to_at.map(|at| at.0),
                seeds: row.seeds,
                neighbours_found: row.neighbours_found,
                known_left_out: row.known_left_out,
                hubs_left_out: row.hubs_left_out,
                shelf_artists: row.shelf_artists,
                shelf_albums: row.shelf_albums,
                linked: row.linked,
            },
            tastes,
        }
    }))
}

/// The albums of `user_id`'s newest set and their artists, lowercased.
///
/// # Errors
///
/// The read's failure.
pub(crate) async fn latest_picks(db: &Db, user_id: &str) -> Result<Vec<(String, String)>, DbError> {
    let user = user_id.to_owned();
    db.read("discover.weekly.latest_picks", move |conn| {
        conn.prepare_cached(sql::SET_LATEST_PICKS.text())?
            .query_map([user], sql::SetLatestPick::read)?
            .map(|row| row.map(|row| (row.release_group_mbid_lower, row.artist_mbid)))
            .collect::<rusqlite::Result<_>>()
            .map_err(DbError::from)
    })
    .await
}

/// The picks of `user_id`'s set for the week starting `week_start_at`, in
/// order, each with what became of it at `now_ms`.
///
/// # Errors
///
/// The read's failure.
pub(in crate::discover) async fn set_picks(
    db: &Db,
    user_id: &str,
    week_start_at: i64,
    now_ms: i64,
) -> Result<Vec<sql::SetPickRow>, DbError> {
    let user = user_id.to_owned();
    db.read("discover.weekly.picks", move |conn| {
        conn.prepare_cached(sql::SET_PICKS.text())?
            .query_map(
                params![
                    user,
                    Millis(week_start_at),
                    Millis(now_ms),
                    FeedbackTarget::Album,
                    FeedbackTarget::Artist
                ],
                sql::SetPickRow::read,
            )?
            .collect::<rusqlite::Result<_>>()
            .map_err(DbError::from)
    })
    .await
}

/// The evidence of `user_id`'s newest kept pick of `release_group`
/// (lowercased), as it was built; `None` when no set of theirs held it.
///
/// # Errors
///
/// The read's failure.
pub(crate) async fn stored_evidence(
    db: &Db,
    user_id: &str,
    release_group: &str,
) -> Result<Option<PickEvidence>, DbError> {
    let (user, group) = (user_id.to_owned(), release_group.to_owned());
    let text: Option<String> = db
        .read("discover.weekly.evidence", move |conn| {
            conn.prepare_cached(sql::SET_EVIDENCE.text())?
                .query_map(params![user, group], |row| row.get(0))?
                .next()
                .transpose()
                .map_err(DbError::from)
        })
        .await?;
    Ok(text.and_then(|text| match serde_json::from_str(&text) {
        Ok(evidence) => Some(evidence),
        Err(error) => {
            tracing::warn!(%error, "a kept pick's evidence does not read");
            None
        }
    }))
}

/// `user_id`'s set day and depth; `None` while they have chosen neither.
///
/// # Errors
///
/// The read's failure.
pub(crate) async fn prefs(db: &Db, user_id: &str) -> Result<Option<(SetDay, Depth)>, DbError> {
    let user = user_id.to_owned();
    db.read("discover.weekly.prefs", move |conn| {
        conn.prepare_cached(sql::PREFS_READ.text())?
            .query_map([user], sql::PrefsRow::read)?
            .next()
            .transpose()
            .map(|row| row.map(|row| (row.set_day, row.depth)))
            .map_err(DbError::from)
    })
    .await
}

/// `user_id`'s depth, [`Depth::DEFAULT`] while they have not chosen one.
///
/// # Errors
///
/// The read's failure.
pub(crate) async fn depth(db: &Db, user_id: &str) -> Result<Depth, DbError> {
    let user = user_id.to_owned();
    let depth = db
        .read("discover.weekly.depth", move |conn| {
            conn.prepare_cached(sql::PREFS_DEPTH.text())?
                .query_map([user], |row| row.get(0))?
                .next()
                .transpose()
                .map_err(DbError::from)
        })
        .await?;
    Ok(depth.unwrap_or(Depth::DEFAULT))
}

/// Keep `user_id`'s set day `day` and depth `depth`, chosen at `now_ms`;
/// when the depth is not the one kept before, `changed` runs in the same
/// write.
///
/// # Errors
///
/// The write's or `changed`'s failure.
pub(crate) async fn save_prefs(
    db: &Db,
    user_id: &str,
    (day, depth): (SetDay, Depth),
    now_ms: i64,
    changed: impl FnOnce(&Transaction) -> Result<(), JobError> + Send + 'static,
) -> Result<(), JobError> {
    let user = user_id.to_owned();
    db.write(Lane::Foreground, "discover.weekly.save_prefs", move |tx| {
        let before: Option<Depth> = tx
            .prepare_cached(sql::PREFS_DEPTH.text())?
            .query_map([&user], |row| row.get(0))?
            .next()
            .transpose()?;
        tx.execute(
            sql::PREFS_SAVE.text(),
            params![user, day, depth, Millis(now_ms)],
        )?;
        if before.unwrap_or(Depth::DEFAULT) != depth {
            changed(tx)?;
        }
        Ok(())
    })
    .await
}

/// Note that `user_id`'s first set began waiting for their history at
/// `now_ms`, when they have no set, their import is being read and no wait
/// was noted before.
///
/// # Errors
///
/// The write's failure.
pub(crate) async fn note_history_wait(db: &Db, user_id: &str, now_ms: i64) -> Result<(), DbError> {
    let user = user_id.to_owned();
    db.write(
        Lane::Background,
        "discover.weekly.history_wait",
        move |tx| {
            tx.execute(
                sql::WEEK_HISTORY_WAIT.text(),
                params![
                    user,
                    Millis(now_ms),
                    HistoryImportState::Pending,
                    HistoryImportState::Exporting,
                    HistoryImportState::Importing
                ],
            )?;
            Ok::<_, DbError>(())
        },
    )
    .await
}

/// Forget `user_id`'s sets and their picks, and when their first set began
/// waiting for their history, in one write. Their forget count moves on
/// inside it, so a set build of theirs that began before it keeps nothing.
/// Their set day stays. Answers how many sets and picks went.
///
/// # Errors
///
/// The write's failure.
pub(crate) async fn forget(
    db: &Db,
    forgets: &ForgetCounts,
    user_id: &str,
) -> Result<usize, DbError> {
    let (forgets, user) = (forgets.clone(), user_id.to_owned());
    db.write(Lane::Foreground, "discover.weekly.forget", move |tx| {
        forgets.bump(&user);
        let mut removed = 0;
        for statement in [sql::SET_PICKS_DELETE, sql::SETS_DELETE] {
            removed += tx.execute(statement.text(), [&user])?;
        }
        tx.execute(sql::PREFS_WAIT_CLEARED.text(), [&user])?;
        Ok::<_, DbError>(removed)
    })
    .await
}
