import json
import sqlite3
import threading
from datetime import datetime, timezone
from pathlib import Path
from typing import Optional
from uuid import uuid4


_UNSET = object()


class PlaylistRecord:
    __slots__ = (
        "id", "name", "cover_image_path", "created_at", "updated_at",
        "source_ref", "user_id", "is_public",
    )

    def __init__(
        self,
        id: str,
        name: str,
        cover_image_path: Optional[str],
        created_at: str,
        updated_at: str,
        source_ref: Optional[str] = None,
        user_id: Optional[str] = None,
        is_public: bool = False,
    ):
        self.id = id
        self.name = name
        self.cover_image_path = cover_image_path
        self.created_at = created_at
        self.updated_at = updated_at
        self.source_ref = source_ref
        self.user_id = user_id
        self.is_public = is_public


class PlaylistSummaryRecord:
    __slots__ = (
        "id", "name", "cover_image_path", "created_at", "updated_at",
        "track_count", "total_duration", "cover_urls", "source_ref",
        "user_id", "is_public",
    )

    def __init__(
        self,
        id: str,
        name: str,
        cover_image_path: Optional[str],
        created_at: str,
        updated_at: str,
        track_count: int,
        total_duration: Optional[int],
        cover_urls: list[str],
        source_ref: Optional[str] = None,
        user_id: Optional[str] = None,
        is_public: bool = False,
    ):
        self.id = id
        self.name = name
        self.cover_image_path = cover_image_path
        self.created_at = created_at
        self.updated_at = updated_at
        self.track_count = track_count
        self.total_duration = total_duration
        self.cover_urls = cover_urls
        self.source_ref = source_ref
        self.user_id = user_id
        self.is_public = is_public


class PlaylistTrackRecord:
    __slots__ = (
        "id", "playlist_id", "position", "track_name", "artist_name",
        "album_name", "album_id", "artist_id", "track_source_id", "cover_url",
        "source_type", "available_sources", "format", "track_number", "disc_number",
        "duration", "created_at", "plex_rating_key", "library_file_id",
    )

    def __init__(
        self,
        id: str,
        playlist_id: str,
        position: int,
        track_name: str,
        artist_name: str,
        album_name: str,
        album_id: Optional[str],
        artist_id: Optional[str],
        track_source_id: Optional[str],
        cover_url: Optional[str],
        source_type: str,
        available_sources: Optional[list[str]],
        format: Optional[str],
        track_number: Optional[int],
        disc_number: Optional[int],
        duration: Optional[int],
        created_at: str,
        plex_rating_key: Optional[str] = None,
        library_file_id: Optional[str] = None,
    ):
        self.id = id
        self.playlist_id = playlist_id
        self.position = position
        self.track_name = track_name
        self.artist_name = artist_name
        self.album_name = album_name
        self.album_id = album_id
        self.artist_id = artist_id
        self.track_source_id = track_source_id
        self.cover_url = cover_url
        self.source_type = source_type
        self.available_sources = available_sources
        self.format = format
        self.track_number = track_number
        self.disc_number = disc_number
        self.duration = duration
        self.created_at = created_at
        self.plex_rating_key = plex_rating_key
        self.library_file_id = library_file_id

def get_cache_dir() -> Path:
      from core.config import get_settings
      return get_settings().library_db_path

