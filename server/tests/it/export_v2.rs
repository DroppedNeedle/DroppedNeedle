//! Exporter briefs: the exporter refuses a v2 instance without its key
//! file, and otherwise carries exactly the Q2 set (accounts, settings,
//! follows) into a sealed envelope.

use std::collections::HashMap;
use std::path::Path;

use droppedneedle::export::{
    ExportError, ExportRequest, HashScheme, Opener, SealError, export_v2, export_v2_to_file,
    fernet::FernetKey, parse_export, unseal,
};
use droppedneedle::runtime_config::Secret;
use rusqlite::{Connection, params};
use serde_json::{Value, json};

const PASSPHRASE: &str = "operator passphrase";
const INSTANCE_ID: &str = "9f2c4a1e-0000-4000-8000-0123456789ab";

/// Scratch v2 root, removed when the test ends.
fn scratch_dir(name: &str) -> droppedneedle::tooling::scratch::ScratchDir {
    let dir = droppedneedle::tooling::scratch::ScratchDir::new(name).unwrap();
    std::fs::create_dir_all(dir.join("config")).unwrap();
    std::fs::create_dir_all(dir.join("cache")).unwrap();
    dir
}

fn request(v2_root: &Path) -> ExportRequest {
    ExportRequest {
        v2_root: v2_root.to_owned(),
        db_path: None,
        passphrase: Secret::new(PASSPHRASE),
        exported_at: Some("2026-09-28T12:00:00Z".to_owned()),
        v2_commit: Some("cf7278a1".to_owned()),
    }
}

#[test]
fn missing_key_file_refuses_with_no_output() {
    let dir = scratch_dir("no-key");
    std::fs::write(dir.join("config/config.json"), "{}").unwrap();
    let out = dir.join("export.json");

    let error = export_v2(&request(&dir)).unwrap_err();
    assert_eq!(error.code(), "V2_KEY_NOT_FOUND");
    assert!(matches!(error, ExportError::V2KeyNotFound { .. }));

    let error = export_v2_to_file(&request(&dir), &out).unwrap_err();
    assert_eq!(error.code(), "V2_KEY_NOT_FOUND");
    assert!(!out.exists(), "a refused export must not write a file");
}

#[test]
fn key_file_without_a_key_refuses() {
    let dir = scratch_dir("empty-key");
    std::fs::write(dir.join("config/.env"), "# no key here\nOTHER=1\n").unwrap();
    std::fs::write(dir.join("config/config.json"), "{}").unwrap();
    let out = dir.join("export.json");

    let error = export_v2(&request(&dir)).unwrap_err();
    assert_eq!(error.code(), "V2_KEY_INVALID");
    let error = export_v2_to_file(&request(&dir), &out).unwrap_err();
    assert_eq!(error.code(), "V2_KEY_INVALID");
    assert!(!out.exists(), "a refused export must not write a file");
}

#[test]
fn unparseable_key_refuses() {
    let dir = scratch_dir("bad-key");
    std::fs::write(dir.join("config/.env"), "DATA_ENC_KEY=not-a-key\n").unwrap();
    std::fs::write(dir.join("config/config.json"), "{}").unwrap();
    let out = dir.join("export.json");

    let error = export_v2(&request(&dir)).unwrap_err();
    assert_eq!(error.code(), "V2_KEY_INVALID");
    let error = export_v2_to_file(&request(&dir), &out).unwrap_err();
    assert_eq!(error.code(), "V2_KEY_INVALID");
    assert!(!out.exists(), "a refused export must not write a file");
}

