//! Reading a v2 instance's folders.
//!
//! Layout (v2 defaults under `<ROOT_APP_DIR>`): `config/.env` holds
//! `DATA_ENC_KEY`, `config/config.json` holds settings plus the top-level
//! instance id, `cache/library.db` is the single SQLite file, and
//! `plugins/<name>/plugin.toml` manifests flag secret plugin settings.
//! The config and cache folders are separate mounts in a container, so
//! [`V2Dirs`] names each one instead of assuming a shared root.
//! Everything here is read-only; the exporter never writes into v2.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde_json::{Map, Value};

use crate::export::error::ExportError;
use crate::export::fernet::FernetKey;
use droppedneedle::bootstrap::data_dirs::{DATA_ENC_KEY_VAR, key_file_path, parse_dotenv};

/// v2 config-file name inside the v2 config dir.
const CONFIG_FILE_NAME: &str = "config.json";
/// v2 database location inside the v2 cache dir.
const DB_FILE_NAME: &str = "library.db";
/// v2 plugins location inside the instance root.
const PLUGINS_DIR_NAME: &str = "plugins";

/// The folders one v2 instance keeps its data in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct V2Dirs {
    /// Holds `.env` (the key file) and `config.json`.
    pub config_dir: PathBuf,
    /// Holds `library.db`, avatars, covers, held downloads and saved
    /// originals.
    pub cache_dir: PathBuf,
    /// Holds one folder per plugin, each with its `plugin.toml`.
    pub plugins_dir: PathBuf,
}

impl V2Dirs {
    /// The standard layout under one v2 `<ROOT_APP_DIR>`.
    #[must_use]
    pub fn under_root(v2_root: &Path) -> Self {
        Self {
            config_dir: v2_root.join("config"),
            cache_dir: v2_root.join("cache"),
            plugins_dir: v2_root.join(PLUGINS_DIR_NAME),
        }
    }
}

/// Expected v2 config path.
#[must_use]
pub fn config_file_path(config_dir: &Path) -> PathBuf {
    config_dir.join(CONFIG_FILE_NAME)
}

/// v2 database path: the explicit override when given, else the default.
#[must_use]
pub fn resolve_db_path(cache_dir: &Path, override_path: Option<&Path>) -> PathBuf {
    override_path.map_or_else(|| cache_dir.join(DB_FILE_NAME), Path::to_path_buf)
}

/// Load the v2 data key. A missing file refuses with [`ExportError::V2KeyNotFound`];
/// a present file with no usable key refuses with [`ExportError::V2KeyInvalid`].
/// Minting here would silently re-key stored ciphertext, so both fail.
pub fn read_data_enc_key(config_dir: &Path) -> Result<FernetKey, ExportError> {
    let path = key_file_path(config_dir);
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
pub fn read_config(config_dir: &Path) -> Result<Map<String, Value>, ExportError> {
    let path = config_file_path(config_dir);
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

/// Declared setting flags per plugin, from the `<plugins>/*/plugin.toml`
/// manifests (`[[settings]]` keys to their `secret` bit, keyed by the
/// manifest's own `plugin.name`, which need not match the folder name).
/// Unreadable or invalid manifests are skipped: settings whose flags are
/// unknown seal rather than travel verbatim, so a lost manifest fails safe.
/// A missing plugins dir yields an empty map.
#[must_use]
pub fn plugin_setting_flags(plugins_dir: &Path) -> HashMap<String, HashMap<String, bool>> {
    let mut flags: HashMap<String, HashMap<String, bool>> = HashMap::new();
    let entries = match std::fs::read_dir(plugins_dir) {
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
