//! Runtime config secrets: the mask that keeps the stored value,
//! ciphertext at rest, fail-closed decryption, no secret in Debug output
//! or logs, typed errors instead of panics on bad sections.

use std::collections::HashSet;

use crate::common::ScratchDir;
use crate::common::logs::capture_logs;
use droppedneedle_kernel::runtime_config::crypto::Crypto;
use droppedneedle_kernel::runtime_config::mask::SECRET_MASK;
use droppedneedle_kernel::runtime_config::secret::Secret;
use droppedneedle_kernel::runtime_config::secret_sections::{
    AdvancedSettings, DownloadClients, EventsSettings, JellyfinConnection, LidarrImportConnection,
    ListenBrainzConnection, NavidromeConnection, NewznabIndexer, OidcConnection, PlexConnection,
    ProwlarrConnection, SlskdConnection, SpotifySettings, TypedLibrary, WrappedSettings,
    YouTubeConnection,
};
use droppedneedle_kernel::runtime_config::sections::{
    LibraryScanSchedule, MusicBrainzSettings, PluginConfig,
};
use droppedneedle_kernel::runtime_config::store::ConfigStore;
use droppedneedle_kernel::runtime_config::{ConfigError, DROPPED_SECTIONS};

fn test_dir(name: &str) -> ScratchDir {
    ScratchDir::new(&format!("config-{name}"))
}

fn test_store(name: &str) -> (ScratchDir, ConfigStore) {
    let dir = test_dir(name);
    let store = open_store(&dir, [42u8; 32]);
    (dir, store)
}

/// The store over the settings database in `dir` (made on first use),
/// copying in a `config.json` written there first.
fn open_store(dir: &std::path::Path, key: [u8; 32]) -> ConfigStore {
    ConfigStore::scratch(dir, Crypto::from_key_bytes(&key).unwrap()).unwrap()
}

/// Every byte the settings database holds on disk: the file and its WAL,
/// where a committed row can sit until a checkpoint.
fn stored_bytes(dir: &std::path::Path) -> String {
    let mut bytes = std::fs::read(dir.join("settings.db")).unwrap();
    bytes.extend(std::fs::read(dir.join("settings.db-wal")).unwrap_or_default());
    String::from_utf8_lossy(&bytes).into_owned()
}

/// A fresh install's MusicBrainz source id is minted once at boot and then
/// read back unchanged, also after a reopen. Clients key their caches on
/// it, so a new id per read made them refetch without end.
#[test]
fn musicbrainz_source_id_is_minted_once() {
    let (dir, store) = test_store("mb-source-id");
    store
        .seed_default_blocking::<MusicBrainzSettings>()
        .expect("default seeds");
    let first = store.get::<MusicBrainzSettings>().unwrap().source_id;
    assert_eq!(store.get::<MusicBrainzSettings>().unwrap().source_id, first);
    drop(store);
    let reopened = open_store(&dir, [42u8; 32]);
    reopened
        .seed_default_blocking::<MusicBrainzSettings>()
        .expect("seeding again keeps the stored section");
    assert_eq!(
        reopened.get::<MusicBrainzSettings>().unwrap().source_id,
        first
    );
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
            store.save_secret_blocking(incoming).unwrap();
            let masked: $ty = store.get_masked::<$ty>().unwrap().into_inner();
            assert_eq!(masked.$($field).+.expose(), $mask, "{label} masked");
            let raw = store.get_raw::<$ty>().unwrap();
            assert_eq!(raw.$($field).+.expose(), "live-secret-1", "{label} raw");
            let mut incoming = <$ty>::default();
            *incoming.$($field).+.expose_mut() = $mask.to_owned();
            store.save_secret_blocking(incoming).unwrap();
            let raw = store.get_raw::<$ty>().unwrap();
            assert_eq!(raw.$($field).+.expose(), "live-secret-1", "{label} mask keeps");
            let decoy = format!("{0}{0}-tail", $mask);
            let mut incoming = <$ty>::default();
            *incoming.$($field).+.expose_mut() = decoy.clone();
            store.save_secret_blocking(incoming).unwrap();
            let raw = store.get_raw::<$ty>().unwrap();
            assert_eq!(raw.$($field).+.expose(), decoy, "{label} prefix is new");
            let padded = format!("  {}  ", $mask);
            let mut incoming = <$ty>::default();
            *incoming.$($field).+.expose_mut() = padded.clone();
            store.save_secret_blocking(incoming).unwrap();
            let raw = store.get_raw::<$ty>().unwrap();
            let expected = if $strip { decoy } else { padded };
            assert_eq!(raw.$($field).+.expose(), expected, "{label} padded mask");
        }};
    }
    check!(SlskdConnection, api_key, SECRET_MASK, true, "");
    check!(DownloadClients, sabnzbd.api_key, SECRET_MASK, true, "");
    check!(ProwlarrConnection, api_key, SECRET_MASK, true, "");
    check!(LidarrImportConnection, api_key, SECRET_MASK, true, "");
    check!(JellyfinConnection, api_key, SECRET_MASK, false, "");
    check!(NavidromeConnection, password, SECRET_MASK, false, "");
    check!(PlexConnection, plex_token, SECRET_MASK, false, "");
    check!(ListenBrainzConnection, user_token, SECRET_MASK, false, "");
    check!(YouTubeConnection, api_key, SECRET_MASK, true, "");
    check!(SpotifySettings, client_secret, SECRET_MASK, false, "");
    check!(WrappedSettings, api_key, SECRET_MASK, true, "");
    check!(OidcConnection, client_secret, SECRET_MASK, false, "");
    check!(TypedLibrary, acoustid_api_key, SECRET_MASK, false, "");
    check!(
        AdvancedSettings,
        audiodb_api_key,
        SECRET_MASK,
        false,
        SECRET_MASK
    );
}

