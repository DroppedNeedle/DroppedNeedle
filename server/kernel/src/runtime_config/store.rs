//! `ConfigStore`: the typed gateway to the settings rows.
//!
//! Every section is one row of `config_sections`, with a revision that goes
//! up by one with each change. The store loads every row once when it
//! opens, and reads come from that snapshot: a read never waits for the
//! database, so code inside another write may read settings freely.
//!
//! Every change is one durable write transaction on the writer lane that
//! reads the stored row, applies the change and writes it back. Two saves
//! that touch the same section run one after the other, each seeing the
//! other's result, so neither is lost. After the commit the snapshot takes
//! the new value, and only ever moves to a higher revision, and the
//! section's change notice fires. A change that leaves the stored text as
//! it was writes nothing and fires nothing.
//!
//! A section comes in three forms: [`Plain`] (secrets decrypted, for the
//! server's own use, never serialized), [`Masked`] (the API form) and the
//! stored form, which only this store writes. Services hold a [`Live`]
//! handle for the sections they read, never a copied value.
//!
//! Save discipline per section kind:
//!
//! - plain sections: `save` (validate, normalize, write). It never decodes
//!   the stored row, so saving repairs a section that no longer decodes;
//! - secret sections: `save_secret` (validate, normalize, resolve each mask
//!   against the stored ciphertext, encrypt new values, write);
//! - indexers: `save_indexer_blocking` upserts BY ID (a new id is minted
//!   when blank), plus `delete_indexer_blocking` and
//!   `reorder_indexers_blocking`;
//! - plugins: `save_plugin_blocking` with the manifest's secret-key set
//!   (secret values encrypted at rest).
//!
//! Methods ending `_blocking` wait for their turn on the calling thread, so
//! they are for synchronous code on a blocking thread. The others are for
//! async code and run the same write on the blocking pool.
//!
//! There is one store per process: a second one over the same database
//! would serve a snapshot the first one's writes never reach. The running
//! server never deletes a row (a row deleted and stored again would restart
//! its revision).
//!
//! The store logs one line only: [`Live::read`] warns once per revision of
//! a section that does not read, with the section key and the error (which
//! quotes at most a stored non-secret value or ciphertext, never a secret).
//! The one-time copy of `config.json` ([`super::ingest`]) logs key names
//! and counts only.

use std::any::Any;
use std::collections::{HashMap, HashSet};
use std::marker::PhantomData;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock, PoisonError, RwLock};

use tokio::sync::watch;

use crate::clock::Clock;
use crate::db::{Db, DbError, DurableLane};

use super::crypto::Crypto;
use super::error::ConfigError;
use super::ingest::{self, LegacyConfig};
use super::mask::{Masked, SaveResolution, display_mask, resolve_for_probe, resolve_on_save};
use super::plain::{Plain, PluginValues};
use super::registry::{self, Listed};
use super::secret_sections::{Indexers, NewznabIndexer, SecretSection};
use super::sections::{PlainSection, PluginConfig, Plugins, Section};
use super::sql;

/// Top-level instance-id key (single owner; the v2 `Settings` overwrite
/// quirk is gone).
pub const INSTANCE_ID_KEY: &str = "instance_id";

/// Typed gateway to the settings rows. Built once at startup from the
/// database handle, an injected [`Crypto`] and the clock, and shared by
/// constructor. Cloning shares the same snapshot.
#[derive(Clone)]
pub struct ConfigStore {
    inner: Arc<Inner>,
}

struct Inner {
    db: Db,
    crypto: Crypto,
    clock: Arc<dyn Clock>,
    snapshot: RwLock<HashMap<String, Arc<Entry>>>,
    /// One change notice per listed section, carrying its revision. Built
    /// when the store opens; never added to later.
    notices: HashMap<&'static str, watch::Sender<i64>>,
}

/// One section as last committed. A write replaces the entry whole, so the
/// opened value cached here always belongs to this revision.
struct Entry {
    value: serde_json::Value,
    revision: i64,
    opened: OnceLock<Arc<dyn Any + Send + Sync>>,
    /// Set once a failed read of this revision has been logged.
    failure_logged: AtomicBool,
}

impl Entry {
    fn new(value: serde_json::Value, revision: i64) -> Arc<Self> {
        Arc::new(Self {
            value,
            revision,
            opened: OnceLock::new(),
            failure_logged: AtomicBool::new(false),
        })
    }
}

/// Why an update did not save.
#[derive(Debug)]
pub enum UpdateError<E> {
    /// The update refused the change; nothing was written.
    Rejected(E),
    /// Reading or writing the settings failed.
    Config(ConfigError),
}

impl<E> From<ConfigError> for UpdateError<E> {
    fn from(error: ConfigError) -> Self {
        Self::Config(error)
    }
}