/// One Fernet-encrypted value plus one legacy-plaintext value, so the
/// fixture proves both v2 decrypt paths end up sealed.
fn fixture_config(key: &FernetKey) -> Value {
    json!({
        "instance_id": INSTANCE_ID,
        "jellyfin_url": "http://jellyfin:8096",
        "user_preferences": {"primary_types": ["Album"], "secondary_types": []},
        "download_client": {"enabled": true, "api_key": "legacy-plaintext-slskd"},
        "download_clients": {"sabnzbd": {"enabled": true, "api_key": key.encrypt("sab-key").unwrap()}},
        "indexers": [
            {"name": "first", "url": "https://one.example", "api_key": key.encrypt("one-key").unwrap()},
            {"name": "second", "url": "https://two.example", "api_key": key.encrypt("two-key").unwrap()}
        ],
        "prowlarr": {"enabled": false, "api_key": key.encrypt("prowlarr-key").unwrap()},
        "lastfm_settings": {
            "api_key": key.encrypt("lfm-key").unwrap(),
            "shared_secret": key.encrypt("lfm-secret").unwrap(),
            "session_key": "",
            "username": "dj",
            "enabled": true
        },
        "library_settings": {
            "library_roots": [{"id": "root-1", "path": "/music", "label": "Music", "policy": "automatic", "rules": []}],
            "staging_path": "/staging",
            "naming_template": "{artist}/{album}",
            "acoustid_api_key": key.encrypt("acoustid-key").unwrap(),
            "enabled": true
        },
        "advanced_settings": {
            "audiodb_enabled": true,
            "audiodb_api_key": "plaintext-audiodb-key",
            "http_timeout": 30,
            "artist_discovery_warm_interval": 60
        },
        "download_policy": {
            "quality_min": "mp3_320",
            "quality_max": "lossless",
            "quality_recipe_status": "non_convertible",
            "quality_recipe_error": "dropped codecs"
        },
        "musicbrainz_settings": {
            "source_mode": "official",
            "api_url": "https://musicbrainz.org/ws/2",
            "pending_brainzmash": {"x": 1},
            "source_quarantined": true,
            "quarantine_reason": "flapping",
            "clamped_to_official_limits": true
        },
        "_internal": {
            "plex_client_id": "client-1",
            "droppedneedle_device_id": "device-1",
            "brainzmash_consent_admin": true,
            "audiodb_sweep_cursor": "cursor"
        },
        "plugins": {
            "demo": {"enabled": true, "version": "1.0", "settings": {"token": "plugin-secret", "theme": "dark"}}
        },
        "library_sync_settings": {"sync_frequency": "daily"},
        "library_scan_dirty_scopes": {"scopes": ["a"]},
        "local_files_settings": {"x": 1},
        "home_settings": {"x": 1},
        "_legacy_lidarr": {"api_key": "must-not-survive"}
    })
}

