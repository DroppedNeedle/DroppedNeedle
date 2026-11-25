//! The database backups the server takes by itself, and where it copies
//! them.

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use super::{Section, validation};
use crate::runtime_config::error::ConfigError;

/// Whether the server backs the database up every night, and a second
/// folder each backup is copied to. What each kind of backup keeps is
/// fixed, not a setting.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(default)]
pub struct Backups {
    /// Take a backup once a day.
    pub nightly: bool,
    /// An absolute folder path on another disk, as the server sees it, or
    /// empty for none. The save checks it and marks it as the app's.
    pub second_folder: String,
}

impl Default for Backups {
    fn default() -> Self {
        Self {
            nightly: true,
            second_folder: String::new(),
        }
    }
}

impl Section for Backups {
    const KEY: &'static str = "backups";

    fn validate(&self) -> Result<(), ConfigError> {
        let path = self.second_folder.trim();
        if path.is_empty() {
            return Ok(());
        }
        let parsed = std::path::Path::new(path);
        let climbs = parsed
            .components()
            .any(|part| matches!(part, std::path::Component::ParentDir));
        if !parsed.is_absolute() || climbs {
            return Err(validation(
                Self::KEY,
                "second_folder",
                "must be an absolute folder path without ..".to_owned(),
            ));
        }
        Ok(())
    }
}
