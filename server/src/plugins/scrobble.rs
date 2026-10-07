//! ListenBrainz and scrobble settings backend.
//!
//! Two halves: per-user ListenBrainz links (verify-then-store, token sealed
//! at rest, username-only reads) and per-user scrobble preference reads and
//! writes. The web UI calls these; nothing here renders anything.
//!
//! Storage seams mirror the v2 tables: links behave like the
//! `user_connections` rows with `service = 'listenbrainz'`
//! (`{user_token, username}` sealed JSON), and prefs behave like
//! `user_listening_prefs` (partial upsert, table defaults for new rows).
//! Both baseline tables already exist in migration 0001; the SQLite stores
//! below target them in production, and the memory stores keep the same
//! shapes for tests.

use std::sync::Arc;
#[cfg(any(test, feature = "test-support"))]
use std::{collections::HashMap, sync::Mutex};

use crate::auth::times::to_iso;
use crate::db::{WriteLane, writer::Lane};

use super::runtime::BoxFuture;
use crate::providers::listenbrainz::ListenBrainzVerifier;

/// Allowed `primary_music_source` values.
pub const PRIMARY_SOURCES: &[&str] = &["listenbrainz", "lastfm"];
/// Allowed `now_playing_visibility` values.
pub const NOW_PLAYING_VISIBILITIES: &[&str] = &["full", "track_hidden", "offline"];

/// Per-user scrobble and discovery prefs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScrobblePrefs {
    /// Forward plays to the user's Last.fm account.
    pub scrobble_to_lastfm: bool,
    /// Forward plays to the user's ListenBrainz account.
    pub scrobble_to_listenbrainz: bool,
    /// Navidrome owns external forwarding for Navidrome plays.
    pub navidrome_handles_external_scrobbles: bool,
    /// `listenbrainz` or `lastfm`.
    pub primary_music_source: String,
    /// `full`, `track_hidden`, or `offline`.
    pub now_playing_visibility: String,
    /// Standing intent to auto-request the personal mix.
    pub auto_request_personal_mix: bool,
}

impl Default for ScrobblePrefs {
    fn default() -> Self {
        Self {
            scrobble_to_lastfm: false,
            scrobble_to_listenbrainz: false,
            navidrome_handles_external_scrobbles: true,
            primary_music_source: "listenbrainz".to_owned(),
            now_playing_visibility: "full".to_owned(),
            auto_request_personal_mix: false,
        }
    }
}

/// Partial prefs update: `None` fields keep their stored values.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ScrobblePrefsPatch {
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

/// Per-user scrobble prefs. Unknown users read back the defaults; upserts
/// change only the patched fields.
pub trait ScrobblePrefsStore: Send + Sync {
    /// Read one user's prefs.
    fn get(&self, user_id: &str) -> BoxFuture<'_, ScrobblePrefs>;
    /// Partially update one user's prefs.
    fn upsert(&self, user_id: &str, patch: &ScrobblePrefsPatch) -> BoxFuture<'_, ()>;
}

/// Fold one patch into stored prefs. Both stores share it so partial
/// upserts mean the same thing in memory and on disk.
fn apply_patch(entry: &mut ScrobblePrefs, patch: &ScrobblePrefsPatch) {
    if let Some(value) = patch.scrobble_to_lastfm {
        entry.scrobble_to_lastfm = value;
    }
    if let Some(value) = patch.scrobble_to_listenbrainz {
        entry.scrobble_to_listenbrainz = value;
    }
    if let Some(value) = patch.navidrome_handles_external_scrobbles {
        entry.navidrome_handles_external_scrobbles = value;
    }
    if let Some(value) = patch.primary_music_source.clone() {
        entry.primary_music_source = value;
    }
    if let Some(value) = patch.now_playing_visibility.clone() {
        entry.now_playing_visibility = value;
    }
    if let Some(value) = patch.auto_request_personal_mix {
        entry.auto_request_personal_mix = value;
    }
}

/// In-memory prefs store.
#[cfg(any(test, feature = "test-support"))]
#[derive(Debug, Default)]
pub struct MemoryScrobblePrefsStore {
    prefs: Mutex<HashMap<String, ScrobblePrefs>>,
}

#[cfg(any(test, feature = "test-support"))]
impl MemoryScrobblePrefsStore {
    /// Empty store.
    pub fn new() -> Self {
        Self::default()
    }
}

