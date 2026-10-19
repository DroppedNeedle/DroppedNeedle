//! The one-time copy of `config.json` into the settings rows.
//!
//! Before settings lived in the database, a v3 server kept them in
//! `<config dir>/config.json`. The first time a store opens on a database
//! with no settings rows and no ingest record, that file is copied in, once:
//!
//! - every known top-level key whose value is not JSON `null` becomes a row
//!   at revision 1, its value copied unchanged (parsed and written back as
//!   compact JSON, never decoded into its section, normalized, validated or
//!   decrypted, so every secret stays the ciphertext it was), except that
//!   each download policy recipe step loses `target_bitrate_kbps`, a field
//!   that was removed and that the step's strict decode would refuse;
//! - unknown and dropped keys are not copied, and the record names them;
//! - the record (`config_ingests`) is written in the same transaction, so a
//!   crash before the commit repeats the copy and one after it stops it.
//!
//! The file itself is never renamed, changed or deleted, so restoring a
//! backup from before this change, or running the previous image on one,
//! still finds it.
//!
//! A file next to a key file this same start minted is not one this server
//! wrote: every v3 that wrote `config.json` minted its key first. It is never
//! parsed or stored (it may be a v2 file with plaintext secrets), and the
//! record says so, so a later start never takes it either. The decision is
//! made durable before the key exists: [`mark_skip`] leaves a marker beside
//! the key first, so a start that fails between minting the key and writing
//! the record still skips the file next time. The marker goes once the
//! record has committed.
//!
//! Log lines name keys and counts, never values.

use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::db::{Db, DbError, DurableLane};

use super::error::ConfigError;
use super::sections::{DownloadPolicy, Section as _};
use super::store::encode_text;
use super::{KNOWN_TOP_LEVEL_KEYS, sql};
use crate::clock::Clock;

/// The file name settings were kept in before they moved into the database.
pub const LEGACY_CONFIG_FILE_NAME: &str = "config.json";

/// The marker [`mark_skip`] leaves beside the key: the `config.json` there
/// is never to be copied in. It holds the file's SHA-256.
pub const SKIP_MARKER_FILE_NAME: &str = "config-json-skipped";

/// Where a store looks for a `config.json` to copy in when it opens.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LegacyConfig {
    /// No data directory: a scratch store in a test.
    None,
    /// The config folder of a data directory.
    At {
        /// The folder holding `data_enc.key` and maybe `config.json`.
        config_dir: PathBuf,
        /// The key file was written by this start, not found.
        key_minted: bool,
    },
}

impl LegacyConfig {
    /// The config folder at `config_dir`, whose key this start found
    /// (`key_minted` false) or wrote (true).
    #[must_use]
    pub fn at(config_dir: &Path, key_minted: bool) -> Self {
        Self::At {
            config_dir: config_dir.to_path_buf(),
            key_minted,
        }
    }
}

/// What happened to a `config.json`, as `config_ingests.outcome` stores it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IngestOutcome {
    /// Its sections were copied in.
    Ingested,
    /// It was left unread: the key file was new.
    SkippedNewKey,
}

impl IngestOutcome {
    /// The stored value.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ingested => "ingested",
            Self::SkippedNewKey => "skipped_new_key",
        }
    }
}

/// A `config.json` read and parsed, ready to copy in.
#[derive(Debug, Clone)]
pub struct LegacyFile {
    /// Where it was read from.
    pub path: PathBuf,
    /// Its SHA-256, hex.
    pub sha256: String,
    /// The rows it gives: each known key with a non-null value, as stored
    /// text.
    pub rows: Vec<(String, String)>,
    /// Keys it holds that are not copied: unknown, dropped, or null.
    pub left_behind: Vec<String>,
}

/// Read and parse the `config.json` a store opening on `legacy` would copy
/// in, without writing anything. `None` when there is no file, or when the
/// key is new (the file is then never read). An unreadable file, or one
/// that is not a JSON object, is an error naming the path.
pub fn read_legacy(legacy: &LegacyConfig) -> Result<Option<LegacyFile>, ConfigError> {
    let LegacyConfig::At {
        config_dir,
        key_minted,
    } = legacy
    else {
        return Ok(None);
    };
    let path = config_dir.join(LEGACY_CONFIG_FILE_NAME);
    if *key_minted || present(&config_dir.join(SKIP_MARKER_FILE_NAME)) || !present(&path) {
        return Ok(None);
    }
    parse_file(&path).map(Some)
}

