//! Runtime config secrets: per-section masks that keep the stored value,
//! ciphertext at rest, fail-closed decryption, no secret in Debug output
//! or logs, typed errors instead of panics on bad files, and the YouTube
//! quota file.

use std::collections::HashSet;
use std::sync::{Arc, Mutex};

use crate::common::ScratchDir;
use droppedneedle::runtime_config::crypto::Crypto;
use droppedneedle::runtime_config::mask::{
    ACOUSTID_KEY_MASK, AUDIODB_API_KEY_MASK, INDEXER_API_KEY_MASK, JELLYFIN_API_KEY_MASK,
    LIDARR_API_KEY_MASK, LISTENBRAINZ_TOKEN_MASK, NAVIDROME_PASSWORD_MASK, OIDC_SECRET_MASK,
    PLEX_TOKEN_MASK, PLUGIN_SECRET_MASK, PROWLARR_API_KEY_MASK, SABNZBD_API_KEY_MASK,
    SKIDDLE_KEY_MASK, SLSKD_API_KEY_MASK, SPOTIFY_SECRET_MASK, TICKETMASTER_KEY_MASK,
    WRAPPED_API_KEY_MASK, YOUTUBE_API_KEY_MASK,
};
use droppedneedle::runtime_config::quota::{QuotaError, QuotaStore};
use droppedneedle::runtime_config::secret::Secret;
use droppedneedle::runtime_config::secret_sections::{
    AdvancedSettings, DownloadClients, EventsSettings, JellyfinConnection, LidarrImportConnection,
    ListenBrainzConnection, NavidromeConnection, NewznabIndexer, OidcConnection, PlexConnection,
    ProwlarrConnection, SlskdConnection, SpotifySettings, TypedLibrary, WrappedSettings,
    YouTubeConnection,
};
use droppedneedle::runtime_config::sections::{
    LibraryScanSchedule, MusicBrainzSettings, PluginConfig,
};
use droppedneedle::runtime_config::store::{ConfigStore, INSTANCE_ID_KEY};
use droppedneedle::runtime_config::{ConfigError, DROPPED_SECTIONS};

fn test_dir(name: &str) -> ScratchDir {
    ScratchDir::new(&format!("config-{name}"))
}

fn test_store(name: &str) -> (ScratchDir, ConfigStore) {
    let dir = test_dir(name);
    let crypto = Crypto::from_key_bytes(&[42u8; 32]).unwrap();
    let store = ConfigStore::open(&dir.join("config.json"), crypto).unwrap();
    (dir, store)
}

fn read_config(dir: &std::path::Path) -> String {
    std::fs::read_to_string(dir.join("config.json")).unwrap()
}

