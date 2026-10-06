//! Cover art over a scratch catalog: the scan's art sweep, the local-first
//! order, and the disk cache in front of the Cover Art Archive.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use axum::{
    Router,
    body::Body,
    http::{Request, StatusCode},
};
use droppedneedle::db::{DbConfig, DbRuntime, Lane, OpError, open_runtime};
use droppedneedle::library::scan::SqliteScanStore;
use droppedneedle::providers::coverart::{CaaRequest, CaaTransport, RawResponse, TransportError};
use droppedneedle::reads::platform::artwork::{
    ArtworkService, cache::ArtworkCache, local::LocalArtwork, remote::cover_client,
};
use droppedneedle::reads::platform::covers::{self, CoversState};
use tower::ServiceExt as _;

use crate::common::ScratchDir;

const RG: &str = "0a1b2c3d-0000-4000-8000-000000000001";

/// 1x1 PNG standing in for archive art.
const PNG_BYTES: &[u8] = &[
    0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44, 0x52,
    0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1f, 0x15, 0xc4,
    0x89, 0x00, 0x00, 0x00, 0x0a, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9c, 0x63, 0x00, 0x01, 0x00, 0x00,
    0x05, 0x00, 0x01, 0x0d, 0x0a, 0x2d, 0xb4, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4e, 0x44, 0xae,
    0x42, 0x60, 0x82,
];

/// Archive stand-in: every front cover redirects to the CDN, which answers
/// with the PNG. Records each URL asked for.
#[derive(Clone, Default)]
struct FakeArchive {
    seen: Arc<Mutex<Vec<String>>>,
}

impl FakeArchive {
    fn calls(&self) -> Vec<String> {
        self.seen.lock().unwrap().clone()
    }
}

impl CaaTransport for FakeArchive {
    async fn get(&self, request: &CaaRequest) -> Result<RawResponse, TransportError> {
        self.seen.lock().unwrap().push(request.url.clone());
        if request.url.starts_with("https://coverartarchive.org/") {
            Ok(RawResponse::new(
                307,
                vec![(
                    "location",
                    "https://archive.org/download/mbid-x/front_500.png",
                )],
                Vec::new(),
            ))
        } else {
            Ok(RawResponse::new(
                200,
                vec![("content-type", "image/png")],
                PNG_BYTES.to_vec(),
            ))
        }
    }
}

struct Rig {
    runtime: DbRuntime,
    archive: FakeArchive,
    db_path: PathBuf,
    /// Declared last so the database closes before the directory goes.
    dir: ScratchDir,
}

async fn rig(seed_track: Option<&Path>) -> Rig {
    let dir = ScratchDir::new("reads-artwork");
    let db_path = dir.join("app.db");
    let runtime = open_runtime(&DbConfig::new(&db_path)).await.unwrap();
    let mut seed = format!(
        "INSERT INTO local_artists (id, display_name, folded_name, kind, created_at, updated_at) \
         VALUES ('a1', 'Aurora', 'aurora', 'group', 1, 1); \
         INSERT INTO local_albums (id, root_id, grouping_key, title, title_folded, \
         album_artist_name, album_artist_name_folded, album_artist_id, grouping_source, \
         created_at, updated_at) VALUES ('al1', 'r1', 'g1', 'First Light', 'first light', \
         'Aurora', 'aurora', 'a1', 'automatic', 1, 1); \
         INSERT INTO local_album_external_identities (local_album_id, provider, \
         release_group_mbid, decision_source, selected_at) \
         VALUES ('al1', 'musicbrainz', '{RG}', 'manual', 1);"
    );
    if let Some(track) = seed_track {
        seed.push_str(&format!(
            "INSERT INTO local_tracks (id, local_album_id, root_id, file_path, relative_path, \
             path_hash, file_size_bytes, file_mtime_ns, stat_revision, title, title_folded, \
             album_title, album_title_folded, file_format, availability, ingest_source, \
             imported_at, membership_source) VALUES ('t1', 'al1', 'r1', '{}', 't1.flac', 'h1', \
             1, 1, 's1', 'Opener', 'opener', 'First Light', 'first light', 'flac', 'indexed', \
             'scan', 1, 'automatic');",
            track.display()
        ));
    }
    runtime
        .lane()
        .write(Lane::Foreground, "artwork seed", move |tx| {
            tx.execute_batch(&seed).map_err(OpError::from)?;
            Ok(())
        })
        .await
        .unwrap();
    Rig {
        runtime,
        archive: FakeArchive::default(),
        db_path,
        dir,
    }
}

impl Rig {
    fn app(&self, prefer_local: bool) -> Router {
        let service = ArtworkService::new(
            ArtworkCache::new(self.dir.join("covers"), 1024 * 1024),
            LocalArtwork::new(self.runtime.pool().clone()),
            Some(Arc::new(cover_client(self.archive.clone()))),
            Arc::new(move || prefer_local),
        );
        covers::routes(CoversState::new(Arc::new(service)))
    }
}