#[test]
fn events_masks_resolve_each_key_independently() {
    let (_dir, store) = test_store("events_mask");
    let mut incoming = EventsSettings::default();
    *incoming.ticketmaster_api_key.expose_mut() = "tm-live".to_owned();
    *incoming.skiddle_api_key.expose_mut() = "sk-live".to_owned();
    store.save_secret_blocking(incoming).unwrap();
    let mut incoming = EventsSettings::default();
    *incoming.ticketmaster_api_key.expose_mut() = SECRET_MASK.to_owned();
    *incoming.skiddle_api_key.expose_mut() = "sk-next".to_owned();
    store.save_secret_blocking(incoming).unwrap();
    let raw = store.get_raw::<EventsSettings>().unwrap();
    assert_eq!(raw.ticketmaster_api_key.expose(), "tm-live");
    assert_eq!(raw.skiddle_api_key.expose(), "sk-next");
    let masked: EventsSettings = store.get_masked::<EventsSettings>().unwrap().into_inner();
    assert_eq!(masked.ticketmaster_api_key.expose(), SECRET_MASK);
    assert_eq!(masked.skiddle_api_key.expose(), SECRET_MASK);
}

#[test]
fn secrets_rest_encrypted_and_never_plaintext_in_the_database() {
    let (dir, store) = test_store("at_rest");
    let mut slskd = SlskdConnection::default();
    *slskd.api_key.expose_mut() = "slskd-live-plaintext".to_owned();
    store.save_secret_blocking(slskd).unwrap();
    let mut advanced = AdvancedSettings::default();
    *advanced.audiodb_api_key.expose_mut() = "audiodb-live-plaintext".to_owned();
    store.save_secret_blocking(advanced).unwrap();
    let secrets: HashSet<String> = HashSet::from(["token".to_owned()]);
    let mut settings = std::collections::HashMap::new();
    settings.insert("token".to_owned(), "plugin-live-plaintext".to_owned());
    store
        .save_plugin_blocking(
            "demo",
            PluginConfig {
                enabled: true,
                settings,
            },
            &secrets,
        )
        .unwrap();
    store
        .save_indexer_blocking(NewznabIndexer {
            name: "first".to_owned(),
            url: "https://example.com".to_owned(),
            api_key: Secret::new("indexer-live-plaintext"),
            ..NewznabIndexer::default()
        })
        .unwrap();
    let body = stored_bytes(&dir);
    assert!(!body.contains("slskd-live-plaintext"));
    assert!(!body.contains("audiodb-live-plaintext"));
    assert!(!body.contains("plugin-live-plaintext"));
    assert!(!body.contains("indexer-live-plaintext"));
    assert!(body.contains("v3:"));
    let raw_slskd = store.get_raw::<SlskdConnection>().unwrap();
    assert_eq!(raw_slskd.api_key.expose(), "slskd-live-plaintext");
    let raw_advanced = store.get_raw::<AdvancedSettings>().unwrap();
    assert_eq!(
        raw_advanced.audiodb_api_key.expose(),
        "audiodb-live-plaintext"
    );
    let raw_plugin = store
        .get_plugin_raw("demo", &secrets)
        .unwrap()
        .into_settings();
    assert_eq!(raw_plugin["token"], "plugin-live-plaintext");
    let raw_indexers = store.get_indexers_raw().unwrap();
    assert_eq!(raw_indexers[0].api_key.expose(), "indexer-live-plaintext");
}

