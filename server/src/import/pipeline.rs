//! v2 → v3 importer: parse → unlock → settings → users → secrets →
//! follows/approvals → atomic commit → post-import rebuild.
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
//! instance mismatch. The config
//! write compares secrets decrypted, so idempotent re-imports leave the
//! config bytes untouched; a post-commit config failure marks its audit row
//! `FAILED_INTERNAL` after the fact.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use serde_json::Value;
use sha2::{Digest, Sha256};
use sqlx::SqlitePool;
use thiserror::Error;

use super::envelope::{ExportFile, collect_sealed, is_sealed, unseal_value};
use super::r8::carry_frequency;
use super::report::{ExitCode, ExportProvenance, ImportReport, ReportBuilder, utc_now_iso};
use super::validate::{ValidationIssue, validate_export};
use crate::export::envelope::SecretEnvelope;
use crate::runtime_config::Crypto;

/// Sections the export may carry (kept whole or in part). Present
/// sections replace the v3 config wholesale; absent ones reset to v3
/// defaults. v3-only keys outside this set (today: `lyrics_settings`)
/// are left untouched: the export cannot speak for state v2 never had.
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
    "scrobble_settings",
    "primary_music_source",
    "free_music",
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
    /// The v3 config file cannot be read or written.
    #[error("config failure: {reason}")]
    Config {
        /// What went wrong, without file contents.
        reason: String,
    },
    /// Re-encryption under the v3 key failed.
    #[error("re-encryption failure")]
    Rekey,
    /// Reading the scan schedule from v2's `config.json` failed.
    #[error("v2 scan schedule carry failed: {0}")]
    R8(#[from] super::r8::R8Error),
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
    /// Target v3 `config.json`.
    pub config_path: PathBuf,
    /// v3 data key for re-encryption.
    pub crypto: Crypto,
    /// v2 `config.json` for the one-shot scan-schedule carry, when available.
    pub v2_config_path: Option<PathBuf>,
    /// Dry-run: identical decisions and counts, zero writes.
    pub dry_run: bool,
    /// Test-only: fail after staging everything, before commit, to
    /// prove the rollback (simulated kill mid-import).
    pub fault_before_commit: bool,
    /// Test-only: fail after the DB commit but before the config
    /// write, to prove re-import converges from the split state.
    pub fault_after_commit: bool,
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

    let unsealed = match unseal_consumed(&parsed.root, &parsed.envelope, &request.passphrase) {
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

    match run_guarded(&request, &parsed.root, &unsealed, &mut report).await {
        Ok(()) => report.finish_completed(),
        Err(ImportError::FaultBeforeCommit | ImportError::FaultAfterCommit) => report.finish(
            ExitCode::FailedInternal,
            "simulated crash; rolled back".to_owned(),
        ),
        Err(ImportError::R8(mismatch @ super::r8::R8Error::InstanceMismatch { .. })) => {
            report.finish(ExitCode::FailedValidation, mismatch.to_string())
        }
        Err(error) => report.finish(ExitCode::FailedInternal, error.to_string()),
    }
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

/// Unseal every sealed value the importer consumes (settings plus app
/// passwords), strictly in memory. Ignored sections (reserved, unknown)
/// are not consumed, so a broken blob there cannot fail the import.
fn unseal_consumed(
    root: &Value,
    envelope: &SecretEnvelope,
    passphrase: &str,
) -> Result<HashMap<String, String>, SealFailure> {
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
    let mut out = HashMap::with_capacity(found.len());
    for (path, sealed) in &found {
        match unseal_value(passphrase, envelope, sealed, path) {
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

/// The guarded middle of the pipeline: settings staging, entity merge,
/// atomic commit, config write, rebuild. Any error before the config
/// write leaves prior state untouched (DB rolls back, config unwritten).
async fn run_guarded(
    request: &ImportRequest,
    root: &Value,
    unsealed: &HashMap<String, String>,
    report: &mut ReportBuilder,
) -> Result<(), ImportError> {
    let current_config = read_current_config(&request.config_path)?;
    let staged_config = stage_settings(
        root,
        &current_config,
        unsealed,
        &request.crypto,
        request.v2_config_path.as_deref(),
        report,
    )?;

    let snapshot = DatabaseSnapshot::load(&request.pool).await?;
    let plan = Plan::decide(root, &snapshot, unsealed, report);

    if request.dry_run {
        for action in &plan.rebuild {
            report.note(
                "rebuild",
                action.clone(),
                "queued",
                "dry-run: no writes performed".to_owned(),
            );
        }
        return Ok(());
    }

    let mut tx = request.pool.begin().await?;
    sqlx::query("PRAGMA foreign_keys=ON")
        .execute(&mut *tx)
        .await?;
    plan.apply(&mut tx, &request.crypto, unsealed).await?;
    // A pure re-import (every record identical) writes nothing at all,
    // not even an audit row: idempotency means zero writes.
    let run_id = if plan.has_writes() {
        Some(record_import_run(&mut tx, root, report).await?)
    } else {
        None
    };
    if request.fault_before_commit {
        tx.rollback().await?;
        return Err(ImportError::FaultBeforeCommit);
    }
    tx.commit().await?;
    if request.fault_after_commit {
        // No audit fix-up: a real crash could not run one either.
        return Err(ImportError::FaultAfterCommit);
    }

    if !configs_equivalent(&current_config, &staged_config, &request.crypto)
        && let Err(error) = write_json_atomically(&request.config_path, &staged_config)
    {
        // The DB already committed, so correct the audit row instead of
        // leaving it claiming a success the run never finished. Best
        // effort: the config failure itself is the report's verdict.
        if let Some(id) = run_id {
            mark_import_run_failed(&request.pool, &id).await;
        }
        return Err(error);
    }
    run_rebuild(&request.pool, &plan).await?;
    for action in &plan.rebuild {
        report.note("rebuild", action.clone(), "queued", String::new());
    }
    Ok(())
}

// --- settings staging ------------------------------------------------------

fn read_current_config(path: &std::path::Path) -> Result<Value, ImportError> {
    match std::fs::read_to_string(path) {
        Ok(text) => serde_json::from_str(&text).map_err(|error| ImportError::Config {
            reason: format!("v3 config is not valid JSON: {error}"),
        }),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            Ok(Value::Object(serde_json::Map::new()))
        }
        Err(error) => Err(ImportError::Config {
            reason: format!("cannot read v3 config: {error}"),
        }),
    }
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
        reason: "v3 config root must be a JSON object".to_owned(),
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

    for section in REPLACE_UNIVERSE {
        match exported.get(*section) {
            Some(value) => {
                let filtered = filter_section(section, value);
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
            "r8 one-shot carry from v2 sync_frequency".to_owned(),
        );
    }
    Ok(staged)
}

/// Drop fields the export spec excludes, before re-encryption:
/// lastfm keeps only the master switch (secrets decrypted, then dropped), the
/// MusicBrainz transient trio goes, plugins keep `enabled` + `settings`.
fn filter_section(name: &str, value: &Value) -> Value {
    match name {
        "lastfm_settings" => {
            let mut kept = serde_json::Map::new();
            if let Some(enabled) = value.get("enabled") {
                kept.insert("enabled".to_owned(), enabled.clone());
            }
            Value::Object(kept)
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
                    if let Some(enabled) = config.get("enabled") {
                        entry.insert("enabled".to_owned(), enabled.clone());
                    }
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

/// Atomic JSON write: temp file in the same directory, fsync, rename,
/// directory fsync (the `ConfigStore` shape, owned here so the importer
/// does not need a store handle).
fn write_json_atomically(path: &std::path::Path, value: &Value) -> Result<(), ImportError> {
    use std::io::Write as _;

    let failed = |reason: String| ImportError::Config { reason };
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent).map_err(|error| failed(error.to_string()))?;
    }
    let bytes = serde_json::to_vec_pretty(value).map_err(|error| failed(error.to_string()))?;
    let mut tmp_name = path
        .file_name()
        .map(|name| name.to_os_string())
        .unwrap_or_default();
    tmp_name.push(".tmp");
    let tmp_path = path.with_file_name(tmp_name);
    let outcome: Result<(), String> = (|| {
        let mut file = std::fs::File::create(&tmp_path).map_err(|error| error.to_string())?;
        file.write_all(&bytes).map_err(|error| error.to_string())?;
        file.sync_all().map_err(|error| error.to_string())?;
        drop(file);
        std::fs::rename(&tmp_path, path).map_err(|error| error.to_string())?;
        fsync_parent(path).map_err(|error| error.to_string())?;
        Ok(())
    })();
    if let Err(reason) = outcome {
        let _ = std::fs::remove_file(&tmp_path);
        return Err(failed(reason));
    }
    Ok(())
}

#[cfg(unix)]
fn fsync_parent(path: &std::path::Path) -> std::io::Result<()> {
    let parent = path.parent().unwrap_or(std::path::Path::new("."));
    std::fs::File::open(parent)?.sync_all()
}

#[cfg(not(unix))]
fn fsync_parent(_path: &std::path::Path) -> std::io::Result<()> {
    Ok(())
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
struct ApprovalRow {
    user_id: String,
    artist_mbid_lower: String,
    state: String,
    requested_at: f64,
    reviewed_by_id: Option<String>,
    reviewed_by_name: Option<String>,
    reviewed_at: Option<f64>,
    batch_id: Option<String>,
    source: Option<String>,
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
}

impl DatabaseSnapshot {
    async fn load(pool: &SqlitePool) -> Result<Self, ImportError> {
        let users: Vec<UserRow> = sqlx::query_as(
            "SELECT id, display_name, email, avatar_url, role, created_at, \
             last_login_at, username, username_display FROM auth_users",
        )
        .fetch_all(pool)
        .await?;
        let providers: Vec<ProviderRow> = sqlx::query_as(
            "SELECT user_id, provider, provider_uid, provider_data, created_at \
             FROM auth_providers",
        )
        .fetch_all(pool)
        .await?;
        let app_passwords: Vec<AppPasswordRow> = sqlx::query_as(
            "SELECT user_id, name, secret_sha256, created_at, last_used_at, \
             last_client, revoked FROM connect_app_passwords",
        )
        .fetch_all(pool)
        .await?;
        let recoveries: Vec<RecoveryRow> = sqlx::query_as(
            "SELECT user_id, code_hash, created_at, expires_at \
             FROM auth_password_recovery_codes",
        )
        .fetch_all(pool)
        .await?;
        let follows: Vec<FollowRow> = sqlx::query_as(
            "SELECT user_id, artist_mbid_lower, auto_download, followed_at, \
             updated_at FROM user_followed_artists",
        )
        .fetch_all(pool)
        .await?;
        let approvals: Vec<ApprovalRow> = sqlx::query_as(
            "SELECT user_id, artist_mbid_lower, state, requested_at, \
             reviewed_by_id, reviewed_by_name, reviewed_at, batch_id, source \
             FROM auto_download_approvals",
        )
        .fetch_all(pool)
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
        };
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
        requested_at: f64,
        reviewed_by_id: Option<String>,
        reviewed_by_name: Option<String>,
        reviewed_at: Option<f64>,
        batch_id: Option<String>,
        source: Option<String>,
    },
}

/// The decided import: writes for real runs, rebuild notes for both.
struct Plan {
    writes: Vec<Planned>,
    rebuild: Vec<String>,
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
        let mut plan = Self {
            writes: Vec::new(),
            rebuild: Vec::new(),
        };
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

        let follow_keys: Vec<String> = plan
            .writes
            .iter()
            .filter_map(|write| match write {
                Planned::InsertFollow { record } | Planned::InsertApproval { record, .. } => {
                    follow_key(record)
                }
                Planned::MergeFollow { key, .. } | Planned::MergeApproval { key, .. } => {
                    Some(format!("{}|{}", key.0, key.1))
                }
                _ => None,
            })
            .collect();
        if !follow_keys.is_empty() {
            plan.rebuild.push(format!(
                "follow-poll-requeue: {} artist(s)",
                follow_keys.len()
            ));
        }
        plan.rebuild.push("mbid-warmup: scheduler".to_owned());
        plan.rebuild
            .push("discovery-snapshot: scheduler".to_owned());
        plan.rebuild.push("compat-id-map: scheduler".to_owned());
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
                        opt_num(approval, "reviewed_at"),
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
                let requested_at = existing
                    .requested_at
                    .min(num_or(approval, "requested_at", 0.0));
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

fn users_identical(existing: &UserRow, imported: &Value) -> bool {
    existing.display_name == str_or(imported, "display_name", "")
        && existing.email == opt_str(imported, "email")
        && existing.avatar_url == opt_str(imported, "avatar_url")
        && existing.role == str_or(imported, "role", "user")
        && existing.username == opt_str(imported, "username")
        && existing.username_display == opt_str(imported, "username_display")
        && existing.created_at == str_or(imported, "created_at", "")
        && existing.last_login_at == opt_str(imported, "last_login_at")
}

fn follow_key(record: &Value) -> Option<String> {
    let user_id = record.get("user_id").and_then(Value::as_str)?;
    let mbid = record.get("artist_mbid").and_then(Value::as_str)?;
    Some(format!("{user_id}|{}", mbid.trim().to_lowercase()))
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
    sqlx::query(
        "INSERT INTO auth_users (id, display_name, email, avatar_url, role, \
         created_at, last_login_at, username, username_display) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(str_or(record, "id", ""))
    .bind(str_or(record, "display_name", ""))
    .bind(email)
    .bind(opt_str(record, "avatar_url"))
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
    .bind(num_or(record, "requested_at", 0.0))
    .bind(reviewer)
    .bind(opt_str(record, "reviewed_by_name"))
    .bind(opt_num(record, "reviewed_at"))
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
    requested_at: f64,
    reviewed_by_id: Option<String>,
    reviewed_by_name: Option<String>,
    reviewed_at: Option<f64>,
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

async fn record_import_run(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    root: &Value,
    report: &ReportBuilder,
) -> Result<String, ImportError> {
    // The row only persists when the commit succeeds, so the stored
    // exit reflects a completed run; failures roll this row back too.
    // (A post-commit config failure corrects the row afterwards.)
    let exit = if report.has_drops() {
        "OK_WITH_DROPS"
    } else {
        "OK"
    };
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

/// Best-effort audit correction after a post-commit config failure. Errors
/// are swallowed: the config failure itself is already the run's verdict.
async fn mark_import_run_failed(pool: &SqlitePool, run_id: &str) {
    let _ = sqlx::query("UPDATE import_runs SET exit_code = 'FAILED_INTERNAL' WHERE id = ?")
        .bind(run_id)
        .execute(pool)
        .await;
}

/// True when two configs carry the same plaintexts. Staged secrets use
/// fresh nonces every run, so a byte compare would rewrite the file on
/// every re-import; decrypting first keeps idempotent re-imports at zero
/// writes. Undecryptable values fall back to byte compare, which simply
/// rewrites.
fn configs_equivalent(current: &Value, staged: &Value, crypto: &Crypto) -> bool {
    with_plaintexts(current, crypto) == with_plaintexts(staged, crypto)
}

fn with_plaintexts(value: &Value, crypto: &Crypto) -> Value {
    match value {
        Value::String(text) if text.starts_with(crate::runtime_config::crypto::CIPHER_PREFIX) => {
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

// --- post-import rebuild -----------------------------------------------------

async fn run_rebuild(pool: &SqlitePool, plan: &Plan) -> Result<(), ImportError> {
    let mut mbids = HashSet::new();
    for write in &plan.writes {
        match write {
            Planned::InsertFollow { record } | Planned::InsertApproval { record, .. } => {
                if let Some(mbid) = record
                    .get("artist_mbid")
                    .and_then(Value::as_str)
                    .map(|raw| raw.trim().to_lowercase())
                {
                    mbids.insert(mbid);
                }
            }
            Planned::MergeFollow { key, .. } | Planned::MergeApproval { key, .. } => {
                mbids.insert(key.1.clone());
            }
            _ => {}
        }
    }
    for mbid in &mbids {
        sqlx::query(
            "INSERT INTO follow_due (artist_mbid_lower, due_at, failures, \
             last_serviced) VALUES (?, 0, 0, 0) \
             ON CONFLICT (artist_mbid_lower) DO UPDATE SET due_at = 0",
        )
        .bind(mbid)
        .execute(pool)
        .await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

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
