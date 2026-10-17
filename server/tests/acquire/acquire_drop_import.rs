//! Drop import journey: a zip holding two album folders is unpacked; the
//! folder whose tags match a release is published into the library (as a
//! drop, under the naming template, its bonus track alongside) and
//! resolves the open request for that album; the folder nothing
//! identifies waits for review with a reason and an action, and
//! discarding it removes its files. A second drop with a better copy of
//! a track replaces the library's file, which goes to the recycle bin. Real
//! landing port, publisher and catalog over one scratch database;
//! MusicBrainz is a scripted release source.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use droppedneedle_acquire::drop_import::Viewer;
use droppedneedle_acquire::drop_import::store::JobRecord;
use droppedneedle_acquire::landing::library::LibraryLanding;
use droppedneedle_kernel::ids::{IdGenerator, UuidGenerator};
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

const GROUP: &str = "7d0b7a1e-0000-4000-8000-000000000001";
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

/// The acquire and library bundles over one scratch database, with the
/// curator and asker users and a `music` root, and the scratch folder.
/// The reads bundle and the auth rig are returned only to live as long as
/// the others.
async fn bundles() -> (
    droppedneedle_acquire::AcquireSetup,
    LibrarySetup,
    PathBuf,
    droppedneedle_catalog::reads::ReadsSetup,
    AuthRig,
) {
    let auth = AuthRig::new("drop-import").await;
    let ids: Arc<dyn IdGenerator> = Arc::new(UuidGenerator);
    let mut reads =
        droppedneedle_catalog::reads::ReadsSetup::for_tests(auth.deps.clone(), ids.clone())
            .expect("reads bundle");
    let acquire =
        droppedneedle_acquire::AcquireSetup::for_tests(ids.clone(), &mut reads.collections)
            .expect("acquire bundle");
    for (id, name) in [("u-curator", "Cora"), ("u-asker", "Asa")] {
        acquire.db.add_user(id, name, "user").await.expect("user");
    }
    let dir = acquire.db.path().parent().expect("scratch").to_path_buf();
    let config = Arc::new(
        ConfigStore::open(
            &dir.join("config.json"),
            Crypto::from_key_bytes(&[7u8; 32]).expect("key"),
        )
        .expect("config"),
    );
    let library = LibrarySetup::for_tests_at(auth.deps.clone(), ids, acquire.db.path(), config)
        .expect("library bundle");
    let music = dir.join("music");
    std::fs::create_dir_all(&music).expect("music root");
    library
        .add_root(
            Some("music".to_owned()),
            music.to_string_lossy().into_owned(),
            EffectivePolicy::Automatic,
        )
        .expect("root adds");
    (acquire, library, dir, reads, auth)
}

