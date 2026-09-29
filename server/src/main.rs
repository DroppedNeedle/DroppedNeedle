//! DroppedNeedle v3 server binary: one process, graceful SIGTERM shutdown.

#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;

use droppedneedle::{
    AppConfig, AppState,
    auth::{prod::ProdAuth, users::stores::SystemClock, wiring::AuthSetup},
    create_app,
    db::{DbConfig, open_runtime},
    docs::ApiDoc,
    http_client::HttpClientFactory,
    ids::UuidGenerator,
    observability::init_tracing,
    reads::ReadsSetup,
    runtime_config::{ConfigStore, Crypto, secret_sections::WrappedSettings},
    schema::apply_migrations,
};
use utoipa::OpenApi as _;

/// `--print-openapi` dumps the contract document to stdout for the
/// TypeScript pipeline and CI drift gate.
const PRINT_OPENAPI_ARG: &str = "--print-openapi";

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|arg| arg == PRINT_OPENAPI_ARG) {
        print_openapi();
        return;
    }
    if let Some(unknown) = args.first() {
        eprintln!("unknown argument {unknown:?}: expected {PRINT_OPENAPI_ARG}");
        std::process::exit(2);
    }

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
    let outcome = runtime.block_on(serve());
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
async fn serve() -> Result<(), String> {
    let mut config = AppConfig::load().map_err(|error| error.to_string())?;
    #[cfg(debug_assertions)]
    {
        config.debug_cors = true;
    }
    let http = HttpClientFactory::new().map_err(|error| error.to_string())?;
    let runtime = open_runtime(&DbConfig::new(&config.library_db_path))
        .await
        .map_err(|error| error.to_string())?;
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
        crypto,
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
    let reads = ReadsSetup::build(
        runtime.pool(),
        auth.users.clone(),
        ids.clone(),
        wrapped_api_key,
    );
    let state = AppState::new(ids, http, config.clone(), auth, reads);
    let app = create_app(state);

    // No refresh loops yet: the stage-4 discover ports are static scripted
    // fakes with no rebuild work to run, so there is nothing to schedule.
    // Stage 5 spawns the discover/home loops here (TokioSleeper over a
    // shutdown watch; see reads::discover::refresh) and awaits them after
    // the server below returns.

    let address = SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), config.port);
    let listener = tokio::net::TcpListener::bind(address)
        .await
        .map_err(|error| format!("cannot bind {address}: {error}"))?;
    tracing::info!(%address, "listening");
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .map_err(|error| format!("server fault: {error}"))?;
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
