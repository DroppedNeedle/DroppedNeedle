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
pub mod platform;
mod serve;

use std::{fmt::Display, future::Future, sync::Arc, time::Duration};

use axum::Router;
use thiserror::Error;

pub use outbound::{Outbound, Port};
pub use platform::{Opened, open_platform};
pub use serve::{Background, ServeError, Stopped, bind, serve};

use data_dirs::BootRefusal;
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
use droppedneedle_acquire::{
    AcquireSetup,
    library_removal::AcquireAlbumCleanup,
    requests::discovery::DiscoveryBatchRequests,
    wiring::{AcquirePorts, AcquireStores},
};
use droppedneedle_auth::{
    federated::{MediaLinks, settings::InstallIds},
    prod::ProdAuth,
    wiring::AuthSetup,
};
use droppedneedle_catalog::reads::{
    ReadsInputs, ReadsPorts, ReadsSetup,
    catalog::{
        Catalog,
        library::LocalCatalog,
        plugin_sources::{PluginDetailsSource, PluginPurchaseLinks},
        upstream::{LiveCatalogSettings, Upstream},
    },
    collections::{
        CollectionsState, db::CollectionsDb, remote_import::PlaylistImportSink, store::Stores,
    },
    enrichment::{library::LiveEnrichment, live::production_enrichment},
    library::{lookups::SqliteLookups, sqlite::LibraryDb},
    platform::artwork::ArtworkService,
};
use droppedneedle_compat::{CompatSetup, setup::CompatDeps};
use droppedneedle_kernel::{
    config::AppConfig,
    db::{BOOT_WAIT, BackupService, datalock::DataLockError, error::DbError},
    http_client::HttpClientError,
    platform::Platform,
    runtime_config::{crypto::CryptoError, quota::QUOTA_FILE_NAME},
};
use droppedneedle_library::wiring::LibrarySetup;
use droppedneedle_media::{
    MediaSetup,
    remotes::{
        connections::{
            ConfigServers, ConnectionResolver, CredentialCoder, SignInLinks, SqliteConnectionStore,
        },
        plex::ConfiguredPlexServer,
        presence_feed::MediaJobs,
    },
    stream::local_files::LibraryFiles,
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
/// per-user connections table (the rows the remotes routes read) in the
/// platform's database.
pub fn sign_in_links(platform: &Platform) -> MediaLinks {
    MediaLinks::new(Arc::new(SignInLinks::new(Arc::new(
        ConnectionResolver::new(
            Arc::new(SqliteConnectionStore::new(platform.db.clone())),
            Arc::new(CredentialCoder::new(platform.crypto.clone())),
            Arc::new(ConfigServers::new(&platform.settings)),
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
    // The data lock, the key, the database, the supervisor, the settings
    // and the one event hub: every bundle below publishes into it from the
    // moment it is built, and the revision loops feed it after each commit.
    let Opened {
        platform,
        runtime,
        web,
        data_lock,
    } = open_platform(config).await?;
    let p = &platform;
    let (db, config_store, config, http) = (&p.db, &p.settings, &p.config, &p.http);
    let (crypto, supervisor, events) = (&p.crypto, &p.supervisor, &p.events);
    // The jobs table areas enqueue through; its kinds' runners start with
    // the other loops.
    let queues = jobs::QueueSetup::new(p);

    // The library roots' lease table: the publisher and plugin library
    // writes hold the same leases.
    let library_fs = droppedneedle_library::scan::fs::FsCoordinator::new();
    // The plugin host exists early so playback, acquisition and streaming
    // can hold it; no plugin starts until `PluginsSetup::build` loads it.
    let plugin_ticks = droppedneedle_plugins::wiring::tick_store(p);
    let plugin_host = droppedneedle_plugins::wiring::new_host(p, &plugin_ticks, library_fs.clone());
    let auth = AuthSetup::build_with_upstreams(
        p,
        ProdAuth::new(p),
        &outbound.sign_in(),
        Arc::new(ConfiguredPlexServer::new(
            http.shared().clone(),
            config_store.live(),
            InstallIds::new(config_store.clone()),
        )),
        sign_in_links(p),
    );

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
    let reads_inputs = ReadsInputs {
        covers_dir: config.cache_dir.join("covers"),
        cover_cache_max_bytes: config.cover_cache_max_bytes,
        http: http.shared().clone(),
        no_redirect: http.no_redirect().clone(),
        providers: providers.clone(),
        listenbrainz_links: Arc::new(SqliteListenBrainzLinkStore::new(db.clone(), crypto.clone())),
        youtube_quota_path: config.cache_dir.join(QUOTA_FILE_NAME),
        discover_warmer: config.discover_warmer,
        endpoints: endpoints.clone(),
    };
    // The artist and album pages, with plugin purchase links and details.
    // The artwork service names artist images through it, and the library
    // plans covers through that service.
    let catalog = Catalog::new(
        Upstream::new(
            http,
            providers.clone(),
            Arc::new(LiveCatalogSettings::new(config_store)),
            auth.users.clone(),
            endpoints,
        ),
        LocalCatalog::new(db.sqlx().clone()),
    )
    .with_purchase_links(Arc::new(PluginPurchaseLinks(plugin_host.clone())))
    .with_details(Arc::new(PluginDetailsSource(plugin_host.clone())));
    let artwork = droppedneedle_catalog::reads::artwork(p, &reads_inputs, catalog.clone());
    let collections_db = CollectionsDb::new(db.clone());
    let collections = Stores::new(&collections_db);
    let acquire_stores = AcquireStores::open(db);
    // Read live, so a source switch in Settings reaches the long-lived
    // clients without a restart.
    let mb_source = outbound.musicbrainz_source(config_store.live());
    let scans = jobs::ScansEnded::default();
    let library = LibrarySetup::build(
        p,
        providers.clone(),
        mb_source.clone(),
        queues.queue().clone(),
        outbound.library(),
        library_ports(
            p,
            &outbound,
            &providers,
            &auth,
            &artwork,
            &scans,
            library_fs,
            &acquire_stores,
        )?,
    )
    .map_err(stage("library setup"))?;
    let acquire = AcquireSetup::build(
        p,
        auth.users.clone(),
        acquire_stores,
        acquire_ports(
            p,
            &library,
            &providers,
            mb_source,
            &plugin_host,
            &collections,
            &outbound,
        ),
        &outbound.spotify(),
    )
    .map_err(stage("acquire setup"))?;
    let reads = ReadsSetup::build(
        p,
        auth.users.clone(),
        reads_inputs,
        Some(enrichment),
        ReadsPorts {
            collections: CollectionsState::wired(
                collections_db,
                collections,
                acquire.pending_approvals(),
                acquire.approval_seeds(),
            ),
            catalog,
            artwork,
            library_files: LibraryFiles::new(db.sqlx().clone(), library.root_source()),
            // Discovery batches file their albums through the requests
            // service.
            batch_requests: Arc::new(DiscoveryBatchRequests::new(
                acquire.requests.clone(),
                Some(library.clone()),
            )),
        },
    );
    let (media, media_workers) = MediaSetup::build(
        p,
        auth.users.clone(),
        library.root_source(),
        // Remote playlist imports land in the user's playlists.
        Arc::new(PlaylistImportSink::new(reads.collections.clone())),
        plugin_host.clone(),
        outbound.plex_tv.clone(),
        outbound.scrobbles(),
    )
    .map_err(stage("media setup"))?;
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
        p,
        auth.users.clone(),
        media.playback.clone(),
        media.stream.engine.clone(),
        library.clone(),
        &reads,
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
    let concerts =
        droppedneedle_catalog::concerts::ConcertsSetup::from_platform(p, outbound.concerts());
    // Your Weekly Mix: one builder behind the refresh route, the daily
    // loop and the scrobble-settings toggle.
    let mix_hooks = Arc::new(droppedneedle_acquire::requests::mix::MixGrantHooks::new(
        acquire.requests.mixer.clone(),
    ));
    let jobs = JobsSetup::build(
        p,
        runtime.checkpoint().clone(),
        MediaJobs {
            presence: media.playback.presence.clone(),
            resolver: Some(media.remotes.service.resolver().clone()),
            http: http.shared().clone(),
            pool: Some(db.sqlx().clone()),
        },
        concerts.sweep(),
        scans.clone(),
    )
    .with_queue(queues)
    .with_job(
        droppedneedle_library::contrib::worker::SPEC,
        library.contrib_worker.handler(),
    )
    .with_mixer(mix_hooks.clone())
    .with_precache_sources_from(&reads, config_store)
    .with_discovery_demand(Arc::new(
        droppedneedle_catalog::reads::discover::demand::ContentDemand(
            reads.discover.content.clone(),
        ),
    ));
    let admin = match &reads.artwork {
        Some(artwork) => admin.with_covers(artwork.cache().clone()),
        None => admin,
    }
    .with_precache(jobs.precache_trigger());
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
    let scans = Arc::new(SqliteLookups::new(&LibraryDb::new(db.sqlx())));
    let settings = SettingsSetup::build(
        p,
        effects,
        &outbound,
        library.management_settings.clone(),
        scans,
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
    let plugins = PluginsSetup::build(
        p,
        plugin_host,
        plugin_ticks,
        provider_cache,
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
    // Every publishing bundle took the hub when it was built, so what the
    // loops started above (plugin sync, settings effects, media drains)
    // and the ones started below publish reaches open streams.
    let state = AppState::new(
        p.ids.clone(),
        config.clone(),
        auth,
        reads,
        media,
        acquire,
        library,
        compat,
        admin,
        settings,
        jobs.clone(),
        plugins,
        concerts,
        events.clone(),
    );
    state
        .acquire
        .spawn_loops(supervisor)
        .map_err(|error| stage("acquire loops")(error.to_string()))?;
    state
        .library
        .start_loops(supervisor)
        .map_err(|error| stage("library loops")(error.to_string()))?;
    // `activity.changed` and `downloads.changed` for every open stream.
    droppedneedle_kernel::events::revisions::start(supervisor, events.clone(), db.clone())
        .map_err(|error| stage("event loops")(error.to_string()))?;
    droppedneedle_kernel::events::downloads::start(supervisor, events.clone(), db.clone())
        .map_err(|error| stage("event loops")(error.to_string()))?;
    jobs.spawn_loops().map_err(stage("jobs loops"))?;

    let router = create_app_with_web(state.clone(), web);
    Ok(Built {
        router,
        state,
        background: Background::new(
            grace,
            supervisor.clone(),
            events.clone(),
            runtime,
            data_lock,
        ),
    })
}

/// What the library calls outside itself. Removing an album clears its
/// download and wanted state. Completed scans refresh the library's
/// images, as v2 did after a sync. Library Management plans genres, lyrics
/// and covers through the same clients and artwork cache the pages use, and
/// AcoustID ids on identification's shared rate limit.
#[allow(clippy::too_many_arguments)]
fn library_ports(
    p: &Platform,
    outbound: &Outbound,
    providers: &Arc<Providers>,
    auth: &AuthSetup,
    artwork: &ArtworkService,
    scans: &jobs::ScansEnded,
    fs: droppedneedle_library::scan::fs::FsCoordinator,
    acquire: &AcquireStores,
) -> Result<droppedneedle_library::wiring::LibraryPorts, BootError> {
    let acoustid_pacer =
        droppedneedle_providers::adapters::CorePacer::for_source(providers.clone(), "acoustid")
            .ok_or_else(|| "acoustid has no verified rate row".to_owned())
            .map_err(stage("library management"))?;
    let enrichment = LiveEnrichment {
        acoustid: droppedneedle_providers::acoustid::AcoustIdClient::new(
            p.http.shared().clone(),
            &outbound.acoustid,
            acoustid_pacer,
            droppedneedle_providers::adapters::HealthSink::new(providers),
        ),
        library: p.settings.live(),
        upstream: Upstream::new(
            &p.http,
            providers.clone(),
            Arc::new(LiveCatalogSettings::new(&p.settings)),
            auth.users.clone(),
            outbound.catalog(),
        ),
        http: p.http.shared().clone(),
        artwork: Some(artwork.clone()),
        lrclib: Default::default(),
    };
    let scans = scans.clone();
    Ok(droppedneedle_library::wiring::LibraryPorts {
        removal: Arc::new(AcquireAlbumCleanup::new(acquire)),
        enrichment: Some(Arc::new(enrichment)),
        scan_completed: Arc::new(move || scans.bump()),
        fs,
    })
}

/// What acquisition calls outside itself. Finished downloads land through
/// the library's import seam, and "get missing tracks" asks the library
/// which tracks the chosen edition lacks. Release polls and album lookups
/// pace through the shared limiters. Plugins act as download sources, feed
/// usenet and hear acquisition events.
fn acquire_ports(
    p: &Platform,
    library: &LibrarySetup,
    providers: &Arc<Providers>,
    mb_source: droppedneedle_providers::musicbrainz::LiveSource,
    plugin_host: &Arc<droppedneedle_plugins::host::PluginHost>,
    collections: &Stores,
    outbound: &Outbound,
) -> AcquirePorts {
    AcquirePorts {
        library: Arc::new(
            droppedneedle_acquire::landing::library::LibraryLanding::new(
                library.clone(),
                library.releases.clone(),
                p.db.sqlx().clone(),
            ),
        ),
        edition_plans: Arc::new(droppedneedle_library::operations::service::Operations::new(
            library,
        )),
        album_lookup: Arc::new(droppedneedle_acquire::target::lookup::live_albums(
            &p.http,
            providers.clone(),
            mb_source.clone(),
        )),
        release_poll: Arc::new(droppedneedle_acquire::search::live_release_poll(
            &p.http,
            providers.clone(),
            mb_source,
        )),
        plugins: plugin_host.clone(),
        collections: collections.clone(),
        providers: providers.clone(),
        listenbrainz: outbound.listenbrainz.clone(),
    }
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