/// Before a key is minted in `config_dir`: when a `config.json` sits there,
/// record durably that it is never to be copied in, so a start that fails
/// after minting the key still skips it. The marker is written to a temporary
/// file, synced, renamed into place and its folder synced. Does nothing when
/// there is no file.
pub fn mark_skip(config_dir: &Path) -> Result<(), ConfigError> {
    let path = config_dir.join(LEGACY_CONFIG_FILE_NAME);
    if !present(&path) {
        return Ok(());
    }
    let marker = config_dir.join(SKIP_MARKER_FILE_NAME);
    let failed = |error: std::io::Error| ConfigError::LegacyFile {
        path: marker.clone(),
        reason: format!("cannot record that the file is not to be read ({error})"),
    };
    // The hash only identifies the file for a person reading the record; a
    // file that cannot be read is skipped all the same, with an empty hash.
    let sha256 = std::fs::read(&path)
        .map(|bytes| sha256_hex(&bytes))
        .unwrap_or_default();
    let staging = config_dir.join(format!("{SKIP_MARKER_FILE_NAME}.tmp"));
    {
        use std::io::Write as _;
        let mut file = std::fs::File::create(&staging).map_err(failed)?;
        file.write_all(sha256.as_bytes()).map_err(failed)?;
        file.sync_all().map_err(failed)?;
    }
    std::fs::rename(&staging, &marker).map_err(failed)?;
    std::fs::File::open(config_dir)
        .and_then(|folder| folder.sync_all())
        .map_err(failed)
}

/// Remove the skip marker once the record has committed. A marker that
/// stays is harmless: the record already makes every start skip the file.
fn clear_marker(config_dir: &Path) {
    let marker = config_dir.join(SKIP_MARKER_FILE_NAME);
    if present(&marker)
        && let Err(error) = std::fs::remove_file(&marker)
    {
        tracing::warn!(
            marker = %marker.display(),
            %error,
            "cannot remove the config.json skip marker; the stored record already skips the file"
        );
    }
}

fn present(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok()
}

fn parse_file(path: &Path) -> Result<LegacyFile, ConfigError> {
    let refused = |reason: String| ConfigError::LegacyFile {
        path: path.to_path_buf(),
        reason,
    };
    let bytes = std::fs::read(path).map_err(|error| refused(error.to_string()))?;
    let value: serde_json::Value = serde_json::from_slice(&bytes)
        .map_err(|error| refused(format!("it is not valid JSON ({error})")))?;
    let serde_json::Value::Object(object) = value else {
        return Err(refused("it is not a JSON object".to_owned()));
    };
    let mut rows = Vec::new();
    let mut left_behind = Vec::new();
    for (key, mut value) in object {
        if key == DownloadPolicy::KEY {
            strip_removed_recipe_fields(&mut value);
        }
        if KNOWN_TOP_LEVEL_KEYS.contains(&key.as_str()) && !value.is_null() {
            rows.push((key, encode_text(&value)?));
        } else {
            left_behind.push(key);
        }
    }
    rows.sort();
    left_behind.sort();
    Ok(LegacyFile {
        path: path.to_path_buf(),
        sha256: sha256_hex(&bytes),
        rows,
        left_behind,
    })
}

/// Drop `target_bitrate_kbps` from every recipe step of a stored download
/// policy. Older files wrote it on every step, and a step refuses unknown
/// fields, so copied as it was the whole policy would not read.
fn strip_removed_recipe_fields(policy: &mut serde_json::Value) {
    let Some(steps) = policy
        .get_mut("quality_recipe")
        .and_then(serde_json::Value::as_array_mut)
    else {
        return;
    };
    for step in steps {
        if let Some(step) = step.as_object_mut() {
            step.remove("target_bitrate_kbps");
        }
    }
}

