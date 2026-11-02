//! The attached bundle on the import side: find it, prove it is the file
//! the digest covers, and check it before anything is written.
//!
//! The bundle is attached to the import's connection as schema `bundle`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use super::sql::{self, AnySection, UserSection};
use crate::export::envelope::{BUNDLE_ROLE, ExportDoc};
use crate::export::sections::{ALL, Column, TableSection, Target};
use rusqlite::{Connection, params};

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
pub(crate) fn locate(
    doc: &ExportDoc,
    dir: Option<&Path>,
) -> Result<Option<Bundle>, BundleRejection> {
    let Some(attachment) = doc
        .attachments
        .iter()
        .find(|attachment| attachment.role == BUNDLE_ROLE)
    else {
        return Ok(None);
    };
    let name = attachment.name.as_str();
    let Some(dir) = dir else {
        return Err(BundleRejection::Invalid(format!(
            "BUNDLE_MISSING: the export comes with {name}; keep it beside the export file"
        )));
    };
    let path = dir.join(name);
    let expected_sha = attachment.sha256.to_ascii_lowercase();
    let expected_bytes = Some(attachment.bytes);
    let declared = attachment.sections.clone();
    match crate::export::bundle::file_sha256(&path) {
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

/// Attach the bundle to `conn` as `bundle`, read-only and immutable: the
/// import never writes to it, and the bundle may sit on a read-only mount.
pub(crate) fn attach(conn: &Connection, path: &Path) -> rusqlite::Result<()> {
    let uri = format!(
        "file:{}?mode=ro&immutable=1",
        droppedneedle_kernel::db::factory::sqlite_uri_path(path)
    );
    conn.execute(sql::ATTACH_BUNDLE.text(), params![uri])?;
    Ok(())
}

/// Detach the bundle. Failures are logged: the connection is the import's
/// own and the next attach would report the real problem.
pub(crate) fn detach(conn: &Connection) {
    if let Err(error) = conn.execute(sql::DETACH_BUNDLE.text(), []) {
        tracing::warn!(%error, "import bundle detach failed");
    }
}

/// The names one statement with a single text parameter answers.
fn names_of(conn: &Connection, text: &str, table: &str) -> rusqlite::Result<Vec<String>> {
    let mut statement = conn.prepare(text)?;
    let names = statement.query_map(params![table], |row| row.get(0))?;
    names.collect()
}

/// The spec columns a bundle section actually holds, in spec order.
/// Empty when the section table is missing.
pub(crate) fn present_columns(
    conn: &Connection,
    section: &'static TableSection,
) -> rusqlite::Result<Vec<&'static Column>> {
    let names = names_of(conn, sql::BUNDLE_COLUMNS.text(), section.name)?;
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
pub(crate) fn check(
    conn: &Connection,
    bundle: &Bundle,
    user_ids_json: &str,
) -> Result<(), BundleRejection> {
    let internal = |error: rusqlite::Error| BundleRejection::Internal(error.to_string());
    for section in ALL.iter().copied() {
        let present = present_columns(conn, section).map_err(internal)?;
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
        let rules = section.unique.iter().flat_map(|rule| rule.iter());
        for key in section.key.iter().chain(rules) {
            if !present.iter().any(|column| column.name == *key) {
                return Err(BundleRejection::Invalid(format!(
                    "BUNDLE_SECTION_MISSING: {} lacks its {key} column",
                    section.name
                )));
            }
        }
        if let Target::Table(table) = section.target {
            let required = names_of(conn, sql::REQUIRED_COLUMNS.text(), table).map_err(internal)?;
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
        let rows: i64 = conn
            .query_row(&sql::section_rows(AnySection(section)), [], |row| {
                row.get(0)
            })
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
        let dangling: i64 = conn
            .query_row(
                &sql::section_dangling(UserSection { section, user }),
                params![user_ids_json],
                |row| row.get(0),
            )
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
