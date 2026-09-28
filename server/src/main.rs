//! DroppedNeedle v3 server binary: one process, graceful SIGTERM shutdown.

#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;

use droppedneedle::{
    AppConfig, AppState, create_app, docs::ApiDoc, http_client::HttpClientFactory,
    ids::UuidGenerator, observability::init_tracing,
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
    let config = AppConfig::load().map_err(|error| error.to_string())?;
    let http = HttpClientFactory::new().map_err(|error| error.to_string())?;
    let state = AppState::new(Arc::new(UuidGenerator), http, config.clone());
    let app = create_app(state);

    let address = SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), config.port);
    let listener = tokio::net::TcpListener::bind(address)
        .await
        .map_err(|error| format!("cannot bind {address}: {error}"))?;
    tracing::info!(%address, "listening");
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .map_err(|error| format!("server fault: {error}"))?;
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