#[cfg(any(test, feature = "test-support"))]
impl ScrobblePrefsStore for MemoryScrobblePrefsStore {
    fn get(&self, user_id: &str) -> BoxFuture<'_, ScrobblePrefs> {
        let prefs = self
            .prefs
            .lock()
            .ok()
            .and_then(|guard| guard.get(user_id).cloned())
            .unwrap_or_default();
        Box::pin(async move { prefs })
    }

    fn upsert(&self, user_id: &str, patch: &ScrobblePrefsPatch) -> BoxFuture<'_, ()> {
        if let Ok(mut guard) = self.prefs.lock() {
            let entry = guard.entry(user_id.to_owned()).or_default();
            apply_patch(entry, patch);
        }
        Box::pin(async {})
    }
}

/// SQLite prefs over `user_listening_prefs`: the production store. Reads
/// travel the reader pool; upserts read-modify-write inside one writer-lane
/// transaction, so partial patches never clobber. A database fault logs
/// and degrades (reads fall back to the defaults); the pool is
/// process-local, so faults mean the whole database is down, not this row.
#[derive(Clone, Debug)]
pub struct SqliteScrobblePrefsStore {
    pool: sqlx::SqlitePool,
    lane: WriteLane,
}

impl SqliteScrobblePrefsStore {
    /// Bind the store over a migrated pool plus the writer lane.
    pub fn new(pool: sqlx::SqlitePool, lane: WriteLane) -> Self {
        Self { pool, lane }
    }
}

/// One prefs row: the six user-facing columns, `updated_at` aside.
type PrefsRow = (i64, i64, i64, String, String, i64);

/// Map one row onto prefs. Any nonzero integer reads as set.
fn prefs_from_row(row: PrefsRow) -> ScrobblePrefs {
    ScrobblePrefs {
        scrobble_to_lastfm: row.0 != 0,
        scrobble_to_listenbrainz: row.1 != 0,
        navidrome_handles_external_scrobbles: row.2 != 0,
        primary_music_source: row.3,
        now_playing_visibility: row.4,
        auto_request_personal_mix: row.5 != 0,
    }
}

impl ScrobblePrefsStore for SqliteScrobblePrefsStore {
    fn get(&self, user_id: &str) -> BoxFuture<'_, ScrobblePrefs> {
        let pool = self.pool.clone();
        let user_id = user_id.to_owned();
        Box::pin(async move {
            let row: Option<PrefsRow> = sqlx::query_as(
                "SELECT scrobble_to_lastfm, scrobble_to_listenbrainz,
                        navidrome_handles_external_scrobbles, primary_music_source,
                        now_playing_visibility, auto_request_personal_mix
                 FROM user_listening_prefs WHERE user_id = ?1",
            )
            .bind(&user_id)
            .fetch_optional(&pool)
            .await
            .map_err(|error| {
                tracing::warn!(%user_id, %error, "scrobble prefs read failed; defaults apply");
            })
            .ok()
            .flatten();
            row.map(prefs_from_row).unwrap_or_default()
        })
    }

    fn upsert(&self, user_id: &str, patch: &ScrobblePrefsPatch) -> BoxFuture<'_, ()> {
        let lane = self.lane.clone();
        let user_id = user_id.to_owned();
        let patch = patch.clone();
        Box::pin(async move {
            if let Err(error) = lane
                .write(Lane::Foreground, "scrobble.prefs.upsert", move |tx| {
                    use rusqlite::OptionalExtension as _;

                    let existing: Option<ScrobblePrefs> = tx
                        .query_row(
                            "SELECT scrobble_to_lastfm, scrobble_to_listenbrainz,
                                    navidrome_handles_external_scrobbles, primary_music_source,
                                    now_playing_visibility, auto_request_personal_mix
                             FROM user_listening_prefs WHERE user_id = ?1",
                            rusqlite::params![user_id],
                            |row| {
                                Ok(prefs_from_row((
                                    row.get(0)?,
                                    row.get(1)?,
                                    row.get(2)?,
                                    row.get(3)?,
                                    row.get(4)?,
                                    row.get(5)?,
                                )))
                            },
                        )
                        .optional()?;
                    let mut merged = existing.unwrap_or_default();
                    apply_patch(&mut merged, &patch);
                    tx.execute(
                        "INSERT OR REPLACE INTO user_listening_prefs
                             (user_id, scrobble_to_lastfm, scrobble_to_listenbrainz,
                              navidrome_handles_external_scrobbles, primary_music_source,
                              now_playing_visibility, updated_at, auto_request_personal_mix)
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                        rusqlite::params![
                            user_id,
                            i64::from(merged.scrobble_to_lastfm),
                            i64::from(merged.scrobble_to_listenbrainz),
                            i64::from(merged.navidrome_handles_external_scrobbles),
                            merged.primary_music_source,
                            merged.now_playing_visibility,
                            to_iso(now_unix()),
                            i64::from(merged.auto_request_personal_mix),
                        ],
                    )?;
                    Ok(())
                })
                .await
            {
                tracing::warn!(%error, "scrobble prefs upsert failed");
            }
        })
    }
}

