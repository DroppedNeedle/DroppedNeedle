//! Navidrome music-folder preferences and scope resolution.
//!
//! Each user either browses all folders or a selected subset. The scope
//! resolves against the folders the server currently exposes: selected ids
//! that vanished read back as stale, and a preference saved against a
//! different server identity (the URL changed underneath) resolves to an
//! empty selection with every id stale, failing closed instead of leaking
//! another library's catalog. When the source is down the resolution still
//! echoes the stored preference with `source_available` false.

use super::adapter::BoxFuture;

/// Stored folder preference for one user.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FolderPreference {
    /// "all" or "selected".
    pub mode: String,
    /// Selected folder ids. Empty unless mode is "selected".
    pub selected_folder_ids: Vec<String>,
    /// Server identity the selection was saved against, when selected.
    pub server_identity: Option<String>,
}

impl Default for FolderPreference {
    fn default() -> Self {
        Self {
            mode: "all".to_owned(),
            selected_folder_ids: Vec::new(),
            server_identity: None,
        }
    }
}

/// Effective catalog scope after resolution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FolderScope {
    /// "all" or "selected".
    pub mode: String,
    /// Effective folder ids. `None` means all folders (omit the param).
    pub folder_ids: Option<Vec<String>>,
}

/// Full preference resolution for one caller.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FolderResolution {
    /// Stored preference.
    pub preference: FolderPreference,
    /// Effective scope.
    pub scope: FolderScope,
    /// Folders the server currently exposes, as (id, name) pairs.
    pub available_folders: Vec<(String, String)>,
    /// Selected ids the server no longer exposes.
    pub stale_folder_ids: Vec<String>,
    /// False when the source is unreachable or unconfigured.
    pub source_available: bool,
}

/// Folder preference persistence. Errors carry a log-only cause.
pub trait FolderStore: Send + Sync {
    /// Fetch one user's preference. A missing row reads as the "all" default.
    fn get<'a>(&'a self, user_id: &'a str) -> BoxFuture<'a, Result<FolderPreference, String>>;

    /// Replace one user's preference.
    fn set<'a>(
        &'a self,
        user_id: &'a str,
        preference: FolderPreference,
    ) -> BoxFuture<'a, Result<(), String>>;
}

/// Preferences in `user_navidrome_folder_preferences`.
#[derive(Clone)]
pub struct SqliteFolderStore {
    db: droppedneedle_kernel::db::Db,
    clock: std::sync::Arc<dyn droppedneedle_kernel::clock::Clock>,
}

impl SqliteFolderStore {
    /// Bind the store to a migrated database, its rows dated by `clock`.
    pub fn new(
        db: droppedneedle_kernel::db::Db,
        clock: std::sync::Arc<dyn droppedneedle_kernel::clock::Clock>,
    ) -> Self {
        Self { db, clock }
    }
}

impl FolderStore for SqliteFolderStore {
    fn get<'a>(&'a self, user_id: &'a str) -> BoxFuture<'a, Result<FolderPreference, String>> {
        Box::pin(async move {
            let row: Option<(String, String, Option<String>)> = sqlx::query_as(
                "SELECT mode, selected_ids_json, server_identity \
                 FROM user_navidrome_folder_preferences WHERE user_id = ?1",
            )
            .bind(user_id)
            .fetch_optional(self.db.sqlx())
            .await
            .map_err(|error| {
                droppedneedle_kernel::db::map_sqlx_busy("remotes.folders.get", error).to_string()
            })?;
            let Some((mode, selected, server_identity)) = row else {
                return Ok(FolderPreference::default());
            };
            let selected_folder_ids: Vec<String> = serde_json::from_str(&selected)
                .map_err(|error| format!("folder preference ids are not a JSON list: {error}"))?;
            Ok(FolderPreference {
                mode,
                selected_folder_ids,
                server_identity,
            })
        })
    }

    fn set<'a>(
        &'a self,
        user_id: &'a str,
        preference: FolderPreference,
    ) -> BoxFuture<'a, Result<(), String>> {
        let user_id = user_id.to_owned();
        let now = self.clock.now_ms() as f64 / 1000.0;
        Box::pin(async move {
            let selected = serde_json::to_string(&preference.selected_folder_ids)
                .map_err(|error| error.to_string())?;
            self.db
                .write(
                    droppedneedle_kernel::db::Lane::Foreground,
                    "remotes.folders.set",
                    move |tx| {
                        tx.execute(
                            "INSERT INTO user_navidrome_folder_preferences \
                             (user_id, mode, selected_ids_json, server_identity, updated_at) \
                             VALUES (?1, ?2, ?3, ?4, ?5) \
                             ON CONFLICT (user_id) DO UPDATE SET mode = excluded.mode, \
                             selected_ids_json = excluded.selected_ids_json, \
                             server_identity = excluded.server_identity, \
                             updated_at = excluded.updated_at",
                            rusqlite::params![
                                user_id,
                                preference.mode,
                                selected,
                                preference.server_identity,
                                now
                            ],
                        )?;
                        Ok(())
                    },
                )
                .await
                .map_err(|error: droppedneedle_kernel::db::DbError| error.to_string())
        })
    }
}

/// How long a fetched folder list is reused. A grid of covers or a few
/// pages of browsing then share one `getMusicFolders` call, and a folder
/// added or removed on the server shows within a minute. The preference
/// screen always reads the server afresh.
pub const FOLDER_LIST_TTL_MS: i64 = 60_000;

/// The folders each user's Navidrome server exposed, as last fetched,
/// keyed by user and server identity.
#[derive(Default)]
pub struct FolderListCache {
    entries: std::sync::Mutex<std::collections::HashMap<(String, String), CachedFolders>>,
}

