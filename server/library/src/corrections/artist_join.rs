//! The undo of a join of artist records made because they share a name
//! (`artist_convergence::join_namesakes`).
//!
//! Just before such a join moves anything off a record, `retire_artist`
//! records what it is about to move ([`record_retiree`]): its credits,
//! the albums it leads, its credit proofs, aliases and source links, the
//! favourites, ratings and plays on it, and its open merge candidates.
//! The join's audit row keeps that record as its `before_json`
//! ([`ArtistJoin`]).
//!
//! An administrator's undo ([`undo_join`]) brings each retired record back
//! as a live record of its own and puts back every recorded row that is
//! still where the join left it: on the record the join kept, or the one
//! a later merge moved it to. A row that moved again since (a file read
//! again by a later scan, say) stays where it is, and so does everything
//! made after the join. Every pair of the records is then marked distinct,
//! so no later pass joins them again. No file is read or written.

use std::collections::{BTreeMap, BTreeSet};

use rusqlite::{OptionalExtension as _, Transaction, params};
use serde::{Deserialize, Serialize};

use super::artists::{CREDITS_MOVED, MOVED_BY_NAME, MOVED_WITH_ID, SAME_NAME_JOINED};
use super::sql;
use crate::catalog_actions::ActionKind;
use crate::catalog_revision::bump_catalog;
use droppedneedle_auth::Admin;

/// The reason a dismissal written by an undo records.
pub(crate) const UNDONE_JOIN: &str = "UNDONE_JOIN";

/// What a same-name join moved, as its audit row keeps it.
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct ArtistJoin {
    /// The record the join kept.
    pub survivor: String,
    /// Its sort name before the join.
    pub survivor_sort_name: Option<String>,
    /// The sort name the join gave it, when it had none.
    pub filled_sort_name: Option<String>,
    pub retirees: Vec<Retiree>,
}

/// One record the join retired, and what moved off it.
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct Retiree {
    pub id: String,
    pub display_name: String,
    pub sort_name: Option<String>,
    pub revision: i64,
    /// (album, position) of its album credits.
    pub album_credits: Vec<(String, i64)>,
    /// (track, position) of its track credits.
    pub track_credits: Vec<(String, i64)>,
    /// Albums it was the album artist of.
    pub led_albums: Vec<String>,
    pub proofs: Vec<ProofKey>,
    pub aliases: Vec<String>,
    /// Source links that moved to the survivor.
    pub source_links: Vec<String>,
    /// Source links dropped because the survivor had a copy.
    pub dropped_source_links: Vec<SourceLink>,
    pub favorites: Vec<Favorite>,
    pub ratings: Vec<Rating>,
    pub plays: Vec<String>,
    pub candidates: Vec<Candidate>,
}

/// A credit proof, and which of its columns named the record.
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct ProofKey {
    pub subject_kind: String,
    pub subject_id: String,
    pub position: i64,
    pub source: bool,
    pub local: bool,
}

/// A source link row as it was.
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct SourceLink {
    pub id: String,
    pub provider: String,
    pub external_entity_type: String,
    pub external_id: String,
    pub canonical_url: String,
    pub decision_source: String,
    pub selected_by_user_id: Option<String>,
    pub verified_at: i64,
    pub created_at: i64,
    pub updated_at: i64,
    pub row_revision: i64,
}

/// A user's favourite of the record, and whether the join made the
/// survivor's copy (the user had none there).
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct Favorite {
    pub user_id: String,
    pub created_at: i64,
    pub display_name: Option<String>,
    pub created: bool,
}

/// A user's rating of the record, and whether the join made the
/// survivor's.
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct Rating {
    pub user_id: String,
    pub rating: i64,
    pub rated_at: i64,
    pub created: bool,
}

/// An open merge candidate naming the record, and whether the join
/// repointed it to the survivor rather than resolving it.
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct Candidate {
    pub id: String,
    pub repointed: bool,
}

