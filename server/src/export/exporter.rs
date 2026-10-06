//! The v2 exporter: one instance dir in, one sealed envelope out.
//!
//! The exporter reads a v2 instance read-only (key file, config, database,
//! plugin manifests), resolves every secret through v2 `decrypt()` semantics
//! (Fernet or legacy-plaintext passthrough), seals each secret under the
//! operator passphrase, and assembles the envelope. Only the migrated set leaves
//! v2: accounts, settings, follows. Tokens, sessions, derived follow
//! tables, scan state, history, queues, and jobs stay behind.
//!
//! Two v2 plaintext-at-rest bugs close here: the AudioDB key and
//! secret-flagged plugin settings were stored unencrypted in v2, so they
//! seal straight from their stored form (never through Fernet first, which
//! could mangle a value that happens to parse as a token). Dropped sections
//! and fields never enter the file at all.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use rusqlite::{Connection, OpenFlags};
use serde_json::{Map, Value, json};

use crate::export::envelope::{
    ApprovalRecord, EventCityRecord, EventSeenRecord, ExportDoc, FollowRecord, SealedValue,
    UserRecord, derive_hash_scheme,
};
use crate::export::error::ExportError;
use crate::export::fernet::FernetKey;
use crate::export::seal::Sealer;
use crate::export::{EXPORT_FORMAT, FORMAT_VERSION};
use crate::runtime_config::Secret;

/// Sections exported verbatim (secret positions sealed where present).
const KEPT_SECTIONS: &[&str] = &[
    "user_preferences",
    "library_scan_schedule",
    "library_scan_filesystem_watcher",
    "download_client",
    "download_clients",
    "wanted",
    "source_priority",
    "usenet_search_backend",
    "indexers",
    "prowlarr",
    "lidarr_import",
    "jellyfin_settings",
    "navidrome_settings",
    "plex_settings",
    "listenbrainz_settings",
    "youtube_settings",
    "lastfm_settings",
    "spotify_settings",
    "events",
    "wrapped_settings",
    "oidc_settings",
    "security_settings",
    "connect_apps",
    "library_settings",
    "library_management",
    "scrobble_settings",
    "primary_music_source",
    "free_music",
    "get_it",
];

/// Top-level secret fields per section, resolved through v2 Fernet
/// semantics (including legacy-plaintext passthrough) and then sealed.
/// Nested positions (`download_clients.sabnzbd`, `indexers[]`) and
/// plaintext-at-rest positions (AudioDB, plugins) are handled in code.
const SECTION_SECRETS: &[(&str, &[&str])] = &[
    ("download_client", &["api_key"]),
    ("prowlarr", &["api_key"]),
    ("lidarr_import", &["api_key"]),
    ("jellyfin_settings", &["api_key"]),
    ("navidrome_settings", &["password"]),
    ("plex_settings", &["plex_token"]),
    ("listenbrainz_settings", &["user_token"]),
    ("youtube_settings", &["api_key"]),
    (
        "lastfm_settings",
        &["api_key", "shared_secret", "session_key"],
    ),
    ("spotify_settings", &["client_secret"]),
    ("events", &["ticketmaster_api_key", "skiddle_api_key"]),
    ("wrapped_settings", &["api_key"]),
    ("oidc_settings", &["client_secret"]),
    ("library_settings", &["acoustid_api_key"]),
];

/// Explicit `advanced_settings` keep-list. `cache_ttl_*` and
/// `frontend_ttl_*` keep by prefix; [`ADVANCED_DROPPED`] wins over both.
const ADVANCED_KEPT: &[&str] = &[
    "http_timeout",
    "http_connect_timeout",
    "http_max_connections",
    "batch_artist_images",
    "batch_albums",
    "delay_artist",
    "delay_albums",
    "memory_cache_max_entries",
    "memory_cache_cleanup_interval",
    "cover_memory_cache_max_entries",
    "cover_memory_cache_max_size_mb",
    "disk_cache_cleanup_interval",
    "recent_metadata_max_size_mb",
    "recent_covers_max_size_mb",
    "persistent_metadata_ttl_hours",
    "discover_queue_size",
    "discover_queue_ttl",
    "discover_queue_auto_generate",
    "discover_queue_polling_interval",
    "discover_queue_seed_artists",
    "discover_queue_wildcard_slots",
    "discover_picks_count",
    "discover_picks_genre_affinity_weight",
    "audiodb_enabled",
    "audiodb_name_search_fallback",
    "audiodb_api_key",
    "cache_ttl_audiodb_found",
    "cache_ttl_audiodb_not_found",
    "cache_ttl_audiodb_library",
    "prefer_local_cover_art",
    "direct_remote_images_enabled",
    "genre_section_ttl",
    "request_concurrency",
    "request_history_retention_days",
    "ignored_releases_retention_days",
    "orphan_cover_demote_interval_hours",
    "store_prune_interval_hours",
    "sync_stall_timeout_minutes",
    "sync_max_timeout_hours",
];

