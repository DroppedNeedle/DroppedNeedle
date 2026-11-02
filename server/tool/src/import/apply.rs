//! v2 to v3 importer: parse, unlock, bundle check, then the records and
//! settings in one atomic commit, then the section carry ([`super::carry`],
//! one transaction per section).
//!
//! Every run, including dry-run, produces exactly one [`ImportReport`];
//! failures are exit codes, never panics. Plaintext secrets exist only in
//! memory and are never logged, reported, or written to temp files.
//!
//! The settings rows are written in the same transaction as the records, so
//! an import lands whole or not at all.

use std::collections::HashMap;
use std::path::PathBuf;

use droppedneedle_acquire::follows::rows;
use droppedneedle_auth::federated::users::ProviderBinding;
use droppedneedle_auth::sqlite::{AppPasswords, Users, WriteError};
use droppedneedle_auth::users::models::AppPasswordRecord;
use droppedneedle_catalog::concerts::store::{insert_city_in, set_seen_in};
use droppedneedle_kernel::db::open_connection;
use droppedneedle_kernel::runtime_config::{Crypto, LegacyConfig};

use rusqlite::{Connection, Transaction, TransactionBehavior, params};
use serde_json::Value;
use thiserror::Error;

use super::carry::bundle::{Bundle, BundleRejection};
use super::envelope::{ExportFile, Opener, collect_sealed, unseal_value};
use super::plan::{Plan, Planned, sha_hex};
use super::report::{ExitCode, ExportProvenance, ImportReport, ReportBuilder, utc_now_iso};
use super::settings::{configs_equivalent, current_settings, stage_settings, write_settings};
use super::snapshot::DatabaseSnapshot;
use super::sql;
use super::validate::{ValidationIssue, read_export};
use crate::allowlist::replaced_sections;
use crate::export::envelope::ExportDoc;

