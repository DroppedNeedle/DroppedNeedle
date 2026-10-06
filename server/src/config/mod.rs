//! Deployment-tier configuration, read once at boot.
//!
//! Only deployment and infrastructure values live here (ports, paths, proxy
//! trust, timeouts). Anything user-editable at runtime belongs in a typed
//! config section, never in a new environment variable. [`AppConfig::load`]
//! is the single environment accessor; use sites take `AppConfig` by
//! constructor instead of reading the environment. Every variable it reads
//! is listed in [`deployment::DEPLOYMENT_VARS`].

pub mod base_path;
pub mod deployment;

use std::{
    net::IpAddr,
    path::{Path, PathBuf},
    time::Duration,
};

use thiserror::Error;

use crate::{auth::session::middleware::TrustedProxies, http_client::HttpSettings};

pub use base_path::{BasePathError, normalize_base_path};

/// Default HTTP port, carried over from v2.
pub const DEFAULT_PORT: u16 = 8688;

/// Default app root, matching the v2 container layout.
pub const DEFAULT_ROOT_APP_DIR: &str = "/app";

/// Default slskd completed-downloads mount, the v2 default.
pub const DEFAULT_SLSKD_DOWNLOADS_PATH: &str = "/data/downloads/slskd";

/// Default seconds to wait for connections and background work on shutdown.
pub const DEFAULT_SHUTDOWN_GRACE: Duration = Duration::from_secs(10);

/// Default bound on the cover image cache, in MiB (v2 default).
pub const DEFAULT_COVER_CACHE_MAX_SIZE_MB: u64 = 500;

/// Default trusted proxies: the loopbacks, the v2 default.
pub const DEFAULT_TRUSTED_PROXY_IPS: &str = "127.0.0.1,::1";

/// Where the listener binds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BindHost {
    /// Dual-stack `[::]` when the host has IPv6, else `0.0.0.0`.
    Auto,
    /// One address, exactly as given (`0.0.0.0`, `::`, or an interface IP).
    Fixed(IpAddr),
}

/// Deployment configuration for one process.
#[derive(Debug, Clone)]
pub struct AppConfig {
    /// TCP port to listen on.
    pub port: u16,
    /// Listener address.
    pub bind_host: BindHost,
    /// App root. Derives cache, db, config, plugin and web UI paths.
    pub root_app_dir: PathBuf,
    /// Directory for covers, backups, the stamped web UI and disk caches.
    pub cache_dir: PathBuf,
    /// Bound on the cover image cache under `<cache_dir>/covers`, in bytes.
    pub cover_cache_max_bytes: u64,
    /// The single SQLite WAL file.
    pub library_db_path: PathBuf,
    /// Config file location.
    pub config_file: PathBuf,
    /// Reverse-proxy mount prefix: `""` at the domain root, else a
    /// canonical `/seg[/seg...]` checked by [`normalize_base_path`].
    pub base_path: String,
    /// The pristine web UI build shipped in the image. Boot copies it into
    /// `<cache_dir>/static` with the base path stamped in.
    pub static_dir: PathBuf,
    /// Proxies whose `X-Forwarded-*` headers are honored.
    pub trusted_proxies: TrustedProxies,
    /// slskd's completed-downloads directory as mounted in this container.
    pub slskd_downloads_path: PathBuf,
    /// Tracing filter directive (`RUST_LOG` when set, else from `LOG_LEVEL`).
    pub log_filter: String,
    /// IANA timezone name from `TZ`, shown next to scan schedules.
    pub timezone: Option<String>,
    /// Outbound HTTP timeouts, pool size and User-Agent contact.
    pub http: HttpSettings,
    /// Bound on draining connections and stopping background work.
    pub shutdown_grace: Duration,
    /// Mounts the `__test__` failure hooks. Constructor-only on purpose: no
    /// environment variable can switch these on in a production binary.
    #[cfg(any(test, feature = "test-support"))]
    pub test_hooks: bool,
    /// Mounts the debug-only localhost CORS layer. Constructor-only, and
    /// `main` only enables it in debug builds: release binaries never
    /// serve CORS.
    pub debug_cors: bool,
    /// Mounts the dev-only tooling routes (covers-debug). Constructor-only
    /// with no environment variable, and `create_app` additionally gates
    /// the mount on debug builds: release binaries never serve tooling.
    pub tooling_routes: bool,
}