/// `advanced_settings` fields that never survive, even when they match a
/// keep prefix. Everything outside the keep rules drops too (closed list).
const ADVANCED_DROPPED: &[&str] = &[
    "artist_discovery_warm_interval",
    "artist_discovery_warm_delay",
    "artist_discovery_precache_delay",
    "artist_discovery_precache_concurrency",
    "discover_queue_warm_cycle_build",
    "discover_queue_similar_artists_limit",
    "discover_queue_albums_per_similar",
    "discover_queue_enrich_ttl",
    "discover_queue_lastfm_mbid_max_lookups",
    "audiodb_prewarm_concurrency",
    "audiodb_prewarm_delay",
    "cache_ttl_recently_viewed_bytes",
    "cache_ttl_local_files_recently_added",
];

/// `download_policy` read-only projection metadata. v2 itself strips these
/// before save; they must not cross into the export.
const DOWNLOAD_POLICY_DROPPED: &[&str] = &["quality_recipe_status", "quality_recipe_error"];

/// `musicbrainz_settings` keep-list (transient proposal and quarantine
/// state stays behind).
const MUSICBRAINZ_KEPT: &[&str] = &[
    "source_mode",
    "api_url",
    "rate_limit",
    "concurrent_searches",
    "community_acknowledged",
    "selected_source_mode",
    "source_id",
    "generation",
    "active_brainzmash",
];

/// `_internal` keep-list (device and consent identity only).
const INTERNAL_KEPT: &[&str] = &[
    "plex_client_id",
    "droppedneedle_device_id",
    "brainzmash_consent_admin",
];

/// One export run's inputs. Everything is borrowed or owned plainly; the
/// passphrase stays inside [`Secret`] so a debug print cannot leak it.
#[derive(Debug)]
pub struct ExportRequest {
    /// v2 instance root (`<ROOT_APP_DIR>`).
    pub v2_root: PathBuf,
    /// Database override; defaults to `<root>/cache/library.db`.
    pub db_path: Option<PathBuf>,
    /// Operator passphrase sealing every secret in the file.
    pub passphrase: Secret,
    /// Export timestamp override (tests pin this); defaults to now in UTC.
    pub exported_at: Option<String>,
    /// Best-effort v2 provenance, carried when known.
    pub v2_commit: Option<String>,
}

/// Current UTC time as RFC 3339 (`2026-09-28T12:00:00Z`).
pub fn utc_now_rfc3339() -> Result<String, ExportError> {
    time::OffsetDateTime::now_utc()
        .format(&time::format_description::well_known::Rfc3339)
        .map_err(|_| ExportError::Timestamp)
}

