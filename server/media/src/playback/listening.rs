//! Each user's listening preferences and scrobble links.
//!
//! Preferences are one row per user: a user who saved none reads
//! the column defaults, and a save changes only the fields it names. Links
//! live in `user_connections`, one row per user and service. A ListenBrainz
//! row holds the `{user_token, username}` document sealed whole; a Last.fm
//! row is a plain document whose three secrets are sealed one by one. Reads
//! that cannot open a sealed value behave as unlinked, so no failure ever
//! hands out a token.
//!
//! [`ListeningStore`] answers async callers (the settings routes, discovery,
//! the Weekly Mix) and, through the `_blocking` reads, the synchronous
//! reporting services in `sqlite.rs`. Both run the same statements.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
#[cfg(any(test, feature = "test-support"))]
use std::{collections::HashMap, sync::Mutex};

use droppedneedle_auth::times::to_iso;
use droppedneedle_auth::users::stores::{LastFmKeys, LastFmSwitch};
use droppedneedle_kernel::clock::Clock;
use droppedneedle_kernel::db::{Db, DbError, Lane};
use droppedneedle_kernel::runtime_config::crypto::Crypto;
use rusqlite::{Connection, OptionalExtension};

use super::ports::{VISIBILITY_FULL, VISIBILITY_OFFLINE, VISIBILITY_TRACK_HIDDEN};
use super::sql;

/// A boxed future, for the link store trait.
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Allowed `primary_music_source` values.
pub const PRIMARY_SOURCES: &[&str] = &["listenbrainz", "lastfm"];
/// Allowed `now_playing_visibility` values.
pub const NOW_PLAYING_VISIBILITIES: &[&str] =
    &[VISIBILITY_FULL, VISIBILITY_TRACK_HIDDEN, VISIBILITY_OFFLINE];

/// Service tag of ListenBrainz rows in `user_connections`.
const LISTENBRAINZ: &str = "listenbrainz";
/// Service tag of Last.fm rows in `user_connections`.
const LASTFM: &str = "lastfm";

/// One user's listening preferences.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScrobblePrefs {
    /// Forward plays to the user's linked Last.fm account.
    pub scrobble_to_lastfm: bool,
    /// Forward plays to the user's linked ListenBrainz account.
    pub scrobble_to_listenbrainz: bool,
    /// Navidrome owns external scrobbles: skip our forwarding for
    /// Navidrome-sourced plays (history still records).
    pub navidrome_handles_external_scrobbles: bool,
    /// Where discovery reads the user's listening: one of
    /// [`PRIMARY_SOURCES`].
    pub primary_music_source: String,
    /// Who sees what the user plays: one of [`NOW_PLAYING_VISIBILITIES`].
    pub now_playing_visibility: String,
    /// Standing intent to auto-request the personal mix.
    pub auto_request_personal_mix: bool,
}

/// The column defaults: what a user who saved nothing reads.
impl Default for ScrobblePrefs {
    fn default() -> Self {
        Self {
            scrobble_to_lastfm: false,
            scrobble_to_listenbrainz: false,
            navidrome_handles_external_scrobbles: true,
            primary_music_source: PRIMARY_SOURCES[0].to_owned(),
            now_playing_visibility: VISIBILITY_FULL.to_owned(),
            auto_request_personal_mix: false,
        }
    }
}

/// A partial preferences save: `None` fields keep their stored values.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PrefsPatch {
    /// New Last.fm forwarding switch.
    pub scrobble_to_lastfm: Option<bool>,
    /// New ListenBrainz forwarding switch.
    pub scrobble_to_listenbrainz: Option<bool>,
    /// New Navidrome delegation switch.
    pub navidrome_handles_external_scrobbles: Option<bool>,
    /// New primary source (validated before it reaches the store).
    pub primary_music_source: Option<String>,
    /// New presence visibility (validated before it reaches the store).
    pub now_playing_visibility: Option<String>,
    /// New personal-mix auto-request intent.
    pub auto_request_personal_mix: Option<bool>,
}