fn fixture_db(path: &Path) {
    let db = Connection::open(path).unwrap();
    db.execute_batch(
        "CREATE TABLE auth_users (id TEXT PRIMARY KEY, display_name TEXT NOT NULL,
          email TEXT UNIQUE, avatar_url TEXT, role TEXT NOT NULL DEFAULT 'user',
          created_at TEXT NOT NULL, last_login_at TEXT, username TEXT,
          username_display TEXT);
         CREATE TABLE auth_providers (id TEXT PRIMARY KEY,
          user_id TEXT NOT NULL REFERENCES auth_users(id) ON DELETE CASCADE,
          provider TEXT NOT NULL, provider_uid TEXT NOT NULL, provider_data TEXT,
          created_at TEXT NOT NULL, UNIQUE (provider, provider_uid));
         CREATE TABLE connect_app_passwords (id TEXT PRIMARY KEY,
          user_id TEXT NOT NULL REFERENCES auth_users(id) ON DELETE CASCADE,
          name TEXT NOT NULL, secret_sha256 TEXT NOT NULL UNIQUE,
          secret_encrypted TEXT NOT NULL, created_at TEXT NOT NULL,
          last_used_at TEXT, last_client TEXT, revoked INTEGER NOT NULL DEFAULT 0);
         CREATE TABLE auth_password_recovery_codes (user_id TEXT PRIMARY KEY
          REFERENCES auth_users(id) ON DELETE CASCADE, code_hash TEXT NOT NULL UNIQUE,
          created_at TEXT NOT NULL, expires_at TEXT NOT NULL);
         CREATE TABLE user_followed_artists (user_id TEXT NOT NULL
          REFERENCES auth_users(id) ON DELETE CASCADE, artist_mbid TEXT NOT NULL,
          artist_mbid_lower TEXT NOT NULL, artist_name TEXT NOT NULL,
          auto_download INTEGER NOT NULL DEFAULT 0, followed_at REAL NOT NULL,
          updated_at REAL NOT NULL, PRIMARY KEY (user_id, artist_mbid_lower));
         CREATE TABLE auto_download_approvals (user_id TEXT NOT NULL
          REFERENCES auth_users(id) ON DELETE CASCADE, artist_mbid TEXT NOT NULL,
          artist_mbid_lower TEXT NOT NULL, artist_name TEXT NOT NULL,
          state TEXT NOT NULL DEFAULT 'pending', requested_at REAL NOT NULL,
          reviewed_by_id TEXT, reviewed_by_name TEXT, reviewed_at REAL,
          batch_id TEXT, source TEXT, PRIMARY KEY (user_id, artist_mbid_lower));",
    )
    .unwrap();
    db.execute(
        "INSERT INTO auth_users (id, display_name, email, avatar_url, role,
          created_at, last_login_at, username, username_display)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
        params![
            "user-1",
            "Ada",
            "ada@example.com",
            Option::<String>::None,
            "admin",
            "2026-01-01T00:00:00Z",
            "2026-09-01T00:00:00Z",
            "ada",
            "Ada",
        ],
    )
    .unwrap();
    db.execute(
        "INSERT INTO auth_providers (id, user_id, provider, provider_uid,
          provider_data, created_at) VALUES (?, ?, ?, ?, ?, ?)",
        params![
            "prov-1",
            "user-1",
            "local",
            "ada",
            r#"{"password_hash": "$2b$12$abcdefghijklmnopqrstuvwxyz0123456789ABCDEF"}"#,
            "2026-01-01T00:00:00Z",
        ],
    )
    .unwrap();
    db.execute(
        "INSERT INTO auth_providers (id, user_id, provider, provider_uid,
          provider_data, created_at) VALUES (?, ?, ?, ?, ?, ?)",
        params![
            "prov-2",
            "user-1",
            "plex",
            "plex-uid-9",
            "gAAAAABopaque-blob",
            "2026-02-01T00:00:00Z",
        ],
    )
    .unwrap();
    let env = std::fs::read_to_string(path.parent().unwrap().parent().unwrap().join("config/.env"))
        .unwrap();
    let encoded = env
        .lines()
        .find_map(|line| line.strip_prefix("DATA_ENC_KEY="))
        .unwrap()
        .trim_matches('\'');
    let key = FernetKey::from_base64(encoded).unwrap();
    db.execute(
        "INSERT INTO connect_app_passwords (id, user_id, name, secret_sha256,
          secret_encrypted, created_at, last_used_at, last_client, revoked)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
        params![
            "ap-1",
            "user-1",
            "phone",
            "sha-active",
            key.encrypt("app-secret-active").unwrap(),
            "2026-03-01T00:00:00Z",
            "2026-09-02T00:00:00Z",
            "Symfonium",
            0,
        ],
    )
    .unwrap();
    db.execute(
        "INSERT INTO connect_app_passwords (id, user_id, name, secret_sha256,
          secret_encrypted, created_at, last_used_at, last_client, revoked)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
        params![
            "ap-2",
            "user-1",
            "old-laptop",
            "sha-revoked",
            key.encrypt("app-secret-revoked").unwrap(),
            "2026-04-01T00:00:00Z",
            Option::<String>::None,
            Option::<String>::None,
            1,
        ],
    )
    .unwrap();
    db.execute(
        "INSERT INTO auth_password_recovery_codes (user_id, code_hash,
          created_at, expires_at) VALUES (?, ?, ?, ?)",
        params![
            "user-1",
            "code-hash-1",
            "2026-05-01T00:00:00Z",
            "2026-05-02T00:00:00Z",
        ],
    )
    .unwrap();
    db.execute(
        "INSERT INTO user_followed_artists (user_id, artist_mbid,
          artist_mbid_lower, artist_name, auto_download, followed_at,
          updated_at) VALUES (?, ?, ?, ?, ?, ?, ?)",
        params![
            "user-1",
            "123E4567-E89B-12D3-A456-426614174000",
            "123e4567-e89b-12d3-a456-426614174000",
            "Example Artist",
            1,
            1_700_000_000.0f64,
            1_700_000_100.0f64,
        ],
    )
    .unwrap();
    db.execute(
        "INSERT INTO auto_download_approvals (user_id, artist_mbid,
          artist_mbid_lower, artist_name, state, requested_at, reviewed_by_id,
          reviewed_by_name, reviewed_at, batch_id, source)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        params![
            "user-1",
            "123E4567-E89B-12D3-A456-426614174000",
            "123e4567-e89b-12d3-a456-426614174000",
            "Example Artist",
            "pending",
            1_700_000_200.0f64,
            Option::<String>::None,
            Option::<String>::None,
            Option::<f64>::None,
            Option::<String>::None,
            "lidarr",
        ],
    )
    .unwrap();
}

