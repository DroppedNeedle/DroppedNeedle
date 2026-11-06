//! The import folder: one folder on the server, set by an admin, that
//! DroppedNeedle imports music from by itself and empties as it goes.
//!
//! A supervised loop looks at it every minute. An album folder is taken
//! once nothing in it has changed for three minutes ([`walk`]; a name
//! starting with `.` is never taken), and each album runs through drop
//! import's steps ([`crate::drop_import`]), one at a time and never while a
//! library scan runs. Its covers, cue sheets and logs go with it. What went
//! in leaves the folder only once the library holds its exact bytes:
//! hashed beneath the folder and unchanged up to the unlink. What could not
//! go in stays where it is, as an entry of the held list that belongs to no
//! user, only admins see, and never expires.
//!
//! The albums waiting are drop import's own item rows, which this loop is
//! the one reader of: a wait on MusicBrainz, AcoustID, the library or a
//! scan leaves them queued and spends no try. Before each album the folder
//! is checked again ([`site`]); a folder that changed, cannot be reached,
//! cannot be written or overlaps another folder takes nothing, and work on
//! an earlier folder stops without touching its files.

pub mod http;
pub mod site;
pub mod walk;

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use droppedneedle_kernel::jobs::{
    AlreadyRunning, FnTick, Next, Schedule, Supervisor, TickCtx, TickError,
};
use serde::Serialize;
use utoipa::ToSchema;

use self::site::{ImportFolderOverlap, ImportSite, Unusable};
use crate::drop_import::{DropImports, FolderRun};
use crate::landing::confine;
use crate::landing::ports::LandingLibrary;

/// Job name of the import folder's loop.
pub const IMPORT_FOLDER_JOB: &str = "acquire-import-folder";

/// How long nothing in an album folder must change before it is taken.
const QUIET_MS: i64 = 3 * 60_000;

/// The loop looks at the folder every minute, with a little jitter, and
/// backs off from a minute up to half an hour while something every album
/// needs is not there.
const INTERVAL: Duration = Duration::from_secs(60);
const JITTER: Duration = Duration::from_secs(10);
const BACKOFF_CAP: Duration = Duration::from_secs(30 * 60);

/// What the import folder is doing, or why it waits.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ImportFolderState {
    /// No folder is set.
    Off,
    /// The folder is watched; nothing is waiting to go in.
    Watching,
    /// An album is going in.
    Importing,
    /// A library scan is running; albums wait for it to end.
    WaitingForScan,
    /// Something every album needs (MusicBrainz, AcoustID, the library
    /// folder) is not there now; it tries again by itself.
    Retrying,
    /// The library is switched off, or has no folder.
    LibraryOff,
    /// A setting the folder is checked against does not read.
    SettingsUnreadable,
    /// The folder is not there.
    Unreachable,
    /// The server cannot write in the folder.
    ReadOnly,
    /// The folder is, holds or sits inside another folder.
    Overlaps,
}

/// What the folder's status says now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Status {
    pub state: ImportFolderState,
    /// The kind of folder it overlaps, and that folder's path.
    pub overlap: Option<(ImportFolderOverlap, PathBuf)>,
    /// Albums still to run.
    pub waiting: i64,
}

/// The import folder's loop and status.
pub struct ImportFolders {
    site: Arc<ImportSite>,
    drops: Arc<DropImports>,
    library: Arc<dyn LandingLibrary>,
    /// Per album folder: what its files looked like, and since when.
    quiet: Mutex<HashMap<String, (u64, i64)>>,
    /// The last pass's folder and what it did there.
    activity: Mutex<Option<(PathBuf, ImportFolderState)>>,
    /// The folder work was last let go for: other folders' work is let go
    /// once per change.
    left_for: Mutex<Option<Option<String>>>,
}

impl ImportFolders {
    /// The loop over the folder `site` checks, running albums through
    /// `drops` into `library`.
    pub fn new(
        site: Arc<ImportSite>,
        drops: Arc<DropImports>,
        library: Arc<dyn LandingLibrary>,
    ) -> Self {
        Self {
            site,
            drops,
            library,
            quiet: Mutex::new(HashMap::new()),
            activity: Mutex::new(None),
            left_for: Mutex::new(None),
        }
    }

