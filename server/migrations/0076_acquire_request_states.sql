-- 0076_acquire_request_states.sql - request, watch and approval rows the
-- database checks.
--
-- request_history, its requester and dismissal rows, wanted_watches and
-- auto_download_approvals are rebuilt as STRICT tables:
--   * every state column carries a CHECK listing its Rust enum's values;
--   * request_history gains origin, the source of the ask (existing rows
--     were all asked by a user);
--   * every time column holds integer milliseconds. Old values were ISO
--     text (v2), integer seconds stored as text (v3), SQLite datetime
--     text (dismissals) or REAL seconds (watches, approvals). Text that
--     does not read as a time falls back as noted per column;
--   * user columns get foreign keys: the owner of a watch, a requester row
--     or a dismissal goes with its user, an owner or reviewer name on a
--     shared row is cleared. Requester and dismissal rows go with their
--     request. Rows pointing at a user or request that no longer exists
--     are cleared or dropped while copying, since the keys are enforced.
--   * the wanted watch's dormant state is gone: a dormant watch watches
--     again and is due now.
--
-- request_history is rebuilt and renamed before the new requester and
-- dismissal tables are created: they reference it by name, so if they
-- existed while the old request table was dropped, the drop would cascade
-- into them and empty them.
--
-- No down migration. Rollback is restoring a pre-upgrade backup.

-- Requests.
CREATE TABLE request_history_new (
    musicbrainz_id_lower TEXT PRIMARY KEY,
    musicbrainz_id TEXT NOT NULL,
    artist_name TEXT NOT NULL,
    album_title TEXT NOT NULL,
    artist_mbid TEXT,
    year INTEGER,
    cover_url TEXT,
    requested_at INTEGER NOT NULL,
    completed_at INTEGER,
    status TEXT NOT NULL CHECK(status IN ('pending','queued','downloading',
        'awaiting_approval','cancelling','cancelled','failed','imported',
        'incomplete','rejected')),
    monitor_artist INTEGER NOT NULL DEFAULT 0,
    auto_download_artist INTEGER NOT NULL DEFAULT 0,
    user_id TEXT REFERENCES auth_users(id) ON DELETE SET NULL,
    requested_by_name TEXT,
    reviewed_by_id TEXT REFERENCES auth_users(id) ON DELETE SET NULL,
    reviewed_by_name TEXT,
    reviewed_at INTEGER,
    download_task_id TEXT,
    release_mbid TEXT,
    request_kind TEXT NOT NULL DEFAULT 'album' CHECK(request_kind IN ('album','track')),
    origin TEXT NOT NULL DEFAULT 'user' CHECK(origin IN ('user','approval','retry',
        'wanted','upgrade','edition','follow')),
    track_title TEXT,
    duration_seconds INTEGER,
    track_release_group_mbid TEXT,
    dispatch_authorized INTEGER NOT NULL DEFAULT 0,
    generation INTEGER NOT NULL DEFAULT 1
) STRICT;

INSERT INTO request_history_new (musicbrainz_id_lower, musicbrainz_id, artist_name,
    album_title, artist_mbid, year, cover_url, requested_at, completed_at, status,
    monitor_artist, auto_download_artist, user_id, requested_by_name, reviewed_by_id,
    reviewed_by_name, reviewed_at, download_task_id, release_mbid, request_kind, origin,
    track_title, duration_seconds, track_release_group_mbid, dispatch_authorized, generation)
