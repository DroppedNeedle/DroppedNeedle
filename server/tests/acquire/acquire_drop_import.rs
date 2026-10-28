//! Drop import journey: a zip holding two album folders is unpacked; the
//! folder whose tags match a release is published into the library (as a
//! drop, under the naming template, its bonus track alongside) and
//! resolves the open request for that album; the folder nothing
//! identifies waits in the held list with a reason, its job folder kept
//! while it does, and discarding it removes its files and then the job
//! folder. A second drop with a better copy of
//! a track replaces the library's file, which goes to the recycle bin. Real
//! landing port, publisher and catalog over one scratch database;
//! MusicBrainz is a scripted release source.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use droppedneedle_acquire::db::AcquireDb;
use droppedneedle_acquire::downloads::queue_rows::Viewer as HeldViewer;
use droppedneedle_acquire::drop_import::Viewer;
use droppedneedle_acquire::drop_import::store::JobRecord;
use droppedneedle_acquire::held::store::Entry;
use droppedneedle_acquire::landing::library::LibraryLanding;
use droppedneedle_acquire::landing::ports::LandingLibrary;
use droppedneedle_acquire::reason::Reason;
use droppedneedle_kernel::ids::UuidGenerator;
use droppedneedle_kernel::runtime_config::{ConfigStore, Crypto};
use droppedneedle_library::identify::sources::{
    Edition, EditionPage, EditionQuery, ReleaseHit, ReleaseSource, SourceError,
};
use droppedneedle_library::matching::{CreditedArtist, Release, ReleaseMedium, ReleaseTrack};
use droppedneedle_library::scan::EffectivePolicy;
use droppedneedle_library::tags::TagField;
use droppedneedle_library::tags::save::{TagEdit, save_tags};
use droppedneedle_library::wiring::LibrarySetup;
use futures_util::future::BoxFuture;

use crate::common::auth::AuthRig;
use crate::common::library::{every_priority, library_at};

const GROUP: &str = "7d0b7a1e-0000-4000-8000-000000000001";

/// The open entry of the held list holding a drop item's files, and where
/// those files are.
async fn entry_of(
    acquire: &droppedneedle_acquire::AcquireSetup,
    user_id: &str,
    item_id: i64,
) -> (Entry, Vec<PathBuf>) {
    let viewer = HeldViewer {
        user_id: user_id.to_owned(),
        admin: false,
    };
    let entry = acquire
        .held
        .list(&viewer, None, None, false)
        .await
        .expect("held list")
        .into_iter()
        .find(|entry| entry.drop_item_id == Some(item_id))
        .unwrap_or_else(|| panic!("item {item_id} waits in the held list"));
    let paths = entry
        .files
        .iter()
        .filter_map(|file| acquire.held.folders().locate(file.root, &file.path))
        .collect();
    (entry, paths)
}
const RELEASE: &str = "7d0b7a1e-0000-4000-8000-000000000002";

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

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/library")
        .join(name)
}

/// A fixture copy tagged as one track, without MusicBrainz ids.
fn tagged(dir: &Path, fixture_name: &str, album: &str, title: &str, track: u32) -> Vec<u8> {
    let ext = Path::new(fixture_name)
        .extension()
        .map(|ext| ext.to_string_lossy().into_owned())
        .unwrap_or_default();
    let path = dir.join(format!("{track:02}-{title}.{ext}"));
    std::fs::copy(fixture(fixture_name), &path).expect("copy fixture");
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
    save_tags(&path, &edits).expect("tags save");
    std::fs::read(&path).expect("read tagged copy")
}

