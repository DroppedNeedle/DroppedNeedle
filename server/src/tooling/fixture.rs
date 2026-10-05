//! Scratch v2 instance builder for the pipeline tests.
//!
//! [`build_v2_fixture`] lays out a standard v2 `<ROOT_APP_DIR>` carrying
//! every migrated entity plus the conflict and deleted-ID cases the plan
//! requires: two users (local bcrypt logins, an opaque Plex binding,
//! active + revoked + legacy-plaintext app passwords, a recovery code),
//! follows and approvals (including rows under an unknown user id and an
//! approval reviewed by an unknown user), kept/partial settings sections
//! (Fernet secrets, plaintext AudioDB/plugin secrets, ordered indexers),
//! and a legacy `library_sync_settings` section with no new schedule so
//! the one-shot scan-schedule carry fires.
//!
//! The deleted-ID rows are cascade violations a healthy v2 database cannot
//! hold (v2 enforces its foreign keys); they simulate a corrupt instance.
//! [`delete_orphan_rows`] is the operator repair: drop the violations from
//! v2 and re-export. The pipeline E2E runs the validator against the file
//! both before (dangling references flagged, import refused) and after
//! the repair (clean file, import proceeds).
//!
//! Tests only: nothing here ships in the server binary paths.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use rusqlite::{Connection, params};
use serde_json::{Map, Value, json};
use thiserror::Error;

/// User id no fixture user holds; orphan rows hang under it. Not
/// UUID-shaped on purpose: it only ever compares for equality, never for shape.
pub const UNKNOWN_USER_ID: &str = "fixture-unknown-user";

/// Fresh random id in UUID shape (user ids, MBIDs, instance id).
fn fresh_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

/// Plaintext credentials the fixture mints. Tests assert the pipeline
/// round-trips these without ever writing them to disk outside v2 rest.
#[derive(Debug, Clone)]
pub struct FixtureSecrets {
    /// Alice's v2 password (bcrypt at rest, rehashed at first login).
    pub alice_password: String,
    /// Bob's v2 password.
    pub bob_password: String,
    /// Alice's active app password (Fernet at v2 rest).
    pub alice_phone_secret: String,
    /// Alice's revoked app password (still sealed in the export).
    pub alice_old_secret: String,
    /// Bob's legacy-plaintext app password (passthrough path).
    pub bob_legacy_secret: String,
    /// slskd API key (Fernet at v2 rest).
    pub slskd_key: String,
    /// AudioDB key (plaintext at v2 rest; sealed at export).
    pub audiodb_key: String,
    /// Indexer API keys in priority order (Fernet at v2 rest).
    pub indexer_keys: Vec<String>,
    /// Plugin secret value (plaintext at v2 rest; sealed at export).
    pub plugin_token: String,
    /// Preferred bitrate, a plain plugin value that travels verbatim.
    pub plugin_theme: String,
}

/// One built fixture: its root plus the plaintexts to assert against.
#[derive(Debug, Clone)]
pub struct V2Fixture {
    /// v2 `<ROOT_APP_DIR>` holding `config/` and `cache/`.
    pub root: PathBuf,
    /// Plaintexts minted for this fixture.
    pub secrets: FixtureSecrets,
    /// Alice's user id (the admin login).
    pub alice_id: String,
    /// Bob's user id (trusted, opaque Plex binding).
    pub bob_id: String,
    /// First followed artist's MBID.
    pub artist_one_mbid: String,
    /// Second followed artist's MBID.
    pub artist_two_mbid: String,
    /// Instance id, carried verbatim end to end.
    pub instance_id: String,
}

/// Fixture build failures.
#[derive(Debug, Error)]
pub enum FixtureError {
    /// Filesystem failure under the fixture root.
    #[error("fixture io failed: {0}")]
    Io(String),
    /// Fixture database failure.
    #[error("fixture database failed: {0}")]
    Db(String),
    /// Password hashing failed.
    #[error("fixture hashing failed: {0}")]
    Hash(String),
    /// Fernet key handling failed.
    #[error("fixture key failed: {0}")]
    Key(String),
}