/// Every secret section's mask lifecycle: unset shows `fresh`, a new
/// secret round-trips and masks exactly, the mask keeps the stored value,
/// a value that merely starts with the mask is a new secret, and a padded
/// mask keeps (`strip`) or is stored verbatim.
#[test]
fn every_secret_section_masks_and_keeps() {
    let (_dir, store) = test_store("masks");
    macro_rules! check {
        ($ty:ty, $($field:ident).+, $mask:expr, $strip:expr, $fresh:expr) => {{
            let label = stringify!($ty);
            let masked: $ty = store.get_masked::<$ty>().unwrap().into_inner();
            assert_eq!(masked.$($field).+.expose(), $fresh, "{label} fresh");
            let mut incoming = <$ty>::default();
            *incoming.$($field).+.expose_mut() = "live-secret-1".to_owned();
            store.save_secret(incoming).unwrap();
            let masked: $ty = store.get_masked::<$ty>().unwrap().into_inner();
            assert_eq!(masked.$($field).+.expose(), $mask, "{label} masked");
            let raw: $ty = store.get_raw().unwrap();
            assert_eq!(raw.$($field).+.expose(), "live-secret-1", "{label} raw");
            let mut incoming = <$ty>::default();
            *incoming.$($field).+.expose_mut() = $mask.to_owned();
            store.save_secret(incoming).unwrap();
            let raw: $ty = store.get_raw().unwrap();
            assert_eq!(raw.$($field).+.expose(), "live-secret-1", "{label} mask keeps");
            let decoy = format!("{0}{0}-tail", $mask);
            let mut incoming = <$ty>::default();
            *incoming.$($field).+.expose_mut() = decoy.clone();
            store.save_secret(incoming).unwrap();
            let raw: $ty = store.get_raw().unwrap();
            assert_eq!(raw.$($field).+.expose(), decoy, "{label} prefix is new");
            let padded = format!("  {}  ", $mask);
            let mut incoming = <$ty>::default();
            *incoming.$($field).+.expose_mut() = padded.clone();
            store.save_secret(incoming).unwrap();
            let raw: $ty = store.get_raw().unwrap();
            let expected = if $strip { decoy } else { padded };
            assert_eq!(raw.$($field).+.expose(), expected, "{label} padded mask");
        }};
    }
    check!(SlskdConnection, api_key, SLSKD_API_KEY_MASK, true, "");
    check!(
        DownloadClients,
        sabnzbd.api_key,
        SABNZBD_API_KEY_MASK,
        true,
        ""
    );
    check!(ProwlarrConnection, api_key, PROWLARR_API_KEY_MASK, true, "");
    check!(
        LidarrImportConnection,
        api_key,
        LIDARR_API_KEY_MASK,
        true,
        ""
    );
    check!(
        JellyfinConnection,
        api_key,
        JELLYFIN_API_KEY_MASK,
        false,
        ""
    );
    check!(
        NavidromeConnection,
        password,
        NAVIDROME_PASSWORD_MASK,
        false,
        ""
    );
    check!(PlexConnection, plex_token, PLEX_TOKEN_MASK, false, "");
    check!(
        ListenBrainzConnection,
        user_token,
        LISTENBRAINZ_TOKEN_MASK,
        false,
        ""
    );
    check!(YouTubeConnection, api_key, YOUTUBE_API_KEY_MASK, true, "");
    check!(
        SpotifySettings,
        client_secret,
        SPOTIFY_SECRET_MASK,
        false,
        ""
    );
    check!(WrappedSettings, api_key, WRAPPED_API_KEY_MASK, true, "");
    check!(OidcConnection, client_secret, OIDC_SECRET_MASK, false, "");
    check!(TypedLibrary, acoustid_api_key, ACOUSTID_KEY_MASK, false, "");
    check!(
        AdvancedSettings,
        audiodb_api_key,
        AUDIODB_API_KEY_MASK,
        false,
        AUDIODB_API_KEY_MASK
    );
}

#[test]
fn events_masks_resolve_each_key_independently() {
    let (_dir, store) = test_store("events_mask");
    let mut incoming = EventsSettings::default();
    *incoming.ticketmaster_api_key.expose_mut() = "tm-live".to_owned();
    *incoming.skiddle_api_key.expose_mut() = "sk-live".to_owned();
    store.save_secret(incoming).unwrap();
    let mut incoming = EventsSettings::default();
    *incoming.ticketmaster_api_key.expose_mut() = TICKETMASTER_KEY_MASK.to_owned();
    *incoming.skiddle_api_key.expose_mut() = "sk-next".to_owned();
    store.save_secret(incoming).unwrap();
    let raw: EventsSettings = store.get_raw().unwrap();
    assert_eq!(raw.ticketmaster_api_key.expose(), "tm-live");
    assert_eq!(raw.skiddle_api_key.expose(), "sk-next");
    let masked: EventsSettings = store.get_masked::<EventsSettings>().unwrap().into_inner();
    assert_eq!(masked.ticketmaster_api_key.expose(), TICKETMASTER_KEY_MASK);
    assert_eq!(masked.skiddle_api_key.expose(), SKIDDLE_KEY_MASK);
}

