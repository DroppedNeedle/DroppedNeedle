//! v2 → v3 importer: parse → unlock → bundle check → settings → users →
//! secrets → follows/approvals → concerts cities and seen markers → settings
//! rows → atomic commit → section carry ([`super::carry`], one transaction
//! per section).
//!
//! Every run, including dry-run,
//! produces exactly one [`ImportReport`]; failures are exit codes, never
//! panics. Plaintext secrets exist only in memory and are never logged,
//! reported, or written to temp files.
//!
//! Conflict rules, deleted-ID rules, and idempotent
//! re-import all live here. The merge decisions are pure functions over
//! snapshots so dry-run and real import share them by construction.
//!
//! Deleted IDs fail closed: the validator refuses the whole file on a
//! dangling user reference, and the operator repair is dropping the orphan
//! rows from v2 and re-exporting. The `dropped_unknown_user` branches below
//! are defense in depth unreachable through [`run_import`]; `dropped_invalid`
//! fires only for unnamed app passwords, and the `error` counter is schema
//! reserved with no producer. Merges follow the export spec exactly: most-permissive state
//! wins (a pending import reopens a rejected row), `requested_at` takes the
//! min, and a nulled reviewer keeps its display name as audit residue.
//! Unknown settings sections are ignored with a validator warning and never
//! unsealed. The scan-schedule carry from v2's `config.json` refuses on
//! instance mismatch. The settings
//! rows are written in the same transaction as the records, so an import
//! lands whole or not at all. A row is compared with its stored form
//! decrypted, so an idempotent re-import writes no row.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use droppedneedle_kernel::runtime_config::ingest::read_legacy;
use droppedneedle_kernel::runtime_config::sql as settings_sql;
use droppedneedle_kernel::runtime_config::store::{decode_text, encode_text};
use droppedneedle_kernel::runtime_config::{ConfigError, LegacyConfig};

use serde_json::Value;
use sha2::{Digest, Sha256};
use sqlx::SqlitePool;
use thiserror::Error;

use super::carry::bundle::{Bundle, BundleRejection};
use super::envelope::{ExportFile, Opener, collect_sealed, is_sealed, unseal_value};
use super::report::{
    DROPPED_SETTING, ExitCode, ExportProvenance, ImportReport, ReportBuilder, utc_now_iso,
};
use super::schedule_carry::carry_frequency;
use super::validate::{REMOVED_SECTIONS, ValidationIssue, strip_removed, validate_export};
use droppedneedle_kernel::runtime_config::Crypto;

/// Sections the export may carry (kept whole or in part). Present
/// sections replace the v3 config wholesale; absent ones reset to v3
/// defaults. The settings keys outside this set are left untouched: the
/// export cannot speak for them. A test names those and holds both lists
/// to the kernel's section list.
/// Shared with the validator, which warns on anything outside it.
pub(crate) const REPLACE_UNIVERSE: &[&str] = &[
    "user_preferences",
    "library_scan_schedule",
    "library_scan_filesystem_watcher",
    "download_client",
    "download_clients",
    "wanted",
    "source_priority",
    "usenet_search_backend",
    "indexers",
    "prowlarr",
    "lidarr_import",
    "jellyfin_settings",
    "navidrome_settings",
    "plex_settings",
    "listenbrainz_settings",
    "youtube_settings",
    "lastfm_settings",
    "spotify_settings",
    "events",
    "wrapped_settings",
    "oidc_settings",
    "security_settings",
    "connect_apps",
    "library_settings",
    "library_management",
    "primary_music_source",
    "get_it",
    "advanced_settings",
    "download_policy",
    "musicbrainz_settings",
    "plugins",
    "_internal",
];

/// Transient `musicbrainz_settings` fields dropped at import.
const MUSICBRAINZ_TRANSIENT: &[&str] = &[
    "pending_brainzmash",
    "source_quarantined",
    "quarantine_reason",
];

