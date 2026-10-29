-- no-transaction
PRAGMA foreign_keys=OFF;
BEGIN IMMEDIATE;

-- 0101_auth_schema_conventions.sql - the auth tables follow the schema
-- conventions.
--
-- The six auth tables are rebuilt as STRICT tables, rowids and indexes
-- carried, every row reading as it did before:
--
--   * every point in time is integer milliseconds. A time converts only
--     when it reads as the ISO-8601 text the auth code parsed before
--     (YYYY-MM-DD, T or space, HH:MM:SS, up to nine fraction digits, then
--     nothing, Z, +HH:MM, +HHMM or +HH, on a real calendar day). Anything
--     else ('now', a bare date, garbage) read as expired or unknown, and
--     still does: 0 in an expiry, creation, issue or last-seen time, NULL
--     in a last-login or last-use time;
--   * auth_users.role is checked against the roles; a stored role outside
--     them read as user, and becomes user;
--   * the local credential moves out of the provider_data JSON into
--     password_hash and hash_scheme. A document with no scheme is bcrypt,
--     as it read before; one that read as no password (not JSON, no text
--     hash, a scheme the code does not know) keeps its row with both
--     columns NULL. Local rows end with provider_data NULL, and the other
--     providers keep it byte for byte;
--   * a companion session's label moves out of user_agent, where it sat
--     behind the "DroppedNeedle companion · " prefix, into label, and its
--     user_agent becomes NULL. Standard sessions keep user_agent as sent,
--     prefix or not;
--   * session_kind is checked against the kinds. A session of any other
--     kind authenticated nothing, and is kept as a revoked standard one;
--   * revoked is a 0 or 1 flag: anything but 0 is 1;
--   * connect_app_passwords becomes auth_app_passwords, copied, never
--     renamed. idx_cap_user becomes idx_auth_app_passwords_user, and
--     idx_auth_tokens_hash goes: the UNIQUE on token_hash is its index.
--
-- Rows with a NULL key cannot be named by any other row or any read, and
-- are not copied.
--
-- auth_users is a parent of tables in every area, so the file runs outside
-- sqlx's transaction with foreign keys off (the rebuild recipe in
-- kernel/src/schema/boot.rs): dropping a parent with keys enforced would
-- delete or orphan its children. It refuses, changing nothing, when the
-- copies leave any child pointing at a missing parent that did not before.
--
-- No down migration. Rollback is restoring a pre-upgrade backup.

-- Each guard refuses through a named CHECK, so a refusal says which one.
CREATE TEMP TABLE auth_rebuild_guard (
    foreign_keys_off INTEGER CONSTRAINT guard_foreign_keys_off CHECK (foreign_keys_off = 1),
    stamp_below_101 INTEGER CONSTRAINT guard_stamp_below_101 CHECK (stamp_below_101 = 1),
    no_new_orphan INTEGER CONSTRAINT guard_no_new_orphan CHECK (no_new_orphan = 1)
) STRICT;
INSERT INTO auth_rebuild_guard (foreign_keys_off)
    SELECT (SELECT foreign_keys FROM pragma_foreign_keys) = 0;
INSERT INTO auth_rebuild_guard (stamp_below_101)
    SELECT (SELECT user_version FROM pragma_user_version) < 101;

-- connect_app_passwords is copied into auth_app_passwords, so its old
-- orphans are saved under the name they report under afterwards.
CREATE TEMP TABLE auth_rebuild_orphans AS
    SELECT CASE "table" WHEN 'connect_app_passwords' THEN 'auth_app_passwords'
            ELSE "table" END AS child,
        rowid AS row, parent, fkid
    FROM pragma_foreign_key_check;

-- Every stored time text, with the instant it reads as, or NULL.
CREATE TEMP TABLE auth_times (raw TEXT NOT NULL PRIMARY KEY, ms INTEGER) STRICT;
INSERT INTO auth_times (raw, ms)
SELECT raw,
    CASE WHEN digits <= 9 AND date(day) IS day
            AND CAST(substr(clock, 1, 2) AS INTEGER) <= 23
            AND CAST(substr(clock, 4, 2) AS INTEGER) <= 59
            AND CAST(substr(clock, 7, 2) AS INTEGER) <= 59
            AND zone_hours <= 23 AND zone_minutes <= 59
        THEN (unixepoch(day || ' ' || clock) - zone_sign * (zone_hours * 3600 + zone_minutes * 60))
            * 1000
            + CASE WHEN digits > 0
                THEN CAST(substr(substr(rest, 2, digits) || '00', 1, 3) AS INTEGER)
                ELSE 0 END
    END