impl PrefsPatch {
    fn apply(&self, prefs: &mut ScrobblePrefs) {
        if let Some(value) = self.scrobble_to_lastfm {
            prefs.scrobble_to_lastfm = value;
        }
        if let Some(value) = self.scrobble_to_listenbrainz {
            prefs.scrobble_to_listenbrainz = value;
        }
        if let Some(value) = self.navidrome_handles_external_scrobbles {
            prefs.navidrome_handles_external_scrobbles = value;
        }
        if let Some(value) = &self.primary_music_source {
            prefs.primary_music_source = value.clone();
        }
        if let Some(value) = &self.now_playing_visibility {
            prefs.now_playing_visibility = value.clone();
        }
        if let Some(value) = self.auto_request_personal_mix {
            prefs.auto_request_personal_mix = value;
        }
    }
}

/// Non-secret view of one ListenBrainz link. The token never appears here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListenBrainzLink {
    /// Linked ListenBrainz username.
    pub username: String,
}

/// A user's Last.fm signing material. Never logged.
pub struct LastFmSession {
    /// API key: the user's own, else the instance key.
    pub api_key: String,
    /// Shared secret matching the API key.
    pub shared_secret: String,
    /// Session key from the link flow.
    pub session_key: String,
}

/// Per-user ListenBrainz links. Tokens are sealed at rest; a link whose
/// ciphertext no longer opens reads as unlinked, never as an error. A trait
/// so discovery and the Weekly Mix tests can link accounts without a
/// database.
pub trait ListenBrainzLinkStore: Send + Sync {
    /// Store (or replace) one user's link, sealing the token.
    fn save(&self, user_id: &str, username: &str, token: &str)
    -> BoxFuture<'_, Result<(), String>>;
    /// Non-secret link view, when the user has a usable link.
    fn status(&self, user_id: &str) -> BoxFuture<'_, Option<ListenBrainzLink>>;
    /// Whether any link row exists, even one whose ciphertext no longer opens.
    fn has_link(&self, user_id: &str) -> BoxFuture<'_, bool>;
    /// Decrypted token for server-side use only. Never serializes.
    fn token_for(&self, user_id: &str) -> BoxFuture<'_, Option<String>>;
    /// Remove one user's link. False means there was none.
    fn delete(&self, user_id: &str) -> BoxFuture<'_, bool>;
}

/// Seal one ListenBrainz link into the document its row stores.
fn seal_link(crypto: &Crypto, username: &str, token: &str) -> Result<String, String> {
    crypto
        .encrypt(&serde_json::json!({"user_token": token, "username": username}).to_string())
        .map_err(|error| error.to_string())
}

/// Open one sealed ListenBrainz link into its username and token.
/// Anything unopenable reads as absent.
fn open_link(crypto: &Crypto, sealed: &str) -> Option<(String, String)> {
    let plaintext = crypto.decrypt(sealed).ok()?;
    let value: serde_json::Value = serde_json::from_str(&plaintext).ok()?;
    let username = value.get("username")?.as_str()?.to_owned();
    let token = value.get("user_token")?.as_str()?.to_owned();
    Some((username, token))
}

/// The usable token of an opened link: an empty one is no token.
fn usable_token(link: Option<(String, String)>) -> Option<String> {
    link.map(|(_, token)| token)
        .filter(|token| !token.is_empty())
}

/// The Last.fm document in `user_connections`, its secrets still sealed.
#[derive(serde::Deserialize)]
struct LastFmDocument {
    #[serde(default)]
    api_key: Option<String>,
    #[serde(default)]
    shared_secret: Option<String>,
    #[serde(default)]
    session_key: Option<String>,
}

/// Listening preferences and scrobble links over the database. Reads go
/// through the read pool, saves through the writer lane on the foreground
/// class. A failed read logs and answers the defaults (preferences) or
/// unlinked (links).
#[derive(Clone)]
pub struct ListeningStore {
    db: Db,
    crypto: Arc<Crypto>,
    clock: Arc<dyn Clock>,
}

