//! The import folder: an album copied in goes into the library with its
//! cover and leaves the folder; one it cannot place stays there, listed for
//! admins only and never pruned; a folder still changing waits; a link is
//! never followed and a folder overlapping the library imports nothing.
//! Real landing port, publisher and catalog over one scratch database on a
//! manual clock; MusicBrainz is a scripted release source.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum::extract::{Path as AxumPath, State};
use axum::http::{HeaderMap, StatusCode};
use droppedneedle_acquire::db::AcquireDb;
use droppedneedle_acquire::downloads::queue_rows::Viewer;
use droppedneedle_acquire::held::http::audio_handler;
use droppedneedle_acquire::held::store::{EntryOrigin, HeldRoot};
use droppedneedle_acquire::import_folder::ImportFolderState;
use droppedneedle_acquire::import_folder::site::ImportFolderOverlap;
use droppedneedle_acquire::landing::library::LibraryLanding;
use droppedneedle_acquire::landing::ports::{
    ImportFile, ImportOrigin, ImportRequest, LandingLibrary as _,
};
use droppedneedle_auth::session::store::SessionKind;
use droppedneedle_auth::{Role, User};
use droppedneedle_kernel::runtime_config::sections::ImportFolder;
use droppedneedle_kernel::runtime_config::{ConfigStore, Crypto};
use droppedneedle_kernel::test_support::ManualClock;
use droppedneedle_library::identify::sources::{ReleaseHit, ReleaseSource, SourceError};
use droppedneedle_library::import::SidecarSource;
use droppedneedle_library::matching::{CreditedArtist, Release, ReleaseMedium, ReleaseTrack};
use droppedneedle_library::scan::EffectivePolicy;
use droppedneedle_library::tags::TagField;
use droppedneedle_library::tags::save::{TagEdit, save_tags};
use futures_util::future::BoxFuture;

use crate::common::ScratchDir;
use crate::common::auth::{AuthRig, test_principal};
use crate::common::library::{every_priority, library_at};
use crate::common::probe::Probe as _;

const GROUP: &str = "7d0b7a1e-0000-4000-8000-000000000011";
const RELEASE: &str = "7d0b7a1e-0000-4000-8000-000000000012";
/// Three minutes and a second: longer than the quiet window.
const QUIET: i64 = 181_000;

/// MusicBrainz knowing one release; every search returns it.
struct OneRelease(Release);

impl ReleaseSource for OneRelease {
    fn search<'a>(
        &'a self,
        _title: &'a str,
        _artist: &'a str,
    ) -> BoxFuture<'a, Result<Vec<ReleaseHit>, SourceError>> {
        let hit = ReleaseHit {
            id: self.0.id.clone(),
            release_group_id: self.0.release_group_id.clone(),
            score: 100,
            track_count: Some(self.0.tracks.len() as u32),
            title: self.0.title.clone(),
        };
        Box::pin(async move { Ok(vec![hit]) })
    }

    fn release<'a>(&'a self, mbid: &'a str) -> BoxFuture<'a, Result<Option<Release>, SourceError>> {
        let found = self.0.answers_to(mbid).then(|| self.0.clone());
        Box::pin(async move { Ok(found) })
    }

    fn canonical_recording<'a>(
        &'a self,
        _mbid: &'a str,
    ) -> BoxFuture<'a, Result<Option<String>, SourceError>> {
        Box::pin(async { Ok(None) })
    }
}

fn release() -> Release {
    let titles = ["Mysterons", "Sour Times"];
    Release {
        id: RELEASE.to_owned(),
        release_group_id: GROUP.to_owned(),
        title: "Dummy".to_owned(),
        artists: vec![CreditedArtist {
            id: "8f6bd1e4-fbe1-4f50-aa9b-94c450ec0f11".to_owned(),
            name: "Portishead".to_owned(),
            sort_name: Some("Portishead".to_owned()),
            join: String::new(),
            disambiguation: None,
        }],
        status: Some("Official".to_owned()),
        media: vec![ReleaseMedium {
            position: 1,
            format: Some("CD".to_owned()),
            title: None,
            track_count: titles.len() as u32,
        }],
        tracks: titles
            .iter()
            .enumerate()
            .map(|(index, title)| ReleaseTrack {
                isrcs: Vec::new(),
                genres: Vec::new(),
                id: format!("7d0b7a1e-0000-4000-8000-0000000003{index:02}"),
                recording_id: format!("7d0b7a1e-0000-4000-8000-0000000004{index:02}"),
                title: (*title).to_owned(),
                artists: Vec::new(),
                disc: 1,
                position: index as u32 + 1,
                absolute_position: index as u32 + 1,
                length_ms: None,
            })
            .collect(),
        ..Release::default()
    }
}