/// Export one v2 instance into a sealed envelope document.
///
/// Order matters: the v2 key check refuses first (a missing key must
/// never silently re-key ciphertext), then config, users, follows, and
/// settings assemble in that order. Nothing is written anywhere; see
/// [`export_v2_to_file`] for the atomic file form.
pub fn export_v2(request: &ExportRequest) -> Result<ExportDoc, ExportError> {
    let key = crate::export::v2dir::read_data_enc_key(&request.v2_root)?;
    let config = crate::export::v2dir::read_config(&request.v2_root)?;
    let instance_id = config
        .get("instance_id")
        .and_then(Value::as_str)
        .ok_or(ExportError::InstanceIdMissing)?
        .to_owned();
    let sealer = Sealer::generate(request.passphrase.expose())?;
    let db_path =
        crate::export::v2dir::resolve_db_path(&request.v2_root, request.db_path.as_deref());
    let db = open_v2_db(&db_path)?;
    let mut users = read_users(&db)?;
    attach_user_rows(&db, &mut users, &key, &sealer)?;
    let follows = read_follows(&db)?;
    let approvals = read_approvals(&db)?;
    let event_cities = read_event_cities(&db)?;
    let event_seen = read_event_seen(&db)?;
    let plugin_flags = crate::export::v2dir::plugin_setting_flags(&request.v2_root);
    let settings = build_settings(&config, &key, &sealer, &plugin_flags)?;
    let exported_at = request
        .exported_at
        .clone()
        .map_or_else(utc_now_rfc3339, Ok)?;
    let mut doc = ExportDoc {
        format: EXPORT_FORMAT.to_owned(),
        format_version: FORMAT_VERSION,
        exported_at,
        v2_commit: request.v2_commit.clone(),
        instance_id,
        secret_envelope: sealer.secret_envelope(),
        users,
        settings,
        follows,
        approvals,
        event_cities,
        event_seen,
        content_hmac: String::new(),
    };
    let value = serde_json::to_value(&doc).map_err(|error| ExportError::InvalidEnvelope {
        reason: format!("cannot serialize export document: {error}"),
    })?;
    doc.content_hmac = sealer.digest(&value)?;
    Ok(doc)
}

/// Export one v2 instance straight to a file. The document fully builds
/// before anything is written, so a refusal leaves no file behind; the
/// write itself lands via temp-file-plus-rename.
pub fn export_v2_to_file(request: &ExportRequest, out_path: &Path) -> Result<(), ExportError> {
    let doc = export_v2(request)?;
    let text = doc.to_json_string()?;
    let parent = out_path.parent().filter(|dir| !dir.as_os_str().is_empty());
    if let Some(dir) = parent {
        std::fs::create_dir_all(dir).map_err(|error| ExportError::ExportWrite {
            reason: error.to_string(),
        })?;
    }
    let tmp_path = out_path.with_extension(format!(
        "tmp-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|elapsed| elapsed.as_nanos())
            .unwrap_or(0)
    ));
    write_private(&tmp_path, text.as_bytes()).map_err(|error| {
        let _ = std::fs::remove_file(&tmp_path);
        ExportError::ExportWrite {
            reason: error.to_string(),
        }
    })?;
    std::fs::rename(&tmp_path, out_path).map_err(|error| {
        let _ = std::fs::remove_file(&tmp_path);
        ExportError::ExportWrite {
            reason: error.to_string(),
        }
    })?;
    Ok(())
}

/// Write `bytes` to a new owner-only file and flush it to disk. The export
/// holds password and recovery hashes.
fn write_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write as _;
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    file.write_all(bytes)?;
    file.sync_all()
}

/// Open the stopped v2 database read-only and immutable, so the export
/// works from a read-only mount (no WAL or shm files are touched).
fn open_v2_db(path: &Path) -> Result<Connection, ExportError> {
    if !path.is_file() {
        return Err(ExportError::V2Database {
            table: "database".to_owned(),
            detail: format!("{} is not a file", path.display()),
        });
    }
    // An immutable open ignores the write-ahead log, so committed writes
    // still sitting in it would be silently missing from the export.
    let mut wal = path.as_os_str().to_owned();
    wal.push("-wal");
    let wal = PathBuf::from(wal);
    if std::fs::metadata(&wal).is_ok_and(|meta| meta.len() > 0) {
        return Err(ExportError::V2WalPresent { path: wal });
    }
    let uri = format!("file:{}?immutable=1", uri_path(path));
    Connection::open_with_flags(
        uri,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI,
    )
    .map_err(|error| ExportError::V2Database {
        table: "database".to_owned(),
        detail: error.to_string(),
    })
}

/// Percent-encode the characters a SQLite URI path cannot carry raw.
fn uri_path(path: &Path) -> String {
    let mut out = String::new();
    for ch in path.to_string_lossy().chars() {
        match ch {
            '%' => out.push_str("%25"),
            '?' => out.push_str("%3f"),
            '#' => out.push_str("%23"),
            other => out.push(other),
        }
    }
    out
}