FROM (
    SELECT raw, day, clock, rest, digits,
        CASE WHEN substr(zone, 1, 1) = '-' THEN -1 ELSE 1 END AS zone_sign,
        CASE
            WHEN zone IN ('', 'Z', 'z') THEN 0
            WHEN zone GLOB '[+-][0-9][0-9]:[0-9][0-9]'
                OR zone GLOB '[+-][0-9][0-9][0-9][0-9]'
                OR zone GLOB '[+-][0-9][0-9]'
                THEN CAST(substr(zone, 2, 2) AS INTEGER)
        END AS zone_hours,
        CASE
            WHEN zone IN ('', 'Z', 'z') THEN 0
            WHEN zone GLOB '[+-][0-9][0-9]:[0-9][0-9]' THEN CAST(substr(zone, 5, 2) AS INTEGER)
            WHEN zone GLOB '[+-][0-9][0-9][0-9][0-9]' THEN CAST(substr(zone, 4, 2) AS INTEGER)
            WHEN zone GLOB '[+-][0-9][0-9]' THEN 0
        END AS zone_minutes
    FROM (
        SELECT raw, day, clock, rest, digits,
            CASE WHEN substr(rest, 1, 1) = '.'
                THEN CASE WHEN digits > 0 THEN substr(rest, 2 + digits) ELSE NULL END
                ELSE rest END AS zone
        FROM (
            SELECT raw, substr(t, 1, 10) AS day, substr(t, 12, 8) AS clock,
                substr(t, 20) AS rest,
                CASE WHEN substr(t, 20, 1) = '.'
                    THEN length(t) - 20 - length(ltrim(substr(t, 21), '0123456789'))
                    ELSE 0 END AS digits
            FROM (
                SELECT raw, trim(raw, ' ' || char(9, 10, 11, 12, 13)) AS t
                FROM (
                    SELECT created_at AS raw FROM auth_users
                    UNION SELECT last_login_at FROM auth_users
                    UNION SELECT created_at FROM auth_providers
                    UNION SELECT issued_at FROM auth_tokens
                    UNION SELECT expires_at FROM auth_tokens
                    UNION SELECT last_seen_at FROM auth_tokens
                    UNION SELECT created_at FROM auth_password_recovery_codes
                    UNION SELECT expires_at FROM auth_password_recovery_codes
                    UNION SELECT created_at FROM auth_oidc_states
                    UNION SELECT expires_at FROM auth_oidc_states
                    UNION SELECT created_at FROM connect_app_passwords
                    UNION SELECT last_used_at FROM connect_app_passwords
                )
                WHERE typeof(raw) = 'text'
            )
            WHERE substr(t, 1, 19) GLOB
                '[0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9][Tt ][0-9][0-9]:[0-9][0-9]:[0-9][0-9]'
        )
    )
);

CREATE TABLE auth_users_new (
    id TEXT NOT NULL PRIMARY KEY,
    display_name TEXT NOT NULL,
    email TEXT UNIQUE,
    avatar_url TEXT,
    role TEXT NOT NULL DEFAULT 'user' CHECK(role IN ('user', 'trusted', 'admin')),
    created_at INTEGER NOT NULL,
    last_login_at INTEGER,
    username TEXT,
    username_display TEXT
) STRICT;
INSERT INTO auth_users_new (rowid, id, display_name, email, avatar_url, role, created_at,
    last_login_at, username, username_display)
SELECT rowid, CAST(id AS TEXT), CAST(display_name AS TEXT), CAST(email AS TEXT),
    CAST(avatar_url AS TEXT),
    CASE WHEN role IN ('user', 'trusted', 'admin') THEN role ELSE 'user' END,
    COALESCE((SELECT ms FROM auth_times WHERE raw = auth_users.created_at), 0),
    (SELECT ms FROM auth_times WHERE raw = auth_users.last_login_at),
    CAST(username AS TEXT), CAST(username_display AS TEXT)
FROM auth_users WHERE id IS NOT NULL;
DROP TABLE auth_users;
ALTER TABLE auth_users_new RENAME TO auth_users;
CREATE UNIQUE INDEX idx_auth_users_username
    ON auth_users(username) WHERE username IS NOT NULL;