fn release(titles: &[&str], lengths: &[Option<u64>]) -> Release {
    Release {
        id: RELEASE.to_owned(),
        release_group_id: GROUP.to_owned(),
        title: "Dummy".to_owned(),
        artists: vec![CreditedArtist {
            id: "8f6bd1e4-fbe1-4f50-aa9b-94c450ec0f11".to_owned(),
            name: "Portishead".to_owned(),
            sort_name: Some("Portishead".to_owned()),
            join: String::new(),
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
            .zip(lengths)
            .enumerate()
            .map(|(index, (title, length))| ReleaseTrack {
                isrcs: Vec::new(),
                genres: Vec::new(),
                id: format!("7d0b7a1e-0000-4000-8000-0000000001{index:02}"),
                recording_id: format!("7d0b7a1e-0000-4000-8000-0000000002{index:02}"),
                title: (*title).to_owned(),
                artists: Vec::new(),
                disc: 1,
                position: index as u32 + 1,
                absolute_position: index as u32 + 1,
                length_ms: *length,
            })
            .collect(),
        ..Release::default()
    }
}

fn length_ms(bytes: &[u8], scratch: &Path) -> Option<u64> {
    let path = scratch.join("probe.flac");
    std::fs::write(&path, bytes).expect("probe copy");
    let format = droppedneedle_library::tags::format_for_path(&path).expect("format");
    let (_, header) =
        droppedneedle_library::tags::read::read_scan_metadata(&path, format).expect("header");
    header
        .duration_seconds
        .map(|seconds| (seconds * 1000.0) as u64)
}

/// The scratch database the acquire bundle is built on, the library bundle
/// over it, with the curator and asker users and a `music` root, and the
/// scratch folder. The auth rig is returned only to live as long as the
/// others.
async fn bundles() -> (AcquireDb, LibrarySetup, PathBuf, AuthRig) {
    let auth = AuthRig::new("drop-import").await;
    let db = AcquireDb::scratch().expect("scratch db");
    for (id, name) in [("u-curator", "Cora"), ("u-asker", "Asa")] {
        db.add_user(id, name, "user").await.expect("user");
    }
    let dir = db.path().parent().expect("scratch").to_path_buf();
    let config = Arc::new(
        ConfigStore::scratch(&dir, Crypto::from_key_bytes(&[7u8; 32]).expect("key"))
            .expect("config"),
    );
    let platform = crate::common::TestPlatform::on(db.db(), &dir)
        .settings(config)
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
    (db, library, dir, auth)
}

/// The acquire bundle over `db`, finished downloads landing in `library`.
fn acquire_over(
    db: &AcquireDb,
    library: Arc<dyn LandingLibrary>,
) -> droppedneedle_acquire::AcquireSetup {
    droppedneedle_acquire::AcquireSetup::for_tests(
        db.clone(),
        Arc::new(UuidGenerator),
        droppedneedle_kernel::events::EventHub::new(),
        Some(library),
    )
    .expect("acquire bundle")
}

#[tokio::test]
async fn dropped_album_lands_and_the_unknown_folder_waits_for_review() {
    let (db, library, dir, _auth) = bundles().await;
    let music = dir.join("music");
    let work = dir.join("work");
    std::fs::create_dir_all(&work).expect("work dir");
    // The first drop has a low-quality MP3 of track 1 and a bonus track
    // the release does not list; the second brings track 1 in FLAC.
    let mysterons_mp3 = tagged(&work, "mp3_full_01.mp3", "Dummy", "Mysterons", 1);
    let sour_times = tagged(&work, "flac_full_02.flac", "Dummy", "Sour Times", 2);
    let hidden = tagged(&work, "flac_full_01.flac", "Dummy", "Hidden Track", 3);
    let stranger = tagged(&work, "flac_full_01.flac", "Nothing Alike", "Glory Box", 9);
    let mysterons = tagged(&work, "flac_full_01.flac", "Dummy", "Mysterons", 1);
    let impostor = tagged(&work, "flac_full_01.flac", "Dummy", "Glory Box", 1);
    let lengths = [None, length_ms(&sour_times, &work)];
    let acquire = acquire_over(
        &db,
        Arc::new(LibraryLanding::new(
            &library,
            every_priority(Arc::new(OneRelease(release(
                &["Mysterons", "Sour Times"],
                &lengths,
            )))),
        )),
    );

    // Somebody else asked for the album.
    acquire
        .db
        .write("test.request", |tx| {
            tx.execute(
                "INSERT INTO request_history (musicbrainz_id_lower, musicbrainz_id, artist_name, \
                 album_title, requested_at, status, user_id, request_kind) \
                 VALUES (?1, ?1, 'Portishead', 'Dummy', 1700000000000, 'pending', 'u-asker', \
                 'album')",
                [GROUP],
            )?;
            Ok(())
        })
        .await
        .expect("request");

    let curator = Viewer {
        user_id: "u-curator".to_owned(),
        admin: false,
    };
    // One zip, two folders.
    let job = drop_zip(
        &acquire,
        &curator,
        "upload-1",
        &[
            ("Dummy/01 - Mysterons.mp3", &mysterons_mp3),
            ("Dummy/02 - Sour Times.flac", &sour_times),
            ("Dummy/03 - Hidden Track.flac", &hidden),
            ("Odds/09 Glory Box.flac", &stranger),
        ],
    )
    .await;

    let album = job
        .items
        .iter()
        .find(|item| item.folder_name == "Dummy")
        .expect("album item");
    assert_eq!(album.status, "imported", "{:?}", album.detail);
    assert_eq!(album.files_imported, 3, "{:?}", album.detail);
    assert_eq!(album.release_group_mbid.as_deref(), Some(GROUP));
    let shelf = music.join("Portishead/Dummy");
    assert!(shelf.join("0101 Mysterons.mp3").is_file());
    assert!(shelf.join("0102 Sour Times.flac").is_file());
    assert!(
        shelf.join("0103 Hidden Track.flac").is_file(),
        "the bonus track is kept alongside the album"
    );
    let drops: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM local_tracks WHERE availability = 'indexed' \
         AND ingest_source = 'drop' AND download_task_id IS NULL",
    )
    .fetch_one(acquire.db.db().sqlx())
    .await
    .expect("catalog");
    assert_eq!(drops, 3, "indexed as a drop, not a download");
    let request: String =
        sqlx::query_scalar("SELECT status FROM request_history WHERE musicbrainz_id_lower = ?1")
            .bind(GROUP)
            .fetch_one(acquire.db.db().sqlx())
            .await
            .expect("request row");
    assert_eq!(
        request, "imported",
        "the complete album resolves the request"
    );

    let odd = job
        .items
        .iter()
        .find(|item| item.folder_name == "Odds")
        .expect("review item");
    assert_eq!(odd.status, "needs_review");
    assert_eq!(odd.reason_code, Some(Reason::NoAlbumMatch));
    assert!(odd.detail.is_some());
    let (entry, staged) = entry_of(&acquire, &curator.user_id, odd.id).await;
    assert_eq!(staged.len(), 1);
    assert!(staged.iter().all(|path| path.is_file()));
    assert!(entry.expires_at > entry.created_at);
    // The job's folder stays while a file of it waits for a decision.
    let job_dir = PathBuf::from(&job.staging_dir);
    acquire.drops.sweep_stale().await.unwrap();
    assert!(job_dir.is_dir(), "a folder with a waiting upload was swept");
    let held_viewer = HeldViewer {
        user_id: curator.user_id.clone(),
        admin: false,
    };
    acquire
        .held
        .discard(&held_viewer, entry.id, None)
        .await
        .expect("discard");
    let now = acquire.drops.get_job(&curator, &job.id).await.expect("job");
    let odd = now
        .items
        .iter()
        .find(|item| item.id == odd.id)
        .expect("review item");
    assert_eq!(odd.status, "discarded");
    assert!(staged.iter().all(|path| !path.exists()));
    acquire.drops.sweep_stale().await.unwrap();
    assert!(!job_dir.exists(), "the job folder goes once nothing waits");

    // A better file tagged as another song never replaces the owned copy.
    let job = drop_zip(
        &acquire,
        &curator,
        "upload-wrong",
        &[("Dummy/01 - Glory Box.flac", &impostor)],
    )
    .await;
    assert!(
        job.items.iter().all(|item| item.status != "imported"),
        "{:?}",
        job.items
    );
    assert!(
        shelf.join("0101 Mysterons.mp3").is_file(),
        "owned copy kept"
    );
    assert!(!shelf.join("0101 Mysterons.flac").exists());

    // A better copy replaces the library's; the old file goes to the bin.
    let job = drop_zip(
        &acquire,
        &curator,
        "upload-2",
        &[("Dummy/01 - Mysterons.flac", &mysterons)],
    )
    .await;
    let upgrade = &job.items[0];
    assert_eq!(upgrade.status, "imported", "{:?}", upgrade.detail);
    assert!(
        upgrade
            .detail
            .as_deref()
            .is_some_and(|detail| detail.contains("upgraded 1")),
        "{:?}",
        upgrade.detail
    );
    assert!(shelf.join("0101 Mysterons.flac").is_file());
    assert!(!shelf.join("0101 Mysterons.mp3").exists(), "recycled");
}

/// An unidentified upload whose discs hold files of the same name waits as
/// one entry with every file: CD1/01.flac and CD2/01.flac are two files,
/// both kept on disk.
#[tokio::test]
async fn same_named_files_on_two_discs_both_wait() {
    let (db, library, dir, _auth) = bundles().await;
    let work = dir.join("work");
    std::fs::create_dir_all(&work).expect("work dir");
    let first = tagged(&work, "flac_full_01.flac", "Nothing Alike", "Glory Box", 1);
    let second = tagged(&work, "flac_full_02.flac", "Nothing Alike", "Roads", 1);
    let acquire = acquire_over(
        &db,
        Arc::new(LibraryLanding::new(
            &library,
            every_priority(Arc::new(OneRelease(release(&["Mysterons"], &[None])))),
        )),
    );
    let curator = Viewer {
        user_id: "u-curator".to_owned(),
        admin: false,
    };
    let job = drop_zip(
        &acquire,
        &curator,
        "upload-discs",
        &[("Odds/CD1/01.flac", &first), ("Odds/CD2/01.flac", &second)],
    )
    .await;
    assert_eq!(job.items.len(), 1, "{:?}", job.items);
    assert_eq!(job.items[0].status, "needs_review");
    let (entry, staged) = entry_of(&acquire, &curator.user_id, job.items[0].id).await;
    assert_eq!(entry.files.len(), 2, "{:?}", entry.files);
    assert!(staged.iter().all(|path| path.is_file()));
    acquire.drops.sweep_stale().await.unwrap();
    assert!(
        staged.iter().all(|path| path.is_file()),
        "a waiting file was swept"
    );
}

/// A better copy whose library copy can't go to the recycle bin (the
/// folder's `.recycle` links back into the library) changes nothing in
/// the library, and the upload is kept as a review item that still exists
/// on disk: it is never thrown away.
#[tokio::test]
async fn an_upgrade_the_bin_cannot_take_keeps_the_upload() {
    let (db, library, dir, _auth) = bundles().await;
    let music = dir.join("music");
    let work = dir.join("work");
    std::fs::create_dir_all(&work).expect("work dir");
    std::fs::create_dir_all(music.join("Elsewhere")).expect("folder");
    std::os::unix::fs::symlink(music.join("Elsewhere"), music.join(".recycle")).expect("bin link");
    let mysterons_mp3 = tagged(&work, "mp3_full_01.mp3", "Dummy", "Mysterons", 1);
    let mysterons = tagged(&work, "flac_full_01.flac", "Dummy", "Mysterons", 1);
    let acquire = acquire_over(
        &db,
        Arc::new(LibraryLanding::new(
            &library,
            every_priority(Arc::new(OneRelease(release(&["Mysterons"], &[None])))),
        )),
    );
    let curator = Viewer {
        user_id: "u-curator".to_owned(),
        admin: false,
    };
    drop_zip(
        &acquire,
        &curator,
        "upload-mp3",
        &[("Dummy/01 - Mysterons.mp3", &mysterons_mp3)],
    )
    .await;
    let shelf = music.join("Portishead/Dummy");
    let old = std::fs::read(shelf.join("0101 Mysterons.mp3")).expect("library copy");

    let job = drop_zip(
        &acquire,
        &curator,
        "upload-flac",
        &[("Dummy/01 - Mysterons.flac", &mysterons)],
    )
    .await;
    assert_eq!(
        std::fs::read(shelf.join("0101 Mysterons.mp3")).expect("library copy"),
        old,
        "the library copy stays"
    );
    assert!(!shelf.join("0101 Mysterons.flac").exists());
    let kept = job
        .items
        .iter()
        .find(|item| item.reason_code == Some(Reason::UpgradeBlocked))
        .unwrap_or_else(|| panic!("a review item keeps the upload: {:?}", job.items));
    assert_eq!(kept.status, "needs_review", "{:?}", kept.detail);
    let (entry, staged) = entry_of(&acquire, &curator.user_id, kept.id).await;
    assert_eq!(entry.files[0].reason, Reason::UpgradeBlocked);
    assert_eq!(staged.len(), 1);
    for path in &staged {
        assert_eq!(
            std::fs::read(path).expect("the upload is on disk"),
            mysterons,
            "{}",
            path.display()
        );
    }
}

/// MusicBrainz knowing large release groups: the search finds nothing,
/// the release index lists a group by page.
struct LargeGroups(Vec<Release>);

impl ReleaseSource for LargeGroups {
    fn search<'a>(
        &'a self,
        _title: &'a str,
        _artist: &'a str,
    ) -> BoxFuture<'a, Result<Vec<ReleaseHit>, SourceError>> {
        Box::pin(async { Ok(Vec::new()) })
    }

    fn release<'a>(&'a self, mbid: &'a str) -> BoxFuture<'a, Result<Option<Release>, SourceError>> {
        let found = self
            .0
            .iter()
            .find(|release| release.answers_to(mbid))
            .cloned();
        Box::pin(async move { Ok(found) })
    }

    fn canonical_recording<'a>(
        &'a self,
        _mbid: &'a str,
    ) -> BoxFuture<'a, Result<Option<String>, SourceError>> {
        Box::pin(async { Ok(None) })
    }

    fn search_editions<'a>(
        &'a self,
        query: &'a EditionQuery,
    ) -> BoxFuture<'a, Result<EditionPage, SourceError>> {
        let in_group = |release: &&Release| {
            query
                .release_group_mbid
                .as_deref()
                .is_some_and(|group| group.eq_ignore_ascii_case(&release.release_group_id))
        };
        let total = self.0.iter().filter(in_group).count() as u64;
        let items = self
            .0
            .iter()
            .filter(in_group)
            .skip(query.offset as usize)
            .take(query.limit as usize)
            .map(|release| Edition {
                release_mbid: release.id.clone(),
                release_group_mbid: release.release_group_id.clone(),
                title: release.title.clone(),
                disc_count: 1,
                track_count: release.tracks.len() as u32,
                score: 100,
                ..Edition::default()
            })
            .collect();
        Box::pin(async move {
            Ok(EditionPage {
                items,
                total,
                offset: u64::from(query.offset),
            })
        })
    }
}