/// Resolve one stored v2 secret, refusing a token the key cannot open.
fn decrypt_v2(key: &FernetKey, stored: &str, field: &str) -> Result<String, ExportError> {
    key.decrypt_legacy(stored)
        .map(|(plaintext, _legacy)| plaintext)
        .map_err(|_| ExportError::V2KeyMismatch {
            field: field.to_owned(),
        })
}

fn db_error(table: &str, error: rusqlite::Error) -> ExportError {
    ExportError::V2Database {
        table: table.to_owned(),
        detail: error.to_string(),
    }
}

fn read_users(db: &Connection) -> Result<Vec<UserRecord>, ExportError> {
    let mut stmt = db
        .prepare(
            "SELECT id, display_name, email, avatar_url, role, created_at,
                    last_login_at, username, username_display
             FROM auth_users ORDER BY id",
        )
        .map_err(|error| db_error("auth_users", error))?;
    let rows = stmt
        .query_map([], |row| {
            Ok(UserRecord {
                id: row.get(0)?,
                display_name: row.get(1)?,
                email: row.get(2)?,
                avatar_url: row.get(3)?,
                role: row.get(4)?,
                created_at: row.get(5)?,
                last_login_at: row.get(6)?,
                username: row.get(7)?,
                username_display: row.get(8)?,
                providers: Vec::new(),
                app_passwords: Vec::new(),
                recovery_code: None,
            })
        })
        .map_err(|error| db_error("auth_users", error))?;
    let mut users = Vec::new();
    for row in rows {
        users.push(row.map_err(|error| db_error("auth_users", error))?);
    }
    Ok(users)
}

/// Attach providers, app passwords, and recovery codes to their users.
/// v2 foreign keys make orphans impossible; anything unmatched is skipped
/// rather than inventing an account.
fn attach_user_rows(
    db: &Connection,
    users: &mut [UserRecord],
    key: &FernetKey,
    sealer: &Sealer,
) -> Result<(), ExportError> {
    let mut by_id: HashMap<String, usize> = HashMap::new();
    for (index, user) in users.iter().enumerate() {
        by_id.insert(user.id.clone(), index);
    }

    let mut stmt = db
        .prepare(
            "SELECT user_id, provider, provider_uid, provider_data, created_at
             FROM auth_providers ORDER BY user_id, provider, provider_uid",
        )
        .map_err(|error| db_error("auth_providers", error))?;
    let rows = stmt
        .query_map([], |row| {
            let user_id: String = row.get(0)?;
            let provider: String = row.get(1)?;
            let provider_uid: String = row.get(2)?;
            let provider_data: Option<String> = row.get(3)?;
            let created_at: String = row.get(4)?;
            Ok((user_id, provider, provider_uid, provider_data, created_at))
        })
        .map_err(|error| db_error("auth_providers", error))?;
    for row in rows {
        let (user_id, provider, provider_uid, provider_data, created_at) =
            row.map_err(|error| db_error("auth_providers", error))?;
        if let Some(&index) = by_id.get(user_id.as_str()) {
            let hash_scheme = derive_hash_scheme(&provider, provider_data.as_deref());
            users[index]
                .providers
                .push(crate::export::envelope::ProviderRecord {
                    provider,
                    provider_uid,
                    provider_data,
                    hash_scheme,
                    created_at,
                });
        }
    }

    let mut stmt = db
        .prepare(
            "SELECT user_id, name, secret_encrypted, created_at, last_used_at,
                    last_client, revoked
             FROM connect_app_passwords ORDER BY user_id, created_at",
        )
        .map_err(|error| db_error("connect_app_passwords", error))?;
    let rows = stmt
        .query_map([], |row| {
            let user_id: String = row.get(0)?;
            let name: String = row.get(1)?;
            let secret_encrypted: String = row.get(2)?;
            let created_at: String = row.get(3)?;
            let last_used_at: Option<String> = row.get(4)?;
            let last_client: Option<String> = row.get(5)?;
            let revoked: i64 = row.get(6)?;
            Ok((
                user_id,
                name,
                secret_encrypted,
                created_at,
                last_used_at,
                last_client,
                revoked,
            ))
        })
        .map_err(|error| db_error("connect_app_passwords", error))?;
    for row in rows {
        let (user_id, name, secret_encrypted, created_at, last_used_at, last_client, revoked) =
            row.map_err(|error| db_error("connect_app_passwords", error))?;
        if let Some(&index) = by_id.get(user_id.as_str()) {
            let plaintext = decrypt_v2(key, &secret_encrypted, "connect_app_passwords")?;
            let sealed = sealer.seal(&plaintext)?;
            users[index]
                .app_passwords
                .push(crate::export::envelope::AppPasswordRecord {
                    name,
                    secret: SealedValue { sealed },
                    created_at,
                    last_used_at,
                    last_client,
                    revoked: revoked != 0,
                });
        }
    }

    let mut stmt = db
        .prepare(
            "SELECT user_id, code_hash, created_at, expires_at
             FROM auth_password_recovery_codes",
        )
        .map_err(|error| db_error("auth_password_recovery_codes", error))?;
    let rows = stmt
        .query_map([], |row| {
            let user_id: String = row.get(0)?;
            let code_hash: String = row.get(1)?;
            let created_at: String = row.get(2)?;
            let expires_at: String = row.get(3)?;
            Ok((user_id, code_hash, created_at, expires_at))
        })
        .map_err(|error| db_error("auth_password_recovery_codes", error))?;
    for row in rows {
        let (user_id, code_hash, created_at, expires_at) =
            row.map_err(|error| db_error("auth_password_recovery_codes", error))?;
        if let Some(&index) = by_id.get(user_id.as_str()) {
            users[index].recovery_code = Some(crate::export::envelope::RecoveryCode {
                code_hash,
                created_at,
                expires_at,
            });
        }
    }
    Ok(())
}

