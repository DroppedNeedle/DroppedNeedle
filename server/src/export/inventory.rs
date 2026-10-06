//! What stays behind in v2: every table the export does not read that
//! still holds rows, with its row count and a plain reason.
//!
//! The list rides in the export file, `validate` prints it and the import
//! report repeats it, so nothing is lost silently: the operator sees what
//! v3 will not have before anything is imported.

use std::collections::HashSet;

use rusqlite::Connection;

use crate::export::envelope::LeftBehind;
use crate::export::error::ExportError;

/// Older copies v2 kept from before its own library upgrade. v2 itself
/// reads the `library_*` tables the export carries instead.
const PRE_UPGRADE_COPIES: &[&str] = &[
    "album_release_pins",
    "cache_meta",
    "compat_bookmarks",
    "compat_id_map",
    "compat_play_queue_items",
    "compat_play_queues",
    "library_album_meta",
    "library_albums",
    "library_artists",
    "library_files",
    "manual_review_queue",
    "play_history",
    "playlist_tracks",
    "playlists",
    "user_favorites",
];

/// Tables v3 rebuilds from carried rows on its own, so nothing is lost:
/// the landed-release index follows the carried downloads by trigger.
const REBUILT_FROM_CARRIED: &[&str] = &["download_landed_groups"];

/// Data v3 has no place to keep yet, with what the user does instead.
const NO_V3_STORE: &[(&str, &str)] = &[(
    "ignored_releases",
    "releases you hid from the new-release list; v3 cannot keep them yet. Hide them \
         again in v3 when they show up",
)];

/// Sign-in state: everyone signs in again on v3.
const SIGN_IN_STATE: &[&str] = &["auth_oidc_states", "auth_tokens", "spotify_oauth_states"];

/// Download and import job state; v3 starts with an empty queue.
const JOB_STATE: &[&str] = &[
    "acquisition_snapshot_backfill",
    "download_attempts",
    "download_attempts_new",
    "download_cleanup_reconciliation",
    "drop_import_items",
    "drop_import_jobs",
    "free_music_tasks",
    "search_jobs",
];

/// Lookups v3 fills again from the providers as it needs them.
const CACHES: &[&str] = &[
    "artist_event_check",
    "artist_genre_lookup",
    "artist_genres",
    "artist_release_check",
    "artist_skiddle_ids",
    "artist_skiddle_resolution",
    "artist_tm_attraction",
    "audio_fingerprint_outcomes",
    "canonical_redirect",
    "follow_due",
    "follow_inventory",
    "follow_inventory_pages",
    "follow_inventory_rows",
    "live_event_feed",
    "mbid_resolution_map",
    "processed_items",
    "recording_isrc",
    "release_to_rg",
    "sync_state",
];

/// Undo records of single v2 Library Management operations.
const OPERATION_UNDO: &[&str] = &[
    "library_automatic_edition_undo",
    "library_file_mutation_journal",
    "library_management_blob_references",
    "library_management_import_bundles",
    "library_management_import_journal",
    "library_management_operation_snapshots",
];

/// Identification history and queue, and evidence v3 gathers again.
const IDENTIFY_HISTORY: &[&str] = &[
    "library_artist_credit_proofs",
    "library_artist_reconciliation_state",
    "library_enqueue_sequence",
    "library_identification_attempts",
    "library_identification_evidence",
    "library_identification_jobs",
    "library_identity_repair_findings",
    "library_reidentification_snapshots",
];

/// Why one v2 table stays behind, and what to do about it.
#[must_use]
pub fn reason_for(table: &str) -> &'static str {
    if let Some((_, reason)) = NO_V3_STORE.iter().find(|(name, _)| *name == table) {
        reason
    } else if PRE_UPGRADE_COPIES.contains(&table) || table.contains("__") {
        "older copy from before v2's own library upgrade; v2 no longer reads it. Nothing \
         to do"
    } else if SIGN_IN_STATE.contains(&table) {
        "sign-in sessions. Nothing to do: everyone signs in again on v3"
    } else if JOB_STATE.contains(&table) || table.starts_with("download_activity_") {
        "download and import job state; v3 starts with an empty queue. Let downloads \
         finish in v2 before you export, or request what is missing again in v3"
    } else if CACHES.contains(&table)
        || table.starts_with("mb_")
        || table.starts_with("discovery_")
        || table.ends_with("_mbid_index")
    {
        "cache. Nothing to do: v3 fills it again"
    } else if OPERATION_UNDO.contains(&table) {
        "undo records of single v2 Library Management changes. Restoring a file's \
         original still works in v3; to undo one particular v2 change, do it in v2 \
         before upgrading"
    } else if IDENTIFY_HISTORY.contains(&table) {
        "identification history. Nothing to do: matches and decisions move, and v3 \
         identifies albums again when their files change"
    } else if table == "local_album_artwork" || table == "library_genre_artwork_revisions" {
        "album art. v3 reads it from your files again; a cover you picked by hand in \
         v2 has to be picked again on the album page"
    } else if table.starts_with("library_scan_")
        || table.starts_with("library_policy_")
        || table.starts_with("library_migration_")
    {
        "scan and housekeeping state. Nothing to do: v3 builds its own on its first \
         scan"
    } else if table.starts_with("local_") || table.starts_with("library_") {
        "Library Management job, preview and housekeeping history. Nothing to do: \
         let v2's management jobs finish before you export, and v3 keeps its own from \
         now on"
    } else {
        "a table this upgrade does not know, so it is not carried. Keep your v2 backup \
         if it holds something you need"
    }
}

/// Count the rows of every v2 table outside `carried`, keeping the ones
/// that hold any. Sorted by table name.
pub fn left_behind_tables(
    db: &Connection,
    carried: &HashSet<&str>,
) -> Result<Vec<LeftBehind>, ExportError> {
    let read_error = |error: rusqlite::Error| ExportError::V2Database {
        table: "sqlite_master".to_owned(),
        detail: error.to_string(),
    };
    let mut stmt = db
        .prepare(
            "SELECT name FROM sqlite_master WHERE type = 'table' \
             AND name NOT LIKE 'sqlite_%' ORDER BY name",
        )
        .map_err(read_error)?;
    let tables = stmt
        .query_map([], |row| row.get::<_, String>(0))
        .map_err(read_error)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(read_error)?;
    let mut out = Vec::new();
    for table in tables {
        if carried.contains(table.as_str()) || REBUILT_FROM_CARRIED.contains(&table.as_str()) {
            continue;
        }
        let rows: i64 = db
            .query_row(&format!("SELECT COUNT(*) FROM \"{table}\""), [], |row| {
                row.get(0)
            })
            .map_err(|error| ExportError::V2Database {
                table: table.clone(),
                detail: error.to_string(),
            })?;
        if rows > 0 {
            out.push(LeftBehind {
                reason: reason_for(&table).to_owned(),
                table,
                rows: u64::try_from(rows).unwrap_or(0),
            });
        }
    }
    Ok(out)
}