SELECT musicbrainz_id_lower, musicbrainz_id, artist_name, album_title, artist_mbid,
    CASE WHEN typeof(year) = 'integer' THEN year
         WHEN typeof(year) = 'real' THEN CAST(round(year) AS INTEGER)
         WHEN year GLOB '[0-9]*' AND year NOT GLOB '*[^0-9]*' THEN CAST(year AS INTEGER)
    END,
    cover_url,
    -- An unreadable ask time takes the finish time, then now.
    COALESCE(
        CASE WHEN typeof(requested_at) = 'integer' THEN requested_at * 1000
             WHEN typeof(requested_at) = 'real' THEN CAST(round(requested_at * 1000) AS INTEGER)
             WHEN requested_at GLOB '[0-9]*' AND requested_at NOT GLOB '*[^0-9.]*'
                 THEN CAST(round(CAST(requested_at AS REAL) * 1000) AS INTEGER)
             ELSE CAST(round((julianday(requested_at) - 2440587.5) * 86400000.0) AS INTEGER)
        END,
        CASE WHEN typeof(completed_at) = 'integer' THEN completed_at * 1000
             WHEN typeof(completed_at) = 'real' THEN CAST(round(completed_at * 1000) AS INTEGER)
             WHEN completed_at GLOB '[0-9]*' AND completed_at NOT GLOB '*[^0-9.]*'
                 THEN CAST(round(CAST(completed_at AS REAL) * 1000) AS INTEGER)
             ELSE CAST(round((julianday(completed_at) - 2440587.5) * 86400000.0) AS INTEGER)
        END,
        CAST(round((julianday('now') - 2440587.5) * 86400000.0) AS INTEGER)),
    CASE WHEN typeof(completed_at) = 'integer' THEN completed_at * 1000
         WHEN typeof(completed_at) = 'real' THEN CAST(round(completed_at * 1000) AS INTEGER)
         WHEN completed_at GLOB '[0-9]*' AND completed_at NOT GLOB '*[^0-9.]*'
             THEN CAST(round(CAST(completed_at AS REAL) * 1000) AS INTEGER)
         ELSE CAST(round((julianday(completed_at) - 2440587.5) * 86400000.0) AS INTEGER)
    END,
    status, monitor_artist, auto_download_artist,
    CASE WHEN EXISTS (SELECT 1 FROM auth_users u WHERE u.id = request_history.user_id)
        THEN user_id END,
    requested_by_name,
    CASE WHEN EXISTS (SELECT 1 FROM auth_users u WHERE u.id = request_history.reviewed_by_id)
        THEN reviewed_by_id END,
    reviewed_by_name,
    CASE WHEN typeof(reviewed_at) = 'integer' THEN reviewed_at * 1000
         WHEN typeof(reviewed_at) = 'real' THEN CAST(round(reviewed_at * 1000) AS INTEGER)
         WHEN reviewed_at GLOB '[0-9]*' AND reviewed_at NOT GLOB '*[^0-9.]*'
             THEN CAST(round(CAST(reviewed_at AS REAL) * 1000) AS INTEGER)
         ELSE CAST(round((julianday(reviewed_at) - 2440587.5) * 86400000.0) AS INTEGER)
    END,
    download_task_id, release_mbid, request_kind, 'user', track_title,
    CASE WHEN typeof(duration_seconds) = 'integer' THEN duration_seconds
         WHEN typeof(duration_seconds) = 'real' THEN CAST(round(duration_seconds) AS INTEGER)
         WHEN duration_seconds GLOB '[0-9]*' AND duration_seconds NOT GLOB '*[^0-9]*'
             THEN CAST(duration_seconds AS INTEGER)
    END,
    track_release_group_mbid, dispatch_authorized, generation
FROM request_history;

DROP TABLE request_history;
ALTER TABLE request_history_new RENAME TO request_history;

CREATE INDEX idx_request_history_retrying_keyset
    ON request_history(status, requested_at DESC, musicbrainz_id_lower DESC);
CREATE INDEX idx_request_history_user ON request_history(user_id);
CREATE INDEX idx_request_history_reviewed_by ON request_history(reviewed_by_id);
CREATE INDEX idx_request_history_download_task ON request_history(download_task_id);

