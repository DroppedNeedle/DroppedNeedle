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
    /// Setting of a plugin whose v2 manifest is gone (sealed, flagged).
    pub orphan_plugin_mode: String,
    /// Alice's Last.fm session key (per-user connection).
    pub lastfm_session: String,
    /// Alice's ListenBrainz user token (per-user connection).
    pub listenbrainz_token: String,
    /// Bob's Navidrome password (per-user connection).
    pub navidrome_password: String,
    /// Bob's Spotify refresh token (per-user connection).
    pub spotify_refresh: String,
}

/// v2 library ids the carried user rows point at. The fixture has no
/// catalog, so every one of them waits as a pending link.
pub const V2_TRACK_ID: &str = "v2-track-1";
/// Album of [`V2_TRACK_ID`].
pub const V2_ALBUM_ID: &str = "v2-album-1";
/// Artist of [`V2_TRACK_ID`].
pub const V2_ARTIST_ID: &str = "v2-artist-1";
/// A second track, queued after the first.
pub const V2_SECOND_TRACK_ID: &str = "v2-track-2";
/// Alice's playlist, with a cover file.
pub const PLAYLIST_ID: &str = "playlist-alice";
/// A second playlist of Alice's imported from the same source. v3 keeps
/// one playlist per user and source, so this one and its entry stay out.
pub const DUPLICATE_PLAYLIST_ID: &str = "playlist-alice-again";
/// The source both of Alice's playlists came from.
const SHARED_SOURCE_REF: &str = "spotify:playlist:road";
/// Alice's finished request.
pub const FINISHED_REQUEST_MBID: &str = "aaaaaaaa-0000-4000-8000-000000000001";
/// Bob's request still waiting for approval; it stays behind.
pub const PENDING_REQUEST_MBID: &str = "aaaaaaaa-0000-4000-8000-000000000002";
/// Alice's completed download.
pub const FINISHED_DOWNLOAD_ID: &str = "download-done";
/// Bytes of the playlist cover file.
pub const COVER_BYTES: &[u8] = b"\x89PNG fixture cover";
/// Bytes of Alice's avatar file.
pub const AVATAR_BYTES: &[u8] = b"\x89PNG fixture avatar";

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
        orphan_plugin_mode: "fast".to_owned(),
        lastfm_session: "alice-lastfm-session".to_owned(),
        listenbrainz_token: "alice-listenbrainz-token".to_owned(),
        navidrome_password: "bob-navidrome-password".to_owned(),
        spotify_refresh: "bob-spotify-refresh".to_owned(),
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
    build_user_data(&cache_dir.join("library.db"), &fernet, &secrets, &ids)?;
    let covers = cache_dir.join("covers").join("playlists");
    let avatars = cache_dir.join("avatars");
    for dir in [&covers, &avatars] {
        std::fs::create_dir_all(dir).map_err(|error| FixtureError::Io(error.to_string()))?;
    }
    std::fs::write(covers.join(format!("{PLAYLIST_ID}.png")), COVER_BYTES)
        .map_err(|error| FixtureError::Io(error.to_string()))?;
    std::fs::write(avatars.join(format!("{}.png", ids.alice_id)), AVATAR_BYTES)
        .map_err(|error| FixtureError::Io(error.to_string()))?;
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
        json!({
            "demo": {"enabled": true, "settings": {
                "api_token": secrets.plugin_token,
                "theme": secrets.plugin_theme,
            }},
            // No plugins/orphan/plugin.toml: nobody knows if "mode" is secret.
            "orphan": {"enabled": true, "settings": {"mode": secrets.orphan_plugin_mode}},
        }),
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
             PRIMARY KEY (user_id, artist_mbid_lower));
         CREATE TABLE user_event_cities (
             user_id TEXT NOT NULL REFERENCES auth_users(id) ON DELETE CASCADE,
             city_name TEXT NOT NULL, country_code TEXT,
             latitude REAL NOT NULL, longitude REAL NOT NULL,
             radius_km REAL NOT NULL, position INTEGER NOT NULL,
             PRIMARY KEY (user_id, latitude, longitude));
         CREATE TABLE user_event_seen (
             user_id TEXT PRIMARY KEY REFERENCES auth_users(id) ON DELETE CASCADE,
             seen_at REAL NOT NULL);",
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
    // Concerts: Alice's two cities and Bob's seen marker.
    db.execute_batch(&format!(
        "INSERT INTO user_event_cities (user_id, city_name, country_code, latitude,
             longitude, radius_km, position)
         VALUES ('{alice}', 'Liverpool', 'GB', 53.41, -2.98, 30.0, 0),
                ('{alice}', 'Leeds', 'GB', 53.8, -1.55, 50.0, 1);
         INSERT INTO user_event_seen (user_id, seen_at) VALUES ('{bob}', 1760000500.0);",
        alice = ids.alice_id,
        bob = ids.bob_id,
    ))
    .map_err(|error| FixtureError::Db(error.to_string()))?;
    db.execute_batch("PRAGMA foreign_keys=ON;")
        .map_err(|error| FixtureError::Db(error.to_string()))?;
    Ok(())
}

