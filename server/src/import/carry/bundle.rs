//! The attached bundle on the import side: find it, prove it is the file
//! the digest covers, check it before anything is written, and build the
//! set-based SQL that applies one section.
//!
//! The bundle is attached to one connection as schema `bundle`. Column
//! names never come from the bundle itself: only names the section spec
//! knows are used, so a crafted bundle cannot inject SQL through them.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde_json::Value;
use sqlx::SqliteConnection;

use crate::export::envelope::BUNDLE_ROLE;
use crate::export::sections::{ALL, Column, LinkKind, LinkMode, TableSection, Target};

/// Schema name the bundle is attached under.
pub(crate) const SCHEMA: &str = "bundle";

/// A bundle the import may read: its path and its declared row counts.
#[derive(Debug, Clone)]
pub(crate) struct Bundle {
    /// File on disk, hash already checked.
    pub path: PathBuf,
    /// Rows per section as the export file declares them.
    pub declared: BTreeMap<String, u64>,
}

/// Why a bundle cannot be used. Messages name files and sections, never
/// row contents.
#[derive(Debug)]
pub(crate) enum BundleRejection {
    /// The file is missing, swapped, or inconsistent with the export.
    Invalid(String),
    /// Reading it failed for reasons outside the file.
    Internal(String),
}

/// Find the bundle the export names beside it and check its hash and
/// size. `None` when the export carries no bundle (format 1 files).
pub(crate) async fn locate(
    root: &Value,
    dir: Option<&Path>,
) -> Result<Option<Bundle>, BundleRejection> {
    let Some(attachment) = root
        .get("attachments")
        .and_then(Value::as_array)
        .and_then(|items| {
            items
                .iter()
                .find(|item| item.get("role").and_then(Value::as_str) == Some(BUNDLE_ROLE))
        })
    else {
        return Ok(None);
    };
    let name = attachment
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let Some(dir) = dir else {
        return Err(BundleRejection::Invalid(format!(
            "BUNDLE_MISSING: the export comes with {name}; keep it beside the export file"
        )));
    };
    let path = dir.join(name);
    let expected_sha = attachment
        .get("sha256")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_ascii_lowercase();
    let expected_bytes = attachment.get("bytes").and_then(Value::as_u64);
    let declared = attachment
        .get("sections")
        .and_then(|sections| serde_json::from_value(sections.clone()).ok())
        .unwrap_or_default();
    let hashed = path.clone();
    let digest = tokio::task::spawn_blocking(move || crate::export::bundle::file_sha256(&hashed))
        .await
        .map_err(|error| BundleRejection::Internal(error.to_string()))?;
    match digest {
        Ok((sha, bytes)) if sha == expected_sha && Some(bytes) == expected_bytes => {
            Ok(Some(Bundle { path, declared }))
        }
        Ok(_) => Err(BundleRejection::Invalid(format!(
            "BUNDLE_MISMATCH: {} is not the bundle this export was made with",
            path.display()
        ))),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            Err(BundleRejection::Invalid(format!(
                "BUNDLE_MISSING: {} is not there; keep it beside the export file",
                path.display()
            )))
        }
        Err(error) => Err(BundleRejection::Internal(format!(
            "cannot read {}: {error}",
            path.display()
        ))),
    }
}

/// Attach the bundle to `conn` under [`SCHEMA`], read-only and
/// immutable: the import never writes to it, and the bundle may sit on a
/// read-only mount.
pub(crate) async fn attach(conn: &mut SqliteConnection, path: &Path) -> Result<(), sqlx::Error> {
    let uri = format!(
        "file:{}?mode=ro&immutable=1",
        crate::export::v2dir::sqlite_uri_path(path)
    );
    sqlx::query("ATTACH DATABASE ?1 AS bundle")
        .bind(uri)
        .execute(&mut *conn)
        .await?;
    Ok(())
}

/// Detach the bundle. Failures are logged: the connection is the import's
/// own and the next attach would report the real problem.
pub(crate) async fn detach(conn: &mut SqliteConnection) {
    if let Err(error) = sqlx::query("DETACH DATABASE bundle")
        .execute(&mut *conn)
        .await
    {
        tracing::warn!(%error, "import bundle detach failed");
    }
}