-- Who asked: a requester row goes with its user and its request. An
-- unreadable ask time stays 0, which no quota window counts.
CREATE TABLE request_history_requesters_new (
    user_id TEXT NOT NULL REFERENCES auth_users(id) ON DELETE CASCADE,
    musicbrainz_id_lower TEXT NOT NULL
        REFERENCES request_history(musicbrainz_id_lower) ON DELETE CASCADE,
    requested_at INTEGER NOT NULL,
    requested_by_name TEXT,
    PRIMARY KEY (user_id, musicbrainz_id_lower)
) STRICT;

INSERT INTO request_history_requesters_new (user_id, musicbrainz_id_lower, requested_at,
    requested_by_name)
SELECT r.user_id, r.musicbrainz_id_lower,
    COALESCE(
        CASE WHEN typeof(r.requested_at) = 'integer' THEN r.requested_at * 1000
             WHEN typeof(r.requested_at) = 'real' THEN CAST(round(r.requested_at * 1000) AS INTEGER)
             WHEN r.requested_at GLOB '[0-9]*' AND r.requested_at NOT GLOB '*[^0-9.]*'
                 THEN CAST(round(CAST(r.requested_at AS REAL) * 1000) AS INTEGER)
             ELSE CAST(round((julianday(r.requested_at) - 2440587.5) * 86400000.0) AS INTEGER)
        END, 0),
    r.requested_by_name
FROM request_history_requesters r
WHERE EXISTS (SELECT 1 FROM auth_users u WHERE u.id = r.user_id)
  AND EXISTS (SELECT 1 FROM request_history h
              WHERE h.musicbrainz_id_lower = r.musicbrainz_id_lower);

DROP TABLE request_history_requesters;
ALTER TABLE request_history_requesters_new RENAME TO request_history_requesters;

CREATE INDEX idx_request_history_requesters_mbid
    ON request_history_requesters(musicbrainz_id_lower);
CREATE INDEX idx_request_history_requesters_user_time
    ON request_history_requesters(user_id, requested_at DESC, musicbrainz_id_lower DESC);

-- Per-user hides: a dismissal goes with its user and its request. The
-- server writes the time itself; an unreadable one becomes now.
CREATE TABLE request_history_dismissals_new (
    user_id TEXT NOT NULL REFERENCES auth_users(id) ON DELETE CASCADE,
    musicbrainz_id_lower TEXT NOT NULL
        REFERENCES request_history(musicbrainz_id_lower) ON DELETE CASCADE,
    dismissed_at INTEGER NOT NULL,
    PRIMARY KEY (user_id, musicbrainz_id_lower)
) STRICT;

INSERT INTO request_history_dismissals_new (user_id, musicbrainz_id_lower, dismissed_at)
SELECT d.user_id, d.musicbrainz_id_lower,
    COALESCE(
        CASE WHEN typeof(d.dismissed_at) = 'integer' THEN d.dismissed_at * 1000
             WHEN typeof(d.dismissed_at) = 'real' THEN CAST(round(d.dismissed_at * 1000) AS INTEGER)
             WHEN d.dismissed_at GLOB '[0-9]*' AND d.dismissed_at NOT GLOB '*[^0-9.]*'
                 THEN CAST(round(CAST(d.dismissed_at AS REAL) * 1000) AS INTEGER)
             ELSE CAST(round((julianday(d.dismissed_at) - 2440587.5) * 86400000.0) AS INTEGER)
        END,
        CAST(round((julianday('now') - 2440587.5) * 86400000.0) AS INTEGER))
FROM request_history_dismissals d
WHERE EXISTS (SELECT 1 FROM auth_users u WHERE u.id = d.user_id)
  AND EXISTS (SELECT 1 FROM request_history h
              WHERE h.musicbrainz_id_lower = d.musicbrainz_id_lower);

DROP TABLE request_history_dismissals;
ALTER TABLE request_history_dismissals_new RENAME TO request_history_dismissals;

CREATE INDEX idx_request_history_dismissals_mbid
    ON request_history_dismissals(musicbrainz_id_lower);

