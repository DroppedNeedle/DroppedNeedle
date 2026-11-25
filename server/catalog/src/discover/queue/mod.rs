//! The album pools and seeds Discover's builds share.
//!
//! The pieces: [`build`] holds what one build reads from and the pools of
//! similar artists' albums, [`seeds`] the person's seed artists,
//! [`select`] the pure dealing the radio surfaces share and [`store`]
//! Discover's own tables.

pub mod build;
pub mod seeds;
pub mod select;
pub mod store;

use droppedneedle_kernel::runtime_config::Live;
use droppedneedle_kernel::runtime_config::secret_sections::AdvancedSettings;

/// The Discover settings one call works with, read fresh each time.
#[derive(Debug, Clone, PartialEq)]
pub struct QueueSettings {
    /// MusicBrainz lookups one Last.fm album batch may spend.
    pub lastfm_mbid_max_lookups: usize,
    /// Whether a user with Last.fm and ListenBrainz both linked gets seed
    /// and similar artists from both.
    pub blend_services: bool,
    /// Days a hide carried from before typed answers is kept, at most
    /// its year.
    pub ignored_retention_days: i64,
    /// How long between prunes of lapsed answers, ms.
    pub prune_interval_ms: i64,
}

impl QueueSettings {
    /// Discover's view of the advanced settings, clamped to the ranges the
    /// settings form allows.
    pub fn from_advanced(advanced: &AdvancedSettings) -> Self {
        Self {
            lastfm_mbid_max_lookups: advanced.discover_queue_lastfm_mbid_max_lookups.clamp(1, 50)
                as usize,
            blend_services: advanced.discover_blend_listening_services,
            ignored_retention_days: advanced.ignored_releases_retention_days.max(1),
            prune_interval_ms: advanced.store_prune_interval_hours.max(1) * 3_600_000,
        }
    }

    /// The Discover settings in the advanced settings now. `None` when they
    /// cannot be read: the work they steer waits until they are saved
    /// again, and [`Live::read`] logs it once.
    pub fn read(advanced: &Live<AdvancedSettings>) -> Option<Self> {
        advanced
            .read()
            .map(|advanced| Self::from_advanced(&advanced))
    }
}

/// What a person who asks for Discover while its settings cannot be read
/// is told.
pub const SETTINGS_UNREADABLE: &str =
    "Discover's settings cannot be read. Save Settings > Advanced again to resume Discover.";