/// The spec columns a bundle section actually holds, in spec order.
/// Empty when the section table is missing.
pub(crate) async fn present_columns(
    conn: &mut SqliteConnection,
    section: &'static TableSection,
) -> Result<Vec<&'static Column>, sqlx::Error> {
    let names: Vec<String> = sqlx::query_scalar(&format!(
        "SELECT name FROM pragma_table_info('{}', '{SCHEMA}')",
        section.name
    ))
    .persistent(false)
    .fetch_all(&mut *conn)
    .await?;
    Ok(section
        .columns
        .iter()
        .filter(|column| names.iter().any(|name| name == column.name))
        .collect())
}

/// Spec target columns the bundle section lacks: v3 fills them with its
/// defaults, and the report lists them.
pub(crate) fn omitted_columns(section: &TableSection, present: &[&Column]) -> Vec<&'static str> {
    section
        .columns
        .iter()
        .filter(|column| column.in_target())
        .filter(|column| !present.iter().any(|held| held.name == column.name))
        .map(|column| column.name)
        .collect()
}

/// Check the attached bundle against the export before anything is
/// written. A section the export declares must be there with its key
/// columns and the declared row count; a section neither the bundle nor
/// the export knows (an older exporter) reads as empty. Every v3 column
/// that has no default must be in the bundle, and rows may name only
/// users the export carries.
pub(crate) async fn check(
    conn: &mut SqliteConnection,
    bundle: &Bundle,
    user_ids_json: &str,
) -> Result<(), BundleRejection> {
    let internal = |error: sqlx::Error| BundleRejection::Internal(error.to_string());
    for section in ALL {
        let present = present_columns(conn, section).await.map_err(internal)?;
        let declared = bundle.declared.get(section.name);
        if present.is_empty() {
            if declared.is_some() {
                return Err(BundleRejection::Invalid(format!(
                    "BUNDLE_SECTION_MISSING: the bundle has no {} section",
                    section.name
                )));
            }
            continue;
        }
        for key in section.key {
            if !present.iter().any(|column| column.name == *key) {
                return Err(BundleRejection::Invalid(format!(
                    "BUNDLE_SECTION_MISSING: {} lacks its {key} column",
                    section.name
                )));
            }
        }
        if let Target::Table(table) = section.target {
            let required: Vec<String> = sqlx::query_scalar(
                "SELECT name FROM pragma_table_info(?1, 'main') \
                 WHERE \"notnull\" = 1 AND dflt_value IS NULL",
            )
            .bind(table)
            .persistent(false)
            .fetch_all(&mut *conn)
            .await
            .map_err(internal)?;
            let lands = |name: &str| {
                present
                    .iter()
                    .any(|column| column.in_target() && column.name == name)
            };
            if let Some(missing) = required.iter().find(|name| !lands(name)) {
                return Err(BundleRejection::Invalid(format!(
                    "BUNDLE_COLUMN_MISSING: {} has no {missing} values and v3 has no default",
                    section.name
                )));
            }
        }
        let rows: i64 = sqlx::query_scalar(&format!(
            "SELECT COUNT(*) FROM {SCHEMA}.\"{}\"",
            section.name
        ))
        .persistent(false)
        .fetch_one(&mut *conn)
        .await
        .map_err(internal)?;
        if let Some(declared) = declared
            && u64::try_from(rows).ok() != Some(*declared)
        {
            return Err(BundleRejection::Invalid(format!(
                "BUNDLE_MISMATCH: {} holds {rows} rows, the export says {declared}",
                section.name
            )));
        }
        let Some(user) = section.user_column else {
            continue;
        };
        if !present.iter().any(|column| column.name == user) {
            continue;
        }
        let dangling: i64 = sqlx::query_scalar(&format!(
            "SELECT COUNT(*) FROM {SCHEMA}.\"{name}\" WHERE \"{user}\" IS NOT NULL \
             AND \"{user}\" NOT IN (SELECT value FROM json_each(?1))",
            name = section.name
        ))
        .bind(user_ids_json)
        .persistent(false)
        .fetch_one(&mut *conn)
        .await
        .map_err(internal)?;
        if dangling > 0 {
            return Err(BundleRejection::Invalid(format!(
                "DANGLING_USER_REF: {dangling} {} row(s) belong to users the export does not carry",
                section.name
            )));
        }
    }
    Ok(())
}