impl std::fmt::Debug for ListeningStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ListeningStore").finish_non_exhaustive()
    }
}

impl ListeningStore {
    /// Preferences and links on `db`, sealed under `crypto`, saves dated
    /// by `clock`.
    pub fn new(db: Db, crypto: Arc<Crypto>, clock: Arc<dyn Clock>) -> Self {
        Self { db, crypto, clock }
    }

    /// One user's preferences; the defaults when they saved none or the
    /// read failed.
    pub async fn prefs(&self, user_id: &str) -> ScrobblePrefs {
        let user = user_id.to_owned();
        self.db
            .read("listening-prefs", move |conn| read_prefs(conn, &user))
            .await
            .unwrap_or_else(|error: DbError| {
                tracing::warn!(%user_id, %error, "listening prefs read failed; defaults apply");
                ScrobblePrefs::default()
            })
    }

    /// [`ListeningStore::prefs`] for synchronous code, failure included.
    pub fn prefs_blocking(&self, user_id: &str) -> Result<ScrobblePrefs, DbError> {
        self.db
            .read_blocking("listening-prefs", |conn| read_prefs(conn, user_id))
    }

    /// Save the fields `patch` names over the user's stored preferences,
    /// in one transaction so two partial saves never undo each other. A
    /// failed save is logged and the stored row stays as it was.
    pub async fn update_prefs(&self, user_id: &str, patch: &PrefsPatch) {
        let user = user_id.to_owned();
        let patch = patch.clone();
        let now = to_iso(self.clock.now_unix());
        let saved = self
            .db
            .write(Lane::Foreground, "listening-prefs-save", move |tx| {
                let mut prefs = read_prefs(tx, &user)?;
                patch.apply(&mut prefs);
                tx.execute(
                    sql::SAVE_PREFS.text(),
                    rusqlite::params![
                        user,
                        prefs.scrobble_to_lastfm,
                        prefs.scrobble_to_listenbrainz,
                        prefs.navidrome_handles_external_scrobbles,
                        prefs.primary_music_source,
                        prefs.now_playing_visibility,
                        now,
                        prefs.auto_request_personal_mix,
                    ],
                )?;
                Ok::<_, DbError>(())
            })
            .await;
        if let Err(error) = saved {
            tracing::warn!(%user_id, %error, "listening prefs save failed");
        }
    }

    /// The user's ListenBrainz token, read synchronously; `None` when
    /// unlinked, unopenable or unreadable.
    pub fn listenbrainz_token_blocking(&self, user_id: &str) -> Option<String> {
        self.listenbrainz_token_checked(user_id)
            .unwrap_or_else(|error| {
                tracing::warn!(%user_id, %error, "listenbrainz token read failed");
                None
            })
    }

    /// [`ListeningStore::listenbrainz_token_blocking`], with a failed read
    /// kept apart from a link that is missing or does not open.
    pub fn listenbrainz_token_checked(&self, user_id: &str) -> Result<Option<String>, DbError> {
        let sealed = self.db.read_blocking("listening-link", |conn| {
            read_connection(conn, user_id, LISTENBRAINZ)
        })?;
        Ok(sealed.and_then(|sealed| usable_token(open_link(&self.crypto, &sealed))))
    }

    /// The user's Last.fm signing material, read synchronously: `None`
    /// while Last.fm is switched off, or when the user has no usable
    /// session. The user's own key pair wins; without one, the session was
    /// granted to the instance pair the admin saved (v2 parity).
    pub fn lastfm_session_blocking(
        &self,
        user_id: &str,
        lastfm: &LastFmSwitch,
    ) -> Option<LastFmSession> {
        if !lastfm.enabled() {
            return None;
        }
        self.lastfm_session_checked(user_id, lastfm.instance_keys().as_ref())
            .unwrap_or_else(|error| {
                tracing::warn!(%error, "last.fm link read failed; skipping the forward");
                None
            })
    }

