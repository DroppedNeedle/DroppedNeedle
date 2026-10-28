//! Per-user section visibility prefs (`/me/section-prefs`).
//!
//! The home and discover entries come from the catalog's section list,
//! the sidebar's from the list here; only the per-user disabled sets
//! persist, in
//! `user_section_prefs`. Reads overlay the disabled set on the catalog and
//! compute `available` from service requirements plus per-user link state
//! (ListenBrainz row, Last.fm session behind the master switch; the
//! native library is always present). Writes replace one page's disabled
//! set per call and reject unknown keys, so a stale client learns its
//! catalog drifted instead of silently dropping toggles.

use std::collections::{BTreeMap, HashMap, HashSet};

use futures_util::future::BoxFuture;
use sqlx::{Row as _, SqlitePool};

use super::models::{SectionPrefItem, SectionPrefsResponse, SectionPrefsUpdate};
use droppedneedle_auth::sqlite::LastFmLinks;
use droppedneedle_auth::users::stores::StoreError;
use droppedneedle_catalog::reads::discover::sections::{self, Page};
use droppedneedle_kernel::db::{Db, DbError, Lane, sqlx_is_busy};
use droppedneedle_kernel::runtime_config::settings_error::SettingsError;

/// Visible pages.
pub const PAGES: &[&str] = &["home", "discover", "sidebar"];

/// One entry of the section settings.
pub struct CatalogEntry {
    /// Section key.
    pub key: String,
    /// Display title.
    pub title: &'static str,
    /// Description.
    pub description: &'static str,
    /// Layout zone.
    pub zone: &'static str,
    /// Service requirement (`listenbrainz`, `lastfm`, or `library`).
    pub requires: Option<&'static str>,
}

/// The sidebar's service entries: key, title and description.
const SIDEBAR: &[(&str, &str, &str)] = &[
    ("youtube", "YouTube", "The YouTube entry in the sidebar."),
    ("jellyfin", "Jellyfin", "The Jellyfin entry in the sidebar."),
    (
        "navidrome",
        "Navidrome",
        "The Navidrome entry in the sidebar.",
    ),
    ("plex", "Plex", "The Plex entry in the sidebar."),
    (
        "localfiles",
        "Local Files",
        "The Local Files entry in the sidebar.",
    ),
];

/// The entries of one page, in page order.
pub fn catalog(page: &str) -> Vec<CatalogEntry> {
    let sections_of = |page: Page| {
        sections::of(page)
            .map(|def| CatalogEntry {
                key: def.key.name(),
                title: def.title,
                description: def.description,
                zone: def.zone.label(),
                requires: def.requires.map(sections::Requires::as_str),
            })
            .collect()
    };
    match page {
        "home" => sections_of(Page::Home),
        "discover" => sections_of(Page::Discover),
        "sidebar" => SIDEBAR
            .iter()
            .map(|(key, title, description)| CatalogEntry {
                key: (*key).to_owned(),
                title,
                description,
                zone: "Services",
                requires: None,
            })
            .collect(),
        _ => Vec::new(),
    }
}

/// Known keys for one page.
pub fn valid_keys(page: &str) -> HashSet<String> {
    catalog(page).into_iter().map(|entry| entry.key).collect()
}

/// Per-user service link state behind section availability.
pub trait LinkStatus: Send + Sync {
    /// Whether the user linked ListenBrainz (an enabled row exists).
    fn is_listenbrainz_linked<'a>(&'a self, user_id: &'a str) -> BoxFuture<'a, bool>;
    /// Whether the user linked Last.fm (a session was exchanged).
    fn is_lastfm_linked<'a>(&'a self, user_id: &'a str) -> BoxFuture<'a, bool>;
}

/// Per-user disabled sets, one replaceable set per page.
pub trait SectionPrefsStore: Send + Sync {
    /// The disabled keys for one user page.
    fn get_disabled<'a>(
        &'a self,
        user_id: &'a str,
        page: &'a str,
    ) -> BoxFuture<'a, Result<HashSet<String>, StoreError>>;
    /// Replace one user page's disabled set.
    fn set_disabled<'a>(
        &'a self,
        user_id: &'a str,
        page: &'a str,
        disabled: &'a HashSet<String>,
    ) -> BoxFuture<'a, Result<(), StoreError>>;
}

/// Production link state: ListenBrainz rows from `user_connections`,
/// Last.fm sessions from the auth-owned link store.
pub struct SqliteLinkStatus {
    /// Reader pool.
    pub pool: SqlitePool,
    /// Last.fm link store.
    pub lastfm: LastFmLinks,
}

