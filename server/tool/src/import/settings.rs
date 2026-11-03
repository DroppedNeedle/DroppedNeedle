//! The settings half of an import: what the target holds, what the export
//! replaces it with, and the rows that come out, compared decrypted so an
//! identical re-import writes no row.
//!
//! Unknown settings sections are ignored with a validator warning and never
//! unsealed. The scan-schedule carry from v2's `config.json` refuses on
//! instance mismatch.

use std::collections::HashMap;

use droppedneedle_kernel::runtime_config::ingest::{
    move_recycle_bin, move_recycle_retention, read_legacy, retire_fast_scan_schedule,
};
use droppedneedle_kernel::runtime_config::sql as settings_sql;
use droppedneedle_kernel::runtime_config::store::{decode_text, encode_text};
use droppedneedle_kernel::runtime_config::{ConfigError, Crypto, LegacyConfig};

use rusqlite::{Connection, params};
use serde_json::Value;

use super::apply::ImportError;
use super::envelope::is_sealed;
use super::naming_carry;
use super::quality_carry;
use super::report::{DROPPED_SETTING, ReportBuilder};
use super::schedule_carry::carry_frequency;
use super::validate::strip_removed;
use crate::allowlist::{is_replaced, removed_sections, replaced_sections};
use crate::export::envelope::ExportDoc;

/// Transient `musicbrainz_settings` fields dropped at import.
const MUSICBRAINZ_TRANSIENT: &[&str] = &[
    "pending_brainzmash",
    "source_quarantined",
    "quarantine_reason",
];

/// The target's settings as one object, key by key: its stored rows, or,
/// while it stores none and has recorded no copy of `config.json`, what a
/// first start would copy in from that file. A file that does not read
/// refuses the import before anything is written.
pub(super) fn current_settings(
    conn: &Connection,
    legacy: &LegacyConfig,
) -> Result<Value, ImportError> {
    let settled: bool = conn.query_row(settings_sql::ANY_SETTINGS.text(), [], |row| row.get(0))?;
    let mut current = serde_json::Map::new();
    if settled {
        for (key, value) in stored_settings(conn)? {
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
fn stored_settings(conn: &Connection) -> Result<Vec<(String, Value)>, ImportError> {
    let mut statement = conn.prepare(settings_sql::ALL_SECTIONS.text())?;
    let rows = statement
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    rows.into_iter()
        .map(|(key, text)| {
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
pub(super) fn write_settings(
    tx: &Connection,
    staged: &Value,
    crypto: &Crypto,
) -> Result<usize, ImportError> {
    let staged = staged.as_object().ok_or_else(|| ImportError::Config {
        reason: "staged settings are not a JSON object".to_owned(),
    })?;
    let stored: HashMap<String, Value> = stored_settings(tx)?.into_iter().collect();
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
        tx.query_row(
            settings_sql::PUT_SECTION.text(),
            params![key, text, now],
            |row| row.get::<_, i64>(0),
        )?;
        changed += 1;
    }
    // Only a replaced section is ever removed: rows outside the replace
    // list (the key's check value among them) are not the export's to drop.
    for key in stored
        .keys()
        .filter(|key| !staged.contains_key(*key) && is_replaced(key))
    {
        tx.execute(settings_sql::DELETE_SECTION.text(), params![key])?;
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

pub(super) fn stage_settings(
    doc: &ExportDoc,
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
    staged_object.insert(
        "instance_id".to_owned(),
        Value::String(doc.instance_id.clone()),
    );
    let exported = &doc.settings;
    let client_policy = quality_carry::policy_from_client(exported);
    if client_policy.is_some() {
        report.note(
            "settings",
            "download_policy".to_owned(),
            "imported",
            "moved from the download client, where this v2 install kept it".to_owned(),
        );
    }

    for section in removed_sections() {
        if exported.contains_key(section) {
            note_dropped_setting(report, section.to_owned());
        }
    }
    for section in replaced_sections() {
        let carried = client_policy
            .as_ref()
            .filter(|_| section == "download_policy");
        match exported.get(section).or(carried) {
            Some(value) => {
                let mut filtered = filter_section(section, value);
                let v2_quality = (section == "download_policy")
                    .then(|| quality_carry::take_v2_quality(&mut filtered));
                for path in strip_removed(section, &mut filtered) {
                    note_dropped_setting(report, path);
                }
                if let Some(v2_quality) = v2_quality {
                    quality_carry::convert_policy(&mut filtered, v2_quality, report)?;
                }
                if section == "download_client" {
                    quality_carry::strip_client_policy(&mut filtered);
                }
                if section == "plugins" {
                    note_unflagged_plugin_settings(&filtered, report);
                    note_plugins_need_porting(&filtered, report);
                }
                if section == "library_management" {
                    carry_v2_management_fields(&mut filtered);
                }
                if section == "library_settings" {
                    naming_carry::convert_template(&mut filtered, report);
                }
                let applied = rekey_section(
                    &filtered,
                    &format!("settings.{section}"),
                    unsealed,
                    crypto,
                    report,
                )?;
                staged_object.insert(section.to_owned(), applied);
                report.setting_applied(section.to_owned());
            }
            None => {
                staged_object.remove(section);
                report.setting_defaulted(section.to_owned());
            }
        }
    }

    // v2 named the recycle bin in its download policy; v3 sets it in
    // Library Management only.
    if let Some(moved) = move_recycle_bin(staged_object) {
        report.note(
            "settings",
            "library_management.recycle_bin_path".to_owned(),
            "imported",
            "moved from the download policy".to_owned(),
        );
        if let Some(replaced) = moved.replaced {
            report.note(
                "settings",
                format!("library_management.recycle_bin_path ({replaced})"),
                DROPPED_SETTING,
                "v2's removals used the download policy's path, which replaces it".to_owned(),
            );
        }
    }

    if move_recycle_retention(staged_object) {
        report.note(
            "settings",
            "library_management.recycle_retention_days".to_owned(),
            "imported",
            "moved from the download policy".to_owned(),
        );
    }

    if retire_fast_scan_schedule(staged_object) {
        report.note(
            "settings",
            "library_scan_schedule.scan_frequency".to_owned(),
            "imported",
            "v3 has no 5 or 10 minute scan schedule; it is now 30 minutes".to_owned(),
        );
    }

    // The schedule carry fires whenever the export lacks the schedule: idempotent across
    // re-imports, and consistent with whole-config replace (the export,
    // plus the one-shot v2 carry, is the source of truth).
    let schedule_present = exported.contains_key("library_scan_schedule");
    let export_instance = doc.instance_id.as_str();
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

/// True when two settings values carry the same plaintexts. Staged secrets
/// use fresh nonces every run, so a byte compare would rewrite the row on
/// every re-import; decrypting first keeps idempotent re-imports at zero
/// writes. Undecryptable values fall back to byte compare, which simply
/// rewrites.
pub(super) fn configs_equivalent(current: &Value, staged: &Value, crypto: &Crypto) -> bool {
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