-- Wanted watches: a watch goes with its user. Unreadable times become now.
CREATE TABLE wanted_watches_new (
    release_group_mbid_lower TEXT PRIMARY KEY,
    release_group_mbid TEXT NOT NULL,
    user_id TEXT NOT NULL REFERENCES auth_users(id) ON DELETE CASCADE,
    artist_name TEXT NOT NULL,
    album_title TEXT NOT NULL,
    artist_mbid TEXT,
    year INTEGER,
    cover_url TEXT,
    kind TEXT NOT NULL CHECK(kind IN ('missing','partial')),
    state TEXT NOT NULL DEFAULT 'watching' CHECK(state IN ('watching','stopped','fulfilled')),
    created_at INTEGER NOT NULL,
    first_release_date TEXT,
    check_count INTEGER NOT NULL DEFAULT 0,
    quiet_streak INTEGER NOT NULL DEFAULT 0,
    last_checked_at INTEGER,
    next_check_at INTEGER NOT NULL,
    last_outcome TEXT,
    new_candidate_count INTEGER NOT NULL DEFAULT 0
) STRICT;

INSERT INTO wanted_watches_new (release_group_mbid_lower, release_group_mbid, user_id,
    artist_name, album_title, artist_mbid, year, cover_url, kind, state, created_at,
    first_release_date, check_count, quiet_streak, last_checked_at, next_check_at,
    last_outcome, new_candidate_count)
SELECT w.release_group_mbid_lower, w.release_group_mbid, w.user_id, w.artist_name,
    w.album_title, w.artist_mbid,
    CASE WHEN typeof(w.year) = 'integer' THEN w.year
         WHEN typeof(w.year) = 'real' THEN CAST(round(w.year) AS INTEGER)
         WHEN w.year GLOB '[0-9]*' AND w.year NOT GLOB '*[^0-9]*' THEN CAST(w.year AS INTEGER)
    END,
    w.cover_url, w.kind,
    CASE w.state WHEN 'dormant' THEN 'watching' ELSE w.state END,
    COALESCE(
        CASE WHEN typeof(w.created_at) = 'integer' THEN w.created_at * 1000
             WHEN typeof(w.created_at) = 'real' THEN CAST(round(w.created_at * 1000) AS INTEGER)
             WHEN w.created_at GLOB '[0-9]*' AND w.created_at NOT GLOB '*[^0-9.]*'
                 THEN CAST(round(CAST(w.created_at AS REAL) * 1000) AS INTEGER)
             ELSE CAST(round((julianday(w.created_at) - 2440587.5) * 86400000.0) AS INTEGER)
        END,
        CAST(round((julianday('now') - 2440587.5) * 86400000.0) AS INTEGER)),
    w.first_release_date, w.check_count, w.quiet_streak,
    CASE WHEN typeof(w.last_checked_at) = 'integer' THEN w.last_checked_at * 1000
         WHEN typeof(w.last_checked_at) = 'real' THEN CAST(round(w.last_checked_at * 1000) AS INTEGER)
         WHEN w.last_checked_at GLOB '[0-9]*' AND w.last_checked_at NOT GLOB '*[^0-9.]*'
             THEN CAST(round(CAST(w.last_checked_at AS REAL) * 1000) AS INTEGER)
         ELSE CAST(round((julianday(w.last_checked_at) - 2440587.5) * 86400000.0) AS INTEGER)
    END,
    CASE WHEN w.state = 'dormant'
        THEN CAST(round((julianday('now') - 2440587.5) * 86400000.0) AS INTEGER)
    ELSE COALESCE(
        CASE WHEN typeof(w.next_check_at) = 'integer' THEN w.next_check_at * 1000
             WHEN typeof(w.next_check_at) = 'real' THEN CAST(round(w.next_check_at * 1000) AS INTEGER)
             WHEN w.next_check_at GLOB '[0-9]*' AND w.next_check_at NOT GLOB '*[^0-9.]*'
                 THEN CAST(round(CAST(w.next_check_at AS REAL) * 1000) AS INTEGER)
             ELSE CAST(round((julianday(w.next_check_at) - 2440587.5) * 86400000.0) AS INTEGER)
        END,
        CAST(round((julianday('now') - 2440587.5) * 86400000.0) AS INTEGER))
    END,
    w.last_outcome, w.new_candidate_count
