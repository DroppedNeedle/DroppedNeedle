//! Settings live in the database: two saves at once both land, an old
//! `config.json` is copied in once and never again, a start that cannot
//! trust what it finds refuses or leaves the file unread, and a database
//! backup carries the settings.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Barrier, Mutex};

use axum::http::StatusCode;
use serde_json::{Value, json};

use crate::common::logs::capture_logs;
use crate::common::{ScratchDir, TestApp, app_settings, closed_outbound, config_store};
use droppedneedle::bootstrap::{self, BootError, data_dirs::BootRefusal};
use droppedneedle_kernel::config::AppConfig;
use droppedneedle_kernel::db::DurableLane;
use droppedneedle_kernel::runtime_config::secret_sections::{
    LidarrImportConnection, NewznabIndexer, WrappedSettings,
};
use droppedneedle_kernel::runtime_config::sections::{
    DownloadPolicy, FilesystemWatcher, InternalState, PluginConfig, Plugins, UserPreferences,
};
use droppedneedle_kernel::runtime_config::{
    ConfigError, Crypto, Secret, Section as _, UpdateError,
};

/// Every settings row's key, revision and change time, in key order.
fn revisions(config: &AppConfig) -> Vec<(String, i64, i64)> {
    let db = rusqlite::Connection::open(&config.library_db_path).unwrap();
    let mut statement = db
        .prepare("SELECT key, revision, updated_at FROM config_sections ORDER BY key")
        .unwrap();
    statement
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
}

/// The ingest record's outcome and the keys it left behind.
fn ingest_record(config: &AppConfig) -> Option<(String, Vec<String>)> {
    let db = rusqlite::Connection::open(&config.library_db_path).unwrap();
    db.query_row("SELECT outcome, keys_left FROM config_ingests", [], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })
    .ok()
    .map(|(outcome, left)| (outcome, serde_json::from_str(&left).unwrap()))
}

/// Every byte of the database on disk, its WAL included.
fn database_bytes(config: &AppConfig) -> String {
    let mut bytes = std::fs::read(&config.library_db_path).unwrap();
    let mut wal = config.library_db_path.as_os_str().to_owned();
    wal.push("-wal");
    bytes.extend(std::fs::read(wal).unwrap_or_default());
    String::from_utf8_lossy(&bytes).into_owned()
}

fn legacy_file(config: &AppConfig) -> std::path::PathBuf {
    config.config_dir().join("config.json")
}

/// W3.8: two saves of the same section at once both land. Each save reads
/// the stored row inside its own write, so neither overwrites the other.
#[test]
fn two_saves_at_once_both_land() {
    let dir = ScratchDir::new("settings-parallel");
    let store = config_store(&dir);
    let rounds = 12;
    for round in 0..rounds {
        let barrier = Arc::new(Barrier::new(2));
        let savers: Vec<_> = ["a", "b"]
            .into_iter()
            .map(|side| {
                let store = store.clone();
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    barrier.wait();
                    store
                        .save_indexer_blocking(NewznabIndexer {
                            name: format!("{side}-{round}"),
                            url: "https://indexer.example".to_owned(),
                            api_key: Secret::new("indexer-key"),
                            ..NewznabIndexer::default()
                        })
                        .unwrap();
                    barrier.wait();
                    store
                        .save_plugin_blocking(
                            &format!("{side}-{round}"),
                            PluginConfig {
                                enabled: true,
                                settings: HashMap::new(),
                            },
                            &HashSet::new(),
                        )
                        .unwrap();
                })
            })
            .collect();
        for saver in savers {
            saver.join().unwrap();
        }
    }
    assert_eq!(store.get_indexers_raw().unwrap().len(), 2 * rounds);
    let plugins: serde_json::Value =
        serde_json::from_str(&store.stored_text(Plugins::KEY).unwrap()).unwrap();
    assert_eq!(plugins.as_object().unwrap().len(), 2 * rounds);
}