/// The v2 user tables beyond accounts and follows, with a few rows each:
/// connections, playlists, favorites, history, the request and download
/// ledgers (finished and unfinished rows), watches, quotas, quarantine,
/// preferences, compat queues and bookmarks, known releases, plus two
/// tables that stay behind. Column sets match v2's final schema, which v3's
/// baseline copies.
fn build_user_data(
    db_path: &Path,
    fernet: &crate::export::fernet::FernetKey,
    secrets: &FixtureSecrets,
    ids: &FixtureIds,
) -> Result<(), FixtureError> {
    let db = Connection::open(db_path).map_err(|error| FixtureError::Db(error.to_string()))?;
    let run = |sql: &str| {
        db.execute_batch(sql)
            .map_err(|error| FixtureError::Db(error.to_string()))
    };
    run(USER_DATA_SCHEMA)?;
    let seal = |plaintext: String| {
        fernet
            .encrypt(&plaintext)
            .map_err(|error| FixtureError::Key(error.to_string()))
    };
    let (alice, bob) = (ids.alice_id.as_str(), ids.bob_id.as_str());
    let at = "2025-06-01T00:00:00+00:00";
    for (user, service, data) in [
        (
            alice,
            "lastfm",
            json!({"session_key": secrets.lastfm_session, "username": "alice_fm"}),
        ),
        (
            alice,
            "listenbrainz",
            json!({"user_token": secrets.listenbrainz_token, "username": "alice_lb"}),
        ),
        (
            bob,
            "navidrome",
            json!({"username": "bob", "password": secrets.navidrome_password}),
        ),
        (
            bob,
            "spotify",
            json!({
                "access_token": "bob-spotify-access",
                "refresh_token": secrets.spotify_refresh,
                "expires_at": "2025-06-01T01:00:00+00:00",
                "username": "bob_sp",
                "spotify_user_id": "bob-sp-id",
            }),
        ),
    ] {
        db.execute(
            "INSERT INTO user_connections (user_id, service, connection_data, enabled,
                 created_at, updated_at) VALUES (?, ?, ?, 1, ?, ?)",
            params![user, service, seal(data.to_string())?, at, at],
        )
        .map_err(|error| FixtureError::Db(error.to_string()))?;
    }
    let artist = ids.artist_one_mbid.to_lowercase();
    let rows = format!(
        "INSERT INTO library_playlists (id, name, cover_image_path, created_at, updated_at,
             source_ref, user_id, is_public)
         VALUES ('{PLAYLIST_ID}', 'Road trip', '/app/cache/covers/playlists/{PLAYLIST_ID}.png',
                 '{at}', '{at}', '{SHARED_SOURCE_REF}', '{alice}', 1),
                ('{DUPLICATE_PLAYLIST_ID}', 'Road trip (again)', NULL, '{at}', '{at}',
                 '{SHARED_SOURCE_REF}', '{alice}', 0),
                ('playlist-bob', 'Bob mix', NULL, '{at}', '{at}', NULL, '{bob}', 0);
         INSERT INTO library_playlist_tracks (id, playlist_id, position, track_name,
             artist_name, album_name, track_source_id, source_type, created_at,
             library_file_id, local_track_id, local_album_id, local_artist_id)
         VALUES ('entry-local', '{PLAYLIST_ID}', 0, 'Song', 'Artist', 'Album', '{V2_TRACK_ID}',
                 'local', '{at}', '{V2_TRACK_ID}', '{V2_TRACK_ID}', '{V2_ALBUM_ID}',
                 '{V2_ARTIST_ID}'),
                ('entry-remote', '{PLAYLIST_ID}', 1, 'Other', 'Someone', 'Elsewhere', 'yt-abc',
                 'youtube', '{at}', NULL, NULL, NULL, NULL),
                ('entry-duplicate', '{DUPLICATE_PLAYLIST_ID}', 0, 'Other', 'Someone',
                 'Elsewhere', 'yt-abc', 'youtube', '{at}', NULL, NULL, NULL, NULL);
         INSERT INTO local_tracks (id, title) VALUES ('{V2_TRACK_ID}', 'Song');
         INSERT INTO local_albums (id, title) VALUES ('{V2_ALBUM_ID}', 'Album');
         INSERT INTO download_attempts (id, task_id, source, candidate_index, handle_json,
             state, created_at, updated_at)
         VALUES ('attempt-done', '{FINISHED_DOWNLOAD_ID}', 'soulseek', 0, '{{}}', 'complete',
                 1700000100.0, 1700000400.0),
                ('attempt-cleanup', '{FINISHED_DOWNLOAD_ID}', 'soulseek', 1, '{{}}',
                 'cleanup_pending', 1700000100.0, 1700000400.0),
                ('attempt-running', 'download-running', 'soulseek', 0, '{{}}', 'in_use',
                 1700001100.0, 1700001100.0);
         INSERT INTO library_user_favorites (user_id, item_kind, item_id, created_at)
         VALUES ('{alice}', 'track', '{V2_TRACK_ID}', 1700000000.0),
                ('{alice}', 'album', '{V2_ALBUM_ID}', 1700000001.0);
         INSERT INTO library_play_history (id, user_id, local_track_id, local_album_id,
             local_artist_id, track_name, artist_name, album_name, played_at)
         VALUES ('listen-1', '{alice}', '{V2_TRACK_ID}', '{V2_ALBUM_ID}', '{V2_ARTIST_ID}',
                 'Song', 'Artist', 'Album', '{at}'),
                ('listen-2', '{bob}', NULL, NULL, NULL, 'Other', 'Someone', NULL, '{at}');
         INSERT INTO request_history (musicbrainz_id_lower, musicbrainz_id, artist_name,
             album_title, requested_at, completed_at, status, user_id)
         VALUES ('{FINISHED_REQUEST_MBID}', '{FINISHED_REQUEST_MBID}', 'Artist', 'Album',
                 '1700000000', '1700000500', 'imported', '{alice}'),
                ('{PENDING_REQUEST_MBID}', '{PENDING_REQUEST_MBID}', 'Artist', 'Next',
                 '1700001000', NULL, 'awaiting_approval', '{bob}');
         INSERT INTO request_history_requesters (user_id, musicbrainz_id_lower, requested_at)
         VALUES ('{alice}', '{FINISHED_REQUEST_MBID}', '1700000000'),
                ('{bob}', '{PENDING_REQUEST_MBID}', '1700001000');
         INSERT INTO request_history_dismissals (user_id, musicbrainz_id_lower, dismissed_at)
         VALUES ('{alice}', '{FINISHED_REQUEST_MBID}', '{at}');
         INSERT INTO download_tasks (id, user_id, release_group_mbid, artist_name,
             album_title, status, total_size_bytes, created_at, completed_at, updated_at)
         VALUES ('{FINISHED_DOWNLOAD_ID}', '{alice}', '{FINISHED_REQUEST_MBID}', 'Artist',
                 'Album', 'completed', 52428800, 1700000100.0, 1700000400.0, 1700000400.0),
                ('download-running', '{bob}', '{PENDING_REQUEST_MBID}', 'Artist', 'Next',
                 'downloading', NULL, 1700001100.0, NULL, 1700001100.0);
         INSERT INTO wanted_watches (release_group_mbid_lower, release_group_mbid, user_id,
             artist_name, album_title, kind, state, created_at, next_check_at)
         VALUES ('bbbbbbbb-0000-4000-8000-000000000001', 'BBBBBBBB-0000-4000-8000-000000000001',
                 '{alice}', 'Artist', 'Rare', 'missing', 'watching', 1700000000.0, 1700086400.0);
         INSERT INTO wanted_seen_candidates (release_group_mbid_lower, source, identity,
             first_seen_at)
         VALUES ('bbbbbbbb-0000-4000-8000-000000000001', 'soulseek', 'peer/Rare', 1700000300.0);
         INSERT INTO user_quotas (user_id, request_quota_count, request_quota_days,
             storage_quota_gb)
         VALUES ('{bob}', 5, 7, 20);
         INSERT INTO download_quarantine (source, identity, release_group_mbid, reason,
             quarantined_at)
         VALUES ('soulseek', 'peer/broken', NULL, 'corrupt', 1700000200.0);
         INSERT INTO user_listening_prefs (user_id, scrobble_to_lastfm,
             scrobble_to_listenbrainz, updated_at)
         VALUES ('{alice}', 1, 1, '{at}');
         INSERT INTO personal_mix_approvals (user_id, state, requested_at)
         VALUES ('{bob}', 'approved', 1700000000.0);
         INSERT INTO user_section_prefs (user_id, page, section_key, enabled, updated_at)
         VALUES ('{alice}', 'home', 'recently_played', 1, '{at}');
         INSERT INTO user_navidrome_folder_preferences (user_id, mode, selected_ids_json,
             updated_at)
         VALUES ('{bob}', 'selected', '[\"1\"]', 1700000000.0);
         INSERT INTO user_new_release_seen (user_id, seen_at) VALUES ('{alice}', 1700000000.0);
         INSERT INTO library_compat_play_queues (user_id, current_index, position_ms,
             updated_at, changed_by_client)
         VALUES ('{alice}', 1, 4200, 1700000000.0, 'Feishin');
         INSERT INTO library_compat_play_queue_items (user_id, item_index, local_track_id)
         VALUES ('{alice}', 0, '{V2_TRACK_ID}'), ('{alice}', 1, '{V2_SECOND_TRACK_ID}');
         INSERT INTO library_compat_bookmarks (user_id, local_track_id, position_ms, comment,
             created_at, changed_at)
         VALUES ('{alice}', '{V2_TRACK_ID}', 90000, 'chapter two', 1700000000.0, 1700000000.0);
         INSERT INTO artist_known_releases (artist_mbid_lower, rg_mbid_lower)
         VALUES ('{artist}', 'cccccccc-0000-4000-8000-000000000001');
         INSERT INTO new_release_feed (release_group_mbid_lower, release_group_mbid,
             artist_mbid_lower, artist_name, title, discovered_at)
         VALUES ('cccccccc-0000-4000-8000-000000000001', 'CCCCCCCC-0000-4000-8000-000000000001',
                 '{artist}', 'Fixture Artist One', 'New One', 1700000000.0);
         INSERT INTO auth_tokens (token_hash, user_id, created_at) VALUES ('t', '{alice}', '{at}');
         INSERT INTO youtube_links (id, user_id) VALUES ('yt-1', '{alice}');"
    );
    run(&rows)
}