/// Internal import failure. Messages stay secret-free: sqlx errors echo
/// SQL text, never bind values; crypto errors are fixed strings.
#[derive(Debug, Error)]
enum ImportError {
    /// A database read or write failed.
    #[error("database failure: {0}")]
    Db(#[from] sqlx::Error),
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

/// One import run: inputs and test-only fault switches.
pub struct ImportRequest {
    /// Raw export file bytes.
    pub export_bytes: Vec<u8>,
    /// Operator passphrase unlocking the secret envelope.
    pub passphrase: String,
    /// Target v3 database. Tests use scratch pools only.
    pub pool: SqlitePool,
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
    let validation = validate_export(&parsed.root);
    for warning in &validation.warnings {
        report.note(
            "export",
            warning.path.clone(),
            "warning",
            format!("{}: {}", warning.code, warning.message),
        );
    }
    if !validation.valid() {
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
    }

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
    if let Some(left_behind) = parsed
        .root
        .get("left_behind")
        .and_then(|list| serde_json::from_value(list.clone()).ok())
    {
        report.set_left_behind(left_behind);
    }
    if let Some(notes) = parsed
        .root
        .get("notes")
        .and_then(|list| serde_json::from_value(list.clone()).ok())
    {
        report.set_notes(notes);
    }

    // The bundle is checked whole before anything is written: a missing,
    // swapped or inconsistent bundle refuses the import with zero writes.
    let bundle = match prepare_bundle(&request, &parsed.root).await {
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
        &request,
        &parsed.root,
        &unsealed,
        bundle.as_ref(),
        &mut report,
    )
    .await;
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
async fn prepare_bundle(
    request: &ImportRequest,
    root: &Value,
) -> Result<Option<Bundle>, BundleRejection> {
    let Some(found) =
        super::carry::bundle::locate(root, request.attachments_dir.as_deref()).await?
    else {
        return Ok(None);
    };
    super::carry::check_bundle(&request.pool, &found, root).await?;
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

/// Every sealed value the importer consumes, with its path.
fn consumed_sealed(root: &Value) -> Vec<(String, Value)> {
    let mut found = Vec::new();
    if let Some(settings) = root.get("settings").and_then(Value::as_object) {
        // Known sections only: unknown ones are ignored with a warning, so a
        // broken blob there must not fail the import.
        for section in REPLACE_UNIVERSE {
            if let Some(block) = settings.get(*section) {
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
pub(crate) fn carries_connections(root: &Value) -> bool {
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
async fn run_guarded(
    request: &ImportRequest,
    root: &Value,
    unsealed: &HashMap<String, String>,
    bundle: Option<&Bundle>,
    report: &mut ReportBuilder,
) -> Result<(), ImportError> {
    let carry = |dry_run: bool| super::carry::CarryRun {
        pool: &request.pool,
        root,
        bundle,
        unsealed,
        crypto: &request.crypto,
        cache_dir: request.cache_dir.as_deref(),
        dry_run,
        stop_after: request.fault_after_sections,
    };
    let current_config = current_settings(&request.pool, &request.legacy).await?;
    let secrets_before = report.secrets_counted();
    let staged_config = stage_settings(
        root,
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
        let mut tx = request.pool.begin().await?;
        let snapshot = DatabaseSnapshot::load(&mut tx).await?;
        tx.rollback().await?;
        Plan::decide(root, &snapshot, unsealed, report);
        super::carry::run(carry(true), report).await?;
        return Ok(());
    }

    // The write lock comes first and the snapshot is read under it, so no
    // other writer can change a row between the plan and the apply. The
    // pool's connect options keep foreign keys on.
    let mut tx = request.pool.begin_with("BEGIN IMMEDIATE").await?;
    let snapshot = DatabaseSnapshot::load(&mut tx).await?;
    let plan = Plan::decide(root, &snapshot, unsealed, report);
    plan.apply(&mut tx, &request.crypto, unsealed).await?;
    let settings_written = write_settings(&mut tx, &staged_config, &request.crypto).await?;
    // A pure re-import (every record and setting identical) writes nothing
    // at all, not even an audit row: idempotency means zero writes.
    let run_id = if plan.has_writes() || settings_written > 0 {
        Some(record_import_run(&mut tx, root, report, completed_exit(report)).await?)
    } else {
        None
    };
    if request.fault_before_commit {
        tx.rollback().await?;
        return Err(ImportError::FaultBeforeCommit);
    }
    tx.commit().await?;
    report.mark_committed();
    if request.fault_after_commit {
        // No audit fix-up: a real crash could not run one either.
        return Err(ImportError::FaultAfterCommit);
    }

    let carried = super::carry::run(carry(false), report).await;
    // The audit row carries the whole run's real counts, the carried
    // sections included, and a failed run says so. A run whose only writes
    // were carried sections (a resume) records its own row.
    let exit = match &carried {
        Ok(_) => completed_exit(report),
        Err(_) => "FAILED_INTERNAL",
    };
    let audited = match run_id {
        Some(id) => update_import_run(&request.pool, &id, exit, report).await,
        None if report.carried_writes() > 0 => {
            insert_import_run(&request.pool, root, exit, report).await
        }
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

// --- settings staging ------------------------------------------------------

/// The target's settings as one object, key by key: its stored rows, or,
/// while it stores none and has recorded no copy of `config.json`, what a
/// first start would copy in from that file. A file that does not read
/// refuses the import before anything is written.
async fn current_settings(pool: &SqlitePool, legacy: &LegacyConfig) -> Result<Value, ImportError> {
    let settled: bool = sqlx::query_scalar(settings_sql::ANY_SETTINGS.text())
        .fetch_one(pool)
        .await?;
    let mut current = serde_json::Map::new();
    if settled {
        for (key, value) in stored_settings(pool).await? {
            current.insert(key, value);
        }
        return Ok(Value::Object(current));
    }
    let config_failed = |error: ConfigError| ImportError::Config {
        reason: error.to_string(),
    };
    if let Some(file) = read_legacy(legacy).map_err(config_failed)? {
        for (key, text) in file.rows {
            current.insert(key, decode_text(&text).map_err(config_failed)?);
        }
    }
    Ok(Value::Object(current))
}

/// Every stored settings row, decoded.
async fn stored_settings<'c, E>(executor: E) -> Result<Vec<(String, Value)>, ImportError>
where
    E: sqlx::SqliteExecutor<'c>,
{
    let rows: Vec<(String, String, i64)> = sqlx::query_as(settings_sql::ALL_SECTIONS.text())
        .fetch_all(executor)
        .await?;
    rows.into_iter()
        .map(|(key, text, _)| {
            decode_text(&text)
                .map(|value| (key, value))
                .map_err(|error| ImportError::Config {
                    reason: error.to_string(),
                })
        })
        .collect()
}

/// Write the staged settings as rows in the import's transaction: a row
/// whose plaintexts equal the stored row's is left alone, a changed one is
/// stored at its next revision, and a stored row the staged settings no
/// longer hold (a replaced section the export lacks) is deleted, so it
/// reads as its default. Returns how many rows changed.
async fn write_settings(
    tx: &mut sqlx::SqliteConnection,
    staged: &Value,
    crypto: &Crypto,
) -> Result<usize, ImportError> {
    let staged = staged.as_object().ok_or_else(|| ImportError::Config {
        reason: "staged settings are not a JSON object".to_owned(),
    })?;
    let stored: HashMap<String, Value> = stored_settings(&mut *tx).await?.into_iter().collect();
    let now = unix_millis();
    let mut changed = 0;
    for (key, value) in staged {
        if stored
            .get(key)
            .is_some_and(|current| configs_equivalent(current, value, crypto))
        {
            continue;
        }
        let text = encode_text(value).map_err(|error| ImportError::Config {
            reason: error.to_string(),
        })?;
        sqlx::query(settings_sql::PUT_SECTION.text())
            .bind(key)
            .bind(text)
            .bind(now)
            .fetch_one(&mut *tx)
            .await?;
        changed += 1;
    }
    // Only a replaced section is ever removed: rows outside the replace
    // list (the key's check value among them) are not the export's to drop.
    for key in stored
        .keys()
        .filter(|key| !staged.contains_key(*key) && REPLACE_UNIVERSE.contains(&key.as_str()))
    {
        sqlx::query(settings_sql::DELETE_SECTION.text())
            .bind(key)
            .execute(&mut *tx)
            .await?;
        changed += 1;
    }
    Ok(changed)
}

fn unix_millis() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |span| {
            i64::try_from(span.as_millis()).unwrap_or(i64::MAX)
        })
}

fn stage_settings(
    root: &Value,
    current: &Value,
    unsealed: &HashMap<String, String>,
    crypto: &Crypto,
    v2_config_path: Option<&std::path::Path>,
    report: &mut ReportBuilder,
) -> Result<Value, ImportError> {
    let mut staged = current.clone();
    let staged_object = staged.as_object_mut().ok_or_else(|| ImportError::Config {
        reason: "v3 settings must be a JSON object".to_owned(),
    })?;
    if let Some(instance_id) = root.get("instance_id").and_then(Value::as_str) {
        staged_object.insert(
            "instance_id".to_owned(),
            Value::String(instance_id.to_owned()),
        );
    }
    let empty = serde_json::Map::new();
    let exported = root
        .get("settings")
        .and_then(Value::as_object)
        .unwrap_or(&empty);

    for section in REMOVED_SECTIONS {
        if exported.contains_key(*section) {
            note_dropped_setting(report, (*section).to_owned());
        }
    }
    for section in REPLACE_UNIVERSE {
        match exported.get(*section) {
            Some(value) => {
                let mut filtered = filter_section(section, value);
                for path in strip_removed(section, &mut filtered) {
                    note_dropped_setting(report, path);
                }
                if *section == "plugins" {
                    note_unflagged_plugin_settings(&filtered, report);
                    note_plugins_need_porting(&filtered, report);
                }
                if *section == "library_management" {
                    carry_v2_management_fields(&mut filtered);
                }
                let applied = rekey_section(
                    &filtered,
                    &format!("settings.{section}"),
                    unsealed,
                    crypto,
                    report,
                )?;
                staged_object.insert((*section).to_owned(), applied);
                report.setting_applied((*section).to_owned());
            }
            None => {
                staged_object.remove(*section);
                report.setting_defaulted((*section).to_owned());
            }
        }
    }

    // The schedule carry fires whenever the export lacks the schedule: idempotent across
    // re-imports, and consistent with whole-config replace (the export,
    // plus the one-shot v2 carry, is the source of truth).
    let schedule_present = exported.contains_key("library_scan_schedule");
    let export_instance = root
        .get("instance_id")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if let Some(v2_path) = v2_config_path
        && let Some(frequency) = carry_frequency(v2_path, schedule_present, export_instance)?
    {
        let schedule = serde_json::json!({ "scan_frequency": frequency });
        staged_object.insert("library_scan_schedule".to_owned(), schedule);
        report.setting_reapplied("library_scan_schedule");
        report.note(
            "settings",
            "library_scan_schedule".to_owned(),
            "imported",
            "carried once from v2's library sync frequency".to_owned(),
        );
    }
    Ok(staged)
}

/// Note one v2 setting v3 removed: it is not imported.
fn note_dropped_setting(report: &mut ReportBuilder, path: String) {
    report.note(
        "settings",
        path,
        DROPPED_SETTING,
        "v3 removed this setting; it is not imported".to_owned(),
    );
}

/// Note each carried plugin: a v2 plugin cannot run in v3 until it is
/// ported, so its entry arrives switched off.
fn note_plugins_need_porting(plugins: &Value, report: &mut ReportBuilder) {
    let Some(plugins) = plugins.as_object() else {
        return;
    };
    for plugin in plugins.keys() {
        report.note(
            "settings",
            format!("plugins.{plugin}"),
            "needs_porting",
            format!("{plugin} needs porting before it can run"),
        );
    }
}

/// Note every plugin setting the exporter sealed without knowing whether it
/// is a secret (v2 had no manifest for the plugin). It lands encrypted,
/// and v3 opens it on read whichever way the plugin's manifest flags it.
fn note_unflagged_plugin_settings(plugins: &Value, report: &mut ReportBuilder) {
    use crate::export::exporter::{UNKNOWN_FLAG, UNKNOWN_FLAG_KEY};
    let Some(plugins) = plugins.as_object() else {
        return;
    };
    for (plugin, config) in plugins {
        let Some(settings) = config.get("settings").and_then(Value::as_object) else {
            continue;
        };
        for (key, value) in settings {
            if is_sealed(value)
                && value.get(UNKNOWN_FLAG_KEY).and_then(Value::as_str) == Some(UNKNOWN_FLAG)
            {
                report.note(
                    "settings",
                    format!("plugins.{plugin}.settings.{key}"),
                    "kept_sealed",
                    "v2 had no manifest saying whether this is a secret; it is stored \
                     encrypted and v3 reads it either way"
                        .to_owned(),
                );
            }
        }
    }
}

/// Carry each v2 profile's field entries as they are. A field it did not
/// list was off in v2 and stays off, except the Other role fields, which
/// v2 never had: completion sets those from the profile's Relationships.
/// `artists` and `album_artists` take
/// the mode and the preserve of `artist` and `album_artist`: v2's
/// `artist` wrote the ARTISTS list, and `album_artist` kept the album
/// artist list (on MP3 and M4A it now also goes to its own ALBUMARTISTS
/// tag). Preserved names v2 matched in any case take the field's
/// spelling. A field list that does not decode is left for the settings
/// reader to report.
fn carry_v2_management_fields(section: &mut Value) {
    use droppedneedle_kernel::runtime_config::sections::{FieldMode, ManagedField};
    use droppedneedle_library::management::enrich::other_role_field;
    use droppedneedle_library::management::settings::MANAGED_FIELDS;
    let Some(profiles) = section.get_mut("profiles").and_then(Value::as_array_mut) else {
        return;
    };
    for profile in profiles {
        let Some(metadata) = profile.get_mut("metadata").and_then(Value::as_object_mut) else {
            continue;
        };
        let Some(Ok(mut parsed)) = metadata
            .get("fields")
            .map(|fields| serde_json::from_value::<Vec<ManagedField>>(fields.clone()))
        else {
            continue;
        };
        let mut preserved: Vec<String> = metadata
            .get("preserve_fields")
            .and_then(Value::as_array)
            .map(|names| {
                names
                    .iter()
                    .filter_map(Value::as_str)
                    .map(|name| {
                        let folded = name.to_lowercase();
                        if MANAGED_FIELDS.iter().any(|spec| spec.name == folded) {
                            folded
                        } else {
                            name.to_owned()
                        }
                    })
                    .collect()
            })
            .unwrap_or_default();
        let listed =
            |parsed: &[ManagedField], name: &str| parsed.iter().any(|field| field.field == name);
        for (name, parent) in [("artists", "artist"), ("album_artists", "album_artist")] {
            if listed(&parsed, name) {
                continue;
            }
            if let Some(entry) = parsed.iter().find(|field| field.field == parent) {
                let mut copy = entry.clone();
                copy.field = name.to_owned();
                parsed.push(copy);
                if preserved.iter().any(|kept| kept == parent) {
                    preserved.push(name.to_owned());
                }
            }
        }
        // v2 never had the Other role fields: completion sets them.
        for spec in MANAGED_FIELDS {
            if !listed(&parsed, spec.name) && !other_role_field(spec.name) {
                parsed.push(ManagedField {
                    field: spec.name.to_owned(),
                    mode: FieldMode::Disabled,
                    clear_when_canonical_missing: false,
                });
            }
        }
        match serde_json::to_value(&parsed) {
            Ok(carried) => {
                metadata.insert("fields".to_owned(), carried);
                if metadata.contains_key("preserve_fields") {
                    metadata.insert("preserve_fields".to_owned(), Value::from(preserved));
                }
            }
            Err(error) => tracing::warn!(%error, "could not carry a v2 managed field list"),
        }
    }
}

/// Drop fields the export spec excludes, before re-encryption:
/// lastfm keeps the switch and the instance app key pair (v2's legacy
/// global session key and username go), the MusicBrainz transient trio
/// goes, plugins keep `settings` and arrive switched off (a v2 plugin
/// needs porting before v3 can run it). A configured Spotify app
/// gains the `legacy_callback` marker.
fn filter_section(name: &str, value: &Value) -> Value {
    match name {
        "lastfm_settings" => {
            let mut kept = serde_json::Map::new();
            for field in ["enabled", "api_key", "shared_secret"] {
                if let Some(kept_value) = value.get(field) {
                    kept.insert(field.to_owned(), kept_value.clone());
                }
            }
            Value::Object(kept)
        }
        // A Spotify app configured on v2 is registered with v2's callback
        // path. Mark it so v3 keeps sending that path and the app keeps
        // working with no dashboard change.
        "spotify_settings" => {
            let mut stamped = value.clone();
            let configured = value
                .get("client_id")
                .and_then(Value::as_str)
                .is_some_and(|id| !id.trim().is_empty());
            if configured && let Some(object) = stamped.as_object_mut() {
                object.insert("legacy_callback".to_owned(), Value::Bool(true));
            }
            stamped
        }
        "musicbrainz_settings" => {
            let mut cleaned = value.clone();
            if let Some(object) = cleaned.as_object_mut() {
                for field in MUSICBRAINZ_TRANSIENT {
                    object.remove(*field);
                }
            }
            cleaned
        }
        "plugins" => {
            let mut kept = serde_json::Map::new();
            if let Some(object) = value.as_object() {
                for (plugin, config) in object {
                    let mut entry = serde_json::Map::new();
                    entry.insert("enabled".to_owned(), Value::Bool(false));
                    if let Some(settings) = config.get("settings") {
                        entry.insert("settings".to_owned(), settings.clone());
                    }
                    kept.insert(plugin.clone(), Value::Object(entry));
                }
            }
            Value::Object(kept)
        }
        _ => value.clone(),
    }
}

/// Replace every sealed object in a staged section with v3 ciphertext,
/// counting each re-encryption. Any nested shape works: the walk finds
/// sealed leaves wherever the exporter placed them. Bare strings pass
/// through verbatim: plugin secret positions are manifest-defined at export
/// time, so a hand-edited export with plaintext plugin secrets lands as
/// plaintext, indistinguishable from plaintext plugin settings without the
/// v2 manifest.
fn rekey_section(
    section: &Value,
    path: &str,
    unsealed: &HashMap<String, String>,
    crypto: &Crypto,
    report: &mut ReportBuilder,
) -> Result<Value, ImportError> {
    if is_sealed(section) {
        let plaintext = unsealed.get(path).ok_or(ImportError::Rekey)?;
        let ciphertext = crypto.encrypt(plaintext).map_err(|_| ImportError::Rekey)?;
        report.secret_reencrypted();
        return Ok(Value::String(ciphertext));
    }
    match section {
        Value::Object(object) => {
            let mut out = serde_json::Map::with_capacity(object.len());
            for (key, child) in object {
                let child_path = format!("{path}.{key}");
                out.insert(
                    key.clone(),
                    rekey_section(child, &child_path, unsealed, crypto, report)?,
                );
            }
            Ok(Value::Object(out))
        }
        Value::Array(items) => {
            let mut out = Vec::with_capacity(items.len());
            for (index, child) in items.iter().enumerate() {
                out.push(rekey_section(
                    child,
                    &format!("{path}[{index}]"),
                    unsealed,
                    crypto,
                    report,
                )?);
            }
            Ok(Value::Array(out))
        }
        _ => Ok(section.clone()),
    }
}

// --- database snapshot -----------------------------------------------------

#[derive(Debug, Clone, sqlx::FromRow)]
struct UserRow {
    id: String,
    display_name: String,
    email: Option<String>,
    avatar_url: Option<String>,
    role: String,
    created_at: String,
    last_login_at: Option<String>,
    username: Option<String>,
    username_display: Option<String>,
}

#[derive(Debug, Clone, sqlx::FromRow)]
struct ProviderRow {
    user_id: String,
    provider: String,
    provider_uid: String,
    provider_data: Option<String>,
    created_at: String,
}

#[derive(Debug, Clone, sqlx::FromRow)]
struct AppPasswordRow {
    user_id: String,
    name: String,
    secret_sha256: String,
    created_at: String,
    last_used_at: Option<String>,
    last_client: Option<String>,
    revoked: i64,
}

#[derive(Debug, Clone, sqlx::FromRow)]
struct RecoveryRow {
    user_id: String,
    code_hash: String,
    created_at: String,
    expires_at: String,
}

#[derive(Debug, Clone, sqlx::FromRow)]
struct FollowRow {
    user_id: String,
    artist_mbid_lower: String,
    auto_download: i64,
    followed_at: f64,
    updated_at: f64,
}

#[derive(Debug, Clone, sqlx::FromRow)]
struct EventCityRow {
    user_id: String,
    city_name: String,
    country_code: Option<String>,
    latitude: f64,
    longitude: f64,
    radius_km: f64,
    position: i64,
}

/// Concerts city key: the table's (user, latitude, longitude) primary key.
type CityKey = (String, u64, u64);

/// A coordinate as a key part. SQLite compares REAL keys by value, so
/// -0.0 folds into 0.0 here as it does there.
fn coord_bits(value: f64) -> u64 {
    (value + 0.0).to_bits()
}

#[derive(Debug, Clone, sqlx::FromRow)]
struct ApprovalRow {
    user_id: String,
    artist_mbid_lower: String,
    state: String,
    requested_at: i64,
    reviewed_by_id: Option<String>,
    reviewed_by_name: Option<String>,
    reviewed_at: Option<i64>,
    batch_id: Option<String>,
    source: Option<String>,
}

/// An approval time from the export (REAL epoch seconds) as the table
/// stores it: integer milliseconds.
fn approval_ms(seconds: f64) -> i64 {
    (seconds * 1000.0).round() as i64
}

/// Read-only view of the target database the merge decisions run against.
struct DatabaseSnapshot {
    users: HashMap<String, UserRow>,
    emails: HashMap<String, String>,
    usernames: HashMap<String, String>,
    providers: HashMap<(String, String), ProviderRow>,
    app_passwords: HashMap<(String, String), Vec<AppPasswordRow>>,
    app_password_shas: HashSet<String>,
    recoveries: HashMap<String, RecoveryRow>,
    follows: HashMap<(String, String), FollowRow>,
    approvals: HashMap<(String, String), ApprovalRow>,
    event_cities: HashMap<CityKey, EventCityRow>,
    event_seen: HashMap<String, f64>,
}

impl DatabaseSnapshot {
    async fn load(conn: &mut sqlx::SqliteConnection) -> Result<Self, ImportError> {
        let users: Vec<UserRow> = sqlx::query_as(
            "SELECT id, display_name, email, avatar_url, role, created_at, \
             last_login_at, username, username_display FROM auth_users",
        )
        .fetch_all(&mut *conn)
        .await?;
        let providers: Vec<ProviderRow> = sqlx::query_as(
            "SELECT user_id, provider, provider_uid, provider_data, created_at \
             FROM auth_providers",
        )
        .fetch_all(&mut *conn)
        .await?;
        let app_passwords: Vec<AppPasswordRow> = sqlx::query_as(
            "SELECT user_id, name, secret_sha256, created_at, last_used_at, \
             last_client, revoked FROM connect_app_passwords",
        )
        .fetch_all(&mut *conn)
        .await?;
        let recoveries: Vec<RecoveryRow> = sqlx::query_as(
            "SELECT user_id, code_hash, created_at, expires_at \
             FROM auth_password_recovery_codes",
        )
        .fetch_all(&mut *conn)
        .await?;
        let follows: Vec<FollowRow> = sqlx::query_as(
            "SELECT user_id, artist_mbid_lower, auto_download, followed_at, \
             updated_at FROM user_followed_artists",
        )
        .fetch_all(&mut *conn)
        .await?;
        let approvals: Vec<ApprovalRow> = sqlx::query_as(
            "SELECT user_id, artist_mbid_lower, state, requested_at, \
             reviewed_by_id, reviewed_by_name, reviewed_at, batch_id, source \
             FROM auto_download_approvals",
        )
        .fetch_all(&mut *conn)
        .await?;
        let event_cities: Vec<EventCityRow> = sqlx::query_as(
            "SELECT user_id, city_name, country_code, latitude, longitude, radius_km, \
             position FROM user_event_cities",
        )
        .fetch_all(&mut *conn)
        .await?;
        let event_seen: Vec<(String, f64)> =
            sqlx::query_as("SELECT user_id, seen_at FROM user_event_seen")
                .fetch_all(&mut *conn)
                .await?;

        let mut snapshot = Self {
            users: HashMap::new(),
            emails: HashMap::new(),
            usernames: HashMap::new(),
            providers: HashMap::new(),
            app_passwords: HashMap::new(),
            app_password_shas: HashSet::new(),
            recoveries: HashMap::new(),
            follows: HashMap::new(),
            approvals: HashMap::new(),
            event_cities: HashMap::new(),
            event_seen: event_seen.into_iter().collect(),
        };
        for city in event_cities {
            snapshot.event_cities.insert(
                (
                    city.user_id.clone(),
                    coord_bits(city.latitude),
                    coord_bits(city.longitude),
                ),
                city,
            );
        }
        for user in users {
            if let Some(email) = user.email.clone() {
                snapshot.emails.insert(email, user.id.clone());
            }
            if let Some(username) = user.username.clone() {
                snapshot.usernames.insert(username, user.id.clone());
            }
            snapshot.users.insert(user.id.clone(), user);
        }
        for provider in providers {
            snapshot.providers.insert(
                (provider.provider.clone(), provider.provider_uid.clone()),
                provider,
            );
        }
        for password in app_passwords {
            snapshot
                .app_password_shas
                .insert(password.secret_sha256.clone());
            snapshot
                .app_passwords
                .entry((password.user_id.clone(), password.name.clone()))
                .or_default()
                .push(password);
        }
        for recovery in recoveries {
            snapshot
                .recoveries
                .insert(recovery.user_id.clone(), recovery);
        }
        for follow in follows {
            snapshot.follows.insert(
                (follow.user_id.clone(), follow.artist_mbid_lower.clone()),
                follow,
            );
        }
        for approval in approvals {
            snapshot.approvals.insert(
                (approval.user_id.clone(), approval.artist_mbid_lower.clone()),
                approval,
            );
        }
        Ok(snapshot)
    }
}

// --- merge plan --------------------------------------------------------------

/// One decided record: what to write (real runs) and what to report.
enum Planned {
    InsertUser {
        record: Value,
        email_nulled: bool,
        username_nulled: bool,
    },
    InsertProvider {
        record: Value,
        user_id: String,
    },
    InsertAppPassword {
        record: Value,
        user_id: String,
        name: String,
        secret_path: String,
    },
    InsertRecovery {
        record: Value,
        user_id: String,
    },
    InsertFollow {
        record: Value,
    },
    MergeFollow {
        key: (String, String),
        auto_download: bool,
        followed_at: f64,
        updated_at: f64,
    },
    InsertApproval {
        record: Value,
        reviewer_nulled: bool,
    },
    MergeApproval {
        key: (String, String),
        state: String,
        requested_at: i64,
        reviewed_by_id: Option<String>,
        reviewed_by_name: Option<String>,
        reviewed_at: Option<i64>,
        batch_id: Option<String>,
        source: Option<String>,
    },
    InsertEventCity {
        record: Value,
    },
    SetEventSeen {
        user_id: String,
        seen_at: f64,
    },
}

/// The decided import. A dry run decides it for the report and applies
/// nothing.
struct Plan {
    writes: Vec<Planned>,
}

impl Plan {
    /// True when the plan carries at least one database write.
    fn has_writes(&self) -> bool {
        !self.writes.is_empty()
    }

    /// Decide every record against the snapshot, recording outcomes.
    /// Pure apart from the report: dry-run and real import share it.
    fn decide(
        root: &Value,
        snapshot: &DatabaseSnapshot,
        unsealed: &HashMap<String, String>,
        report: &mut ReportBuilder,
    ) -> Self {
        let mut plan = Self { writes: Vec::new() };
        let empty_list = Vec::new();
        let users = root
            .get("users")
            .and_then(Value::as_array)
            .unwrap_or(&empty_list);
        let mut known_users: HashSet<String> = snapshot.users.keys().cloned().collect();
        let mut batch_emails: HashMap<String, String> = HashMap::new();
        let mut batch_usernames: HashMap<String, String> = HashMap::new();
        let mut batch_shas: HashSet<String> = HashSet::new();

        for user in users {
            plan.decide_user(
                user,
                snapshot,
                &mut batch_emails,
                &mut batch_usernames,
                report,
            );
            if let Some(id) = user.get("id").and_then(Value::as_str) {
                known_users.insert(id.to_owned());
            }
        }
        for (user_index, user) in users.iter().enumerate() {
            plan.decide_user_children(
                user,
                user_index,
                snapshot,
                unsealed,
                &mut batch_shas,
                report,
            );
        }
        let follows = root
            .get("follows")
            .and_then(Value::as_array)
            .unwrap_or(&empty_list);
        for follow in follows {
            plan.decide_follow(follow, snapshot, &known_users, report);
        }
        let approvals = root
            .get("approvals")
            .and_then(Value::as_array)
            .unwrap_or(&empty_list);
        for approval in approvals {
            plan.decide_approval(approval, snapshot, &known_users, report);
        }
        let event_cities = root
            .get("event_cities")
            .and_then(Value::as_array)
            .unwrap_or(&empty_list);
        for city in event_cities {
            plan.decide_event_city(city, snapshot, &known_users, report);
        }
        let event_seen = root
            .get("event_seen")
            .and_then(Value::as_array)
            .unwrap_or(&empty_list);
        for seen in event_seen {
            plan.decide_event_seen(seen, snapshot, &known_users, report);
        }
        plan
    }

    fn decide_user(
        &mut self,
        user: &Value,
        snapshot: &DatabaseSnapshot,
        batch_emails: &mut HashMap<String, String>,
        batch_usernames: &mut HashMap<String, String>,
        report: &mut ReportBuilder,
    ) {
        let id = user.get("id").and_then(Value::as_str).unwrap_or_default();
        if id.is_empty() {
            report.record(
                "user",
                String::new(),
                "dropped_invalid",
                "user record without an id".to_owned(),
            );
            return;
        }
        if let Some(existing) = snapshot.users.get(id) {
            if users_identical(existing, user) {
                report.record("user", id.to_owned(), "skipped_identical", String::new());
            } else {
                report.record(
                    "user",
                    id.to_owned(),
                    "conflict_kept_existing",
                    "same id, different fields; existing row kept".to_owned(),
                );
            }
            return;
        }
        let mut email_nulled = false;
        let mut username_nulled = false;
        if let Some(email) = opt_str(user, "email") {
            let taken_elsewhere = snapshot.emails.get(&email).is_some_and(|owner| owner != id)
                || batch_emails.get(&email).is_some_and(|owner| owner != id);
            if taken_elsewhere {
                email_nulled = true;
                report.record(
                    "user",
                    id.to_owned(),
                    "nulled_field",
                    "email collides across ids; imported nulled".to_owned(),
                );
            } else {
                batch_emails.insert(email, id.to_owned());
            }
        }
        if let Some(username) = opt_str(user, "username") {
            let taken_elsewhere = snapshot
                .usernames
                .get(&username)
                .is_some_and(|owner| owner != id)
                || batch_usernames
                    .get(&username)
                    .is_some_and(|owner| owner != id);
            if taken_elsewhere {
                username_nulled = true;
                report.record(
                    "user",
                    id.to_owned(),
                    "nulled_field",
                    "username collides across ids; imported nulled".to_owned(),
                );
            } else {
                batch_usernames.insert(username, id.to_owned());
            }
        }
        report.record("user", id.to_owned(), "imported", String::new());
        self.writes.push(Planned::InsertUser {
            record: user.clone(),
            email_nulled,
            username_nulled,
        });
    }

    fn decide_user_children(
        &mut self,
        user: &Value,
        user_index: usize,
        snapshot: &DatabaseSnapshot,
        unsealed: &HashMap<String, String>,
        batch_shas: &mut HashSet<String>,
        report: &mut ReportBuilder,
    ) {
        let user_id = user.get("id").and_then(Value::as_str).unwrap_or_default();
        if user_id.is_empty() {
            return;
        }
        let empty_list = Vec::new();
        let providers = user
            .get("providers")
            .and_then(Value::as_array)
            .unwrap_or(&empty_list);
        for provider in providers {
            self.decide_provider(provider, user_id, snapshot, report);
        }
        let passwords = user
            .get("app_passwords")
            .and_then(Value::as_array)
            .unwrap_or(&empty_list);
        for (index, password) in passwords.iter().enumerate() {
            let secret_path = format!("users[{user_index}].app_passwords[{index}].secret");
            self.decide_app_password(
                password,
                user_id,
                &secret_path,
                snapshot,
                unsealed,
                batch_shas,
                report,
            );
        }
        if let Some(recovery) = user.get("recovery_code")
            && !recovery.is_null()
        {
            self.decide_recovery(recovery, user_id, snapshot, report);
        }
    }

    fn decide_provider(
        &mut self,
        provider: &Value,
        user_id: &str,
        snapshot: &DatabaseSnapshot,
        report: &mut ReportBuilder,
    ) {
        let name = provider
            .get("provider")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let uid = provider
            .get("provider_uid")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let key = format!("{name}|{uid}");
        if name.is_empty() || uid.is_empty() {
            report.record(
                "provider",
                key,
                "dropped_invalid",
                "provider record without a binding".to_owned(),
            );
            return;
        }
        match snapshot.providers.get(&(name.to_owned(), uid.to_owned())) {
            None => {
                report.record("provider", key, "imported", String::new());
                self.writes.push(Planned::InsertProvider {
                    record: provider.clone(),
                    user_id: user_id.to_owned(),
                });
            }
            Some(existing) => {
                if existing.user_id == user_id
                    && existing.provider_data == opt_str(provider, "provider_data")
                    && existing.created_at == str_or(provider, "created_at", "")
                {
                    report.record("provider", key, "skipped_identical", String::new());
                } else {
                    report.record(
                        "provider",
                        key,
                        "conflict_kept_existing",
                        "provider binding already mapped; existing row kept".to_owned(),
                    );
                }
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn decide_app_password(
        &mut self,
        password: &Value,
        user_id: &str,
        secret_path: &str,
        snapshot: &DatabaseSnapshot,
        unsealed: &HashMap<String, String>,
        batch_shas: &mut HashSet<String>,
        report: &mut ReportBuilder,
    ) {
        let name = str_or(password, "name", "");
        if name.is_empty() {
            report.record(
                "app_password",
                format!("{user_id}|"),
                "dropped_invalid",
                "app password without a name".to_owned(),
            );
            return;
        }
        let created_at = str_or(password, "created_at", "");
        let key = format!("{user_id}|{name}|{created_at}");
        let secret_sha = unsealed
            .get(secret_path)
            .map(|plaintext| sha_hex(plaintext));
        let rows = snapshot
            .app_passwords
            .get(&(user_id.to_owned(), name.clone()));
        // The hash column is UNIQUE across all users: a reused secret
        // cannot insert twice, so the first registration wins.
        let sha_taken = secret_sha.as_ref().is_some_and(|sha| {
            let same_row = rows.is_some_and(|existing| {
                existing
                    .iter()
                    .any(|row| row.secret_sha256 == *sha && row.created_at == created_at)
            });
            !same_row && (snapshot.app_password_shas.contains(sha) || batch_shas.contains(sha))
        });
        if sha_taken {
            report.record(
                "app_password",
                key,
                "conflict_kept_existing",
                "secret hash already registered under another name; existing kept".to_owned(),
            );
            return;
        }
        if let Some(sha) = secret_sha.clone() {
            batch_shas.insert(sha);
        }
        match rows {
            None => {
                report.record("app_password", key, "imported", String::new());
                report.secret_reencrypted();
                self.writes.push(Planned::InsertAppPassword {
                    record: password.clone(),
                    user_id: user_id.to_owned(),
                    name,
                    secret_path: secret_path.to_owned(),
                });
            }
            Some(existing) => {
                let identical = existing.iter().any(|row| {
                    row.created_at == created_at
                        && secret_sha
                            .as_ref()
                            .is_some_and(|sha| *sha == row.secret_sha256)
                        && row.last_used_at == opt_str(password, "last_used_at")
                        && row.last_client == opt_str(password, "last_client")
                        && row.revoked == i64::from(bool_or(password, "revoked", false))
                });
                if identical {
                    report.record("app_password", key, "skipped_identical", String::new());
                    return;
                }
                if existing.iter().any(|row| row.created_at == created_at) {
                    report.record(
                        "app_password",
                        key,
                        "conflict_kept_existing",
                        "same name and timestamp with a different secret; existing kept".to_owned(),
                    );
                    return;
                }
                let mut candidate = format!("{name} ({created_at})");
                let mut suffix = 2;
                while snapshot
                    .app_passwords
                    .contains_key(&(user_id.to_owned(), candidate.clone()))
                {
                    candidate = format!("{name} ({created_at}) #{suffix}");
                    suffix += 1;
                }
                report.record(
                    "app_password",
                    key,
                    "conflict_kept_existing",
                    format!("name collides; imported as {candidate:?}"),
                );
                report.secret_reencrypted();
                self.writes.push(Planned::InsertAppPassword {
                    record: password.clone(),
                    user_id: user_id.to_owned(),
                    name: candidate,
                    secret_path: secret_path.to_owned(),
                });
            }
        }
    }

    fn decide_recovery(
        &mut self,
        recovery: &Value,
        user_id: &str,
        snapshot: &DatabaseSnapshot,
        report: &mut ReportBuilder,
    ) {
        match snapshot.recoveries.get(user_id) {
            None => {
                report.record(
                    "recovery_code",
                    user_id.to_owned(),
                    "imported",
                    String::new(),
                );
                self.writes.push(Planned::InsertRecovery {
                    record: recovery.clone(),
                    user_id: user_id.to_owned(),
                });
            }
            Some(existing) => {
                if existing.code_hash == str_or(recovery, "code_hash", "")
                    && existing.created_at == str_or(recovery, "created_at", "")
                    && existing.expires_at == str_or(recovery, "expires_at", "")
                {
                    report.record(
                        "recovery_code",
                        user_id.to_owned(),
                        "skipped_identical",
                        String::new(),
                    );
                } else {
                    report.record(
                        "recovery_code",
                        user_id.to_owned(),
                        "conflict_kept_existing",
                        "recovery code already set; existing kept".to_owned(),
                    );
                }
            }
        }
    }

    fn decide_follow(
        &mut self,
        follow: &Value,
        snapshot: &DatabaseSnapshot,
        known_users: &HashSet<String>,
        report: &mut ReportBuilder,
    ) {
        let user_id = follow
            .get("user_id")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let mbid_lower = follow
            .get("artist_mbid")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .trim()
            .to_lowercase();
        let key = format!("{user_id}|{mbid_lower}");
        if !known_users.contains(user_id) {
            report.record(
                "follow",
                key,
                "dropped_unknown_user",
                "user is not imported; never invented".to_owned(),
            );
            return;
        }
        if mbid_lower.is_empty() {
            report.record(
                "follow",
                key,
                "dropped_invalid",
                "follow without an MBID".to_owned(),
            );
            return;
        }
        let lookup = (user_id.to_owned(), mbid_lower);
        match snapshot.follows.get(&lookup) {
            None => {
                report.record("follow", key, "imported", String::new());
                self.writes.push(Planned::InsertFollow {
                    record: follow.clone(),
                });
            }
            Some(existing) => {
                let merged_auto =
                    existing.auto_download != 0 || bool_or(follow, "auto_download", false);
                let merged_followed = existing.followed_at.min(num_or(follow, "followed_at", 0.0));
                let merged_updated = existing.updated_at.max(num_or(follow, "updated_at", 0.0));
                if existing.auto_download == i64::from(merged_auto)
                    && existing.followed_at == merged_followed
                    && existing.updated_at == merged_updated
                {
                    report.record("follow", key, "skipped_identical", String::new());
                } else {
                    report.record(
                        "follow",
                        key,
                        "conflict_kept_existing",
                        "merged: auto_download OR, followed_at min, updated_at max".to_owned(),
                    );
                    self.writes.push(Planned::MergeFollow {
                        key: lookup,
                        auto_download: merged_auto,
                        followed_at: merged_followed,
                        updated_at: merged_updated,
                    });
                }
            }
        }
    }

    /// A city the user already has (same coordinates) keeps its saved
    /// name, radius and order; a new one is added.
    fn decide_event_city(
        &mut self,
        city: &Value,
        snapshot: &DatabaseSnapshot,
        known_users: &HashSet<String>,
        report: &mut ReportBuilder,
    ) {
        let user_id = str_or(city, "user_id", "");
        let latitude = num_or(city, "latitude", 0.0);
        let longitude = num_or(city, "longitude", 0.0);
        let key = format!("{user_id}|{latitude},{longitude}");
        if !known_users.contains(&user_id) {
            report.record(
                "event_city",
                key,
                "dropped_unknown_user",
                "user is not imported; never invented".to_owned(),
            );
            return;
        }
        match snapshot
            .event_cities
            .get(&(user_id, coord_bits(latitude), coord_bits(longitude)))
        {
            None => {
                report.record("event_city", key, "imported", String::new());
                self.writes.push(Planned::InsertEventCity {
                    record: city.clone(),
                });
            }
            Some(existing) => {
                let identical = existing.city_name == str_or(city, "city_name", "")
                    && existing.country_code == opt_str(city, "country_code")
                    && existing.radius_km == num_or(city, "radius_km", 0.0)
                    && existing.position
                        == city.get("position").and_then(Value::as_i64).unwrap_or(0);
                if identical {
                    report.record("event_city", key, "skipped_identical", String::new());
                } else {
                    report.record(
                        "event_city",
                        key,
                        "conflict_kept_existing",
                        "same coordinates already saved; existing row kept".to_owned(),
                    );
                }
            }
        }
    }

    /// Seen markers merge to the later time, so the badge never counts
    /// gigs the user already saw on either side.
    fn decide_event_seen(
        &mut self,
        seen: &Value,
        snapshot: &DatabaseSnapshot,
        known_users: &HashSet<String>,
        report: &mut ReportBuilder,
    ) {
        let user_id = str_or(seen, "user_id", "");
        if !known_users.contains(&user_id) {
            report.record(
                "event_seen",
                user_id,
                "dropped_unknown_user",
                "user is not imported; never invented".to_owned(),
            );
            return;
        }
        let imported = num_or(seen, "seen_at", 0.0);
        match snapshot.event_seen.get(&user_id) {
            None => {
                report.record("event_seen", user_id.clone(), "imported", String::new());
                self.writes.push(Planned::SetEventSeen {
                    user_id,
                    seen_at: imported,
                });
            }
            Some(&existing) if existing >= imported => {
                report.record("event_seen", user_id, "skipped_identical", String::new());
            }
            Some(_) => {
                report.record(
                    "event_seen",
                    user_id.clone(),
                    "conflict_kept_existing",
                    "merged: seen_at max".to_owned(),
                );
                self.writes.push(Planned::SetEventSeen {
                    user_id,
                    seen_at: imported,
                });
            }
        }
    }

    fn decide_approval(
        &mut self,
        approval: &Value,
        snapshot: &DatabaseSnapshot,
        known_users: &HashSet<String>,
        report: &mut ReportBuilder,
    ) {
        let user_id = approval
            .get("user_id")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let mbid_lower = approval
            .get("artist_mbid")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .trim()
            .to_lowercase();
        let key = format!("{user_id}|{mbid_lower}");
        if !known_users.contains(user_id) {
            report.record(
                "approval",
                key,
                "dropped_unknown_user",
                "user is not imported; never invented".to_owned(),
            );
            return;
        }
        if mbid_lower.is_empty() {
            report.record(
                "approval",
                key,
                "dropped_invalid",
                "approval without an MBID".to_owned(),
            );
            return;
        }
        let reviewer = opt_str(approval, "reviewed_by_id");
        // Only the id is nulled; the display name stays verbatim as audit
        // residue (pinned by a test).
        let reviewer_nulled = reviewer
            .as_ref()
            .is_some_and(|id| !known_users.contains(id));
        if reviewer_nulled {
            report.record(
                "approval",
                key.clone(),
                "nulled_field",
                "reviewer is not an imported user; reviewed_by_id nulled".to_owned(),
            );
        }
        let lookup = (user_id.to_owned(), mbid_lower);
        match snapshot.approvals.get(&lookup) {
            None => {
                report.record("approval", key, "imported", String::new());
                self.writes.push(Planned::InsertApproval {
                    record: approval.clone(),
                    reviewer_nulled,
                });
            }
            Some(existing) => {
                let incoming_state = str_or(approval, "state", "pending");
                let winner_incoming =
                    permissiveness(&incoming_state) > permissiveness(&existing.state);
                let (state, batch_id, source, by_id, by_name, at) = if winner_incoming {
                    (
                        incoming_state,
                        opt_str(approval, "batch_id"),
                        opt_str(approval, "source"),
                        opt_str(approval, "reviewed_by_id").filter(|_| !reviewer_nulled),
                        opt_str(approval, "reviewed_by_name"),
                        opt_num(approval, "reviewed_at").map(approval_ms),
                    )
                } else {
                    (
                        existing.state.clone(),
                        existing.batch_id.clone(),
                        existing.source.clone(),
                        existing.reviewed_by_id.clone(),
                        existing.reviewed_by_name.clone(),
                        existing.reviewed_at,
                    )
                };
                // Earliest request wins. The spec does not name this field; min
                // keeps re-imports convergent.
                let requested_at =
                    existing
                        .requested_at
                        .min(approval_ms(num_or(approval, "requested_at", 0.0)));
                if existing.state == state
                    && existing.batch_id == batch_id
                    && existing.source == source
                    && existing.reviewed_by_id == by_id
                    && existing.reviewed_by_name == by_name
                    && existing.reviewed_at == at
                    && existing.requested_at == requested_at
                {
                    report.record("approval", key, "skipped_identical", String::new());
                } else {
                    report.record(
                        "approval",
                        key,
                        "conflict_kept_existing",
                        "merged: most-permissive state wins".to_owned(),
                    );
                    self.writes.push(Planned::MergeApproval {
                        key: lookup,
                        state,
                        requested_at,
                        reviewed_by_id: by_id,
                        reviewed_by_name: by_name,
                        reviewed_at: at,
                        batch_id,
                        source,
                    });
                }
            }
        }
    }

    /// Execute the decided writes inside the caller's transaction.
    async fn apply(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
        crypto: &Crypto,
        unsealed: &HashMap<String, String>,
    ) -> Result<(), ImportError> {
        for write in &self.writes {
            match write {
                Planned::InsertUser {
                    record,
                    email_nulled,
                    username_nulled,
                } => insert_user(tx, record, *email_nulled, *username_nulled).await?,
                Planned::InsertProvider { record, user_id } => {
                    insert_provider(tx, record, user_id).await?;
                }
                Planned::InsertAppPassword {
                    record,
                    user_id,
                    name,
                    secret_path,
                } => {
                    let plaintext = unsealed.get(secret_path).ok_or(ImportError::Rekey)?;
                    insert_app_password(tx, crypto, record, user_id, name, plaintext).await?;
                }
                Planned::InsertRecovery { record, user_id } => {
                    insert_recovery(tx, record, user_id).await?;
                }
                Planned::InsertFollow { record } => insert_follow(tx, record).await?,
                Planned::MergeFollow {
                    key,
                    auto_download,
                    followed_at,
                    updated_at,
                } => {
                    merge_follow(tx, key, *auto_download, *followed_at, *updated_at).await?;
                }
                Planned::InsertApproval {
                    record,
                    reviewer_nulled,
                } => insert_approval(tx, record, *reviewer_nulled).await?,
                Planned::MergeApproval {
                    key,
                    state,
                    requested_at,
                    reviewed_by_id,
                    reviewed_by_name,
                    reviewed_at,
                    batch_id,
                    source,
                } => {
                    merge_approval(
                        tx,
                        key,
                        state,
                        *requested_at,
                        reviewed_by_id.clone(),
                        reviewed_by_name.clone(),
                        *reviewed_at,
                        batch_id.clone(),
                        source.clone(),
                    )
                    .await?;
                }
                Planned::InsertEventCity { record } => insert_event_city(tx, record).await?,
                Planned::SetEventSeen { user_id, seen_at } => {
                    set_event_seen(tx, user_id, *seen_at).await?;
                }
            }
        }
        Ok(())
    }
}

/// Approval-state permissiveness, as the export spec defines it: approved beats pending beats
/// the rest. A pending import therefore reopens a rejected row, and unknown
/// states never win a merge. Pinned by a test, not just convergent.
fn permissiveness(state: &str) -> u8 {
    match state {
        "approved" => 2,
        "pending" => 1,
        _ => 0,
    }
}

/// v2 served uploaded avatars at `/api/v1/profile/avatar/{id}`; v3 serves
/// the same file at `/api/v3/users/{id}/avatar`. The cache-busting query
/// is kept. Any other URL (a Plex picture, say) passes through.
fn v3_avatar_url(user_id: &str, raw: Option<String>) -> Option<String> {
    let raw = raw?;
    let Some(rest) = raw.strip_prefix(&format!("/api/v1/profile/avatar/{user_id}")) else {
        return Some(raw);
    };
    let version = rest
        .strip_prefix("?v=")
        .filter(|version| !version.is_empty())
        .unwrap_or("0");
    Some(droppedneedle_auth::users::services::avatar_url(
        user_id, version,
    ))
}

fn users_identical(existing: &UserRow, imported: &Value) -> bool {
    existing.display_name == str_or(imported, "display_name", "")
        && existing.email == opt_str(imported, "email")
        && existing.avatar_url == v3_avatar_url(&existing.id, opt_str(imported, "avatar_url"))
        && existing.role == str_or(imported, "role", "user")
        && existing.username == opt_str(imported, "username")
        && existing.username_display == opt_str(imported, "username_display")
        && existing.created_at == str_or(imported, "created_at", "")
        && existing.last_login_at == opt_str(imported, "last_login_at")
}

fn str_or(record: &Value, field: &str, fallback: &str) -> String {
    record
        .get(field)
        .and_then(Value::as_str)
        .unwrap_or(fallback)
        .to_owned()
}

fn opt_str(record: &Value, field: &str) -> Option<String> {
    record.get(field).and_then(Value::as_str).map(str::to_owned)
}

fn num_or(record: &Value, field: &str, fallback: f64) -> f64 {
    record
        .get(field)
        .and_then(Value::as_f64)
        .unwrap_or(fallback)
}

fn opt_num(record: &Value, field: &str) -> Option<f64> {
    record.get(field).and_then(Value::as_f64)
}

fn bool_or(record: &Value, field: &str, fallback: bool) -> bool {
    record
        .get(field)
        .and_then(Value::as_bool)
        .unwrap_or(fallback)
}

// --- write execution ---------------------------------------------------------

/// Lowercase hex SHA-256 of an app-password secret, matching the
/// `secret_sha256` column the compat clients authenticate against.
fn sha_hex(plaintext: &str) -> String {
    let digest = Sha256::digest(plaintext.as_bytes());
    format!("{digest:x}")
}

async fn insert_user(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    record: &Value,
    email_nulled: bool,
    username_nulled: bool,
) -> Result<(), ImportError> {
    let email = (!email_nulled).then(|| opt_str(record, "email")).flatten();
    let username = (!username_nulled)
        .then(|| opt_str(record, "username"))
        .flatten();
    let id = str_or(record, "id", "");
    let avatar_url = v3_avatar_url(&id, opt_str(record, "avatar_url"));
    sqlx::query(
        "INSERT INTO auth_users (id, display_name, email, avatar_url, role, \
         created_at, last_login_at, username, username_display) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&id)
    .bind(str_or(record, "display_name", ""))
    .bind(email)
    .bind(avatar_url)
    .bind(str_or(record, "role", "user"))
    .bind(str_or(record, "created_at", ""))
    .bind(opt_str(record, "last_login_at"))
    .bind(username)
    .bind(opt_str(record, "username_display"))
    .execute(&mut **tx)
    .await?;
    Ok(())
}

async fn insert_provider(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    record: &Value,
    user_id: &str,
) -> Result<(), ImportError> {
    sqlx::query(
        "INSERT INTO auth_providers (id, user_id, provider, provider_uid, \
         provider_data, created_at) VALUES (?, ?, ?, ?, ?, ?)",
    )
    .bind(uuid::Uuid::new_v4().to_string())
    .bind(user_id)
    .bind(str_or(record, "provider", ""))
    .bind(str_or(record, "provider_uid", ""))
    .bind(opt_str(record, "provider_data"))
    .bind(str_or(record, "created_at", ""))
    .execute(&mut **tx)
    .await?;
    Ok(())
}

async fn insert_app_password(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    crypto: &Crypto,
    record: &Value,
    user_id: &str,
    name: &str,
    plaintext: &str,
) -> Result<(), ImportError> {
    let ciphertext = crypto.encrypt(plaintext).map_err(|_| ImportError::Rekey)?;
    let sha = sha_hex(plaintext);
    sqlx::query(
        "INSERT INTO connect_app_passwords (id, user_id, name, secret_sha256, \
         secret_encrypted, created_at, last_used_at, last_client, revoked) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(uuid::Uuid::new_v4().to_string())
    .bind(user_id)
    .bind(name)
    .bind(sha)
    .bind(ciphertext)
    .bind(str_or(record, "created_at", ""))
    .bind(opt_str(record, "last_used_at"))
    .bind(opt_str(record, "last_client"))
    .bind(i64::from(bool_or(record, "revoked", false)))
    .execute(&mut **tx)
    .await?;
    Ok(())
}

async fn insert_recovery(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    record: &Value,
    user_id: &str,
) -> Result<(), ImportError> {
    sqlx::query(
        "INSERT INTO auth_password_recovery_codes (user_id, code_hash, \
         created_at, expires_at) VALUES (?, ?, ?, ?)",
    )
    .bind(user_id)
    .bind(str_or(record, "code_hash", ""))
    .bind(str_or(record, "created_at", ""))
    .bind(str_or(record, "expires_at", ""))
    .execute(&mut **tx)
    .await?;
    Ok(())
}

async fn insert_follow(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    record: &Value,
) -> Result<(), ImportError> {
    let mbid = str_or(record, "artist_mbid", "");
    sqlx::query(
        "INSERT INTO user_followed_artists (user_id, artist_mbid, \
         artist_mbid_lower, artist_name, auto_download, followed_at, \
         updated_at) VALUES (?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(str_or(record, "user_id", ""))
    .bind(mbid.trim().to_owned())
    .bind(mbid.trim().to_lowercase())
    .bind(str_or(record, "artist_name", ""))
    .bind(i64::from(bool_or(record, "auto_download", false)))
    .bind(num_or(record, "followed_at", 0.0))
    .bind(num_or(record, "updated_at", 0.0))
    .execute(&mut **tx)
    .await?;
    Ok(())
}

async fn merge_follow(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    key: &(String, String),
    auto_download: bool,
    followed_at: f64,
    updated_at: f64,
) -> Result<(), ImportError> {
    sqlx::query(
        "UPDATE user_followed_artists SET auto_download = ?, followed_at = ?, \
         updated_at = ? WHERE user_id = ? AND artist_mbid_lower = ?",
    )
    .bind(i64::from(auto_download))
    .bind(followed_at)
    .bind(updated_at)
    .bind(&key.0)
    .bind(&key.1)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

async fn insert_event_city(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    record: &Value,
) -> Result<(), ImportError> {
    sqlx::query(
        "INSERT INTO user_event_cities (user_id, city_name, country_code, latitude, \
         longitude, radius_km, position) VALUES (?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(str_or(record, "user_id", ""))
    .bind(str_or(record, "city_name", "").trim().to_owned())
    .bind(opt_str(record, "country_code"))
    .bind(num_or(record, "latitude", 0.0))
    .bind(num_or(record, "longitude", 0.0))
    .bind(num_or(record, "radius_km", 30.0))
    .bind(record.get("position").and_then(Value::as_i64).unwrap_or(0))
    .execute(&mut **tx)
    .await?;
    Ok(())
}

async fn set_event_seen(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    user_id: &str,
    seen_at: f64,
) -> Result<(), ImportError> {
    sqlx::query(
        "INSERT INTO user_event_seen (user_id, seen_at) VALUES (?, ?) \
         ON CONFLICT(user_id) DO UPDATE SET seen_at = excluded.seen_at",
    )
    .bind(user_id)
    .bind(seen_at)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

async fn insert_approval(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    record: &Value,
    reviewer_nulled: bool,
) -> Result<(), ImportError> {
    let mbid = str_or(record, "artist_mbid", "");
    let reviewer = (!reviewer_nulled)
        .then(|| opt_str(record, "reviewed_by_id"))
        .flatten();
    sqlx::query(
        "INSERT INTO auto_download_approvals (user_id, artist_mbid, \
         artist_mbid_lower, artist_name, state, requested_at, reviewed_by_id, \
         reviewed_by_name, reviewed_at, batch_id, source) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(str_or(record, "user_id", ""))
    .bind(mbid.trim().to_owned())
    .bind(mbid.trim().to_lowercase())
    .bind(str_or(record, "artist_name", ""))
    .bind(str_or(record, "state", "pending"))
    .bind(approval_ms(num_or(record, "requested_at", 0.0)))
    .bind(reviewer)
    .bind(opt_str(record, "reviewed_by_name"))
    .bind(opt_num(record, "reviewed_at").map(approval_ms))
    .bind(opt_str(record, "batch_id"))
    .bind(opt_str(record, "source"))
    .execute(&mut **tx)
    .await?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn merge_approval(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    key: &(String, String),
    state: &str,
    requested_at: i64,
    reviewed_by_id: Option<String>,
    reviewed_by_name: Option<String>,
    reviewed_at: Option<i64>,
    batch_id: Option<String>,
    source: Option<String>,
) -> Result<(), ImportError> {
    sqlx::query(
        "UPDATE auto_download_approvals SET state = ?, requested_at = ?, \
         reviewed_by_id = ?, reviewed_by_name = ?, reviewed_at = ?, \
         batch_id = ?, source = ? WHERE user_id = ? AND artist_mbid_lower = ?",
    )
    .bind(state)
    .bind(requested_at)
    .bind(reviewed_by_id)
    .bind(reviewed_by_name)
    .bind(reviewed_at)
    .bind(batch_id)
    .bind(source)
    .bind(&key.0)
    .bind(&key.1)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

/// Exit code of a run that completed.
fn completed_exit(report: &ReportBuilder) -> &'static str {
    if report.has_drops() {
        "OK_WITH_DROPS"
    } else {
        "OK"
    }
}

async fn record_import_run(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    root: &Value,
    report: &ReportBuilder,
    exit: &str,
) -> Result<String, ImportError> {
    // The row only persists when the commit succeeds; failures roll this
    // row back too. The carry and a post-commit config failure correct it
    // afterwards.
    let id = uuid::Uuid::new_v4().to_string();
    sqlx::query(
        "INSERT INTO import_runs (id, instance_id, exported_at, exit_code, \
         entity_counts, applied_at) VALUES (?, ?, ?, ?, ?, ?)",
    )
    .bind(&id)
    .bind(str_or(root, "instance_id", ""))
    .bind(str_or(root, "exported_at", ""))
    .bind(exit)
    .bind(report.counts_json())
    .bind(utc_now_iso())
    .execute(&mut **tx)
    .await?;
    Ok(id)
}

/// Audit row for a run whose only writes were carried sections.
async fn insert_import_run(
    pool: &SqlitePool,
    root: &Value,
    exit: &str,
    report: &ReportBuilder,
) -> Result<(), ImportError> {
    let mut tx = pool.begin().await?;
    record_import_run(&mut tx, root, report, exit).await?;
    tx.commit().await?;
    Ok(())
}

/// Refresh the audit row with the counts of the carried sections, which
/// commit after the row was first written.
async fn update_import_run(
    pool: &SqlitePool,
    run_id: &str,
    exit: &str,
    report: &ReportBuilder,
) -> Result<(), ImportError> {
    sqlx::query("UPDATE import_runs SET exit_code = ?, entity_counts = ? WHERE id = ?")
        .bind(exit)
        .bind(report.counts_json())
        .bind(run_id)
        .execute(pool)
        .await?;
    Ok(())
}

/// True when two settings values carry the same plaintexts. Staged secrets
/// use fresh nonces every run, so a byte compare would rewrite the row on
/// every re-import; decrypting first keeps idempotent re-imports at zero
/// writes. Undecryptable values fall back to byte compare, which simply
/// rewrites.
fn configs_equivalent(current: &Value, staged: &Value, crypto: &Crypto) -> bool {
    with_plaintexts(current, crypto) == with_plaintexts(staged, crypto)
}

fn with_plaintexts(value: &Value, crypto: &Crypto) -> Value {
    match value {
        Value::String(text)
            if text.starts_with(droppedneedle_kernel::runtime_config::crypto::CIPHER_PREFIX) =>
        {
            crypto
                .decrypt(text)
                .map(Value::String)
                .unwrap_or_else(|_| value.clone())
        }
        Value::Array(items) => Value::Array(
            items
                .iter()
                .map(|item| with_plaintexts(item, crypto))
                .collect(),
        ),
        Value::Object(map) => Value::Object(
            map.iter()
                .map(|(key, child)| (key.clone(), with_plaintexts(child, crypto)))
                .collect(),
        ),
        _ => value.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every settings key the server stores is either replaced by an
    /// import or named as left alone, and nothing else is.
    #[test]
    fn replace_list_covers_the_section_list() {
        use droppedneedle_kernel::runtime_config::KNOWN_TOP_LEVEL_KEYS;

        /// The keys an import leaves as they are.
        const LEFT_ALONE: &[&str] = &[
            // v3 only.
            "lyrics_settings",
            "public_address",
            "edition_preferences",
            // This install's own id.
            "instance_id",
        ];
        let mut named: Vec<&str> = REPLACE_UNIVERSE.iter().chain(LEFT_ALONE).copied().collect();
        named.sort_unstable();
        let mut known = KNOWN_TOP_LEVEL_KEYS.to_vec();
        known.sort_unstable();
        assert_eq!(named, known);
    }

    fn empty_snapshot() -> DatabaseSnapshot {
        DatabaseSnapshot {
            users: HashMap::new(),
            emails: HashMap::new(),
            usernames: HashMap::new(),
            providers: HashMap::new(),
            app_passwords: HashMap::new(),
            app_password_shas: HashSet::new(),
            recoveries: HashMap::new(),
            follows: HashMap::new(),
            approvals: HashMap::new(),
            event_cities: HashMap::new(),
            event_seen: HashMap::new(),
        }
    }

    fn builder() -> ReportBuilder {
        ReportBuilder::new(
            false,
            ExportProvenance {
                format_version: 1,
                exported_at: String::new(),
                instance_id: String::new(),
            },
        )
    }

    /// Defense in depth, unreachable through `run_import`: validation
    /// refuses dangling references before planning, so this branch only
    /// fires on an unvalidated document. A follow or approval whose user is
    /// unknown drops with a count, and no user row is ever invented for it.
    #[test]
    fn dangling_members_drop_without_inventing_users() {
        let root = serde_json::json!({
            "users": [],
            "follows": [{
                "user_id": "ghost",
                "artist_mbid": "01234567-89ab-cdef-0123-456789abcdef",
                "artist_name": "Ghost Artist",
                "auto_download": false,
                "followed_at": 1.0,
                "updated_at": 2.0,
            }],
            "approvals": [{
                "user_id": "ghost",
                "artist_mbid": "01234567-89ab-cdef-0123-456789abcdef",
                "artist_name": "Ghost Artist",
                "state": "pending",
                "requested_at": 1.0,
            }],
        });
        let mut report = builder();
        let plan = Plan::decide(&root, &empty_snapshot(), &HashMap::new(), &mut report);
        assert!(plan.writes.is_empty(), "drops plan no writes");
        let finished = report.finish_completed();
        assert_eq!(finished.entities["follow"].dropped_unknown_user, 1);
        assert_eq!(finished.entities["approval"].dropped_unknown_user, 1);
        assert_eq!(finished.entities["user"].imported, 0);
    }
}
