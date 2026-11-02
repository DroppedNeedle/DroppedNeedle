//! Reconciliation SQL over the 0001 tables.
//!
//! An album counts while it is not retired and still has an indexed file;
//! a track counts while it is indexed. Credits on anything else do not keep
//! a duplicate group open.
//!
//! A record a scan left with no credits at all, holding something a person
//! made, counts too while a recorded move of its credits is open
//! (`corrections::artist_convergence::record_move`): when every row its
//! credits went to shares its folded name, it joins their same-name group;
//! otherwise it heads a group of its own with those rows.

use std::collections::{BTreeMap, BTreeSet};

use rusqlite::{Connection, OptionalExtension as _, Transaction, params, params_from_iter};

use super::models::{
    Candidate, CreditEvidence, Dismissal, GroupInputs, GroupReferences, Member, MergeAction,
    MergeBasisLabel, OwnedReference, ReconcileError, ReferenceCounts,
};
use super::reasons;
use crate::corrections::artist_convergence::{MOVED_BY_NAME, MOVED_WITH_ID};
use crate::corrections::sql::{ReferenceCount, ReferenceKind, references_of};
use crate::reconcile::sql::{
    CandidateRow, DismissalRow, EvidenceRow, MergeActionRow, MergedArtist, MovedPair, OwnedRow,
    SharedPair,
};
use droppedneedle_kernel::db::statement::{Choice, ListLen};

