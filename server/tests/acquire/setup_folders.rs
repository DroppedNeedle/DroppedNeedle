//! The setup guide's folder look: from a mount table, it suggests the
//! library, the import folder and the download folders the app would
//! accept, counts without following links, and says when it stopped early.

use std::collections::HashMap;
use std::path::Path;

use droppedneedle_acquire::import_folder::site::Neighbours;
use droppedneedle_acquire::imports::mount::MountRow;
use droppedneedle_acquire::setup_folders::{
    FolderInputs, FoundFolder, SetupFolderRole, WalkBudget, find_folders,
};

use crate::common::ScratchDir;

fn files(folder: &Path, count: usize) {
    std::fs::create_dir_all(folder).unwrap();
    for at in 0..count {
        std::fs::write(folder.join(format!("{at:03}.flac")), b"").unwrap();
    }
}

fn mount(id: i64, point: &Path) -> MountRow {
    MountRow {
        id,
        point: point.to_path_buf(),
        fs_type: "ext4".to_owned(),
    }
}

fn inputs(mounts: Vec<MountRow>, neighbours: Neighbours, budget: WalkBudget) -> FolderInputs {
    FolderInputs {
        mounts,
        left_out: neighbours.app.clone(),
        neighbours,
        import_folder: None,
        library_counts: HashMap::new(),
        scanning: false,
        budget,
    }
}

fn at<'a>(found: &'a [FoundFolder], path: &Path) -> Option<&'a FoundFolder> {
    found.iter().find(|folder| Path::new(&folder.path) == path)
}

#[test]
fn the_look_suggests_the_library_import_and_download_folders() {
    let root = ScratchDir::new("setup-folders");
    let root = std::fs::canonicalize(&*root).unwrap();
    let data = root.join("data");
    let music = data.join("music");
    let album = music.join("Artist/Album");
    files(&album, 120);
    let elsewhere = root.join("elsewhere");
    files(&elsewhere, 40);
    std::os::unix::fs::symlink(&elsewhere, music.join("Artist/Linked")).unwrap();
    std::os::unix::fs::symlink(elsewhere.join("000.flac"), album.join("linked.flac")).unwrap();
    let complete = data.join("slskd/complete");
    std::fs::create_dir_all(&complete).unwrap();
    let cache = data.join("cache");
    files(&cache.join("Artist/Album"), 150);
    let import = root.join("import");
    files(&import, 2);

    let neighbours = Neighbours {
        downloads: vec![complete.clone()],
        app: vec![cache.clone()],
        ..Neighbours::default()
    };
    let found = find_folders(&inputs(
        vec![mount(1, &data), mount(2, &import)],
        neighbours,
        WalkBudget::SETUP,
    ));

    let library = at(&found, &music).unwrap_or_else(|| panic!("no library in {found:?}"));
    assert_eq!(library.role, SetupFolderRole::Library);
    assert_eq!(library.audio_files, 120, "links are not followed");
    assert!(library.counted_all && !library.already_set);
    let suggested = at(&found, &import).unwrap_or_else(|| panic!("no import in {found:?}"));
    assert_eq!(suggested.role, SetupFolderRole::ImportFolder);
    let downloads = at(&found, &complete).unwrap_or_else(|| panic!("no downloads in {found:?}"));
    assert_eq!(downloads.role, SetupFolderRole::Downloads);
    assert!(downloads.already_set);
    for never in [&data, &cache, &data.join("slskd")] {
        assert!(at(&found, never).is_none(), "{never:?} in {found:?}");
    }
}

#[test]
fn a_folder_over_its_budget_is_counted_as_at_least() {
    let root = ScratchDir::new("setup-folders-budget");
    let root = std::fs::canonicalize(&*root).unwrap();
    let big = root.join("big");
    files(&big.join("Artist/Album"), 150);
    let budget = WalkBudget {
        entries: 110,
        ..WalkBudget::SETUP
    };
    let found = find_folders(&inputs(vec![mount(1, &big)], Neighbours::default(), budget));

    let library = at(&found, &big).unwrap_or_else(|| panic!("no library in {found:?}"));
    assert_eq!(library.role, SetupFolderRole::Library);
    assert!(!library.counted_all);
    assert!(library.audio_files >= 100 && library.audio_files < 150);
}