    /// One pass: check the folder, remove what went in, queue the albums
    /// done arriving, then run the queue one album at a time until it is
    /// empty, a scan starts or the folder changes. An album that waits on
    /// something of its own (an earlier publish still being recovered) is
    /// passed over; a wait on something every album needs (MusicBrainz,
    /// AcoustID, the library) ends the pass as an error, so the loop backs
    /// off.
    pub async fn pass(&self) -> Result<(), TickError> {
        let folder = match self.check().await {
            Ok(folder) => folder,
            Err(unusable) => {
                self.note(None);
                if unusable == Unusable::Off {
                    self.leave(None).await;
                }
                return Ok(());
            }
        };
        let text = folder.to_string_lossy().into_owned();
        self.leave(Some(&text)).await;
        let library = self.library.clone();
        let off = tokio::task::spawn_blocking(move || {
            library.library_off() || library.library_dirs().is_empty()
        })
        .await
        .unwrap_or(true);
        if off {
            self.note(Some((folder, ImportFolderState::LibraryOff)));
            return Ok(());
        }
        self.remove_imported(&folder, &text).await;
        self.discover(&folder, &text).await;
        let mut passed_over: HashSet<i64> = HashSet::new();
        loop {
            let library = self.library.clone();
            if tokio::task::spawn_blocking(move || library.scanning())
                .await
                .unwrap_or(true)
            {
                self.note(Some((folder, ImportFolderState::WaitingForScan)));
                return Ok(());
            }
            if self.check().await.ok().as_ref() != Some(&folder) {
                return Ok(());
            }
            let next = match self.drops.store().folder_items(&text).await {
                Ok(items) => items
                    .into_iter()
                    .find(|item| !passed_over.contains(&item.id)),
                Err(error) => {
                    tracing::warn!(%error, "import folder: the albums to run do not read");
                    return Err(TickError::new("IMPORT_FOLDER_UNREADABLE", error));
                }
            };
            let Some(item) = next else {
                break;
            };
            self.note(Some((folder.clone(), ImportFolderState::Importing)));
            let item_id = item.id;
            match self.drops.run_folder_item(&folder, item).await {
                FolderRun::Done => self.remove_imported(&folder, &text).await,
                FolderRun::PassOver => {
                    passed_over.insert(item_id);
                }
                FolderRun::Waits => {
                    self.note(Some((folder, ImportFolderState::Retrying)));
                    return Err(TickError::new(
                        "IMPORT_FOLDER_WAITS",
                        "an outside service or the library is not there now",
                    ));
                }
                FolderRun::Stopped => return Ok(()),
            }
        }
        if let Err(error) = self.drops.store().settle_folder_jobs().await {
            tracing::warn!(%error, "import folder: finished jobs not settled");
        }
        self.note(Some((folder, ImportFolderState::Watching)));
        Ok(())
    }

    /// What the folder's status says now: why it cannot be used, else what
    /// the last pass did there and how many albums wait.
    pub async fn status(&self) -> Status {
        let folder = match self.check().await {
            Ok(folder) => folder,
            Err(unusable) => {
                let (state, overlap) = match unusable {
                    Unusable::Off => (ImportFolderState::Off, None),
                    Unusable::SettingsUnreadable => (ImportFolderState::SettingsUnreadable, None),
                    Unusable::Unreachable => (ImportFolderState::Unreachable, None),
                    Unusable::ReadOnly => (ImportFolderState::ReadOnly, None),
                    Unusable::Overlaps(kind, other) => {
                        (ImportFolderState::Overlaps, Some((kind, other)))
                    }
                };
                return Status {
                    state,
                    overlap,
                    waiting: 0,
                };
            }
        };
        let state = self
            .activity
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_ref()
            .filter(|(at, _)| *at == folder)
            .map_or(ImportFolderState::Watching, |(_, state)| *state);
        let text = folder.to_string_lossy().into_owned();
        let waiting = match self.drops.store().folder_waiting(&text).await {
            Ok(waiting) => waiting,
            Err(error) => {
                tracing::warn!(%error, "import folder: the albums waiting do not read");
                0
            }
        };
        Status {
            state,
            overlap: None,
            waiting,
        }
    }

    async fn check(&self) -> Result<PathBuf, Unusable> {
        let site = self.site.clone();
        match tokio::task::spawn_blocking(move || site.check()).await {
            Ok(checked) => checked,
            Err(error) => {
                tracing::error!(%error, "import folder check did not run");
                Err(Unusable::Unreachable)
            }
        }
    }