impl<E> From<DbError> for UpdateError<E> {
    fn from(error: DbError) -> Self {
        Self::Config(ConfigError::from(error))
    }
}

impl<E: std::fmt::Display> std::fmt::Display for UpdateError<E> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Rejected(error) => error.fmt(f),
            Self::Config(error) => error.fmt(f),
        }
    }
}

impl std::fmt::Debug for ConfigStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ConfigStore").finish_non_exhaustive()
    }
}

/// What a change decided about its row.
enum Change<R> {
    /// Store this value.
    Write(serde_json::Value, R),
    /// Leave the row as it is.
    Keep(R),
}

/// A secret section in its stored form: every secret is ciphertext or
/// empty. Only the store builds one, and it is what the store writes.
#[derive(serde::Serialize)]
#[serde(transparent)]
pub(super) struct Sealed<S>(S);

/// A cheap handle on one section that always reads the current value.
/// Services hold one for each section they read, never a copied value.
pub struct Live<S> {
    store: ConfigStore,
    section: PhantomData<fn() -> S>,
}

impl<S> Clone for Live<S> {
    fn clone(&self) -> Self {
        Self {
            store: self.store.clone(),
            section: PhantomData,
        }
    }
}

impl<S> std::fmt::Debug for Live<S> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Live").finish_non_exhaustive()
    }
}

impl<S: Listed> Live<S> {
    /// The section as last committed (the default when none is stored),
    /// secrets decrypted. Opened once per revision and shared after that.
    pub fn get(&self) -> Result<Arc<Plain<S>>, ConfigError> {
        let entry = self.store.entry(S::KEY);
        if let Some(cached) = entry.as_ref().and_then(|entry| entry.opened.get())
            && let Ok(opened) = Arc::clone(cached).downcast::<Plain<S>>()
        {
            return Ok(opened);
        }
        let opened = Arc::new(S::open(
            &self.store,
            entry.as_ref().map(|entry| &entry.value),
        )?);
        if let Some(entry) = entry {
            let shared: Arc<dyn Any + Send + Sync> = opened.clone();
            let _ = entry.opened.set(shared);
        }
        Ok(opened)
    }

    /// The section as [`Live::get`] reads it, or `None` when it does not
    /// read. A section that does not read is never its defaults: the work
    /// it controls waits until it is saved again. The failure is logged
    /// once per revision.
    pub fn read(&self) -> Option<Arc<Plain<S>>> {
        match self.get() {
            Ok(section) => Some(section),
            Err(error) => {
                let first = self
                    .store
                    .entry(S::KEY)
                    .is_none_or(|entry| !entry.failure_logged.swap(true, Ordering::Relaxed));
                if first {
                    tracing::warn!(
                        section = S::KEY,
                        %error,
                        "settings section cannot be read; the work it controls waits until it is saved again in Settings"
                    );
                }
                None
            }
        }
    }

    /// Wakes after each committed change to this section.
    #[must_use]
    pub fn subscribe(&self) -> watch::Receiver<i64> {
        self.store.subscribe(S::KEY)
    }
}

impl ConfigStore {
    /// Open the store on `db`: first copy in a `config.json` this data
    /// directory's settings have never seen ([`super::ingest`]), then load
    /// every row. A file that cannot be read stops the open.
    pub async fn open(
        db: Db,
        crypto: Crypto,
        clock: Arc<dyn Clock>,
        legacy: LegacyConfig,
    ) -> Result<Self, ConfigError> {
        match tokio::task::spawn_blocking(move || Self::open_blocking(db, crypto, clock, &legacy))
            .await
        {
            Ok(opened) => opened,
            Err(error) => Err(ConfigError::Database(format!(
                "opening the settings stopped: {error}"
            ))),
        }
    }