/// Files matched by hand to an album of 150 editions land on the edition
/// they fit: the release their tags name, else the one with their track
/// count, wherever the release index lists it.
#[tokio::test]
async fn a_hand_match_finds_the_edition_in_a_large_group() {
    const TITLES: [&str; 5] = ["Wandering Star", "It's a Fire", "Numb", "Roads", "Pedestal"];
    let group = |case: u32| format!("8e1c8b2f-0000-4000-8000-00000000000{case}");
    let release_id = |case: u32, index: usize| format!("8e1c8b2f-000{case}-4000-8000-{index:012}");
    // Case 1: every edition has the files' three tracks, and the files
    // name edition 130. Case 2: only edition 130 has three tracks, and
    // the files name none.
    let editions = |case: u32| -> Vec<Release> {
        (0..150)
            .map(|index| {
                let count = if case == 2 && index != 130 { 5 } else { 3 };
                let mut edition = release(&TITLES[..count], &[None; 5][..count]);
                edition.id = release_id(case, index);
                edition.release_group_id = group(case);
                edition.title = format!("Dummy, part {case}");
                for (position, track) in edition.tracks.iter_mut().enumerate() {
                    track.id = format!("8e1c8b2f-000{case}-4000-9000-{index:06}{position:06}");
                    track.recording_id = format!("8e1c8b2f-000{case}-4000-a000-{position:012}");
                }
                edition
            })
            .collect()
    };
    let (db, library, dir, _auth) = bundles().await;
    let mut known = editions(1);
    known.extend(editions(2));
    let acquire = acquire_over(
        &db,
        Arc::new(LibraryLanding::new(
            &library,
            every_priority(Arc::new(LargeGroups(known))),
        )),
    );
    let curator = Viewer {
        user_id: "u-curator".to_owned(),
        admin: false,
    };

    for case in [1, 2] {
        let work = dir.join(format!("work-{case}"));
        std::fs::create_dir_all(&work).expect("work dir");
        let mut files: Vec<(String, Vec<u8>)> = Vec::new();
        for (index, title) in TITLES[..3].iter().enumerate() {
            let track = index as u32 + 1;
            let mut bytes = tagged(&work, "flac_full_01.flac", "Rough Mixes", title, track);
            if case == 1 {
                let path = work.join(format!("{track:02}-{title}.flac"));
                let named = TagEdit::new(TagField::MusicBrainzReleaseId, vec![release_id(1, 130)]);
                save_tags(&path, &[named]).expect("release id saved");
                bytes = std::fs::read(&path).expect("read tagged copy");
            }
            files.push((format!("Rough Mixes {case}/{track:02} {title}.flac"), bytes));
        }
        let entries: Vec<(&str, &Vec<u8>)> = files
            .iter()
            .map(|(name, bytes)| (name.as_str(), bytes))
            .collect();
        let job = drop_zip(&acquire, &curator, &format!("upload-{case}"), &entries).await;
        let item = &job.items[0];
        assert_eq!(
            item.status, "needs_review",
            "case {case}: {:?}",
            item.detail
        );

        let (entry, _) = entry_of(&acquire, &curator.user_id, item.id).await;
        let held_viewer = HeldViewer {
            user_id: curator.user_id.clone(),
            admin: false,
        };
        let matched = acquire
            .held
            .match_album(&held_viewer, entry.id, &group(case), None, &curator.user_id)
            .await
            .expect("matched by hand");
        assert_eq!(matched.imported, 3, "case {case}: {matched:?}");
        let now = acquire.drops.get_job(&curator, &job.id).await.expect("job");
        assert_eq!(
            now.items[0].status, "imported",
            "case {case}: {:?}",
            now.items[0].detail
        );
        let releases: Vec<String> = sqlx::query_scalar(
            "SELECT DISTINCT release_mbid FROM local_album_external_identities \
             WHERE release_group_mbid = ?1",
        )
        .bind(group(case))
        .fetch_all(acquire.db.db().sqlx())
        .await
        .expect("identities");
        assert_eq!(releases, vec![release_id(case, 130)], "case {case}");
    }
}

