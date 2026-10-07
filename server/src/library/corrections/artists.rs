//! Merge duplicate local artists into one survivor.
//!
//! Every reference to a merged artist (album and track credits, album
//! artists, favorites, play history, playlist snapshots, compatibility
//! ids, credit proofs, migration provenance, source links) moves to the
//! survivor; the merged rows stay as retired records with an alias, so
//! old links keep resolving. As in v2, this is not undoable.

use std::collections::{BTreeMap, BTreeSet};

use rusqlite::types::Value;
use rusqlite::{OptionalExtension as _, Transaction, params, params_from_iter};
use serde::Serialize;

use super::edition::placeholders;
use super::models::{ArtistMergeOutcome, ArtistMergeRequest, CorrectionError, ProviderChoice};
use super::reasons;

const VARIOUS_ARTISTS_ID: &str = "00000000-0000-4000-8000-000000000001";
const UNKNOWN_ARTIST_ID: &str = "00000000-0000-4000-8000-000000000002";

/// Reference kinds a merge moves, counted for the preview (v2's list).
const REFERENCE_COUNTS: [(&str, &str); 8] = [
    (
        "album_credits",
        "SELECT local_artist_id, COUNT(*) FROM local_album_artists \
         WHERE local_artist_id IN ({}) GROUP BY 1",
    ),
    (
        "track_credits",
        "SELECT local_artist_id, COUNT(*) FROM local_track_artists \
         WHERE local_artist_id IN ({}) GROUP BY 1",
    ),
    (
        "primary_albums",
        "SELECT album_artist_id, COUNT(*) FROM local_albums \
         WHERE album_artist_id IN ({}) GROUP BY 1",
    ),
    (
        "migration_references",
        "SELECT target_id, COUNT(*) FROM library_migration_provenance \
         WHERE target_kind = 'local_artist' AND target_id IN ({}) GROUP BY 1",
    ),
    (
        "favorites",
        "SELECT item_id, COUNT(*) FROM library_user_favorites \
         WHERE item_kind = 'artist' AND item_id IN ({}) GROUP BY 1",
    ),
    (
        "playlist_snapshots",
        "SELECT local_artist_id, COUNT(*) FROM library_playlist_tracks \
         WHERE local_artist_id IN ({}) GROUP BY 1",
    ),
    (
        "history",
        "SELECT local_artist_id, COUNT(*) FROM library_play_history \
         WHERE local_artist_id IN ({}) GROUP BY 1",
    ),
    (
        "compatibility_ids",
        "SELECT internal_id, COUNT(*) FROM library_compat_id_map \
         WHERE kind = 'artist' AND internal_id IN ({}) GROUP BY 1",
    ),
];

/// A merge as run: what it did and the material its token covers.
pub(super) struct ArtistRun {
    pub outcome: ArtistMergeOutcome,
    pub material: String,
}

#[derive(Serialize)]
struct Material<'a> {
    kind: &'static str,
    surviving_artist_id: &'a str,
    retired: &'a [String],
    revisions: Vec<(String, i64)>,
    identities: Vec<(String, String)>,
}