impl LinkStatus for SqliteLinkStatus {
    fn is_listenbrainz_linked<'a>(&'a self, user_id: &'a str) -> BoxFuture<'a, bool> {
        Box::pin(async move {
            let row: Option<i64> = sqlx::query_scalar(
                "SELECT 1 FROM user_connections WHERE user_id = ?1 AND service = 'listenbrainz' AND enabled = 1 LIMIT 1",
            )
            .bind(user_id)
            .fetch_optional(&self.pool)
            .await
            .unwrap_or(None);
            row.is_some()
        })
    }

    fn is_lastfm_linked<'a>(&'a self, user_id: &'a str) -> BoxFuture<'a, bool> {
        Box::pin(async move {
            self.lastfm
                .get(user_id)
                .await
                .ok()
                .flatten()
                .is_some_and(|link| link.session_key_encrypted.is_some())
        })
    }
}

/// Production toggle store over `user_section_prefs`.
pub struct SqliteSectionPrefsStore {
    /// The database.
    pub db: Db,
}

impl SectionPrefsStore for SqliteSectionPrefsStore {
    fn get_disabled<'a>(
        &'a self,
        user_id: &'a str,
        page: &'a str,
    ) -> BoxFuture<'a, Result<HashSet<String>, StoreError>> {
        Box::pin(async move {
            let rows = sqlx::query(
                "SELECT section_key FROM user_section_prefs WHERE user_id = ?1 AND page = ?2 AND enabled = 0",
            )
            .bind(user_id)
            .bind(page)
            .fetch_all(self.db.sqlx())
            .await
            .map_err(|cause| {
                if sqlx_is_busy(&cause) {
                    StoreError::Busy
                } else {
                    StoreError::Internal(cause.to_string())
                }
            })?;
            Ok(rows
                .iter()
                .map(|row| row.get::<String, _>("section_key"))
                .collect())
        })
    }

    fn set_disabled<'a>(
        &'a self,
        user_id: &'a str,
        page: &'a str,
        disabled: &'a HashSet<String>,
    ) -> BoxFuture<'a, Result<(), StoreError>> {
        let db = self.db.clone();
        let user_id = user_id.to_owned();
        let page = page.to_owned();
        let mut disabled: Vec<String> = disabled.iter().cloned().collect();
        disabled.sort();
        Box::pin(async move {
            db.write(
                Lane::Foreground,
                "settings.section_prefs.replace",
                move |tx| {
                    tx.execute(
                        "DELETE FROM user_section_prefs WHERE user_id = ?1 AND page = ?2",
                        rusqlite::params![user_id, page],
                    )?;
                    for key in &disabled {
                        tx.execute(
                            "INSERT INTO user_section_prefs (user_id, page, section_key, enabled, updated_at)
                             VALUES (?1, ?2, ?3, 0, strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))",
                            rusqlite::params![user_id, page, key],
                        )?;
                    }
                    Ok(())
                },
            )
            .await
            .map_err(|cause| match cause {
                DbError::Busy { .. } => StoreError::Busy,
                other => StoreError::Internal(other.to_string()),
            })
        })
    }
}

/// Build one page: catalog entries overlaid with the disabled set,
/// `available` from requirements plus link state. The Last.fm master
/// switch gates `lastfm` sections (v2 required global creds, which v3
/// dropped); the library is always present.
pub async fn page(
    store: &dyn SectionPrefsStore,
    links: &dyn LinkStatus,
    user_id: &str,
    page: &str,
    lastfm_master: bool,
) -> Result<Vec<SectionPrefItem>, SettingsError> {
    let disabled = store
        .get_disabled(user_id, page)
        .await
        .map_err(|cause| store_failure("section prefs read", cause))?;
    let listenbrainz = links.is_listenbrainz_linked(user_id).await;
    let lastfm = lastfm_master && links.is_lastfm_linked(user_id).await;
    let availability: HashMap<&str, bool> = HashMap::from([
        ("listenbrainz", listenbrainz),
        ("lastfm", lastfm),
        ("library", true),
    ]);
    Ok(catalog(page)
        .into_iter()
        .map(|entry| SectionPrefItem {
            enabled: !disabled.contains(&entry.key),
            key: entry.key,
            title: entry.title.to_owned(),
            description: entry.description.to_owned(),
            zone: entry.zone.to_owned(),
            available: entry
                .requires
                .and_then(|requirement| availability.get(requirement).copied())
                .unwrap_or(true),
            requires: entry.requires.map(str::to_owned),
        })
        .collect())
}

/// Build the full GET view (all three pages).
pub async fn full_response(
    store: &dyn SectionPrefsStore,
    links: &dyn LinkStatus,
    user_id: &str,
    lastfm_master: bool,
) -> Result<SectionPrefsResponse, SettingsError> {
    let mut pages = BTreeMap::new();
    for page_key in PAGES {
        pages.insert(
            (*page_key).to_owned(),
            page(store, links, user_id, page_key, lastfm_master).await?,
        );
    }
    Ok(SectionPrefsResponse { pages })
}