/// A curator's Match the library puts off (here its folder is gone)
/// leaves an upload's file as it was: still held for the reason a person
/// decides on, still paired to the album the app guessed, so the held
/// loop's automatic retry never places it there once the folder is back.
#[tokio::test]
async fn a_deferred_upload_match_leaves_its_files_as_they_were() {
    const GUESS_GROUP: &str = "9d0b7a1e-0000-4000-8000-000000000001";
    const GUESS: &str = "9d0b7a1e-0000-4000-8000-000000000002";
    let mut picked = release(&["Numb"], &[None]);
    picked.title = "Rough Mixes".to_owned();
    let mut guess = release(&["Numb"], &[None]);
    guess.id = GUESS.to_owned();
    guess.release_group_id = GUESS_GROUP.to_owned();
    guess.tracks[0].id = "9d0b7a1e-0000-4000-8000-000000000003".to_owned();
    let guessed_track = guess.tracks[0].clone();
    let (db, library, dir, _auth) = bundles().await;
    let acquire = acquire_over(
        &db,
        Arc::new(LibraryLanding::new(
            &library,
            every_priority(Arc::new(LargeGroups(vec![picked, guess]))),
        )),
    );
    let curator = Viewer {
        user_id: "u-curator".to_owned(),
        admin: false,
    };
    let work = dir.join("work");
    std::fs::create_dir_all(&work).expect("work dir");
    let bytes = tagged(&work, "flac_full_01.flac", "Odds", "Numb", 1);
    let job = drop_zip(&acquire, &curator, "odds", &[("Odds/01 Numb.flac", &bytes)]).await;
    let item = &job.items[0];
    assert_eq!(item.status, "needs_review", "{:?}", item.detail);
    let (entry, _) = entry_of(&acquire, &curator.user_id, item.id).await;
    let file_id = entry.files[0].id;
    // AcoustID heard another recording; the app's guess was another album.
    let (release, track, recording) = (
        GUESS.to_owned(),
        guessed_track.id.clone(),
        guessed_track.recording_id.clone(),
    );
    acquire
        .db
        .write("test.guess", move |tx| {
            tx.execute(
                "UPDATE acquire_held_files SET reason = 'fingerprint_mismatch', \
                 release_mbid = ?2, release_track_mbid = ?3, recording_mbid = ?4, \
                 disc_number = 1, track_number = 1 WHERE id = ?1",
                rusqlite::params![file_id, release, track, recording],
            )?;
            Ok(())
        })
        .await
        .expect("held as heard");

    let music = dir.join("music");
    std::fs::remove_dir(&music).expect("library folder gone");
    let held_viewer = HeldViewer {
        user_id: curator.user_id.clone(),
        admin: false,
    };
    let matched = acquire
        .held
        .match_album(&held_viewer, entry.id, GROUP, None, &curator.user_id)
        .await;
    assert!(matched.is_err(), "the person is told: {matched:?}");
    let held = || async {
        sqlx::query_as::<_, (String, String, Option<String>)>(
            "SELECT state, reason, release_mbid FROM acquire_held_files WHERE id = ?1",
        )
        .bind(file_id)
        .fetch_one(acquire.db.db().sqlx())
        .await
        .expect("held file")
    };
    let as_it_was = (
        "held".to_owned(),
        "fingerprint_mismatch".to_owned(),
        Some(GUESS.to_owned()),
    );
    assert_eq!(held().await, as_it_was);
    let now = acquire.drops.get_job(&curator, &job.id).await.expect("job");
    assert_eq!(now.items[0].status, "needs_review");

    std::fs::create_dir_all(&music).expect("library folder back");
    acquire.held.tend().await;
    assert_eq!(held().await, as_it_was, "nothing retried it");
    assert!(
        files_under(&music).is_empty(),
        "nothing reached the library"
    );
}

