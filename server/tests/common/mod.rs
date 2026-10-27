//! Shared test doubles: a fixed-id generator proving the trait seam, the
//! whole app for journeys, the auth rig with its test sign-in layer, and a
//! scratch directory that removes itself.

// Each test binary uses part of this module.
#![allow(dead_code, unused_imports)]

mod app;
pub mod app_passwords;
pub mod auth;
pub mod auth_fakes;
pub mod bundles;
pub mod compat;
pub mod library;
pub mod logs;
pub mod platform;
pub mod plugin_fakes;
pub mod sources;

pub use app::{Login, TestApp};
pub use platform::TestPlatform;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use axum::http::{HeaderMap, StatusCode, header::RETRY_AFTER};
use droppedneedle_kernel::{http_client::HttpClientFactory, ids::IdGenerator};

/// Fixed id used across tests. A valid UUID so id-format assertions hold.
pub const FIXED_ID: &str = "123e4567-e89b-12d3-a456-426614174000";

/// Fake id generator returning one fixed value.
#[derive(Debug, Clone)]
pub struct FixedIdGenerator {
    id: String,
}

impl FixedIdGenerator {
    pub fn new(id: &str) -> Self {
        Self { id: id.to_owned() }
    }
}

impl IdGenerator for FixedIdGenerator {
    fn new_id(&self) -> String {
        self.id.clone()
    }
}

fn test_http() -> HttpClientFactory {
    HttpClientFactory::new().expect("test client factory builds")
}

/// A fresh directory under the system temp dir, removed with everything in
/// it when dropped. Use this for every scratch path so test runs leave
/// nothing behind.
#[derive(Debug)]
pub struct ScratchDir {
    path: PathBuf,
}

impl ScratchDir {
    /// Create `<tmp>/dn-it-<tag>-<pid>-<seq>`.
    pub fn new(tag: &str) -> Self {
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let seq = SEQ.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!("dn-it-{tag}-{}-{seq}", std::process::id()));
        std::fs::create_dir_all(&path).expect("scratch dir creates");
        Self { path }
    }
}

impl std::ops::Deref for ScratchDir {
    type Target = Path;

    fn deref(&self) -> &Path {
        &self.path
    }
}

impl AsRef<Path> for ScratchDir {
    fn as_ref(&self) -> &Path {
        &self.path
    }
}

impl Drop for ScratchDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

/// Ask again while the app answers busy (503 with `Retry-After`), waiting
/// as long as it says, the way a client does. A busy answer means the
/// request was not carried out, so asking again is safe. For the requests
/// a test only needs to get through, such as signing in, on a host slow
/// enough that a pooled connection takes longer than its limit to free.
pub async fn through_busy<T, F>(mut ask: impl FnMut() -> F) -> (StatusCode, HeaderMap, T)
where
    F: std::future::Future<Output = (StatusCode, HeaderMap, T)>,
{
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let answer = ask().await;
        let retry_after = answer
            .1
            .get(RETRY_AFTER)
            .and_then(|value| value.to_str().ok()?.parse::<u64>().ok())
            .map(Duration::from_secs);
        let Some(wait) = retry_after.filter(|wait| {
            answer.0 == StatusCode::SERVICE_UNAVAILABLE && Instant::now() + *wait < deadline
        }) else {
            return answer;
        };
        tokio::time::sleep(wait).await;
    }
}

/// Write a private copy of the migrated database to `path`, which must not
/// exist yet. The migrations run once per test binary, not once per test.
pub fn migrated_db(path: &Path) {
    droppedneedle_kernel::schema::template::copy_to(path).expect("migrated database copies");
}

/// A one-connection pool on a private migrated database. Hold the guard:
/// dropping it removes the file.
pub async fn migrated_pool(tag: &str) -> (sqlx::SqlitePool, ScratchDir) {
    let dir = ScratchDir::new(tag);
    let path = dir.join("app.db");
    migrated_db(&path);
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(sqlx::sqlite::SqliteConnectOptions::new().filename(&path))
        .await
        .expect("migrated pool opens");
    (pool, dir)
}

/// The production runtime on a fresh migrated database at `path`, with
/// patient limits: a write or read waits up to a minute for its turn and a
/// write may run that long, so a loaded host slows a test instead of
/// answering `Busy`. A test that checks a limit opens its own runtime.
pub async fn scratch_runtime(path: &Path) -> droppedneedle_kernel::db::DbRuntime {
    use droppedneedle_kernel::db::{DbConfig, LaneLimits, TEST_PATIENCE, open_runtime};

    migrated_db(path);
    let config = DbConfig::new(path)
        .with_lane_limits(LaneLimits::patient())
        .with_read_wait(TEST_PATIENCE);
    open_runtime(&config).await.expect("scratch runtime opens")
}

/// A started loop supervisor over a fresh scratch runtime. Hold the guard:
/// dropping it removes the database.
pub async fn scratch_supervisor(
    tag: &str,
) -> (
    droppedneedle_kernel::jobs::Supervisor,
    droppedneedle_kernel::db::DbRuntime,
    ScratchDir,
) {
    let dir = ScratchDir::new(tag);
    let runtime = scratch_runtime(&dir.join("app.db")).await;
    let supervisor = droppedneedle_kernel::jobs::Supervisor::start(
        runtime.db().clone(),
        Arc::new(droppedneedle_kernel::clock::SystemClock),
        droppedneedle_kernel::jobs::CancellationToken::new(),
    )
    .await
    .expect("loop supervisor starts");
    (supervisor, runtime, dir)
}

