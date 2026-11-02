//! The Jellyfin id step: the ids v2 stored keep working, so the downloads
//! and pins of a Jellyfin app stay matched after the upgrade.
//!
//! It runs after the catalog. Each row of the bundle's `jellyfin_id` section
//! lands by these rules; stored rows are never changed:
//!
//! - a row that is not 32 lowercase hex of a known kind, or that names
//!   nothing in v3, is left out and counted;
//! - a row whose id is the unkeyed id of its own item is left to the fill,
//!   which stores it the same way, unless the item has others (below);
//! - a row whose id is the unkeyed id of another live item is left out and
//!   counted: that item would never get a row, and its id would open the
//!   wrong track;
//! - a genre or library id that differs from the formula is left out and
//!   counted: nothing can hand it out;
//! - of a track's or playlist's ids, the smallest is current, as v2 handed
//!   out the first by id, unless the item already has a current id; every
//!   other is an alias;
//! - an album or artist id is always an alias, never current, so nobody
//!   without an account can resolve one.
//!
//! A real run and a dry run count the same.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use rusqlite::{Connection, params};
use serde_json::json;
use sha2::{Digest as _, Sha256};

use super::editions::bundle_rows;
use super::sql;
use super::{CarryError, SectionResult, to_count};
use crate::export::sections::compat::JELLYFIN_IDS;
use crate::export::sections::library::ALBUMS;

/// Report entity and section marker name.
pub(crate) const ENTITY: &str = "jellyfin_id_carry";

/// The fill's unkeyed id, repeated because only the tool's tests depend on
/// compat. `migration_jellyfin_ids` pins it against `JellyfinIds::derive`.
fn derive(kind: &str, internal: &str) -> String {
    let digest = format!(
        "{:x}",
        Sha256::digest(format!("{kind}:{internal}").as_bytes())
    );
    digest[..32].to_owned()
}

/// The rows that can land, and the counts of those already left out.
#[derive(Default)]
struct Sorted {
    /// Item (kind, internal) by carried id.
    carried: HashMap<String, (String, String)>,
    /// Malformed rows and rows naming nothing.
    nothing: u64,
    /// Rows left out because they clash with what the fill hands out.
    clashes: u64,
}

/// Read the bundle's rows into those whose id differs from their own unkeyed
/// id, then drop any that is the unkeyed id of a live item.
fn sort_rows(conn: &Connection, total: u64, dry_run: bool) -> Result<Sorted, CarryError> {
    let mut sorted = Sorted::default();
    let mut well_formed = 0;
    let mut rows = conn.prepare(&sql::jellyfin_id_rows(dry_run))?;
    for row in rows.query_map([], sql::JellyfinIdRow::read)? {
        let row = row?;
        well_formed += 1;
        if !row.named {
            sorted.nothing += 1;
        } else if row.jf_id != derive(&row.kind, &row.internal_id) {
            if matches!(row.kind.as_str(), "genre" | "library") {
                sorted.clashes += 1;
            } else {
                sorted
                    .carried
                    .insert(row.jf_id, (row.kind, row.internal_id));
            }
        }
    }
    sorted.nothing += total.saturating_sub(well_formed);
    if sorted.carried.is_empty() {
        return Ok(sorted);
    }
    let mut live = conn.prepare(&sql::jellyfin_live_items(dry_run))?;
    for item in live.query_map([], sql::JellyfinLiveItem::read)? {
        let item = item?;
        if sorted
            .carried
            .remove(&derive(&item.kind, &item.id))
            .is_some()
        {
            sorted.clashes += 1;
        }
    }
    Ok(sorted)
}

/// The rows to store, as the JSON list the plan statements read: id, kind,
/// item, and whether it wants to be current.
fn plan(
    conn: &Connection,
    carried: HashMap<String, (String, String)>,
) -> Result<String, CarryError> {
    let mut rows = Vec::with_capacity(carried.len());
    let mut by_item: BTreeMap<(String, String), BTreeSet<String>> = BTreeMap::new();
    for (id, (kind, internal)) in carried {
        if matches!(kind.as_str(), "track" | "playlist") {
            by_item.entry((kind, internal)).or_default().insert(id);
        } else {
            rows.push(json!([id, kind, internal, 0]));
        }
    }
    let mut held = conn.prepare(sql::JELLYFIN_ID_HELD.text())?;
    for ((kind, internal), mut ids) in by_item {
        // The unkeyed id, when v2 stored it too, takes its place in the
        // order: it may be the one v2 handed out.
        let own = derive(&kind, &internal);
        if held.query_row(params![own, kind, internal], |row| row.get(0))? {
            ids.insert(own);
        }
        for (position, id) in ids.into_iter().enumerate() {
            let wanted = i64::from(position == 0);
            rows.push(json!([id, kind, internal, wanted]));
        }
    }
    Ok(serde_json::Value::Array(rows).to_string())
}

/// Store the ids v2 handed out. On a dry run, count only. Unmarked when the
/// bundle has no catalog, so a later export that brings one carries them.
pub(crate) fn apply(conn: &Connection, dry_run: bool) -> Result<SectionResult, CarryError> {
    let mut result = SectionResult::default();
    if bundle_rows(conn, &ALBUMS)? == 0 {
        return Ok(result);
    }
    let total = to_count(bundle_rows(conn, &JELLYFIN_IDS)?);
    result.rows = total;
    if total == 0 {
        return Ok(result);
    }
    let Sorted {
        carried,
        nothing,
        clashes,
    } = sort_rows(conn, total, dry_run)?;
    let plan = plan(conn, carried)?;
    let sql::JellyfinIdPlan {
        landing,
        taken,
        demoted,
    } = conn.query_row(
        sql::JELLYFIN_ID_PLAN.text(),
        params![plan],
        sql::JellyfinIdPlan::read,
    )?;
    if !dry_run {
        result.written = conn.execute(sql::INSERT_JELLYFIN_IDS.text(), params![plan])? as u64;
    }
    let landing = to_count(landing);
    let clashes = clashes + to_count(taken);
    let demoted = to_count(demoted);
    result.counts.imported = landing;
    result.counts.conflict_kept_existing = clashes;
    if landing > 0 {
        result.note(
            String::new(),
            "imported",
            format!("{landing} Jellyfin id(s) v2 handed out keep working"),
        );
    }
    // Ids of songs long gone are normal after years of use, so they are told in
    // the summary and do not mark the upgrade as having left records out.
    if nothing > 0 {
        result.summary.push(format!(
            "{nothing} old Jellyfin id(s) from v2 name nothing in v3 (a song or album no longer \
             there) and were left out. Nothing to do."
        ));
    }
    if clashes > 0 {
        result.note(
            String::new(),
            "conflict_kept_existing",
            format!(
                "{clashes} Jellyfin id(s) v2 stored clash with an id v3 gives another item, \
                 or cannot be handed out; left out"
            ),
        );
    }
    if demoted > 0 {
        result.note(
            String::new(),
            "landed_as_alias",
            format!(
                "{demoted} Jellyfin id(s) landed as a second id of an item that already had \
                 one; they still open it for a signed-in app"
            ),
        );
    }
    Ok(result)
}