impl AppConfig {
    /// Defaults for every value, as if the environment were empty, except
    /// the port. Tests and tooling start here.
    pub fn new(port: u16) -> Self {
        let root = PathBuf::from(DEFAULT_ROOT_APP_DIR);
        Self::with_root(port, &root)
    }

    /// Defaults with every derived path under `root`.
    pub fn with_root(port: u16, root: &Path) -> Self {
        let cache_dir = root.join("cache");
        Self {
            port,
            bind_host: BindHost::Auto,
            root_app_dir: root.to_owned(),
            library_db_path: cache_dir.join("library.db"),
            cache_dir,
            cover_cache_max_bytes: DEFAULT_COVER_CACHE_MAX_SIZE_MB * 1024 * 1024,
            config_file: root.join("config").join("config.json"),
            base_path: String::new(),
            static_dir: root.join("static"),
            trusted_proxies: TrustedProxies::loopback(),
            slskd_downloads_path: PathBuf::from(DEFAULT_SLSKD_DOWNLOADS_PATH),
            log_filter: "info".to_owned(),
            timezone: None,
            http: HttpSettings::default(),
            shutdown_grace: DEFAULT_SHUTDOWN_GRACE,
            #[cfg(any(test, feature = "test-support"))]
            test_hooks: false,
            debug_cors: false,
            tooling_routes: false,
        }
    }

    /// Test configuration with the failure hooks mounted.
    #[cfg(any(test, feature = "test-support"))]
    pub fn with_test_hooks(mut self) -> Self {
        self.test_hooks = true;
        self
    }

    /// Debug configuration with the dev-only tooling routes mounted.
    /// Tests and debug servers only; release builds cannot mount them.
    pub fn with_tooling_routes(mut self) -> Self {
        self.tooling_routes = true;
        self
    }

    /// Read the deployment tier from the process environment.
    pub fn load() -> Result<Self, ConfigError> {
        Self::from_env(|name| std::env::var(name).ok())
    }

    /// Read the deployment tier through `lookup`. Blank values count as
    /// unset. Every name passed to `lookup` is in
    /// [`deployment::DEPLOYMENT_VARS`]; a test holds the two in step.
    pub fn from_env(lookup: impl Fn(&str) -> Option<String>) -> Result<Self, ConfigError> {
        let env = |name: &str| {
            lookup(name)
                .map(|value| value.trim().to_owned())
                .filter(|value| !value.is_empty())
        };
        let path = |name: &str| env(name).map(PathBuf::from);

        let port = parse_port(env("PORT").as_deref())?;
        let root = path("ROOT_APP_DIR").unwrap_or_else(|| PathBuf::from(DEFAULT_ROOT_APP_DIR));
        let mut config = Self::with_root(port, &root);
        config.bind_host = parse_bind_host(env("BIND_HOST").as_deref())?;
        if let Some(cache_dir) = path("CACHE_DIR") {
            config.library_db_path = cache_dir.join("library.db");
            config.cache_dir = cache_dir;
        }
        if let Some(megabytes) = env("COVER_CACHE_MAX_SIZE_MB") {
            let parsed: u64 = megabytes.parse().map_err(|_| ConfigError::InvalidNumber {
                name: "COVER_CACHE_MAX_SIZE_MB",
                value: megabytes.clone(),
            })?;
            config.cover_cache_max_bytes = parsed.saturating_mul(1024 * 1024);
        }
        if let Some(db) = path("LIBRARY_DB_PATH") {
            config.library_db_path = db;
        }
        if let Some(file) = path("CONFIG_FILE_PATH") {
            config.config_file = file;
        }
        // Not trimmed: a stray space in BASE_PATH is refused, not fixed up.
        let base = lookup("BASE_PATH").unwrap_or_default();
        config.base_path =
            normalize_base_path(&base).map_err(|reason| ConfigError::InvalidBasePath {
                value: base.clone(),
                reason,
            })?;
        if let Some(dir) = path("DROPPEDNEEDLE_STATIC_DIR") {
            config.static_dir = dir;
        }
        let proxies = env("TRUSTED_PROXY_IPS").unwrap_or_else(|| DEFAULT_TRUSTED_PROXY_IPS.into());
        config.trusted_proxies = TrustedProxies::parse(&proxies)
            .map_err(|error| ConfigError::InvalidTrustedProxies(error.to_string()))?;
        if let Some(downloads) = path("SLSKD_DOWNLOADS_PATH") {
            config.slskd_downloads_path = downloads;
        }
        let level = log_filter_for(env("LOG_LEVEL").as_deref())?;
        config.log_filter = env("RUST_LOG").unwrap_or(level);
        config.timezone = env("TZ");
        if let Some(seconds) = env("HTTP_TIMEOUT") {
            config.http.timeout = parse_seconds("HTTP_TIMEOUT", &seconds)?;
        }
        if let Some(seconds) = env("HTTP_CONNECT_TIMEOUT") {
            config.http.connect_timeout = parse_seconds("HTTP_CONNECT_TIMEOUT", &seconds)?;
        }
        if let Some(count) = env("HTTP_MAX_KEEPALIVE") {
            config.http.max_idle_per_host =
                count.parse().map_err(|_| ConfigError::InvalidNumber {
                    name: "HTTP_MAX_KEEPALIVE",
                    value: count.clone(),
                })?;
        }
        if let Some(email) = env("CONTACT_EMAIL") {
            if !email.chars().all(|ch| ch.is_ascii_graphic()) {
                return Err(ConfigError::InvalidContactEmail(email));
            }
            config.http.contact_email = email;
        }
        if let Some(seconds) = env("SHUTDOWN_GRACE_PERIOD") {
            config.shutdown_grace = parse_seconds("SHUTDOWN_GRACE_PERIOD", &seconds)?;
        }
        Ok(config)
    }