/// A stored number reads back exactly: from the snapshot, inside a write
/// (where a compare-and-swap decodes the row) and from a store opened
/// again. A number one step off breaks every revision compared against it.
#[test]
fn a_stored_number_reads_back_exactly_everywhere() {
    const EXACT: f64 = 1792386784.7085721;
    let dir = ScratchDir::new("settings-float");
    let store = config_store(&dir);
    store
        .save_blocking(FilesystemWatcher {
            poll_interval_seconds: EXACT,
            ..FilesystemWatcher::default()
        })
        .unwrap();
    assert_eq!(
        store
            .get::<FilesystemWatcher>()
            .unwrap()
            .poll_interval_seconds,
        EXACT
    );
    let mut in_write = None;
    store
        .update_blocking::<FilesystemWatcher, ConfigError, _>(DurableLane::Foreground, |stored| {
            in_write = Some(stored.poll_interval_seconds);
            Ok(stored)
        })
        .map_err(|error| match error {
            UpdateError::Rejected(error) | UpdateError::Config(error) => error,
        })
        .unwrap();
    assert_eq!(in_write, Some(EXACT));
    drop(store);
    let reopened = config_store(&dir);
    assert_eq!(
        reopened
            .get::<FilesystemWatcher>()
            .unwrap()
            .poll_interval_seconds,
        EXACT
    );
}

/// A v3 `config.json` from before settings moved into the database:
/// a plain section, a secret section, an indexer, a plugin with a secret,
/// the instance id, the install ids and one key v3 does not know.
fn old_config(crypto: &Crypto) -> Value {
    let indexer = NewznabIndexer {
        id: "indexer-1".to_owned(),
        name: "one".to_owned(),
        url: "https://indexer.example".to_owned(),
        api_key: Secret::new(crypto.encrypt("indexer-plaintext").unwrap()),
        priority: 1,
        ..NewznabIndexer::default()
    };
    json!({
        "user_preferences": {"primary_types": ["album"], "secondary_types": ["studio"]},
        "lidarr_import": {
            "url": "http://lidarr.lan:8686",
            "api_key": crypto.encrypt("lidarr-plaintext").unwrap(),
        },
        "indexers": [indexer],
        "plugins": {"toy": {"enabled": true, "settings": {
            "token": crypto.encrypt("plugin-plaintext").unwrap(),
            "nick": "toybrick",
        }}},
        "instance_id": "instance-from-the-file",
        "_internal": {"plex_client_id": "plex-from-the-file", "droppedneedle_device_id": "device-from-the-file"},
        "future_section": {"x": 1},
    })
}