/// The SQL that applies one table section.
pub(crate) struct SectionSql {
    /// One row: total, orphans (their parent row is not in v3), new
    /// (parent present, no key or uniqueness clash), identical (every
    /// column equal in v3).
    pub count: String,
    /// One statement per library reference column, recording unresolved
    /// references of new rows in `import_pending_links`.
    pub links: Vec<String>,
    /// The same references counted, for dry runs.
    pub link_counts: Vec<String>,
    /// Insert the new rows. A plain insert: the new-row test already
    /// excludes every key and uniqueness clash, so any other constraint
    /// failure is a real error and fails the section.
    pub insert: String,
}

fn quote(name: &str) -> String {
    format!("\"{name}\"")
}

/// The v3 catalog table holding one link kind.
fn catalog_table(kind: &str) -> &'static str {
    match kind {
        "album" => "local_albums",
        "artist" => "local_artists",
        "tombstone" => "library_reference_tombstones",
        _ => "local_tracks",
    }
}

/// SQL for the reference kind of `kind` on bundle row `b`.
fn kind_sql(kind: LinkKind) -> String {
    match kind {
        LinkKind::Track => "'track'".to_owned(),
        LinkKind::Album => "'album'".to_owned(),
        LinkKind::Artist => "'artist'".to_owned(),
        LinkKind::Tombstone => "'tombstone'".to_owned(),
        LinkKind::ByColumn(column) => format!("b.{}", quote(column)),
    }
}

/// SQL that is true when the v3 catalog already holds `id`.
fn resolved_sql(kind: LinkKind, id: &str) -> String {
    let exists = |kind: &str| {
        format!(
            "EXISTS (SELECT 1 FROM main.{} c WHERE c.id = {id})",
            catalog_table(kind)
        )
    };
    match kind {
        LinkKind::Track => exists("track"),
        LinkKind::Album => exists("album"),
        LinkKind::Artist => exists("artist"),
        LinkKind::Tombstone => exists("tombstone"),
        LinkKind::ByColumn(column) => format!(
            "CASE b.{} WHEN 'track' THEN {} WHEN 'album' THEN {} WHEN 'artist' THEN {} ELSE 0 END",
            quote(column),
            exists("track"),
            exists("album"),
            exists("artist"),
        ),
    }
}

/// The value a column takes in v3 for bundle row `b`.
fn value_sql(column: &Column) -> String {
    let raw = format!("b.{}", quote(column.name));
    match column.link {
        Some(link) if link.mode == LinkMode::UntilResolved => {
            format!("CASE WHEN {} THEN {raw} END", resolved_sql(link.kind, &raw))
        }
        _ => raw,
    }
}

fn joined(parts: impl Iterator<Item = String>, separator: &str) -> String {
    parts.collect::<Vec<_>>().join(separator)
}

/// The bundle section that fills one v3 table.
fn section_for(table: &str) -> Option<&'static TableSection> {
    ALL.iter()
        .copied()
        .find(|section| matches!(section.target, Target::Table(name) if name == table))
}

/// SQL that is true when every parent row of bundle row `row` is in v3.
/// On a dry run nothing is written, so a parent also counts when its own
/// bundle row would land.
fn has_parent_sql(section: &TableSection, row: &str, dry_run: bool) -> String {
    if section.parents.is_empty() {
        return "1".to_owned();
    }
    joined(
        section.parents.iter().map(|parent| {
            let matched = |alias: &str| {
                joined(
                    parent.columns.iter().map(|(own, theirs)| {
                        format!("{alias}.{} = {row}.{}", quote(theirs), quote(own))
                    }),
                    " AND ",
                )
            };
            let in_v3 = format!(
                "EXISTS (SELECT 1 FROM main.{} {row}p WHERE {})",
                quote(parent.table),
                matched(&format!("{row}p"))
            );
            match section_for(parent.table).filter(|_| dry_run) {
                Some(source) => {
                    let alias = format!("{row}q");
                    format!(
                        "({in_v3} OR EXISTS (SELECT 1 FROM {SCHEMA}.{} {alias} WHERE {} AND {}))",
                        quote(source.name),
                        matched(&alias),
                        lands_sql(source, &alias, dry_run)
                    )
                }
                None => in_v3,
            }
        }),
        " AND ",
    )
}

