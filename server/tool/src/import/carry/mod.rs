//! The section carry: every piece of user data the export brings beyond
//! accounts, settings and follows.
//!
//! Sections apply one at a time, each in its own transaction: per-user
//! connections from the export JSON first, then every bundle section in
//! [`ALL`] order. A section that held any rows is marked carried in
//! `import_progress`, keyed by the v2 instance and the section name,
//! inside that same transaction. Each section is carried once per
//! instance: an interrupted import resumes at the first unmarked section,
//! a repeat import writes nothing, and a later export of the same instance
//! brings only sections never carried before, so rows a user deleted in v3
//! do not come back. Rows land only where their key, unique rules and
//! parent rows allow, so a section never writes the same row twice even
//! without its marker, and a row whose parent did not land is counted
//! instead of breaking the section.
//!
//! The work is set-based SQL against the attached bundle, so memory stays
//! flat however many listens or downloads v2 had.
//!
//! Section transactions are deferred: the import already holds the database
//! alone (the data lock plus the write probe), so nothing else can write
//! between a section's read and write, and an immediate begin would also touch
//! the attached bundle, which may be on a read-only mount.

pub(crate) mod avatars;
pub(crate) mod baselines;
pub(crate) mod bundle;
pub(crate) mod connections;
pub(crate) mod editions;
pub(crate) mod held;
pub(crate) mod jellyfin_ids;
pub(crate) mod links;
pub(crate) mod sql;
pub(crate) mod unknown_artist;

use std::collections::HashMap;
use std::path::Path;

use rusqlite::{Connection, OptionalExtension as _, params};
use thiserror::Error;

use crate::export::envelope::ExportDoc;
use crate::export::sections::contributions::CALLBACK_TOKENS;
use crate::export::sections::{ALL, Column, TableSection, Target};
use crate::import::report::{EntityCounts, ReportBuilder, now_ms};
use droppedneedle_kernel::db::time::Millis;
use droppedneedle_kernel::runtime_config::Crypto;
use droppedneedle_library::scan::naming::UNKNOWN_ARTIST;