fn fixture_v2_root() -> (droppedneedle::tooling::scratch::ScratchDir, FernetKey) {
    let dir = scratch_dir("full");
    let key = FernetKey::generate().unwrap();
    std::fs::write(
        dir.join("config/.env"),
        format!("DATA_ENC_KEY='{}'\n", key.to_base64()),
    )
    .unwrap();
    std::fs::write(
        dir.join("config/config.json"),
        serde_json::to_string(&fixture_config(&key)).unwrap(),
    )
    .unwrap();
    std::fs::create_dir_all(dir.join("plugins/demo")).unwrap();
    std::fs::write(
        dir.join("plugins/demo/plugin.toml"),
        "[plugin]\nname = \"demo\"\nversion = \"1.0\"\napi_version = 1\n\
         entrypoint = \"demo:Demo\"\ncapabilities = [\"indexer\"]\n\
         [[settings]]\nkey = \"token\"\nlabel = \"Token\"\nsecret = true\n\
         [[settings]]\nkey = \"theme\"\nlabel = \"Theme\"\n\
         [[capability]]\nid = \"indexer\"\ntarget_source = \"usenet\"\n",
    )
    .unwrap();
    fixture_db(&dir.join("cache/library.db"));
    (dir, key)
}

fn sealed(value: &Value) -> &str {
    value.get("$sealed").unwrap().as_str().unwrap()
}

/// A full export, serialized and re-parsed the way the importer sees it.
fn exported() -> (String, droppedneedle::export::ParsedExport) {
    let (dir, _key) = fixture_v2_root();
    let doc = export_v2(&request(&dir)).unwrap();
    let text = doc.to_json_string().unwrap();
    let parsed = parse_export(&text).unwrap();
    assert!(parsed.warnings.is_empty());
    (text, parsed)
}

fn open(parsed: &droppedneedle::export::ParsedExport, value: &Value) -> String {
    unseal(PASSPHRASE, &parsed.doc.secret_envelope, sealed(value)).unwrap()
}

#[test]
fn export_marks_provenance() {
    let (_text, parsed) = exported();
    assert_eq!(parsed.doc.instance_id, INSTANCE_ID);
    assert_eq!(parsed.doc.exported_at, "2026-09-28T12:00:00Z");
    assert_eq!(parsed.doc.v2_commit.as_deref(), Some("cf7278a1"));
}

