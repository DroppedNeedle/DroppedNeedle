//! `ConfigStore`: the typed gateway to `config.json`.
//!
//! Reads go through typed getters per section (never raw untyped reads);
//! writes are atomic (temp file + rename + fsync, the v2 `atomic_write_json`
//! shape) and serialized under one write lock (the v2 `_section_save_lock`
//! shape). The file is cached in memory after `open`, like v2's
//! `_config_cache`, and the cache only advances after a durable write.
//! Secret-section saves also hold a second lock across their read and
//! write, so [`ConfigStore::update_secret`] can check the stored section
//! and save in one step.
//!
//! Save discipline per section kind:
//!
//! - plain sections: `save` (validate, normalize, write);
//! - secret sections: `save_secret` (validate, normalize, resolve each mask
//!   against the stored ciphertext, encrypt new values, write);
//! - indexers: `save_indexer` upserts BY ID (a new id is minted when blank),
//!   plus `delete_indexer` and `reorder_indexers`;
//! - plugins: `save_plugin` with the manifest's secret-key set (secret
//!   values encrypted at rest, unlike v2).
//!
//! The store emits no logs at all: there must be no code path on which a
//! secret can reach structured output. See the secrets-never-logged test.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, PoisonError, RwLock};

use super::crypto::Crypto;
use super::error::ConfigError;
use super::mask::{
    INDEXER_API_KEY_MASK, Masked, PLUGIN_SECRET_MASK, SaveResolution, display_mask,
    resolve_for_probe, resolve_on_save,
};
use super::secret_sections::{Indexers, NewznabIndexer, SecretSection};
use super::sections::{PlainSection, PluginConfig, Plugins, Section};
use super::{DROPPED_SECTION_KEYS, KNOWN_TOP_LEVEL_KEYS};

/// Top-level instance-id key (single owner; the v2 `Settings` overwrite
/// quirk is gone).
pub const INSTANCE_ID_KEY: &str = "instance_id";

/// Typed gateway to one `config.json` file. Built once at startup from an
/// injected [`Crypto`] and shared by constructor.
pub struct ConfigStore {
    path: PathBuf,
    crypto: Crypto,
    cache: RwLock<serde_json::Value>,
    /// Held across every secret-section read-modify-write.
    secret_writes: Mutex<()>,
}

/// Why [`ConfigStore::update_secret`] did not save.
#[derive(Debug)]
pub enum UpdateError<E> {
    /// The update refused the change; nothing was written.
    Rejected(E),
    /// Reading or writing the config failed.
    Config(ConfigError),
}

impl<E> From<ConfigError> for UpdateError<E> {
    fn from(error: ConfigError) -> Self {
        Self::Config(error)
    }
}

impl std::fmt::Debug for ConfigStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ConfigStore")
            .field("path", &self.path)
            .finish_non_exhaustive()
    }
}