/// Wall-clock now as unix seconds for the `updated_at` stamps.
fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|span| span.as_secs().min(i64::MAX as u64) as i64)
        .unwrap_or(0)
}

/// Non-secret view of one ListenBrainz link. The token never appears here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListenBrainzLink {
    /// Linked ListenBrainz username.
    pub username: String,
}

/// Per-user ListenBrainz links. Tokens seal at rest under the config key;
/// reads that cannot open the ciphertext behave as unlinked, never as an
/// error to the caller.
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

/// Service tag for the link rows in `user_connections`.
const LISTENBRAINZ_SERVICE: &str = "listenbrainz";

/// Seal one link into the `{user_token, username}` JSON both stores persist.
fn seal_link(
    crypto: &crate::runtime_config::crypto::Crypto,
    username: &str,
    token: &str,
) -> Result<String, String> {
    crypto
        .encrypt(&serde_json::json!({"user_token": token, "username": username}).to_string())
        .map_err(|error| error.to_string())
}

/// Open one sealed link into its username and token. Anything unopenable
/// reads as absent; the caller decides whether to log.
fn open_link(
    crypto: &crate::runtime_config::crypto::Crypto,
    sealed: &str,
) -> Option<(String, String)> {
    let plaintext = crypto.decrypt(sealed).ok()?;
    let value: serde_json::Value = serde_json::from_str(&plaintext).ok()?;
    let username = value.get("username")?.as_str()?.to_owned();
    let token = value.get("user_token")?.as_str()?.to_owned();
    Some((username, token))
}

/// One sealed link row: the `{user_token, username}` JSON, encrypted.
#[cfg(any(test, feature = "test-support"))]
#[derive(Debug, Clone)]
struct SealedLink {
    sealed: String,
}

/// In-memory link store with sealed tokens.
#[cfg(any(test, feature = "test-support"))]
pub struct MemoryListenBrainzLinkStore {
    crypto: Arc<crate::runtime_config::crypto::Crypto>,
    links: Mutex<HashMap<String, SealedLink>>,
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
    /// Empty store under one config key.
    pub fn new(crypto: Arc<crate::runtime_config::crypto::Crypto>) -> Self {
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
            .map(|link| link.sealed)
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
        let sealed = seal_link(&self.crypto, username, token);
        let stored = match sealed {
            Ok(sealed) => {
                if let Ok(mut guard) = self.links.lock() {
                    guard.insert(user_id.to_owned(), SealedLink { sealed });
                }
                Ok(())
            }
            Err(error) => Err(error),
        };
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
        let linked = self
            .links
            .lock()
            .map(|guard| guard.contains_key(user_id))
            .unwrap_or(false);
        Box::pin(async move { linked })
    }

    fn token_for(&self, user_id: &str) -> BoxFuture<'_, Option<String>> {
        let token = self
            .sealed_for(user_id)
            .and_then(|sealed| open_link(&self.crypto, &sealed))
            .and_then(|(_, token)| if token.is_empty() { None } else { Some(token) });
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

/// SQLite links over `user_connections` (`service = 'listenbrainz'`): the
/// production store, mirroring the auth Last.fm rows. Reads travel
/// the reader pool; writes travel the writer lane. Failures degrade toward
/// unlinked (safe: no token ever leaks through a failure) and log.
pub struct SqliteListenBrainzLinkStore {
    pool: sqlx::SqlitePool,
    lane: WriteLane,
    crypto: Arc<crate::runtime_config::crypto::Crypto>,
}

impl std::fmt::Debug for SqliteListenBrainzLinkStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SqliteListenBrainzLinkStore")
            .finish_non_exhaustive()
    }
}

impl SqliteListenBrainzLinkStore {
    /// Bind the store over a migrated pool, the writer lane, and one
    /// config key.
    pub fn new(
        pool: sqlx::SqlitePool,
        lane: WriteLane,
        crypto: Arc<crate::runtime_config::crypto::Crypto>,
    ) -> Self {
        Self { pool, lane, crypto }
    }
}