/// How the carry can fail. Messages stay free of row contents and secrets.
#[derive(Debug, Error)]
pub(crate) enum CarryError {
    /// A database read or write failed.
    #[error("database failure: {0}")]
    Db(#[from] rusqlite::Error),
    /// A file read or write failed.
    #[error("file failure: {0}")]
    Io(String),
    /// Sealing under the v3 key failed.
    #[error("re-encryption failure")]
    Rekey,
    /// Simulated crash between sections (tests only).
    #[error("simulated crash after {0} section(s); run the import again to resume")]
    Fault(usize),
}

/// What one section decided, kept apart from the report until the
/// section commits.
#[derive(Debug, Default)]
pub(crate) struct SectionResult {
    /// Outcome counts.
    pub counts: EntityCounts,
    /// Secrets sealed under the v3 key.
    pub secrets: u64,
    /// Library references recorded for the library carry.
    pub pending_links: u64,
    /// Notes worth itemizing: key, outcome, detail.
    pub notes: Vec<(String, &'static str, String)>,
    /// Rows or files written.
    pub written: u64,
    /// Rows the export held for this section; a section that held any is
    /// marked carried.
    pub rows: u64,
    /// Lines for the notes the upgrade summary prints.
    pub summary: Vec<String>,
    /// v2 Unknown Artist rows that folded, so the export's note about rows
    /// kept under their v2 id no longer lists them.
    pub unknown_artists_folded: usize,
}

impl SectionResult {
    /// Add an itemized note.
    pub fn note(&mut self, key: String, outcome: &'static str, detail: impl Into<String>) {
        self.notes.push((key, outcome, detail.into()));
    }
}

/// One carry run's inputs.
pub(crate) struct CarryRun<'a> {
    /// The typed export document.
    pub doc: &'a ExportDoc,
    /// The checked bundle, when the export has one.
    pub bundle: Option<&'a bundle::Bundle>,
    /// Opened sealed values by JSON path.
    pub unsealed: &'a HashMap<String, String>,
    /// v3 key for the connections.
    pub crypto: &'a Crypto,
    /// v3 cache dir, where avatars land.
    pub cache_dir: Option<&'a Path>,
    /// Count only; write nothing.
    pub dry_run: bool,
    /// Test-only: fail after this many sections.
    pub stop_after: Option<usize>,
}

/// One unit of carry work.
#[derive(Clone, Copy)]
enum Work {
    Connections,
    Section(&'static TableSection),
    /// Make carried edition choices sticky (after the catalog).
    Editions,
    /// Translate the carried v2 baselines (after the catalog and blobs).
    Baselines,
    /// Store the Jellyfin ids v2 handed out (after the catalog).
    JellyfinIds,
    /// Settle pending library links (on every import).
    Links,
    /// Fold v2's Unknown Artist into the built-in one (after the links, so
    /// the plays they settle move with it; on every import with a bundle).
    UnknownArtist,
}

impl Work {
    fn name(self) -> &'static str {
        match self {
            Self::Connections => connections::ENTITY,
            Self::Section(section) => section.name,
            Self::Editions => editions::ENTITY,
            Self::Baselines => baselines::ENTITY,
            Self::JellyfinIds => jellyfin_ids::ENTITY,
            Self::Links => links::ENTITY,
            Self::UnknownArtist => unknown_artist::ENTITY,
        }
    }
}

/// Check the bundle before anything is written: attach it, run
/// [`bundle::check`], detach.
pub(crate) fn check_bundle(
    conn: &Connection,
    found: &bundle::Bundle,
    doc: &ExportDoc,
) -> Result<(), bundle::BundleRejection> {
    let user_ids: Vec<&str> = doc.users.iter().map(|user| user.id.as_str()).collect();
    let user_ids_json = serde_json::to_string(&user_ids).unwrap_or_else(|_| "[]".to_owned());
    bundle::attach(conn, &found.path)
        .map_err(|error| bundle::BundleRejection::Invalid(format!("BUNDLE_UNREADABLE: {error}")))?;
    let outcome = bundle::check(conn, found, &user_ids_json);
    bundle::detach(conn);
    outcome
}

/// Carry every section. Returns how many rows and files were written.
pub(crate) fn run(
    conn: &mut Connection,
    run: CarryRun<'_>,
    report: &mut ReportBuilder,
) -> Result<u64, CarryError> {
    if let Some(found) = run.bundle {
        bundle::attach(conn, &found.path)?;
    }
    let outcome = run_sections(conn, &run, report);
    if run.bundle.is_some() {
        bundle::detach(conn);
    }
    outcome
}

fn run_sections(
    conn: &mut Connection,
    run: &CarryRun<'_>,
    report: &mut ReportBuilder,
) -> Result<u64, CarryError> {
    let marker = Marker {
        instance_id: run.doc.instance_id.clone(),
        digest: run.doc.content_hmac.clone(),
    };
    let mut work = Vec::new();
    // Per-user connections are a format 2 section.
    if run.doc.format_version >= 2 {
        work.push(Work::Connections);
    }
    if run.bundle.is_some() {
        work.extend(ALL.iter().copied().map(Work::Section));
        work.push(Work::Editions);
        work.push(Work::Baselines);
        work.push(Work::JellyfinIds);
    }
    work.push(Work::Links);
    if run.bundle.is_some() {
        work.push(Work::UnknownArtist);
    }
    let mut written = 0;
    for (done, unit) in work.into_iter().enumerate() {
        written += carry_section(conn, run, &marker, unit, report)?;
        if run.stop_after == Some(done + 1) {
            return Err(CarryError::Fault(done + 1));
        }
    }
    Ok(written)
}

/// Names the v2 instance a section is carried from, and the export file
/// that carried it.
struct Marker {
    instance_id: String,
    digest: String,
}

/// Apply one section in its own transaction, or skip it when an earlier
/// import from the same v2 instance already carried it.
fn carry_section(
    conn: &mut Connection,
    run: &CarryRun<'_>,
    marker: &Marker,
    unit: Work,
    report: &mut ReportBuilder,
) -> Result<u64, CarryError> {
    let name = unit.name();
    let carried: Option<i64> = conn
        .query_row(
            sql::SECTION_CARRIED.text(),
            params![marker.instance_id, name],
            |row| row.get(0),
        )
        .optional()?;
    if let Some(rows) = carried {
        report.note(
            "section",
            name.to_owned(),
            "already_applied",
            format!("an earlier import from this v2 instance carried it ({rows} row(s) written)"),
        );
        return Ok(0);
    }
    if run.dry_run {
        let result = apply(conn, run, unit)?;
        record(report, name, result);
        return Ok(0);
    }
    let transaction = conn.transaction()?;
    let outcome = match unit {
        Work::UnknownArtist => unknown_artist::fold(&transaction),
        _ => apply_with_marker(&transaction, run, marker, unit),
    };
    match outcome {
        Ok(result) => {
            transaction.commit()?;
            let written = result.written;
            record(report, name, result);
            Ok(written)
        }
        Err(error) => {
            if let Err(rollback) = transaction.rollback() {
                tracing::warn!(section = name, %rollback, "import section rollback failed");
            }
            Err(error)
        }
    }
}

/// Apply a section and, when it held any rows, mark it carried for this
/// instance in the same transaction.
fn apply_with_marker(
    conn: &Connection,
    run: &CarryRun<'_>,
    marker: &Marker,
    unit: Work,
) -> Result<SectionResult, CarryError> {
    let result = apply(conn, run, unit)?;
    if result.rows > 0 {
        conn.execute(
            sql::MARK_SECTION_CARRIED.text(),
            params![
                marker.instance_id,
                unit.name(),
                marker.digest,
                i64::try_from(result.written).unwrap_or(i64::MAX),
                Millis(now_ms()),
            ],
        )?;
    }
    Ok(result)
}

fn apply(conn: &Connection, run: &CarryRun<'_>, unit: Work) -> Result<SectionResult, CarryError> {
    let section = match unit {
        Work::Connections => {
            return connections::apply(
                conn,
                &run.doc.user_connections,
                run.unsealed,
                run.crypto,
                run.dry_run,
            );
        }
        Work::Editions => return editions::apply(conn, run.dry_run),
        Work::Baselines => return baselines::apply(conn, run.dry_run),
        Work::JellyfinIds => return jellyfin_ids::apply(conn, run.dry_run),
        Work::Links => return links::apply(conn, run.dry_run),
        // A real run folds through `carry_section`, on its transaction.
        Work::UnknownArtist => return unknown_artist::would_fold(conn),
        Work::Section(section) => section,
    };
    // A section this bundle does not hold (an older exporter) is empty.
    let present = bundle::present_columns(conn, section)?;
    if present.is_empty() {
        return Ok(SectionResult::default());
    }
    match section.target {
        Target::Table(table) => {
            let mut result = apply_table(conn, section, table, &present, run.dry_run)?;
            if section.name == CALLBACK_TOKENS.name {
                expire_return_links(conn, &mut result, run.dry_run)?;
            }
            Ok(result)
        }
        Target::AvatarFiles => avatars::apply(conn, run.cache_dir, run.dry_run),
        Target::HeldFiles => held::apply(conn, &present, run.cache_dir, run.dry_run),
        // A later step reads these rows from the bundle and reports them.
        Target::BundleOnly => Ok(SectionResult::default()),
    }
}

fn to_count(value: i64) -> u64 {
    u64::try_from(value).unwrap_or(0)
}

/// Apply one table section with set-based SQL.
fn apply_table(
    conn: &Connection,
    section: &'static TableSection,
    table: &str,
    present: &[&'static Column],
    dry_run: bool,
) -> Result<SectionResult, CarryError> {
    let sql::SectionCounts {
        total,
        orphans,
        new,
        identical,
    } = conn.query_row(
        &sql::section_count(section, table, present, dry_run),
        [],
        sql::SectionCounts::read,
    )?;
    let mut result = SectionResult {
        rows: to_count(total),
        ..SectionResult::default()
    };
    result.counts.skipped_identical = to_count(identical);
    result.counts.dropped_invalid = to_count(orphans);
    let landed = if dry_run {
        for column in present {
            let Some(count) = sql::link_count(section, column, dry_run) else {
                continue;
            };
            let links: i64 = conn.query_row(&count, [], |row| row.get(0))?;
            result.pending_links += to_count(links);
        }
        to_count(new)
    } else {
        for column in present {
            let Some(link) = sql::link_insert(section, table, column, dry_run) else {
                continue;
            };
            result.pending_links += conn.execute(&link, [])? as u64;
        }
        let inserted = conn.execute(&sql::section_insert(section, table, present, dry_run), [])?;
        result.written = inserted as u64;
        inserted as u64
    };
    result.counts.imported = landed;
    let conflicts = to_count(total)
        .saturating_sub(to_count(identical))
        .saturating_sub(to_count(orphans))
        .saturating_sub(landed);
    result.counts.conflict_kept_existing = conflicts;
    if conflicts > 0 {
        result.note(
            String::new(),
            "conflict_kept_existing",
            format!(
                "{conflicts} row(s) clash with a row already kept (same key, or the same \
                 place another row takes); the kept row stays. Nothing to do unless you \
                 want the v2 version, then import into an empty V3_ROOT"
            ),
        );
    }
    if orphans > 0 {
        let parents: Vec<&str> = section.parents.iter().map(|parent| parent.table).collect();
        result.note(
            String::new(),
            "dropped_invalid",
            format!(
                "{orphans} row(s) belong to a {} row that did not land in v3; left out. \
                 See that section's notes for why",
                parents.join(" or ")
            ),
        );
    }
    let omitted = bundle::omitted_columns(section, present);
    if !omitted.is_empty() {
        result.note(
            String::new(),
            "columns_defaulted",
            format!("v2 had no {}; v3's defaults were used", omitted.join(", ")),
        );
    }
    Ok(result)
}

/// MusicBrainz sends a carried return link to v2's address, which v3 does
/// not answer, so each one lands expired and the summary says how many
/// edits have to be started again.
fn expire_return_links(
    conn: &Connection,
    result: &mut SectionResult,
    dry_run: bool,
) -> Result<(), CarryError> {
    if !dry_run {
        conn.execute(sql::EXPIRE_CARRIED_RETURN_LINKS.text(), [])?;
    }
    let count = result.counts.imported;
    if count > 0 {
        let edits = if count == 1 { "edit" } else { "edits" };
        result.summary.push(format!(
            "{count} MusicBrainz {edits} open in v2 must be started again from v3."
        ));
    }
    Ok(())
}

/// Move one committed (or dry-run) section result into the report.
fn record(report: &mut ReportBuilder, name: &str, result: SectionResult) {
    report.add_counts(name, &result.counts);
    for _ in 0..result.secrets {
        report.secret_reencrypted();
    }
    report.add_pending_links(result.pending_links);
    report.add_carried_writes(result.written);
    for (key, outcome, detail) in result.notes {
        report.note(name, key, outcome, detail);
    }
    report.unlist_kept_reserved(UNKNOWN_ARTIST, result.unknown_artists_folded);
    for line in result.summary {
        report.add_note(line);
    }
}
