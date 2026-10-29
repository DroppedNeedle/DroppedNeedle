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
//! One [`ListeningStore`] is built at startup and handed to every caller:
//! the settings routes, discovery, the Weekly Mix and, through the
//! `_blocking` reads, the synchronous reporting services in `sqlite.rs`.
//! A save that fails answers its error; reads log and fall back.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use droppedneedle_auth::sqlite::parse_lastfm;
use droppedneedle_auth::times::to_iso;
use droppedneedle_auth::users::stores::{LastFmKeys, LastFmSwitch};
use droppedneedle_kernel::clock::Clock;
use droppedneedle_kernel::db::{Db, DbError, Lane};
use droppedneedle_kernel::runtime_config::crypto::{Crypto, CryptoError};
use rusqlite::{Connection, OptionalExtension};

use super::ports::{ScrobbleTargets, VISIBILITY_FULL, VISIBILITY_OFFLINE, VISIBILITY_TRACK_HIDDEN};
use super::sql;

/// A boxed future, for the ports the routes take.
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

/// Why a ListenBrainz link was not saved.
#[derive(Debug, thiserror::Error)]
pub enum LinkSaveError {
    /// The token could not be sealed.
    #[error("the link could not be sealed: {0}")]
    Seal(#[from] CryptoError),
    /// The database refused the write.
    #[error(transparent)]
    Db(#[from] DbError),
}

/// Seal one ListenBrainz link into the document its row stores.
fn seal_link(crypto: &Crypto, username: &str, token: &str) -> Result<String, CryptoError> {
    crypto.encrypt(&serde_json::json!({"user_token": token, "username": username}).to_string())
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

/// Listening preferences and scrobble links over the database. Reads go
/// through the read pool, saves through the writer lane on the foreground
/// class. A failed read logs and answers the defaults (preferences) or
/// unlinked (links); a failed save answers its error. ListenBrainz tokens
/// are sealed at rest, and a link whose ciphertext no longer opens reads
/// as unlinked.
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
    /// failed save leaves the stored row as it was.
    pub async fn update_prefs(&self, user_id: &str, patch: &PrefsPatch) -> Result<(), DbError> {
        let user = user_id.to_owned();
        let patch = patch.clone();
        let now = to_iso(self.clock.now_unix());
        self.db
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
            .await
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
        let Some(document) = parse_lastfm(&data) else {
            tracing::warn!("last.fm connection row is corrupt; treating as unlinked");
            return Ok(None);
        };
        let open = |sealed: Option<String>| -> Option<String> {
            let plaintext = self.crypto.decrypt(sealed.as_deref()?).ok()?;
            (!plaintext.is_empty()).then_some(plaintext)
        };
        let Some(session_key) = open(document.session_key_encrypted) else {
            return Ok(None);
        };
        let (api_key, shared_secret) = match (
            open(document.api_key_encrypted),
            open(document.shared_secret_encrypted),
        ) {
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

    /// The scrobble services the user linked, read synchronously: an
    /// enabled ListenBrainz row (sealed whole, so presence is the signal)
    /// and an enabled Last.fm row whose document carries a session key.
    pub fn scrobble_links_blocking(&self, user_id: &str) -> Result<ScrobbleTargets, DbError> {
        let (listenbrainz, lastfm) = self.db.read_blocking("playback-scrobble-links", |conn| {
            Ok::<_, DbError>((
                read_enabled_connection(conn, user_id, LISTENBRAINZ)?,
                read_enabled_connection(conn, user_id, LASTFM)?,
            ))
        })?;
        Ok(ScrobbleTargets {
            listenbrainz: listenbrainz.is_some(),
            lastfm: lastfm
                .and_then(|data| parse_lastfm(&data))
                .and_then(|link| link.session_key_encrypted)
                .is_some_and(|key| !key.is_empty()),
        })
    }

    /// Store (or replace) one user's ListenBrainz link, sealing the token.
    pub async fn save_listenbrainz(
        &self,
        user_id: &str,
        username: &str,
        token: &str,
    ) -> Result<(), LinkSaveError> {
        let user = user_id.to_owned();
        let sealed = seal_link(&self.crypto, username, token)?;
        let now = to_iso(self.clock.now_unix());
        self.db
            .write(Lane::Foreground, "listening-link-save", move |tx| {
                tx.execute(
                    sql::SAVE_CONNECTION.text(),
                    rusqlite::params![user, LISTENBRAINZ, sealed, now],
                )?;
                Ok::<_, DbError>(())
            })
            .await?;
        Ok(())
    }

    /// The non-secret view of the user's ListenBrainz link, when they have
    /// a usable one.
    pub async fn listenbrainz_link(&self, user_id: &str) -> Option<ListenBrainzLink> {
        let sealed = self.listenbrainz_document(user_id, "status").await?;
        open_link(&self.crypto, &sealed).map(|(username, _)| ListenBrainzLink { username })
    }

    /// Whether any ListenBrainz row exists, even one whose ciphertext no
    /// longer opens.
    pub async fn has_listenbrainz_link(&self, user_id: &str) -> bool {
        self.listenbrainz_document(user_id, "has_link")
            .await
            .is_some()
    }

    /// The decrypted ListenBrainz token, for server-side use only.
    pub async fn listenbrainz_token(&self, user_id: &str) -> Option<String> {
        let sealed = self.listenbrainz_document(user_id, "token").await?;
        usable_token(open_link(&self.crypto, &sealed))
    }

    /// Remove one user's ListenBrainz link. False means there was none.
    pub async fn delete_listenbrainz(&self, user_id: &str) -> Result<bool, DbError> {
        let user = user_id.to_owned();
        self.db
            .write(Lane::Foreground, "listening-link-delete", move |tx| {
                Ok::<_, DbError>(
                    tx.execute(sql::DELETE_CONNECTION.text(), [user.as_str(), LISTENBRAINZ])?,
                )
            })
            .await
            .map(|changed| changed > 0)
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

/// The stored document of one user's link to `service`, with whether the
/// link is enabled.
fn read_connection_row(
    conn: &Connection,
    user_id: &str,
    service: &str,
) -> Result<Option<(String, bool)>, DbError> {
    Ok(conn
        .query_row(sql::CONNECTION.text(), [user_id, service], |row| {
            Ok((row.get(0)?, row.get::<_, i64>(1)? != 0))
        })
        .optional()?)
}

/// The stored document of one user's link to `service`.
fn read_connection(
    conn: &Connection,
    user_id: &str,
    service: &str,
) -> Result<Option<String>, DbError> {
    Ok(read_connection_row(conn, user_id, service)?.map(|(data, _)| data))
}

/// The stored document of one user's enabled link to `service`.
fn read_enabled_connection(
    conn: &Connection,
    user_id: &str,
    service: &str,
) -> Result<Option<String>, DbError> {
    Ok(read_connection_row(conn, user_id, service)?
        .and_then(|(data, enabled)| enabled.then_some(data)))
}
