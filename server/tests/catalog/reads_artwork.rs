//! Cover art over a scratch catalog: the scan's art sweep, the local-first
//! order, and the disk cache in front of the Cover Art Archive.

use std::path::Path;
use std::sync::Arc;

use axum::{
    Router,
    body::Body,
    http::{Request, StatusCode},
};
use droppedneedle_catalog::reads::platform::artwork::{
    ArtworkService, MISS_TTL_SECS, UNUSABLE_TTL_SECS,
    cache::{ArtworkCache, KeyEntry},
    local::LocalArtwork,
};
use droppedneedle_catalog::reads::platform::covers::{self, CoversState};
use droppedneedle_kernel::clock::SystemClock;
use droppedneedle_kernel::db::{DbError, DbRuntime, Lane};
use droppedneedle_kernel::runtime_config::secret_sections::AdvancedSettings;
use droppedneedle_library::scan::SqliteScanStore;
use droppedneedle_providers::coverart::COVER_ART_ARCHIVE_BASE;
use droppedneedle_providers::executor::ScriptedWire;
use droppedneedle_providers::{ProviderEndpoints, Providers};
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

const CDN: &str = "https://archive.org/download/mbid-x/front_500.png";

fn front_url(mbid: &str) -> String {
    format!("{COVER_ART_ARCHIVE_BASE}/release-group/{mbid}/front-500")
}

/// Archive stand-in: by default the front cover redirects to the CDN,
/// which answers with the PNG. Records each URL asked for.
struct FakeArchive {
    wire: Arc<ScriptedWire>,
    providers: Arc<Providers>,
}

impl FakeArchive {
    fn new() -> Self {
        let wire = Arc::new(ScriptedWire::new());
        let providers = Arc::new(Providers::over_wire(
            wire.clone(),
            Arc::new(SystemClock),
            ProviderEndpoints {
                coverart: COVER_ART_ARCHIVE_BASE.to_owned(),
                ..ProviderEndpoints::closed()
            },
        ));
        wire.answer(&front_url(RG), 307, &[("location", CDN)], "");
        wire.answer(CDN, 200, &[("content-type", "image/png")], PNG_BYTES);
        Self { wire, providers }
    }

    fn calls(&self) -> Vec<String> {
        self.wire
            .sent()
            .into_iter()
            .map(|sent| sent.url.to_string())
            .collect()
    }
}

struct Rig {
    runtime: DbRuntime,
    archive: FakeArchive,
    /// Declared last so the database closes before the directory goes.
    dir: ScratchDir,
}

/// A catalog with one album, and `seed_track` as its track scanned in the
/// library folder that holds it.
async fn rig(seed_track: Option<&Path>) -> Rig {
    let relative = seed_track.map(|track| track.file_name().unwrap().to_str().unwrap());
    rig_at(seed_track.zip(relative)).await
}

/// [`rig`] with the track's path relative to its library folder spelled
/// out, as the scan records it.
async fn rig_at(seed_track: Option<(&Path, &str)>) -> Rig {
    let dir = ScratchDir::new("reads-artwork");
    let db_path = dir.join("app.db");
    let runtime = crate::common::scratch_runtime(&db_path).await;
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
    if let Some((track, relative)) = seed_track {
        seed.push_str(&format!(
            "INSERT INTO local_tracks (id, local_album_id, root_id, file_path, relative_path, \
             path_hash, file_size_bytes, file_mtime_ns, stat_revision, title, title_folded, \
             album_title, album_title_folded, file_format, availability, ingest_source, \
             imported_at, membership_source) VALUES ('t1', 'al1', 'r1', '{}', '{}', 'h1', \
             1, 1, 's1', 'Opener', 'opener', 'First Light', 'first light', 'flac', 'indexed', \
             'scan', 1, 'automatic');",
            track.display(),
            relative
        ));
    }
    runtime
        .db()
        .write(Lane::Foreground, "artwork seed", move |tx| {
            tx.execute_batch(&seed).map_err(DbError::from)
        })
        .await
        .unwrap();
    Rig {
        runtime,
        archive: FakeArchive::new(),
        dir,
    }
}