impl ConfigStore {
    /// Open the store. A missing file starts empty (every section reads as
    /// its default until the first save); an unparseable file fails closed.
    pub fn open(path: &Path, crypto: Crypto) -> Result<Self, ConfigError> {
        let value = match std::fs::read_to_string(path) {
            Ok(text) => serde_json::from_str::<serde_json::Value>(&text).map_err(|error| {
                ConfigError::InvalidJson {
                    reason: error.to_string(),
                }
            })?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                serde_json::Value::Object(serde_json::Map::new())
            }
            Err(error) => {
                return Err(ConfigError::ReadFailed {
                    path: path.to_path_buf(),
                    reason: error.to_string(),
                });
            }
        };
        if !value.is_object() {
            return Err(ConfigError::InvalidJson {
                reason: "config root must be a JSON object".to_owned(),
            });
        }
        Ok(Self {
            path: path.to_path_buf(),
            crypto,
            cache: RwLock::new(value),
            secret_writes: Mutex::new(()),
        })
    }

    /// Sign `message` for `purpose` with the server's data key (see
    /// [`Crypto::mac`]).
    #[must_use]
    pub fn mac(&self, purpose: &str, message: &[u8]) -> [u8; 32] {
        self.crypto.mac(purpose, message)
    }

    /// The config file path.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Read one plain section (missing reads as the default).
    pub fn get<S: Section>(&self) -> Result<S, ConfigError> {
        let mut section = self.decode_section::<S>()?;
        section.normalize();
        Ok(section)
    }

    /// Save one plain section: strict validation first, then normalize and
    /// a durable write. Returns the normalized stored form, so save
    /// responses can echo applied coercions (the MusicBrainz clamp flag).
    pub fn save<S: PlainSection>(&self, incoming: S) -> Result<S, ConfigError> {
        incoming.validate()?;
        let mut normalized = incoming;
        normalized.normalize();
        self.write_section::<S>(&normalized)?;
        Ok(normalized)
    }

    /// Read one secret section with secrets masked (safe for API responses).
    pub fn get_masked<S: SecretSection>(&self) -> Result<Masked<S>, ConfigError> {
        let mut section = self.decode_section::<S>()?;
        self.decrypt_in_place(&mut section)?;
        section.normalize();
        mask_in_place(&mut section);
        Ok(Masked::from_masked(section))
    }

    /// Resolve submitted values for a connection probe: each secret that
    /// still holds its mask becomes the stored plaintext, anything else is
    /// tested as submitted (stripped where a save would strip). Nothing is
    /// written. Server-side only; never serialize the result.
    pub fn unmask<S: SecretSection>(&self, incoming: Masked<S>) -> Result<S, ConfigError> {
        let mut submitted = incoming.into_inner();
        let mut stored = self.decode_section::<S>()?;
        self.decrypt_in_place(&mut stored)?;
        let stored_plain: Vec<String> = stored
            .secret_fields()
            .iter()
            .map(|field| field.value.expose().to_owned())
            .collect();
        for (field, plain) in submitted.secret_fields().into_iter().zip(stored_plain) {
            let resolved = resolve_for_probe(field.value.expose(), field.mask, field.strip, &plain);
            *field.value.expose_mut() = resolved;
        }
        Ok(submitted)
    }

    /// Read one secret section with secrets decrypted (server-side only;
    /// never serialize into a response).
    pub fn get_raw<S: SecretSection>(&self) -> Result<S, ConfigError> {
        let mut section = self.decode_section::<S>()?;
        self.decrypt_in_place(&mut section)?;
        section.normalize();
        Ok(section)
    }

    /// Save one secret section: strict validation first, then normalize,
    /// then per-field mask resolution (mask keeps the stored ciphertext,
    /// anything else is encrypted fresh; empty stays empty). Returns the
    /// saved section masked, so a client that re-saves the echo keeps its
    /// secrets instead of encrypting the mask.
    pub fn save_secret<S: SecretSection>(&self, incoming: S) -> Result<Masked<S>, ConfigError> {
        let _writing = self.lock_secret_writes()?;
        self.save_secret_locked(incoming)
    }

    /// Read, check, and save one secret section with no other secret save
    /// in between. `update` gets the stored section masked and returns the
    /// values to save, which go through the [`ConfigStore::save_secret`]
    /// rules (a mask keeps the stored secret). An `Err` from `update`
    /// writes nothing. `update` must not call back into the store's
    /// secret writes.
    pub fn update_secret<S, E, F>(&self, update: F) -> Result<Masked<S>, UpdateError<E>>
    where
        S: SecretSection,
        F: FnOnce(Masked<S>) -> Result<S, E>,
    {
        let _writing = self.lock_secret_writes()?;
        let current = self.get_masked::<S>()?;
        let proposed = update(current).map_err(UpdateError::Rejected)?;
        Ok(self.save_secret_locked(proposed)?)
    }

    /// The lock guards no data, so a save that panicked while holding it
    /// leaves nothing inconsistent: a poisoned lock is taken as is.
    fn lock_secret_writes(&self) -> Result<MutexGuard<'_, ()>, ConfigError> {
        Ok(self
            .secret_writes
            .lock()
            .unwrap_or_else(PoisonError::into_inner))
    }

    fn save_secret_locked<S: SecretSection>(&self, incoming: S) -> Result<Masked<S>, ConfigError> {
        incoming.validate()?;
        let mut normalized = incoming;
        normalized.normalize();
        let mut stored = self.decode_section::<S>()?;
        let stored_fields: Vec<String> = stored
            .secret_fields()
            .iter()
            .map(|field| field.value.expose().to_owned())
            .collect();
        let mut fresh: Vec<(bool, String)> = Vec::new();
        for (field, stored_ciphertext) in normalized
            .secret_fields()
            .iter_mut()
            .zip(stored_fields.iter())
        {
            let mask = field.mask;
            let strip = field.strip;
            match resolve_on_save(field.value.expose(), mask, strip) {
                SaveResolution::KeepStored => {
                    fresh.push((true, stored_ciphertext.clone()));
                }
                SaveResolution::StoreNew(value) => {
                    fresh.push((false, value));
                }
            }
        }
        for (field, (keep, value)) in normalized.secret_fields().iter_mut().zip(fresh.iter()) {
            if *keep {
                *field.value.expose_mut() = value.clone();
            } else if value.is_empty() {
                field.value.expose_mut().clear();
            } else {
                *field.value.expose_mut() = self.crypto.encrypt(value)?;
            }
        }
        self.write_section::<S>(&normalized)?;
        // Ciphertext is empty exactly when the secret is unset, so masking
        // the stored form gives the same echo as a fresh masked read.
        mask_in_place(&mut normalized);
        Ok(Masked::from_masked(normalized))
    }

    /// All configured indexers, keys masked, ordered by priority.
    pub fn get_indexers(&self) -> Result<Vec<Masked<NewznabIndexer>>, ConfigError> {
        let stored = self.decode_section::<Indexers>()?;
        let mut out = Vec::with_capacity(stored.0.len());
        for mut indexer in stored.0 {
            let plaintext = self.crypto.decrypt(indexer.api_key.expose())?;
            let shown = display_mask(plaintext.trim(), INDEXER_API_KEY_MASK).to_owned();
            *indexer.api_key.expose_mut() = shown;
            out.push(indexer);
        }
        out.sort_by_key(|indexer| indexer.priority);
        Ok(out.into_iter().map(Masked::from_masked).collect())
    }

    /// Resolve one submitted indexer for a caps probe: a masked key
    /// becomes the stored key of the indexer with the same id (empty when
    /// that id is unknown), anything else is tested as submitted.
    pub fn unmask_indexer(
        &self,
        incoming: Masked<NewznabIndexer>,
    ) -> Result<NewznabIndexer, ConfigError> {
        let mut submitted = incoming.into_inner();
        let stored_key = self
            .get_indexers_raw()?
            .into_iter()
            .find(|indexer| indexer.id == submitted.id)
            .map(|indexer| indexer.api_key.expose().to_owned())
            .unwrap_or_default();
        let resolved = resolve_for_probe(
            submitted.api_key.expose(),
            INDEXER_API_KEY_MASK,
            true,
            &stored_key,
        );
        *submitted.api_key.expose_mut() = resolved;
        Ok(submitted)
    }

    /// All configured indexers with keys decrypted and stripped
    /// (server-side only). Stripped on read so a key saved with stray
    /// whitespace before the strip fix still authenticates.
    pub fn get_indexers_raw(&self) -> Result<Vec<NewznabIndexer>, ConfigError> {
        let stored = self.decode_section::<Indexers>()?;
        let mut out = Vec::with_capacity(stored.0.len());
        for mut indexer in stored.0 {
            let plaintext = self.crypto.decrypt(indexer.api_key.expose())?;
            *indexer.api_key.expose_mut() = plaintext.trim().to_owned();
            out.push(indexer);
        }
        out.sort_by_key(|indexer| indexer.priority);
        Ok(out)
    }

    /// Upsert one indexer BY ID (a new id is minted when blank). The key is
    /// encrypted, or preserved when the masked sentinel comes back.
    /// Returns the indexer id.
    pub fn save_indexer(&self, incoming: NewznabIndexer) -> Result<String, ConfigError> {
        let mut row = incoming;
        super::sections::normalize_http_url(&mut row.url, "https://");
        let mut stored = self.decode_section::<Indexers>()?;
        let existing = stored.0.iter().find(|item| item.id == row.id);
        let resolved = match resolve_on_save(row.api_key.expose(), INDEXER_API_KEY_MASK, true) {
            SaveResolution::KeepStored => existing
                .map(|item| item.api_key.expose().to_owned())
                .unwrap_or_default(),
            SaveResolution::StoreNew(value) => {
                if value.is_empty() {
                    String::new()
                } else {
                    self.crypto.encrypt(&value)?
                }
            }
        };
        *row.api_key.expose_mut() = resolved;
        if row.id.is_empty() {
            row.id = uuid::Uuid::new_v4().simple().to_string();
        }
        match stored.0.iter().position(|item| item.id == row.id) {
            Some(index) => stored.0[index] = row.clone(),
            None => stored.0.push(row.clone()),
        }
        self.write_section::<Indexers>(&stored)?;
        Ok(row.id)
    }

    /// Delete one indexer by id. Unknown ids are a silent no-op.
    pub fn delete_indexer(&self, indexer_id: &str) -> Result<(), ConfigError> {
        let stored = self.decode_section::<Indexers>()?;
        let kept: Vec<NewznabIndexer> = stored
            .0
            .into_iter()
            .filter(|item| item.id != indexer_id)
            .collect();
        self.write_section::<Indexers>(&Indexers(kept))
    }

    /// Persist a new priority order (1-based) from the dragged card order.
    /// Ids not listed keep their relative order after the listed ones.
    pub fn reorder_indexers(&self, ordered_ids: &[String]) -> Result<(), ConfigError> {
        let mut stored = self.decode_section::<Indexers>()?;
        for (position, wanted) in ordered_ids.iter().enumerate() {
            if let Some(item) = stored.0.iter_mut().find(|item| &item.id == wanted) {
                item.priority = position as i64 + 1;
            }
        }
        self.write_section::<Indexers>(&stored)
    }

    /// One plugin's stored state (secret-flagged values still ciphertext).
    pub fn get_plugin(&self, name: &str) -> Result<PluginConfig, ConfigError> {
        let stored = self.decode_section::<Plugins>()?;
        Ok(stored.0.get(name).cloned().unwrap_or_default())
    }

    /// One plugin's state with secret-flagged values masked (safe for API
    /// responses). `secret_keys` comes from the plugin manifest.
    pub fn get_plugin_masked(
        &self,
        name: &str,
        secret_keys: &HashSet<String>,
    ) -> Result<PluginConfig, ConfigError> {
        let mut config = self.get_plugin(name)?;
        for (key, value) in config.settings.iter_mut() {
            if secret_keys.contains(key) {
                let plaintext = self.crypto.decrypt(value)?;
                *value = display_mask(&plaintext, PLUGIN_SECRET_MASK).to_owned();
            } else {
                *value = self.open_plain_setting(value);
            }
        }
        Ok(config)
    }

    /// A plugin value the manifest does not flag secret. The v2 import
    /// seals every setting whose v2 manifest was missing, so such a value
    /// can arrive as ciphertext under this install's key; it opens here.
    /// Anything else (whatever a user typed) reads as stored.
    fn open_plain_setting(&self, value: &str) -> String {
        if value.starts_with(crate::runtime_config::crypto::CIPHER_PREFIX)
            && let Ok(plaintext) = self.crypto.decrypt(value)
        {
            return plaintext;
        }
        value.to_owned()
    }

    /// One plugin's state with secret-flagged values decrypted
    /// (server-side only; passed to the plugin host).
    pub fn get_plugin_raw(
        &self,
        name: &str,
        secret_keys: &HashSet<String>,
    ) -> Result<PluginConfig, ConfigError> {
        let mut config = self.get_plugin(name)?;
        for (key, value) in config.settings.iter_mut() {
            if secret_keys.contains(key) {
                *value = self.crypto.decrypt(value)?;
            } else {
                *value = self.open_plain_setting(value);
            }
        }
        Ok(config)
    }

    /// Save one plugin's state. Secret-flagged values resolve the mask
    /// against the stored ciphertext and are encrypted at rest (v2 stored
    /// them plaintext); other values store verbatim, masks included.
    pub fn save_plugin(
        &self,
        name: &str,
        incoming: PluginConfig,
        secret_keys: &HashSet<String>,
    ) -> Result<(), ConfigError> {
        let mut stored = self.decode_section::<Plugins>()?;
        let current = stored.0.get(name).cloned().unwrap_or_default();
        let mut merged = PluginConfig {
            enabled: incoming.enabled,
            settings: std::collections::HashMap::new(),
        };
        for (key, value) in &incoming.settings {
            if secret_keys.contains(key) {
                match resolve_on_save(value, PLUGIN_SECRET_MASK, false) {
                    SaveResolution::KeepStored => {
                        merged.settings.insert(
                            key.clone(),
                            current.settings.get(key).cloned().unwrap_or_default(),
                        );
                    }
                    SaveResolution::StoreNew(plain) => {
                        let sealed = if plain.is_empty() {
                            String::new()
                        } else {
                            self.crypto.encrypt(&plain)?
                        };
                        merged.settings.insert(key.clone(), sealed);
                    }
                }
            } else {
                merged.settings.insert(key.clone(), value.clone());
            }
        }
        stored.0.insert(name.to_owned(), merged);
        self.write_section::<Plugins>(&stored)
    }

    /// The instance id, or "" when never minted. The config file is the
    /// single owner.
    pub fn instance_id(&self) -> Result<String, ConfigError> {
        let guard = self
            .cache
            .read()
            .map_err(|_| ConfigError::LockUnavailable("config cache"))?;
        match guard.get(INSTANCE_ID_KEY) {
            None | Some(serde_json::Value::Null) => Ok(String::new()),
            Some(serde_json::Value::String(id)) => Ok(id.clone()),
            Some(_) => Err(ConfigError::SectionDecode {
                section: INSTANCE_ID_KEY,
                reason: "instance_id must be a string".to_owned(),
            }),
        }
    }

    /// The instance id, minting and persisting one on first call.
    pub fn ensure_instance_id(&self) -> Result<String, ConfigError> {
        let current = self.instance_id()?;
        if !current.is_empty() {
            return Ok(current);
        }
        let minted = uuid::Uuid::new_v4().to_string();
        let mut guard = self
            .cache
            .write()
            .map_err(|_| ConfigError::LockUnavailable("config cache"))?;
        if let Some(serde_json::Value::String(existing)) = guard.get(INSTANCE_ID_KEY)
            && !existing.is_empty()
        {
            return Ok(existing.clone());
        }
        let mut snapshot = guard.clone();
        let object = snapshot.as_object_mut().ok_or(ConfigError::InvalidJson {
            reason: "config root must be a JSON object".to_owned(),
        })?;
        object.insert(
            INSTANCE_ID_KEY.to_owned(),
            serde_json::Value::String(minted.clone()),
        );
        write_json_atomically(&self.path, &snapshot)?;
        *guard = snapshot;
        Ok(minted)
    }

    /// Top-level keys v3 does not own (typos, future keys, leftovers).
    /// For boot diagnostics; unknown keys never fail a read.
    pub fn unknown_top_level_keys(&self) -> Result<Vec<String>, ConfigError> {
        let guard = self
            .cache
            .read()
            .map_err(|_| ConfigError::LockUnavailable("config cache"))?;
        let object = guard.as_object().cloned().unwrap_or_default();
        let mut unknown: Vec<String> = object
            .keys()
            .filter(|key| !KNOWN_TOP_LEVEL_KEYS.contains(&key.as_str()))
            .cloned()
            .collect();
        unknown.sort();
        Ok(unknown)
    }

    /// Dropped sections still present in the file (a v2 file opened
    /// directly, or a hand edit). The import validator rejects these in
    /// exports; here they are reported, never read.
    pub fn dropped_sections_present(&self) -> Result<Vec<&'static str>, ConfigError> {
        let guard = self
            .cache
            .read()
            .map_err(|_| ConfigError::LockUnavailable("config cache"))?;
        Ok(DROPPED_SECTION_KEYS
            .iter()
            .filter(|key| guard.get(**key).is_some())
            .copied()
            .collect())
    }

    fn decode_section<S: Section>(&self) -> Result<S, ConfigError> {
        let guard = self
            .cache
            .read()
            .map_err(|_| ConfigError::LockUnavailable("config cache"))?;
        match guard.get(S::KEY) {
            None | Some(serde_json::Value::Null) => Ok(S::default()),
            Some(raw) => serde_json::from_value::<S>(raw.clone()).map_err(|error| {
                ConfigError::SectionDecode {
                    section: S::KEY,
                    reason: error.to_string(),
                }
            }),
        }
    }

    fn decrypt_in_place<S: SecretSection>(&self, section: &mut S) -> Result<(), ConfigError> {
        for field in section.secret_fields() {
            let strip = field.strip;
            let plaintext = self.crypto.decrypt(field.value.expose())?;
            let cleaned = if strip {
                plaintext.trim().to_owned()
            } else {
                plaintext
            };
            *field.value.expose_mut() = cleaned;
        }
        Ok(())
    }

    fn write_section<S: Section>(&self, section: &S) -> Result<(), ConfigError> {
        let value = serde_json::to_value(section).map_err(|error| ConfigError::SectionDecode {
            section: S::KEY,
            reason: error.to_string(),
        })?;
        let mut guard = self
            .cache
            .write()
            .map_err(|_| ConfigError::LockUnavailable("config cache"))?;
        let mut snapshot = guard.clone();
        let object = snapshot.as_object_mut().ok_or(ConfigError::SectionDecode {
            section: S::KEY,
            reason: "config root must be a JSON object".to_owned(),
        })?;
        object.insert(S::KEY.to_owned(), value);
        write_json_atomically(&self.path, &snapshot)?;
        *guard = snapshot;
        Ok(())
    }
}

/// Replace every set secret with its mask (unset stays empty).
fn mask_in_place<S: SecretSection>(section: &mut S) {
    for field in section.secret_fields() {
        let shown = display_mask(field.value.expose(), field.mask).to_owned();
        *field.value.expose_mut() = shown;
    }
}

/// Atomic JSON write: temp file in the same directory, fsync, rename,
/// directory fsync (the v2 `atomic_write_json` shape). Temp leftovers are
/// removed on failure.
fn write_json_atomically(path: &Path, value: &serde_json::Value) -> Result<(), ConfigError> {
    use std::io::Write as _;

    let failed = |reason: String| ConfigError::WriteFailed {
        path: path.to_path_buf(),
        reason,
    };
    if let Some(parent) = path.parent() {
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
fn fsync_parent(path: &Path) -> std::io::Result<()> {
    let parent = path.parent().unwrap_or(Path::new("."));
    std::fs::File::open(parent)?.sync_all()
}

#[cfg(not(unix))]
fn fsync_parent(_path: &Path) -> std::io::Result<()> {
    Ok(())
}
