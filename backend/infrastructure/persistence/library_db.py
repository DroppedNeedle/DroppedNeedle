"""Domain 1: Library state persistence (artists, albums, metadata).

Also owns the native-engine tables: ``library_files`` (the scanner's source of
truth), ``manual_review_queue``, and ``library_album_meta``. Library reads
aggregate from ``library_files`` on read - there is no materialised album table
or nightly reconcile.
"""

import json
import logging
import sqlite3
import time
from pathlib import Path
from typing import Any
from uuid import uuid4

from infrastructure.persistence._database import (
    PersistenceBase,
    _decode_json,
    _decode_rows,
    _encode_json,
    _normalize,
)
from infrastructure.serialization import to_jsonable

logger = logging.getLogger(__name__)


def _escape_like(term: str) -> str:
    """Escape SQL LIKE metacharacters so they match literally."""
    return term.replace("\\", "\\\\").replace("%", "\\%").replace("_", "\\_")


def _safe_alter(conn: sqlite3.Connection, sql: str) -> bool:
    """Run an ``ALTER TABLE ... ADD COLUMN`` that may already have been applied.

    Returns True if the column was added, False if it already existed."""
    try:
        conn.execute(sql)
        return True
    except sqlite3.OperationalError as exc:
        if "duplicate column" not in str(exc).lower():
            raise
        return False


# On UPDATE, id / imported_at / download_task_id are preserved, never overwritten.
_LIBRARY_FILE_VALUE_COLUMNS = (
    "release_group_mbid",
    "release_mbid",
    "recording_mbid",
    "disc_number",
    "track_number",
    "track_title",
    "artist_name",
    "artist_mbid",
    "album_artist_name",
    "album_artist_mbid",
    "album_title",
    "year",
    "file_path",
    "source_path",
    "file_size_bytes",
    "file_mtime",
    "duration_seconds",
    "file_format",
    "bit_rate",
    "sample_rate",
    "bit_depth",
    "source",
    "confidence",
    "is_compilation",
    "tagged_at",
    "genre",
    "channels",
)

# SQL mirror of quality_tiers.tier_for (lossless extension set + kbps bands), ranked
# like quality_tiers._RANK (low=0 .. lossless=4). test_cutoff_unmet asserts this CASE
# agrees with tier_for for every (format, bitrate) band - change BOTH together.
_TIER_RANK_CASE = """
    CASE
        WHEN LOWER(COALESCE(file_format, '')) IN ('flac', 'alac', 'wav', 'ape', 'wv') THEN 4
        WHEN COALESCE(bit_rate, 0) >= 320 THEN 3
        WHEN COALESCE(bit_rate, 0) >= 256 THEN 2
        WHEN COALESCE(bit_rate, 0) >= 192 THEN 1
        ELSE 0
    END
"""

# Stored, pre-folded mirrors of the searchable text columns: {folded_column: source_column}.
# Search LIKEs run against these so the per-row fold() UDF is not invoked on every
# scanned row (it stays only on the query pattern). Kept in sync on every write via
# SQL fold(), the same function used at search time, so the two never diverge.
_LIBRARY_FILE_FOLDED_COLUMNS = {
    "album_artist_name_folded": "album_artist_name",
    "album_title_folded": "album_title",
    "track_title_folded": "track_title",
    "artist_name_folded": "artist_name",
}

_ALBUM_AGG_SORTS = {
    "recent": "last_imported_at DESC",
    "title": "album_title COLLATE NOCASE ASC",
    "artist": "album_artist_name COLLATE NOCASE ASC",
    "random": "RANDOM()",
}

# each ends with lf.id so the order is total and pagination stays stable
# (ties on imported_at/title would otherwise let rows shift between pages).
_TRACK_LIST_SORTS = {
    "recent": "lf.imported_at DESC, lf.id",
    "title": "lf.track_title COLLATE NOCASE ASC, lf.album_title COLLATE NOCASE, "
    "lf.disc_number, lf.track_number, lf.id",
    "artist": "lf.artist_name COLLATE NOCASE ASC, lf.album_title COLLATE NOCASE, "
    "lf.disc_number, lf.track_number, lf.id",
    "album": "lf.album_artist_name COLLATE NOCASE ASC, lf.album_title COLLATE NOCASE, "
    "lf.disc_number, lf.track_number, lf.id",
}

_ARTIST_AGG_SORT_COLUMNS = {
    "name": "artist_name COLLATE NOCASE",
    "album_count": "album_count",
    "date_added": "date_added",
}

# belong to other stores but must be reset atomically with library data
_CROSS_DOMAIN_CLEAR_TABLES = (
    "artist_genres",
    "artist_genre_lookup",
)

_FULL_CLEAR_EXTRA_TABLES = (
    "sync_state",
    "jellyfin_mbid_index",
    "navidrome_album_mbid_index",
    "navidrome_artist_mbid_index",
)


def _safe_delete(conn: sqlite3.Connection, table: str) -> None:
    """DELETE FROM a table that may not exist yet (cross-domain dependency)."""
    try:
        conn.execute(f'DELETE FROM "{table}"')
    except sqlite3.OperationalError as exc:
        if "no such table" not in str(exc):
            logger.warning("Unexpected error clearing cross-domain table %s: %s", table, exc)