    /// Directory holding `config.json` and the data-encryption key.
    pub fn config_dir(&self) -> PathBuf {
        self.config_file
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| self.root_app_dir.join("config"))
    }

    /// Installed plugins.
    pub fn plugins_dir(&self) -> PathBuf {
        self.root_app_dir.join("plugins")
    }

    /// Download and drop-import staging (`/app/imports`, as in v2).
    pub fn imports_dir(&self) -> PathBuf {
        self.root_app_dir.join("imports")
    }

    /// The web UI as served, rebuilt from [`AppConfig::static_dir`] on boot.
    pub fn served_static_dir(&self) -> PathBuf {
        self.cache_dir.join("static")
    }
}

/// Parse an optional raw port value, defaulting when absent.
pub fn parse_port(raw: Option<&str>) -> Result<u16, ConfigError> {
    match raw {
        None => Ok(DEFAULT_PORT),
        Some(text) => text
            .trim()
            .parse::<u16>()
            .map_err(|_| ConfigError::InvalidPort(text.to_owned())),
    }
}

/// `auto` (or unset) picks the dual-stack wildcard; anything else must be
/// one IP literal, brackets allowed around IPv6.
fn parse_bind_host(raw: Option<&str>) -> Result<BindHost, ConfigError> {
    let Some(text) = raw else {
        return Ok(BindHost::Auto);
    };
    if text.eq_ignore_ascii_case("auto") {
        return Ok(BindHost::Auto);
    }
    let bare = text
        .strip_prefix('[')
        .and_then(|inner| inner.strip_suffix(']'))
        .unwrap_or(text);
    bare.parse()
        .map(BindHost::Fixed)
        .map_err(|_| ConfigError::InvalidBindHost(text.to_owned()))
}

/// Map v2's `LOG_LEVEL` names onto a tracing filter. `CRITICAL` has no
/// tracing level of its own and maps to `error`.
fn log_filter_for(raw: Option<&str>) -> Result<String, ConfigError> {
    let Some(text) = raw else {
        return Ok("info".to_owned());
    };
    let level = match text.to_ascii_uppercase().as_str() {
        "TRACE" => "trace",
        "DEBUG" => "debug",
        "INFO" => "info",
        "WARNING" | "WARN" => "warn",
        "ERROR" | "CRITICAL" => "error",
        _ => return Err(ConfigError::InvalidLogLevel(text.to_owned())),
    };
    Ok(level.to_owned())
}

/// Seconds as a decimal number (`10`, `2.5`), above zero.
fn parse_seconds(name: &'static str, text: &str) -> Result<Duration, ConfigError> {
    text.parse::<f64>()
        .ok()
        .filter(|seconds| seconds.is_finite() && *seconds > 0.0 && *seconds <= 86_400.0)
        .map(Duration::from_secs_f64)
        .ok_or_else(|| ConfigError::InvalidSeconds {
            name,
            value: text.to_owned(),
        })
}