/// A bonus track of an upload a curator matched, which the library could
/// not take because its place was taken, loses the pairing the app had
/// guessed for it: the held loop's retry never places it on that album.
#[tokio::test]
async fn a_matched_uploads_bonus_track_is_not_retried_onto_the_old_guess() {
    const GUESS_GROUP: &str = "9d0b7a1e-0000-4000-8000-000000000011";
    const GUESS: &str = "9d0b7a1e-0000-4000-8000-000000000012";
    let picked = release(&["Mysterons", "Sour Times"], &[None, None]);
    let mut guess = release(&["Hidden Track"], &[None]);
    guess.id = GUESS.to_owned();
    guess.release_group_id = GUESS_GROUP.to_owned();
    guess.title = "Guess".to_owned();
    guess.tracks[0].id = "9d0b7a1e-0000-4000-8000-000000000013".to_owned();
    guess.tracks[0].recording_id = "9d0b7a1e-0000-4000-8000-000000000014".to_owned();
    let guessed = guess.tracks[0].clone();
    let (db, library, dir, _auth) = bundles().await;
    let acquire = acquire_over(
        &db,
        Arc::new(LibraryLanding::new(
            &library,
            every_priority(Arc::new(LargeGroups(vec![picked, guess]))),
        )),
    );
    let curator = Viewer {
        user_id: "u-curator".to_owned(),
        admin: false,
    };
    let work = dir.join("work");
    std::fs::create_dir_all(&work).expect("work dir");
    let mysterons = tagged(&work, "flac_full_01.flac", "Odds", "Mysterons", 1);
    let sour_times = tagged(&work, "flac_full_02.flac", "Odds", "Sour Times", 2);
    let hidden = tagged(&work, "flac_full_01.flac", "Odds", "Hidden Track", 3);
    let job = drop_zip(
        &acquire,
        &curator,
        "odds",
        &[
            ("Odds/01 Mysterons.flac", &mysterons),
            ("Odds/02 Sour Times.flac", &sour_times),
            ("Odds/03 Hidden Track.flac", &hidden),
        ],
    )
    .await;
    let item = &job.items[0];
    assert_eq!(item.status, "needs_review", "{:?}", item.detail);
    let (entry, _) = entry_of(&acquire, &curator.user_id, item.id).await;
    let file_id = entry
        .files
        .iter()
        .find(|file| file.original_name.contains("Hidden Track"))
        .expect("bonus track held")
        .id;
    // The app had guessed the bonus track was another album's.
    let (track, recording) = (guessed.id.clone(), guessed.recording_id.clone());
    acquire
        .db
        .write("test.guess", move |tx| {
            tx.execute(
                "UPDATE acquire_held_files SET release_mbid = ?2, release_track_mbid = ?3, \
                 recording_mbid = ?4, disc_number = 1, track_number = 1 WHERE id = ?1",
                rusqlite::params![file_id, GUESS, track, recording],
            )?;
            Ok(())
        })
        .await
        .expect("guessed pairing");

    let music = dir.join("music");
    let taken = music.join("Portishead/Dummy/0103 Hidden Track.flac");
    std::fs::create_dir_all(taken.parent().expect("album folder")).expect("album folder");
    std::fs::write(&taken, b"someone else's file").expect("stray file");
    let held_viewer = HeldViewer {
        user_id: curator.user_id.clone(),
        admin: false,
    };
    let matched = acquire
        .held
        .match_album(&held_viewer, entry.id, GROUP, None, &curator.user_id)
        .await
        .expect("matched by hand");
    assert_eq!(matched.imported, 2, "{matched:?}");
    let held = || async {
        sqlx::query_as::<_, (String, String, Option<String>)>(
            "SELECT state, reason, release_mbid FROM acquire_held_files WHERE id = ?1",
        )
        .bind(file_id)
        .fetch_one(acquire.db.db().sqlx())
        .await
        .expect("held file")
    };
    assert_eq!(
        held().await,
        ("held".to_owned(), "target_occupied".to_owned(), None),
        "the Match left it unpaired"
    );

    std::fs::remove_file(&taken).expect("place freed");
    acquire.held.tend().await;
    assert_eq!(
        held().await,
        ("held".to_owned(), "target_occupied".to_owned(), None),
        "nothing retried it"
    );
    assert!(
        !files_under(&music)
            .iter()
            .any(|path| path.to_string_lossy().contains("Guess")),
        "nothing went onto the guessed album"
    );
}