impl Clone for SqliteListenBrainzLinkStore {
    fn clone(&self) -> Self {
        Self {
            pool: self.pool.clone(),
            lane: self.lane.clone(),
            crypto: Arc::clone(&self.crypto),
        }
    }
}

impl ListenBrainzLinkStore for SqliteListenBrainzLinkStore {
    fn save(
        &self,
        user_id: &str,
        username: &str,
        token: &str,
    ) -> BoxFuture<'_, Result<(), String>> {
        let lane = self.lane.clone();
        let user_id = user_id.to_owned();
        let sealed = seal_link(&self.crypto, username, token);
        Box::pin(async move {
            let sealed = sealed?;
            lane.write(Lane::Foreground, "scrobble.lb.save", move |tx| {
                let now = to_iso(now_unix());
                tx.execute(
                    "INSERT INTO user_connections
                         (user_id, service, connection_data, enabled, created_at, updated_at)
                     VALUES (?1, ?2, ?3, 1, ?4, ?4)
                     ON CONFLICT (user_id, service) DO UPDATE SET
                         connection_data = excluded.connection_data,
                         updated_at = excluded.updated_at",
                    rusqlite::params![user_id, LISTENBRAINZ_SERVICE, sealed, now],
                )?;
                Ok(())
            })
            .await
            .map_err(|error| {
                tracing::warn!(%error, "listenbrainz link save failed");
                error.to_string()
            })
        })
    }

    fn status(&self, user_id: &str) -> BoxFuture<'_, Option<ListenBrainzLink>> {
        let pool = self.pool.clone();
        let crypto = Arc::clone(&self.crypto);
        let user_id = user_id.to_owned();
        Box::pin(async move {
            let sealed: Option<String> = sqlx::query_scalar(
                "SELECT connection_data FROM user_connections
                 WHERE user_id = ?1 AND service = ?2",
            )
            .bind(&user_id)
            .bind(LISTENBRAINZ_SERVICE)
            .fetch_optional(&pool)
            .await
            .map_err(|error| {
                tracing::warn!(%user_id, %error, "listenbrainz link read failed");
            })
            .ok()
            .flatten();
            let sealed: String = sealed?;
            open_link(&crypto, &sealed).map(|(username, _)| ListenBrainzLink { username })
        })
    }

    fn has_link(&self, user_id: &str) -> BoxFuture<'_, bool> {
        let pool = self.pool.clone();
        let user_id = user_id.to_owned();
        Box::pin(async move {
            let found: Option<String> = sqlx::query_scalar(
                "SELECT user_id FROM user_connections WHERE user_id = ?1 AND service = ?2",
            )
            .bind(&user_id)
            .bind(LISTENBRAINZ_SERVICE)
            .fetch_optional(&pool)
            .await
            .map_err(|error| {
                tracing::warn!(%user_id, %error, "listenbrainz link check failed");
            })
            .ok()
            .flatten();
            found.is_some()
        })
    }

    fn token_for(&self, user_id: &str) -> BoxFuture<'_, Option<String>> {
        let pool = self.pool.clone();
        let crypto = Arc::clone(&self.crypto);
        let user_id = user_id.to_owned();
        Box::pin(async move {
            let sealed: Option<String> = sqlx::query_scalar(
                "SELECT connection_data FROM user_connections
                 WHERE user_id = ?1 AND service = ?2",
            )
            .bind(&user_id)
            .bind(LISTENBRAINZ_SERVICE)
            .fetch_optional(&pool)
            .await
            .map_err(|error| {
                tracing::warn!(%user_id, %error, "listenbrainz token read failed");
            })
            .ok()
            .flatten();
            let sealed: String = sealed?;
            open_link(&crypto, &sealed)
                .and_then(|(_, token)| if token.is_empty() { None } else { Some(token) })
        })
    }

    fn delete(&self, user_id: &str) -> BoxFuture<'_, bool> {
        let lane = self.lane.clone();
        let user_id = user_id.to_owned();
        Box::pin(async move {
            lane.write(Lane::Foreground, "scrobble.lb.delete", move |tx| {
                Ok(tx.execute(
                    "DELETE FROM user_connections WHERE user_id = ?1 AND service = ?2",
                    rusqlite::params![user_id, LISTENBRAINZ_SERVICE],
                )?)
            })
            .await
            .map(|changed| changed > 0)
            .map_err(|error| {
                tracing::warn!(%error, "listenbrainz link delete failed");
            })
            .unwrap_or(false)
        })
    }
}

