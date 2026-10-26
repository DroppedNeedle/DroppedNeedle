//! Artist ids MusicBrainz has merged into others.
//!
//! When the ids an artist record is proven to be disagree (its files hold
//! one and a matched album proves another), each of them is recorded to be
//! checked. Identification then asks MusicBrainz about each id, and that
//! answer is the only way the app learns of a merge: never a name, a
//! release document or a file. When MusicBrainz says id `a` now names `c`,
//! the stored artist ids `a` (the credits' evidence, the proofs and the
//! convergence marks) become `c`, a person's or v2's link to `a` follows,
//! and the records `c` then proves to be one artist are merged. From then
//! on a scan and a pass read `a` as `c`. The retired id stays here as an
//! alias, and the files and `local_tracks` keep it. No file is read or
//! written.

use std::collections::BTreeSet;

use rusqlite::{Connection, OptionalExtension as _, Transaction, params};

use super::artist_links::{mark_stale, placeholder, settle, touch_artist};
use super::sources::ArtistAnswer;
use crate::identity::sql::{PERSONS, REVISABLE};
use crate::operations::decisions::{ArtistAction, record_artist_action};

/// A kept id is asked about again after this long, while it still
/// disagrees with another id on a record.
const RECHECK_AFTER_MS: i64 = 30 * 24 * 3600 * 1000;

/// Where checking one artist id stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckState {
    /// Waiting for MusicBrainz's answer.
    Pending,
    /// The id stays as it is: current, unknown, refused, or a merge not
    /// followed.
    Kept,
    /// MusicBrainz merged the id into another.
    Merged,
}

impl CheckState {
    pub const ALL: [Self; 3] = [Self::Pending, Self::Kept, Self::Merged];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Kept => "kept",
            Self::Merged => "merged",
        }
    }
}

/// The id `mbid` was merged into, when one is recorded.
pub(crate) fn merged_into(conn: &Connection, mbid: &str) -> rusqlite::Result<Option<String>> {
    conn.prepare_cached(
        "SELECT canonical_mbid FROM library_identify_artist_id_checks \
         WHERE artist_mbid = ?1 AND state = ?2",
    )?
    .query_row(params![mbid, CheckState::Merged.as_str()], |row| row.get(0))
    .optional()
}

/// `mbid` lowercased, or the id it was merged into.
pub(crate) fn current_id(conn: &Connection, mbid: &str) -> rusqlite::Result<String> {
    let mbid = mbid.trim().to_ascii_lowercase();
    Ok(merged_into(conn, &mbid)?.unwrap_or(mbid))
}

/// Record `ids` to be asked about: a new id, or a kept one last checked
/// over 30 days ago. Placeholders and malformed ids are skipped; an id
/// already pending or merged is left as it is.
pub(crate) fn nominate(
    conn: &Connection,
    ids: &BTreeSet<String>,
    now_ms: i64,
) -> rusqlite::Result<()> {
    for id in ids {
        if placeholder(id) || id.len() != 36 || uuid::Uuid::parse_str(id).is_err() {
            continue;
        }
        conn.prepare_cached(
            "INSERT INTO library_identify_artist_id_checks (artist_mbid, state, nominated_at) \
             VALUES (?1, ?4, ?2) ON CONFLICT (artist_mbid) DO UPDATE SET \
             state = excluded.state, nominated_at = excluded.nominated_at, checked_at = NULL \
             WHERE library_identify_artist_id_checks.state = ?5 \
             AND library_identify_artist_id_checks.checked_at < ?3",
        )?
        .execute(params![
            id,
            now_ms,
            now_ms - RECHECK_AFTER_MS,
            CheckState::Pending.as_str(),
            CheckState::Kept.as_str()
        ])?;
    }
    Ok(())
}