pub(super) fn merge(
    tx: &Transaction<'_>,
    request: &ArtistMergeRequest,
    now: f64,
) -> Result<ArtistRun, CorrectionError> {
    let survivor = request.surviving_artist_id.trim().to_owned();
    let retired: Vec<String> = request
        .source_artist_ids
        .iter()
        .map(|id| id.trim().to_owned())
        .filter(|id| !id.is_empty() && *id != survivor)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    if retired.is_empty() {
        return Err(CorrectionError::Invalid(reasons::NO_DUPLICATE_ARTIST));
    }
    if retired
        .iter()
        .any(|id| id == VARIOUS_ARTISTS_ID || id == UNKNOWN_ARTIST_ID)
    {
        return Err(CorrectionError::Invalid(reasons::RESERVED_ARTIST));
    }
    let all: Vec<String> = std::iter::once(survivor.clone())
        .chain(retired.iter().cloned())
        .collect();
    let mut revisions = Vec::with_capacity(all.len());
    for id in &all {
        let revision: i64 = tx
            .query_row(
                "SELECT row_revision FROM local_artists WHERE id = ?1 \
                 AND retired_into_artist_id IS NULL",
                params![id],
                |row| row.get(0),
            )
            .optional()?
            .ok_or(CorrectionError::NotFound(reasons::ARTIST_NOT_FOUND))?;
        if let Some(expected) = request.expected_revisions.get(id)
            && *expected > 0
            && *expected != revision
        {
            return Err(CorrectionError::Conflict(reasons::REVISION_STALE));
        }
        revisions.push((id.clone(), revision));
    }
    let identities = identities(tx, &all)?;
    let distinct: BTreeSet<&str> = identities.iter().map(|(_, mbid)| mbid.as_str()).collect();
    let identity_conflicts: Vec<String> = if distinct.len() > 1 {
        distinct.iter().map(|mbid| (*mbid).to_owned()).collect()
    } else {
        Vec::new()
    };
    let reference_counts = reference_counts(tx, &all)?;

    // The survivor's MusicBrainz link: kept, taken from the only merged
    // artist that has one, or dropped.
    let survivor_link = identities.iter().find(|(id, _)| *id == survivor).cloned();
    let carried = match (request.provider_choice, &survivor_link) {
        (ProviderChoice::RetainSurvivor, None) if distinct.len() == 1 => {
            identities.first().cloned()
        }
        _ => None,
    };
    let carried_row = match &carried {
        Some((from, _)) => Some(identity_row(tx, from)?),
        None => None,
    };
    for id in &retired {
        retire_artist(tx, id, &survivor, now)?;
    }
    match request.provider_choice {
        ProviderChoice::Detach => {
            tx.execute(
                "DELETE FROM local_artist_external_identities WHERE local_artist_id = ?1",
                params![survivor],
            )?;
        }
        ProviderChoice::RetainSurvivor => {
            if let Some(row) = carried_row.flatten() {
                tx.execute(
                    "INSERT INTO local_artist_external_identities (local_artist_id, provider, \
                     provider_artist_id, decision_source, selected_at) VALUES (?1, 'musicbrainz', \
                     ?2, ?3, ?4)",
                    params![survivor, row.0, row.1, now],
                )?;
            }
        }
    }
    tx.execute(
        "UPDATE local_artists SET updated_at = ?2, row_revision = row_revision + 1 WHERE id = ?1",
        params![survivor, now],
    )?;
    let material = serde_json::to_string(&Material {
        kind: "artist_merge",
        surviving_artist_id: &survivor,
        retired: &retired,
        revisions,
        identities: identities.clone(),
    })
    .unwrap_or_default();
    Ok(ArtistRun {
        outcome: ArtistMergeOutcome {
            surviving_artist_id: survivor,
            retired_artist_ids: retired,
            identity_conflicts,
            reference_counts,
        },
        material,
    })
}

/// (artist id, MusicBrainz artist id) for the artists that have a link.
fn identities(tx: &Transaction<'_>, ids: &[String]) -> rusqlite::Result<Vec<(String, String)>> {
    let marks = placeholders(ids.len());
    tx.prepare(&format!(
        "SELECT local_artist_id, provider_artist_id FROM local_artist_external_identities \
         WHERE provider = 'musicbrainz' AND local_artist_id IN ({marks}) \
         ORDER BY local_artist_id"
    ))?
    .query_map(params_from_iter(ids.iter()), |row| {
        Ok((row.get(0)?, row.get(1)?))
    })?
    .collect()
}

/// The link row a carried identity copies: (MBID, decision source).
fn identity_row(
    tx: &Transaction<'_>,
    artist_id: &str,
) -> rusqlite::Result<Option<(String, String)>> {
    tx.query_row(
        "SELECT provider_artist_id, decision_source FROM local_artist_external_identities \
         WHERE local_artist_id = ?1 AND provider = 'musicbrainz'",
        params![artist_id],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )
    .optional()
}