#[test]
fn secrets_rest_encrypted_and_never_plaintext_in_file() {
    let (dir, store) = test_store("at_rest");
    let mut slskd = SlskdConnection::default();
    *slskd.api_key.expose_mut() = "slskd-live-plaintext".to_owned();
    store.save_secret(slskd).unwrap();
    let mut advanced = AdvancedSettings::default();
    *advanced.audiodb_api_key.expose_mut() = "audiodb-live-plaintext".to_owned();
    store.save_secret(advanced).unwrap();
    let secrets: HashSet<String> = HashSet::from(["token".to_owned()]);
    let mut settings = std::collections::HashMap::new();
    settings.insert("token".to_owned(), "plugin-live-plaintext".to_owned());
    store
        .save_plugin(
            "demo",
            PluginConfig {
                enabled: true,
                settings,
            },
            &secrets,
        )
        .unwrap();
    store
        .save_indexer(NewznabIndexer {
            name: "first".to_owned(),
            url: "https://example.com".to_owned(),
            api_key: Secret::new("indexer-live-plaintext"),
            ..NewznabIndexer::default()
        })
        .unwrap();
    let body = read_config(&dir);
    assert!(!body.contains("slskd-live-plaintext"));
    assert!(!body.contains("audiodb-live-plaintext"));
    assert!(!body.contains("plugin-live-plaintext"));
    assert!(!body.contains("indexer-live-plaintext"));
    assert!(body.contains("v3:"));
    let raw_slskd: SlskdConnection = store.get_raw().unwrap();
    assert_eq!(raw_slskd.api_key.expose(), "slskd-live-plaintext");
    let raw_advanced: AdvancedSettings = store.get_raw().unwrap();
    assert_eq!(
        raw_advanced.audiodb_api_key.expose(),
        "audiodb-live-plaintext"
    );
    let raw_plugin = store.get_plugin_raw("demo", &secrets).unwrap();
    assert_eq!(raw_plugin.settings["token"], "plugin-live-plaintext");
    let raw_indexers = store.get_indexers_raw().unwrap();
    assert_eq!(raw_indexers[0].api_key.expose(), "indexer-live-plaintext");
}

#[test]
fn empty_secret_stays_empty_and_clears() {
    let (_dir, store) = test_store("empty_secret");
    let mut incoming = PlexConnection::default();
    *incoming.plex_token.expose_mut() = "plex-live".to_owned();
    store.save_secret(incoming).unwrap();
    let cleared = PlexConnection::default();
    store.save_secret(cleared).unwrap();
    let masked: PlexConnection = store.get_masked::<PlexConnection>().unwrap().into_inner();
    assert_eq!(masked.plex_token.expose(), "");
    let raw: PlexConnection = store.get_raw().unwrap();
    assert_eq!(raw.plex_token.expose(), "");
}

#[test]
fn wrong_key_fails_closed_never_legacy_passthrough() {
    let (dir, store) = test_store("wrong_key");
    let mut incoming = SlskdConnection::default();
    *incoming.api_key.expose_mut() = "slskd-live".to_owned();
    store.save_secret(incoming).unwrap();
    drop(store);
    let other = Crypto::from_key_bytes(&[9u8; 32]).unwrap();
    let store = ConfigStore::open(&dir.join("config.json"), other).unwrap();
    let result: Result<SlskdConnection, ConfigError> = store.get_raw();
    assert!(matches!(
        result,
        Err(ConfigError::Crypto(
            droppedneedle::runtime_config::crypto::CryptoError::DecryptFailed
        ))
    ));
}