CREATE TABLE auth_providers_new (
    id TEXT NOT NULL PRIMARY KEY,
    user_id TEXT NOT NULL REFERENCES auth_users(id) ON DELETE CASCADE,
    provider TEXT NOT NULL,
    provider_uid TEXT NOT NULL,
    provider_data TEXT,
    password_hash TEXT,
    hash_scheme TEXT CHECK(hash_scheme IN ('bcrypt', 'argon2id')),
    created_at INTEGER NOT NULL,
    UNIQUE (provider, provider_uid),
    CHECK ((password_hash IS NULL) = (hash_scheme IS NULL)),
    CHECK (provider = 'local' OR password_hash IS NULL),
    CHECK (provider <> 'local' OR provider_data IS NULL)
) STRICT;
INSERT INTO auth_providers_new (rowid, id, user_id, provider, provider_uid, provider_data,
    password_hash, hash_scheme, created_at)
SELECT row_id, id, user_id, provider, provider_uid,
    CASE WHEN provider = 'local' THEN NULL ELSE data END,
    CASE WHEN tag IN ('bcrypt', 'argon2id') THEN hash END,
    CASE WHEN hash IS NOT NULL AND tag IN ('bcrypt', 'argon2id') THEN tag END,
    created
FROM (
    SELECT rowid AS row_id, CAST(id AS TEXT) AS id, CAST(user_id AS TEXT) AS user_id,
        CAST(provider AS TEXT) AS provider, CAST(provider_uid AS TEXT) AS provider_uid,
        CAST(provider_data AS TEXT) AS data,
        COALESCE((SELECT ms FROM auth_times WHERE raw = auth_providers.created_at), 0)
            AS created,
        CASE WHEN provider = 'local' AND typeof(provider_data) = 'text' THEN
            CASE WHEN json_valid(provider_data) THEN
                CASE WHEN json_type(provider_data, '$.password_hash') = 'text'
                    THEN json_extract(provider_data, '$.password_hash') END
            END
        END AS hash,
        CASE WHEN provider = 'local' AND typeof(provider_data) = 'text' THEN
            CASE WHEN json_valid(provider_data) THEN
                CASE WHEN json_type(provider_data, '$.scheme') = 'text'
                    THEN json_extract(provider_data, '$.scheme') ELSE 'bcrypt' END
            END
        END AS tag
    FROM auth_providers WHERE id IS NOT NULL
);
DROP TABLE auth_providers;
ALTER TABLE auth_providers_new RENAME TO auth_providers;
CREATE INDEX idx_auth_providers_user
    ON auth_providers(user_id);

CREATE TABLE auth_tokens_new (
    id TEXT NOT NULL PRIMARY KEY,
    user_id TEXT NOT NULL REFERENCES auth_users(id) ON DELETE CASCADE,
    token_hash TEXT NOT NULL UNIQUE,
    issued_at INTEGER NOT NULL,
    expires_at INTEGER NOT NULL,
    last_seen_at INTEGER NOT NULL,
    revoked INTEGER NOT NULL DEFAULT 0 CHECK (revoked IN (0, 1)),
    user_agent TEXT,
    session_kind TEXT NOT NULL DEFAULT 'standard'
        CHECK(session_kind IN ('standard', 'companion')),
    label TEXT,
    CHECK (session_kind = 'companion' OR label IS NULL)
) STRICT;
INSERT INTO auth_tokens_new (rowid, id, user_id, token_hash, issued_at, expires_at,
    last_seen_at, revoked, user_agent, session_kind, label)
SELECT row_id, id, user_id, token_hash, issued, expires, seen,
    CASE WHEN known AND revoked IS 0 THEN 0 ELSE 1 END,
    CASE WHEN labelled THEN NULL ELSE agent END,
    CASE WHEN known THEN kind ELSE 'standard' END,
    CASE WHEN labelled THEN substr(agent, length(prefix) + 1) END
FROM (
    SELECT auth_tokens.rowid AS row_id, CAST(id AS TEXT) AS id, CAST(user_id AS TEXT) AS user_id,
        CAST(token_hash AS TEXT) AS token_hash,
        COALESCE((SELECT ms FROM auth_times WHERE raw = auth_tokens.issued_at), 0) AS issued,
        COALESCE((SELECT ms FROM auth_times WHERE raw = auth_tokens.expires_at), 0) AS expires,
        COALESCE((SELECT ms FROM auth_times WHERE raw = auth_tokens.last_seen_at), 0) AS seen,
        revoked, CAST(user_agent AS TEXT) AS agent, session_kind AS kind,
        session_kind IN ('standard', 'companion') AS known,
        session_kind = 'companion'
            AND substr(CAST(user_agent AS TEXT), 1, length(prefix)) = prefix AS labelled,
        prefix
    FROM auth_tokens, (SELECT 'DroppedNeedle companion ' || char(183) || ' ' AS prefix)
    WHERE id IS NOT NULL
);
DROP TABLE auth_tokens;
ALTER TABLE auth_tokens_new RENAME TO auth_tokens;
CREATE INDEX idx_auth_tokens_user
    ON auth_tokens(user_id);
