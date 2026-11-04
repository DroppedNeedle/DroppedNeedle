//! What crosses from v2 to v3: the settings sections, the fields kept inside
//! the filtered ones, and where secrets sit. The exporter and the importer
//! read these same lists, so they cannot drift.

/// How the exporter treats a v2 settings section.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Export {
    /// Copied as it is, with its secret positions sealed.
    Whole,
    /// Filtered or rebuilt by a rule of its own in the exporter.
    ByItsOwnRule,
    /// No exporter writes it; only a file from elsewhere can hold it.
    Never,
}

/// How the importer treats a section the export holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Import {
    /// Replaces the v3 section; when absent, the section resets to defaults.
    Replaces,
    /// v3 removed it: not imported, and the report lists it as dropped.
    Removed,
}

/// Every settings section the two sides know, in export order.
const SECTIONS: &[(&str, Export, Import)] = &[
    ("user_preferences", Export::Whole, Import::Replaces),
    ("library_scan_schedule", Export::Whole, Import::Replaces),
    (
        "library_scan_filesystem_watcher",
        Export::Whole,
        Import::Replaces,
    ),
    ("download_client", Export::Whole, Import::Replaces),
    ("download_clients", Export::Whole, Import::Replaces),
    ("wanted", Export::Whole, Import::Replaces),
    ("source_priority", Export::Whole, Import::Replaces),
    ("usenet_search_backend", Export::Whole, Import::Replaces),
    ("indexers", Export::Whole, Import::Replaces),
    ("prowlarr", Export::Whole, Import::Replaces),
    ("lidarr_import", Export::Whole, Import::Replaces),
    ("jellyfin_settings", Export::Whole, Import::Replaces),
    ("navidrome_settings", Export::Whole, Import::Replaces),
    ("plex_settings", Export::Whole, Import::Replaces),
    ("listenbrainz_settings", Export::Whole, Import::Replaces),
    ("youtube_settings", Export::Whole, Import::Replaces),
    ("lastfm_settings", Export::Whole, Import::Replaces),
    ("spotify_settings", Export::Whole, Import::Replaces),
    ("events", Export::Whole, Import::Replaces),
    ("wrapped_settings", Export::Whole, Import::Replaces),
    ("oidc_settings", Export::Whole, Import::Replaces),
    ("security_settings", Export::Whole, Import::Replaces),
    ("connect_apps", Export::Whole, Import::Replaces),
    ("library_settings", Export::Whole, Import::Replaces),
    ("library_management", Export::Whole, Import::Replaces),
    ("scrobble_settings", Export::Whole, Import::Removed),
    ("primary_music_source", Export::Whole, Import::Removed),
    ("get_it", Export::Whole, Import::Replaces),
    ("advanced_settings", Export::ByItsOwnRule, Import::Replaces),
    ("download_policy", Export::ByItsOwnRule, Import::Replaces),
    (
        "musicbrainz_settings",
        Export::ByItsOwnRule,
        Import::Replaces,
    ),
    ("plugins", Export::ByItsOwnRule, Import::Replaces),
    ("_internal", Export::ByItsOwnRule, Import::Replaces),
    ("home_settings", Export::Never, Import::Removed),
];

/// The sections the exporter copies whole.
pub(crate) fn exported_whole() -> impl Iterator<Item = &'static str> {
    SECTIONS
        .iter()
        .filter(|(_, export, _)| *export == Export::Whole)
        .map(|(key, _, _)| *key)
}

/// The sections an import replaces. The settings keys outside this set are
/// left untouched: the export cannot speak for them.
pub(crate) fn replaced_sections() -> impl Iterator<Item = &'static str> {
    SECTIONS
        .iter()
        .filter(|(_, _, import)| *import == Import::Replaces)
        .map(|(key, _, _)| *key)
}

/// The v2 sections v3 removed. An export carrying one imports, and the
/// report lists it as a dropped setting.
pub(crate) fn removed_sections() -> impl Iterator<Item = &'static str> {
    SECTIONS
        .iter()
        .filter(|(_, _, import)| *import == Import::Removed)
        .map(|(key, _, _)| *key)
}

/// True when an import replaces this section.
pub(crate) fn is_replaced(section: &str) -> bool {
    replaced_sections().any(|key| key == section)
}

/// True when v3 removed this section.
pub(crate) fn is_removed_section(section: &str) -> bool {
    removed_sections().any(|key| key == section)
}

/// The `advanced_settings` fields v3 still reads.
pub(crate) const ADVANCED_KEPT: &[&str] = &[
    "discover_queue_size",
    "discover_queue_auto_generate",
    "discover_queue_seed_artists",
    "discover_queue_wildcard_slots",
    "discover_queue_warm_cycle_build",
    "discover_queue_similar_artists_limit",
    "discover_queue_albums_per_similar",
    "discover_queue_lastfm_mbid_max_lookups",
    "discover_picks_count",
    "discover_picks_genre_affinity_weight",
    "audiodb_enabled",
    "audiodb_name_search_fallback",
    "audiodb_api_key",
    "prefer_local_cover_art",
    "direct_remote_images_enabled",
    "request_history_retention_days",
    "ignored_releases_retention_days",
    "store_prune_interval_hours",
];