/// The ingest: the first start copies every known section in unchanged
/// and leaves the file as it was; the next start writes nothing; a file
/// changed by hand afterwards is never read; a database with no settings
/// and no record (a backup from before this change) takes the file again.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn config_json_is_copied_in_once() {
    let file_text = Arc::new(Mutex::new(String::new()));
    let placed = file_text.clone();
    let app = TestApp::start_prepared("settings-ingest", move |config| {
        let (crypto, _) = Crypto::load_or_generate(&config.config_dir()).unwrap();
        let text = serde_json::to_string_pretty(&old_config(&crypto)).unwrap();
        std::fs::write(legacy_file(config), &text).unwrap();
        *placed.lock().unwrap() = text;
    })
    .await;
    let config = app.config.clone();
    let text = file_text.lock().unwrap().clone();
    let mode = |config: &AppConfig| {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::metadata(legacy_file(config))
            .unwrap()
            .permissions()
            .mode()
    };
    let placed_mode = mode(&config);

    let (app, first) = app.while_stopped(revisions).await;
    let (app, ()) = app
        .while_stopped(|config| {
            let store = app_settings(config);
            let preferences: UserPreferences = store.get().unwrap();
            assert_eq!(preferences.primary_types, ["album"]);
            assert_eq!(preferences.secondary_types, ["studio"]);
            let lidarr = store.get_raw::<LidarrImportConnection>().unwrap();
            assert_eq!(lidarr.url, "http://lidarr.lan:8686");
            assert_eq!(lidarr.api_key.expose(), "lidarr-plaintext");
            let indexers = store.get_indexers_raw().unwrap();
            assert_eq!(indexers.len(), 1);
            assert_eq!(indexers[0].api_key.expose(), "indexer-plaintext");
            let secret = HashSet::from(["token".to_owned()]);
            let plugin = store
                .get_plugin_raw("toy", &secret)
                .unwrap()
                .into_settings();
            assert_eq!(plugin["token"], "plugin-plaintext");
            assert_eq!(plugin["nick"], "toybrick");
            let internal: InternalState = store.get().unwrap();
            assert_eq!(
                internal.plex_client_id.as_deref(),
                Some("plex-from-the-file")
            );
            let instance: String = rusqlite::Connection::open(&config.library_db_path)
                .unwrap()
                .query_row(
                    "SELECT value_json FROM config_sections WHERE key = 'instance_id'",
                    [],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(instance, "\"instance-from-the-file\"");
            assert_eq!(std::fs::read_to_string(legacy_file(config)).unwrap(), text);
            assert_eq!(mode(config), placed_mode);
            let (outcome, left) = ingest_record(config).unwrap();
            assert_eq!(outcome, "ingested");
            assert_eq!(left, ["future_section"]);
            assert!(
                revisions(config)
                    .iter()
                    .all(|(key, _, _)| key != "future_section")
            );
        })
        .await;

    // The second start writes nothing.
    let (app, second) = app.while_stopped(revisions).await;
    assert_eq!(second, first);

    // A file changed by hand once settings are stored is never read, even
    // one that would refuse the start.
    let (app, ()) = app
        .while_stopped(|config| std::fs::write(legacy_file(config), "{not json").unwrap())
        .await;
    let (app, third) = app.while_stopped(revisions).await;
    assert_eq!(third, first);

    // A database with no settings and no record, as a backup from before
    // this change leaves it, takes the file again.
    let (app, ()) = app
        .while_stopped(|config| {
            std::fs::write(legacy_file(config), &text).unwrap();
            let db = rusqlite::Connection::open(&config.library_db_path).unwrap();
            db.execute_batch("DELETE FROM config_sections; DELETE FROM config_ingests;")
                .unwrap();
        })
        .await;
    let (app, again) = app
        .while_stopped(|config| {
            let preferences: UserPreferences = app_settings(config).get().unwrap();
            assert_eq!(preferences.primary_types, ["album"]);
            ingest_record(config)
        })
        .await;
    assert_eq!(
        again.map(|(outcome, _)| outcome).as_deref(),
        Some("ingested")
    );
    app.stop().await;
}

/// The copy logs key names and counts, never a value: neither a secret's
/// plaintext nor its ciphertext reaches the log, at any level.
#[test]
fn the_copy_logs_no_secret() {
    let dir = ScratchDir::new("settings-ingest-log");
    let crypto = Crypto::from_key_bytes(&[7u8; 32]).unwrap();
    let config = old_config(&crypto);
    let ciphertext = config["lidarr_import"]["api_key"]
        .as_str()
        .unwrap()
        .to_owned();
    std::fs::write(dir.join("config.json"), config.to_string()).unwrap();
    let (store, text) = capture_logs(|| config_store(&dir));
    let lidarr = store.get_raw::<LidarrImportConnection>().unwrap();
    assert_eq!(lidarr.api_key.expose(), "lidarr-plaintext");
    assert!(text.contains("future_section"), "{text}");
    for secret in ["lidarr-plaintext", "indexer-plaintext", "plugin-plaintext"] {
        assert!(!text.contains(secret), "leaked {secret}: {text}");
    }
    assert!(!text.contains(&ciphertext), "leaked ciphertext: {text}");
}