/// Typed boot-configuration failure.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum ConfigError {
    /// The `PORT` value is not a valid TCP port number.
    #[error("invalid PORT value {0:?}: expected a number from 0 to 65535")]
    InvalidPort(String),
    /// `BIND_HOST` is neither `auto` nor an IP address.
    #[error("invalid BIND_HOST value {0:?}: expected auto or an IP address")]
    InvalidBindHost(String),
    /// `BASE_PATH` is not canonical.
    #[error("invalid BASE_PATH value {value:?}: {reason}")]
    InvalidBasePath {
        /// The value as given.
        value: String,
        /// What is wrong with it.
        reason: BasePathError,
    },
    /// `TRUSTED_PROXY_IPS` has an entry that is not an IP, CIDR or `*`.
    #[error("invalid TRUSTED_PROXY_IPS: {0}")]
    InvalidTrustedProxies(String),
    /// `LOG_LEVEL` is not a known level.
    #[error("invalid LOG_LEVEL value {0:?}: expected DEBUG, INFO, WARNING or ERROR")]
    InvalidLogLevel(String),
    /// A seconds value is not a positive number of at most one day.
    #[error("invalid {name} value {value:?}: expected seconds above zero")]
    InvalidSeconds {
        /// Variable name.
        name: &'static str,
        /// The value as given.
        value: String,
    },
    /// A count is not a whole number.
    #[error("invalid {name} value {value:?}: expected a whole number")]
    InvalidNumber {
        /// Variable name.
        name: &'static str,
        /// The value as given.
        value: String,
    },
    /// `CONTACT_EMAIL` cannot go into a User-Agent header.
    #[error("invalid CONTACT_EMAIL value {0:?}: spaces and control characters are not allowed")]
    InvalidContactEmail(String),
}

#[cfg(test)]
mod tests {
    use std::{cell::RefCell, collections::BTreeSet};

    use super::*;

    fn load(vars: &[(&str, &str)]) -> Result<AppConfig, ConfigError> {
        AppConfig::from_env(|name| {
            vars.iter()
                .find(|(key, _)| *key == name)
                .map(|(_, value)| (*value).to_owned())
        })
    }

    #[test]
    fn every_registry_entry_is_read_and_nothing_else() {
        let asked = RefCell::new(BTreeSet::new());
        AppConfig::from_env(|name| {
            asked.borrow_mut().insert(name.to_owned());
            None
        })
        .unwrap();
        let registry: BTreeSet<String> = deployment::DEPLOYMENT_VARS
            .iter()
            .map(|var| var.env_name.to_owned())
            .collect();
        assert_eq!(asked.into_inner(), registry);
    }

    #[test]
    fn values_parse_and_bad_ones_fail_the_boot() {
        let config = load(&[
            ("PORT", "9000"),
            ("BIND_HOST", "[::1]"),
            ("ROOT_APP_DIR", "/srv/dn"),
            ("BASE_PATH", "/music"),
            ("TRUSTED_PROXY_IPS", "10.0.0.0/8"),
            ("LOG_LEVEL", "warning"),
            ("HTTP_TIMEOUT", "2.5"),
            ("SHUTDOWN_GRACE_PERIOD", "3"),
        ])
        .unwrap();
        assert_eq!(config.port, 9000);
        assert_eq!(config.bind_host, BindHost::Fixed("::1".parse().unwrap()));
        assert_eq!(
            config.library_db_path,
            Path::new("/srv/dn/cache/library.db")
        );
        assert_eq!(config.static_dir, Path::new("/srv/dn/static"));
        assert_eq!(config.base_path, "/music");
        assert_eq!(config.log_filter, "warn");
        assert_eq!(config.http.timeout, Duration::from_millis(2500));
        assert_eq!(config.shutdown_grace, Duration::from_secs(3));
        assert!(
            config
                .trusted_proxies
                .is_trusted(Some("10.1.2.3:1".parse().unwrap()))
        );
        assert!(
            !config
                .trusted_proxies
                .is_trusted(Some("127.0.0.1:1".parse().unwrap()))
        );
        assert_eq!(
            load(&[("RUST_LOG", "debug,hyper=info")])
                .unwrap()
                .log_filter,
            "debug,hyper=info"
        );

        for bad in [
            ("PORT", "99999"),
            ("BIND_HOST", "example.com"),
            ("BASE_PATH", "/api"),
            ("BASE_PATH", "music/"),
            ("TRUSTED_PROXY_IPS", "proxy"),
            ("LOG_LEVEL", "LOUD"),
            ("HTTP_TIMEOUT", "0"),
            ("HTTP_MAX_KEEPALIVE", "-1"),
            ("CONTACT_EMAIL", "a b@c"),
        ] {
            assert!(load(&[bad]).is_err(), "{bad:?} should fail");
        }
    }
}