/// `advanced_settings` fields the exporter never writes. Two match a removed
/// prefix below, so an import still accepts them in a file from elsewhere.
const ADVANCED_NOT_EXPORTED: &[&str] = &[
    "artist_discovery_warm_interval",
    "artist_discovery_warm_delay",
    "cache_ttl_recently_viewed_bytes",
    "cache_ttl_local_files_recently_added",
];

/// The `_internal` fields v3 still reads.
pub(crate) const INTERNAL_KEPT: &[&str] = &["plex_client_id", "droppedneedle_device_id"];

/// v2 settings fields v3 removed from sections it keeps, as dotted paths
/// from the section key. `name[]` walks every element of a list. The
/// exporter still writes them, so a bundle's format does not move; an import
/// takes them out and the report lists each one dropped.
pub(crate) const REMOVED_FIELDS: &[&str] = &[
    "advanced_settings.http_timeout",
    "advanced_settings.http_connect_timeout",
    "advanced_settings.http_max_connections",
    "advanced_settings.batch_artist_images",
    "advanced_settings.batch_albums",
    "advanced_settings.delay_artist",
    "advanced_settings.delay_albums",
    "advanced_settings.memory_cache_max_entries",
    "advanced_settings.memory_cache_cleanup_interval",
    "advanced_settings.cover_memory_cache_max_entries",
    "advanced_settings.cover_memory_cache_max_size_mb",
    "advanced_settings.disk_cache_cleanup_interval",
    "advanced_settings.recent_metadata_max_size_mb",
    "advanced_settings.recent_covers_max_size_mb",
    "advanced_settings.persistent_metadata_ttl_hours",
    "advanced_settings.discover_queue_ttl",
    "advanced_settings.discover_queue_polling_interval",
    "advanced_settings.discover_queue_enrich_ttl",
    "advanced_settings.genre_section_ttl",
    "advanced_settings.request_concurrency",
    "advanced_settings.orphan_cover_demote_interval_hours",
    "advanced_settings.sync_stall_timeout_minutes",
    "advanced_settings.sync_max_timeout_hours",
    "advanced_settings.artist_discovery_precache_delay",
    "advanced_settings.artist_discovery_precache_concurrency",
    "advanced_settings.audiodb_prewarm_concurrency",
    "advanced_settings.audiodb_prewarm_delay",
    "connect_apps.exact_track_approval_supported",
    "connect_apps.discover_mode",
    "wanted.dormant_after_days",
    "download_policy.verify_downloads",
    "download_policy.usenet_min_release_age_minutes",
    "download_policy.source_selection_mode",
    "download_policy.quality_recipe[].target_bitrate_kbps",
    "download_client.client_type",
    "download_client.verify_downloads",
    "download_clients.sabnzbd.client_type",
    "download_clients.sabnzbd.post_processing",
    "indexers[].type",
    "library_management.profiles[].artwork.providers",
    "library_management.profiles[].artwork.approved_only",
    "library_management.profiles[].artwork.download_size",
    "library_management.profiles[].artwork.image_types",
    "library_management.profiles[].artwork.minimum_width",
    "library_management.profiles[].artwork.minimum_height",
    "library_management.profiles[].artwork.embedded_front_only",
    "library_management.profiles[].artwork.external_front_only",
    "library_management.profiles[].artwork.preserve_existing_types",
    "library_management.profiles[].artwork.external_naming_script_id",
    "library_management.profiles[].artwork.overwrite_external_files",
    "library_management.profiles[].metadata.format_compatibility",
    "library_management.profiles[].genres.maximum_ancestry_depth",
    "library_management.profiles[].organization.source_cleanup",
    "library_management.profiles[].organization.remove_empty_directories",
    "library_management.profiles[].file_behavior",
    "library_management.profiles[].enrichment.lyrics.provider",
    "library_management.profiles[].notification.refresh_droppedneedle",
    "library_management.root_assignments[].overrides.source_cleanup",
    "library_management.root_assignments[].overrides.preserve_timestamps",
    "library_management.root_assignments[].overrides.automatic_edition_acceptance_enabled",
    "library_management.external_refresh.retry_attempts",
    "library_management.external_refresh.retry_delay_seconds",
    "library_scan_schedule.last_scan",
    "library_scan_schedule.last_scan_success",
    "musicbrainz_settings.concurrent_searches",
    "plex_settings.scrobble_to_plex",
    "_internal.brainzmash_consent_admin",
];

/// Field families v3 removed whole, as (section, field prefix): every
/// server and browser cache lifetime is fixed in code now.
pub(crate) const REMOVED_PREFIXES: &[(&str, &str)] = &[
    ("advanced_settings", "cache_ttl_"),
    ("advanced_settings", "frontend_ttl_"),
];

