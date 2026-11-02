//! The importer's reads of the target, and its audit rows. Everything else
//! the import writes goes through the owning area's repository.

use droppedneedle_kernel::db::statement::Statement;
use rusqlite::{Connection, Row};

droppedneedle_kernel::sql! {
    /// Every account in the target.
    pub(super) USERS = "SELECT id, display_name, email, avatar_url, role, created_at, \
             last_login_at, username, username_display FROM auth_users";
    /// Every sign-in provider link in the target.
    pub(super) PROVIDERS = "SELECT user_id, provider, provider_uid, provider_data, password_hash, \
             hash_scheme, created_at FROM auth_providers";
    /// Every app password in the target.
    pub(super) APP_PASSWORDS = "SELECT user_id, name, secret_sha256, created_at, last_used_at, \
             last_client, revoked FROM auth_app_passwords";
    /// Every recovery code in the target.
    pub(super) RECOVERY_CODES = "SELECT user_id, code_hash, created_at, expires_at \
             FROM auth_password_recovery_codes";
    /// Every followed artist in the target.
    pub(super) FOLLOWS = "SELECT user_id, artist_mbid_lower, auto_download, followed_at, \
             updated_at FROM acquire_followed_artists";
    /// Every auto-download approval in the target.
    pub(super) APPROVALS = "SELECT user_id, artist_mbid_lower, state, requested_at, \
             reviewed_by_id, reviewed_by_name, reviewed_at, batch_id, source \
             FROM auto_download_approvals";
    /// Every saved concert city in the target.
    pub(super) EVENT_CITIES = "SELECT user_id, city_name, country_code, latitude, longitude, radius_km, \
             position FROM user_event_cities";
    /// When each user in the target last looked at their concerts.
    pub(super) EVENT_SEEN = "SELECT user_id, seen_at FROM user_event_seen";

    /// An account in the target.
    #[derive(Debug, Clone)]
    pub(super) row UserRow for [USERS] {
        id: String,
        display_name: String,
        email: Option<String>,
        avatar_url: Option<String>,
        role: String,
        created_at: i64,
        last_login_at: Option<i64>,
        username: Option<String>,
        username_display: Option<String>,
    }
    /// A sign-in provider link in the target.
    #[derive(Debug, Clone)]
    pub(super) row ProviderRow for [PROVIDERS] {
        user_id: String,
        provider: String,
        provider_uid: String,
        provider_data: Option<String>,
        password_hash: Option<String>,
        hash_scheme: Option<String>,
        created_at: i64,
    }
    /// An app password in the target.
    #[derive(Debug, Clone)]
    pub(super) row AppPasswordRow for [APP_PASSWORDS] {
        user_id: String,
        name: String,
        secret_sha256: String,
        created_at: i64,
        last_used_at: Option<i64>,
        last_client: Option<String>,
        revoked: i64,
    }
    /// A recovery code in the target.
    #[derive(Debug, Clone)]
    pub(super) row RecoveryRow for [RECOVERY_CODES] {
        user_id: String,
        code_hash: String,
        created_at: i64,
        expires_at: i64,
    }
    /// A followed artist in the target.
    #[derive(Debug, Clone)]
    pub(super) row FollowRow for [FOLLOWS] {
        user_id: String,
        artist_mbid_lower: String,
        auto_download: i64,
        followed_at: i64,
        updated_at: i64,
    }
    /// An auto-download approval in the target.
    #[derive(Debug, Clone)]
    pub(super) row ApprovalRow for [APPROVALS] {
        user_id: String,
        artist_mbid_lower: String,
        state: String,
        requested_at: i64,
        reviewed_by_id: Option<String>,
        reviewed_by_name: Option<String>,
        reviewed_at: Option<i64>,
        batch_id: Option<String>,
        source: Option<String>,
    }
    /// A saved concert city in the target.
    #[derive(Debug, Clone)]
    pub(super) row EventCityRow for [EVENT_CITIES] {
        user_id: String,
        city_name: String,
        country_code: Option<String>,
        latitude: f64,
        longitude: f64,
        radius_km: f64,
        position: i64,
    }
    /// When a user in the target last looked at their concerts.
    pub(super) row EventSeenRow for [EVENT_SEEN] {
        user_id: String,
        seen_at: f64,
    }

    /// Add an account.
    pub(super) INSERT_USER = "INSERT INTO auth_users (id, display_name, email, avatar_url, role, \
             created_at, last_login_at, username, username_display) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)";
    /// Add a sign-in provider link.
    pub(super) INSERT_PROVIDER = "INSERT INTO auth_providers (id, user_id, provider, provider_uid, \
             provider_data, password_hash, hash_scheme, created_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?)";
    /// Add an app password.
    pub(super) INSERT_APP_PASSWORD = "INSERT INTO auth_app_passwords (id, user_id, name, secret_sha256, \
             secret_encrypted, created_at, last_used_at, last_client, revoked) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)";
    /// Add a recovery code.
    pub(super) INSERT_RECOVERY_CODE = "INSERT INTO auth_password_recovery_codes (user_id, code_hash, \
             created_at, expires_at) VALUES (?, ?, ?, ?)";
    /// Add a followed artist.
    pub(super) INSERT_FOLLOW = "INSERT INTO acquire_followed_artists (user_id, artist_mbid, \
             artist_mbid_lower, artist_name, auto_download, followed_at, \
             updated_at) VALUES (?, ?, ?, ?, ?, ?, ?)";
    /// Write a follow's merged values.
    pub(super) MERGE_FOLLOW = "UPDATE acquire_followed_artists SET auto_download = ?, followed_at = ?, \
             updated_at = ? WHERE user_id = ? AND artist_mbid_lower = ?";
    /// Add a concert city.
    pub(super) INSERT_EVENT_CITY = "INSERT INTO user_event_cities (user_id, city_name, country_code, latitude, \
             longitude, radius_km, position) VALUES (?, ?, ?, ?, ?, ?, ?)";
    /// Set when a user last looked at their concerts.
    pub(super) SET_EVENT_SEEN = "INSERT INTO user_event_seen (user_id, seen_at) VALUES (?, ?) \
             ON CONFLICT(user_id) DO UPDATE SET seen_at = excluded.seen_at";
    /// Add an auto-download approval.
    pub(super) INSERT_APPROVAL = "INSERT INTO auto_download_approvals (user_id, artist_mbid, \
             artist_mbid_lower, artist_name, state, requested_at, reviewed_by_id, \
             reviewed_by_name, reviewed_at, batch_id, source) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)";
    /// Write an approval's merged values.
    pub(super) MERGE_APPROVAL = "UPDATE auto_download_approvals SET state = ?, requested_at = ?, \
             reviewed_by_id = ?, reviewed_by_name = ?, reviewed_at = ?, \
             batch_id = ?, source = ? WHERE user_id = ? AND artist_mbid_lower = ?";
    /// Add the audit row of an import run.
    pub(super) INSERT_IMPORT_RUN = "INSERT INTO import_runs (id, instance_id, exported_at, exit_code, \
             entity_counts, applied_at) VALUES (?, ?, ?, ?, ?, ?)";
    /// Refresh an import run's audit row.
    pub(super) UPDATE_IMPORT_RUN = "UPDATE import_runs SET exit_code = ?, entity_counts = ? WHERE id = ?";
}

/// Every row a statement with no parameters answers.
pub(super) fn all_rows<T>(
    conn: &Connection,
    statement: Statement,
    read: fn(&Row<'_>) -> rusqlite::Result<T>,
) -> rusqlite::Result<Vec<T>> {
    let mut prepared = conn.prepare(statement.text())?;
    let rows = prepared.query_map([], read)?;
    rows.collect()
}