struct CachedFolders {
    fetched_at_ms: i64,
    folders: Vec<(String, String)>,
}

impl FolderListCache {
    /// The list fetched for `user_id` from the server at `identity`, while
    /// it is fresh at `now_ms`.
    pub fn get(&self, user_id: &str, identity: &str, now_ms: i64) -> Option<Vec<(String, String)>> {
        let entries = self.lock();
        let key = (user_id.to_owned(), identity.to_owned());
        entries
            .get(&key)
            .filter(|cached| now_ms - cached.fetched_at_ms < FOLDER_LIST_TTL_MS)
            .map(|cached| cached.folders.clone())
    }

    /// Keep a list just fetched, dropping entries that have gone stale.
    pub fn put(&self, user_id: &str, identity: &str, now_ms: i64, folders: Vec<(String, String)>) {
        let mut entries = self.lock();
        entries.retain(|_, cached| now_ms - cached.fetched_at_ms < FOLDER_LIST_TTL_MS);
        entries.insert(
            (user_id.to_owned(), identity.to_owned()),
            CachedFolders {
                fetched_at_ms: now_ms,
                folders,
            },
        );
    }

    /// Drop every list kept for `user_id`.
    pub fn forget(&self, user_id: &str) {
        self.lock().retain(|(user, _), _| user != user_id);
    }

    fn lock(
        &self,
    ) -> std::sync::MutexGuard<'_, std::collections::HashMap<(String, String), CachedFolders>> {
        self.entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

/// Resolve one user's scope. `available` is `None` when the source is
/// down or unconfigured; `server_identity` is the adapter's current
/// identity for the staleness check.
pub fn resolve_scope(
    preference: &FolderPreference,
    available: Option<&[(String, String)]>,
    server_identity: &str,
) -> FolderResolution {
    let unavailable = || FolderResolution {
        preference: preference.clone(),
        scope: FolderScope {
            mode: preference.mode.clone(),
            folder_ids: if preference.mode == "selected" {
                Some(preference.selected_folder_ids.clone())
            } else {
                None
            },
        },
        available_folders: Vec::new(),
        stale_folder_ids: Vec::new(),
        source_available: false,
    };
    let Some(available) = available else {
        return unavailable();
    };
    let available_ids: std::collections::HashSet<&str> =
        available.iter().map(|(id, _)| id.as_str()).collect();
    if preference.mode == "all" {
        return FolderResolution {
            preference: preference.clone(),
            scope: FolderScope {
                mode: "all".to_owned(),
                folder_ids: None,
            },
            available_folders: available.to_vec(),
            stale_folder_ids: Vec::new(),
            source_available: true,
        };
    }
    let identity_matches = preference
        .server_identity
        .as_deref()
        .is_none_or(|saved| saved == server_identity);
    if !identity_matches {
        return FolderResolution {
            preference: preference.clone(),
            scope: FolderScope {
                mode: "selected".to_owned(),
                folder_ids: Some(Vec::new()),
            },
            available_folders: available.to_vec(),
            stale_folder_ids: preference.selected_folder_ids.clone(),
            source_available: true,
        };
    }
    let (live, stale): (Vec<String>, Vec<String>) = preference
        .selected_folder_ids
        .iter()
        .cloned()
        .partition(|id| available_ids.contains(id.as_str()));
    FolderResolution {
        preference: preference.clone(),
        scope: FolderScope {
            mode: "selected".to_owned(),
            folder_ids: Some(live),
        },
        available_folders: available.to_vec(),
        stale_folder_ids: stale,
        source_available: true,
    }
}

/// Validate and store one user's preference. Selected ids must exist on
/// the server right now; the current server identity pins the selection.
pub fn checked_preference(
    mode: &str,
    selected_folder_ids: &[String],
    available: &[(String, String)],
    server_identity: &str,
) -> Result<FolderPreference, FolderSaveError> {
    if mode == "all" {
        if !selected_folder_ids.is_empty() {
            return Err(FolderSaveError::AllWithIds);
        }
        return Ok(FolderPreference::default());
    }
    if mode != "selected" {
        return Err(FolderSaveError::InvalidMode);
    }
    if selected_folder_ids.is_empty() {
        return Err(FolderSaveError::EmptySelection);
    }
    let unique: std::collections::HashSet<&str> =
        selected_folder_ids.iter().map(String::as_str).collect();
    if unique.len() != selected_folder_ids.len() {
        return Err(FolderSaveError::DuplicateIds);
    }
    let available_ids: std::collections::HashSet<&str> =
        available.iter().map(|(id, _)| id.as_str()).collect();
    if selected_folder_ids
        .iter()
        .any(|id| !available_ids.contains(id.as_str()))
    {
        return Err(FolderSaveError::UnknownIds);
    }
    Ok(FolderPreference {
        mode: "selected".to_owned(),
        selected_folder_ids: selected_folder_ids.to_vec(),
        server_identity: Some(server_identity.to_owned()),
    })
}

/// Every way a folder-preference save can fail. All render as 4xx.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FolderSaveError {
    /// "all" arrived with selected ids.
    AllWithIds,
    /// Mode is neither "all" nor "selected".
    InvalidMode,
    /// "selected" arrived with no ids.
    EmptySelection,
    /// The selection repeats an id.
    DuplicateIds,
    /// The selection names folders the server does not expose.
    UnknownIds,
}

impl std::fmt::Display for FolderSaveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::AllWithIds => f.write_str("All folders cannot include selected folder IDs"),
            Self::InvalidMode => f.write_str("Invalid folder preference mode"),
            Self::EmptySelection => f.write_str("Select at least one music folder"),
            Self::DuplicateIds => f.write_str("Duplicate music folder IDs are not allowed"),
            Self::UnknownIds => f.write_str("One or more selected music folders are unavailable"),
        }
    }
}