class LibraryDB(PersistenceBase):
    """Owns tables: ``cache_meta``, ``library_artists``, ``library_albums``."""

    def _ensure_tables(self) -> None:
        conn = self._connect()
        try:
            conn.execute(
                """
                CREATE TABLE IF NOT EXISTS cache_meta (
                    key TEXT PRIMARY KEY,
                    value TEXT NOT NULL,
                    updated_at REAL NOT NULL
                )
                """
            )
            conn.execute(
                """
                CREATE TABLE IF NOT EXISTS library_artists (
                    mbid_lower TEXT PRIMARY KEY,
                    mbid TEXT NOT NULL,
                    name TEXT NOT NULL,
                    album_count INTEGER DEFAULT 0,
                    date_added INTEGER,
                    raw_json TEXT NOT NULL
                )
                """
            )
            conn.execute(
                "CREATE INDEX IF NOT EXISTS idx_library_artists_date_added ON library_artists(date_added DESC)"
            )
            conn.execute(
                """
                CREATE TABLE IF NOT EXISTS library_albums (
                    mbid_lower TEXT PRIMARY KEY,
                    mbid TEXT NOT NULL,
                    artist_mbid TEXT,
                    artist_mbid_lower TEXT,
                    artist_name TEXT,
                    title TEXT NOT NULL,
                    year INTEGER,
                    cover_url TEXT,
                    date_added INTEGER,
                    raw_json TEXT NOT NULL
                )
                """
            )
            conn.execute(
                "CREATE INDEX IF NOT EXISTS idx_library_albums_artist_mbid ON library_albums(artist_mbid_lower)"
            )
            conn.execute(
                "CREATE INDEX IF NOT EXISTS idx_library_albums_date_added ON library_albums(date_added DESC)"
            )
            conn.execute(
                "CREATE INDEX IF NOT EXISTS idx_library_albums_title ON library_albums(title COLLATE NOCASE)"
            )
            conn.execute(
                "CREATE INDEX IF NOT EXISTS idx_library_albums_artist_name ON library_albums(artist_name COLLATE NOCASE)"
            )
            conn.execute(
                "CREATE INDEX IF NOT EXISTS idx_library_albums_year ON library_albums(year)"
            )
            conn.execute(
                "CREATE INDEX IF NOT EXISTS idx_library_artists_name ON library_artists(name COLLATE NOCASE)"
            )
            self._ensure_native_tables(conn)
            self._alter_library_albums(conn)
            conn.commit()
        finally:
            conn.close()

    def _ensure_native_tables(self, conn: sqlite3.Connection) -> None:
        """DDL copied verbatim from plan.md §Database Schema."""
        conn.execute(
            """
            CREATE TABLE IF NOT EXISTS library_files (
                id TEXT PRIMARY KEY,
                download_task_id TEXT,
                release_group_mbid TEXT,
                release_mbid TEXT,
                recording_mbid TEXT,
                disc_number INTEGER NOT NULL DEFAULT 1,
                track_number INTEGER NOT NULL,
                track_title TEXT NOT NULL,
                artist_name TEXT,
                artist_mbid TEXT,
                album_artist_name TEXT,
                album_artist_mbid TEXT,
                album_title TEXT NOT NULL,
                year INTEGER,
                file_path TEXT NOT NULL,
                source_path TEXT,
                file_size_bytes INTEGER NOT NULL,
                file_mtime REAL NOT NULL,
                duration_seconds REAL,
                file_format TEXT NOT NULL,
                bit_rate INTEGER,
                sample_rate INTEGER,
                bit_depth INTEGER,
                source TEXT NOT NULL DEFAULT 'scan',
                confidence REAL NOT NULL DEFAULT 1.0,
                is_compilation INTEGER NOT NULL DEFAULT 0
                    CHECK(is_compilation IN (0, 1)),
                deleted_at REAL,
                tagged_at REAL,
                imported_at REAL NOT NULL,
                -- table-level: SQLite requires table constraints after all column defs
                CHECK(release_group_mbid IS NOT NULL OR source = 'manual_review')
            )
            """
        )
        for index_sql in (
            "CREATE INDEX IF NOT EXISTS idx_library_files_album ON library_files(release_group_mbid)",
            "CREATE INDEX IF NOT EXISTS idx_library_files_path ON library_files(file_path)",
            "CREATE INDEX IF NOT EXISTS idx_library_files_recording ON library_files(recording_mbid)",
            "CREATE INDEX IF NOT EXISTS idx_library_files_artist ON library_files(artist_mbid)",
            "CREATE INDEX IF NOT EXISTS idx_library_files_task ON library_files(download_task_id)",
            "CREATE INDEX IF NOT EXISTS idx_library_files_deleted ON library_files(deleted_at)",
            "CREATE INDEX IF NOT EXISTS idx_library_files_album_track "
            "ON library_files(release_group_mbid, disc_number, track_number)",
            "CREATE INDEX IF NOT EXISTS idx_library_files_path_mtime_size "
            "ON library_files(file_path, file_mtime, file_size_bytes)",
            "CREATE INDEX IF NOT EXISTS idx_library_files_source ON library_files(source)",
            "CREATE UNIQUE INDEX IF NOT EXISTS idx_library_files_active_path "
            "ON library_files(file_path) WHERE deleted_at IS NULL",
        ):
            conn.execute(index_sql)

        conn.execute(
            """
            CREATE TABLE IF NOT EXISTS manual_review_queue (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                file_path TEXT NOT NULL UNIQUE,
                extracted_title TEXT,
                extracted_artist TEXT,
                extracted_album TEXT,
                extracted_year INTEGER,
                track_number INTEGER,
                disc_number INTEGER,
                file_format TEXT,
                duration REAL,
                file_size INTEGER,
                fingerprint TEXT,
                fingerprint_score REAL,
                candidate_mbids_encoded TEXT,
                source TEXT NOT NULL DEFAULT 'text_match'
                    CHECK(source IN ('text_match','acoustid')),
                created_at REAL NOT NULL,
                resolved_at REAL,
                resolution TEXT CHECK(resolution IN ('accepted','rejected','manual_id'))
            )
            """
        )
        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_manual_review_created ON manual_review_queue(created_at DESC)"
        )
        _safe_alter(conn, "ALTER TABLE manual_review_queue ADD COLUMN track_number INTEGER")
        _safe_alter(conn, "ALTER TABLE manual_review_queue ADD COLUMN disc_number INTEGER")
        conn.execute(
            """
            CREATE TABLE IF NOT EXISTS library_album_meta (
                release_group_mbid TEXT PRIMARY KEY,
                cover_url TEXT,
                last_cover_refresh_at REAL
            )
            """
        )
        # Pre-folded search columns. Backfilled once on first add; no index, since
        # substring LIKE ('%term%') isn't sargable - the win is dropping the per-row
        # Python fold() UDF, not an index seek.
        added = [
            _safe_alter(conn, f"ALTER TABLE library_files ADD COLUMN {folded} TEXT")
            for folded in _LIBRARY_FILE_FOLDED_COLUMNS
        ]
        if any(added):
            set_clause = ", ".join(
                f"{folded} = fold({source})"
                for folded, source in _LIBRARY_FILE_FOLDED_COLUMNS.items()
            )
            conn.execute(f"UPDATE library_files SET {set_clause}")
        # Connect Apps (Q9/Q17): genre + channels populated during the normal scan
        # (no separate backfill). Existing NULL rows fill on the next re-scan.
        _safe_alter(conn, "ALTER TABLE library_files ADD COLUMN genre TEXT")
        _safe_alter(conn, "ALTER TABLE library_files ADD COLUMN channels INTEGER")
        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_lf_genre ON library_files(genre) "
            "WHERE deleted_at IS NULL"
        )
        # Connect Apps (Q20): lazy-MB related-artist cache (csv); empty by default.
        _safe_alter(
            conn, "ALTER TABLE library_artists ADD COLUMN related_artist_mbids TEXT"
        )

    def _alter_library_albums(self, conn: sqlite3.Connection) -> None:
        """Scan-derived columns on library_albums; legacy columns stay in place
        (harmless, reads aggregate from library_files)."""
        for column_sql in (
            "ALTER TABLE library_albums ADD COLUMN track_count INTEGER",
            "ALTER TABLE library_albums ADD COLUMN expected_track_count INTEGER",
            "ALTER TABLE library_albums ADD COLUMN total_size_bytes INTEGER",
            "ALTER TABLE library_albums ADD COLUMN quality_format TEXT",
            "ALTER TABLE library_albums ADD COLUMN is_compilation INTEGER NOT NULL DEFAULT 0",
            "ALTER TABLE library_albums ADD COLUMN source TEXT NOT NULL DEFAULT 'scan'",
        ):
            _safe_alter(conn, column_sql)

    async def save_library(self, artists: list[Any], albums: list[Any]) -> None:
        builtins_artists = [to_jsonable(artist) for artist in artists]
        builtins_albums = [to_jsonable(album) for album in albums]
        now = time.time()

        def operation(conn: sqlite3.Connection) -> None:
            conn.execute("DELETE FROM library_artists")
            conn.execute("DELETE FROM library_albums")
            for tbl in _CROSS_DOMAIN_CLEAR_TABLES:
                _safe_delete(conn, tbl)

            artist_rows = []
            for artist in builtins_artists:
                if not isinstance(artist, dict):
                    continue
                mbid = artist.get("mbid")
                if not isinstance(mbid, str) or not mbid:
                    continue
                artist_rows.append((
                    _normalize(mbid),
                    mbid,
                    str(artist.get("name") or "Unknown"),
                    int(artist.get("album_count") or 0),
                    artist.get("date_added"),
                    _encode_json(artist),
                ))
            if artist_rows:
                conn.executemany(
                    "INSERT INTO library_artists (mbid_lower, mbid, name, album_count, date_added, raw_json) VALUES (?, ?, ?, ?, ?, ?)",
                    artist_rows,
                )

            album_rows = []
            for album in builtins_albums:
                if not isinstance(album, dict):
                    continue
                mbid = album.get("mbid")
                if not isinstance(mbid, str) or not mbid:
                    continue
                artist_mbid = album.get("artist_mbid")
                album_rows.append((
                    _normalize(mbid),
                    mbid,
                    artist_mbid,
                    _normalize(artist_mbid if isinstance(artist_mbid, str) else None),
                    album.get("artist_name"),
                    str(album.get("title") or "Unknown Album"),
                    album.get("year"),
                    album.get("cover_url"),
                    album.get("date_added"),
                    _encode_json(album),
                ))
            if album_rows:
                conn.executemany(
                    """
                    INSERT INTO library_albums (
                        mbid_lower, mbid, artist_mbid, artist_mbid_lower, artist_name,
                        title, year, cover_url, date_added, raw_json
                    ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
                    """,
                    album_rows,
                )

            conn.execute(
                "INSERT INTO cache_meta (key, value, updated_at) VALUES (?, ?, ?) ON CONFLICT(key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at",
                ("last_library_sync", str(now), now),
            )

        await self._write(operation)

    async def upsert_album(self, album: dict[str, Any]) -> None:
        mbid = album.get("mbid")
        if not isinstance(mbid, str) or not mbid:
            return
        artist_mbid = album.get("artist_mbid")
        raw_json = _encode_json(album)

        def operation(conn: sqlite3.Connection) -> None:
            conn.execute(
                """
                INSERT INTO library_albums (
                    mbid_lower, mbid, artist_mbid, artist_mbid_lower, artist_name,
                    title, year, cover_url, date_added, raw_json
                ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
                ON CONFLICT(mbid_lower) DO UPDATE SET
                    artist_mbid = excluded.artist_mbid,
                    artist_mbid_lower = excluded.artist_mbid_lower,
                    artist_name = excluded.artist_name,
                    title = excluded.title,
                    year = excluded.year,
                    cover_url = excluded.cover_url,
                    date_added = excluded.date_added,
                    raw_json = excluded.raw_json
                """,
                (
                    _normalize(mbid),
                    mbid,
                    artist_mbid,
                    _normalize(artist_mbid if isinstance(artist_mbid, str) else None),
                    album.get("artist_name"),
                    str(album.get("title") or "Unknown Album"),
                    album.get("year"),
                    album.get("cover_url"),
                    album.get("date_added"),
                    raw_json,
                ),
            )

        await self._write(operation)

    async def get_artists(self, limit: int | None = None) -> list[dict[str, Any]]:
        def operation(conn: sqlite3.Connection) -> list[dict[str, Any]]:
            query = "SELECT raw_json FROM library_artists ORDER BY COALESCE(date_added, 0) DESC, name COLLATE NOCASE ASC"
            params: tuple[object, ...] = ()
            if limit is not None:
                query += " LIMIT ?"
                params = (limit,)
            rows = conn.execute(query, params).fetchall()
            return _decode_rows(rows)

        return await self._read(operation)

    async def get_albums(self) -> list[dict[str, Any]]:
        def operation(conn: sqlite3.Connection) -> list[dict[str, Any]]:
            rows = conn.execute(
                "SELECT raw_json FROM library_albums ORDER BY COALESCE(date_added, 0) DESC, title COLLATE NOCASE ASC"
            ).fetchall()
            return _decode_rows(rows)

        return await self._read(operation)

    _ALBUM_SORT_COLUMNS = {
        "date_added": "COALESCE(date_added, 0)",
        "title": "title COLLATE NOCASE",
        "artist": "artist_name COLLATE NOCASE",
        "year": "COALESCE(year, 0)",
    }

    _ARTIST_SORT_COLUMNS = {
        "name": "name COLLATE NOCASE",
        "album_count": "album_count",
        "date_added": "COALESCE(date_added, 0)",
    }

    async def get_albums_paginated(
        self,
        limit: int = 50,
        offset: int = 0,
        sort_by: str = "date_added",
        sort_order: str = "desc",
        search: str | None = None,
    ) -> tuple[list[dict[str, Any]], int]:
        sort_col = self._ALBUM_SORT_COLUMNS.get(sort_by, "COALESCE(date_added, 0)")
        direction = "ASC" if sort_order.lower() == "asc" else "DESC"

        def operation(conn: sqlite3.Connection) -> tuple[list[dict[str, Any]], int]:
            where = ""
            params: list[object] = []
            if search:
                term = f"%{_escape_like(search)}%"
                where = "WHERE (artist_name LIKE ? ESCAPE '\\' COLLATE NOCASE OR title LIKE ? ESCAPE '\\' COLLATE NOCASE)"
                params = [term, term]

            count_row = conn.execute(
                f"SELECT COUNT(*) AS cnt FROM library_albums {where}", params
            ).fetchone()
            total = int(count_row["cnt"]) if count_row else 0

            rows = conn.execute(
                f"SELECT raw_json FROM library_albums {where} ORDER BY {sort_col} {direction}, title COLLATE NOCASE ASC, mbid_lower ASC LIMIT ? OFFSET ?",
                [*params, max(limit, 1), max(offset, 0)],
            ).fetchall()
            return _decode_rows(rows), total

        return await self._read(operation)

    async def get_artists_paginated(
        self,
        limit: int = 50,
        offset: int = 0,
        sort_by: str = "name",
        sort_order: str = "asc",
        search: str | None = None,
    ) -> tuple[list[dict[str, Any]], int]:
        sort_col = self._ARTIST_SORT_COLUMNS.get(sort_by, "name COLLATE NOCASE")
        direction = "ASC" if sort_order.lower() == "asc" else "DESC"

        def operation(conn: sqlite3.Connection) -> tuple[list[dict[str, Any]], int]:
            where = ""
            params: list[object] = []
            if search:
                term = f"%{_escape_like(search)}%"
                where = "WHERE name LIKE ? ESCAPE '\\' COLLATE NOCASE"
                params = [term]

            count_row = conn.execute(
                f"SELECT COUNT(*) AS cnt FROM library_artists {where}", params
            ).fetchone()
            total = int(count_row["cnt"]) if count_row else 0

            rows = conn.execute(
                f"SELECT raw_json FROM library_artists {where} ORDER BY {sort_col} {direction}, name COLLATE NOCASE ASC, mbid_lower ASC LIMIT ? OFFSET ?",
                [*params, max(limit, 1), max(offset, 0)],
            ).fetchall()
            return _decode_rows(rows), total

        return await self._read(operation)

    async def get_recently_added(self, limit: int = 20) -> list[dict[str, Any]]:
        def operation(conn: sqlite3.Connection) -> list[dict[str, Any]]:
            rows = conn.execute(
                "SELECT raw_json FROM library_albums ORDER BY COALESCE(date_added, 0) DESC, title COLLATE NOCASE ASC LIMIT ?",
                (max(limit, 1),),
            ).fetchall()
            return _decode_rows(rows)

        return await self._read(operation)

    async def get_album_by_mbid(self, musicbrainz_id: str) -> dict[str, Any] | None:
        normalized_mbid = _normalize(musicbrainz_id)

        def operation(conn: sqlite3.Connection) -> dict[str, Any] | None:
            row = conn.execute(
                "SELECT raw_json FROM library_albums WHERE mbid_lower = ?",
                (normalized_mbid,),
            ).fetchone()
            if row is None:
                return None
            try:
                payload = _decode_json(row["raw_json"])
            except (json.JSONDecodeError, TypeError):
                return None
            return payload if isinstance(payload, dict) else None

        return await self._read(operation)

    async def get_all_album_mbids(self) -> set[str]:
        # Only return MBIDs that have active (non-deleted) files. The
        # materialised library_albums table gains rows when downloads are
        # queued (upsert_album) but isn't cleaned if the download fails or
        # files are removed without a successful remove_album - so an
        # unfiltered SELECT returns stale "ghost" MBIDs that make albums
        # show as in-library with no files to play or remove.
        def operation(conn: sqlite3.Connection) -> set[str]:
            rows = conn.execute(
                "SELECT la.mbid FROM library_albums la "
                "WHERE EXISTS ("
                "  SELECT 1 FROM library_files lf "
                "  WHERE lf.release_group_mbid = la.mbid_lower "
                "  AND lf.deleted_at IS NULL)"
            ).fetchall()
            return {str(row["mbid"]) for row in rows if row["mbid"]}

        return await self._read(operation)

    async def delete_album_by_mbid(self, musicbrainz_id: str) -> None:
        """Hard-delete the materialised ``library_albums`` row for a release group.

        Removal soft-deletes the album's ``library_files``, but ``in_library`` on
        the ``/basic`` album response is derived from whether a ``library_albums``
        row exists (the ``album_service`` fallback). That table has no soft-delete,
        so the row must be dropped here or the album keeps reporting
        ``in_library=true`` until a full sync rebuilds the table."""
        normalized_mbid = _normalize(musicbrainz_id)

        def operation(conn: sqlite3.Connection) -> None:
            conn.execute(
                "DELETE FROM library_albums WHERE mbid_lower = ?",
                (normalized_mbid,),
            )

        await self._write(operation)

    async def get_all_artist_mbids(self) -> set[str]:
        def operation(conn: sqlite3.Connection) -> set[str]:
            rows = conn.execute("SELECT mbid FROM library_artists").fetchall()
            return {str(row["mbid"]) for row in rows if row["mbid"]}

        return await self._read(operation)

    async def get_all_albums_for_matching(self) -> list[tuple[str, str, str, str]]:
        """Return (title, artist_name, album_mbid, artist_mbid) for all library albums."""

        def operation(conn: sqlite3.Connection) -> list[tuple[str, str, str, str]]:
            rows = conn.execute(
                "SELECT title, artist_name, mbid, COALESCE(artist_mbid, '') AS artist_mbid FROM library_albums"
            ).fetchall()
            return [
                (str(row["title"]), str(row["artist_name"] or ""), str(row["mbid"]), str(row["artist_mbid"]))
                for row in rows
                if row["title"] and row["mbid"]
            ]

        return await self._read(operation)

    async def get_stats(self) -> dict[str, Any]:
        def operation(conn: sqlite3.Connection) -> dict[str, Any]:
            artist_row = conn.execute("SELECT COUNT(*) AS count FROM library_artists").fetchone()
            album_row = conn.execute("SELECT COUNT(*) AS count FROM library_albums").fetchone()
            sync_row = conn.execute("SELECT value FROM cache_meta WHERE key = 'last_library_sync'").fetchone()
            last_sync = None
            if sync_row is not None:
                try:
                    last_sync = float(sync_row["value"])
                except (TypeError, ValueError):
                    last_sync = None
            db_size_bytes = self.db_path.stat().st_size if self.db_path.exists() else 0
            return {
                "artist_count": int(artist_row["count"] if artist_row is not None else 0),
                "album_count": int(album_row["count"] if album_row is not None else 0),
                "db_size_bytes": db_size_bytes,
                "last_sync": last_sync,
            }

        return await self._read(operation)

    async def upsert_library_file(self, row: dict[str, Any]) -> str:
        """Read-then-update-or-insert; NOT INSERT OR REPLACE, which would destroy
        row identity. UPDATE preserves id/imported_at/download_task_id and clears
        deleted_at (re-import un-deletes). Returns the row id."""
        now = time.time()
        file_path = row["file_path"]

        def operation(conn: sqlite3.Connection) -> str:
            # partial unique index only constrains non-deleted rows, so a path can
            # have several soft-deleted rows; prefer active then most recent so
            # re-import deterministically revives the right one
            existing = conn.execute(
                "SELECT id FROM library_files WHERE file_path = ? "
                "ORDER BY deleted_at IS NULL DESC, imported_at DESC LIMIT 1",
                (file_path,),
            ).fetchone()
            value_params = [row.get(col) for col in _LIBRARY_FILE_VALUE_COLUMNS]
            # folded mirrors are written via SQL fold() - the same function used at
            # search time - so the stored values never drift from the query side
            folded_params = [row.get(src) for src in _LIBRARY_FILE_FOLDED_COLUMNS.values()]
            if existing is not None:
                set_clause = ", ".join(f"{col} = ?" for col in _LIBRARY_FILE_VALUE_COLUMNS)
                folded_clause = ", ".join(
                    f"{col} = fold(?)" for col in _LIBRARY_FILE_FOLDED_COLUMNS
                )
                conn.execute(
                    f"UPDATE library_files SET {set_clause}, {folded_clause}, "
                    "deleted_at = NULL WHERE id = ?",
                    (*value_params, *folded_params, existing["id"]),
                )
                return str(existing["id"])
            new_id = uuid4().hex
            columns = (
                "id",
                "download_task_id",
                *_LIBRARY_FILE_VALUE_COLUMNS,
                "imported_at",
                *_LIBRARY_FILE_FOLDED_COLUMNS,
            )
            placeholders = ", ".join(
                ["?"] * (len(columns) - len(_LIBRARY_FILE_FOLDED_COLUMNS))
                + ["fold(?)"] * len(_LIBRARY_FILE_FOLDED_COLUMNS)
            )
            conn.execute(
                f"INSERT INTO library_files ({', '.join(columns)}) VALUES ({placeholders})",
                (
                    new_id,
                    row.get("download_task_id"),
                    *value_params,
                    row.get("imported_at") or now,
                    *folded_params,
                ),
            )
            return new_id

        return await self._write(operation)

    async def get_albums_aggregated(
        self,
        *,
        limit: int = 50,
        offset: int = 0,
        sort: str = "recent",
        q: str | None = None,
        file_format: str | None = None,
        decade: int | None = None,
    ) -> tuple[list[dict[str, Any]], int]:
        """Aggregation-on-read: group non-deleted files by release group.

        q filters on album title / album artist (LIKE, accent- and
        case-insensitive via fold()).
        file_format keeps albums with at least one file of that format
        (an album with mixed FLAC+MP3 matches both filters)."""
        order = _ALBUM_AGG_SORTS.get(sort, _ALBUM_AGG_SORTS["recent"])
        filters = ["lf.deleted_at IS NULL", "lf.release_group_mbid IS NOT NULL"]
        params: list[object] = []
        if q:
            term = f"%{_escape_like(q)}%"
            filters.append(
                "(lf.album_title_folded LIKE fold(?) ESCAPE '\\' "
                "OR lf.album_artist_name_folded LIKE fold(?) ESCAPE '\\')"
            )
            params.extend([term, term])
        if file_format:
            filters.append(
                "lf.release_group_mbid IN (SELECT release_group_mbid FROM library_files "
                "WHERE deleted_at IS NULL AND release_group_mbid IS NOT NULL AND file_format = ?)"
            )
            params.append(file_format)
        if decade is not None:
            filters.append("lf.year >= ? AND lf.year <= ?")
            params.extend([decade, decade + 9])
        where = " AND ".join(filters)

        def operation(conn: sqlite3.Connection) -> tuple[list[dict[str, Any]], int]:
            total_row = conn.execute(
                f"SELECT COUNT(DISTINCT lf.release_group_mbid) AS cnt "
                f"FROM library_files lf WHERE {where}",
                params,
            ).fetchone()
            total = int(total_row["cnt"]) if total_row else 0
            rows = conn.execute(
                f"""
                SELECT lf.release_group_mbid AS release_group_mbid,
                       MAX(lf.album_title) AS album_title,
                       MAX(lf.album_artist_name) AS album_artist_name,
                       MAX(lf.imported_at) AS last_imported_at,
                       COUNT(*) AS track_count,
                       SUM(lf.file_size_bytes) AS total_size_bytes,
                       -- highest-quality format present, not MIN() which is
                       -- alphabetical (would pick 'alac' over 'flac', 'mp3' over 'wav')
                       (SELECT q.file_format FROM library_files q
                        WHERE q.release_group_mbid = lf.release_group_mbid
                          AND q.deleted_at IS NULL
                        ORDER BY CASE LOWER(q.file_format)
                            WHEN 'flac' THEN 0 WHEN 'wav' THEN 1 WHEN 'alac' THEN 2
                            WHEN 'aiff' THEN 3 WHEN 'mp3' THEN 4 WHEN 'm4a' THEN 5
                            WHEN 'aac' THEN 6 WHEN 'opus' THEN 7 WHEN 'ogg' THEN 8
                            ELSE 99 END
                        LIMIT 1) AS file_format,
                       MAX(lf.year) AS year,
                       MAX(lf.is_compilation) AS is_compilation,
                       lam.cover_url AS cover_url
                FROM library_files lf
                LEFT JOIN library_album_meta lam
                    ON lam.release_group_mbid = lf.release_group_mbid
                WHERE {where}
                GROUP BY lf.release_group_mbid
                ORDER BY {order}
                LIMIT ? OFFSET ?
                """,
                (*params, max(limit, 1), max(offset, 0)),
            ).fetchall()
            return [dict(r) for r in rows], total

        return await self._read(operation)

    async def get_tracks_paginated(
        self,
        *,
        limit: int = 48,
        offset: int = 0,
        sort: str = "recent",
        q: str | None = None,
    ) -> tuple[list[dict[str, Any]], int]:
        """Flat, paginated track list across the whole library. One row per
        non-deleted, matched file. q filters on track title / artist / album
        artist / album title (LIKE, accent- and case-insensitive via fold()).
        Returns (rows, total)."""
        order = _TRACK_LIST_SORTS.get(sort, _TRACK_LIST_SORTS["recent"])
        filters = ["lf.deleted_at IS NULL", "lf.release_group_mbid IS NOT NULL"]
        params: list[object] = []
        if q:
            term = f"%{_escape_like(q)}%"
            filters.append(
                "(lf.track_title_folded LIKE fold(?) ESCAPE '\\' "
                "OR lf.artist_name_folded LIKE fold(?) ESCAPE '\\' "
                "OR lf.album_artist_name_folded LIKE fold(?) ESCAPE '\\' "
                "OR lf.album_title_folded LIKE fold(?) ESCAPE '\\')"
            )
            params.extend([term, term, term, term])
        where = " AND ".join(filters)

        def operation(conn: sqlite3.Connection) -> tuple[list[dict[str, Any]], int]:
            total_row = conn.execute(
                f"SELECT COUNT(*) AS cnt FROM library_files lf WHERE {where}", params
            ).fetchone()
            total = int(total_row["cnt"]) if total_row else 0
            rows = conn.execute(
                f"""
                SELECT lf.id, lf.track_title, lf.album_title, lf.artist_name,
                       lf.album_artist_name, lf.release_group_mbid, lf.file_format,
                       lf.track_number, lf.disc_number, lf.duration_seconds,
                       lam.cover_url
                FROM library_files lf
                LEFT JOIN library_album_meta lam
                    ON lam.release_group_mbid = lf.release_group_mbid
                WHERE {where}
                ORDER BY {order}
                LIMIT ? OFFSET ?
                """,
                (*params, max(limit, 1), max(offset, 0)),
            ).fetchall()
            return [dict(r) for r in rows], total

        return await self._read(operation)

    async def get_crate_tracks(
        self, *, order: str = "random", limit: int = 8, decade: int | None = None
    ) -> list[dict[str, Any]]:
        """Individual tracks (with album context + cover) for the Listening Room
        crate. order: 'recent' (newest imports), 'oldest', else random. decade
        filters to a 10-year window."""
        order_sql = {"recent": "lf.imported_at DESC", "oldest": "lf.imported_at ASC"}.get(
            order, "RANDOM()"
        )
        filters = ["lf.deleted_at IS NULL", "lf.release_group_mbid IS NOT NULL"]
        params: list[object] = []
        if decade is not None:
            filters.append("lf.year >= ? AND lf.year <= ?")
            params.extend([decade, decade + 9])
        where = " AND ".join(filters)

        def operation(conn: sqlite3.Connection) -> list[dict[str, Any]]:
            rows = conn.execute(
                f"""
                SELECT lf.id, lf.track_title, lf.album_title, lf.album_artist_name,
                       lf.artist_name, lf.release_group_mbid, lf.file_format, lf.year,
                       lf.duration_seconds, lam.cover_url
                FROM library_files lf
                LEFT JOIN library_album_meta lam
                    ON lam.release_group_mbid = lf.release_group_mbid
                WHERE {where}
                ORDER BY {order_sql}
                LIMIT ?
                """,
                (*params, max(limit, 1)),
            ).fetchall()
            return [dict(r) for r in rows]

        return await self._read(operation)

    async def search_tracks(
        self, q: str, *, limit: int = 30
    ) -> list[dict[str, Any]]:
        """Individual tracks (with album context + cover) matching q on track
        title, artist, album artist, or album title (LIKE, accent- and
        case-insensitive via fold()).
        Prefix matches on track title rank first, then album/disc/track order so
        an album's tracks come out together in playing order."""
        term = f"%{_escape_like(q)}%"
        prefix = f"{_escape_like(q)}%"

        def operation(conn: sqlite3.Connection) -> list[dict[str, Any]]:
            rows = conn.execute(
                """
                SELECT lf.id, lf.track_title, lf.album_title, lf.album_artist_name,
                       lf.artist_name, lf.release_group_mbid, lf.file_format, lf.year,
                       lf.duration_seconds, lam.cover_url
                FROM library_files lf
                LEFT JOIN library_album_meta lam
                    ON lam.release_group_mbid = lf.release_group_mbid
                WHERE lf.deleted_at IS NULL AND lf.release_group_mbid IS NOT NULL
                  AND (lf.track_title_folded LIKE fold(?) ESCAPE '\\'
                       OR lf.artist_name_folded LIKE fold(?) ESCAPE '\\'
                       OR lf.album_artist_name_folded LIKE fold(?) ESCAPE '\\'
                       OR lf.album_title_folded LIKE fold(?) ESCAPE '\\')
                ORDER BY
                    CASE WHEN lf.track_title_folded LIKE fold(?) ESCAPE '\\' THEN 0 ELSE 1 END,
                    lf.album_title COLLATE NOCASE,
                    lf.disc_number, lf.track_number
                LIMIT ?
                """,
                (term, term, term, term, prefix, max(limit, 1)),
            ).fetchall()
            return [dict(r) for r in rows]

        return await self._read(operation)

    async def get_albums_for_artist(self, artist_mbid: str) -> list[dict[str, Any]]:
        """Aggregated albums (release groups) whose album-artist is this MBID,
        for Subsonic getArtist. Carries album_artist_mbid so the view can emit a
        non-null artistId (Q14)."""

        def operation(conn: sqlite3.Connection) -> list[dict[str, Any]]:
            rows = conn.execute(
                """
                SELECT lf.release_group_mbid AS release_group_mbid,
                       MAX(lf.album_title) AS album_title,
                       MAX(lf.album_artist_name) AS album_artist_name,
                       MAX(lf.album_artist_mbid) AS album_artist_mbid,
                       COUNT(*) AS track_count,
                       SUM(lf.file_size_bytes) AS total_size_bytes,
                       MAX(lf.year) AS year,
                       MAX(lf.is_compilation) AS is_compilation,
                       MAX(lf.imported_at) AS last_imported_at,
                       lam.cover_url AS cover_url
                FROM library_files lf
                LEFT JOIN library_album_meta lam
                    ON lam.release_group_mbid = lf.release_group_mbid
                WHERE lf.deleted_at IS NULL AND lf.release_group_mbid IS NOT NULL
                  AND lf.album_artist_mbid = ?
                GROUP BY lf.release_group_mbid
                ORDER BY MAX(lf.year), MAX(lf.album_title) COLLATE NOCASE
                """,
                (artist_mbid,),
            ).fetchall()
            return [dict(r) for r in rows]

        return await self._read(operation)

    async def get_genres(self) -> list[dict[str, Any]]:
        """Distinct non-empty genres with song + album counts (Subsonic getGenres)."""

        def operation(conn: sqlite3.Connection) -> list[dict[str, Any]]:
            rows = conn.execute(
                "SELECT genre, COUNT(*) AS song_count, "
                "COUNT(DISTINCT release_group_mbid) AS album_count "
                "FROM library_files "
                "WHERE deleted_at IS NULL AND genre IS NOT NULL AND genre != '' "
                "GROUP BY genre ORDER BY genre COLLATE NOCASE"
            ).fetchall()
            return [dict(r) for r in rows]

        return await self._read(operation)

    async def get_files_by_genre(
        self, genre: str, *, limit: int = 50, offset: int = 0
    ) -> list[dict[str, Any]]:
        """Active files matching a genre (case-insensitive), album/disc/track order."""

        def operation(conn: sqlite3.Connection) -> list[dict[str, Any]]:
            rows = conn.execute(
                "SELECT * FROM library_files "
                "WHERE deleted_at IS NULL AND release_group_mbid IS NOT NULL "
                "AND genre = ? COLLATE NOCASE "
                "ORDER BY album_title COLLATE NOCASE, disc_number, track_number "
                "LIMIT ? OFFSET ?",
                (genre, max(limit, 1), max(offset, 0)),
            ).fetchall()
            return [dict(r) for r in rows]

        return await self._read(operation)

    async def get_files_by_artist_name(
        self, artist_name: str, *, limit: int = 200
    ) -> list[dict[str, Any]]:
        """Active files by an artist (track OR album artist), newest first. The
        discovery service ranks these by play count from play_history (Q12)."""

        def operation(conn: sqlite3.Connection) -> list[dict[str, Any]]:
            rows = conn.execute(
                "SELECT * FROM library_files "
                "WHERE deleted_at IS NULL AND release_group_mbid IS NOT NULL "
                "AND (artist_name = ? OR album_artist_name = ?) "
                "ORDER BY imported_at DESC LIMIT ?",
                (artist_name, artist_name, max(limit, 1)),
            ).fetchall()
            return [dict(r) for r in rows]

        return await self._read(operation)

    async def get_files_by_artist_mbids(
        self, mbids: list[str], *, limit: int = 50
    ) -> list[dict[str, Any]]:
        """Active files whose track OR album artist is one of the given MBIDs
        (Q12 same-artist + related pools; Q23 union semantics). Random order."""
        if not mbids:
            return []

        def operation(conn: sqlite3.Connection) -> list[dict[str, Any]]:
            ph = ", ".join("?" for _ in mbids)
            rows = conn.execute(
                f"""
                SELECT * FROM library_files
                WHERE deleted_at IS NULL AND release_group_mbid IS NOT NULL
                  AND (artist_mbid IN ({ph}) OR album_artist_mbid IN ({ph}))
                ORDER BY RANDOM()
                LIMIT ?
                """,
                (*mbids, *mbids, max(limit, 1)),
            ).fetchall()
            return [dict(r) for r in rows]

        return await self._read(operation)

    async def get_files_by_album_artist_mbids(
        self, mbids: list[str], *, limit: int = 500
    ) -> list[dict[str, Any]]:
        """Active files whose ALBUM artist is one of the given MBIDs (Jellyfin
        AlbumArtistIds - strict, decision Q23). Album/disc/track order."""
        if not mbids:
            return []

        def operation(conn: sqlite3.Connection) -> list[dict[str, Any]]:
            ph = ", ".join("?" for _ in mbids)
            rows = conn.execute(
                f"SELECT * FROM library_files "
                f"WHERE deleted_at IS NULL AND release_group_mbid IS NOT NULL "
                f"AND album_artist_mbid IN ({ph}) "
                f"ORDER BY album_title COLLATE NOCASE, disc_number, track_number "
                f"LIMIT ?",
                (*mbids, max(limit, 1)),
            ).fetchall()
            return [dict(r) for r in rows]

        return await self._read(operation)

    async def get_random_files(
        self,
        *,
        limit: int = 50,
        genre: str | None = None,
        from_year: int | None = None,
        to_year: int | None = None,
    ) -> list[dict[str, Any]]:
        """Random active files, optionally filtered by genre / year range (Q12)."""
        filters = ["deleted_at IS NULL", "release_group_mbid IS NOT NULL"]
        params: list[object] = []
        if genre:
            filters.append("genre = ?")
            params.append(genre)
        if from_year is not None:
            filters.append("year >= ?")
            params.append(from_year)
        if to_year is not None:
            filters.append("year <= ?")
            params.append(to_year)
        where = " AND ".join(filters)

        def operation(conn: sqlite3.Connection) -> list[dict[str, Any]]:
            rows = conn.execute(
                f"SELECT * FROM library_files WHERE {where} ORDER BY RANDOM() LIMIT ?",
                (*params, max(limit, 1)),
            ).fetchall()
            return [dict(r) for r in rows]

        return await self._read(operation)

    async def get_related_artist_mbids(self, artist_mbid: str) -> str | None:
        """The lazy-MB related-artist cache (csv) for an artist, or None if never
        fetched (Q20). Empty string means 'fetched, no relations' (don't refetch)."""
        normalized = _normalize(artist_mbid)

        def operation(conn: sqlite3.Connection) -> str | None:
            row = conn.execute(
                "SELECT related_artist_mbids FROM library_artists WHERE mbid_lower = ?",
                (normalized,),
            ).fetchone()
            return row["related_artist_mbids"] if row else None

        return await self._read(operation)

    async def set_related_artist_mbids(self, artist_mbid: str, csv: str) -> None:
        normalized = _normalize(artist_mbid)

        def operation(conn: sqlite3.Connection) -> None:
            conn.execute(
                "UPDATE library_artists SET related_artist_mbids = ? WHERE mbid_lower = ?",
                (csv, normalized),
            )

        await self._write(operation)

    async def get_decades(self) -> list[dict[str, Any]]:
        """Album counts grouped by decade (from album year), newest decade first."""

        def operation(conn: sqlite3.Connection) -> list[dict[str, Any]]:
            rows = conn.execute(
                """
                SELECT (lf.year / 10) * 10 AS decade,
                       COUNT(DISTINCT lf.release_group_mbid) AS album_count
                FROM library_files lf
                WHERE lf.deleted_at IS NULL AND lf.release_group_mbid IS NOT NULL
                  AND lf.year IS NOT NULL AND lf.year > 0
                GROUP BY decade
                ORDER BY decade DESC
                """
            ).fetchall()
            return [dict(r) for r in rows]

        return await self._read(operation)

    async def get_library_file_by_id(self, file_id: str) -> dict[str, Any] | None:
        """One library_files row by id (active or soft-deleted)."""

        def operation(conn: sqlite3.Connection) -> dict[str, Any] | None:
            row = conn.execute(
                "SELECT * FROM library_files WHERE id = ?", (file_id,)
            ).fetchone()
            return dict(row) if row is not None else None

        return await self._read(operation)

    async def get_library_files_for_album(self, release_group_mbid: str) -> list[dict[str, Any]]:
        # stored lower-cased (see upsert_library_file / has_album_files), so normalize
        # the input the same way or a mixed-case MBID silently returns no rows
        normalized = _normalize(release_group_mbid)

        def operation(conn: sqlite3.Connection) -> list[dict[str, Any]]:
            rows = conn.execute(
                "SELECT * FROM library_files WHERE release_group_mbid = ? "
                "AND deleted_at IS NULL ORDER BY disc_number, track_number",
                (normalized,),
            ).fetchall()
            return [dict(r) for r in rows]

        return await self._read(operation)

    async def get_attributions_for_paths(
        self, paths: list[str]
    ) -> dict[str, dict[str, Any]]:
        """Active rows for the given file paths, keyed by ``file_path`` - the scanner's
        anchor read (prior release-group/confidence/source, so a re-scan can protect
        known-good identity instead of blindly re-deciding it)."""
        if not paths:
            return {}

        def operation(conn: sqlite3.Connection) -> dict[str, dict[str, Any]]:
            out: dict[str, dict[str, Any]] = {}
            # chunk to stay under SQLite's bound-parameter limit
            for start in range(0, len(paths), 500):
                chunk = paths[start : start + 500]
                placeholders = ",".join("?" * len(chunk))
                rows = conn.execute(
                    f"SELECT * FROM library_files WHERE file_path IN ({placeholders}) "
                    "AND deleted_at IS NULL",
                    tuple(chunk),
                ).fetchall()
                for r in rows:
                    row = dict(r)
                    out[row["file_path"]] = row
            return out

        return await self._read(operation)

    async def get_active_file_at_position(
        self, release_group_mbid: str, disc_number: int, track_number: int
    ) -> dict[str, Any] | None:
        """The single active (non-deleted) file occupying one album (disc, track)
        slot, or None. Used by the download import to dedupe a re-pull / alternate-
        format copy of a track the library already holds. MBID is stored lower-cased,
        so normalize the input the same way."""
        normalized = _normalize(release_group_mbid)

        def operation(conn: sqlite3.Connection) -> dict[str, Any] | None:
            row = conn.execute(
                "SELECT * FROM library_files WHERE release_group_mbid = ? "
                "AND disc_number = ? AND track_number = ? AND deleted_at IS NULL "
                "ORDER BY imported_at LIMIT 1",
                (normalized, disc_number, track_number),
            ).fetchone()
            return dict(row) if row is not None else None

        return await self._read(operation)

    async def get_library_files_for_task(self, download_task_id: str) -> list[dict[str, Any]]:
        """Active rows imported by one download task (crash-idempotency reconcile)."""

        def operation(conn: sqlite3.Connection) -> list[dict[str, Any]]:
            rows = conn.execute(
                "SELECT * FROM library_files WHERE download_task_id = ? "
                "AND deleted_at IS NULL",
                (download_task_id,),
            ).fetchall()
            return [dict(r) for r in rows]

        return await self._read(operation)

    async def has_album_files(self, release_group_mbid: str) -> bool:
        # MBIDs are case-insensitive identifiers stored lower-cased; lower the input
        # so matching is consistent with the get_library_mbids consumers (which .lower()
        # the key) and stays case-insensitive like the old library_albums lookup.
        return await self._exists(
            "SELECT 1 FROM library_files WHERE release_group_mbid = ? "
            "AND deleted_at IS NULL LIMIT 1",
            (release_group_mbid.lower(),),
        )

    async def get_album_release_mbid(self, release_group_mbid: str) -> str | None:
        """The specific release edition the library's files for this group belong to.

        Returns the most common non-null ``release_mbid`` among the group's non-deleted
        files (a whole folder is claimed under one release, so there's normally just one),
        or ``None`` when nothing is stored - callers then fall back to release ranking.
        """

        def operation(conn: sqlite3.Connection) -> str | None:
            row = conn.execute(
                "SELECT release_mbid FROM library_files "
                "WHERE release_group_mbid = ? AND release_mbid IS NOT NULL AND deleted_at IS NULL "
                "GROUP BY release_mbid ORDER BY COUNT(*) DESC LIMIT 1",
                (release_group_mbid.lower(),),
            ).fetchone()
            return str(row["release_mbid"]) if row and row["release_mbid"] else None

        return await self._read(operation)

    async def get_total_library_bytes(self) -> int:
        """Whole-library active bytes - the global storage cap's usage side (a thin
        cut of get_library_stats so the admission check doesn't pay for the rest)."""

        def operation(conn: sqlite3.Connection) -> int:
            row = conn.execute(
                "SELECT COALESCE(SUM(file_size_bytes), 0) FROM library_files "
                "WHERE deleted_at IS NULL"
            ).fetchone()
            return int(row[0])

        return await self._read(operation)

    async def get_user_library_bytes(self, user_id: str) -> int:
        """Bytes attributable to one user's downloads (A5): active files whose
        download_task_id belongs to a task that user created. Scan-discovered files
        have no task -> unowned, they count only toward the global cap. The stores
        share one SQLite file, so the cross-table join is local."""

        def operation(conn: sqlite3.Connection) -> int:
            row = conn.execute(
                """SELECT COALESCE(SUM(lf.file_size_bytes), 0)
                   FROM library_files lf
                   JOIN download_tasks dt ON dt.id = lf.download_task_id
                   WHERE lf.deleted_at IS NULL AND dt.user_id = ?""",
                (user_id,),
            ).fetchone()
            return int(row[0])

        return await self._read(operation)

    async def list_cutoff_unmet(self, cutoff_rank: int) -> list[dict[str, Any]]:
        """Albums whose WORST active file ranks below ``cutoff_rank`` - the upgrade
        worklist (CollectionManagement D7). ``_TIER_RANK_CASE`` must rank every
        (format, bitrate) band exactly like ``quality_tiers.tier_for`` (asserted by
        ``test_cutoff_unmet``); rows return ``worst_rank`` for the caller to map back
        to a tier key."""

        def operation(conn: sqlite3.Connection) -> list[dict[str, Any]]:
            rows = conn.execute(
                f"""SELECT release_group_mbid,
                       MIN({_TIER_RANK_CASE}) AS worst_rank,
                       COUNT(*) AS track_count,
                       COALESCE(MAX(album_artist_name), MAX(artist_name)) AS artist_name,
                       MAX(album_artist_mbid) AS artist_mbid,
                       MAX(album_title) AS album_title,
                       MAX(year) AS year
                   FROM library_files
                   WHERE deleted_at IS NULL
                     AND release_group_mbid IS NOT NULL AND release_group_mbid != ''
                   GROUP BY release_group_mbid
                   HAVING MIN({_TIER_RANK_CASE}) < ?
                   ORDER BY artist_name COLLATE NOCASE, album_title COLLATE NOCASE""",
                (cutoff_rank,),
            ).fetchall()
            return [dict(r) for r in rows]

        return await self._read(operation)

    async def get_library_files_for_recording(self, recording_mbid: str) -> list[dict[str, Any]]:
        def operation(conn: sqlite3.Connection) -> list[dict[str, Any]]:
            rows = conn.execute(
                "SELECT * FROM library_files WHERE recording_mbid = ? AND deleted_at IS NULL",
                (recording_mbid,),
            ).fetchall()
            return [dict(r) for r in rows]

        return await self._read(operation)

    async def has_recording(self, recording_mbid: str) -> bool:
        return await self._exists(
            "SELECT 1 FROM library_files WHERE recording_mbid = ? "
            "AND deleted_at IS NULL LIMIT 1",
            (recording_mbid,),
        )

    async def has_any_files(self) -> bool:
        return await self._exists(
            "SELECT 1 FROM library_files WHERE deleted_at IS NULL LIMIT 1", ()
        )

    async def get_library_mbids(self, include_release_ids: bool = True) -> set[str]:
        """Distinct release-group MBIDs in the native library (excluding soft-deleted).
        With ``include_release_ids``, also include per-edition release MBIDs. Powers the
        "is this album in my library" set."""

        def operation(conn: sqlite3.Connection) -> set[str]:
            mbids: set[str] = set()
            for row in conn.execute(
                "SELECT DISTINCT release_group_mbid FROM library_files "
                "WHERE release_group_mbid IS NOT NULL AND deleted_at IS NULL"
            ).fetchall():
                if row["release_group_mbid"]:
                    mbids.add(str(row["release_group_mbid"]))
            if include_release_ids:
                for row in conn.execute(
                    "SELECT DISTINCT release_mbid FROM library_files "
                    "WHERE release_mbid IS NOT NULL AND deleted_at IS NULL"
                ).fetchall():
                    if row["release_mbid"]:
                        mbids.add(str(row["release_mbid"]))
            return mbids

        return await self._read(operation)

    async def _exists(self, sql: str, params: tuple[Any, ...]) -> bool:
        def operation(conn: sqlite3.Connection) -> bool:
            return conn.execute(sql, params).fetchone() is not None

        return await self._read(operation)

    async def soft_delete_library_file(self, file_path: str) -> None:
        now = time.time()

        def operation(conn: sqlite3.Connection) -> None:
            conn.execute(
                "UPDATE library_files SET deleted_at = ? WHERE file_path = ? AND deleted_at IS NULL",
                (now, file_path),
            )

        await self._write(operation)

    async def soft_delete_album_files(self, release_group_mbid: str) -> list[str]:
        """Soft-delete every active file of an album. Returns the affected file
        paths so the caller can optionally unlink them from disk."""
        now = time.time()
        normalized = _normalize(release_group_mbid)

        def operation(conn: sqlite3.Connection) -> list[str]:
            rows = conn.execute(
                "SELECT file_path FROM library_files "
                "WHERE release_group_mbid = ? AND deleted_at IS NULL",
                (normalized,),
            ).fetchall()
            paths = [str(r["file_path"]) for r in rows]
            if paths:
                conn.execute(
                    "UPDATE library_files SET deleted_at = ? "
                    "WHERE release_group_mbid = ? AND deleted_at IS NULL",
                    (now, normalized),
                )
            return paths

        return await self._write(operation)

    async def count_artist_albums(
        self,
        *,
        artist_mbid: str | None = None,
        artist_name: str | None = None,
        exclude_release_group_mbid: str | None = None,
    ) -> int:
        """Distinct non-deleted albums for an album-artist (matched by MBID when
        present, else by name). ``exclude_release_group_mbid`` drops one album from
        the count, used by the removal preview before its files are soft-deleted."""

        def operation(conn: sqlite3.Connection) -> int:
            filters = ["deleted_at IS NULL", "release_group_mbid IS NOT NULL"]
            params: list[object] = []
            if artist_mbid:
                filters.append("album_artist_mbid = ?")
                params.append(artist_mbid)
            elif artist_name:
                filters.append("album_artist_name = ?")
                params.append(artist_name)
            else:
                return 0
            if exclude_release_group_mbid:
                filters.append("release_group_mbid != ?")
                params.append(exclude_release_group_mbid)
            where = " AND ".join(filters)
            row = conn.execute(
                f"SELECT COUNT(DISTINCT release_group_mbid) AS cnt "
                f"FROM library_files WHERE {where}",
                params,
            ).fetchone()
            return int(row["cnt"]) if row else 0

        return await self._read(operation)

    async def mark_missing_files(
        self,
        present_paths: set[str],
        *,
        scope_dirs: list[str] | None = None,
        protect_downloads_after: float | None = None,
        protected_roots: list[str] | None = None,
    ) -> int:
        """Soft-delete non-deleted files whose path is not in present_paths.

        scope_dirs limits the soft-delete to files under those dirs; a partial
        reconcile (e.g. post-import of one album) must never flag files outside
        the dirs it walked, or importing one album marks the rest of the library
        as missing. No scope_dirs = full-library reconcile. protect_downloads_after
        spares recent downloads from a race with the orchestrator. protected_roots
        spares a library root that returned no files (likely unmounted) from a mass
        soft-delete while siblings stay healthy. Returns the count."""
        now = time.time()
        protected = [Path(r) for r in protected_roots or []]
        scope = [Path(d) for d in scope_dirs or []]

        def _is_protected(file_path: str) -> bool:
            if not protected:
                return False
            candidate = Path(file_path)
            return any(candidate.is_relative_to(root) for root in protected)

        def _in_scope(file_path: str) -> bool:
            if not scope:
                return True  # no scope limit -> consider the whole library
            candidate = Path(file_path)
            return any(candidate.is_relative_to(d) for d in scope)

        def operation(conn: sqlite3.Connection) -> int:
            rows = conn.execute(
                "SELECT id, file_path, source, imported_at FROM library_files WHERE deleted_at IS NULL"
            ).fetchall()
            to_delete: list[tuple[float, str]] = []
            for row in rows:
                if row["file_path"] in present_paths:
                    continue
                if not _in_scope(row["file_path"]):
                    continue
                if (
                    protect_downloads_after is not None
                    and row["source"] == "download"
                    and (row["imported_at"] or 0) > protect_downloads_after
                ):
                    continue
                if _is_protected(row["file_path"]):
                    continue
                to_delete.append((now, str(row["id"])))
            if to_delete:
                conn.executemany(
                    "UPDATE library_files SET deleted_at = ? WHERE id = ?", to_delete
                )
            return len(to_delete)

        return await self._write(operation)

    async def get_unmatched_files(self) -> list[dict[str, Any]]:
        def operation(conn: sqlite3.Connection) -> list[dict[str, Any]]:
            # only unresolved rows; matches get_library_stats' unmatched_count
            # (also filters resolution IS NULL)
            rows = conn.execute(
                "SELECT * FROM manual_review_queue WHERE resolution IS NULL "
                "ORDER BY created_at DESC"
            ).fetchall()
            return [self._decode_review_row(row) for row in rows]

        return await self._read(operation)

    async def get_manual_review_by_id(self, review_id: int) -> dict[str, Any] | None:
        """One ``manual_review_queue`` row by id, candidate MBIDs decoded."""

        def operation(conn: sqlite3.Connection) -> dict[str, Any] | None:
            row = conn.execute(
                "SELECT * FROM manual_review_queue WHERE id = ?", (review_id,)
            ).fetchone()
            return self._decode_review_row(row) if row is not None else None

        return await self._read(operation)

    async def resolve_manual_review(self, review_id: int, resolution: str) -> bool:
        """Mark a review row resolved (``accepted`` | ``rejected`` | ``manual_id``).
        Returns ``True`` if a still-unresolved row was updated."""
        now = time.time()

        def operation(conn: sqlite3.Connection) -> bool:
            cursor = conn.execute(
                "UPDATE manual_review_queue SET resolution = ?, resolved_at = ? "
                "WHERE id = ? AND resolution IS NULL",
                (resolution, now, review_id),
            )
            return cursor.rowcount > 0

        return await self._write(operation)

    @staticmethod
    def _decode_review_row(row: Any) -> dict[str, Any]:
        """Row dict with candidate_mbids_encoded blob decoded back to a list, so
        the raw encoded column never leaks to the API."""
        entry = dict(row)
        encoded = entry.pop("candidate_mbids_encoded", None)
        entry["candidate_mbids"] = _decode_json(encoded) if encoded else []
        return entry

    async def add_to_manual_review(self, entry: dict[str, Any]) -> None:
        """Insert (or re-queue) a file the scanner couldn't confidently identify.
        file_path is UNIQUE, so a re-scan refreshes the row in place rather than
        duplicating."""
        now = time.time()
        candidates_encoded = _encode_json(to_jsonable(entry.get("candidate_mbids") or []))

        def operation(conn: sqlite3.Connection) -> None:
            conn.execute(
                """
                INSERT INTO manual_review_queue
                    (file_path, extracted_title, extracted_artist, extracted_album,
                     extracted_year, track_number, disc_number, file_format, duration,
                     file_size, fingerprint, fingerprint_score, candidate_mbids_encoded,
                     source, created_at)
                VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
                ON CONFLICT(file_path) DO UPDATE SET
                    extracted_title = excluded.extracted_title,
                    extracted_artist = excluded.extracted_artist,
                    extracted_album = excluded.extracted_album,
                    extracted_year = excluded.extracted_year,
                    track_number = excluded.track_number,
                    disc_number = excluded.disc_number,
                    file_format = excluded.file_format,
                    duration = excluded.duration,
                    file_size = excluded.file_size,
                    fingerprint = excluded.fingerprint,
                    fingerprint_score = excluded.fingerprint_score,
                    candidate_mbids_encoded = excluded.candidate_mbids_encoded,
                    source = excluded.source,
                    created_at = excluded.created_at,
                    resolved_at = NULL,
                    resolution = NULL
                """,
                (
                    entry["file_path"],
                    entry.get("extracted_title"),
                    entry.get("extracted_artist"),
                    entry.get("extracted_album"),
                    entry.get("extracted_year"),
                    entry.get("track_number"),
                    entry.get("disc_number"),
                    entry.get("file_format"),
                    entry.get("duration"),
                    entry.get("file_size"),
                    entry.get("fingerprint"),
                    entry.get("fingerprint_score"),
                    candidates_encoded,
                    entry.get("source", "text_match"),
                    now,
                ),
            )

        await self._write(operation)

    async def get_release_groups_needing_artist(self) -> list[str]:
        """Release groups of matched, non-compilation files with no *real* resolved
        album-artist MBID yet. A synthesized id (Q14) is dashless 32-hex while a
        real MusicBrainz MBID is dashed, so ``NOT LIKE '%-%'`` still flags the
        synthetic placeholders for upgrade by the reconcile pass."""

        def operation(conn: sqlite3.Connection) -> list[str]:
            rows = conn.execute(
                "SELECT DISTINCT release_group_mbid FROM library_files "
                "WHERE deleted_at IS NULL AND release_group_mbid IS NOT NULL "
                "AND is_compilation = 0 "
                "AND (album_artist_mbid IS NULL OR album_artist_mbid = '' "
                "OR album_artist_mbid NOT LIKE '%-%')"
            ).fetchall()
            return [r["release_group_mbid"] for r in rows]

        return await self._read(operation)

    async def set_album_artist(
        self, release_group_mbid: str, artist_mbid: str, artist_name: str
    ) -> int:
        """Stamp the canonical album-artist (MBID + name) onto a release group's non-compilation files; returns rows updated."""
        normalized = _normalize(release_group_mbid)

        def operation(conn: sqlite3.Connection) -> int:
            cursor = conn.execute(
                "UPDATE library_files SET album_artist_mbid = ?, album_artist_name = ?, "
                "album_artist_name_folded = fold(?) "
                "WHERE release_group_mbid = ? AND deleted_at IS NULL AND is_compilation = 0",
                (artist_mbid, artist_name, artist_name, normalized),
            )
            return cursor.rowcount

        return await self._write(operation)

    async def get_album_artist_for_release_group(
        self, release_group_mbid: str
    ) -> tuple[str, str] | None:
        """The (mbid, name) of a release group's already-resolved album artist, if any
        live non-compilation file carries a real (dashed) MusicBrainz id. Lets a new
        file joining a known album inherit the album's artist identity instead of
        synthesizing a second one (Q14) and splitting the artist in two."""
        normalized = _normalize(release_group_mbid)

        def operation(conn: sqlite3.Connection) -> tuple[str, str] | None:
            row = conn.execute(
                "SELECT album_artist_mbid, album_artist_name FROM library_files "
                "WHERE release_group_mbid = ? AND deleted_at IS NULL "
                "AND is_compilation = 0 AND album_artist_mbid LIKE '%-%' "
                "AND album_artist_name IS NOT NULL LIMIT 1",
                (normalized,),
            ).fetchone()
            if row is None:
                return None
            return (str(row["album_artist_mbid"]), str(row["album_artist_name"]))

        return await self._read(operation)

    async def prune_manual_review_for_imported(self) -> int:
        """Drop pending review rows whose file is now an active ``library_files`` row; returns rows removed."""

        def operation(conn: sqlite3.Connection) -> int:
            cursor = conn.execute(
                "DELETE FROM manual_review_queue WHERE resolution IS NULL AND file_path IN "
                "(SELECT file_path FROM library_files WHERE deleted_at IS NULL)"
            )
            return cursor.rowcount

        return await self._write(operation)

    async def get_file_index(self) -> dict[str, tuple[float, int]]:
        """Map active file_path -> (file_mtime, file_size_bytes) for the scanner's
        incremental skip. Only non-deleted rows."""

        def operation(conn: sqlite3.Connection) -> dict[str, tuple[float, int]]:
            rows = conn.execute(
                "SELECT file_path, file_mtime, file_size_bytes FROM library_files "
                "WHERE deleted_at IS NULL"
            ).fetchall()
            return {
                str(r["file_path"]): (float(r["file_mtime"]), int(r["file_size_bytes"]))
                for r in rows
            }

        return await self._read(operation)

    async def upsert_artist(self, mbid: str, name: str) -> None:
        """Idempotent insert of a (possibly synthetic) artist row (Q14, 06 s7.5).

        The native scanner is the sole writer of library_files, so it also owns
        the matching library_artists rows for MBID-less artists. ``raw_json`` is
        ``'{}'`` (only the legacy outbound sync populates real MB json). An
        existing row is left untouched so a re-scan never clobbers an aggregated
        album_count or a real-MB row."""
        normalized = _normalize(mbid)
        now = int(time.time())

        def operation(conn: sqlite3.Connection) -> None:
            conn.execute(
                "INSERT INTO library_artists "
                "(mbid_lower, mbid, name, album_count, date_added, raw_json) "
                "VALUES (?, ?, ?, 0, ?, '{}') ON CONFLICT(mbid_lower) DO NOTHING",
                (normalized, mbid, name, now),
            )

        await self._write(operation)

    async def get_artists_aggregated(
        self,
        *,
        limit: int = 50,
        offset: int = 0,
        sort_by: str = "name",
        sort_order: str = "asc",
        q: str | None = None,
    ) -> tuple[list[dict[str, Any]], int]:
        """Distinct album artists aggregated from non-deleted files.

        q filters on artist name (LIKE, accent- and case-insensitive via fold())."""
        key = "COALESCE(NULLIF(album_artist_mbid, ''), album_artist_name)"
        column = _ARTIST_AGG_SORT_COLUMNS.get(sort_by, _ARTIST_AGG_SORT_COLUMNS["name"])
        direction = "DESC" if sort_order.lower() == "desc" else "ASC"
        # end on the group key so the order is total and LIMIT/OFFSET pages are
        # stable - ties on the sort column would otherwise let a group repeat on
        # one page and vanish from another (the each-key duplicate that froze the
        # artists view). Matches the tiebreaker invariant on the track-list sorts.
        order = f"{column} {direction}, artist_name COLLATE NOCASE ASC, {key}"
        filters = ["deleted_at IS NULL", "release_group_mbid IS NOT NULL"]
        params: list[object] = []
        if q:
            filters.append("album_artist_name_folded LIKE fold(?) ESCAPE '\\'")
            params.append(f"%{_escape_like(q)}%")
        where = " AND ".join(filters)

        def operation(conn: sqlite3.Connection) -> tuple[list[dict[str, Any]], int]:
            total_row = conn.execute(
                f"SELECT COUNT(*) AS cnt FROM (SELECT 1 FROM library_files "
                f"WHERE {where} GROUP BY {key})",
                params,
            ).fetchone()
            rows = conn.execute(
                f"""
                SELECT MAX(album_artist_name) AS artist_name,
                       MAX(album_artist_mbid) AS artist_mbid,
                       COUNT(DISTINCT release_group_mbid) AS album_count,
                       COUNT(*) AS track_count,
                       MAX(imported_at) AS date_added
                FROM library_files
                WHERE {where}
                GROUP BY {key}
                ORDER BY {order}
                LIMIT ? OFFSET ?
                """,
                (*params, max(limit, 1), max(offset, 0)),
            ).fetchall()
            return [dict(r) for r in rows], int(total_row["cnt"]) if total_row else 0

        return await self._read(operation)

    async def get_library_stats(self) -> dict[str, Any]:
        def operation(conn: sqlite3.Connection) -> dict[str, Any]:
            agg = conn.execute(
                "SELECT COUNT(*) AS tracks, COUNT(DISTINCT release_group_mbid) AS albums, "
                "COUNT(DISTINCT COALESCE(NULLIF(album_artist_mbid, ''), album_artist_name)) "
                "AS artists, "
                "COALESCE(SUM(file_size_bytes), 0) AS size FROM library_files "
                "WHERE deleted_at IS NULL AND release_group_mbid IS NOT NULL"
            ).fetchone()
            fmt_rows = conn.execute(
                "SELECT file_format, COUNT(*) AS cnt FROM library_files "
                "WHERE deleted_at IS NULL GROUP BY file_format"
            ).fetchall()
            unmatched = conn.execute(
                "SELECT COUNT(*) AS cnt FROM manual_review_queue WHERE resolution IS NULL"
            ).fetchone()
            last_scan_at = None
            try:
                scan_row = conn.execute(
                    "SELECT started_at FROM scan_state WHERE id = 1"
                ).fetchone()
                if scan_row is not None:
                    last_scan_at = scan_row["started_at"]
            except sqlite3.OperationalError:
                pass
            return {
                "total_albums": int(agg["albums"] or 0) if agg else 0,
                "total_artists": int(agg["artists"] or 0) if agg else 0,
                "total_tracks": int(agg["tracks"] or 0) if agg else 0,
                "total_size_bytes": int(agg["size"] or 0) if agg else 0,
                "format_breakdown": {str(r["file_format"]): int(r["cnt"]) for r in fmt_rows},
                "unmatched_count": int(unmatched["cnt"]) if unmatched else 0,
                "last_scan_at": last_scan_at,
            }

        return await self._read(operation)

    async def clear(self) -> None:
        def operation(conn: sqlite3.Connection) -> None:
            conn.execute("DELETE FROM library_artists")
            conn.execute("DELETE FROM library_albums")
            for tbl in _CROSS_DOMAIN_CLEAR_TABLES + _FULL_CLEAR_EXTRA_TABLES:
                _safe_delete(conn, tbl)
            conn.execute("DELETE FROM cache_meta WHERE key = 'last_library_sync'")

        await self._write(operation)