fn read_follows(db: &Connection) -> Result<Vec<FollowRecord>, ExportError> {
    let mut stmt = db
        .prepare(
            "SELECT user_id, artist_mbid, artist_name, auto_download,
                    followed_at, updated_at
             FROM user_followed_artists ORDER BY user_id, artist_mbid_lower",
        )
        .map_err(|error| db_error("user_followed_artists", error))?;
    let rows = stmt
        .query_map([], |row| {
            let auto_download: i64 = row.get(3)?;
            Ok(FollowRecord {
                user_id: row.get(0)?,
                artist_mbid: row.get(1)?,
                artist_name: row.get(2)?,
                auto_download: auto_download != 0,
                followed_at: row.get(4)?,
                updated_at: row.get(5)?,
            })
        })
        .map_err(|error| db_error("user_followed_artists", error))?;
    let mut follows = Vec::new();
    for row in rows {
        follows.push(row.map_err(|error| db_error("user_followed_artists", error))?);
    }
    Ok(follows)
}

fn read_approvals(db: &Connection) -> Result<Vec<ApprovalRecord>, ExportError> {
    let mut stmt = db
        .prepare(
            "SELECT user_id, artist_mbid, artist_name, state, requested_at,
                    reviewed_by_id, reviewed_by_name, reviewed_at, batch_id, source
             FROM auto_download_approvals ORDER BY user_id, artist_mbid_lower",
        )
        .map_err(|error| db_error("auto_download_approvals", error))?;
    let rows = stmt
        .query_map([], |row| {
            Ok(ApprovalRecord {
                user_id: row.get(0)?,
                artist_mbid: row.get(1)?,
                artist_name: row.get(2)?,
                state: row.get(3)?,
                requested_at: row.get(4)?,
                reviewed_by_id: row.get(5)?,
                reviewed_by_name: row.get(6)?,
                reviewed_at: row.get(7)?,
                batch_id: row.get(8)?,
                source: row.get(9)?,
            })
        })
        .map_err(|error| db_error("auto_download_approvals", error))?;
    let mut approvals = Vec::new();
    for row in rows {
        approvals.push(row.map_err(|error| db_error("auto_download_approvals", error))?);
    }
    Ok(approvals)
}

/// Whether the v2 database has `table`. The concerts tables only exist once
/// a v2 server with the feature has started, so their absence is empty.
fn has_table(db: &Connection, table: &str) -> Result<bool, ExportError> {
    db.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = ?",
        [table],
        |row| row.get::<_, i64>(0),
    )
    .map(|count| count > 0)
    .map_err(|error| db_error(table, error))
}