/// Internal import failure. Messages stay secret-free: rusqlite errors echo
/// SQL text, never bind values; crypto errors are fixed strings.
#[derive(Debug, Error)]
pub(super) enum ImportError {
    /// A database read or write failed.
    #[error("database failure: {0}")]
    Db(#[from] rusqlite::Error),
    /// An area's write failed in the database.
    #[error("database failure: {0}")]
    Area(String),
    /// An area refused a row: one the plan did not expect in the target.
    #[error("a row already in the target conflicts with one being imported")]
    Conflict,
    /// The settings cannot be read or staged.
    #[error("settings failure: {reason}")]
    Config {
        /// What went wrong, without file contents.
        reason: String,
    },
    /// Re-encryption under the v3 key failed.
    #[error("re-encryption failure")]
    Rekey,
    /// Reading the scan schedule from v2's `config.json` failed.
    #[error("v2 scan schedule carry failed: {0}")]
    ScheduleCarry(#[from] super::schedule_carry::ScheduleCarryError),
    /// A carried section failed; earlier sections stay applied.
    #[error("{0}")]
    Carry(#[from] super::carry::CarryError),
    /// Simulated crash before commit (tests only).
    #[error("simulated crash before commit")]
    FaultBeforeCommit,
    /// Simulated crash after commit (tests only).
    #[error("simulated crash after commit")]
    FaultAfterCommit,
}

/// Auth's write errors, with text as free of secrets as the rest:
/// rusqlite's text holds SQL, never bind values.
impl From<WriteError> for ImportError {
    fn from(error: WriteError) -> Self {
        match error {
            WriteError::Conflict => Self::Conflict,
            WriteError::Db(cause) => Self::Area(cause.to_string()),
        }
    }
}

/// One import run: inputs and test-only fault switches.
pub struct ImportRequest {
    /// Raw export file bytes.
    pub export_bytes: Vec<u8>,
    /// Operator passphrase unlocking the secret envelope.
    pub passphrase: String,
    /// Target v3 database file, already migrated. The import opens its own
    /// connection on it, which runs the import's transactions and has the
    /// bundle attached. Tests use scratch files only.
    pub db_path: PathBuf,
    /// Target v3 config folder. While the target stores no settings yet,
    /// its `config.json` (from before settings moved into the database) is
    /// what the import starts from, as the server's first start would.
    pub legacy: LegacyConfig,
    /// v3 data key for re-encryption.
    pub crypto: Crypto,
    /// v2 `config.json` for the one-shot scan-schedule carry, when available.
    pub v2_config_path: Option<PathBuf>,
    /// Folder holding the files the export names (its data bundle);
    /// normally the export file's own folder.
    pub attachments_dir: Option<PathBuf>,
    /// v3 cache folder, where carried avatar files land.
    pub cache_dir: Option<PathBuf>,
    /// Dry-run: identical decisions and counts, zero writes.
    pub dry_run: bool,
    /// Test-only: fail after staging everything, before commit, to
    /// prove the rollback (simulated kill mid-import).
    pub fault_before_commit: bool,
    /// Test-only: fail right after the DB commit, before the section
    /// carry, to prove re-import converges from there.
    pub fault_after_commit: bool,
    /// Test-only: fail after this many carried sections, to prove an
    /// interrupted import resumes.
    pub fault_after_sections: Option<usize>,
}

/// Run the full pipeline and return the report. This function does
/// not print; the CLI prints [`ImportReport::to_json`].
pub async fn run_import(request: ImportRequest) -> ImportReport {
    let provenance = lenient_provenance(&request.export_bytes);
    let dry_run = request.dry_run;
    let for_task = provenance.clone();
    match tokio::task::spawn_blocking(move || import_blocking(request, for_task)).await {
        Ok(report) => report,
        Err(error) => {
            tracing::error!(%error, "the import task failed");
            ReportBuilder::new(dry_run, provenance).finish(
                ExitCode::FailedInternal,
                "the import stopped unexpectedly; run it again".to_owned(),
            )
        }
    }
}

/// The pipeline, on a thread that may wait on files and on SQLite.
fn import_blocking(request: ImportRequest, provenance: ExportProvenance) -> ImportReport {
    let mut report = ReportBuilder::new(request.dry_run, provenance);

    let parsed = match ExportFile::parse(&request.export_bytes) {
        Ok(parsed) => parsed,
        Err(error) => {
            return report.finish(
                ExitCode::FailedValidation,
                format!("export file rejected: {error}"),
            );
        }
    };
    // Integrity first: a document that does not match its digest is not
    // read any further. An envelope that cannot derive a key at all is
    // left to the validator, which names the bad field.
    let opener = Opener::derive(&request.passphrase, &parsed.envelope);
    if let Ok(opener) = &opener
        && opener.verify(&parsed.root).is_err()
    {
        return digest_failure(report, &parsed.root, opener);
    }
    let (validation, doc) = read_export(&parsed.root);
    for warning in &validation.warnings {
        report.note(
            "export",
            warning.path.clone(),
            "warning",
            format!("{}: {}", warning.code, warning.message),
        );
    }
    let Some(doc) = doc else {
        for issue in &validation.errors {
            report.note(
                "export",
                issue.path.clone(),
                "error",
                format!("{}: {}", issue.code, issue.message),
            );
        }
        return report.finish(
            ExitCode::FailedValidation,
            validation_summary(&validation.errors),
        );
    };

    let opener = match opener {
        Ok(opener) => opener,
        Err(error) => {
            return report.finish(
                ExitCode::FailedValidation,
                format!("{}: {error}", error.code()),
            );
        }
    };
    let unsealed = match unseal_consumed(&parsed.root, &opener) {
        Ok(map) => map,
        Err(SealFailure::AuthFailed(path)) => {
            return report.finish(
                ExitCode::EnvelopeAuthFailed,
                format!("passphrase did not open sealed value at {path}"),
            );
        }
        Err(SealFailure::Corrupt(reason)) => {
            return report.finish(ExitCode::FailedInternal, reason);
        }
    };
    report.set_left_behind(doc.left_behind.clone());
    report.set_notes(doc.notes.clone());

    let mut conn = match open_connection(&request.db_path) {
        Ok(conn) => conn,
        Err(error) => {
            return report.finish(
                ExitCode::FailedInternal,
                format!("cannot open the target database: {error}"),
            );
        }
    };
    // A section insert reads the table it writes, so SQLite holds its new rows
    // in a temporary table first. On disk, memory stays flat however many
    // listens v2 had; in memory, a large one could kill a small host every run.
    if let Err(error) = conn.pragma_update(None, "temp_store", "FILE") {
        return report.finish(
            ExitCode::FailedInternal,
            format!("cannot open the target database: {error}"),
        );
    }

    // The bundle is checked whole before anything is written: a missing,
    // swapped or inconsistent bundle refuses the import with zero writes.
    let bundle = match prepare_bundle(&conn, &request, &doc) {
        Ok(bundle) => bundle,
        Err(BundleRejection::Invalid(reason)) => {
            report.note("export", "attachments".to_owned(), "error", reason.clone());
            return report.finish(ExitCode::FailedValidation, reason);
        }
        Err(BundleRejection::Internal(reason)) => {
            return report.finish(ExitCode::FailedInternal, reason);
        }
    };

    let outcome = run_guarded(
        &mut conn,
        &request,
        &doc,
        &unsealed,
        bundle.as_ref(),
        &mut report,
    );
    if outcome.is_err() && !report.committed() {
        report.discard_counts();
    }
    match outcome {
        Ok(()) => report.finish_completed(),
        Err(ImportError::FaultBeforeCommit) => report.finish(
            ExitCode::FailedInternal,
            "simulated crash before commit; rolled back".to_owned(),
        ),
        Err(ImportError::FaultAfterCommit) => report.finish(
            ExitCode::FailedInternal,
            "simulated crash after commit".to_owned(),
        ),
        Err(ImportError::ScheduleCarry(
            mismatch @ super::schedule_carry::ScheduleCarryError::InstanceMismatch { .. },
        )) => report.finish(ExitCode::FailedValidation, mismatch.to_string()),
        Err(error) => report.finish(ExitCode::FailedInternal, error.to_string()),
    }
}

/// Find the export's bundle, check its hash, then check its content
/// against the export.
fn prepare_bundle(
    conn: &Connection,
    request: &ImportRequest,
    doc: &ExportDoc,
) -> Result<Option<Bundle>, BundleRejection> {
    let Some(found) = super::carry::bundle::locate(doc, request.attachments_dir.as_deref())? else {
        return Ok(None);
    };
    super::carry::check_bundle(conn, &found, doc)?;
    Ok(Some(found))
}

/// Best-effort provenance for reports on unparseable files.
fn lenient_provenance(bytes: &[u8]) -> ExportProvenance {
    let parsed: Value = serde_json::from_slice(bytes).unwrap_or(Value::Null);
    ExportProvenance {
        format_version: parsed
            .get("format_version")
            .and_then(Value::as_u64)
            .and_then(|raw| u32::try_from(raw).ok())
            .unwrap_or(0),
        exported_at: parsed
            .get("exported_at")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        instance_id: parsed
            .get("instance_id")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
    }
}

fn validation_summary(errors: &[ValidationIssue]) -> String {
    let codes: Vec<&str> = errors.iter().take(5).map(|issue| issue.code).collect();
    let mut summary = format!("{} validation error(s): {}", errors.len(), codes.join(", "));
    if errors.len() > 5 {
        summary.push_str(", ...");
    }
    summary
}

/// How unsealing a consumed value can fail.
enum SealFailure {
    /// AEAD rejection: wrong passphrase or corrupt data.
    AuthFailed(String),
    /// Malformed sealed object or non-text plaintext.
    Corrupt(String),
}

/// Report a digest mismatch. When a consumed sealed value does not open
/// either, the passphrase is wrong rather than the file modified.
fn digest_failure(report: ReportBuilder, root: &Value, opener: &Opener) -> ImportReport {
    let sealed = consumed_sealed(root);
    if let Some((path, value)) = sealed.first()
        && let Err(super::envelope::EnvelopeError::UnlockFailed { .. }) =
            unseal_value(opener, value, path)
    {
        return report.finish(
            ExitCode::EnvelopeAuthFailed,
            format!("passphrase did not open sealed value at {path}"),
        );
    }
    let mut report = report;
    report.note(
        "export",
        crate::export::seal::DIGEST_KEY.to_owned(),
        "error",
        "CHECKSUM_MISMATCH: the content digest does not match the file".to_owned(),
    );
    report.finish(
        ExitCode::FailedValidation,
        "CHECKSUM_MISMATCH: export file was modified, truncated, or carries no digest".to_owned(),
    )
}

/// Every sealed value the importer consumes, with its path. This walk stays
/// on the raw JSON: a digest failure reads it before any document exists.
fn consumed_sealed(root: &Value) -> Vec<(String, Value)> {
    let mut found = Vec::new();
    if let Some(settings) = root.get("settings").and_then(Value::as_object) {
        // Known sections only: unknown ones are ignored with a warning, so a
        // broken blob there must not fail the import.
        for section in replaced_sections() {
            if let Some(block) = settings.get(section) {
                collect_sealed(block, format!("settings.{section}"), &mut found);
            }
        }
    }
    if let Some(users) = root.get("users") {
        collect_sealed(users, "users".to_owned(), &mut found);
    }
    if carries_connections(root)
        && let Some(connections) = root.get("user_connections")
    {
        collect_sealed(connections, "user_connections".to_owned(), &mut found);
    }
    found
}

/// Per-user connections are a format 2 section; a format 1 reader never
/// had them.
fn carries_connections(root: &Value) -> bool {
    root.get("format_version").and_then(Value::as_u64) >= Some(2)
        && root.get("user_connections").is_some()
}

/// Unseal every sealed value the importer consumes (settings plus app
/// passwords), strictly in memory. Ignored sections (reserved, unknown)
/// are not consumed, so a broken blob there cannot fail the import.
fn unseal_consumed(root: &Value, opener: &Opener) -> Result<HashMap<String, String>, SealFailure> {
    let found = consumed_sealed(root);
    let mut out = HashMap::with_capacity(found.len());
    for (path, sealed) in &found {
        match unseal_value(opener, sealed, path) {
            Ok(plaintext) => {
                out.insert(path.clone(), plaintext);
            }
            Err(super::envelope::EnvelopeError::UnlockFailed { .. }) => {
                return Err(SealFailure::AuthFailed(path.clone()));
            }
            Err(error) => {
                return Err(SealFailure::Corrupt(error.to_string()));
            }
        }
    }
    Ok(out)
}

/// The guarded middle of the pipeline: settings staging, entity merge and
/// settings rows in one atomic commit, then the section carry. Any error
/// before the commit leaves prior state untouched. Carried sections commit
/// one by one after that; a failure there keeps the sections already
/// applied, and a rerun resumes.
fn run_guarded(
    conn: &mut Connection,
    request: &ImportRequest,
    doc: &ExportDoc,
    unsealed: &HashMap<String, String>,
    bundle: Option<&Bundle>,
    report: &mut ReportBuilder,
) -> Result<(), ImportError> {
    let carry = |dry_run: bool| super::carry::CarryRun {
        doc,
        bundle,
        unsealed,
        crypto: &request.crypto,
        cache_dir: request.cache_dir.as_deref(),
        dry_run,
        stop_after: request.fault_after_sections,
    };
    let current_config = current_settings(conn, &request.legacy)?;
    let secrets_before = report.secrets_counted();
    let staged_config = stage_settings(
        doc,
        &current_config,
        unsealed,
        &request.crypto,
        request.v2_config_path.as_deref(),
        report,
    )?;
    // Unchanged settings are not written, so their secrets are not
    // re-encrypted after all.
    let config_unchanged = configs_equivalent(&current_config, &staged_config, &request.crypto);
    if config_unchanged {
        let staged = report.secrets_counted().saturating_sub(secrets_before);
        report.uncount_secrets(staged);
    }

    if request.dry_run {
        // One read transaction, so the snapshot is consistent.
        let tx = conn.transaction()?;
        let snapshot = DatabaseSnapshot::load(&tx)?;
        tx.rollback()?;
        Plan::decide(doc, &snapshot, unsealed, report);
        super::carry::run(conn, carry(true), report)?;
        return Ok(());
    }

    // The write lock comes first and the snapshot is read under it, so no
    // other writer can change a row between the plan and the apply. The
    // connection keeps foreign keys on.
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let snapshot = DatabaseSnapshot::load(&tx)?;
    let plan = Plan::decide(doc, &snapshot, unsealed, report);
    plan.apply(&tx, &request.crypto, unsealed)?;
    let settings_written = write_settings(&tx, &staged_config, &request.crypto)?;
    // A pure re-import (every record and setting identical) writes nothing
    // at all, not even an audit row: idempotency means zero writes.
    let run_id = if plan.has_writes() || settings_written > 0 {
        Some(record_import_run(&tx, doc, report, completed_exit(report))?)
    } else {
        None
    };
    if request.fault_before_commit {
        tx.rollback()?;
        return Err(ImportError::FaultBeforeCommit);
    }
    tx.commit()?;
    report.mark_committed();
    if request.fault_after_commit {
        // No audit fix-up: a real crash could not run one either.
        return Err(ImportError::FaultAfterCommit);
    }

    let carried = super::carry::run(conn, carry(false), report);
    // The audit row carries the whole run's real counts, the carried
    // sections included, and a failed run says so. A run whose only writes
    // were carried sections (a resume) records its own row.
    let exit = match &carried {
        Ok(_) => completed_exit(report),
        Err(_) => "FAILED_INTERNAL",
    };
    let audited = match run_id {
        Some(id) => update_import_run(conn, &id, exit, report),
        None if report.carried_writes() > 0 => insert_import_run(conn, doc, exit, report),
        None => Ok(()),
    };
    match (carried, audited) {
        (Err(error), audited) => {
            if let Err(audit) = audited {
                tracing::warn!(%audit, "import audit row write failed after a section failure");
            }
            return Err(error.into());
        }
        (Ok(_), audited) => audited?,
    }
    Ok(())
}

impl Plan {
    /// Execute the decided writes inside the caller's transaction, each
    /// through the area that owns the table.
    fn apply(
        &self,
        tx: &Transaction<'_>,
        crypto: &Crypto,
        unsealed: &HashMap<String, String>,
    ) -> Result<(), ImportError> {
        for write in &self.writes {
            match write {
                Planned::InsertUser(user) => Users::insert_user_in(tx, user)?,
                Planned::InsertProvider {
                    user_id,
                    provider,
                    provider_uid,
                    provider_data,
                    password,
                    created_at,
                } => {
                    let binding = ProviderBinding {
                        id: uuid::Uuid::new_v4().to_string(),
                        user_id: user_id.clone(),
                        provider: provider.clone(),
                        provider_uid: provider_uid.clone(),
                    };
                    match password {
                        Some((scheme, hash)) => Users::insert_password_binding_in(
                            tx,
                            &binding,
                            hash,
                            scheme,
                            *created_at,
                        )?,
                        None => Users::insert_provider_binding_in(
                            tx,
                            &binding,
                            provider_data.as_deref(),
                            *created_at,
                        )?,
                    }
                }
                Planned::InsertAppPassword {
                    user_id,
                    name,
                    secret_path,
                    created_at,
                    last_used_at,
                    last_client,
                    revoked,
                } => {
                    let plaintext = unsealed.get(secret_path).ok_or(ImportError::Rekey)?;
                    let row = AppPasswordRecord {
                        id: uuid::Uuid::new_v4().to_string(),
                        user_id: user_id.clone(),
                        name: name.clone(),
                        secret_sha256: sha_hex(plaintext),
                        secret_encrypted: crypto
                            .encrypt(plaintext)
                            .map_err(|_| ImportError::Rekey)?,
                        created_at: *created_at,
                        last_used_at: *last_used_at,
                        last_client: last_client.clone(),
                    };
                    AppPasswords::insert_in(tx, &row, *revoked)?;
                }
                Planned::InsertFollow(follow) => rows::insert_follow_in(tx, follow)?,
                Planned::MergeFollow {
                    user_id,
                    artist_mbid_lower,
                    auto_download,
                    followed_at,
                    updated_at,
                } => rows::merge_follow_in(
                    tx,
                    user_id,
                    artist_mbid_lower,
                    *auto_download,
                    *followed_at,
                    *updated_at,
                )?,
                Planned::InsertApproval {
                    user_id,
                    artist_mbid,
                    artist_name,
                    fields,
                } => rows::insert_approval_in(tx, user_id, artist_mbid, artist_name, fields)?,
                Planned::MergeApproval {
                    user_id,
                    artist_mbid_lower,
                    fields,
                } => rows::merge_approval_in(tx, user_id, artist_mbid_lower, fields)?,
                Planned::InsertEventCity {
                    user_id,
                    city,
                    position,
                } => insert_city_in(tx, user_id, city, *position)?,
                Planned::SetEventSeen { user_id, seen_at } => set_seen_in(tx, user_id, *seen_at)?,
            }
        }
        Ok(())
    }
}

/// Exit code of a run that completed.
fn completed_exit(report: &ReportBuilder) -> &'static str {
    if report.has_drops() {
        "OK_WITH_DROPS"
    } else {
        "OK"
    }
}

fn record_import_run(
    tx: &Connection,
    doc: &ExportDoc,
    report: &ReportBuilder,
    exit: &str,
) -> Result<String, ImportError> {
    // The row only persists when the commit succeeds; failures roll this
    // row back too. The carry and a post-commit config failure correct it
    // afterwards.
    let id = uuid::Uuid::new_v4().to_string();
    tx.execute(
        sql::INSERT_IMPORT_RUN.text(),
        params![
            id,
            doc.instance_id,
            doc.exported_at,
            exit,
            report.counts_json(),
            utc_now_iso(),
        ],
    )?;
    Ok(id)
}

/// Audit row for a run whose only writes were carried sections.
fn insert_import_run(
    conn: &mut Connection,
    doc: &ExportDoc,
    exit: &str,
    report: &ReportBuilder,
) -> Result<(), ImportError> {
    let tx = conn.transaction()?;
    record_import_run(&tx, doc, report, exit)?;
    tx.commit()?;
    Ok(())
}

/// Refresh the audit row with the counts of the carried sections, which
/// commit after the row was first written.
fn update_import_run(
    conn: &Connection,
    run_id: &str,
    exit: &str,
    report: &ReportBuilder,
) -> Result<(), ImportError> {
    conn.execute(
        sql::UPDATE_IMPORT_RUN.text(),
        params![exit, report.counts_json(), run_id],
    )?;
    Ok(())
}