/// Replace one page's disabled set from a PUT body. The page must be a
/// known page and every key must belong to that page's catalog; unknown
/// keys are a 400 naming the offenders, so a stale client learns its
/// catalog drifted instead of silently dropping toggles. Returns the
/// rebuilt page.
pub async fn save_page(
    store: &dyn SectionPrefsStore,
    links: &dyn LinkStatus,
    user_id: &str,
    update: &SectionPrefsUpdate,
    lastfm_master: bool,
) -> Result<Vec<SectionPrefItem>, SettingsError> {
    if !PAGES.contains(&update.page.as_str()) {
        return Err(SettingsError::InvalidInput {
            message: format!(
                "Unknown section page: {:?}. Expected one of: {}.",
                update.page,
                PAGES.join(", ")
            ),
        });
    }
    let known = valid_keys(&update.page);
    let mut unknown: Vec<&str> = update
        .sections
        .iter()
        .map(|section| section.key.as_str())
        .filter(|key| !known.contains(*key))
        .collect();
    unknown.sort_unstable();
    unknown.dedup();
    if !unknown.is_empty() {
        return Err(SettingsError::InvalidInput {
            message: format!("Unknown section keys: {}.", unknown.join(", ")),
        });
    }
    let disabled: HashSet<String> = update
        .sections
        .iter()
        .filter(|section| !section.enabled)
        .map(|section| section.key.clone())
        .collect();
    store
        .set_disabled(user_id, &update.page, &disabled)
        .await
        .map_err(|cause| store_failure("section prefs write", cause))?;
    page(store, links, user_id, &update.page, lastfm_master).await
}

/// A busy database answers `SERVER_BUSY`; anything else is a 500.
fn store_failure(operation: &str, cause: StoreError) -> SettingsError {
    match cause {
        StoreError::Busy => SettingsError::Busy {
            cause: format!("{operation}: database busy"),
        },
        other => SettingsError::internal(&format_args!("{operation}: {other}")),
    }
}

/// In-memory disabled sets, one per user page. Test wiring only: the
/// settings test bundle mounts the prefs routes over this so tests
/// round-trip without a database.
#[cfg(any(test, feature = "test-support"))]
pub struct MemorySectionPrefsStore {
    /// Disabled keys by `(user id, page)`.
    pub disabled: std::sync::Mutex<HashMap<(String, String), HashSet<String>>>,
}

#[cfg(any(test, feature = "test-support"))]
impl MemorySectionPrefsStore {
    /// Empty store.
    pub fn new() -> Self {
        Self {
            disabled: std::sync::Mutex::new(HashMap::new()),
        }
    }
}

#[cfg(any(test, feature = "test-support"))]
impl Default for MemorySectionPrefsStore {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(any(test, feature = "test-support"))]
impl SectionPrefsStore for MemorySectionPrefsStore {
    fn get_disabled<'a>(
        &'a self,
        user_id: &'a str,
        page: &'a str,
    ) -> BoxFuture<'a, Result<HashSet<String>, StoreError>> {
        Box::pin(async move {
            let guard = self
                .disabled
                .lock()
                .map_err(|cause| StoreError::Internal(format!("prefs lock poisoned: {cause}")))?;
            Ok(guard
                .get(&(user_id.to_owned(), page.to_owned()))
                .cloned()
                .unwrap_or_default())
        })
    }

    fn set_disabled<'a>(
        &'a self,
        user_id: &'a str,
        page: &'a str,
        disabled: &'a HashSet<String>,
    ) -> BoxFuture<'a, Result<(), StoreError>> {
        Box::pin(async move {
            let mut guard = self
                .disabled
                .lock()
                .map_err(|cause| StoreError::Internal(format!("prefs lock poisoned: {cause}")))?;
            guard.insert((user_id.to_owned(), page.to_owned()), disabled.clone());
            Ok(())
        })
    }
}

/// Fixed link state. Test wiring only: tests pin availability both
/// ways without seeding connection rows.
#[cfg(any(test, feature = "test-support"))]
pub struct StaticLinkStatus {
    /// Reported ListenBrainz link state.
    pub listenbrainz: bool,
    /// Reported Last.fm link state.
    pub lastfm: bool,
}

#[cfg(any(test, feature = "test-support"))]
impl LinkStatus for StaticLinkStatus {
    fn is_listenbrainz_linked<'a>(&'a self, _user_id: &'a str) -> BoxFuture<'a, bool> {
        Box::pin(async move { self.listenbrainz })
    }

    fn is_lastfm_linked<'a>(&'a self, _user_id: &'a str) -> BoxFuture<'a, bool> {
        Box::pin(async move { self.lastfm })
    }
}
