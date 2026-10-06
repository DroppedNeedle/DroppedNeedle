//! Library service: roots, scans, identification, and reviews.

use std::collections::HashMap;
use std::path::PathBuf;

use super::clock::now_ms;
use super::identify::models::{AlbumIdentity, IdentifyJob, IdentifyKind, LocalAlbumFacts};
use super::manage::publish_error;
use super::scan::models::{
    EffectivePolicy, ScanInventoryItem, ScanKind, ScanRequest, ScanRequestResult, ScanRun,
    ScanScope, ScanTrigger,
};
use super::scan::roots::{LibraryRoot, RootRegistry, fingerprint_roots};
use super::scan::store::ScanStore;
use super::wiring::LibrarySetup;

/// Why a library operation failed, independent of transport. The
/// HTTP layer maps each variant to a status.
#[derive(Debug)]
pub enum ServiceError {
    /// The named resource does not exist.
    NotFound,
    /// The request itself is invalid; the message is safe to show.
    InvalidInput { message: String },
    /// Valid input against the wrong state; the message is safe to show.
    Conflict { message: String },
    /// A store or I/O fault. The cause goes to the log, never the caller.
    Internal { cause: String },
}

impl ServiceError {
    /// A store or I/O fault with its cause kept for the log.
    pub fn internal(cause: &dyn std::fmt::Display) -> Self {
        Self::Internal {
            cause: cause.to_string(),
        }
    }
}

impl std::fmt::Display for ServiceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotFound => write!(f, "not found"),
            Self::InvalidInput { message } | Self::Conflict { message } => write!(f, "{message}"),
            Self::Internal { cause } => write!(f, "{cause}"),
        }
    }
}

impl std::error::Error for ServiceError {}

impl LibrarySetup {
    /// Add a library root. The path must exist, be absolute, and be a
    /// directory; the id must be unused. Adding the first root
    /// enables the library and marks the root dirty so Hook B picks
    /// it up for an initial scan. Blocking file and database work:
    /// handlers run this off the async runtime.
    pub fn add_root(
        &self,
        id: Option<String>,
        path: String,
        policy: EffectivePolicy,
    ) -> Result<(LibraryRoot, String), ServiceError> {
        let dir = PathBuf::from(&path);
        if !dir.is_absolute() {
            return Err(ServiceError::InvalidInput {
                message: "Root path must be absolute".to_owned(),
            });
        }
        let meta = std::fs::symlink_metadata(&dir).map_err(|_| ServiceError::InvalidInput {
            message: "Root path does not exist".to_owned(),
        })?;
        if !meta.file_type().is_dir() {
            return Err(ServiceError::InvalidInput {
                message: "Root path is not a directory".to_owned(),
            });
        }
        let registry = self.live_registry();
        let id = id.unwrap_or_else(|| self.ids.new_id());
        if id.is_empty() || id.contains('/') || id.contains('\0') {
            return Err(ServiceError::InvalidInput {
                message: "Root id is not a plain name".to_owned(),
            });
        }
        if registry.resolve(&id).is_some() {
            return Err(ServiceError::Conflict {
                message: "Root id already exists".to_owned(),
            });
        }
        let root = LibraryRoot::new(&id, dir, policy);
        let mut roots = registry.roots().to_vec();
        roots.push(root.clone());
        let revision = fingerprint_roots(&roots, true);
        self.registry
            .update(RootRegistry::new(roots, true, &revision));
        self.dirty.mark(&id);
        self.wakeups.notify("scan");
        // Open (or reopen) the publish cell under the new root set.
        // A reconcile failure here is a 409: the root is registered
        // but publishing stays closed until recovery passes.
        {
            let mut cell = self
                .publish
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let registry = self.live_registry();
            let _guards = if cell.needs_refresh(&registry) {
                self.publish_guards(&registry)
            } else {
                Vec::new()
            };
            cell.refresh(&registry, &self.root_dirs)
                .map_err(publish_error)?;
        }
        Ok((root, revision))
    }

    /// Request a manual scan over one root or every scheduled root.
    pub fn request_scan(
        &self,
        root_id: Option<&str>,
        user_id: &str,
    ) -> Result<ScanRequestResult, ServiceError> {
        let registry = self.live_registry();
        let mut scopes = registry.scheduled_root_scopes();
        if let Some(wanted) = root_id {
            scopes.retain(|scope| scope.root_id == wanted);
            if scopes.is_empty() {
                return Err(ServiceError::NotFound);
            }
        }
        let result = self
            .coordinator
            .request_run(&ScanRequest {
                kind: ScanKind::Incremental,
                trigger: ScanTrigger::Manual,
                scopes,
                requested_by_user_id: Some(user_id.to_owned()),
                policy_revision: registry.policy_revision().to_owned(),
            })
            .map_err(|error| match error {
                super::scan::coordinator::ScanRequestError::Disabled => ServiceError::Conflict {
                    message: error.to_string(),
                },
                super::scan::coordinator::ScanRequestError::EmptyScopes => {
                    ServiceError::InvalidInput {
                        message: error.to_string(),
                    }
                }
                super::scan::coordinator::ScanRequestError::StalePolicy
                | super::scan::coordinator::ScanRequestError::UnknownRoots => {
                    ServiceError::Conflict {
                        message: error.to_string(),
                    }
                }
                super::scan::coordinator::ScanRequestError::BadCursor => {
                    ServiceError::InvalidInput {
                        message: error.to_string(),
                    }
                }
                super::scan::coordinator::ScanRequestError::Store(cause) => {
                    ServiceError::internal(&cause)
                }
            })?;
        Ok(result)
    }