#[tokio::test]
async fn dropped_album_lands_and_the_unknown_folder_waits_for_review() {
    let (acquire, library, dir, _reads, _auth) = bundles().await;
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
    let pool = acquire.db.pool().clone();
    let acquire = acquire.with_library(Arc::new(LibraryLanding::new(
        library.clone(),
        Arc::new(OneRelease(release(&["Mysterons", "Sour Times"], &lengths))),
        pool,
    )));

    // Somebody else asked for the album.
    acquire
        .db
        .write("test.request", |tx| {
            tx.execute(
                "INSERT INTO request_history (musicbrainz_id_lower, musicbrainz_id, artist_name, \
                 album_title, requested_at, status, user_id, request_kind) \
                 VALUES (?1, ?1, 'Portishead', 'Dummy', '1700000000', 'pending', 'u-asker', \
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
    .fetch_one(acquire.db.pool())
    .await
    .expect("catalog");
    assert_eq!(drops, 3, "indexed as a drop, not a download");
    let request: String =
        sqlx::query_scalar("SELECT status FROM request_history WHERE musicbrainz_id_lower = ?1")
            .bind(GROUP)
            .fetch_one(acquire.db.pool())
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
    assert_eq!(odd.reason_code.as_deref(), Some("no_album_match"));
    assert!(odd.detail.is_some() && odd.reason_action.is_some());
    let staged: Vec<PathBuf> = odd.staging_paths.iter().map(PathBuf::from).collect();
    assert!(staged.iter().all(|path| path.is_file()));
    let discarded = acquire
        .drops
        .discard_item(&curator, odd.id)
        .await
        .expect("discard");
    assert_eq!(discarded.status, "discarded");
    assert!(staged.iter().all(|path| !path.exists()));

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
    let (acquire, library, dir, _reads, _auth) = bundles().await;
    let mut known = editions(1);
    known.extend(editions(2));
    let pool = acquire.db.pool().clone();
    let acquire = acquire.with_library(Arc::new(LibraryLanding::new(
        library.clone(),
        Arc::new(LargeGroups(known)),
        pool,
    )));
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

        let matched = acquire
            .drops
            .match_item(&curator, item.id, &group(case))
            .await
            .expect("matched by hand");
        assert_eq!(
            matched.status, "imported",
            "case {case}: {:?}",
            matched.detail
        );
        let releases: Vec<String> = sqlx::query_scalar(
            "SELECT DISTINCT release_mbid FROM local_album_external_identities \
             WHERE release_group_mbid = ?1",
        )
        .bind(group(case))
        .fetch_all(acquire.db.pool())
        .await
        .expect("identities");
        assert_eq!(releases, vec![release_id(case, 130)], "case {case}");
    }
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

/// Start a drop job on the zip at `upload` and wait for it to settle.
async fn upload_and_settle(
    acquire: &droppedneedle_acquire::AcquireSetup,
    curator: &Viewer,
    upload: PathBuf,
) -> JobRecord {
    let job = acquire
        .drops
        .create_job(
            &curator.user_id,
            "Cora",
            vec![("Purchase.zip".to_owned(), upload)],
        )
        .await
        .expect("job starts");
    for _ in 0..200 {
        let now = acquire.drops.get_job(curator, &job.id).await.expect("job");
        if now.status != "processing" {
            return now;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("the drop job never settled");
}

/// An entry whose header understates its size is refused whole: no part
/// of it stays in staging and nothing from it reaches the library.
#[tokio::test]
async fn an_entry_longer_than_its_header_leaves_nothing_behind() {
    let (acquire, library, _dir, _reads, _auth) = bundles().await;
    let pool = acquire.db.pool().clone();
    let acquire = acquire.with_library(Arc::new(LibraryLanding::new(
        library,
        Arc::new(OneRelease(release(&["Big"], &[None]))),
        pool,
    )));
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
        .fetch_one(acquire.db.pool())
        .await
        .expect("catalog");
    assert_eq!(indexed, 0);
}

/// An upload a restart cut off before it became items keeps its files when
/// the startup sweep cannot read the jobs: the job stays processing, and the
/// next sweep turns its files into items.
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
    let auth = AuthRig::on(&runtime);
    let ids: Arc<dyn IdGenerator> = Arc::new(UuidGenerator);
    let mut reads =
        droppedneedle_catalog::reads::ReadsSetup::for_tests(auth.deps.clone(), ids.clone())
            .expect("reads bundle");
    let acquire = droppedneedle_acquire::AcquireSetup::build(
        droppedneedle_acquire::db::AcquireDb::from_runtime(&runtime),
        &config,
        auth.deps.clone(),
        &droppedneedle_kernel::http_client::HttpClientFactory::new().expect("http"),
        ids,
        crate::common::config_store(&dir),
        &mut reads.collections,
        &crate::common::closed_outbound().spotify(),
    )
    .expect("acquire bundle");
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

    // The only reader is out, as on a swapping host at boot.
    let held = runtime.pool().acquire().await.expect("reader");
    acquire.drops.sweep_stale().await;
    drop(held);
    assert!(track.exists(), "the cut-off upload was swept");
    let job = store.get_job("job-cut").await.expect("read").expect("job");
    assert_eq!(job.status, "processing", "{:?}", job.error);
    assert!(job.items.is_empty(), "{:?}", job.items);

    acquire.drops.sweep_stale().await;
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