impl Rig {
    fn app(&self, prefer_local: bool) -> Router {
        Self::router(self.service(prefer_local))
    }

    fn router(service: Arc<ArtworkService>) -> Router {
        covers::routes()
            .with_state(CoversState::new(service))
            .into_router()
    }

    fn service(&self, prefer_local: bool) -> Arc<ArtworkService> {
        let config =
            crate::common::config_store(&self.dir.join(format!("settings-{prefer_local}")));
        config
            .save_secret_blocking(AdvancedSettings {
                prefer_local_cover_art: prefer_local,
                ..AdvancedSettings::default()
            })
            .expect("art preference saves");
        let service = ArtworkService::new(
            ArtworkCache::new(
                self.dir.join("covers"),
                1024 * 1024,
                std::sync::Arc::new(droppedneedle_kernel::clock::SystemClock),
            ),
            LocalArtwork::new(self.runtime.db().sqlx().clone()),
            Some(Arc::new(self.archive.providers.coverart.clone())),
            config.live(),
        );
        Arc::new(service)
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
    let uri = format!("/api/v3/covers/release-group/{RG}?size=500");
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

/// How long a cover the archive could not give is remembered, and as
/// what: "no cover" (404) for hours and an image too large to use for
/// minutes, both as "no art"; an outage or a rate limit only as long as the
/// archive asked, as "could not ask", which the route answers 202.
#[tokio::test]
async fn archive_failures_are_remembered_for_as_long_as_they_mean() {
    type Headers = &'static [(&'static str, &'static str)];
    let rows: [(&str, u16, Headers, usize, u64, bool); 4] = [
        ("no cover", 404, &[], 0, MISS_TTL_SECS, false),
        ("outage", 503, &[("retry-after", "3")], 0, 3, true),
        (
            "too large to use",
            200,
            &[("content-type", "image/png")],
            20 * 1024 * 1024 + 1,
            UNUSABLE_TTL_SECS,
            false,
        ),
        ("rate limited", 429, &[("retry-after", "7")], 0, 7, true),
    ];
    for (index, (name, status, headers, body_len, ttl, unreachable)) in rows.into_iter().enumerate()
    {
        let rig = rig(None).await;
        let mbid = format!("0a1b2c3d-0000-4000-8000-00000000010{index}");
        rig.archive
            .wire
            .answer(&front_url(&mbid), status, headers, vec![0u8; body_len]);
        let before = unix_now();
        let (status, _, _) = get(
            rig.app(true),
            &format!("/api/v3/covers/release-group/{mbid}?size=500"),
        )
        .await;
        // A loaded host can outlast the route's wait, which then answers
        // "still warming" while the fetch finishes behind it.
        if unreachable {
            assert_eq!(status, StatusCode::ACCEPTED, "{name}: still warming");
        } else {
            assert!(
                matches!(status, StatusCode::NOT_FOUND | StatusCode::ACCEPTED),
                "{name}: no cover to serve, got {status}"
            );
        }
        let cache = ArtworkCache::new(
            rig.dir.join("covers"),
            1024 * 1024,
            std::sync::Arc::new(droppedneedle_kernel::clock::SystemClock),
        );
        let key = format!("caa:release-group:{mbid}:500");
        // The route answers after a bounded wait while the fetch carries on
        // in the background, so the marker can land after the answer.
        let until = tokio::time::timeout(std::time::Duration::from_secs(30), async {
            loop {
                match cache.key(&key).await {
                    Some(KeyEntry::Miss { until }) if !unreachable => break until,
                    Some(KeyEntry::Unreachable { until }) if unreachable => break until,
                    _ => {}
                }
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            }
        })
        .await
        .unwrap_or_else(|_| panic!("{name}: the failure is remembered"));
        let upper = unix_now();
        assert!(
            (before + ttl..=upper + ttl).contains(&until),
            "{name}: remembered for {ttl} s, until {until} (asked at {before})"
        );
    }
}

/// An archive that cannot be reached says nothing about the art: the cover
/// answers "warming" (202) while it is down, never "no art" (404), and is
/// served once the archive answers again.
#[tokio::test]
async fn a_cover_the_archive_cannot_give_yet_is_warming_until_it_can() {
    use droppedneedle_providers::TransportKind;
    use droppedneedle_providers::executor::{Method, Scripted};

    let rig = rig(None).await;
    let mbid = "0a1b2c3d-0000-4000-8000-000000000201";
    // The first request's try and its retry fail; the archive then answers.
    let down = Scripted::Transport(TransportKind::Connect);
    for _ in 0..2 {
        rig.archive
            .wire
            .on(Method::Get, &front_url(mbid), down.clone());
    }
    rig.archive
        .wire
        .answer(&front_url(mbid), 307, &[("location", CDN)], "");
    let uri = format!("/api/v3/covers/release-group/{mbid}?size=500");

    for _ in 0..2 {
        let (status, _, _) = get(rig.app(true), &uri).await;
        assert_eq!(
            status,
            StatusCode::ACCEPTED,
            "an unreachable archive is not 'no art'"
        );
    }

    // The wait before asking again runs out.
    let cache = ArtworkCache::new(
        rig.dir.join("covers"),
        1024 * 1024,
        std::sync::Arc::new(droppedneedle_kernel::clock::SystemClock),
    );
    cache
        .put_miss(&format!("caa:release-group:{mbid}:500"), 0)
        .await;
    let (status, _, body) = get(rig.app(true), &uri).await;
    assert_eq!(status, StatusCode::OK, "served once the archive answers");
    assert_eq!(body, PNG_BYTES);
}

/// A music app cannot poll a "warming" answer, so its cover request joins a
/// fetch a web page already started and gets the cover, where a web
/// page's poll is answered at once.
#[tokio::test]
async fn a_compat_cover_joins_a_running_fetch() {
    use droppedneedle_catalog::reads::platform::covers::CoverArt as _;

    let rig = rig(None).await;
    let service = rig.service(true);
    // The archive's turn comes in two seconds: within a page's wait.
    rig.archive
        .providers
        .limiter("coverartarchive")
        .unwrap()
        .cool_down(std::time::Duration::from_secs(2));
    let router = Rig::router(service.clone());
    let page = tokio::spawn(async move {
        get(
            router,
            &format!("/api/v3/covers/release-group/{RG}?size=500"),
        )
        .await
    });
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;

    let cover = service.album_cover("al1", Some("500")).await;
    assert_eq!(
        cover.map(|cover| cover.bytes),
        Some(PNG_BYTES.to_vec()),
        "the app gets the cover"
    );
    let (status, _, _) = page.await.unwrap();
    assert_eq!(status, StatusCode::OK);
}

const RED: [u8; 3] = [220, 20, 20];
const GREEN: [u8; 3] = [20, 200, 20];
const BLUE: [u8; 3] = [20, 20, 220];
const YELLOW: [u8; 3] = [230, 220, 20];
const MAGENTA: [u8; 3] = [210, 20, 210];

/// A solid square PNG of `side` pixels.
fn solid_png(colour: [u8; 3], side: u32) -> Vec<u8> {
    solid_png_sized(colour, side, side)
}

/// A solid PNG of `width` by `height` pixels.
fn solid_png_sized(colour: [u8; 3], width: u32, height: u32) -> Vec<u8> {
    let image = image::RgbImage::from_pixel(width, height, image::Rgb(colour));
    let mut out = Vec::new();
    image
        .write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png)
        .unwrap();
    out
}

/// Gives artist `a1` one album per cover, newest first, each with that
/// file as its folder art. Album `m{i}`'s cover is `files/m{i}/cover.png`.
async fn seed_artist_albums(rig: &Rig, files: &Path, covers: &[Vec<u8>]) {
    let mut seed = String::new();
    for (i, cover) in covers.iter().enumerate() {
        let folder = files.join(format!("m{i}"));
        std::fs::create_dir_all(&folder).unwrap();
        std::fs::write(folder.join("cover.png"), cover).unwrap();
        let year = 2020 - i;
        seed.push_str(&format!(
            "INSERT INTO local_albums (id, root_id, grouping_key, title, title_folded, \
             album_artist_name, album_artist_name_folded, album_artist_id, year, \
             grouping_source, created_at, updated_at) VALUES ('m{i}', 'r1', 'gm{i}', \
             'Album {i}', 'album {i}', 'Aurora', 'aurora', 'a1', {year}, 'automatic', 1, 1); \
             INSERT INTO local_tracks (id, local_album_id, root_id, file_path, relative_path, \
             path_hash, file_size_bytes, file_mtime_ns, stat_revision, title, title_folded, \
             album_title, album_title_folded, file_format, availability, ingest_source, \
             imported_at, membership_source) VALUES ('mt{i}', 'm{i}', 'r1', '{track}', \
             'm{i}/01.flac', 'mh{i}', 1, 1, 's1', 'Opener', 'opener', 'Album {i}', \
             'album {i}', 'flac', 'indexed', 'scan', 1, 'automatic'); \
             INSERT INTO local_album_artwork (local_album_id, source, source_locator, \
             version, updated_at) VALUES ('m{i}', 'folder', '{cover}', 1, 1);",
            track = folder.join("01.flac").display(),
            cover = folder.join("cover.png").display(),
        ));
    }
    rig.runtime
        .db()
        .write(Lane::Foreground, "artist albums", move |tx| {
            tx.execute_batch(&seed).map_err(DbError::from)
        })
        .await
        .unwrap();
}

/// Asks for `a1`'s picture at `size` and checks it is a `size` pixel JPEG
/// mosaic whose quadrants (left to right, top to bottom) show `quadrants`.
async fn assert_mosaic(rig: &Rig, size: u32, quadrants: [[u8; 3]; 4]) -> String {
    let (status, headers, body) = get(
        rig.app(true),
        &format!("/api/v3/library/artists/a1/artwork?size={size}"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(header(&headers, "content-type"), Some("image/jpeg"));
    assert_eq!(header(&headers, "x-cover-source"), Some("mosaic"));
    let picture = image::load_from_memory(&body).unwrap().to_rgb8();
    assert_eq!(picture.dimensions(), (size, size));
    let (near, far) = (size / 4, size * 3 / 4);
    let points = [(near, near), (far, near), (near, far), (far, far)];
    for ((x, y), want) in points.into_iter().zip(quadrants) {
        let got = picture.get_pixel(x, y).0;
        assert!(
            got.iter()
                .zip(want)
                .all(|(got, want)| got.abs_diff(want) <= 30),
            "pixel at ({x}, {y}) is {got:?}, wanted {want:?}"
        );
    }
    header(&headers, "etag").unwrap().to_owned()
}

/// Four or more albums with art: a 2x2 of the first four, newest first.
/// A cover that does not decode is passed over, not a failed picture.
#[cfg(unix)]
#[tokio::test]
async fn an_unlinked_artist_with_four_covers_is_a_two_by_two() {
    let files = ScratchDir::new("reads-artwork-mosaic-four");
    let rig = rig(None).await;
    let broken = b"\x89PNG\r\n\x1a\nnot really a picture".to_vec();
    let covers = [
        solid_png(RED, 300),
        broken,
        solid_png(GREEN, 300),
        solid_png(BLUE, 300),
        solid_png(YELLOW, 300),
        solid_png(MAGENTA, 300),
    ];
    seed_artist_albums(&rig, &files, &covers).await;
    assert_mosaic(&rig, 250, [RED, GREEN, BLUE, YELLOW]).await;
}

/// Two or three albums with art: the first two crosswise.
#[cfg(unix)]
#[tokio::test]
async fn an_unlinked_artist_with_two_or_three_covers_is_crosswise() {
    let files = ScratchDir::new("reads-artwork-mosaic-three");
    let rig = rig(None).await;
    let covers = [
        solid_png(RED, 300),
        solid_png(GREEN, 300),
        solid_png(BLUE, 300),
    ];
    seed_artist_albums(&rig, &files, &covers).await;
    assert_mosaic(&rig, 500, [RED, GREEN, GREEN, RED]).await;
}

/// One album with art: that cover as the album shows it.
#[cfg(unix)]
#[tokio::test]
async fn an_unlinked_artist_with_one_cover_shows_it() {
    let files = ScratchDir::new("reads-artwork-mosaic-one");
    let rig = rig(None).await;
    let cover = solid_png(RED, 300);
    seed_artist_albums(&rig, &files, std::slice::from_ref(&cover)).await;

    let (status, headers, body) = get(rig.app(true), "/api/v3/library/artists/a1/artwork").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, cover);
    assert_eq!(header(&headers, "x-cover-source"), Some("folder"));
}

/// No album with art (the rig's album has none): 404, and the page shows
/// its placeholder.
#[tokio::test]
async fn an_unlinked_artist_with_no_covers_has_no_picture() {
    let rig = rig(None).await;
    let (status, _, _) = get(rig.app(true), "/api/v3/library/artists/a1/artwork").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

/// A cover one pixel wide and thousands tall is cut to its centre square
/// before it is scaled, so it costs no more memory than its own pixels
/// and still fills its quadrant.
#[cfg(unix)]
#[tokio::test]
async fn a_sliver_cover_takes_its_quadrant_without_blowing_up() {
    let files = ScratchDir::new("reads-artwork-mosaic-sliver");
    let rig = rig(None).await;
    let covers = [
        solid_png_sized(YELLOW, 1, 200_000),
        solid_png(RED, 300),
        solid_png(GREEN, 300),
        solid_png(BLUE, 300),
    ];
    seed_artist_albums(&rig, &files, &covers).await;
    assert_mosaic(&rig, 250, [YELLOW, RED, GREEN, BLUE]).await;
}

/// A cover replaced on disk draws a new mosaic; the old one is not served.
#[cfg(unix)]
#[tokio::test]
async fn a_changed_cover_draws_a_new_mosaic() {
    let files = ScratchDir::new("reads-artwork-mosaic-change");
    let rig = rig(None).await;
    let covers = [solid_png(RED, 300), solid_png(GREEN, 300)];
    seed_artist_albums(&rig, &files, &covers).await;
    let before = assert_mosaic(&rig, 250, [RED, GREEN, GREEN, RED]).await;

    std::fs::write(files.join("m0").join("cover.png"), solid_png(BLUE, 200)).unwrap();
    let after = assert_mosaic(&rig, 250, [BLUE, GREEN, GREEN, BLUE]).await;
    assert_ne!(before, after);
}

#[tokio::test]
async fn embedded_art_from_the_scan_beats_the_archive_when_preferred() {
    let files = ScratchDir::new("reads-artwork-files");
    let track = files.join("01.flac");
    std::fs::copy("tests/fixtures/library/management_full.flac", &track).unwrap();
    let embedded = droppedneedle_library::tags::read_cover_art(&track)
        .unwrap()
        .expect("fixture carries art");
    let rig = rig(Some(&track)).await;

    let store = SqliteScanStore::new(
        rig.runtime.db().clone(),
        std::sync::Arc::new(droppedneedle_kernel::clock::SystemClock),
    );
    let first = store.refresh_album_artwork(&|| false).unwrap();
    assert_eq!((first.checked, first.changed), (1, 1));
    let again = store.refresh_album_artwork(&|| false).unwrap();
    assert_eq!(again.checked, 0, "unchanged files are not read again");

    let uri = format!("/api/v3/covers/release-group/{RG}?size=500");
    let (status, headers, body) = get(rig.app(true), &uri).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, embedded);
    assert_eq!(header(&headers, "x-cover-source"), Some("embedded"));
    assert!(rig.archive.calls().is_empty(), "local art needs no fetch");

    let (status, headers, body) =
        get(rig.app(true), "/api/v3/library/albums/al1/artwork?v=1").await;
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
        .fetch_optional(rig.runtime.db().sqlx())
        .await
        .unwrap()
}

#[tokio::test]
async fn art_that_comes_back_gets_a_new_version() {
    let files = ScratchDir::new("reads-artwork-version");
    let rig = rig(Some(&files.join("01.flac"))).await;
    let store = SqliteScanStore::new(
        rig.runtime.db().clone(),
        std::sync::Arc::new(droppedneedle_kernel::clock::SystemClock),
    );

    std::fs::write(files.join("cover.png"), PNG_BYTES).unwrap();
    touch_dir(&files, 1_000);
    assert!(store.refresh_album_artwork(&|| false).unwrap().complete);
    assert_eq!(art_version(&rig).await, Some(1));

    std::fs::remove_file(files.join("cover.png")).unwrap();
    touch_dir(&files, 2_000);
    store.refresh_album_artwork(&|| false).unwrap();
    assert_eq!(art_version(&rig).await, None, "removed art is cleared");

    let mut other = PNG_BYTES.to_vec();
    other.push(0);
    std::fs::write(files.join("Folder.PNG"), &other).unwrap();
    touch_dir(&files, 3_000);
    store.refresh_album_artwork(&|| false).unwrap();
    assert_eq!(
        art_version(&rig).await,
        Some(2),
        "a cleared version is never reused"
    );

    let stopped = store.refresh_album_artwork(&|| true).unwrap();
    assert!(!stopped.complete, "a stop request ends the sweep early");
}

/// A folder image is read only when nothing below the library folder on
/// the way to it is a link: a linked album folder can lead anywhere.
#[cfg(unix)]
#[tokio::test]
async fn folder_art_behind_a_linked_folder_is_not_served() {
    let files = ScratchDir::new("reads-artwork-links");
    let music = files.join("music");
    let outside = files.join("outside");
    std::fs::create_dir_all(music.join("Real")).unwrap();
    std::fs::create_dir_all(&outside).unwrap();
    let mut private = PNG_BYTES.to_vec();
    private.extend_from_slice(b"private");
    std::fs::write(music.join("Real/cover.png"), &private).unwrap();
    std::fs::write(outside.join("cover.png"), &private).unwrap();
    std::os::unix::fs::symlink(&outside, music.join("Linked")).unwrap();
    let rig = rig_at(Some((&music.join("Real/01.flac"), "Real/01.flac"))).await;

    set_folder_art(&rig, &music.join("Real/cover.png"), 1).await;
    let (status, _, body) = get(rig.app(true), "/api/v3/library/albums/al1/artwork?v=1").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, private, "a plain folder image is served");

    set_folder_art(&rig, &music.join("Linked/cover.png"), 2).await;
    let (_, headers, body) = get(rig.app(true), "/api/v3/library/albums/al1/artwork?v=2").await;
    assert_ne!(body, private, "nothing is read through the link");
    assert_ne!(header(&headers, "x-cover-source"), Some("folder"));
}

/// The scan records a track's path below its library folder in NFC while
/// the file keeps its name on disk, often NFD on music copied from a Mac.
/// The library folder is still found, so the art is served, and a link at
/// the first folder below it is still refused.
#[cfg(unix)]
#[tokio::test]
async fn folder_art_is_found_and_checked_under_decomposed_names() {
    let files = ScratchDir::new("reads-artwork-nfd");
    let music = files.join("music");
    let outside = files.join("outside");
    let (artist, album) = ("O\u{308}", "Bjo\u{308}rk");
    let on_disk = music.join(artist).join(album);
    std::fs::create_dir_all(&on_disk).unwrap();
    std::fs::create_dir_all(outside.join(album)).unwrap();
    let mut private = PNG_BYTES.to_vec();
    private.extend_from_slice(b"private");
    std::fs::write(on_disk.join("cover.png"), &private).unwrap();
    std::fs::write(outside.join(album).join("cover.png"), &private).unwrap();
    let rig = rig_at(Some((
        &on_disk.join("01.flac"),
        "\u{d6}/Bj\u{f6}rk/01.flac",
    )))
    .await;

    set_folder_art(&rig, &on_disk.join("cover.png"), 1).await;
    let (status, _, body) = get(rig.app(true), "/api/v3/library/albums/al1/artwork?v=1").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, private, "the folder image is served");