/// Up to `limit` ids waiting for an answer, newest first.
pub fn pending(conn: &Connection, limit: usize) -> rusqlite::Result<Vec<String>> {
    conn.prepare_cached(
        "SELECT artist_mbid FROM library_identify_artist_id_checks \
         WHERE state = ?2 ORDER BY nominated_at DESC LIMIT ?1",
    )?
    .query_map(params![limit as i64, CheckState::Pending.as_str()], |row| {
        row.get(0)
    })?
    .collect()
}

fn keep(tx: &Transaction<'_>, mbid: &str, now_ms: i64) -> rusqlite::Result<()> {
    tx.prepare_cached(
        "UPDATE library_identify_artist_id_checks SET state = ?3, canonical_mbid = NULL, \
         checked_at = ?2 WHERE artist_mbid = ?1",
    )?
    .execute(params![mbid, now_ms, CheckState::Kept.as_str()])?;
    Ok(())
}

/// Write MusicBrainz's answer about `mbid`. Nothing is written unless the
/// id is pending. Returns the ids whose records should be judged for a
/// merge: the id a followed merge led to and any id newly linked.
pub(crate) fn answer(
    tx: &Transaction<'_>,
    mbid: &str,
    answer: &ArtistAnswer,
    now_ms: i64,
) -> rusqlite::Result<Vec<String>> {
    let retired = mbid.trim().to_ascii_lowercase();
    let pending: bool = tx
        .prepare_cached(
            "SELECT EXISTS (SELECT 1 FROM library_identify_artist_id_checks \
             WHERE artist_mbid = ?1 AND state = ?2)",
        )?
        .query_row(params![retired, CheckState::Pending.as_str()], |row| {
            row.get(0)
        })?;
    if !pending {
        return Ok(Vec::new());
    }
    match answer {
        ArtistAnswer::Current | ArtistAnswer::Missing => {
            keep(tx, &retired, now_ms)?;
            Ok(Vec::new())
        }
        ArtistAnswer::Refused(reason) => {
            tracing::warn!(
                artist_mbid = retired,
                reason,
                "MusicBrainz refused an artist id; it is kept as it is"
            );
            keep(tx, &retired, now_ms)?;
            Ok(Vec::new())
        }
        ArtistAnswer::MergedInto(current) => follow(tx, &retired, current, now_ms),
    }
}

/// Why the merge of `retired` into `current` is not followed, if it is not.
fn not_followed(
    tx: &Transaction<'_>,
    retired: &str,
    current: &str,
) -> rusqlite::Result<Option<&'static str>> {
    if placeholder(current) {
        return Ok(Some("merged into a placeholder artist"));
    }
    if current == retired {
        return Ok(Some("the answers point at each other"));
    }
    let known: bool = tx
        .prepare_cached(
            "SELECT EXISTS (SELECT 1 FROM library_identify_artist_id_checks \
             WHERE artist_mbid = ?1) OR EXISTS (SELECT 1 FROM library_identify_artist_id_checks \
             WHERE canonical_mbid = ?1)",
        )?
        .query_row(params![current], |row| row.get(0))?;
    if !known {
        return Ok(Some("no artist record held the id beside another"));
    }
    let linked: Vec<String> = tx
        .prepare_cached(&format!(
            "SELECT local_artist_id FROM local_artist_external_identities \
             WHERE provider = 'musicbrainz' AND decision_source IN {PERSONS} \
             AND lower(provider_artist_id) = ?1"
        ))?
        .query_map(params![retired], |row| row.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    let stays = match linked.as_slice() {
        [] => false,
        [artist] => person_link_elsewhere(tx, artist, current)?,
        _ => true,
    };
    Ok(stays.then_some("a person's link holds the kept id on another artist"))
}

/// Whether a row other than `artist` holds `mbid` by a person's or v2's
/// link.
fn person_link_elsewhere(conn: &Connection, artist: &str, mbid: &str) -> rusqlite::Result<bool> {
    conn.prepare_cached(&format!(
        "SELECT EXISTS (SELECT 1 FROM local_artist_external_identities \
         WHERE provider = 'musicbrainz' AND decision_source IN {PERSONS} \
         AND lower(provider_artist_id) = ?1 AND local_artist_id <> ?2)"
    ))?
    .query_row(params![mbid, artist], |row| row.get(0))
}

