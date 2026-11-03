-- no-transaction
PRAGMA foreign_keys=OFF;
BEGIN IMMEDIATE;

-- 0131_auth_user_connections_strict.sql - the per-user service links follow
-- the schema conventions.
--
-- user_connections is rebuilt as a STRICT table, rowids carried (the rebuild
-- recipe in kernel/src/schema/boot.rs, so a link whose account is gone stays
-- as it was rather than refusing the copy). What each row becomes:
--
--   * created_at and updated_at are integer milliseconds, read by the rule
--     migration 0101 used for the other auth times and the v2 importer uses
--     (YYYY-MM-DD, T or space, HH:MM:SS, up to nine fraction digits, then
--     nothing, Z, +HH:MM, +HHMM or +HH, on a real calendar day). Anything
--     else becomes 0. Nothing reads these times but a repeat import, which
--     compares them by the same rule;
--   * enabled is a checked flag: anything but 0 is 1, as it read before.
--
-- Rows with a NULL key cannot be named by any read and are not copied.
--
-- No down migration. Rollback is restoring a pre-upgrade backup.

-- Each guard refuses through a named CHECK, so a refusal says which one.
CREATE TEMP TABLE connections_131_guard (
    foreign_keys_off INTEGER CONSTRAINT guard_foreign_keys_off CHECK (foreign_keys_off = 1),
    stamp_below_131 INTEGER CONSTRAINT guard_stamp_below_131 CHECK (stamp_below_131 = 1),
    no_new_orphan INTEGER CONSTRAINT guard_no_new_orphan CHECK (no_new_orphan = 1)
) STRICT;
INSERT INTO connections_131_guard (foreign_keys_off)
    SELECT (SELECT foreign_keys FROM pragma_foreign_keys) = 0;
INSERT INTO connections_131_guard (stamp_below_131)
    SELECT (SELECT user_version FROM pragma_user_version) < 131;

CREATE TEMP TABLE connections_131_orphans AS
    SELECT "table" AS child, rowid AS row, parent FROM pragma_foreign_key_check;
CREATE INDEX temp.connections_131_orphans_at ON connections_131_orphans(child, row, parent);

-- Every stored time text, with the instant it reads as, or NULL.
CREATE TEMP TABLE connections_131_times (raw TEXT NOT NULL PRIMARY KEY, ms INTEGER) STRICT;
INSERT INTO connections_131_times (raw, ms)
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
                    SELECT created_at AS raw FROM user_connections
                    UNION SELECT updated_at FROM user_connections
                )
                WHERE typeof(raw) = 'text'
            )
            WHERE substr(t, 1, 19) GLOB
                '[0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9][Tt ][0-9][0-9]:[0-9][0-9]:[0-9][0-9]'
        )
    )
);

CREATE TABLE user_connections_new (
    user_id TEXT NOT NULL REFERENCES auth_users(id) ON DELETE CASCADE,
    service TEXT NOT NULL,
    connection_data TEXT NOT NULL,
    enabled INTEGER NOT NULL DEFAULT 1 CHECK (enabled IN (0, 1)),
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    PRIMARY KEY (user_id, service)
) STRICT;
INSERT INTO user_connections_new (rowid, user_id, service, connection_data, enabled,
    created_at, updated_at)
SELECT c.rowid, CAST(c.user_id AS TEXT), CAST(c.service AS TEXT),
    CAST(c.connection_data AS TEXT), CASE WHEN c.enabled != 0 THEN 1 ELSE 0 END,
    COALESCE((SELECT ms FROM connections_131_times WHERE raw = c.created_at), 0),
    COALESCE((SELECT ms FROM connections_131_times WHERE raw = c.updated_at), 0)
FROM user_connections AS c
WHERE c.user_id IS NOT NULL AND c.service IS NOT NULL;
DROP TABLE user_connections;
ALTER TABLE user_connections_new RENAME TO user_connections;

INSERT INTO connections_131_guard (no_new_orphan) SELECT NOT EXISTS (
    SELECT 1 FROM (
        SELECT "table" AS child, rowid AS row, parent, COUNT(*) AS n
        FROM pragma_foreign_key_check GROUP BY 1, 2, 3) AS now
    WHERE now.n > (SELECT COUNT(*) FROM connections_131_orphans AS before
        WHERE before.child = now.child AND before.row IS now.row
            AND before.parent = now.parent));

DROP TABLE connections_131_guard;
DROP TABLE connections_131_orphans;
DROP TABLE connections_131_times;

-- Migration high-water mark.
PRAGMA user_version = 131;
COMMIT;
PRAGMA foreign_keys=ON;