    /// Run detail plus discovered files (bounded to the latest 500).
    pub fn run_detail(
        &self,
        run_id: &str,
    ) -> Result<(ScanRun, Vec<ScanScope>, Vec<ScanInventoryItem>), ServiceError> {
        let (run, scopes, _) = self
            .coordinator
            .snapshot(run_id)
            .map_err(|error| match error {
                super::scan::store::ScanStoreError::NotFound { .. } => ServiceError::NotFound,
                other => ServiceError::Conflict {
                    message: other.to_string(),
                },
            })?;
        let mut files = self.scan_store.inventory_for_run(run_id);
        if files.len() > 500 {
            files = files.split_off(files.len() - 500);
        }
        // Inventory rows freeze at discovery; track ids assign at
        // index time into the catalog, so join them for the view.
        let mut catalog: HashMap<(String, String), String> = HashMap::new();
        for scope in &scopes {
            for (relative_path, entry) in self.scan_store.catalog_entries(&scope.root_id) {
                catalog.insert(
                    (scope.root_id.clone(), relative_path),
                    entry.track_id.clone(),
                );
            }
        }
        for file in &mut files {
            if file.local_track_id.is_none() {
                file.local_track_id = catalog
                    .get(&(file.root_id.clone(), file.relative_path.clone()))
                    .cloned();
            }
        }
        Ok((run, scopes, files))
    }

    /// Enqueue one album for identification, seeding title/artist
    /// facts when the scan never saw the album.
    pub fn enqueue_identify(
        &self,
        album_id: &str,
        kind: IdentifyKind,
        title: Option<&str>,
        artist: Option<&str>,
        user_id: &str,
    ) -> IdentifyJob {
        use super::identify::stores::IdentityStore;

        if self.identities.album_facts(album_id).is_none() {
            self.identities.save_album_facts(LocalAlbumFacts {
                local_album_id: album_id.to_owned(),
                title: title.unwrap_or_default().to_owned(),
                album_artist_name: artist.unwrap_or_default().to_owned(),
                tracks: Vec::new(),
                locked_track_ids: Vec::new(),
                is_compilation: false,
            });
        }
        let job_id = self.ids.new_id();
        self.identify
            .enqueue_album(&job_id, album_id, kind, &job_id, Some(user_id), now_ms())
    }

    /// Approve a pending review with the curator's chosen candidate,
    /// sealing a manual identity. Unknown reviews and unknown
    /// candidates are 404s; settled reviews are 409s.
    pub fn approve_review(
        &self,
        review_id: &str,
        user_id: &str,
        candidate_key: &str,
    ) -> Result<(super::identify::models::ReviewItem, Option<AlbumIdentity>), ServiceError> {
        use super::identify::stores::{IdentityStore, ReviewStore};

        let Some(review) = self.reviews.get(review_id) else {
            return Err(ServiceError::NotFound);
        };
        if review.state != super::identify::models::ReviewState::Pending {
            return Err(ServiceError::Conflict {
                message: "Review is already settled".to_owned(),
            });
        }
        if !review
            .candidates
            .iter()
            .any(|candidate| candidate.candidate_key == candidate_key)
        {
            return Err(ServiceError::NotFound);
        }
        if !self
            .identify
            .approve_candidate(review_id, user_id, candidate_key)
        {
            return Err(ServiceError::Conflict {
                message: "Review could not be approved".to_owned(),
            });
        }
        let settled = self.reviews.get(review_id).unwrap_or(review);
        let identity = self.identities.album_identity(&settled.local_album_id);
        Ok((settled, identity))
    }

    /// Reject a pending review: the album keeps its tags, nothing seals.
    pub fn reject_review(
        &self,
        review_id: &str,
        user_id: &str,
    ) -> Result<super::identify::models::ReviewItem, ServiceError> {
        use super::identify::stores::ReviewStore;

        let Some(review) = self.reviews.get(review_id) else {
            return Err(ServiceError::NotFound);
        };
        if review.state != super::identify::models::ReviewState::Pending {
            return Err(ServiceError::Conflict {
                message: "Review is already settled".to_owned(),
            });
        }
        if !self.identify.reject_candidates(review_id, user_id) {
            return Err(ServiceError::Conflict {
                message: "Review could not be rejected".to_owned(),
            });
        }
        Ok(self.reviews.get(review_id).unwrap_or(review))
    }
}