/// A fixture copy at `path`, tagged as one track of `album`, without
/// MusicBrainz ids.
fn tagged(path: &Path, fixture: &str, album: &str, title: &str, track: u32) {
    std::fs::create_dir_all(path.parent().expect("folder")).expect("album folder");
    std::fs::copy(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/library")
            .join(fixture),
        path,
    )
    .expect("copy fixture");
    let mut edits = vec![
        TagEdit::new(TagField::Title, vec![title.to_owned()]),
        TagEdit::new(TagField::Artist, vec!["Portishead".to_owned()]),
        TagEdit::new(TagField::AlbumArtist, vec!["Portishead".to_owned()]),
        TagEdit::new(TagField::Album, vec![album.to_owned()]),
        TagEdit::new(TagField::TrackNumber, vec![track.to_string()]),
        TagEdit::new(TagField::DiscNumber, vec!["1".to_owned()]),
    ];
    for field in [
        TagField::MusicBrainzRecordingId,
        TagField::MusicBrainzReleaseTrackId,
        TagField::MusicBrainzReleaseId,
        TagField::MusicBrainzReleaseGroupId,
    ] {
        edits.push(TagEdit::new(field, Vec::new()));
    }
    save_tags(path, &edits).expect("tags save");
}

/// The acquire bundle over a scratch library with a `music` root, on a
/// manual clock, with an admin and a user, and an import folder of its own.
struct Rig {
    acquire: droppedneedle_acquire::AcquireSetup,
    port: Arc<LibraryLanding>,
    config: Arc<ConfigStore>,
    clock: Arc<ManualClock>,
    music: PathBuf,
    import: ScratchDir,
    /// The plugins folder the bundle reads.
    plugins: PathBuf,
    _db: AcquireDb,
}

impl Rig {
    async fn new() -> Self {
        let scratch = AcquireDb::scratch().expect("scratch db");
        let clock = Arc::new(ManualClock::at(1_800_000_000_000));
        let db = AcquireDb::new(scratch.db().clone(), clock.clone());
        db.add_user("u-admin", "Ada", "admin").await.expect("admin");
        db.add_user("u-user", "Una", "user").await.expect("user");
        let dir = scratch.path().parent().expect("scratch dir").to_path_buf();
        let config = Arc::new(
            ConfigStore::scratch(&dir, Crypto::from_key_bytes(&[7u8; 32]).expect("key"))
                .expect("config"),
        );
        let platform = crate::common::TestPlatform::on(db.db(), &dir)
            .clock(clock.clone())
            .settings(config.clone())
            .build()
            .await;
        let (library, _) = library_at(&platform);
        let music = dir.join("music");
        std::fs::create_dir_all(&music).expect("music root");
        library
            .scans
            .add_root(
                Some("music".to_owned()),
                music.to_string_lossy().into_owned(),
                EffectivePolicy::Automatic,
            )
            .expect("root adds");
        let auth = AuthRig::on(&platform);
        let port = Arc::new(LibraryLanding::new(
            &library,
            every_priority(Arc::new(OneRelease(release()))),
        ));
        let acquire = crate::common::bundles::acquire_landing_at(
            &auth.platform,
            auth.deps.clone(),
            &library,
            &crate::common::bundles::collections_at(&auth.platform).stores,
            port.clone(),
        );
        let import = ScratchDir::new("import-folder");
        let rig = Self {
            acquire,
            port,
            config,
            clock,
            music,
            import,
            plugins: auth.platform.config.plugins_dir(),
            _db: scratch,
        };
        rig.set_folder(&rig.import);
        rig
    }

    fn set_folder(&self, path: &Path) {
        self.config
            .save_blocking(ImportFolder {
                path: path.to_string_lossy().into_owned(),
            })
            .expect("import folder saves");
    }

    /// One pass of the loop. A pass that ends waiting on an outside
    /// service answers an error for the loop's backoff; what it did is
    /// what the tests look at.
    async fn pass(&self) {
        let _ = self.acquire.import_folder.pass().await;
    }

