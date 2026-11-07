//! Appearance: each person's theme and custom CSS, kept on the account so
//! they follow the person to every device, and the site's own CSS.
//!
//! - A person's row is in `app_user_appearances`; no row means the
//!   defaults (no theme chosen, no CSS, CSS switched on). Each save that
//!   changes the CSS to something not empty is kept in
//!   `app_custom_css_saves`, the newest ten per person.
//! - The site CSS and the switch that lets people add their own are the
//!   `appearance` settings section. It controls none of the work a section
//!   that cannot be read holds back, so such a section reads as no site CSS
//!   with people's CSS allowed.
//! - Every sheet passes [`check`] before it is stored. The browser applies
//!   the stored sheets, running its copy of the check again first.
//! - An admin can switch a person's CSS off and see a summary of it, never
//!   its text (`crate::admin`).

pub mod check;
pub mod handlers;
pub mod models;
pub mod service;
pub(crate) mod sql;
pub mod store;
pub mod wiring;

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// The theme a person chose.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum ThemeChoice {
    /// Follow the device's light or dark setting.
    System,
    /// The light theme.
    Light,
    /// The dark theme.
    Dark,
    /// The Midnight theme.
    Midnight,
}

droppedneedle_kernel::spelled!(ThemeChoice {
    System => "system",
    Light => "light",
    Dark => "dark",
    Midnight => "midnight",
});
