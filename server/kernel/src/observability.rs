//! Tracing setup. Logs go to stderr so stdout stays clean for commands
//! like `--print-openapi` whose output feeds other tools.

use std::sync::OnceLock;
use tracing_subscriber::{EnvFilter, fmt};

/// Process-wide init guard; tests build many states but init once.
static INIT: OnceLock<()> = OnceLock::new();

/// Install the global tracing subscriber with `filter`, the directive
/// `AppConfig` resolved from `RUST_LOG` or `LOG_LEVEL`. A directive that
/// does not parse falls back to `info` and says so. Safe to call
/// repeatedly: later calls are no-ops.
pub fn init_tracing(filter: &str) {
    INIT.get_or_init(|| {
        let (env_filter, rejected) = match EnvFilter::try_new(filter) {
            Ok(parsed) => (parsed, None),
            Err(error) => (EnvFilter::new("info"), Some(error)),
        };
        fmt()
            .with_env_filter(env_filter)
            .with_writer(std::io::stderr)
            .init();
        if let Some(error) = rejected {
            tracing::warn!(filter, %error, "log filter does not parse; logging at info");
        }
    });
}