    /// One pass after the album folders have been still for long enough.
    async fn pass_after_quiet(&self) {
        self.pass().await;
        self.clock.advance(QUIET);
        self.pass().await;
    }

    /// The import folder's real path, as its jobs name it.
    fn folder(&self) -> PathBuf {
        self.import.canonicalize().expect("import folder")
    }

    /// An album of the import folder as a restart left it: queued, its
    /// files and cover already published under its staging name, nothing
    /// recorded. Answers its item.
    async fn cut_off(&self, album: &str, files: &[&str]) -> i64 {
        let (dir, album_name) = (
            self.folder().to_string_lossy().into_owned(),
            album.to_owned(),
        );
        let paths: Vec<String> = files.iter().map(|file| format!("{album}/{file}")).collect();
        let staged = serde_json::to_string(&paths).expect("paths");
        let item = self
            .acquire
            .db
            .write("test.cut_off", move |tx| {
                tx.execute(
                    "INSERT INTO drop_import_jobs (id, user_id, status, created_at, upload_name, \
                     staging_dir, source) VALUES ('job-cut', NULL, 'processing', 1, \
                     'Import folder', ?1, 'import_folder')",
                    [dir],
                )?;
                tx.execute(
                    "INSERT INTO drop_import_items (job_id, folder_name, status, files_total, \
                     staging_paths, updated_at) VALUES ('job-cut', ?1, 'processing', 2, ?2, 1)",
                    rusqlite::params![album_name, staged],
                )?;
                Ok(tx.last_insert_rowid())
            })
            .await
            .expect("seeded");
        let album_dir = self.folder().join(album);
        self.port
            .import(ImportRequest {
                task_id: format!("drop-{item}"),
                origin: ImportOrigin::Folder,
                staging: None,
                read_beneath: Some(vec![self.folder()]),
                release: release(),
                join: None,
                files: files
                    .iter()
                    .enumerate()
                    .map(|(track, file)| ImportFile {
                        path: album_dir.join(file),
                        track,
                        replaces: Vec::new(),
                    })
                    .collect(),
                extras: Vec::new(),
                actor: None,
                sidecars: vec![SidecarSource {
                    path: album_dir.join("cover.jpg"),
                    rel: "cover.jpg".to_owned(),
                }],
            })
            .await
            .expect("the publish committed");
        item
    }

    fn item_status(&self, item: i64) -> String {
        self.acquire.db.db().one(
            "SELECT status FROM drop_import_items WHERE id = ?1",
            rusqlite::params![item],
        )
    }

    fn drop_tracks(&self, imported: &str) -> i64 {
        self.acquire.db.db().one(
            "SELECT COUNT(*) FROM local_tracks WHERE availability = 'indexed' \
             AND ingest_source = ?1",
            rusqlite::params![imported],
        )
    }
}

fn admin() -> Viewer {
    Viewer {
        user_id: "u-admin".to_owned(),
        admin: true,
    }
}

fn user() -> Viewer {
    Viewer {
        user_id: "u-user".to_owned(),
        admin: false,
    }
}

