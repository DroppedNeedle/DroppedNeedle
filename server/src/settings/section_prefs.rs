//! Per-user section visibility prefs (`/me/section-prefs`).
//!
//! The catalog is static code (home/discover/sidebar times a fixed entry
//! list); only the per-user disabled sets persist, in
//! `user_section_prefs`. Reads overlay the disabled set on the catalog and
//! compute `available` from service requirements plus per-user link state
//! (ListenBrainz row, Last.fm session behind the master switch; the
//! native library is always present). Writes replace one page's disabled
//! set per call and reject unknown keys, so a stale client learns its
//! catalog drifted instead of silently dropping toggles.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;

use futures_util::future::BoxFuture;
use sqlx::{Row as _, SqlitePool};

use super::error::SettingsError;
use super::models::{SectionPrefItem, SectionPrefsResponse, SectionPrefsUpdate};
use crate::auth::users::stores::{LastFmStore, StoreError};
use crate::db::{Lane, WriteLane};
use crate::ids::IdGenerator;

/// Visible pages.
pub const PAGES: &[&str] = &["home", "discover", "sidebar"];

/// One static catalog entry.
pub struct CatalogEntry {
    /// Page id.
    pub page: &'static str,
    /// Section key.
    pub key: &'static str,
    /// Display title.
    pub title: &'static str,
    /// Description.
    pub description: &'static str,
    /// Layout zone.
    pub zone: &'static str,
    /// Service requirement (`listenbrainz`, `lastfm`, or `library`).
    pub requires: Option<&'static str>,
}