fn read_event_cities(db: &Connection) -> Result<Vec<EventCityRecord>, ExportError> {
    if !has_table(db, "user_event_cities")? {
        return Ok(Vec::new());
    }
    let mut stmt = db
        .prepare(
            "SELECT user_id, city_name, country_code, latitude, longitude, radius_km,
                    position
             FROM user_event_cities ORDER BY user_id, position",
        )
        .map_err(|error| db_error("user_event_cities", error))?;
    let rows = stmt
        .query_map([], |row| {
            Ok(EventCityRecord {
                user_id: row.get(0)?,
                city_name: row.get(1)?,
                country_code: row.get(2)?,
                latitude: row.get(3)?,
                longitude: row.get(4)?,
                radius_km: row.get(5)?,
                position: row.get(6)?,
            })
        })
        .map_err(|error| db_error("user_event_cities", error))?;
    rows.collect::<Result<_, _>>()
        .map_err(|error| db_error("user_event_cities", error))
}

fn read_event_seen(db: &Connection) -> Result<Vec<EventSeenRecord>, ExportError> {
    if !has_table(db, "user_event_seen")? {
        return Ok(Vec::new());
    }
    let mut stmt = db
        .prepare("SELECT user_id, seen_at FROM user_event_seen ORDER BY user_id")
        .map_err(|error| db_error("user_event_seen", error))?;
    let rows = stmt
        .query_map([], |row| {
            Ok(EventSeenRecord {
                user_id: row.get(0)?,
                seen_at: row.get(1)?,
            })
        })
        .map_err(|error| db_error("user_event_seen", error))?;
    rows.collect::<Result<_, _>>()
        .map_err(|error| db_error("user_event_seen", error))
}

/// Replace one stored string with its sealed object, resolving through v2
/// Fernet semantics first (legacy plaintext seals as-is). Absent or
/// non-string values pass through for the validator to judge.
fn seal_v2_field(
    section: &mut Map<String, Value>,
    field: &str,
    key: &FernetKey,
    sealer: &Sealer,
) -> Result<(), ExportError> {
    let stored = section.get(field).and_then(Value::as_str);
    let Some(stored) = stored else {
        return Ok(());
    };
    let plaintext = decrypt_v2(key, stored, field)?;
    let sealed = sealer.seal(&plaintext)?;
    section.insert(field.to_owned(), json!({ "$sealed": sealed }));
    Ok(())
}

/// Seal one plaintext-at-rest string (AudioDB, plugin secrets) straight
/// from its stored form: these were never Fernet-encrypted, so running
/// Fernet first could mangle a value that happens to parse as a token.
fn seal_plaintext_field(
    section: &mut Map<String, Value>,
    field: &str,
    sealer: &Sealer,
) -> Result<(), ExportError> {
    let stored = section.get(field).and_then(Value::as_str);
    let Some(stored) = stored else {
        return Ok(());
    };
    let sealed = sealer.seal(stored)?;
    section.insert(field.to_owned(), json!({ "$sealed": sealed }));
    Ok(())
}

fn advanced_kept(field: &str) -> bool {
    if ADVANCED_DROPPED.contains(&field) {
        return false;
    }
    field.starts_with("cache_ttl_")
        || field.starts_with("frontend_ttl_")
        || ADVANCED_KEPT.contains(&field)
}