#[test]
fn secrets_never_appear_in_debug_output() {
    let (_dir, store) = test_store("debug_redaction");
    let mut slskd = SlskdConnection::default();
    *slskd.api_key.expose_mut() = "debug-live-slskd".to_owned();
    store.save_secret(slskd).unwrap();
    let mut events = EventsSettings::default();
    *events.ticketmaster_api_key.expose_mut() = "debug-live-tm".to_owned();
    store.save_secret(events).unwrap();
    let raw_slskd: SlskdConnection = store.get_raw().unwrap();
    let masked_slskd: SlskdConnection = store.get_masked::<SlskdConnection>().unwrap().into_inner();
    let raw_events: EventsSettings = store.get_raw().unwrap();
    let crypto = Crypto::from_key_bytes(&[1u8; 32]).unwrap();
    for shown in [
        format!("{raw_slskd:?}"),
        format!("{masked_slskd:?}"),
        format!("{raw_events:?}"),
        format!("{crypto:?}"),
        format!("{store:?}"),
    ] {
        assert!(!shown.contains("debug-live-slskd"), "leaked: {shown}");
        assert!(!shown.contains("debug-live-tm"), "leaked: {shown}");
        assert!(!shown.contains(SLSKD_API_KEY_MASK), "mask leaked: {shown}");
        assert!(
            !shown.contains(TICKETMASTER_KEY_MASK),
            "mask leaked: {shown}"
        );
    }
}

#[test]
fn secrets_never_reach_structured_logs() {
    struct Capture(Arc<Mutex<Vec<u8>>>);
    impl std::io::Write for Capture {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let buffer = Arc::new(Mutex::new(Vec::new()));
    let subscriber = tracing_subscriber::fmt()
        .with_writer({
            let buffer = buffer.clone();
            move || Capture(buffer.clone())
        })
        .with_max_level(tracing::level_filters::LevelFilter::TRACE)
        .finish();
    let (_dir, store) = test_store("log_capture");
    tracing::dispatcher::with_default(&tracing::Dispatch::new(subscriber), || {
        let mut slskd = SlskdConnection::default();
        *slskd.api_key.expose_mut() = "log-live-slskd-secret".to_owned();
        store.save_secret(slskd).unwrap();
        let _: SlskdConnection = store.get_raw().unwrap();
        let _: SlskdConnection = store.get_masked::<SlskdConnection>().unwrap().into_inner();
        tracing::info!("round-trip done");
    });
    let text = String::from_utf8(buffer.lock().unwrap().clone()).unwrap();
    assert!(text.contains("round-trip done"));
    assert!(!text.contains("log-live-slskd-secret"), "leaked: {text}");
    assert!(!text.contains(SLSKD_API_KEY_MASK), "mask leaked: {text}");
}

#[test]
fn wrong_shapes_fail_typed_not_panics() {
    let dir = test_dir("shapes");
    std::fs::write(
        dir.join("config.json"),
        r#"{"youtube_settings": {"daily_quota_limit": "lots"},
            "scan_frequency": "x",
            "library_scan_schedule": {"scan_frequency": "fortnightly"}}"#,
    )
    .unwrap();
    let crypto = Crypto::from_key_bytes(&[42u8; 32]).unwrap();
    let store = ConfigStore::open(&dir.join("config.json"), crypto).unwrap();
    let result: Result<YouTubeConnection, ConfigError> = store.get_raw();
    assert!(matches!(
        result,
        Err(ConfigError::SectionDecode {
            section: "youtube_settings",
            ..
        })
    ));
    let result: Result<LibraryScanSchedule, ConfigError> = store.get();
    assert!(matches!(
        result,
        Err(ConfigError::SectionDecode {
            section: "library_scan_schedule",
            ..
        })
    ));
}

