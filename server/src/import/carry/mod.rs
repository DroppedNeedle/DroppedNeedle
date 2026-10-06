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
//! Section transactions are deferred (`BEGIN`), not `BEGIN IMMEDIATE`: with
//! the bundle attached, an immediate begin also takes a write lock on the
//! bundle, which fails when the bundle sits on a read-only mount. The
//! import already holds the database alone (the data lock plus the write
//! probe), so nothing else can write between a section's read and write.

pub(crate) mod avatars;
pub(crate) mod baselines;
pub(crate) mod bundle;
pub(crate) mod connections;
pub(crate) mod editions;
pub(crate) mod held;
pub(crate) mod links;

use std::collections::HashMap;
use std::path::Path;

use serde_json::Value;
use sqlx::{SqliteConnection, SqlitePool};
use thiserror::Error;

use crate::export::sections::{ALL, Column, TableSection, Target};
use crate::import::report::{EntityCounts, ReportBuilder, utc_now_iso};
use crate::runtime_config::Crypto;

/// How the carry can fail. Messages stay free of row contents and secrets.
#[derive(Debug, Error)]
pub(crate) enum CarryError {
    /// A database read or write failed.
    #[error("database failure: {0}")]
    Db(#[from] sqlx::Error),
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
}

impl SectionResult {
    /// Add an itemized note.
    pub fn note(&mut self, key: String, outcome: &'static str, detail: impl Into<String>) {
        self.notes.push((key, outcome, detail.into()));
    }
}

/// One carry run's inputs.
pub(crate) struct CarryRun<'a> {
    /// Target database.
    pub pool: &'a SqlitePool,
    /// The parsed export document.
    pub root: &'a Value,
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
    /// Settle pending library links (last, on every import).
    Links,
}

impl Work {
    fn name(self) -> &'static str {
        match self {
            Self::Connections => connections::ENTITY,
            Self::Section(section) => section.name,
            Self::Editions => editions::ENTITY,
            Self::Baselines => baselines::ENTITY,
            Self::Links => links::ENTITY,
        }
    }
}

/// Check the bundle before anything is written: attach it to a pool
/// connection, run [`bundle::check`], detach.
pub(crate) async fn check_bundle(
    pool: &SqlitePool,
    found: &bundle::Bundle,
    root: &Value,
) -> Result<(), bundle::BundleRejection> {
    let internal = |error: sqlx::Error| bundle::BundleRejection::Internal(error.to_string());
    let user_ids: Vec<&str> = root
        .get("users")
        .and_then(Value::as_array)
        .map(|users| {
            users
                .iter()
                .filter_map(|user| user.get("id").and_then(Value::as_str))
                .collect()
        })
        .unwrap_or_default();
    let user_ids_json = serde_json::to_string(&user_ids).unwrap_or_else(|_| "[]".to_owned());
    let mut conn = pool.acquire().await.map_err(internal)?;
    bundle::attach(&mut conn, &found.path)
        .await
        .map_err(|error| bundle::BundleRejection::Invalid(format!("BUNDLE_UNREADABLE: {error}")))?;
    let outcome = bundle::check(&mut conn, found, &user_ids_json).await;
    bundle::detach(&mut conn).await;
    outcome
}

/// Carry every section. Returns how many rows and files were written.
pub(crate) async fn run(run: CarryRun<'_>, report: &mut ReportBuilder) -> Result<u64, CarryError> {
    let mut conn = run.pool.acquire().await?;
    if let Some(found) = run.bundle {
        bundle::attach(&mut conn, &found.path).await?;
    }
    let outcome = run_sections(&mut conn, &run, report).await;
    if run.bundle.is_some() {
        bundle::detach(&mut conn).await;
    }
    outcome
}