#[test]
fn empty_secret_stays_empty_and_clears() {
    let (_dir, store) = test_store("empty_secret");
    let mut incoming = PlexConnection::default();
    *incoming.plex_token.expose_mut() = "plex-live".to_owned();
    store.save_secret_blocking(incoming).unwrap();
    let cleared = PlexConnection::default();
    store.save_secret_blocking(cleared).unwrap();
    let masked: PlexConnection = store.get_masked::<PlexConnection>().unwrap().into_inner();
    assert_eq!(masked.plex_token.expose(), "");
    let raw = store.get_raw::<PlexConnection>().unwrap();
    assert_eq!(raw.plex_token.expose(), "");
}

#[test]
fn wrong_key_fails_closed_never_legacy_passthrough() {
    let (dir, store) = test_store("wrong_key");
    let mut incoming = SlskdConnection::default();
    *incoming.api_key.expose_mut() = "slskd-live".to_owned();
    store.save_secret_blocking(incoming).unwrap();
    drop(store);
    let store = open_store(&dir, [9u8; 32]);
    let result = store.get_raw::<SlskdConnection>();
    assert!(matches!(
        result,
        Err(ConfigError::Crypto(
            droppedneedle_kernel::runtime_config::crypto::CryptoError::DecryptFailed
        ))
    ));
}

#[test]
fn secrets_never_appear_in_debug_output() {
    let (_dir, store) = test_store("debug_redaction");
    let mut slskd = SlskdConnection::default();
    *slskd.api_key.expose_mut() = "debug-live-slskd".to_owned();
    store.save_secret_blocking(slskd).unwrap();
    let mut events = EventsSettings::default();
    *events.ticketmaster_api_key.expose_mut() = "debug-live-tm".to_owned();
    store.save_secret_blocking(events).unwrap();
    let raw_slskd = store.get_raw::<SlskdConnection>().unwrap();
    let masked_slskd: SlskdConnection = store.get_masked::<SlskdConnection>().unwrap().into_inner();
    let raw_events = store.get_raw::<EventsSettings>().unwrap();
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
        assert!(!shown.contains(SECRET_MASK), "mask leaked: {shown}");
    }
}

#[test]
fn secrets_never_reach_structured_logs() {
    let (_dir, store) = test_store("log_capture");
    let (_, text) = capture_logs(|| {
        let mut slskd = SlskdConnection::default();
        *slskd.api_key.expose_mut() = "log-live-slskd-secret".to_owned();
        store.save_secret_blocking(slskd).unwrap();
        let _ = store.get_raw::<SlskdConnection>().unwrap();
        let _: SlskdConnection = store.get_masked::<SlskdConnection>().unwrap().into_inner();
        tracing::info!("round-trip done");
    });
    assert!(text.contains("round-trip done"));
    assert!(!text.contains("log-live-slskd-secret"), "leaked: {text}");
    assert!(!text.contains(SECRET_MASK), "mask leaked: {text}");
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
    let store = open_store(&dir, [42u8; 32]);
    let result = store.get_raw::<YouTubeConnection>();
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
        .save_indexer_blocking(NewznabIndexer {
            name: "first".to_owned(),
            url: "example.com".to_owned(),
            api_key: Secret::new("indexer-live-1"),
            ..NewznabIndexer::default()
        })
        .unwrap();
    assert_eq!(id.len(), 32);
    let masked = store.get_indexers().unwrap();
    assert_eq!(masked.len(), 1);
    assert_eq!(masked[0].api_key.expose(), SECRET_MASK);
    assert_eq!(masked[0].url, "https://example.com");
    let raw = store.get_indexers_raw().unwrap();
    assert_eq!(raw[0].api_key.expose(), "indexer-live-1");
    store
        .save_indexer_blocking(NewznabIndexer {
            id: id.clone(),
            name: "renamed".to_owned(),
            url: "https://example.com".to_owned(),
            api_key: Secret::new(SECRET_MASK),
            ..NewznabIndexer::default()
        })
        .unwrap();
    let raw = store.get_indexers_raw().unwrap();
    assert_eq!(raw.len(), 1);
    assert_eq!(raw[0].name, "renamed");
    assert_eq!(raw[0].api_key.expose(), "indexer-live-1");
    let second = store
        .save_indexer_blocking(NewznabIndexer {
            name: "second".to_owned(),
            url: "https://two.example".to_owned(),
            api_key: Secret::new("indexer-live-2"),
            priority: 5,
            ..NewznabIndexer::default()
        })
        .unwrap();
    store
        .reorder_indexers_blocking(&[second.clone(), id.clone()])
        .unwrap();
    let raw = store.get_indexers_raw().unwrap();
    assert_eq!(raw[0].id, second);
    assert_eq!(raw[0].priority, 1);
    assert_eq!(raw[1].priority, 2);
    store.delete_indexer_blocking(&id).unwrap();
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
        .save_plugin_blocking(
            "demo",
            droppedneedle_kernel::runtime_config::sections::PluginConfig {
                enabled: true,
                settings,
            },
            &secrets,
        )
        .unwrap();
    let masked = store.get_plugin_masked("demo", &secrets).unwrap();
    assert_eq!(masked.settings["token"], SECRET_MASK);
    assert_eq!(masked.settings["nick"], "plain");
    let raw = store
        .get_plugin_raw("demo", &secrets)
        .unwrap()
        .into_settings();
    assert_eq!(raw["token"], "plugin-live-token");
    let mut settings = std::collections::HashMap::new();
    settings.insert("token".to_owned(), SECRET_MASK.to_owned());
    settings.insert("nick".to_owned(), "renamed".to_owned());
    store
        .save_plugin_blocking(
            "demo",
            droppedneedle_kernel::runtime_config::sections::PluginConfig {
                enabled: true,
                settings,
            },
            &secrets,
        )
        .unwrap();
    let raw = store
        .get_plugin_raw("demo", &secrets)
        .unwrap()
        .into_settings();
    assert_eq!(raw["token"], "plugin-live-token");
    assert_eq!(raw["nick"], "renamed");
}