/// A start on an empty data directory and its own boot, stopped again.
async fn boot_once(config: &AppConfig) -> Result<(), BootError> {
    let bootstrap::Built {
        router,
        state,
        background,
    } = bootstrap::build_with(config.clone(), closed_outbound()).await?;
    drop(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let stopped = bootstrap::serve(listener, router, background, async {}).await;
    assert!(stopped.outcome.is_ok(), "{:?}", stopped.outcome);
    Ok(())
}

/// A `config.json` beside a key file a start had to create is not one this
/// server wrote (it may be v2's, with plaintext plugin secrets). The skip is
/// settled before the key exists, so a first start that fails between
/// creating the key and opening the settings still never takes the file:
/// the next start records the skip and stores nothing from it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_file_beside_a_new_key_is_never_read_even_after_a_failed_start() {
    let dir = ScratchDir::new("settings-new-key");
    let mut config = AppConfig::with_root(0, &dir);
    std::fs::create_dir_all(config.config_dir()).unwrap();
    std::fs::write(
        legacy_file(&config),
        json!({
            "user_preferences": {"primary_types": ["album"]},
            "plugins": {"toy": {"enabled": true, "settings": {"token": "v2-plaintext-token"}}},
        })
        .to_string(),
    )
    .unwrap();
    // The first start fails after it has created the key and the database,
    // before the settings open: its web UI cannot take the base path.
    std::fs::create_dir_all(&config.static_dir).unwrap();
    std::fs::write(config.static_dir.join("index.html"), "<html></html>").unwrap();
    config.base_path = "/music".to_owned();
    let Err(failed) = bootstrap::build_with(config.clone(), closed_outbound()).await else {
        panic!("the first start must fail before the settings open");
    };
    assert!(matches!(failed, BootError::Web(_)), "{failed:?}");
    assert!(config.config_dir().join("data_enc.key").exists());
    assert!(revisions(&config).is_empty());

    config.base_path = String::new();
    std::fs::remove_dir_all(&config.static_dir).unwrap();
    boot_once(&config).await.unwrap();
    let (outcome, _) = ingest_record(&config).expect("the skip is recorded");
    assert_eq!(outcome, "skipped_new_key");
    let stored = revisions(&config);
    assert!(
        stored
            .iter()
            .all(|(key, _, _)| key != "user_preferences" && key != "plugins"),
        "{stored:?}"
    );
    assert!(!database_bytes(&config).contains("v2-plaintext-token"));
    assert!(!config.config_dir().join("config-json-skipped").exists());

    boot_once(&config).await.unwrap();
    assert_eq!(revisions(&config), stored);
}

/// A database that holds no settings and no accounts needs no key: a dry
/// run of the import into an empty folder leaves one behind. The server
/// starts on it and creates the key.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_empty_database_without_a_key_starts() {
    let dir = ScratchDir::new("settings-empty-db");
    let config = AppConfig::with_root(0, &dir);
    std::fs::create_dir_all(config.config_dir()).unwrap();
    std::fs::create_dir_all(&config.cache_dir).unwrap();
    crate::common::migrated_db(&config.library_db_path);

    boot_once(&config).await.unwrap();
    assert!(config.config_dir().join("data_enc.key").exists());
}

/// A `config.json` that does not read, found where settings would be copied
/// from, stops the start with the path and writes no row.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_config_json_that_does_not_read_refuses_the_start() {
    let dir = ScratchDir::new("settings-bad-file");
    let config = AppConfig::with_root(0, &dir);
    std::fs::create_dir_all(config.config_dir()).unwrap();
    std::fs::create_dir_all(&config.cache_dir).unwrap();
    crate::common::migrated_db(&config.library_db_path);
    Crypto::load_or_generate(&config.config_dir()).unwrap();
    std::fs::write(legacy_file(&config), "{not json").unwrap();

    let Err(refused) = bootstrap::build_with(config.clone(), closed_outbound()).await else {
        panic!("a config.json that does not read must refuse the start");
    };
    assert!(
        matches!(
            refused,
            BootError::Stage {
                stage: "settings",
                ..
            }
        ),
        "{refused:?}"
    );
    let message = refused.to_string();
    assert!(
        message.contains(&legacy_file(&config).display().to_string()),
        "{message}"
    );
    assert!(message.contains("move it away"), "{message}");
    assert!(revisions(&config).is_empty());
    assert!(ingest_record(&config).is_none());
}

