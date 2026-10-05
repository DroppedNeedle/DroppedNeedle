//! DroppedNeedle v3 server binary: one process, graceful SIGTERM shutdown.
//!
//! This file keeps the process concerns: arguments, allocator tuning,
//! the async runtime, the listener and the stop signal. Everything the
//! server is made of is wired in `droppedneedle::bootstrap`.

#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use droppedneedle::{AppConfig, bootstrap, docs::ApiDoc, observability::init_tracing};
use utoipa::OpenApi as _;

/// `--print-openapi` dumps the contract document to stdout for the
/// TypeScript pipeline and CI drift gate.
const PRINT_OPENAPI_ARG: &str = "--print-openapi";

/// `--tooling-routes` mounts the dev-only tooling routes (covers-debug).
/// Debug builds only: a release binary rejects it as unknown.
#[cfg(debug_assertions)]
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
    let config = match AppConfig::load() {
        Ok(config) => config,
        Err(error) => {
            eprintln!("{error}");
            std::process::exit(1);
        }
    };
    #[cfg(debug_assertions)]
    let config = AppConfig {
        debug_cors: true,
        tooling_routes: args.iter().any(|arg| arg == TOOLING_ROUTES_ARG),
        ..config
    };
    init_tracing(&config.log_filter);
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
    if let Err(message) = runtime.block_on(run(config)) {
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

/// Boot, bind, and serve until SIGTERM or Ctrl-C.
async fn run(config: AppConfig) -> Result<(), String> {
    let (host, port) = (config.bind_host, config.port);
    let (router, background) = bootstrap::build(config)
        .await
        .map_err(|error| format!("boot failed: {error}"))?;
    let listener = bootstrap::bind(host, port)
        .await
        .map_err(|error| error.to_string())?;
    if let Ok(address) = listener.local_addr() {
        tracing::info!(%address, "listening");
    }
    bootstrap::serve(listener, router, background, shutdown_signal())
        .await
        .map_err(|error| error.to_string())
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
