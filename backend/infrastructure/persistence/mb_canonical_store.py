"""MB Localization P1: durable canonical maps (ST2 core).

A SQLite-backed store beneath the in-process memory cache holding three
MusicBrainz-derived mapping families:

- ``canonical_redirect`` - recording merge-redirect resolution (#6). The
  identity lane reads it with ``trusted_identity_source_only=True``; display
  lanes tolerate any capture source.
- ``release_to_rg`` - the release→release-group map behind the six fan-out
  services (#11). ``rg_mbid = ''`` is an authoritative negative, mirroring
  the F-MATCH-05 sentinel discipline; transient failures write nothing.
- ``recording_isrc`` - ISRC → recording mbid pairs from Spotify-import
  /isrc/ lookups (#22).

Provenance: every row stamps ``source_host`` with the privacy-safe source
origin that answered. Per internal-mb-surface.md §5a, persisted MB-derived
mappings inherit MB proof status regardless of serving host - but the
identity-lane gate additionally narrows to rows captured against the official
endpoint, because a user-settable ``api_url`` is not provenance-stamped
upstream.

The store survives ``musicbrainz_prefixes()`` sweeps by design (no prefix-list
entry anywhere): its rows are durable derived state in the shared library DB,
the same category as ``local_album_external_identities``.
"""

from __future__ import annotations

import logging
import sqlite3
import time
from pathlib import Path
from typing import Any

from infrastructure.persistence._database import PersistenceBase
from repositories.musicbrainz_base import (
    MB_TRUSTED_IDENTITY_ORIGINS,
    normalize_mb_id,
    normalize_mb_source_label,
)

logger = logging.getLogger(__name__)

OFFICIAL_MB_API_BASE = "https://musicbrainz.org/ws/2"

_SOURCE_MB_RECORDING_LOOKUP = "mb-recording-lookup"