fn reference_counts(
    tx: &Transaction<'_>,
    ids: &[String],
) -> rusqlite::Result<BTreeMap<String, i64>> {
    let marks = placeholders(ids.len());
    let mut counts = BTreeMap::new();
    for (kind, query) in REFERENCE_COUNTS {
        let mut total = 0;
        let mut stmt = tx.prepare(&query.replace("{}", &marks))?;
        let rows = stmt.query_map(params_from_iter(ids.iter()), |row| row.get::<_, i64>(1))?;
        for count in rows {
            total += count?;
        }
        counts.insert(kind.to_owned(), total);
    }
    Ok(counts)
}

/// Point every reference of `artist` at `survivor` and retire it.
fn retire_artist(
    tx: &Transaction<'_>,
    artist: &str,
    survivor: &str,
    now: f64,
) -> rusqlite::Result<()> {
    let moves = [
        "UPDATE local_album_artists SET local_artist_id = ?2, row_revision = row_revision + 1 \
         WHERE local_artist_id = ?1",
        "UPDATE local_track_artists SET local_artist_id = ?2, row_revision = row_revision + 1 \
         WHERE local_artist_id = ?1",
        "UPDATE local_albums SET album_artist_id = ?2, updated_at = ?3, \
         row_revision = row_revision + 1 WHERE album_artist_id = ?1",
        "UPDATE library_scan_grouping_groups SET local_artist_id = ?2 WHERE local_artist_id = ?1",
        "DELETE FROM local_entity_source_links WHERE local_artist_id = ?1 AND EXISTS \
         (SELECT 1 FROM local_entity_source_links kept WHERE kept.local_artist_id = ?2 \
          AND kept.provider = local_entity_source_links.provider \
          AND kept.external_entity_type = local_entity_source_links.external_entity_type \
          AND kept.external_id = local_entity_source_links.external_id)",
        "UPDATE local_entity_source_links SET local_artist_id = ?2, updated_at = ?3, \
         row_revision = row_revision + 1 WHERE local_artist_id = ?1",
        "DELETE FROM local_artist_external_identities WHERE local_artist_id = ?1",
        "UPDATE local_artists SET retired_into_artist_id = ?2, updated_at = ?3, \
         row_revision = row_revision + 1 WHERE id = ?1",
        "UPDATE local_artist_aliases SET local_artist_id = ?2 WHERE local_artist_id = ?1",
        "INSERT OR IGNORE INTO local_artist_aliases (alias, local_artist_id, kind, created_at) \
         VALUES (?1, ?2, 'merged_artist', ?3)",
        "UPDATE library_migration_provenance SET target_id = ?2 \
         WHERE target_kind = 'local_artist' AND target_id = ?1",
        "INSERT OR IGNORE INTO library_user_favorites (user_id, item_kind, item_id, created_at) \
         SELECT user_id, item_kind, ?2, created_at FROM library_user_favorites \
         WHERE item_kind = 'artist' AND item_id = ?1",
        "DELETE FROM library_user_favorites WHERE item_kind = 'artist' AND item_id = ?1",
        "UPDATE library_play_history SET local_artist_id = ?2 WHERE local_artist_id = ?1",
        "UPDATE library_playlist_tracks SET local_artist_id = ?2 WHERE local_artist_id = ?1",
        "UPDATE library_compat_id_map SET internal_id = ?2 \
         WHERE kind = 'artist' AND internal_id = ?1",
        "UPDATE library_artist_credit_proofs SET local_artist_id = ?2, updated_at = ?3, \
         row_revision = row_revision + 1 WHERE local_artist_id = ?1",
        "UPDATE OR IGNORE library_identify_credit_proofs SET source_local_artist_id = ?2 \
         WHERE source_local_artist_id = ?1",
        "DELETE FROM library_identify_credit_proofs WHERE source_local_artist_id = ?1",
        "UPDATE local_artist_merge_candidates SET state = 'resolved', updated_at = ?3, \
         row_revision = row_revision + 1 WHERE state = 'open' \
         AND (left_artist_id = ?1 OR right_artist_id = ?1)",
    ];
    for sql in moves {
        let mut stmt = tx.prepare(sql)?;
        let values = [
            Value::Text(artist.to_owned()),
            Value::Text(survivor.to_owned()),
            Value::Real(now),
        ];
        let wanted = stmt.parameter_count();
        stmt.execute(params_from_iter(values.into_iter().take(wanted)))?;
    }
    Ok(())
}
