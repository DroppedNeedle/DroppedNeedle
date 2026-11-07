//! The site's look: the admin's site CSS and whether people may add their
//! own.

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use super::Section;

/// Site-wide appearance. The server's appearance routes check the sheet
/// before it is saved; the kernel only holds it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(default)]
pub struct Appearance {
    /// CSS every signed-in page loads on top of the app's own styles.
    pub site_css: String,
    /// Let people add their own CSS on top of the site's.
    pub allow_user_css: bool,
}

impl Default for Appearance {
    fn default() -> Self {
        Self {
            site_css: String::new(),
            allow_user_css: true,
        }
    }
}

impl Section for Appearance {
    const KEY: &'static str = "appearance";
}
