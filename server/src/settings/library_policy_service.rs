//! The library policy routes: policy tree, impact and apply previews,
//! restorable roots and restore, and the path-mapping dry run, plus the
//! guard that keeps a save from orphaning the catalog.
//!
//! Each call reads the saved library settings, normalizes them on the
//! blocking pool (normalizing checks root paths on disk), and reads the
//! catalog through the [`LibraryPolicyCatalog`] port. Writes go through
//! [`SettingsService::update_library`], which checks the revision and
//! saves under the config store's write lock and runs the post-save
//! fan-out. Without a wired catalog the
//! tree and impact previews leave their counts empty and the routes that
//! need catalog rows answer 503.

use std::collections::BTreeMap;
use std::sync::Arc;

use super::error::SettingsError;
use super::library_policy::{self, LibraryPolicyCatalog, ResolvedLibraryPolicy};
use super::models::{
    LibraryPathMappingReport, LibraryPolicyApplyPreviewResponse, LibraryPolicyApplyRequest,
    LibraryPolicyImpactRequest, LibraryPolicyImpactResponse, LibraryPolicyTreeResponse,
    LibraryRestorableRootsResponse, LibraryRestoreRootsRequest, LibrarySettingsResponse,
    LibrarySettingsSaveRequest,
};
use super::services::SettingsService;
use crate::runtime_config::Masked;
use crate::runtime_config::secret_sections::TypedLibrary;

/// Library policy reads, previews, and the guarded saves.
pub struct LibraryPolicyService {
    settings: Arc<SettingsService>,
    catalog: Option<Arc<dyn LibraryPolicyCatalog>>,
}

impl LibraryPolicyService {
    /// Build over the settings service and, when wired, the catalog.
    pub fn new(
        settings: Arc<SettingsService>,
        catalog: Option<Arc<dyn LibraryPolicyCatalog>>,
    ) -> Self {
        Self { settings, catalog }
    }

    fn catalog(&self) -> Result<&dyn LibraryPolicyCatalog, SettingsError> {
        self.catalog
            .as_deref()
            .ok_or_else(|| SettingsError::Unavailable {
                message: "Library policy previews need the catalog database.".to_owned(),
            })
    }

    fn catalog_error(&self, cause: String) -> SettingsError {
        SettingsError::internal(&cause, self.settings.ids.as_ref())
    }

    /// Run `op` on the blocking pool with the saved settings normalized.
    async fn with_saved<T, F>(&self, op: F) -> Result<T, SettingsError>
    where
        T: Send + 'static,
        F: FnOnce(Masked<ResolvedLibraryPolicy>) -> Result<T, SettingsError> + Send + 'static,
    {
        let settings = self.settings.clone();
        let task = tokio::task::spawn_blocking(move || {
            let stored = settings.get_masked::<TypedLibrary>()?;
            op(stored.map(|library| library_policy::resolve_stored(&library)))
        });
        match task.await {
            Ok(result) => result,
            Err(cause) => Err(SettingsError::internal(
                &format!("library policy task failed: {cause}"),
                self.settings.ids.as_ref(),
            )),
        }
    }

    /// The saved roots and rules as a tree, with catalog file counts.
    pub async fn policy_tree(&self) -> Result<LibraryPolicyTreeResponse, SettingsError> {
        let mut tree = self
            .with_saved(|saved| Ok(library_policy::policy_tree(&saved)))
            .await?;
        if let Some(catalog) = self.catalog.as_deref() {
            let counts = catalog
                .scope_counts(&library_policy::tree_scopes(&tree))
                .await
                .map_err(|cause| self.catalog_error(cause))?;
            library_policy::fill_tree_counts(&mut tree, &counts);
        }
        Ok(tree)
    }

    /// What saving the candidate settings would change, and how many
    /// catalog files sit under the changed scopes.
    pub async fn preview_impact(
        &self,
        request: LibraryPolicyImpactRequest,
    ) -> Result<LibraryPolicyImpactResponse, SettingsError> {
        let (mut response, scopes) = self
            .with_saved(move |saved| {
                let proposed = library_policy::resolve(&request.settings)?;
                Ok(library_policy::preview_impact(
                    &saved,
                    proposed,
                    request.expected_policy_revision.as_deref(),
                ))
            })
            .await?;
        if let Some(catalog) = self.catalog.as_deref() {
            let totals = catalog
                .scope_totals(&scopes)
                .await
                .map_err(|cause| self.catalog_error(cause))?;
            response.indexed_file_count = Some(totals.indexed);
            response.on_disk_file_count = Some(totals.on_disk);
        }
        Ok(response)
    }