    /// [`ConfigStore::open`] for a blocking thread.
    pub fn open_blocking(
        db: Db,
        crypto: Crypto,
        clock: Arc<dyn Clock>,
        legacy: &LegacyConfig,
    ) -> Result<Self, ConfigError> {
        ingest::run(&db, legacy, clock.as_ref())?;
        let rows = db.read_blocking("config.load", |conn| {
            let mut statement = conn
                .prepare_cached(sql::ALL_SECTIONS.text())
                .map_err(DbError::from)?;
            let rows = statement
                .query_map([], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, i64>(2)?,
                    ))
                })
                .map_err(DbError::from)?
                .collect::<Result<Vec<_>, _>>()
                .map_err(DbError::from)?;
            Ok::<_, ConfigError>(rows)
        })?;
        let mut snapshot = HashMap::with_capacity(rows.len());
        for (key, text, revision) in rows {
            let value = decode_text(&text)?;
            snapshot.insert(key, Entry::new(value, revision));
        }
        let mut notices = HashMap::with_capacity(registry::SECTION_KEYS.len());
        for key in registry::SECTION_KEYS {
            let revision = snapshot
                .entry((*key).to_owned())
                .or_insert_with(|| Entry::new(serde_json::Value::Null, 0))
                .revision;
            notices.insert(*key, watch::Sender::new(revision));
        }
        Ok(Self {
            inner: Arc::new(Inner {
                db,
                crypto,
                clock,
                snapshot: RwLock::new(snapshot),
                notices,
            }),
        })
    }

    /// A live handle on one section.
    #[must_use]
    pub fn live<S: Listed>(&self) -> Live<S> {
        Live {
            store: self.clone(),
            section: PhantomData,
        }
    }

    /// Wakes after each committed change to the section stored under
    /// `key`. A key off the section list has no notice: its receiver is
    /// already closed, so `changed()` answers `Err` at once.
    #[must_use]
    pub fn subscribe(&self, key: &str) -> watch::Receiver<i64> {
        match self.inner.notices.get(key) {
            Some(notice) => notice.subscribe(),
            None => watch::Sender::new(0).subscribe(),
        }
    }

    /// Sign `message` for `purpose` with the server's data key (see
    /// [`Crypto::mac`]).
    #[must_use]
    pub fn mac(&self, purpose: &str, message: &[u8]) -> [u8; 32] {
        self.inner.crypto.mac(purpose, message)
    }

    /// Read one plain section (missing reads as the default).
    pub fn get<S: PlainSection>(&self) -> Result<S, ConfigError> {
        let mut section = self.decode_section::<S>()?;
        section.normalize();
        Ok(section)
    }

    /// The stored revision of one section, 0 when it has never been stored.
    #[must_use]
    pub fn revision(&self, key: &str) -> i64 {
        self.entry(key).map_or(0, |entry| entry.revision)
    }

    /// Save one plain section: strict validation first, then normalize and
    /// a durable write. Returns the normalized stored form, so save
    /// responses can echo applied coercions (the MusicBrainz clamp flag).
    pub async fn save<S: PlainSection + Send + 'static>(
        &self,
        incoming: S,
    ) -> Result<S, ConfigError> {
        let store = self.clone();
        off_thread(move || store.save_blocking(incoming)).await
    }

    /// [`ConfigStore::save`] for a blocking thread.
    pub fn save_blocking<S: PlainSection>(&self, incoming: S) -> Result<S, ConfigError> {
        incoming.validate()?;
        let mut normalized = incoming;
        normalized.normalize();
        let value = encode_section(&normalized)?;
        self.change(DurableLane::Foreground, "config.save", S::KEY, |_| {
            Ok::<_, ConfigError>(Change::Write(value, ()))
        })?;
        Ok(normalized)
    }

    /// Store a section's default when none is stored, so a default that
    /// mints values (the MusicBrainz source id) is minted once instead of
    /// on every read. A section already stored is left as it is.
    pub async fn seed_default<S: PlainSection + Send + 'static>(&self) -> Result<(), ConfigError> {
        let store = self.clone();
        off_thread(move || store.seed_default_blocking::<S>()).await
    }

    /// [`ConfigStore::seed_default`] for a blocking thread.
    pub fn seed_default_blocking<S: PlainSection>(&self) -> Result<(), ConfigError> {
        let mut section = S::default();
        section.normalize();
        let value = encode_section(&section)?;
        self.change(DurableLane::Background, "config.seed", S::KEY, |stored| {
            Ok::<_, ConfigError>(match stored {
                None | Some(serde_json::Value::Null) => Change::Write(value, ()),
                Some(_) => Change::Keep(()),
            })
        })
    }

    /// Read, check and save one plain section in one transaction. `update`
    /// gets the stored section (normalized; the default when none is
    /// stored) and returns the section to save, which is validated and
    /// normalized like [`ConfigStore::save`]. An `Err` from `update`
    /// writes nothing.
    ///
    /// `update` runs inside the write transaction, so it must be pure: no
    /// call on this store, no database, no file or network access. A value
    /// it needs from another section is read before the call and moved in.
    pub async fn update<S, E, F>(&self, lane: DurableLane, update: F) -> Result<S, UpdateError<E>>
    where
        S: PlainSection + Send + 'static,
        E: std::fmt::Display + Send + 'static,
        F: FnOnce(S) -> Result<S, E> + Send + 'static,
    {
        let store = self.clone();
        match tokio::task::spawn_blocking(move || store.update_blocking(lane, update)).await {
            Ok(updated) => updated,
            Err(error) => Err(UpdateError::Config(ConfigError::Database(format!(
                "the settings write stopped: {error}"
            )))),
        }
    }

    /// [`ConfigStore::update`] for a blocking thread.
    pub fn update_blocking<S, E, F>(
        &self,
        lane: DurableLane,
        update: F,
    ) -> Result<S, UpdateError<E>>
    where
        S: PlainSection,
        E: std::fmt::Display,
        F: FnOnce(S) -> Result<S, E>,
    {
        self.change(lane, "config.update", S::KEY, |stored| {
            let mut current: S = decode_value(S::KEY, stored.as_ref())?;
            current.normalize();
            let mut proposed = update(current).map_err(UpdateError::Rejected)?;
            proposed.validate()?;
            proposed.normalize();
            let value = encode_section(&proposed)?;
            Ok(Change::Write(value, proposed))
        })
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
    /// still holds the mask becomes the stored plaintext, anything else is
    /// tested as submitted (stripped where a save would strip). Nothing is
    /// written.
    pub fn unmask<S: SecretSection>(&self, incoming: Masked<S>) -> Result<Plain<S>, ConfigError> {
        let mut submitted = incoming.into_inner();
        let mut stored = self.decode_section::<S>()?;
        self.decrypt_in_place(&mut stored)?;
        let stored_plain: Vec<String> = stored
            .secret_fields()
            .iter()
            .map(|field| field.value.expose().to_owned())
            .collect();
        for (field, plain) in submitted.secret_fields().into_iter().zip(stored_plain) {
            let resolved = resolve_for_probe(field.value.expose(), field.strip, &plain);
            field.value.open(resolved);
        }
        Ok(Plain::opened(submitted))
    }

    /// Read one secret section with secrets decrypted, for the server's own
    /// use. Services that keep reading a section hold a [`Live`] instead.
    pub fn get_raw<S: SecretSection>(&self) -> Result<Plain<S>, ConfigError> {
        let value = self.entry(S::KEY);
        self.open_secret(value.as_ref().map(|entry| &entry.value))
    }

    /// Save one secret section: strict validation first, then normalize,
    /// then per-field mask resolution against the stored row (the mask
    /// keeps the stored ciphertext, anything else is encrypted fresh; empty
    /// stays empty). Returns the saved section masked, so a client that
    /// re-saves the echo keeps its secrets instead of encrypting the mask.
    pub async fn save_secret<S: SecretSection + Send + 'static>(
        &self,
        incoming: S,
    ) -> Result<Masked<S>, ConfigError> {
        let store = self.clone();
        off_thread(move || store.save_secret_blocking(incoming)).await
    }

    /// [`ConfigStore::save_secret`] for a blocking thread.
    pub fn save_secret_blocking<S: SecretSection>(
        &self,
        incoming: S,
    ) -> Result<Masked<S>, ConfigError> {
        incoming.validate()?;
        let mut normalized = incoming;
        normalized.normalize();
        self.change(
            DurableLane::Foreground,
            "config.save_secret",
            S::KEY,
            |stored| {
                let stored: S = decode_value(S::KEY, stored.as_ref())?;
                let sealed = self.seal(normalized, stored)?;
                Ok::<_, ConfigError>(Change::Write(encode_sealed::<S>(&sealed)?, sealed))
            },
        )
        .map(masked_echo)
    }

    /// Read, check and save one secret section in one transaction.
    /// `update` gets the stored section masked and returns the values to
    /// save, which go through the [`ConfigStore::save_secret`] rules (the
    /// mask keeps the stored secret). An `Err` from `update` writes nothing.
    ///
    /// `update` runs inside the write transaction, so it must be pure: no
    /// call on this store, no database, no file or network access.
    pub async fn update_secret<S, E, F>(&self, update: F) -> Result<Masked<S>, UpdateError<E>>
    where
        S: SecretSection + Send + 'static,
        E: std::fmt::Display + Send + 'static,
        F: FnOnce(Masked<S>) -> Result<S, E> + Send + 'static,
    {
        let store = self.clone();
        match tokio::task::spawn_blocking(move || store.update_secret_blocking(update)).await {
            Ok(updated) => updated,
            Err(error) => Err(UpdateError::Config(ConfigError::Database(format!(
                "the settings write stopped: {error}"
            )))),
        }
    }

    /// [`ConfigStore::update_secret`] for a blocking thread.
    pub fn update_secret_blocking<S, E, F>(&self, update: F) -> Result<Masked<S>, UpdateError<E>>
    where
        S: SecretSection,
        E: std::fmt::Display,
        F: FnOnce(Masked<S>) -> Result<S, E>,
    {
        self.change(
            DurableLane::Foreground,
            "config.update_secret",
            S::KEY,
            |stored| {
                let mut current: S = decode_value(S::KEY, stored.as_ref())?;
                let stored: S = decode_value(S::KEY, stored.as_ref())?;
                self.decrypt_in_place(&mut current)?;
                current.normalize();
                mask_in_place(&mut current);
                let proposed =
                    update(Masked::from_masked(current)).map_err(UpdateError::Rejected)?;
                proposed.validate()?;
                let mut normalized = proposed;
                normalized.normalize();
                let sealed = self.seal(normalized, stored)?;
                Ok(Change::Write(encode_sealed::<S>(&sealed)?, sealed))
            },
        )
        .map(masked_echo)
    }

    /// All configured indexers, keys masked, ordered by priority.
    pub fn get_indexers(&self) -> Result<Vec<Masked<NewznabIndexer>>, ConfigError> {
        let stored = self.decode_section::<Indexers>()?;
        let mut out = Vec::with_capacity(stored.0.len());
        for mut indexer in stored.0 {
            let plaintext = self.inner.crypto.decrypt(indexer.api_key.expose())?;
            let shown = display_mask(plaintext.trim()).to_owned();
            indexer.api_key.close(shown);
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
    ) -> Result<Plain<NewznabIndexer>, ConfigError> {
        let mut submitted = incoming.into_inner();
        let stored_key = self
            .get_indexers_raw()?
            .into_iter()
            .find(|indexer| indexer.id == submitted.id)
            .map(|indexer| indexer.api_key.expose().to_owned())
            .unwrap_or_default();
        let resolved = resolve_for_probe(submitted.api_key.expose(), true, &stored_key);
        submitted.api_key.open(resolved);
        Ok(Plain::opened(submitted))
    }

    /// All configured indexers with keys decrypted and stripped
    /// (server-side only). Stripped on read so a key saved with stray
    /// whitespace before the strip fix still authenticates.
    pub fn get_indexers_raw(&self) -> Result<Vec<Plain<NewznabIndexer>>, ConfigError> {
        let stored = self.decode_section::<Indexers>()?;
        let mut out = Vec::with_capacity(stored.0.len());
        for mut indexer in stored.0 {
            let plaintext = self.inner.crypto.decrypt(indexer.api_key.expose())?;
            indexer.api_key.open(plaintext.trim().to_owned());
            out.push(indexer);
        }
        out.sort_by_key(|indexer| indexer.priority);
        Ok(out.into_iter().map(Plain::opened).collect())
    }

    /// Upsert one indexer BY ID (a new id is minted when blank). The key is
    /// encrypted, or preserved when the mask comes back. Returns the
    /// indexer id.
    pub fn save_indexer_blocking(&self, incoming: NewznabIndexer) -> Result<String, ConfigError> {
        let mut row = incoming;
        super::sections::normalize_http_url(&mut row.url, "https://");
        if row.id.is_empty() {
            row.id = uuid::Uuid::new_v4().simple().to_string();
        }
        let resolution = resolve_on_save(row.api_key.expose(), true);
        let fresh = match &resolution {
            SaveResolution::StoreNew(value) if !value.is_empty() => {
                Some(self.inner.crypto.encrypt(value)?)
            }
            _ => None,
        };
        self.change(
            DurableLane::Foreground,
            "config.save_indexer",
            Indexers::KEY,
            |stored| {
                let mut stored: Indexers = decode_value(Indexers::KEY, stored.as_ref())?;
                let existing = stored.0.iter().find(|item| item.id == row.id);
                let sealed = match resolution {
                    SaveResolution::KeepStored => existing
                        .map(|item| item.api_key.expose().to_owned())
                        .unwrap_or_default(),
                    SaveResolution::StoreNew(_) => fresh.unwrap_or_default(),
                };
                row.api_key.close(sealed);
                let id = row.id.clone();
                match stored.0.iter().position(|item| item.id == row.id) {
                    Some(index) => stored.0[index] = row,
                    None => stored.0.push(row),
                }
                Ok::<_, ConfigError>(Change::Write(encode_section(&stored)?, id))
            },
        )
    }

    /// Delete one indexer by id. Unknown ids are a silent no-op.
    pub fn delete_indexer_blocking(&self, indexer_id: &str) -> Result<(), ConfigError> {
        self.change(
            DurableLane::Foreground,
            "config.delete_indexer",
            Indexers::KEY,
            |stored| {
                let stored: Indexers = decode_value(Indexers::KEY, stored.as_ref())?;
                let kept: Vec<NewznabIndexer> = stored
                    .0
                    .into_iter()
                    .filter(|item| item.id != indexer_id)
                    .collect();
                Ok::<_, ConfigError>(Change::Write(encode_section(&Indexers(kept))?, ()))
            },
        )
    }

    /// Persist a new priority order (1-based) from the dragged card order.
    /// Ids not listed keep their relative order after the listed ones.
    pub fn reorder_indexers_blocking(&self, ordered_ids: &[String]) -> Result<(), ConfigError> {
        self.change(
            DurableLane::Foreground,
            "config.reorder_indexers",
            Indexers::KEY,
            |stored| {
                let mut stored: Indexers = decode_value(Indexers::KEY, stored.as_ref())?;
                for (position, wanted) in ordered_ids.iter().enumerate() {
                    if let Some(item) = stored.0.iter_mut().find(|item| &item.id == wanted) {
                        item.priority = position as i64 + 1;
                    }
                }
                Ok::<_, ConfigError>(Change::Write(encode_section(&stored)?, ()))
            },
        )
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
                let plaintext = self.inner.crypto.decrypt(value)?;
                *value = display_mask(&plaintext).to_owned();
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
            && let Ok(plaintext) = self.inner.crypto.decrypt(value)
        {
            return plaintext;
        }
        value.to_owned()
    }

    /// One plugin's state with secret-flagged values decrypted, for the
    /// plugin host to hand to the plugin.
    pub fn get_plugin_raw(
        &self,
        name: &str,
        secret_keys: &HashSet<String>,
    ) -> Result<PluginValues, ConfigError> {
        let mut config = self.get_plugin(name)?;
        for (key, value) in config.settings.iter_mut() {
            if secret_keys.contains(key) {
                *value = self.inner.crypto.decrypt(value)?;
            } else {
                *value = self.open_plain_setting(value);
            }
        }
        Ok(PluginValues::opened(config.settings))
    }

    /// Save one plugin's state. Secret-flagged values resolve the mask
    /// against the stored ciphertext and are encrypted at rest (v2 stored
    /// them plaintext); other values store verbatim, masks included.
    pub fn save_plugin_blocking(
        &self,
        name: &str,
        incoming: PluginConfig,
        secret_keys: &HashSet<String>,
    ) -> Result<(), ConfigError> {
        let mut sealed_new = HashMap::new();
        for (key, value) in &incoming.settings {
            if secret_keys.contains(key)
                && let SaveResolution::StoreNew(plain) = resolve_on_save(value, false)
                && !plain.is_empty()
            {
                sealed_new.insert(key.clone(), self.inner.crypto.encrypt(&plain)?);
            }
        }
        self.change(
            DurableLane::Foreground,
            "config.save_plugin",
            Plugins::KEY,
            |stored| {
                let mut stored: Plugins = decode_value(Plugins::KEY, stored.as_ref())?;
                let current = stored.0.get(name).cloned().unwrap_or_default();
                let mut merged = PluginConfig {
                    enabled: incoming.enabled,
                    settings: HashMap::new(),
                };
                for (key, value) in &incoming.settings {
                    if secret_keys.contains(key) {
                        let kept = match resolve_on_save(value, false) {
                            SaveResolution::KeepStored => {
                                current.settings.get(key).cloned().unwrap_or_default()
                            }
                            SaveResolution::StoreNew(_) => {
                                sealed_new.get(key).cloned().unwrap_or_default()
                            }
                        };
                        merged.settings.insert(key.clone(), kept);
                    } else {
                        merged.settings.insert(key.clone(), value.clone());
                    }
                }
                stored.0.insert(name.to_owned(), merged);
                Ok::<_, ConfigError>(Change::Write(encode_section(&stored)?, ()))
            },
        )
    }

    /// Open a plain section from its stored value, for [`Live`].
    pub(super) fn open_plain<S: Section>(
        &self,
        value: Option<&serde_json::Value>,
    ) -> Result<Plain<S>, ConfigError> {
        let mut section: S = decode_value(S::KEY, value)?;
        section.normalize();
        Ok(Plain::opened(section))
    }

    /// Open a secret section from its stored value: decode, decrypt,
    /// normalize.
    pub(super) fn open_secret<S: SecretSection>(
        &self,
        value: Option<&serde_json::Value>,
    ) -> Result<Plain<S>, ConfigError> {
        let mut section: S = decode_value(S::KEY, value)?;
        self.decrypt_in_place(&mut section)?;
        section.normalize();
        Ok(Plain::opened(section))
    }

    /// One durable transaction on `key`'s row: `apply` gets the stored
    /// value (`None` when the row is absent) and decides. A value whose
    /// text equals the stored text is not written. After the commit the
    /// snapshot takes the written value at its new revision and the
    /// section's notice fires.
    fn change<R, E, F>(
        &self,
        lane: DurableLane,
        name: &'static str,
        key: &str,
        apply: F,
    ) -> Result<R, E>
    where
        F: FnOnce(Option<serde_json::Value>) -> Result<Change<R>, E>,
        E: From<DbError> + From<ConfigError> + std::fmt::Display,
    {
        let now = self.inner.clock.now_ms();
        let (written, out) = self.inner.db.write_durable(lane, name, |tx| {
            let stored: Option<(String, i64)> = tx
                .query_row(sql::ONE_SECTION.text(), [key], |row| {
                    Ok((row.get(0)?, row.get(1)?))
                })
                .map(Some)
                .or_else(|error| match error {
                    rusqlite::Error::QueryReturnedNoRows => Ok(None),
                    other => Err(other),
                })
                .map_err(DbError::from)?;
            let stored_value = match &stored {
                Some((text, _)) => Some(decode_text(text)?),
                None => None,
            };
            match apply(stored_value)? {
                Change::Keep(out) => Ok::<_, E>((None, out)),
                Change::Write(value, out) => {
                    let text = encode_text(&value)?;
                    if stored.as_ref().is_some_and(|(old, _)| *old == text) {
                        return Ok((None, out));
                    }
                    let revision: i64 = tx
                        .query_row(
                            sql::PUT_SECTION.text(),
                            rusqlite::params![key, text, now],
                            |row| row.get(0),
                        )
                        .map_err(DbError::from)?;
                    Ok((Some((value, revision)), out))
                }
            }
        })?;
        if let Some((value, revision)) = written {
            let newer = {
                let mut snapshot = self
                    .inner
                    .snapshot
                    .write()
                    .unwrap_or_else(PoisonError::into_inner);
                let newer = snapshot
                    .get(key)
                    .is_none_or(|current| current.revision < revision);
                if newer {
                    snapshot.insert(key.to_owned(), Entry::new(value, revision));
                }
                newer
            };
            // Two writes to one key can reach here in either order once the
            // lock is released, so the notice only ever moves forward.
            if newer && let Some(notice) = self.inner.notices.get(key) {
                notice.send_if_modified(|seen| {
                    let forward = revision > *seen;
                    if forward {
                        *seen = revision;
                    }
                    forward
                });
            }
        }
        Ok(out)
    }

    /// Mask resolution and encryption of one secret section against its
    /// stored (sealed) form.
    pub(super) fn seal<S: SecretSection>(
        &self,
        mut normalized: S,
        mut stored: S,
    ) -> Result<Sealed<S>, ConfigError> {
        let stored_fields: Vec<String> = stored
            .secret_fields()
            .iter()
            .map(|field| field.value.expose().to_owned())
            .collect();
        for (field, stored_ciphertext) in normalized.secret_fields().into_iter().zip(stored_fields)
        {
            match resolve_on_save(field.value.expose(), field.strip) {
                SaveResolution::KeepStored => field.value.close(stored_ciphertext),
                SaveResolution::StoreNew(value) if value.is_empty() => {
                    field.value.close(String::new());
                }
                SaveResolution::StoreNew(value) => {
                    let sealed = self.inner.crypto.encrypt(&value)?;
                    field.value.close(sealed);
                }
            }
        }
        Ok(Sealed(normalized))
    }

    fn entry(&self, key: &str) -> Option<Arc<Entry>> {
        // Entries are replaced whole, so a writer that panicked left none
        // half-written.
        self.inner
            .snapshot
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .get(key)
            .cloned()
    }

    fn decode_section<S: Section>(&self) -> Result<S, ConfigError> {
        let entry = self.entry(S::KEY);
        decode_value(S::KEY, entry.as_ref().map(|entry| &entry.value))
    }

    fn decrypt_in_place<S: SecretSection>(&self, section: &mut S) -> Result<(), ConfigError> {
        for field in section.secret_fields() {
            let strip = field.strip;
            let plaintext = self.inner.crypto.decrypt(field.value.expose())?;
            let cleaned = if strip {
                plaintext.trim().to_owned()
            } else {
                plaintext
            };
            field.value.open(cleaned);
        }
        Ok(())
    }
}

