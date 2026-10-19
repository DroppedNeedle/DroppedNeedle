//! Composition root: one call from deployment config to a router and the
//! background work behind it.
//!
//! [`build`] opens the database (taking a verified backup first when the
//! schema is about to change), stamps the web UI, wires every bundle,
//! runs startup recovery, and starts the background loops. [`serve`]
//! (in `serve.rs`) owns the listener and the shutdown order. `main` only
//! parses arguments, tunes the allocator, starts the runtime and hands in
//! the stop signal.

pub mod data_dirs;
pub mod jobs;
pub mod key_check;
mod outbound;
mod serve;

use std::{fmt::Display, future::Future, sync::Arc, time::Duration};

use axum::Router;
use thiserror::Error;

pub use outbound::{Outbound, Port};
pub use serve::{Background, ServeError, Stopped, bind, serve};

use data_dirs::{BootRefusal, DataDirs, check_bootable, refuse_lost_key};
use jobs::JobsSetup;

use crate::{
    AppState,
    admin::{AdminDb, AdminSetup, quota::reload_overrides},
    app::create_app_with_web,
    settings::{
        effects::{Effects, LiveSaveEffects},
        library_catalog::SqliteLibraryPolicyCatalog,
        section_prefs::{SqliteLinkStatus, SqliteSectionPrefsStore},
        services::SqliteImpactBuckets,
        wiring::SettingsSetup,
    },
    web::{WebError, WebUi},
};
use droppedneedle_acquire::AcquireSetup;
use droppedneedle_auth::{
    federated::{MediaLinks, settings::InstallIds},
    prod::ProdAuth,
    wiring::AuthSetup,
};
use droppedneedle_catalog::reads::{
    ReadsInputs, ReadsSetup,
    catalog::{
        Catalog,
        library::LocalCatalog,
        plugin_sources::{PluginDetailsSource, PluginPurchaseLinks},
        upstream::{LiveCatalogSettings, Upstream},
    },
    collections::{db::CollectionsDb, remote_import::PlaylistImportSink},
    enrichment::{library::LiveEnrichment, live::production_enrichment},
};
use droppedneedle_compat::{
    CompatSetup, settings::LiveSettings, setup::CompatDeps, shared::address::ListenerConfig,
};
use droppedneedle_kernel::{
    config::AppConfig,
    db::{
        BOOT_WAIT, BackupService, DbConfig,
        datalock::{DataLock, DataLockError},
        error::DbError,
        open_runtime,
    },
    events::EventHub,
    http_client::{HttpClientError, HttpClientFactory},
    ids::UuidGenerator,
    jobs::{CancellationToken, Supervisor},
    runtime_config::{
        ConfigError, ConfigStore, Crypto, KeyOrigin, LegacyConfig,
        crypto::{CryptoError, KEY_FILE_NAME},
        ingest::mark_skip,
        quota::QUOTA_FILE_NAME,
        sections::MusicBrainzSettings,
    },
};
use droppedneedle_library::wiring::LibrarySetup;
use droppedneedle_media::{
    MediaSetup,
    playback::plugin_events::PluginPlayEvents,
    remotes::{
        connections::{
            ConfigServers, ConnectionResolver, CredentialCoder, SignInLinks, SqliteConnectionStore,
        },
        plex::ConfiguredPlexServer,
        presence_feed::MediaJobs,
    },
};
use droppedneedle_plugins::{scrobble::SqliteListenBrainzLinkStore, wiring::PluginsSetup};
use droppedneedle_providers::{InMemoryProviderCache, Providers};

