//! The exporter's reads of a v2 database, prepared by the schema test
//! against the v2 fixture.

droppedneedle_kernel::sql! {
    schema = V2;

    /// Whether the v2 database has a table (?1).
    pub(crate) TABLE_EXISTS =
        "SELECT EXISTS (SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1)";
    /// How many v2 tables are named ?1.
    pub(crate) TABLE_COUNT =
        "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = ?";
    /// Every v2 table but SQLite's own, by name.
    pub(crate) TABLE_NAMES = "SELECT name FROM sqlite_master WHERE type = 'table' \
         AND name NOT LIKE 'sqlite_%' ORDER BY name";
    /// The start of a row count; the table name read from the database
    /// follows, quoted.
    pub(crate) fragment COUNT_ROWS_FROM = "SELECT COUNT(*) FROM ";

    /// Every account.
    pub(crate) USERS = "SELECT id, display_name, email, avatar_url, role, created_at,
                    last_login_at, username, username_display
             FROM auth_users ORDER BY id";
    /// Every sign-in provider link.
    pub(crate) PROVIDERS = "SELECT user_id, provider, provider_uid, provider_data, created_at
             FROM auth_providers ORDER BY user_id, provider, provider_uid";
    /// Every app password, with its stored secret.
    pub(crate) APP_PASSWORDS = "SELECT user_id, name, secret_encrypted, created_at, last_used_at,
                    last_client, revoked
             FROM connect_app_passwords ORDER BY user_id, created_at";
    /// Every recovery code hash.
    pub(crate) RECOVERY_CODES = "SELECT user_id, code_hash, created_at, expires_at
             FROM auth_password_recovery_codes";
    /// Per-user service links of accounts that still exist.
    pub(crate) CONNECTIONS = "SELECT user_id, service, connection_data, enabled, created_at, updated_at
             FROM user_connections WHERE user_id IN (SELECT id FROM auth_users)
             ORDER BY user_id, service";
    /// Every followed artist.
    pub(crate) FOLLOWS = "SELECT user_id, artist_mbid, artist_name, auto_download,
                    followed_at, updated_at
             FROM user_followed_artists ORDER BY user_id, artist_mbid_lower";
    /// Every auto-download approval.
    pub(crate) APPROVALS = "SELECT user_id, artist_mbid, artist_name, state, requested_at,
                    reviewed_by_id, reviewed_by_name, reviewed_at, batch_id, source
             FROM auto_download_approvals ORDER BY user_id, artist_mbid_lower";
    /// Every saved concert city.
    pub(crate) EVENT_CITIES = "SELECT user_id, city_name, country_code, latitude, longitude, radius_km,
                    position
             FROM user_event_cities ORDER BY user_id, position";
    /// When each user last looked at their concerts.
    pub(crate) EVENT_SEEN = "SELECT user_id, seen_at FROM user_event_seen ORDER BY user_id";
}