/// Standing-grant bookkeeping for personal-mix auto-request.
/// `acquire::requests` owns the approval queue; this seam lets prefs
/// updates notify it without depending on that service.
pub trait MixApprovalHook: Send + Sync {
    /// The auto-request toggle changed value.
    fn on_auto_request_toggled<'a>(
        &'a self,
        user_id: &'a str,
        role: &'a str,
        enabled: bool,
    ) -> BoxFuture<'a, ()>;
}

/// No-op hook for tests and for builds without the requests service wired.
#[derive(Debug, Default)]
pub struct NoopMixApprovalHook;

impl MixApprovalHook for NoopMixApprovalHook {
    fn on_auto_request_toggled<'a>(
        &'a self,
        _user_id: &'a str,
        _role: &'a str,
        _enabled: bool,
    ) -> BoxFuture<'a, ()> {
        Box::pin(async {})
    }
}

/// Cache invalidation after a link mutation. v2 resets the ListenBrainz
/// circuit breaker and clears the dependent caches on every per-user
/// change; the production hook does the same once those singletons exist
/// in v3.
pub trait ConnectionChangedHook: Send + Sync {
    /// A ListenBrainz link changed.
    fn on_listenbrainz_connection_changed(&self);
}

/// No-op hook for tests.
#[cfg(any(test, feature = "test-support"))]
#[derive(Debug, Default)]
pub struct NoopConnectionChangedHook;

#[cfg(any(test, feature = "test-support"))]
impl ConnectionChangedHook for NoopConnectionChangedHook {
    fn on_listenbrainz_connection_changed(&self) {}
}

/// Live presence privacy: a saved `now_playing_visibility` must reach the
/// now-playing feed at once, not after a restart (v2
/// `NowPlayingService.set_visibility`). Playback owns the feed; this seam
/// keeps plugins from depending on it.
pub trait VisibilityHook: Send + Sync {
    /// The user saved a visibility value.
    fn on_visibility_changed(&self, user_id: &str, visibility: &str);
}

/// No-op hook for builds without a presence feed.
#[derive(Debug, Default)]
pub struct NoopVisibilityHook;

impl VisibilityHook for NoopVisibilityHook {
    fn on_visibility_changed(&self, _user_id: &str, _visibility: &str) {}
}

/// Reads the standing-grant state for the prefs response
/// (`none`, `pending`, `approved`, `rejected`, `revoked`). `acquire::requests` owns
/// the queue; this seam keeps the response complete without depending on
/// it. Admins read `approved` by role whenever the toggle is on.
pub trait MixStateReader: Send + Sync {
    /// Standing-grant state for one user.
    fn auto_request_state<'a>(
        &'a self,
        user_id: &'a str,
        role: &'a str,
        toggle_on: bool,
    ) -> BoxFuture<'a, String>;
}

/// Static reader for tests: `none` off, `pending` on for non-admins,
/// `approved` for admins with the toggle on.
#[derive(Debug, Default)]
pub struct StaticMixState;

impl MixStateReader for StaticMixState {
    fn auto_request_state<'a>(
        &'a self,
        _user_id: &'a str,
        role: &'a str,
        toggle_on: bool,
    ) -> BoxFuture<'a, String> {
        let state = if !toggle_on {
            "none"
        } else if role == "admin" {
            "approved"
        } else {
            "pending"
        };
        Box::pin(async move { state.to_owned() })
    }
}

/// Scrobble settings service dependencies.
pub struct ScrobbleDeps {
    /// Prefs store.
    pub prefs: Arc<dyn ScrobblePrefsStore>,
    /// Link store.
    pub links: Arc<dyn ListenBrainzLinkStore>,
    /// Credential checker.
    pub verifier: Arc<dyn ListenBrainzVerifier>,
    /// Personal-mix hook.
    pub mix_hook: Arc<dyn MixApprovalHook>,
    /// Link-change hook.
    pub cache_hook: Arc<dyn ConnectionChangedHook>,
    /// Presence privacy hook.
    pub visibility_hook: Arc<dyn VisibilityHook>,
}

impl ScrobbleDeps {
    /// Wire the service from its parts.
    pub fn new(
        prefs: Arc<dyn ScrobblePrefsStore>,
        links: Arc<dyn ListenBrainzLinkStore>,
        verifier: Arc<dyn ListenBrainzVerifier>,
        mix_hook: Arc<dyn MixApprovalHook>,
        cache_hook: Arc<dyn ConnectionChangedHook>,
    ) -> Self {
        Self {
            prefs,
            links,
            verifier,
            mix_hook,
            cache_hook,
            visibility_hook: Arc::new(NoopVisibilityHook),
        }
    }

