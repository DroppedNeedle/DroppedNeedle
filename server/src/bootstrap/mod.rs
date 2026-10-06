//! Composition root: one call from deployment config to a router and the
//! background work behind it.
//!
//! [`build`] opens the database (taking a verified backup first when the
//! schema is about to change), stamps the web UI, wires every bundle,
//! runs startup recovery, and starts the background loops. [`serve`]
//! (in `serve.rs`) owns the listener and the shutdown order. `main` only
//! parses arguments, tunes the allocator, starts the runtime and hands in
//! the stop signal.

mod serve;

use std::sync::Arc;

use axum::Router;
use thiserror::Error;

pub use serve::{Background, ServeError, bind, serve};

use crate::{
    AppConfig, AppState,
    acquire::AcquireSetup,
    admin::{AdminDb, AdminSetup, quota::reload_overrides},
    app::create_app_with_web,
    auth::{prod::ProdAuth, users::stores::SystemClock, wiring::AuthSetup},
    compat::{CompatSetup, settings::LiveSettings, setup::CompatDeps},
    db::{BackupService, DbConfig, error::DbError, open_runtime},
    http_client::{HttpClientError, HttpClientFactory},
    ids::UuidGenerator,
    jobs::{media::MediaJobs, wiring::JobsSetup},
    library::wiring::LibrarySetup,
    media::MediaSetup,
    plugins::wiring::PluginsSetup,
    providers::{InMemoryProviderCache, Providers, adapters::production_enrichment},
    reads::{
        ReadsSetup,
        catalog::{Catalog, library::LocalCatalog, upstream::Upstream},
        collections::db::CollectionsDb,
        platform::wrapped::ConfigWrappedKey,
    },
    remotes::adapter::PlaylistImportSink,
    runtime_config::{
        ConfigStore, Crypto,
        crypto::CryptoError,
        secret_sections::ListenBrainzConnection,
        sections::{ConnectApps, LyricsSettings},
    },
    settings::{
        effects::{LiveSaveEffects, SaveEffects},
        section_prefs::{SqliteLinkStatus, SqliteSectionPrefsStore},
        services::SqliteImpactBuckets,
        wiring::SettingsSetup,
    },
    tooling::datalock::{DataLock, DataLockError},
    web::{WebError, WebUi},
};

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
    /// The settings file could not be opened.
    #[error("settings: {0}")]
    Settings(#[from] crate::runtime_config::ConfigError),
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

/// Build the production graph and start its background work.
pub async fn build(config: AppConfig) -> Result<(Router, Background), BootError> {
    let http = HttpClientFactory::with_settings(&config.http)?;
    // Held until shutdown, and taken before the database opens (so before
    // the pre-upgrade backup and the migrations): the offline import and
    // restore refuse to run while it is held, and a running one keeps the
    // server from starting.
    let data_lock = DataLock::shared(&config.library_db_path)?;
    let runtime = open_runtime(&DbConfig::new(&config.library_db_path)).await?;
    let web = prepare_web(&config).await?;

    let crypto = Arc::new(Crypto::load_or_generate(&config.config_dir())?);
    // The store owns its own key handle; the file exists after the line above.
    let config_store = Arc::new(ConfigStore::open(
        &config.config_file,
        Crypto::load(&config.config_dir())?,
    )?);
    let ids = Arc::new(UuidGenerator);
    let clock = Arc::new(SystemClock);
    // The plugin host exists early so playback, acquisition and streaming
    // can hold it; no plugin starts until `PluginsSetup::build` loads it.
    let plugin_host = crate::plugins::wiring::new_host(config.plugins_dir(), config_store.clone());
    let auth_bundle = ProdAuth::new(
        runtime.pool(),
        runtime.lane(),
        crypto.clone(),
        ids.clone(),
        clock.clone(),
        &config.cache_dir,
    );
    let auth = AuthSetup::build(
        auth_bundle,
        config_store.clone(),
        crypto.clone(),
        &http,
        ids.clone(),
        clock,
        &config.base_path,
    )?
    .with_trusted_proxies(config.trusted_proxies.clone());

    // Shared provider deps first: the reads enrichment pair paces through
    // these same limiters, so production holds one limiter set. The byte
    // cache stays shared with the admin stats and clear routes.
    let provider_cache = Arc::new(InMemoryProviderCache::new());
    let providers = Arc::new(Providers::new(provider_cache.clone()));
    // Read per call so turning ListenBrainz on or off takes effect at once.
    let listenbrainz_store = config_store.clone();
    let listenbrainz_enabled: crate::providers::adapters::Switch = Arc::new(move || {
        listenbrainz_store
            .get_raw::<ListenBrainzConnection>()
            .map(|settings| settings.enabled)
            .unwrap_or_else(|error| {
                tracing::warn!(%error, "cannot read listenbrainz settings; popularity enrichment off");
                false
            })
    });
    // Read per call so turning lyrics on or off takes effect at once.
    let lyrics_store = config_store.clone();
    let lyrics_enabled: crate::providers::adapters::Switch = Arc::new(move || {
        lyrics_store
            .get::<LyricsSettings>()
            .map(|settings| settings.enabled)
            .unwrap_or_else(|error| {
                tracing::warn!(%error, "cannot read lyrics settings; lyrics enrichment off");
                false
            })
    });
    let enrichment = production_enrichment(
        http.shared(),
        &providers,
        listenbrainz_enabled,
        lyrics_enabled,
    );
    let mut reads = ReadsSetup::build(
        runtime.pool(),
        auth.users.clone(),
        ids.clone(),
        ConfigWrappedKey::new(config_store.clone()),
        Some(enrichment),
    )
    .with_collections(CollectionsDb::new(
        runtime.pool().clone(),
        runtime.lane().clone(),
    ))
    .with_catalog(Catalog::new(
        Upstream::new(
            &http,
            providers.clone(),
            config_store.clone(),
            auth.users.clone(),
        ),
        LocalCatalog::new(runtime.pool().clone()),
    ));
    let connect_apps = match config_store.get::<ConnectApps>() {
        Ok(settings) => settings,
        Err(error) => {
            tracing::warn!(%error, "cannot read connect_apps settings; transcode defaults apply");
            ConnectApps::default()
        }
    };
    let mb_settings = config_store.clone();
    let library = LibrarySetup::build(
        auth.users.clone(),
        &http,
        ids.clone(),
        providers.clone(),
        Arc::new(move || {
            crate::providers::musicbrainz::MbSource::from_settings(
                &crate::reads::catalog::upstream::CatalogSettings::musicbrainz(
                    mb_settings.as_ref(),
                ),
            )
        }),
        &config.library_db_path,
    )
    .map_err(stage("library setup"))?;
    let (media, media_workers) = MediaSetup::build(
        &config.library_db_path,
        &config,
        auth.users.clone(),
        crypto.clone(),
        http.shared().clone(),
        ids.clone(),
        connect_apps.clone(),
        Some(library.root_source()),
        runtime.pool().clone(),
        runtime.lane().clone(),
        config_store.clone(),
        // Remote playlist imports land in the user's playlists.
        Arc::new(PlaylistImportSink::new(reads.collections.clone())),
    )
    .map_err(stage("media setup"))?;
    let media = media.with_play_events(Arc::new(
        crate::plugins::capabilities::events::PluginPlayEvents(plugin_host.clone()),
    ));
    media
        .stream
        .engine
        .attach_plugins(plugin_host.clone(), http.direct_no_redirect().clone());
    let acquire = AcquireSetup::build(
        crate::acquire::db::AcquireDb::from_runtime(&runtime),
        &config,
        auth.users.clone(),
        &http,
        ids.clone(),
        config_store.clone(),
        &mut reads.collections,
    )
    .map_err(stage("acquire setup"))?
    .with_plugins(plugin_host.clone());
    acquire.refresh_admins().await;
    // Startup recovery before serving traffic. Re-running after a clean
    // shutdown is a no-op: nothing destructive repeats.
    let recovery = acquire
        .run_recovery(runtime.wakeups(), runtime.lane())
        .await
        .map_err(stage("acquire recovery"))?;
    tracing::info!(
        redispatched = recovery.redispatched,
        resumed = recovery.resumed,
        restarted = recovery.restarted,
        "acquire recovery complete"
    );
    // Publish journal reconciliation and contribution lease recovery; scan
    // recovery runs when the scan supervisor starts.
    let library_recovery = library
        .run_recovery()
        .await
        .map_err(stage("library recovery"))?;
    tracing::info!(
        publish_bundles = library_recovery.publish_recoveries.len(),
        contrib_recovered = library_recovery.contrib_recovered,
        "library recovery complete"
    );
    let compat = CompatSetup::build(CompatDeps::over_reads(
        auth.users.clone(),
        crypto.clone(),
        media.playback.clone(),
        media.stream.engine.clone(),
        library.clone(),
        &reads,
        LiveSettings::from_config(config_store.clone()),
    ))
    .with_trusted_proxies(config.trusted_proxies.clone());
    let admin = AdminSetup::new(
        auth.users.clone(),
        acquire.requests.quota.clone(),
        provider_cache.clone(),
        providers.clone(),
    )
    .with_db(AdminDb::new(runtime.pool().clone(), runtime.lane().clone()))
    .with_backups(BackupService::new(
        &config.library_db_path,
        runtime.backups().backup_dir(),
    ))
    .with_checkpoint(runtime.checkpoint().clone());
    // Durable quota overrides back into the live ledger. A failure warns
    // instead of stopping the boot; the admin can re-save.
    match reload_overrides(runtime.pool(), &acquire.requests.quota).await {
        Ok(loaded) => tracing::info!(loaded, "quota overrides reloaded"),
        Err(error) => tracing::warn!(%error, "quota overrides failed to reload; defaults apply"),
    }
    // The one registry every background job registers on.
    let jobs = JobsSetup::build(
        auth.users.clone(),
        runtime.wakeups().clone(),
        runtime.lane().clone(),
        runtime.checkpoint().clone(),
        config_store.clone(),
        MediaJobs {
            presence: media.playback.presence.clone(),
            resolver: Some(media.remotes.service.resolver().clone()),
            http: http.shared().clone(),
            pool: Some(runtime.pool().clone()),
        },
    );
    let admin = admin.with_precache(jobs.precache_trigger());
    let effects: Arc<dyn SaveEffects> = Arc::new(LiveSaveEffects::new(
        provider_cache.clone(),
        jobs.events_kick(),
    ));
    let settings = SettingsSetup::build(
        config_store.clone(),
        effects,
        ids.clone(),
        auth.users.clone(),
        http.shared().clone(),
        config.timezone.clone(),
    )
    .with_section_prefs(
        Arc::new(SqliteSectionPrefsStore {
            pool: runtime.pool().clone(),
            lane: Arc::new(runtime.lane().clone()),
        }),
        Arc::new(SqliteLinkStatus {
            pool: runtime.pool().clone(),
            lastfm: auth.users.lastfm.clone(),
        }),
    )
    .with_impact_buckets(Arc::new(SqliteImpactBuckets {
        pool: runtime.pool().clone(),
    }));
    let plugins = PluginsSetup::build(
        plugin_host,
        auth.users.clone(),
        http.shared().clone(),
        ids.clone(),
        crypto,
        provider_cache,
        jobs.registry().clone(),
        runtime.pool().clone(),
        runtime.lane().clone(),
    )
    .with_presence(Arc::new(media.playback.presence.clone()));
    plugins.sync_ticks().await;

    let mut background = Background::new(config.shutdown_grace);
    let stop = background.stop_signal();
    // Plugin processes get `shutdown`, then a kill, when the server stops.
    let plugin_host = plugins.host().clone();
    let mut plugin_stop = background.stop_signal();
    background.push(
        "plugins",
        tokio::spawn(async move {
            let _ = plugin_stop.wait_for(|stopping| *stopping).await;
            plugin_host.stop_all().await;
        }),
    );
    // Attribution and scrobble drains: exit once the app drops its queue handles.
    background.push(
        "media-workers",
        tokio::spawn(async move { media_workers.run().await }),
    );
    let acquire_loops = acquire
        .spawn_loops(
            runtime.wakeups().clone(),
            runtime.lane().clone(),
            stop.clone(),
        )
        .await
        .map_err(stage("acquire loops"))?;
    background.extend(acquire_loops);
    background.extend(library.spawn_loops(stop));
    // Registry jobs stop through the registry at shutdown, not the watch.
    jobs.spawn_loops().await.map_err(stage("jobs loops"))?;

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
    );
    let router = create_app_with_web(state, web);
    Ok((
        router,
        background
            .with_jobs(jobs)
            .with_runtime(runtime)
            .with_data_lock(data_lock),
    ))
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