/// Everything the grouping rules read.
pub fn inputs(conn: &Connection) -> Result<GroupInputs, ReconcileError> {
    let candidates = candidates(conn)?;
    let moved = moved_groups(conn)?;
    let shared = shared_groups(conn)?;
    let dismissals = conn
        .prepare(crate::reconcile::sql::INPUTS.text())?
        .query_map([], |row| {
            let row = DismissalRow::read(row)?;
            Ok(Dismissal {
                left_id: row.left_artist_id,
                right_id: row.right_artist_id,
                left_revision: row.left_artist_revision,
                right_revision: row.right_artist_revision,
            })
        })?
        .collect::<rusqlite::Result<_>>()?;
    let ambiguous_artist_ids = conn
        .prepare(crate::reconcile::sql::ARTISTS_OF_WAITING_ALBUMS.text())?
        .query_map([], |row| row.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    let merges = merges(conn)?;
    let merged_ids: BTreeSet<&str> = merges
        .iter()
        .flat_map(|m| std::iter::once(&m.survivor_id).chain(&m.retired_ids))
        .map(String::as_str)
        .collect();
    let (merged_artists, merged_reference_totals) = merged_artists(conn, &merged_ids)?;
    Ok(GroupInputs {
        candidates,
        moved,
        shared,
        dismissals,
        ambiguous_artist_ids,
        merges,
        merged_artists,
        merged_reference_totals,
    })
}

fn candidate_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Candidate> {
    let row = CandidateRow::read(row)?;
    Ok(Candidate {
        member: Member {
            id: row.id,
            name: row.display_name,
            sort_name: row.sort_name,
            row_revision: row.row_revision,
            provider_mbid: row.provider_artist_id,
            counts: ReferenceCounts {
                album_credits: row.album_credits,
                track_credits: row.track_credits,
                primary_albums: row.primary_albums,
                favorites: row.favorites,
                ratings: row.ratings,
                history: row.history,
                proven_credits: 0,
            },
        },
        folded_name: row.folded_name,
        created_at: row.created_at,
        proof_mbids: Vec::new(),
    })
}

/// Current artist records with indexed credits, or emptied by a scan
/// with every credit moved to a row of the same folded name (module
/// docs), whose folded name another such record shares, with their
/// reference counts and proof artists.
fn candidates(conn: &Connection) -> Result<Vec<Candidate>, ReconcileError> {
    let mut rows: Vec<Candidate> = conn
        .prepare(crate::reconcile::sql::SAME_NAME_CANDIDATES.text())?
        .query_map([], candidate_row)?
        .collect::<rusqlite::Result<_>>()?;
    with_proofs(conn, &mut rows)?;
    Ok(rows)
}

/// Each live record a scan emptied whose credits went, at least in part,
/// to a row of another folded name (module docs), first in its group,
/// followed by the live rows its credits went to.
fn moved_groups(conn: &Connection) -> Result<Vec<Vec<Candidate>>, ReconcileError> {
    // A pair moved both by name and with an id has a record of each kind:
    // DISTINCT lists its destination once.
    let pairs: Vec<(String, String)> = conn
        .prepare(crate::reconcile::sql::EMPTIED_MOVED_PAIRS.text())?
        .query_map(params![MOVED_BY_NAME, MOVED_WITH_ID], |row| {
            let row = MovedPair::read(row)?;
            Ok((row.left_artist_id, row.right_artist_id))
        })?
        .collect::<rusqlite::Result<_>>()?;
    let mut ids: Vec<(String, Vec<String>)> = Vec::new();
    for (from, to) in pairs {
        match ids.last_mut() {
            Some((last, dests)) if *last == from => dests.push(to),
            _ => ids.push((from, vec![to])),
        }
    }
    let mut member = conn.prepare(crate::reconcile::sql::CANDIDATE_BY_ID.text())?;
    let mut groups = Vec::with_capacity(ids.len());
    for (from, dests) in ids {
        let mut rows = Vec::with_capacity(dests.len() + 1);
        for id in std::iter::once(&from).chain(&dests) {
            rows.push(member.query_row(params![id], candidate_row)?);
        }
        with_proofs(conn, &mut rows)?;
        groups.push(rows);
    }
    Ok(groups)
}

/// Each open offer of two live records as one artist, recorded when a
/// person's link named a MusicBrainz artist the other holds, as its two
/// records, the older first.
fn shared_groups(conn: &Connection) -> Result<Vec<Vec<Candidate>>, ReconcileError> {
    let pairs: Vec<(String, String)> = conn
        .prepare(crate::reconcile::sql::SHARED_GROUPS.text())?
        .query_map(params![reasons::SHARED_PROVIDER_IDENTITY.code], |row| {
            let row = SharedPair::read(row)?;
            Ok((row.left_artist_id, row.right_artist_id))
        })?
        .collect::<rusqlite::Result<_>>()?;
    let mut member = conn.prepare(crate::reconcile::sql::CANDIDATE_BY_ID.text())?;
    let mut groups = Vec::with_capacity(pairs.len());
    for (left, right) in pairs {
        let mut rows = vec![
            member.query_row(params![left], candidate_row)?,
            member.query_row(params![right], candidate_row)?,
        ];
        rows.sort_by(|a, b| {
            a.created_at
                .total_cmp(&b.created_at)
                .then_with(|| a.member.id.cmp(&b.member.id))
        });
        with_proofs(conn, &mut rows)?;
        groups.push(rows);
    }
    Ok(groups)
}

/// Fill in each record's proof artists and proven credit count, by the
/// one current-proof rule merges use (`identify::sql::CURRENT_PROOF`).
fn with_proofs(conn: &Connection, rows: &mut [Candidate]) -> Result<(), ReconcileError> {
    let mut proof_mbids = conn.prepare(crate::reconcile::sql::PROVEN_MBIDS_OF_ARTIST.text())?;
    // A credit is proven when a current proof sits at its exact position
    // and names its row.
    let mut proven = conn.prepare(crate::reconcile::sql::ARTIST_CREDIT_COUNTS.text())?;
    for row in rows.iter_mut() {
        let id = row.member.id.clone();
        row.proof_mbids = proof_mbids
            .query_map(params![id], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        row.member.counts.proven_credits = proven.query_row(params![id], |r| r.get(0))?;
    }
    Ok(())
}

/// Automatic merges from the catalog action log, newest first. Rows whose
/// payload does not name a survivor and retired records are skipped.
fn merges(conn: &Connection) -> Result<Vec<MergeAction>, ReconcileError> {
    let rows: Vec<MergeActionRow> = conn
        .prepare(crate::reconcile::sql::AUTOMATIC_MERGE_ACTIONS.text())?
        .query_map([], MergeActionRow::read)?
        .collect::<rusqlite::Result<_>>()?;
    Ok(rows
        .into_iter()
        .filter_map(|row| {
            let MergeActionRow {
                id,
                after_json,
                reason_code,
                created_at,
            } = row;
            let created_at = crate::clock::at_secs(created_at);
            let after: serde_json::Value = match serde_json::from_str(&after_json) {
                Ok(value) => value,
                Err(error) => {
                    tracing::warn!(action_id = id, %error, "artist merge action has unreadable JSON; not listed");
                    return None;
                }
            };
            let survivor_id = after.get("surviving_artist_id")?.as_str()?.to_owned();
            let retired_ids: Vec<String> = after
                .get("retired_artist_ids")?
                .as_array()?
                .iter()
                .filter_map(|v| v.as_str().map(str::to_owned))
                .collect();
            if retired_ids.is_empty() {
                return None;
            }
            let reason_code = reason_code.unwrap_or_default();
            Some(MergeAction {
                id,
                basis: MergeBasisLabel::for_reason(&reason_code),
                reason_code,
                created_at,
                survivor_id,
                retired_ids,
                provider_mbid: after
                    .get("provider_artist_mbid")
                    .and_then(|v| v.as_str())
                    .map(str::to_owned),
            })
        })
        .collect())
}

/// Artists of past merges (retired ones included), and every reference
/// each one has now.
#[allow(clippy::type_complexity)]
fn merged_artists(
    conn: &Connection,
    ids: &BTreeSet<&str>,
) -> Result<(BTreeMap<String, Member>, BTreeMap<String, i64>), ReconcileError> {
    let mut artists = BTreeMap::new();
    let mut totals = BTreeMap::new();
    if ids.is_empty() {
        return Ok((artists, totals));
    }
    let mut stmt = conn.prepare(&crate::reconcile::sql::merged_artist_rows(ListLen::new(
        ids.len(),
    )))?;
    let rows = stmt.query_map(params_from_iter(ids.iter()), |row| {
        let row = MergedArtist::read(row)?;
        Ok(Member {
            id: row.id,
            name: row.display_name,
            sort_name: row.sort_name,
            row_revision: row.row_revision,
            provider_mbid: row.provider_artist_id,
            counts: ReferenceCounts::default(),
        })
    })?;
    for member in rows {
        let member = member?;
        artists.insert(member.id.clone(), member);
    }
    for kind in <ReferenceKind as Choice>::all() {
        let mut stmt = conn.prepare(&references_of(kind, ListLen::new(ids.len())))?;
        let rows = stmt.query_map(params_from_iter(ids.iter()), ReferenceCount::read)?;
        for row in rows {
            let row = row?;
            *totals.entry(row.id).or_insert(0) += row.count;
        }
    }
    Ok((artists, totals))
}

/// How many records were merged automatically so far.
pub fn progress(conn: &Connection) -> Result<i64, ReconcileError> {
    let merged = conn.query_row(
        crate::reconcile::sql::AUTOMATIC_MERGE_COUNT.text(),
        [],
        |row| row.get(0),
    )?;
    Ok(merged)
}

/// Proofs, albums, and tracks the given artists are credited on. Capped
/// at 500 proofs, 250 albums, 500 tracks.
pub fn references(conn: &Connection, ids: &[String]) -> Result<GroupReferences, ReconcileError> {
    let unique: BTreeSet<&str> = ids.iter().map(String::as_str).collect();
    if unique.is_empty() {
        return Ok(GroupReferences::default());
    }
    let twice = || unique.iter().chain(unique.iter());
    let evidence = conn
        .prepare(&crate::reconcile::sql::group_credit_evidence(ListLen::new(
            unique.len(),
        )))?
        .query_map(params_from_iter(twice()), |row| {
            let row = EvidenceRow::read(row)?;
            Ok(CreditEvidence {
                subject_kind: row.subject_kind,
                subject_id: row.subject_id,
                subject_name: row.subject_name,
                source_local_artist_id: row.source_local_artist_id,
                local_artist_id: row.local_artist_id,
                artist_mbid: row.artist_mbid,
                canonical_name: row.canonical_name,
                credited_name: row.credited_name,
                join_phrase: row.join_phrase,
                release_mbid: row.release_mbid,
                release_track_mbid: row.release_track_mbid,
                album_identity_revision: row.album_identity_revision,
                track_identity_revision: row.track_identity_revision,
                evidence_hash: row.evidence_hash,
            })
        })?
        .collect::<rusqlite::Result<_>>()?;
    let owned = |row: &rusqlite::Row<'_>| {
        let row = OwnedRow::read(row)?;
        Ok(OwnedReference {
            id: row.id,
            name: row.title,
            row_revision: row.row_revision,
            identity_ready: row.identity_ready,
            exact_track_mapping_ready: row.exact_track_mapping_ready,
        })
    };
    let releases = conn
        .prepare(&crate::reconcile::sql::group_owned_albums(ListLen::new(
            unique.len(),
        )))?
        .query_map(params_from_iter(twice()), owned)?
        .collect::<rusqlite::Result<_>>()?;
    let tracks = conn
        .prepare(&crate::reconcile::sql::group_owned_tracks(ListLen::new(
            unique.len(),
        )))?
        .query_map(params_from_iter(unique.iter()), owned)?
        .collect::<rusqlite::Result<_>>()?;
    Ok(GroupReferences {
        evidence,
        releases,
        tracks,
    })
}

/// Mark every pair of `artist_ids` as distinct at the revisions the
/// administrator saw, and close matching open merge suggestions. Refuses
/// when any record moved on or was merged. Returns the pairs recorded.
pub fn dismiss(
    tx: &Transaction<'_>,
    artist_ids: &[String],
    expected: &BTreeMap<String, i64>,
    actor_user_id: &str,
    now: f64,
) -> Result<usize, ReconcileError> {
    let unique: Vec<&str> = artist_ids
        .iter()
        .map(String::as_str)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    if unique.len() < 2 {
        return Err(ReconcileError::Invalid(reasons::GROUP_NOT_FOUND));
    }
    let mut revisions: BTreeMap<&str, i64> = BTreeMap::new();
    for id in &unique {
        let revision: Option<i64> = tx
            .query_row(
                crate::reconcile::sql::LIVE_ARTIST_REVISION.text(),
                params![id],
                |row| row.get(0),
            )
            .optional()?;
        match revision {
            Some(current) if Some(current) == expected.get(*id).copied() => {
                revisions.insert(id, current);
            }
            _ => return Err(ReconcileError::Conflict(reasons::GROUP_STALE)),
        }
    }
    let mut insert = tx.prepare(crate::reconcile::sql::WRITE_DISMISSAL.text())?;
    let mut pairs = 0;
    let pinned: Vec<(&str, i64)> = revisions.into_iter().collect();
    for (index, (left, left_revision)) in pinned.iter().enumerate() {
        for (right, right_revision) in &pinned[index + 1..] {
            insert.execute(params![
                left,
                right,
                left_revision,
                right_revision,
                actor_user_id,
                crate::clock::ms(now)
            ])?;
            pairs += 1;
        }
    }
    let mut values: Vec<rusqlite::types::Value> = vec![crate::clock::ms(now).into()];
    for _ in 0..2 {
        values.extend(unique.iter().map(|id| (*id).to_owned().into()));
    }
    tx.execute(
        &crate::reconcile::sql::dismiss_group_candidates(ListLen::new(unique.len())),
        params_from_iter(values),
    )?;
    Ok(pairs)
}