#[test]
fn indexer_crud_matches_masks_by_id() {
    let (_dir, store) = test_store("indexers");
    let id = store
        .save_indexer(NewznabIndexer {
            name: "first".to_owned(),
            url: "example.com".to_owned(),
            api_key: Secret::new("indexer-live-1"),
            ..NewznabIndexer::default()
        })
        .unwrap();
    assert_eq!(id.len(), 32);
    let masked = store.get_indexers().unwrap();
    assert_eq!(masked.len(), 1);
    assert_eq!(masked[0].api_key.expose(), INDEXER_API_KEY_MASK);
    assert_eq!(masked[0].url, "https://example.com");
    let raw = store.get_indexers_raw().unwrap();
    assert_eq!(raw[0].api_key.expose(), "indexer-live-1");
    store
        .save_indexer(NewznabIndexer {
            id: id.clone(),
            name: "renamed".to_owned(),
            url: "https://example.com".to_owned(),
            api_key: Secret::new(INDEXER_API_KEY_MASK),
            ..NewznabIndexer::default()
        })
        .unwrap();
    let raw = store.get_indexers_raw().unwrap();
    assert_eq!(raw.len(), 1);
    assert_eq!(raw[0].name, "renamed");
    assert_eq!(raw[0].api_key.expose(), "indexer-live-1");
    let second = store
        .save_indexer(NewznabIndexer {
            name: "second".to_owned(),
            url: "https://two.example".to_owned(),
            api_key: Secret::new("indexer-live-2"),
            priority: 5,
            ..NewznabIndexer::default()
        })
        .unwrap();
    store
        .reorder_indexers(&[second.clone(), id.clone()])
        .unwrap();
    let raw = store.get_indexers_raw().unwrap();
    assert_eq!(raw[0].id, second);
    assert_eq!(raw[0].priority, 1);
    assert_eq!(raw[1].priority, 2);
    store.delete_indexer(&id).unwrap();
    let raw = store.get_indexers_raw().unwrap();
    assert_eq!(raw.len(), 1);
    assert_eq!(raw[0].id, second);
}

#[test]
fn plugin_secrets_encrypted_and_mask_resolved() {
    let (_dir, store) = test_store("plugins");
    let secrets: HashSet<String> = HashSet::from(["token".to_owned()]);
    let mut settings = std::collections::HashMap::new();
    settings.insert("token".to_owned(), "plugin-live-token".to_owned());
    settings.insert("nick".to_owned(), "plain".to_owned());
    store
        .save_plugin(
            "demo",
            droppedneedle::runtime_config::sections::PluginConfig {
                enabled: true,
                settings,
            },
            &secrets,
        )
        .unwrap();
    let masked = store.get_plugin_masked("demo", &secrets).unwrap();
    assert_eq!(masked.settings["token"], PLUGIN_SECRET_MASK);
    assert_eq!(masked.settings["nick"], "plain");
    let raw = store.get_plugin_raw("demo", &secrets).unwrap();
    assert_eq!(raw.settings["token"], "plugin-live-token");
    let mut settings = std::collections::HashMap::new();
    settings.insert("token".to_owned(), PLUGIN_SECRET_MASK.to_owned());
    settings.insert("nick".to_owned(), "renamed".to_owned());
    store
        .save_plugin(
            "demo",
            droppedneedle::runtime_config::sections::PluginConfig {
                enabled: true,
                settings,
            },
            &secrets,
        )
        .unwrap();
    let raw = store.get_plugin_raw("demo", &secrets).unwrap();
    assert_eq!(raw.settings["token"], "plugin-live-token");
    assert_eq!(raw.settings["nick"], "renamed");
}

#[test]
fn musicbrainz_official_clamp_and_mirror_rules() {
    let (_dir, store) = test_store("musicbrainz");
    let settings = MusicBrainzSettings {
        source_mode: droppedneedle::runtime_config::sections::MbSourceMode::Mirror,
        api_url: "https://musicbrainz.org/ws/2".to_owned(),
        rate_limit: 50.0,
        concurrent_searches: 32,
        ..Default::default()
    };
    let saved = store.save(settings).unwrap();
    assert_eq!(saved.rate_limit, 1.0);
    assert_eq!(saved.concurrent_searches, 6);
    assert!(saved.clamped_to_official_limits);
    let reread: MusicBrainzSettings = store.get().unwrap();
    assert_eq!(reread.rate_limit, 1.0);
    assert!(!reread.clamped_to_official_limits);
    let settings = MusicBrainzSettings {
        source_mode: droppedneedle::runtime_config::sections::MbSourceMode::Mirror,
        api_url: "not-a-url".to_owned(),
        ..Default::default()
    };
    assert!(matches!(
        store.save(settings),
        Err(ConfigError::Validation {
            field: "api_url",
            ..
        })
    ));
}