#[cfg(any(test, feature = "test-support"))]
impl ConfigStore {
    /// A store for tests over `settings.db` in `dir`: a fresh migrated copy,
    /// or the one an earlier call left there, so a test can open it again.
    /// A `config.json` the test wrote in `dir` is copied in as at a first
    /// start, which is how a test stores a section exactly as written.
    pub fn scratch(dir: &std::path::Path, crypto: Crypto) -> Result<Self, String> {
        std::fs::create_dir_all(dir).map_err(|error| error.to_string())?;
        let path = dir.join("settings.db");
        if !path.exists() {
            crate::schema::template::copy_to(&path)?;
        }
        let db = Db::open_for_tests(&path).map_err(|error| error.to_string())?;
        Self::open_blocking(
            db,
            crypto,
            Arc::new(crate::clock::SystemClock),
            &LegacyConfig::at(dir, false),
        )
        .map_err(|error| error.to_string())
    }

    /// The stored text of one section, for tests that check what reaches
    /// the database.
    pub fn stored_text(&self, key: &str) -> Option<String> {
        self.entry(key)
            .and_then(|entry| serde_json::to_string(&entry.value).ok())
    }
}

#[cfg(test)]
impl ConfigStore {
    /// A sealed section as a masked read would show it.
    pub(super) fn mask_sealed<S: SecretSection>(
        &self,
        sealed: Sealed<S>,
    ) -> Result<Masked<S>, ConfigError> {
        let mut section = sealed.0;
        self.decrypt_in_place(&mut section)?;
        section.normalize();
        mask_in_place(&mut section);
        Ok(Masked::from_masked(section))
    }
}