    /// How many catalog files a reconcile of the chosen saved scopes
    /// would revisit. A stale revision is a 409, an unknown scope a 400.
    pub async fn preview_apply(
        &self,
        request: LibraryPolicyApplyRequest,
    ) -> Result<LibraryPolicyApplyPreviewResponse, SettingsError> {
        let catalog = self.catalog()?;
        let scope_ids = request.scope_ids.clone();
        let (saved, scopes) = self
            .with_saved(move |saved| {
                if saved.policy_revision != request.expected_policy_revision {
                    return Err(SettingsError::StaleRevision {
                        message: "The library policy changed. Preview the reconciliation again."
                            .to_owned(),
                    });
                }
                let scopes = library_policy::apply_scopes(&saved, &request.scope_ids)?;
                Ok((saved, scopes))
            })
            .await?;
        let totals = catalog
            .scope_totals(&library_policy::scope_pairs(&scopes))
            .await
            .map_err(|cause| self.catalog_error(cause))?;
        Ok(library_policy::apply_preview(
            &saved, scope_ids, &scopes, totals.all,
        ))
    }

    /// Roots the catalog still holds tracks for but the settings no
    /// longer list.
    pub async fn restorable_roots(&self) -> Result<LibraryRestorableRootsResponse, SettingsError> {
        let catalog = self.catalog()?;
        let roots = catalog
            .catalog_roots()
            .await
            .map_err(|cause| self.catalog_error(cause))?;
        self.with_saved(move |saved| {
            Ok(LibraryRestorableRootsResponse {
                restorable_roots: library_policy::restorable_roots(&saved.settings, &roots),
                policy_revision: saved.policy_revision.clone(),
            })
        })
        .await
    }

    /// Put every removed root back (automatic, no rules), at its
    /// recovered path or the caller's override. The revision check, the
    /// choice of roots, and the write happen in one locked update.
    pub async fn restore_roots(
        &self,
        request: LibraryRestoreRootsRequest,
    ) -> Result<LibrarySettingsResponse, SettingsError> {
        let catalog = self.catalog()?;
        let roots = catalog
            .catalog_roots()
            .await
            .map_err(|cause| self.catalog_error(cause))?;
        let paths: BTreeMap<String, String> = request.paths.unwrap_or_default();
        let expected = request.expected_policy_revision;
        self.settings
            .update_library(move |stored| {
                library_policy::check_revision(&stored, &expected)?;
                let restorable = library_policy::restorable_roots(&stored, &roots);
                let restored = library_policy::with_restored_roots(stored, &restorable, &paths)?;
                Ok(library_policy::resolve(&restored)?.settings)
            })
            .await
    }

    /// Path-mapping dry run: does every catalog file map to exactly one
    /// saved root.
    pub async fn path_mapping(&self) -> Result<LibraryPathMappingReport, SettingsError> {
        let catalog = self.catalog()?;
        let sources = catalog
            .track_paths()
            .await
            .map_err(|cause| self.catalog_error(cause))?;
        self.with_saved(move |saved| Ok(library_policy::path_mapping(&saved, sources)))
            .await
    }

    /// Save the library settings, refusing to drop every root while the
    /// catalog holds tracks.
    pub async fn save(
        &self,
        request: LibrarySettingsSaveRequest,
    ) -> Result<LibrarySettingsResponse, SettingsError> {
        // A proposal keeps every submitted root, so only an empty one can
        // trip the guard.
        let has_tracks = if request.settings.library_roots.is_empty() {
            self.catalog_has_tracks().await?
        } else {
            false
        };
        self.settings.save_library(request, has_tracks).await
    }

    /// Remove every root at one path, with the same guard as a save.
    pub async fn remove_path(&self, path: &str) -> Result<LibrarySettingsResponse, SettingsError> {
        let has_tracks = self.catalog_has_tracks().await?;
        self.settings.remove_library_path(path, has_tracks).await
    }

    /// Whether the catalog holds tracks. False when the catalog is
    /// unwired, which turns the last-root guard off.
    async fn catalog_has_tracks(&self) -> Result<bool, SettingsError> {
        let Some(catalog) = self.catalog.as_deref() else {
            return Ok(false);
        };
        catalog
            .has_tracks()
            .await
            .map_err(|cause| self.catalog_error(cause))
    }
}