/// SQL that is true when bundle row `row` clashes with no v3 row on its
/// key or another unique rule, nor with an earlier bundle row on such a
/// rule (the first one wins). Unique columns compare with `=`: rows
/// holding a NULL there never clash, as in SQLite.
fn is_new_sql(section: &TableSection, row: &str) -> String {
    let target_table = match section.target {
        Target::Table(table) => format!("main.{}", quote(table)),
        Target::AvatarFiles => return "1".to_owned(),
    };
    let key_match = joined(
        section
            .key
            .iter()
            .map(|key| format!("{row}m.{0} IS {row}.{0}", quote(key))),
        " AND ",
    );
    let mut tests = vec![format!(
        "NOT EXISTS (SELECT 1 FROM {target_table} {row}m WHERE {key_match})"
    )];
    for rule in section.unique {
        let against = |alias: &str| {
            joined(
                rule.iter()
                    .map(|name| format!("{alias}.{0} = {row}.{0}", quote(name))),
                " AND ",
            )
        };
        tests.push(format!(
            "NOT EXISTS (SELECT 1 FROM {target_table} {row}m WHERE {})",
            against(&format!("{row}m"))
        ));
        tests.push(format!(
            "NOT EXISTS (SELECT 1 FROM {SCHEMA}.{} {row}e WHERE {row}e.rowid < {row}.rowid AND {})",
            quote(section.name),
            against(&format!("{row}e"))
        ));
    }
    tests.join(" AND ")
}

/// SQL that is true when bundle row `row` lands in v3: its parents are
/// there and it clashes with nothing.
fn lands_sql(section: &TableSection, row: &str, dry_run: bool) -> String {
    format!(
        "({}) AND {}",
        has_parent_sql(section, row, dry_run),
        is_new_sql(section, row)
    )
}

/// Build the statements for one table section over the columns the
/// bundle holds. `dry_run` lets parents count when they would land.
pub(crate) fn section_sql(
    section: &TableSection,
    table: &str,
    present: &[&Column],
    dry_run: bool,
) -> SectionSql {
    let target: Vec<&Column> = present
        .iter()
        .copied()
        .filter(|column| column.in_target())
        .collect();
    let source = format!("{SCHEMA}.{} b", quote(section.name));
    let target_table = format!("main.{}", quote(table));
    let all_match = joined(
        target
            .iter()
            .map(|column| format!("m.{} IS {}", quote(column.name), value_sql(column))),
        " AND ",
    );
    let is_new = is_new_sql(section, "b");
    let has_parent = has_parent_sql(section, "b", dry_run);
    let identical = format!("EXISTS (SELECT 1 FROM {target_table} m WHERE {all_match})");
    let count = format!(
        "SELECT COUNT(*), \
         COALESCE(SUM(CASE WHEN NOT ({identical}) AND NOT ({has_parent}) THEN 1 ELSE 0 END), 0), \
         COALESCE(SUM(CASE WHEN NOT ({identical}) AND ({has_parent}) AND {is_new} \
         THEN 1 ELSE 0 END), 0), \
         COALESCE(SUM(CASE WHEN {identical} THEN 1 ELSE 0 END), 0) FROM {source}"
    );
    let lands = format!("({has_parent}) AND {is_new}");
    let key_json = format!(
        "json_array({})",
        joined(
            section.key.iter().map(|key| format!("b.{}", quote(key))),
            ", "
        )
    );
    let mut links = Vec::new();
    let mut link_counts = Vec::new();
    for column in present {
        let Some(link) = column.link else { continue };
        let raw = format!("b.{}", quote(column.name));
        let when = if link.when.is_empty() {
            String::new()
        } else {
            format!(" AND ({})", link.when)
        };
        let pending = format!(
            "{raw} IS NOT NULL AND {raw} <> '' AND NOT ({resolved}){when} AND {lands}",
            resolved = resolved_sql(link.kind, &raw),
        );
        links.push(format!(
            "INSERT OR IGNORE INTO main.import_pending_links \
             (target_table, target_key, ref_column, ref_kind, v2_id, mode) \
             SELECT '{table}', {key_json}, '{name}', {kind}, {raw}, '{mode}' FROM {source} \
             WHERE {pending}",
            name = column.name,
            kind = kind_sql(link.kind),
            mode = link.mode.as_str(),
        ));
        link_counts.push(format!("SELECT COUNT(*) FROM {source} WHERE {pending}"));
    }
    let insert = format!(
        "INSERT INTO {target_table} ({cols}) SELECT {values} FROM {source} \
         WHERE {lands} ORDER BY b.rowid",
        cols = joined(target.iter().map(|column| quote(column.name)), ", "),
        values = joined(target.iter().map(|column| value_sql(column)), ", "),
    );
    SectionSql {
        count,
        links,
        link_counts,
        insert,
    }
}