/// The matched album goes in with its cover and leaves the folder; a file
/// the patterns do not name stays. The album nothing identifies stays where
/// it is, listed for the admin with no expiry and hidden from a user, and a
/// held prune 91 days later leaves it in place; Discard deletes it.
#[tokio::test]
async fn an_album_goes_in_and_leaves_the_folder() {
    let rig = Rig::new().await;
    let album = rig.import.join("Portishead - Dummy");
    tagged(
        &album.join("01 Mysterons.flac"),
        "flac_full_01.flac",
        "Dummy",
        "Mysterons",
        1,
    );
    tagged(
        &album.join("02 Sour Times.flac"),
        "flac_full_02.flac",
        "Dummy",
        "Sour Times",
        2,
    );
    std::fs::write(album.join("cover.jpg"), b"a front cover").expect("cover");
    std::fs::write(album.join("notes.txt"), b"ripped in 1998").expect("notes");
    let odd = rig.import.join("Odds/09 Glory Box.flac");
    tagged(&odd, "flac_full_01.flac", "Nothing Alike", "Glory Box", 9);

    rig.pass_after_quiet().await;

    let shelf = rig.music.join("Portishead/Dummy");
    assert!(shelf.join("0101 Mysterons.flac").is_file());
    assert!(shelf.join("0102 Sour Times.flac").is_file());
    assert_eq!(
        std::fs::read(shelf.join("cover.jpg")).expect("cover in the library"),
        b"a front cover"
    );
    assert_eq!(
        rig.drop_tracks("drop_folder"),
        2,
        "filed as from the import folder"
    );
    for gone in ["01 Mysterons.flac", "02 Sour Times.flac", "cover.jpg"] {
        assert!(!album.join(gone).exists(), "{gone} left the import folder");
    }
    assert!(
        album.join("notes.txt").is_file(),
        "a file no pattern names stays"
    );
    assert!(odd.is_file(), "an album it cannot place stays where it is");

    let listed = rig
        .acquire
        .held
        .list(&admin(), None, None, false)
        .await
        .expect("held list");
    let entry = listed
        .iter()
        .find(|entry| entry.origin == EntryOrigin::ImportFolder)
        .expect("the leftover is listed for the admin");
    assert_eq!(entry.user_id, None);
    assert_eq!(entry.expires_at, None, "a person's own copy never expires");
    assert_eq!(entry.files.len(), 1);
    assert_eq!(entry.files[0].root, HeldRoot::Import);
    assert_eq!(entry.files[0].path, "Odds/09 Glory Box.flac");
    let mine = rig
        .acquire
        .held
        .list(&user(), None, None, false)
        .await
        .expect("held list");
    assert!(mine.is_empty(), "a user sees none of it: {mine:?}");
    assert!(
        rig.acquire
            .held
            .discard(&user(), entry.id, None)
            .await
            .is_err(),
        "a user cannot act on it"
    );

    rig.clock.advance(91 * 86_400_000);
    rig.acquire.held.tend().await;
    assert!(odd.is_file(), "the prune never touches the import folder");
    assert!(
        rig.acquire.held.get(&admin(), entry.id).await.is_ok(),
        "still listed after 91 days"
    );

    rig.acquire
        .held
        .discard(&admin(), entry.id, None)
        .await
        .expect("discard");
    assert!(!odd.exists(), "discard deletes it from the import folder");
    assert!(
        !rig.import.join("Odds").exists(),
        "the folder it leaves empty goes"
    );
    assert!(rig.import.is_dir(), "the import folder itself stays");
}

/// An album folder whose files keep changing is not taken until they have
/// been still for three minutes.
#[tokio::test]
async fn a_folder_still_changing_waits() {
    let rig = Rig::new().await;
    let album = rig.import.join("Portishead - Dummy");
    tagged(
        &album.join("01 Mysterons.flac"),
        "flac_full_01.flac",
        "Dummy",
        "Mysterons",
        1,
    );
    rig.pass().await;
    rig.clock.advance(120_000);
    tagged(
        &album.join("02 Sour Times.flac"),
        "flac_full_02.flac",
        "Dummy",
        "Sour Times",
        2,
    );
    rig.pass().await;
    rig.clock.advance(120_000);
    rig.pass().await;
    assert_eq!(
        rig.drop_tracks("drop_folder"),
        0,
        "the folder changed two minutes ago"
    );
    assert!(album.join("01 Mysterons.flac").is_file());

    rig.clock.advance(70_000);
    rig.pass().await;
    assert_eq!(
        rig.drop_tracks("drop_folder"),
        2,
        "still for three minutes: taken whole"
    );
    assert!(!album.join("01 Mysterons.flac").exists());
}

/// A linked album folder is never read or removed, and its target stays
/// whole. A folder set inside a library folder imports nothing and its
/// status says why.
#[tokio::test]
async fn links_and_overlaps_are_refused() {
    let rig = Rig::new().await;
    let outside = ScratchDir::new("import-folder-outside");
    let target = outside.join("Portishead - Dummy/01 Mysterons.flac");
    tagged(&target, "flac_full_01.flac", "Dummy", "Mysterons", 1);
    std::os::unix::fs::symlink(
        outside.join("Portishead - Dummy"),
        rig.import.join("Linked"),
    )
    .expect("link");

    rig.pass_after_quiet().await;
    assert_eq!(
        rig.drop_tracks("drop_folder"),
        0,
        "a link is never followed"
    );
    assert!(target.is_file(), "its target stays");

    let inside = rig.music.join("incoming");
    tagged(
        &inside.join("Portishead - Dummy/01 Mysterons.flac"),
        "flac_full_01.flac",
        "Dummy",
        "Mysterons",
        1,
    );
    rig.set_folder(&inside);
    rig.pass_after_quiet().await;
    let status = rig.acquire.import_folder.status().await;
    assert_eq!(status.state, ImportFolderState::Overlaps);
    assert_eq!(
        status.overlap.map(|(kind, _)| kind),
        Some(ImportFolderOverlap::Library)
    );
    assert_eq!(rig.drop_tracks("drop_folder"), 0, "nothing is imported");
    assert!(
        inside
            .join("Portishead - Dummy/01 Mysterons.flac")
            .is_file()
    );
}