/// A database that holds this server's data, sealed values among it and no
/// key check yet, with its key file gone, refuses the start instead of
/// taking a new key that could read none of its secrets.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_database_whose_key_is_gone_refuses_the_start() {
    let dir = ScratchDir::new("settings-lost-key");
    let config = AppConfig::with_root(0, &dir);
    std::fs::create_dir_all(config.config_dir()).unwrap();
    std::fs::create_dir_all(&config.cache_dir).unwrap();
    crate::common::migrated_db(&config.library_db_path);
    rusqlite::Connection::open(&config.library_db_path)
        .unwrap()
        .execute(
            "INSERT INTO config_sections (key, value_json, revision, updated_at) \
             VALUES ('wrapped_settings', '{\"api_key\": \"v3:c2VhbGVkLWJ5LXRoZS1sb3N0LWtleQ==\"}', 1, 1)",
            [],
        )
        .unwrap();

    let Err(refused) = bootstrap::build_with(config.clone(), closed_outbound()).await else {
        panic!("a database without its key must refuse the start");
    };
    assert!(
        matches!(refused, BootError::DataDir(BootRefusal::KeyMissing { .. })),
        "{refused:?}"
    );
    assert!(refused.to_string().contains("CONFIG_DIR"), "{refused}");
    assert!(!config.config_dir().join("data_enc.key").exists());
}

/// The stored check value for the data key, if any.
fn key_check(config: &AppConfig) -> Option<String> {
    rusqlite::Connection::open(&config.library_db_path)
        .unwrap()
        .query_row(
            "SELECT value_json FROM config_sections WHERE key = 'key_check'",
            [],
            |row| row.get(0),
        )
        .ok()
}

/// A data directory whose database was made before the key check existed:
/// migrated, with a key file and one account, and no check stored.
fn unchecked_instance(name: &str) -> (ScratchDir, AppConfig, Crypto) {
    let dir = ScratchDir::new(name);
    let config = AppConfig::with_root(0, &dir);
    std::fs::create_dir_all(config.config_dir()).unwrap();
    std::fs::create_dir_all(&config.cache_dir).unwrap();
    crate::common::migrated_db(&config.library_db_path);
    let (crypto, _) = Crypto::load_or_generate(&config.config_dir()).unwrap();
    rusqlite::Connection::open(&config.library_db_path)
        .unwrap()
        .execute(
            "INSERT INTO auth_users (id, display_name, role, created_at) \
             VALUES ('u1', 'U', 'admin', '2026-01-01T00:00:00+00:00')",
            [],
        )
        .unwrap();
    (dir, config, crypto)
}

/// A key file that is not the database's own refuses the start and is left
/// as it was; with the key file gone, the stored check still refuses and no
/// key is minted.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_key_that_does_not_belong_refuses_the_start() {
    let dir = ScratchDir::new("settings-wrong-key");
    let config = AppConfig::with_root(0, &dir);
    boot_once(&config).await.unwrap();
    assert!(key_check(&config).is_some());

    let key = config.config_dir().join("data_enc.key");
    std::fs::remove_file(&key).unwrap();
    Crypto::mint_file(&key).unwrap();
    let wrong = std::fs::read(&key).unwrap();
    let Err(refused) = bootstrap::build_with(config.clone(), closed_outbound()).await else {
        panic!("a key from elsewhere must refuse the start");
    };
    assert!(
        matches!(refused, BootError::DataDir(BootRefusal::KeyMismatch { .. })),
        "{refused:?}"
    );
    assert!(refused.to_string().contains("reset-key"), "{refused}");
    assert_eq!(std::fs::read(&key).unwrap(), wrong);

    std::fs::remove_file(&key).unwrap();
    let Err(refused) = bootstrap::build_with(config.clone(), closed_outbound()).await else {
        panic!("a database with a check and no key must refuse the start");
    };
    assert!(
        matches!(refused, BootError::DataDir(BootRefusal::KeyMissing { .. })),
        "{refused:?}"
    );
    assert!(!key.exists());
}