/// Build the fixture v2 instance under `root`.
pub fn build_v2_fixture(root: &Path) -> Result<V2Fixture, FixtureError> {
    let secrets = FixtureSecrets {
        alice_password: "alice-v2-pass".to_owned(),
        bob_password: "bob-v2-pass".to_owned(),
        alice_phone_secret: "alice-phone-plaintext".to_owned(),
        alice_old_secret: "alice-old-plaintext".to_owned(),
        bob_legacy_secret: "bob-legacy-plaintext".to_owned(),
        slskd_key: "slskd-key-plaintext".to_owned(),
        audiodb_key: "audiodb-plaintext-key".to_owned(),
        indexer_keys: vec![
            "indexer-one-plaintext".to_owned(),
            "indexer-two-plaintext".to_owned(),
        ],
        plugin_token: "plugin-secret-plaintext".to_owned(),
        plugin_theme: "dark".to_owned(),
    };
    let config_dir = root.join("config");
    let cache_dir = root.join("cache");
    let plugins_dir = root.join("plugins").join("demo");
    for dir in [&config_dir, &cache_dir, &plugins_dir] {
        std::fs::create_dir_all(dir).map_err(|error| FixtureError::Io(error.to_string()))?;
    }
    let fernet = crate::export::fernet::FernetKey::generate()
        .map_err(|error| FixtureError::Key(error.to_string()))?;
    std::fs::write(
        config_dir.join(".env"),
        format!("DATA_ENC_KEY={}\n", fernet.to_base64()),
    )
    .map_err(|error| FixtureError::Io(error.to_string()))?;
    std::fs::write(
        plugins_dir.join("plugin.toml"),
        "[plugin]\nname = \"demo\"\n\n[[settings]]\nkey = \"api_token\"\nsecret = true\n\n[[settings]]\nkey = \"theme\"\nsecret = false\n",
    )
    .map_err(|error| FixtureError::Io(error.to_string()))?;
    let ids = FixtureIds {
        alice_id: fresh_id(),
        bob_id: fresh_id(),
        artist_one_mbid: fresh_id(),
        artist_two_mbid: fresh_id(),
        instance_id: fresh_id(),
    };
    let config = fixture_config(&fernet, &secrets, &ids)?;
    std::fs::write(
        config_dir.join("config.json"),
        serde_json::to_string_pretty(&config)
            .map_err(|error| FixtureError::Io(error.to_string()))?,
    )
    .map_err(|error| FixtureError::Io(error.to_string()))?;
    build_v2_db(&cache_dir.join("library.db"), &fernet, &secrets, &ids)?;
    Ok(V2Fixture {
        root: root.to_owned(),
        secrets,
        alice_id: ids.alice_id,
        bob_id: ids.bob_id,
        artist_one_mbid: ids.artist_one_mbid,
        artist_two_mbid: ids.artist_two_mbid,
        instance_id: ids.instance_id,
    })
}

/// Random ids minted per fixture build.
struct FixtureIds {
    alice_id: String,
    bob_id: String,
    artist_one_mbid: String,
    artist_two_mbid: String,
    instance_id: String,
}

/// One fixture approval row: owned ids, static display text.
struct ApprovalRow {
    user_id: String,
    mbid: String,
    name: &'static str,
    state: &'static str,
    requested_at: f64,
    by_id: Option<String>,
    by_name: Option<&'static str>,
    reviewed_at: Option<f64>,
    batch_id: Option<&'static str>,
    source: Option<&'static str>,
}

