//! Where the import folder is, and whether it may be used now.
//!
//! The setting is only an absolute path. Each use resolves it to its real
//! path and checks it there, the way the recycle bin is checked on use: a
//! library or download folder can be added inside it at any time. A
//! folder that is missing, not writable (removing what went in is part of
//! the job), or is, holds or sits inside a library folder, the recycle bin,
//! a download folder or one of the app's own folders, is not used, and
//! nothing in it is read or removed. Blocking: callers run it off the
//! async runtime.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use droppedneedle_kernel::runtime_config::Live;
use droppedneedle_kernel::runtime_config::sections::{ImportFolder, LibraryManagement};
use droppedneedle_library::publish::bin::DownloadTrees;
use serde::Serialize;
use utoipa::ToSchema;

use crate::landing::canonical_dirs;
use crate::landing::ports::LandingLibrary;

/// Which kind of folder the import folder overlaps.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ImportFolderOverlap {
    /// A library folder.
    Library,
    /// The recycle bin.
    RecycleBin,
    /// A download client's folder, or the download staging folder.
    Downloads,
    /// One of the app's own folders (data, cache, config).
    App,
}

/// Why the import folder is not used now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Unusable {
    /// No folder is set.
    Off,
    /// A setting it is checked against does not read.
    SettingsUnreadable,
    /// The folder is not there, is not a folder, or does not resolve.
    Unreachable,
    /// The server cannot write in it.
    ReadOnly,
    /// It is, holds or sits inside another folder: which kind, and that
    /// folder's real path.
    Overlaps(ImportFolderOverlap, PathBuf),
}

/// The import folder's setting and every folder it must stay clear of.
pub struct ImportSite {
    section: Live<ImportFolder>,
    library: Arc<dyn LandingLibrary>,
    downloads: DownloadTrees,
    management: Live<LibraryManagement>,
    app: Vec<PathBuf>,
}

impl ImportSite {
    /// The site over the setting, the library folders `library` lists, the
    /// download folders `downloads` answers, the recycle bin in
    /// `management`, and the app's own folders `app`.
    pub fn new(
        section: Live<ImportFolder>,
        library: Arc<dyn LandingLibrary>,
        downloads: DownloadTrees,
        management: Live<LibraryManagement>,
        app: Vec<PathBuf>,
    ) -> Self {
        Self {
            section,
            library,
            downloads,
            management,
            app,
        }
    }

    /// The import folder's real path, when it may be used now.
    pub fn check(&self) -> Result<PathBuf, Unusable> {
        let section = self.section.read().ok_or(Unusable::SettingsUnreadable)?;
        let configured = section.path.trim();
        if configured.is_empty() {
            return Err(Unusable::Off);
        }
        let real = std::fs::canonicalize(configured).map_err(|_| Unusable::Unreachable)?;
        if !real.is_dir() {
            return Err(Unusable::Unreachable);
        }
        if !crate::library_ready::writable(&real) {
            return Err(Unusable::ReadOnly);
        }
        let management = self.management.read().ok_or(Unusable::SettingsUnreadable)?;
        let downloads = (self.downloads)().ok_or(Unusable::SettingsUnreadable)?;
        let bin: Vec<PathBuf> = Some(management.recycle_bin_path.trim())
            .filter(|path| !path.is_empty())
            .map(PathBuf::from)
            .into_iter()
            .collect();
        let others = [
            (ImportFolderOverlap::Library, self.library.library_dirs()),
            (ImportFolderOverlap::RecycleBin, bin),
            (ImportFolderOverlap::Downloads, downloads),
            (ImportFolderOverlap::App, self.app.clone()),
        ];
        for (kind, dirs) in others {
            if let Some(other) = canonical_dirs(&dirs)
                .into_iter()
                .find(|other| overlaps(&real, other))
            {
                return Err(Unusable::Overlaps(kind, other));
            }
        }
        Ok(real)
    }

    /// The import folder's real path for files a job read from `job_dir`:
    /// only while that is still the import folder and it may be used.
    pub fn folder_of(&self, job_dir: &str) -> Result<PathBuf, Unusable> {
        let real = self.check()?;
        if real.as_os_str() == job_dir {
            Ok(real)
        } else {
            Err(Unusable::Off)
        }
    }
}

/// Whether one folder is, holds or sits inside the other, by whole path
/// components.
fn overlaps(left: &Path, right: &Path) -> bool {
    !right.as_os_str().is_empty() && (left.starts_with(right) || right.starts_with(left))
}