/// The first start after the check arrived, on a database holding a link
/// sealed whole (not JSON), accepts its own key and records the check.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_whole_sealed_link_lets_the_right_key_start() {
    let (_dir, config, crypto) = unchecked_instance("settings-check-link");
    rusqlite::Connection::open(&config.library_db_path)
        .unwrap()
        .execute(
            "INSERT INTO user_connections (user_id, service, connection_data, created_at, \
             updated_at) VALUES ('u1', 'plex', ?1, 'now', 'now')",
            [crypto.encrypt(r#"{"token":"plex-token"}"#).unwrap()],
        )
        .unwrap();

    boot_once(&config).await.unwrap();
    assert_eq!(
        key_check(&config),
        Some(serde_json::to_string(&crypto.check_value()).unwrap())
    );
}

/// One stale value that opens under no key does not refuse the right key,
/// as long as another sealed value opens under it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_stale_value_beside_a_good_one_lets_the_right_key_start() {
    let (_dir, config, crypto) = unchecked_instance("settings-check-stale");
    let stale = Crypto::from_key_bytes(&[9u8; 32])
        .unwrap()
        .encrypt("stale")
        .unwrap();
    let db = rusqlite::Connection::open(&config.library_db_path).unwrap();
    db.execute(
        "INSERT INTO connect_app_passwords (id, user_id, name, secret_sha256, \
         secret_encrypted, created_at) VALUES ('p1', 'u1', 'phone', 'sha', ?1, 'now')",
        [stale],
    )
    .unwrap();
    db.execute(
        "INSERT INTO user_connections (user_id, service, connection_data, created_at, \
         updated_at) VALUES ('u1', 'jellyfin', ?1, 'now', 'now')",
        [crypto.encrypt(r#"{"token":"jellyfin-token"}"#).unwrap()],
    )
    .unwrap();

    boot_once(&config).await.unwrap();
    assert!(key_check(&config).is_some());
}

/// W3.8: a backup carries the settings. A secret saved over HTTP, backed up
/// through the backup service and restored into an empty data directory
/// beside the key file reads back at the next start.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_restored_backup_brings_its_settings() {
    let source = TestApp::start("settings-backup-source").await;
    let admin = source.login_admin().await;
    let auth = [("authorization", admin.as_str())];
    let (status, body) = source
        .call(
            "PUT",
            "/api/v3/settings/wrapped",
            &auth,
            Some(json!({"api_key": "wrapped-plaintext"})),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, run) = source
        .call("POST", "/api/v3/admin/backups", &auth, None)
        .await;
    assert_eq!(status, StatusCode::CREATED, "{run}");
    let backup = source
        .config
        .cache_dir
        .join("backups")
        .join(run["name"].as_str().unwrap());
    let key = source.config.config_dir().join("data_enc.key");

    let restored = TestApp::start_prepared("settings-backup-target", |config| {
        std::fs::remove_file(&config.library_db_path).unwrap();
        std::fs::copy(&backup, &config.library_db_path).unwrap();
        std::fs::copy(&key, config.config_dir().join("data_enc.key")).unwrap();
    })
    .await;
    source.stop().await;
    let (restored, wrapped) = restored
        .while_stopped(|config| app_settings(config).get_raw::<WrappedSettings>().unwrap())
        .await;
    assert_eq!(wrapped.api_key.expose(), "wrapped-plaintext");
    restored.stop().await;
}

/// An older `config.json` wrote `target_bitrate_kbps` on every recipe step.
/// The copy drops that removed field, so the download policy still reads.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_old_recipe_step_still_reads_after_the_copy() {
    let app = TestApp::start_prepared("settings-ingest-recipe", |config| {
        Crypto::load_or_generate(&config.config_dir()).unwrap();
        let file = json!({"download_policy": {
            "flac_mp3_only": true,
            "quality_recipe": [
                {"format": "flac", "quality": "cd", "min_bitrate_kbps": null,
                 "target_bitrate_kbps": null, "max_bitrate_kbps": null,
                 "bit_depth": null, "sample_rate_hz": null},
                {"format": "mp3", "quality": "320_plus", "min_bitrate_kbps": 320,
                 "target_bitrate_kbps": 320, "max_bitrate_kbps": null,
                 "bit_depth": null, "sample_rate_hz": null},
            ],
        }});
        std::fs::write(legacy_file(config), file.to_string()).unwrap();
    })
    .await;
    let (_app, ()) = app
        .while_stopped(|config| {
            let policy: DownloadPolicy = app_settings(config).get().expect("the policy reads");
            assert_eq!(policy.quality_recipe.len(), 2);
            assert_eq!(policy.quality_recipe[1].min_bitrate_kbps, Some(320));
            let stored: String = rusqlite::Connection::open(&config.library_db_path)
                .unwrap()
                .query_row(
                    "SELECT value_json FROM config_sections WHERE key = 'download_policy'",
                    [],
                    |row| row.get(0),
                )
                .unwrap();
            assert!(!stored.contains("target_bitrate_kbps"), "{stored}");
        })
        .await;
}