class MbCanonicalStore(PersistenceBase):
    """Owns tables: ``canonical_redirect``, ``release_to_rg``,
    ``recording_isrc``. Shares the library database file and write lock with
    every other store; never creates a second database file."""

    connection_label = "mb_canonical_store"

    def __init__(self, db_path: Path | str, write_lock) -> None:
        self.db_path = Path(db_path)
        self.db_path.parent.mkdir(parents=True, exist_ok=True)
        self._write_lock = write_lock
        with self._write_lock:
            self._ensure_tables()
            self._seed_from_mbid_resolution_map()

    def _ensure_tables(self) -> None:
        conn = self._connect()
        try:
            conn.execute(
                """
                CREATE TABLE IF NOT EXISTS canonical_redirect (
                    entity_kind TEXT NOT NULL,
                    from_mbid_lower TEXT NOT NULL,
                    to_mbid_lower TEXT NOT NULL,
                    source TEXT NOT NULL,
                    source_host TEXT NOT NULL,
                    first_seen_at REAL NOT NULL,
                    last_confirmed_at REAL NOT NULL,
                    PRIMARY KEY (entity_kind, from_mbid_lower)
                )
                """
            )
            conn.execute(
                """
                CREATE TABLE IF NOT EXISTS release_to_rg (
                    release_mbid_lower TEXT PRIMARY KEY,
                    rg_mbid TEXT NOT NULL DEFAULT '',
                    source TEXT NOT NULL,
                    source_host TEXT NOT NULL,
                    saved_at REAL NOT NULL
                )
                """
            )
            conn.execute(
                """
                CREATE TABLE IF NOT EXISTS recording_isrc (
                    isrc TEXT NOT NULL,
                    recording_mbid_lower TEXT NOT NULL,
                    first_seen_at REAL NOT NULL,
                    PRIMARY KEY (isrc, recording_mbid_lower)
                )
                """
            )
            # Additive ratchets only - later columns join through _safe_alter.
            self._ratchet_source_labels(conn)
            conn.commit()
        finally:
            conn.close()

    @staticmethod
    def _ratchet_source_labels(conn: sqlite3.Connection) -> None:
        """Strip credentials and URL detail from existing provenance rows."""
        for table, key_columns in (
            ("canonical_redirect", ("entity_kind", "from_mbid_lower")),
            ("release_to_rg", ("release_mbid_lower",)),
        ):
            selected_columns = ", ".join((*key_columns, "source_host"))
            rows = conn.execute(
                f"SELECT {selected_columns} FROM {table} WHERE source_host <> ''"
            ).fetchall()
            where_clause = " AND ".join(f"{column} = ?" for column in key_columns)
            for row in rows:
                current = str(row["source_host"])
                normalized = normalize_mb_source_label(current)
                if normalized != current:
                    conn.execute(
                        f"UPDATE {table} SET source_host = ? WHERE {where_clause}",
                        (normalized, *(row[column] for column in key_columns)),
                    )

    def _seed_from_mbid_resolution_map(self) -> None:
        """One-time idempotent migration: bank legacy discover-lane
        release→RG resolutions from ``mbid_resolution_map`` (if that table
        exists yet) into ``release_to_rg``. PK conflict-ignore makes this
        re-runnable at every construction."""
        conn = self._connect()
        try:
            exists = conn.execute(
                "SELECT name FROM sqlite_master WHERE type='table' "
                "AND name='mbid_resolution_map'"
            ).fetchone()
            if exists is None:
                return
            cur = conn.execute(
                """
                INSERT OR IGNORE INTO release_to_rg (
                    release_mbid_lower, rg_mbid, source, source_host, saved_at
                )
                SELECT LOWER(source_mbid),
                       COALESCE(release_group_mbid, ''),
                       'legacy-mbid-resolution-map',
                       '',
                       strftime('%s','now')
                FROM mbid_resolution_map
                WHERE release_group_mbid IS NOT NULL
                  AND source_mbid <> ''
                """
            )
            if cur.rowcount:
                logger.info(
                    "Seeded %s legacy mbid_resolution_map rows into release_to_rg",
                    cur.rowcount,
                )
            conn.commit()
        finally:
            conn.close()

    async def get_release_to_rg_batch(self, release_mbids: list[str]) -> dict[str, str]:
        """Map of lowercased release id -> rg id ('' = known-negative). Only
        ids present in the store appear in the result."""
        normalized = sorted({normalize_mb_id(m) for m in release_mbids if m})
        if not normalized:
            return {}

        def operation(conn: sqlite3.Connection) -> dict[str, str]:
            placeholders = ",".join("?" for _ in normalized)
            rows = conn.execute(
                f"SELECT release_mbid_lower, rg_mbid FROM release_to_rg "
                f"WHERE release_mbid_lower IN ({placeholders})",
                normalized,
            ).fetchall()
            return {str(row["release_mbid_lower"]): str(row["rg_mbid"]) for row in rows}

        return await self._read(operation)

    async def save_release_to_rg(
        self, mapping: dict[str, str], source_host: str
    ) -> None:
        """Batch upsert. Empty-string values are authoritative negatives;
        callers must never pass failures here."""
        source_label = normalize_mb_source_label(source_host)
        rows: dict[str, str] = {}
        for release_mbid, release_group_mbid in mapping.items():
            normalized_release = normalize_mb_id(release_mbid)
            if not normalized_release:
                continue
            rows[normalized_release] = (
                normalize_mb_id(release_group_mbid) if release_group_mbid else ""
            )
        if not rows:
            return
        now = time.time()

        def operation(conn: sqlite3.Connection) -> None:
            conn.executemany(
                """
                INSERT INTO release_to_rg (
                    release_mbid_lower, rg_mbid, source, source_host, saved_at
                )
                VALUES (?, ?, 'mb-release-lookup', ?, ?)
                ON CONFLICT(release_mbid_lower) DO UPDATE SET
                    rg_mbid = excluded.rg_mbid,
                    source = excluded.source,
                    source_host = excluded.source_host,
                    saved_at = excluded.saved_at
                """,
                [(mbid, rg, source_label, now) for mbid, rg in sorted(rows.items())],
            )
            conn.commit()

        await self._write(operation)

    async def get_canonical_redirect(
        self,
        kind: str,
        from_mbids: list[str],
        *,
        trusted_identity_source_only: bool = False,
    ) -> dict[str, str]:
        normalized = sorted({normalize_mb_id(m) for m in from_mbids if m})
        if not normalized:
            return {}

        def operation(conn: sqlite3.Connection) -> dict[str, str]:
            placeholders = ",".join("?" for _ in normalized)
            params: list[Any] = [kind, *normalized]
            sql = (
                "SELECT from_mbid_lower, to_mbid_lower FROM canonical_redirect "
                "WHERE entity_kind = ? "
                f"AND from_mbid_lower IN ({placeholders})"
            )
            if trusted_identity_source_only:
                source_placeholders = ",".join("?" for _ in MB_TRUSTED_IDENTITY_ORIGINS)
                sql += f" AND source_host IN ({source_placeholders})"
                params.extend(MB_TRUSTED_IDENTITY_ORIGINS)
            rows = conn.execute(sql, params).fetchall()
            return {
                str(row["from_mbid_lower"]): str(row["to_mbid_lower"]) for row in rows
            }

        return await self._read(operation)

    async def save_canonical_redirect(
        self, rows: list[dict[str, Any]], source_host: str
    ) -> None:
        """Upsert redirect rows in a single transaction. Each row needs
        ``entity_kind``, ``from_mbid``, ``to_mbid`` and optionally ``source``
        (default ``mb-recording-lookup``)."""
        source_label = normalize_mb_source_label(source_host)
        clean = [
            (
                str(row["entity_kind"]),
                normalize_mb_id(row["from_mbid"]),
                normalize_mb_id(row["to_mbid"]),
                str(row.get("source") or _SOURCE_MB_RECORDING_LOOKUP),
                source_label,
                time.time(),
            )
            for row in rows
            if row.get("from_mbid") and row.get("to_mbid")
        ]
        if not clean:
            return

        def operation(conn: sqlite3.Connection) -> None:
            conn.executemany(
                """
                INSERT INTO canonical_redirect (
                    entity_kind, from_mbid_lower, to_mbid_lower, source,
                    source_host, first_seen_at, last_confirmed_at
                )
                VALUES (?, ?, ?, ?, ?, ?, ?)
                ON CONFLICT(entity_kind, from_mbid_lower) DO UPDATE SET
                    to_mbid_lower = excluded.to_mbid_lower,
                    source = excluded.source,
                    source_host = excluded.source_host,
                    last_confirmed_at = excluded.last_confirmed_at
                """,
                [(k, f, t, s, h, ts, ts) for (k, f, t, s, h, ts) in clean],
            )
            conn.commit()

        await self._write(operation)

    async def get_recordings_by_isrc(self, isrc: str) -> list[str]:
        isrc_normalized = isrc.strip().upper()
        if not isrc_normalized:
            return []

        def operation(conn: sqlite3.Connection) -> list[str]:
            rows = conn.execute(
                "SELECT recording_mbid_lower FROM recording_isrc WHERE isrc = ?",
                (isrc_normalized,),
            ).fetchall()
            return [str(row["recording_mbid_lower"]) for row in rows]

        return await self._read(operation)

    async def save_isrc_recordings(self, pairs: list[tuple[str, str]]) -> None:
        """Bank ``(isrc, recording_mbid)`` pairs in one transaction."""
        clean = [
            (str(isrc).strip().upper(), str(rec).casefold())
            for isrc, rec in pairs
            if isrc and rec
        ]
        if not clean:
            return
        now = time.time()

        def operation(conn: sqlite3.Connection) -> None:
            conn.executemany(
                """
                INSERT OR IGNORE INTO recording_isrc (
                    isrc, recording_mbid_lower, first_seen_at
                ) VALUES (?, ?, ?)
                """,
                [(isrc, rec, now) for isrc, rec in clean],
            )
            conn.commit()

        await self._write(operation)