/// v2 user tables the fixture fills, in v2's final shape. `download_tasks`
/// keeps only the columns the rows set, as an older v2 would lack some.
const USER_DATA_SCHEMA: &str = "
    CREATE TABLE user_connections (
        user_id TEXT NOT NULL REFERENCES auth_users(id) ON DELETE CASCADE,
        service TEXT NOT NULL, connection_data TEXT NOT NULL,
        enabled INTEGER NOT NULL DEFAULT 1, created_at TEXT NOT NULL,
        updated_at TEXT NOT NULL, PRIMARY KEY (user_id, service));
    CREATE TABLE library_playlists (
        id TEXT PRIMARY KEY, name TEXT NOT NULL, cover_image_path TEXT,
        created_at TEXT NOT NULL, updated_at TEXT NOT NULL, source_ref TEXT,
        user_id TEXT, is_public INTEGER NOT NULL DEFAULT 0);
    CREATE TABLE library_playlist_tracks (
        id TEXT PRIMARY KEY, playlist_id TEXT NOT NULL, position INTEGER NOT NULL,
        track_name TEXT NOT NULL, artist_name TEXT NOT NULL, album_name TEXT NOT NULL,
        album_id TEXT, artist_id TEXT, track_source_id TEXT, cover_url TEXT,
        source_type TEXT NOT NULL, available_sources TEXT, format TEXT,
        track_number INTEGER, disc_number INTEGER, duration INTEGER,
        created_at TEXT NOT NULL, plex_rating_key TEXT, library_file_id TEXT,
        local_track_id TEXT, local_album_id TEXT, local_artist_id TEXT,
        reference_tombstone_id TEXT, UNIQUE(playlist_id, position));
    CREATE TABLE library_user_favorites (
        user_id TEXT NOT NULL, item_kind TEXT NOT NULL, item_id TEXT NOT NULL,
        created_at REAL NOT NULL, PRIMARY KEY(user_id, item_kind, item_id));
    CREATE TABLE library_play_history (
        id TEXT PRIMARY KEY, user_id TEXT NOT NULL, local_track_id TEXT,
        local_album_id TEXT, local_artist_id TEXT, track_name TEXT NOT NULL,
        artist_name TEXT NOT NULL, album_name TEXT, recording_mbid TEXT,
        release_group_mbid TEXT, duration_ms INTEGER, source TEXT,
        played_at TEXT NOT NULL);
    CREATE TABLE request_history (
        musicbrainz_id_lower TEXT PRIMARY KEY, musicbrainz_id TEXT NOT NULL,
        artist_name TEXT NOT NULL, album_title TEXT NOT NULL, artist_mbid TEXT,
        year INTEGER, cover_url TEXT, requested_at TEXT NOT NULL, completed_at TEXT,
        status TEXT NOT NULL, monitor_artist INTEGER NOT NULL DEFAULT 0,
        auto_download_artist INTEGER NOT NULL DEFAULT 0, user_id TEXT,
        requested_by_name TEXT, reviewed_by_id TEXT, reviewed_by_name TEXT,
        reviewed_at TEXT, download_task_id TEXT, release_mbid TEXT,
        request_kind TEXT NOT NULL DEFAULT 'album', track_title TEXT,
        duration_seconds INTEGER, track_release_group_mbid TEXT,
        dispatch_authorized INTEGER NOT NULL DEFAULT 0,
        generation INTEGER NOT NULL DEFAULT 1);
    CREATE TABLE request_history_requesters (
        user_id TEXT NOT NULL, musicbrainz_id_lower TEXT NOT NULL,
        requested_at TEXT NOT NULL, requested_by_name TEXT,
        PRIMARY KEY (user_id, musicbrainz_id_lower));
    CREATE TABLE request_history_dismissals (
        user_id TEXT NOT NULL, musicbrainz_id_lower TEXT NOT NULL,
        dismissed_at TEXT NOT NULL, PRIMARY KEY (user_id, musicbrainz_id_lower));
    CREATE TABLE download_tasks (
        id TEXT PRIMARY KEY, user_id TEXT NOT NULL, release_group_mbid TEXT NOT NULL,
        artist_name TEXT NOT NULL, album_title TEXT NOT NULL,
        status TEXT NOT NULL DEFAULT 'queued', total_size_bytes INTEGER,
        created_at REAL NOT NULL, completed_at REAL, updated_at REAL NOT NULL);
    CREATE TABLE wanted_watches (
        release_group_mbid_lower TEXT PRIMARY KEY, release_group_mbid TEXT NOT NULL,
        user_id TEXT NOT NULL, artist_name TEXT NOT NULL, album_title TEXT NOT NULL,
        artist_mbid TEXT, year INTEGER, cover_url TEXT, kind TEXT NOT NULL,
        state TEXT NOT NULL DEFAULT 'watching', created_at REAL NOT NULL,
        first_release_date TEXT, check_count INTEGER NOT NULL DEFAULT 0,
        quiet_streak INTEGER NOT NULL DEFAULT 0, last_checked_at REAL,
        next_check_at REAL NOT NULL, last_outcome TEXT,
        new_candidate_count INTEGER NOT NULL DEFAULT 0);
    CREATE TABLE wanted_seen_candidates (
        release_group_mbid_lower TEXT NOT NULL, source TEXT NOT NULL,
        identity TEXT NOT NULL, first_seen_at REAL NOT NULL,
        PRIMARY KEY (release_group_mbid_lower, source, identity));
    CREATE TABLE user_quotas (
        user_id TEXT PRIMARY KEY, request_quota_count INTEGER,
        request_quota_days INTEGER, storage_quota_gb INTEGER);
    CREATE TABLE download_quarantine (
        id INTEGER PRIMARY KEY AUTOINCREMENT, source TEXT NOT NULL DEFAULT 'soulseek',
        identity TEXT NOT NULL, release_group_mbid TEXT, reason TEXT NOT NULL,
        quarantined_at REAL NOT NULL, UNIQUE (source, identity, release_group_mbid));
    CREATE TABLE user_listening_prefs (
        user_id TEXT PRIMARY KEY, scrobble_to_lastfm INTEGER NOT NULL DEFAULT 0,
        scrobble_to_listenbrainz INTEGER NOT NULL DEFAULT 0,
        navidrome_handles_external_scrobbles INTEGER NOT NULL DEFAULT 1,
        primary_music_source TEXT NOT NULL DEFAULT 'listenbrainz',
        now_playing_visibility TEXT NOT NULL DEFAULT 'full', updated_at TEXT NOT NULL,
        auto_request_personal_mix INTEGER NOT NULL DEFAULT 0);
    CREATE TABLE personal_mix_approvals (
        user_id TEXT NOT NULL PRIMARY KEY, state TEXT NOT NULL DEFAULT 'pending',
        requested_at REAL NOT NULL, reviewed_by_id TEXT, reviewed_by_name TEXT,
        reviewed_at REAL);
    CREATE TABLE user_section_prefs (
        user_id TEXT NOT NULL, page TEXT NOT NULL, section_key TEXT NOT NULL,
        enabled INTEGER NOT NULL DEFAULT 0, updated_at TEXT NOT NULL,
        PRIMARY KEY (user_id, page, section_key));
    CREATE TABLE user_navidrome_folder_preferences (
        user_id TEXT PRIMARY KEY, mode TEXT NOT NULL,
        selected_ids_json TEXT NOT NULL DEFAULT '[]', server_identity TEXT,
        updated_at REAL NOT NULL);
    CREATE TABLE user_new_release_seen (user_id TEXT PRIMARY KEY, seen_at REAL NOT NULL);
    CREATE TABLE library_compat_play_queues (
        user_id TEXT PRIMARY KEY, current_index INTEGER,
        position_ms INTEGER NOT NULL DEFAULT 0, updated_at REAL NOT NULL,
        changed_by_client TEXT NOT NULL DEFAULT '');
    CREATE TABLE library_compat_play_queue_items (
        user_id TEXT NOT NULL, item_index INTEGER NOT NULL,
        local_track_id TEXT NOT NULL, PRIMARY KEY(user_id, item_index));
    CREATE TABLE library_compat_bookmarks (
        user_id TEXT NOT NULL, local_track_id TEXT NOT NULL,
        position_ms INTEGER NOT NULL, comment TEXT NOT NULL DEFAULT '',
        created_at REAL NOT NULL, changed_at REAL NOT NULL,
        PRIMARY KEY(user_id, local_track_id));
    CREATE TABLE artist_known_releases (
        artist_mbid_lower TEXT NOT NULL, rg_mbid_lower TEXT NOT NULL,
        auto_policy_revision INTEGER, PRIMARY KEY (artist_mbid_lower, rg_mbid_lower));
    CREATE TABLE new_release_feed (
        release_group_mbid_lower TEXT PRIMARY KEY, release_group_mbid TEXT NOT NULL,
        artist_mbid_lower TEXT NOT NULL, artist_name TEXT NOT NULL, title TEXT NOT NULL,
        primary_type TEXT, secondary_types TEXT, first_release_date TEXT,
        discovered_at REAL NOT NULL);
    CREATE TABLE auth_tokens (
        token_hash TEXT PRIMARY KEY, user_id TEXT NOT NULL, created_at TEXT NOT NULL);
    CREATE TABLE youtube_links (id TEXT PRIMARY KEY, user_id TEXT NOT NULL);
    CREATE TABLE local_tracks (id TEXT PRIMARY KEY, title TEXT NOT NULL);
    CREATE TABLE local_albums (id TEXT PRIMARY KEY, title TEXT NOT NULL);
    CREATE TABLE local_artists (id TEXT PRIMARY KEY, display_name TEXT NOT NULL);
    CREATE TABLE download_attempts (
        id TEXT PRIMARY KEY, task_id TEXT NOT NULL, source TEXT NOT NULL,
        candidate_index INTEGER NOT NULL, job_name TEXT NOT NULL DEFAULT '',
        handle_json TEXT NOT NULL, remote_storage TEXT, mount_root TEXT,
        workspace_path TEXT, materialized_paths_json TEXT NOT NULL DEFAULT '[]',
        materialized_fingerprints_json TEXT NOT NULL DEFAULT '{}',
        publisher_bundle_ids_json TEXT NOT NULL DEFAULT '[]',
        legacy_reconciled INTEGER NOT NULL DEFAULT 0, state TEXT NOT NULL,
        disposition TEXT NOT NULL DEFAULT 'undecided',
        cleanup_failures INTEGER NOT NULL DEFAULT 0, next_retry_at REAL NOT NULL DEFAULT 0,
        lease_owner TEXT, lease_expires_at REAL, error_code TEXT, created_at REAL NOT NULL,
        updated_at REAL NOT NULL, completed_at REAL,
        row_revision INTEGER NOT NULL DEFAULT 1);
";

/// Low-cost bcrypt for fixtures (fast, still `$2b$` shaped).
fn bcrypt_hash(password: &str) -> Result<String, FixtureError> {
    bcrypt::hash(password, 4).map_err(|error| FixtureError::Hash(error.to_string()))
}