/// The static catalog (v2 `section_catalog`, verbatim).
pub const CATALOG: &[CatalogEntry] = &[
    CatalogEntry {
        page: "home",
        key: "trending_artists",
        title: "Trending Artists",
        description: "Artists trending across your music source right now.",
        zone: "What's Hot",
        requires: None,
    },
    CatalogEntry {
        page: "home",
        key: "popular_albums",
        title: "Popular Now",
        description: "Albums popular across your music source this week.",
        zone: "What's Hot",
        requires: None,
    },
    CatalogEntry {
        page: "home",
        key: "weekly_exploration",
        title: "Weekly Exploration",
        description: "Your ListenBrainz weekly exploration playlist.",
        zone: "For You",
        requires: Some("listenbrainz"),
    },
    CatalogEntry {
        page: "home",
        key: "your_top_albums",
        title: "Your Top Albums",
        description: "What you personally played most this month.",
        zone: "For You",
        requires: None,
    },
    CatalogEntry {
        page: "home",
        key: "recently_played",
        title: "Recently Played",
        description: "Your latest plays in DroppedNeedle.",
        zone: "For You",
        requires: None,
    },
    CatalogEntry {
        page: "home",
        key: "recently_added",
        title: "Recently Added",
        description: "The newest imports in your library.",
        zone: "For You",
        requires: Some("library"),
    },
    CatalogEntry {
        page: "home",
        key: "favorite_artists",
        title: "Favorite Artists",
        description: "Artists from your loved tracks.",
        zone: "Your Library",
        requires: None,
    },
    CatalogEntry {
        page: "home",
        key: "library_artists",
        title: "Library Artists",
        description: "A shelf of artists from your library.",
        zone: "Your Library",
        requires: Some("library"),
    },
    CatalogEntry {
        page: "home",
        key: "library_albums",
        title: "Library Albums",
        description: "A shelf of albums from your library.",
        zone: "Your Library",
        requires: Some("library"),
    },
    CatalogEntry {
        page: "home",
        key: "genre_list",
        title: "Browse Genres",
        description: "Genre tiles built from your library.",
        zone: "Browse Genres",
        requires: Some("library"),
    },
    CatalogEntry {
        page: "discover",
        key: "discover_queue",
        title: "Discover Queue",
        description: "A personalised album-by-album discovery deck.",
        zone: "Essentials",
        requires: None,
    },
    CatalogEntry {
        page: "discover",
        key: "playlist_discovery",
        title: "Discover for a Playlist",
        description: "Album suggestions seeded from any playlist.",
        zone: "Essentials",
        requires: None,
    },
    CatalogEntry {
        page: "discover",
        key: "listeners_like_you",
        title: "Listening Lounge",
        description: "Browse albums picked for you by ear - tap a cover to hear it.",
        zone: "Essentials",
        requires: None,
    },
    CatalogEntry {
        page: "discover",
        key: "daily_mixes",
        title: "Daily Mixes",
        description: "Genre-clustered mixes of new and familiar albums.",
        zone: "Made For You",
        requires: Some("library"),
    },
    CatalogEntry {
        page: "discover",
        key: "radio_sections",
        title: "Radio Stations",
        description: "Album radios seeded by your top artists.",
        zone: "Made For You",
        requires: None,
    },
    CatalogEntry {
        page: "discover",
        key: "because_you_listen_to",
        title: "Because You Listened",
        description: "Artists similar to the ones you play most.",
        zone: "Because You Listened",
        requires: None,
    },
    CatalogEntry {
        page: "discover",
        key: "artists_you_might_like",
        title: "Artists You Might Like",
        description: "A wider net of similar artists.",
        zone: "Because You Listened",
        requires: None,
    },
    CatalogEntry {
        page: "discover",
        key: "popular_in_your_genres",
        title: "Popular in Your Genres",
        description: "Big names in the genres you listen to.",
        zone: "Because You Listened",
        requires: None,
    },
    CatalogEntry {
        page: "discover",
        key: "fresh_releases",
        title: "Fresh Releases",
        description: "New releases picked for you by ListenBrainz.",
        zone: "New & Fresh",
        requires: Some("listenbrainz"),
    },
    CatalogEntry {
        page: "discover",
        key: "top_picks",
        title: "Top Picks for You",
        description: "Albums we think you'd like, scored against your taste.",
        zone: "New & Fresh",
        requires: None,
    },
    CatalogEntry {
        page: "discover",
        key: "new_from_followed",
        title: "New From Artists You Follow",
        description: "Fresh releases from your followed artists.",
        zone: "New & Fresh",
        requires: None,
    },
    CatalogEntry {
        page: "discover",
        key: "missing_essentials",
        title: "Missing Essentials",
        description: "Celebrated albums missing from artists you collect.",
        zone: "New & Fresh",
        requires: Some("library"),
    },
    CatalogEntry {
        page: "discover",
        key: "weekly_exploration",
        title: "Weekly Exploration",
        description: "Your ListenBrainz weekly exploration playlist.",
        zone: "New & Fresh",
        requires: Some("listenbrainz"),
    },
    CatalogEntry {
        page: "discover",
        key: "anniversaries",
        title: "Milestone Anniversaries",
        description: "Library albums turning 10, 20, 30… this year.",
        zone: "From Your Library",
        requires: Some("library"),
    },
    CatalogEntry {
        page: "discover",
        key: "rediscover",
        title: "Rediscover",
        description: "Library albums you haven't played in a while.",
        zone: "From Your Library",
        requires: None,
    },
    CatalogEntry {
        page: "discover",
        key: "lastfm_recent_scrobbles",
        title: "Recent Scrobbles",
        description: "Your latest Last.fm scrobbles.",
        zone: "From Your Library",
        requires: Some("lastfm"),
    },
    CatalogEntry {
        page: "discover",
        key: "unexplored_genres",
        title: "Unexplored Genres",
        description: "Genres adjacent to your taste you haven't dug into.",
        zone: "Browse Genres",
        requires: None,
    },
    CatalogEntry {
        page: "discover",
        key: "genre_list",
        title: "Browse Genres",
        description: "Genre tiles built from your library.",
        zone: "Browse Genres",
        requires: Some("library"),
    },
    CatalogEntry {
        page: "discover",
        key: "globally_trending",
        title: "Globally Trending",
        description: "Artists trending worldwide.",
        zone: "Trending Now",
        requires: None,
    },
    CatalogEntry {
        page: "discover",
        key: "lastfm_weekly_artist_chart",
        title: "Last.fm Weekly Artists",
        description: "Your Last.fm weekly artist chart.",
        zone: "Trending Now",
        requires: Some("lastfm"),
    },
    CatalogEntry {
        page: "discover",
        key: "lastfm_weekly_album_chart",
        title: "Last.fm Weekly Albums",
        description: "Your Last.fm weekly album chart.",
        zone: "Trending Now",
        requires: Some("lastfm"),
    },
    CatalogEntry {
        page: "sidebar",
        key: "youtube",
        title: "YouTube",
        description: "The YouTube entry in the sidebar.",
        zone: "Services",
        requires: None,
    },
    CatalogEntry {
        page: "sidebar",
        key: "jellyfin",
        title: "Jellyfin",
        description: "The Jellyfin entry in the sidebar.",
        zone: "Services",
        requires: None,
    },
    CatalogEntry {
        page: "sidebar",
        key: "navidrome",
        title: "Navidrome",
        description: "The Navidrome entry in the sidebar.",
        zone: "Services",
        requires: None,
    },
    CatalogEntry {
        page: "sidebar",
        key: "plex",
        title: "Plex",
        description: "The Plex entry in the sidebar.",
        zone: "Services",
        requires: None,
    },
    CatalogEntry {
        page: "sidebar",
        key: "localfiles",
        title: "Local Files",
        description: "The Local Files entry in the sidebar.",
        zone: "Services",
        requires: None,
    },
];