async fn run_sections(
    conn: &mut SqliteConnection,
    run: &CarryRun<'_>,
    report: &mut ReportBuilder,
) -> Result<u64, CarryError> {
    let text = |key: &str| {
        run.root
            .get(key)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned()
    };
    let marker = Marker {
        instance_id: text("instance_id"),
        digest: text(crate::export::seal::DIGEST_KEY),
    };
    let mut work = Vec::new();
    if crate::import::pipeline::carries_connections(run.root) {
        work.push(Work::Connections);
    }
    if run.bundle.is_some() {
        work.extend(ALL.iter().copied().map(Work::Section));
        work.push(Work::Editions);
        work.push(Work::Baselines);
    }
    work.push(Work::Links);
    let mut written = 0;
    for (done, unit) in work.into_iter().enumerate() {
        written += carry_section(conn, run, &marker, unit, report).await?;
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
async fn carry_section(
    conn: &mut SqliteConnection,
    run: &CarryRun<'_>,
    marker: &Marker,
    unit: Work,
    report: &mut ReportBuilder,
) -> Result<u64, CarryError> {
    let name = unit.name();
    let carried: Option<i64> = sqlx::query_scalar(
        "SELECT rows_written FROM import_progress WHERE instance_id = ?1 AND section = ?2",
    )
    .bind(&marker.instance_id)
    .bind(name)
    .fetch_optional(&mut *conn)
    .await?;
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
        let result = apply(conn, run, unit).await?;
        record(report, name, result);
        return Ok(0);
    }
    sqlx::query("BEGIN").execute(&mut *conn).await?;
    match apply_with_marker(conn, run, marker, unit).await {
        Ok(result) => {
            sqlx::query("COMMIT").execute(&mut *conn).await?;
            let written = result.written;
            record(report, name, result);
            Ok(written)
        }
        Err(error) => {
            if let Err(rollback) = sqlx::query("ROLLBACK").execute(&mut *conn).await {
                tracing::warn!(section = name, %rollback, "import section rollback failed");
            }
            Err(error)
        }
    }
}

/// Apply a section and, when it held any rows, mark it carried for this
/// instance in the same transaction.
async fn apply_with_marker(
    conn: &mut SqliteConnection,
    run: &CarryRun<'_>,
    marker: &Marker,
    unit: Work,
) -> Result<SectionResult, CarryError> {
    let result = apply(conn, run, unit).await?;
    if result.rows > 0 {
        sqlx::query(
            "INSERT INTO import_progress (instance_id, section, export_digest, rows_written, \
             applied_at) VALUES (?1, ?2, ?3, ?4, ?5)",
        )
        .bind(&marker.instance_id)
        .bind(unit.name())
        .bind(&marker.digest)
        .bind(i64::try_from(result.written).unwrap_or(i64::MAX))
        .bind(utc_now_iso())
        .execute(&mut *conn)
        .await?;
    }
    Ok(result)
}

async fn apply(
    conn: &mut SqliteConnection,
    run: &CarryRun<'_>,
    unit: Work,
) -> Result<SectionResult, CarryError> {
    let section = match unit {
        Work::Connections => {
            return connections::apply(conn, run.root, run.unsealed, run.crypto, run.dry_run).await;
        }
        Work::Editions => return editions::apply(conn, run.dry_run).await,
        Work::Baselines => return baselines::apply(conn, run.dry_run).await,
        Work::Links => return links::apply(conn, run.dry_run).await,
        Work::Section(section) => section,
    };
    // A section this bundle does not hold (an older exporter) is empty.
    let present = bundle::present_columns(conn, section).await?;
    if present.is_empty() {
        return Ok(SectionResult::default());
    }
    match section.target {
        Target::Table(table) => apply_table(conn, section, table, &present, run.dry_run).await,
        Target::AvatarFiles => avatars::apply(conn, run.cache_dir, run.dry_run).await,
        Target::HeldFiles => held::apply(conn, section, &present, run.cache_dir, run.dry_run).await,
    }
}

fn to_count(value: i64) -> u64 {
    u64::try_from(value).unwrap_or(0)
}

/// Apply one table section with set-based SQL.
async fn apply_table(
    conn: &mut SqliteConnection,
    section: &'static TableSection,
    table: &str,
    present: &[&'static Column],
    dry_run: bool,
) -> Result<SectionResult, CarryError> {
    let sql = bundle::section_sql(section, table, present, dry_run);
    let (total, orphans, new, identical): (i64, i64, i64, i64) = sqlx::query_as(&sql.count)
        .persistent(false)
        .fetch_one(&mut *conn)
        .await?;
    let mut result = SectionResult {
        rows: to_count(total),
        ..SectionResult::default()
    };
    result.counts.skipped_identical = to_count(identical);
    result.counts.dropped_invalid = to_count(orphans);
    let landed = if dry_run {
        for count in &sql.link_counts {
            let links: i64 = sqlx::query_scalar(count)
                .persistent(false)
                .fetch_one(&mut *conn)
                .await?;
            result.pending_links += to_count(links);
        }
        to_count(new)
    } else {
        for link in &sql.links {
            let done = sqlx::query(link)
                .persistent(false)
                .execute(&mut *conn)
                .await?;
            result.pending_links += done.rows_affected();
        }
        let done = sqlx::query(&sql.insert)
            .persistent(false)
            .execute(&mut *conn)
            .await?;
        result.written = done.rows_affected();
        done.rows_affected()
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
                 place another row takes); the kept row stays"
            ),
        );
    }
    if orphans > 0 {
        let parents: Vec<&str> = section.parents.iter().map(|parent| parent.table).collect();
        result.note(
            String::new(),
            "dropped_invalid",
            format!(
                "{orphans} row(s) belong to a {} row that did not land in v3; left out",
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
}