/// Record row `artist` and the row a person's or v2's link holds `mbid`
/// on as an open merge candidate, the pair a contribution records when
/// two rows are linked to one id.
fn suggest_merge(tx: &Transaction<'_>, artist: &str, mbid: &str, now: f64) -> rusqlite::Result<()> {
    let other: Option<String> = tx
        .prepare_cached(&format!(
            "SELECT local_artist_id FROM local_artist_external_identities \
             WHERE provider = 'musicbrainz' AND decision_source IN {PERSONS} \
             AND lower(provider_artist_id) = ?1 AND local_artist_id <> ?2 \
             ORDER BY local_artist_id LIMIT 1"
        ))?
        .query_row(params![mbid, artist], |row| row.get(0))
        .optional()?;
    let Some(other) = other else {
        return Ok(());
    };
    let (left, right) = if artist < other.as_str() {
        (artist, other.as_str())
    } else {
        (other.as_str(), artist)
    };
    tx.prepare_cached(
        "INSERT OR IGNORE INTO local_artist_merge_candidates (id, left_artist_id, \
         right_artist_id, reason_code, created_at, updated_at) \
         VALUES (?1, ?2, ?3, 'SHARED_PROVIDER_IDENTITY', ?4, ?4)",
    )?
    .execute(params![
        format!("provider:{left}:{right}"),
        left,
        right,
        now
    ])?;
    Ok(())
}