/// Wait for the row of loop `name` to satisfy `check`, and return it. The
/// rows are written on the lane's own thread, so the bound is wall-clock
/// time; the wait yields instead of sleeping, so a paused clock does not
/// move while it waits.
pub async fn loop_row(
    db: &droppedneedle_kernel::db::Db,
    name: &str,
    check: impl Fn(&droppedneedle_kernel::db::LoopRecord) -> bool,
) -> droppedneedle_kernel::db::LoopRecord {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let rows = droppedneedle_kernel::db::list_loops(db)
            .await
            .expect("loop rows read");
        if let Some(row) = rows.into_iter().find(|row| row.name == name && check(row)) {
            return row;
        }
        assert!(Instant::now() < deadline, "loop row {name} never matched");
        tokio::task::yield_now().await;
        std::thread::sleep(Duration::from_millis(2));
    }
}

/// Reads inputs for production-shaped rigs: covers and the YouTube quota
/// under `dir` and no linked ListenBrainz accounts.
/// Nothing dials out unless a test asks for a cover or a chart.
pub fn reads_inputs(dir: &Path) -> droppedneedle_catalog::reads::ReadsInputs {
    let http = test_http();
    droppedneedle_catalog::reads::ReadsInputs {
        covers_dir: dir.join("covers"),
        cover_cache_max_bytes: 16 * 1024 * 1024,
        http: http.shared().clone(),
        providers: Arc::new(droppedneedle_providers::Providers::closed()),
        listenbrainz_links: Arc::new(
            droppedneedle_media::playback::listening::MemoryListenBrainzLinkStore::new(Arc::new(
                droppedneedle_kernel::runtime_config::Crypto::from_key_bytes(&[7u8; 32])
                    .expect("test key"),
            )),
        ),
        youtube_quota_path: dir.join("youtube_quota.json"),
        discover_warmer: false,
    }
}

/// Every outside service with a fixed address on the discard port, where
/// nothing listens, and no ffmpeg, so nothing transcodes.
pub fn closed_outbound() -> droppedneedle::bootstrap::Outbound {
    outbound_at("http://127.0.0.1:9")
}

/// Every outside service with a fixed address at its own path under one
/// origin.
pub fn outbound_at(origin: &str) -> droppedneedle::bootstrap::Outbound {
    let at = |path: &str| format!("{origin}/{path}");
    droppedneedle::bootstrap::Outbound {
        listenbrainz: at("listenbrainz"),
        lastfm: at("lastfm/"),
        audiodb: at("audiodb"),
        wikidata: at("wikidata"),
        wikipedia: at("wikipedia-{lang}"),
        commons: at("commons"),
        itunes: at("itunes/search"),
        deezer: at("deezer"),
        cover_art_archive: at("coverart"),
        discogs: at("discogs"),
        lrclib: at("lrclib"),
        github_releases: at("releases"),
        github_api: at("github"),
        github_codeload: at("codeload"),
        youtube: at("youtube"),
        acoustid: at("acoustid"),
        pwned_passwords: at("pwned/range/"),
        plex_tv: at("plex"),
        spotify_api: at("spotify"),
        spotify_accounts: at("spotify-accounts"),
        spotify_cover_cdn: Some(at("spotify-cdn")),
        ticketmaster: at("ticketmaster"),
        skiddle: at("skiddle"),
        geocoding: at("geocoding"),
        musicbrainz: at("musicbrainz"),
        brainzmash: at("brainzmash"),
        identify_providers: droppedneedle::bootstrap::Port::live(),
        contribution_providers: droppedneedle::bootstrap::Port::live(),
        ffmpeg: None,
    }
}

/// The committed program that stands in for ffmpeg: it checks the shape of
/// the transcode command and answers one line,
/// `ffmpeg stand-in: <muxer> <bitrate>`.
pub fn ffmpeg_stand_in() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("testkit/ffmpeg/ffmpeg")
}

/// The settings of the server `config` describes, while it is stopped:
/// its database (a migrated copy is placed first when there is none) and
/// its key (minted when there is none), as the server's first start
/// would leave them.
pub fn app_settings(
    config: &droppedneedle_kernel::config::AppConfig,
) -> droppedneedle_kernel::runtime_config::ConfigStore {
    use droppedneedle_kernel::runtime_config::{ConfigStore, Crypto, LegacyConfig};
    std::fs::create_dir_all(config.config_dir()).expect("config dir creates");
    std::fs::create_dir_all(&config.cache_dir).expect("cache dir creates");
    if !config.library_db_path.exists() {
        migrated_db(&config.library_db_path);
    }
    let (crypto, _) = Crypto::load_or_generate(&config.config_dir()).expect("key loads");
    let db = droppedneedle_kernel::db::Db::open_for_tests(&config.library_db_path)
        .expect("database opens");
    ConfigStore::open_blocking(
        db,
        crypto,
        Arc::new(droppedneedle_kernel::clock::SystemClock),
        &LegacyConfig::None,
    )
    .expect("settings open")
}

/// A settings store over its own database in `dir`, with a fixed test key.
/// A `config.json` the test wrote in `dir` first is copied in, as at a
/// first start. Calling it again on the same `dir` reopens the same rows.
pub fn config_store(dir: &Path) -> Arc<droppedneedle_kernel::runtime_config::ConfigStore> {
    Arc::new(
        droppedneedle_kernel::runtime_config::ConfigStore::scratch(
            dir,
            droppedneedle_kernel::runtime_config::Crypto::from_key_bytes(&[7u8; 32])
                .expect("test key"),
        )
        .expect("config store opens"),
    )
}
