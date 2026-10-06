//! Reading a v2 instance directory.
//!
//! Layout (v2 defaults under `<ROOT_APP_DIR>`): `config/.env` holds
//! `DATA_ENC_KEY`, `config/config.json` holds settings plus the top-level
//! instance id, `cache/library.db` is the single SQLite file, and
//! `plugins/<name>/plugin.toml` manifests flag secret plugin settings.
//! Everything here is read-only; the exporter never writes into v2.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde_json::{Map, Value};

use crate::export::error::ExportError;
use crate::export::fernet::FernetKey;

/// Key-file location inside the v2 config dir.
const KEY_FILE_NAME: &str = ".env";
/// v2 config-file name inside the v2 config dir.
const CONFIG_FILE_NAME: &str = "config.json";
/// v2 database location inside the v2 cache dir.
const DB_FILE_NAME: &str = "library.db";
/// v2 plugins location inside the instance root.
const PLUGINS_DIR_NAME: &str = "plugins";
/// v2 key variable name.
const DATA_ENC_KEY_VAR: &str = "DATA_ENC_KEY";

/// Expected v2 key-file path, for errors and refusal tests.
#[must_use]
pub fn key_file_path(v2_root: &Path) -> PathBuf {
    v2_root.join("config").join(KEY_FILE_NAME)
}

/// Expected v2 config path.
#[must_use]
pub fn config_file_path(v2_root: &Path) -> PathBuf {
    v2_root.join("config").join(CONFIG_FILE_NAME)
}

/// v2 database path: the explicit override when given, else the default.
#[must_use]
pub fn resolve_db_path(v2_root: &Path, override_path: Option<&Path>) -> PathBuf {
    override_path.map_or_else(
        || v2_root.join("cache").join(DB_FILE_NAME),
        Path::to_path_buf,
    )
}

/// Percent-encode the characters a SQLite URI path cannot carry raw.
#[must_use]
pub(crate) fn sqlite_uri_path(path: &Path) -> String {
    let mut out = String::new();
    for ch in path.to_string_lossy().chars() {
        match ch {
            '%' => out.push_str("%25"),
            '?' => out.push_str("%3f"),
            '#' => out.push_str("%23"),
            other => out.push(other),
        }
    }
    out
}

/// v2 cache dir (`<root>/cache`): avatars and playlist covers live here.
#[must_use]
pub fn cache_dir(v2_root: &Path) -> PathBuf {
    v2_root.join("cache")
}

/// Parse dotenv text into variables. Enough for the v2 key file: blank
/// lines and `#` comments skipped, an optional `export` prefix tolerated,
/// and matching single or double quotes stripped from values.
#[must_use]
pub fn parse_dotenv(text: &str) -> HashMap<String, String> {
    let mut vars = HashMap::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let line = line.strip_prefix("export ").unwrap_or(line).trim();
        let Some((name, value)) = line.split_once('=') else {
            continue;
        };
        let name = name.trim();
        if name.is_empty() {
            continue;
        }
        let mut value = value.trim().to_owned();
        if value.len() >= 2
            && ((value.starts_with('\'') && value.ends_with('\''))
                || (value.starts_with('"') && value.ends_with('"')))
        {
            value = value[1..value.len() - 1].to_owned();
        }
        vars.insert(name.to_owned(), value);
    }
    vars
}

/// Load the v2 data key. A missing file refuses with [`ExportError::V2KeyNotFound`];
/// a present file with no usable key refuses with [`ExportError::V2KeyInvalid`].
/// Minting here would silently re-key stored ciphertext, so both fail.
pub fn read_data_enc_key(v2_root: &Path) -> Result<FernetKey, ExportError> {
    let path = key_file_path(v2_root);
    let text = std::fs::read_to_string(&path).map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            ExportError::V2KeyNotFound { path: path.clone() }
        } else {
            ExportError::V2ConfigUnreadable {
                path: path.clone(),
                reason: error.to_string(),
            }
        }
    })?;
    let vars = parse_dotenv(&text);
    let encoded = vars
        .get(DATA_ENC_KEY_VAR)
        .map(String::as_str)
        .unwrap_or("")
        .trim();
    if encoded.is_empty() {
        return Err(ExportError::V2KeyInvalid);
    }
    FernetKey::from_base64(encoded).map_err(|_| ExportError::V2KeyInvalid)
}

/// Read the v2 config file as a JSON object.
pub fn read_config(v2_root: &Path) -> Result<Map<String, Value>, ExportError> {
    let path = config_file_path(v2_root);
    let text = std::fs::read_to_string(&path).map_err(|error| ExportError::V2ConfigUnreadable {
        path: path.clone(),
        reason: error.to_string(),
    })?;
    let parsed: Value = serde_json::from_str(&text).map_err(|_| ExportError::V2ConfigInvalid)?;
    parsed
        .as_object()
        .cloned()
        .ok_or(ExportError::V2ConfigInvalid)
}

/// Declared setting flags per plugin, from `<v2root>/plugins/*/plugin.toml`
/// manifests (`[[settings]]` keys to their `secret` bit, keyed by the
/// manifest's own `plugin.name`, which need not match the folder name).
/// Unreadable or invalid manifests are skipped: settings whose flags are
/// unknown seal rather than travel verbatim, so a lost manifest fails safe.
/// A missing plugins dir yields an empty map.
#[must_use]
pub fn plugin_setting_flags(v2_root: &Path) -> HashMap<String, HashMap<String, bool>> {
    let mut flags: HashMap<String, HashMap<String, bool>> = HashMap::new();
    let dir = v2_root.join(PLUGINS_DIR_NAME);
    let entries = match std::fs::read_dir(&dir) {
        Ok(entries) => entries,
        Err(_) => return flags,
    };
    for entry in entries.flatten() {
        let manifest_path = entry.path().join("plugin.toml");
        let text = match std::fs::read_to_string(&manifest_path) {
            Ok(text) => text,
            Err(_) => continue,
        };
        let parsed: toml::Value = match toml::from_str(&text) {
            Ok(value) => value,
            Err(_) => continue,
        };
        let Some(name) = parsed
            .get("plugin")
            .and_then(|plugin| plugin.get("name"))
            .and_then(toml::Value::as_str)
        else {
            continue;
        };
        let mut declared = HashMap::new();
        if let Some(settings) = parsed.get("settings").and_then(toml::Value::as_array) {
            for setting in settings {
                let key = setting.get("key").and_then(toml::Value::as_str);
                let secret = setting
                    .get("secret")
                    .and_then(toml::Value::as_bool)
                    .unwrap_or(false);
                if let Some(key) = key {
                    declared.insert(key.to_owned(), secret);
                }
            }
        }
        flags.insert(name.to_owned(), declared);
    }
    flags
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dotenv_parsing_covers_key_file_shapes() {
        let vars = parse_dotenv(
            "# v2 key file\nDATA_ENC_KEY='abc123'\nEMPTY=\nexport OTHER=\"x y\"\nnot-a-pair\n",
        );
        assert_eq!(vars.get("DATA_ENC_KEY").unwrap(), "abc123");
        assert_eq!(vars.get("EMPTY").unwrap(), "");
        assert_eq!(vars.get("OTHER").unwrap(), "x y");
        assert!(!vars.contains_key("not-a-pair"));
    }

    #[test]
    fn missing_key_file_refuses_without_minting() {
        let dir = std::env::temp_dir().join(format!(
            "dn-export-key-test-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let error = read_data_enc_key(&dir).unwrap_err();
        assert_eq!(error.code(), "V2_KEY_NOT_FOUND");
        assert!(!key_file_path(&dir).exists());
    }
}