fn export_plugins(
    value: &Value,
    sealer: &Sealer,
    plugin_flags: &HashMap<String, HashMap<String, bool>>,
) -> Result<Value, ExportError> {
    let Some(entries) = value.as_object() else {
        return Ok(value.clone());
    };
    let mut out = Map::new();
    for (name, entry) in entries {
        let Some(entry) = entry.as_object() else {
            out.insert(name.clone(), entry.clone());
            continue;
        };
        let mut kept = Map::new();
        if let Some(enabled) = entry.get("enabled") {
            kept.insert("enabled".to_owned(), enabled.clone());
        }
        if let Some(settings) = entry.get("settings") {
            let Some(settings) = settings.as_object() else {
                kept.insert("settings".to_owned(), settings.clone());
                out.insert(name.clone(), Value::Object(kept));
                continue;
            };
            let declared = plugin_flags.get(name);
            let mut sealed_settings = Map::new();
            for (setting_key, setting_value) in settings {
                let Some(stored) = setting_value.as_str() else {
                    sealed_settings.insert(setting_key.clone(), setting_value.clone());
                    continue;
                };
                let known_plain = declared
                    .and_then(|flags| flags.get(setting_key))
                    .is_some_and(|secret| !secret);
                if known_plain {
                    sealed_settings.insert(setting_key.clone(), setting_value.clone());
                } else {
                    let sealed = sealer.seal(stored)?;
                    sealed_settings.insert(setting_key.clone(), json!({ "$sealed": sealed }));
                }
            }
            kept.insert("settings".to_owned(), Value::Object(sealed_settings));
        }
        out.insert(name.clone(), Value::Object(kept));
    }
    Ok(Value::Object(out))
}

#[allow(clippy::too_many_lines)]
fn build_settings(
    config: &Map<String, Value>,
    key: &FernetKey,
    sealer: &Sealer,
    plugin_flags: &HashMap<String, HashMap<String, bool>>,
) -> Result<Map<String, Value>, ExportError> {
    let mut settings = Map::new();
    for section in KEPT_SECTIONS {
        if let Some(value) = config.get(*section) {
            settings.insert((*section).to_owned(), value.clone());
        }
    }

    for (section, fields) in SECTION_SECRETS {
        if let Some(Value::Object(map)) = settings.get_mut(*section) {
            for field in *fields {
                seal_v2_field(map, field, key, sealer)?;
            }
        }
    }

    if let Some(Value::Object(clients)) = settings.get_mut("download_clients")
        && let Some(Value::Object(sabnzbd)) = clients.get_mut("sabnzbd")
    {
        seal_v2_field(sabnzbd, "api_key", key, sealer)?;
    }
    if let Some(Value::Array(indexers)) = settings.get_mut("indexers") {
        for indexer in indexers {
            if let Value::Object(map) = indexer {
                seal_v2_field(map, "api_key", key, sealer)?;
            }
        }
    }

    if let Some(value) = config.get("advanced_settings") {
        let mut filtered = Map::new();
        if let Some(map) = value.as_object() {
            for (field, field_value) in map {
                if advanced_kept(field) {
                    filtered.insert(field.clone(), field_value.clone());
                }
            }
            seal_plaintext_field(&mut filtered, "audiodb_api_key", sealer)?;
            settings.insert("advanced_settings".to_owned(), Value::Object(filtered));
        } else {
            settings.insert("advanced_settings".to_owned(), value.clone());
        }
    }
    if let Some(value) = config.get("download_policy") {
        if let Some(map) = value.as_object() {
            let mut filtered = map.clone();
            for field in DOWNLOAD_POLICY_DROPPED {
                filtered.remove(*field);
            }
            settings.insert("download_policy".to_owned(), Value::Object(filtered));
        } else {
            settings.insert("download_policy".to_owned(), value.clone());
        }
    }
    if let Some(value) = config.get("musicbrainz_settings") {
        if let Some(map) = value.as_object() {
            let mut filtered = Map::new();
            for (field, field_value) in map {
                if MUSICBRAINZ_KEPT.contains(&field.as_str()) {
                    filtered.insert(field.clone(), field_value.clone());
                }
            }
            settings.insert("musicbrainz_settings".to_owned(), Value::Object(filtered));
        } else {
            settings.insert("musicbrainz_settings".to_owned(), value.clone());
        }
    }
    if let Some(value) = config.get("plugins") {
        settings.insert(
            "plugins".to_owned(),
            export_plugins(value, sealer, plugin_flags)?,
        );
    }
    if let Some(value) = config.get("_internal") {
        if let Some(map) = value.as_object() {
            let mut filtered = Map::new();
            for (field, field_value) in map {
                if INTERNAL_KEPT.contains(&field.as_str()) {
                    filtered.insert(field.clone(), field_value.clone());
                }
            }
            settings.insert("_internal".to_owned(), Value::Object(filtered));
        } else {
            settings.insert("_internal".to_owned(), value.clone());
        }
    }
    Ok(settings)
}