    fn note(&self, activity: Option<(PathBuf, ImportFolderState)>) {
        *self
            .activity
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = activity;
    }

    /// Let go of work on any folder but `folder`, once per change of
    /// folder. Its files are never touched.
    async fn leave(&self, folder: Option<&str>) {
        let wanted = folder.map(str::to_owned);
        if self
            .left_for
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_ref()
            == Some(&wanted)
        {
            return;
        }
        match self.drops.store().leave_other_folders(folder).await {
            Ok(left) => {
                if left > 0 {
                    tracing::info!(left, "import folder: work on an earlier folder let go");
                }
                self.quiet
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .clear();
                *self
                    .left_for
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(wanted);
            }
            Err(error) => tracing::warn!(%error, "import folder: earlier work not let go"),
        }
    }

    /// Queue every album folder that is done arriving: one whose files
    /// have looked the same for [`QUIET_MS`].
    async fn discover(&self, folder: &Path, text: &str) {
        let root = folder.to_path_buf();
        let units = match tokio::task::spawn_blocking(move || walk::units(&root)).await {
            Ok(Ok(units)) => units,
            Ok(Err(error)) => {
                tracing::warn!(%error, "import folder does not read");
                return;
            }
            Err(error) => {
                tracing::error!(%error, "import folder walk did not run");
                return;
            }
        };
        let now = self.drops.store().db().clock().now_ms();
        let mut listed: HashMap<String, (i64, i64)> = HashMap::new();
        let ready: Vec<(String, Vec<String>)> = {
            let mut quiet = self
                .quiet
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let seen: HashSet<&str> = units.iter().map(|unit| unit.folder.as_str()).collect();
            quiet.retain(|folder, _| seen.contains(folder.as_str()));
            units
                .into_iter()
                .filter(|unit| {
                    let since = quiet
                        .entry(unit.folder.clone())
                        .and_modify(|(digest, since)| {
                            if *digest != unit.digest {
                                *digest = unit.digest;
                                *since = now;
                            }
                        })
                        .or_insert((unit.digest, now))
                        .1;
                    now - since >= QUIET_MS
                })
                .map(|unit| {
                    listed.extend(unit.listed);
                    (unit.folder, unit.audio)
                })
                .collect()
        };
        if ready.is_empty() {
            return;
        }
        let job = uuid::Uuid::new_v4().simple().to_string();
        match self
            .drops
            .store()
            .queue_folder(&job, text, ready, listed)
            .await
        {
            Ok(0) => {}
            Ok(albums) => tracing::info!(albums, "import folder: albums queued"),
            Err(error) => tracing::warn!(%error, "import folder: albums not queued"),
        }
    }

    /// Remove the files settled albums put in the library, each only while
    /// it still has the exact bytes the library took. Albums whose publish
    /// waits for recovery wait too.
    async fn remove_imported(&self, folder: &Path, text: &str) {
        let items = match self.drops.store().folder_removals(text).await {
            Ok(items) => items,
            Err(error) => {
                tracing::warn!(%error, "import folder: files to remove do not read");
                return;
            }
        };
        for item in items {
            let placed = match self
                .library
                .placed_sources(&format!("drop-{}", item.id))
                .await
            {
                Ok(Some(placed)) => placed,
                Ok(None) => continue,
                Err(error) => {
                    tracing::warn!(item_id = item.id, %error, "import folder: the publish journal does not read");
                    continue;
                }
            };
            let (root, rels) = (folder.to_path_buf(), item.staging_paths.clone());
            if let Err(error) =
                tokio::task::spawn_blocking(move || remove_imported_files(&root, &rels, &placed))
                    .await
            {
                tracing::error!(item_id = item.id, %error, "import folder removal did not run");
                continue;
            }
            if let Err(error) = self.drops.store().clear_removals(item.id).await {
                tracing::warn!(item_id = item.id, %error, "import folder: removed files not recorded");
            }
        }
    }
}