#[test]
fn export_carries_users_providers_and_recovery() {
    let (text, parsed) = exported();
    let users = &parsed.doc.users;
    assert_eq!(users.len(), 1);
    let user = &users[0];
    assert_eq!(user.id, "user-1");
    assert_eq!(user.role, "admin");
    assert_eq!(user.username.as_deref(), Some("ada"));
    assert_eq!(user.providers.len(), 2);
    assert_eq!(user.providers[0].hash_scheme, HashScheme::Bcrypt);
    assert!(
        user.providers[0]
            .provider_data
            .as_deref()
            .unwrap()
            .contains("$2b$12$")
    );
    assert_eq!(user.providers[1].hash_scheme, HashScheme::Opaque);
    assert_eq!(
        user.providers[1].provider_data.as_deref(),
        Some("gAAAAABopaque-blob")
    );
    let recovery = user.recovery_code.as_ref().unwrap();
    assert_eq!(recovery.code_hash, "code-hash-1");
    assert!(!text.contains("prov-1"), "surrogate ids stay behind");
}

#[test]
fn export_seals_app_passwords_revoked_kept() {
    let (text, parsed) = exported();
    let passwords = &parsed.doc.users[0].app_passwords;
    assert_eq!(passwords.len(), 2);
    assert!(!passwords[0].revoked);
    assert!(passwords[1].revoked, "revoked rows survive as revoked");
    let secrets: HashMap<&str, String> = passwords
        .iter()
        .map(|row| {
            (
                row.name.as_str(),
                unseal(PASSPHRASE, &parsed.doc.secret_envelope, &row.secret.sealed).unwrap(),
            )
        })
        .collect();
    assert_eq!(secrets["phone"], "app-secret-active");
    assert_eq!(secrets["old-laptop"], "app-secret-revoked");
    assert!(!text.contains("secret_sha256"), "hashes stay derived");
}

#[test]
fn export_carries_follows_and_approvals() {
    let (text, parsed) = exported();
    assert_eq!(parsed.doc.follows.len(), 1);
    assert_eq!(
        parsed.doc.follows[0].artist_mbid,
        "123E4567-E89B-12D3-A456-426614174000"
    );
    assert!(parsed.doc.follows[0].auto_download);
    assert_eq!(parsed.doc.approvals.len(), 1);
    assert_eq!(parsed.doc.approvals[0].source.as_deref(), Some("lidarr"));
    assert!(
        !text.contains("artist_mbid_lower"),
        "derived keys stay derived"
    );
}

#[test]
fn export_seals_config_secrets() {
    let (_text, parsed) = exported();
    let settings = &parsed.doc.settings;
    assert_eq!(
        settings["user_preferences"]["primary_types"],
        json!(["Album"])
    );
    assert_eq!(
        open(&parsed, &settings["download_client"]["api_key"]),
        "legacy-plaintext-slskd"
    );
    assert_eq!(
        open(&parsed, &settings["download_clients"]["sabnzbd"]["api_key"]),
        "sab-key"
    );
    assert_eq!(
        open(&parsed, &settings["prowlarr"]["api_key"]),
        "prowlarr-key"
    );
    assert_eq!(
        open(&parsed, &settings["lastfm_settings"]["api_key"]),
        "lfm-key"
    );
    assert_eq!(
        open(&parsed, &settings["library_settings"]["acoustid_api_key"]),
        "acoustid-key"
    );
    assert_eq!(
        settings["library_settings"]["library_roots"][0]["id"],
        json!("root-1")
    );
}

#[test]
fn export_keeps_indexer_order() {
    let (_text, parsed) = exported();
    let indexers = parsed.doc.settings["indexers"].as_array().unwrap();
    assert_eq!(indexers[0]["name"], json!("first"));
    assert_eq!(indexers[1]["name"], json!("second"));
    assert_eq!(open(&parsed, &indexers[0]["api_key"]), "one-key");
    assert_eq!(open(&parsed, &indexers[1]["api_key"]), "two-key");
}

