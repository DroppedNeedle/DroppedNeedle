//! The statements of the account, credential, app-password, recovery-code
//! and Last.fm link repositories.

use droppedneedle_kernel::db::statement::ListLen;

droppedneedle_kernel::sql! {
    /// Columns read for every user row, in [`super::users`]' mapping order.
    pub(super) fragment USER_COLUMNS = "id, display_name, email, avatar_url, role, created_at, \
        last_login_at, username, username_display";
    /// One user by id.
    pub(super) USER_BY_ID = "SELECT " + USER_COLUMNS + " FROM auth_users WHERE id = ?1";
    /// One user by lowercased username.
    pub(super) USER_BY_USERNAME = "SELECT " + USER_COLUMNS + " FROM auth_users WHERE username = ?1";
    /// One user by lowercased email.
    pub(super) USER_BY_EMAIL = "SELECT " + USER_COLUMNS + " FROM auth_users WHERE email = ?1";
    /// Several users by id.
    family USERS_BY_IDS = users_by_ids;
    /// One page of users: creation order, id tiebreak.
    pub(super) USER_PAGE = "SELECT " + USER_COLUMNS + " FROM auth_users \
        ORDER BY created_at ASC, id ASC LIMIT ?1 OFFSET ?2";
    /// Every account.
    pub(super) USER_COUNT = "SELECT COUNT(*) FROM auth_users";
    /// Whether any account exists.
    pub(super) ANY_USER = "SELECT 1 FROM auth_users LIMIT 1";
    /// Whether one account exists.
    pub(super) USER_EXISTS = "SELECT 1 FROM auth_users WHERE id = ?1";
    /// One account's role.
    pub(super) USER_ROLE = "SELECT role FROM auth_users WHERE id = ?1";
    /// One account's username.
    pub(super) USER_USERNAME = "SELECT username FROM auth_users WHERE id = ?1";
    /// Accounts holding one role.
    pub(super) ROLE_COUNT = "SELECT COUNT(*) FROM auth_users WHERE role = ?1";
    /// Insert one account.
    pub(super) INSERT_USER = "INSERT INTO auth_users (id, display_name, email, avatar_url, role, \
        created_at, last_login_at, username, username_display) \
        VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)";
    /// Set the display name and avatar address; a NULL keeps the stored one.
    pub(super) UPDATE_PROFILE = "UPDATE auth_users SET display_name = COALESCE(?1, display_name), \
        avatar_url = COALESCE(?2, avatar_url) WHERE id = ?3";
    /// Rename an account.
    pub(super) UPDATE_USERNAME = "UPDATE auth_users SET username = ?1, username_display = ?2 \
        WHERE id = ?3";
    /// Set or clear an account's email.
    pub(super) UPDATE_EMAIL = "UPDATE auth_users SET email = ?1 WHERE id = ?2";
    /// Set an account's role.
    pub(super) UPDATE_ROLE = "UPDATE auth_users SET role = ?1 WHERE id = ?2";
    /// Stamp the last sign-in.
    pub(super) TOUCH_LOGIN = "UPDATE auth_users SET last_login_at = ?1 WHERE id = ?2";
    /// Delete an account; its own rows cascade.
    pub(super) DELETE_USER = "DELETE FROM auth_users WHERE id = ?1";
    /// Whether an artist reconciliation dismissal names the account.
    pub(super) HELD_BY_RECONCILIATION_DISMISSALS = "SELECT 1 FROM \
        library_artist_reconciliation_dismissals WHERE dismissed_by_user_id = ?1 LIMIT 1";
    /// Whether a custom edition manifest names the account.
    pub(super) HELD_BY_EDITION_MANIFESTS = "SELECT 1 FROM library_custom_edition_manifests \
        WHERE sealed_by_user_id = ?1 LIMIT 1";
    /// Whether a library management exclusion names the account.
    pub(super) HELD_BY_MANAGEMENT_EXCLUSIONS = "SELECT 1 FROM library_management_exclusions \
        WHERE excluded_by_user_id = ?1 LIMIT 1";
    /// The account's discovery batch items, which have no foreign key.
    pub(super) DELETE_DISCOVERY_BATCH_ITEMS = "DELETE FROM discovery_batch_items WHERE batch_id IN \
        (SELECT id FROM discovery_batches WHERE user_id = ?1)";
    /// The account's discovery batches.
    pub(super) DELETE_DISCOVERY_BATCHES = "DELETE FROM discovery_batches WHERE user_id = ?1";

    /// Insert one provider row: a local credential or a sign-in binding.
    pub(super) INSERT_PROVIDER = "INSERT INTO auth_providers (id, user_id, provider, provider_uid, \
        provider_data, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6)";
    /// Bound provider names of one account.
    pub(super) USER_PROVIDERS = "SELECT provider FROM auth_providers WHERE user_id = ?1 \
        ORDER BY provider ASC";
    /// One account's local credential row.
    pub(super) LOCAL_CREDENTIAL = "SELECT id, provider_data FROM auth_providers \
        WHERE user_id = ?1 AND provider = 'local'";
    /// One account's local credential document.
    pub(super) LOCAL_DATA = "SELECT provider_data FROM auth_providers \
        WHERE user_id = ?1 AND provider = 'local'";
    /// Replace one account's local credential document.
    pub(super) SET_LOCAL_DATA = "UPDATE auth_providers SET provider_data = ?1 \
        WHERE user_id = ?2 AND provider = 'local'";
    /// Keep the local binding's uid on the username.
    pub(super) SET_LOCAL_UID = "UPDATE auth_providers SET provider_uid = ?1 \
        WHERE user_id = ?2 AND provider = 'local'";
    /// One sign-in binding by provider and provider-side id.
    pub(super) PROVIDER_BINDING = "SELECT id, user_id, provider, provider_uid FROM auth_providers \
        WHERE provider = ?1 AND provider_uid = ?2";
    /// The sign-in lookup: the account and its local credential document.
    pub(super) LOGIN_CREDENTIAL = "SELECT u.id, u.display_name, p.provider_data \
        FROM auth_users u \
        LEFT JOIN auth_providers p ON p.user_id = u.id AND p.provider = 'local' \
        WHERE u.username = ?1";
    /// Whether a recovery code may create the account's first password: it
    /// has a username and no local credential yet.
    pub(super) FIRST_PASSWORD_READY = "SELECT username IS NOT NULL AND NOT EXISTS (SELECT 1 FROM \
        auth_providers WHERE user_id = auth_users.id AND provider = 'local') \
        FROM auth_users WHERE id = ?1";

    /// Revoke every session of one account but the one named.
    pub(super) REVOKE_OTHER_SESSIONS = "UPDATE auth_tokens SET revoked = 1 \
        WHERE user_id = ?1 AND id != ?2";
    /// Revoke every session of one account.
    pub(super) REVOKE_USER_SESSIONS = "UPDATE auth_tokens SET revoked = 1 WHERE user_id = ?1";

    /// Columns read for every app-password row.
    pub(super) fragment APP_PASSWORD_COLUMNS = "id, user_id, name, secret_sha256, \
        secret_encrypted, created_at, last_used_at, last_client";
    /// Live app passwords of one account.
    pub(super) ACTIVE_APP_PASSWORD_COUNT = "SELECT COUNT(*) FROM connect_app_passwords \
        WHERE user_id = ?1 AND revoked = 0";
    /// Insert one app password.
    pub(super) INSERT_APP_PASSWORD = "INSERT INTO connect_app_passwords (id, user_id, name, \
        secret_sha256, secret_encrypted, created_at, last_used_at, last_client, revoked) \
        VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)";
    /// One app password by id, revoked or not.
    pub(super) APP_PASSWORD_BY_ID = "SELECT " + APP_PASSWORD_COLUMNS
        + " FROM connect_app_passwords WHERE id = ?1";
    /// The live app password with one secret hash.
    pub(super) ACTIVE_APP_PASSWORD_BY_SHA256 = "SELECT " + APP_PASSWORD_COLUMNS
        + " FROM connect_app_passwords WHERE secret_sha256 = ?1 AND revoked = 0";
    /// Live app passwords of one account: oldest first, id tiebreak.
    pub(super) ACTIVE_APP_PASSWORDS_OF_USER = "SELECT " + APP_PASSWORD_COLUMNS
        + " FROM connect_app_passwords WHERE user_id = ?1 AND revoked = 0 \
        ORDER BY created_at ASC, id ASC";
    /// Every live app password: owner, then age, then id.
    pub(super) ACTIVE_APP_PASSWORDS = "SELECT " + APP_PASSWORD_COLUMNS
        + " FROM connect_app_passwords WHERE revoked = 0 \
        ORDER BY user_id ASC, created_at ASC, id ASC";
    /// Revoke one live app password.
    pub(super) REVOKE_APP_PASSWORD = "UPDATE connect_app_passwords SET revoked = 1 \
        WHERE id = ?1 AND revoked = 0";
    /// Stamp an app password's last use; a NULL client keeps the stored one.
    pub(super) TOUCH_APP_PASSWORD = "UPDATE connect_app_passwords SET last_used_at = ?1, \
        last_client = COALESCE(?2, last_client) WHERE secret_sha256 = ?3";
    /// Revoke and empty every app password sealed under the data key.
    pub(super) FORGET_APP_PASSWORD_SECRETS = "UPDATE connect_app_passwords \
        SET revoked = 1, secret_encrypted = '' WHERE secret_encrypted LIKE 'v3:%'";
    /// Drop every sign-in token set sealed under the data key.
    pub(super) FORGET_SIGN_IN_TOKENS = "UPDATE auth_providers SET provider_data = NULL \
        WHERE provider_data LIKE 'v3:%'";

    /// Drop recovery codes that expired by a time.
    pub(super) DELETE_EXPIRED_RECOVERY_CODES = "DELETE FROM auth_password_recovery_codes \
        WHERE expires_at <= ?1";
    /// Store one account's code, replacing its last one.
    pub(super) STORE_RECOVERY_CODE = "INSERT OR REPLACE INTO auth_password_recovery_codes \
        (user_id, code_hash, created_at, expires_at) VALUES (?1, ?2, ?3, ?4)";
    /// One code by its hash.
    pub(super) RECOVERY_CODE_BY_HASH = "SELECT user_id, code_hash, created_at, expires_at \
        FROM auth_password_recovery_codes WHERE code_hash = ?1";
    /// Drop one account's code.
    pub(super) DELETE_USER_RECOVERY_CODE = "DELETE FROM auth_password_recovery_codes \
        WHERE user_id = ?1";
    /// Consume one account's code while it is live.
    pub(super) CONSUME_RECOVERY_CODE = "DELETE FROM auth_password_recovery_codes \
        WHERE user_id = ?1 AND code_hash = ?2 AND expires_at > ?3";

    /// One account's Last.fm link document.
    pub(super) LASTFM_LINK = "SELECT connection_data FROM user_connections \
        WHERE user_id = ?1 AND service = 'lastfm'";
    /// Insert or replace one account's Last.fm link.
    pub(super) UPSERT_LASTFM_LINK = "INSERT INTO user_connections (user_id, service, \
        connection_data, enabled, created_at, updated_at) VALUES (?1, 'lastfm', ?2, 1, ?3, ?3) \
        ON CONFLICT (user_id, service) DO UPDATE SET connection_data = ?2, updated_at = ?3";
    /// Drop one account's Last.fm link.
    pub(super) DELETE_LASTFM_LINK = "DELETE FROM user_connections \
        WHERE user_id = ?1 AND service = 'lastfm'";
}

/// [`USERS_BY_IDS`] for `len` ids.
pub(super) fn users_by_ids(len: ListLen) -> String {
    format!(
        "SELECT {USER_COLUMNS} FROM auth_users WHERE id IN ({})",
        vec!["?"; len.get()].join(", ")
    )
}
