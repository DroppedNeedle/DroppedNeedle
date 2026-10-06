//! The link step: references to v2 library items that carried rows hold
//! while the catalog was not there yet.
//!
//! User data is carried before the catalog, so its library references are
//! first recorded in `import_pending_links` (or held by an earlier import
//! that had no catalog at all). Once the catalog has landed, every
//! reference whose item exists is settled:
//!
//! - `as_written` rows already hold the id, and `link_only` ones have no
//!   v3 column: their pending entry just goes;
//! - `until_resolved` columns (which have a foreign key) get the id
//!   written now, then the entry goes.
//!
//! References whose v2 item is gone stay pending and are listed. The
//! report notes the numbers rather than counting rows (no row is added),
//! so a dry run and a real run report the same counts. The step runs on
//! every import and is never marked, so a later import that
//! brings the catalog settles links an earlier one left waiting.

use sqlx::SqliteConnection;

use super::{CarryError, SectionResult};
use crate::export::sections::{ALL, LinkMode, Target};

/// Report entity name.
pub(crate) const ENTITY: &str = "library_link";

/// SQL that is true when pending link `p` names an item the catalog has.
const RESOLVED: &str = "CASE p.ref_kind \
     WHEN 'track' THEN EXISTS (SELECT 1 FROM main.local_tracks c WHERE c.id = p.v2_id) \
     WHEN 'album' THEN EXISTS (SELECT 1 FROM main.local_albums c WHERE c.id = p.v2_id) \
     WHEN 'artist' THEN EXISTS (SELECT 1 FROM main.local_artists c WHERE c.id = p.v2_id) \
     WHEN 'tombstone' THEN EXISTS (SELECT 1 FROM main.library_reference_tombstones c \
       WHERE c.id = p.v2_id) \
     ELSE 0 END";

/// Settle every pending link whose item now exists. On a dry run, count.
pub(crate) async fn apply(
    conn: &mut SqliteConnection,
    dry_run: bool,
) -> Result<SectionResult, CarryError> {
    let mut result = SectionResult::default();
    let resolvable: i64 = sqlx::query_scalar(&format!(
        "SELECT COUNT(*) FROM main.import_pending_links p WHERE {RESOLVED}"
    ))
    .fetch_one(&mut *conn)
    .await?;
    if dry_run || resolvable == 0 {
        // A dry run writes no pending links to settle; it only says how
        // many earlier imports left that the catalog now answers.
        if resolvable > 0 {
            result.note(
                String::new(),
                "would_settle",
                format!("{resolvable} reference(s) left by an earlier import"),
            );
        }
        return Ok(result);
    }
    for section in ALL {
        let Target::Table(table) = section.target else {
            continue;
        };
        let key = section
            .key
            .iter()
            .map(|column| format!("t.\"{column}\""))
            .collect::<Vec<_>>()
            .join(", ");
        for column in section.columns {
            if !matches!(column.link, Some(link) if link.mode == LinkMode::UntilResolved) {
                continue;
            }
            let name = column.name;
            let pending = format!(
                "FROM main.import_pending_links p WHERE p.target_table = '{table}' \
                 AND p.ref_column = '{name}' AND p.mode = 'until_resolved' \
                 AND p.target_key = json_array({key}) AND {RESOLVED}"
            );
            sqlx::query(&format!(
                "UPDATE main.\"{table}\" AS t SET \"{name}\" = (SELECT p.v2_id {pending}) \
                 WHERE t.\"{name}\" IS NULL AND EXISTS (SELECT 1 {pending})"
            ))
            .persistent(false)
            .execute(&mut *conn)
            .await?;
        }
    }
    let settled = sqlx::query(&format!(
        "DELETE FROM main.import_pending_links WHERE rowid IN \
         (SELECT p.rowid FROM main.import_pending_links p WHERE {RESOLVED})"
    ))
    .execute(&mut *conn)
    .await?
    .rows_affected();
    result.written = settled;
    result.note(
        String::new(),
        "settled",
        format!("{settled} reference(s) to v2 library items now point at the carried catalog"),
    );
    let waiting: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM main.import_pending_links")
        .fetch_one(&mut *conn)
        .await?;
    if waiting > 0 {
        result.note(
            String::new(),
            "pending",
            format!(
                "{waiting} reference(s) name v2 library items that were already gone in v2; \
                 the rows keep their names. Nothing to do"
            ),
        );
    }
    Ok(result)
}