class PlaylistRepository:
    def __init__(self, db_path: Path = get_cache_dir(), write_lock: Optional[threading.Lock] = None):
        self.db_path = db_path
        self._local = threading.local()
        self._write_lock = write_lock or threading.Lock()
        self._ensure_tables()

    def _get_connection(self) -> sqlite3.Connection:
        if not hasattr(self._local, "conn") or self._local.conn is None:
            conn = sqlite3.connect(self.db_path, check_same_thread=False)
            conn.execute("PRAGMA journal_mode=WAL")
            conn.execute("PRAGMA synchronous=NORMAL")
            conn.execute("PRAGMA foreign_keys=ON")
            conn.row_factory = sqlite3.Row
            self._local.conn = conn
        return self._local.conn

    def _ensure_tables(self) -> None:
        conn = sqlite3.connect(self.db_path)
        try:
            conn.execute("PRAGMA journal_mode=WAL")
            conn.execute("PRAGMA foreign_keys=ON")
            try:
                conn.execute(
                    "ALTER TABLE playlist_tracks RENAME COLUMN video_id TO track_source_id"
                )
                conn.commit()
            except sqlite3.OperationalError:
                pass
            conn.execute("""
                CREATE TABLE IF NOT EXISTS playlists (
                    id TEXT PRIMARY KEY,
                    name TEXT NOT NULL,
                    cover_image_path TEXT,
                    created_at TEXT NOT NULL,
                    updated_at TEXT NOT NULL
                )
            """)
            conn.execute("""
                CREATE TABLE IF NOT EXISTS playlist_tracks (
                    id TEXT PRIMARY KEY,
                    playlist_id TEXT NOT NULL REFERENCES playlists(id) ON DELETE CASCADE,
                    position INTEGER NOT NULL,
                    track_name TEXT NOT NULL,
                    artist_name TEXT NOT NULL,
                    album_name TEXT NOT NULL,
                    album_id TEXT,
                    artist_id TEXT,
                    track_source_id TEXT,
                    cover_url TEXT,
                    source_type TEXT NOT NULL,
                    available_sources TEXT,
                    format TEXT,
                    track_number INTEGER,
                    duration INTEGER,
                    created_at TEXT NOT NULL,
                    UNIQUE(playlist_id, position)
                )
            """)
            conn.execute("""
                CREATE INDEX IF NOT EXISTS idx_playlist_tracks_playlist_position
                ON playlist_tracks(playlist_id, position)
            """)
            try:
                conn.execute("ALTER TABLE playlist_tracks ADD COLUMN disc_number INTEGER")
                conn.commit()
            except sqlite3.OperationalError:
                pass
            try:
                conn.execute("ALTER TABLE playlist_tracks ADD COLUMN plex_rating_key TEXT")
                conn.commit()
            except sqlite3.OperationalError:
                pass
            # links a compat entry to its owned library file so the shims can stream it
            # (NULL for legacy outbound entries). Plain column, no FK: playlist dbs can be
            # isolated from library_files, which soft-deletes anyway; the shim resolves a
            # dangling id to None and skips it.
            try:
                conn.execute("ALTER TABLE playlist_tracks ADD COLUMN library_file_id TEXT")
                conn.commit()
            except sqlite3.OperationalError:
                pass
            conn.execute("""
                UPDATE playlist_tracks
                SET cover_url = '/api/v1/covers/' || SUBSTR(cover_url, LENGTH('/api/covers/') + 1)
                WHERE cover_url LIKE '/api/covers/%'
            """)
            try:
                conn.execute("ALTER TABLE playlists ADD COLUMN source_ref TEXT")
                conn.commit()
            except sqlite3.OperationalError:
                pass
            # Per-user ownership + visibility (D4). user_id is nullable in DDL (an
            # additive ALTER cannot be NOT NULL on a populated table); it is backfilled
            # to the first admin and treated NOT NULL by app logic (AMU-1).
            try:
                conn.execute("ALTER TABLE playlists ADD COLUMN user_id TEXT")
                conn.commit()
            except sqlite3.OperationalError:
                pass
            try:
                conn.execute("ALTER TABLE playlists ADD COLUMN is_public INTEGER NOT NULL DEFAULT 0")
                conn.commit()
            except sqlite3.OperationalError:
                pass
            # Replace the GLOBAL source_ref uniqueness with a PER-USER one so two users
            # can each import the same source playlist without colliding (D4). SQLite
            # treats NULL as distinct in a unique index, so pre-backfill (NULL, source_ref)
            # rows never collide.
            conn.execute("DROP INDEX IF EXISTS idx_playlists_source_ref")
            conn.execute("""
                CREATE UNIQUE INDEX IF NOT EXISTS idx_playlists_user_source_ref
                ON playlists(user_id, source_ref) WHERE source_ref IS NOT NULL
            """)
            conn.commit()
        finally:
            conn.close()


    def create_playlist(
        self, name: str, source_ref: Optional[str] = None, user_id: Optional[str] = None,
    ) -> PlaylistRecord:
        playlist_id = str(uuid4())
        now = datetime.now(timezone.utc).isoformat()
        with self._write_lock:
            conn = self._get_connection()
            conn.execute(
                "INSERT INTO playlists (id, name, cover_image_path, created_at, updated_at, source_ref, user_id) "
                "VALUES (?, ?, NULL, ?, ?, ?, ?)",
                (playlist_id, name, now, now, source_ref, user_id),
            )
            conn.commit()
        return PlaylistRecord(
            id=playlist_id, name=name, cover_image_path=None,
            created_at=now, updated_at=now, source_ref=source_ref,
            user_id=user_id, is_public=False,
        )

    def get_playlist(self, playlist_id: str) -> Optional[PlaylistRecord]:
        conn = self._get_connection()
        row = conn.execute(
            "SELECT * FROM playlists WHERE id = ?", (playlist_id,)
        ).fetchone()
        return self._row_to_playlist(row) if row else None

    def get_by_source_ref(
        self, source_ref: str, user_id: Optional[str] = None,
    ) -> Optional[PlaylistRecord]:
        conn = self._get_connection()
        if user_id is None:
            row = conn.execute(
                "SELECT * FROM playlists WHERE source_ref = ?", (source_ref,)
            ).fetchone()
        else:
            row = conn.execute(
                "SELECT * FROM playlists WHERE source_ref = ? AND user_id = ?",
                (source_ref, user_id),
            ).fetchone()
        return self._row_to_playlist(row) if row else None

    def get_imported_source_ids(self, prefix: str, user_id: Optional[str] = None) -> set[str]:
        """Return the set of source IDs imported for a given prefix (e.g. 'plex:').

        Scoped to ``user_id`` when given so the import banner only reflects the
        requesting user's imports (D4).
        """
        conn = self._get_connection()
        if user_id is None:
            rows = conn.execute(
                "SELECT source_ref FROM playlists WHERE source_ref LIKE ?",
                (f"{prefix}%",),
            ).fetchall()
        else:
            rows = conn.execute(
                "SELECT source_ref FROM playlists WHERE source_ref LIKE ? AND user_id = ?",
                (f"{prefix}%", user_id),
            ).fetchall()
        plen = len(prefix)
        return {row["source_ref"][plen:] for row in rows if row["source_ref"]}

    def get_all_playlists(self, user_id: Optional[str] = None) -> list[PlaylistSummaryRecord]:
        conn = self._get_connection()
        base_select = """
            SELECT p.*,
                   COUNT(pt.id) AS track_count,
                   SUM(pt.duration) AS total_duration
            FROM playlists p
            LEFT JOIN playlist_tracks pt ON pt.playlist_id = p.id
        """
        if user_id is None:
            rows = conn.execute(
                base_select + " GROUP BY p.id ORDER BY p.updated_at DESC"
            ).fetchall()
        else:
            rows = conn.execute(
                base_select
                + " WHERE p.user_id = ? OR p.is_public = 1 GROUP BY p.id ORDER BY p.updated_at DESC",
                (user_id,),
            ).fetchall()

        results: list[PlaylistSummaryRecord] = []
        for row in rows:
            cover_urls = [
                r["cover_url"] for r in conn.execute(
                    "SELECT DISTINCT cover_url FROM playlist_tracks "
                    "WHERE playlist_id = ? AND cover_url IS NOT NULL LIMIT 4",
                    (row["id"],),
                ).fetchall()
            ]
            results.append(PlaylistSummaryRecord(
                id=row["id"],
                name=row["name"],
                cover_image_path=row["cover_image_path"],
                created_at=row["created_at"],
                updated_at=row["updated_at"],
                track_count=row["track_count"],
                total_duration=row["total_duration"],
                cover_urls=cover_urls,
                source_ref=row["source_ref"],
                user_id=row["user_id"] if "user_id" in row.keys() else None,
                is_public=bool(row["is_public"]) if "is_public" in row.keys() else False,
            ))
        return results

    def get_summary(self, playlist_id: str) -> Optional[PlaylistSummaryRecord]:
        conn = self._get_connection()
        row = conn.execute(
            """
            SELECT p.*,
                   COUNT(pt.id) AS track_count,
                   SUM(pt.duration) AS total_duration
            FROM playlists p
            LEFT JOIN playlist_tracks pt ON pt.playlist_id = p.id
            WHERE p.id = ?
            GROUP BY p.id
            """,
            (playlist_id,),
        ).fetchone()
        if row is None:
            return None
        cover_urls = [
            r["cover_url"] for r in conn.execute(
                "SELECT DISTINCT cover_url FROM playlist_tracks "
                "WHERE playlist_id = ? AND cover_url IS NOT NULL LIMIT 4",
                (playlist_id,),
            ).fetchall()
        ]
        return PlaylistSummaryRecord(
            id=row["id"],
            name=row["name"],
            cover_image_path=row["cover_image_path"],
            created_at=row["created_at"],
            updated_at=row["updated_at"],
            track_count=row["track_count"],
            total_duration=row["total_duration"],
            cover_urls=cover_urls,
            source_ref=row["source_ref"] if "source_ref" in row.keys() else None,
            user_id=row["user_id"] if "user_id" in row.keys() else None,
            is_public=bool(row["is_public"]) if "is_public" in row.keys() else False,
        )

    def update_playlist(
        self,
        playlist_id: str,
        name: Optional[str] = None,
        cover_image_path: Optional[str] = _UNSET,
    ) -> Optional[PlaylistRecord]:
        with self._write_lock:
            conn = self._get_connection()
            existing = conn.execute(
                "SELECT * FROM playlists WHERE id = ?", (playlist_id,)
            ).fetchone()
            if not existing:
                return None

            new_name = name if name is not None else existing["name"]
            new_cover = (
                cover_image_path
                if cover_image_path is not _UNSET
                else existing["cover_image_path"]
            )
            now = datetime.now(timezone.utc).isoformat()

            conn.execute(
                "UPDATE playlists SET name = ?, cover_image_path = ?, updated_at = ? WHERE id = ?",
                (new_name, new_cover, now, playlist_id),
            )
            conn.commit()
        return PlaylistRecord(
            id=playlist_id, name=new_name, cover_image_path=new_cover,
            created_at=existing["created_at"], updated_at=now,
            source_ref=existing["source_ref"] if "source_ref" in existing.keys() else None,
            user_id=existing["user_id"] if "user_id" in existing.keys() else None,
            is_public=bool(existing["is_public"]) if "is_public" in existing.keys() else False,
        )

    def delete_playlist(self, playlist_id: str) -> bool:
        with self._write_lock:
            conn = self._get_connection()
            cursor = conn.execute(
                "DELETE FROM playlists WHERE id = ?", (playlist_id,)
            )
            conn.commit()
            return cursor.rowcount > 0

    def set_public(self, playlist_id: str, is_public: bool) -> Optional[PlaylistRecord]:
        with self._write_lock:
            conn = self._get_connection()
            existing = conn.execute(
                "SELECT * FROM playlists WHERE id = ?", (playlist_id,)
            ).fetchone()
            if not existing:
                return None
            now = datetime.now(timezone.utc).isoformat()
            conn.execute(
                "UPDATE playlists SET is_public = ?, updated_at = ? WHERE id = ?",
                (1 if is_public else 0, now, playlist_id),
            )
            conn.commit()
            updated = conn.execute(
                "SELECT * FROM playlists WHERE id = ?", (playlist_id,)
            ).fetchone()
        return self._row_to_playlist(updated)

    def assign_unowned_to(self, user_id: str) -> int:
        """Backfill ownerless playlists to ``user_id`` (the first admin, D7). Idempotent."""
        with self._write_lock:
            conn = self._get_connection()
            cursor = conn.execute(
                "UPDATE playlists SET user_id = ? WHERE user_id IS NULL",
                (user_id,),
            )
            conn.commit()
            return cursor.rowcount

    def add_tracks(
        self,
        playlist_id: str,
        tracks: list[dict],
        position: Optional[int] = None,
    ) -> list[PlaylistTrackRecord]:
        if not tracks:
            return []

        now = datetime.now(timezone.utc).isoformat()
        created_records: list[PlaylistTrackRecord] = []

        with self._write_lock:
            conn = self._get_connection()

            max_row = conn.execute(
                "SELECT MAX(position) FROM playlist_tracks WHERE playlist_id = ?",
                (playlist_id,),
            ).fetchone()
            current_max = max_row[0] if max_row[0] is not None else -1

            if position is None or position > current_max + 1:
                insert_at = current_max + 1
            else:
                insert_at = max(0, position)
                shift_count = len(tracks)
                rows_to_shift = conn.execute(
                    "SELECT id, position FROM playlist_tracks "
                    "WHERE playlist_id = ? AND position >= ? "
                    "ORDER BY position DESC",
                    (playlist_id, insert_at),
                ).fetchall()
                for idx, r in enumerate(rows_to_shift):
                    conn.execute(
                        "UPDATE playlist_tracks SET position = ? WHERE id = ?",
                        (-(idx + 1), r["id"]),
                    )
                for r in rows_to_shift:
                    conn.execute(
                        "UPDATE playlist_tracks SET position = ? WHERE id = ?",
                        (r["position"] + shift_count, r["id"]),
                    )

            for i, track in enumerate(tracks):
                track_id = str(uuid4())
                pos = insert_at + i
                available_sources_json = (
                    json.dumps(track["available_sources"])
                    if track.get("available_sources") else None
                )
                conn.execute(
                    "INSERT INTO playlist_tracks "
                    "(id, playlist_id, position, track_name, artist_name, album_name, "
                    "album_id, artist_id, track_source_id, cover_url, source_type, "
                    "available_sources, format, track_number, disc_number, duration, "
                    "plex_rating_key, library_file_id, created_at) "
                    "VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
                    (
                        track_id, playlist_id, pos,
                        track["track_name"], track["artist_name"], track["album_name"],
                        track.get("album_id"), track.get("artist_id"),
                        track.get("track_source_id"), track.get("cover_url"),
                        track["source_type"], available_sources_json,
                        track.get("format"), track.get("track_number"), track.get("disc_number"),
                        track.get("duration"), track.get("plex_rating_key"),
                        track.get("library_file_id"), now,
                    ),
                )
                created_records.append(PlaylistTrackRecord(
                    id=track_id, playlist_id=playlist_id, position=pos,
                    track_name=track["track_name"], artist_name=track["artist_name"],
                    album_name=track["album_name"], album_id=track.get("album_id"),
                    artist_id=track.get("artist_id"), track_source_id=track.get("track_source_id"),
                    cover_url=track.get("cover_url"), source_type=track["source_type"],
                    available_sources=track.get("available_sources"),
                    format=track.get("format"), track_number=track.get("track_number"),
                    disc_number=track.get("disc_number"),
                    duration=track.get("duration"), created_at=now,
                    plex_rating_key=track.get("plex_rating_key"),
                    library_file_id=track.get("library_file_id"),
                ))

            conn.execute(
                "UPDATE playlists SET updated_at = ? WHERE id = ?",
                (now, playlist_id),
            )
            conn.commit()

        return created_records

    def remove_track(self, playlist_id: str, track_id: str) -> bool:
        with self._write_lock:
            conn = self._get_connection()
            row = conn.execute(
                "SELECT position FROM playlist_tracks WHERE id = ? AND playlist_id = ?",
                (track_id, playlist_id),
            ).fetchone()
            if not row:
                return False

            removed_pos = row["position"]
            conn.execute(
                "DELETE FROM playlist_tracks WHERE id = ? AND playlist_id = ?",
                (track_id, playlist_id),
            )
            rows_to_shift = conn.execute(
                "SELECT id, position FROM playlist_tracks "
                "WHERE playlist_id = ? AND position > ? "
                "ORDER BY position ASC",
                (playlist_id, removed_pos),
            ).fetchall()
            for r in rows_to_shift:
                conn.execute(
                    "UPDATE playlist_tracks SET position = ? WHERE id = ?",
                    (r["position"] - 1, r["id"]),
                )
            now = datetime.now(timezone.utc).isoformat()
            conn.execute(
                "UPDATE playlists SET updated_at = ? WHERE id = ?",
                (now, playlist_id),
            )
            conn.commit()
            return True

    def remove_tracks(self, playlist_id: str, track_ids: list[str]) -> int:
        if not track_ids:
            return 0
        with self._write_lock:
            conn = self._get_connection()
            placeholders = ",".join("?" for _ in track_ids)
            existing = conn.execute(
                f"SELECT id FROM playlist_tracks WHERE playlist_id = ? AND id IN ({placeholders})",
                [playlist_id, *track_ids],
            ).fetchall()
            if not existing:
                return 0
            ids_to_remove = [r["id"] for r in existing]
            rm_placeholders = ",".join("?" for _ in ids_to_remove)
            conn.execute(
                f"DELETE FROM playlist_tracks WHERE playlist_id = ? AND id IN ({rm_placeholders})",
                [playlist_id, *ids_to_remove],
            )
            remaining = conn.execute(
                "SELECT id FROM playlist_tracks WHERE playlist_id = ? ORDER BY position ASC",
                (playlist_id,),
            ).fetchall()
            for new_pos, row in enumerate(remaining):
                conn.execute(
                    "UPDATE playlist_tracks SET position = ? WHERE id = ?",
                    (new_pos, row["id"]),
                )
            now = datetime.now(timezone.utc).isoformat()
            conn.execute(
                "UPDATE playlists SET updated_at = ? WHERE id = ?",
                (now, playlist_id),
            )
            conn.commit()
            return len(ids_to_remove)

    def reorder_track(self, playlist_id: str, track_id: str, new_position: int) -> Optional[int]:
        with self._write_lock:
            conn = self._get_connection()
            row = conn.execute(
                "SELECT position FROM playlist_tracks WHERE id = ? AND playlist_id = ?",
                (track_id, playlist_id),
            ).fetchone()
            if not row:
                return None

            old_position = row["position"]

            max_row = conn.execute(
                "SELECT MAX(position) FROM playlist_tracks WHERE playlist_id = ?",
                (playlist_id,),
            ).fetchone()
            max_pos = max_row[0] if max_row[0] is not None else 0
            actual_position = min(new_position, max_pos)

            if old_position == actual_position:
                return actual_position

            conn.execute(
                "UPDATE playlist_tracks SET position = -1 WHERE id = ?",
                (track_id,),
            )

            if old_position < actual_position:
                rows = conn.execute(
                    "SELECT id, position FROM playlist_tracks "
                    "WHERE playlist_id = ? AND position > ? AND position <= ? "
                    "ORDER BY position ASC",
                    (playlist_id, old_position, actual_position),
                ).fetchall()
                for r in rows:
                    conn.execute(
                        "UPDATE playlist_tracks SET position = ? WHERE id = ?",
                        (r["position"] - 1, r["id"]),
                    )
            else:
                rows = conn.execute(
                    "SELECT id, position FROM playlist_tracks "
                    "WHERE playlist_id = ? AND position >= ? AND position < ? "
                    "ORDER BY position DESC",
                    (playlist_id, actual_position, old_position),
                ).fetchall()
                for r in rows:
                    conn.execute(
                        "UPDATE playlist_tracks SET position = ? WHERE id = ?",
                        (r["position"] + 1, r["id"]),
                    )

            conn.execute(
                "UPDATE playlist_tracks SET position = ? WHERE id = ?",
                (actual_position, track_id),
            )

            now = datetime.now(timezone.utc).isoformat()
            conn.execute(
                "UPDATE playlists SET updated_at = ? WHERE id = ?",
                (now, playlist_id),
            )
            conn.commit()
            return actual_position

    def batch_update_available_sources(
        self,
        playlist_id: str,
        updates: dict[str, list[str]],
    ) -> int:
        if not updates:
            return 0
        updated = 0
        with self._write_lock:
            conn = self._get_connection()
            for track_id, sources in updates.items():
                conn.execute(
                    "UPDATE playlist_tracks SET available_sources = ? "
                    "WHERE id = ? AND playlist_id = ?",
                    (json.dumps(sources), track_id, playlist_id),
                )
                updated += 1
            if updated:
                now = datetime.now(timezone.utc).isoformat()
                conn.execute(
                    "UPDATE playlists SET updated_at = ? WHERE id = ?",
                    (now, playlist_id),
                )
                conn.commit()
        return updated

    def update_track_source(
        self,
        playlist_id: str,
        track_id: str,
        source_type: Optional[str] = None,
        available_sources: Optional[list[str]] = None,
        track_source_id: Optional[str] = None,
        plex_rating_key: Optional[str] = _UNSET,
        library_file_id: Optional[str] = _UNSET,
    ) -> Optional[PlaylistTrackRecord]:
        with self._write_lock:
            conn = self._get_connection()
            row = conn.execute(
                "SELECT * FROM playlist_tracks WHERE id = ? AND playlist_id = ?",
                (track_id, playlist_id),
            ).fetchone()
            if not row:
                return None

            new_source_type = source_type if source_type is not None else row["source_type"]
            new_available = (
                json.dumps(available_sources)
                if available_sources is not None
                else row["available_sources"]
            )
            new_track_source_id = track_source_id if track_source_id is not None else row["track_source_id"]
            new_plex_rating_key = (
                plex_rating_key
                if plex_rating_key is not _UNSET
                else (row["plex_rating_key"] if "plex_rating_key" in row.keys() else None)
            )
            new_library_file_id = (
                library_file_id
                if library_file_id is not _UNSET
                else (row["library_file_id"] if "library_file_id" in row.keys() else None)
            )

            conn.execute(
                "UPDATE playlist_tracks SET source_type = ?, available_sources = ?, "
                "track_source_id = ?, plex_rating_key = ?, library_file_id = ? "
                "WHERE id = ? AND playlist_id = ?",
                (new_source_type, new_available, new_track_source_id, new_plex_rating_key,
                 new_library_file_id, track_id, playlist_id),
            )
            now = datetime.now(timezone.utc).isoformat()
            conn.execute(
                "UPDATE playlists SET updated_at = ? WHERE id = ?",
                (now, playlist_id),
            )
            conn.commit()

        avail_parsed = (
            available_sources
            if available_sources is not None
            else self._parse_available_sources(row["available_sources"])
        )
        return PlaylistTrackRecord(
            id=row["id"], playlist_id=row["playlist_id"], position=row["position"],
            track_name=row["track_name"], artist_name=row["artist_name"],
            album_name=row["album_name"], album_id=row["album_id"],
            artist_id=row["artist_id"], track_source_id=new_track_source_id,
            cover_url=row["cover_url"], source_type=new_source_type,
            available_sources=avail_parsed, format=row["format"],
            track_number=row["track_number"],
            disc_number=row["disc_number"] if "disc_number" in row.keys() else None,
            duration=row["duration"],
            created_at=row["created_at"],
            plex_rating_key=new_plex_rating_key,
            library_file_id=new_library_file_id,
        )

    def get_tracks(self, playlist_id: str) -> list[PlaylistTrackRecord]:
        conn = self._get_connection()
        rows = conn.execute(
            "SELECT * FROM playlist_tracks WHERE playlist_id = ? ORDER BY position",
            (playlist_id,),
        ).fetchall()
        return [self._row_to_track(r) for r in rows]

    def get_track(self, playlist_id: str, track_id: str) -> Optional[PlaylistTrackRecord]:
        conn = self._get_connection()
        row = conn.execute(
            "SELECT * FROM playlist_tracks WHERE id = ? AND playlist_id = ?",
            (track_id, playlist_id),
        ).fetchone()
        if not row:
            return None
        return self._row_to_track(row)

    def check_track_membership(
        self, tracks: list[tuple[str, str, str]], user_id: Optional[str] = None,
    ) -> dict[str, list[int]]:
        """Check which playlists already contain the given tracks.

        Args:
            tracks: list of (track_name, artist_name, album_name) tuples.
            user_id: when given, only the caller's own playlists are considered, so
                membership never leaks another user's playlist IDs (D4).

        Returns:
            dict mapping playlist_id to list of input indices that are already present.
        """
        if not tracks:
            return {}

        conn = self._get_connection()
        if user_id is None:
            rows = conn.execute(
                "SELECT playlist_id, LOWER(track_name) AS tn, "
                "LOWER(artist_name) AS an, LOWER(album_name) AS aln "
                "FROM playlist_tracks"
            ).fetchall()
        else:
            rows = conn.execute(
                "SELECT pt.playlist_id, LOWER(pt.track_name) AS tn, "
                "LOWER(pt.artist_name) AS an, LOWER(pt.album_name) AS aln "
                "FROM playlist_tracks pt JOIN playlists p ON p.id = pt.playlist_id "
                "WHERE p.user_id = ?",
                (user_id,),
            ).fetchall()

        lookup: dict[str, set[tuple[str, str, str]]] = {}
        for row in rows:
            pid = row["playlist_id"]
            key = (row["tn"], row["an"], row["aln"])
            lookup.setdefault(pid, set()).add(key)

        result: dict[str, list[int]] = {}
        normalised = [
            (t[0].lower(), t[1].lower(), t[2].lower()) for t in tracks
        ]
        for pid, existing in lookup.items():
            matched = [i for i, t in enumerate(normalised) if t in existing]
            if matched:
                result[pid] = matched
        return result


    def close(self) -> None:
        conn = getattr(self._local, "conn", None)
        if conn is not None:
            try:
                conn.close()
            except sqlite3.Error:
                pass
            self._local.conn = None


    @staticmethod
    def _parse_available_sources(raw: Optional[str]) -> Optional[list[str]]:
        if raw is None:
            return None
        try:
            return json.loads(raw)
        except (json.JSONDecodeError, TypeError):
            return None

    @staticmethod
    def _row_to_playlist(row: sqlite3.Row) -> PlaylistRecord:
        return PlaylistRecord(
            id=row["id"],
            name=row["name"],
            cover_image_path=row["cover_image_path"],
            created_at=row["created_at"],
            updated_at=row["updated_at"],
            source_ref=row["source_ref"] if "source_ref" in row.keys() else None,
            user_id=row["user_id"] if "user_id" in row.keys() else None,
            is_public=bool(row["is_public"]) if "is_public" in row.keys() else False,
        )

    @classmethod
    def _row_to_track(cls, row: sqlite3.Row) -> PlaylistTrackRecord:
        return PlaylistTrackRecord(
            id=row["id"],
            playlist_id=row["playlist_id"],
            position=row["position"],
            track_name=row["track_name"],
            artist_name=row["artist_name"],
            album_name=row["album_name"],
            album_id=row["album_id"],
            artist_id=row["artist_id"],
            track_source_id=row["track_source_id"],
            cover_url=row["cover_url"],
            source_type=row["source_type"],
            available_sources=cls._parse_available_sources(row["available_sources"]),
            format=row["format"],
            track_number=row["track_number"],
            disc_number=row["disc_number"] if "disc_number" in row.keys() else None,
            duration=row["duration"],
            created_at=row["created_at"],
            plex_rating_key=row["plex_rating_key"] if "plex_rating_key" in row.keys() else None,
            library_file_id=row["library_file_id"] if "library_file_id" in row.keys() else None,
        )
