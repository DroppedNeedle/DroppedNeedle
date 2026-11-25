//! The free-space watch's folders: every folder the app writes, read again
//! on each pass from the settings that name them.

use std::path::PathBuf;
use std::sync::Arc;

use droppedneedle_kernel::disk::{DiskWatch, FolderList, FolderRole, Watched, read_space};
use droppedneedle_kernel::platform::Platform;
use droppedneedle_kernel::runtime_config::Live;
use droppedneedle_kernel::runtime_config::secret_sections::TypedLibrary;
use droppedneedle_kernel::runtime_config::sections::{ImportFolder, LibraryManagement};

/// The one watch over the platform's folders and the folders its settings
/// name. A section that cannot be read leaves its folders out of that pass.
pub fn disk_watch(platform: &Platform) -> DiskWatch {
    let config = &platform.config;
    let fixed = [
        (FolderRole::Config, config.config_dir.clone()),
        (FolderRole::Temp, config.temp_dir.clone()),
        (FolderRole::Imports, config.imports_dir()),
    ];
    let library: Live<TypedLibrary> = platform.settings.live();
    let management: Live<LibraryManagement> = platform.settings.live();
    let import_folder: Live<ImportFolder> = platform.settings.live();
    let downloads = droppedneedle_acquire::wiring::recycle_guard(
        &platform.settings,
        config.imports_dir(),
        config.slskd_downloads_path.clone(),
        config.plugins_dir(),
    );
    let folders: FolderList = Arc::new(move || {
        let mut folders: Vec<Watched> = fixed
            .iter()
            .map(|(role, path)| Watched {
                role: *role,
                path: path.clone(),
            })
            .collect();
        let mut add = |role: FolderRole, path: PathBuf| {
            let folder = Watched { role, path };
            if !folder.path.as_os_str().is_empty()
                && !folders.iter().any(|seen| seen.path == folder.path)
            {
                folders.push(folder);
            }
        };
        if let Some(library) = library.read() {
            for root in &library.library_roots {
                add(FolderRole::Library, PathBuf::from(root.path.trim()));
            }
        }
        if let Some(management) = management.read() {
            add(
                FolderRole::RecycleBin,
                PathBuf::from(management.recycle_bin_path.trim()),
            );
        }
        if let Some(import) = import_folder.read() {
            add(FolderRole::ImportFolder, PathBuf::from(import.path.trim()));
        }
        for path in downloads().unwrap_or_default() {
            add(FolderRole::Downloads, path);
        }
        folders
    });
    DiskWatch::new(
        Arc::new(read_space),
        folders,
        config.library_db_path.clone(),
        config.undo_dir(),
        platform.clock.clone(),
    )
}