/// Known keys for one page.
pub fn valid_keys(page: &str) -> HashSet<&'static str> {
    CATALOG
        .iter()
        .filter(|entry| entry.page == page)
        .map(|entry| entry.key)
        .collect()
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
    pub lastfm: Arc<dyn LastFmStore>,
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
    /// Reader pool.
    pub pool: SqlitePool,
    /// Writer lane.
    pub lane: Arc<WriteLane>,
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
            .fetch_all(&self.pool)
            .await
            .map_err(|cause| StoreError::Internal(cause.to_string()))?;
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
        let lane = self.lane.clone();
        let user_id = user_id.to_owned();
        let page = page.to_owned();
        let mut disabled: Vec<String> = disabled.iter().cloned().collect();
        disabled.sort();
        Box::pin(async move {
            lane.write(
                Lane::Foreground,
                "settings.section_prefs.replace",
                move |tx| {
                    tx.execute(
                        "DELETE FROM user_section_prefs WHERE user_id = ?1 AND page = ?2",
                        rusqlite::params![user_id, page],
                    )
                    .map_err(crate::db::OpError::Sql)?;
                    for key in &disabled {
                        tx.execute(
                            "INSERT INTO user_section_prefs (user_id, page, section_key, enabled, updated_at)
                             VALUES (?1, ?2, ?3, 0, strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))",
                            rusqlite::params![user_id, page, key],
                        )
                        .map_err(crate::db::OpError::Sql)?;
                    }
                    Ok(())
                },
            )
            .await
            .map_err(|cause| StoreError::Internal(cause.to_string()))
        })
    }
}

/// Build one page: catalog entries overlaid with the disabled set,
/// `available` from requirements plus link state. The Last.fm master
/// switch gates `lastfm` sections (v2 required the global creds that R7
/// deleted); the library is always present.
pub async fn page(
    store: &dyn SectionPrefsStore,
    links: &dyn LinkStatus,
    ids: &dyn IdGenerator,
    user_id: &str,
    page: &str,
    lastfm_master: bool,
) -> Result<Vec<SectionPrefItem>, SettingsError> {
    let disabled = store
        .get_disabled(user_id, page)
        .await
        .map_err(|cause| SettingsError::internal(&cause.to_string(), ids))?;
    let listenbrainz = links.is_listenbrainz_linked(user_id).await;
    let lastfm = lastfm_master && links.is_lastfm_linked(user_id).await;
    let availability: HashMap<&str, bool> = HashMap::from([
        ("listenbrainz", listenbrainz),
        ("lastfm", lastfm),
        ("library", true),
    ]);
    Ok(CATALOG
        .iter()
        .filter(|entry| entry.page == page)
        .map(|entry| SectionPrefItem {
            key: entry.key.to_owned(),
            title: entry.title.to_owned(),
            description: entry.description.to_owned(),
            zone: entry.zone.to_owned(),
            enabled: !disabled.contains(entry.key),
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
    ids: &dyn IdGenerator,
    user_id: &str,
    lastfm_master: bool,
) -> Result<SectionPrefsResponse, SettingsError> {
    let mut pages = BTreeMap::new();
    for page_key in PAGES {
        pages.insert(
            (*page_key).to_owned(),
            page(store, links, ids, user_id, page_key, lastfm_master).await?,
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
    ids: &dyn IdGenerator,
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
        .filter(|key| !known.contains(key))
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
        .map_err(|cause| SettingsError::internal(&cause.to_string(), ids))?;
    page(store, links, ids, user_id, &update.page, lastfm_master).await
}

/// In-memory disabled sets, one per user page. Test wiring only: the
/// settings test bundle mounts the prefs routes over this so briefs
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

/// Fixed link state. Test wiring only: briefs pin availability both
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
