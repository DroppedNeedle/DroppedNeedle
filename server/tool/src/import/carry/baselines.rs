//! The baseline step: every carried v2 original-file baseline becomes the
//! v3 baseline "restore original" reads.
//!
//! It runs after the catalog, v2's baseline rows and the blob bytes have
//! landed. Each baseline's tag snapshot is translated
//! ([`droppedneedle_library::publish::v2_baseline`]) and stored the way v3's
//! publisher stores its own: the before-state blob, the baseline row, and
//! a blob reference. v2's own snapshot and the sidecar and artwork blobs
//! it lists stay referenced too, so nothing a baseline held is ever swept.
//!
//! A track that already has a v3 baseline keeps it (baselines never
//! change). A baseline that cannot be translated is listed with its reason
//! and gets no v3 baseline; v3 then refuses to manage that track, because
//! its first managed write would otherwise record the file as v2 left it
//! as the "original".

use rusqlite::{Connection, OptionalExtension as _, params};

use super::sql;
use super::{CarryError, SectionResult};
use crate::export::sections::management::{BASELINES, BLOB_BYTES};
use droppedneedle_library::publish::snapshots::sha256_hex;
use droppedneedle_library::publish::v2_baseline::translate;

/// Report entity and section marker name.
pub(crate) const ENTITY: &str = "original_baseline";

fn bundle_has(conn: &Connection, table: &str) -> rusqlite::Result<bool> {
    conn.query_row(sql::BUNDLE_HAS_TABLE.text(), params![table], |row| {
        row.get(0)
    })
}

/// Blob hashes an ancillary snapshot lists.
fn ancillary_blobs(json: Option<&str>) -> Vec<String> {
    let Some(entries) = json
        .and_then(|text| serde_json::from_str::<serde_json::Value>(text).ok())
        .and_then(|value| value.as_array().cloned())
    else {
        return Vec::new();
    };
    entries
        .iter()
        .filter_map(|entry| entry.get("blob_sha256").and_then(|sha| sha.as_str()))
        .map(str::to_owned)
        .collect()
}

/// Translate and store every carried baseline. On a dry run, count only.
pub(crate) fn apply(conn: &Connection, dry_run: bool) -> Result<SectionResult, CarryError> {
    let mut result = SectionResult::default();
    if !bundle_has(conn, BASELINES.name)? || !bundle_has(conn, BLOB_BYTES.name)? {
        return Ok(result);
    }
    let candidates = sql::all_rows(conn, sql::BASELINE_CANDIDATES, sql::BaselineCandidate::read)?;
    result.rows = candidates.len() as u64;
    let track_landed = if dry_run {
        sql::BASELINE_TRACK_WOULD_LAND
    } else {
        sql::BASELINE_TRACK_LANDED
    };
    for candidate in candidates {
        let track = candidate.local_track_id.clone();
        let landed: bool = conn.query_row(track_landed.text(), params![track], |row| row.get(0))?;
        if !landed {
            result.counts.dropped_invalid += 1;
            result.note(
                track,
                "dropped_invalid",
                "its song did not come across (see the library_track counts), so there is \
                 nothing to restore; if you need that song's original, restore it in v2 \
                 before upgrading",
            );
            continue;
        }
        let snapshot: Option<Vec<u8>> = conn
            .query_row(
                sql::BUNDLE_BLOB.text(),
                params![candidate.semantic_snapshot_blob_sha256],
                |row| row.get(0),
            )
            .optional()?;
        let Some(snapshot) = snapshot else {
            result.counts.dropped_invalid += 1;
            result.note(
                track,
                "dropped_invalid",
                "its saved original tags were missing or damaged in v2's blob folder, \
                 so v3 will not change this song. Restore V2_ROOT/cache/library-management \
                 from your backup, export again and import into an empty V3_ROOT",
            );
            continue;
        };
        let before = match translate(
            &snapshot,
            &candidate.format,
            &candidate.original_root_id,
            &candidate.original_relative_path,
        ) {
            Ok(before) => before,
            Err(error) => {
                result.counts.dropped_invalid += 1;
                result.note(
                    track,
                    "dropped_invalid",
                    format!(
                        "{error}, so v3 will not change this song. If you need its \
                         original, restore it in v2 before upgrading"
                    ),
                );
                continue;
            }
        };
        let blob =
            serde_json::to_vec(&before).map_err(|error| CarryError::Io(error.to_string()))?;
        let blob_sha = sha256_hex(&blob);
        let existing: Option<String> = conn
            .query_row(sql::BASELINE_BLOB.text(), params![track], |row| row.get(0))
            .optional()?;
        match existing {
            Some(kept) if kept == blob_sha => {
                result.counts.skipped_identical += 1;
                continue;
            }
            Some(_) => {
                result.counts.conflict_kept_existing += 1;
                result.note(
                    track,
                    "conflict_kept_existing",
                    "v3 already recorded an original for this song and keeps it; it may be \
                     the state v2 had already changed. To use v2's original instead, import \
                     into an empty V3_ROOT",
                );
                continue;
            }
            None => {}
        }
        result.counts.imported += 1;
        if dry_run {
            continue;
        }
        conn.execute(sql::INSERT_BLOB.text(), params![blob_sha, blob])?;
        conn.execute(
            sql::INSERT_BASELINE.text(),
            params![
                track,
                blob_sha,
                candidate.original_root_id,
                candidate.original_relative_path,
                candidate.created_at,
            ],
        )?;
        let mut owned = vec![(blob_sha, "baseline")];
        owned.push((
            candidate.semantic_snapshot_blob_sha256.clone(),
            "v2_baseline",
        ));
        for sha in ancillary_blobs(candidate.ancillary_snapshot_json.as_deref()) {
            owned.push((sha, "v2_baseline"));
        }
        for (sha, kind) in owned {
            conn.execute(sql::INSERT_BLOB_REF.text(), params![sha, kind, track])?;
        }
        result.written += 1;
    }
    Ok(result)
}
