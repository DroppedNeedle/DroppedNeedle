//! Tracing setup. Logs go to stderr so stdout stays clean for commands
//! like `--print-openapi` whose output feeds other tools.
//!
//! This module builds the one subscriber. A layer that wants the events too
//! (with a filter of its own) is passed to [`init_tracing`], never installed
//! beside it.

use std::io::IsTerminal as _;
use std::sync::OnceLock;

use tracing_subscriber::{
    EnvFilter, Layer, Registry, fmt, layer::SubscriberExt as _, util::SubscriberInitExt as _,
};

use crate::config::{AppConfig, LogFormat};

/// Process-wide init guard; tests build many states but init once.
static INIT: OnceLock<()> = OnceLock::new();

/// A layer [`init_tracing`] adds beside the log writer.
pub type ExtraLayer = Box<dyn Layer<Registry> + Send + Sync>;

/// How the log is written, from the deployment config.
#[derive(Debug, Clone, Copy)]
pub struct LogSetup<'a> {
    /// The directive `AppConfig` resolved from `RUST_LOG` or `LOG_LEVEL`.
    pub filter: &'a str,
    /// Readable lines or JSON objects.
    pub format: LogFormat,
    /// Never colour the text, even on a terminal.
    pub no_color: bool,
}

impl<'a> LogSetup<'a> {
    /// The log setup `config` asks for.
    pub fn from_config(config: &'a AppConfig) -> Self {
        Self {
            filter: &config.log_filter,
            format: config.log_format,
            no_color: config.no_color,
        }
    }
}

/// Install the global tracing subscriber: the log writer under `setup`'s
/// filter, plus each of `extra` under its own. A filter that does not parse
/// falls back to `info` and says so. Text is coloured only when stderr is a
/// terminal and `NO_COLOR` is unset, so a container log holds no escape
/// codes. JSON is one object per line, with the event's fields at the top
/// level and the request's span (its id) beside them. Safe to call
/// repeatedly: later calls are no-ops.
pub fn init_tracing(setup: &LogSetup<'_>, extra: Vec<ExtraLayer>) {
    INIT.get_or_init(|| {
        let (env_filter, rejected) = match EnvFilter::try_new(setup.filter) {
            Ok(parsed) => (parsed, None),
            Err(error) => (EnvFilter::new("info"), Some(error)),
        };
        let writer = fmt::layer().with_writer(std::io::stderr);
        let log: ExtraLayer = match setup.format {
            LogFormat::Text => writer
                .with_ansi(!setup.no_color && std::io::stderr().is_terminal())
                .with_filter(env_filter)
                .boxed(),
            LogFormat::Json => writer
                .json()
                .flatten_event(true)
                .with_current_span(true)
                .with_span_list(false)
                .with_filter(env_filter)
                .boxed(),
        };
        let mut layers = extra;
        layers.push(log);
        tracing_subscriber::registry().with(layers).init();
        if let Some(error) = rejected {
            tracing::warn!(
                filter = setup.filter,
                %error,
                "log filter does not parse; logging at info"
            );
        }
    });
}