#[test]
fn instance_id_minted_once_and_stable() {
    let (_dir, store) = test_store("instance");
    assert_eq!(store.instance_id().unwrap(), "");
    let first = store.ensure_instance_id().unwrap();
    assert_eq!(first.len(), 36);
    assert_eq!(store.ensure_instance_id().unwrap(), first);
    assert_eq!(store.instance_id().unwrap(), first);
    let file: serde_json::Value = serde_json::from_str(&read_config(&_dir)).unwrap();
    assert_eq!(file[INSTANCE_ID_KEY], serde_json::Value::String(first));
}

#[test]
fn dropped_and_unknown_keys_reported_never_read() {
    let dir = test_dir("dropped");
    std::fs::write(
        dir.join("config.json"),
        r#"{"local_files_settings": {"enabled": true},
            "_legacy_lidarr": {},
            "jellyfin_url": "http://x",
            "future_section": {}}"#,
    )
    .unwrap();
    let crypto = Crypto::from_key_bytes(&[42u8; 32]).unwrap();
    let store = ConfigStore::open(&dir.join("config.json"), crypto).unwrap();
    let dropped = store.dropped_sections_present().unwrap();
    assert!(dropped.contains(&"local_files_settings"));
    assert!(dropped.contains(&"_legacy_lidarr"));
    assert!(dropped.contains(&"jellyfin_url"));
    let unknown = store.unknown_top_level_keys().unwrap();
    assert!(unknown.contains(&"future_section".to_owned()));
    assert!(!DROPPED_SECTIONS.is_empty());
    let prefs: droppedneedle::runtime_config::sections::UserPreferences = store.get().unwrap();
    assert_eq!(prefs.primary_types, vec!["album", "ep", "single"]);
}

#[test]
fn quota_reserve_refund_and_limit() {
    let dir = test_dir("quota_basic");
    let path = dir.join("youtube_quota.json");
    let today = Arc::new(Mutex::new("2026-09-28".to_owned()));
    let clock = {
        let today = today.clone();
        Arc::new(move || Ok::<String, QuotaError>(today.lock().unwrap().clone()))
    };
    let store = QuotaStore::open_with_clock(&path, clock).unwrap();
    assert_eq!(store.count().unwrap(), 0);
    let status = store.status(2).unwrap();
    assert_eq!((status.used, status.remaining), (0, 2));
    let date = store.reserve(2).unwrap();
    assert_eq!(date, "2026-09-28");
    assert_eq!(store.count().unwrap(), 1);
    store.reserve(2).unwrap();
    assert_eq!(store.count().unwrap(), 2);
    assert_eq!(store.reserve(2), Err(QuotaError::RateLimited));
    let file: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(
        file["date"],
        serde_json::Value::String("2026-09-28".to_owned())
    );
    assert_eq!(file["count"], serde_json::Value::Number(2.into()));
    assert!(std::fs::read_dir(&dir).unwrap().count() == 1);
    store.refund("2026-09-28").unwrap();
    assert_eq!(store.count().unwrap(), 1);
    store.refund("2026-09-27").unwrap();
    assert_eq!(store.count().unwrap(), 1);
}

