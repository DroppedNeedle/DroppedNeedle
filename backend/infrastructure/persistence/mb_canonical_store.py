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

Provenance: every row stamps opaque ``source_mode``, ``source_id``, and
``source_generation`` fields. The legacy ``source_host`` columns remain only
for SQLite compatibility and are always blank after migration. Canonical
redirect rows retain a derived ``official_evidence`` bit for the identity-lane
gate; endpoint labels are never persisted.

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

from core.exceptions import ConfigurationError
from infrastructure.persistence._database import PersistenceBase, _safe_alter
from repositories.musicbrainz_base import (
    MB_TRUSTED_IDENTITY_ORIGINS,
    MbSourceContext,
    mb_publish_if_current,
    normalize_mb_id,
    normalize_mb_source_label,
)

logger = logging.getLogger(__name__)


_SOURCE_MB_RECORDING_LOOKUP = "mb-recording-lookup"


def _provenance(
    source_context: MbSourceContext | None,
    legacy_source: str | None = None,
) -> tuple[str, str, int]:
    if source_context is not None:
        return (
            source_context.source_mode,
            source_context.source_id,
            source_context.generation,
        )
    if legacy_source:
        normalized = normalize_mb_source_label(legacy_source)
        if normalized in MB_TRUSTED_IDENTITY_ORIGINS:
            return ("official", "", 0)
        return ("legacy", "", 0)
    return ("", "", 0)


def _official_evidence(
    source_context: MbSourceContext | None,
    legacy_source: str | None = None,
) -> int:
    if source_context is not None:
        return int(
            source_context.source_mode == "official"
            and normalize_mb_source_label(source_context.source_url)
            in MB_TRUSTED_IDENTITY_ORIGINS
        )
    return int(normalize_mb_source_label(legacy_source) in MB_TRUSTED_IDENTITY_ORIGINS)


def _require_source_context(source_context: MbSourceContext | None) -> MbSourceContext:
    if source_context is None:
        raise ConfigurationError("MusicBrainz source context is required")
    return source_context