/// An enabled download plugin's folder inside the import folder imports
/// nothing: the loop would take the plugin's downloads as the person's
/// albums, and the plugin's landings could remove the person's files.
#[tokio::test]
async fn a_download_plugins_folder_inside_is_refused() {
    use droppedneedle_kernel::runtime_config::sections::PluginConfig;

    let rig = Rig::new().await;
    let plugin = rig.plugins.join("fetcher");
    std::fs::create_dir_all(&plugin).expect("plugin folder");
    std::fs::write(
        plugin.join("plugin.toml"),
        "[plugin]\n\
         name = \"fetcher\"\n\
         version = \"1.0.0\"\n\
         api_version = 2\n\
         entrypoint = \"plugin:Fetcher\"\n\
         capabilities = [\"download_client\"]\n\
         [[capability]]\n\
         id = \"download_client\"\n\
         source = \"fetcher\"\n\
         display_name = \"Fetcher\"\n\
         [[settings]]\n\
         key = \"downloads_dir\"\n\
         label = \"Downloads directory\"\n",
    )
    .expect("manifest");
    let downloads = rig.import.join("fetcher-downloads");
    std::fs::create_dir_all(&downloads).expect("plugin downloads");
    rig.config
        .save_plugin_blocking(
            "fetcher",
            PluginConfig {
                enabled: true,
                settings: std::collections::HashMap::from([(
                    "downloads_dir".to_owned(),
                    downloads.to_string_lossy().into_owned(),
                )]),
            },
            &std::collections::HashSet::new(),
        )
        .expect("plugin saves");

    let status = rig.acquire.import_folder.status().await;
    assert_eq!(status.state, ImportFolderState::Overlaps);
    assert_eq!(
        status.overlap.map(|(kind, _)| kind),
        Some(ImportFolderOverlap::Downloads)
    );
}

/// An album whose publish committed before a restart cut it off, one of
/// whose files changed since: the unchanged file leaves the folder (its
/// bytes are the library's), the changed one stays, and the library keeps
/// one copy of each track.
#[tokio::test]
async fn a_cut_off_album_leaves_only_what_the_library_holds() {
    let rig = Rig::new().await;
    let album = rig.folder().join("Portishead - Dummy");
    tagged(
        &album.join("01 Mysterons.flac"),
        "flac_full_01.flac",
        "Dummy",
        "Mysterons",
        1,
    );
    tagged(
        &album.join("02 Sour Times.flac"),
        "flac_full_02.flac",
        "Dummy",
        "Sour Times",
        2,
    );
    std::fs::write(album.join("cover.jpg"), b"a front cover").expect("cover");
    rig.cut_off(
        "Portishead - Dummy",
        &["01 Mysterons.flac", "02 Sour Times.flac"],
    )
    .await;
    {
        use std::io::Write as _;
        let mut changed = std::fs::OpenOptions::new()
            .append(true)
            .open(album.join("02 Sour Times.flac"))
            .expect("open");
        changed.write_all(&[0u8; 16]).expect("bytes change");
    }

    rig.pass().await;

    assert!(
        !album.join("01 Mysterons.flac").exists(),
        "its bytes are in the library"
    );
    assert!(
        album.join("02 Sour Times.flac").is_file(),
        "changed bytes stay"
    );
    assert!(
        !album.join("cover.jpg").exists(),
        "the cover it placed leaves too"
    );
    assert_eq!(
        rig.drop_tracks("drop_folder"),
        2,
        "one library copy of each track"
    );
}

