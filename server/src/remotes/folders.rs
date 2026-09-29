//! Navidrome music-folder preferences and scope resolution.
//!
//! Each user either browses all folders or a selected subset. The scope
//! resolves against the folders the server currently exposes: selected ids
//! that vanished read back as stale, and a preference saved against a
//! different server identity (the URL changed underneath) resolves to an
//! empty selection with every id stale, failing closed instead of leaking
//! another library's catalog. When the source is down the resolution still
//! echoes the stored preference with `source_available` false.

use std::collections::HashMap;
use std::sync::Mutex;

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

/// Folder preference persistence port.
pub trait FolderStore: Send + Sync {
    /// Fetch one user's preference. Missing rows read as the "all" default.
    fn get<'a>(&'a self, user_id: &'a str) -> BoxFuture<'a, FolderPreference>;

    /// Replace one user's preference.
    fn set<'a>(&'a self, user_id: &'a str, preference: FolderPreference) -> BoxFuture<'a, ()>;
}

/// In-memory folder preference store.
#[derive(Debug, Default)]
pub struct MemoryFolderStore {
    inner: Mutex<HashMap<String, FolderPreference>>,
}

impl MemoryFolderStore {
    /// Empty store.
    pub fn new() -> Self {
        Self::default()
    }
}

impl FolderStore for MemoryFolderStore {
    fn get<'a>(&'a self, user_id: &'a str) -> BoxFuture<'a, FolderPreference> {
        Box::pin(async move {
            self.inner
                .lock()
                .map(|guard| guard.get(user_id).cloned().unwrap_or_default())
                .unwrap_or_default()
        })
    }

    fn set<'a>(&'a self, user_id: &'a str, preference: FolderPreference) -> BoxFuture<'a, ()> {
        Box::pin(async move {
            if let Ok(mut guard) = self.inner.lock() {
                guard.insert(user_id.to_owned(), preference);
            }
        })
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