FROM wanted_watches w
WHERE EXISTS (SELECT 1 FROM auth_users u WHERE u.id = w.user_id);

DROP TABLE wanted_watches;
ALTER TABLE wanted_watches_new RENAME TO wanted_watches;

CREATE INDEX idx_wanted_due ON wanted_watches(state, next_check_at);
CREATE INDEX idx_wanted_watches_user ON wanted_watches(user_id);

-- Follow auto-download approvals. Unreadable ask times become now.
CREATE TABLE auto_download_approvals_new (
    user_id TEXT NOT NULL REFERENCES auth_users(id) ON DELETE CASCADE,
    artist_mbid TEXT NOT NULL,
    artist_mbid_lower TEXT NOT NULL,
    artist_name TEXT NOT NULL,
    state TEXT NOT NULL DEFAULT 'pending'
        CHECK(state IN ('pending','approved','rejected','revoked')),
    requested_at INTEGER NOT NULL,
    reviewed_by_id TEXT REFERENCES auth_users(id) ON DELETE SET NULL,
    reviewed_by_name TEXT,
    reviewed_at INTEGER,
    batch_id TEXT,
    source TEXT,
    PRIMARY KEY (user_id, artist_mbid_lower)
) STRICT;

INSERT INTO auto_download_approvals_new (user_id, artist_mbid, artist_mbid_lower,
    artist_name, state, requested_at, reviewed_by_id, reviewed_by_name, reviewed_at,
    batch_id, source)
SELECT a.user_id, a.artist_mbid, a.artist_mbid_lower, a.artist_name, a.state,
    COALESCE(
        CASE WHEN typeof(a.requested_at) = 'integer' THEN a.requested_at * 1000
             WHEN typeof(a.requested_at) = 'real' THEN CAST(round(a.requested_at * 1000) AS INTEGER)
             WHEN a.requested_at GLOB '[0-9]*' AND a.requested_at NOT GLOB '*[^0-9.]*'
                 THEN CAST(round(CAST(a.requested_at AS REAL) * 1000) AS INTEGER)
             ELSE CAST(round((julianday(a.requested_at) - 2440587.5) * 86400000.0) AS INTEGER)
        END,
        CAST(round((julianday('now') - 2440587.5) * 86400000.0) AS INTEGER)),
    CASE WHEN EXISTS (SELECT 1 FROM auth_users u WHERE u.id = a.reviewed_by_id)
        THEN a.reviewed_by_id END,
    a.reviewed_by_name,
    CASE WHEN typeof(a.reviewed_at) = 'integer' THEN a.reviewed_at * 1000
         WHEN typeof(a.reviewed_at) = 'real' THEN CAST(round(a.reviewed_at * 1000) AS INTEGER)
         WHEN a.reviewed_at GLOB '[0-9]*' AND a.reviewed_at NOT GLOB '*[^0-9.]*'
             THEN CAST(round(CAST(a.reviewed_at AS REAL) * 1000) AS INTEGER)
         ELSE CAST(round((julianday(a.reviewed_at) - 2440587.5) * 86400000.0) AS INTEGER)
    END,
    a.batch_id, a.source
FROM auto_download_approvals a;

DROP TABLE auto_download_approvals;
ALTER TABLE auto_download_approvals_new RENAME TO auto_download_approvals;

CREATE INDEX idx_ada_pending ON auto_download_approvals(state) WHERE state = 'pending';
CREATE INDEX idx_ada_batch ON auto_download_approvals(batch_id) WHERE batch_id IS NOT NULL;
CREATE INDEX idx_ada_reviewed_by ON auto_download_approvals(reviewed_by_id);

-- Migration high-water mark.
PRAGMA user_version = 76;