    /// The user's Last.fm signing material, with `instance_keys` standing
    /// in for a user without their own pair. A failed read is an error;
    /// `None` is a link that is missing or does not open.
    pub fn lastfm_session_checked(
        &self,
        user_id: &str,
        instance_keys: Option<&LastFmKeys>,
    ) -> Result<Option<LastFmSession>, DbError> {
        let Some(data) = self.db.read_blocking("listening-link", |conn| {
            read_connection(conn, user_id, LASTFM)
        })?
        else {
            return Ok(None);
        };
        let Ok(document) = serde_json::from_str::<LastFmDocument>(&data) else {
            tracing::warn!("last.fm connection row is corrupt; treating as unlinked");
            return Ok(None);
        };
        let open = |sealed: Option<String>| -> Option<String> {
            let plaintext = self.crypto.decrypt(sealed.as_deref()?).ok()?;
            (!plaintext.is_empty()).then_some(plaintext)
        };
        let Some(session_key) = open(document.session_key) else {
            return Ok(None);
        };
        let (api_key, shared_secret) = match (open(document.api_key), open(document.shared_secret))
        {
            (Some(api_key), Some(shared_secret)) => (api_key, shared_secret),
            _ => match instance_keys {
                Some(keys) => (keys.api_key.clone(), keys.shared_secret.clone()),
                None => return Ok(None),
            },
        };
        Ok(Some(LastFmSession {
            api_key,
            shared_secret,
            session_key,
        }))
    }

    /// The stored ListenBrainz document, or `None` after logging a failed
    /// read.
    async fn listenbrainz_document(&self, user_id: &str, read: &'static str) -> Option<String> {
        let user = user_id.to_owned();
        self.db
            .read("listening-link", move |conn| {
                read_connection(conn, &user, LISTENBRAINZ)
            })
            .await
            .unwrap_or_else(|error: DbError| {
                tracing::warn!(%user_id, %error, read, "listenbrainz link read failed");
                None
            })
    }
}

/// One user's preferences row, or the defaults when there is none.
fn read_prefs(conn: &Connection, user_id: &str) -> Result<ScrobblePrefs, DbError> {
    let prefs = conn
        .query_row(sql::PREFS.text(), [user_id], |row| {
            Ok(ScrobblePrefs {
                scrobble_to_lastfm: row.get::<_, i64>(0)? != 0,
                scrobble_to_listenbrainz: row.get::<_, i64>(1)? != 0,
                navidrome_handles_external_scrobbles: row.get::<_, i64>(2)? != 0,
                primary_music_source: row.get(3)?,
                now_playing_visibility: row.get(4)?,
                auto_request_personal_mix: row.get::<_, i64>(5)? != 0,
            })
        })
        .optional()?;
    Ok(prefs.unwrap_or_default())
}

/// The stored document of one user's link to `service`.
fn read_connection(
    conn: &Connection,
    user_id: &str,
    service: &str,
) -> Result<Option<String>, DbError> {
    Ok(conn
        .query_row(sql::CONNECTION.text(), [user_id, service], |row| row.get(0))
        .optional()?)
}