fn files_under(dir: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let path = entry.path();
        if path.is_dir() {
            found.extend(files_under(&path));
        } else {
            found.push(path);
        }
    }
    found
}

/// Upload one zip of `entries` and wait for its job to settle.
async fn drop_zip(
    acquire: &droppedneedle_acquire::AcquireSetup,
    curator: &Viewer,
    name: &str,
    entries: &[(&str, &Vec<u8>)],
) -> JobRecord {
    let incoming = acquire.drops.incoming_dir();
    std::fs::create_dir_all(&incoming).expect("incoming");
    let upload = incoming.join(name);
    {
        let mut zip = zip::ZipWriter::new(std::fs::File::create(&upload).expect("zip file"));
        let stored = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);
        for (entry, bytes) in entries {
            zip.start_file(*entry, stored).expect("zip entry");
            zip.write_all(bytes).expect("zip bytes");
        }
        zip.finish().expect("zip finish");
    }
    let job = upload_and_settle(acquire, curator, upload).await;
    assert_eq!(job.status, "completed", "{:?}", job.error);
    job
}

/// Start a drop job on the zip at `upload` and wait for the
/// `drop_import_updated` event that announces it settled: a job that
/// settles without one leaves the person's open tabs waiting on the poll.
async fn upload_and_settle(
    acquire: &droppedneedle_acquire::AcquireSetup,
    curator: &Viewer,
    upload: PathBuf,
) -> JobRecord {
    let mut events = acquire.events.subscribe(&curator.user_id);
    let job = acquire
        .drops
        .create_job(
            &curator.user_id,
            "Cora",
            vec![("Purchase.zip".to_owned(), upload)],
        )
        .await
        .expect("job starts");
    loop {
        let frame = tokio::time::timeout(Duration::from_secs(30), events.next())
            .await
            .expect("the settled drop job was announced")
            .expect("the event hub stays open");
        if frame.name() != "drop_import_updated" {
            continue;
        }
        let now = acquire.drops.get_job(curator, &job.id).await.expect("job");
        if now.status != "processing" {
            return now;
        }
    }
}