fn follow(
    tx: &Transaction<'_>,
    retired: &str,
    current: &str,
    now_ms: i64,
) -> rusqlite::Result<Vec<String>> {
    let mut current = current.trim().to_ascii_lowercase();
    // MusicBrainz answers with the final id, so one step is enough; a
    // second one means stale data on file.
    if let Some(next) = merged_into(tx, &current)? {
        if let Some(further) = merged_into(tx, &next)? {
            tracing::info!(
                artist_mbid = current,
                merged_into = next,
                then_into = further,
                "a recorded artist merge leads to another; following one step"
            );
        }
        current = next;
    }
    if let Some(reason) = not_followed(tx, retired, &current)? {
        tracing::info!(
            artist_mbid = retired,
            merged_into = current,
            reason,
            "MusicBrainz merged an artist id; not followed"
        );
        keep(tx, retired, now_ms)?;
        return Ok(Vec::new());
    }
    tx.prepare_cached(
        "UPDATE library_identify_artist_id_checks SET state = ?4, canonical_mbid = ?2, \
         checked_at = ?3 WHERE artist_mbid = ?1",
    )?
    .execute(params![
        retired,
        current,
        now_ms,
        CheckState::Merged.as_str()
    ])?;
    tx.prepare_cached(
        "UPDATE library_identify_artist_id_checks SET canonical_mbid = ?2 \
         WHERE canonical_mbid = ?1",
    )?
    .execute(params![retired, current])?;
    let albums: Vec<String> = tx
        .prepare_cached(
            "SELECT DISTINCT local_album_id FROM library_artist_credit_proofs \
             WHERE artist_mbid = ?1",
        )?
        .query_map(params![retired], |row| row.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    let mut credits = 0;
    for table in ["local_track_artists", "local_album_artists"] {
        credits += tx
            .prepare_cached(&format!(
                "UPDATE {table} SET artist_mbid = ?2 WHERE artist_mbid = ?1"
            ))?
            .execute(params![retired, current])?;
    }
    let proofs = tx
        .prepare_cached(
            "UPDATE library_artist_credit_proofs SET artist_mbid = ?2 WHERE artist_mbid = ?1",
        )?
        .execute(params![retired, current])?;
    tx.prepare_cached("UPDATE local_artists SET converged_mbid = ?2 WHERE converged_mbid = ?1")?
        .execute(params![retired, current])?;
    for album in &albums {
        mark_stale(tx, album)?;
    }
    let rows: Vec<String> = tx
        .prepare_cached(
            "SELECT local_artist_id FROM local_track_artists WHERE artist_mbid = ?2 \
             UNION SELECT local_artist_id FROM local_album_artists WHERE artist_mbid = ?2 \
             UNION SELECT source_local_artist_id FROM library_artist_credit_proofs \
              WHERE artist_mbid = ?2 AND source_local_artist_id IS NOT NULL \
             UNION SELECT local_artist_id FROM local_artist_external_identities \
              WHERE provider = 'musicbrainz' AND lower(provider_artist_id) = ?1",
        )?
        .query_map(params![retired, current], |row| row.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    let mut ids = BTreeSet::from([current.clone()]);
    ids.extend(settle(tx, &rows, now_ms)?);
    tracing::info!(
        artist_mbid = retired,
        merged_into = current,
        credits,
        proofs,
        "following MusicBrainz's merge of an artist id"
    );
    Ok(ids.into_iter().collect())
}

/// Move each person's or v2's link among `rows` that names an id recorded
/// as merged to the id it was merged into, unless another row holds that
/// id by a person's or v2's link: then the pair is recorded as a merge
/// candidate for a person to decide. The app's own links to that id give
/// way. Returns whether any link moved.
pub(crate) fn follow_person_links(
    tx: &Transaction<'_>,
    rows: &[String],
    now_ms: i64,
) -> rusqlite::Result<bool> {
    let now = now_ms as f64 / 1000.0;
    let mut moved = false;
    for artist in rows {
        let link: Option<String> = tx
            .prepare_cached(&format!(
                "SELECT lower(provider_artist_id) FROM local_artist_external_identities \
                 WHERE local_artist_id = ?1 AND provider = 'musicbrainz' \
                 AND decision_source IN {PERSONS}"
            ))?
            .query_row(params![artist], |row| row.get(0))
            .optional()?;
        let Some(retired) = link else { continue };
        let Some(current) = merged_into(tx, &retired)? else {
            continue;
        };
        if person_link_elsewhere(tx, artist, &current)? {
            tracing::debug!(
                artist_id = artist,
                artist_mbid = retired,
                merged_into = current,
                "another artist holds the kept id by a person's link; link not moved"
            );
            suggest_merge(tx, artist, &current, now)?;
            continue;
        }
        let giving_way: Vec<String> = tx
            .prepare_cached(&format!(
                "SELECT local_artist_id FROM local_artist_external_identities \
                 WHERE provider = 'musicbrainz' AND decision_source IN {REVISABLE} \
                 AND lower(provider_artist_id) = ?1 AND local_artist_id <> ?2"
            ))?
            .query_map(params![current, artist], |row| row.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        for other in &giving_way {
            tx.prepare_cached(
                "DELETE FROM local_artist_external_identities \
                 WHERE local_artist_id = ?1 AND provider = 'musicbrainz'",
            )?
            .execute(params![other])?;
            touch_artist(tx, other, now)?;
        }
        tx.prepare_cached(
            "UPDATE local_artist_external_identities SET provider_artist_id = ?2, \
             row_revision = row_revision + 1 WHERE local_artist_id = ?1 AND provider = 'musicbrainz'",
        )?
        .execute(params![artist, current])?;
        touch_artist(tx, artist, now)?;
        record_artist_action(
            tx,
            ArtistAction {
                actor: "system",
                kind: "follow_merged_artist_id",
                artist_id: artist,
                before: serde_json::json!({ "provider_artist_id": retired }),
                after: serde_json::json!({ "provider_artist_id": current }),
                reason: "MUSICBRAINZ_MERGED_ARTIST_ID",
            },
            now,
        )?;
        tracing::info!(
            artist_id = artist,
            artist_mbid = retired,
            merged_into = current,
            "a person's artist link follows MusicBrainz's merge"
        );
        moved = true;
    }
    Ok(moved)
}
