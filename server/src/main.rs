//! DroppedNeedle v3 server binary: one process, graceful SIGTERM shutdown.

#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;

use droppedneedle::{
    AppConfig, AppState,
    admin::{AdminDb, AdminSetup, backups::ensure_pre_upgrade_backup, quota::reload_overrides},
    auth::{prod::ProdAuth, users::stores::SystemClock, wiring::AuthSetup},
    compat::CompatSetup,
    create_app,
    db::{BackupService, DbConfig, open_runtime},
    docs::ApiDoc,
    http_client::HttpClientFactory,
    ids::UuidGenerator,
    jobs::wiring::{JobsSetup, SHUTDOWN_GRACE},
    media::MediaSetup,
    observability::init_tracing,
    plugins::wiring::PluginsSetup,
    providers::{Providers, adapters::production_enrichment},
    reads::ReadsSetup,
    runtime_config::{
        ConfigStore, Crypto,
        secret_sections::{ListenBrainzConnection, WrappedSettings},
        sections::{ConnectApps, LyricsSettings},
    },
    schema::apply_migrations,
    settings::{
        effects::{LiveSaveEffects, SaveEffects},
        section_prefs::{SqliteLinkStatus, SqliteSectionPrefsStore},
        services::SqliteImpactBuckets,
        wiring::SettingsSetup,
    },
};
use utoipa::OpenApi as _;

/// `--print-openapi` dumps the contract document to stdout for the
/// TypeScript pipeline and CI drift gate.
const PRINT_OPENAPI_ARG: &str = "--print-openapi";

/// `--tooling-routes` mounts the dev-only tooling routes (covers-debug).
/// Debug builds only: a release binary rejects it as unknown.
const TOOLING_ROUTES_ARG: &str = "--tooling-routes";

/// Bound glibc malloc arenas and trim retained heap promptly.
///
/// Measured: the default 96-arena layout held ~100 MB of
/// post-scan fragment freelists (8 arenas per core) with wide run-to-run
/// swings. One arena plus a 128 KiB trim threshold coalesces the scan and
/// read churn onto a single heap that trims at idle: post-100k-workload
/// RSS falls from ~265 MB to ~111 MB with no scan-throughput or read-
/// latency regression (mimalloc+override was trialled and reverted: +20 MB
/// boot overhead and worse bench retention on this workload). Runs first
/// so the cap lands before worker threads allocate; a non-glibc platform
/// reports failure and keeps running on defaults.
fn tune_allocator() {
    // Safety: mallopt only adjusts global tuning knobs; the parameters
    // are valid and failure is a non-fatal 0 return.
    let arena_result = unsafe { libc::mallopt(libc::M_ARENA_MAX, 1) };
    let trim_result = unsafe { libc::mallopt(libc::M_TRIM_THRESHOLD, 128 * 1024) };
    if arena_result == 0 || trim_result == 0 {
        tracing::debug!("allocator tuning not applied; running on malloc defaults");
    }
}

/// True for the arguments this binary accepts. The tooling flag only
/// exists in debug builds.
fn is_known_arg(arg: &str) -> bool {
    if arg == PRINT_OPENAPI_ARG {
        return true;
    }
    #[cfg(debug_assertions)]
    if arg == TOOLING_ROUTES_ARG {
        return true;
    }
    false
}

fn main() {
    tune_allocator();
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|arg| arg == PRINT_OPENAPI_ARG) {
        print_openapi();
        return;
    }
    if let Some(unknown) = args.iter().find(|arg| !is_known_arg(arg)) {
        eprintln!("unknown argument {unknown:?}: expected {PRINT_OPENAPI_ARG}");
        std::process::exit(2);
    }
    #[cfg(debug_assertions)]
    let tooling_routes = args.iter().any(|arg| arg == TOOLING_ROUTES_ARG);
    #[cfg(not(debug_assertions))]
    let tooling_routes = false;

    init_tracing();
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("cannot start async runtime: {error}");
            std::process::exit(1);
        }
    };
    let outcome = runtime.block_on(serve(tooling_routes));
    if let Err(message) = outcome {
        eprintln!("{message}");
        std::process::exit(1);
    }
}

/// Print the OpenAPI document as JSON. Logging stays on stderr.
fn print_openapi() {
    match serde_json::to_string_pretty(&ApiDoc::openapi()) {
        Ok(document) => println!("{document}"),
        Err(error) => {
            eprintln!("cannot render OpenAPI document: {error}");
            std::process::exit(1);
        }
    }
}