/// Why the server could not boot.
#[derive(Debug, Error)]
pub enum BootError {
    /// The outbound HTTP clients could not be built.
    #[error(transparent)]
    Http(#[from] HttpClientError),
    /// The database failed a boot check, its backup or its migration.
    #[error("database: {0}")]
    Database(#[from] DbError),
    /// The web UI could not be stamped with the base path.
    #[error(transparent)]
    Web(#[from] WebError),
    /// The data-encryption key could not be loaded or created.
    #[error("encryption key: {0}")]
    Key(#[from] CryptoError),
    /// The offline tool holds the database.
    #[error("data lock: {0}")]
    DataLock(#[from] DataLockError),
    /// The data directory is not a v3 one: v2 data, or an upgrade that
    /// stopped halfway.
    #[error(transparent)]
    DataDir(#[from] BootRefusal),
    /// The settings could not be opened.
    #[error("settings: {0}")]
    Settings(#[from] droppedneedle_kernel::runtime_config::ConfigError),
    /// One bundle failed to wire or recover.
    #[error("{stage}: {message}")]
    Stage {
        /// Which part of the boot failed.
        stage: &'static str,
        /// Its error, as text.
        message: String,
    },
}

fn stage(stage: &'static str) -> impl FnOnce(String) -> BootError {
    move |message| BootError::Stage { stage, message }
}

/// The first pause before a boot step runs again on a busy database. Each
/// later pause doubles, up to [`LONGEST_BOOT_PAUSE`].
const FIRST_BOOT_PAUSE: Duration = Duration::from_millis(250);
/// The longest pause between two runs of a boot step.
const LONGEST_BOOT_PAUSE: Duration = Duration::from_secs(5);

/// Run one boot step, and run it again while it fails because the database
/// is busy, for up to [`BOOT_WAIT`]. Boot serves nothing yet, so a slow or
/// swapping disk is waited out rather than refused, as the migration
/// connection is. Any other failure, or a busy one past the wait, stops at
/// once with `stage` named. The step must be safe to run again.
pub async fn wait_out_busy<T, E, F, Fut>(
    stage: &'static str,
    is_busy: fn(&E) -> bool,
    mut step: F,
) -> Result<T, BootError>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<T, E>>,
    E: Display,
{
    let deadline = tokio::time::Instant::now() + BOOT_WAIT;
    let mut pause = FIRST_BOOT_PAUSE;
    loop {
        let error = match step().await {
            Ok(value) => return Ok(value),
            Err(error) => error,
        };
        let left = deadline.saturating_duration_since(tokio::time::Instant::now());
        if !is_busy(&error) || left.is_zero() {
            return Err(BootError::Stage {
                stage,
                message: error.to_string(),
            });
        }
        let wait = pause.min(left);
        tracing::warn!(
            stage,
            %error,
            wait_ms = u64::try_from(wait.as_millis()).unwrap_or(u64::MAX),
            "database busy during boot; trying the step again"
        );
        tokio::time::sleep(wait).await;
        pause = (pause * 2).min(LONGEST_BOOT_PAUSE);
    }
}

/// The media links a Plex or Jellyfin sign-in saves, written to the
/// per-user connections table (the rows the remotes routes read) in `db`.
pub fn sign_in_links(
    db: droppedneedle_kernel::db::Db,
    crypto: Arc<Crypto>,
    config_store: &ConfigStore,
) -> MediaLinks {
    MediaLinks::new(Arc::new(SignInLinks::new(Arc::new(
        ConnectionResolver::new(
            Arc::new(SqliteConnectionStore::new(db)),
            Arc::new(CredentialCoder::new(crypto)),
            Arc::new(ConfigServers::new(config_store)),
        ),
    ))))
}

/// What [`build_with`] hands back. Every background loop stops on the one
/// stop [`serve`] gives at shutdown.
pub struct Built {
    /// The app, ready to serve.
    pub router: Router,
    /// The dependencies the router serves from.
    pub state: AppState,
    /// The background work, for [`serve`] to stop.
    pub background: Background,
}

/// Build the production graph against the real outside services and start
/// its background work.
pub async fn build(config: AppConfig) -> Result<(Router, Background), BootError> {
    let built = build_with(config, Outbound::live()).await?;
    Ok((built.router, built.background))
}

/// Build the production graph and start its background work, reaching
/// every outside service with an address fixed in code at the one
/// `outbound` gives.
pub async fn build_with(config: AppConfig, outbound: Outbound) -> Result<Built, BootError> {
    let endpoints = outbound.catalog();
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
    let runtime = open_runtime(
        &DbConfig::new(&config.library_db_path).with_shutdown_grace(config.shutdown_grace),
    )
    .await?;
    // The one database handle every area is given.
    let db = runtime.db().clone();
    // The one supervisor every background loop runs on, and its token the
    // one stop. Starting it clears the last start's loop rows, so it comes
    // before anything writes one.
    let supervisor = wait_out_busy(
        "loop supervisor",
        |error: &DbError| matches!(error, DbError::Busy { .. }),
        || {
            Supervisor::start(
                db.clone(),
                Arc::new(droppedneedle_kernel::clock::SystemClock),
                CancellationToken::new(),
            )
        },
    )
    .await?;
    // The jobs table areas enqueue through; its kinds' runners start with
    // the other loops.
    let queues = jobs::QueueSetup::new(db.clone(), &config);
    let web = prepare_web(&config).await?;

    let clock = Arc::new(droppedneedle_kernel::clock::SystemClock);
    let config_store = Arc::new(open_settings(&db, &config_dir, key_origin, clock.clone()).await?);
    let wall: Arc<dyn droppedneedle_kernel::clock::Clock> = clock.clone();
    key_check::verify(&db, &crypto, &wall, &config_dir, &config.library_db_path).await?;
    let ids = Arc::new(UuidGenerator);
    // One live event hub for the process. Bundles built below publish into
    // it once the state attaches it; the revision loops feed it after
    // each commit.
    let events = EventHub::new();
    // The plugin host exists early so playback, acquisition and streaming
    // can hold it; no plugin starts until `PluginsSetup::build` loads it.
    let plugin_host = droppedneedle_plugins::wiring::new_host(
        config.plugins_dir(),
        config_store.clone(),
        supervisor.clone(),
    );
    let auth_bundle = ProdAuth::new(
        &db,
        crypto.clone(),
        ids.clone(),
        clock.clone(),
        &config.cache_dir,
    );
    let auth = AuthSetup::build_with_upstreams(
        auth_bundle,
        config_store.clone(),
        crypto.clone(),
        &http,
        ids.clone(),
        clock,
        &config.base_path,
        &outbound.sign_in(),
        Arc::new(ConfiguredPlexServer::new(
            http.shared().clone(),
            config_store.live(),
            InstallIds::new(config_store.clone()),
        )),
        sign_in_links(db.clone(), crypto.clone(), &config_store),
    )
    .with_trusted_proxies(config.trusted_proxies.clone());

    // Shared provider deps first: the reads enrichment pair paces through
    // these same limiters, so production holds one limiter set. The byte
    // cache stays shared with the admin stats and clear routes.
    let provider_cache = Arc::new(InMemoryProviderCache::new());
    let providers = Arc::new(Providers::new(provider_cache.clone()));
    // Read live so turning ListenBrainz or lyrics on or off takes effect
    // at once.
    let enrichment = production_enrichment(
        http.shared(),
        &providers,
        &endpoints,
        config_store.live(),
        config_store.live(),
    );
    let mut reads = ReadsSetup::build(
        &db,
        auth.users.clone(),
        ids.clone(),
        &config_store,
        ReadsInputs {
            covers_dir: config.cache_dir.join("covers"),
            cover_cache_max_bytes: config.cover_cache_max_bytes,
            http: http.shared().clone(),
            no_redirect: http.no_redirect().clone(),
            providers: providers.clone(),
            listenbrainz_links: Arc::new(SqliteListenBrainzLinkStore::new(
                db.clone(),
                crypto.clone(),
            )),
            youtube_quota_path: config.cache_dir.join(QUOTA_FILE_NAME),
            discover_warmer: config.discover_warmer,
            endpoints: endpoints.clone(),
        },
        Some(enrichment),
    )
    .with_collections(CollectionsDb::new(db.clone()))
    .with_catalog(
        Catalog::new(
            Upstream::new(
                &http,
                providers.clone(),
                Arc::new(LiveCatalogSettings::new(&config_store)),
                auth.users.clone(),
                endpoints.clone(),
            ),
            LocalCatalog::new(db.sqlx().clone()),
        )
        .with_purchase_links(Arc::new(PluginPurchaseLinks(plugin_host.clone())))
        .with_details(Arc::new(PluginDetailsSource(plugin_host.clone()))),
    );
    // Read live, so a source switch in Settings reaches the long-lived
    // clients without a restart.
    let mb_source = outbound.musicbrainz_source(config_store.live());
    let library = LibrarySetup::build(
        &http,
        ids.clone(),
        providers.clone(),
        mb_source.clone(),
        &db,
        queues.queue().clone(),
        &config_store,
        outbound.library(),
    )
    .map_err(stage("library setup"))?;
    // Plugin library writes hold the same root leases as the publisher.
    plugin_host.set_library_fs(library.fs.clone());
    reads = reads.with_library_files(droppedneedle_media::stream::local_files::LibraryFiles::new(
        db.sqlx().clone(),
        library.root_source(),
    ));
    let (media, media_workers) = MediaSetup::build(
        &db,
        &config,
        auth.users.clone(),
        crypto.clone(),
        &http,
        ids.clone(),
        Some(library.root_source()),
        &config_store,
        // Remote playlist imports land in the user's playlists.
        Arc::new(PlaylistImportSink::new(reads.collections.clone())),
        outbound.plex_tv.clone(),
        outbound.scrobbles(),
    )
    .map_err(stage("media setup"))?;
    let media = media.with_play_events(Arc::new(PluginPlayEvents(plugin_host.clone())));
    media
        .stream
        .engine
        .attach_plugins(plugin_host.clone(), http.direct_no_redirect().clone());
    let acquire = AcquireSetup::build(
        droppedneedle_acquire::db::AcquireDb::new(db.clone()),
        &config,
        auth.users.clone(),
        &http,
        ids.clone(),
        config_store.clone(),
        &mut reads.collections,
        &outbound.spotify(),
    )
    .map_err(stage("acquire setup"))?
    .with_plugins(plugin_host.clone())
    .with_release_poll(Arc::new(droppedneedle_acquire::search::live_release_poll(
        &http,
        providers.clone(),
        mb_source.clone(),
    )))
    .with_album_lookup(Arc::new(
        droppedneedle_acquire::target::lookup::live_albums(&http, providers.clone(), mb_source),
    ));
    // Finished downloads reach the library through its import seam.
    let acquire = match library.releases.clone() {
        Some(releases) => acquire.with_library(Arc::new(
            droppedneedle_acquire::landing::library::LibraryLanding::new(
                library.clone(),
                releases,
                db.sqlx().clone(),
            ),
        )),
        None => acquire,
    };
    // "Get missing tracks" asks the library which tracks the chosen
    // edition lacks.
    if acquire
        .requests
        .edition_plans
        .set(Arc::new(
            droppedneedle_library::operations::service::Operations::new(&library),
        ))
        .is_err()
    {
        tracing::warn!("edition plans were already attached");
    }
    // Removing an album also clears its download and wanted state.
    if library
        .removal_hook
        .set(Arc::new(
            droppedneedle_acquire::library_removal::AcquireAlbumCleanup::new(
                acquire.journal.clone(),
                acquire.requests.wanted.clone(),
            ),
        ))
        .is_err()
    {
        tracing::warn!("album removal cleanup was already attached");
    }
    // Discovery batches file their albums through the requests service.
    if reads
        .discover_requests
        .set(Arc::new(
            droppedneedle_acquire::requests::discovery::DiscoveryBatchRequests::new(
                acquire.requests.clone(),
                Some(library.clone()),
            ),
        ))
        .is_err()
    {
        tracing::warn!("discovery batch requests were already attached");
    }
    // Startup recovery before serving traffic. Re-running after a clean
    // shutdown is a no-op: nothing destructive repeats.
    let recovery = wait_out_busy(
        "acquire recovery",
        droppedneedle_acquire::wiring::RecoveryError::is_busy,
        || acquire.run_recovery(),
    )
    .await?;
    tracing::info!(
        redispatched = recovery.redispatched,
        resumed = recovery.resumed,
        restarted = recovery.restarted,
        "acquire recovery complete"
    );
    // Publish journal reconciliation; scan recovery runs when the scan
    // supervisor starts.
    let library_recovery = library.run_recovery().await;
    tracing::info!(
        publish_bundles = library_recovery.publish_recoveries.len(),
        "library recovery complete"
    );
    let compat = CompatSetup::build(CompatDeps::over_reads(
        auth.users.clone(),
        crypto.clone(),
        media.playback.clone(),
        media.stream.engine.clone(),
        library.clone(),
        &reads,
        LiveSettings::from_config(&config_store),
        ListenerConfig {
            port: config.port,
            trusted_proxies: config.trusted_proxies.clone(),
            base_path: config.base_path.clone(),
        },
    ));
    let admin = AdminSetup::new(
        auth.users.clone(),
        acquire.requests.quota.clone(),
        provider_cache.clone(),
        providers.clone(),
    )
    .with_db(AdminDb::new(db.clone()))
    .with_backups(BackupService::new(
        &config.library_db_path,
        runtime.backups().backup_dir(),
    ))
    .with_checkpoint(runtime.checkpoint().clone());
    // Durable quota overrides back into the live ledger. A failure warns
    // instead of stopping the boot; the admin can re-save.
    match wait_out_busy(
        "quota overrides",
        |error: &DbError| matches!(error, DbError::Busy { .. }),
        || reload_overrides(db.sqlx(), &acquire.requests.quota),
    )
    .await
    {
        Ok(loaded) => tracing::info!(loaded, "quota overrides reloaded"),
        Err(error) => tracing::warn!(%error, "quota overrides failed to reload; defaults apply"),
    }
    let concerts = droppedneedle_catalog::concerts::ConcertsSetup::from_db(
        &db,
        &http,
        &config_store,
        outbound.concerts(),
    )
    .with_events(Arc::new(events.clone()));
    // Your Weekly Mix: one builder behind the refresh route, the daily
    // loop and the scrobble-settings toggle.
    let mix_events = droppedneedle_kernel::events::EventSink::default();
    mix_events.attach(&events);
    if let Some(builder) = droppedneedle_acquire::requests::mix_sources::live_builder(
        &acquire.requests,
        providers.clone(),
        http.shared().clone(),
        Arc::new(SqliteListenBrainzLinkStore::new(db.clone(), crypto.clone())),
        Arc::new(droppedneedle_plugins::scrobble::SqliteScrobblePrefsStore::new(db.clone())),
        reads.collections.stores.playlists.clone(),
        mix_events,
        &outbound.listenbrainz,
    ) && acquire.requests.mixer.set(builder).is_err()
    {
        tracing::warn!("personal mix builder was already set");
    }
    let mix_hooks = Arc::new(droppedneedle_acquire::requests::mix::MixGrantHooks::new(
        acquire.requests.mixer.clone(),
    ));
    let jobs = JobsSetup::build(
        supervisor.clone(),
        runtime.checkpoint().clone(),
        config_store.clone(),
        MediaJobs {
            presence: media.playback.presence.clone(),
            resolver: Some(media.remotes.service.resolver().clone()),
            http: http.shared().clone(),
            pool: Some(db.sqlx().clone()),
        },
        concerts.sweep(),
    )
    .with_queue(queues)
    .with_job(
        droppedneedle_library::contrib::worker::SPEC,
        library.contrib_worker.handler(),
    )
    .with_mixer(mix_hooks.clone())
    .with_precache_sources_from(&reads, &config_store)
    .with_discovery_demand(Arc::new(
        droppedneedle_catalog::reads::discover::demand::ContentDemand(
            reads.discover.content.clone(),
        ),
    ));
    // Library Management plans genres, lyrics and covers through the same
    // clients and artwork cache the pages use, and AcoustID ids on
    // identification's shared rate limit.
    let acoustid_pacer =
        droppedneedle_providers::adapters::CorePacer::for_source(providers.clone(), "acoustid")
            .ok_or_else(|| "acoustid has no verified rate row".to_owned())
            .map_err(stage("library management"))?;
    let enrichment = LiveEnrichment {
        acoustid: droppedneedle_providers::acoustid::AcoustIdClient::new(
            http.shared().clone(),
            &outbound.acoustid,
            acoustid_pacer,
            droppedneedle_providers::adapters::HealthSink::new(&providers),
        ),
        library: config_store.live(),
        upstream: Upstream::new(
            &http,
            providers.clone(),
            Arc::new(LiveCatalogSettings::new(&config_store)),
            auth.users.clone(),
            endpoints,
        ),
        http: http.shared().clone(),
        artwork: reads.artwork.clone(),
        lrclib: Default::default(),
    };
    if library.enrichment.set(Arc::new(enrichment)).is_err() {
        tracing::warn!("library management enrichment was already attached");
    }
    let admin = match &reads.artwork {
        Some(artwork) => admin.with_covers(artwork.cache().clone()),
        None => admin,
    }
    .with_precache(jobs.precache_trigger());
    // Completed scans refresh the library's images, as v2 did after a sync.
    if library
        .scan_completed
        .set(jobs.precache_trigger().after_scan_hook())
        .is_err()
    {
        tracing::warn!("scan completion hook was already set");
    }
    let download_worker = acquire.worker.clone();
    // Each section's declared effects run after every committed change to
    // it, whoever saved it; settings routes wait for them before answering.
    let (effects, effects_runner) = Effects::new(
        config_store.clone(),
        Arc::new(
            LiveSaveEffects::new(provider_cache.clone(), jobs.events_kick())
                .with_download_wake(Arc::new(move || download_worker.wake())),
        ),
    );
    if supervisor
        .one_shot("settings_effects", move |ctx| async move {
            effects_runner.run(ctx.token().clone()).await;
            Ok(())
        })
        .is_err()
    {
        tracing::warn!("the settings effects loop was already running");
    }
    let settings = SettingsSetup::build(
        config_store.clone(),
        effects,
        ids.clone(),
        http.shared().clone(),
        config.timezone.clone(),
        &outbound,
    )
    .with_section_prefs(
        Arc::new(SqliteSectionPrefsStore { db: db.clone() }),
        Arc::new(SqliteLinkStatus {
            pool: db.sqlx().clone(),
            lastfm: auth.users.lastfm.clone(),
        }),
    )
    .with_impact_buckets(Arc::new(SqliteImpactBuckets {
        pool: db.sqlx().clone(),
    }))
    .with_library_catalog(Arc::new(SqliteLibraryPolicyCatalog {
        pool: db.sqlx().clone(),
    }));
    // Activation dry runs save the settings through the same service as
    // the settings page, so the two never save over each other.
    if library
        .management_settings
        .set(settings.library_management_shared())
        .is_err()
    {
        tracing::warn!("library management settings service was already attached");
    }
    let plugins = PluginsSetup::build(
        plugin_host,
        http.shared().clone(),
        crypto,
        provider_cache,
        supervisor.clone(),
        db.clone(),
        outbound.plugin_hosts(),
        &outbound.listenbrainz,
    )
    .with_presence(Arc::new(media.playback.presence.clone()))
    .with_mix_grants(mix_hooks.clone(), mix_hooks);
    plugins.sync_ticks().await;

    // Attribution and scrobble drains: they deliver what is queued and end
    // on the stop, or once the app drops its queue handles.
    supervisor
        .one_shot(droppedneedle_media::MEDIA_WORKERS, move |ctx| async move {
            media_workers.run(ctx.token().clone()).await;
            Ok(())
        })
        .map_err(|error| stage("media workers")(error.to_string()))?;

    let grace = config.shutdown_grace;
    // The state attaches the hub to every publishing bundle (presence,
    // acquisition, library), so it is built before any loop starts: the
    // first wanted sweep, scan or presence poll already reaches the hub.
    let state = AppState::new(
        ids,
        http,
        config,
        auth,
        reads,
        providers,
        media,
        acquire,
        library,
        compat,
        admin,
        settings,
        jobs.clone(),
        plugins,
        concerts,
    )
    .with_events(events.clone());
    state
        .acquire
        .spawn_loops(&supervisor)
        .map_err(|error| stage("acquire loops")(error.to_string()))?;
    state
        .library
        .start_loops(&supervisor)
        .map_err(|error| stage("library loops")(error.to_string()))?;
    // `activity.changed` and `downloads.changed` for every open stream.
    droppedneedle_kernel::events::revisions::start(&supervisor, events.clone(), db.clone())
        .map_err(|error| stage("event loops")(error.to_string()))?;
    droppedneedle_kernel::events::downloads::start(&supervisor, events.clone(), db.clone())
        .map_err(|error| stage("event loops")(error.to_string()))?;
    jobs.spawn_loops().map_err(stage("jobs loops"))?;

    let router = create_app_with_web(state.clone(), web);
    Ok(Built {
        router,
        state,
        background: Background::new(grace, supervisor, events, runtime, data_lock),
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
    clock: Arc<dyn droppedneedle_kernel::clock::Clock>,
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

/// Stamp the shipped web UI into the cache, off the async workers.
async fn prepare_web(config: &AppConfig) -> Result<Option<WebUi>, BootError> {
    let template = config.static_dir.clone();
    let served = config.served_static_dir();
    let base_path = config.base_path.clone();
    let prepared =
        tokio::task::spawn_blocking(move || WebUi::prepare(&template, &served, &base_path))
            .await
            .map_err(|error| BootError::Stage {
                stage: "web UI",
                message: error.to_string(),
            })??;
    match &prepared {
        Some(web) => tracing::info!(root = %web.root().display(), "serving the web UI"),
        None => tracing::warn!(
            build = %config.static_dir.display(),
            "no web UI build found; serving the API only"
        ),
    }
    Ok(prepared)
}