/// True when `field`, directly under `section`, is a removed setting.
pub(crate) fn is_removed_field(section: &str, field: &str) -> bool {
    REMOVED_PREFIXES
        .iter()
        .any(|(owner, prefix)| *owner == section && field.starts_with(prefix))
        || REMOVED_FIELDS.iter().any(|path| {
            path.strip_prefix(section)
                .and_then(|rest| rest.strip_prefix('.'))
                == Some(field)
        })
}

/// True when the exporter writes this `advanced_settings` field.
pub(crate) fn advanced_exported(field: &str) -> bool {
    !ADVANCED_NOT_EXPORTED.contains(&field)
        && (ADVANCED_KEPT.contains(&field) || is_removed_field("advanced_settings", field))
}

/// True when the exporter writes this `_internal` field.
pub(crate) fn internal_exported(field: &str) -> bool {
    INTERNAL_KEPT.contains(&field) || is_removed_field("_internal", field)
}

/// How a secret position reaches the export.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Sealing {
    /// A top-level field v2 encrypted: opened with the v2 key, then sealed.
    FieldOfSection,
    /// Sealed by the exporter's code for its nested position.
    Nested,
    /// v2 keeps it in plain text: sealed as it is.
    PlainInV2,
}

/// One secret position: (section, dotted path inside it, how it is sealed).
/// `[]` in a path applies to every array element.
pub(crate) const SECRETS: &[(&str, &str, Sealing)] = &[
    ("download_client", "api_key", Sealing::FieldOfSection),
    ("download_clients", "sabnzbd.api_key", Sealing::Nested),
    ("indexers", "[].api_key", Sealing::Nested),
    ("prowlarr", "api_key", Sealing::FieldOfSection),
    ("lidarr_import", "api_key", Sealing::FieldOfSection),
    ("jellyfin_settings", "api_key", Sealing::FieldOfSection),
    ("navidrome_settings", "password", Sealing::FieldOfSection),
    ("plex_settings", "plex_token", Sealing::FieldOfSection),
    (
        "listenbrainz_settings",
        "user_token",
        Sealing::FieldOfSection,
    ),
    ("youtube_settings", "api_key", Sealing::FieldOfSection),
    ("lastfm_settings", "api_key", Sealing::FieldOfSection),
    ("lastfm_settings", "shared_secret", Sealing::FieldOfSection),
    ("lastfm_settings", "session_key", Sealing::FieldOfSection),
    ("spotify_settings", "client_secret", Sealing::FieldOfSection),
    ("events", "ticketmaster_api_key", Sealing::FieldOfSection),
    ("events", "skiddle_api_key", Sealing::FieldOfSection),
    ("wrapped_settings", "api_key", Sealing::FieldOfSection),
    ("oidc_settings", "client_secret", Sealing::FieldOfSection),
    (
        "library_settings",
        "acoustid_api_key",
        Sealing::FieldOfSection,
    ),
    ("advanced_settings", "audiodb_api_key", Sealing::PlainInV2),
];

#[cfg(test)]
mod tests {
    use super::*;

    /// Every settings key the server stores is either replaced by an import
    /// or named as left alone, and nothing else is. A section only v2 had is
    /// listed as removed, so it needs no v3 section.
    #[test]
    fn the_lists_cover_the_section_list() {
        use droppedneedle_kernel::runtime_config::KNOWN_TOP_LEVEL_KEYS;

        /// The keys an import leaves as they are.
        const LEFT_ALONE: &[&str] = &[
            // v3 only.
            "lyrics_settings",
            "public_address",
            "edition_preferences",
            "app_connect",
            // This install's own id.
            "instance_id",
        ];
        let mut named: Vec<&str> = replaced_sections()
            .chain(LEFT_ALONE.iter().copied())
            .collect();
        named.sort_unstable();
        let mut known = KNOWN_TOP_LEVEL_KEYS.to_vec();
        known.sort_unstable();
        assert_eq!(named, known);
        for section in removed_sections() {
            assert!(
                !known.contains(&section),
                "{section} is listed as removed but v3 stores it"
            );
        }
    }

    /// The exporter keeps writing every field it wrote when the lists were
    /// two, and the removed ones reach the importer, which reports them.
    #[test]
    fn the_exporter_still_writes_removed_fields() {
        for field in [
            "http_timeout",
            "genre_section_ttl",
            "cache_ttl_audiodb_found",
        ] {
            assert!(advanced_exported(field), "{field}");
        }
        for field in ["cache_ttl_recently_viewed_bytes", "unlisted_field"] {
            assert!(!advanced_exported(field), "{field}");
        }
        // Dropped on the way out, yet accepted in a file from elsewhere.
        assert!(is_removed_field(
            "advanced_settings",
            "cache_ttl_recently_viewed_bytes"
        ));
        assert!(internal_exported("brainzmash_consent_admin"));
        assert!(!INTERNAL_KEPT.contains(&"brainzmash_consent_admin"));
    }
}
