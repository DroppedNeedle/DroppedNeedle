//! Reconciliation SQL over the 0001 tables.
//!
//! An album counts while it is not retired and still has an indexed file;
//! a track counts while it is indexed. Credits on anything else do not keep
//! a duplicate group open.

use std::collections::{BTreeMap, BTreeSet};

use rusqlite::{Connection, OptionalExtension as _, Transaction, params, params_from_iter};

use super::models::{
    Candidate, CreditEvidence, Dismissal, GroupInputs, GroupReferences, Member, MergeAction,
    OwnedReference, ReconcileError, ReconcileJob, ReferenceCounts,
};
use super::reasons;

/// Catalog action reasons that record an automatic artist merge (v2).
const MERGE_REASONS: &str = "('AUTOMATIC_PROVIDER_PROVEN_ARTIST_CONVERGENCE', \
     'AUTOMATIC_PROVIDER_ANCHORED_ARTIST_CONVERGENCE')";

/// `album` still has an indexed file and was not merged away.
const ACTIVE_ALBUM: &str = "album.retired_into_album_id IS NULL AND EXISTS (SELECT 1 \
     FROM local_tracks live WHERE live.local_album_id = album.id AND live.availability = 'indexed')";

/// The proof still matches the album's (and track's) current identity.
const CURRENT_PROOF: &str = "proof.subject_kind = 'album' OR (track.availability = 'indexed' \
     AND track_identity.row_revision = proof.track_identity_revision \
     AND track_identity.release_mbid = proof.release_mbid \
     AND track_identity.release_track_mbid = proof.release_track_mbid)";

/// Joins that let [`CURRENT_PROOF`] check a proof row.
const PROOF_JOINS: &str = "JOIN local_albums album ON album.id = proof.local_album_id \
     JOIN local_album_external_identities album_identity \
       ON album_identity.local_album_id = proof.local_album_id \
      AND album_identity.provider = 'musicbrainz' \
      AND album_identity.row_revision = proof.album_identity_revision \
      AND album_identity.release_mbid = proof.release_mbid \
     LEFT JOIN local_track_external_identities track_identity \
       ON proof.subject_kind = 'track' AND track_identity.local_track_id = proof.local_track_id \
      AND track_identity.provider = 'musicbrainz' \
     LEFT JOIN local_tracks track ON proof.subject_kind = 'track' \
      AND track.id = proof.local_track_id";

fn placeholders(n: usize) -> String {
    vec!["?"; n].join(",")
}

/// Everything the grouping rules read.
pub fn inputs(conn: &Connection) -> Result<GroupInputs, ReconcileError> {
    let candidates = candidates(conn)?;
    let dismissals = conn
        .prepare(
            "SELECT left_artist_id, right_artist_id, left_artist_revision, \
             right_artist_revision FROM library_artist_reconciliation_dismissals",
        )?
        .query_map([], |row| {
            Ok(Dismissal {
                left_id: row.get(0)?,
                right_id: row.get(1)?,
                left_revision: row.get(2)?,
                right_revision: row.get(3)?,
            })
        })?
        .collect::<rusqlite::Result<_>>()?;
    let ambiguous_artist_ids = conn
        .prepare(&format!(
            "SELECT DISTINCT album.album_artist_id FROM library_artist_reconciliation_state state \
             JOIN local_albums album ON album.id = state.local_album_id \
             WHERE state.state = 'ambiguous_credit_structure' AND {ACTIVE_ALBUM}"
        ))?
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
        dismissals,
        ambiguous_artist_ids,
        merges,
        merged_artists,
        merged_reference_totals,
    })
}

