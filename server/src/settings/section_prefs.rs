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

use rusqlite::OptionalExtension as _;

use super::models::{SectionPrefItem, SectionPrefsResponse, SectionPrefsUpdate};
use super::sql;
use droppedneedle_auth::sqlite::LastFmLinks;
use droppedneedle_catalog::discover::sections::{self, Page};
use droppedneedle_kernel::db::{Db, DbError, Lane};
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

/// Per-user section prefs over `user_section_prefs`, and the link state
/// behind section availability: ListenBrainz rows from `user_connections`,
/// Last.fm sessions from the auth-owned link store.
pub struct SectionPrefs {
    db: Db,
    lastfm: LastFmLinks,
}

impl SectionPrefs {
    /// The prefs over the database and the Last.fm link store.
    pub fn new(db: Db, lastfm: LastFmLinks) -> Self {
        Self { db, lastfm }
    }

    /// The disabled keys for one user page.
    async fn disabled(&self, user_id: &str, page: &str) -> Result<HashSet<String>, DbError> {
        let (user_id, page) = (user_id.to_owned(), page.to_owned());
        self.db
            .read("settings.section_prefs.read", move |conn| {
                conn.prepare_cached(sql::DISABLED_SECTIONS.text())?
                    .query_map([user_id, page], |row| row.get(0))?
                    .collect::<rusqlite::Result<HashSet<String>>>()
                    .map_err(DbError::from)
            })
            .await
    }

    /// Replace one user page's disabled set.
    async fn set_disabled(
        &self,
        user_id: &str,
        page: &str,
        disabled: &HashSet<String>,
    ) -> Result<(), DbError> {
        let (user_id, page) = (user_id.to_owned(), page.to_owned());
        let mut disabled: Vec<String> = disabled.iter().cloned().collect();
        disabled.sort();
        self.db
            .write(
                Lane::Foreground,
                "settings.section_prefs.replace",
                move |tx| {
                    tx.execute(
                        sql::CLEAR_SECTION_PREFS.text(),
                        rusqlite::params![user_id, page],
                    )?;
                    for key in &disabled {
                        tx.execute(
                            sql::DISABLE_SECTION.text(),
                            rusqlite::params![user_id, page, key],
                        )?;
                    }
                    Ok(())
                },
            )
            .await
    }

    /// Whether the user linked ListenBrainz (an enabled row exists). A
    /// read that fails counts as not linked and is logged.
    async fn is_listenbrainz_linked(&self, user_id: &str) -> bool {
        let user_id = user_id.to_owned();
        let linked = self
            .db
            .read("settings.section_prefs.listenbrainz", move |conn| {
                conn.prepare_cached(sql::LISTENBRAINZ_LINKED.text())?
                    .query_row([user_id], |_| Ok(()))
                    .optional()
                    .map_err(DbError::from)
            })
            .await;
        match linked {
            Ok(found) => found.is_some(),
            Err(error) => {
                tracing::warn!(%error, "ListenBrainz link read failed; its sections show as unavailable");
                false
            }
        }
    }

    /// Whether the user linked Last.fm (a session was exchanged).
    async fn is_lastfm_linked(&self, user_id: &str) -> bool {
        self.lastfm
            .get(user_id)
            .await
            .ok()
            .flatten()
            .is_some_and(|link| link.session_key_encrypted.is_some())
    }
}

/// Build one page: catalog entries overlaid with the disabled set,
/// `available` from requirements plus link state. The Last.fm master
/// switch gates `lastfm` sections (v2 required global creds, which v3
/// dropped); the library is always present.
pub async fn page(
    prefs: &SectionPrefs,
    user_id: &str,
    page: &str,
    lastfm_master: bool,
) -> Result<Vec<SectionPrefItem>, SettingsError> {
    let disabled = prefs
        .disabled(user_id, page)
        .await
        .map_err(|cause| store_failure("section prefs read", cause))?;
    let listenbrainz = prefs.is_listenbrainz_linked(user_id).await;
    let lastfm = lastfm_master && prefs.is_lastfm_linked(user_id).await;
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
    prefs: &SectionPrefs,
    user_id: &str,
    lastfm_master: bool,
) -> Result<SectionPrefsResponse, SettingsError> {
    let mut pages = BTreeMap::new();
    for page_key in PAGES {
        pages.insert(
            (*page_key).to_owned(),
            page(prefs, user_id, page_key, lastfm_master).await?,
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
    prefs: &SectionPrefs,
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
    prefs
        .set_disabled(user_id, &update.page, &disabled)
        .await
        .map_err(|cause| store_failure("section prefs write", cause))?;
    page(prefs, user_id, &update.page, lastfm_master).await
}

/// A busy database answers `SERVER_BUSY`; anything else is a 500.
fn store_failure(operation: &str, cause: DbError) -> SettingsError {
    match cause {
        DbError::Busy { .. } => SettingsError::Busy {
            cause: format!("{operation}: database busy"),
        },
        other => SettingsError::internal(&format_args!("{operation}: {other}")),
    }
}