/// An album all of whose files a publish placed before a restart settles
/// as imported, and its files and cover leave the folder.
#[tokio::test]
async fn a_cut_off_album_already_in_settles_as_imported() {
    let rig = Rig::new().await;
    let album = rig.folder().join("Portishead - Dummy");
    tagged(
        &album.join("01 Mysterons.flac"),
        "flac_full_01.flac",
        "Dummy",
        "Mysterons",
        1,
    );
    tagged(
        &album.join("02 Sour Times.flac"),
        "flac_full_02.flac",
        "Dummy",
        "Sour Times",
        2,
    );
    std::fs::write(album.join("cover.jpg"), b"a front cover").expect("cover");
    let item = rig
        .cut_off(
            "Portishead - Dummy",
            &["01 Mysterons.flac", "02 Sour Times.flac"],
        )
        .await;

    rig.pass().await;

    assert_eq!(rig.item_status(item), "imported");
    assert!(!album.exists(), "its files, cover and folder are gone");
    assert!(rig.music.join("Portishead/Dummy/cover.jpg").is_file());
    assert_eq!(rig.drop_tracks("drop_folder"), 2);
}

/// An album done arriving is not taken while a library scan runs, and is
/// once it ends.
#[tokio::test]
async fn a_ready_album_waits_out_a_scan() {
    let rig = Rig::new().await;
    let album = rig.import.join("Portishead - Dummy");
    tagged(
        &album.join("01 Mysterons.flac"),
        "flac_full_01.flac",
        "Dummy",
        "Mysterons",
        1,
    );
    let scan = |state: &'static str| {
        rig.acquire.db.write("test.scan", move |tx| {
            tx.execute(
                "INSERT INTO library_scan_runs (id, kind, trigger, state, phase, \
                 aggregate_scope, queued_at, updated_at) \
                 VALUES ('run-1', 'incremental', 'manual', ?1, 'indexing', '[]', 1, 1) \
                 ON CONFLICT(id) DO UPDATE SET state = excluded.state",
                [state],
            )?;
            Ok(())
        })
    };
    scan("indexing").await.expect("a scan runs");
    rig.pass_after_quiet().await;
    assert_eq!(
        rig.acquire.import_folder.status().await.state,
        ImportFolderState::WaitingForScan
    );
    assert!(
        album.join("01 Mysterons.flac").is_file(),
        "nothing taken while it runs"
    );
    assert_eq!(rig.drop_tracks("drop_folder"), 0);

    scan("completed").await.expect("the scan ends");
    rig.pass().await;
    assert_eq!(rig.drop_tracks("drop_folder"), 1, "taken once it ended");
    assert!(!album.join("01 Mysterons.flac").exists());
}

/// A leftover whose file changed since it was listed is not the file a
/// person was shown: Discard refuses it and leaves it, and the next pass
/// takes the old row off the list and looks at the file again.
#[tokio::test]
async fn a_leftover_that_changed_is_looked_at_again() {
    let rig = Rig::new().await;
    let odd = rig.import.join("Odds/09 Glory Box.flac");
    tagged(&odd, "flac_full_01.flac", "Nothing Alike", "Glory Box", 9);
    rig.pass_after_quiet().await;
    let listed = |entries: Vec<droppedneedle_acquire::held::store::Entry>| {
        entries
            .into_iter()
            .find(|entry| entry.origin == EntryOrigin::ImportFolder)
    };
    let first = listed(
        rig.acquire
            .held
            .list(&admin(), None, None, false)
            .await
            .expect("list"),
    )
    .expect("the leftover is listed");
    {
        use std::io::Write as _;
        let mut changed = std::fs::OpenOptions::new()
            .append(true)
            .open(&odd)
            .expect("open");
        changed.write_all(&[0u8; 16]).expect("bytes change");
    }

    assert!(
        rig.acquire
            .held
            .discard(&admin(), first.id, None)
            .await
            .is_err(),
        "a changed file is not deleted as the one listed"
    );
    assert!(odd.is_file());

    rig.pass_after_quiet().await;
    let again = listed(
        rig.acquire
            .held
            .list(&admin(), None, None, false)
            .await
            .expect("list"),
    )
    .expect("looked at again and listed");
    assert_ne!(again.id, first.id, "the old row went");
    assert!(odd.is_file());
}

