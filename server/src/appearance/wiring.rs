//! The appearance bundle and its routes.

use std::sync::Arc;

use super::check::CssRules;
use super::handlers as h;
use super::store::AppearanceStore;
use crate::settings::services::SettingsService;
use droppedneedle_kernel::mount::{Routes, routes};
use droppedneedle_kernel::platform::Platform;
use droppedneedle_kernel::runtime_config::Live;
use droppedneedle_kernel::runtime_config::sections::Appearance;

/// What the appearance routes need, built once with the settings bundle.
#[derive(Clone)]
pub struct AppearanceSetup {
    /// Each person's stored appearance.
    pub store: AppearanceStore,
    /// The site section, read on every request.
    pub section: Live<Appearance>,
    /// The section service the site section saves through, so its save
    /// runs like every other section's.
    pub settings: Arc<SettingsService>,
    /// The custom CSS rules, decoded once for every check.
    pub rules: Arc<CssRules>,
}

impl AppearanceSetup {
    /// Build over `store` and the platform's settings, saving the site
    /// section through `settings`.
    pub fn build(
        platform: &Platform,
        store: AppearanceStore,
        settings: Arc<SettingsService>,
    ) -> Self {
        Self {
            store,
            section: platform.settings.live(),
            settings,
            rules: Arc::new(CssRules::compiled()),
        }
    }
}

/// The caller's own appearance for any signed-in user, and the site
/// section for admins.
pub fn routes() -> Routes<AppearanceSetup> {
    Routes::new()
        .user(routes!(h::get_my_appearance, h::put_my_appearance))
        .user(routes!(h::import_my_appearance))
        .user(routes!(h::list_my_css_saves))
        .admin(routes!(h::get_site_appearance, h::put_site_appearance))
}
