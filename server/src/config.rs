//! Deployment-tier configuration, read once at boot.
//!
//! Only deployment and infrastructure values live here (port, paths). Anything
//! user-editable at runtime belongs in a typed config section, never in a new
//! environment variable. `load` is the single environment accessor; use sites
//! take `AppConfig` by constructor instead of reading the environment.

use thiserror::Error;

/// Default HTTP port, carried over from v2.
pub const DEFAULT_PORT: u16 = 8688;

/// Environment variable naming the HTTP port.
pub const PORT_ENV_VAR: &str = "PORT";

/// Deployment configuration for one process.
#[derive(Debug, Clone)]
pub struct AppConfig {
    /// TCP port to bind on all interfaces.
    pub port: u16,
    /// Mounts the `__test__` failure hooks. Constructor-only on purpose: no
    /// environment variable can switch these on in a production binary.
    pub test_hooks: bool,
}

impl AppConfig {
    /// Production configuration: default port, test hooks off.
    pub fn new(port: u16) -> Self {
        Self {
            port,
            test_hooks: false,
        }
    }

    /// Test configuration with the failure hooks mounted.
    pub fn with_test_hooks(mut self) -> Self {
        self.test_hooks = true;
        self
    }

    /// Read the deployment tier from the environment.
    pub fn load() -> Result<Self, ConfigError> {
        let raw = std::env::var(PORT_ENV_VAR).ok();
        Ok(Self::new(parse_port(raw.as_deref())?))
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
