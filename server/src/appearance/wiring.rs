//! The appearance bundle and its routes.

use std::sync::Arc;

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
}

impl AppearanceSetup {
    /// Build over the platform's database, clock and settings, saving the
    /// site section through `settings`.
    pub fn build(platform: &Platform, settings: Arc<SettingsService>) -> Self {
        Self {
            store: AppearanceStore::new(platform.db.clone(), platform.clock.clone()),
            section: platform.settings.live(),
            settings,
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