#[test]
fn export_filters_partial_sections() {
    let (_text, parsed) = exported();
    let settings = &parsed.doc.settings;
    let advanced = &settings["advanced_settings"];
    assert_eq!(
        open(&parsed, &advanced["audiodb_api_key"]),
        "plaintext-audiodb-key"
    );
    assert_eq!(advanced["http_timeout"], json!(30));
    assert!(
        advanced.get("artist_discovery_warm_interval").is_none(),
        "dropped tuning stays behind"
    );

    let policy = &settings["download_policy"];
    assert_eq!(policy["quality_min"], json!("mp3_320"));
    assert!(policy.get("quality_recipe_status").is_none());
    assert!(policy.get("quality_recipe_error").is_none());

    let brainz = &settings["musicbrainz_settings"];
    assert_eq!(brainz["source_mode"], json!("official"));
    assert!(brainz.get("pending_brainzmash").is_none());
    assert!(brainz.get("source_quarantined").is_none());

    let internal = settings["_internal"].as_object().unwrap();
    assert_eq!(internal.len(), 3);
    assert!(internal.contains_key("plex_client_id"));
    assert!(internal.contains_key("droppedneedle_device_id"));
    assert!(internal.contains_key("brainzmash_consent_admin"));

    let demo = &settings["plugins"]["demo"];
    assert_eq!(demo["enabled"], json!(true));
    assert_eq!(open(&parsed, &demo["settings"]["token"]), "plugin-secret");
    assert_eq!(demo["settings"]["theme"], json!("dark"));
    assert!(demo.get("version").is_none());
}

#[test]
fn export_drops_vestigial_sections() {
    let (text, parsed) = exported();
    let settings = &parsed.doc.settings;
    for dropped in [
        "library_sync_settings",
        "library_scan_dirty_scopes",
        "local_files_settings",
        "home_settings",
        "_legacy_lidarr",
    ] {
        assert!(
            settings.get(dropped).is_none(),
            "dropped section survives: {dropped}"
        );
    }
    assert!(
        !text.contains("must-not-survive"),
        "legacy plaintext never crosses"
    );
}

#[test]
fn export_to_file_writes_a_parseable_envelope() {
    let (dir, _key) = fixture_v2_root();
    let out = dir.join("export.json");
    export_v2_to_file(&request(&dir), &out).unwrap();
    let text = std::fs::read_to_string(&out).unwrap();
    assert!(parse_export(&text).unwrap().warnings.is_empty());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = std::fs::metadata(&out).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600, "the export holds password hashes");
    }
}

// The export carries a digest that authenticates every section.
#[test]
fn export_digest_covers_every_section() {
    let (text, parsed) = exported();
    let opener = Opener::derive(PASSPHRASE, &parsed.doc.secret_envelope).unwrap();
    let mut root: Value = serde_json::from_str(&text).unwrap();
    opener.verify(&root).unwrap();
    root["users"][0]["display_name"] = json!("someone else");
    assert_eq!(opener.verify(&root), Err(SealError::DigestMismatch));
}

// A token that fails its MAC means the v2 key is wrong: refuse instead of
// exporting the ciphertext as the secret.
#[test]
fn wrong_v2_key_refuses_token_values() {
    let (dir, _key) = fixture_v2_root();
    let other = FernetKey::generate().unwrap();
    std::fs::write(
        dir.join("config/.env"),
        format!("DATA_ENC_KEY='{}'\n", other.to_base64()),
    )
    .unwrap();
    let out = dir.join("export.json");
    let error = export_v2_to_file(&request(&dir), &out).unwrap_err();
    assert_eq!(error.code(), "V2_KEY_MISMATCH");
    assert!(!out.exists());
}

// The stopped v2 database opens immutable: a WAL database exports
// without the exporter creating its shared-memory file.
#[test]
fn export_reads_the_database_immutable() {
    let (dir, _key) = fixture_v2_root();
    let db = dir.join("cache/library.db");
    let conn = rusqlite::Connection::open(&db).unwrap();
    let mode: String = conn
        .query_row("PRAGMA journal_mode=WAL", [], |row| row.get(0))
        .unwrap();
    assert_eq!(mode, "wal");
    drop(conn);
    let shm = dir.join("cache/library.db-shm");
    assert!(!shm.exists());
    export_v2(&request(&dir)).unwrap();
    assert!(!shm.exists(), "an immutable open touches no side files");
}