/// Operator repair for the deleted-ID case: drop follows and approvals
/// whose user id has no `auth_users` row. Returns rows removed.
pub fn delete_orphan_rows(db_path: &Path) -> Result<usize, FixtureError> {
    let db = Connection::open(db_path).map_err(|error| FixtureError::Db(error.to_string()))?;
    let follows = db
        .execute(
            "DELETE FROM user_followed_artists WHERE user_id NOT IN (SELECT id FROM auth_users)",
            [],
        )
        .map_err(|error| FixtureError::Db(error.to_string()))?;
    let approvals = db
        .execute(
            "DELETE FROM auto_download_approvals WHERE user_id NOT IN (SELECT id FROM auth_users)",
            [],
        )
        .map_err(|error| FixtureError::Db(error.to_string()))?;
    Ok(follows + approvals)
}

/// v2 `config.json`: kept sections, partial sections with kept + dropped
/// fields, secret positions in v2 rest form, and the legacy sync section
/// (never exported; the scan-schedule carry reads it directly).
fn fixture_config(
    fernet: &crate::export::fernet::FernetKey,
    secrets: &FixtureSecrets,
    ids: &FixtureIds,
) -> Result<Map<String, Value>, FixtureError> {
    let seal = |plaintext: &str| {
        fernet
            .encrypt(plaintext)
            .map_err(|error| FixtureError::Key(error.to_string()))
    };
    let mut config = Map::new();
    config.insert("instance_id".to_owned(), json!(ids.instance_id));
    config.insert(
        "user_preferences".to_owned(),
        json!({"primary_types": ["Album"], "secondary_types": []}),
    );
    config.insert(
        "download_client".to_owned(),
        json!({"host": "127.0.0.1:5030", "api_key": seal(&secrets.slskd_key)?}),
    );
    config.insert(
        "download_clients".to_owned(),
        json!({"sabnzbd": {"host": "127.0.0.1:8080", "api_key": seal("sabnzbd-key-plaintext")?}}),
    );
    let mut advanced = HashMap::from([
        ("http_timeout".to_owned(), json!(30)),
        ("discover_queue_size".to_owned(), json!(25)),
        ("audiodb_enabled".to_owned(), json!(true)),
        // Plaintext at v2 rest (the bug export closes by sealing).
        ("audiodb_api_key".to_owned(), json!(secrets.audiodb_key)),
        // Dropped fields: never cross into the export.
        ("artist_discovery_warm_interval".to_owned(), json!(60)),
        ("audiodb_prewarm_delay".to_owned(), json!(5)),
    ]);
    advanced.insert("cache_ttl_audiodb_found".to_owned(), json!(7200));
    advanced.insert("frontend_ttl_covers".to_owned(), json!(300));
    config.insert("advanced_settings".to_owned(), json!(advanced));
    config.insert(
        "indexers".to_owned(),
        json!([
            {"name": "first", "host": "https://one.example", "api_key": seal(&secrets.indexer_keys[0])?},
            {"name": "second", "host": "https://two.example", "api_key": seal(&secrets.indexer_keys[1])?},
        ]),
    );
    config.insert(
        "plugins".to_owned(),
        json!({"demo": {"enabled": true, "settings": {
            "api_token": secrets.plugin_token,
            "theme": secrets.plugin_theme,
        }}}),
    );
    config.insert(
        "lastfm_settings".to_owned(),
        json!({
            "enabled": true,
            "api_key": "lastfm-public-key",
            "shared_secret": seal("lastfm-shared-plaintext")?,
            "session_key": seal("lastfm-session-plaintext")?,
        }),
    );
    config.insert(
        "musicbrainz_settings".to_owned(),
        json!({
            "source_mode": "official",
            "api_url": "https://musicbrainz.org",
            "rate_limit": 1,
            "pending_brainzmash": {"proposal": "transient"},
        }),
    );
    config.insert(
        "_internal".to_owned(),
        json!({
            "plex_client_id": "plex-client-fixture",
            "audiodb_sweep_cursor": "dropped-cursor",
        }),
    );
    config.insert(
        "download_policy".to_owned(),
        json!({
            "quality_min": "mp3_320",
            "quality_max": "lossless",
            "quality_recipe": [],
            "quality_recipe_status": "v1",
        }),
    );
    config.insert("wanted".to_owned(), json!({"enabled": true}));
    // Legacy section: excluded from the export stream, read by the
    // scan-schedule carry straight from this file.
    config.insert(
        "library_sync_settings".to_owned(),
        json!({"sync_frequency": "6hr", "last_sync": 1_700_000_000}),
    );
    Ok(config)
}