/// An entry whose header understates its size is refused whole: no part
/// of it stays in staging and nothing from it reaches the library.
#[tokio::test]
async fn an_entry_longer_than_its_header_leaves_nothing_behind() {
    let (db, library, _dir, _auth) = bundles().await;
    let acquire = acquire_over(
        &db,
        Arc::new(LibraryLanding::new(
            &library,
            every_priority(Arc::new(OneRelease(release(&["Big"], &[None])))),
        )),
    );
    let curator = Viewer {
        user_id: "u-curator".to_owned(),
        admin: false,
    };
    let mut writer = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    let stored =
        zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
    writer
        .start_file("Album/01 - Big.flac", stored)
        .expect("zip entry");
    writer.write_all(&vec![7u8; 3 << 20]).expect("zip bytes");
    let mut bytes = writer.finish().expect("zip finish").into_inner();
    // Claim 1 KiB unpacked in the local header (offset 22) and the central
    // directory entry (offset 24); the stored data stays 3 MiB.
    let central = bytes
        .windows(4)
        .position(|window| window == [0x50, 0x4b, 0x01, 0x02])
        .expect("central directory");
    for at in [22, central + 24] {
        bytes[at..at + 4].copy_from_slice(&1024u32.to_le_bytes());
    }
    let incoming = acquire.drops.incoming_dir();
    std::fs::create_dir_all(&incoming).expect("incoming");
    let upload = incoming.join("upload-liar");
    std::fs::write(&upload, &bytes).expect("upload");

    let job = upload_and_settle(&acquire, &curator, upload).await;
    assert_eq!(job.status, "failed", "{:?}", job.error);
    assert!(job.items.is_empty(), "{:?}", job.items);
    let mut left = Vec::new();
    let mut pending = vec![PathBuf::from(&job.staging_dir)];
    while let Some(dir) = pending.pop() {
        let Ok(children) = std::fs::read_dir(&dir) else {
            continue;
        };
        for child in children {
            let path = child.expect("staging entry").path();
            if path.is_dir() {
                pending.push(path);
            } else {
                left.push(path);
            }
        }
    }
    assert!(left.is_empty(), "staging still holds {left:?}");
    let indexed: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM local_tracks")
        .fetch_one(acquire.db.db().sqlx())
        .await
        .expect("catalog");
    assert_eq!(indexed, 0);
}

/// An upload a restart cut off before it became items keeps its files when
/// the startup sweep cannot read the jobs: the sweep answers busy, the job
/// stays processing, and the next sweep turns its files into items.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_cut_off_upload_survives_a_sweep_that_cannot_read_its_jobs() {
    let dir = crate::common::ScratchDir::new("drop-sweep-busy");
    let config = droppedneedle_kernel::config::AppConfig::with_root(0, &dir);
    // One reader and a short checkout limit, so the busy answer comes
    // without the real five-second wait.
    let mut db = droppedneedle_kernel::db::DbConfig::new(&config.library_db_path)
        .with_read_wait(Duration::from_millis(100));
    db.readers = 1;
    let runtime = droppedneedle_kernel::db::open_runtime(&db)
        .await
        .expect("runtime opens");
    let platform = crate::common::TestPlatform::on(runtime.db(), &dir)
        .config(config.clone())
        .build()
        .await;
    let auth = AuthRig::on(&platform);
    let (library, _) = library_at(&platform);
    let acquire = crate::common::bundles::acquire_at(
        &platform,
        auth.deps.clone(),
        &library,
        &crate::common::bundles::collections_at(&platform).stores,
    );
    acquire
        .db
        .add_user("u-ada", "Ada", "user")
        .await
        .expect("user");
    let staging = config.imports_dir().join("drop").join("job-cut");
    let track = staging.join("Album").join("01 Track.flac");
    std::fs::create_dir_all(track.parent().expect("album folder")).expect("staging");
    std::fs::write(&track, b"audio").expect("track");
    let store = droppedneedle_acquire::drop_import::store::DropStore::new(acquire.db.clone());
    store
        .create_job(
            "job-cut",
            "u-ada",
            "Ada",
            "album.zip",
            &staging.to_string_lossy(),
        )
        .await
        .expect("job");

    // The only reader is out, as on a swapping host at boot: the sweep
    // answers busy so its caller runs it again.
    let (started_tx, started_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
    let reader_db = runtime.db().clone();
    let holder = tokio::spawn(async move {
        reader_db
            .read("test.hold", move |_| {
                started_tx.send(()).ok();
                release_rx.recv().ok();
                Ok::<_, droppedneedle_kernel::db::DbError>(())
            })
            .await
    });
    tokio::task::spawn_blocking(move || started_rx.recv())
        .await
        .expect("join")
        .expect("the reader checks out");
    let swept = acquire.drops.sweep_stale().await;
    assert!(
        matches!(
            swept,
            Err(droppedneedle_acquire::drop_import::store::StoreError::Busy { .. })
        ),
        "{swept:?}"
    );
    release_tx.send(()).expect("release");
    holder.await.expect("join").expect("hold");
    assert!(track.exists(), "the cut-off upload was swept");
    let job = store.get_job("job-cut").await.expect("read").expect("job");
    assert_eq!(job.status, "processing", "{:?}", job.error);
    assert!(job.items.is_empty(), "{:?}", job.items);

    acquire.drops.sweep_stale().await.unwrap();
    assert!(track.exists(), "the grouped upload was swept");
    let job = store.get_job("job-cut").await.expect("read").expect("job");
    assert_eq!(job.items.len(), 1, "{:?}", job.items);
    assert_eq!(
        job.items[0].staging_paths,
        vec![track.to_string_lossy().into_owned()]
    );
    drop(acquire);
    runtime.shutdown().await;
}