/// Boot state, bind, and serve until SIGTERM or Ctrl-C.
async fn serve(tooling_routes: bool) -> Result<(), String> {
    let mut config = AppConfig::load().map_err(|error| error.to_string())?;
    #[cfg(debug_assertions)]
    {
        config.debug_cors = true;
        config.tooling_routes = tooling_routes;
    }
    #[cfg(not(debug_assertions))]
    let _ = tooling_routes;
    let http = HttpClientFactory::new().map_err(|error| error.to_string())?;
    let runtime = open_runtime(&DbConfig::new(&config.library_db_path))
        .await
        .map_err(|error| error.to_string())?;
    // Pre-upgrade safety net: a verified backup before any schema change.
    // A failure here is fatal: migrating without one risks the catalog.
    ensure_pre_upgrade_backup(
        &config.library_db_path,
        &config.root_app_dir.join("backups"),
    )
    .await
    .map_err(|error| format!("pre-upgrade backup failed: {error}"))?;
    apply_migrations(runtime.pool())
        .await
        .map_err(|error| error.to_string())?;
    // Two handles over one key file: the store owns its copy outright, so
    // the adapters load a second handle rather than sharing state.
    let crypto = Arc::new(
        Crypto::load_or_generate(&config.config_dir()).map_err(|error| error.to_string())?,
    );
    let config_store = Arc::new(
        ConfigStore::open(
            &config.config_file,
            Crypto::load_or_generate(&config.config_dir()).map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string())?,
    );
    let ids = Arc::new(UuidGenerator);
    let clock = Arc::new(SystemClock);
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
        http.shared().clone(),
        ids.clone(),
        clock,
        &config.base_path,
    )
    .map_err(|error| error.to_string())?;
    let wrapped_api_key = config_store
        .get_raw::<WrappedSettings>()
        .map(|settings| settings.api_key.expose().to_owned())
        .map_err(|error| error.to_string())?;
    // Shared provider deps first: the reads enrichment pair paces through
    // these same limiters, so production holds one limiter set, not two.
    // The byte cache stays shared with the admin UX (stats/clear observe
    // the same entries the clients read).
    let provider_cache = Arc::new(droppedneedle::providers::InMemoryProviderCache::new());
    let providers = Arc::new(Providers::new(provider_cache.clone()));
    let listenbrainz_enabled = match config_store.get_raw::<ListenBrainzConnection>() {
        Ok(settings) => settings.enabled,
        Err(error) => {
            tracing::warn!(%error, "cannot read listenbrainz settings; popularity enrichment disabled");
            false
        }
    };
    let lyrics_enabled = match config_store.get::<LyricsSettings>() {
        Ok(settings) => settings.enabled,
        Err(error) => {
            tracing::warn!(%error, "cannot read lyrics settings; lyrics enrichment disabled");
            false
        }
    };
    let enrichment = production_enrichment(
        http.shared(),
        &providers,
        listenbrainz_enabled,
        lyrics_enabled,
    );
    let reads = ReadsSetup::build(
        runtime.pool(),
        auth.users.clone(),
        ids.clone(),
        wrapped_api_key,
        Some(enrichment),
    );
    let connect_apps = match config_store.get::<ConnectApps>() {
        Ok(settings) => settings,
        Err(error) => {
            tracing::warn!(%error, "cannot read connect_apps settings; transcode defaults apply");
            ConnectApps::default()
        }
    };
    let library = droppedneedle::library::wiring::LibrarySetup::build(
        auth.users.clone(),
        http.shared().clone(),
        ids.clone(),
        providers.clone(),
        &config.library_db_path,
    )
    .map_err(|error| format!("library setup: {error}"))?;
    let compat_crypto = crypto.clone();
    let plugins_crypto = crypto.clone();
    let (media, report_worker) = MediaSetup::build(
        &config.library_db_path,
        &config,
        auth.users.clone(),
        crypto,
        http.shared().clone(),
        ids.clone(),
        connect_apps.clone(),
        Some(library.root_source()),
    )
    .map_err(|error| error.to_string())?;
    let mut reads = reads;
    let acquire = droppedneedle::acquire::AcquireSetup::build(
        &config.library_db_path,
        &config,
        auth.users.clone(),
        http.shared().clone(),
        ids.clone(),
        config_store.clone(),
        &mut reads.collections,
    )
    .map_err(|error| format!("acquire setup: {error}"))?;
    acquire.refresh_admins().await;
    // Startup recovery before serving traffic: journal classification
    // plus durable-op registration. Re-running after a clean shutdown
    // is a no-op (nothing destructive repeats).
    let recovery = acquire
        .run_recovery(runtime.wakeups(), runtime.lane())
        .await
        .map_err(|error| format!("acquire recovery: {error}"))?;
    tracing::info!(
        redispatched = recovery.redispatched,
        resumed = recovery.resumed,
        restarted = recovery.restarted,
        "acquire recovery complete"
    );
    // Library recovery before serving traffic: publish journal
    // reconciliation (resume-or-compensate) plus contribution lease
    // recovery. Scan recovery runs in the supervisor preamble.
    // Re-running after a clean shutdown is a no-op.
    let library_recovery = library
        .run_recovery()
        .await
        .map_err(|error| format!("library recovery: {error}"))?;
    tracing::info!(
        publish_bundles = library_recovery.publish_recoveries.len(),
        contrib_recovered = library_recovery.contrib_recovered,
        "library recovery complete"
    );
    // Compat shims: production auth/playback/engine bindings. No new
    // loops: everything runs inline in the request path.
    let compat = CompatSetup::build(
        auth.users.clone(),
        compat_crypto,
        media.playback.clone(),
        media.stream.engine.clone(),
        library.clone(),
        &connect_apps,
    );
    let admin = AdminSetup::new(
        auth.users.clone(),
        acquire.requests.quota.clone(),
        provider_cache.clone(),
        providers.clone(),
    )
    .with_db(AdminDb::new(runtime.pool().clone(), runtime.lane().clone()))
    .with_backups(BackupService::new(
        &config.library_db_path,
        &config.root_app_dir.join("backups"),
    ))
    .with_checkpoint(runtime.checkpoint().clone());
    // Durable quota overrides back into the live ledger. A failure here
    // warns instead of bricking the boot; the admin can re-save.
    match reload_overrides(runtime.pool(), &acquire.requests.quota).await {
        Ok(loaded) => tracing::info!(loaded, "quota overrides reloaded"),
        Err(error) => tracing::warn!(%error, "quota overrides failed to reload; defaults apply"),
    }
    // Jobs: the one registry every background loop registers on.
    let jobs = JobsSetup::build(
        auth.users.clone(),
        runtime.wakeups().clone(),
        runtime.lane().clone(),
        runtime.checkpoint().clone(),
        config_store.clone(),
    );
    // The precache trigger rides the same registry; the admin route is its
    // only production caller.
    let admin = admin.with_precache(jobs.precache_trigger());
    // Settings: the section service with its save fan-out (provider-cache
    // invalidation over the shared cache plus the jobs-owned events kick),
    // the admin-gated HTTP surface, the per-user section prefs, and the
    // policy-impact buckets.
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
    // Plugins: host, routes, and scrobble backend over the shared
    // jobs registry (one durable mechanism, no duplicate tick loops).
    let plugins = PluginsSetup::build(
        auth.users.clone(),
        http.shared().clone(),
        config_store.clone(),
        ids.clone(),
        plugins_crypto,
        config.root_app_dir.join("plugins"),
        provider_cache.clone(),
        jobs.registry().clone(),
        runtime.pool().clone(),
        runtime.lane().clone(),
    );
    plugins.sync_ticks().await;
    let state = AppState::new(
        ids,
        http,
        config.clone(),
        auth,
        reads,
        providers,
        media,
        acquire.clone(),
        library.clone(),
        compat,
        admin,
        settings,
        jobs.clone(),
        plugins,
    );
    let app = create_app(state);

    // Discover and home refresh loops: they sleep on their intervals behind
    // one shutdown watch. The loop bodies are provider-cache rebuild hooks;
    // no rebuildable provider cache exists yet, so each tick is a guarded
    // no-op. Intervals, single-flight, shutdown and await are live.
    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
    let refresh_registry =
        Arc::new(droppedneedle::reads::discover::refresh::RefreshRegistry::new());
    let discover_loop = {
        let registry = refresh_registry.clone();
        let sleeper =
            droppedneedle::reads::discover::refresh::TokioSleeper::new(shutdown_rx.clone());
        tokio::spawn(async move {
            droppedneedle::reads::discover::refresh::run_refresh_loop(
                registry,
                sleeper,
                droppedneedle::reads::discover::refresh::RefreshScope::Discover,
                || async { Ok::<(), String>(()) },
            )
            .await;
        })
    };
    let home_loop = {
        let registry = refresh_registry.clone();
        let sleeper =
            droppedneedle::reads::discover::refresh::TokioSleeper::new(shutdown_rx.clone());
        tokio::spawn(async move {
            droppedneedle::reads::discover::refresh::run_refresh_loop(
                registry,
                sleeper,
                droppedneedle::reads::discover::refresh::RefreshScope::Home,
                || async { Ok::<(), String>(()) },
            )
            .await;
        })
    };

    // MBID warmup loops (Jellyfin one-shot, Navidrome/Plex every 4h) over
    // the same shutdown watch. The loop bodies are provider index rebuild
    // hooks; no rebuildable index exists yet, so each tick is a guarded
    // no-op. Cadences, single-flight, shutdown and await are live.
    let warmup = {
        use droppedneedle::playback::{TokioSleeper, WarmupStats, spawn_warmup_loops};

        let sleeper = TokioSleeper::new(shutdown_rx.clone());
        spawn_warmup_loops(
            sleeper,
            || {
                Box::pin(async {
                    Ok::<_, String>(WarmupStats {
                        scope: "jellyfin",
                        warmed: 0,
                        pruned: 0,
                    })
                })
            },
            || {
                Box::pin(async {
                    Ok::<_, String>(WarmupStats {
                        scope: "navidrome",
                        warmed: 0,
                        pruned: 0,
                    })
                })
            },
            || {
                Box::pin(async {
                    Ok::<_, String>(WarmupStats {
                        scope: "plex",
                        warmed: 0,
                        pruned: 0,
                    })
                })
            },
        )
    };

    // Attribution drain: remote session reports queued by playback
    // handlers. The worker exits once the app drops its queue handles.
    let report_loop = tokio::spawn(async move {
        report_worker.run().await;
    });

    // Acquisition loops: the four flows loops, the download
    // worker, and the probe refresh loop over the same shutdown watch.
    let acquire_loops = acquire
        .spawn_loops(
            runtime.wakeups().clone(),
            runtime.lane().clone(),
            shutdown_rx.clone(),
        )
        .await
        .map_err(|error| format!("acquire loops: {error}"))?;

    // Library loops: scan supervisor, filesystem watcher,
    // identify queue, contribution verifier, publish maintenance.
    let library_loops = library.spawn_loops(shutdown_rx.clone());

    // Jobs loops: checkpoint, presence, personal-mix, playlist
    // sync, and the events watcher on the shared registry. Cancellation
    // runs through the registry at shutdown (below), not the watch.
    jobs.spawn_loops()
        .await
        .map_err(|error| format!("jobs loops: {error}"))?;

    let address = SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), config.port);
    let listener = tokio::net::TcpListener::bind(address)
        .await
        .map_err(|error| format!("cannot bind {address}: {error}"))?;
    tracing::info!(%address, "listening");
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .map_err(|error| format!("server fault: {error}"))?;
    let _ = shutdown_tx.send(true);
    // The registry-owned loops (boot loops, the kick, precache runs, and
    // plugin ticks) stop through their stop signals, each with the same
    // grace, before the runtime and its writer lane shut down.
    jobs.cancel_all(SHUTDOWN_GRACE).await;
    let mut loops = vec![("discover", discover_loop), ("home", home_loop)];
    for (scope, task) in ["warmup-jellyfin", "warmup-navidrome", "warmup-plex"]
        .into_iter()
        .zip(warmup.tasks)
    {
        loops.push((scope, task));
    }
    loops.push(("report-worker", report_loop));
    for (scope, handle) in acquire_loops {
        loops.push((scope, handle));
    }
    for (scope, handle) in library_loops {
        loops.push((scope, handle));
    }
    for (scope, handle) in loops {
        if let Err(error) = handle.await {
            tracing::warn!(scope, %error, "background loop ended early");
        }
    }
    runtime.shutdown().await;
    tracing::info!("shutdown complete");
    Ok(())
}

/// Resolve when the process should stop: SIGTERM or Ctrl-C.
async fn shutdown_signal() {
    let ctrl_c = tokio::signal::ctrl_c();
    #[cfg(unix)]
    {
        let mut terminate = match tokio::signal::unix::signal(
            tokio::signal::unix::SignalKind::terminate(),
        ) {
            Ok(terminate) => terminate,
            Err(error) => {
                tracing::warn!(%error, "cannot watch for SIGTERM; Ctrl-C still stops the server");
                let _ = ctrl_c.await;
                return;
            }
        };
        tokio::select! {
            _ = ctrl_c => {},
            _ = terminate.recv() => {},
        }
    }
    #[cfg(not(unix))]
    {
        let _ = ctrl_c.await;
    }
    tracing::info!("shutdown signal received");
}
