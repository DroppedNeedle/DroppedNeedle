//! Open what the [`Platform`] is made of, in the order a safe start needs:
//! the data lock, the data-directory checks, the key, the database, the
//! provider cache file, the loop supervisor, the web UI, the settings and
//! the key check.

use std::sync::Arc;

use droppedneedle_auth::federated::settings::InstallIds;
use droppedneedle_kernel::{
    clock::{Clock, SystemClock},
    config::AppConfig,
    db::{DbConfig, DbRuntime, cache::CacheDb, datalock::DataLock, error::DbError, open_runtime},
    http_client::HttpClientFactory,
    ids::UuidGenerator,
    jobs::{CancellationToken, Supervisor},
    platform::{Platform, PlatformParts},
    runtime_config::{
        ConfigError, ConfigStore, Crypto, KeyOrigin, LegacyConfig, crypto::KEY_FILE_NAME,
        ingest::mark_skip, sections::MusicBrainzSettings,
    },
};

use crate::web::WebUi;

use super::{
    BootError,
    data_dirs::{DataDirs, check_bootable, refuse_lost_key},
    key_check, prepare_web, wait_out_busy,
};

/// The opened platform, with what boot keeps for itself: the database
/// runtime (backups, checkpoints and the shutdown), the provider cache
/// file, the stamped web UI and the data lock, held until the process
/// exits.
pub struct Opened {
    /// What every area is built from.
    pub platform: Platform,
    /// The database runtime behind `platform.db`.
    pub runtime: DbRuntime,
    /// The provider cache file. Only the providers' cache store holds it;
    /// boot keeps a handle to stop it at shutdown.
    pub cache_db: CacheDb,
    /// The web UI, or `None` when no build was found.
    pub web: Option<WebUi>,
    /// The data directory's lock.
    pub data_lock: DataLock,
}

/// Open the platform `config` describes.
pub async fn open_platform(config: AppConfig) -> Result<Opened, BootError> {
    // First, so a bad HTTP setting stops the start before any backup or
    // migration runs.
    let http = HttpClientFactory::with_settings(&config.http)?;
    // Held until shutdown, and taken before the database opens (so before
    // the pre-upgrade backup and the migrations): the offline import and
    // restore refuse to run while it is held, a running one keeps the
    // server from starting, and the server lock refuses a second server.
    let data_lock = DataLock::server(&config.library_db_path)?;
    refuse_foreign_data_dir(&config).await?;
    // The key comes before the database: a first start that stops early
    // never leaves a database without its key, and a database whose key is
    // gone was refused above rather than given a new one.
    let config_dir = config.config_dir();
    // A config.json found where no key is yet was not written by this
    // server: record that it is never to be copied in before the key
    // exists, so a start that fails before the settings open still skips it.
    if !config_dir.join(KEY_FILE_NAME).exists() {
        mark_skip(&config_dir)?;
    }
    let (crypto, key_origin) = Crypto::load_or_generate(&config_dir)?;
    let crypto = Arc::new(crypto);
    // The one clock: the database's backups, checkpoints and statistics,
    // the supervisor's rows and every area take time from it.
    let clock: Arc<dyn Clock> = Arc::new(SystemClock);
    let runtime = open_runtime(
        &DbConfig::new(&config.library_db_path)
            .with_shutdown_grace(config.shutdown_grace)
            .with_clock(Arc::clone(&clock)),
    )
    .await?;
    let db = runtime.db().clone();
    // After the data lock and the data-directory check, so a refused
    // folder never gains a new file. It takes a lock of its own: the
    // database may live elsewhere.
    let cache_db = CacheDb::open(&config.cache_dir, &config.library_db_path).await;
    // The one supervisor every background loop runs on, and its token the
    // one stop. Starting it clears the last start's loop rows, so it comes
    // before anything writes one.
    let supervisor = wait_out_busy(
        "loop supervisor",
        |error: &DbError| matches!(error, DbError::Busy { .. }),
        || Supervisor::start(db.clone(), Arc::clone(&clock), CancellationToken::new()),
    )
    .await?;
    // Before the settings open, so a base path the web UI cannot take
    // stops the start before anything is copied in.
    let web = prepare_web(&config).await?;
    let settings = Arc::new(open_settings(&db, &config_dir, key_origin, Arc::clone(&clock)).await?);
    key_check::verify(&db, &crypto, &clock, &config_dir, &config.library_db_path).await?;
    let platform = Platform::new(PlatformParts {
        config,
        db,
        settings,
        crypto,
        http,
        ids: Arc::new(UuidGenerator),
        supervisor,
    });
    Ok(Opened {
        platform,
        runtime,
        cache_db,
        web,
        data_lock,
    })
}

/// Open the settings, before anything reads or writes one: the store
/// copies in a `config.json` this database has never seen (which refuses
/// while any row exists). Then store the defaults that mint values once: a
/// fresh install has no MusicBrainz section, and its default mints a source
/// id, so it is stored once or every read reports a new source (and clients
/// keyed on it refetch without end); the Plex and Jellyfin install ids
/// likewise. Each step is safe to run again, so a busy database is waited
/// out.
async fn open_settings(
    db: &droppedneedle_kernel::db::Db,
    config_dir: &std::path::Path,
    key_origin: KeyOrigin,
    clock: Arc<dyn Clock>,
) -> Result<ConfigStore, BootError> {
    let legacy = LegacyConfig::at(config_dir, key_origin == KeyOrigin::Minted);
    let store = wait_out_busy("settings", ConfigError::is_busy, || async {
        let store = ConfigStore::open(
            db.clone(),
            Crypto::load(config_dir).map_err(|error| ConfigError::Database(error.to_string()))?,
            clock.clone(),
            legacy.clone(),
        )
        .await?;
        store.seed_default::<MusicBrainzSettings>().await?;
        // A busy database is waited out; any other failure (an install id
        // section that does not read) leaves the ids to their per-process
        // fallback rather than blocking every start.
        match InstallIds::new(Arc::new(store.clone())).ensure().await {
            Err(error) if error.is_busy() => return Err(error),
            Err(error) => {
                tracing::warn!(%error, "cannot store the install ids; using the fallback");
            }
            Ok(()) => {}
        }
        Ok::<_, ConfigError>(store)
    })
    .await?;
    Ok(store)
}

/// Refuse a v2 data directory, or one an upgrade left halfway, before the
/// database is opened, backed up or migrated: migrating would run v3's
/// schema over the v2 database. Refuse too a v3 database whose key file is
/// missing, before a new key is minted. Runs under the data lock, so the
/// upgrade tool cannot be moving files while this looks.
async fn refuse_foreign_data_dir(config: &AppConfig) -> Result<(), BootError> {
    let dirs = DataDirs::from_config(config);
    tokio::task::spawn_blocking(move || {
        check_bootable(&dirs).and_then(|()| refuse_lost_key(&dirs.config_dir, &dirs.db_path))
    })
    .await
    .map_err(|error| BootError::Stage {
        stage: "data directory check",
        message: error.to_string(),
    })?
    .map_err(BootError::from)
}