/// What a join is about to move off `retiree` into `survivor`, read in the
/// join's transaction before anything moves.
pub(super) fn record_retiree(
    tx: &Transaction<'_>,
    retiree: &str,
    survivor: &str,
) -> rusqlite::Result<Retiree> {
    let names = tx
        .prepare_cached(sql::JOIN_NAMES.text())?
        .query_row(params![retiree], sql::JoinNames::read)?;
    let credits = |statement: droppedneedle_kernel::db::statement::Statement| {
        tx.prepare_cached(statement.text())?
            .query_map(params![retiree], |row| {
                let row = sql::JoinCredit::read(row)?;
                Ok((row.subject, row.position))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()
    };
    let ids = |statement: droppedneedle_kernel::db::statement::Statement| {
        tx.prepare_cached(statement.text())?
            .query_map(params![retiree], |row| row.get(0))?
            .collect::<rusqlite::Result<Vec<String>>>()
    };
    let proofs = tx
        .prepare_cached(sql::JOIN_PROOFS.text())?
        .query_map(params![retiree], |row| {
            let row = sql::JoinProof::read(row)?;
            Ok(ProofKey {
                subject_kind: row.subject_kind,
                subject_id: row.subject_id,
                position: row.credit_position,
                source: row.source,
                local: row.local,
            })
        })?
        .collect::<rusqlite::Result<_>>()?;
    let source_links = tx
        .prepare_cached(sql::JOIN_SOURCE_LINKS_MOVED.text())?
        .query_map(params![retiree, survivor], |row| row.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    let dropped_source_links = tx
        .prepare_cached(sql::JOIN_SOURCE_LINKS_DROPPED.text())?
        .query_map(params![retiree, survivor], |row| {
            let row = sql::JoinSourceLink::read(row)?;
            Ok(SourceLink {
                id: row.id,
                provider: row.provider,
                external_entity_type: row.external_entity_type,
                external_id: row.external_id,
                canonical_url: row.canonical_url,
                decision_source: row.decision_source,
                selected_by_user_id: row.selected_by_user_id,
                verified_at: row.verified_at,
                created_at: row.created_at,
                updated_at: row.updated_at,
                row_revision: row.row_revision,
            })
        })?
        .collect::<rusqlite::Result<_>>()?;
    let favorites = tx
        .prepare_cached(sql::JOIN_FAVORITES.text())?
        .query_map(params![retiree, survivor], |row| {
            let row = sql::JoinFavorite::read(row)?;
            Ok(Favorite {
                user_id: row.user_id,
                created_at: row.created_at,
                display_name: row.display_name,
                created: row.created,
            })
        })?
        .collect::<rusqlite::Result<_>>()?;
    let ratings = tx
        .prepare_cached(sql::JOIN_RATINGS.text())?
        .query_map(params![retiree, survivor], |row| {
            let row = sql::JoinRating::read(row)?;
            Ok(Rating {
                user_id: row.user_id,
                rating: row.rating,
                rated_at: row.rated_at,
                created: row.created,
            })
        })?
        .collect::<rusqlite::Result<_>>()?;
    let candidates = tx
        .prepare_cached(sql::JOIN_CANDIDATES.text())?
        .query_map(
            params![
                retiree,
                survivor,
                CREDITS_MOVED,
                MOVED_BY_NAME,
                MOVED_WITH_ID
            ],
            |row| {
                let row = sql::JoinCandidate::read(row)?;
                Ok(Candidate {
                    id: row.id,
                    repointed: row.repointed,
                })
            },
        )?
        .collect::<rusqlite::Result<_>>()?;
    Ok(Retiree {
        id: retiree.to_owned(),
        display_name: names.display_name,
        sort_name: names.sort_name,
        revision: names.row_revision,
        album_credits: credits(sql::JOIN_ALBUM_CREDITS)?,
        track_credits: credits(sql::JOIN_TRACK_CREDITS)?,
        led_albums: ids(sql::JOIN_LED_ALBUMS)?,
        proofs,
        aliases: ids(sql::JOIN_ALIASES)?,
        source_links,
        dropped_source_links,
        favorites,
        ratings,
        plays: ids(sql::JOIN_PLAYS)?,
        candidates,
    })
}

/// Why an undo was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum UndoRefused {
    /// No action has that id.
    NotFound,
    /// The action is not a same-name join with a readable record, or a
    /// record it retired is live again.
    NotUndoable,
    /// The join was undone already.
    Undone,
}

/// What an undo did: the records brought back, and by kind how many
/// recorded rows went back and how many stay where they are now.
#[derive(Debug, Clone)]
pub(crate) struct UndoneJoin {
    pub survivor: String,
    pub restored: Vec<String>,
    pub put_back: BTreeMap<&'static str, u64>,
    pub left: BTreeMap<&'static str, u64>,
}

impl UndoneJoin {
    fn count(&mut self, kind: &'static str, moved: bool) {
        let tally = if moved {
            &mut self.put_back
        } else {
            &mut self.left
        };
        *tally.entry(kind).or_insert(0) += 1;
    }
}

/// The idempotency key of the undo of action `action_id`.
fn undo_key(action_id: &str) -> String {
    format!("merge-artist-undone:{action_id}")
}

/// Whether the join recorded by action `action_id` was undone.
pub(crate) fn undone(tx: &rusqlite::Connection, action_id: &str) -> rusqlite::Result<bool> {
    tx.prepare_cached(sql::ACTION_WITH_KEY.text())?
        .query_row(params![undo_key(action_id)], |row| row.get(0))
}

/// Undo the same-name join recorded by action `action_id` (module docs),
/// in one transaction. A refusal writes nothing.
pub(crate) fn undo_join(
    tx: &Transaction<'_>,
    admin: &Admin,
    action_id: &str,
    now: f64,
) -> rusqlite::Result<Result<UndoneJoin, UndoRefused>> {
    if undone(tx, action_id)? {
        return Ok(Err(UndoRefused::Undone));
    }
    let action = tx
        .prepare_cached(sql::JOIN_ACTION.text())?
        .query_row(params![action_id], sql::JoinAction::read)
        .optional()?;
    let Some(action) = action else {
        return Ok(Err(UndoRefused::NotFound));
    };
    if action.action_kind != ActionKind::MergeArtist
        || action.reason_code.as_deref() != Some(SAME_NAME_JOINED)
    {
        return Ok(Err(UndoRefused::NotUndoable));
    }
    let join: ArtistJoin = match serde_json::from_str(&action.before_json) {
        Ok(join) => join,
        Err(error) => {
            tracing::warn!(action_id, %error, "artist join has an unreadable undo record");
            return Ok(Err(UndoRefused::NotUndoable));
        }
    };
    let mut targets = Vec::with_capacity(join.retirees.len());
    for retiree in &join.retirees {
        let retired: Option<Option<String>> = tx
            .prepare_cached(sql::ARTIST_RETIRED_INTO.text())?
            .query_row(params![retiree.id], |row| row.get(0))
            .optional()?;
        let live: Option<String> = tx
            .prepare_cached(sql::LIVE_ROW.text())?
            .query_row(params![retiree.id], |row| row.get(0))
            .optional()?;
        match (retired, live) {
            (Some(Some(_)), Some(live)) if live != retiree.id => targets.push(live),
            _ => return Ok(Err(UndoRefused::NotUndoable)),
        }
    }
    let ms = crate::clock::ms(now);
    let mut done = UndoneJoin {
        survivor: join.survivor.clone(),
        restored: Vec::new(),
        put_back: BTreeMap::new(),
        left: BTreeMap::new(),
    };
    // Each retiree was recorded after the ones before it had moved (a later
    // one finds the survivor's copy of a favourite an earlier one made), so
    // they go back last first.
    for (retiree, target) in join.retirees.iter().zip(&targets).rev() {
        put_back(tx, retiree, target, ms, &mut done)?;
        done.restored.push(retiree.id.clone());
    }
    if let Some(filled) = &join.filled_sort_name {
        tx.prepare_cached(sql::RESTORE_SORT_NAME.text())?
            .execute(params![join.survivor, filled, join.survivor_sort_name, ms])?;
    }
    let mut members: BTreeSet<&str> = targets.iter().map(String::as_str).collect();
    for target in &members {
        tx.prepare_cached(sql::BUMP_ARTIST_REVISION.text())?
            .execute(params![target, ms])?;
    }
    members.extend(join.retirees.iter().map(|retiree| retiree.id.as_str()));
    mark_distinct(tx, admin, &members, ms)?;
    bump_catalog(tx)?;
    tx.prepare_cached(crate::catalog_actions::sql::RECORD_CORRECTION.text())?
        .execute(params![
            uuid::Uuid::new_v4().to_string(),
            undo_key(action_id),
            admin.user_id(),
            ActionKind::MergeArtistUndone,
            join.survivor,
            None::<&str>,
            serde_json::json!({ "undoes": action_id }).to_string(),
            serde_json::json!({
                "kind": "same_name_join_undone",
                "surviving_artist_id": join.survivor,
                "restored_artist_ids": done.restored,
                "put_back": done.put_back,
                "left": done.left,
                "filesystem_writes": 0,
            })
            .to_string(),
            UNDONE_JOIN,
            ms,
        ])?;
    Ok(Ok(done))
}

/// Bring `retiree` back as a live record and move back each recorded row
/// still on `target`, where the join (or a later merge) left it.
fn put_back(
    tx: &Transaction<'_>,
    retiree: &Retiree,
    target: &str,
    ms: i64,
    done: &mut UndoneJoin,
) -> rusqlite::Result<()> {
    let id = retiree.id.as_str();
    tx.prepare_cached(sql::UNRETIRE_ARTIST.text())?
        .execute(params![id, ms])?;
    tx.prepare_cached(sql::DROP_MERGED_ALIAS.text())?
        .execute(params![id])?;
    let moved = |statement: droppedneedle_kernel::db::statement::Statement,
                 values: &[&dyn rusqlite::ToSql]|
     -> rusqlite::Result<bool> {
        Ok(tx.prepare_cached(statement.text())?.execute(values)? > 0)
    };
    for alias in &retiree.aliases {
        let back = moved(sql::UNDO_ALIAS, params![alias, target, id])?;
        done.count("aliases", back);
    }
    for (album, position) in &retiree.album_credits {
        let back = moved(sql::UNDO_ALBUM_CREDIT, params![album, target, id, position])?;
        done.count("album_credits", back);
    }
    for (track, position) in &retiree.track_credits {
        let back = moved(sql::UNDO_TRACK_CREDIT, params![track, target, id, position])?;
        done.count("track_credits", back);
    }
    for album in &retiree.led_albums {
        let back = moved(sql::UNDO_LED_ALBUM, params![album, target, id, ms])?;
        done.count("albums_led", back);
    }
    for proof in &retiree.proofs {
        let back = moved(
            sql::UNDO_PROOF,
            params![
                ms,
                target,
                id,
                proof.subject_kind,
                proof.subject_id,
                proof.position,
                proof.local,
                proof.source
            ],
        )?;
        done.count("proofs", back);
    }
    for link in &retiree.source_links {
        let back = moved(sql::UNDO_SOURCE_LINK, params![link, target, id, ms])?;
        done.count("source_links", back);
    }
    for link in &retiree.dropped_source_links {
        let back = moved(
            sql::RESTORE_SOURCE_LINK,
            params![
                link.id,
                id,
                link.provider,
                link.external_entity_type,
                link.external_id,
                link.canonical_url,
                link.decision_source,
                link.selected_by_user_id,
                link.verified_at,
                link.created_at,
                link.updated_at,
                link.row_revision
            ],
        )?;
        done.count("source_links", back);
    }
    for play in &retiree.plays {
        let back = moved(sql::UNDO_PLAY, params![play, target, id])?;
        done.count("plays", back);
    }
    for candidate in &retiree.candidates {
        moved(
            sql::UNDO_CANDIDATE,
            params![candidate.id, target, id, candidate.repointed, ms],
        )?;
    }
    for favorite in &retiree.favorites {
        let user = favorite.user_id.as_str();
        let on_target: Option<i64> = tx
            .prepare_cached(sql::FAVORITE_AT.text())?
            .query_row(params![user, target], |row| row.get(0))
            .optional()?;
        let Some(at) = on_target else {
            done.count("favorites", false);
            continue;
        };
        moved(
            sql::RESTORE_FAVORITE,
            params![user, id, favorite.created_at],
        )?;
        if let Some(name) = &favorite.display_name {
            moved(sql::RESTORE_FAVORITE_NAME, params![user, id, name])?;
        }
        if favorite.created && at == favorite.created_at {
            moved(sql::DROP_FAVORITE, params![user, target])?;
        }
        done.count("favorites", true);
    }
    for rating in &retiree.ratings {
        let user = rating.user_id.as_str();
        let on_target = tx
            .prepare_cached(sql::RATING_OF.text())?
            .query_row(params![user, target], sql::RatingOf::read)
            .optional()?;
        let Some(on_target) = on_target else {
            done.count("ratings", false);
            continue;
        };
        moved(
            sql::RESTORE_RATING,
            params![user, id, rating.rating, rating.rated_at],
        )?;
        if rating.created
            && on_target.rating == rating.rating
            && on_target.rated_at == rating.rated_at
        {
            moved(sql::DROP_RATING, params![user, target])?;
        }
        done.count("ratings", true);
    }
    Ok(())
}

/// Mark every pair of `members` distinct, by the administrator, at the
/// revisions they have now.
fn mark_distinct(
    tx: &Transaction<'_>,
    admin: &Admin,
    members: &BTreeSet<&str>,
    ms: i64,
) -> rusqlite::Result<()> {
    let mut revisions: Vec<(&str, i64)> = Vec::with_capacity(members.len());
    for member in members {
        let revision: Option<i64> = tx
            .prepare_cached(sql::LIVE_ARTIST_REVISION.text())?
            .query_row(params![member], |row| row.get(0))
            .optional()?;
        if let Some(revision) = revision {
            revisions.push((member, revision));
        }
    }
    for (index, (left, left_revision)) in revisions.iter().enumerate() {
        for (right, right_revision) in &revisions[index + 1..] {
            tx.prepare_cached(sql::WRITE_UNDONE_DISMISSAL.text())?
                .execute(params![
                    left,
                    right,
                    left_revision,
                    right_revision,
                    admin.user_id(),
                    ms,
                    UNDONE_JOIN
                ])?;
        }
    }
    Ok(())
}