/// Current artist records with indexed credits whose folded name another
/// such record shares, with their reference counts and proof artists.
fn candidates(conn: &Connection) -> Result<Vec<Candidate>, ReconcileError> {
    let sql = format!(
        "WITH active_artists AS ( \
           SELECT credit.local_artist_id id FROM local_album_artists credit \
           JOIN local_albums album ON album.id = credit.local_album_id WHERE {ACTIVE_ALBUM} \
           UNION SELECT credit.local_artist_id FROM local_track_artists credit \
           JOIN local_tracks track ON track.id = credit.local_track_id \
           JOIN local_albums album ON album.id = track.local_album_id \
           WHERE album.retired_into_album_id IS NULL AND track.availability = 'indexed'), \
         current_artists AS ( \
           SELECT artist.* FROM local_artists artist JOIN active_artists a ON a.id = artist.id \
           WHERE artist.retired_into_artist_id IS NULL), \
         duplicate_names AS ( \
           SELECT folded_name FROM current_artists GROUP BY folded_name HAVING COUNT(*) > 1) \
         SELECT artist.id, artist.display_name, artist.sort_name, artist.row_revision, \
           identity.provider_artist_id, artist.folded_name, artist.created_at, \
           (SELECT COUNT(*) FROM local_album_artists credit \
              JOIN local_albums album ON album.id = credit.local_album_id \
              WHERE credit.local_artist_id = artist.id AND {ACTIVE_ALBUM}), \
           (SELECT COUNT(*) FROM local_track_artists credit \
              JOIN local_tracks track ON track.id = credit.local_track_id \
              JOIN local_albums album ON album.id = track.local_album_id \
              WHERE credit.local_artist_id = artist.id \
                AND album.retired_into_album_id IS NULL AND track.availability = 'indexed'), \
           (SELECT COUNT(*) FROM local_albums album \
              WHERE album.album_artist_id = artist.id AND {ACTIVE_ALBUM}), \
           (SELECT COUNT(*) FROM library_user_favorites \
              WHERE item_kind = 'artist' AND item_id = artist.id), \
           (SELECT COUNT(*) FROM library_playlist_tracks WHERE local_artist_id = artist.id), \
           (SELECT COUNT(*) FROM library_play_history WHERE local_artist_id = artist.id), \
           (SELECT COUNT(*) FROM library_compat_id_map \
              WHERE kind = 'artist' AND internal_id = artist.id) \
         FROM current_artists artist \
         LEFT JOIN local_artist_external_identities identity \
           ON identity.local_artist_id = artist.id AND identity.provider = 'musicbrainz' \
         WHERE artist.folded_name IN (SELECT folded_name FROM duplicate_names) \
         ORDER BY artist.folded_name, artist.created_at, artist.id"
    );
    let mut rows: Vec<Candidate> = conn
        .prepare(&sql)?
        .query_map([], |row| {
            Ok(Candidate {
                member: Member {
                    id: row.get(0)?,
                    name: row.get(1)?,
                    sort_name: row.get(2)?,
                    row_revision: row.get(3)?,
                    provider_mbid: row.get(4)?,
                    counts: ReferenceCounts {
                        album_credits: row.get(7)?,
                        track_credits: row.get(8)?,
                        primary_albums: row.get(9)?,
                        favorites: row.get(10)?,
                        playlist_snapshots: row.get(11)?,
                        history: row.get(12)?,
                        compatibility_ids: row.get(13)?,
                        proven_credits: 0,
                    },
                },
                folded_name: row.get(5)?,
                created_at: row.get(6)?,
                proof_mbids: Vec::new(),
            })
        })?
        .collect::<rusqlite::Result<_>>()?;

    let mut proof_mbids = conn.prepare(&format!(
        "SELECT DISTINCT proof.artist_mbid FROM library_artist_credit_proofs proof {PROOF_JOINS} \
         WHERE proof.source_local_artist_id = ?1 AND {ACTIVE_ALBUM} AND ({CURRENT_PROOF}) \
         ORDER BY proof.artist_mbid"
    ))?;
    // A credit is proven when a proof sits at its exact position and at
    // the album's (and track's) current identity.
    let mut proven = conn.prepare(&format!(
        "SELECT \
           (SELECT COUNT(*) FROM local_album_artists credit \
              JOIN local_albums album ON album.id = credit.local_album_id \
              JOIN local_album_external_identities identity \
                ON identity.local_album_id = credit.local_album_id \
               AND identity.provider = 'musicbrainz' \
              JOIN library_artist_credit_proofs proof ON proof.subject_kind = 'album' \
               AND proof.subject_id = credit.local_album_id \
               AND proof.credit_position = credit.position \
               AND proof.source_local_artist_id = credit.local_artist_id \
               AND proof.album_identity_revision = identity.row_revision \
               AND proof.release_mbid = identity.release_mbid \
              WHERE credit.local_artist_id = ?1 AND {ACTIVE_ALBUM}) + \
           (SELECT COUNT(*) FROM local_track_artists credit \
              JOIN local_tracks track ON track.id = credit.local_track_id \
              JOIN local_albums album ON album.id = track.local_album_id \
              JOIN local_album_external_identities album_identity \
                ON album_identity.local_album_id = track.local_album_id \
               AND album_identity.provider = 'musicbrainz' \
              JOIN local_track_external_identities track_identity \
                ON track_identity.local_track_id = track.id \
               AND track_identity.provider = 'musicbrainz' \
              JOIN library_artist_credit_proofs proof ON proof.subject_kind = 'track' \
               AND proof.subject_id = credit.local_track_id \
               AND proof.credit_position = credit.position \
               AND proof.source_local_artist_id = credit.local_artist_id \
               AND proof.album_identity_revision = album_identity.row_revision \
               AND proof.track_identity_revision = track_identity.row_revision \
               AND proof.release_mbid = album_identity.release_mbid \
               AND proof.release_track_mbid = track_identity.release_track_mbid \
              WHERE credit.local_artist_id = ?1 \
                AND album.retired_into_album_id IS NULL AND track.availability = 'indexed')"
    ))?;
    for row in &mut rows {
        let id = row.member.id.clone();
        row.proof_mbids = proof_mbids
            .query_map(params![id], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        row.member.counts.proven_credits = proven.query_row(params![id], |r| r.get(0))?;
    }
    Ok(rows)
}

/// Automatic merges from the catalog action log, newest first. Rows whose
/// payload does not name a survivor and retired records are skipped.
fn merges(conn: &Connection) -> Result<Vec<MergeAction>, ReconcileError> {
    let rows: Vec<(String, String, Option<String>, f64)> = conn
        .prepare(&format!(
            "SELECT id, after_json, reason_code, created_at FROM library_catalog_actions \
             WHERE reason_code IN {MERGE_REASONS} ORDER BY created_at DESC, id DESC"
        ))?
        .query_map([], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
        })?
        .collect::<rusqlite::Result<_>>()?;
    Ok(rows
        .into_iter()
        .filter_map(|(id, after, reason_code, created_at)| {
            let after: serde_json::Value = match serde_json::from_str(&after) {
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
            Some(MergeAction {
                id,
                reason_code: reason_code.unwrap_or_default(),
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
    let marks = placeholders(ids.len());
    let mut stmt = conn.prepare(&format!(
        "SELECT artist.id, artist.display_name, artist.sort_name, artist.row_revision, \
           identity.provider_artist_id FROM local_artists artist \
         LEFT JOIN local_artist_external_identities identity \
           ON identity.local_artist_id = artist.id AND identity.provider = 'musicbrainz' \
         WHERE artist.id IN ({marks})"
    ))?;
    let rows = stmt.query_map(params_from_iter(ids.iter()), |row| {
        Ok(Member {
            id: row.get(0)?,
            name: row.get(1)?,
            sort_name: row.get(2)?,
            row_revision: row.get(3)?,
            provider_mbid: row.get(4)?,
            counts: ReferenceCounts::default(),
        })
    })?;
    for member in rows {
        let member = member?;
        artists.insert(member.id.clone(), member);
    }
    let count_queries = [
        "SELECT local_artist_id, COUNT(*) FROM local_album_artists WHERE local_artist_id IN ({}) GROUP BY 1",
        "SELECT local_artist_id, COUNT(*) FROM local_track_artists WHERE local_artist_id IN ({}) GROUP BY 1",
        "SELECT album_artist_id, COUNT(*) FROM local_albums WHERE album_artist_id IN ({}) GROUP BY 1",
        "SELECT target_id, COUNT(*) FROM library_migration_provenance \
           WHERE target_kind = 'local_artist' AND target_id IN ({}) GROUP BY 1",
        "SELECT item_id, COUNT(*) FROM library_user_favorites \
           WHERE item_kind = 'artist' AND item_id IN ({}) GROUP BY 1",
        "SELECT local_artist_id, COUNT(*) FROM library_playlist_tracks WHERE local_artist_id IN ({}) GROUP BY 1",
        "SELECT local_artist_id, COUNT(*) FROM library_play_history WHERE local_artist_id IN ({}) GROUP BY 1",
        "SELECT internal_id, COUNT(*) FROM library_compat_id_map \
           WHERE kind = 'artist' AND internal_id IN ({}) GROUP BY 1",
    ];
    for query in count_queries {
        let sql = query.replace("{}", &marks);
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(params_from_iter(ids.iter()), |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        })?;
        for row in rows {
            let (id, count) = row?;
            *totals.entry(id).or_insert(0) += count;
        }
    }
    Ok((artists, totals))
}

/// The newest reconciliation job and how many records were merged
/// automatically so far.
pub fn progress(conn: &Connection) -> Result<(Option<ReconcileJob>, i64), ReconcileError> {
    let job = conn
        .query_row(
            "SELECT job.id, job.state, job.completed_count, job.expected_work_count \
             FROM library_operation_jobs job \
             JOIN library_repair_snapshots snapshot ON snapshot.job_id = job.id \
             WHERE json_extract(snapshot.scope_json, '$.purpose') = 'artist_identity_reconciliation' \
             ORDER BY job.created_at DESC, job.id DESC LIMIT 1",
            [],
            |row| {
                Ok(ReconcileJob {
                    id: row.get(0)?,
                    state: row.get(1)?,
                    completed_count: row.get(2)?,
                    expected_count: row.get(3)?,
                })
            },
        )
        .optional()?;
    let merged = conn.query_row(
        &format!(
            "SELECT COALESCE(SUM(json_array_length(json_extract(after_json, \
             '$.retired_artist_ids'))), 0) FROM library_catalog_actions \
             WHERE reason_code IN {MERGE_REASONS} AND json_valid(after_json)"
        ),
        [],
        |row| row.get(0),
    )?;
    Ok((job, merged))
}

/// Proofs, albums, and tracks the given artists are credited on. Capped
/// like v2: 500 proofs, 250 albums, 500 tracks.
pub fn references(conn: &Connection, ids: &[String]) -> Result<GroupReferences, ReconcileError> {
    let unique: BTreeSet<&str> = ids.iter().map(String::as_str).collect();
    if unique.is_empty() {
        return Ok(GroupReferences::default());
    }
    let marks = placeholders(unique.len());
    let twice = || unique.iter().chain(unique.iter());
    let evidence = conn
        .prepare(&format!(
            "SELECT proof.subject_kind, proof.subject_id, \
               COALESCE(subject_album.title, subject_track.title, proof.subject_id), \
               proof.source_local_artist_id, proof.local_artist_id, proof.artist_mbid, \
               proof.canonical_name, proof.credited_name, proof.join_phrase, proof.release_mbid, \
               proof.release_track_mbid, proof.album_identity_revision, \
               proof.track_identity_revision, proof.evidence_hash \
             FROM library_artist_credit_proofs proof {PROOF_JOINS} \
             LEFT JOIN local_albums subject_album ON proof.subject_kind = 'album' \
              AND subject_album.id = proof.subject_id \
             LEFT JOIN local_tracks subject_track ON proof.subject_kind = 'track' \
              AND subject_track.id = proof.subject_id \
             WHERE (proof.source_local_artist_id IN ({marks}) OR proof.local_artist_id IN ({marks})) \
               AND {ACTIVE_ALBUM} AND ({CURRENT_PROOF}) \
             ORDER BY proof.local_album_id, proof.subject_kind, proof.subject_id, \
               proof.credit_position LIMIT 500"
        ))?
        .query_map(params_from_iter(twice()), |row| {
            Ok(CreditEvidence {
                subject_kind: row.get(0)?,
                subject_id: row.get(1)?,
                subject_name: row.get(2)?,
                source_local_artist_id: row.get(3)?,
                local_artist_id: row.get(4)?,
                artist_mbid: row.get(5)?,
                canonical_name: row.get(6)?,
                credited_name: row.get(7)?,
                join_phrase: row.get(8)?,
                release_mbid: row.get(9)?,
                release_track_mbid: row.get(10)?,
                album_identity_revision: row.get(11)?,
                track_identity_revision: row.get(12)?,
                evidence_hash: row.get(13)?,
            })
        })?
        .collect::<rusqlite::Result<_>>()?;
    let owned = |row: &rusqlite::Row<'_>| {
        Ok(OwnedReference {
            id: row.get(0)?,
            name: row.get(1)?,
            row_revision: row.get(2)?,
            identity_ready: row.get(3)?,
            exact_track_mapping_ready: row.get(4)?,
        })
    };
    let releases = conn
        .prepare(&format!(
            "SELECT DISTINCT album.id, album.title, album.row_revision, \
               album_identity.release_mbid IS NOT NULL, \
               NOT EXISTS (SELECT 1 FROM local_tracks child \
                 LEFT JOIN local_track_external_identities track_identity \
                   ON track_identity.local_track_id = child.id \
                  AND track_identity.provider = 'musicbrainz' \
                 WHERE child.local_album_id = album.id AND child.availability = 'indexed' \
                   AND (track_identity.release_track_mbid IS NULL \
                    OR track_identity.release_mbid IS NOT album_identity.release_mbid)), \
               album.title_folded \
             FROM local_albums album \
             LEFT JOIN local_album_external_identities album_identity \
               ON album_identity.local_album_id = album.id AND album_identity.provider = 'musicbrainz' \
             WHERE {ACTIVE_ALBUM} AND (album.album_artist_id IN ({marks}) OR EXISTS ( \
               SELECT 1 FROM local_album_artists credit WHERE credit.local_album_id = album.id \
                 AND credit.local_artist_id IN ({marks}))) \
             ORDER BY album.title_folded, album.id LIMIT 250"
        ))?
        .query_map(params_from_iter(twice()), owned)?
        .collect::<rusqlite::Result<_>>()?;
    let tracks = conn
        .prepare(&format!(
            "SELECT DISTINCT track.id, track.title, track.row_revision, \
               album_identity.release_mbid IS NOT NULL, \
               track_identity.release_track_mbid IS NOT NULL \
                 AND track_identity.release_mbid IS album_identity.release_mbid, \
               track.title_folded \
             FROM local_tracks track \
             JOIN local_albums album ON album.id = track.local_album_id \
             LEFT JOIN local_album_external_identities album_identity \
               ON album_identity.local_album_id = album.id AND album_identity.provider = 'musicbrainz' \
             LEFT JOIN local_track_external_identities track_identity \
               ON track_identity.local_track_id = track.id AND track_identity.provider = 'musicbrainz' \
             WHERE album.retired_into_album_id IS NULL AND track.availability = 'indexed' \
               AND EXISTS (SELECT 1 FROM local_track_artists credit \
                 WHERE credit.local_track_id = track.id AND credit.local_artist_id IN ({marks})) \
             ORDER BY track.title_folded, track.id LIMIT 500"
        ))?
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
                "SELECT row_revision FROM local_artists WHERE id = ?1 \
                 AND retired_into_artist_id IS NULL",
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
    let mut insert = tx.prepare(
        "INSERT INTO library_artist_reconciliation_dismissals \
         (left_artist_id, right_artist_id, left_artist_revision, right_artist_revision, \
          dismissed_by_user_id, created_at, updated_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6) \
         ON CONFLICT(left_artist_id, right_artist_id) DO UPDATE SET \
           left_artist_revision = excluded.left_artist_revision, \
           right_artist_revision = excluded.right_artist_revision, \
           dismissed_by_user_id = excluded.dismissed_by_user_id, \
           updated_at = excluded.updated_at, \
           row_revision = library_artist_reconciliation_dismissals.row_revision + 1",
    )?;
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
                now
            ])?;
            pairs += 1;
        }
    }
    let marks = placeholders(unique.len());
    let mut values: Vec<rusqlite::types::Value> = vec![now.into()];
    for _ in 0..2 {
        values.extend(unique.iter().map(|id| (*id).to_owned().into()));
    }
    tx.execute(
        &format!(
            "UPDATE local_artist_merge_candidates SET state = 'dismissed', updated_at = ?, \
             row_revision = row_revision + 1 WHERE state = 'open' \
             AND left_artist_id IN ({marks}) AND right_artist_id IN ({marks})"
        ),
        params_from_iter(values),
    )?;
    Ok(pairs)
}