def _legacy_source_host(
    source_context: MbSourceContext | None,
    source_host: str | None,
) -> str:
    """Keep the compatibility column empty; provenance is opaque only."""
    return ""


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
                    official_evidence INTEGER NOT NULL DEFAULT 0,
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
            # Opaque source provenance is additive so existing SQLite files
            # remain readable. ``source_host`` is retained only as a legacy
            # column and is blanked by the ratchet below.
            for table in ("canonical_redirect", "release_to_rg", "recording_isrc"):
                _safe_alter(
                    conn,
                    f"ALTER TABLE {table} ADD COLUMN source_mode TEXT NOT NULL DEFAULT ''",
                )
                _safe_alter(
                    conn,
                    f"ALTER TABLE {table} ADD COLUMN source_id TEXT NOT NULL DEFAULT ''",
                )
                _safe_alter(
                    conn,
                    f"ALTER TABLE {table} ADD COLUMN source_generation INTEGER NOT NULL DEFAULT 0",
                )
            _safe_alter(
                conn,
                "ALTER TABLE canonical_redirect ADD COLUMN official_evidence INTEGER NOT NULL DEFAULT 0",
            )
            _safe_alter(
                conn,
                "ALTER TABLE release_to_rg ADD COLUMN official_evidence INTEGER NOT NULL DEFAULT 0",
            )
            self._ratchet_source_labels(conn)
            conn.commit()
        finally:
            conn.close()

    @staticmethod
    def _ratchet_source_labels(conn: sqlite3.Connection) -> None:
        """Derive official evidence once, then clear legacy endpoint labels."""
        rows = conn.execute(
            "SELECT entity_kind, from_mbid_lower, source_host "
            "FROM canonical_redirect WHERE source_host <> ''"
        ).fetchall()
        for row in rows:
            normalized = normalize_mb_source_label(str(row["source_host"]))
            conn.execute(
                """
                UPDATE canonical_redirect
                SET source_host = '', official_evidence = ?
                WHERE entity_kind = ? AND from_mbid_lower = ?
                """,
                (
                    int(normalized in MB_TRUSTED_IDENTITY_ORIGINS),
                    row["entity_kind"],
                    row["from_mbid_lower"],
                ),
            )
        release_rows = conn.execute(
            "SELECT release_mbid_lower, source_host FROM release_to_rg "
            "WHERE source_host <> ''"
        ).fetchall()
        for row in release_rows:
            normalized = normalize_mb_source_label(str(row["source_host"]))
            conn.execute(
                "UPDATE release_to_rg SET source_host = '', official_evidence = ? "
                "WHERE release_mbid_lower = ?",
                (
                    int(normalized in MB_TRUSTED_IDENTITY_ORIGINS),
                    row["release_mbid_lower"],
                ),
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

    async def get_release_to_rg_batch(
        self,
        release_mbids: list[str],
        *,
        source_context: MbSourceContext,
    ) -> dict[str, str]:
        """Return mappings for the captured source context only."""
        source_context = _require_source_context(source_context)
        normalized = sorted({normalize_mb_id(m) for m in release_mbids if m})
        if not normalized:
            return {}

        def operation(conn: sqlite3.Connection) -> dict[str, str]:
            placeholders = ",".join("?" for _ in normalized)
            params: list[Any] = [
                *normalized,
                source_context.source_mode,
                source_context.source_id,
                source_context.generation,
            ]
            sql = (
                "SELECT release_mbid_lower, rg_mbid FROM release_to_rg "
                f"WHERE release_mbid_lower IN ({placeholders}) "
                "AND source_mode = ? AND source_id = ? AND source_generation = ?"
            )
            rows = conn.execute(sql, params).fetchall()
            return {str(row["release_mbid_lower"]): str(row["rg_mbid"]) for row in rows}

        return await self._read(operation)

    async def save_release_to_rg(
        self,
        mapping: dict[str, str],
        source_host: str | None = None,
        *,
        source_context: MbSourceContext | None = None,
    ) -> None:
        """Batch upsert with opaque source provenance."""
        source_mode, source_id, source_generation = _provenance(
            source_context, source_host
        )
        persisted_source_host = _legacy_source_host(source_context, source_host)
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
                    release_mbid_lower, rg_mbid, source, source_host,
                    official_evidence, source_mode, source_id, source_generation, saved_at
                )
                VALUES (?, ?, 'mb-release-lookup', ?, ?, ?, ?, ?, ?)
                ON CONFLICT(release_mbid_lower) DO UPDATE SET
                    rg_mbid = excluded.rg_mbid,
                    source = excluded.source,
                    source_host = excluded.source_host,
                    official_evidence = excluded.official_evidence,
                    source_mode = excluded.source_mode,
                    source_id = excluded.source_id,
                    source_generation = excluded.source_generation,
                    saved_at = excluded.saved_at
                """,
                [
                    (
                        mbid,
                        rg,
                        persisted_source_host,
                        _official_evidence(source_context, source_host),
                        source_mode,
                        source_id,
                        source_generation,
                        now,
                    )
                    for mbid, rg in sorted(rows.items())
                ],
            )
            conn.commit()

        await mb_publish_if_current(source_context, lambda: self._write(operation))

    async def get_canonical_redirect(
        self,
        kind: str,
        from_mbids: list[str],
        *,
        source_context: MbSourceContext,
        trusted_identity_source_only: bool = False,
    ) -> dict[str, str]:
        source_context = _require_source_context(source_context)
        normalized = sorted({normalize_mb_id(m) for m in from_mbids if m})
        if not normalized:
            return {}

        def operation(conn: sqlite3.Connection) -> dict[str, str]:
            placeholders = ",".join("?" for _ in normalized)
            params: list[Any] = [
                kind,
                *normalized,
                source_context.source_mode,
                source_context.source_id,
                source_context.generation,
            ]
            sql = (
                "SELECT from_mbid_lower, to_mbid_lower FROM canonical_redirect "
                "WHERE entity_kind = ? "
                f"AND from_mbid_lower IN ({placeholders}) "
                "AND source_mode = ? AND source_id = ? "
                "AND source_generation = ?"
            )
            if trusted_identity_source_only:
                sql += " AND official_evidence = 1"
            rows = conn.execute(sql, params).fetchall()
            return {
                str(row["from_mbid_lower"]): str(row["to_mbid_lower"]) for row in rows
            }

        return await self._read(operation)

    async def save_canonical_redirect(
        self,
        rows: list[dict[str, Any]],
        source_host: str | None = None,
        *,
        source_context: MbSourceContext | None = None,
    ) -> None:
        """Upsert redirect rows with opaque provenance."""
        source_mode, source_id, source_generation = _provenance(
            source_context, source_host
        )
        persisted_source_host = _legacy_source_host(source_context, source_host)
        clean = [
            (
                str(row["entity_kind"]),
                normalize_mb_id(row["from_mbid"]),
                normalize_mb_id(row["to_mbid"]),
                str(row.get("source") or _SOURCE_MB_RECORDING_LOOKUP),
                persisted_source_host,
                source_mode,
                source_id,
                source_generation,
                _official_evidence(source_context, source_host),
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
                    source_host, source_mode, source_id, source_generation,
                    official_evidence, first_seen_at, last_confirmed_at
                )
                VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
                ON CONFLICT(entity_kind, from_mbid_lower) DO UPDATE SET
                    to_mbid_lower = excluded.to_mbid_lower,
                    source = excluded.source,
                    source_host = excluded.source_host,
                    source_mode = excluded.source_mode,
                    source_id = excluded.source_id,
                    source_generation = excluded.source_generation,
                    official_evidence = excluded.official_evidence,
                    last_confirmed_at = excluded.last_confirmed_at
                WHERE canonical_redirect.official_evidence = 0
                   OR excluded.official_evidence = 1
                """,
                [
                    (k, f, t, s, host, mode, sid, generation, official, ts, ts)
                    for (
                        k,
                        f,
                        t,
                        s,
                        host,
                        mode,
                        sid,
                        generation,
                        official,
                        ts,
                    ) in clean
                ],
            )
            conn.commit()

        await mb_publish_if_current(source_context, lambda: self._write(operation))

    async def get_recordings_by_isrc(
        self,
        isrc: str,
        *,
        source_context: MbSourceContext,
    ) -> list[str]:
        source_context = _require_source_context(source_context)
        isrc_normalized = isrc.strip().upper()
        if not isrc_normalized:
            return []

        def operation(conn: sqlite3.Connection) -> list[str]:
            sql = (
                "SELECT recording_mbid_lower FROM recording_isrc "
                "WHERE isrc = ? AND source_mode = ? AND source_id = ? "
                "AND source_generation = ?"
            )
            params: list[Any] = [
                isrc_normalized,
                source_context.source_mode,
                source_context.source_id,
                source_context.generation,
            ]
            rows = conn.execute(sql, params).fetchall()
            return [str(row["recording_mbid_lower"]) for row in rows]

        return await self._read(operation)

    async def save_isrc_recordings(
        self,
        pairs: list[tuple[str, str]],
        *,
        source_context: MbSourceContext | None = None,
    ) -> None:
        """Bank ``(isrc, recording_mbid)`` pairs with opaque provenance."""
        source_mode, source_id, source_generation = _provenance(source_context)
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
                INSERT INTO recording_isrc (
                    isrc, recording_mbid_lower, source_mode, source_id,
                    source_generation, first_seen_at
                ) VALUES (?, ?, ?, ?, ?, ?)
                ON CONFLICT(isrc, recording_mbid_lower) DO UPDATE SET
                    source_mode = excluded.source_mode,
                    source_id = excluded.source_id,
                    source_generation = excluded.source_generation
                """,
                [
                    (isrc, rec, source_mode, source_id, source_generation, now)
                    for isrc, rec in clean
                ],
            )
            conn.commit()

        await mb_publish_if_current(source_context, lambda: self._write(operation))