/// v2 `library.db`: current v2 auth/follows shape with the fixture rows.
/// Foreign keys stay off while the orphan rows insert (they simulate a
/// corrupt instance), then switch on for everything after.
fn build_v2_db(
    db_path: &Path,
    fernet: &crate::export::fernet::FernetKey,
    secrets: &FixtureSecrets,
    ids: &FixtureIds,
) -> Result<(), FixtureError> {
    let db = Connection::open(db_path).map_err(|error| FixtureError::Db(error.to_string()))?;
    db.execute_batch(
        "PRAGMA foreign_keys=OFF;
         CREATE TABLE auth_users (
             id TEXT PRIMARY KEY, display_name TEXT NOT NULL, email TEXT UNIQUE,
             avatar_url TEXT, role TEXT NOT NULL DEFAULT 'user',
             created_at TEXT NOT NULL, last_login_at TEXT,
             username TEXT, username_display TEXT);
         CREATE UNIQUE INDEX idx_auth_users_username
             ON auth_users(username) WHERE username IS NOT NULL;
         CREATE TABLE auth_providers (
             id TEXT PRIMARY KEY,
             user_id TEXT NOT NULL REFERENCES auth_users(id) ON DELETE CASCADE,
             provider TEXT NOT NULL, provider_uid TEXT NOT NULL,
             provider_data TEXT, created_at TEXT NOT NULL,
             UNIQUE (provider, provider_uid));
         CREATE TABLE connect_app_passwords (
             id TEXT PRIMARY KEY,
             user_id TEXT NOT NULL REFERENCES auth_users(id) ON DELETE CASCADE,
             name TEXT NOT NULL, secret_sha256 TEXT NOT NULL UNIQUE,
             secret_encrypted TEXT NOT NULL, created_at TEXT NOT NULL,
             last_used_at TEXT, last_client TEXT,
             revoked INTEGER NOT NULL DEFAULT 0);
         CREATE TABLE auth_password_recovery_codes (
             user_id TEXT PRIMARY KEY REFERENCES auth_users(id) ON DELETE CASCADE,
             code_hash TEXT NOT NULL UNIQUE, created_at TEXT NOT NULL,
             expires_at TEXT NOT NULL);
         CREATE TABLE user_followed_artists (
             user_id TEXT NOT NULL REFERENCES auth_users(id) ON DELETE CASCADE,
             artist_mbid TEXT NOT NULL, artist_mbid_lower TEXT NOT NULL,
             artist_name TEXT NOT NULL, auto_download INTEGER NOT NULL DEFAULT 0,
             followed_at REAL NOT NULL, updated_at REAL NOT NULL,
             PRIMARY KEY (user_id, artist_mbid_lower));
         CREATE TABLE auto_download_approvals (
             user_id TEXT NOT NULL REFERENCES auth_users(id) ON DELETE CASCADE,
             artist_mbid TEXT NOT NULL, artist_mbid_lower TEXT NOT NULL,
             artist_name TEXT NOT NULL, state TEXT NOT NULL DEFAULT 'pending',
             requested_at REAL NOT NULL, reviewed_by_id TEXT,
             reviewed_by_name TEXT, reviewed_at REAL, batch_id TEXT, source TEXT,
             PRIMARY KEY (user_id, artist_mbid_lower));",
    )
    .map_err(|error| FixtureError::Db(error.to_string()))?;

    let alice_hash = bcrypt_hash(&secrets.alice_password)?;
    let bob_hash = bcrypt_hash(&secrets.bob_password)?;
    db.execute(
        "INSERT INTO auth_users (id, display_name, email, avatar_url, role, created_at,
                                 last_login_at, username, username_display)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
        params![
            ids.alice_id.as_str(),
            "Alice",
            "alice@example.com",
            Option::<String>::None,
            "admin",
            "2024-01-01T00:00:00+00:00",
            Some("2026-01-01T00:00:00+00:00".to_owned()),
            Some("alice".to_owned()),
            Some("Alice".to_owned()),
        ],
    )
    .map_err(|error| FixtureError::Db(error.to_string()))?;
    db.execute(
        "INSERT INTO auth_users (id, display_name, email, avatar_url, role, created_at,
                                 last_login_at, username, username_display)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
        params![
            ids.bob_id.as_str(),
            "Bob",
            "bob@example.com",
            Option::<String>::None,
            "trusted",
            "2024-02-01T00:00:00+00:00",
            Option::<String>::None,
            Some("bob".to_owned()),
            Some("Bob".to_owned()),
        ],
    )
    .map_err(|error| FixtureError::Db(error.to_string()))?;
    let local_data = |hash: &str| format!("{{\"password_hash\": \"{hash}\"}}");
    for (id, user, uid, data) in [
        (
            "prov-alice-local",
            ids.alice_id.as_str(),
            "alice",
            local_data(&alice_hash),
        ),
        (
            "prov-bob-local",
            ids.bob_id.as_str(),
            "bob",
            local_data(&bob_hash),
        ),
        (
            "prov-bob-plex",
            ids.bob_id.as_str(),
            "plex-uid-bob",
            "{\"token\": \"opaque-plex-blob\"}".to_owned(),
        ),
    ] {
        let provider = if id.ends_with("plex") {
            "plex"
        } else {
            "local"
        };
        db.execute(
            "INSERT INTO auth_providers (id, user_id, provider, provider_uid, provider_data,
                                         created_at)
             VALUES (?, ?, ?, ?, ?, ?)",
            params![id, user, provider, uid, data, "2024-01-01T00:00:00+00:00"],
        )
        .map_err(|error| FixtureError::Db(error.to_string()))?;
    }

    let seal = |plaintext: &str| {
        fernet
            .encrypt(plaintext)
            .map_err(|error| FixtureError::Key(error.to_string()))
    };
    for (id, user, name, encrypted, revoked) in [
        (
            "cap-alice-phone",
            ids.alice_id.as_str(),
            "phone",
            seal(&secrets.alice_phone_secret)?,
            0,
        ),
        (
            "cap-alice-old",
            ids.alice_id.as_str(),
            "old-laptop",
            seal(&secrets.alice_old_secret)?,
            1,
        ),
        // Legacy plaintext at v2 rest: the exporter seals it like any secret.
        (
            "cap-bob-legacy",
            ids.bob_id.as_str(),
            "legacy-client",
            secrets.bob_legacy_secret.clone(),
            0,
        ),
    ] {
        db.execute(
            "INSERT INTO connect_app_passwords (id, user_id, name, secret_sha256,
                 secret_encrypted, created_at, last_used_at, last_client, revoked)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
            params![
                id,
                user,
                name,
                format!("sha-stub-{id}"),
                encrypted,
                "2024-03-01T00:00:00+00:00",
                Option::<String>::None,
                Option::<String>::None,
                revoked,
            ],
        )
        .map_err(|error| FixtureError::Db(error.to_string()))?;
    }
    db.execute(
        "INSERT INTO auth_password_recovery_codes (user_id, code_hash, created_at, expires_at)
         VALUES (?, ?, ?, ?)",
        params![
            ids.alice_id.as_str(),
            "fixture-code-hash-alice",
            "2024-04-01T00:00:00+00:00",
            "2027-04-01T00:00:00+00:00",
        ],
    )
    .map_err(|error| FixtureError::Db(error.to_string()))?;

    for (user, mbid, name, auto, followed, updated) in [
        (
            ids.alice_id.as_str(),
            ids.artist_one_mbid.as_str(),
            "Fixture Artist One",
            1,
            1_700_000_000.0,
            1_760_000_000.0,
        ),
        (
            ids.alice_id.as_str(),
            ids.artist_two_mbid.as_str(),
            "Fixture Artist Two",
            0,
            1_710_000_000.0,
            1_760_000_100.0,
        ),
        (
            ids.bob_id.as_str(),
            ids.artist_one_mbid.as_str(),
            "Fixture Artist One",
            0,
            1_720_000_000.0,
            1_760_000_200.0,
        ),
        (
            UNKNOWN_USER_ID,
            ids.artist_two_mbid.as_str(),
            "Fixture Artist Two",
            0,
            1_730_000_000.0,
            1_760_000_300.0,
        ),
    ] {
        db.execute(
            "INSERT INTO user_followed_artists (user_id, artist_mbid, artist_mbid_lower,
                 artist_name, auto_download, followed_at, updated_at)
             VALUES (?, ?, ?, ?, ?, ?, ?)",
            params![
                user,
                mbid,
                mbid.to_lowercase(),
                name,
                auto,
                followed,
                updated
            ],
        )
        .map_err(|error| FixtureError::Db(error.to_string()))?;
    }
    let approvals = [
        ApprovalRow {
            user_id: ids.alice_id.clone(),
            mbid: ids.artist_one_mbid.clone(),
            name: "Fixture Artist One",
            state: "pending",
            requested_at: 1_740_000_000.0,
            by_id: Some(ids.bob_id.clone()),
            by_name: Some("Bob"),
            reviewed_at: None,
            batch_id: None,
            source: Some("fixture"),
        },
        ApprovalRow {
            user_id: ids.bob_id.clone(),
            mbid: ids.artist_one_mbid.clone(),
            name: "Fixture Artist One",
            state: "approved",
            requested_at: 1_741_000_000.0,
            by_id: Some(ids.alice_id.clone()),
            by_name: Some("Alice"),
            reviewed_at: Some(1_742_000_000.0),
            batch_id: Some("batch-1"),
            source: Some("fixture"),
        },
        ApprovalRow {
            user_id: ids.alice_id.clone(),
            mbid: ids.artist_two_mbid.clone(),
            name: "Fixture Artist Two",
            state: "pending",
            requested_at: 1_743_000_000.0,
            by_id: Some(UNKNOWN_USER_ID.to_owned()),
            by_name: Some("Gone"),
            reviewed_at: None,
            batch_id: None,
            source: None,
        },
        ApprovalRow {
            user_id: UNKNOWN_USER_ID.to_owned(),
            mbid: ids.artist_one_mbid.clone(),
            name: "Fixture Artist One",
            state: "pending",
            requested_at: 1_744_000_000.0,
            by_id: None,
            by_name: None,
            reviewed_at: None,
            batch_id: None,
            source: None,
        },
    ];
    for approval in approvals {
        db.execute(
            "INSERT INTO auto_download_approvals (user_id, artist_mbid, artist_mbid_lower,
                 artist_name, state, requested_at, reviewed_by_id, reviewed_by_name,
                 reviewed_at, batch_id, source)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            params![
                approval.user_id,
                approval.mbid,
                approval.mbid.to_lowercase(),
                approval.name,
                approval.state,
                approval.requested_at,
                approval.by_id,
                approval.by_name,
                approval.reviewed_at,
                approval.batch_id,
                approval.source,
            ],
        )
        .map_err(|error| FixtureError::Db(error.to_string()))?;
    }
    db.execute_batch("PRAGMA foreign_keys=ON;")
        .map_err(|error| FixtureError::Db(error.to_string()))?;
    Ok(())
}

/// Low-cost bcrypt for fixtures (fast, still `$2b$` shaped).
fn bcrypt_hash(password: &str) -> Result<String, FixtureError> {
    bcrypt::hash(password, 4).map_err(|error| FixtureError::Hash(error.to_string()))
}
