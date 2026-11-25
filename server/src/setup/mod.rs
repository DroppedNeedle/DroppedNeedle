//! The setup guide's look: which of the services a home server usually
//! runs answer on this network, and which mounted folders could be the
//! library, the import folder or a download folder.
//!
//! It only suggests. It asks the fixed names and ports in
//! [`SetupCandidates`] through the factory's `lan_probe` client, one keyless
//! GET each, all at once, and reads folder listings through acquire's
//! folder look. Nothing from an answer becomes an address or a setting:
//! the web UI fills in the settings forms with what is here, and only each
//! form's Save writes. Nothing is kept between looks; one runs at a time.

pub mod handlers;
pub mod models;
mod probes;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use droppedneedle_acquire::imports::mount::MountRow;
use droppedneedle_acquire::setup_folders::SetupFolders;
use droppedneedle_kernel::db::Db;
use droppedneedle_kernel::mount::{Routes, routes};
use droppedneedle_kernel::runtime_config::Live;
use droppedneedle_kernel::runtime_config::secret_sections::TypedLibrary;
use droppedneedle_library::policy::LibraryPolicyCatalog as _;

use crate::settings::library_catalog::SqliteLibraryPolicyCatalog;
pub use models::{
    FoundService, ProbeOutcome, ServiceNote, SetupCandidates, SetupFound, SetupServiceKind,
};

/// The setup guide's look, built once at boot.
pub struct SetupFinder {
    client: reqwest::Client,
    candidates: SetupCandidates,
    mounts: Vec<MountRow>,
    folders: Arc<SetupFolders>,
    catalog: SqliteLibraryPolicyCatalog,
    library: Live<TypedLibrary>,
    one_at_a_time: tokio::sync::Mutex<()>,
}

impl SetupFinder {
    /// The look over the factory's `lan_probe` client, the candidates and
    /// mount table `Outbound` gives, acquire's folder look, and the
    /// catalog and library settings the set library folders' counts come
    /// from.
    pub fn new(
        client: reqwest::Client,
        candidates: SetupCandidates,
        mounts: Vec<MountRow>,
        folders: Arc<SetupFolders>,
        db: Db,
        library: Live<TypedLibrary>,
    ) -> Self {
        Self {
            client,
            candidates,
            mounts,
            folders,
            catalog: SqliteLibraryPolicyCatalog::new(db),
            library,
            one_at_a_time: tokio::sync::Mutex::new(()),
        }
    }

    /// Look now. A second call waits for the look running, which answers
    /// within the folder budget and a second.
    pub async fn found(&self) -> SetupFound {
        let _turn = self.one_at_a_time.lock().await;
        let services = futures_util::future::join_all(
            self.candidates
                .list()
                .into_iter()
                .map(|(kind, base)| probes::probe(&self.client, kind, base)),
        );
        let folders = async {
            let counts = self.library_counts().await;
            self.folders.find(self.mounts.clone(), counts).await
        };
        let (services, look) = tokio::join!(services, folders);
        SetupFound {
            services,
            folders: look.folders,
            folder_look: look.state,
        }
    }

    /// The catalog's track count for each library folder set now.
    async fn library_counts(&self) -> HashMap<PathBuf, u64> {
        let Some(library) = self.library.read() else {
            return HashMap::new();
        };
        let roots = match self.catalog.catalog_roots().await {
            Ok(roots) => roots,
            Err(error) => {
                tracing::warn!(%error, "setup: the library folders' counts do not read");
                return HashMap::new();
            }
        };
        library
            .library_roots
            .iter()
            .filter_map(|root| {
                let count = roots.iter().find(|row| row.root_id == root.id)?;
                let tracks = u64::try_from(count.indexed_track_count).unwrap_or(0);
                Some((PathBuf::from(root.path.trim()), tracks))
            })
            .collect()
    }
}

/// The setup guide's route.
pub fn routes() -> Routes<Arc<SetupFinder>> {
    Routes::new().admin(routes!(handlers::get_setup_found))
}