    /// Route saved visibility changes to the live presence feed.
    pub fn with_visibility_hook(mut self, hook: Arc<dyn VisibilityHook>) -> Self {
        self.visibility_hook = hook;
        self
    }
}

/// Every way a ListenBrainz connect can fail.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConnectError {
    /// No username was given. Usernames drive every per-user read; without
    /// one a "connected" account yields silently empty discovery.
    UsernameRequired,
    /// The upstream is rate-limiting.
    RateLimited,
    /// The credential did not check out.
    Rejected(String),
    /// The link failed to persist. The message is log-only.
    StoreFailed(String),
}

/// Every way a prefs update can fail.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PrefsError {
    /// An enum field named an unknown value.
    InvalidValue(String),
}

/// Connect one user's ListenBrainz account: verify first, store only on
/// success. The username is required even with a token.
pub async fn connect_listenbrainz(
    deps: &ScrobbleDeps,
    user_id: &str,
    username: &str,
    token: &str,
) -> Result<ListenBrainzLink, ConnectError> {
    if username.trim().is_empty() {
        return Err(ConnectError::UsernameRequired);
    }
    let outcome = deps.verifier.verify(username, token).await;
    if outcome.rate_limited {
        return Err(ConnectError::RateLimited);
    }
    if !outcome.valid {
        return Err(ConnectError::Rejected(outcome.message));
    }
    deps.links
        .save(user_id, username, token)
        .await
        .map_err(|reason| {
            tracing::warn!("listenbrainz link save failed: {reason}");
            ConnectError::StoreFailed(reason)
        })?;
    deps.cache_hook.on_listenbrainz_connection_changed();
    Ok(ListenBrainzLink {
        username: username.to_owned(),
    })
}

/// Disconnect one user's ListenBrainz account. False means there was none.
pub async fn disconnect_listenbrainz(deps: &ScrobbleDeps, user_id: &str) -> bool {
    let removed = deps.links.delete(user_id).await;
    if removed {
        deps.cache_hook.on_listenbrainz_connection_changed();
    }
    removed
}

/// Read one user's prefs.
pub async fn get_prefs(deps: &ScrobbleDeps, user_id: &str) -> ScrobblePrefs {
    deps.prefs.get(user_id).await
}

/// Update one user's prefs. Enum fields validate before anything writes;
/// only a real toggle change on auto-request notifies the approval hook,
/// so resending an unchanged full object never re-queues a grant.
pub async fn update_prefs(
    deps: &ScrobbleDeps,
    user_id: &str,
    role: &str,
    patch: &ScrobblePrefsPatch,
) -> Result<ScrobblePrefs, PrefsError> {
    if let Some(source) = &patch.primary_music_source
        && !PRIMARY_SOURCES.contains(&source.as_str())
    {
        return Err(PrefsError::InvalidValue(format!(
            "primary_music_source must be one of {}",
            PRIMARY_SOURCES.join(", ")
        )));
    }
    if let Some(visibility) = &patch.now_playing_visibility
        && !NOW_PLAYING_VISIBILITIES.contains(&visibility.as_str())
    {
        return Err(PrefsError::InvalidValue(format!(
            "now_playing_visibility must be one of {}",
            NOW_PLAYING_VISIBILITIES.join(", ")
        )));
    }
    let before = deps.prefs.get(user_id).await;
    deps.prefs.upsert(user_id, patch).await;
    if let Some(visibility) = &patch.now_playing_visibility {
        deps.visibility_hook
            .on_visibility_changed(user_id, visibility);
    }
    if let Some(enabled) = patch.auto_request_personal_mix
        && enabled != before.auto_request_personal_mix
    {
        deps.mix_hook
            .on_auto_request_toggled(user_id, role, enabled)
            .await;
    }
    Ok(deps.prefs.get(user_id).await)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn partial_upsert_keeps_untouched_fields() {
        let store = MemoryScrobblePrefsStore::new();
        store
            .upsert(
                "u1",
                &ScrobblePrefsPatch {
                    scrobble_to_listenbrainz: Some(true),
                    ..ScrobblePrefsPatch::default()
                },
            )
            .await;
        let prefs = store.get("u1").await;
        assert!(prefs.scrobble_to_listenbrainz);
        assert!(prefs.navidrome_handles_external_scrobbles);
        assert_eq!(prefs.primary_music_source, "listenbrainz");
    }
}