/// A folder for one test, removed when the test ends.
#[cfg(test)]
pub(super) struct TestDir(std::path::PathBuf);

#[cfg(test)]
impl TestDir {
    pub(super) fn new(tag: &str) -> Self {
        use std::sync::atomic::{AtomicU64, Ordering};
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "dn-config-{tag}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        Self(path)
    }

    pub(super) fn path(&self) -> &std::path::Path {
        &self.0
    }
}

#[cfg(test)]
impl Drop for TestDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Run a blocking store call on the blocking pool.
async fn off_thread<T, F>(call: F) -> Result<T, ConfigError>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T, ConfigError> + Send + 'static,
{
    match tokio::task::spawn_blocking(call).await {
        Ok(result) => result,
        Err(error) => Err(ConfigError::Database(format!(
            "the settings write stopped: {error}"
        ))),
    }
}

/// The saved section masked: ciphertext is empty exactly when the secret is
/// unset, so masking the stored form gives the same echo as a fresh masked
/// read.
fn masked_echo<S: SecretSection>(sealed: Sealed<S>) -> Masked<S> {
    let mut section = sealed.0;
    mask_in_place(&mut section);
    Masked::from_masked(section)
}

/// Replace every set secret with the mask (unset stays empty).
fn mask_in_place<S: SecretSection>(section: &mut S) {
    for field in section.secret_fields() {
        let shown = display_mask(field.value.expose()).to_owned();
        field.value.close(shown);
    }
}