CREATE INDEX idx_auth_tokens_expires
    ON auth_tokens(expires_at);

CREATE TABLE auth_password_recovery_codes_new (
    user_id TEXT NOT NULL PRIMARY KEY REFERENCES auth_users(id) ON DELETE CASCADE,
    code_hash TEXT NOT NULL UNIQUE,
    created_at INTEGER NOT NULL,
    expires_at INTEGER NOT NULL
) STRICT;
INSERT INTO auth_password_recovery_codes_new (rowid, user_id, code_hash, created_at, expires_at)
SELECT rowid, CAST(user_id AS TEXT), CAST(code_hash AS TEXT),
    COALESCE((SELECT ms FROM auth_times
        WHERE raw = auth_password_recovery_codes.created_at), 0),
    COALESCE((SELECT ms FROM auth_times
        WHERE raw = auth_password_recovery_codes.expires_at), 0)
FROM auth_password_recovery_codes WHERE user_id IS NOT NULL;
DROP TABLE auth_password_recovery_codes;
ALTER TABLE auth_password_recovery_codes_new RENAME TO auth_password_recovery_codes;
CREATE INDEX idx_auth_password_recovery_expires
    ON auth_password_recovery_codes(expires_at);

CREATE TABLE auth_oidc_states_new (
    state TEXT NOT NULL PRIMARY KEY,
    created_at INTEGER NOT NULL,
    expires_at INTEGER NOT NULL,
    code_verifier TEXT
) STRICT;
INSERT INTO auth_oidc_states_new (rowid, state, created_at, expires_at, code_verifier)
SELECT rowid, CAST(state AS TEXT),
    COALESCE((SELECT ms FROM auth_times WHERE raw = auth_oidc_states.created_at), 0),
    COALESCE((SELECT ms FROM auth_times WHERE raw = auth_oidc_states.expires_at), 0),
    CAST(code_verifier AS TEXT)
FROM auth_oidc_states WHERE state IS NOT NULL;
DROP TABLE auth_oidc_states;
ALTER TABLE auth_oidc_states_new RENAME TO auth_oidc_states;

CREATE TABLE auth_app_passwords (
    id TEXT NOT NULL PRIMARY KEY,
    user_id TEXT NOT NULL REFERENCES auth_users(id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    secret_sha256 TEXT NOT NULL UNIQUE,
    secret_encrypted TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    last_used_at INTEGER,
    last_client TEXT,
    revoked INTEGER NOT NULL DEFAULT 0 CHECK (revoked IN (0, 1))
) STRICT;
INSERT INTO auth_app_passwords (rowid, id, user_id, name, secret_sha256, secret_encrypted,
    created_at, last_used_at, last_client, revoked)
SELECT rowid, CAST(id AS TEXT), CAST(user_id AS TEXT), CAST(name AS TEXT),
    CAST(secret_sha256 AS TEXT), CAST(secret_encrypted AS TEXT),
    COALESCE((SELECT ms FROM auth_times WHERE raw = connect_app_passwords.created_at), 0),
    (SELECT ms FROM auth_times WHERE raw = connect_app_passwords.last_used_at),
    CAST(last_client AS TEXT),
    CASE WHEN revoked IS 0 THEN 0 ELSE 1 END
FROM connect_app_passwords WHERE id IS NOT NULL;
DROP TABLE connect_app_passwords;
CREATE INDEX idx_auth_app_passwords_user
    ON auth_app_passwords(user_id);

INSERT INTO auth_rebuild_guard (no_new_orphan) SELECT NOT EXISTS (
    SELECT 1 FROM (
        SELECT "table" AS child, rowid AS row, parent, fkid, COUNT(*) AS n
        FROM pragma_foreign_key_check GROUP BY 1, 2, 3, 4) AS now
    WHERE now.n > (SELECT COUNT(*) FROM auth_rebuild_orphans AS before
        WHERE before.child = now.child AND before.row IS now.row
            AND before.parent = now.parent AND before.fkid = now.fkid));

DROP TABLE auth_times;
DROP TABLE auth_rebuild_guard;
DROP TABLE auth_rebuild_orphans;

-- Migration high-water mark.
PRAGMA user_version = 101;
COMMIT;
PRAGMA foreign_keys=ON;
