//! The database backups the server takes by itself.

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use super::Section;

/// Whether the server backs the database up every night. What each kind
/// of backup keeps is fixed, not a setting.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(default)]
pub struct Backups {
    /// Take a backup once a day.
    pub nightly: bool,
}

impl Default for Backups {
    fn default() -> Self {
        Self { nightly: true }
    }
}

impl Section for Backups {
    const KEY: &'static str = "backups";
}