/// Switching the import folder leaves the old one's leftover as it is: a
/// file at the same path in the new folder is new work of its own, the old
/// leftover stays listed, Match on it says its folder is no longer set, and
/// Discard only takes it off the list.
#[tokio::test]
async fn a_new_folder_neither_blocks_nor_drops_the_old_ones_leftovers() {
    let rig = Rig::new().await;
    let odd = rig.import.join("Odds/09 Glory Box.flac");
    tagged(&odd, "flac_full_01.flac", "Nothing Alike", "Glory Box", 9);
    rig.pass_after_quiet().await;
    let leftovers = || async {
        rig.acquire
            .held
            .list(&admin(), None, None, false)
            .await
            .expect("list")
            .into_iter()
            .filter(|entry| entry.origin == EntryOrigin::ImportFolder)
            .collect::<Vec<_>>()
    };
    let old = leftovers().await;
    assert_eq!(old.len(), 1, "the old folder's leftover");

    // The same file, same size and time, at the same path in a new folder.
    let other = ScratchDir::new("import-folder-b");
    let copy = other.join("Odds/09 Glory Box.flac");
    std::fs::create_dir_all(copy.parent().expect("folder")).expect("folder");
    std::fs::copy(&odd, &copy).expect("copy");
    let modified = std::fs::metadata(&odd)
        .and_then(|meta| meta.modified())
        .expect("time");
    std::fs::File::options()
        .write(true)
        .open(&copy)
        .and_then(|file| file.set_modified(modified))
        .expect("same time");
    rig.set_folder(&other);
    rig.pass_after_quiet().await;

    let now = leftovers().await;
    assert_eq!(now.len(), 2, "the new folder's file is new work: {now:?}");
    assert!(
        now.iter().any(|entry| entry.id == old[0].id),
        "the old leftover stays"
    );

    let matched = rig
        .acquire
        .held
        .match_album(&admin(), old[0].id, GROUP, None, "u-admin")
        .await;
    assert!(
        matched
            .as_ref()
            .is_err_and(|error| error.to_string().contains("no longer set")),
        "{matched:?}"
    );
    rig.acquire
        .held
        .discard(&admin(), old[0].id, None)
        .await
        .expect("taken off the list");
    assert!(odd.is_file(), "nothing in the old folder is touched");
    assert!(copy.is_file());
    let left = leftovers().await;
    assert_eq!(left.len(), 1);
    assert_ne!(left[0].id, old[0].id);
}

/// Play reads a leftover beneath its real folder and follows no link on
/// the way: an album folder swapped for a link once the file is checked
/// streams nothing from where the link points.
#[tokio::test]
async fn play_never_streams_through_a_folder_swapped_for_a_link() {
    let rig = Rig::new().await;
    let odd = rig.import.join("Odds/09 Glory Box.flac");
    tagged(&odd, "flac_full_01.flac", "Nothing Alike", "Glory Box", 9);
    rig.pass_after_quiet().await;
    let entry = rig
        .acquire
        .held
        .list(&admin(), None, None, false)
        .await
        .expect("held list")
        .into_iter()
        .find(|entry| entry.origin == EntryOrigin::ImportFolder)
        .expect("the leftover is listed");
    let file_id = entry.files[0].id.to_string();
    let play = || {
        audio_handler(
            State(rig.acquire.held.clone()),
            User(test_principal(
                "u-admin",
                Role::Admin,
                "sess-admin",
                SessionKind::Standard,
            )),
            AxumPath(file_id.clone()),
            HeaderMap::new(),
        )
    };
    let heard = play().await.expect("it plays").into_body();
    assert_eq!(
        axum::body::to_bytes(heard, usize::MAX)
            .await
            .expect("the whole file"),
        std::fs::read(&odd).expect("leftover")
    );

    let answer = play().await.expect("the file is checked");
    assert_eq!(answer.status(), StatusCode::OK);

    let elsewhere = ScratchDir::new("import-elsewhere");
    let size = std::fs::metadata(&odd).expect("leftover").len();
    std::fs::create_dir_all(elsewhere.join("Odds")).expect("folder");
    std::fs::write(
        elsewhere.join("Odds/09 Glory Box.flac"),
        vec![b'x'; usize::try_from(size).expect("size")],
    )
    .expect("a file of the same size");
    std::fs::rename(rig.import.join("Odds"), elsewhere.join("moved")).expect("moved out");
    std::os::unix::fs::symlink(elsewhere.join("Odds"), rig.import.join("Odds"))
        .expect("swapped for a link");

    let body = axum::body::to_bytes(answer.into_body(), usize::MAX).await;
    assert!(body.is_err(), "the swapped folder is refused at the read");
}