/// Run the one-time copy for a store opening on `db`. See the module doc.
pub fn run(db: &Db, legacy: &LegacyConfig, clock: &dyn Clock) -> Result<(), ConfigError> {
    let LegacyConfig::At {
        config_dir,
        key_minted,
    } = legacy
    else {
        return Ok(());
    };
    let path = config_dir.join(LEGACY_CONFIG_FILE_NAME);
    let settled = db.read_blocking("config.ingest_check", |conn| {
        conn.query_row(sql::ANY_SETTINGS.text(), [], |row| row.get::<_, bool>(0))
            .map_err(DbError::from)
    })?;
    let mut marked = present(&config_dir.join(SKIP_MARKER_FILE_NAME));
    // A marker is written for a database with no accounts yet (a first
    // start). One beside a database that holds accounts was left for another
    // database (this one restored in its place), so it does not apply.
    if marked && !settled && holds_accounts(db)? {
        clear_marker(config_dir);
        marked = false;
    }
    if settled {
        if marked {
            clear_marker(config_dir);
        }
        if present(&path) {
            tracing::info!(
                file = %path.display(),
                "settings live in the database; this config.json is no longer read"
            );
        }
        return Ok(());
    }
    if !present(&path) {
        if marked {
            clear_marker(config_dir);
        }
        return Ok(());
    }
    if *key_minted || marked {
        let sha256 = std::fs::read(&path)
            .map(|bytes| sha256_hex(&bytes))
            .unwrap_or_default();
        let recorded = record(db, clock, IngestOutcome::SkippedNewKey, &sha256, &[], &[])?;
        clear_marker(config_dir);
        if recorded {
            tracing::warn!(
                file = %path.display(),
                "a config.json sat beside a key file a start created, so it was not \
                 written by this server; it is never parsed or stored"
            );
        }
        return Ok(());
    }
    let file = parse_file(&path)?;
    let copied: Vec<String> = file.rows.iter().map(|(key, _)| key.clone()).collect();
    let now = clock.now_ms();
    let wrote = db.write_durable(DurableLane::Background, "config.ingest", |tx| {
        if tx
            .query_row(sql::ANY_SETTINGS.text(), [], |row| row.get::<_, bool>(0))
            .map_err(DbError::from)?
        {
            return Ok::<_, ConfigError>(false);
        }
        for (key, text) in &file.rows {
            tx.query_row(
                sql::PUT_SECTION.text(),
                rusqlite::params![key, text, now],
                |row| row.get::<_, i64>(0),
            )
            .map_err(DbError::from)?;
        }
        insert_record(
            tx,
            IngestOutcome::Ingested,
            &file.sha256,
            &copied,
            &file.left_behind,
            now,
        )?;
        Ok(true)
    })?;
    if wrote {
        tracing::info!(
            file = %path.display(),
            copied = copied.len(),
            left_behind = ?file.left_behind,
            "copied the settings from config.json into the database; the file is left as it was"
        );
    }
    Ok(())
}

/// Whether the database holds any account.
fn holds_accounts(db: &Db) -> Result<bool, ConfigError> {
    db.read_blocking("config.ingest_accounts", |conn| {
        conn.query_row(sql::ANY_ACCOUNT.text(), [], |row| row.get::<_, bool>(0))
            .map_err(DbError::from)
            .map_err(ConfigError::from)
    })
}

/// Write the record alone, when nothing is stored yet. Says whether it was
/// written.
fn record(
    db: &Db,
    clock: &dyn Clock,
    outcome: IngestOutcome,
    sha256: &str,
    copied: &[String],
    left: &[String],
) -> Result<bool, ConfigError> {
    let now = clock.now_ms();
    db.write_durable(DurableLane::Background, "config.ingest_record", |tx| {
        if tx
            .query_row(sql::ANY_SETTINGS.text(), [], |row| row.get::<_, bool>(0))
            .map_err(DbError::from)?
        {
            return Ok::<_, ConfigError>(false);
        }
        insert_record(tx, outcome, sha256, copied, left, now)?;
        Ok(true)
    })
}

fn insert_record(
    tx: &rusqlite::Transaction<'_>,
    outcome: IngestOutcome,
    sha256: &str,
    copied: &[String],
    left: &[String],
    now: i64,
) -> Result<(), ConfigError> {
    let list = |keys: &[String]| encode_text(&serde_json::json!(keys));
    tx.execute(
        sql::RECORD_INGEST.text(),
        rusqlite::params![outcome.as_str(), sha256, now, list(copied)?, list(left)?],
    )
    .map_err(DbError::from)?;
    Ok(())
}

fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}