async fn get(app: Router, uri: &str) -> (StatusCode, Vec<(String, String)>, Vec<u8>) {
    let response = app
        .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let headers = response
        .headers()
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_str().unwrap_or("").to_owned()))
        .collect();
    let body = axum::body::to_bytes(response.into_body(), 4 * 1024 * 1024)
        .await
        .unwrap()
        .to_vec();
    (status, headers, body)
}

fn header<'a>(headers: &'a [(String, String)], name: &str) -> Option<&'a str> {
    headers
        .iter()
        .find(|(k, _)| k == name)
        .map(|(_, v)| v.as_str())
}

#[tokio::test]
async fn archive_cover_is_fetched_once_then_served_from_cache() {
    let rig = rig(None).await;
    let uri = format!("/covers/release-group/{RG}?size=500");
    for _ in 0..2 {
        let (status, headers, body) = get(rig.app(true), &uri).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, PNG_BYTES);
        assert_eq!(
            header(&headers, "x-cover-source"),
            Some("cover-art-archive")
        );
    }
    assert_eq!(
        rig.archive.calls(),
        vec![
            format!("https://coverartarchive.org/release-group/{RG}/front-500"),
            "https://archive.org/download/mbid-x/front_500.png".to_owned(),
        ],
        "one fetch (the front plus its redirect hop), then the disk cache"
    );
}

#[tokio::test]
async fn embedded_art_from_the_scan_beats_the_archive_when_preferred() {
    let files = ScratchDir::new("reads-artwork-files");
    let track = files.join("01.flac");
    std::fs::copy("tests/fixtures/library/management_full.flac", &track).unwrap();
    let embedded = droppedneedle::library::tags::read_cover_art(&track)
        .unwrap()
        .expect("fixture carries art");
    let rig = rig(Some(&track)).await;

    let store = SqliteScanStore::open(&rig.db_path).unwrap();
    let first = store.refresh_album_artwork(&|| false);
    assert_eq!((first.checked, first.changed), (1, 1));
    let again = store.refresh_album_artwork(&|| false);
    assert_eq!(again.checked, 0, "unchanged files are not read again");

    let uri = format!("/covers/release-group/{RG}?size=500");
    let (status, headers, body) = get(rig.app(true), &uri).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, embedded);
    assert_eq!(header(&headers, "x-cover-source"), Some("embedded"));
    assert!(rig.archive.calls().is_empty(), "local art needs no fetch");

    let (status, headers, body) = get(rig.app(true), "/library/albums/al1/artwork?v=1").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, embedded);
    assert_eq!(
        header(&headers, "cache-control"),
        Some("private, max-age=31536000, immutable")
    );

    let (_, headers, body) = get(rig.app(false), &uri).await;
    assert_eq!(body, PNG_BYTES, "with the preference off the archive wins");
    assert_eq!(
        header(&headers, "x-cover-source"),
        Some("cover-art-archive")
    );
}

/// Stamp a folder's modification time, so the sweep sees a change however
/// coarse the filesystem's clock is.
fn touch_dir(dir: &Path, secs: u64) {
    std::fs::File::open(dir)
        .unwrap()
        .set_modified(std::time::UNIX_EPOCH + std::time::Duration::from_secs(secs))
        .unwrap();
}

async fn art_version(rig: &Rig) -> Option<i64> {
    sqlx::query_scalar("SELECT version FROM local_album_artwork WHERE local_album_id = 'al1'")
        .fetch_optional(rig.runtime.pool())
        .await
        .unwrap()
}

#[tokio::test]
async fn art_that_comes_back_gets_a_new_version() {
    let files = ScratchDir::new("reads-artwork-version");
    let rig = rig(Some(&files.join("01.flac"))).await;
    let store = SqliteScanStore::open(&rig.db_path).unwrap();

    std::fs::write(files.join("cover.png"), PNG_BYTES).unwrap();
    touch_dir(&files, 1_000);
    assert!(store.refresh_album_artwork(&|| false).complete);
    assert_eq!(art_version(&rig).await, Some(1));

    std::fs::remove_file(files.join("cover.png")).unwrap();
    touch_dir(&files, 2_000);
    store.refresh_album_artwork(&|| false);
    assert_eq!(art_version(&rig).await, None, "removed art is cleared");

    let mut other = PNG_BYTES.to_vec();
    other.push(0);
    std::fs::write(files.join("Folder.PNG"), &other).unwrap();
    touch_dir(&files, 3_000);
    store.refresh_album_artwork(&|| false);
    assert_eq!(
        art_version(&rig).await,
        Some(2),
        "a cleared version is never reused"
    );

    let stopped = store.refresh_album_artwork(&|| true);
    assert!(!stopped.complete, "a stop request ends the sweep early");
}
