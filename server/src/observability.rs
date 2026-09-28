//! Tracing setup. Logs go to stderr so stdout stays clean for commands
//! like `--print-openapi` whose output feeds other tools.

use std::sync::OnceLock;
use tracing_subscriber::{EnvFilter, fmt};

/// Process-wide init guard; tests build many states but init once.
static INIT: OnceLock<()> = OnceLock::new();

/// Install the global tracing subscriber. Safe to call repeatedly: later
/// calls are no-ops.
pub fn init_tracing() {
    INIT.get_or_init(|| {
        let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
        fmt()
            .with_env_filter(filter)
            .with_writer(std::io::stderr)
            .init();
    });
}