#[test]
fn musicbrainz_official_clamp_and_mirror_rules() {
    let (_dir, store) = test_store("musicbrainz");
    let settings = MusicBrainzSettings {
        source_mode: droppedneedle_kernel::runtime_config::sections::MbSourceMode::Mirror,
        api_url: "https://musicbrainz.org/ws/2".to_owned(),
        rate_limit: 50.0,
        ..Default::default()
    };
    let saved = store.save_blocking(settings).unwrap();
    assert_eq!(saved.rate_limit, 1.0);
    assert!(saved.clamped_to_official_limits);
    let reread: MusicBrainzSettings = store.get().unwrap();
    assert_eq!(reread.rate_limit, 1.0);
    assert!(!reread.clamped_to_official_limits);
    let settings = MusicBrainzSettings {
        source_mode: droppedneedle_kernel::runtime_config::sections::MbSourceMode::Mirror,
        api_url: "not-a-url".to_owned(),
        ..Default::default()
    };
    assert!(matches!(
        store.save_blocking(settings),
        Err(ConfigError::Validation {
            field: "api_url",
            ..
        })
    ));
}

#[test]
fn dropped_and_unknown_keys_left_behind_never_read() {
    let dir = test_dir("dropped");
    std::fs::write(
        dir.join("config.json"),
        r#"{"local_files_settings": {"enabled": true},
            "_legacy_lidarr": {},
            "jellyfin_url": "http://x",
            "future_section": {}}"#,
    )
    .unwrap();
    let store = open_store(&dir, [42u8; 32]);
    let db = rusqlite::Connection::open(dir.join("settings.db")).unwrap();
    let left: String = db
        .query_row("SELECT keys_left FROM config_ingests", [], |row| row.get(0))
        .unwrap();
    let left: Vec<String> = serde_json::from_str(&left).unwrap();
    for key in [
        "_legacy_lidarr",
        "future_section",
        "jellyfin_url",
        "local_files_settings",
    ] {
        assert!(
            left.contains(&key.to_owned()),
            "{key} left behind: {left:?}"
        );
    }
    let rows: i64 = db
        .query_row("SELECT COUNT(*) FROM config_sections", [], |row| row.get(0))
        .unwrap();
    assert_eq!(rows, 0);
    assert!(
        DROPPED_SECTIONS
            .iter()
            .any(|dropped| dropped.key == "jellyfin_url")
    );
    let prefs: droppedneedle_kernel::runtime_config::sections::UserPreferences =
        store.get().unwrap();
    assert_eq!(prefs.primary_types, vec!["album", "ep", "single"]);
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
