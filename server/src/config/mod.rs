//! Deployment-tier configuration, read once at boot.
//!
//! Only deployment and infrastructure values live here (port, paths). Anything
//! user-editable at runtime belongs in a typed config section, never in a new
//! environment variable. `load` is the single environment accessor; use sites
//! take `AppConfig` by constructor instead of reading the environment.

pub mod deployment;

use thiserror::Error;

/// Default HTTP port, carried over from v2.
pub const DEFAULT_PORT: u16 = 8688;

/// Environment variable naming the HTTP port.
pub const PORT_ENV_VAR: &str = "PORT";

/// Environment variable naming the app root. Derives cache, db, and config
/// paths when the specific overrides below are absent.
pub const ROOT_APP_DIR_ENV_VAR: &str = "ROOT_APP_DIR";

/// Environment variable naming the reverse-proxy mount prefix.
pub const BASE_PATH_ENV_VAR: &str = "BASE_PATH";

/// Default app root, matching the v2 container layout.
pub const DEFAULT_ROOT_APP_DIR: &str = "/app";

/// Deployment configuration for one process.
#[derive(Debug, Clone)]
pub struct AppConfig {
    /// TCP port to bind on all interfaces.
    pub port: u16,
    /// App root. Derives cache, db, and config paths.
    pub root_app_dir: std::path::PathBuf,
    /// Directory for covers, quota file, staging, and disk caches.
    pub cache_dir: std::path::PathBuf,
    /// The single SQLite WAL file.
    pub library_db_path: std::path::PathBuf,
    /// Config file location.
    pub config_file: std::path::PathBuf,
    /// Reverse-proxy mount prefix (`""` at the domain root).
    pub base_path: String,
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
    /// Production configuration: default port, default root, hooks off.
    pub fn new(port: u16) -> Self {
        let root = std::path::PathBuf::from(DEFAULT_ROOT_APP_DIR);
        let cache_dir = root.join("cache");
        let library_db_path = cache_dir.join("library.db");
        let config_file = root.join("config").join("config.json");
        Self {
            port,
            root_app_dir: root,
            cache_dir,
            library_db_path,
            config_file,
            base_path: String::new(),
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

    /// Read the deployment tier from the environment. This is the single
    /// environment accessor; every override resolves here, never at use
    /// sites.
    pub fn load() -> Result<Self, ConfigError> {
        let raw = std::env::var(PORT_ENV_VAR).ok();
        let root = read_env_path(ROOT_APP_DIR_ENV_VAR)
            .unwrap_or_else(|| std::path::PathBuf::from(DEFAULT_ROOT_APP_DIR));
        let cache_dir = read_env_path("CACHE_DIR").unwrap_or_else(|| root.join("cache"));
        let library_db_path =
            read_env_path("LIBRARY_DB_PATH").unwrap_or_else(|| cache_dir.join("library.db"));
        let config_file = read_env_path("CONFIG_FILE_PATH")
            .unwrap_or_else(|| root.join("config").join("config.json"));
        let base_path = std::env::var(BASE_PATH_ENV_VAR)
            .ok()
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_default();
        Ok(Self {
            port: parse_port(raw.as_deref())?,
            root_app_dir: root,
            cache_dir,
            library_db_path,
            config_file,
            base_path,
            #[cfg(any(test, feature = "test-support"))]
            test_hooks: false,
            debug_cors: false,
            tooling_routes: false,
        })
    }

    /// Directory holding `config.json` and the data-encryption key.
    pub fn config_dir(&self) -> std::path::PathBuf {
        self.config_file
            .parent()
            .map(std::path::Path::to_path_buf)
            .unwrap_or_else(|| self.root_app_dir.join("config"))
    }
}

/// Read an optional path override, ignoring blank values.
fn read_env_path(name: &str) -> Option<std::path::PathBuf> {
    std::env::var(name)
        .ok()
        .filter(|value| !value.trim().is_empty())
        .map(std::path::PathBuf::from)
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

/// Typed boot-configuration failure.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum ConfigError {
    /// The `PORT` value is not a valid TCP port number.
    #[error("invalid PORT value {0:?}: expected a number from 0 to 65535")]
    InvalidPort(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn port_defaults_and_parses() {
        assert_eq!(parse_port(None).unwrap(), DEFAULT_PORT);
        assert_eq!(parse_port(Some("8080")).unwrap(), 8080);
        assert!(parse_port(Some("nope")).is_err());
        assert!(parse_port(Some("99999")).is_err());
    }

    #[test]
    fn test_hooks_default_off() {
        assert!(!AppConfig::new(DEFAULT_PORT).test_hooks);
        assert!(AppConfig::new(DEFAULT_PORT).with_test_hooks().test_hooks);
    }
}