/// Remove each file at `rels` below `folder` that has bytes the library
/// holds (`placed`, their hashes), beneath the folder and only while it is
/// the file that was hashed, then the folders that leaves empty. A file
/// with other bytes, or one that does not read, stays, logged. Blocking.
pub(crate) fn remove_imported_files(folder: &Path, rels: &[String], placed: &HashSet<String>) {
    let folders = vec![folder.to_path_buf()];
    for rel in rels {
        let Some(path) = crate::held::below(folder, rel) else {
            tracing::warn!(rel, "import folder: a path that climbs out is left alone");
            continue;
        };
        match confine::hash_beneath(&folders, &path) {
            Ok(hashed) if placed.contains(&hashed.sha256) => {
                match confine::remove_beneath_if_unchanged(&folders, &path, &hashed) {
                    Ok(true) => {}
                    Ok(false) => {
                        tracing::info!(
                            rel,
                            "import folder: a file changed after it went in; it stays"
                        );
                    }
                    Err(error) => {
                        tracing::warn!(rel, %error, "import folder: a file that went in was not removed");
                    }
                }
            }
            Ok(_) => tracing::debug!(
                rel,
                "import folder: the library holds other bytes; the file stays"
            ),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                tracing::warn!(rel, %error, "import folder: a file that went in does not read; it stays");
            }
        }
        remove_empty_folders(folder, &path);
    }
}

/// Remove the folders above `path` that are empty now, deepest first, up
/// to and never including `folder`, beneath it and never through a link.
/// Blocking.
pub(crate) fn remove_empty_folders(folder: &Path, path: &Path) {
    let folders = vec![folder.to_path_buf()];
    for dir in path.ancestors().skip(1) {
        if dir == folder || !dir.starts_with(folder) {
            break;
        }
        match confine::remove_dir_beneath(&folders, dir) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => break,
        }
    }
}

/// The import folder's loop: a minute after start, then every minute.
pub(crate) fn spawn_import_folder(
    supervisor: &Supervisor,
    folders: Arc<ImportFolders>,
) -> Result<(), AlreadyRunning> {
    supervisor.periodic(
        IMPORT_FOLDER_JOB,
        Schedule::new(INTERVAL)
            .with_jitter(JITTER)
            .with_initial_delay(INTERVAL)
            .with_backoff(INTERVAL, BACKOFF_CAP),
        FnTick(move |ctx: TickCtx| {
            let folders = folders.clone();
            async move {
                tokio::select! {
                    () = ctx.token().cancelled() => Ok(Next::Continue),
                    passed = folders.pass() => passed.map(|()| Next::Continue),
                }
            }
        }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A file leaves only while it has bytes the library holds and is the
    /// file that was hashed: one whose bytes changed stays, and a link
    /// whose target has those bytes is never followed or removed.
    #[test]
    fn a_file_leaves_only_when_the_library_has_its_bytes() {
        let scratch =
            droppedneedle_kernel::test_support::ScratchDir::new("import-removal").expect("scratch");
        let base = scratch.path().canonicalize().expect("scratch path");
        let folder = base.join("import");
        let outside = base.join("outside");
        std::fs::create_dir_all(folder.join("Album/CD1")).expect("album");
        std::fs::create_dir_all(&outside).expect("outside");
        let hash = |bytes: &[u8]| {
            droppedneedle_kernel::db::backup::read_sha256(bytes)
                .expect("hash")
                .0
        };
        std::fs::write(folder.join("Album/CD1/01.flac"), b"one").expect("one");
        std::fs::write(folder.join("Album/02.flac"), b"two, changed later").expect("two");
        std::fs::write(outside.join("03.flac"), b"three").expect("three");
        std::os::unix::fs::symlink(outside.join("03.flac"), folder.join("Album/03.flac"))
            .expect("link");
        let placed: HashSet<String> = [hash(b"one"), hash(b"two"), hash(b"three")].into();

        remove_imported_files(
            &folder,
            &[
                "Album/CD1/01.flac".to_owned(),
                "Album/02.flac".to_owned(),
                "Album/03.flac".to_owned(),
                "../outside/03.flac".to_owned(),
            ],
            &placed,
        );

        assert!(
            !folder.join("Album/CD1/01.flac").exists(),
            "its bytes are in the library"
        );
        assert!(
            !folder.join("Album/CD1").exists(),
            "the folder it left empty goes"
        );
        assert!(folder.join("Album/02.flac").is_file(), "other bytes stay");
        assert!(
            std::fs::symlink_metadata(folder.join("Album/03.flac")).is_ok(),
            "a link is left as it is"
        );
        assert!(outside.join("03.flac").is_file(), "and so is its target");
        assert!(folder.is_dir());
    }
}