#[test]
fn quota_stale_date_reads_zero_and_rolls_over() {
    let dir = test_dir("quota_rollover");
    let path = dir.join("youtube_quota.json");
    std::fs::write(&path, r#"{"date": "2026-09-27", "count": 9}"#).unwrap();
    let today = Arc::new(Mutex::new("2026-09-28".to_owned()));
    let clock = {
        let today = today.clone();
        Arc::new(move || Ok::<String, QuotaError>(today.lock().unwrap().clone()))
    };
    let store = QuotaStore::open_with_clock(&path, clock).unwrap();
    assert_eq!(store.count().unwrap(), 0);
    store.reserve(80).unwrap();
    assert_eq!(store.count().unwrap(), 1);
    let file: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(
        file["date"],
        serde_json::Value::String("2026-09-28".to_owned())
    );
    assert_eq!(file["count"], serde_json::Value::Number(1.into()));
}

/// Debug output of every secret section, plugin settings included, hides
/// decrypted secrets.
#[test]
fn every_secret_section_debug_redacts() {
    fn assert_redacted<T: std::fmt::Debug>(value: T, secret: &str) {
        let shown = format!("{value:?}");
        assert!(!shown.contains(secret), "leaked in {shown}");
    }
    let mut slskd = SlskdConnection::default();
    *slskd.api_key.expose_mut() = "redact-me".to_owned();
    assert_redacted(slskd, "redact-me");
    let mut jellyfin = JellyfinConnection::default();
    *jellyfin.api_key.expose_mut() = "redact-me".to_owned();
    assert_redacted(jellyfin, "redact-me");
    let mut navidrome = NavidromeConnection::default();
    *navidrome.password.expose_mut() = "redact-me".to_owned();
    assert_redacted(navidrome, "redact-me");
    let mut plex = PlexConnection::default();
    *plex.plex_token.expose_mut() = "redact-me".to_owned();
    assert_redacted(plex, "redact-me");
    let mut listenbrainz = ListenBrainzConnection::default();
    *listenbrainz.user_token.expose_mut() = "redact-me".to_owned();
    assert_redacted(listenbrainz, "redact-me");
    let mut youtube = YouTubeConnection::default();
    *youtube.api_key.expose_mut() = "redact-me".to_owned();
    assert_redacted(youtube, "redact-me");
    let mut spotify = SpotifySettings::default();
    *spotify.client_secret.expose_mut() = "redact-me".to_owned();
    assert_redacted(spotify, "redact-me");
    let mut events = EventsSettings::default();
    *events.ticketmaster_api_key.expose_mut() = "redact-me".to_owned();
    *events.skiddle_api_key.expose_mut() = "redact-me".to_owned();
    assert_redacted(events, "redact-me");
    let mut wrapped = WrappedSettings::default();
    *wrapped.api_key.expose_mut() = "redact-me".to_owned();
    assert_redacted(wrapped, "redact-me");
    let mut oidc = OidcConnection::default();
    *oidc.client_secret.expose_mut() = "redact-me".to_owned();
    assert_redacted(oidc, "redact-me");
    let mut library = TypedLibrary::default();
    *library.acoustid_api_key.expose_mut() = "redact-me".to_owned();
    assert_redacted(library, "redact-me");
    let mut advanced = AdvancedSettings::default();
    *advanced.audiodb_api_key.expose_mut() = "redact-me".to_owned();
    assert_redacted(advanced, "redact-me");
    let mut prowlarr = ProwlarrConnection::default();
    *prowlarr.api_key.expose_mut() = "redact-me".to_owned();
    assert_redacted(prowlarr, "redact-me");
    let mut lidarr = LidarrImportConnection::default();
    *lidarr.api_key.expose_mut() = "redact-me".to_owned();
    assert_redacted(lidarr, "redact-me");
    let mut clients = DownloadClients::default();
    *clients.sabnzbd.api_key.expose_mut() = "redact-me".to_owned();
    assert_redacted(clients, "redact-me");
    let mut indexer = NewznabIndexer::default();
    *indexer.api_key.expose_mut() = "redact-me".to_owned();
    assert_redacted(indexer, "redact-me");
    let mut plugin_settings = std::collections::HashMap::new();
    plugin_settings.insert("token".to_owned(), "redact-me".to_owned());
    plugin_settings.insert("nick".to_owned(), "plain".to_owned());
    let shown = format!(
        "{:?}",
        PluginConfig {
            enabled: true,
            settings: plugin_settings,
        }
    );
    assert!(!shown.contains("redact-me"), "leaked in {shown}");
    assert!(!shown.contains("plain"), "leaked in {shown}");
    assert!(shown.contains("token"), "keys must stay visible: {shown}");
}