/// A cut-off upload whose items cannot all be written keeps none of them,
/// so the next start groups every folder: a job left with some items
/// would never be grouped again and its other files would be swept.
#[tokio::test]
async fn a_cut_off_upload_is_grouped_whole_or_not_at_all() {
    let (db, library, dir, _auth) = bundles().await;
    let acquire = acquire_over(
        &db,
        Arc::new(LibraryLanding::new(
            &library,
            every_priority(Arc::new(OneRelease(release(&["Mysterons"], &[None])))),
        )),
    );
    let staging = dir.join("cut-staging");
    for folder in ["Album A", "Album B"] {
        let track = staging.join(folder).join("01 Track.flac");
        std::fs::create_dir_all(track.parent().expect("album folder")).expect("staging");
        std::fs::write(&track, b"audio").expect("track");
    }
    let store = droppedneedle_acquire::drop_import::store::DropStore::new(acquire.db.clone());
    store
        .create_job(
            "job-cut",
            "u-curator",
            "Cora",
            "albums.zip",
            &staging.to_string_lossy(),
        )
        .await
        .expect("job");
    // The second item of the job is refused, as a write cut off partway.
    let refuse = |sql: &'static str| {
        acquire.db.write("test.refuse", move |tx| {
            tx.execute_batch(sql)?;
            Ok(())
        })
    };
    refuse(
        "CREATE TRIGGER test_refuse_second_item BEFORE INSERT ON drop_import_items \
         WHEN EXISTS (SELECT 1 FROM drop_import_items WHERE job_id = NEW.job_id) \
         BEGIN SELECT RAISE(ABORT, 'refused'); END;",
    )
    .await
    .expect("trigger");
    acquire.drops.sweep_stale().await.unwrap();
    let job = store.get_job("job-cut").await.expect("read").expect("job");
    assert!(job.items.is_empty(), "{:?}", job.items);

    refuse("DROP TRIGGER test_refuse_second_item;")
        .await
        .expect("drop trigger");
    acquire.drops.sweep_stale().await.unwrap();
    let job = store.get_job("job-cut").await.expect("read").expect("job");
    let mut folders: Vec<&str> = job
        .items
        .iter()
        .map(|item| item.folder_name.as_str())
        .collect();
    folders.sort_unstable();
    assert_eq!(folders, ["Album A", "Album B"]);
}

/// An upload whose declared length is over the 8 GB limit is refused with
/// 413 before any of it is written, so a person learns the cap at once
/// instead of after sending gigabytes.
#[tokio::test]
async fn an_upload_declared_over_the_limit_is_refused_before_writing() {
    let app = crate::common::TestApp::start("drop-too-large").await;
    let admin = app.login_admin().await;
    let boundary = "too-large-boundary";
    let form = format!(
        "--{boundary}\r\nContent-Disposition: form-data; name=\"files\"; filename=\"big.zip\"\r\n\
         Content-Type: application/zip\r\n\r\nPK\r\n--{boundary}--\r\n"
    );
    let content_type = format!("multipart/form-data; boundary={boundary}");
    let declared = ((8u64 << 30) + 1).to_string();
    let (status, _, bytes) = app
        .raw_bytes(
            "POST",
            "/api/v3/import/uploads",
            &[
                ("authorization", admin.as_str()),
                ("content-type", content_type.as_str()),
                ("content-length", declared.as_str()),
            ],
            form.into_bytes(),
        )
        .await;
    let body: serde_json::Value = serde_json::from_slice(&bytes).expect("body is json");
    assert_eq!(status, axum::http::StatusCode::PAYLOAD_TOO_LARGE, "{body}");
    assert_eq!(body["error"]["code"], "PAYLOAD_TOO_LARGE");
    assert_eq!(body["error"]["message"], "Uploads are limited to 8 GB.");
    let incoming = app.state().acquire.drops.incoming_dir();
    let written = std::fs::read_dir(&incoming).map_or(0, |dir| dir.count());
    assert_eq!(written, 0, "nothing lands in {}", incoming.display());
}