impl ListenBrainzLinkStore for ListeningStore {
    fn save(
        &self,
        user_id: &str,
        username: &str,
        token: &str,
    ) -> BoxFuture<'_, Result<(), String>> {
        let user = user_id.to_owned();
        let sealed = seal_link(&self.crypto, username, token);
        let now = to_iso(self.clock.now_unix());
        Box::pin(async move {
            let sealed = sealed?;
            self.db
                .write(Lane::Foreground, "listening-link-save", move |tx| {
                    tx.execute(
                        sql::SAVE_CONNECTION.text(),
                        rusqlite::params![user, LISTENBRAINZ, sealed, now],
                    )?;
                    Ok(())
                })
                .await
                .map_err(|error: DbError| {
                    tracing::warn!(%error, "listenbrainz link save failed");
                    error.to_string()
                })
        })
    }

    fn status(&self, user_id: &str) -> BoxFuture<'_, Option<ListenBrainzLink>> {
        let user = user_id.to_owned();
        Box::pin(async move {
            let sealed = self.listenbrainz_document(&user, "status").await?;
            open_link(&self.crypto, &sealed).map(|(username, _)| ListenBrainzLink { username })
        })
    }

    fn has_link(&self, user_id: &str) -> BoxFuture<'_, bool> {
        let user = user_id.to_owned();
        Box::pin(async move {
            self.listenbrainz_document(&user, "has_link")
                .await
                .is_some()
        })
    }

    fn token_for(&self, user_id: &str) -> BoxFuture<'_, Option<String>> {
        let user = user_id.to_owned();
        Box::pin(async move {
            let sealed = self.listenbrainz_document(&user, "token").await?;
            usable_token(open_link(&self.crypto, &sealed))
        })
    }

    fn delete(&self, user_id: &str) -> BoxFuture<'_, bool> {
        let user = user_id.to_owned();
        Box::pin(async move {
            self.db
                .write(Lane::Foreground, "listening-link-delete", move |tx| {
                    Ok(tx.execute(sql::DELETE_CONNECTION.text(), [user.as_str(), LISTENBRAINZ])?)
                })
                .await
                .map(|changed| changed > 0)
                .unwrap_or_else(|error: DbError| {
                    tracing::warn!(%error, "listenbrainz link delete failed");
                    false
                })
        })
    }
}

/// In-memory link store with sealed tokens, for tests that link accounts
/// without a database.
#[cfg(any(test, feature = "test-support"))]
pub struct MemoryListenBrainzLinkStore {
    crypto: Arc<Crypto>,
    links: Mutex<HashMap<String, String>>,
}

#[cfg(any(test, feature = "test-support"))]
impl std::fmt::Debug for MemoryListenBrainzLinkStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MemoryListenBrainzLinkStore")
            .finish_non_exhaustive()
    }
}

#[cfg(any(test, feature = "test-support"))]
impl MemoryListenBrainzLinkStore {
    /// Empty store under one key.
    pub fn new(crypto: Arc<Crypto>) -> Self {
        Self {
            crypto,
            links: Mutex::new(HashMap::new()),
        }
    }

    fn sealed_for(&self, user_id: &str) -> Option<String> {
        self.links
            .lock()
            .ok()
            .and_then(|guard| guard.get(user_id).cloned())
    }
}

#[cfg(any(test, feature = "test-support"))]
impl ListenBrainzLinkStore for MemoryListenBrainzLinkStore {
    fn save(
        &self,
        user_id: &str,
        username: &str,
        token: &str,
    ) -> BoxFuture<'_, Result<(), String>> {
        let stored = seal_link(&self.crypto, username, token).map(|sealed| {
            if let Ok(mut guard) = self.links.lock() {
                guard.insert(user_id.to_owned(), sealed);
            }
        });
        Box::pin(async move { stored })
    }

    fn status(&self, user_id: &str) -> BoxFuture<'_, Option<ListenBrainzLink>> {
        let link = self
            .sealed_for(user_id)
            .and_then(|sealed| open_link(&self.crypto, &sealed))
            .map(|(username, _)| ListenBrainzLink { username });
        Box::pin(async move { link })
    }

    fn has_link(&self, user_id: &str) -> BoxFuture<'_, bool> {
        let linked = self.sealed_for(user_id).is_some();
        Box::pin(async move { linked })
    }

    fn token_for(&self, user_id: &str) -> BoxFuture<'_, Option<String>> {
        let token = usable_token(
            self.sealed_for(user_id)
                .and_then(|sealed| open_link(&self.crypto, &sealed)),
        );
        Box::pin(async move { token })
    }

    fn delete(&self, user_id: &str) -> BoxFuture<'_, bool> {
        let removed = self
            .links
            .lock()
            .map(|mut guard| guard.remove(user_id).is_some())
            .unwrap_or(false);
        Box::pin(async move { removed })
    }
}