fn decode_value<S: Section>(
    key: &'static str,
    value: Option<&serde_json::Value>,
) -> Result<S, ConfigError> {
    match value {
        None | Some(serde_json::Value::Null) => Ok(S::default()),
        Some(raw) => S::deserialize(raw).map_err(|error| ConfigError::SectionDecode {
            section: key,
            reason: error.to_string(),
        }),
    }
}

fn encode_section<S: Section>(section: &S) -> Result<serde_json::Value, ConfigError> {
    serde_json::to_value(section).map_err(|error| ConfigError::SectionDecode {
        section: S::KEY,
        reason: error.to_string(),
    })
}

fn encode_sealed<S: Section>(sealed: &Sealed<S>) -> Result<serde_json::Value, ConfigError> {
    serde_json::to_value(sealed).map_err(|error| ConfigError::SectionDecode {
        section: S::KEY,
        reason: error.to_string(),
    })
}

/// A stored row's text as JSON.
pub fn decode_text(text: &str) -> Result<serde_json::Value, ConfigError> {
    serde_json::from_str(text)
        .map_err(|error| ConfigError::Database(format!("a stored section is not JSON: {error}")))
}

/// The text a row stores for `value`: compact JSON.
pub fn encode_text(value: &serde_json::Value) -> Result<String, ConfigError> {
    serde_json::to_string(value)
        .map_err(|error| ConfigError::Database(format!("a section does not encode: {error}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime_config::secret_sections::SlskdConnection;

    #[test]
    fn a_reference_through_plain_still_refuses_to_serialize() {
        let dir = TestDir::new("store");
        let store =
            ConfigStore::scratch(dir.path(), Crypto::from_key_bytes(&[3u8; 32]).unwrap()).unwrap();
        store
            .save_secret_blocking(SlskdConnection {
                api_key: "opened-sentinel".into(),
                ..SlskdConnection::default()
            })
            .unwrap();
        let plain = store.get_raw::<SlskdConnection>().unwrap();
        let response = axum::response::IntoResponse::into_response(axum::Json(&*plain));
        assert_eq!(
            response.status(),
            axum::http::StatusCode::INTERNAL_SERVER_ERROR
        );
        let body = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap()
            .block_on(axum::body::to_bytes(response.into_body(), usize::MAX))
            .unwrap();
        assert!(!String::from_utf8_lossy(&body).contains("opened-sentinel"));
        let live = store.live::<SlskdConnection>().get().unwrap();
        assert!(serde_json::to_value(&**live).is_err());
    }

    #[test]
    fn live_follows_each_commit_and_notices_fire() {
        let dir = TestDir::new("store");
        let store =
            ConfigStore::scratch(dir.path(), Crypto::from_key_bytes(&[3u8; 32]).unwrap()).unwrap();
        let live = store.live::<SlskdConnection>();
        let mut notice = live.subscribe();
        let before = live.get().unwrap();
        assert!(Arc::ptr_eq(&before, &live.get().unwrap()));
        store
            .save_secret_blocking(SlskdConnection {
                api_key: "first".into(),
                ..SlskdConnection::default()
            })
            .unwrap();
        assert!(notice.has_changed().unwrap());
        assert_eq!(*notice.borrow_and_update(), 1);
        assert_eq!(live.get().unwrap().api_key.expose(), "first");
    }
}