    std::fs::remove_dir_all(music.join(artist)).unwrap();
    std::os::unix::fs::symlink(&outside, music.join(artist)).unwrap();
    set_folder_art(&rig, &on_disk.join("cover.png"), 2).await;
    let (_, headers, body) = get(rig.app(true), "/api/v3/library/albums/al1/artwork?v=2").await;
    assert_ne!(body, private, "nothing is read through the link");
    assert_ne!(header(&headers, "x-cover-source"), Some("folder"));
}

/// Point the album's art at the folder image `path`.
async fn set_folder_art(rig: &Rig, path: &Path, version: i64) {
    let locator = path.display().to_string();
    rig.runtime
        .db()
        .write(Lane::Foreground, "folder art", move |tx| {
            tx.execute(
                "INSERT INTO local_album_artwork (local_album_id, source, source_locator, \
                 version, updated_at) VALUES ('al1', 'folder', ?1, ?2, 1) \
                 ON CONFLICT(local_album_id) DO UPDATE SET source_locator = ?1, version = ?2",
                rusqlite::params![locator, version],
            )
            .map_err(DbError::from)
        })
        .await
        .unwrap();
}

/// The covers on a user's Discover page are fetched in the background once
/// the page is built, so a later visit draws them from disk even when the
/// archive no longer answers. The warm runs on the jobs table, whose runner
/// the admin view lists.
#[tokio::test]
async fn a_built_discover_page_warms_its_covers_onto_disk() {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    use axum::response::IntoResponse as _;
    use axum::routing::get as route;

    #[derive(Default)]
    struct Archive {
        closed: AtomicBool,
        calls: AtomicUsize,
    }
    let archive = Arc::new(Archive::default());
    let front = archive.clone();
    let stub = Router::new()
        .route(
            "/listenbrainz/1/stats/sitewide/release-groups",
            route(|| async {
                axum::Json(serde_json::json!({"payload": {"release_groups": [{
                    "release_group_name": "First Light",
                    "artist_name": "Aurora",
                    "listen_count": 900,
                    "release_group_mbid": RG,
                }]}}))
            }),
        )
        .route(
            &format!("/coverart/release-group/{RG}/front-500"),
            route(move || {
                let front = front.clone();
                async move {
                    front.calls.fetch_add(1, Ordering::SeqCst);
                    if front.closed.load(Ordering::SeqCst) {
                        return StatusCode::SERVICE_UNAVAILABLE.into_response();
                    }
                    ([("content-type", "image/png")], PNG_BYTES).into_response()
                }
            }),
        )
        .fallback(|| async { StatusCode::NOT_FOUND });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move { axum::serve(listener, stub).await.unwrap() });
    let app =
        crate::common::TestApp::start_with("artwork-warm", crate::common::outbound_at(&origin))
            .await;
    let user = app.login_user("warm").await;
    let auth = [("authorization", user.authorization.as_str())];

    let (status, _) = app.call("GET", "/api/v3/discover", &auth, None).await;
    assert_eq!(status, StatusCode::OK);
    app.until("the page's warm job has run", || async {
        app.db()
            .query_row(
                "SELECT state FROM durable_jobs WHERE kind = 'artwork.warm'",
                [],
                |row| row.get::<_, String>(0),
            )
            .ok()
            .filter(|state| state == "done")
    })
    .await;
    assert_eq!(
        archive.calls.load(Ordering::SeqCst),
        1,
        "the warm fetched it"
    );

    archive.closed.store(true, Ordering::SeqCst);
    let (status, headers, body) = app
        .raw(
            "GET",
            &format!("/api/v3/covers/release-group/{RG}?size=250"),
            &auth,
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, PNG_BYTES);
    assert_eq!(headers["x-cover-source"], "cover-art-archive");
    assert_eq!(archive.calls.load(Ordering::SeqCst), 1, "served from disk");

    let admin = app.login_admin().await;
    let (_, stats) = app
        .call(
            "GET",
            "/api/v3/admin/queue-stats",
            &[("authorization", admin.as_str())],
            None,
        )
        .await;
    assert!(
        stats["jobs"]
            .as_array()
            .unwrap()
            .iter()
            .any(|job| job["name"] == "job-runner:artwork.warm"),
        "{stats}"
    );
    app.stop().await;
    server.abort();
}

/// Now, by the system clock the cache under test runs on.
fn unix_now() -> u64 {
    use droppedneedle_kernel::clock::Clock as _;
    u64::try_from(droppedneedle_kernel::clock::SystemClock.now_unix()).unwrap_or(0)
}
