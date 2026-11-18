//! Whether each person arranges their own home, or the admin arranges it for
//! everyone.

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use super::Section;

/// Who decides the home layout.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum PersonalizationMode {
    /// Each person keeps their own layout.
    #[default]
    Allow,
    /// The admin's saved layout is served to everyone.
    Force,
}

/// Home layout personalization.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(default)]
pub struct Personalization {
    /// Allow or force.
    pub mode: PersonalizationMode,
}

impl Section for Personalization {
    const KEY: &'static str = "personalization";
}
