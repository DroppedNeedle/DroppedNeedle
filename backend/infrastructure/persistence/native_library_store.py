"""The sole transaction owner for the inactive target native-library schema."""

from __future__ import annotations

import asyncio
import json
import hashlib
import logging
import sqlite3
import unicodedata
import uuid
from collections.abc import Awaitable, Callable
from pathlib import Path
import threading
import time
from typing import Any, TypeVar

import msgspec

from core.exceptions import (
    ConflictError,
    ResourceNotFoundError,
    RevisionOverflowError,
    StaleRevisionError,
    ValidationError,
)
from infrastructure.persistence._database import PersistenceBase
from infrastructure.persistence.native_library_schema import SCHEMA_SQL
from models.identification import (
    CandidateEvidence,
    FingerprintOutcome,
    GroupingApplication,
    IdentificationAttempt,
    IdentificationEvidenceRecord,
)
from models.library_migration import (
    LegacyCatalogImportBundle,
    MigrationReview,
    MigrationTombstone,
)
from models.library_work import (
    IdentificationJob,
    MigrationProvenance,
    OperationJob,
    OperationWorkItem,
    RepairFinding,
    ReviewDecision,
    ScanInventoryItem,
    ScanRequest,
    ScanRequestResult,
    ScanRun,
    ScanScope,
    ScannedTrackWrite,
)
from models.local_catalog import (
    CatalogMembership,
    LocalAlbum,
    LocalAlbumAlias,
    LocalAlbumExternalIdentity,
    LocalArtist,
    LocalArtistAlias,
    LocalArtistCredit,
    LocalArtistExternalIdentity,
    LocalArtworkAssociation,
    LocalTrack,
    LocalTrackExternalIdentity,
)

MAX_REVISION = 9_223_372_036_854_775_807
VARIOUS_ARTISTS_ID = "00000000-0000-4000-8000-000000000001"
UNKNOWN_ARTIST_ID = "00000000-0000-4000-8000-000000000002"
AUTOMATIC_SAFE_EVIDENCE_REASONS = frozenset(
    {"SUPPORTED", "ACCEPTED", "SUPPORTED_EMBEDDED_IDS"}
)
BULK_PREVIEW_BATCH_SIZE = 500
BULK_PREVIEW_CLEANUP_BATCH_SIZE = 5_000
_T = TypeVar("_T")
logger = logging.getLogger(__name__)

_TARGET_TRACK_SELECT = """
SELECT
    t.id,
    t.root_id,
    t.relative_path,
    t.file_path,
    t.file_size_bytes,
    t.imported_at,
    t.stat_revision,
    t.tag_revision,
    t.applied_policy_revision,
    t.applied_policy,
    t.title AS track_title,
    t.artist_name,
    t.album_title,
    t.disc_number,
    t.track_number,
    t.year,
    t.genre,
    t.duration_seconds,
    t.file_format,
    t.bit_rate,
    t.sample_rate,
    t.bit_depth,
    t.channels,
    t.title_sort AS track_sort_name,
    t.artist_sort AS artist_sort_name,
    t.album_sort AS album_sort_name,
    t.album_artist_sort AS album_artist_sort_name,
    t.disc_subtitle,
    t.replaygain_track_gain,
    t.replaygain_album_gain,
    t.replaygain_track_peak,
    t.replaygain_album_peak,
    t.availability,
    t.ingest_source,
    t.download_task_id,
    t.source_path,
    a.id AS release_group_mbid,
    a.title AS canonical_album_title,
    a.album_artist_name,
    a.album_artist_id AS album_artist_mbid,
    a.album_artist_sort_name,
    a.original_release_date,
    a.is_compilation,
    COALESCE(ta.local_artist_id, a.album_artist_id) AS artist_mbid,
    te.recording_mbid,
    ae.release_group_mbid AS provider_release_group_mbid,
    ae.release_mbid AS provider_release_mbid,
    aae.provider_artist_id AS provider_album_artist_mbid,
    tae.provider_artist_id AS provider_artist_mbid,
    artwork.cover_url,
    artwork.source AS artwork_source
FROM local_tracks t
JOIN local_albums a ON a.id = t.local_album_id
LEFT JOIN local_track_artists ta
    ON ta.local_track_id = t.id AND ta.position = 0
LEFT JOIN local_track_external_identities te
    ON te.local_track_id = t.id AND te.provider = 'musicbrainz'
LEFT JOIN local_album_external_identities ae
    ON ae.local_album_id = a.id AND ae.provider = 'musicbrainz'
LEFT JOIN local_artist_external_identities aae
    ON aae.local_artist_id = a.album_artist_id AND aae.provider = 'musicbrainz'
LEFT JOIN local_artist_external_identities tae
    ON tae.local_artist_id = COALESCE(ta.local_artist_id, a.album_artist_id)
    AND tae.provider = 'musicbrainz'
LEFT JOIN local_album_artwork artwork ON artwork.local_album_id = a.id
"""

_LEGACY_MIGRATION_TABLE_ORDER = {
    "auth_users": "id",
    "library_files": "id",
    "manual_review_queue": "id",
    "library_albums": "mbid_lower",
    "library_artists": "mbid_lower",
    "library_album_meta": "release_group_mbid",
    "user_favorites": "user_id, item_kind, item_id",
    "play_history": "user_id, played_at, id",
    "playlists": "id",
    "playlist_tracks": "playlist_id, position, id",
    "album_release_pins": "release_group_mbid",
    "compat_bookmarks": "user_id, file_id",
    "compat_play_queues": "user_id",
    "compat_play_queue_items": "user_id, item_index",
    "compat_id_map": "kind, internal_id",
}

_LEGACY_REFERENCE_TABLES = frozenset(
    {
        "manual_review_queue",
        "user_favorites",
        "play_history",
        "playlist_tracks",
        "album_release_pins",
        "compat_bookmarks",
        "compat_play_queues",
        "compat_play_queue_items",
        "compat_id_map",
    }
)

_SCAN_ACTIVE_STATES = (
    "discovering",
    "indexing",
    "reconciling",
    "pausing",
    "paused",
    "stopping",
)

_SCAN_TRANSITIONS: dict[str, set[str]] = {
    "queued": {"discovering", "cancelled", "failed"},
    "discovering": {
        "indexing",
        "pausing",
        "stopping",
        "failed",
        "superseded_policy_changed",
    },
    "indexing": {
        "reconciling",
        "pausing",
        "stopping",
        "failed",
        "superseded_policy_changed",
    },
    "reconciling": {
        "completed",
        "pausing",
        "stopping",
        "failed",
        "superseded_policy_changed",
    },
    "pausing": {"paused", "stopping", "failed", "superseded_policy_changed"},
    "paused": {
        "discovering",
        "indexing",
        "reconciling",
        "cancelled",
        "superseded_policy_changed",
        "failed",
    },
    "stopping": {"cancelled", "failed"},
}


def _fold(value: str | None) -> str | None:
    if value is None:
        return None
    decomposed = unicodedata.normalize("NFKD", value.strip())
    without_marks = "".join(
        character for character in decomposed if not unicodedata.combining(character)
    ).casefold()
    return " ".join(without_marks.split())


def _normalize_exact(value: str | None) -> str:
    if value is None:
        return ""
    return " ".join(unicodedata.normalize("NFKC", value).strip().casefold().split())


def _escape_like(value: str) -> str:
    return value.replace("\\", "\\\\").replace("%", "\\%").replace("_", "\\_")


def _album_input_revision(rows: list[sqlite3.Row]) -> str:
    ordered = sorted(rows, key=lambda row: str(row["id"]))

    def digest(values: list[str]) -> str:
        return hashlib.sha256("|".join(values).encode()).hexdigest()

    return ":".join(
        (
            digest([f"{row['id']}:{row['tag_revision'] or ''}" for row in ordered]),
            digest([f"{row['id']}:{row['stat_revision']}" for row in ordered]),
            digest(
                [
                    f"{row['id']}:{row['applied_policy_revision']}:{row['applied_policy']}"
                    for row in ordered
                ]
            ),
        )
    )


def _row(row: sqlite3.Row | None) -> dict[str, Any] | None:
    return dict(row) if row is not None else None


def _review_filter_predicate(
    normalized_filter: dict[str, str],
) -> tuple[list[str], list[Any]]:
    clauses: list[str] = []
    parameters: list[Any] = []
    exact_columns = {
        "state": "r.state",
        "reason_code": "r.reason_code",
        "root_id": "COALESCE(a.root_id, t.root_id)",
        "policy": "COALESCE(t.applied_policy, at.applied_policy)",
        "job_state": "job.state",
    }
    for key, value in sorted(normalized_filter.items()):
        if key in exact_columns:
            clauses.append(f"{exact_columns[key]} = ?")
            parameters.append(value)
        elif key in {"metadata_incomplete", "candidate_available"}:
            normalized = value.strip().casefold()
            if normalized not in {"true", "false"}:
                raise ValueError(f"Invalid boolean review selection filter: {key}")
            expression = (
                "COALESCE((SELECT SUM(album_track.metadata_incomplete) "
                "FROM local_tracks album_track WHERE album_track.local_album_id = a.id), "
                "t.metadata_incomplete, 0)"
                if key == "metadata_incomplete"
                else "COALESCE(attempt.candidate_count, 0)"
            )
            clauses.append(f"{expression} {'> 0' if normalized == 'true' else '= 0'}")
        elif key == "states":
            try:
                states = msgspec.json.decode(value.encode())
            except msgspec.DecodeError as error:
                raise ValueError("Invalid review selection states.") from error
            if (
                not isinstance(states, list)
                or not states
                or any(not isinstance(state, str) for state in states)
            ):
                raise ValueError("Review selection states must be a non-empty list.")
            placeholders = ",".join("?" for _ in states)
            clauses.append(f"r.state IN ({placeholders})")
            parameters.extend(states)
        elif key in {"created_from", "created_to", "updated_from", "updated_to"}:
            try:
                boundary = float(value)
            except ValueError as error:
                raise ValueError(
                    f"Invalid time review selection filter: {key}"
                ) from error
            column = "r.created_at" if key.startswith("created_") else "r.updated_at"
            clauses.append(f"{column} {'>=' if key.endswith('_from') else '<='} ?")
            parameters.append(boundary)
        elif key == "search":
            folded = f"%{_fold(value) or ''}%"
            clauses.append(
                "(a.title_folded LIKE ? OR a.album_artist_name_folded LIKE ? "
                "OR t.title_folded LIKE ? OR t.relative_path LIKE ?)"
            )
            parameters.extend((folded, folded, folded, folded))
        elif key == "scopes":
            try:
                scopes = msgspec.json.decode(value.encode())
            except msgspec.DecodeError as error:
                raise ValueError("Invalid review selection scopes.") from error
            if not isinstance(scopes, list) or not scopes:
                raise ValueError("Review selection scopes must be a non-empty list.")
            scope_clauses: list[str] = []
            for scope in scopes:
                if not isinstance(scope, dict):
                    raise ValueError("Invalid review selection scope.")
                root_id = scope.get("root_id")
                relative_path = scope.get("relative_path", ".")
                if not isinstance(root_id, str) or not isinstance(relative_path, str):
                    raise ValueError("Invalid review selection scope values.")
                root_clause = "COALESCE(a.root_id, t.root_id) = ?"
                parameters.append(root_id)
                prefix = relative_path.strip("/")
                if not prefix or prefix == ".":
                    scope_clauses.append(f"({root_clause})")
                    continue
                escaped = _escape_like(prefix)
                scope_clauses.append(
                    f"({root_clause} AND (COALESCE((SELECT MIN(album_track.relative_path) "
                    "FROM local_tracks album_track WHERE album_track.local_album_id = a.id), "
                    "t.relative_path, '') = ? OR COALESCE((SELECT MIN(album_track.relative_path) "
                    "FROM local_tracks album_track WHERE album_track.local_album_id = a.id), "
                    "t.relative_path, '') LIKE ? ESCAPE '\\'))"
                )
                parameters.extend((prefix, f"{escaped}/%"))
            clauses.append("(" + " OR ".join(scope_clauses) + ")")
        else:
            raise ValueError(f"Unsupported review selection filter: {key}")
    return clauses, parameters


def _bulk_preview_ineligibility(
    action: str,
    row: dict[str, Any],
    candidate_key: str | None,
    candidate_evidence: CandidateEvidence | None,
) -> str | None:
    if (
        str(row["state"]) == "excluded"
        or (action == "retry" and str(row["effective_policy"]) == "excluded")
    ) and action != "exclude":
        return "EXCLUDED"
    if action == "keep_tagged" and row.get("release_group_mbid"):
        return "IDENTITY_REQUIRES_DETACH"
    if action == "accept_candidate" and (
        not candidate_key or candidate_evidence is None
    ):
        return "CANDIDATE_NOT_AVAILABLE"
    if action == "accept_candidate" and candidate_evidence is not None:
        if candidate_evidence.reason_code not in AUTOMATIC_SAFE_EVIDENCE_REASONS:
            return "CANDIDATE_NOT_AUTOMATIC_SAFE"
        current_identity = row.get("release_group_mbid")
        if (
            current_identity
            and current_identity != candidate_evidence.release_group_mbid
        ):
            return "IDENTITY_CONFLICT"
    return None


class NativeLibraryStore(PersistenceBase):
    """Own every target catalog, job, review, scan, and migration transaction."""

    def __init__(
        self,
        db_path: Path,
        write_lock: threading.Lock,
        invalidator: Callable[[], Awaitable[None]] | None = None,
        scan_invalidator: Callable[[], Awaitable[None]] | None = None,
    ) -> None:
        self._invalidator = invalidator
        self._scan_invalidator = scan_invalidator or invalidator
        self._scan_invalidation_lock = asyncio.Lock()
        self._scan_invalidation_pending = False
        self._last_scan_invalidation_at = 0.0
        self._scan_catalog_dirty = False
        self._provider_album_snapshot: tuple[int, frozenset[str]] | None = None
        self._provider_album_snapshot_lock = asyncio.Lock()
        super().__init__(db_path, write_lock)

    async def _write(self, operation: Callable[[sqlite3.Connection], _T]) -> _T:
        def tracked(connection: sqlite3.Connection) -> tuple[_T, bool]:
            before = int(
                connection.execute(
                    "SELECT value FROM library_catalog_revision WHERE singleton = 1"
                ).fetchone()[0]
            )
            result = operation(connection)
            after = int(
                connection.execute(
                    "SELECT value FROM library_catalog_revision WHERE singleton = 1"
                ).fetchone()[0]
            )
            return result, before != after

        result, catalog_changed = await super()._write(tracked)
        if catalog_changed:
            try:
                await self._invalidate()
            except Exception:  # noqa: BLE001 - the database commit already succeeded
                logger.exception(
                    "Target catalog cache invalidation failed after commit"
                )
        return result

    async def _invalidate(self) -> None:
        if self._invalidator is not None:
            await self._invalidator()

    async def _write_scan(self, operation: Callable[[sqlite3.Connection], _T]) -> _T:
        def tracked(connection: sqlite3.Connection) -> tuple[_T, bool]:
            before = int(
                connection.execute(
                    "SELECT value FROM library_catalog_revision WHERE singleton = 1"
                ).fetchone()[0]
            )
            result = operation(connection)
            after = int(
                connection.execute(
                    "SELECT value FROM library_catalog_revision WHERE singleton = 1"
                ).fetchone()[0]
            )
            return result, before != after

        result, catalog_changed = await super()._background_write(tracked)
        if catalog_changed:
            async with self._scan_invalidation_lock:
                self._scan_catalog_dirty = True
                now = time.monotonic()
                if now - self._last_scan_invalidation_at >= 30.0:
                    if self._scan_invalidator is not None:
                        await self._scan_invalidator()
                    self._last_scan_invalidation_at = now
                    self._scan_invalidation_pending = False
                else:
                    self._scan_invalidation_pending = True
        return result

    async def flush_scan_invalidation(self, *, terminal: bool = False) -> None:
        async with self._scan_invalidation_lock:
            if terminal and self._scan_catalog_dirty:
                await self._invalidate()
                self._last_scan_invalidation_at = time.monotonic()
                self._scan_invalidation_pending = False
                self._scan_catalog_dirty = False
                return
            if not self._scan_invalidation_pending:
                return
            if self._scan_invalidator is not None:
                await self._scan_invalidator()
            self._last_scan_invalidation_at = time.monotonic()
            self._scan_invalidation_pending = False

    def _connect(self) -> sqlite3.Connection:
        connection = super()._connect()
        connection.execute("PRAGMA foreign_keys=ON")
        return connection

    @staticmethod
    def _repair_resolved_release_alias_identities(
        connection: sqlite3.Connection,
    ) -> int:
        """Correct release MBIDs previously stored as release-group identities.

        ``mbid_resolution_map`` contains durable, provider-verified release-to-group
        mappings. Older album-page requests could propagate the source release into
        the target catalog's release-group column. Preserve that source as edition
        context and replace only the selected external identity; embedded tag facts
        and immutable identification evidence remain unchanged.
        """
        has_resolution_map = connection.execute(
            "SELECT 1 FROM sqlite_master WHERE type = 'table' "
            "AND name = 'mbid_resolution_map'"
        ).fetchone()
        if has_resolution_map is None:
            return 0
        corrections = connection.execute(
            """
            SELECT ae.local_album_id,
                   ae.release_group_mbid AS release_mbid,
                   map.release_group_mbid AS canonical_release_group_mbid,
                   ae.selected_at
            FROM local_album_external_identities ae
            JOIN mbid_resolution_map map
              ON map.source_mbid_lower = lower(ae.release_group_mbid)
            WHERE ae.provider = 'musicbrainz'
              AND map.release_group_mbid IS NOT NULL
              AND trim(map.release_group_mbid) != ''
              AND lower(map.release_group_mbid) != lower(ae.release_group_mbid)
            ORDER BY ae.local_album_id
            """
        ).fetchall()
        changed = 0
        for row in corrections:
            result = connection.execute(
                """
                UPDATE local_album_external_identities
                SET release_group_mbid = ?,
                    release_mbid = COALESCE(release_mbid, ?),
                    row_revision = row_revision + 1
                WHERE local_album_id = ?
                  AND provider = 'musicbrainz'
                  AND lower(release_group_mbid) = lower(?)
                  AND row_revision < ?
                """,
                (
                    row["canonical_release_group_mbid"],
                    row["release_mbid"],
                    row["local_album_id"],
                    row["release_mbid"],
                    MAX_REVISION,
                ),
            )
            if result.rowcount != 1:
                raise RevisionOverflowError(
                    "The album identity revision cannot be increased."
                )
            connection.execute(
                "UPDATE library_album_release_pins SET release_group_mbid = ? "
                "WHERE local_album_id = ?",
                (row["canonical_release_group_mbid"], row["local_album_id"]),
            )
            connection.execute(
                "INSERT OR IGNORE INTO local_album_aliases "
                "(alias, local_album_id, kind, created_at) "
                "VALUES (?, ?, 'compat_migration', ?)",
                (
                    str(row["release_mbid"]).casefold(),
                    row["local_album_id"],
                    row["selected_at"],
                ),
            )
            changed += 1
        if changed:
            result = connection.execute(
                "UPDATE library_catalog_revision SET value = value + 1 "
                "WHERE singleton = 1 AND value < ?",
                (MAX_REVISION,),
            )
            if result.rowcount != 1:
                raise RevisionOverflowError(
                    "The library catalog revision cannot be increased."
                )
        return changed

    def _ensure_tables(self) -> None:
        connection = self._connect()
        try:
            connection.executescript(SCHEMA_SQL)
            for statement in (
                "ALTER TABLE library_identification_jobs ADD COLUMN checkpoint_json TEXT",
                "ALTER TABLE library_work_control ADD COLUMN high_priority_claim_count INTEGER NOT NULL DEFAULT 0",
                "ALTER TABLE local_tracks ADD COLUMN embedded_release_group_mbid TEXT",
                "ALTER TABLE local_tracks ADD COLUMN embedded_release_mbid TEXT",
                "ALTER TABLE local_tracks ADD COLUMN embedded_recording_mbid TEXT",
                "ALTER TABLE local_tracks ADD COLUMN embedded_artist_mbid TEXT",
                "ALTER TABLE local_tracks ADD COLUMN embedded_album_artist_mbid TEXT",
                "ALTER TABLE audio_fingerprint_outcomes ADD COLUMN release_group_ids_json TEXT NOT NULL DEFAULT '[]'",
                "ALTER TABLE local_artists ADD COLUMN normalized_name TEXT NOT NULL DEFAULT ''",
                "ALTER TABLE local_tracks ADD COLUMN tag_album_title TEXT",
                "ALTER TABLE local_tracks ADD COLUMN tag_album_artist_name TEXT",
                "ALTER TABLE local_tracks ADD COLUMN manual_excluded INTEGER NOT NULL DEFAULT 0 CHECK(manual_excluded IN (0,1))",
                "ALTER TABLE local_tracks ADD COLUMN genre_folded TEXT",
                "ALTER TABLE local_tracks ADD COLUMN stat_revision_kind TEXT NOT NULL DEFAULT 'unclassified' CHECK(stat_revision_kind IN ('exact','legacy_float','legacy_review','unclassified'))",
                "ALTER TABLE library_identity_repair_findings ADD COLUMN expected_identity_revision INTEGER",
                "ALTER TABLE library_identity_repair_findings ADD COLUMN reason_code TEXT NOT NULL DEFAULT ''",
                "ALTER TABLE library_identity_repair_findings ADD COLUMN apply_eligible INTEGER NOT NULL DEFAULT 0 CHECK(apply_eligible IN (0,1))",
                "ALTER TABLE library_identity_repair_findings ADD COLUMN apply_result TEXT",
                "ALTER TABLE library_catalog_actions ADD COLUMN local_artist_id TEXT REFERENCES local_artists(id) ON DELETE RESTRICT",
                "ALTER TABLE library_scan_run_scopes ADD COLUMN scope_id TEXT",
                "ALTER TABLE library_scan_run_scopes ADD COLUMN root_path TEXT",
                "ALTER TABLE library_policy_state ADD COLUMN changed_track_count INTEGER NOT NULL DEFAULT 0",
                "ALTER TABLE library_policy_state ADD COLUMN cancelled_work_count INTEGER NOT NULL DEFAULT 0",
                "ALTER TABLE library_policy_state ADD COLUMN pending_scopes_json TEXT NOT NULL DEFAULT '[]'",
                "ALTER TABLE library_bulk_review_previews ADD COLUMN requires_local_metadata_confirmation INTEGER NOT NULL DEFAULT 0 CHECK(requires_local_metadata_confirmation IN (0,1))",
                "ALTER TABLE library_bulk_review_previews ADD COLUMN state TEXT NOT NULL DEFAULT 'ready' CHECK(state IN ('staging','ready'))",
                "ALTER TABLE library_bulk_review_previews ADD COLUMN summary_json TEXT NOT NULL DEFAULT '{}'",
                "ALTER TABLE library_bulk_review_previews ADD COLUMN cursor_updated_at REAL",
                "ALTER TABLE library_bulk_review_previews ADD COLUMN cursor_review_id TEXT",
                "ALTER TABLE library_bulk_review_previews ADD COLUMN subject_count INTEGER NOT NULL DEFAULT 0 CHECK(subject_count >= 0)",
                "ALTER TABLE library_bulk_review_snapshots ADD COLUMN staging_state TEXT NOT NULL DEFAULT 'ready' CHECK(staging_state IN ('staging','ready'))",
                "ALTER TABLE library_bulk_review_snapshots ADD COLUMN staging_cursor INTEGER NOT NULL DEFAULT -1 CHECK(staging_cursor >= -1)",
                "ALTER TABLE library_scan_runs ADD COLUMN phase_started_at REAL",
                "ALTER TABLE library_scan_runs ADD COLUMN phase_timings_json TEXT NOT NULL DEFAULT '{}'",
                "ALTER TABLE library_scan_runs ADD COLUMN new_count INTEGER NOT NULL DEFAULT 0",
                "ALTER TABLE library_scan_runs ADD COLUMN inventory_cleanup_pending INTEGER NOT NULL DEFAULT 0 CHECK(inventory_cleanup_pending IN (0,1))",
                "ALTER TABLE library_scan_run_scopes ADD COLUMN discovery_generation INTEGER NOT NULL DEFAULT 1",
                "ALTER TABLE library_scan_inventory ADD COLUMN scope_relative_path TEXT NOT NULL DEFAULT '.'",
                "ALTER TABLE library_scan_inventory ADD COLUMN discovery_generation INTEGER NOT NULL DEFAULT 1",
                "ALTER TABLE library_scan_grouping_contexts ADD COLUMN staging_state TEXT NOT NULL DEFAULT 'pending' CHECK(staging_state IN ('pending','tracks','tokens','groups','continuity','albums','memberships','retirement','queue','completed'))",
                "ALTER TABLE library_scan_grouping_contexts ADD COLUMN staging_cursor TEXT",
                "ALTER TABLE library_scan_grouping_contexts ADD COLUMN application_cursor TEXT",
                "ALTER TABLE library_scan_grouping_contexts ADD COLUMN queue_cursor TEXT",
                "ALTER TABLE library_scan_grouping_contexts ADD COLUMN grouping_merge_target TEXT",
                "ALTER TABLE library_scan_grouping_contexts ADD COLUMN grouping_merge_ready INTEGER NOT NULL DEFAULT 0 CHECK(grouping_merge_ready IN (0,1))",
                "ALTER TABLE library_scan_grouping_groups ADD COLUMN tag_revision_accumulator TEXT NOT NULL DEFAULT '0000000000000000000000000000000000000000000000000000000000000000'",
                "ALTER TABLE library_scan_grouping_groups ADD COLUMN stat_revision_accumulator TEXT NOT NULL DEFAULT '0000000000000000000000000000000000000000000000000000000000000000'",
                "ALTER TABLE library_scan_grouping_groups ADD COLUMN policy_revision_accumulator TEXT NOT NULL DEFAULT '0000000000000000000000000000000000000000000000000000000000000000'",
                "ALTER TABLE library_scan_grouping_groups ADD COLUMN automatic_track_count INTEGER NOT NULL DEFAULT 0",
                "ALTER TABLE library_scan_grouping_groups ADD COLUMN local_metadata_track_count INTEGER NOT NULL DEFAULT 0",
                "ALTER TABLE library_scan_grouping_groups ADD COLUMN excluded_track_count INTEGER NOT NULL DEFAULT 0",
                "ALTER TABLE library_scan_grouping_groups ADD COLUMN embedded_identity_count INTEGER NOT NULL DEFAULT 0",
                "ALTER TABLE library_scan_grouping_old_nodes ADD COLUMN matched_grouping_token TEXT",
                "ALTER TABLE library_scan_grouping_new_nodes ADD COLUMN matched_old_album_id TEXT REFERENCES local_albums(id) ON DELETE RESTRICT",
                "ALTER TABLE library_migration_runs ADD COLUMN root_revision TEXT NOT NULL DEFAULT ''",
            ):
                try:
                    connection.execute(statement)
                except sqlite3.OperationalError as error:
                    if "duplicate column name" not in str(error).casefold():
                        raise
            connection.execute(
                "UPDATE local_tracks SET genre_folded = fold(genre) "
                "WHERE genre IS NOT NULL AND genre != '' AND genre_folded IS NULL"
            )
            connection.execute(
                "UPDATE local_tracks SET stat_revision_kind = 'exact' "
                "WHERE stat_revision_kind = 'unclassified' "
                "AND stat_revision = CAST(file_size_bytes AS TEXT) || ':' || CAST(file_mtime_ns AS TEXT)"
            )
            connection.execute(
                "UPDATE local_tracks SET stat_revision_kind = 'legacy_review', "
                "tags_read_at = COALESCE(tags_read_at, imported_at) "
                "WHERE stat_revision_kind = 'unclassified' AND ingest_source = 'legacy_review'"
            )
            connection.execute(
                "UPDATE local_tracks SET stat_revision_kind = 'legacy_float' "
                "WHERE stat_revision_kind = 'unclassified' "
                "AND membership_source = 'legacy_import'"
            )
            connection.execute(
                "UPDATE library_scan_inventory AS inventory "
                "SET scope_relative_path = ("
                "SELECT scope.relative_path FROM library_scan_run_scopes AS scope "
                "WHERE scope.run_id = inventory.run_id "
                "AND scope.root_id = inventory.root_id "
                "AND (inventory.relative_path = scope.relative_path "
                "OR inventory.relative_path LIKE scope.relative_path || '/%') "
                "ORDER BY length(scope.relative_path) DESC, scope.scope_sequence LIMIT 1"
                ") WHERE inventory.scope_relative_path = '.' "
                "AND NOT EXISTS ("
                "SELECT 1 FROM library_scan_run_scopes AS root_scope "
                "WHERE root_scope.run_id = inventory.run_id "
                "AND root_scope.root_id = inventory.root_id "
                "AND root_scope.relative_path = '.'"
                ") AND EXISTS ("
                "SELECT 1 FROM library_scan_run_scopes AS matching_scope "
                "WHERE matching_scope.run_id = inventory.run_id "
                "AND matching_scope.root_id = inventory.root_id "
                "AND (inventory.relative_path = matching_scope.relative_path "
                "OR inventory.relative_path LIKE matching_scope.relative_path || '/%')"
                ")"
            )
            connection.execute(
                "UPDATE local_albums SET title_folded = fold(title), "
                "album_artist_name_folded = fold(album_artist_name) "
                "WHERE title_folded IS NOT fold(title) "
                "OR album_artist_name_folded IS NOT fold(album_artist_name)"
            )
            connection.execute(
                "CREATE INDEX IF NOT EXISTS idx_local_tracks_genre_artwork "
                "ON local_tracks(genre_folded, availability, local_album_id)"
            )
            connection.executescript(
                """
                CREATE TRIGGER IF NOT EXISTS trg_genre_artwork_track_insert
                AFTER INSERT ON local_tracks
                WHEN NEW.genre IS NOT NULL AND trim(NEW.genre) != ''
                BEGIN
                    INSERT INTO library_genre_artwork_revisions(genre_folded, value)
                    VALUES (COALESCE(NEW.genre_folded, lower(trim(NEW.genre))), 1)
                    ON CONFLICT(genre_folded) DO UPDATE SET value = value + 1;
                END;

                CREATE TRIGGER IF NOT EXISTS trg_genre_artwork_track_delete
                AFTER DELETE ON local_tracks
                WHEN OLD.genre IS NOT NULL AND trim(OLD.genre) != ''
                BEGIN
                    INSERT INTO library_genre_artwork_revisions(genre_folded, value)
                    VALUES (COALESCE(OLD.genre_folded, lower(trim(OLD.genre))), 1)
                    ON CONFLICT(genre_folded) DO UPDATE SET value = value + 1;
                END;

                CREATE TRIGGER IF NOT EXISTS trg_genre_artwork_track_update
                AFTER UPDATE OF genre, genre_folded, availability, local_album_id ON local_tracks
                BEGIN
                    INSERT INTO library_genre_artwork_revisions(genre_folded, value)
                    SELECT COALESCE(OLD.genre_folded, lower(trim(OLD.genre))), 1
                    WHERE OLD.genre IS NOT NULL AND trim(OLD.genre) != ''
                    ON CONFLICT(genre_folded) DO UPDATE SET value = value + 1;
                    INSERT INTO library_genre_artwork_revisions(genre_folded, value)
                    SELECT COALESCE(NEW.genre_folded, lower(trim(NEW.genre))), 1
                    WHERE NEW.genre IS NOT NULL AND trim(NEW.genre) != ''
                      AND COALESCE(NEW.genre_folded, lower(trim(NEW.genre)))
                          != COALESCE(OLD.genre_folded, lower(trim(OLD.genre)), '')
                    ON CONFLICT(genre_folded) DO UPDATE SET value = value + 1;
                END;

                CREATE TRIGGER IF NOT EXISTS trg_genre_artwork_artwork_insert
                AFTER INSERT ON local_album_artwork
                BEGIN
                    INSERT INTO library_genre_artwork_revisions(genre_folded, value)
                    SELECT DISTINCT COALESCE(t.genre_folded, lower(trim(t.genre))), 1
                    FROM local_tracks t
                    WHERE t.local_album_id = NEW.local_album_id
                      AND t.availability = 'indexed'
                      AND t.genre IS NOT NULL AND trim(t.genre) != ''
                    ON CONFLICT(genre_folded) DO UPDATE SET value = value + 1;
                END;

                CREATE TRIGGER IF NOT EXISTS trg_genre_artwork_artwork_update
                AFTER UPDATE ON local_album_artwork
                BEGIN
                    INSERT INTO library_genre_artwork_revisions(genre_folded, value)
                    SELECT DISTINCT COALESCE(t.genre_folded, lower(trim(t.genre))), 1
                    FROM local_tracks t
                    WHERE t.local_album_id = NEW.local_album_id
                      AND t.availability = 'indexed'
                      AND t.genre IS NOT NULL AND trim(t.genre) != ''
                    ON CONFLICT(genre_folded) DO UPDATE SET value = value + 1;
                END;

                CREATE TRIGGER IF NOT EXISTS trg_genre_artwork_artwork_delete
                AFTER DELETE ON local_album_artwork
                BEGIN
                    INSERT INTO library_genre_artwork_revisions(genre_folded, value)
                    SELECT DISTINCT COALESCE(t.genre_folded, lower(trim(t.genre))), 1
                    FROM local_tracks t
                    WHERE t.local_album_id = OLD.local_album_id
                      AND t.availability = 'indexed'
                      AND t.genre IS NOT NULL AND trim(t.genre) != ''
                    ON CONFLICT(genre_folded) DO UPDATE SET value = value + 1;
                END;

                CREATE TRIGGER IF NOT EXISTS trg_genre_artwork_album_update
                AFTER UPDATE OF title, album_artist_id, album_artist_name, retired_into_album_id
                ON local_albums
                BEGIN
                    INSERT INTO library_genre_artwork_revisions(genre_folded, value)
                    SELECT DISTINCT COALESCE(t.genre_folded, lower(trim(t.genre))), 1
                    FROM local_tracks t
                    WHERE t.local_album_id = NEW.id
                      AND t.availability = 'indexed'
                      AND t.genre IS NOT NULL AND trim(t.genre) != ''
                    ON CONFLICT(genre_folded) DO UPDATE SET value = value + 1;
                END;
                """
            )
            connection.executemany(
                "INSERT OR IGNORE INTO local_artists "
                "(id, display_name, folded_name, normalized_name, kind, created_at, updated_at) "
                "VALUES (?, ?, ?, ?, ?, 0, 0)",
                [
                    (
                        VARIOUS_ARTISTS_ID,
                        "Various Artists",
                        "various artists",
                        "various artists",
                        "various_artists",
                    ),
                    (
                        UNKNOWN_ARTIST_ID,
                        "Unknown Artist",
                        "unknown artist",
                        "unknown artist",
                        "unknown",
                    ),
                ],
            )
            self._repair_resolved_release_alias_identities(connection)
            connection.commit()
        finally:
            connection.close()

    async def foreign_keys_enabled(self) -> bool:
        def operation(connection: sqlite3.Connection) -> bool:
            return bool(connection.execute("PRAGMA foreign_keys").fetchone()[0])

        return await self._read(operation)

    @staticmethod
    def _increment_singleton(
        connection: sqlite3.Connection,
        table: str,
        key_column: str,
        key: str | int,
    ) -> int:
        cursor = connection.execute(
            f"UPDATE {table} SET value = value + 1 "
            f"WHERE {key_column} = ? AND value < ? RETURNING value",
            (key, MAX_REVISION),
        )
        row = cursor.fetchone()
        if row is None:
            raise RevisionOverflowError("A library revision reached its maximum value.")
        return int(row["value"])

    @classmethod
    def _bump_catalog(cls, connection: sqlite3.Connection) -> int:
        return cls._increment_singleton(
            connection, "library_catalog_revision", "singleton", 1
        )

    @classmethod
    def _bump_stream(cls, connection: sqlite3.Connection, stream: str) -> int:
        return cls._increment_singleton(
            connection, "library_event_stream_revisions", "stream_kind", stream
        )

    @staticmethod
    def _require_revision_update(
        connection: sqlite3.Connection,
        *,
        table: str,
        entity_id: str,
        expected_revision: int,
        assignments: str,
        parameters: tuple[Any, ...],
    ) -> int:
        cursor = connection.execute(
            f"UPDATE {table} SET {assignments}, row_revision = row_revision + 1 "
            "WHERE id = ? AND row_revision = ? AND row_revision < ? "
            "RETURNING row_revision",
            (*parameters, entity_id, expected_revision, MAX_REVISION),
        )
        updated = cursor.fetchone()
        if updated is not None:
            return int(updated["row_revision"])
        current = connection.execute(
            f"SELECT row_revision FROM {table} WHERE id = ?", (entity_id,)
        ).fetchone()
        if current is None:
            raise ResourceNotFoundError(f"Target row not found: {entity_id}")
        if int(current["row_revision"]) >= MAX_REVISION:
            raise RevisionOverflowError(
                "A library row revision reached its maximum value."
            )
        raise StaleRevisionError(
            "The library item changed before this action was applied."
        )

    @staticmethod
    def _refuse_max_revision(
        connection: sqlite3.Connection,
        *,
        table: str,
        predicate: str,
        parameters: tuple[Any, ...],
        include_event_revision: bool = False,
        include_version: bool = False,
    ) -> None:
        columns = ["row_revision"]
        if include_event_revision:
            columns.append("event_revision")
        if include_version:
            columns.append("version")
        rows = connection.execute(
            f"SELECT {', '.join(columns)} FROM {table} WHERE {predicate}", parameters
        ).fetchall()
        if any(int(row[column]) >= MAX_REVISION for row in rows for column in columns):
            raise RevisionOverflowError("A library revision reached its maximum value.")

    async def get_catalog_revision(self) -> int:
        def operation(connection: sqlite3.Connection) -> int:
            row = connection.execute(
                "SELECT value FROM library_catalog_revision WHERE singleton = 1"
            ).fetchone()
            return int(row["value"])

        return await self._read(operation)

    @staticmethod
    def _resolve_target_id(
        connection: sqlite3.Connection, *, kind: str, identifier: str
    ) -> str | None:
        if kind == "album":
            row = connection.execute(
                "SELECT COALESCE(retired_into_album_id, id) AS id FROM local_albums "
                "WHERE id = ? UNION ALL "
                "SELECT local_album_id FROM local_album_aliases WHERE alias = ? LIMIT 1",
                (identifier, identifier.casefold()),
            ).fetchone()
        elif kind == "artist":
            row = connection.execute(
                "SELECT COALESCE(retired_into_artist_id, id) AS id FROM local_artists "
                "WHERE id = ? UNION ALL "
                "SELECT local_artist_id FROM local_artist_aliases WHERE alias = ? LIMIT 1",
                (identifier, identifier.casefold()),
            ).fetchone()
        elif kind == "track":
            row = connection.execute(
                "SELECT id FROM local_tracks WHERE id = ?",
                (identifier,),
            ).fetchone()
        else:
            raise ValueError(f"Unsupported target identifier kind: {kind}")
        return str(row["id"]) if row is not None else None

    async def resolve_target_id(self, kind: str, identifier: str) -> str | None:
        def operation(connection: sqlite3.Connection) -> str | None:
            return self._resolve_target_id(connection, kind=kind, identifier=identifier)

        return await self._read(operation)

    async def resolve_target_ids(
        self, kind: str, identifiers: list[str]
    ) -> dict[str, str]:
        def operation(connection: sqlite3.Connection) -> dict[str, str]:
            resolved: dict[str, str] = {}
            for identifier in dict.fromkeys(identifiers):
                target = self._resolve_target_id(
                    connection, kind=kind, identifier=identifier
                )
                if target is not None:
                    resolved[identifier] = target
            return resolved

        return await self._read(operation)

    @classmethod
    def _resolve_target_membership_ids(
        cls, connection: sqlite3.Connection, *, kind: str, identifier: str
    ) -> list[str]:
        direct = cls._resolve_target_id(connection, kind=kind, identifier=identifier)
        if direct is not None:
            return [direct]
        if kind == "album":
            query = (
                "SELECT identity.local_album_id AS id "
                "FROM local_album_external_identities identity "
                "JOIN local_albums subject ON subject.id = identity.local_album_id "
                "WHERE identity.provider = 'musicbrainz' "
                "AND (LOWER(identity.release_group_mbid) = LOWER(?) "
                "OR LOWER(identity.release_mbid) = LOWER(?)) "
                "AND subject.retired_into_album_id IS NULL "
                "AND EXISTS (SELECT 1 FROM local_tracks active_track "
                "WHERE active_track.local_album_id = subject.id "
                "AND active_track.availability = 'indexed')"
            )
        elif kind == "artist":
            query = (
                "SELECT identity.local_artist_id AS id "
                "FROM local_artist_external_identities identity "
                "JOIN local_artists subject ON subject.id = identity.local_artist_id "
                "WHERE identity.provider = 'musicbrainz' "
                "AND LOWER(identity.provider_artist_id) = LOWER(?) "
                "AND subject.retired_into_artist_id IS NULL "
                "AND EXISTS (SELECT 1 FROM local_tracks active_track "
                "WHERE active_track.availability = 'indexed' AND ("
                "EXISTS (SELECT 1 FROM local_album_artists album_credit "
                "WHERE album_credit.local_artist_id = subject.id "
                "AND album_credit.local_album_id = active_track.local_album_id) "
                "OR EXISTS (SELECT 1 FROM local_track_artists track_credit "
                "WHERE track_credit.local_artist_id = subject.id "
                "AND track_credit.local_track_id = active_track.id)))"
            )
        elif kind == "track":
            query = (
                "SELECT identity.local_track_id AS id "
                "FROM local_track_external_identities identity "
                "JOIN local_tracks subject ON subject.id = identity.local_track_id "
                "WHERE identity.provider = 'musicbrainz' "
                "AND LOWER(identity.recording_mbid) = LOWER(?) "
                "AND subject.availability = 'indexed'"
            )
        else:
            raise ValueError(f"Unsupported target identifier kind: {kind}")
        parameters = (identifier, identifier) if kind == "album" else (identifier,)
        rows = connection.execute(query + " ORDER BY id", parameters).fetchall()
        return [str(row["id"]) for row in rows]

    async def resolve_canonical_target_id(
        self, kind: str, identifier: str
    ) -> str | None:
        """Resolve local IDs, aliases, and unambiguous provider identities."""

        def operation(connection: sqlite3.Connection) -> str | None:
            matches = self._resolve_target_membership_ids(
                connection, kind=kind, identifier=identifier
            )
            return matches[0] if len(matches) == 1 else None

        return await self._read(operation)

    @classmethod
    def _resolve_unique_target_subject_id(
        cls, connection: sqlite3.Connection, *, kind: str, identifier: str
    ) -> str | None:
        matches = cls._resolve_target_membership_ids(
            connection, kind=kind, identifier=identifier
        )
        if len(matches) > 1:
            raise ConflictError(
                "The provider identity matches multiple local library items; "
                "select a local item before changing it."
            )
        return matches[0] if matches else None

    async def get_target_album_release_pin(self, album_identifier: str) -> str | None:
        def operation(connection: sqlite3.Connection) -> str | None:
            album_id = self._resolve_target_album_pin_id(connection, album_identifier)
            if album_id is None:
                return None
            row = connection.execute(
                "SELECT release_mbid FROM library_album_release_pins "
                "WHERE local_album_id = ?",
                (album_id,),
            ).fetchone()
            return str(row["release_mbid"]) if row is not None else None

        return await self._read(operation)

    async def set_target_album_release_pin(
        self,
        album_identifier: str,
        release_mbid: str,
        set_by_user_id: str | None,
        set_at: str,
    ) -> None:
        def operation(connection: sqlite3.Connection) -> None:
            album_id = self._resolve_target_album_pin_id(connection, album_identifier)
            if album_id is None:
                raise ResourceNotFoundError(
                    f"Album {album_identifier} is not in the local library"
                )
            identity = connection.execute(
                "SELECT release_group_mbid FROM local_album_external_identities "
                "WHERE local_album_id = ? AND provider = 'musicbrainz'",
                (album_id,),
            ).fetchone()
            if identity is None:
                raise ResourceNotFoundError(
                    f"Album {album_identifier} has no MusicBrainz identity"
                )
            connection.execute(
                "INSERT INTO library_album_release_pins "
                "(local_album_id, release_group_mbid, release_mbid, "
                "set_by_user_id, set_at) VALUES (?, ?, ?, ?, ?) "
                "ON CONFLICT(local_album_id) DO UPDATE SET "
                "release_group_mbid = excluded.release_group_mbid, "
                "release_mbid = excluded.release_mbid, "
                "set_by_user_id = excluded.set_by_user_id, "
                "set_at = excluded.set_at",
                (
                    album_id,
                    str(identity["release_group_mbid"]),
                    release_mbid,
                    set_by_user_id,
                    set_at,
                ),
            )

        await self._write(operation)

    async def clear_target_album_release_pin(self, album_identifier: str) -> bool:
        def operation(connection: sqlite3.Connection) -> bool:
            album_id = self._resolve_target_album_pin_id(connection, album_identifier)
            if album_id is None:
                return False
            cursor = connection.execute(
                "DELETE FROM library_album_release_pins WHERE local_album_id = ?",
                (album_id,),
            )
            return cursor.rowcount > 0

        return await self._write(operation)

    @staticmethod
    def _resolve_target_album_pin_id(
        connection: sqlite3.Connection, album_identifier: str
    ) -> str | None:
        direct = connection.execute(
            "SELECT COALESCE(retired_into_album_id, id) AS id FROM local_albums "
            "WHERE id = ? UNION ALL "
            "SELECT local_album_id FROM local_album_aliases WHERE alias = ? LIMIT 1",
            (album_identifier, album_identifier.casefold()),
        ).fetchone()
        if direct is not None:
            return str(direct["id"])
        matches = connection.execute(
            "SELECT identity.local_album_id AS id, EXISTS("
            "SELECT 1 FROM local_tracks track "
            "WHERE track.local_album_id = identity.local_album_id "
            "AND track.availability = 'indexed') AS has_indexed_tracks "
            "FROM local_album_external_identities identity "
            "JOIN local_albums album ON album.id = identity.local_album_id "
            "WHERE identity.provider = 'musicbrainz' "
            "AND LOWER(identity.release_group_mbid) = LOWER(?) "
            "AND album.retired_into_album_id IS NULL "
            "ORDER BY identity.local_album_id",
            (album_identifier,),
        ).fetchall()
        active_matches = [row for row in matches if row["has_indexed_tracks"]]
        if len(active_matches) == 1:
            return str(active_matches[0]["id"])
        if len(active_matches) > 1 or len(matches) > 1:
            raise ConflictError(
                "This MusicBrainz release group matches multiple local albums; "
                "select a local edition before changing its release pin."
            )
        return str(matches[0]["id"]) if matches else None

    async def get_target_artists_by_genre(
        self, genre: str, *, limit: int
    ) -> list[dict[str, Any]]:
        needle = _fold(genre)
        if not needle:
            return []

        def operation(connection: sqlite3.Connection) -> list[dict[str, Any]]:
            rows = connection.execute(
                "WITH genre_tracks AS (SELECT id, local_album_id "
                "FROM local_tracks WHERE availability = 'indexed' "
                "AND genre_folded = ?), "
                "genre_credits(local_artist_id, local_album_id) AS ("
                "SELECT lta.local_artist_id, gt.local_album_id FROM genre_tracks gt "
                "JOIN local_track_artists lta ON lta.local_track_id = gt.id UNION "
                "SELECT laa.local_artist_id, gt.local_album_id FROM genre_tracks gt "
                "JOIN local_album_artists laa ON laa.local_album_id = gt.local_album_id UNION "
                "SELECT a.album_artist_id, gt.local_album_id FROM genre_tracks gt "
                "JOIN local_albums a ON a.id = gt.local_album_id) "
                "SELECT aie.provider_artist_id AS mbid, ar.id AS local_id, "
                "ar.display_name AS name, COUNT(DISTINCT gc.local_album_id) AS album_count "
                "FROM genre_credits gc JOIN local_artists ar ON ar.id = gc.local_artist_id "
                "LEFT JOIN local_artist_external_identities aie "
                "ON aie.local_artist_id = ar.id AND aie.provider = 'musicbrainz' "
                "WHERE ar.retired_into_artist_id IS NULL "
                "GROUP BY ar.id ORDER BY ar.display_name COLLATE NOCASE, ar.id LIMIT ?",
                (needle, max(1, limit)),
            ).fetchall()
            return [dict(row) for row in rows]

        return await self._read(operation)

    async def get_target_albums_by_genre(
        self, genre: str, *, limit: int
    ) -> list[dict[str, Any]]:
        needle = _fold(genre)
        if not needle:
            return []

        def operation(connection: sqlite3.Connection) -> list[dict[str, Any]]:
            rows = connection.execute(
                "SELECT ae.release_group_mbid AS mbid, a.id AS local_id, "
                "ae.release_group_mbid AS release_group_mbid, a.title, "
                "a.album_artist_name AS artist_name, "
                "aie.provider_artist_id AS artist_mbid, "
                "a.year, artwork.cover_url "
                "FROM local_albums a "
                "LEFT JOIN local_album_external_identities ae ON ae.local_album_id = a.id "
                "AND ae.provider = 'musicbrainz' "
                "LEFT JOIN local_artist_external_identities aie "
                "ON aie.local_artist_id = a.album_artist_id "
                "AND aie.provider = 'musicbrainz' "
                "LEFT JOIN local_album_artwork artwork ON artwork.local_album_id = a.id "
                "WHERE a.retired_into_album_id IS NULL "
                "AND EXISTS (SELECT 1 FROM local_tracks t "
                "WHERE t.local_album_id = a.id AND t.availability = 'indexed' "
                "AND t.genre_folded = ?) "
                "ORDER BY a.updated_at DESC, a.title COLLATE NOCASE, a.id LIMIT ?",
                (needle, max(1, limit)),
            ).fetchall()
            return [dict(row) for row in rows]

        return await self._read(operation)

    async def get_target_top_genres(self, *, limit: int) -> list[tuple[str, int]]:
        def operation(connection: sqlite3.Connection) -> list[tuple[str, int]]:
            rows = connection.execute(
                "WITH genre_tracks AS (SELECT id, local_album_id, genre_folded "
                "FROM local_tracks WHERE availability = 'indexed' "
                "AND genre_folded IS NOT NULL AND genre_folded != ''), "
                "genre_credits(genre_folded, local_artist_id) AS ("
                "SELECT gt.genre_folded, lta.local_artist_id FROM genre_tracks gt "
                "JOIN local_track_artists lta ON lta.local_track_id = gt.id UNION "
                "SELECT gt.genre_folded, laa.local_artist_id FROM genre_tracks gt "
                "JOIN local_album_artists laa ON laa.local_album_id = gt.local_album_id UNION "
                "SELECT gt.genre_folded, a.album_artist_id FROM genre_tracks gt "
                "JOIN local_albums a ON a.id = gt.local_album_id) "
                "SELECT genre_folded, COUNT(DISTINCT local_artist_id) AS cnt "
                "FROM genre_credits GROUP BY genre_folded "
                "ORDER BY cnt DESC, genre_folded LIMIT ?",
                (max(1, limit),),
            ).fetchall()
            return [(str(row["genre_folded"]), int(row["cnt"])) for row in rows]

        return await self._read(operation)

    async def get_target_genre_artist_counts(self, genres: list[str]) -> dict[str, int]:
        normalized = list(
            dict.fromkeys(folded for genre in genres if (folded := _fold(genre)))
        )
        if not normalized:
            return {}

        def operation(connection: sqlite3.Connection) -> dict[str, int]:
            placeholders = ",".join("?" for _ in normalized)
            rows = connection.execute(
                "WITH genre_tracks AS (SELECT id, local_album_id, genre_folded "
                "FROM local_tracks "
                "WHERE availability = 'indexed' "
                f"AND genre_folded IN ({placeholders})), "
                "genre_credits(genre_folded, local_artist_id) AS ("
                "SELECT gt.genre_folded, lta.local_artist_id FROM genre_tracks gt "
                "JOIN local_track_artists lta ON lta.local_track_id = gt.id UNION "
                "SELECT gt.genre_folded, laa.local_artist_id FROM genre_tracks gt "
                "JOIN local_album_artists laa ON laa.local_album_id = gt.local_album_id UNION "
                "SELECT gt.genre_folded, a.album_artist_id FROM genre_tracks gt "
                "JOIN local_albums a ON a.id = gt.local_album_id) "
                "SELECT genre_folded, COUNT(DISTINCT local_artist_id) AS cnt "
                "FROM genre_credits GROUP BY genre_folded",
                normalized,
            ).fetchall()
            return {str(row["genre_folded"]): int(row["cnt"]) for row in rows}

        return await self._read(operation)

    async def get_target_artists_for_genres(
        self, genres: list[str]
    ) -> dict[str, list[str]]:
        normalized = list(
            dict.fromkeys(folded for genre in genres if (folded := _fold(genre)))
        )
        if not normalized:
            return {}

        def operation(connection: sqlite3.Connection) -> dict[str, list[str]]:
            placeholders = ",".join("?" for _ in normalized)
            rows = connection.execute(
                "WITH genre_tracks AS (SELECT id, local_album_id, genre_folded "
                "FROM local_tracks "
                "WHERE availability = 'indexed' "
                f"AND genre_folded IN ({placeholders})), "
                "genre_credits(genre_folded, local_artist_id) AS ("
                "SELECT gt.genre_folded, lta.local_artist_id FROM genre_tracks gt "
                "JOIN local_track_artists lta ON lta.local_track_id = gt.id UNION "
                "SELECT gt.genre_folded, laa.local_artist_id FROM genre_tracks gt "
                "JOIN local_album_artists laa ON laa.local_album_id = gt.local_album_id UNION "
                "SELECT gt.genre_folded, a.album_artist_id FROM genre_tracks gt "
                "JOIN local_albums a ON a.id = gt.local_album_id) "
                "SELECT DISTINCT gc.genre_folded, "
                "LOWER(aie.provider_artist_id) AS artist_mbid "
                "FROM genre_credits gc JOIN local_artist_external_identities aie "
                "ON aie.local_artist_id = gc.local_artist_id "
                "AND aie.provider = 'musicbrainz' "
                "ORDER BY gc.genre_folded, artist_mbid",
                normalized,
            ).fetchall()
            result: dict[str, list[str]] = {}
            for row in rows:
                result.setdefault(str(row["genre_folded"]), []).append(
                    str(row["artist_mbid"])
                )
            return result

        return await self._read(operation)

    async def get_target_genres_for_artists(
        self, artist_mbids: list[str]
    ) -> dict[str, list[str]]:
        normalized = list(
            dict.fromkeys(
                mbid.strip().casefold() for mbid in artist_mbids if mbid.strip()
            )
        )
        if not normalized:
            return {}

        def operation(connection: sqlite3.Connection) -> dict[str, list[str]]:
            placeholders = ",".join("?" for _ in normalized)
            rows = connection.execute(
                "SELECT artist_mbid_lower, genres_json FROM artist_genres "
                f"WHERE artist_mbid_lower IN ({placeholders})",
                normalized,
            ).fetchall()
            result: dict[str, list[str]] = {}
            for row in rows:
                try:
                    decoded = json.loads(str(row["genres_json"]))
                except (json.JSONDecodeError, TypeError):
                    continue
                if isinstance(decoded, list):
                    result[str(row["artist_mbid_lower"])] = [
                        str(value).strip()
                        for value in decoded
                        if isinstance(value, str) and value.strip()
                    ]
            return result

        return await self._read(operation)

    async def get_target_underrepresented_genres(
        self, known_genres: list[str], *, threshold: int
    ) -> list[str]:
        known = {folded for genre in known_genres if (folded := _fold(genre))}
        rows = await self.get_target_top_genres(limit=100_000)
        return [
            genre
            for genre, count in rows
            if 1 <= count < max(1, threshold) and genre not in known
        ]

    async def get_target_track(self, track_id: str) -> dict[str, Any] | None:
        def operation(connection: sqlite3.Connection) -> dict[str, Any] | None:
            resolved = self._resolve_target_id(
                connection, kind="track", identifier=track_id
            )
            if resolved is None:
                return None
            row = connection.execute(
                _TARGET_TRACK_SELECT + " WHERE t.id = ?", (resolved,)
            ).fetchone()
            return _row(row)

        return await self._read(operation)

    async def get_target_track_by_path(self, file_path: str) -> dict[str, Any] | None:
        def operation(connection: sqlite3.Connection) -> dict[str, Any] | None:
            row = connection.execute(
                _TARGET_TRACK_SELECT + " WHERE t.file_path = ? ORDER BY t.id LIMIT 1",
                (file_path,),
            ).fetchone()
            return _row(row)

        return await self._read(operation)

    async def get_target_imported_track(
        self, download_task_id: str, source_path: str
    ) -> dict[str, Any] | None:
        normalized = source_path.replace("\\", "/").strip("/")

        def operation(connection: sqlite3.Connection) -> dict[str, Any] | None:
            rows = connection.execute(
                _TARGET_TRACK_SELECT
                + " WHERE t.download_task_id = ? AND t.availability = 'indexed' "
                "ORDER BY t.id",
                (download_task_id,),
            ).fetchall()
            for row in rows:
                stored = str(row["source_path"] or "").replace("\\", "/")
                if stored == normalized or stored.endswith("/" + normalized):
                    return dict(row)
            return None

        return await self._read(operation)

    async def get_target_attributions_for_paths(
        self, file_paths: list[str]
    ) -> dict[str, dict[str, Any]]:
        if not file_paths:
            return {}

        def operation(connection: sqlite3.Connection) -> dict[str, dict[str, Any]]:
            placeholders = ",".join("?" for _ in file_paths)
            rows = connection.execute(
                _TARGET_TRACK_SELECT + f" WHERE t.file_path IN ({placeholders})",
                tuple(file_paths),
            ).fetchall()
            return {str(row["file_path"]): dict(row) for row in rows}

        return await self._read(operation)

    async def find_target_artist_by_name(self, display_name: str) -> str | None:
        folded = _fold(display_name)

        def operation(connection: sqlite3.Connection) -> str | None:
            rows = connection.execute(
                "SELECT id FROM local_artists WHERE folded_name = ? "
                "ORDER BY created_at, id LIMIT 2",
                (folded,),
            ).fetchall()
            return str(rows[0]["id"]) if len(rows) == 1 else None

        return await self._read(operation)

    async def get_target_tracks_under_paths(
        self, scope_paths: list[str]
    ) -> list[dict[str, Any]]:
        if not scope_paths:
            return []

        def operation(connection: sqlite3.Connection) -> list[dict[str, Any]]:
            clauses: list[str] = []
            parameters: list[str] = []
            for scope in scope_paths:
                prefix = scope.rstrip("/")
                escaped = _escape_like(prefix)
                clauses.append("(t.file_path = ? OR t.file_path LIKE ? ESCAPE '\\')")
                parameters.extend((prefix, f"{escaped}/%"))
            rows = connection.execute(
                _TARGET_TRACK_SELECT
                + " WHERE t.availability = 'indexed' AND ("
                + " OR ".join(clauses)
                + ") ORDER BY t.file_path, t.id",
                tuple(parameters),
            ).fetchall()
            return rows

        return await self._read(operation)

    async def get_target_tracks_by_ids(
        self, track_ids: list[str]
    ) -> dict[str, dict[str, Any]]:
        if not track_ids:
            return {}

        def operation(
            connection: sqlite3.Connection,
        ) -> dict[str, dict[str, Any]]:
            placeholders = ",".join("?" for _ in track_ids)
            rows = connection.execute(
                _TARGET_TRACK_SELECT + f" WHERE t.id IN ({placeholders})", track_ids
            ).fetchall()
            return {str(row["id"]): dict(row) for row in rows}

        return await self._read(operation)

    async def update_target_track_tags(
        self,
        track_id: str,
        *,
        tag: Any,
        info: Any,
        file_size_bytes: int,
        file_mtime_ns: int,
        stat_revision: str,
        tag_revision: str,
        actor_user_id: str,
        updated_at: float,
    ) -> int:
        """Commit an explicit administrator tag edit to the target projection."""

        def operation(connection: sqlite3.Connection) -> int:
            row = connection.execute(
                "SELECT id, local_album_id, title, artist_name, album_title, "
                "row_revision FROM local_tracks WHERE id = ? AND availability = 'indexed'",
                (track_id,),
            ).fetchone()
            if row is None:
                raise ResourceNotFoundError("Library track not found.")
            before = {
                "title": row["title"],
                "artist": row["artist_name"],
                "album": row["album_title"],
                "row_revision": int(row["row_revision"]),
            }
            connection.execute(
                "UPDATE local_tracks SET file_size_bytes = ?, file_mtime_ns = ?, "
                "stat_revision = ?, stat_revision_kind = 'exact', "
                "tag_revision = ?, tags_read_at = ?, "
                "metadata_incomplete = 0, title = ?, title_folded = ?, artist_name = ?, "
                "artist_name_folded = ?, album_title = ?, album_title_folded = ?, "
                "album_artist_name = ?, album_artist_name_folded = ?, tag_album_title = ?, "
                "tag_album_artist_name = ?, disc_number = ?, track_number = ?, year = ?, "
                "genre = ?, genre_folded = ?, title_sort = ?, artist_sort = ?, album_sort = ?, "
                "album_artist_sort = ?, disc_subtitle = ?, is_compilation = ?, "
                "embedded_release_group_mbid = ?, embedded_release_mbid = ?, "
                "embedded_recording_mbid = ?, embedded_artist_mbid = ?, "
                "embedded_album_artist_mbid = ?, duration_seconds = ?, file_format = ?, "
                "bit_rate = ?, sample_rate = ?, bit_depth = ?, channels = ?, "
                "replaygain_track_gain = ?, replaygain_album_gain = ?, "
                "replaygain_track_peak = ?, replaygain_album_peak = ?, "
                "row_revision = row_revision + 1 WHERE id = ?",
                (
                    file_size_bytes,
                    file_mtime_ns,
                    stat_revision,
                    tag_revision,
                    updated_at,
                    tag.title,
                    _fold(tag.title),
                    tag.artist,
                    _fold(tag.artist),
                    tag.album,
                    _fold(tag.album),
                    tag.album_artist or tag.artist,
                    _fold(tag.album_artist or tag.artist),
                    tag.album,
                    tag.album_artist or "",
                    tag.disc_number,
                    tag.track_number,
                    tag.year,
                    tag.genre,
                    _fold(tag.genre),
                    tag.title_sort,
                    tag.artist_sort,
                    tag.album_sort,
                    tag.album_artist_sort,
                    tag.disc_subtitle,
                    int(tag.compilation),
                    tag.musicbrainz_release_group_id,
                    tag.musicbrainz_release_id,
                    tag.musicbrainz_recording_id,
                    tag.musicbrainz_artist_id,
                    tag.musicbrainz_album_artist_id,
                    info.duration_seconds,
                    info.file_format,
                    info.bitrate,
                    info.sample_rate,
                    info.bit_depth,
                    info.channels,
                    tag.replaygain_track_gain,
                    tag.replaygain_album_gain,
                    tag.replaygain_track_peak,
                    tag.replaygain_album_peak,
                    track_id,
                ),
            )
            after_revision = int(row["row_revision"]) + 1
            connection.execute(
                "INSERT INTO library_catalog_actions "
                "(id, actor_user_id, action_kind, local_album_id, local_track_id, "
                "before_json, after_json, reason_code, created_at) "
                "VALUES (?,?,?,?,?,?,?,?,?)",
                (
                    str(uuid.uuid4()),
                    actor_user_id,
                    "update_track_tags",
                    str(row["local_album_id"]),
                    track_id,
                    json.dumps(before, separators=(",", ":"), sort_keys=True),
                    json.dumps(
                        {
                            "title": tag.title,
                            "artist": tag.artist,
                            "album": tag.album,
                            "row_revision": after_revision,
                        },
                        separators=(",", ":"),
                        sort_keys=True,
                    ),
                    "EXPLICIT_TAG_EDIT",
                    updated_at,
                ),
            )
            self._bump_catalog(connection)
            return after_revision

        return await self._write(operation)

    @staticmethod
    def _migration_reference_matches(
        connection: sqlite3.Connection, provenance: MigrationProvenance
    ) -> bool:
        target_tables = {
            "local_track": "local_tracks",
            "local_album": "local_albums",
            "local_artist": "local_artists",
            "playlist": "library_playlists",
            "reference_tombstone": "library_reference_tombstones",
        }
        if provenance.target_kind == "library_root":
            target_exists = provenance.source_kind == "root" and bool(
                provenance.target_id
            )
        elif provenance.target_kind in {"library", "genre"}:
            target_exists = provenance.source_kind == "jellyfin_id_map" and bool(
                provenance.target_id
            )
        elif provenance.target_kind == "compat_play_queue":
            target_exists = connection.execute(
                "SELECT 1 FROM library_compat_play_queues WHERE user_id = ?",
                (provenance.target_id,),
            ).fetchone()
        else:
            table = target_tables.get(provenance.target_kind)
            target_exists = (
                connection.execute(
                    f"SELECT 1 FROM {table} WHERE id = ?", (provenance.target_id,)
                ).fetchone()
                if table is not None
                else None
            )
        if not target_exists:
            return False

        kind = provenance.source_kind
        if kind == "favorite":
            user_id, item_kind, _source_id = provenance.source_key.split(":", 2)
            return (
                connection.execute(
                    "SELECT 1 FROM library_user_favorites WHERE user_id = ? "
                    "AND item_kind = ? AND item_id = ?",
                    (user_id, item_kind, provenance.target_id),
                ).fetchone()
                is not None
            )
        if kind == "history":
            column = {
                "local_track": "local_track_id",
                "local_album": "local_album_id",
                "local_artist": "local_artist_id",
            }.get(provenance.target_kind)
            return bool(
                column
                and connection.execute(
                    f"SELECT 1 FROM library_play_history WHERE id = ? AND {column} = ?",
                    (provenance.source_key, provenance.target_id),
                ).fetchone()
            )
        if kind == "playlist_track":
            column = {
                "local_track": "local_track_id",
                "local_album": "local_album_id",
                "local_artist": "local_artist_id",
                "reference_tombstone": "reference_tombstone_id",
            }.get(provenance.target_kind)
            return bool(
                column
                and connection.execute(
                    f"SELECT 1 FROM library_playlist_tracks WHERE id = ? AND {column} = ?",
                    (provenance.source_key, provenance.target_id),
                ).fetchone()
            )
        if kind == "album_release_pin":
            return (
                connection.execute(
                    "SELECT 1 FROM library_album_release_pins WHERE local_album_id = ? "
                    "AND release_group_mbid = ?",
                    (provenance.target_id, provenance.source_key),
                ).fetchone()
                is not None
            )
        if kind == "compat_bookmark":
            user_id, _source_id = provenance.source_key.split(":", 1)
            return (
                connection.execute(
                    "SELECT 1 FROM library_compat_bookmarks WHERE user_id = ? "
                    "AND local_track_id = ?",
                    (user_id, provenance.target_id),
                ).fetchone()
                is not None
            )
        if kind == "compat_play_queue":
            return provenance.source_key == provenance.target_id
        if kind == "compat_play_queue_item":
            user_id, item_index = provenance.source_key.rsplit(":", 1)
            return (
                connection.execute(
                    "SELECT 1 FROM library_compat_play_queue_items WHERE user_id = ? "
                    "AND item_index = ? AND local_track_id = ?",
                    (user_id, int(item_index), provenance.target_id),
                ).fetchone()
                is not None
            )
        if kind == "jellyfin_id_map":
            if provenance.target_kind == "reference_tombstone":
                source = connection.execute(
                    "SELECT kind, internal_id FROM compat_id_map WHERE jf_id = ?",
                    (provenance.source_key,),
                ).fetchone()
                if source is None:
                    return False
                return (
                    connection.execute(
                        "SELECT 1 FROM library_compat_id_map WHERE jf_id = ? "
                        "AND kind = ? AND internal_id = ?",
                        (
                            provenance.source_key,
                            source["kind"],
                            source["internal_id"],
                        ),
                    ).fetchone()
                    is not None
                )
            target_kind = {
                "local_artist": "artist",
                "local_album": "album",
                "local_track": "track",
            }.get(provenance.target_kind, provenance.target_kind)
            return (
                connection.execute(
                    "SELECT 1 FROM library_compat_id_map WHERE jf_id = ? AND kind = ? "
                    "AND internal_id = ?",
                    (provenance.source_key, target_kind, provenance.target_id),
                ).fetchone()
                is not None
            )
        if kind in {"review_row", "manual_decision"}:
            return (
                connection.execute(
                    "SELECT 1 FROM library_identification_reviews "
                    "WHERE local_track_id = ? AND reason_code LIKE 'legacy_%'",
                    (provenance.target_id,),
                ).fetchone()
                is not None
            )
        if kind == "native_album_alias":
            return (
                connection.execute(
                    "SELECT 1 FROM local_album_aliases WHERE alias = ? AND local_album_id = ?",
                    (provenance.source_key, provenance.target_id),
                ).fetchone()
                is not None
            )
        if kind == "native_artist_alias":
            return (
                connection.execute(
                    "SELECT 1 FROM local_artist_aliases WHERE alias = ? AND local_artist_id = ?",
                    (provenance.source_key, provenance.target_id),
                ).fetchone()
                is not None
            )
        if kind == "artwork_reference":
            return (
                connection.execute(
                    "SELECT 1 FROM local_album_artwork WHERE local_album_id = ?",
                    (provenance.target_id,),
                ).fetchone()
                is not None
            )
        return kind in {"root", "library_file", "subsonic_id"}

    async def mark_target_tracks_missing(
        self,
        track_ids: list[str],
        *,
        actor_user_id: str | None,
        reason_code: str,
        missing_at: float,
    ) -> list[str]:
        """Retain stable rows and references while removing tracks from projections."""
        ordered = list(dict.fromkeys(track_ids))

        def operation(connection: sqlite3.Connection) -> list[str]:
            changed: list[str] = []
            for track_id in ordered:
                row = connection.execute(
                    "SELECT id, local_album_id, availability, row_revision "
                    "FROM local_tracks WHERE id = ?",
                    (track_id,),
                ).fetchone()
                if row is None or row["availability"] != "indexed":
                    continue
                connection.execute(
                    "UPDATE local_tracks SET availability = 'missing', missing_since = ?, "
                    "row_revision = row_revision + 1 WHERE id = ?",
                    (missing_at, track_id),
                )
                connection.execute(
                    "INSERT INTO library_catalog_actions "
                    "(id, actor_user_id, action_kind, local_album_id, local_track_id, "
                    "before_json, after_json, reason_code, created_at) "
                    "VALUES (?,?,?,?,?,?,?,?,?)",
                    (
                        str(uuid.uuid4()),
                        actor_user_id,
                        "remove_track",
                        str(row["local_album_id"]),
                        track_id,
                        json.dumps(
                            {
                                "availability": "indexed",
                                "row_revision": int(row["row_revision"]),
                            },
                            separators=(",", ":"),
                            sort_keys=True,
                        ),
                        json.dumps(
                            {
                                "availability": "missing",
                                "row_revision": int(row["row_revision"]) + 1,
                            },
                            separators=(",", ":"),
                            sort_keys=True,
                        ),
                        reason_code,
                        missing_at,
                    ),
                )
                changed.append(track_id)
            if changed:
                self._bump_catalog(connection)
            return changed

        return await self._write(operation)

    async def get_target_album_tracks(
        self, album_identifier: str, *, include_unavailable: bool = False
    ) -> list[dict[str, Any]]:
        def operation(connection: sqlite3.Connection) -> list[dict[str, Any]]:
            direct = connection.execute(
                "SELECT COALESCE(retired_into_album_id, id) AS id "
                "FROM local_albums WHERE id = ?",
                (album_identifier,),
            ).fetchone()
            if direct is None:
                direct = connection.execute(
                    "SELECT local_album_id AS id FROM local_album_aliases "
                    "WHERE alias = ?",
                    (album_identifier.casefold(),),
                ).fetchone()
            if direct is not None:
                album_ids = [str(direct["id"])]
            else:
                album_ids = [
                    str(row["local_album_id"])
                    for row in connection.execute(
                        "SELECT identity.local_album_id "
                        "FROM local_album_external_identities identity "
                        "JOIN local_albums album ON album.id = identity.local_album_id "
                        "WHERE LOWER(identity.release_group_mbid) = LOWER(?) "
                        "AND album.retired_into_album_id IS NULL "
                        "ORDER BY identity.local_album_id",
                        (album_identifier,),
                    ).fetchall()
                ]
            if not album_ids:
                return []
            availability = (
                "" if include_unavailable else " AND t.availability = 'indexed'"
            )
            placeholders = ",".join("?" for _ in album_ids)
            rows = connection.execute(
                _TARGET_TRACK_SELECT
                + f" WHERE a.id IN ({placeholders})"
                + availability
                + " ORDER BY a.id, t.disc_number, t.track_number, t.id",
                album_ids,
            ).fetchall()
            return [dict(row) for row in rows]

        return await self._read(operation)

    async def get_target_recording_tracks(
        self, track_identifier: str, *, include_unavailable: bool = False
    ) -> list[dict[str, Any]]:
        def operation(connection: sqlite3.Connection) -> list[dict[str, Any]]:
            direct = connection.execute(
                "SELECT id FROM local_tracks WHERE id = ?", (track_identifier,)
            ).fetchone()
            if direct is not None:
                track_ids = [str(direct["id"])]
            else:
                track_ids = [
                    str(row["local_track_id"])
                    for row in connection.execute(
                        "SELECT identity.local_track_id "
                        "FROM local_track_external_identities identity "
                        "WHERE LOWER(identity.recording_mbid) = LOWER(?) "
                        "ORDER BY identity.local_track_id",
                        (track_identifier,),
                    ).fetchall()
                ]
            if not track_ids:
                return []
            placeholders = ",".join("?" for _ in track_ids)
            availability = (
                "" if include_unavailable else " AND t.availability = 'indexed'"
            )
            rows = connection.execute(
                _TARGET_TRACK_SELECT
                + f" WHERE t.id IN ({placeholders})"
                + availability
                + " ORDER BY t.id",
                track_ids,
            ).fetchall()
            return [dict(row) for row in rows]

        return await self._read(operation)

    async def list_target_albums(
        self,
        *,
        limit: int = 50,
        offset: int = 0,
        sort: str = "recent",
        search: str | None = None,
        from_year: int | None = None,
        to_year: int | None = None,
        genre: str | None = None,
        artist_id: str | None = None,
        album_ids: list[str] | None = None,
        file_format: str | None = None,
    ) -> tuple[list[dict[str, Any]], int]:
        def operation(
            connection: sqlite3.Connection,
        ) -> tuple[list[dict[str, Any]], int]:
            clauses = ["a.retired_into_album_id IS NULL", "t.availability = 'indexed'"]
            parameters: list[Any] = []
            if search:
                folded = f"%{_fold(search) or ''}%"
                clauses.append(
                    "(a.title_folded LIKE ? OR a.album_artist_name_folded LIKE ?)"
                )
                parameters.extend((folded, folded))
            if from_year is not None:
                clauses.append("a.year >= ?")
                parameters.append(from_year)
            if to_year is not None:
                clauses.append("a.year <= ?")
                parameters.append(to_year)
            if genre:
                clauses.append("t.genre_folded = ?")
                parameters.append(_fold(genre))
            if file_format:
                clauses.append("LOWER(t.file_format) = LOWER(?)")
                parameters.append(file_format)
            if album_ids:
                resolved_albums = list(
                    dict.fromkeys(
                        album_id
                        for value in album_ids
                        for album_id in self._resolve_target_membership_ids(
                            connection, kind="album", identifier=value
                        )
                    )
                )
                if not resolved_albums:
                    return [], 0
                placeholders = ",".join("?" for _ in resolved_albums)
                clauses.append(f"a.id IN ({placeholders})")
                parameters.extend(resolved_albums)
            if artist_id:
                resolved_artists = self._resolve_target_membership_ids(
                    connection, kind="artist", identifier=artist_id
                )
                if not resolved_artists:
                    return [], 0
                placeholders = ",".join("?" for _ in resolved_artists)
                clauses.append(
                    "(EXISTS (SELECT 1 FROM local_album_artists laa "
                    "WHERE laa.local_album_id = a.id "
                    f"AND laa.local_artist_id IN ({placeholders})) "
                    "OR EXISTS (SELECT 1 FROM local_track_artists lta "
                    "JOIN local_tracks credited_track ON credited_track.id = lta.local_track_id "
                    "WHERE credited_track.local_album_id = a.id "
                    f"AND lta.local_artist_id IN ({placeholders})))"
                )
                parameters.extend(resolved_artists)
                parameters.extend(resolved_artists)
            where = " AND ".join(clauses)
            total = int(
                connection.execute(
                    "SELECT COUNT(DISTINCT a.id) FROM local_albums a "
                    "JOIN local_tracks t ON t.local_album_id = a.id WHERE " + where,
                    parameters,
                ).fetchone()[0]
            )
            ordering = {
                "recent": "MAX(t.imported_at) DESC, a.id",
                "newest": "COALESCE(a.year, 0) DESC, a.title_folded, a.id",
                "oldest": "COALESCE(a.year, 0), a.title_folded, a.id",
                "name": "a.title_folded, a.id",
                "artist": "a.album_artist_name_folded, a.title_folded, a.id",
                "random": "a.id",
            }.get(sort, "MAX(t.imported_at) DESC, a.id")
            rows = connection.execute(
                "SELECT a.id AS release_group_mbid, a.title AS album_title, "
                "a.album_artist_name, a.album_artist_id AS album_artist_mbid, "
                "a.year, a.is_compilation, a.original_release_date, "
                "a.album_artist_sort_name, COUNT(t.id) AS track_count, "
                "SUM(t.file_size_bytes) AS total_size_bytes, "
                "SUM(COALESCE(t.duration_seconds, 0)) AS total_duration_seconds, "
                "MAX(t.imported_at) AS last_imported_at, "
                "MAX(t.file_format) AS file_format, MAX(t.album_sort) AS album_sort_name, "
                "GROUP_CONCAT(DISTINCT NULLIF(t.genre, '')) AS genres, "
                "ae.release_group_mbid AS provider_release_group_mbid, "
                "ae.release_mbid AS provider_release_mbid, "
                "aie.provider_artist_id AS provider_artist_mbid, artwork.cover_url, "
                "artwork.source AS artwork_source, contribution.id AS contribution_id, "
                "contribution.state AS contribution_state "
                "FROM local_albums a JOIN local_tracks t ON t.local_album_id = a.id "
                "LEFT JOIN local_album_external_identities ae "
                "ON ae.local_album_id = a.id AND ae.provider = 'musicbrainz' "
                "LEFT JOIN local_artist_external_identities aie "
                "ON aie.local_artist_id = a.album_artist_id "
                "AND aie.provider = 'musicbrainz' "
                "LEFT JOIN local_album_artwork artwork ON artwork.local_album_id = a.id "
                "LEFT JOIN library_contribution_drafts contribution "
                "ON contribution.local_album_id = a.id "
                "AND contribution.state NOT IN ('linked','cancelled','stale') "
                "WHERE "
                + where
                + " GROUP BY a.id ORDER BY "
                + ordering
                + " LIMIT ? OFFSET ?",
                (*parameters, max(1, limit), max(0, offset)),
            ).fetchall()
            return [dict(row) for row in rows], total

        return await self._read(operation)

    async def list_target_artists(
        self,
        *,
        limit: int = 50,
        offset: int = 0,
        search: str | None = None,
        sort_by: str = "name",
        sort_order: str = "asc",
        artist_ids: list[str] | None = None,
    ) -> tuple[list[dict[str, Any]], int]:
        def operation(
            connection: sqlite3.Connection,
        ) -> tuple[list[dict[str, Any]], int]:
            clauses = [
                "ar.retired_into_artist_id IS NULL",
                "t.availability = 'indexed'",
            ]
            parameters: list[Any] = []
            if search:
                clauses.append("ar.folded_name LIKE ?")
                parameters.append(f"%{_fold(search) or ''}%")
            if artist_ids:
                resolved_artists = list(
                    dict.fromkeys(
                        artist_id
                        for value in artist_ids
                        for artist_id in self._resolve_target_membership_ids(
                            connection, kind="artist", identifier=value
                        )
                    )
                )
                if not resolved_artists:
                    return [], 0
                placeholders = ",".join("?" for _ in resolved_artists)
                clauses.append(f"ar.id IN ({placeholders})")
                parameters.extend(resolved_artists)
            where = " AND ".join(clauses)
            joins = (
                " FROM local_artists ar "
                "JOIN ("
                "SELECT aa.local_artist_id, t0.local_album_id, t0.id AS local_track_id "
                "FROM local_album_artists aa JOIN local_tracks t0 "
                "ON t0.local_album_id = aa.local_album_id "
                "UNION "
                "SELECT ta.local_artist_id, t1.local_album_id, t1.id AS local_track_id "
                "FROM local_track_artists ta JOIN local_tracks t1 "
                "ON t1.id = ta.local_track_id"
                ") credit ON credit.local_artist_id = ar.id "
                "JOIN local_albums a ON a.id = credit.local_album_id "
                "JOIN local_tracks t ON t.id = credit.local_track_id "
            )
            total = int(
                connection.execute(
                    "SELECT COUNT(DISTINCT ar.id)" + joins + "WHERE " + where,
                    parameters,
                ).fetchone()[0]
            )
            direction = "DESC" if sort_order == "desc" else "ASC"
            order_expression = {
                "album_count": "album_count",
                "date_added": "date_added",
            }.get(sort_by, "ar.folded_name")
            rows = connection.execute(
                "SELECT ar.id AS artist_mbid, ar.display_name AS artist_name, "
                "ar.row_revision, "
                "ar.sort_name, COUNT(DISTINCT a.id) AS album_count, "
                "COUNT(DISTINCT t.id) AS track_count, MIN(t.imported_at) AS date_added, "
                "aie.provider_artist_id AS provider_artist_mbid"
                + joins
                + "LEFT JOIN local_artist_external_identities aie "
                "ON aie.local_artist_id = ar.id AND aie.provider = 'musicbrainz' "
                "WHERE "
                + where
                + f" GROUP BY ar.id ORDER BY {order_expression} {direction}, "
                f"ar.folded_name {direction}, ar.id {direction} "
                "LIMIT ? OFFSET ?",
                (*parameters, max(1, limit), max(0, offset)),
            ).fetchall()
            return [dict(row) for row in rows], total

        return await self._read(operation)

    async def list_target_tracks(
        self,
        *,
        limit: int = 50,
        offset: int = 0,
        sort: str = "recent",
        search: str | None = None,
        genre: str | None = None,
        from_year: int | None = None,
        to_year: int | None = None,
        artist_name: str | None = None,
        artist_ids: list[str] | None = None,
        album_artist_only: bool = False,
    ) -> tuple[list[dict[str, Any]], int]:
        def operation(
            connection: sqlite3.Connection,
        ) -> tuple[list[dict[str, Any]], int]:
            clauses = ["t.availability = 'indexed'", "a.retired_into_album_id IS NULL"]
            parameters: list[Any] = []
            if search:
                folded = f"%{_fold(search) or ''}%"
                clauses.append(
                    "(t.title_folded LIKE ? OR t.artist_name_folded LIKE ? "
                    "OR t.album_title_folded LIKE ?)"
                )
                parameters.extend((folded, folded, folded))
            if genre:
                clauses.append("t.genre_folded = ?")
                parameters.append(_fold(genre))
            if from_year is not None:
                clauses.append("t.year >= ?")
                parameters.append(from_year)
            if to_year is not None:
                clauses.append("t.year <= ?")
                parameters.append(to_year)
            if artist_name:
                clauses.append("t.artist_name_folded = ?")
                parameters.append(_fold(artist_name))
            if artist_ids:
                resolved = list(
                    dict.fromkeys(
                        artist_id
                        for value in artist_ids
                        for artist_id in self._resolve_target_membership_ids(
                            connection, kind="artist", identifier=value
                        )
                    )
                )
                if not resolved:
                    return [], 0
                placeholders = ",".join("?" for _ in resolved)
                if album_artist_only:
                    clauses.append(f"a.album_artist_id IN ({placeholders})")
                else:
                    clauses.append(
                        "(a.album_artist_id IN ("
                        + placeholders
                        + ") OR EXISTS (SELECT 1 FROM local_track_artists fta "
                        "WHERE fta.local_track_id = t.id AND fta.local_artist_id IN ("
                        + placeholders
                        + ")))"
                    )
                    parameters.extend(resolved)
                parameters.extend(resolved)
            where = " AND ".join(clauses)
            total = int(
                connection.execute(
                    "SELECT COUNT(*) FROM local_tracks t "
                    "JOIN local_albums a ON a.id = t.local_album_id WHERE " + where,
                    parameters,
                ).fetchone()[0]
            )
            ordering = {
                "recent": "t.imported_at DESC, t.id",
                "title": "t.title_folded, t.id",
                "artist": "t.artist_name_folded, t.title_folded, t.id",
                "album": "t.album_title_folded, t.disc_number, t.track_number, t.id",
                "random": "RANDOM()",
            }.get(sort, "t.imported_at DESC, t.id")
            rows = connection.execute(
                _TARGET_TRACK_SELECT
                + " WHERE "
                + where
                + " ORDER BY "
                + ordering
                + " LIMIT ? OFFSET ?",
                (*parameters, max(1, limit), max(0, offset)),
            ).fetchall()
            return [dict(row) for row in rows], total

        return await self._read(operation)

    async def target_provider_album_snapshot(self) -> tuple[int, set[str]]:
        """Return one coalesced provider-album snapshot per catalog revision."""
        revision = await self.get_catalog_revision()
        cached = self._provider_album_snapshot
        if cached is not None and cached[0] == revision:
            return revision, set(cached[1])

        async with self._provider_album_snapshot_lock:
            revision = await self.get_catalog_revision()
            cached = self._provider_album_snapshot
            if cached is not None and cached[0] == revision:
                return revision, set(cached[1])

            def operation(connection: sqlite3.Connection) -> tuple[int, set[str]]:
                connection.execute("BEGIN")
                current = int(
                    connection.execute(
                        "SELECT value FROM library_catalog_revision WHERE singleton = 1"
                    ).fetchone()[0]
                )
                rows = connection.execute(
                    "SELECT identity.release_group_mbid "
                    "FROM local_album_external_identities identity "
                    "JOIN local_albums album ON album.id = identity.local_album_id "
                    "WHERE identity.provider = 'musicbrainz' "
                    "AND album.retired_into_album_id IS NULL "
                    "AND EXISTS (SELECT 1 FROM local_tracks track "
                    "WHERE track.local_album_id = identity.local_album_id "
                    "AND track.availability = 'indexed') "
                    "ORDER BY identity.release_group_mbid"
                ).fetchall()
                return current, {str(row["release_group_mbid"]) for row in rows}

            revision, values = await self._read(operation)
            self._provider_album_snapshot = (revision, frozenset(values))
            return revision, set(values)

    async def target_provider_release_ids(self) -> set[str]:
        def operation(connection: sqlite3.Connection) -> set[str]:
            rows = connection.execute(
                "SELECT identity.release_mbid "
                "FROM local_album_external_identities identity "
                "JOIN local_albums album ON album.id = identity.local_album_id "
                "WHERE identity.provider = 'musicbrainz' "
                "AND identity.release_mbid IS NOT NULL "
                "AND album.retired_into_album_id IS NULL "
                "AND EXISTS (SELECT 1 FROM local_tracks track "
                "WHERE track.local_album_id = identity.local_album_id "
                "AND track.availability = 'indexed') "
                "ORDER BY identity.release_mbid"
            ).fetchall()
            return {str(row["release_mbid"]) for row in rows}

        return await self._read(operation)

    async def target_provider_artist_ids(self) -> set[str]:
        def operation(connection: sqlite3.Connection) -> set[str]:
            rows = connection.execute(
                "SELECT identity.provider_artist_id "
                "FROM local_artist_external_identities identity "
                "WHERE EXISTS (SELECT 1 FROM local_album_artists credit "
                "JOIN local_tracks track ON track.local_album_id = credit.local_album_id "
                "WHERE credit.local_artist_id = identity.local_artist_id "
                "AND track.availability = 'indexed') "
                "OR EXISTS (SELECT 1 FROM local_track_artists credit "
                "JOIN local_tracks track ON track.id = credit.local_track_id "
                "WHERE credit.local_artist_id = identity.local_artist_id "
                "AND track.availability = 'indexed') "
                "ORDER BY identity.provider_artist_id"
            ).fetchall()
            return {str(row["provider_artist_id"]) for row in rows}

        return await self._read(operation)

    async def target_existing_provider_artist_ids(
        self, identifiers: list[str]
    ) -> set[str]:
        normalized = sorted(
            {value.strip().casefold() for value in identifiers if value.strip()}
        )
        if not normalized:
            return set()

        def operation(connection: sqlite3.Connection) -> set[str]:
            found: set[str] = set()
            for offset in range(0, len(normalized), 500):
                batch = normalized[offset : offset + 500]
                placeholders = ",".join("?" for _ in batch)
                rows = connection.execute(
                    "SELECT identity.provider_artist_id FROM "
                    "local_artist_external_identities identity WHERE "
                    f"lower(identity.provider_artist_id) IN ({placeholders}) AND ("
                    "EXISTS (SELECT 1 FROM local_album_artists credit "
                    "JOIN local_tracks track ON track.local_album_id=credit.local_album_id "
                    "WHERE credit.local_artist_id=identity.local_artist_id "
                    "AND track.availability='indexed') OR EXISTS ("
                    "SELECT 1 FROM local_track_artists credit JOIN local_tracks track "
                    "ON track.id=credit.local_track_id WHERE "
                    "credit.local_artist_id=identity.local_artist_id "
                    "AND track.availability='indexed'))",
                    batch,
                ).fetchall()
                found.update(str(row["provider_artist_id"]).casefold() for row in rows)
            return found

        return await self._read(operation)

    async def target_has_provider_artist(self, provider_artist_id: str) -> bool:
        def operation(connection: sqlite3.Connection) -> bool:
            return (
                connection.execute(
                    "SELECT 1 FROM local_artist_external_identities identity "
                    "WHERE LOWER(identity.provider_artist_id) = LOWER(?) AND ("
                    "EXISTS (SELECT 1 FROM local_album_artists credit "
                    "JOIN local_tracks track ON track.local_album_id = credit.local_album_id "
                    "WHERE credit.local_artist_id = identity.local_artist_id "
                    "AND track.availability = 'indexed') OR "
                    "EXISTS (SELECT 1 FROM local_track_artists credit "
                    "JOIN local_tracks track ON track.id = credit.local_track_id "
                    "WHERE credit.local_artist_id = identity.local_artist_id "
                    "AND track.availability = 'indexed')) LIMIT 1",
                    (provider_artist_id,),
                ).fetchone()
                is not None
            )

        return await self._read(operation)

    async def target_album_provider_identity(self, album_id: str) -> str | None:
        def operation(connection: sqlite3.Connection) -> str | None:
            row = connection.execute(
                "SELECT identity.release_group_mbid "
                "FROM local_album_external_identities identity "
                "LEFT JOIN local_album_aliases alias "
                "ON alias.local_album_id = identity.local_album_id "
                "WHERE identity.provider = 'musicbrainz' "
                "AND (identity.local_album_id = ? OR alias.alias = LOWER(?) "
                "OR LOWER(identity.release_group_mbid) = LOWER(?)) "
                "ORDER BY identity.local_album_id LIMIT 1",
                (album_id, album_id, album_id),
            ).fetchone()
            return str(row["release_group_mbid"]) if row is not None else None

        return await self._read(operation)

    async def target_album_ownership_rows(
        self,
        *,
        provider_ids: set[str] | None = None,
        folded_keys: set[tuple[str, str]] | None = None,
    ) -> list[dict[str, Any]]:
        """Return only ownership rows that can match the supplied candidates."""
        normalized_ids = sorted(
            {value.casefold() for value in provider_ids or set() if value}
        )
        normalized_keys = sorted(
            {
                (title, artist)
                for title, artist in folded_keys or set()
                if title and artist
            }
        )
        if not normalized_ids and not normalized_keys:
            return []

        def operation(connection: sqlite3.Connection) -> list[dict[str, Any]]:
            selected: dict[str, dict[str, Any]] = {}
            base = (
                "SELECT album.id AS local_album_id, album.title, "
                "album.album_artist_name, album.year, identity.release_group_mbid "
                "FROM local_albums album LEFT JOIN local_album_external_identities identity "
                "ON identity.local_album_id = album.id AND identity.provider = 'musicbrainz' "
                "WHERE album.retired_into_album_id IS NULL "
                "AND EXISTS (SELECT 1 FROM local_tracks track "
                "WHERE track.local_album_id = album.id "
                "AND track.availability = 'indexed') AND "
            )
            for offset in range(0, len(normalized_ids), 500):
                batch = normalized_ids[offset : offset + 500]
                placeholders = ",".join("?" for _ in batch)
                rows = connection.execute(
                    base + f"LOWER(identity.release_group_mbid) IN ({placeholders})",
                    batch,
                ).fetchall()
                selected.update((str(row["local_album_id"]), dict(row)) for row in rows)
            for offset in range(0, len(normalized_keys), 400):
                batch = normalized_keys[offset : offset + 400]
                placeholders = ",".join("(?, ?)" for _ in batch)
                parameters = [value for pair in batch for value in pair]
                rows = connection.execute(
                    base
                    + "(album.title_folded, album.album_artist_name_folded) IN ("
                    + placeholders
                    + ") AND identity.release_group_mbid IS NULL",
                    parameters,
                ).fetchall()
                selected.update((str(row["local_album_id"]), dict(row)) for row in rows)
            return [selected[key] for key in sorted(selected)]

        return await self._read(operation)

    async def target_has_any_tracks(self) -> bool:
        def operation(connection: sqlite3.Connection) -> bool:
            return (
                connection.execute(
                    "SELECT 1 FROM local_tracks WHERE availability = 'indexed' LIMIT 1"
                ).fetchone()
                is not None
            )

        return await self._read(operation)

    async def target_decades(self) -> list[dict[str, int]]:
        def operation(connection: sqlite3.Connection) -> list[dict[str, int]]:
            rows = connection.execute(
                "SELECT (a.year / 10) * 10 AS decade, COUNT(DISTINCT a.id) AS album_count "
                "FROM local_albums a JOIN local_tracks t ON t.local_album_id = a.id "
                "WHERE t.availability = 'indexed' AND a.year IS NOT NULL "
                "GROUP BY decade ORDER BY decade DESC"
            ).fetchall()
            return [
                {"decade": int(row["decade"]), "album_count": int(row["album_count"])}
                for row in rows
            ]

        return await self._read(operation)

    async def list_target_genres(self) -> list[dict[str, Any]]:
        def operation(connection: sqlite3.Connection) -> list[dict[str, Any]]:
            rows = connection.execute(
                "WITH spellings AS ("
                "SELECT COALESCE(genre_folded, fold(genre)) AS genre_key, genre, "
                "COUNT(*) AS spelling_count FROM local_tracks "
                "WHERE availability = 'indexed' AND genre IS NOT NULL AND trim(genre) != '' "
                "GROUP BY genre_key, genre), ranked AS ("
                "SELECT genre_key, genre, spelling_count, "
                "ROW_NUMBER() OVER (PARTITION BY genre_key "
                "ORDER BY spelling_count DESC, genre) AS spelling_rank "
                "FROM spellings), totals AS ("
                "SELECT COALESCE(genre_folded, fold(genre)) AS genre_key, "
                "COUNT(*) AS song_count, COUNT(DISTINCT local_album_id) AS album_count "
                "FROM local_tracks WHERE availability = 'indexed' "
                "AND genre IS NOT NULL AND trim(genre) != '' GROUP BY genre_key) "
                "SELECT ranked.genre, totals.song_count, totals.album_count "
                "FROM totals JOIN ranked USING (genre_key) WHERE spelling_rank = 1 "
                "ORDER BY genre_key, ranked.genre"
            ).fetchall()
            return [dict(row) for row in rows]

        return await self._read(operation)

    async def list_genre_artwork_candidates(
        self, genres: list[str]
    ) -> dict[str, dict[str, Any]]:
        """Read all visible genre artwork candidates in one bounded statement."""

        requested: list[tuple[str, str, int]] = []
        seen: set[str] = set()
        for display_name in genres[:20]:
            folded = _fold(display_name)
            if not folded or folded in seen:
                continue
            seen.add(folded)
            requested.append((folded, display_name, len(requested)))
        if not requested:
            return {}

        def operation(connection: sqlite3.Connection) -> dict[str, dict[str, Any]]:
            values = ",".join("(?,?,?)" for _ in requested)
            parameters = [value for row in requested for value in row]
            rows = connection.execute(
                f"WITH requested(genre_folded, display_name, ordinal) AS (VALUES {values}), "
                "candidates AS ("
                "SELECT r.genre_folded, r.display_name, r.ordinal, a.id AS album_id, "
                "a.title AS album_title, a.album_artist_name, a.album_artist_id, "
                "COUNT(*) AS match_count, artwork.source, artwork.source_locator, "
                "artwork.cover_url, artwork.version AS cover_version, "
                "ae.release_group_mbid AS provider_id, embedded.file_path AS embedded_file_path, "
                "embedded.availability AS embedded_file_availability "
                "FROM requested r JOIN local_tracks t "
                "ON t.genre_folded = r.genre_folded "
                "AND t.availability = 'indexed' "
                "JOIN local_albums a ON a.id = t.local_album_id "
                "AND a.retired_into_album_id IS NULL "
                "JOIN local_album_artwork artwork ON artwork.local_album_id = a.id "
                "LEFT JOIN local_album_external_identities ae "
                "ON ae.local_album_id = a.id AND ae.provider = 'musicbrainz' "
                "LEFT JOIN local_tracks embedded ON embedded.id = artwork.source_locator "
                "GROUP BY r.genre_folded, r.display_name, r.ordinal, a.id, a.title, "
                "a.album_artist_name, a.album_artist_id, artwork.source, "
                "artwork.source_locator, artwork.cover_url, artwork.version, "
                "ae.release_group_mbid, embedded.file_path, embedded.availability) "
                "SELECT r.genre_folded, r.display_name, r.ordinal, "
                "COALESCE(rev.value, 0) AS genre_revision, candidates.* "
                "FROM requested r LEFT JOIN library_genre_artwork_revisions rev "
                "ON rev.genre_folded = r.genre_folded LEFT JOIN candidates "
                "ON candidates.genre_folded = r.genre_folded "
                "ORDER BY r.ordinal, candidates.match_count DESC, candidates.album_id",
                parameters,
            ).fetchall()
            output: dict[str, dict[str, Any]] = {}
            for row in rows:
                display_name = str(row["display_name"])
                entry = output.setdefault(
                    display_name,
                    {
                        "genre_folded": str(row["genre_folded"]),
                        "revision": int(row["genre_revision"]),
                        "candidates": [],
                    },
                )
                if row["album_id"] is not None:
                    entry["candidates"].append(dict(row))
            return output

        return await self._read(operation)

    async def get_target_library_stats(self) -> dict[str, Any]:
        def operation(connection: sqlite3.Connection) -> dict[str, Any]:
            row = connection.execute(
                "SELECT COUNT(*) AS total_tracks, "
                "COUNT(DISTINCT local_album_id) AS total_albums, "
                "COALESCE(SUM(file_size_bytes), 0) AS total_size_bytes "
                "FROM local_tracks WHERE availability = 'indexed'"
            ).fetchone()
            artist_count = int(
                connection.execute(
                    "SELECT COUNT(*) FROM ("
                    "SELECT aa.local_artist_id FROM local_album_artists aa "
                    "JOIN local_tracks t ON t.local_album_id = aa.local_album_id "
                    "WHERE t.availability = 'indexed' "
                    "UNION "
                    "SELECT ta.local_artist_id FROM local_track_artists ta "
                    "JOIN local_tracks t ON t.id = ta.local_track_id "
                    "WHERE t.availability = 'indexed'"
                    ")"
                ).fetchone()[0]
            )
            formats = connection.execute(
                "SELECT file_format, COUNT(*) AS count FROM local_tracks "
                "WHERE availability = 'indexed' GROUP BY file_format"
            ).fetchall()
            unmatched = int(
                connection.execute(
                    "SELECT COUNT(*) FROM library_identification_reviews "
                    "WHERE state = 'needs_review'"
                ).fetchone()[0]
            )
            local_only = int(
                connection.execute(
                    "SELECT COUNT(DISTINCT t.local_album_id) FROM local_tracks t "
                    "LEFT JOIN local_album_external_identities i "
                    "ON i.local_album_id = t.local_album_id "
                    "WHERE t.availability = 'indexed' AND i.local_album_id IS NULL"
                ).fetchone()[0]
            )
            last_scan = connection.execute(
                "SELECT MAX(terminal_at) FROM library_scan_runs "
                "WHERE state = 'completed'"
            ).fetchone()[0]
            return {
                "total_albums": int(row["total_albums"]),
                "total_artists": artist_count,
                "total_tracks": int(row["total_tracks"]),
                "total_size_bytes": int(row["total_size_bytes"]),
                "format_breakdown": {
                    str(item["file_format"]): int(item["count"]) for item in formats
                },
                "unmatched_count": unmatched,
                "local_only_count": local_only,
                "last_scan_at": float(last_scan) if last_scan is not None else None,
            }

        return await self._read(operation)

    async def get_target_total_library_bytes(self) -> int:
        def operation(connection: sqlite3.Connection) -> int:
            row = connection.execute(
                "SELECT COALESCE(SUM(file_size_bytes), 0) FROM local_tracks "
                "WHERE availability = 'indexed'"
            ).fetchone()
            return int(row[0])

        return await self._read(operation)

    async def get_target_user_library_bytes(self, user_id: str) -> int:
        def operation(connection: sqlite3.Connection) -> int:
            row = connection.execute(
                "SELECT COALESCE(SUM(t.file_size_bytes), 0) FROM local_tracks t "
                "JOIN download_tasks d ON d.id = t.download_task_id "
                "WHERE t.availability = 'indexed' AND d.user_id = ?",
                (user_id,),
            ).fetchone()
            return int(row[0])

        return await self._read(operation)

    async def target_existing_ids(
        self,
        *,
        artist_ids: list[str],
        album_ids: list[str],
        track_ids: list[str],
    ) -> dict[str, set[str]]:
        def operation(connection: sqlite3.Connection) -> dict[str, set[str]]:
            return {
                kind: {
                    identifier
                    for identifier in identifiers
                    if self._resolve_target_id(
                        connection, kind=kind, identifier=identifier
                    )
                    is not None
                }
                for kind, identifiers in (
                    ("artist", artist_ids),
                    ("album", album_ids),
                    ("track", track_ids),
                )
            }

        return await self._read(operation)

    async def add_target_favorite(
        self, user_id: str, item_kind: str, item_id: str, created_at: float
    ) -> None:
        def operation(connection: sqlite3.Connection) -> None:
            resolved = self._resolve_target_id(
                connection, kind=item_kind, identifier=item_id
            )
            if resolved is None:
                raise ResourceNotFoundError("The library item was not found.")
            connection.execute(
                "INSERT INTO library_user_favorites "
                "(user_id, item_kind, item_id, created_at) VALUES (?,?,?,?) "
                "ON CONFLICT DO NOTHING",
                (user_id, item_kind, resolved, created_at),
            )

        await self._write(operation)
        await self._invalidate()

    async def apply_target_favorites(
        self,
        user_id: str,
        targets: list[tuple[str, str]],
        *,
        add: bool,
        created_at: float,
    ) -> None:
        def operation(connection: sqlite3.Connection) -> None:
            resolved_targets: list[tuple[str, str]] = []
            for kind, identifier in targets:
                resolved = self._resolve_target_id(
                    connection, kind=kind, identifier=identifier
                )
                if resolved is None:
                    raise ResourceNotFoundError("A library item was not found.")
                resolved_targets.append((kind, resolved))
            if add:
                connection.executemany(
                    "INSERT INTO library_user_favorites "
                    "(user_id, item_kind, item_id, created_at) VALUES (?,?,?,?) "
                    "ON CONFLICT DO NOTHING",
                    [
                        (user_id, kind, item_id, created_at)
                        for kind, item_id in resolved_targets
                    ],
                )
            else:
                connection.executemany(
                    "DELETE FROM library_user_favorites WHERE user_id = ? "
                    "AND item_kind = ? AND item_id = ?",
                    [(user_id, kind, item_id) for kind, item_id in resolved_targets],
                )

        await self._write(operation)
        await self._invalidate()

    async def remove_target_favorite(
        self, user_id: str, item_kind: str, item_id: str
    ) -> None:
        def operation(connection: sqlite3.Connection) -> None:
            resolved = self._resolve_target_id(
                connection, kind=item_kind, identifier=item_id
            )
            if resolved is None:
                return
            connection.execute(
                "DELETE FROM library_user_favorites WHERE user_id = ? "
                "AND item_kind = ? AND item_id = ?",
                (user_id, item_kind, resolved),
            )

        await self._write(operation)
        await self._invalidate()

    async def list_target_favorites(
        self, user_id: str, item_kind: str
    ) -> list[tuple[str, float]]:
        def operation(connection: sqlite3.Connection) -> list[tuple[str, float]]:
            rows = connection.execute(
                "SELECT item_id, created_at FROM library_user_favorites "
                "WHERE user_id = ? AND item_kind = ? ORDER BY created_at DESC, item_id",
                (user_id, item_kind),
            ).fetchall()
            return [(str(row["item_id"]), float(row["created_at"])) for row in rows]

        return await self._read(operation)

    async def target_favorite_map(
        self, user_id: str, item_kind: str, item_ids: list[str]
    ) -> dict[str, float]:
        if not item_ids:
            return {}

        def operation(connection: sqlite3.Connection) -> dict[str, float]:
            resolved_to_requested: dict[str, list[str]] = {}
            for identifier in item_ids:
                resolved = self._resolve_target_id(
                    connection, kind=item_kind, identifier=identifier
                )
                if resolved:
                    resolved_to_requested.setdefault(resolved, []).append(identifier)
            if not resolved_to_requested:
                return {}
            placeholders = ",".join("?" for _ in resolved_to_requested)
            rows = connection.execute(
                "SELECT item_id, created_at FROM library_user_favorites "
                "WHERE user_id = ? AND item_kind = ? "
                f"AND item_id IN ({placeholders})",
                (user_id, item_kind, *resolved_to_requested),
            ).fetchall()
            result: dict[str, float] = {}
            for row in rows:
                for requested in resolved_to_requested[str(row["item_id"])]:
                    result[requested] = float(row["created_at"])
            return result

        return await self._read(operation)

    async def insert_target_play_history(
        self,
        *,
        history_id: str,
        user_id: str,
        track_id: str | None,
        track_name: str,
        artist_name: str,
        played_at: str,
        album_name: str | None,
        duration_ms: int | None,
        source: str | None,
    ) -> None:
        def operation(connection: sqlite3.Connection) -> None:
            resolved = (
                self._resolve_target_id(connection, kind="track", identifier=track_id)
                if track_id
                else None
            )
            membership = (
                connection.execute(
                    "SELECT t.local_album_id, "
                    "COALESCE(ta.local_artist_id, a.album_artist_id) AS local_artist_id, "
                    "te.recording_mbid, ae.release_group_mbid "
                    "FROM local_tracks t JOIN local_albums a ON a.id = t.local_album_id "
                    "LEFT JOIN local_track_artists ta "
                    "ON ta.local_track_id = t.id AND ta.position = 0 "
                    "LEFT JOIN local_track_external_identities te "
                    "ON te.local_track_id = t.id AND te.provider = 'musicbrainz' "
                    "LEFT JOIN local_album_external_identities ae "
                    "ON ae.local_album_id = a.id AND ae.provider = 'musicbrainz' "
                    "WHERE t.id = ?",
                    (resolved,),
                ).fetchone()
                if resolved
                else None
            )
            connection.execute(
                "INSERT INTO library_play_history "
                "(id, user_id, local_track_id, local_album_id, local_artist_id, "
                "track_name, artist_name, album_name, recording_mbid, "
                "release_group_mbid, duration_ms, source, played_at) "
                "VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?)",
                (
                    history_id,
                    user_id,
                    resolved,
                    membership["local_album_id"] if membership else None,
                    membership["local_artist_id"] if membership else None,
                    track_name,
                    artist_name,
                    album_name,
                    membership["recording_mbid"] if membership else None,
                    membership["release_group_mbid"] if membership else None,
                    duration_ms,
                    source,
                    played_at,
                ),
            )

        await self._write(operation)
        await self._invalidate()

    async def target_play_history_stats(
        self,
        user_id: str,
        *,
        track_ids: list[str],
        album_ids: list[str],
        artist_ids: list[str],
    ) -> dict[str, dict[str, tuple[int, str]]]:
        def operation(
            connection: sqlite3.Connection,
        ) -> dict[str, dict[str, tuple[int, str]]]:
            def grouped(
                column: str, kind: str, values: list[str]
            ) -> dict[str, tuple[int, str]]:
                resolved_to_requested: dict[str, list[str]] = {}
                for value in values:
                    resolved = self._resolve_target_id(
                        connection, kind=kind, identifier=value
                    )
                    if resolved:
                        resolved_to_requested.setdefault(resolved, []).append(value)
                if not resolved_to_requested:
                    return {}
                placeholders = ",".join("?" for _ in resolved_to_requested)
                rows = connection.execute(
                    f"SELECT {column} AS item_id, COUNT(*) AS plays, "
                    "MAX(played_at) AS played_at FROM library_play_history "
                    f"WHERE user_id = ? AND {column} IN ({placeholders}) "
                    f"GROUP BY {column}",
                    (user_id, *resolved_to_requested),
                ).fetchall()
                result: dict[str, tuple[int, str]] = {}
                for row in rows:
                    value = (int(row["plays"]), str(row["played_at"]))
                    for requested in resolved_to_requested[str(row["item_id"])]:
                        result[requested] = value
                return result

            return {
                "track": grouped("local_track_id", "track", track_ids),
                "album": grouped("local_album_id", "album", album_ids),
                "artist": grouped("local_artist_id", "artist", artist_ids),
            }

        return await self._read(operation)

    async def list_target_play_history(
        self, user_id: str, *, limit: int = 50
    ) -> list[dict[str, Any]]:
        def operation(connection: sqlite3.Connection) -> list[dict[str, Any]]:
            rows = connection.execute(
                "SELECT id, user_id, track_name, artist_name, played_at, album_name, "
                "recording_mbid, release_group_mbid, duration_ms, source "
                "FROM library_play_history WHERE user_id = ? "
                "ORDER BY played_at DESC, id LIMIT ?",
                (user_id, max(1, limit)),
            ).fetchall()
            return [dict(row) for row in rows]

        return await self._read(operation)

    async def target_play_counts_by_artist(
        self, user_id: str, artist_name: str
    ) -> dict[str, int]:
        def operation(connection: sqlite3.Connection) -> dict[str, int]:
            rows = connection.execute(
                "SELECT recording_mbid, track_name, COUNT(*) AS plays "
                "FROM library_play_history WHERE user_id = ? AND artist_name = ? "
                "GROUP BY recording_mbid, track_name",
                (user_id, artist_name),
            ).fetchall()
            counts: dict[str, int] = {}
            for row in rows:
                plays = int(row["plays"])
                if row["recording_mbid"]:
                    key = f"rec:{row['recording_mbid']}"
                    counts[key] = counts.get(key, 0) + plays
                name_key = f"name:{str(row['track_name'] or '').casefold()}"
                counts[name_key] = counts.get(name_key, 0) + plays
            return counts

        return await self._read(operation)

    async def target_history_album_ids(
        self,
        user_id: str,
        *,
        frequent: bool,
        limit: int,
        offset: int,
    ) -> list[str]:
        def operation(connection: sqlite3.Connection) -> list[str]:
            ordering = (
                "COUNT(*) DESC, MAX(played_at) DESC, local_album_id"
                if frequent
                else "MAX(played_at) DESC, local_album_id"
            )
            rows = connection.execute(
                "SELECT local_album_id FROM library_play_history "
                "WHERE user_id = ? AND local_album_id IS NOT NULL "
                "GROUP BY local_album_id ORDER BY " + ordering + " LIMIT ? OFFSET ?",
                (user_id, max(1, limit), max(0, offset)),
            ).fetchall()
            return [str(row["local_album_id"]) for row in rows]

        return await self._read(operation)

    async def get_target_compat_id(self, kind: str, internal_id: str) -> str | None:
        def operation(connection: sqlite3.Connection) -> str | None:
            resolved = (
                self._resolve_target_id(connection, kind=kind, identifier=internal_id)
                if kind in {"artist", "album", "track"}
                else internal_id
            )
            if resolved is None:
                return None
            row = connection.execute(
                "SELECT jf_id FROM library_compat_id_map "
                "WHERE kind = ? AND internal_id = ? ORDER BY jf_id LIMIT 1",
                (kind, resolved),
            ).fetchone()
            return str(row["jf_id"]) if row else None

        return await self._read(operation)

    async def get_target_compat_mapping(self, jf_id: str) -> tuple[str, str] | None:
        def operation(connection: sqlite3.Connection) -> tuple[str, str] | None:
            row = connection.execute(
                "SELECT kind, internal_id FROM library_compat_id_map WHERE jf_id = ?",
                (jf_id,),
            ).fetchone()
            if row is None:
                return None
            kind = str(row["kind"])
            internal_id = str(row["internal_id"])
            if kind in {"artist", "album", "track"}:
                internal_id = (
                    self._resolve_target_id(
                        connection, kind=kind, identifier=internal_id
                    )
                    or internal_id
                )
            return kind, internal_id

        return await self._read(operation)

    async def insert_target_compat_mapping(
        self, jf_id: str, kind: str, internal_id: str
    ) -> None:
        def operation(connection: sqlite3.Connection) -> None:
            resolved = (
                self._resolve_target_id(connection, kind=kind, identifier=internal_id)
                if kind in {"artist", "album", "track"}
                else internal_id
            )
            if resolved is None:
                raise ResourceNotFoundError("The library item was not found.")
            connection.execute(
                "INSERT INTO library_compat_id_map (jf_id, kind, internal_id) "
                "VALUES (?,?,?) ON CONFLICT(jf_id) DO NOTHING",
                (jf_id, kind, resolved),
            )

        await self._write(operation)
        await self._invalidate()

    async def list_target_bookmarks(self, user_id: str) -> list[dict[str, Any]]:
        def operation(connection: sqlite3.Connection) -> list[dict[str, Any]]:
            rows = connection.execute(
                "SELECT local_track_id AS file_id, position_ms, comment, created_at, "
                "changed_at FROM library_compat_bookmarks WHERE user_id = ? "
                "ORDER BY changed_at DESC, local_track_id",
                (user_id,),
            ).fetchall()
            return [dict(row) for row in rows]

        return await self._read(operation)

    async def upsert_target_bookmark(
        self,
        user_id: str,
        track_id: str,
        position_ms: int,
        comment: str,
        now: float,
    ) -> None:
        def operation(connection: sqlite3.Connection) -> None:
            resolved = self._resolve_target_id(
                connection, kind="track", identifier=track_id
            )
            if resolved is None:
                raise ResourceNotFoundError("The library track was not found.")
            connection.execute(
                "INSERT INTO library_compat_bookmarks "
                "(user_id, local_track_id, position_ms, comment, created_at, changed_at) "
                "VALUES (?,?,?,?,?,?) ON CONFLICT(user_id, local_track_id) DO UPDATE SET "
                "position_ms = excluded.position_ms, comment = excluded.comment, "
                "changed_at = excluded.changed_at",
                (user_id, resolved, position_ms, comment, now, now),
            )

        await self._write(operation)
        await self._invalidate()

    async def delete_target_bookmark(self, user_id: str, track_id: str) -> None:
        def operation(connection: sqlite3.Connection) -> None:
            resolved = self._resolve_target_id(
                connection, kind="track", identifier=track_id
            )
            if resolved:
                connection.execute(
                    "DELETE FROM library_compat_bookmarks "
                    "WHERE user_id = ? AND local_track_id = ?",
                    (user_id, resolved),
                )

        await self._write(operation)
        await self._invalidate()

    async def get_target_play_queue(self, user_id: str) -> dict[str, Any]:
        def operation(connection: sqlite3.Connection) -> dict[str, Any]:
            row = connection.execute(
                "SELECT current_index, position_ms, updated_at, changed_by_client "
                "FROM library_compat_play_queues WHERE user_id = ?",
                (user_id,),
            ).fetchone()
            if row is None:
                return {
                    "file_ids": (),
                    "current_index": None,
                    "position_ms": 0,
                    "updated_at": 0.0,
                    "changed_by_client": "",
                }
            items = connection.execute(
                "SELECT local_track_id FROM library_compat_play_queue_items "
                "WHERE user_id = ? ORDER BY item_index",
                (user_id,),
            ).fetchall()
            return {
                "file_ids": tuple(str(item["local_track_id"]) for item in items),
                "current_index": row["current_index"],
                "position_ms": int(row["position_ms"]),
                "updated_at": float(row["updated_at"]),
                "changed_by_client": str(row["changed_by_client"]),
            }

        return await self._read(operation)

    async def replace_target_play_queue(
        self,
        user_id: str,
        track_ids: tuple[str, ...],
        *,
        current_index: int | None,
        position_ms: int,
        changed_by_client: str,
        updated_at: float,
    ) -> dict[str, Any]:
        def operation(connection: sqlite3.Connection) -> dict[str, Any]:
            resolved_ids: list[str] = []
            for track_id in track_ids:
                resolved = self._resolve_target_id(
                    connection, kind="track", identifier=track_id
                )
                if resolved is None:
                    raise ResourceNotFoundError("A queued library track was not found.")
                resolved_ids.append(resolved)
            if current_index is not None and not 0 <= current_index < len(resolved_ids):
                raise ValueError(
                    "The play queue current index is outside its item list."
                )
            connection.execute(
                "INSERT INTO library_compat_play_queues "
                "(user_id, current_index, position_ms, updated_at, changed_by_client) "
                "VALUES (?,?,?,?,?) ON CONFLICT(user_id) DO UPDATE SET "
                "current_index = excluded.current_index, "
                "position_ms = excluded.position_ms, updated_at = excluded.updated_at, "
                "changed_by_client = excluded.changed_by_client",
                (
                    user_id,
                    current_index,
                    position_ms,
                    updated_at,
                    changed_by_client,
                ),
            )
            connection.execute(
                "DELETE FROM library_compat_play_queue_items WHERE user_id = ?",
                (user_id,),
            )
            connection.executemany(
                "INSERT INTO library_compat_play_queue_items "
                "(user_id, item_index, local_track_id) VALUES (?,?,?)",
                [
                    (user_id, index, track_id)
                    for index, track_id in enumerate(resolved_ids)
                ],
            )
            return {
                "file_ids": tuple(resolved_ids),
                "current_index": current_index,
                "position_ms": position_ms,
                "updated_at": updated_at,
                "changed_by_client": changed_by_client,
            }

        result = await self._write(operation)
        await self._invalidate()
        return result

    async def resolve_target_track_for_history(
        self,
        *,
        recording_mbid: str | None,
        album_identifier: str | None,
        track_name: str,
        artist_name: str,
    ) -> str | None:
        def operation(connection: sqlite3.Connection) -> str | None:
            if recording_mbid:
                rows = connection.execute(
                    "SELECT local_track_id FROM local_track_external_identities "
                    "WHERE provider = 'musicbrainz' AND recording_mbid = ? "
                    "ORDER BY local_track_id LIMIT 2",
                    (recording_mbid.casefold(),),
                ).fetchall()
                if len(rows) == 1:
                    return str(rows[0]["local_track_id"])
            album_id = (
                self._resolve_target_id(
                    connection, kind="album", identifier=album_identifier
                )
                if album_identifier
                else None
            )
            clauses = ["title_folded = ?", "artist_name_folded = ?"]
            parameters: list[Any] = [_fold(track_name), _fold(artist_name)]
            if album_id:
                clauses.append("local_album_id = ?")
                parameters.append(album_id)
            rows = connection.execute(
                "SELECT id FROM local_tracks WHERE "
                + " AND ".join(clauses)
                + " AND availability = 'indexed' ORDER BY id LIMIT 2",
                parameters,
            ).fetchall()
            return str(rows[0]["id"]) if len(rows) == 1 else None

        return await self._read(operation)

    async def get_target_artwork_context(
        self, kind: str, identifier: str
    ) -> dict[str, Any] | None:
        def operation(connection: sqlite3.Connection) -> dict[str, Any] | None:
            resolved = self._resolve_target_id(
                connection, kind=kind, identifier=identifier
            )
            if resolved is None:
                return None
            if kind == "album":
                row = connection.execute(
                    "SELECT a.id, artwork.source, artwork.source_locator, "
                    "artwork.cover_url, artwork.version, ae.release_group_mbid AS provider_id, "
                    "t.file_path AS embedded_file_path, "
                    "t.availability AS embedded_file_availability "
                    "FROM local_albums a "
                    "LEFT JOIN local_album_artwork artwork "
                    "ON artwork.local_album_id = a.id "
                    "LEFT JOIN local_album_external_identities ae "
                    "ON ae.local_album_id = a.id AND ae.provider = 'musicbrainz' "
                    "LEFT JOIN local_tracks t ON t.id = artwork.source_locator "
                    "WHERE a.id = ? ORDER BY t.id LIMIT 1",
                    (resolved,),
                ).fetchone()
            elif kind == "artist":
                row = connection.execute(
                    "SELECT a.id, NULL AS source, NULL AS source_locator, "
                    "NULL AS cover_url, ae.provider_artist_id AS provider_id, "
                    "NULL AS embedded_file_path, "
                    "NULL AS embedded_file_availability FROM local_artists a "
                    "LEFT JOIN local_artist_external_identities ae "
                    "ON ae.local_artist_id = a.id AND ae.provider = 'musicbrainz' "
                    "WHERE a.id = ?",
                    (resolved,),
                ).fetchone()
            else:
                raise ValueError("Artwork is available only for albums and artists.")
            return _row(row)

        return await self._read(operation)

    async def get_cached_local_artwork_context(
        self, album_id: str, expected_version: int
    ) -> dict[str, Any] | None:
        def operation(connection: sqlite3.Connection) -> dict[str, Any] | None:
            row = connection.execute(
                "SELECT a.id, artwork.source, artwork.source_locator, artwork.cover_url, "
                "artwork.version, ae.release_group_mbid AS provider_id, "
                "embedded.file_path AS embedded_file_path, "
                "embedded.availability AS embedded_file_availability "
                "FROM local_albums a JOIN local_album_artwork artwork "
                "ON artwork.local_album_id = a.id "
                "LEFT JOIN local_album_external_identities ae "
                "ON ae.local_album_id = a.id AND ae.provider = 'musicbrainz' "
                "LEFT JOIN local_tracks embedded ON embedded.id = artwork.source_locator "
                "WHERE a.id = ? AND a.retired_into_album_id IS NULL "
                "AND artwork.version = ? LIMIT 1",
                (album_id, expected_version),
            ).fetchone()
            return _row(row)

        return await self._read(operation)

    async def count_target_references(
        self, *, album_ids: list[str], track_ids: list[str]
    ) -> dict[str, int]:
        def operation(connection: sqlite3.Connection) -> dict[str, int]:
            clauses: list[str] = []
            parameters: list[str] = []
            if album_ids:
                placeholders = ",".join("?" for _ in album_ids)
                clauses.append(f"local_album_id IN ({placeholders})")
                parameters.extend(album_ids)
            if track_ids:
                placeholders = ",".join("?" for _ in track_ids)
                clauses.append(f"local_track_id IN ({placeholders})")
                parameters.extend(track_ids)
            if not clauses:
                return {"playlists": 0, "history": 0}
            predicate = " OR ".join(f"({clause})" for clause in clauses)
            return {
                "playlists": int(
                    connection.execute(
                        "SELECT COUNT(*) FROM library_playlist_tracks WHERE "
                        + predicate,
                        parameters,
                    ).fetchone()[0]
                ),
                "history": int(
                    connection.execute(
                        "SELECT COUNT(*) FROM library_play_history WHERE " + predicate,
                        parameters,
                    ).fetchone()[0]
                ),
            }

        return await self._read(operation)

    async def create_target_playlist(
        self,
        *,
        playlist_id: str,
        name: str,
        source_ref: str | None,
        user_id: str,
        created_at: str,
    ) -> dict[str, Any]:
        def operation(connection: sqlite3.Connection) -> dict[str, Any]:
            connection.execute(
                "INSERT INTO library_playlists "
                "(id, name, created_at, updated_at, source_ref, user_id, is_public) "
                "VALUES (?,?,?,?,?,?,0)",
                (playlist_id, name, created_at, created_at, source_ref, user_id),
            )
            return dict(
                connection.execute(
                    "SELECT * FROM library_playlists WHERE id = ?", (playlist_id,)
                ).fetchone()
            )

        result = await self._write(operation)
        await self._invalidate()
        return result

    async def assign_unowned_target_playlists(self, user_id: str) -> int:
        def operation(connection: sqlite3.Connection) -> int:
            cursor = connection.execute(
                "UPDATE library_playlists SET user_id = ?, is_public = 0 "
                "WHERE user_id IS NULL",
                (user_id,),
            )
            return int(cursor.rowcount)

        changed = await self._write(operation)
        if changed:
            await self._invalidate()
        return changed

    async def get_target_playlist(self, playlist_id: str) -> dict[str, Any] | None:
        def operation(connection: sqlite3.Connection) -> dict[str, Any] | None:
            return _row(
                connection.execute(
                    "SELECT * FROM library_playlists WHERE id = ?", (playlist_id,)
                ).fetchone()
            )

        return await self._read(operation)

    async def get_target_playlist_by_source(
        self, source_ref: str, user_id: str | None
    ) -> dict[str, Any] | None:
        def operation(connection: sqlite3.Connection) -> dict[str, Any] | None:
            if user_id is None:
                row = connection.execute(
                    "SELECT * FROM library_playlists WHERE source_ref = ? "
                    "ORDER BY id LIMIT 1",
                    (source_ref,),
                ).fetchone()
            else:
                row = connection.execute(
                    "SELECT * FROM library_playlists "
                    "WHERE source_ref = ? AND user_id = ? LIMIT 1",
                    (source_ref, user_id),
                ).fetchone()
            return _row(row)

        return await self._read(operation)

    async def get_target_imported_playlist_source_ids(
        self, prefix: str, user_id: str | None
    ) -> set[str]:
        def operation(connection: sqlite3.Connection) -> set[str]:
            query = (
                "SELECT source_ref FROM library_playlists "
                "WHERE source_ref LIKE ? AND source_ref IS NOT NULL"
            )
            parameters: list[str] = [f"{prefix}%"]
            if user_id is not None:
                query += " AND user_id = ?"
                parameters.append(user_id)
            prefix_length = len(prefix)
            return {
                str(row["source_ref"])[prefix_length:]
                for row in connection.execute(query, parameters).fetchall()
            }

        return await self._read(operation)

    async def list_target_playlists(
        self, user_id: str | None = None
    ) -> list[dict[str, Any]]:
        def operation(connection: sqlite3.Connection) -> list[dict[str, Any]]:
            where = "" if user_id is None else "WHERE p.user_id = ? OR p.is_public = 1"
            parameters = () if user_id is None else (user_id,)
            rows = connection.execute(
                "SELECT p.*, COUNT(pt.id) AS track_count, "
                "COALESCE(SUM(pt.duration), 0) AS total_duration, "
                "GROUP_CONCAT(NULLIF(pt.cover_url, '')) AS cover_urls "
                "FROM library_playlists p LEFT JOIN library_playlist_tracks pt "
                "ON pt.playlist_id = p.id "
                + where
                + " GROUP BY p.id ORDER BY p.updated_at DESC, p.id",
                parameters,
            ).fetchall()
            return [dict(row) for row in rows]

        return await self._read(operation)

    async def get_target_playlist_summary(
        self, playlist_id: str
    ) -> dict[str, Any] | None:
        rows = await self.list_target_playlists()
        return next((row for row in rows if row["id"] == playlist_id), None)

    async def update_target_playlist(
        self,
        playlist_id: str,
        *,
        name: str | None,
        cover_image_path: str | None | object,
        changed_at: str,
        cover_unchanged: object,
    ) -> dict[str, Any] | None:
        def operation(connection: sqlite3.Connection) -> dict[str, Any] | None:
            row = connection.execute(
                "SELECT * FROM library_playlists WHERE id = ?", (playlist_id,)
            ).fetchone()
            if row is None:
                return None
            next_name = name if name is not None else row["name"]
            next_cover = (
                row["cover_image_path"]
                if cover_image_path is cover_unchanged
                else cover_image_path
            )
            connection.execute(
                "UPDATE library_playlists SET name = ?, cover_image_path = ?, "
                "updated_at = ? WHERE id = ?",
                (next_name, next_cover, changed_at, playlist_id),
            )
            return dict(
                connection.execute(
                    "SELECT * FROM library_playlists WHERE id = ?", (playlist_id,)
                ).fetchone()
            )

        result = await self._write(operation)
        if result is not None:
            await self._invalidate()
        return result

    async def set_target_playlist_public(
        self, playlist_id: str, is_public: bool, changed_at: str
    ) -> dict[str, Any] | None:
        def operation(connection: sqlite3.Connection) -> dict[str, Any] | None:
            cursor = connection.execute(
                "UPDATE library_playlists SET is_public = ?, updated_at = ? "
                "WHERE id = ?",
                (int(is_public), changed_at, playlist_id),
            )
            if cursor.rowcount == 0:
                return None
            return dict(
                connection.execute(
                    "SELECT * FROM library_playlists WHERE id = ?", (playlist_id,)
                ).fetchone()
            )

        result = await self._write(operation)
        if result is not None:
            await self._invalidate()
        return result

    async def delete_target_playlist(self, playlist_id: str) -> bool:
        def operation(connection: sqlite3.Connection) -> bool:
            return (
                connection.execute(
                    "DELETE FROM library_playlists WHERE id = ?", (playlist_id,)
                ).rowcount
                > 0
            )

        deleted = await self._write(operation)
        if deleted:
            await self._invalidate()
        return deleted

    async def add_target_playlist_tracks(
        self,
        playlist_id: str,
        tracks: list[dict[str, Any]],
        *,
        position: int | None,
        changed_at: str,
    ) -> list[dict[str, Any]]:
        def operation(connection: sqlite3.Connection) -> list[dict[str, Any]]:
            if (
                connection.execute(
                    "SELECT 1 FROM library_playlists WHERE id = ?", (playlist_id,)
                ).fetchone()
                is None
            ):
                return []
            current_count = int(
                connection.execute(
                    "SELECT COUNT(*) FROM library_playlist_tracks WHERE playlist_id = ?",
                    (playlist_id,),
                ).fetchone()[0]
            )
            insert_at = (
                current_count
                if position is None
                else max(0, min(position, current_count))
            )
            if insert_at < current_count:
                rows = connection.execute(
                    "SELECT id, position FROM library_playlist_tracks "
                    "WHERE playlist_id = ? AND position >= ? ORDER BY position DESC",
                    (playlist_id, insert_at),
                ).fetchall()
                for row in rows:
                    connection.execute(
                        "UPDATE library_playlist_tracks SET position = ? WHERE id = ?",
                        (int(row["position"]) + len(tracks), row["id"]),
                    )
            created: list[dict[str, Any]] = []
            for index, track in enumerate(tracks):
                source_type = str(track.get("source_type") or "")
                candidate_id = track.get("library_file_id")
                if not candidate_id and source_type in {
                    "local",
                    "droppedneedle-local",
                    "howler",
                }:
                    candidate_id = track.get("track_source_id")
                local_track_id = (
                    self._resolve_unique_target_subject_id(
                        connection, kind="track", identifier=str(candidate_id)
                    )
                    if candidate_id
                    else None
                )
                local_album_id = local_artist_id = None
                if local_track_id:
                    membership = connection.execute(
                        "SELECT t.local_album_id, "
                        "COALESCE(ta.local_artist_id, a.album_artist_id) AS artist_id "
                        "FROM local_tracks t JOIN local_albums a "
                        "ON a.id = t.local_album_id LEFT JOIN local_track_artists ta "
                        "ON ta.local_track_id = t.id AND ta.position = 0 WHERE t.id = ?",
                        (local_track_id,),
                    ).fetchone()
                    local_album_id = membership["local_album_id"]
                    local_artist_id = membership["artist_id"]
                elif candidate_id and source_type in {
                    "local",
                    "droppedneedle-local",
                    "howler",
                }:
                    raise ResourceNotFoundError(
                        "The local playlist track was not found."
                    )
                values = (
                    track["id"],
                    playlist_id,
                    insert_at + index,
                    track["track_name"],
                    track["artist_name"],
                    track["album_name"],
                    track.get("album_id"),
                    track.get("artist_id"),
                    track.get("track_source_id"),
                    track.get("cover_url"),
                    source_type,
                    (
                        json.dumps(track["available_sources"])
                        if track.get("available_sources") is not None
                        else None
                    ),
                    track.get("format"),
                    track.get("track_number"),
                    track.get("disc_number"),
                    track.get("duration"),
                    track["created_at"],
                    track.get("plex_rating_key"),
                    local_track_id,
                    local_track_id,
                    local_album_id,
                    local_artist_id,
                    None,
                )
                connection.execute(
                    "INSERT INTO library_playlist_tracks "
                    "(id, playlist_id, position, track_name, artist_name, album_name, "
                    "album_id, artist_id, track_source_id, cover_url, source_type, "
                    "available_sources, format, track_number, disc_number, duration, "
                    "created_at, plex_rating_key, library_file_id, local_track_id, "
                    "local_album_id, local_artist_id, reference_tombstone_id) "
                    "VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)",
                    values,
                )
                created.append(
                    dict(
                        connection.execute(
                            "SELECT * FROM library_playlist_tracks WHERE id = ?",
                            (track["id"],),
                        ).fetchone()
                    )
                )
            connection.execute(
                "UPDATE library_playlists SET updated_at = ? WHERE id = ?",
                (changed_at, playlist_id),
            )
            return created

        result = await self._write(operation)
        if result:
            await self._invalidate()
        return result

    async def list_target_playlist_tracks(
        self, playlist_id: str
    ) -> list[dict[str, Any]]:
        def operation(connection: sqlite3.Connection) -> list[dict[str, Any]]:
            rows = connection.execute(
                "SELECT * FROM library_playlist_tracks WHERE playlist_id = ? "
                "ORDER BY position",
                (playlist_id,),
            ).fetchall()
            return [dict(row) for row in rows]

        return await self._read(operation)

    async def get_target_playlist_track(
        self, playlist_id: str, track_id: str
    ) -> dict[str, Any] | None:
        def operation(connection: sqlite3.Connection) -> dict[str, Any] | None:
            return _row(
                connection.execute(
                    "SELECT * FROM library_playlist_tracks "
                    "WHERE playlist_id = ? AND id = ?",
                    (playlist_id, track_id),
                ).fetchone()
            )

        return await self._read(operation)

    async def remove_target_playlist_tracks(
        self, playlist_id: str, track_ids: list[str], changed_at: str
    ) -> int:
        if not track_ids:
            return 0

        def operation(connection: sqlite3.Connection) -> int:
            placeholders = ",".join("?" for _ in track_ids)
            cursor = connection.execute(
                "DELETE FROM library_playlist_tracks WHERE playlist_id = ? "
                f"AND id IN ({placeholders})",
                (playlist_id, *track_ids),
            )
            remaining = connection.execute(
                "SELECT id FROM library_playlist_tracks WHERE playlist_id = ? "
                "ORDER BY position, id",
                (playlist_id,),
            ).fetchall()
            for index, row in enumerate(remaining):
                connection.execute(
                    "UPDATE library_playlist_tracks SET position = ? WHERE id = ?",
                    (index, row["id"]),
                )
            if cursor.rowcount:
                connection.execute(
                    "UPDATE library_playlists SET updated_at = ? WHERE id = ?",
                    (changed_at, playlist_id),
                )
            return cursor.rowcount

        removed = await self._write(operation)
        if removed:
            await self._invalidate()
        return removed

    async def reorder_target_playlist_track(
        self, playlist_id: str, track_id: str, new_position: int, changed_at: str
    ) -> int | None:
        def operation(connection: sqlite3.Connection) -> int | None:
            rows = connection.execute(
                "SELECT id FROM library_playlist_tracks WHERE playlist_id = ? "
                "ORDER BY position, id",
                (playlist_id,),
            ).fetchall()
            ids = [str(row["id"]) for row in rows]
            if track_id not in ids:
                return None
            ids.remove(track_id)
            actual = max(0, min(new_position, len(ids)))
            ids.insert(actual, track_id)
            for index, item_id in enumerate(ids):
                connection.execute(
                    "UPDATE library_playlist_tracks SET position = ? WHERE id = ?",
                    (-(index + 1), item_id),
                )
            for index, item_id in enumerate(ids):
                connection.execute(
                    "UPDATE library_playlist_tracks SET position = ? WHERE id = ?",
                    (index, item_id),
                )
            connection.execute(
                "UPDATE library_playlists SET updated_at = ? WHERE id = ?",
                (changed_at, playlist_id),
            )
            return actual

        result = await self._write(operation)
        if result is not None:
            await self._invalidate()
        return result

    async def update_target_playlist_track_source(
        self,
        playlist_id: str,
        track_id: str,
        *,
        source_type: str | None,
        available_sources: list[str] | None,
        track_source_id: str | None,
        plex_rating_key: str | None | object,
        library_file_id: str | None | object,
        unchanged: object,
        changed_at: str,
    ) -> dict[str, Any] | None:
        def operation(connection: sqlite3.Connection) -> dict[str, Any] | None:
            row = connection.execute(
                "SELECT * FROM library_playlist_tracks "
                "WHERE playlist_id = ? AND id = ?",
                (playlist_id, track_id),
            ).fetchone()
            if row is None:
                return None
            next_source_type = source_type or row["source_type"]
            next_sources = (
                json.dumps(available_sources)
                if available_sources is not None
                else row["available_sources"]
            )
            next_source_id = track_source_id or row["track_source_id"]
            next_plex_key = (
                row["plex_rating_key"]
                if plex_rating_key is unchanged
                else plex_rating_key
            )
            next_file_id = (
                row["library_file_id"]
                if library_file_id is unchanged
                else library_file_id
            )
            candidate_id = next_file_id
            if not candidate_id and next_source_type in {
                "local",
                "droppedneedle-local",
                "howler",
            }:
                candidate_id = next_source_id
            local_track_id = None
            local_album_id = None
            local_artist_id = None
            if candidate_id:
                local_track_id = self._resolve_unique_target_subject_id(
                    connection, kind="track", identifier=str(candidate_id)
                )
            if local_track_id:
                membership = connection.execute(
                    "SELECT t.local_album_id, "
                    "COALESCE(ta.local_artist_id, a.album_artist_id) AS artist_id "
                    "FROM local_tracks t JOIN local_albums a "
                    "ON a.id = t.local_album_id LEFT JOIN local_track_artists ta "
                    "ON ta.local_track_id = t.id AND ta.position = 0 WHERE t.id = ?",
                    (local_track_id,),
                ).fetchone()
                local_album_id = membership["local_album_id"]
                local_artist_id = membership["artist_id"]
                next_file_id = local_track_id
            elif candidate_id and next_source_type in {
                "local",
                "droppedneedle-local",
                "howler",
            }:
                raise ResourceNotFoundError("The local playlist track was not found.")
            connection.execute(
                "UPDATE library_playlist_tracks SET source_type = ?, "
                "available_sources = ?, track_source_id = ?, plex_rating_key = ?, "
                "library_file_id = ?, local_track_id = ?, local_album_id = ?, "
                "local_artist_id = ?, reference_tombstone_id = NULL "
                "WHERE playlist_id = ? AND id = ?",
                (
                    next_source_type,
                    next_sources,
                    next_source_id,
                    next_plex_key,
                    next_file_id,
                    local_track_id,
                    local_album_id,
                    local_artist_id,
                    playlist_id,
                    track_id,
                ),
            )
            connection.execute(
                "UPDATE library_playlists SET updated_at = ? WHERE id = ?",
                (changed_at, playlist_id),
            )
            return dict(
                connection.execute(
                    "SELECT * FROM library_playlist_tracks WHERE id = ?", (track_id,)
                ).fetchone()
            )

        result = await self._write(operation)
        if result is not None:
            await self._invalidate()
        return result

    async def update_target_playlist_sources(
        self,
        playlist_id: str,
        updates: dict[str, list[str]],
        changed_at: str,
    ) -> int:
        if not updates:
            return 0

        def operation(connection: sqlite3.Connection) -> int:
            updated = 0
            for track_id, sources in updates.items():
                updated += connection.execute(
                    "UPDATE library_playlist_tracks SET available_sources = ? "
                    "WHERE id = ? AND playlist_id = ?",
                    (json.dumps(sources), track_id, playlist_id),
                ).rowcount
            if updated:
                connection.execute(
                    "UPDATE library_playlists SET updated_at = ? WHERE id = ?",
                    (changed_at, playlist_id),
                )
            return updated

        result = await self._write(operation)
        if result:
            await self._invalidate()
        return result

    async def link_target_playlist_tracks(
        self,
        playlist_id: str,
        updates: dict[str, str],
        changed_at: str,
    ) -> int:
        if not updates:
            return 0

        def operation(connection: sqlite3.Connection) -> int:
            updated = 0
            for playlist_track_id, identifier in updates.items():
                local_track_id = self._resolve_unique_target_subject_id(
                    connection, kind="track", identifier=identifier
                )
                if local_track_id is None:
                    continue
                membership = connection.execute(
                    "SELECT t.local_album_id, "
                    "COALESCE(ta.local_artist_id, a.album_artist_id) AS artist_id "
                    "FROM local_tracks t JOIN local_albums a "
                    "ON a.id = t.local_album_id LEFT JOIN local_track_artists ta "
                    "ON ta.local_track_id = t.id AND ta.position = 0 WHERE t.id = ?",
                    (local_track_id,),
                ).fetchone()
                updated += connection.execute(
                    "UPDATE library_playlist_tracks SET library_file_id = ?, "
                    "local_track_id = ?, local_album_id = ?, local_artist_id = ?, "
                    "reference_tombstone_id = NULL "
                    "WHERE id = ? AND playlist_id = ? AND local_track_id IS NULL",
                    (
                        local_track_id,
                        local_track_id,
                        membership["local_album_id"],
                        membership["artist_id"],
                        playlist_track_id,
                        playlist_id,
                    ),
                ).rowcount
            if updated:
                connection.execute(
                    "UPDATE library_playlists SET updated_at = ? WHERE id = ?",
                    (changed_at, playlist_id),
                )
            return updated

        result = await self._write(operation)
        if result:
            await self._invalidate()
        return result

    async def target_playlist_streamable_counts(
        self,
    ) -> dict[str, tuple[int, int]]:
        def operation(connection: sqlite3.Connection) -> dict[str, tuple[int, int]]:
            rows = connection.execute(
                "SELECT playlist_id, COUNT(*) AS count, "
                "COALESCE(SUM(COALESCE(duration, 0)), 0) AS duration "
                "FROM library_playlist_tracks WHERE local_track_id IS NOT NULL "
                "GROUP BY playlist_id"
            ).fetchall()
            return {
                str(row["playlist_id"]): (int(row["count"]), int(row["duration"]))
                for row in rows
            }

        return await self._read(operation)

    async def target_playlist_membership(
        self,
        tracks: list[tuple[str, str, str]],
        user_id: str | None,
    ) -> dict[str, list[int]]:
        def operation(connection: sqlite3.Connection) -> dict[str, list[int]]:
            query = (
                "SELECT pt.playlist_id, LOWER(pt.track_name) AS track_name, "
                "LOWER(pt.artist_name) AS artist_name, LOWER(pt.album_name) AS album_name "
                "FROM library_playlist_tracks pt JOIN library_playlists p "
                "ON p.id = pt.playlist_id"
            )
            parameters: tuple[str, ...] = ()
            if user_id is not None:
                query += " WHERE p.user_id = ?"
                parameters = (user_id,)
            wanted = [
                (track.casefold(), artist.casefold(), album.casefold())
                for track, artist, album in tracks
            ]
            result: dict[str, list[int]] = {}
            for row in connection.execute(query, parameters).fetchall():
                key = (row["track_name"], row["artist_name"], row["album_name"])
                matches = [index for index, value in enumerate(wanted) if value == key]
                if matches:
                    result.setdefault(str(row["playlist_id"]), []).extend(matches)
            return result

        return await self._read(operation)

    async def get_stream_revision(self, stream: str) -> int:
        def operation(connection: sqlite3.Connection) -> int:
            row = connection.execute(
                "SELECT value FROM library_event_stream_revisions WHERE stream_kind = ?",
                (stream,),
            ).fetchone()
            if row is None:
                raise ResourceNotFoundError(f"Unknown library event stream: {stream}")
            return int(row["value"])

        return await self._read(operation)

    async def get_legacy_migration_snapshot(self) -> dict[str, list[dict[str, Any]]]:
        """Read every catalog/reference source from an isolated copied database."""

        def operation(
            connection: sqlite3.Connection,
        ) -> dict[str, list[dict[str, Any]]]:
            present = {
                str(row["name"])
                for row in connection.execute(
                    "SELECT name FROM sqlite_master WHERE type = 'table'"
                ).fetchall()
            }
            snapshot: dict[str, list[dict[str, Any]]] = {}
            for table, ordering in _LEGACY_MIGRATION_TABLE_ORDER.items():
                if table not in present:
                    snapshot[table] = []
                    continue
                rows = connection.execute(
                    f"SELECT * FROM {table} ORDER BY {ordering}"
                ).fetchall()
                snapshot[table] = [dict(row) for row in rows]
            return snapshot

        return await self._read(operation)

    async def get_bounded_legacy_source_revision(self) -> str:
        """Hash ordered legacy rows without materializing the database in memory."""

        def operation(connection: sqlite3.Connection) -> str:
            present = {
                str(row["name"])
                for row in connection.execute(
                    "SELECT name FROM sqlite_master WHERE type = 'table'"
                )
            }
            digest = hashlib.sha256(b"bounded-legacy-catalog-v1\0")
            for table, ordering in _LEGACY_MIGRATION_TABLE_ORDER.items():
                digest.update(table.encode())
                digest.update(b"\0")
                if table not in present:
                    digest.update(b"missing\0")
                    continue
                for row in connection.execute(
                    f"SELECT * FROM {table} ORDER BY {ordering}"
                ):
                    digest.update(msgspec.json.encode(dict(row)))
                    digest.update(b"\n")
            return digest.hexdigest()

        return await self._read(operation)

    async def get_bounded_legacy_source_counts(self) -> dict[str, int]:
        def operation(connection: sqlite3.Connection) -> dict[str, int]:
            present = {
                str(row["name"])
                for row in connection.execute(
                    "SELECT name FROM sqlite_master WHERE type = 'table'"
                )
            }
            counts: dict[str, int] = {}
            for table in _LEGACY_MIGRATION_TABLE_ORDER:
                if table not in present:
                    counts[table] = 0
                    continue
                predicate = (
                    " WHERE deleted_at IS NULL" if table == "library_files" else ""
                )
                counts[table] = int(
                    connection.execute(
                        f"SELECT COUNT(*) FROM {table}{predicate}"
                    ).fetchone()[0]
                )
            return counts

        return await self._read(operation)

    async def prepare_bounded_legacy_migration(self) -> None:
        def operation(connection: sqlite3.Connection) -> None:
            connection.execute(
                "CREATE INDEX IF NOT EXISTS idx_bounded_migration_local_track_path "
                "ON local_tracks(file_path)"
            )
            present = connection.execute(
                "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'library_files'"
            ).fetchone()
            if present is not None:
                connection.execute(
                    "CREATE INDEX IF NOT EXISTS idx_bounded_legacy_library_files "
                    "ON library_files(LOWER(COALESCE(release_group_mbid, '')), id) "
                    "WHERE deleted_at IS NULL"
                )
            connection.execute(
                "CREATE TABLE IF NOT EXISTS library_migration_file_staging ("
                "source_id TEXT PRIMARY KEY, group_key TEXT NOT NULL, "
                "root_id TEXT NOT NULL, relative_path TEXT NOT NULL, "
                "directory_key TEXT NOT NULL, album_key TEXT NOT NULL, "
                "artist_key TEXT NOT NULL, track_number INTEGER NOT NULL, "
                "legacy_release_key TEXT NOT NULL)"
            )
            connection.execute(
                "CREATE INDEX IF NOT EXISTS idx_library_migration_file_staging_group "
                "ON library_migration_file_staging(group_key, source_id)"
            )
            connection.execute(
                "CREATE INDEX IF NOT EXISTS idx_library_migration_file_staging_directory "
                "ON library_migration_file_staging("
                "root_id, directory_key, album_key, group_key, track_number)"
            )
            connection.execute(
                "CREATE INDEX IF NOT EXISTS idx_library_migration_file_staging_release "
                "ON library_migration_file_staging(legacy_release_key, group_key)"
            )
            connection.execute("DELETE FROM library_migration_file_staging")

        await self._write(operation)

    async def finish_bounded_legacy_migration(self) -> None:
        def operation(connection: sqlite3.Connection) -> None:
            connection.execute("DROP TABLE IF EXISTS library_migration_file_staging")
            connection.execute("DROP TABLE IF EXISTS library_migration_review_staging")
            connection.execute("DROP INDEX IF EXISTS idx_bounded_legacy_library_files")
            connection.execute(
                "DROP INDEX IF EXISTS idx_bounded_migration_local_track_path"
            )

        await self._write(operation)

    async def get_bounded_legacy_library_file_preflight_batch(
        self, *, after_id: str, limit: int
    ) -> list[dict[str, Any]]:
        def operation(connection: sqlite3.Connection) -> list[dict[str, Any]]:
            present = connection.execute(
                "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'library_files'"
            ).fetchone()
            if present is None:
                return []
            return [
                dict(row)
                for row in connection.execute(
                    "SELECT * FROM library_files WHERE deleted_at IS NULL AND id > ? "
                    "ORDER BY id LIMIT ?",
                    (after_id, limit),
                ).fetchall()
            ]

        return await self._read(operation)

    async def stage_bounded_legacy_local_groups(
        self, rows: list[tuple[str, str, str, str, str, str, str, int, str]]
    ) -> None:
        def operation(connection: sqlite3.Connection) -> None:
            connection.executemany(
                "INSERT OR REPLACE INTO library_migration_file_staging "
                "(source_id, group_key, root_id, relative_path, directory_key, "
                "album_key, artist_key, track_number, legacy_release_key) "
                "VALUES (?,?,?,?,?,?,?,?,?)",
                rows,
            )

        await self._write(operation)

    async def finalize_bounded_legacy_local_groups(self) -> None:
        def operation(connection: sqlite3.Connection) -> None:
            cursor = connection.execute(
                "SELECT root_id, directory_key, MIN(group_key) AS group_key "
                "FROM library_migration_file_staging WHERE album_key != '' "
                "GROUP BY root_id, directory_key "
                "HAVING COUNT(DISTINCT group_key) = 1"
            )
            while rows := cursor.fetchmany(500):
                for row in rows:
                    connection.execute(
                        "UPDATE library_migration_file_staging AS untagged "
                        "SET group_key = ? WHERE root_id = ? AND directory_key = ? "
                        "AND album_key = '' "
                        "AND NOT EXISTS ("
                        "SELECT 1 FROM library_migration_file_staging AS missing "
                        "WHERE missing.root_id = untagged.root_id "
                        "AND missing.directory_key = untagged.directory_key "
                        "AND missing.album_key = '' AND missing.track_number <= 0) "
                        "AND NOT EXISTS ("
                        "SELECT 1 FROM library_migration_file_staging AS missing "
                        "JOIN library_migration_file_staging AS tagged "
                        "ON tagged.root_id = missing.root_id "
                        "AND tagged.directory_key = missing.directory_key "
                        "AND tagged.group_key = ? AND tagged.album_key != '' "
                        "AND tagged.track_number = missing.track_number "
                        "WHERE missing.root_id = untagged.root_id "
                        "AND missing.directory_key = untagged.directory_key "
                        "AND missing.album_key = '')",
                        (
                            row["group_key"],
                            row["root_id"],
                            row["directory_key"],
                            row["group_key"],
                        ),
                    )

        await self._write(operation)

    async def get_bounded_legacy_library_file_batch(
        self,
        *,
        after_release_group: str | None,
        after_id: str,
        limit: int,
        exclude_staged: bool = False,
    ) -> list[dict[str, Any]]:
        def operation(connection: sqlite3.Connection) -> list[dict[str, Any]]:
            present = connection.execute(
                "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'library_files'"
            ).fetchone()
            if present is None:
                return []
            select = (
                "SELECT *, LOWER(COALESCE(release_group_mbid, '')) AS "
                "__migration_release_group FROM library_files "
                "WHERE deleted_at IS NULL "
            )
            if exclude_staged:
                select += (
                    "AND NOT EXISTS (SELECT 1 FROM library_migration_file_staging s "
                    "WHERE s.source_id = library_files.id) "
                )
            if after_release_group is None:
                rows = connection.execute(
                    f"{select} ORDER BY __migration_release_group, id LIMIT ?",
                    (limit,),
                ).fetchall()
            else:
                rows = connection.execute(
                    f"{select}AND "
                    "(LOWER(COALESCE(release_group_mbid, '')) > ? OR "
                    "(LOWER(COALESCE(release_group_mbid, '')) = ? AND id > ?)) "
                    "ORDER BY __migration_release_group, id LIMIT ?",
                    (after_release_group, after_release_group, after_id, limit),
                ).fetchall()
            return [dict(row) for row in rows]

        return await self._read(operation)

    async def get_bounded_legacy_local_group_keys(
        self, *, after_key: str, limit: int
    ) -> list[str]:
        def operation(connection: sqlite3.Connection) -> list[str]:
            return [
                str(row["group_key"])
                for row in connection.execute(
                    "SELECT DISTINCT group_key FROM library_migration_file_staging "
                    "WHERE group_key > ? ORDER BY group_key LIMIT ?",
                    (after_key, limit),
                ).fetchall()
            ]

        return await self._read(operation)

    async def get_bounded_legacy_local_group_context(
        self, group_key: str
    ) -> dict[str, Any] | None:
        def operation(connection: sqlite3.Connection) -> dict[str, Any] | None:
            first = connection.execute(
                "SELECT f.*, s.root_id, s.relative_path AS migration_relative_path "
                "FROM library_files f JOIN library_migration_file_staging s "
                "ON s.source_id = f.id WHERE s.group_key = ? "
                "ORDER BY CASE WHEN s.album_key != '' "
                "THEN 0 ELSE 1 END, COALESCE(f.disc_number, 1), "
                "COALESCE(f.track_number, 0), f.id LIMIT 1",
                (group_key,),
            ).fetchone()
            if first is None:
                return None
            aggregate = connection.execute(
                "SELECT MAX(COALESCE(f.is_compilation, 0)) AS is_compilation, "
                "MIN(f.imported_at) AS created_at FROM library_files f "
                "JOIN library_migration_file_staging s ON s.source_id = f.id "
                "WHERE s.group_key = ?",
                (group_key,),
            ).fetchone()
            aliases = connection.execute(
                "SELECT DISTINCT staged.legacy_release_key "
                "FROM library_migration_file_staging AS staged "
                "WHERE staged.group_key = ? AND staged.legacy_release_key != '' "
                "AND NOT EXISTS ("
                "SELECT 1 FROM library_migration_file_staging AS other "
                "WHERE other.legacy_release_key = staged.legacy_release_key "
                "AND other.group_key != staged.group_key) "
                "ORDER BY staged.legacy_release_key",
                (group_key,),
            ).fetchall()
            return {
                "first": dict(first),
                "root_id": str(first["root_id"]),
                "relative_path": str(first["migration_relative_path"]),
                "is_compilation": bool(aggregate["is_compilation"]),
                "created_at": aggregate["created_at"],
                "legacy_release_aliases": [
                    str(row["legacy_release_key"]) for row in aliases
                ],
            }

        return await self._read(operation)

    async def get_bounded_legacy_local_group_batch(
        self, group_key: str, *, after_id: str, limit: int
    ) -> list[dict[str, Any]]:
        def operation(connection: sqlite3.Connection) -> list[dict[str, Any]]:
            return [
                dict(row)
                for row in connection.execute(
                    "SELECT f.* FROM library_files f "
                    "JOIN library_migration_file_staging s ON s.source_id = f.id "
                    "WHERE s.group_key = ? AND f.id > ? ORDER BY f.id LIMIT ?",
                    (group_key, after_id, limit),
                ).fetchall()
            ]

        return await self._read(operation)

    async def get_bounded_legacy_library_group_context(
        self, release_group: str
    ) -> dict[str, Any] | None:
        def operation(connection: sqlite3.Connection) -> dict[str, Any] | None:
            first = connection.execute(
                "SELECT * FROM library_files "
                "WHERE deleted_at IS NULL "
                "AND LOWER(COALESCE(release_group_mbid, '')) = ? "
                "ORDER BY COALESCE(disc_number, 1), COALESCE(track_number, 0), id "
                "LIMIT 1",
                (release_group,),
            ).fetchone()
            if first is None:
                return None
            aggregate = connection.execute(
                "SELECT MAX(COALESCE(is_compilation, 0)) AS is_compilation, "
                "MIN(imported_at) AS created_at FROM library_files "
                "WHERE deleted_at IS NULL "
                "AND LOWER(COALESCE(release_group_mbid, '')) = ?",
                (release_group,),
            ).fetchone()
            release = connection.execute(
                "SELECT LOWER(release_mbid) AS release_mbid FROM library_files "
                "WHERE deleted_at IS NULL "
                "AND LOWER(COALESCE(release_group_mbid, '')) = ? "
                "AND LENGTH(release_mbid) = 36 "
                "AND SUBSTR(release_mbid, 9, 1) = '-' "
                "AND SUBSTR(release_mbid, 14, 1) = '-' "
                "AND SUBSTR(release_mbid, 19, 1) = '-' "
                "AND SUBSTR(release_mbid, 24, 1) = '-' "
                "AND LOWER(REPLACE(release_mbid, '-', '')) "
                "NOT GLOB '*[^0-9a-f]*' "
                "GROUP BY LOWER(release_mbid) "
                "ORDER BY COUNT(*) DESC, LOWER(release_mbid) LIMIT 1",
                (release_group,),
            ).fetchone()
            return {
                "first": dict(first),
                "is_compilation": bool(aggregate["is_compilation"]),
                "created_at": aggregate["created_at"],
                "release_mbid": (
                    str(release["release_mbid"]) if release is not None else None
                ),
            }

        return await self._read(operation)

    async def get_bounded_legacy_rows(
        self,
        table: str,
        *,
        after_rowid: int,
        limit: int,
    ) -> list[dict[str, Any]]:
        if table not in _LEGACY_REFERENCE_TABLES:
            raise ValueError(f"Unsupported bounded legacy table: {table}")

        def operation(connection: sqlite3.Connection) -> list[dict[str, Any]]:
            present = connection.execute(
                "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?",
                (table,),
            ).fetchone()
            if present is None:
                return []
            rows = connection.execute(
                f"SELECT rowid AS __migration_rowid, * FROM {table} "
                "WHERE rowid > ? ORDER BY rowid LIMIT ?",
                (after_rowid, limit),
            ).fetchall()
            return [dict(row) for row in rows]

        return await self._read(operation)

    async def get_bounded_legacy_cover_rows(
        self, release_group: str
    ) -> dict[str, list[dict[str, Any]]]:
        def operation(
            connection: sqlite3.Connection,
        ) -> dict[str, list[dict[str, Any]]]:
            present = {
                str(row["name"])
                for row in connection.execute(
                    "SELECT name FROM sqlite_master WHERE type = 'table'"
                )
            }
            metadata = []
            if "library_album_meta" in present:
                metadata = connection.execute(
                    "SELECT * FROM library_album_meta "
                    "WHERE release_group_mbid = ? LIMIT 1",
                    (release_group,),
                ).fetchall()
                if not metadata:
                    metadata = connection.execute(
                        "SELECT * FROM library_album_meta "
                        "WHERE LOWER(release_group_mbid) = ? LIMIT 1",
                        (release_group,),
                    ).fetchall()
            albums = (
                connection.execute(
                    "SELECT * FROM library_albums WHERE mbid_lower = ? LIMIT 1",
                    (release_group,),
                ).fetchall()
                if "library_albums" in present
                else []
            )
            return {
                "library_album_meta": [dict(row) for row in metadata],
                "library_albums": [dict(row) for row in albums],
            }

        return await self._read(operation)

    async def resolve_bounded_legacy_references(
        self, kind: str, rows: list[dict[str, Any]]
    ) -> list[tuple[str, str] | None]:
        def operation(
            connection: sqlite3.Connection,
        ) -> list[tuple[str, str] | None]:
            def map_kind(item_kind: object, item_id: object) -> tuple[str, str] | None:
                normalized = str(item_kind or "").casefold()
                source_id = str(item_id or "")
                if normalized in {"album", "music_album"}:
                    row = connection.execute(
                        "SELECT local_album_id FROM local_album_external_identities "
                        "WHERE release_group_mbid = ? UNION ALL "
                        "SELECT local_album_id FROM local_album_aliases "
                        "WHERE alias = ? LIMIT 1",
                        (source_id.casefold(), source_id.casefold()),
                    ).fetchone()
                    return (
                        ("local_album", str(row["local_album_id"]))
                        if row is not None
                        else None
                    )
                if normalized in {"artist", "music_artist"}:
                    row = connection.execute(
                        "SELECT local_artist_id FROM local_artist_external_identities "
                        "WHERE provider_artist_id = ? LIMIT 1",
                        (source_id.casefold(),),
                    ).fetchone()
                    return (
                        ("local_artist", str(row["local_artist_id"]))
                        if row is not None
                        else None
                    )
                if normalized in {"track", "song", "audio"}:
                    row = connection.execute(
                        "SELECT id FROM local_tracks WHERE id = ?",
                        (source_id,),
                    ).fetchone()
                    return ("local_track", source_id) if row is not None else None
                if normalized == "playlist":
                    row = connection.execute(
                        "SELECT id FROM playlists WHERE id = ?", (source_id,)
                    ).fetchone()
                    return ("playlist", source_id) if row is not None else None
                if normalized == "library" and source_id:
                    return "library", source_id
                if normalized == "genre" and source_id:
                    return "genre", source_id
                return None

            resolved: list[tuple[str, str] | None] = []
            for source in rows:
                if kind == "favorite":
                    resolved.append(
                        map_kind(source.get("item_kind"), source.get("item_id"))
                    )
                    continue
                if kind == "history":
                    target: tuple[str, str] | None = None
                    recording = str(source.get("recording_mbid") or "").casefold()
                    if recording:
                        matches = connection.execute(
                            "SELECT local_track_id FROM local_track_external_identities "
                            "WHERE recording_mbid = ? ORDER BY local_track_id LIMIT 2",
                            (recording,),
                        ).fetchall()
                        if len(matches) == 1:
                            target = "local_track", str(matches[0]["local_track_id"])
                    if target is None and source.get("release_group_mbid"):
                        target = map_kind("album", source["release_group_mbid"])
                    if target is None:
                        matches = connection.execute(
                            "SELECT id FROM local_tracks WHERE title_folded = ? "
                            "AND artist_name_folded = ? AND album_title_folded = ? "
                            "ORDER BY id LIMIT 2",
                            (
                                _fold(str(source.get("track_name") or "")),
                                _fold(str(source.get("artist_name") or "")),
                                _fold(str(source.get("album_name") or "")),
                            ),
                        ).fetchall()
                        if len(matches) == 1:
                            target = "local_track", str(matches[0]["id"])
                    resolved.append(target)
                    continue
                if kind == "playlist_track":
                    local_source = str(source.get("source_type") or "") in {
                        "local",
                        "droppedneedle-local",
                        "howler",
                    }
                    file_id = source.get("library_file_id") or (
                        source.get("track_source_id") if local_source else None
                    )
                    target = map_kind("track", file_id) if file_id else None
                    if target is not None and local_source:
                        membership = connection.execute(
                            "SELECT t.local_album_id, "
                            "COALESCE(ta.local_artist_id, a.album_artist_id) AS artist_id "
                            "FROM local_tracks t JOIN local_albums a "
                            "ON a.id = t.local_album_id LEFT JOIN local_track_artists ta "
                            "ON ta.local_track_id = t.id AND ta.position = 0 "
                            "WHERE t.id = ?",
                            (target[1],),
                        ).fetchone()
                        expected_album = (
                            map_kind("album", source["album_id"])
                            if source.get("album_id")
                            else None
                        )
                        expected_artist = (
                            map_kind("artist", source["artist_id"])
                            if source.get("artist_id")
                            else None
                        )
                        if (
                            membership is None
                            or (
                                expected_album is not None
                                and expected_album[1] != membership["local_album_id"]
                            )
                            or (
                                expected_artist is not None
                                and expected_artist[1] != membership["artist_id"]
                            )
                        ):
                            target = None
                    resolved.append(target)
                    continue
                if kind == "album_release_pin":
                    resolved.append(map_kind("album", source.get("release_group_mbid")))
                    continue
                if kind in {"compat_bookmark", "compat_play_queue_item"}:
                    resolved.append(map_kind("track", source.get("file_id")))
                    continue
                if kind == "compat_play_queue":
                    user_id = str(source.get("user_id") or "")
                    resolved.append(("compat_play_queue", user_id) if user_id else None)
                    continue
                if kind == "jellyfin_id_map":
                    resolved.append(
                        map_kind(source.get("kind"), source.get("internal_id"))
                    )
                    continue
                if kind in {"review_row", "manual_decision"}:
                    row = connection.execute(
                        "SELECT id FROM local_tracks WHERE file_path = ? ORDER BY id LIMIT 1",
                        (str(source.get("file_path") or ""),),
                    ).fetchone()
                    resolved.append(
                        ("local_track", str(row["id"])) if row is not None else None
                    )
                    continue
                raise ValueError(f"Unsupported bounded reference kind: {kind}")
            return resolved

        return await self._read(operation)

    async def apply_bounded_legacy_reviews(
        self, reviews: list[MigrationReview]
    ) -> None:
        def operation(connection: sqlite3.Connection) -> None:
            connection.executemany(
                "INSERT INTO library_identification_reviews "
                "(id, local_album_id, local_track_id, state, reason_code, input_revision, "
                "created_at, updated_at, decided_at) VALUES (?,?,?,?,?,?,?,?,?) "
                "ON CONFLICT(id) DO NOTHING",
                [
                    (
                        review.id,
                        review.local_album_id,
                        review.local_track_id,
                        review.state,
                        review.reason_code,
                        review.input_revision,
                        review.created_at,
                        review.updated_at,
                        review.decided_at,
                    )
                    for review in reviews
                ],
            )

        await self._write(operation)

    async def stage_bounded_legacy_review_groups(
        self, rows: list[tuple[int, str]]
    ) -> None:
        def operation(connection: sqlite3.Connection) -> None:
            connection.execute(
                "CREATE TABLE IF NOT EXISTS library_migration_review_staging ("
                "source_rowid INTEGER PRIMARY KEY, group_key TEXT NOT NULL)"
            )
            connection.execute(
                "CREATE INDEX IF NOT EXISTS idx_library_migration_review_staging_group "
                "ON library_migration_review_staging(group_key, source_rowid)"
            )
            connection.executemany(
                "INSERT OR REPLACE INTO library_migration_review_staging "
                "(source_rowid, group_key) VALUES (?, ?)",
                rows,
            )

        await self._write(operation)

    async def get_bounded_legacy_review_group_keys(
        self, *, after_key: str, limit: int
    ) -> list[str]:
        def operation(connection: sqlite3.Connection) -> list[str]:
            present = connection.execute(
                "SELECT 1 FROM sqlite_master WHERE type = 'table' "
                "AND name = 'library_migration_review_staging'"
            ).fetchone()
            if present is None:
                return []
            return [
                str(row["group_key"])
                for row in connection.execute(
                    "SELECT DISTINCT group_key FROM library_migration_review_staging "
                    "WHERE group_key > ? ORDER BY group_key LIMIT ?",
                    (after_key, limit),
                ).fetchall()
            ]

        return await self._read(operation)

    async def get_bounded_legacy_review_group_context(
        self, group_key: str
    ) -> dict[str, Any] | None:
        def operation(connection: sqlite3.Connection) -> dict[str, Any] | None:
            first = connection.execute(
                "SELECT q.* FROM manual_review_queue q "
                "JOIN library_migration_review_staging s ON s.source_rowid = q.rowid "
                "WHERE s.group_key = ? ORDER BY s.source_rowid LIMIT 1",
                (group_key,),
            ).fetchone()
            if first is None:
                return None
            created_at = connection.execute(
                "SELECT MIN(q.created_at) FROM manual_review_queue q "
                "JOIN library_migration_review_staging s ON s.source_rowid = q.rowid "
                "WHERE s.group_key = ?",
                (group_key,),
            ).fetchone()[0]
            return {"first": dict(first), "created_at": created_at}

        return await self._read(operation)

    async def get_bounded_legacy_pending_review_count(self) -> int:
        def operation(connection: sqlite3.Connection) -> int:
            present = connection.execute(
                "SELECT 1 FROM sqlite_master WHERE type = 'table' "
                "AND name = 'library_migration_review_staging'"
            ).fetchone()
            if present is None:
                return 0
            return int(
                connection.execute(
                    "SELECT COUNT(*) FROM manual_review_queue q "
                    "JOIN library_migration_review_staging s "
                    "ON s.source_rowid = q.rowid "
                    "WHERE NOT EXISTS (SELECT 1 FROM local_tracks t "
                    "WHERE t.file_path = q.file_path)"
                ).fetchone()[0]
            )

        return await self._read(operation)

    async def get_bounded_legacy_review_group_batch(
        self,
        group_key: str,
        *,
        after_rowid: int,
        limit: int,
    ) -> list[dict[str, Any]]:
        def operation(connection: sqlite3.Connection) -> list[dict[str, Any]]:
            return [
                dict(row)
                for row in connection.execute(
                    "SELECT s.source_rowid AS __migration_rowid, q.* "
                    "FROM manual_review_queue q "
                    "JOIN library_migration_review_staging s ON s.source_rowid = q.rowid "
                    "WHERE s.group_key = ? AND s.source_rowid > ? "
                    "AND NOT EXISTS (SELECT 1 FROM local_tracks t "
                    "WHERE t.file_path = q.file_path) "
                    "ORDER BY s.source_rowid LIMIT ?",
                    (group_key, after_rowid, limit),
                ).fetchall()
            ]

        return await self._read(operation)

    async def create_catalog_membership(self, membership: CatalogMembership) -> int:
        def operation(connection: sqlite3.Connection) -> int:
            self._insert_catalog_membership(connection, membership)
            return self._bump_catalog(connection)

        return await self._write(operation)

    def _insert_catalog_membership(
        self,
        connection: sqlite3.Connection,
        membership: CatalogMembership,
        *,
        allow_existing_artists: bool = False,
        allow_existing_album: bool = False,
    ) -> None:
        for artist in membership.artists:
            self._insert_artist(
                connection, artist, allow_existing=allow_existing_artists
            )
        album = membership.album
        connection.execute(
            f"INSERT {'OR IGNORE ' if allow_existing_album else ''}INTO local_albums "
            "(id, root_id, grouping_key, title, title_folded, album_artist_name, "
            "album_artist_name_folded, album_artist_id, album_artist_sort_name, year, "
            "original_release_date, primary_genre, is_compilation, grouping_source, "
            "grouping_locked, retired_into_album_id, created_at, updated_at, row_revision) "
            "VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)",
            (
                album.id,
                album.root_id,
                album.grouping_key,
                album.title,
                _fold(album.title),
                album.album_artist_name,
                _fold(album.album_artist_name),
                album.album_artist_id,
                album.album_artist_sort_name,
                album.year,
                album.original_release_date,
                album.primary_genre,
                int(album.is_compilation),
                album.grouping_source,
                int(album.grouping_locked),
                album.retired_into_album_id,
                album.created_at,
                album.updated_at,
                album.row_revision,
            ),
        )
        album_credits = membership.album_credits or []
        if not album_credits:
            connection.execute(
                f"INSERT {'OR IGNORE ' if allow_existing_album else ''}INTO local_album_artists "
                "(local_album_id, position, local_artist_id, role, credited_name) "
                "VALUES (?, 0, ?, 'primary', ?)",
                (album.id, album.album_artist_id, album.album_artist_name),
            )
        else:
            connection.executemany(
                f"INSERT {'OR IGNORE ' if allow_existing_album else ''}INTO local_album_artists "
                "(local_album_id, position, local_artist_id, role, credited_name) "
                "VALUES (?,?,?,?,?)",
                [
                    (
                        album.id,
                        credit.position,
                        credit.local_artist_id,
                        credit.role,
                        credit.credited_name,
                    )
                    for credit in album_credits
                ],
            )
        for track in membership.tracks:
            self._insert_track(connection, track)
            credits = membership.track_credits.get(track.id, [])
            connection.executemany(
                "INSERT INTO local_track_artists "
                "(local_track_id, position, local_artist_id, role, credited_name) "
                "VALUES (?,?,?,?,?)",
                [
                    (
                        track.id,
                        credit.position,
                        credit.local_artist_id,
                        credit.role,
                        credit.credited_name,
                    )
                    for credit in credits
                ],
            )

    @staticmethod
    def _insert_artist(
        connection: sqlite3.Connection,
        artist: LocalArtist,
        *,
        allow_existing: bool = False,
    ) -> None:
        connection.execute(
            f"INSERT {'OR IGNORE ' if allow_existing else ''}INTO local_artists "
            "(id, display_name, sort_name, folded_name, normalized_name, kind, created_at, updated_at, row_revision) "
            "VALUES (?,?,?,?,?,?,?,?,?)",
            (
                artist.id,
                artist.display_name,
                artist.sort_name,
                artist.folded_name,
                artist.normalized_name or _normalize_exact(artist.display_name),
                artist.kind,
                artist.created_at,
                artist.updated_at,
                artist.row_revision,
            ),
        )

    @staticmethod
    def _insert_track(connection: sqlite3.Connection, track: LocalTrack) -> None:
        connection.execute(
            "INSERT INTO local_tracks "
            "(id, local_album_id, root_id, file_path, relative_path, path_hash, "
            "file_size_bytes, file_mtime_ns, stat_revision, tag_revision, tags_read_at, "
            "stat_revision_kind, "
            "metadata_incomplete, title, title_folded, artist_name, artist_name_folded, "
            "album_title, album_title_folded, album_artist_name, album_artist_name_folded, "
            "tag_album_title, tag_album_artist_name, "
            "disc_number, track_number, year, genre, genre_folded, title_sort, artist_sort, album_sort, "
            "album_artist_sort, disc_subtitle, is_compilation, embedded_release_group_mbid, "
            "embedded_release_mbid, embedded_recording_mbid, embedded_artist_mbid, "
            "embedded_album_artist_mbid, duration_seconds, file_format, "
            "bit_rate, sample_rate, bit_depth, channels, replaygain_track_gain, "
            "replaygain_album_gain, replaygain_track_peak, replaygain_album_peak, availability, "
            "missing_since, excluded_at, ingest_source, download_task_id, source_path, "
            "imported_at, membership_source, membership_locked, manual_excluded, desired_policy_revision, "
            "applied_policy_revision, applied_policy, row_revision) "
            "VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)",
            (
                track.id,
                track.local_album_id,
                track.root_id,
                track.file_path,
                track.relative_path,
                track.path_hash,
                track.file_size_bytes,
                track.file_mtime_ns,
                track.stat_revision,
                track.tag_revision,
                track.tags_read_at,
                track.stat_revision_kind,
                int(track.metadata_incomplete),
                track.title,
                _fold(track.title),
                track.artist_name,
                _fold(track.artist_name),
                track.album_title,
                _fold(track.album_title),
                track.album_artist_name,
                _fold(track.album_artist_name),
                track.tag_album_title,
                track.tag_album_artist_name,
                track.disc_number,
                track.track_number,
                track.year,
                track.genre,
                _fold(track.genre),
                track.title_sort,
                track.artist_sort,
                track.album_sort,
                track.album_artist_sort,
                track.disc_subtitle,
                int(track.is_compilation),
                track.embedded_release_group_mbid,
                track.embedded_release_mbid,
                track.embedded_recording_mbid,
                track.embedded_artist_mbid,
                track.embedded_album_artist_mbid,
                track.duration_seconds,
                track.file_format,
                track.bit_rate,
                track.sample_rate,
                track.bit_depth,
                track.channels,
                track.replaygain_track_gain,
                track.replaygain_album_gain,
                track.replaygain_track_peak,
                track.replaygain_album_peak,
                track.availability,
                track.missing_since,
                track.excluded_at,
                track.ingest_source,
                track.download_task_id,
                track.source_path,
                track.imported_at,
                track.membership_source,
                int(track.membership_locked),
                int(track.manual_excluded),
                track.desired_policy_revision,
                track.applied_policy_revision,
                track.applied_policy,
                track.row_revision,
            ),
        )

    async def get_local_album(self, album_id: str) -> dict[str, Any] | None:
        def operation(connection: sqlite3.Connection) -> dict[str, Any] | None:
            return _row(
                connection.execute(
                    "SELECT * FROM local_albums WHERE id = ?", (album_id,)
                ).fetchone()
            )

        return await self._read(operation)

    async def get_local_track(self, track_id: str) -> dict[str, Any] | None:
        def operation(connection: sqlite3.Connection) -> dict[str, Any] | None:
            return _row(
                connection.execute(
                    "SELECT * FROM local_tracks WHERE id = ?", (track_id,)
                ).fetchone()
            )

        return await self._read(operation)

    async def get_album_tracks(self, album_id: str) -> list[dict[str, Any]]:
        def operation(connection: sqlite3.Connection) -> list[dict[str, Any]]:
            rows = connection.execute(
                "SELECT * FROM local_tracks WHERE local_album_id = ? "
                "ORDER BY disc_number, track_number, id",
                (album_id,),
            ).fetchall()
            return [dict(item) for item in rows]

        return await self._read(operation)

    async def resolve_album_alias(self, alias: str) -> str | None:
        def operation(connection: sqlite3.Connection) -> str | None:
            row = connection.execute(
                "SELECT local_album_id FROM local_album_aliases WHERE alias = ?",
                (alias.casefold(),),
            ).fetchone()
            return str(row["local_album_id"]) if row is not None else None

        return await self._read(operation)

    async def resolve_or_create_local_artist(
        self,
        *,
        display_name: str,
        sort_name: str | None,
        kind: str,
        candidate_id: str,
        now: float,
    ) -> tuple[str, bool]:
        """Reuse only one exact, compatible artist; surface every ambiguity."""
        normalized = _normalize_exact(display_name)
        folded = _fold(display_name) or ""
        normalized_sort = _normalize_exact(sort_name)

        def operation(connection: sqlite3.Connection) -> tuple[str, bool]:
            return self._resolve_or_create_local_artist(
                connection,
                display_name=display_name,
                sort_name=sort_name,
                kind=kind,
                candidate_id=candidate_id,
                normalized=normalized,
                folded=folded,
                normalized_sort=normalized_sort,
                now=now,
            )

        return await self._write(operation)

    async def resolve_or_create_local_artists(
        self,
        candidates: list[tuple[str, str | None, str, str]],
        *,
        now: float,
        background: bool = False,
    ) -> dict[str, tuple[str, bool]]:
        """Resolve one bounded artist batch in a single store-owned transaction."""

        def operation(
            connection: sqlite3.Connection,
        ) -> dict[str, tuple[str, bool]]:
            resolved: dict[str, tuple[str, bool]] = {}
            for display_name, sort_name, kind, candidate_id in candidates:
                resolved[candidate_id] = self._resolve_or_create_local_artist(
                    connection,
                    display_name=display_name,
                    sort_name=sort_name,
                    kind=kind,
                    candidate_id=candidate_id,
                    normalized=_normalize_exact(display_name),
                    folded=_fold(display_name) or "",
                    normalized_sort=_normalize_exact(sort_name),
                    now=now,
                )
            return resolved

        if background:
            return await super()._background_write(operation)
        return await self._write(operation)

    @staticmethod
    def _resolve_or_create_local_artist(
        connection: sqlite3.Connection,
        *,
        display_name: str,
        sort_name: str | None,
        kind: str,
        candidate_id: str,
        normalized: str,
        folded: str,
        normalized_sort: str,
        now: float,
    ) -> tuple[str, bool]:
        prior_candidate = connection.execute(
            "SELECT id FROM local_artists WHERE id = ? AND retired_into_artist_id IS NULL",
            (candidate_id,),
        ).fetchone()
        if prior_candidate is not None:
            return candidate_id, True
        exact = connection.execute(
            "SELECT id, sort_name FROM local_artists WHERE normalized_name = ? "
            "AND kind = ? AND retired_into_artist_id IS NULL ORDER BY created_at, id",
            (normalized, kind),
        ).fetchall()
        compatible = [
            row
            for row in exact
            if not normalized_sort
            or not _normalize_exact(row["sort_name"])
            or _normalize_exact(row["sort_name"]) == normalized_sort
        ]
        if len(compatible) == 1:
            return str(compatible[0]["id"]), True
        collisions = connection.execute(
            "SELECT id FROM local_artists WHERE folded_name = ? "
            "AND retired_into_artist_id IS NULL ORDER BY created_at, id",
            (folded,),
        ).fetchall()
        connection.execute(
            "INSERT INTO local_artists "
            "(id, display_name, sort_name, folded_name, normalized_name, kind, "
            "created_at, updated_at) VALUES (?,?,?,?,?,?,?,?)",
            (
                candidate_id,
                display_name,
                sort_name,
                folded,
                normalized,
                kind,
                now,
                now,
            ),
        )
        for collision in collisions:
            left, right = sorted((candidate_id, str(collision["id"])))
            connection.execute(
                "INSERT OR IGNORE INTO local_artist_merge_candidates "
                "(id, left_artist_id, right_artist_id, reason_code, created_at, updated_at) "
                "VALUES (?,?,?,?,?,?)",
                (
                    str(
                        uuid.uuid5(
                            uuid.NAMESPACE_URL, f"artist-collision:{left}:{right}"
                        )
                    ),
                    left,
                    right,
                    "FOLDED_NAME_COLLISION",
                    now,
                    now,
                ),
            )
        return candidate_id, False

    async def resolve_artist_alias(self, alias: str) -> str | None:
        def operation(connection: sqlite3.Connection) -> str | None:
            row = connection.execute(
                "SELECT local_artist_id FROM local_artist_aliases WHERE alias = ?",
                (alias.casefold(),),
            ).fetchone()
            return str(row["local_artist_id"]) if row is not None else None

        return await self._read(operation)

    async def get_local_artwork(self, album_id: str) -> dict[str, Any] | None:
        def operation(connection: sqlite3.Connection) -> dict[str, Any] | None:
            return _row(
                connection.execute(
                    "SELECT * FROM local_album_artwork WHERE local_album_id = ?",
                    (album_id,),
                ).fetchone()
            )

        return await self._read(operation)

    async def search_local_tracks(self, query: str) -> list[dict[str, Any]]:
        folded = _fold(query) or ""

        def operation(connection: sqlite3.Connection) -> list[dict[str, Any]]:
            rows = connection.execute(
                "SELECT * FROM local_tracks WHERE availability = 'indexed' AND "
                "(title_folded LIKE ? OR artist_name_folded LIKE ? OR album_title_folded LIKE ?) "
                "ORDER BY title_folded, id LIMIT 100",
                (f"%{folded}%", f"%{folded}%", f"%{folded}%"),
            ).fetchall()
            return [dict(row) for row in rows]

        return await self._read(operation)

    async def attach_album_identity(
        self,
        identity: LocalAlbumExternalIdentity,
        *,
        expected_album_revision: int,
    ) -> tuple[int, int]:
        def operation(connection: sqlite3.Connection) -> tuple[int, int]:
            new_revision = self._require_revision_update(
                connection,
                table="local_albums",
                entity_id=identity.local_album_id,
                expected_revision=expected_album_revision,
                assignments="updated_at = ?",
                parameters=(identity.selected_at,),
            )
            existing = connection.execute(
                "SELECT row_revision FROM local_album_external_identities "
                "WHERE local_album_id = ? AND provider = 'musicbrainz'",
                (identity.local_album_id,),
            ).fetchone()
            if existing is None:
                connection.execute(
                    "INSERT INTO local_album_external_identities "
                    "(local_album_id, provider, release_group_mbid, release_mbid, "
                    "decision_source, matcher_version, attempt_id, selected_by_user_id, "
                    "selected_at, row_revision) VALUES (?, 'musicbrainz', ?, ?, ?, ?, ?, ?, ?, ?)",
                    (
                        identity.local_album_id,
                        identity.release_group_mbid,
                        identity.release_mbid,
                        identity.decision_source,
                        identity.matcher_version,
                        identity.attempt_id,
                        identity.selected_by_user_id,
                        identity.selected_at,
                        identity.row_revision,
                    ),
                )
            else:
                updated = connection.execute(
                    "UPDATE local_album_external_identities SET release_group_mbid = ?, "
                    "release_mbid = ?, decision_source = ?, matcher_version = ?, attempt_id = ?, "
                    "selected_by_user_id = ?, selected_at = ?, row_revision = row_revision + 1 "
                    "WHERE local_album_id = ? AND provider = 'musicbrainz' AND row_revision < ?",
                    (
                        identity.release_group_mbid,
                        identity.release_mbid,
                        identity.decision_source,
                        identity.matcher_version,
                        identity.attempt_id,
                        identity.selected_by_user_id,
                        identity.selected_at,
                        identity.local_album_id,
                        MAX_REVISION,
                    ),
                ).rowcount
                if updated != 1:
                    raise RevisionOverflowError(
                        "An album identity revision reached its maximum value."
                    )
            return new_revision, self._bump_catalog(connection)

        return await self._write(operation)

    async def detach_album_identity(
        self,
        album_id: str,
        *,
        expected_album_revision: int,
        expected_identity_revision: int,
        updated_at: float,
    ) -> tuple[int, int]:
        def operation(connection: sqlite3.Connection) -> tuple[int, int]:
            identity = connection.execute(
                "DELETE FROM local_album_external_identities WHERE local_album_id = ? "
                "AND provider = 'musicbrainz' AND row_revision = ? RETURNING local_album_id",
                (album_id, expected_identity_revision),
            ).fetchone()
            if identity is None:
                exists = connection.execute(
                    "SELECT row_revision FROM local_album_external_identities "
                    "WHERE local_album_id = ? AND provider = 'musicbrainz'",
                    (album_id,),
                ).fetchone()
                if exists is None:
                    raise ResourceNotFoundError(f"Album identity not found: {album_id}")
                raise StaleRevisionError(
                    "The album identity changed before it could be detached."
                )
            album_revision = self._require_revision_update(
                connection,
                table="local_albums",
                entity_id=album_id,
                expected_revision=expected_album_revision,
                assignments="updated_at = ?",
                parameters=(updated_at,),
            )
            return album_revision, self._bump_catalog(connection)

        return await self._write(operation)

    async def attach_artist_identity_with_aliases(
        self,
        identity: LocalArtistExternalIdentity,
        aliases: list[LocalArtistAlias],
        *,
        expected_artist_revision: int,
    ) -> tuple[int, int]:
        def operation(connection: sqlite3.Connection) -> tuple[int, int]:
            artist_revision = self._require_revision_update(
                connection,
                table="local_artists",
                entity_id=identity.local_artist_id,
                expected_revision=expected_artist_revision,
                assignments="updated_at = ?",
                parameters=(identity.selected_at,),
            )
            connection.execute(
                "INSERT INTO local_artist_external_identities "
                "(local_artist_id, provider, provider_artist_id, decision_source, attempt_id, "
                "selected_by_user_id, selected_at, row_revision) "
                "VALUES (?, 'musicbrainz', ?, ?, ?, ?, ?, ?)",
                (
                    identity.local_artist_id,
                    identity.provider_artist_id,
                    identity.decision_source,
                    identity.attempt_id,
                    identity.selected_by_user_id,
                    identity.selected_at,
                    identity.row_revision,
                ),
            )
            connection.executemany(
                "INSERT INTO local_artist_aliases "
                "(alias, local_artist_id, kind, created_at) VALUES (?,?,?,?)",
                [
                    (alias.alias, alias.local_artist_id, alias.kind, alias.created_at)
                    for alias in aliases
                ],
            )
            return artist_revision, self._bump_catalog(connection)

        return await self._write(operation)

    async def add_album_aliases(
        self,
        album_id: str,
        aliases: list[LocalAlbumAlias],
        *,
        expected_album_revision: int,
        updated_at: float,
    ) -> tuple[int, int]:
        def operation(connection: sqlite3.Connection) -> tuple[int, int]:
            album_revision = self._require_revision_update(
                connection,
                table="local_albums",
                entity_id=album_id,
                expected_revision=expected_album_revision,
                assignments="updated_at = ?",
                parameters=(updated_at,),
            )
            connection.executemany(
                "INSERT INTO local_album_aliases "
                "(alias, local_album_id, kind, created_at) VALUES (?,?,?,?)",
                [
                    (alias.alias, alias.local_album_id, alias.kind, alias.created_at)
                    for alias in aliases
                ],
            )
            return album_revision, self._bump_catalog(connection)

        return await self._write(operation)

    async def attach_track_identity(
        self, identity: LocalTrackExternalIdentity, *, expected_track_revision: int
    ) -> tuple[int, int]:
        def operation(connection: sqlite3.Connection) -> tuple[int, int]:
            new_revision = self._require_revision_update(
                connection,
                table="local_tracks",
                entity_id=identity.local_track_id,
                expected_revision=expected_track_revision,
                assignments="imported_at = imported_at",
                parameters=(),
            )
            existing = connection.execute(
                "SELECT row_revision FROM local_track_external_identities "
                "WHERE local_track_id = ? AND provider = 'musicbrainz'",
                (identity.local_track_id,),
            ).fetchone()
            if existing is not None and int(existing["row_revision"]) >= MAX_REVISION:
                raise RevisionOverflowError(
                    "A track identity revision reached its maximum value."
                )
            connection.execute(
                "INSERT INTO local_track_external_identities "
                "(local_track_id, provider, recording_mbid, release_mbid, decision_source, "
                "attempt_id, selected_at, row_revision) VALUES (?, 'musicbrainz', ?, ?, ?, ?, ?, ?) "
                "ON CONFLICT(local_track_id, provider) DO UPDATE SET recording_mbid = excluded.recording_mbid, "
                "release_mbid = excluded.release_mbid, decision_source = excluded.decision_source, "
                "attempt_id = excluded.attempt_id, selected_at = excluded.selected_at, "
                "row_revision = local_track_external_identities.row_revision + 1",
                (
                    identity.local_track_id,
                    identity.recording_mbid,
                    identity.release_mbid,
                    identity.decision_source,
                    identity.attempt_id,
                    identity.selected_at,
                    identity.row_revision,
                ),
            )
            return new_revision, self._bump_catalog(connection)

        return await self._write(operation)

    async def detach_track_identity(
        self,
        track_id: str,
        *,
        expected_track_revision: int,
        expected_identity_revision: int,
    ) -> tuple[int, int]:
        def operation(connection: sqlite3.Connection) -> tuple[int, int]:
            identity = connection.execute(
                "DELETE FROM local_track_external_identities WHERE local_track_id = ? "
                "AND provider = 'musicbrainz' AND row_revision = ? RETURNING local_track_id",
                (track_id, expected_identity_revision),
            ).fetchone()
            if identity is None:
                exists = connection.execute(
                    "SELECT row_revision FROM local_track_external_identities "
                    "WHERE local_track_id = ? AND provider = 'musicbrainz'",
                    (track_id,),
                ).fetchone()
                if exists is None:
                    raise ResourceNotFoundError(f"Track identity not found: {track_id}")
                raise StaleRevisionError(
                    "The track identity changed before it could be detached."
                )
            track_revision = self._require_revision_update(
                connection,
                table="local_tracks",
                entity_id=track_id,
                expected_revision=expected_track_revision,
                assignments="imported_at = imported_at",
                parameters=(),
            )
            return track_revision, self._bump_catalog(connection)

        return await self._write(operation)

    async def set_artwork(
        self, artwork: LocalArtworkAssociation, *, expected_album_revision: int
    ) -> tuple[int, int]:
        def operation(connection: sqlite3.Connection) -> tuple[int, int]:
            self._refuse_max_revision(
                connection,
                table="local_album_artwork",
                predicate="local_album_id = ?",
                parameters=(artwork.local_album_id,),
                include_version=True,
            )
            album_revision = self._require_revision_update(
                connection,
                table="local_albums",
                entity_id=artwork.local_album_id,
                expected_revision=expected_album_revision,
                assignments="updated_at = ?",
                parameters=(artwork.updated_at,),
            )
            connection.execute(
                "INSERT INTO local_album_artwork "
                "(local_album_id, cover_url, source, source_locator, version, updated_at, row_revision) "
                "VALUES (?,?,?,?,?,?,?) ON CONFLICT(local_album_id) DO UPDATE SET "
                "cover_url = excluded.cover_url, source = excluded.source, "
                "source_locator = excluded.source_locator, version = local_album_artwork.version + 1, "
                "updated_at = excluded.updated_at, row_revision = local_album_artwork.row_revision + 1",
                (
                    artwork.local_album_id,
                    artwork.cover_url,
                    artwork.source,
                    artwork.source_locator,
                    artwork.version,
                    artwork.updated_at,
                    artwork.row_revision,
                ),
            )
            return album_revision, self._bump_catalog(connection)

        return await self._write(operation)

    async def backfill_identified_provider_artwork(self, *, updated_at: float) -> int:
        """Create reproducible provider artwork associations missed by early imports."""

        def operation(connection: sqlite3.Connection) -> int:
            cursor = connection.execute(
                "INSERT INTO local_album_artwork "
                "(local_album_id, cover_url, source, source_locator, version, updated_at, row_revision) "
                "SELECT identity.local_album_id, NULL, 'provider', "
                "identity.release_group_mbid, 1, ?, 1 "
                "FROM local_album_external_identities identity "
                "LEFT JOIN local_album_artwork artwork "
                "ON artwork.local_album_id = identity.local_album_id "
                "WHERE identity.provider = 'musicbrainz' "
                "AND artwork.local_album_id IS NULL",
                (updated_at,),
            )
            changed = int(cursor.rowcount)
            if changed:
                self._bump_catalog(connection)
            return changed

        return await self._write(operation)

    async def create_review(self, decision: ReviewDecision) -> None:
        def operation(connection: sqlite3.Connection) -> None:
            connection.execute(
                "INSERT INTO library_identification_reviews "
                "(id, local_album_id, local_track_id, state, reason_code, attempt_id, "
                "input_revision, decision_revision, decided_by_user_id, created_at, "
                "updated_at, decided_at, row_revision) VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?)",
                (
                    decision.id,
                    decision.local_album_id,
                    decision.local_track_id,
                    decision.state,
                    decision.reason_code,
                    decision.attempt_id,
                    decision.input_revision,
                    decision.decision_revision,
                    decision.decided_by_user_id,
                    decision.created_at,
                    decision.updated_at,
                    decision.decided_at,
                    decision.row_revision,
                ),
            )

        await self._write(operation)

    async def decide_review(
        self,
        review_id: str,
        *,
        expected_review_revision: int,
        state: str,
        reason_code: str,
        decided_by_user_id: str | None,
        decided_at: float,
    ) -> tuple[int, int]:
        def operation(connection: sqlite3.Connection) -> tuple[int, int]:
            review = connection.execute(
                "SELECT local_album_id, local_track_id FROM library_identification_reviews "
                "WHERE id = ?",
                (review_id,),
            ).fetchone()
            if review is None:
                raise ResourceNotFoundError(f"Review not found: {review_id}")
            if state == "keep_tagged" and review["local_album_id"] is not None:
                identity = connection.execute(
                    "SELECT 1 FROM local_album_external_identities WHERE local_album_id = ?",
                    (review["local_album_id"],),
                ).fetchone()
                if identity is not None:
                    raise StaleRevisionError(
                        "Detach the external identity before keeping this album as tagged."
                    )
            new_revision = self._require_revision_update(
                connection,
                table="library_identification_reviews",
                entity_id=review_id,
                expected_revision=expected_review_revision,
                assignments=(
                    "state = ?, reason_code = ?, decided_by_user_id = ?, decided_at = ?, "
                    "updated_at = ?, decision_revision = decision_revision + 1"
                ),
                parameters=(
                    state,
                    reason_code,
                    decided_by_user_id,
                    decided_at,
                    decided_at,
                ),
            )
            if state == "excluded":
                subject_column = (
                    "local_album_id" if review["local_album_id"] is not None else "id"
                )
                subject_id = review["local_album_id"] or review["local_track_id"]
                self._refuse_max_revision(
                    connection,
                    table="local_tracks",
                    predicate=f"{subject_column} = ?",
                    parameters=(subject_id,),
                )
                if review["local_album_id"] is not None:
                    connection.execute(
                        "UPDATE local_tracks SET availability = 'excluded', excluded_at = ?, "
                        "row_revision = row_revision + 1 WHERE local_album_id = ?",
                        (decided_at, review["local_album_id"]),
                    )
                else:
                    connection.execute(
                        "UPDATE local_tracks SET availability = 'excluded', excluded_at = ?, "
                        "row_revision = row_revision + 1 WHERE id = ?",
                        (decided_at, review["local_track_id"]),
                    )
            return new_revision, self._bump_catalog(connection)

        return await self._write(operation)

    async def replace_review_attempt(
        self,
        review_id: str,
        *,
        expected_review_revision: int,
        attempt: IdentificationAttempt,
        evidence: list[IdentificationEvidenceRecord],
        updated_at: float,
    ) -> int:
        def operation(connection: sqlite3.Connection) -> int:
            connection.execute(
                "INSERT INTO library_identification_attempts "
                "(id, local_album_id, local_track_id, trigger, requested_by_user_id, "
                "input_tag_revision, input_policy_revision, input_file_revision, matcher_version, "
                "state, terminal_reason_code, selected_candidate_key, candidate_count, "
                "degradation_flags_json, started_at, completed_at) "
                "VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)",
                (
                    attempt.id,
                    attempt.local_album_id,
                    attempt.local_track_id,
                    attempt.trigger,
                    attempt.requested_by_user_id,
                    attempt.input_tag_revision,
                    attempt.input_policy_revision,
                    attempt.input_file_revision,
                    attempt.matcher_version,
                    attempt.state,
                    attempt.terminal_reason_code,
                    attempt.selected_candidate_key,
                    attempt.candidate_count,
                    json.dumps(attempt.degradation_flags, separators=(",", ":")),
                    attempt.started_at,
                    attempt.completed_at,
                ),
            )
            self._insert_evidence(connection, evidence)
            return self._require_revision_update(
                connection,
                table="library_identification_reviews",
                entity_id=review_id,
                expected_revision=expected_review_revision,
                assignments="attempt_id = ?, updated_at = ?, decision_revision = decision_revision + 1",
                parameters=(attempt.id, updated_at),
            )

        return await self._write(operation)

    @staticmethod
    def _insert_evidence(
        connection: sqlite3.Connection,
        evidence: list[IdentificationEvidenceRecord],
    ) -> None:
        for record in evidence:
            encoded = msgspec.json.encode(record.evidence)
            connection.execute(
                "INSERT INTO library_identification_evidence "
                "(id, attempt_id, candidate_key, evidence_json, evidence_size_bytes, created_at) "
                "VALUES (?,?,?,?,?,?)",
                (
                    record.id,
                    record.attempt_id,
                    record.candidate_key,
                    encoded,
                    len(encoded),
                    record.created_at,
                ),
            )

    async def enqueue_identification_job(
        self, job: IdentificationJob, *, expected_policy_revision: str | None = None
    ) -> str:
        job_id, _ = await self.enqueue_identification_job_result(
            job, expected_policy_revision=expected_policy_revision
        )
        return job_id

    async def enqueue_identification_job_result(
        self,
        job: IdentificationJob,
        *,
        expected_policy_revision: str | None = None,
    ) -> tuple[str, bool]:
        def operation(connection: sqlite3.Connection) -> tuple[str, bool]:
            if expected_policy_revision is not None:
                self._require_policy_revision_sync(
                    connection, expected_policy_revision=expected_policy_revision
                )
                if job.local_album_id is not None:
                    stale = connection.execute(
                        "SELECT 1 FROM local_tracks WHERE local_album_id = ? "
                        "AND COALESCE(desired_policy_revision, '') != ? LIMIT 1",
                        (job.local_album_id, expected_policy_revision),
                    ).fetchone()
                    if stale is not None:
                        raise StaleRevisionError(
                            "The library policy changed while identification was queued."
                        )
            return self._enqueue_identification_job_result(connection, job)

        return await self._write(operation)

    async def enqueue_identification_job_results(
        self,
        jobs: list[IdentificationJob],
        *,
        scan_run_id: str | None = None,
        grouping_context: tuple[str, str] | None = None,
        queue_cursor: str | None = None,
        background: bool = False,
    ) -> list[tuple[str, bool]]:
        """Enqueue one bounded job batch in a single store-owned transaction."""

        def operation(connection: sqlite3.Connection) -> list[tuple[str, bool]]:
            results = [
                self._enqueue_identification_job_result(connection, job) for job in jobs
            ]
            created = sum(created for _, created in results)
            if scan_run_id is not None and created:
                updated = connection.execute(
                    "UPDATE library_scan_runs SET identification_enqueued_count = "
                    "identification_enqueued_count + ?, row_revision = row_revision + 1, "
                    "event_revision = event_revision + 1 WHERE id = ? "
                    "AND row_revision < ? AND event_revision < ?",
                    (created, scan_run_id, MAX_REVISION, MAX_REVISION),
                )
                if updated.rowcount != 1:
                    raise RevisionOverflowError(
                        "A scan run revision reached its maximum value."
                    )
                self._bump_stream(connection, "scan")
            if grouping_context is not None:
                if scan_run_id is None or queue_cursor is None:
                    raise ValueError(
                        "A grouping queue cursor requires its scan run and context."
                    )
                root_id, relative_directory = grouping_context
                updated = connection.execute(
                    "UPDATE library_scan_grouping_contexts SET queue_cursor=?,"
                    "row_revision=row_revision+1 WHERE run_id=? AND root_id=? "
                    "AND relative_directory=? AND state='pending'",
                    (queue_cursor, scan_run_id, root_id, relative_directory),
                )
                if updated.rowcount != 1:
                    raise StaleRevisionError(
                        "The grouping context is no longer pending."
                    )
            return results

        if background:
            return await super()._background_write(operation)
        return await self._write(operation)

    def _enqueue_identification_job_result(
        self, connection: sqlite3.Connection, job: IdentificationJob
    ) -> tuple[str, bool]:
        subject_column = (
            "local_album_id" if job.local_album_id is not None else "local_track_id"
        )
        subject_id = job.local_album_id or job.local_track_id
        if job.local_album_id is not None and job.kind == "automatic":
            protected = connection.execute(
                "SELECT 1 FROM local_album_external_identities WHERE local_album_id = ? "
                "AND provider = 'musicbrainz' AND decision_source = 'legacy_import'",
                (job.local_album_id,),
            ).fetchone()
            if protected is not None:
                return "", False
        decision = connection.execute(
            f"SELECT id, state, input_revision FROM library_identification_reviews "
            f"WHERE {subject_column} = ? AND state IN ('keep_tagged','excluded') "
            "ORDER BY updated_at DESC LIMIT 1",
            (subject_id,),
        ).fetchone()
        if decision is not None:
            if decision["state"] == "excluded":
                return "", False
            if str(decision["input_revision"]) == job.input_revision:
                return "", False
            connection.execute(
                "UPDATE library_identification_reviews SET state = 'resolved', "
                "updated_at = ?, row_revision = row_revision + 1 WHERE id = ?",
                (job.created_at, decision["id"]),
            )
        existing = connection.execute(
            "SELECT id FROM library_identification_jobs WHERE dedupe_key = ? "
            + (
                "AND state IN ('queued','running','paused') "
                if job.kind == "review_retry"
                else ""
            )
            + "ORDER BY enqueue_sequence DESC LIMIT 1",
            (job.dedupe_key,),
        ).fetchone()
        if existing is not None:
            return str(existing["id"]), False
        queued_subject = connection.execute(
            f"SELECT id FROM library_identification_jobs WHERE {subject_column} = ? "
            "AND kind = ? AND state = 'queued' ORDER BY enqueue_sequence LIMIT 1",
            (subject_id, job.kind),
        ).fetchone()
        if queued_subject is not None:
            connection.execute(
                "UPDATE library_identification_jobs SET input_revision = ?, dedupe_key = ?, "
                "priority = MIN(priority, ?), updated_at = ?, row_revision = row_revision + 1 "
                "WHERE id = ?",
                (
                    job.input_revision,
                    job.dedupe_key,
                    job.priority,
                    job.created_at,
                    queued_subject["id"],
                ),
            )
            return str(queued_subject["id"]), False
        sequence = self._increment_singleton(
            connection, "library_enqueue_sequence", "singleton", 1
        )
        connection.execute(
            "INSERT INTO library_identification_jobs "
            "(id, local_album_id, local_track_id, kind, state, priority, enqueue_sequence, "
            "input_revision, dedupe_key, not_before, requested_by_user_id, created_at, updated_at) "
            "VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?)",
            (
                job.id,
                job.local_album_id,
                job.local_track_id,
                job.kind,
                job.state,
                job.priority,
                sequence,
                job.input_revision,
                job.dedupe_key,
                job.not_before,
                job.requested_by_user_id,
                job.created_at,
                job.created_at,
            ),
        )
        return job.id, True

    @staticmethod
    def _require_policy_revision_sync(
        connection: sqlite3.Connection, *, expected_policy_revision: str
    ) -> None:
        transition = connection.execute(
            "SELECT proposed_policy_revision FROM library_policy_transitions "
            "WHERE singleton = 1 AND state = 'prepared'"
        ).fetchone()
        if transition is not None:
            desired = str(transition["proposed_policy_revision"])
        else:
            state = connection.execute(
                "SELECT desired_policy_revision FROM library_policy_state "
                "WHERE singleton = 1"
            ).fetchone()
            if state is None:
                return
            desired = str(state["desired_policy_revision"])
        if desired != expected_policy_revision:
            raise StaleRevisionError(
                "The library policy changed while the file was being imported."
            )

    async def claim_identification_job(
        self, worker_id: str, *, now: float, lease_seconds: float
    ) -> dict[str, Any] | None:
        def operation(connection: sqlite3.Connection) -> dict[str, Any] | None:
            control = connection.execute(
                "SELECT state, high_priority_claim_count FROM library_work_control "
                "WHERE queue_kind = 'identification'"
            ).fetchone()
            if control is not None and control["state"] == "paused":
                return None
            streak = int(control["high_priority_claim_count"] if control else 0)
            minimum = connection.execute(
                "SELECT MIN(priority) AS value FROM library_identification_jobs "
                "WHERE state = 'queued' AND not_before <= ?",
                (now,),
            ).fetchone()["value"]
            if minimum is None:
                return None
            fairness_clause = ""
            parameters: tuple[Any, ...] = (now,)
            if streak >= 5:
                lower = connection.execute(
                    "SELECT 1 FROM library_identification_jobs WHERE state = 'queued' "
                    "AND not_before <= ? AND priority > ? LIMIT 1",
                    (now, minimum),
                ).fetchone()
                if lower is not None:
                    fairness_clause = "AND priority > ? "
                    parameters = (now, minimum)
            candidate = connection.execute(
                "SELECT id, row_revision FROM library_identification_jobs "
                f"WHERE state = 'queued' AND not_before <= ? {fairness_clause}"
                "ORDER BY priority ASC, enqueue_sequence ASC LIMIT 1",
                parameters,
            ).fetchone()
            if candidate is None:
                return None
            updated = connection.execute(
                "UPDATE library_identification_jobs SET state = 'running', lease_owner = ?, "
                "lease_expires_at = ?, heartbeat_at = ?, attempt_count = attempt_count + 1, "
                "updated_at = ?, row_revision = row_revision + 1, event_revision = event_revision + 1 "
                "WHERE id = ? AND state = 'queued' AND row_revision = ? "
                "AND row_revision < ? AND event_revision < ? RETURNING *",
                (
                    worker_id,
                    now + lease_seconds,
                    now,
                    now,
                    candidate["id"],
                    candidate["row_revision"],
                    MAX_REVISION,
                    MAX_REVISION,
                ),
            ).fetchone()
            if updated is None:
                self._refuse_max_revision(
                    connection,
                    table="library_identification_jobs",
                    predicate="id = ?",
                    parameters=(candidate["id"],),
                    include_event_revision=True,
                )
                return None
            connection.execute(
                "UPDATE library_work_control SET high_priority_claim_count = ? "
                "WHERE queue_kind = 'identification'",
                (streak + 1 if int(updated["priority"]) <= 20 else 0,),
            )
            self._bump_stream(connection, "identification")
            return dict(updated)

        return await self._write(operation)

    async def heartbeat_identification_job(
        self, job_id: str, worker_id: str, *, now: float, lease_seconds: float
    ) -> bool:
        def operation(connection: sqlite3.Connection) -> bool:
            cursor = connection.execute(
                "UPDATE library_identification_jobs SET heartbeat_at = ?, lease_expires_at = ?, "
                "updated_at = ?, row_revision = row_revision + 1 WHERE id = ? "
                "AND state = 'running' AND lease_owner = ? AND row_revision < ?",
                (now, now + lease_seconds, now, job_id, worker_id, MAX_REVISION),
            )
            if cursor.rowcount == 1:
                return True
            self._refuse_max_revision(
                connection,
                table="library_identification_jobs",
                predicate="id = ? AND state = 'running' AND lease_owner = ?",
                parameters=(job_id, worker_id),
            )
            return False

        return await self._write(operation)

    async def recover_expired_identification_leases(self, *, now: float) -> int:
        def operation(connection: sqlite3.Connection) -> int:
            overflow = connection.execute(
                "SELECT 1 FROM library_identification_jobs WHERE state = 'running' "
                "AND lease_expires_at < ? AND (row_revision >= ? OR event_revision >= ?) LIMIT 1",
                (now, MAX_REVISION, MAX_REVISION),
            ).fetchone()
            if overflow is not None:
                raise RevisionOverflowError(
                    "An identification job revision reached its maximum value."
                )
            cursor = connection.execute(
                "UPDATE library_identification_jobs SET state = 'queued', lease_owner = NULL, "
                "lease_expires_at = NULL, heartbeat_at = NULL, updated_at = ?, "
                "row_revision = row_revision + 1, event_revision = event_revision + 1 "
                "WHERE state = 'running' AND lease_expires_at < ? AND row_revision < ? "
                "AND event_revision < ?",
                (now, now, MAX_REVISION, MAX_REVISION),
            )
            if cursor.rowcount:
                self._bump_stream(connection, "identification")
            return cursor.rowcount

        return await self._write(operation)

    async def complete_identification_job(
        self,
        job_id: str,
        *,
        worker_id: str,
        expected_job_revision: int,
        attempt: IdentificationAttempt,
        evidence: list[IdentificationEvidenceRecord],
        terminal_state: str,
        completed_at: float,
    ) -> tuple[int, int]:
        def operation(connection: sqlite3.Connection) -> tuple[int, int]:
            connection.execute(
                "INSERT INTO library_identification_attempts "
                "(id, local_album_id, local_track_id, trigger, requested_by_user_id, "
                "input_tag_revision, input_policy_revision, input_file_revision, matcher_version, "
                "state, terminal_reason_code, selected_candidate_key, candidate_count, "
                "degradation_flags_json, started_at, completed_at) "
                "VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)",
                (
                    attempt.id,
                    attempt.local_album_id,
                    attempt.local_track_id,
                    attempt.trigger,
                    attempt.requested_by_user_id,
                    attempt.input_tag_revision,
                    attempt.input_policy_revision,
                    attempt.input_file_revision,
                    attempt.matcher_version,
                    attempt.state,
                    attempt.terminal_reason_code,
                    attempt.selected_candidate_key,
                    attempt.candidate_count,
                    json.dumps(attempt.degradation_flags, separators=(",", ":")),
                    attempt.started_at,
                    attempt.completed_at,
                ),
            )
            self._insert_evidence(connection, evidence)
            updated = connection.execute(
                "UPDATE library_identification_jobs SET state = ?, terminal_result_id = ?, "
                "terminal_at = ?, updated_at = ?, lease_owner = NULL, lease_expires_at = NULL, "
                "heartbeat_at = NULL, row_revision = row_revision + 1, "
                "event_revision = event_revision + 1 WHERE id = ? AND state = 'running' "
                "AND lease_owner = ? AND row_revision = ? AND row_revision < ? "
                "AND event_revision < ? RETURNING row_revision, event_revision",
                (
                    terminal_state,
                    attempt.id,
                    completed_at,
                    completed_at,
                    job_id,
                    worker_id,
                    expected_job_revision,
                    MAX_REVISION,
                    MAX_REVISION,
                ),
            ).fetchone()
            if updated is None:
                self._refuse_max_revision(
                    connection,
                    table="library_identification_jobs",
                    predicate="id = ?",
                    parameters=(job_id,),
                    include_event_revision=True,
                )
                raise StaleRevisionError(
                    "The identification job changed before completion was recorded."
                )
            stream_revision = self._bump_stream(connection, "identification")
            return int(updated["row_revision"]), stream_revision

        return await self._write(operation)

    async def get_album_identification_context(
        self, album_id: str
    ) -> dict[str, Any] | None:
        def operation(connection: sqlite3.Connection) -> dict[str, Any] | None:
            album = connection.execute(
                "SELECT * FROM local_albums WHERE id = ? AND retired_into_album_id IS NULL",
                (album_id,),
            ).fetchone()
            if album is None:
                return None
            tracks = connection.execute(
                "SELECT t.*, i.recording_mbid, i.release_mbid AS identity_release_mbid, "
                "i.decision_source AS track_identity_source, i.row_revision AS identity_row_revision "
                "FROM local_tracks t LEFT JOIN local_track_external_identities i "
                "ON i.local_track_id = t.id AND i.provider = 'musicbrainz' "
                "WHERE t.local_album_id = ? ORDER BY t.disc_number, t.track_number, t.id",
                (album_id,),
            ).fetchall()
            identity = connection.execute(
                "SELECT * FROM local_album_external_identities "
                "WHERE local_album_id = ? AND provider = 'musicbrainz'",
                (album_id,),
            ).fetchone()
            artist = connection.execute(
                "SELECT * FROM local_artists WHERE id = ?",
                (album["album_artist_id"],),
            ).fetchone()
            artist_identity = connection.execute(
                "SELECT * FROM local_artist_external_identities "
                "WHERE local_artist_id = ? AND provider = 'musicbrainz'",
                (album["album_artist_id"],),
            ).fetchone()
            review = connection.execute(
                "SELECT * FROM library_identification_reviews WHERE local_album_id = ? "
                "AND state != 'resolved' ORDER BY updated_at DESC, id DESC LIMIT 1",
                (album_id,),
            ).fetchone()
            return {
                "album": dict(album),
                "tracks": [dict(track) for track in tracks],
                "identity": _row(identity),
                "artist": _row(artist),
                "artist_identity": _row(artist_identity),
                "review": _row(review),
            }

        return await self._read(operation)

    async def get_attempt_evidence(
        self, attempt_id: str
    ) -> list[IdentificationEvidenceRecord]:
        def operation(
            connection: sqlite3.Connection,
        ) -> list[IdentificationEvidenceRecord]:
            rows = connection.execute(
                "SELECT * FROM library_identification_evidence WHERE attempt_id = ? "
                "ORDER BY candidate_key",
                (attempt_id,),
            ).fetchall()
            return [
                IdentificationEvidenceRecord(
                    id=str(row["id"]),
                    attempt_id=str(row["attempt_id"]),
                    candidate_key=str(row["candidate_key"]),
                    evidence=msgspec.json.decode(
                        bytes(row["evidence_json"]), type=CandidateEvidence
                    ),
                    created_at=float(row["created_at"]),
                )
                for row in rows
            ]

        return await self._read(operation)

    async def get_identification_attempt_input(
        self, attempt_id: str
    ) -> dict[str, str] | None:
        def operation(connection: sqlite3.Connection) -> dict[str, str] | None:
            row = connection.execute(
                "SELECT input_tag_revision, input_file_revision, input_policy_revision "
                "FROM library_identification_attempts WHERE id = ?",
                (attempt_id,),
            ).fetchone()
            return dict(row) if row is not None else None

        return await self._read(operation)

    async def get_selected_album_evidence(
        self, album_id: str
    ) -> IdentificationEvidenceRecord | None:
        def operation(
            connection: sqlite3.Connection,
        ) -> IdentificationEvidenceRecord | None:
            row = connection.execute(
                "SELECT e.* FROM local_album_external_identities i "
                "JOIN library_identification_evidence e ON e.attempt_id = i.attempt_id "
                "JOIN library_identification_attempts a ON a.id = e.attempt_id "
                "AND a.selected_candidate_key = e.candidate_key "
                "WHERE i.local_album_id = ? AND i.provider = 'musicbrainz'",
                (album_id,),
            ).fetchone()
            if row is None:
                return None
            return IdentificationEvidenceRecord(
                id=str(row["id"]),
                attempt_id=str(row["attempt_id"]),
                candidate_key=str(row["candidate_key"]),
                evidence=msgspec.json.decode(
                    bytes(row["evidence_json"]), type=CandidateEvidence
                ),
                created_at=float(row["created_at"]),
            )

        return await self._read(operation)

    async def get_latest_album_candidate_evidence(
        self, album_id: str, candidate_key: str
    ) -> IdentificationEvidenceRecord | None:
        def operation(
            connection: sqlite3.Connection,
        ) -> IdentificationEvidenceRecord | None:
            row = connection.execute(
                "SELECT e.* FROM library_identification_evidence e "
                "JOIN library_identification_attempts a ON a.id = e.attempt_id "
                "WHERE a.local_album_id = ? AND e.candidate_key = ? "
                "ORDER BY a.completed_at DESC, e.id DESC LIMIT 1",
                (album_id, candidate_key),
            ).fetchone()
            if row is None:
                return None
            return IdentificationEvidenceRecord(
                id=str(row["id"]),
                attempt_id=str(row["attempt_id"]),
                candidate_key=str(row["candidate_key"]),
                evidence=msgspec.json.decode(
                    bytes(row["evidence_json"]), type=CandidateEvidence
                ),
                created_at=float(row["created_at"]),
            )

        return await self._read(operation)

    async def finish_identification_job(
        self,
        job_id: str,
        *,
        worker_id: str,
        expected_job_revision: int,
        expected_album_revision: int,
        attempt: IdentificationAttempt,
        evidence: list[IdentificationEvidenceRecord],
        outcome: str,
        review_id: str,
        completed_at: float,
        decision_source: str = "automatic",
        selected_by_user_id: str | None = None,
    ) -> tuple[int, int, int]:
        """Commit evidence, review/identity, catalog, and job revisions atomically."""

        def operation(connection: sqlite3.Connection) -> tuple[int, int, int]:
            job = connection.execute(
                "SELECT * FROM library_identification_jobs WHERE id = ? AND state = 'running' "
                "AND lease_owner = ? AND row_revision = ?",
                (job_id, worker_id, expected_job_revision),
            ).fetchone()
            if job is None or str(job["local_album_id"] or "") != str(
                attempt.local_album_id or ""
            ):
                raise StaleRevisionError(
                    "The identification job changed before its result could be applied."
                )
            album = connection.execute(
                "SELECT row_revision, album_artist_id FROM local_albums WHERE id = ?",
                (attempt.local_album_id,),
            ).fetchone()
            if album is None:
                raise ResourceNotFoundError(
                    f"Album not found: {attempt.local_album_id}"
                )
            if int(album["row_revision"]) != expected_album_revision:
                raise StaleRevisionError(
                    "The album changed before its identification result could be applied."
                )
            connection.execute(
                "INSERT INTO library_identification_attempts "
                "(id, local_album_id, local_track_id, trigger, requested_by_user_id, "
                "input_tag_revision, input_policy_revision, input_file_revision, matcher_version, "
                "state, terminal_reason_code, selected_candidate_key, candidate_count, "
                "degradation_flags_json, started_at, completed_at) "
                "VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)",
                (
                    attempt.id,
                    attempt.local_album_id,
                    attempt.local_track_id,
                    attempt.trigger,
                    attempt.requested_by_user_id,
                    attempt.input_tag_revision,
                    attempt.input_policy_revision,
                    attempt.input_file_revision,
                    attempt.matcher_version,
                    attempt.state,
                    attempt.terminal_reason_code,
                    attempt.selected_candidate_key,
                    attempt.candidate_count,
                    json.dumps(attempt.degradation_flags, separators=(",", ":")),
                    attempt.started_at,
                    attempt.completed_at,
                ),
            )
            self._insert_evidence(connection, evidence)
            selected = next(
                (
                    record.evidence
                    for record in evidence
                    if record.candidate_key == attempt.selected_candidate_key
                ),
                None,
            )
            current_before = connection.execute(
                "SELECT decision_source FROM local_album_external_identities "
                "WHERE local_album_id = ? AND provider = 'musicbrainz'",
                (attempt.local_album_id,),
            ).fetchone()
            protected_identity = bool(
                current_before is not None
                and current_before["decision_source"] in ("manual", "legacy_import")
            )
            if (
                outcome == "identified"
                and selected is not None
                and not protected_identity
            ):
                connection.execute(
                    "UPDATE local_albums SET updated_at = ?, row_revision = row_revision + 1 "
                    "WHERE id = ? AND row_revision = ?",
                    (completed_at, attempt.local_album_id, expected_album_revision),
                )
                connection.execute(
                    "INSERT INTO local_album_external_identities "
                    "(local_album_id, provider, release_group_mbid, release_mbid, decision_source, "
                    "matcher_version, attempt_id, selected_by_user_id, selected_at) "
                    "VALUES (?, 'musicbrainz', ?, ?, ?, ?, ?, ?, ?) "
                    "ON CONFLICT(local_album_id, provider) DO UPDATE SET "
                    "release_group_mbid = excluded.release_group_mbid, "
                    "release_mbid = excluded.release_mbid, decision_source = excluded.decision_source, "
                    "matcher_version = excluded.matcher_version, attempt_id = excluded.attempt_id, "
                    "selected_by_user_id = excluded.selected_by_user_id, "
                    "selected_at = excluded.selected_at, row_revision = row_revision + 1",
                    (
                        attempt.local_album_id,
                        selected.release_group_mbid,
                        selected.release_mbid,
                        decision_source,
                        attempt.matcher_version,
                        attempt.id,
                        selected_by_user_id,
                        completed_at,
                    ),
                )
                connection.execute(
                    "DELETE FROM local_track_external_identities WHERE decision_source = 'automatic' "
                    "AND local_track_id IN (SELECT id FROM local_tracks WHERE local_album_id = ?)",
                    (attempt.local_album_id,),
                )
                for track in selected.track_evidence:
                    if track.classification != "supported" or not track.recording_mbid:
                        continue
                    connection.execute(
                        "INSERT INTO local_track_external_identities "
                        "(local_track_id, provider, recording_mbid, release_mbid, decision_source, "
                        "attempt_id, selected_at) VALUES (?, 'musicbrainz', ?, ?, ?, ?, ?) "
                        "ON CONFLICT(local_track_id, provider) DO UPDATE SET "
                        "recording_mbid = excluded.recording_mbid, release_mbid = excluded.release_mbid, "
                        "decision_source = excluded.decision_source, attempt_id = excluded.attempt_id, "
                        "selected_at = excluded.selected_at, row_revision = row_revision + 1",
                        (
                            track.local_track_id,
                            track.recording_mbid,
                            selected.release_mbid,
                            decision_source,
                            attempt.id,
                            completed_at,
                        ),
                    )
                if (
                    selected.artist_mbid
                    and selected.album_artist_classification == "supported"
                ):
                    artist_id = str(album["album_artist_id"])
                    owner = connection.execute(
                        "SELECT local_artist_id FROM local_artist_external_identities "
                        "WHERE provider = 'musicbrainz' AND provider_artist_id = ?",
                        (selected.artist_mbid,),
                    ).fetchone()
                    if owner is None or str(owner["local_artist_id"]) == artist_id:
                        connection.execute(
                            "INSERT INTO local_artist_external_identities "
                            "(local_artist_id, provider, provider_artist_id, decision_source, "
                            "attempt_id, selected_by_user_id, selected_at) "
                            "VALUES (?, 'musicbrainz', ?, ?, ?, ?, ?) "
                            "ON CONFLICT(local_artist_id, provider) DO UPDATE SET "
                            "provider_artist_id = excluded.provider_artist_id, "
                            "decision_source = excluded.decision_source, attempt_id = excluded.attempt_id, "
                            "selected_by_user_id = excluded.selected_by_user_id, "
                            "selected_at = excluded.selected_at, row_revision = row_revision + 1",
                            (
                                artist_id,
                                selected.artist_mbid,
                                decision_source,
                                attempt.id,
                                selected_by_user_id,
                                completed_at,
                            ),
                        )
                    else:
                        left, right = sorted((artist_id, str(owner["local_artist_id"])))
                        connection.execute(
                            "INSERT OR IGNORE INTO local_artist_merge_candidates "
                            "(id, left_artist_id, right_artist_id, reason_code, created_at, updated_at) "
                            "VALUES (?,?,?,?,?,?)",
                            (
                                f"provider:{left}:{right}",
                                left,
                                right,
                                "SHARED_PROVIDER_IDENTITY",
                                completed_at,
                                completed_at,
                            ),
                        )
                connection.execute(
                    "UPDATE library_identification_reviews SET state = 'resolved', "
                    "attempt_id = ?, updated_at = ?, row_revision = row_revision + 1 "
                    "WHERE local_album_id = ? AND state = 'needs_review'",
                    (attempt.id, completed_at, attempt.local_album_id),
                )
            elif outcome == "identified" and selected is not None:
                connection.execute(
                    "UPDATE library_identification_reviews SET state = 'resolved', "
                    "attempt_id = ?, updated_at = ?, row_revision = row_revision + 1 "
                    "WHERE local_album_id = ? AND state = 'needs_review'",
                    (attempt.id, completed_at, attempt.local_album_id),
                )
            else:
                current_identity = connection.execute(
                    "SELECT decision_source FROM local_album_external_identities "
                    "WHERE local_album_id = ? AND provider = 'musicbrainz'",
                    (attempt.local_album_id,),
                ).fetchone()
                if (
                    current_identity is not None
                    and current_identity["decision_source"] == "automatic"
                    and outcome == "contradictory"
                ):
                    connection.execute(
                        "DELETE FROM local_album_external_identities WHERE local_album_id = ? "
                        "AND provider = 'musicbrainz'",
                        (attempt.local_album_id,),
                    )
                    connection.execute(
                        "DELETE FROM local_track_external_identities "
                        "WHERE decision_source = 'automatic' AND local_track_id IN "
                        "(SELECT id FROM local_tracks WHERE local_album_id = ?)",
                        (attempt.local_album_id,),
                    )
                    connection.execute(
                        "UPDATE local_albums SET updated_at = ?, row_revision = row_revision + 1 "
                        "WHERE id = ? AND row_revision = ?",
                        (completed_at, attempt.local_album_id, expected_album_revision),
                    )
                active_review = connection.execute(
                    "SELECT id FROM library_identification_reviews WHERE local_album_id = ? "
                    "AND input_revision = ? AND state != 'resolved'",
                    (attempt.local_album_id, job["input_revision"]),
                ).fetchone()
                if active_review is None:
                    connection.execute(
                        "INSERT INTO library_identification_reviews "
                        "(id, local_album_id, state, reason_code, attempt_id, input_revision, "
                        "created_at, updated_at) VALUES (?, ?, 'needs_review', ?, ?, ?, ?, ?)",
                        (
                            review_id,
                            attempt.local_album_id,
                            attempt.terminal_reason_code,
                            attempt.id,
                            job["input_revision"],
                            completed_at,
                            completed_at,
                        ),
                    )
                else:
                    connection.execute(
                        "UPDATE library_identification_reviews SET reason_code = ?, attempt_id = ?, "
                        "updated_at = ?, row_revision = row_revision + 1 WHERE id = ?",
                        (
                            attempt.terminal_reason_code,
                            attempt.id,
                            completed_at,
                            active_review["id"],
                        ),
                    )
            terminal_state = "succeeded" if outcome == "identified" else "needs_review"
            updated = connection.execute(
                "UPDATE library_identification_jobs SET state = ?, terminal_result_id = ?, "
                "terminal_at = ?, updated_at = ?, lease_owner = NULL, lease_expires_at = NULL, "
                "heartbeat_at = NULL, checkpoint_json = NULL, row_revision = row_revision + 1, "
                "event_revision = event_revision + 1 WHERE id = ? AND state = 'running' "
                "AND lease_owner = ? AND row_revision = ? RETURNING row_revision, event_revision",
                (
                    terminal_state,
                    attempt.id,
                    completed_at,
                    completed_at,
                    job_id,
                    worker_id,
                    expected_job_revision,
                ),
            ).fetchone()
            if updated is None:
                raise StaleRevisionError(
                    "The identification job changed before completion was recorded."
                )
            catalog_revision = self._bump_catalog(connection)
            stream_revision = self._bump_stream(connection, "identification")
            return int(updated["row_revision"]), catalog_revision, stream_revision

        return await self._write(operation)

    async def defer_identification_job(
        self,
        job_id: str,
        *,
        worker_id: str,
        expected_job_revision: int,
        failure_code: str,
        not_before: float,
        now: float,
    ) -> int:
        def operation(connection: sqlite3.Connection) -> int:
            row = connection.execute(
                "UPDATE library_identification_jobs SET state = 'queued', not_before = ?, "
                "last_failure_code = ?, lease_owner = NULL, lease_expires_at = NULL, "
                "heartbeat_at = NULL, updated_at = ?, row_revision = row_revision + 1, "
                "event_revision = event_revision + 1 WHERE id = ? AND state = 'running' "
                "AND lease_owner = ? AND row_revision = ? RETURNING row_revision",
                (
                    not_before,
                    failure_code,
                    now,
                    job_id,
                    worker_id,
                    expected_job_revision,
                ),
            ).fetchone()
            if row is None:
                raise StaleRevisionError(
                    "The identification job changed before it could be deferred."
                )
            self._bump_stream(connection, "identification")
            return int(row["row_revision"])

        return await self._write(operation)

    async def pause_identification_queue(
        self,
        *,
        requested_by_user_id: str | None,
        requested_at: float,
        expected_revision: int | None = None,
    ) -> int:
        def operation(connection: sqlite3.Connection) -> int:
            revision_clause = (
                "" if expected_revision is None else "AND row_revision = ? "
            )
            parameters: tuple[Any, ...] = (
                requested_at,
                requested_by_user_id,
                MAX_REVISION,
                *(() if expected_revision is None else (expected_revision,)),
            )
            row = connection.execute(
                "UPDATE library_work_control SET state = 'paused', requested_at = ?, "
                "requested_by_user_id = ?, row_revision = row_revision + 1 "
                "WHERE queue_kind = 'identification' AND row_revision < ? "
                + revision_clause
                + "RETURNING row_revision",
                parameters,
            ).fetchone()
            if row is None:
                if expected_revision is not None:
                    raise StaleRevisionError(
                        "Identification controls changed before Pause was requested."
                    )
                raise RevisionOverflowError(
                    "The identification control revision reached its maximum value."
                )
            self._bump_stream(connection, "identification")
            return int(row["row_revision"])

        return await self._write(operation)

    async def checkpoint_identification_pause(
        self,
        job_id: str,
        *,
        worker_id: str,
        expected_job_revision: int,
        checkpoint: dict[str, Any],
        now: float,
    ) -> int:
        def operation(connection: sqlite3.Connection) -> int:
            control = connection.execute(
                "SELECT state FROM library_work_control WHERE queue_kind = 'identification'"
            ).fetchone()
            if control is None or control["state"] != "paused":
                raise StaleRevisionError("Identification is no longer paused.")
            row = connection.execute(
                "UPDATE library_identification_jobs SET state = 'queued', checkpoint_json = ?, "
                "lease_owner = NULL, lease_expires_at = NULL, heartbeat_at = NULL, "
                "attempt_count = CASE WHEN attempt_count > 0 THEN attempt_count - 1 ELSE 0 END, "
                "updated_at = ?, row_revision = row_revision + 1, event_revision = event_revision + 1 "
                "WHERE id = ? AND state = 'running' AND lease_owner = ? AND row_revision = ? "
                "RETURNING row_revision",
                (
                    json.dumps(checkpoint, separators=(",", ":"), sort_keys=True),
                    now,
                    job_id,
                    worker_id,
                    expected_job_revision,
                ),
            ).fetchone()
            if row is None:
                raise StaleRevisionError(
                    "The identification job changed before its pause checkpoint was saved."
                )
            self._bump_stream(connection, "identification")
            return int(row["row_revision"])

        return await self._write(operation)

    async def resume_identification_queue(
        self, *, resumed_at: float, expected_revision: int | None = None
    ) -> int:
        def operation(connection: sqlite3.Connection) -> int:
            revision_clause = (
                "" if expected_revision is None else "AND row_revision = ? "
            )
            parameters: tuple[Any, ...] = (
                MAX_REVISION,
                *(() if expected_revision is None else (expected_revision,)),
            )
            row = connection.execute(
                "UPDATE library_work_control SET state = 'running', requested_at = NULL, "
                "requested_by_user_id = NULL, high_priority_claim_count = 0, "
                "row_revision = row_revision + 1 WHERE queue_kind = 'identification' "
                "AND row_revision < ? " + revision_clause + "RETURNING row_revision",
                parameters,
            ).fetchone()
            if row is None:
                if expected_revision is not None:
                    raise StaleRevisionError(
                        "Identification controls changed before Resume was requested."
                    )
                raise RevisionOverflowError(
                    "The identification control revision reached its maximum value."
                )
            connection.execute(
                "UPDATE library_identification_jobs SET updated_at = ? WHERE state = 'queued'",
                (resumed_at,),
            )
            self._bump_stream(connection, "identification")
            return int(row["row_revision"])

        return await self._write(operation)

    async def get_identification_control(self) -> dict[str, Any]:
        def operation(connection: sqlite3.Connection) -> dict[str, Any]:
            return dict(
                connection.execute(
                    "SELECT * FROM library_work_control WHERE queue_kind = 'identification'"
                ).fetchone()
            )

        return await self._read(operation)

    async def get_identification_activity_snapshot(self) -> dict[str, Any]:
        """Return redacted aggregate queue state for activity and admin progress UI."""

        def operation(connection: sqlite3.Connection) -> dict[str, Any]:
            control = dict(
                connection.execute(
                    "SELECT state, row_revision FROM library_work_control "
                    "WHERE queue_kind = 'identification'"
                ).fetchone()
            )
            counts = {
                str(row["state"]): int(row["count"])
                for row in connection.execute(
                    "SELECT state, COUNT(*) AS count FROM library_identification_jobs "
                    "GROUP BY state"
                ).fetchall()
            }
            active = connection.execute(
                "SELECT MIN(created_at) AS started_at, MAX(updated_at) AS updated_at, "
                "SUM(CASE WHEN last_failure_code IS NOT NULL THEN 1 ELSE 0 END) AS deferred_count "
                "FROM library_identification_jobs WHERE state IN ('queued','running','paused')"
            ).fetchone()
            active_priority = connection.execute(
                "SELECT priority FROM library_identification_jobs "
                "WHERE state = 'running' ORDER BY priority, enqueue_sequence LIMIT 1"
            ).fetchone()
            if active_priority is None:
                active_priority = connection.execute(
                    "SELECT priority FROM library_identification_jobs "
                    "WHERE state IN ('queued','paused') "
                    "ORDER BY priority, enqueue_sequence LIMIT 1"
                ).fetchone()
            kept_local_count = int(
                connection.execute(
                    "SELECT COUNT(*) FROM library_identification_reviews "
                    "WHERE state = 'keep_tagged'"
                ).fetchone()[0]
            )
            failure = connection.execute(
                "SELECT id, terminal_at FROM library_identification_jobs "
                "WHERE state = 'failed' ORDER BY terminal_at DESC, id DESC LIMIT 1"
            ).fetchone()
            foreground_operation_count = int(
                connection.execute(
                    "SELECT COUNT(*) FROM library_operation_jobs "
                    "WHERE state IN ('queued','running','paused')"
                ).fetchone()[0]
            )
            return {
                "control_state": str(control["state"]),
                "control_revision": int(control["row_revision"]),
                "counts": counts,
                "started_at": active["started_at"],
                "updated_at": active["updated_at"],
                "deferred_count": int(active["deferred_count"] or 0),
                "kept_local_count": kept_local_count,
                "active_priority": (
                    int(active_priority["priority"])
                    if active_priority is not None
                    else None
                ),
                "failure_event_id": str(failure["id"]) if failure else None,
                "failure_at": float(failure["terminal_at"]) if failure else None,
                "foreground_operation_count": foreground_operation_count,
            }

        return await self._read(operation)

    async def get_fingerprint_outcome(
        self, local_track_id: str, stat_revision: str, fingerprinter_version: str
    ) -> FingerprintOutcome | None:
        def operation(connection: sqlite3.Connection) -> FingerprintOutcome | None:
            row = connection.execute(
                "SELECT * FROM audio_fingerprint_outcomes WHERE local_track_id = ? "
                "AND stat_revision = ? AND fingerprinter_version = ?",
                (local_track_id, stat_revision, fingerprinter_version),
            ).fetchone()
            if row is None:
                return None
            values = dict(row)
            values["release_group_ids"] = json.loads(
                values.pop("release_group_ids_json")
            )
            return FingerprintOutcome(**values)

        return await self._read(operation)

    async def record_fingerprint_outcome(self, outcome: FingerprintOutcome) -> int:
        def operation(connection: sqlite3.Connection) -> int:
            existing = connection.execute(
                "SELECT state, row_revision FROM audio_fingerprint_outcomes "
                "WHERE local_track_id = ? AND stat_revision = ? AND fingerprinter_version = ?",
                (
                    outcome.local_track_id,
                    outcome.stat_revision,
                    outcome.fingerprinter_version,
                ),
            ).fetchone()
            if existing is not None and existing["state"] in (
                "matched",
                "no_match",
                "disabled",
                "skipped",
            ):
                return int(existing["row_revision"])
            connection.execute(
                "INSERT INTO audio_fingerprint_outcomes "
                "(id, local_track_id, stat_revision, fingerprinter_version, state, fingerprint, "
                "duration_seconds, recording_mbid, release_group_ids_json, score, failure_code, attempt_count, "
                "first_attempt_at, last_attempt_at, retry_after, row_revision) "
                "VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?) "
                "ON CONFLICT(local_track_id, stat_revision, fingerprinter_version) DO UPDATE SET "
                "state = excluded.state, fingerprint = COALESCE(excluded.fingerprint, audio_fingerprint_outcomes.fingerprint), "
                "duration_seconds = COALESCE(excluded.duration_seconds, audio_fingerprint_outcomes.duration_seconds), "
                "recording_mbid = excluded.recording_mbid, "
                "release_group_ids_json = excluded.release_group_ids_json, score = excluded.score, "
                "failure_code = excluded.failure_code, attempt_count = audio_fingerprint_outcomes.attempt_count + 1, "
                "last_attempt_at = excluded.last_attempt_at, retry_after = excluded.retry_after, "
                "row_revision = audio_fingerprint_outcomes.row_revision + 1",
                (
                    outcome.id,
                    outcome.local_track_id,
                    outcome.stat_revision,
                    outcome.fingerprinter_version,
                    outcome.state,
                    outcome.fingerprint,
                    outcome.duration_seconds,
                    outcome.recording_mbid,
                    json.dumps(outcome.release_group_ids, separators=(",", ":")),
                    outcome.score,
                    outcome.failure_code,
                    outcome.attempt_count,
                    outcome.first_attempt_at,
                    outcome.last_attempt_at,
                    outcome.retry_after,
                    outcome.row_revision,
                ),
            )
            row = connection.execute(
                "SELECT row_revision FROM audio_fingerprint_outcomes "
                "WHERE local_track_id = ? AND stat_revision = ? AND fingerprinter_version = ?",
                (
                    outcome.local_track_id,
                    outcome.stat_revision,
                    outcome.fingerprinter_version,
                ),
            ).fetchone()
            return int(row["row_revision"])

        return await self._write(operation)

    async def compact_terminal_identification_evidence(
        self, *, older_than: float
    ) -> tuple[int, int]:
        def operation(connection: sqlite3.Connection) -> tuple[int, int]:
            rows = connection.execute(
                "SELECT e.*, a.terminal_reason_code, a.matcher_version FROM "
                "library_identification_evidence e JOIN library_identification_attempts a "
                "ON a.id = e.attempt_id WHERE a.completed_at < ? AND e.compacted = 0 "
                "AND a.state != 'identified' "
                "AND NOT EXISTS (SELECT 1 FROM local_album_external_identities i WHERE i.attempt_id = a.id) "
                "AND NOT EXISTS (SELECT 1 FROM local_track_external_identities i WHERE i.attempt_id = a.id) "
                "AND NOT EXISTS (SELECT 1 FROM local_artist_external_identities i WHERE i.attempt_id = a.id) "
                "AND NOT EXISTS (SELECT 1 FROM library_identification_reviews r WHERE r.attempt_id = a.id AND r.state != 'resolved') "
                "AND NOT EXISTS (SELECT 1 FROM library_identity_repair_findings f WHERE f.evidence_id = e.id)",
                (older_than,),
            ).fetchall()
            total_bytes = 0
            for row in rows:
                prior = msgspec.json.decode(
                    bytes(row["evidence_json"]), type=CandidateEvidence
                )
                summary = CandidateEvidence(
                    release_group_mbid=prior.release_group_mbid,
                    release_mbid=prior.release_mbid,
                    album_title=prior.album_title,
                    album_artist_name=prior.album_artist_name,
                    release_type=prior.release_type,
                    release_date=prior.release_date,
                    score=prior.score,
                    margin=prior.margin,
                    reason_code=str(row["terminal_reason_code"]),
                    matcher_version=str(row["matcher_version"]),
                )
                encoded = msgspec.json.encode(summary)
                if len(encoded) > 4096:
                    raise ValueError("Compacted identification evidence exceeds 4 KiB.")
                connection.execute(
                    "DELETE FROM library_identification_evidence WHERE id = ?",
                    (row["id"],),
                )
                connection.execute(
                    "INSERT INTO library_identification_evidence "
                    "(id, attempt_id, candidate_key, evidence_json, evidence_size_bytes, compacted, created_at) "
                    "VALUES (?,?,?,?,?,1,?)",
                    (
                        row["id"],
                        row["attempt_id"],
                        row["candidate_key"],
                        encoded,
                        len(encoded),
                        row["created_at"],
                    ),
                )
                total_bytes += len(encoded)
            return len(rows), total_bytes

        return await self._write(operation)

    async def identification_storage_report(self) -> dict[str, int]:
        def operation(connection: sqlite3.Connection) -> dict[str, int]:
            page_size = int(connection.execute("PRAGMA page_size").fetchone()[0])
            page_count = int(connection.execute("PRAGMA page_count").fetchone()[0])
            evidence = connection.execute(
                "SELECT COUNT(*) AS count, COALESCE(SUM(evidence_size_bytes), 0) AS bytes "
                "FROM library_identification_evidence"
            ).fetchone()
            return {
                "database_bytes": page_size * page_count,
                "evidence_rows": int(evidence["count"]),
                "evidence_payload_bytes": int(evidence["bytes"]),
            }

        return await self._read(operation)

    async def diagnostic_snapshot(
        self, run_id: str, *, row_limit: int
    ) -> dict[str, Any]:
        def operation(connection: sqlite3.Connection) -> dict[str, Any]:
            run = connection.execute(
                "SELECT * FROM library_scan_runs WHERE id = ?", (run_id,)
            ).fetchone()
            if run is None:
                raise ResourceNotFoundError("Scan run not found.")
            scopes = connection.execute(
                "SELECT root_id, relative_path, effective_policy, policy_revision, "
                "estimated_count, discovered_count, discovery_state, reconciliation_state, "
                "phase_timings_json, error_code "
                "FROM library_scan_run_scopes WHERE run_id = ? ORDER BY scope_sequence LIMIT ?",
                (run_id, row_limit + 1),
            ).fetchall()
            exported_scopes = scopes[:row_limit]
            inventory_limit = max(0, row_limit - len(exported_scopes))
            inventory = connection.execute(
                "SELECT root_id, relative_path, comparison_result, processing_state, failure_code, "
                "file_size_bytes FROM library_scan_inventory WHERE run_id = ? "
                "ORDER BY root_id, relative_path LIMIT ?",
                (run_id, inventory_limit + 1),
            ).fetchall()
            exported_inventory = inventory[:inventory_limit]
            evidence = connection.execute(
                "SELECT COUNT(*) rows, COALESCE(SUM(evidence_size_bytes),0) bytes, "
                "COALESCE(SUM(compacted),0) compacted_rows, MIN(created_at) oldest_at, "
                "MAX(created_at) newest_at FROM library_identification_evidence"
            ).fetchone()
            protected = connection.execute(
                "SELECT COUNT(DISTINCT e.id) FROM library_identification_evidence e WHERE "
                "EXISTS (SELECT 1 FROM local_album_external_identities i WHERE i.attempt_id = e.attempt_id) "
                "OR EXISTS (SELECT 1 FROM library_identification_reviews r WHERE r.attempt_id = e.attempt_id AND r.state != 'resolved') "
                "OR EXISTS (SELECT 1 FROM library_identity_repair_findings f WHERE f.evidence_id = e.id)"
            ).fetchone()[0]
            compactable = connection.execute(
                "SELECT COUNT(*) rows, MIN(a.completed_at) oldest_eligible_at "
                "FROM library_identification_evidence e "
                "JOIN library_identification_attempts a ON a.id = e.attempt_id "
                "WHERE e.compacted = 0 AND a.completed_at < ? AND a.state IN "
                "('no_candidate','ambiguous','insufficient_evidence','failed') "
                "AND NOT EXISTS (SELECT 1 FROM local_album_external_identities i WHERE i.attempt_id = e.attempt_id) "
                "AND NOT EXISTS (SELECT 1 FROM library_identification_reviews r WHERE r.attempt_id = e.attempt_id AND r.state != 'resolved') "
                "AND NOT EXISTS (SELECT 1 FROM library_identity_repair_findings f WHERE f.evidence_id = e.id)",
                (float(run["updated_at"]) - 90 * 86400,),
            ).fetchone()
            categories = connection.execute(
                "SELECT a.state category, COUNT(*) rows, "
                "COALESCE(SUM(e.evidence_size_bytes),0) bytes "
                "FROM library_identification_evidence e "
                "JOIN library_identification_attempts a ON a.id = e.attempt_id "
                "GROUP BY a.state ORDER BY a.state"
            ).fetchall()
            return {
                "run": dict(run),
                "scopes": [dict(row) for row in exported_scopes],
                "scopes_truncated": len(scopes) > row_limit,
                "inventory": [dict(row) for row in exported_inventory],
                "inventory_truncated": len(inventory) > inventory_limit,
                "exported_row_count": len(exported_scopes) + len(exported_inventory),
                "evidence": {
                    **dict(evidence),
                    "protected_rows": int(protected),
                    "compactable_terminal_misses": int(compactable["rows"]),
                    "oldest_cleanup_eligible_at": compactable["oldest_eligible_at"],
                    "by_attempt_state": [dict(row) for row in categories],
                },
            }

        return await self._read(operation)

    async def create_scan_run(self, run: ScanRun) -> None:
        def operation(connection: sqlite3.Connection) -> None:
            connection.execute(
                "INSERT INTO library_scan_runs "
                "(id, kind, trigger, requested_by_user_id, state, phase, aggregate_scope, "
                "queued_at, updated_at, row_revision, event_revision) VALUES (?,?,?,?,?,?,?,?,?,?,?)",
                (
                    run.id,
                    run.kind,
                    run.trigger,
                    run.requested_by_user_id,
                    run.state,
                    run.phase,
                    run.aggregate_scope,
                    run.queued_at,
                    run.queued_at,
                    run.row_revision,
                    run.event_revision,
                ),
            )

        await self._write(operation)

    @staticmethod
    def _scan_scope_covers(existing: sqlite3.Row, requested: ScanScope) -> bool:
        if str(existing["root_id"]) != requested.root_id:
            return False
        parent = str(existing["relative_path"])
        child = requested.relative_path
        return parent == "." or parent == child or child.startswith(f"{parent}/")

    @staticmethod
    def _scan_state_from_row(row: sqlite3.Row) -> ScanRun:
        counter_names = (
            "total_count",
            "discovered_count",
            "inspected_count",
            "new_count",
            "changed_count",
            "indexed_count",
            "unchanged_count",
            "excluded_count",
            "missing_count",
            "errored_count",
            "identification_enqueued_count",
        )
        phase_timings = json.loads(str(row["phase_timings_json"] or "{}"))
        return ScanRun(
            id=str(row["id"]),
            kind=row["kind"],
            trigger=row["trigger"],
            state=row["state"],
            phase=row["phase"],
            requested_by_user_id=row["requested_by_user_id"],
            aggregate_scope=str(row["aggregate_scope"]),
            queued_at=float(row["queued_at"]),
            started_at=row["started_at"],
            updated_at=float(row["updated_at"]),
            terminal_at=row["terminal_at"],
            resume_phase=row["resume_phase"],
            requested_control=row["requested_control"],
            terminal_code=row["terminal_code"],
            coalesced_request_count=int(row["coalesced_request_count"]),
            row_revision=int(row["row_revision"]),
            event_revision=int(row["event_revision"]),
            counters={name: int(row[name]) for name in counter_names},
            phase_timings={
                str(name): float(seconds)
                for name, seconds in phase_timings.items()
                if isinstance(seconds, (int, float))
            },
        )

    @staticmethod
    def _insert_scan_trigger(
        connection: sqlite3.Connection,
        run_id: str,
        request: ScanRequest,
        *,
        reason: str,
        requested_at: float,
    ) -> None:
        next_sequence = connection.execute(
            "SELECT COALESCE(MAX(trigger_sequence), -1) + 1 FROM library_scan_run_triggers "
            "WHERE run_id = ?",
            (run_id,),
        ).fetchone()[0]
        connection.execute(
            "INSERT INTO library_scan_run_triggers "
            "(run_id, trigger_sequence, trigger, requested_by_user_id, reason, requested_at) "
            "VALUES (?,?,?,?,?,?)",
            (
                run_id,
                next_sequence,
                request.trigger,
                request.requested_by_user_id,
                reason,
                requested_at,
            ),
        )

    @staticmethod
    def _insert_scan_scopes(
        connection: sqlite3.Connection, run_id: str, scopes: list[ScanScope]
    ) -> None:
        next_sequence = connection.execute(
            "SELECT COALESCE(MAX(scope_sequence), -1) + 1 FROM library_scan_run_scopes "
            "WHERE run_id = ?",
            (run_id,),
        ).fetchone()[0]
        for offset, scope in enumerate(scopes):
            connection.execute(
                "INSERT OR IGNORE INTO library_scan_run_scopes "
                "(run_id, scope_sequence, root_id, scope_id, relative_path, root_path, "
                "effective_policy, policy_revision, estimated_count) VALUES (?,?,?,?,?,?,?,?,?)",
                (
                    run_id,
                    int(next_sequence) + offset,
                    scope.root_id,
                    scope.scope_id,
                    scope.relative_path,
                    scope.root_path,
                    scope.effective_policy,
                    scope.policy_revision,
                    scope.estimated_count,
                ),
            )

    async def request_scan_run(
        self,
        request: ScanRequest,
        *,
        run_id: str,
        requested_at: float,
    ) -> ScanRequestResult:
        """Atomically create, coalesce, expand, or reject target scan work."""

        def operation(connection: sqlite3.Connection) -> ScanRequestResult:
            rows = connection.execute(
                "SELECT * FROM library_scan_runs WHERE state IN "
                "('discovering','indexing','reconciling','pausing','paused','stopping','queued') "
                "ORDER BY CASE WHEN state = 'queued' THEN 1 ELSE 0 END, queued_at, id"
            ).fetchall()
            active = next((row for row in rows if row["state"] != "queued"), None)
            queued = next((row for row in rows if row["state"] == "queued"), None)

            def covers(row: sqlite3.Row) -> bool:
                if row["kind"] != request.kind:
                    return False
                existing = connection.execute(
                    "SELECT root_id, relative_path, policy_revision "
                    "FROM library_scan_run_scopes WHERE run_id = ?",
                    (row["id"],),
                ).fetchall()
                return bool(existing) and all(
                    any(
                        scope["policy_revision"] == request.policy_revision
                        and self._scan_scope_covers(scope, requested)
                        for scope in existing
                    )
                    for requested in request.scopes
                )

            covering = next((row for row in rows if covers(row)), None)
            if covering is not None:
                self._refuse_max_revision(
                    connection,
                    table="library_scan_runs",
                    predicate="id = ?",
                    parameters=(covering["id"],),
                    include_event_revision=True,
                )
                updated = connection.execute(
                    "UPDATE library_scan_runs SET coalesced_request_count = "
                    "coalesced_request_count + 1, updated_at = ?, row_revision = row_revision + 1, "
                    "event_revision = event_revision + 1 WHERE id = ? RETURNING *",
                    (requested_at, covering["id"]),
                ).fetchone()
                self._insert_scan_trigger(
                    connection,
                    str(covering["id"]),
                    request,
                    reason="covered",
                    requested_at=requested_at,
                )
                self._bump_stream(connection, "scan")
                return ScanRequestResult(
                    run_id=str(updated["id"]),
                    disposition="coalesced",
                    state=updated["state"],
                    row_revision=int(updated["row_revision"]),
                )

            if active is not None and queued is not None:
                if (
                    queued["kind"] != request.kind
                    or connection.execute(
                        "SELECT 1 FROM library_scan_run_scopes WHERE run_id = ? "
                        "AND policy_revision != ? LIMIT 1",
                        (queued["id"], request.policy_revision),
                    ).fetchone()
                    is not None
                ):
                    return ScanRequestResult(
                        run_id=str(queued["id"]),
                        disposition="conflict",
                        state="queued",
                        row_revision=int(queued["row_revision"]),
                        queued_reason="The follow-up slot already contains incompatible work.",
                        conflicting_kind=queued["kind"],
                    )
                existing = connection.execute(
                    "SELECT root_id, relative_path FROM library_scan_run_scopes WHERE run_id = ?",
                    (queued["id"],),
                ).fetchall()
                additions = [
                    scope
                    for scope in request.scopes
                    if not any(self._scan_scope_covers(row, scope) for row in existing)
                ]
                self._insert_scan_scopes(connection, str(queued["id"]), additions)
                updated = connection.execute(
                    "UPDATE library_scan_runs SET aggregate_scope = ?, updated_at = ?, "
                    "row_revision = row_revision + 1, event_revision = event_revision + 1 "
                    "WHERE id = ? AND row_revision < ? AND event_revision < ? RETURNING *",
                    (
                        "all"
                        if any(scope.relative_path == "." for scope in request.scopes)
                        else "selected",
                        requested_at,
                        queued["id"],
                        MAX_REVISION,
                        MAX_REVISION,
                    ),
                ).fetchone()
                if updated is None:
                    raise RevisionOverflowError(
                        "A scan run revision reached its maximum value."
                    )
                self._insert_scan_trigger(
                    connection,
                    str(queued["id"]),
                    request,
                    reason="scope_expanded",
                    requested_at=requested_at,
                )
                self._bump_stream(connection, "scan")
                return ScanRequestResult(
                    run_id=str(updated["id"]),
                    disposition="expanded",
                    state="queued",
                    row_revision=int(updated["row_revision"]),
                )

            connection.execute(
                "INSERT INTO library_scan_runs "
                "(id, kind, trigger, requested_by_user_id, state, phase, aggregate_scope, "
                "queued_at, updated_at) VALUES (?,?,?,?,?,?,?,?,?)",
                (
                    run_id,
                    request.kind,
                    request.trigger,
                    request.requested_by_user_id,
                    "queued",
                    "queued",
                    "all"
                    if any(scope.relative_path == "." for scope in request.scopes)
                    else "selected",
                    requested_at,
                    requested_at,
                ),
            )
            self._insert_scan_scopes(connection, run_id, request.scopes)
            self._insert_scan_trigger(
                connection,
                run_id,
                request,
                reason="accepted",
                requested_at=requested_at,
            )
            self._bump_stream(connection, "scan")
            return ScanRequestResult(
                run_id=run_id,
                disposition="started" if active is None else "queued",
                state="queued",
                row_revision=1,
                queued_reason=None if active is None else "Another scan is active.",
            )

        return await self._write(operation)

    async def get_scan_run(
        self, run_id: str
    ) -> tuple[ScanRun, list[ScanScope], dict[str, int]]:
        def operation(
            connection: sqlite3.Connection,
        ) -> tuple[ScanRun, list[ScanScope], dict[str, int]]:
            row = connection.execute(
                "SELECT * FROM library_scan_runs WHERE id = ?", (run_id,)
            ).fetchone()
            if row is None:
                raise ResourceNotFoundError(f"Scan run not found: {run_id}")
            scopes = [
                ScanScope(
                    root_id=str(scope["root_id"]),
                    scope_id=scope["scope_id"],
                    relative_path=str(scope["relative_path"]),
                    root_path=scope["root_path"],
                    effective_policy=scope["effective_policy"],
                    policy_revision=str(scope["policy_revision"]),
                    estimated_count=scope["estimated_count"],
                )
                for scope in connection.execute(
                    "SELECT * FROM library_scan_run_scopes WHERE run_id = ? "
                    "ORDER BY scope_sequence",
                    (run_id,),
                ).fetchall()
            ]
            counter_names = (
                "total_count",
                "discovered_count",
                "inspected_count",
                "new_count",
                "changed_count",
                "indexed_count",
                "unchanged_count",
                "excluded_count",
                "missing_count",
                "errored_count",
                "identification_enqueued_count",
            )
            return (
                self._scan_state_from_row(row),
                scopes,
                {name: int(row[name]) for name in counter_names},
            )

        return await self._read(operation)

    async def list_current_scan_runs(self) -> list[ScanRun]:
        def operation(connection: sqlite3.Connection) -> list[ScanRun]:
            rows = connection.execute(
                "SELECT * FROM library_scan_runs WHERE state IN "
                "('queued','discovering','indexing','reconciling','pausing','paused','stopping') "
                "ORDER BY CASE WHEN state = 'queued' THEN 1 ELSE 0 END, queued_at"
            ).fetchall()
            return [self._scan_state_from_row(row) for row in rows]

        return await self._read(operation)

    async def list_scan_history(
        self,
        *,
        limit: int = 50,
        before_terminal_at: float | None = None,
        before_id: str | None = None,
    ) -> list[ScanRun]:
        def operation(connection: sqlite3.Connection) -> list[ScanRun]:
            bounded = min(max(limit, 1), 51)
            if before_terminal_at is None or before_id is None:
                rows = connection.execute(
                    "SELECT * FROM library_scan_runs WHERE terminal_at IS NOT NULL "
                    "ORDER BY terminal_at DESC, id DESC LIMIT ?",
                    (bounded,),
                ).fetchall()
            else:
                rows = connection.execute(
                    "SELECT * FROM library_scan_runs WHERE terminal_at IS NOT NULL AND "
                    "(terminal_at < ? OR (terminal_at = ? AND id < ?)) "
                    "ORDER BY terminal_at DESC, id DESC LIMIT ?",
                    (before_terminal_at, before_terminal_at, before_id, bounded),
                ).fetchall()
            return [self._scan_state_from_row(row) for row in rows]

        return await self._read(operation)

    async def get_latest_filesystem_scan_terminal(self) -> ScanRun | None:
        def operation(connection: sqlite3.Connection) -> ScanRun | None:
            row = connection.execute(
                "SELECT * FROM library_scan_runs WHERE terminal_at IS NOT NULL "
                "AND (kind != 'policy_reconcile' OR aggregate_scope = 'all') "
                "ORDER BY terminal_at DESC, id DESC LIMIT 1"
            ).fetchone()
            return self._scan_state_from_row(row) if row is not None else None

        return await self._read(operation)

    async def claim_next_scan_run(self, *, now: float) -> ScanRun | None:
        def operation(connection: sqlite3.Connection) -> ScanRun | None:
            if (
                connection.execute(
                    "SELECT 1 FROM library_scan_runs WHERE state IN "
                    "('discovering','indexing','reconciling','pausing','paused','stopping') LIMIT 1"
                ).fetchone()
                is not None
            ):
                return None
            candidate = connection.execute(
                "SELECT * FROM library_scan_runs WHERE state = 'queued' "
                "ORDER BY queued_at, id LIMIT 1"
            ).fetchone()
            if candidate is None:
                return None
            updated = connection.execute(
                "UPDATE library_scan_runs SET state = 'discovering', phase = 'discovering', "
                "started_at = COALESCE(started_at, ?), updated_at = ?, heartbeat_at = ?, "
                "phase_started_at = COALESCE(phase_started_at, ?), "
                "row_revision = row_revision + 1, event_revision = event_revision + 1 "
                "WHERE id = ? AND state = 'queued' AND row_revision = ? AND row_revision < ? "
                "AND event_revision < ? RETURNING *",
                (
                    now,
                    now,
                    now,
                    now,
                    candidate["id"],
                    candidate["row_revision"],
                    MAX_REVISION,
                    MAX_REVISION,
                ),
            ).fetchone()
            if updated is None:
                self._refuse_max_revision(
                    connection,
                    table="library_scan_runs",
                    predicate="id = ?",
                    parameters=(candidate["id"],),
                    include_event_revision=True,
                )
                return None
            self._bump_stream(connection, "scan")
            return self._scan_state_from_row(updated)

        return await super()._background_write(operation)

    async def get_resumable_scan_run(self) -> ScanRun | None:
        def operation(connection: sqlite3.Connection) -> ScanRun | None:
            row = connection.execute(
                "SELECT * FROM library_scan_runs WHERE state IN "
                "('discovering','indexing','reconciling') "
                "AND requested_control = 'none' ORDER BY started_at, id LIMIT 1"
            ).fetchone()
            return self._scan_state_from_row(row) if row is not None else None

        return await self._read(operation)

    async def prepare_scan_discovery_resume(self, run_id: str) -> None:
        """Start a new generation for each partially discovered scope."""

        def operation(connection: sqlite3.Connection) -> None:
            incomplete = connection.execute(
                "SELECT root_id, relative_path FROM library_scan_run_scopes "
                "WHERE run_id = ? AND discovery_state != 'completed'",
                (run_id,),
            ).fetchall()
            for scope in incomplete:
                connection.execute(
                    "UPDATE library_scan_run_scopes SET discovery_state = 'pending', "
                    "discovery_generation = discovery_generation + 1, "
                    "discovered_count = 0, error_code = NULL, row_revision = row_revision + 1 "
                    "WHERE run_id = ? AND root_id = ? AND relative_path = ?",
                    (run_id, scope["root_id"], scope["relative_path"]),
                )
            discovered = connection.execute(
                "SELECT COUNT(*) FROM library_scan_inventory inventory "
                "JOIN library_scan_run_scopes scope ON scope.run_id = inventory.run_id "
                "AND scope.root_id = inventory.root_id "
                "AND scope.relative_path = inventory.scope_relative_path "
                "AND scope.discovery_generation = inventory.discovery_generation "
                "WHERE inventory.run_id = ?",
                (run_id,),
            ).fetchone()[0]
            connection.execute(
                "UPDATE library_scan_runs SET discovered_count = ?, updated_at = updated_at, "
                "row_revision = row_revision + 1 WHERE id = ?",
                (discovered, run_id),
            )

        await super()._background_write(operation)

    async def finalize_scan_discovery(
        self, run_id: str, *, updated_at: float
    ) -> ScanRun:
        def operation(connection: sqlite3.Connection) -> ScanRun:
            total = int(
                connection.execute(
                    "SELECT COUNT(*) FROM library_scan_inventory inventory "
                    "JOIN library_scan_run_scopes scope ON scope.run_id = inventory.run_id "
                    "AND scope.root_id = inventory.root_id "
                    "AND scope.relative_path = inventory.scope_relative_path "
                    "AND scope.discovery_generation = inventory.discovery_generation "
                    "WHERE inventory.run_id = ?",
                    (run_id,),
                ).fetchone()[0]
            )
            updated = connection.execute(
                "UPDATE library_scan_runs SET total_count = ?, discovered_count = ?, "
                "updated_at = ?, heartbeat_at = ?, row_revision = row_revision + 1, "
                "event_revision = event_revision + 1 WHERE id = ? RETURNING *",
                (total, total, updated_at, updated_at, run_id),
            ).fetchone()
            if updated is None:
                raise ResourceNotFoundError("Scan run not found.")
            self._bump_stream(connection, "scan")
            return self._scan_state_from_row(updated)

        return await super()._background_write(operation)

    async def cleanup_stale_scan_inventory(
        self, run_id: str, *, limit: int = 5_000
    ) -> int:
        """Delete one bounded page left behind by older discovery generations."""

        def operation(connection: sqlite3.Connection) -> int:
            return int(
                connection.execute(
                    "DELETE FROM library_scan_inventory WHERE rowid IN ("
                    "SELECT inventory.rowid FROM library_scan_inventory inventory "
                    "LEFT JOIN library_scan_run_scopes scope "
                    "ON scope.run_id=inventory.run_id "
                    "AND scope.root_id=inventory.root_id "
                    "AND scope.relative_path=inventory.scope_relative_path "
                    "AND scope.discovery_generation=inventory.discovery_generation "
                    "WHERE inventory.run_id=? AND scope.run_id IS NULL LIMIT ?)",
                    (run_id, max(1, limit)),
                ).rowcount
            )

        return await super()._background_write(operation)

    async def get_scan_scope_discovery_state(
        self, run_id: str, root_id: str, relative_path: str
    ) -> str:
        def operation(connection: sqlite3.Connection) -> str:
            row = connection.execute(
                "SELECT discovery_state FROM library_scan_run_scopes WHERE run_id = ? "
                "AND root_id = ? AND relative_path = ?",
                (run_id, root_id, relative_path),
            ).fetchone()
            if row is None:
                raise ResourceNotFoundError("Scan scope not found.")
            return str(row["discovery_state"])

        return await self._read(operation)

    async def get_scan_scope_discovery_generation(
        self, run_id: str, root_id: str, relative_path: str
    ) -> int:
        def operation(connection: sqlite3.Connection) -> int:
            row = connection.execute(
                "SELECT discovery_generation FROM library_scan_run_scopes "
                "WHERE run_id = ? AND root_id = ? AND relative_path = ?",
                (run_id, root_id, relative_path),
            ).fetchone()
            if row is None:
                raise ResourceNotFoundError("Scan scope not found.")
            return int(row["discovery_generation"])

        return await self._read(operation)

    async def transition_scan_run(
        self,
        run_id: str,
        *,
        expected_state: str,
        expected_revision: int,
        new_state: str,
        now: float,
        terminal_code: str | None = None,
    ) -> ScanRun:
        if new_state not in _SCAN_TRANSITIONS.get(expected_state, set()):
            raise StaleRevisionError(
                f"Scan state cannot move from {expected_state} to {new_state}."
            )

        def operation(connection: sqlite3.Connection) -> ScanRun:
            current = connection.execute(
                "SELECT * FROM library_scan_runs WHERE id = ?", (run_id,)
            ).fetchone()
            if current is None:
                raise ResourceNotFoundError(f"Scan run not found: {run_id}")
            terminal = new_state in {
                "completed",
                "cancelled",
                "superseded_policy_changed",
                "failed",
            }
            phase = (
                new_state
                if new_state in {"discovering", "indexing", "reconciling"}
                else None
            )
            phase_timings = json.loads(str(current["phase_timings_json"] or "{}"))
            phase_started_at = current["phase_started_at"]
            closes_phase = new_state in {
                "indexing",
                "reconciling",
                "pausing",
                "stopping",
                "completed",
                "cancelled",
                "superseded_policy_changed",
                "failed",
            }
            if phase_started_at is not None and closes_phase:
                current_phase = str(current["phase"])
                phase_timings[current_phase] = float(
                    phase_timings.get(current_phase, 0.0)
                ) + max(0.0, now - float(phase_started_at))
            next_phase_started_at = (
                now if new_state in {"discovering", "indexing", "reconciling"} else None
            )
            resume_phase = expected_state if new_state == "pausing" else None
            updated = connection.execute(
                "UPDATE library_scan_runs SET state = ?, "
                "phase = COALESCE(?, phase), resume_phase = COALESCE(?, resume_phase), "
                "phase_started_at = ?, phase_timings_json = ?, "
                "requested_control = CASE WHEN ? IN ('paused','cancelled') THEN 'none' "
                "ELSE requested_control END, terminal_at = CASE WHEN ? THEN ? ELSE terminal_at END, "
                "terminal_code = COALESCE(?, terminal_code), updated_at = ?, heartbeat_at = ?, "
                "row_revision = row_revision + 1, event_revision = event_revision + 1 "
                "WHERE id = ? AND state = ? AND row_revision = ? AND row_revision < ? "
                "AND event_revision < ? RETURNING *",
                (
                    new_state,
                    phase,
                    resume_phase,
                    next_phase_started_at,
                    json.dumps(phase_timings, sort_keys=True),
                    new_state,
                    terminal,
                    now,
                    terminal_code,
                    now,
                    now,
                    run_id,
                    expected_state,
                    expected_revision,
                    MAX_REVISION,
                    MAX_REVISION,
                ),
            ).fetchone()
            if updated is None:
                self._refuse_max_revision(
                    connection,
                    table="library_scan_runs",
                    predicate="id = ?",
                    parameters=(run_id,),
                    include_event_revision=True,
                )
                current = connection.execute(
                    "SELECT 1 FROM library_scan_runs WHERE id = ?", (run_id,)
                ).fetchone()
                if current is None:
                    raise ResourceNotFoundError(f"Scan run not found: {run_id}")
                raise StaleRevisionError(
                    "The scan run changed before the transition was applied."
                )
            if new_state == "paused":
                connection.execute(
                    "UPDATE library_scan_runs SET control_latency_ms = "
                    "MAX(0, CAST((? - pause_requested_at) * 1000 AS INTEGER)) "
                    "WHERE id = ? AND pause_requested_at IS NOT NULL",
                    (now, run_id),
                )
            elif new_state == "cancelled":
                connection.execute(
                    "UPDATE library_scan_runs SET control_latency_ms = "
                    "MAX(0, CAST((? - stop_requested_at) * 1000 AS INTEGER)) "
                    "WHERE id = ? AND stop_requested_at IS NOT NULL",
                    (now, run_id),
                )
            if new_state == "completed" and updated["kind"] == "policy_reconcile":
                policy_state = connection.execute(
                    "SELECT * FROM library_policy_state WHERE singleton = 1"
                ).fetchone()
                scopes = connection.execute(
                    "SELECT scope_id, policy_revision FROM library_scan_run_scopes "
                    "WHERE run_id = ?",
                    (run_id,),
                ).fetchall()
                if (
                    policy_state is not None
                    and scopes
                    and all(
                        scope["policy_revision"]
                        == policy_state["desired_policy_revision"]
                        for scope in scopes
                    )
                ):
                    applied = {
                        str(scope["scope_id"])
                        for scope in scopes
                        if scope["scope_id"] is not None
                    }
                    pending = [
                        scope_id
                        for scope_id in json.loads(
                            str(policy_state["pending_scope_ids_json"])
                        )
                        if scope_id not in applied
                    ]
                    pending_scopes = [
                        scope
                        for scope in json.loads(
                            str(policy_state["pending_scopes_json"])
                        )
                        if scope.get("scope_id") not in applied
                    ]
                    connection.execute(
                        "UPDATE library_policy_state SET pending_scope_ids_json = ?, "
                        "pending_scopes_json = ?, updated_at = ? WHERE singleton = 1",
                        (
                            json.dumps(pending, sort_keys=True),
                            json.dumps(pending_scopes, sort_keys=True),
                            now,
                        ),
                    )
            if terminal:
                connection.execute(
                    "UPDATE library_scan_runs SET inventory_cleanup_pending = (EXISTS("
                    "SELECT 1 FROM library_scan_inventory WHERE run_id = ?) OR EXISTS("
                    "SELECT 1 FROM library_scan_grouping_contexts WHERE run_id = ?)) "
                    "WHERE id = ?",
                    (run_id, run_id, run_id),
                )
            self._bump_stream(connection, "scan")
            return self._scan_state_from_row(updated)

        return await super()._background_write(operation)

    async def request_scan_control(
        self,
        run_id: str,
        *,
        control: str,
        expected_revision: int,
        now: float,
    ) -> tuple[ScanRun, int]:
        if control not in {"pause", "resume", "stop"}:
            raise StaleRevisionError("Unknown scan control request.")

        def operation(connection: sqlite3.Connection) -> tuple[ScanRun, int]:
            row = connection.execute(
                "SELECT * FROM library_scan_runs WHERE id = ?", (run_id,)
            ).fetchone()
            if row is None:
                raise ResourceNotFoundError(f"Scan run not found: {run_id}")
            state = str(row["state"])
            if control == "pause" and state in {"pausing", "paused"}:
                return self._scan_state_from_row(row), self.get_stream_revision_sync(
                    connection, "scan"
                )
            if control == "stop" and state in {"stopping", "cancelled"}:
                return self._scan_state_from_row(row), self.get_stream_revision_sync(
                    connection, "scan"
                )
            if (
                control == "resume"
                and state in {"discovering", "indexing", "reconciling"}
                and row["requested_control"] == "none"
            ):
                return self._scan_state_from_row(row), self.get_stream_revision_sync(
                    connection, "scan"
                )
            if int(row["row_revision"]) != expected_revision:
                raise StaleRevisionError(
                    "The scan run changed before the control was applied."
                )
            if control == "pause":
                if state not in {"discovering", "indexing", "reconciling"}:
                    raise StaleRevisionError(
                        "This scan cannot be paused in its current state."
                    )
                new_state, requested, resume_phase = (
                    "pausing",
                    "pause",
                    str(row["phase"]),
                )
            elif control == "resume":
                if state != "paused" or row["resume_phase"] is None:
                    raise StaleRevisionError("Only a paused scan can be resumed.")
                new_state, requested, resume_phase = (
                    str(row["resume_phase"]),
                    "none",
                    None,
                )
            else:
                if state == "paused":
                    new_state, requested, resume_phase = (
                        "cancelled",
                        "none",
                        row["resume_phase"],
                    )
                elif state in {
                    "queued",
                    "discovering",
                    "indexing",
                    "reconciling",
                    "pausing",
                }:
                    new_state = "cancelled" if state == "queued" else "stopping"
                    requested, resume_phase = "stop", row["resume_phase"]
                else:
                    raise StaleRevisionError(
                        "This scan cannot be stopped in its current state."
                    )
            terminal = new_state == "cancelled"
            updated = connection.execute(
                "UPDATE library_scan_runs SET state = ?, requested_control = ?, resume_phase = ?, "
                "pause_requested_at = CASE WHEN ? = 'pause' THEN ? ELSE pause_requested_at END, "
                "stop_requested_at = CASE WHEN ? = 'stop' THEN ? ELSE stop_requested_at END, "
                "terminal_at = CASE WHEN ? THEN ? ELSE terminal_at END, updated_at = ?, "
                "row_revision = row_revision + 1, event_revision = event_revision + 1 "
                "WHERE id = ? AND row_revision = ? AND row_revision < ? AND event_revision < ? "
                "RETURNING *",
                (
                    new_state,
                    requested,
                    resume_phase,
                    control,
                    now,
                    control,
                    now,
                    terminal,
                    now,
                    now,
                    run_id,
                    expected_revision,
                    MAX_REVISION,
                    MAX_REVISION,
                ),
            ).fetchone()
            if updated is None:
                raise RevisionOverflowError(
                    "A scan run revision reached its maximum value."
                )
            if terminal:
                connection.execute(
                    "UPDATE library_scan_runs SET inventory_cleanup_pending = (EXISTS("
                    "SELECT 1 FROM library_scan_inventory WHERE run_id = ?) OR EXISTS("
                    "SELECT 1 FROM library_scan_grouping_contexts WHERE run_id = ?)) "
                    "WHERE id = ?",
                    (run_id, run_id, run_id),
                )
            stream_revision = self._bump_stream(connection, "scan")
            return self._scan_state_from_row(updated), stream_revision

        return await self._write(operation)

    @staticmethod
    def get_stream_revision_sync(connection: sqlite3.Connection, stream: str) -> int:
        row = connection.execute(
            "SELECT value FROM library_event_stream_revisions WHERE stream_kind = ?",
            (stream,),
        ).fetchone()
        return int(row["value"])

    async def complete_scan_scope_discovery(
        self,
        run_id: str,
        root_id: str,
        relative_path: str,
        *,
        state: str,
        error_code: str | None,
    ) -> None:
        def operation(connection: sqlite3.Connection) -> None:
            cursor = connection.execute(
                "UPDATE library_scan_run_scopes SET discovery_state = ?, error_code = ?, "
                "row_revision = row_revision + 1 WHERE run_id = ? AND root_id = ? "
                "AND relative_path = ? AND row_revision < ?",
                (state, error_code, run_id, root_id, relative_path, MAX_REVISION),
            )
            if cursor.rowcount != 1:
                raise ResourceNotFoundError("Scan scope not found.")

        await super()._background_write(operation)

    async def get_scan_inventory_batch(
        self, run_id: str, *, processing_state: str, limit: int
    ) -> list[dict[str, Any]]:
        def operation(connection: sqlite3.Connection) -> list[dict[str, Any]]:
            return [
                dict(row)
                for row in connection.execute(
                    "SELECT inventory.* FROM library_scan_inventory inventory "
                    "JOIN library_scan_run_scopes scope ON scope.run_id = inventory.run_id "
                    "AND scope.root_id = inventory.root_id "
                    "AND scope.relative_path = inventory.scope_relative_path "
                    "AND scope.discovery_generation = inventory.discovery_generation "
                    "WHERE inventory.run_id = ? AND inventory.processing_state = ? "
                    "ORDER BY inventory.root_id, inventory.relative_path LIMIT ?",
                    (run_id, processing_state, limit),
                ).fetchall()
            ]

        return await self._read(operation)

    async def mark_scan_inventory_batch(
        self,
        run_id: str,
        keys: list[tuple[str, str]],
        *,
        processing_state: str,
        failure_code: str | None = None,
    ) -> None:
        def operation(connection: sqlite3.Connection) -> None:
            connection.executemany(
                "UPDATE library_scan_inventory SET processing_state = ?, failure_code = ?, "
                "row_revision = row_revision + 1 WHERE run_id = ? AND root_id = ? "
                "AND relative_path = ? AND row_revision < ?",
                [
                    (
                        processing_state,
                        failure_code,
                        run_id,
                        root_id,
                        path,
                        MAX_REVISION,
                    )
                    for root_id, path in keys
                ],
            )

        await super()._background_write(operation)

    async def add_scan_counters(
        self,
        run_id: str,
        increments: dict[str, int],
        *,
        updated_at: float,
    ) -> ScanRun:
        allowed = {
            "inspected_count",
            "new_count",
            "changed_count",
            "indexed_count",
            "unchanged_count",
            "excluded_count",
            "missing_count",
            "errored_count",
            "identification_enqueued_count",
        }
        invalid = set(increments) - allowed
        if invalid:
            raise ValueError(f"Unknown scan counters: {sorted(invalid)}")

        def operation(connection: sqlite3.Connection) -> ScanRun:
            assignments = [f"{name} = {name} + ?" for name in increments]
            parameters: list[Any] = [increments[name] for name in increments]
            assignments.extend(
                [
                    "updated_at = ?",
                    "row_revision = row_revision + 1",
                    "event_revision = event_revision + 1",
                ]
            )
            parameters.extend([updated_at, run_id, MAX_REVISION, MAX_REVISION])
            updated = connection.execute(
                f"UPDATE library_scan_runs SET {', '.join(assignments)} WHERE id = ? "
                "AND row_revision < ? AND event_revision < ? RETURNING *",
                parameters,
            ).fetchone()
            if updated is None:
                raise RevisionOverflowError(
                    "A scan run revision reached its maximum value."
                )
            self._bump_stream(connection, "scan")
            return self._scan_state_from_row(updated)

        return await super()._background_write(operation)

    async def recover_scan_runs(self, *, now: float) -> list[ScanRun]:
        def operation(connection: sqlite3.Connection) -> list[ScanRun]:
            stopping = connection.execute(
                "SELECT id FROM library_scan_runs WHERE state = 'stopping' "
                "OR requested_control = 'stop'"
            ).fetchall()
            for row in stopping:
                connection.execute(
                    "UPDATE library_scan_runs SET state = 'cancelled', terminal_at = ?, "
                    "updated_at = ?, requested_control = 'none', row_revision = row_revision + 1, "
                    "event_revision = event_revision + 1 WHERE id = ?",
                    (now, now, row["id"]),
                )
                connection.execute(
                    "UPDATE library_scan_runs SET inventory_cleanup_pending = (EXISTS("
                    "SELECT 1 FROM library_scan_inventory WHERE run_id = ?) OR EXISTS("
                    "SELECT 1 FROM library_scan_grouping_contexts WHERE run_id = ?)) "
                    "WHERE id = ?",
                    (row["id"], row["id"], row["id"]),
                )
            connection.execute(
                "UPDATE library_scan_runs SET state = 'paused', requested_control = 'none', "
                "updated_at = ?, row_revision = row_revision + 1, event_revision = event_revision + 1 "
                "WHERE state = 'pausing' OR requested_control = 'pause'",
                (now,),
            )
            resumable = connection.execute(
                "SELECT id FROM library_scan_runs WHERE state IN "
                "('discovering','indexing','reconciling','paused') ORDER BY queued_at"
            ).fetchall()
            for row in resumable:
                sequence = connection.execute(
                    "SELECT COALESCE(MAX(trigger_sequence), -1) + 1 "
                    "FROM library_scan_run_triggers WHERE run_id = ?",
                    (row["id"],),
                ).fetchone()[0]
                connection.execute(
                    "INSERT INTO library_scan_run_triggers "
                    "(run_id, trigger_sequence, trigger, requested_by_user_id, reason, requested_at) "
                    "VALUES (?, ?, 'startup_resume', NULL, 'process_restart', ?)",
                    (row["id"], sequence, now),
                )
            if stopping or resumable:
                self._bump_stream(connection, "scan")
            rows = connection.execute(
                "SELECT * FROM library_scan_runs WHERE state IN "
                "('queued','discovering','indexing','reconciling','paused') ORDER BY queued_at"
            ).fetchall()
            return [self._scan_state_from_row(row) for row in rows]

        return await super()._background_write(operation)

    async def cleanup_terminal_scan_inventory(
        self, *, limit: int = 5_000
    ) -> tuple[str | None, int, bool]:
        """Remove one bounded terminal inventory page and resume after restarts."""

        def operation(
            connection: sqlite3.Connection,
        ) -> tuple[str | None, int, bool]:
            row = connection.execute(
                "SELECT id FROM library_scan_runs WHERE terminal_at IS NOT NULL "
                "AND inventory_cleanup_pending = 1 ORDER BY terminal_at, id LIMIT 1"
            ).fetchone()
            if row is None:
                connection.execute(
                    "DELETE FROM library_scan_runs WHERE id IN ("
                    "SELECT id FROM library_scan_runs WHERE terminal_at IS NOT NULL "
                    "AND inventory_cleanup_pending = 0 ORDER BY terminal_at DESC, id DESC "
                    "LIMIT -1 OFFSET 50)"
                )
                return None, 0, True
            run_id = str(row["id"])
            for table in (
                "library_scan_grouping_evidence",
                "library_scan_grouping_edges",
                "library_scan_grouping_new_nodes",
                "library_scan_grouping_old_nodes",
                "library_scan_grouping_values",
                "library_scan_grouping_groups",
            ):
                deleted = connection.execute(
                    f"DELETE FROM {table} WHERE rowid IN ("
                    f"SELECT rowid FROM {table} WHERE run_id=? LIMIT ?)",
                    (run_id, max(1, limit)),
                ).rowcount
                if deleted:
                    return run_id, deleted, False
            deleted_contexts = connection.execute(
                "DELETE FROM library_scan_grouping_contexts WHERE rowid IN ("
                "SELECT rowid FROM library_scan_grouping_contexts "
                "WHERE run_id=? LIMIT ?)",
                (run_id, max(1, limit)),
            ).rowcount
            if deleted_contexts:
                return run_id, deleted_contexts, False
            deleted = connection.execute(
                "DELETE FROM library_scan_inventory WHERE rowid IN ("
                "SELECT rowid FROM library_scan_inventory WHERE run_id = ? "
                "ORDER BY root_id, relative_path LIMIT ?)",
                (run_id, max(1, limit)),
            ).rowcount
            remaining = connection.execute(
                "SELECT 1 FROM library_scan_inventory WHERE run_id = ? LIMIT 1",
                (run_id,),
            ).fetchone()
            done = remaining is None
            if done:
                connection.execute(
                    "UPDATE library_scan_runs SET inventory_cleanup_pending = 0 WHERE id = ?",
                    (run_id,),
                )
                connection.execute(
                    "DELETE FROM library_scan_runs WHERE id IN ("
                    "SELECT id FROM library_scan_runs WHERE terminal_at IS NOT NULL "
                    "AND inventory_cleanup_pending = 0 ORDER BY terminal_at DESC, id DESC "
                    "LIMIT -1 OFFSET 50)"
                )
            return run_id, deleted, done

        return await super()._background_write(operation)

    async def add_scan_inventory_batch(
        self,
        run_id: str,
        items: list[ScanInventoryItem],
        *,
        expected_run_revision: int,
        updated_at: float,
        discovery_generation: int = 1,
    ) -> tuple[int, int]:
        def operation(connection: sqlite3.Connection) -> tuple[int, int]:
            connection.executemany(
                "INSERT INTO library_scan_inventory "
                "(run_id, root_id, relative_path, scope_relative_path, discovery_generation, absolute_path, file_size_bytes, "
                "file_mtime_ns, stat_revision, policy_revision, effective_policy, "
                "comparison_result, local_track_id) VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?) "
                "ON CONFLICT(run_id, root_id, relative_path) DO UPDATE SET "
                "scope_relative_path = excluded.scope_relative_path, "
                "discovery_generation = excluded.discovery_generation, "
                "absolute_path = excluded.absolute_path, file_size_bytes = excluded.file_size_bytes, "
                "file_mtime_ns = excluded.file_mtime_ns, stat_revision = excluded.stat_revision, "
                "policy_revision = excluded.policy_revision, "
                "effective_policy = excluded.effective_policy, "
                "comparison_result = excluded.comparison_result, "
                "local_track_id = excluded.local_track_id, row_revision = "
                "library_scan_inventory.row_revision + 1",
                [
                    (
                        run_id,
                        item.root_id,
                        item.relative_path,
                        item.scope_relative_path,
                        discovery_generation,
                        item.absolute_path,
                        item.file_size_bytes,
                        item.file_mtime_ns,
                        item.stat_revision,
                        item.policy_revision,
                        item.effective_policy,
                        item.comparison_result,
                        item.local_track_id,
                    )
                    for item in items
                ],
            )
            if items:
                connection.execute(
                    "UPDATE library_scan_run_scopes SET discovered_count = discovered_count + ?, "
                    "row_revision = row_revision + 1 WHERE run_id = ? AND root_id = ? "
                    "AND relative_path = ? AND discovery_generation = ?",
                    (
                        len(items),
                        run_id,
                        items[0].root_id,
                        items[0].scope_relative_path,
                        discovery_generation,
                    ),
                )
            updated = connection.execute(
                "UPDATE library_scan_runs SET discovered_count = discovered_count + ?, "
                "updated_at = ?, row_revision = row_revision + 1, event_revision = event_revision + 1 "
                "WHERE id = ? AND row_revision = ? AND row_revision < ? "
                "AND event_revision < ? RETURNING row_revision",
                (
                    len(items),
                    updated_at,
                    run_id,
                    expected_run_revision,
                    MAX_REVISION,
                    MAX_REVISION,
                ),
            ).fetchone()
            if updated is None:
                self._refuse_max_revision(
                    connection,
                    table="library_scan_runs",
                    predicate="id = ?",
                    parameters=(run_id,),
                    include_event_revision=True,
                )
                raise StaleRevisionError(
                    "The scan run changed before its inventory batch was recorded."
                )
            return int(updated["row_revision"]), self._bump_stream(connection, "scan")

        return await super()._background_write(operation)

    async def classify_scan_paths(
        self, root_id: str, paths: list[tuple[str, int, int, float, str]]
    ) -> dict[str, tuple[str, str | None]]:
        """Classify one inventory page and promote compatible legacy revisions."""

        if not paths:
            return {}

        def has_legacy_candidates(connection: sqlite3.Connection) -> bool:
            relative_paths = [path[0] for path in paths]
            placeholders = ",".join("?" for _ in relative_paths)
            return (
                connection.execute(
                    "SELECT 1 FROM local_tracks WHERE root_id=? "
                    f"AND relative_path IN ({placeholders}) "
                    "AND stat_revision_kind!='exact' LIMIT 1",
                    (root_id, *relative_paths),
                ).fetchone()
                is not None
            )

        needs_promotion = await self._read(has_legacy_candidates)

        def operation(
            connection: sqlite3.Connection,
        ) -> dict[str, tuple[str, str | None]]:
            result: dict[str, tuple[str, str | None]] = {}
            values = ",".join("(?,?,?,?,?)" for _ in paths)
            parameters: list[Any] = []
            for path in paths:
                parameters.extend(path)
            rows = connection.execute(
                "WITH incoming(relative_path, file_size_bytes, file_mtime_ns, "
                f"file_mtime, stat_revision) AS (VALUES {values}) "
                "SELECT incoming.*, track.id, track.file_size_bytes AS saved_size, "
                "track.file_mtime_ns AS saved_mtime_ns, "
                "track.stat_revision AS saved_revision, track.stat_revision_kind, "
                "track.tags_read_at FROM incoming "
                "LEFT JOIN local_tracks track ON track.root_id = ? "
                "AND track.relative_path = incoming.relative_path",
                (*parameters, root_id),
            ).fetchall()
            promotions: list[tuple[str, int, int, str]] = []
            for row in rows:
                relative_path = str(row["relative_path"])
                if row["id"] is None:
                    result[relative_path] = ("new", None)
                    continue
                kind = str(row["stat_revision_kind"] or "unclassified")
                same_size = int(row["saved_size"]) == int(row["file_size_bytes"])
                unchanged = False
                if kind in {"exact", "unclassified"}:
                    unchanged = str(row["saved_revision"]) == str(row["stat_revision"])
                elif kind == "legacy_float":
                    unchanged = same_size and int(
                        float(row["file_mtime"]) * 1_000_000_000
                    ) == int(row["saved_mtime_ns"])
                elif kind == "legacy_review" and row["tags_read_at"] is not None:
                    unchanged = same_size and float(row["file_mtime"]) <= float(
                        row["tags_read_at"]
                    )
                track_id = str(row["id"])
                result[relative_path] = (
                    "unchanged" if unchanged else "changed",
                    track_id,
                )
                if unchanged and kind != "exact":
                    promotions.append(
                        (
                            str(row["stat_revision"]),
                            int(row["file_size_bytes"]),
                            int(row["file_mtime_ns"]),
                            track_id,
                        )
                    )
            connection.executemany(
                "UPDATE local_tracks SET stat_revision = ?, file_size_bytes = ?, "
                "file_mtime_ns = ?, stat_revision_kind = 'exact' WHERE id = ?",
                promotions,
            )
            return result

        if needs_promotion:
            return await super()._background_write(operation)
        return await self._read(operation)

    async def normalize_pending_legacy_inventory(
        self, run_id: str, *, limit: int
    ) -> int:
        """Repair migration-shaped rows in a persisted, already-discovered scan."""

        def has_candidates(connection: sqlite3.Connection) -> bool:
            return (
                connection.execute(
                    "SELECT 1 FROM library_scan_inventory inventory JOIN local_tracks track "
                    "ON track.root_id=inventory.root_id "
                    "AND track.relative_path=inventory.relative_path "
                    "WHERE inventory.run_id=? AND inventory.processing_state='pending' "
                    "AND inventory.comparison_result='changed' "
                    "AND track.stat_revision_kind IN "
                    "('legacy_float','legacy_review','unclassified') LIMIT 1",
                    (run_id,),
                ).fetchone()
                is not None
            )

        if not await self._read(has_candidates):
            return 0

        def operation(connection: sqlite3.Connection) -> int:
            rows = connection.execute(
                "SELECT inventory.root_id, inventory.relative_path, "
                "inventory.file_size_bytes, inventory.file_mtime_ns, "
                "inventory.stat_revision, track.id, track.file_size_bytes AS saved_size, "
                "track.file_mtime_ns AS saved_mtime_ns, track.stat_revision_kind, "
                "track.tags_read_at, track.availability FROM library_scan_inventory inventory "
                "JOIN library_scan_run_scopes scope ON scope.run_id = inventory.run_id "
                "AND scope.root_id = inventory.root_id "
                "AND scope.relative_path = inventory.scope_relative_path "
                "AND scope.discovery_generation = inventory.discovery_generation "
                "JOIN local_tracks track ON track.root_id = inventory.root_id "
                "AND track.relative_path = inventory.relative_path "
                "WHERE inventory.run_id = ? AND inventory.processing_state = 'pending' "
                "AND inventory.comparison_result = 'changed' "
                "AND track.stat_revision_kind IN ('legacy_float','legacy_review','unclassified') "
                "ORDER BY inventory.root_id, inventory.relative_path LIMIT ?",
                (run_id, limit),
            ).fetchall()
            repaired: list[sqlite3.Row] = []
            for row in rows:
                same_size = int(row["saved_size"]) == int(row["file_size_bytes"])
                kind = str(row["stat_revision_kind"])
                if kind == "legacy_float":
                    legacy_current_ns = int(
                        (int(row["file_mtime_ns"]) / 1_000_000_000) * 1_000_000_000
                    )
                    unchanged = (
                        same_size and int(row["saved_mtime_ns"]) == legacy_current_ns
                    )
                elif kind == "legacy_review":
                    unchanged = (
                        same_size
                        and row["tags_read_at"] is not None
                        and int(row["file_mtime_ns"])
                        <= int(float(row["tags_read_at"]) * 1_000_000_000)
                    )
                else:
                    unchanged = False
                if unchanged and row["availability"] == "indexed":
                    repaired.append(row)
            connection.executemany(
                "UPDATE local_tracks SET stat_revision = ?, file_size_bytes = ?, "
                "file_mtime_ns = ?, stat_revision_kind = 'exact' WHERE id = ?",
                [
                    (
                        row["stat_revision"],
                        row["file_size_bytes"],
                        row["file_mtime_ns"],
                        row["id"],
                    )
                    for row in repaired
                ],
            )
            connection.executemany(
                "UPDATE library_scan_inventory SET comparison_result = 'unchanged', "
                "local_track_id = ?, row_revision = row_revision + 1 "
                "WHERE run_id = ? AND root_id = ? AND relative_path = ?",
                [
                    (row["id"], run_id, row["root_id"], row["relative_path"])
                    for row in repaired
                ],
            )
            return len(repaired)

        return await super()._background_write(operation)

    async def estimate_scan_scope(self, scopes: list[ScanScope]) -> int:
        def operation(connection: sqlite3.Connection) -> int:
            track_ids: set[str] = set()
            for scope in scopes:
                if scope.relative_path == ".":
                    rows = connection.execute(
                        "SELECT id FROM local_tracks WHERE root_id = ?",
                        (scope.root_id,),
                    ).fetchall()
                else:
                    prefix = _escape_like(scope.relative_path.rstrip("/"))
                    rows = connection.execute(
                        "SELECT id FROM local_tracks WHERE root_id = ? "
                        "AND relative_path LIKE ? ESCAPE '\\'",
                        (scope.root_id, prefix + "/%"),
                    ).fetchall()
                track_ids.update(str(row["id"]) for row in rows)
            return len(track_ids)

        return await self._read(operation)

    async def get_stored_sibling_context(
        self, root_id: str, relative_directory: str
    ) -> list[dict[str, Any]]:
        prefix = (
            f"{relative_directory.rstrip('/')}/" if relative_directory != "." else ""
        )
        pattern = f"{_escape_like(prefix)}%"

        def operation(connection: sqlite3.Connection) -> list[dict[str, Any]]:
            rows = connection.execute(
                "SELECT * FROM local_tracks WHERE root_id = ? "
                "AND relative_path LIKE ? ESCAPE '\\' "
                "ORDER BY relative_path",
                (root_id, pattern),
            ).fetchall()
            return [dict(row) for row in rows]

        return await self._read(operation)

    def _upsert_scanned_track_tx(
        self,
        connection: sqlite3.Connection,
        write: ScannedTrackWrite,
        *,
        scan_run_id: str,
    ) -> tuple[str, bool]:
        artist = write.artist
        resolved_artist_id, _ = self._resolve_or_create_local_artist(
            connection,
            display_name=artist.display_name,
            sort_name=artist.sort_name,
            kind=artist.kind,
            candidate_id=artist.id,
            normalized=_normalize_exact(artist.display_name),
            folded=_fold(artist.display_name) or "",
            normalized_sort=_normalize_exact(artist.sort_name),
            now=artist.updated_at,
        )
        album = msgspec.structs.replace(write.album, album_artist_id=resolved_artist_id)
        credit = msgspec.structs.replace(
            write.credit, local_artist_id=resolved_artist_id
        )
        track = write.track
        connection.execute(
            "INSERT INTO local_albums "
            "(id, root_id, grouping_key, title, title_folded, album_artist_name, "
            "album_artist_name_folded, album_artist_id, album_artist_sort_name, year, "
            "original_release_date, primary_genre, is_compilation, grouping_source, "
            "created_at, updated_at) VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?) "
            "ON CONFLICT(id) DO UPDATE SET title = excluded.title, "
            "title_folded = excluded.title_folded, album_artist_name = excluded.album_artist_name, "
            "album_artist_name_folded = excluded.album_artist_name_folded, "
            "album_artist_id = excluded.album_artist_id, year = excluded.year, "
            "primary_genre = excluded.primary_genre, updated_at = excluded.updated_at, "
            "row_revision = local_albums.row_revision + 1",
            (
                album.id,
                album.root_id,
                album.grouping_key,
                album.title,
                _fold(album.title),
                album.album_artist_name,
                _fold(album.album_artist_name),
                album.album_artist_id,
                album.album_artist_sort_name,
                album.year,
                album.original_release_date,
                album.primary_genre,
                int(album.is_compilation),
                album.grouping_source,
                album.created_at,
                album.updated_at,
            ),
        )
        connection.execute(
            "INSERT OR IGNORE INTO local_album_artists "
            "(local_album_id, position, local_artist_id, role, credited_name) "
            "VALUES (?,0,?,?,?)",
            (album.id, resolved_artist_id, credit.role, credit.credited_name),
        )
        existing = connection.execute(
            "SELECT id,membership_locked,local_album_id,tag_revision,duration_seconds,"
            "file_format,bit_rate,sample_rate,bit_depth,channels,availability "
            "FROM local_tracks "
            "WHERE root_id = ? AND relative_path = ?",
            (track.root_id, track.relative_path),
        ).fetchone()
        if existing is None:
            self._insert_track(connection, track)
            track_id = track.id
            catalog_changed = True
        else:
            track_id = str(existing["id"])
            local_album_id = (
                str(existing["local_album_id"])
                if bool(existing["membership_locked"])
                else track.local_album_id
            )
            catalog_changed = any(
                (
                    str(existing["local_album_id"]) != local_album_id,
                    str(existing["tag_revision"]) != track.tag_revision,
                    existing["duration_seconds"] != track.duration_seconds,
                    existing["file_format"] != track.file_format,
                    existing["bit_rate"] != track.bit_rate,
                    existing["sample_rate"] != track.sample_rate,
                    existing["bit_depth"] != track.bit_depth,
                    existing["channels"] != track.channels,
                    existing["availability"] != "indexed",
                )
            )
            connection.execute(
                "UPDATE local_tracks SET local_album_id = ?, file_path = ?, path_hash = ?, "
                "file_size_bytes = ?, file_mtime_ns = ?, stat_revision = ?, "
                "stat_revision_kind = ?, tag_revision = ?, tags_read_at = ?, "
                "metadata_incomplete = ?, title = ?, title_folded = ?, "
                "artist_name = ?, artist_name_folded = ?, album_title = ?, "
                "album_title_folded = ?, album_artist_name = ?, album_artist_name_folded = ?, "
                "tag_album_title = ?, tag_album_artist_name = ?, "
                "disc_number = ?, track_number = ?, year = ?, genre = ?, genre_folded = ?, title_sort = ?, "
                "artist_sort = ?, album_sort = ?, album_artist_sort = ?, disc_subtitle = ?, "
                "is_compilation = ?, embedded_release_group_mbid = ?, embedded_release_mbid = ?, "
                "embedded_recording_mbid = ?, embedded_artist_mbid = ?, "
                "embedded_album_artist_mbid = ?, duration_seconds = ?, file_format = ?, bit_rate = ?, "
                "sample_rate = ?, bit_depth = ?, channels = ?, replaygain_track_gain = ?, "
                "replaygain_album_gain = ?, replaygain_track_peak = ?, replaygain_album_peak = ?, "
                "availability = 'indexed', missing_since = NULL, excluded_at = NULL, "
                "ingest_source = ?, download_task_id = ?, source_path = ?, imported_at = ?, "
                "desired_policy_revision = ?, applied_policy_revision = ?, applied_policy = ?, "
                "row_revision = row_revision + 1 WHERE id = ?",
                (
                    local_album_id,
                    track.file_path,
                    track.path_hash,
                    track.file_size_bytes,
                    track.file_mtime_ns,
                    track.stat_revision,
                    track.stat_revision_kind,
                    track.tag_revision,
                    track.tags_read_at,
                    int(track.metadata_incomplete),
                    track.title,
                    _fold(track.title),
                    track.artist_name,
                    _fold(track.artist_name),
                    track.album_title,
                    _fold(track.album_title),
                    track.album_artist_name,
                    _fold(track.album_artist_name),
                    track.tag_album_title,
                    track.tag_album_artist_name,
                    track.disc_number,
                    track.track_number,
                    track.year,
                    track.genre,
                    _fold(track.genre),
                    track.title_sort,
                    track.artist_sort,
                    track.album_sort,
                    track.album_artist_sort,
                    track.disc_subtitle,
                    int(track.is_compilation),
                    track.embedded_release_group_mbid,
                    track.embedded_release_mbid,
                    track.embedded_recording_mbid,
                    track.embedded_artist_mbid,
                    track.embedded_album_artist_mbid,
                    track.duration_seconds,
                    track.file_format,
                    track.bit_rate,
                    track.sample_rate,
                    track.bit_depth,
                    track.channels,
                    track.replaygain_track_gain,
                    track.replaygain_album_gain,
                    track.replaygain_track_peak,
                    track.replaygain_album_peak,
                    track.ingest_source,
                    track.download_task_id,
                    track.source_path,
                    track.imported_at,
                    track.desired_policy_revision,
                    track.applied_policy_revision,
                    track.applied_policy,
                    track_id,
                ),
            )
        connection.execute(
            "INSERT OR IGNORE INTO local_track_artists "
            "(local_track_id, position, local_artist_id, role, credited_name) "
            "VALUES (?,0,?,?,?)",
            (track_id, resolved_artist_id, credit.role, credit.credited_name),
        )
        connection.execute(
            "INSERT OR IGNORE INTO library_scan_grouping_contexts "
            "(run_id, root_id, relative_directory) VALUES (?,?,?)",
            (scan_run_id, track.root_id, write.grouping_context),
        )
        return track_id, catalog_changed

    async def commit_scan_index_batch(
        self,
        run_id: str,
        *,
        writes: list[ScannedTrackWrite],
        states: dict[str, list[tuple[str, str]]],
        failures: list[tuple[str, str, str]],
        increments: dict[str, int],
        updated_at: float,
    ) -> ScanRun:
        """Commit catalog, inventory, counters, heartbeat, and stream atomically."""

        def operation(connection: sqlite3.Connection) -> ScanRun:
            catalog_changed = False
            for write in writes:
                track_id, changed = self._upsert_scanned_track_tx(
                    connection, write, scan_run_id=run_id
                )
                catalog_changed = catalog_changed or changed
                connection.execute(
                    "UPDATE library_scan_inventory SET processing_state = 'indexed', "
                    "local_track_id = ?, failure_code = NULL, row_revision = row_revision + 1 "
                    "WHERE run_id = ? AND root_id = ? AND relative_path = ?",
                    (track_id, run_id, write.root_id, write.relative_path),
                )
            for state, keys in states.items():
                connection.executemany(
                    "UPDATE library_scan_inventory SET processing_state = ?, "
                    "failure_code = NULL, row_revision = row_revision + 1 "
                    "WHERE run_id = ? AND root_id = ? AND relative_path = ?",
                    [(state, run_id, root_id, path) for root_id, path in keys],
                )
            connection.executemany(
                "UPDATE library_scan_inventory SET processing_state = 'failed', "
                "failure_code = ?, row_revision = row_revision + 1 "
                "WHERE run_id = ? AND root_id = ? AND relative_path = ?",
                [
                    (failure_code, run_id, root_id, path)
                    for root_id, path, failure_code in failures
                ],
            )
            if catalog_changed:
                self._bump_catalog(connection)
            allowed = {
                "inspected_count",
                "new_count",
                "changed_count",
                "indexed_count",
                "unchanged_count",
                "excluded_count",
                "errored_count",
            }
            if set(increments) - allowed:
                raise ValueError("Unknown scan counter increment")
            assignments = [f"{name} = {name} + ?" for name in increments]
            parameters: list[Any] = [increments[name] for name in increments]
            assignments.extend(
                [
                    "updated_at = ?",
                    "heartbeat_at = ?",
                    "row_revision = row_revision + 1",
                    "event_revision = event_revision + 1",
                ]
            )
            parameters.extend(
                [updated_at, updated_at, run_id, MAX_REVISION, MAX_REVISION]
            )
            updated = connection.execute(
                f"UPDATE library_scan_runs SET {', '.join(assignments)} WHERE id = ? "
                "AND row_revision < ? AND event_revision < ? RETURNING *",
                parameters,
            ).fetchone()
            if updated is None:
                raise RevisionOverflowError(
                    "A scan run revision reached its maximum value."
                )
            self._bump_stream(connection, "scan")
            return self._scan_state_from_row(updated)

        return await self._write_scan(operation)

    async def upsert_scanned_track(
        self,
        *,
        artist: LocalArtist,
        album: LocalAlbum,
        track: LocalTrack,
        credit: LocalArtistCredit,
        scan_run_id: str | None = None,
        grouping_context: str | None = None,
        expected_policy_revision: str | None = None,
    ) -> tuple[str, int]:
        """Persist one bounded scan unit while preserving established local IDs."""

        def operation(connection: sqlite3.Connection) -> tuple[str, int]:
            if expected_policy_revision is not None:
                self._require_policy_revision_sync(
                    connection, expected_policy_revision=expected_policy_revision
                )
            connection.execute(
                "INSERT INTO local_artists "
                "(id, display_name, sort_name, folded_name, normalized_name, kind, created_at, updated_at) "
                "VALUES (?,?,?,?,?,?,?,?) ON CONFLICT(id) DO UPDATE SET "
                "display_name = excluded.display_name, sort_name = excluded.sort_name, "
                "folded_name = excluded.folded_name, normalized_name = excluded.normalized_name, "
                "updated_at = excluded.updated_at, "
                "row_revision = local_artists.row_revision + 1",
                (
                    artist.id,
                    artist.display_name,
                    artist.sort_name,
                    artist.folded_name,
                    artist.normalized_name or _normalize_exact(artist.display_name),
                    artist.kind,
                    artist.created_at,
                    artist.updated_at,
                ),
            )
            connection.execute(
                "INSERT INTO local_albums "
                "(id, root_id, grouping_key, title, title_folded, album_artist_name, "
                "album_artist_name_folded, album_artist_id, album_artist_sort_name, year, "
                "original_release_date, primary_genre, is_compilation, grouping_source, "
                "created_at, updated_at) VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?) "
                "ON CONFLICT(id) DO UPDATE SET title = excluded.title, "
                "title_folded = excluded.title_folded, album_artist_name = excluded.album_artist_name, "
                "album_artist_name_folded = excluded.album_artist_name_folded, "
                "album_artist_id = excluded.album_artist_id, year = excluded.year, "
                "primary_genre = excluded.primary_genre, updated_at = excluded.updated_at, "
                "row_revision = local_albums.row_revision + 1",
                (
                    album.id,
                    album.root_id,
                    album.grouping_key,
                    album.title,
                    _fold(album.title),
                    album.album_artist_name,
                    _fold(album.album_artist_name),
                    album.album_artist_id,
                    album.album_artist_sort_name,
                    album.year,
                    album.original_release_date,
                    album.primary_genre,
                    int(album.is_compilation),
                    album.grouping_source,
                    album.created_at,
                    album.updated_at,
                ),
            )
            connection.execute(
                "INSERT OR IGNORE INTO local_album_artists "
                "(local_album_id, position, local_artist_id, role, credited_name) "
                "VALUES (?,0,?,?,?)",
                (album.id, artist.id, credit.role, credit.credited_name),
            )
            existing = connection.execute(
                "SELECT id, membership_locked, local_album_id FROM local_tracks "
                "WHERE root_id = ? AND relative_path = ?",
                (track.root_id, track.relative_path),
            ).fetchone()
            if existing is None:
                self._insert_track(connection, track)
                track_id = track.id
            else:
                track_id = str(existing["id"])
                local_album_id = (
                    str(existing["local_album_id"])
                    if bool(existing["membership_locked"])
                    else track.local_album_id
                )
                connection.execute(
                    "UPDATE local_tracks SET local_album_id = ?, file_path = ?, path_hash = ?, "
                    "file_size_bytes = ?, file_mtime_ns = ?, stat_revision = ?, "
                    "stat_revision_kind = ?, tag_revision = ?, "
                    "tags_read_at = ?, metadata_incomplete = ?, title = ?, title_folded = ?, "
                    "artist_name = ?, artist_name_folded = ?, album_title = ?, "
                    "album_title_folded = ?, album_artist_name = ?, album_artist_name_folded = ?, "
                    "tag_album_title = ?, tag_album_artist_name = ?, "
                    "disc_number = ?, track_number = ?, year = ?, genre = ?, genre_folded = ?, title_sort = ?, "
                    "artist_sort = ?, album_sort = ?, album_artist_sort = ?, disc_subtitle = ?, "
                    "is_compilation = ?, embedded_release_group_mbid = ?, embedded_release_mbid = ?, "
                    "embedded_recording_mbid = ?, embedded_artist_mbid = ?, "
                    "embedded_album_artist_mbid = ?, duration_seconds = ?, file_format = ?, bit_rate = ?, "
                    "sample_rate = ?, bit_depth = ?, channels = ?, replaygain_track_gain = ?, "
                    "replaygain_album_gain = ?, replaygain_track_peak = ?, replaygain_album_peak = ?, "
                    "availability = 'indexed', missing_since = NULL, excluded_at = NULL, "
                    "ingest_source = ?, download_task_id = ?, source_path = ?, imported_at = ?, "
                    "desired_policy_revision = ?, applied_policy_revision = ?, applied_policy = ?, "
                    "row_revision = row_revision + 1 WHERE id = ?",
                    (
                        local_album_id,
                        track.file_path,
                        track.path_hash,
                        track.file_size_bytes,
                        track.file_mtime_ns,
                        track.stat_revision,
                        track.stat_revision_kind,
                        track.tag_revision,
                        track.tags_read_at,
                        int(track.metadata_incomplete),
                        track.title,
                        _fold(track.title),
                        track.artist_name,
                        _fold(track.artist_name),
                        track.album_title,
                        _fold(track.album_title),
                        track.album_artist_name,
                        _fold(track.album_artist_name),
                        track.tag_album_title,
                        track.tag_album_artist_name,
                        track.disc_number,
                        track.track_number,
                        track.year,
                        track.genre,
                        _fold(track.genre),
                        track.title_sort,
                        track.artist_sort,
                        track.album_sort,
                        track.album_artist_sort,
                        track.disc_subtitle,
                        int(track.is_compilation),
                        track.embedded_release_group_mbid,
                        track.embedded_release_mbid,
                        track.embedded_recording_mbid,
                        track.embedded_artist_mbid,
                        track.embedded_album_artist_mbid,
                        track.duration_seconds,
                        track.file_format,
                        track.bit_rate,
                        track.sample_rate,
                        track.bit_depth,
                        track.channels,
                        track.replaygain_track_gain,
                        track.replaygain_album_gain,
                        track.replaygain_track_peak,
                        track.replaygain_album_peak,
                        track.ingest_source,
                        track.download_task_id,
                        track.source_path,
                        track.imported_at,
                        track.desired_policy_revision,
                        track.applied_policy_revision,
                        track.applied_policy,
                        track_id,
                    ),
                )
            connection.execute(
                "INSERT OR IGNORE INTO local_track_artists "
                "(local_track_id, position, local_artist_id, role, credited_name) "
                "VALUES (?,0,?,?,?)",
                (track_id, artist.id, credit.role, credit.credited_name),
            )
            if scan_run_id is not None and grouping_context is not None:
                connection.execute(
                    "INSERT OR IGNORE INTO library_scan_grouping_contexts "
                    "(run_id, root_id, relative_directory) VALUES (?,?,?)",
                    (scan_run_id, track.root_id, grouping_context),
                )
            return track_id, self._bump_catalog(connection)

        return await self._write(operation)

    async def get_pending_grouping_contexts(
        self, run_id: str, *, limit: int = 64
    ) -> list[dict[str, Any]]:
        def operation(connection: sqlite3.Connection) -> list[dict[str, Any]]:
            rows = connection.execute(
                "SELECT * FROM library_scan_grouping_contexts WHERE run_id = ? "
                "AND state = 'pending' ORDER BY root_id, relative_directory LIMIT ?",
                (run_id, limit),
            ).fetchall()
            return [dict(row) for row in rows]

        return await self._read(operation)

    async def get_grouping_context_tracks(
        self, root_id: str, relative_directory: str
    ) -> list[dict[str, Any]]:
        prefix = (
            "" if relative_directory == "." else f"{relative_directory.rstrip('/')}/"
        )
        upper_bound = f"{prefix}\U0010ffff"

        def operation(connection: sqlite3.Connection) -> list[dict[str, Any]]:
            rows = connection.execute(
                "SELECT t.*, a.created_at AS album_created_at, "
                "a.row_revision AS album_row_revision FROM local_tracks t "
                "JOIN local_albums a ON a.id = t.local_album_id "
                "WHERE t.root_id = ? AND t.relative_path >= ? "
                "AND t.relative_path < ? "
                "AND (length(substr(t.relative_path, length(?) + 1)) - "
                "length(replace(substr(t.relative_path, length(?) + 1), '/', ''))) <= 1 "
                "ORDER BY t.relative_path, t.id",
                (root_id, prefix, upper_bound, prefix, prefix),
            ).fetchall()
            return [dict(row) for row in rows]

        return await self._read(operation)

    async def count_grouping_context_candidates(
        self, root_id: str, relative_directory: str, *, limit: int
    ) -> int:
        prefix = (
            "" if relative_directory == "." else f"{relative_directory.rstrip('/')}/"
        )
        upper_bound = f"{prefix}\U0010ffff"

        def operation(connection: sqlite3.Connection) -> int:
            row = connection.execute(
                "SELECT COUNT(*) AS count FROM (SELECT 1 FROM local_tracks "
                "WHERE root_id = ? AND relative_path >= ? AND relative_path < ? "
                "AND (length(substr(relative_path, length(?) + 1)) - "
                "length(replace(substr(relative_path, length(?) + 1), '/', ''))) <= 1 "
                "LIMIT ?)",
                (root_id, prefix, upper_bound, prefix, prefix, limit),
            ).fetchone()
            return int(row["count"])

        return await self._read(operation)

    async def get_grouping_context_track_page(
        self,
        run_id: str,
        root_id: str,
        relative_directory: str,
        *,
        limit: int,
    ) -> tuple[list[dict[str, Any]], str | None]:
        prefix = (
            "" if relative_directory == "." else f"{relative_directory.rstrip('/')}/"
        )
        upper_bound = f"{prefix}\U0010ffff"

        def operation(
            connection: sqlite3.Connection,
        ) -> tuple[list[dict[str, Any]], str | None]:
            context = connection.execute(
                "SELECT staging_cursor FROM library_scan_grouping_contexts "
                "WHERE run_id=? AND root_id=? AND relative_directory=? "
                "AND state='pending'",
                (run_id, root_id, relative_directory),
            ).fetchone()
            if context is None:
                raise StaleRevisionError("The grouping context is no longer pending.")
            cursor_path = ""
            cursor_id = ""
            if context["staging_cursor"]:
                cursor_path, cursor_id = json.loads(str(context["staging_cursor"]))
            rows = connection.execute(
                "SELECT t.*, a.created_at AS album_created_at FROM local_tracks t "
                "JOIN local_albums a ON a.id=t.local_album_id "
                "WHERE t.root_id=? AND t.relative_path>=? AND t.relative_path<? "
                "AND (length(substr(t.relative_path, length(?) + 1)) - "
                "length(replace(substr(t.relative_path, length(?) + 1), '/', ''))) <= 1 "
                "AND (t.relative_path>? OR (t.relative_path=? AND t.id>?)) "
                "ORDER BY t.relative_path,t.id LIMIT ?",
                (
                    root_id,
                    prefix,
                    upper_bound,
                    prefix,
                    prefix,
                    cursor_path,
                    cursor_path,
                    cursor_id,
                    limit,
                ),
            ).fetchall()
            next_cursor = (
                json.dumps([rows[-1]["relative_path"], rows[-1]["id"]])
                if rows
                else None
            )
            return [dict(row) for row in rows], next_cursor

        return await self._read(operation)

    async def stage_grouping_track_page(
        self,
        run_id: str,
        root_id: str,
        relative_directory: str,
        evidence: list[dict[str, Any]],
        *,
        next_cursor: str | None,
        exhausted: bool,
    ) -> None:
        def operation(connection: sqlite3.Connection) -> None:
            context = connection.execute(
                "SELECT state,staging_state FROM library_scan_grouping_contexts "
                "WHERE run_id=? AND root_id=? AND relative_directory=?",
                (run_id, root_id, relative_directory),
            ).fetchone()
            if context is None or context["state"] != "pending":
                raise StaleRevisionError("The grouping context is no longer pending.")
            if evidence:
                connection.executemany(
                    "INSERT OR REPLACE INTO library_scan_grouping_evidence "
                    "(run_id,root_id,relative_directory,local_track_id,preliminary_key,"
                    "title,title_normalized,album_artist_name,album_artist_normalized,"
                    "track_number,old_album_id,album_created_at,reason_code) "
                    "VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?)",
                    [
                        (
                            run_id,
                            root_id,
                            relative_directory,
                            row["local_track_id"],
                            row["preliminary_key"],
                            row["title"],
                            row["title_normalized"],
                            row["album_artist_name"],
                            row["album_artist_normalized"],
                            row["track_number"],
                            row["old_album_id"],
                            row["album_created_at"],
                            row["reason_code"],
                        )
                        for row in evidence
                    ],
                )
            connection.execute(
                "UPDATE library_scan_grouping_contexts SET staging_cursor=?,"
                "staging_state=?,row_revision=row_revision+1 WHERE run_id=? "
                "AND root_id=? AND relative_directory=?",
                (
                    None if exhausted else next_cursor,
                    "tokens" if exhausted else "tracks",
                    run_id,
                    root_id,
                    relative_directory,
                ),
            )

        await super()._background_write(operation)

    async def prepare_staged_grouping_tokens(
        self,
        run_id: str,
        root_id: str,
        relative_directory: str,
        *,
        limit: int,
    ) -> bool:
        parameters = (run_id, root_id, relative_directory)

        def inspect_merge_target(connection: sqlite3.Connection) -> str | None:
            context = connection.execute(
                "SELECT grouping_merge_target,grouping_merge_ready "
                "FROM library_scan_grouping_contexts "
                "WHERE run_id=? AND root_id=? AND relative_directory=? "
                "AND state='pending'",
                parameters,
            ).fetchone()
            if context is None:
                raise StaleRevisionError("The grouping context is no longer pending.")
            if bool(context["grouping_merge_ready"]):
                return context["grouping_merge_target"]
            tagged = connection.execute(
                "SELECT preliminary_key FROM library_scan_grouping_evidence "
                "WHERE run_id=? AND root_id=? AND relative_directory=? "
                "AND preliminary_key!='missing' "
                "AND preliminary_key NOT LIKE 'manual:%' "
                "GROUP BY preliminary_key ORDER BY preliminary_key LIMIT 2",
                parameters,
            ).fetchall()
            if len(tagged) != 1:
                return None
            candidate = str(tagged[0]["preliminary_key"])
            invalid_missing = connection.execute(
                "SELECT 1 FROM library_scan_grouping_evidence missing "
                "WHERE missing.run_id=? AND missing.root_id=? "
                "AND missing.relative_directory=? "
                "AND missing.preliminary_key='missing' "
                "AND (missing.track_number<=0 OR EXISTS ("
                "SELECT 1 FROM library_scan_grouping_evidence tagged "
                "WHERE tagged.run_id=missing.run_id "
                "AND tagged.root_id=missing.root_id "
                "AND tagged.relative_directory=missing.relative_directory "
                "AND tagged.preliminary_key=? "
                "AND tagged.track_number=missing.track_number "
                "AND tagged.track_number>0)) LIMIT 1",
                (*parameters, candidate),
            ).fetchone()
            return candidate if invalid_missing is None else None

        merge_target = await self._read(inspect_merge_target)

        def operation(connection: sqlite3.Connection) -> int:
            context = connection.execute(
                "SELECT staging_cursor,grouping_merge_target,grouping_merge_ready "
                "FROM library_scan_grouping_contexts "
                "WHERE run_id=? AND root_id=? AND relative_directory=? "
                "AND state='pending'",
                parameters,
            ).fetchone()
            if context is None:
                raise StaleRevisionError("The grouping context is no longer pending.")
            effective_merge_target = context["grouping_merge_target"]
            if not bool(context["grouping_merge_ready"]):
                connection.execute(
                    "UPDATE library_scan_grouping_contexts SET "
                    "grouping_merge_target=?,grouping_merge_ready=1,"
                    "row_revision=row_revision+1 WHERE run_id=? AND root_id=? "
                    "AND relative_directory=?",
                    (merge_target, *parameters),
                )
                effective_merge_target = merge_target
            cursor = str(context["staging_cursor"] or "")
            rows = connection.execute(
                "SELECT evidence.local_track_id,evidence.preliminary_key,evidence.title,"
                "evidence.title_normalized,evidence.album_artist_name,"
                "evidence.album_artist_normalized,evidence.old_album_id,"
                "evidence.reason_code,track.tag_revision,track.stat_revision,"
                "track.applied_policy_revision,track.applied_policy,"
                "track.embedded_release_group_mbid,track.embedded_release_mbid FROM "
                "library_scan_grouping_evidence evidence JOIN local_tracks track "
                "ON track.id=evidence.local_track_id WHERE evidence.run_id=? "
                "AND evidence.root_id=? AND evidence.relative_directory=? "
                "AND evidence.local_track_id>? ORDER BY evidence.local_track_id LIMIT ?",
                (*parameters, cursor, limit),
            ).fetchall()
            if not rows:
                connection.execute(
                    "UPDATE library_scan_grouping_contexts SET staging_state='groups',"
                    "staging_cursor=NULL,row_revision=row_revision+1 WHERE run_id=? "
                    "AND root_id=? AND relative_directory=?",
                    parameters,
                )
                return 0
            assigned: list[tuple[sqlite3.Row, str]] = []
            for row in rows:
                preliminary_key = str(row["preliminary_key"])
                token = (
                    preliminary_key
                    if preliminary_key != "missing"
                    else effective_merge_target or f"untagged:{row['local_track_id']}"
                )
                assigned.append((row, token))
            connection.executemany(
                "UPDATE library_scan_grouping_evidence SET grouping_token=? "
                "WHERE run_id=? AND root_id=? AND relative_directory=? "
                "AND local_track_id=?",
                [
                    (
                        token,
                        run_id,
                        root_id,
                        relative_directory,
                        row["local_track_id"],
                    )
                    for row, token in assigned
                ],
            )
            for row, token in assigned:
                connection.execute(
                    "INSERT OR IGNORE INTO library_scan_grouping_groups "
                    "(run_id,root_id,relative_directory,grouping_token,grouping_key,"
                    "title,album_artist_name,reason_code) VALUES (?,?,?,?,?,?,?,?)",
                    (
                        run_id,
                        root_id,
                        relative_directory,
                        token,
                        "",
                        "",
                        "",
                        "AMBIGUOUS_FALLBACK_GROUP",
                    ),
                )
                values = (
                    (
                        "title",
                        str(row["title_normalized"]),
                        str(row["title"]).strip(),
                    ),
                    (
                        "artist",
                        str(row["album_artist_normalized"]),
                        str(row["album_artist_name"]).strip(),
                    ),
                    (
                        "reason",
                        str(row["reason_code"]),
                        str(row["reason_code"]),
                    ),
                )
                for kind, normalized, display in values:
                    if not normalized or (
                        kind == "reason" and normalized == "MISSING_ALBUM_TAGS"
                    ):
                        continue
                    connection.execute(
                        "INSERT INTO library_scan_grouping_values "
                        "(run_id,root_id,relative_directory,grouping_token,value_kind,"
                        "normalized_value,display_value,occurrence_count) "
                        "VALUES (?,?,?,?,?,?,?,1) ON CONFLICT "
                        "(run_id,root_id,relative_directory,grouping_token,value_kind,"
                        "normalized_value) DO UPDATE SET "
                        "occurrence_count=occurrence_count+1,"
                        "display_value=MIN(display_value,excluded.display_value)",
                        (
                            run_id,
                            root_id,
                            relative_directory,
                            token,
                            kind,
                            normalized,
                            display,
                        ),
                    )

            revision_modulus = 1 << 256
            revision_sums: dict[str, list[int]] = {}
            policy_counts: dict[str, list[int]] = {}
            for row, token in assigned:
                sums = revision_sums.setdefault(token, [0, 0, 0])
                revision_values = (
                    f"{row['local_track_id']}:{row['tag_revision'] or ''}",
                    f"{row['local_track_id']}:{row['stat_revision']}",
                    f"{row['local_track_id']}:{row['applied_policy_revision']}:"
                    f"{row['applied_policy']}",
                )
                for index, value in enumerate(revision_values):
                    sums[index] = (
                        sums[index]
                        + int.from_bytes(hashlib.sha256(value.encode()).digest(), "big")
                    ) % revision_modulus
                counts = policy_counts.setdefault(token, [0, 0, 0, 0])
                policy_index = {
                    "automatic": 0,
                    "local_metadata": 1,
                    "excluded": 2,
                }[str(row["applied_policy"])]
                counts[policy_index] += 1
                counts[3] += int(
                    row["embedded_release_group_mbid"] is not None
                    or row["embedded_release_mbid"] is not None
                )
            for token, sums in revision_sums.items():
                group = connection.execute(
                    "SELECT tag_revision_accumulator,stat_revision_accumulator,"
                    "policy_revision_accumulator FROM library_scan_grouping_groups "
                    "WHERE run_id=? AND root_id=? AND relative_directory=? "
                    "AND grouping_token=?",
                    (run_id, root_id, relative_directory, token),
                ).fetchone()
                accumulated = [
                    (int(str(group[column]), 16) + sums[index]) % revision_modulus
                    for index, column in enumerate(
                        (
                            "tag_revision_accumulator",
                            "stat_revision_accumulator",
                            "policy_revision_accumulator",
                        )
                    )
                ]
                counts = policy_counts[token]
                connection.execute(
                    "UPDATE library_scan_grouping_groups SET "
                    "tag_revision_accumulator=?,stat_revision_accumulator=?,"
                    "policy_revision_accumulator=?,"
                    "automatic_track_count=automatic_track_count+?,"
                    "local_metadata_track_count=local_metadata_track_count+?,"
                    "excluded_track_count=excluded_track_count+?,"
                    "embedded_identity_count=embedded_identity_count+? "
                    "WHERE run_id=? AND root_id=? AND relative_directory=? "
                    "AND grouping_token=?",
                    (
                        *(f"{value:064x}" for value in accumulated),
                        *counts,
                        run_id,
                        root_id,
                        relative_directory,
                        token,
                    ),
                )

            overlaps: dict[tuple[str, str], int] = {}
            for row, token in assigned:
                key = (str(row["old_album_id"]), token)
                overlaps[key] = overlaps.get(key, 0) + 1
            for (old_album_id, token), overlap_count in overlaps.items():
                total = int(
                    connection.execute(
                        "INSERT INTO library_scan_grouping_edges "
                        "(run_id,root_id,relative_directory,old_album_id,grouping_token,"
                        "overlap_count) VALUES (?,?,?,?,?,?) ON CONFLICT "
                        "(run_id,root_id,relative_directory,old_album_id,grouping_token) "
                        "DO UPDATE SET overlap_count=overlap_count+excluded.overlap_count "
                        "RETURNING overlap_count",
                        (
                            run_id,
                            root_id,
                            relative_directory,
                            old_album_id,
                            token,
                            overlap_count,
                        ),
                    ).fetchone()[0]
                )
                if total != overlap_count:
                    continue
                connection.execute(
                    "INSERT INTO library_scan_grouping_old_nodes "
                    "(run_id,root_id,relative_directory,old_album_id,degree) "
                    "VALUES (?,?,?,?,1) ON CONFLICT "
                    "(run_id,root_id,relative_directory,old_album_id) "
                    "DO UPDATE SET degree=degree+1",
                    (run_id, root_id, relative_directory, old_album_id),
                )
                connection.execute(
                    "INSERT INTO library_scan_grouping_new_nodes "
                    "(run_id,root_id,relative_directory,grouping_token,degree) "
                    "VALUES (?,?,?,?,1) ON CONFLICT "
                    "(run_id,root_id,relative_directory,grouping_token) "
                    "DO UPDATE SET degree=degree+1",
                    (run_id, root_id, relative_directory, token),
                )
            connection.execute(
                "UPDATE library_scan_grouping_contexts SET staging_cursor=?,"
                "row_revision=row_revision+1 WHERE run_id=? AND root_id=? "
                "AND relative_directory=?",
                (rows[-1]["local_track_id"], *parameters),
            )
            return len(rows)

        return await super()._background_write(operation) == 0

    async def stage_grouping_summary_page(
        self,
        run_id: str,
        root_id: str,
        relative_directory: str,
        *,
        limit: int,
    ) -> bool:
        def operation(connection: sqlite3.Connection) -> bool:
            context = connection.execute(
                "SELECT staging_cursor FROM library_scan_grouping_contexts "
                "WHERE run_id=? AND root_id=? AND relative_directory=? "
                "AND state='pending'",
                (run_id, root_id, relative_directory),
            ).fetchone()
            if context is None:
                raise StaleRevisionError("The grouping context is no longer pending.")
            cursor = str(context["staging_cursor"] or "")
            tokens = connection.execute(
                "SELECT grouping_token FROM library_scan_grouping_groups "
                "WHERE run_id=? AND root_id=? AND relative_directory=? "
                "AND grouping_token>? ORDER BY grouping_token LIMIT ?",
                (run_id, root_id, relative_directory, cursor, limit),
            ).fetchall()
            if not tokens:
                connection.execute(
                    "UPDATE library_scan_grouping_contexts SET staging_state='continuity',"
                    "staging_cursor=NULL,row_revision=row_revision+1 WHERE run_id=? "
                    "AND root_id=? AND relative_directory=?",
                    (run_id, root_id, relative_directory),
                )
                return True
            token_values = [str(row["grouping_token"]) for row in tokens]
            fallback_title = (
                relative_directory.rstrip("/").rsplit("/", 1)[-1]
                if relative_directory != "."
                else "Unknown Album"
            )
            for token in token_values:
                title_row = connection.execute(
                    "SELECT display_value FROM library_scan_grouping_values "
                    "WHERE run_id=? AND root_id=? AND relative_directory=? "
                    "AND grouping_token=? AND value_kind='title' "
                    "ORDER BY occurrence_count DESC,normalized_value LIMIT 1",
                    (run_id, root_id, relative_directory, token),
                ).fetchone()
                artist_row = connection.execute(
                    "SELECT display_value FROM library_scan_grouping_values "
                    "WHERE run_id=? AND root_id=? AND relative_directory=? "
                    "AND grouping_token=? AND value_kind='artist' "
                    "ORDER BY occurrence_count DESC,normalized_value LIMIT 1",
                    (run_id, root_id, relative_directory, token),
                ).fetchone()
                reason_row = connection.execute(
                    "SELECT display_value FROM library_scan_grouping_values "
                    "WHERE run_id=? AND root_id=? AND relative_directory=? "
                    "AND grouping_token=? AND value_kind='reason' "
                    "ORDER BY normalized_value LIMIT 1",
                    (run_id, root_id, relative_directory, token),
                ).fetchone()
                title = str(title_row["display_value"] if title_row else fallback_title)
                artist = str(
                    artist_row["display_value"] if artist_row else "Unknown Artist"
                )
                directory = (
                    token.removeprefix("manual:")
                    if token.startswith("manual:")
                    else relative_directory
                )
                base_key = (
                    f"{root_id}:{directory}:{_normalize_exact(title)}:"
                    f"{_normalize_exact(artist)}"
                )
                grouping_key = (
                    f"{base_key}:{token.removeprefix('untagged:')}"
                    if token.startswith("untagged:")
                    else base_key
                )
                connection.execute(
                    "UPDATE library_scan_grouping_groups SET grouping_key=?,title=?,"
                    "album_artist_name=?,reason_code=? WHERE run_id=? AND root_id=? "
                    "AND relative_directory=? AND grouping_token=?",
                    (
                        grouping_key,
                        title,
                        artist,
                        str(
                            reason_row["display_value"]
                            if reason_row
                            else "AMBIGUOUS_FALLBACK_GROUP"
                        ),
                        run_id,
                        root_id,
                        relative_directory,
                        token,
                    ),
                )
            connection.execute(
                "UPDATE library_scan_grouping_contexts SET staging_cursor=?,"
                "row_revision=row_revision+1 WHERE run_id=? AND root_id=? "
                "AND relative_directory=?",
                (token_values[-1], run_id, root_id, relative_directory),
            )
            return False

        return await super()._background_write(operation)

    async def get_grouping_staging_state(
        self, run_id: str, root_id: str, relative_directory: str
    ) -> str:
        def operation(connection: sqlite3.Connection) -> str:
            row = connection.execute(
                "SELECT staging_state FROM library_scan_grouping_contexts "
                "WHERE run_id=? AND root_id=? AND relative_directory=? "
                "AND state='pending'",
                (run_id, root_id, relative_directory),
            ).fetchone()
            if row is None:
                raise StaleRevisionError("The grouping context is no longer pending.")
            return str(row["staging_state"])

        return await self._read(operation)

    async def assign_isolated_grouping_continuity(
        self,
        run_id: str,
        root_id: str,
        relative_directory: str,
        *,
        limit: int,
    ) -> int:
        def operation(connection: sqlite3.Connection) -> int:
            rows = connection.execute(
                "SELECT edge.old_album_id,edge.grouping_token FROM "
                "library_scan_grouping_edges edge JOIN library_scan_grouping_old_nodes "
                "old_node USING(run_id,root_id,relative_directory,old_album_id) "
                "JOIN library_scan_grouping_new_nodes new_node "
                "USING(run_id,root_id,relative_directory,grouping_token) "
                "WHERE edge.run_id=? "
                "AND edge.root_id=? AND edge.relative_directory=? AND edge.processed=0 "
                "AND old_node.degree=1 AND new_node.degree=1 "
                "ORDER BY edge.old_album_id,edge.grouping_token LIMIT ?",
                (
                    run_id,
                    root_id,
                    relative_directory,
                    limit,
                ),
            ).fetchall()
            for row in rows:
                connection.execute(
                    "UPDATE library_scan_grouping_groups SET retained_album_id=?,"
                    "continuity_reason_code='MAXIMUM_TRACK_OVERLAP' WHERE run_id=? "
                    "AND root_id=? AND relative_directory=? AND grouping_token=?",
                    (
                        row["old_album_id"],
                        run_id,
                        root_id,
                        relative_directory,
                        row["grouping_token"],
                    ),
                )
                connection.execute(
                    "UPDATE library_scan_grouping_edges SET processed=1 WHERE run_id=? "
                    "AND root_id=? AND relative_directory=? AND old_album_id=? "
                    "AND grouping_token=?",
                    (
                        run_id,
                        root_id,
                        relative_directory,
                        row["old_album_id"],
                        row["grouping_token"],
                    ),
                )
            return len(rows)

        return await super()._background_write(operation)

    async def get_next_grouping_edge(
        self, run_id: str, root_id: str, relative_directory: str
    ) -> dict[str, Any] | None:
        def operation(connection: sqlite3.Connection) -> dict[str, Any] | None:
            return _row(
                connection.execute(
                    "SELECT edges.old_album_id,edges.grouping_token,edges.overlap_count,"
                    "albums.created_at AS album_created_at,groups.grouping_key FROM "
                    "library_scan_grouping_edges edges JOIN local_albums albums "
                    "ON albums.id=edges.old_album_id JOIN library_scan_grouping_groups groups "
                    "USING(run_id,root_id,relative_directory,grouping_token) JOIN "
                    "library_scan_grouping_old_nodes old_node "
                    "USING(run_id,root_id,relative_directory,old_album_id) JOIN "
                    "library_scan_grouping_new_nodes new_node "
                    "USING(run_id,root_id,relative_directory,grouping_token) "
                    "WHERE edges.run_id=? AND edges.root_id=? "
                    "AND edges.relative_directory=? AND edges.processed=0 "
                    "AND old_node.matched_grouping_token IS NULL "
                    "AND new_node.matched_old_album_id IS NULL "
                    "ORDER BY edges.old_album_id,edges.grouping_token LIMIT 1",
                    (run_id, root_id, relative_directory),
                ).fetchone()
            )

        return await self._read(operation)

    async def get_grouping_edges_for_old_albums(
        self,
        run_id: str,
        root_id: str,
        relative_directory: str,
        old_album_ids: list[str],
        *,
        limit: int,
    ) -> list[dict[str, Any]]:
        if not old_album_ids:
            return []

        def operation(connection: sqlite3.Connection) -> list[dict[str, Any]]:
            placeholders = ",".join("?" for _ in old_album_ids)
            rows = connection.execute(
                "SELECT edges.old_album_id,edges.grouping_token,edges.overlap_count,"
                "albums.created_at AS album_created_at,groups.grouping_key FROM "
                "library_scan_grouping_edges edges JOIN local_albums albums "
                "ON albums.id=edges.old_album_id JOIN library_scan_grouping_groups groups "
                "USING(run_id,root_id,relative_directory,grouping_token) "
                "WHERE edges.run_id=? AND edges.root_id=? "
                "AND edges.relative_directory=? AND edges.processed=0 "
                f"AND edges.old_album_id IN ({placeholders}) "
                "ORDER BY edges.old_album_id,edges.grouping_token LIMIT ?",
                (run_id, root_id, relative_directory, *old_album_ids, limit),
            ).fetchall()
            return [dict(row) for row in rows]

        return await self._read(operation)

    async def get_grouping_edges_for_tokens(
        self,
        run_id: str,
        root_id: str,
        relative_directory: str,
        grouping_tokens: list[str],
        *,
        limit: int,
    ) -> list[dict[str, Any]]:
        if not grouping_tokens:
            return []

        def operation(connection: sqlite3.Connection) -> list[dict[str, Any]]:
            placeholders = ",".join("?" for _ in grouping_tokens)
            rows = connection.execute(
                "SELECT edges.old_album_id,edges.grouping_token,edges.overlap_count,"
                "albums.created_at AS album_created_at,groups.grouping_key FROM "
                "library_scan_grouping_edges edges JOIN local_albums albums "
                "ON albums.id=edges.old_album_id JOIN library_scan_grouping_groups groups "
                "USING(run_id,root_id,relative_directory,grouping_token) "
                "WHERE edges.run_id=? AND edges.root_id=? "
                "AND edges.relative_directory=? AND edges.processed=0 "
                f"AND edges.grouping_token IN ({placeholders}) "
                "ORDER BY edges.grouping_token,edges.old_album_id LIMIT ?",
                (run_id, root_id, relative_directory, *grouping_tokens, limit),
            ).fetchall()
            return [dict(row) for row in rows]

        return await self._read(operation)

    async def discard_resolved_grouping_edges(
        self,
        run_id: str,
        root_id: str,
        relative_directory: str,
        *,
        limit: int,
    ) -> int:
        def operation(connection: sqlite3.Connection) -> int:
            rows = connection.execute(
                "SELECT edges.old_album_id,edges.grouping_token FROM "
                "library_scan_grouping_edges edges JOIN "
                "library_scan_grouping_old_nodes old_node "
                "USING(run_id,root_id,relative_directory,old_album_id) JOIN "
                "library_scan_grouping_new_nodes new_node "
                "USING(run_id,root_id,relative_directory,grouping_token) "
                "WHERE edges.run_id=? AND edges.root_id=? "
                "AND edges.relative_directory=? AND edges.processed=0 "
                "AND (old_node.matched_grouping_token IS NOT NULL "
                "OR new_node.matched_old_album_id IS NOT NULL) "
                "ORDER BY edges.old_album_id,edges.grouping_token LIMIT ?",
                (run_id, root_id, relative_directory, limit),
            ).fetchall()
            connection.executemany(
                "UPDATE library_scan_grouping_edges SET processed=1 WHERE run_id=? "
                "AND root_id=? AND relative_directory=? AND old_album_id=? "
                "AND grouping_token=?",
                [
                    (
                        run_id,
                        root_id,
                        relative_directory,
                        row["old_album_id"],
                        row["grouping_token"],
                    )
                    for row in rows
                ],
            )
            return len(rows)

        return await super()._background_write(operation)

    async def apply_next_sparse_grouping_continuity(
        self, run_id: str, root_id: str, relative_directory: str
    ) -> bool:
        def operation(connection: sqlite3.Connection) -> bool:
            edge = connection.execute(
                "SELECT edges.old_album_id,edges.grouping_token,edges.overlap_count "
                "FROM library_scan_grouping_edges edges JOIN "
                "library_scan_grouping_old_nodes old_node "
                "USING(run_id,root_id,relative_directory,old_album_id) JOIN "
                "library_scan_grouping_new_nodes new_node "
                "USING(run_id,root_id,relative_directory,grouping_token) JOIN "
                "local_albums albums ON albums.id=edges.old_album_id JOIN "
                "library_scan_grouping_groups groups "
                "USING(run_id,root_id,relative_directory,grouping_token) "
                "WHERE edges.run_id=? AND edges.root_id=? "
                "AND edges.relative_directory=? AND edges.processed=0 "
                "AND old_node.matched_grouping_token IS NULL "
                "AND new_node.matched_old_album_id IS NULL "
                "ORDER BY edges.overlap_count DESC,albums.created_at,"
                "edges.old_album_id,groups.grouping_key LIMIT 1",
                (run_id, root_id, relative_directory),
            ).fetchone()
            if edge is None:
                return False
            old_album_id = str(edge["old_album_id"])
            grouping_token = str(edge["grouping_token"])
            overlap_count = int(edge["overlap_count"])
            old_ties = int(
                connection.execute(
                    "SELECT COUNT(*) FROM library_scan_grouping_edges edges JOIN "
                    "library_scan_grouping_new_nodes node "
                    "USING(run_id,root_id,relative_directory,grouping_token) "
                    "WHERE edges.run_id=? AND edges.root_id=? "
                    "AND edges.relative_directory=? AND edges.old_album_id=? "
                    "AND edges.processed=0 AND node.matched_old_album_id IS NULL "
                    "AND edges.overlap_count=?",
                    (
                        run_id,
                        root_id,
                        relative_directory,
                        old_album_id,
                        overlap_count,
                    ),
                ).fetchone()[0]
            )
            new_ties = int(
                connection.execute(
                    "SELECT COUNT(*) FROM library_scan_grouping_edges edges JOIN "
                    "library_scan_grouping_old_nodes node "
                    "USING(run_id,root_id,relative_directory,old_album_id) "
                    "WHERE edges.run_id=? AND edges.root_id=? "
                    "AND edges.relative_directory=? AND edges.grouping_token=? "
                    "AND edges.processed=0 AND node.matched_grouping_token IS NULL "
                    "AND edges.overlap_count=?",
                    (
                        run_id,
                        root_id,
                        relative_directory,
                        grouping_token,
                        overlap_count,
                    ),
                ).fetchone()[0]
            )
            reason = (
                "CONTINUITY_TIE_BROKEN"
                if old_ties > 1 or new_ties > 1
                else "MAXIMUM_TRACK_OVERLAP"
            )
            connection.execute(
                "UPDATE library_scan_grouping_old_nodes SET matched_grouping_token=? "
                "WHERE run_id=? AND root_id=? AND relative_directory=? "
                "AND old_album_id=? AND matched_grouping_token IS NULL",
                (
                    grouping_token,
                    run_id,
                    root_id,
                    relative_directory,
                    old_album_id,
                ),
            )
            connection.execute(
                "UPDATE library_scan_grouping_new_nodes SET matched_old_album_id=? "
                "WHERE run_id=? AND root_id=? AND relative_directory=? "
                "AND grouping_token=? AND matched_old_album_id IS NULL",
                (
                    old_album_id,
                    run_id,
                    root_id,
                    relative_directory,
                    grouping_token,
                ),
            )
            connection.execute(
                "UPDATE library_scan_grouping_groups SET retained_album_id=?,"
                "continuity_reason_code=? WHERE run_id=? AND root_id=? "
                "AND relative_directory=? AND grouping_token=?",
                (
                    old_album_id,
                    reason,
                    run_id,
                    root_id,
                    relative_directory,
                    grouping_token,
                ),
            )
            connection.execute(
                "UPDATE library_scan_grouping_edges SET processed=1 WHERE run_id=? "
                "AND root_id=? AND relative_directory=? AND old_album_id=? "
                "AND grouping_token=?",
                (
                    run_id,
                    root_id,
                    relative_directory,
                    old_album_id,
                    grouping_token,
                ),
            )
            return True

        return await super()._background_write(operation)

    async def apply_grouping_continuity_component(
        self,
        run_id: str,
        root_id: str,
        relative_directory: str,
        edges: list[dict[str, Any]],
        assignments: dict[str, tuple[str, str]],
    ) -> None:
        def operation(connection: sqlite3.Connection) -> None:
            for token, (album_id, reason) in assignments.items():
                connection.execute(
                    "UPDATE library_scan_grouping_groups SET retained_album_id=?,"
                    "continuity_reason_code=? WHERE run_id=? AND root_id=? "
                    "AND relative_directory=? AND grouping_token=?",
                    (
                        album_id,
                        reason,
                        run_id,
                        root_id,
                        relative_directory,
                        token,
                    ),
                )
            connection.executemany(
                "UPDATE library_scan_grouping_edges SET processed=1 WHERE run_id=? "
                "AND root_id=? AND relative_directory=? AND old_album_id=? "
                "AND grouping_token=?",
                [
                    (
                        run_id,
                        root_id,
                        relative_directory,
                        edge["old_album_id"],
                        edge["grouping_token"],
                    )
                    for edge in edges
                ],
            )

        await super()._background_write(operation)

    async def finish_grouping_continuity(
        self, run_id: str, root_id: str, relative_directory: str
    ) -> None:
        def operation(connection: sqlite3.Connection) -> None:
            connection.execute(
                "UPDATE library_scan_grouping_contexts SET staging_state='albums',"
                "staging_cursor=NULL,row_revision=row_revision+1 WHERE run_id=? "
                "AND root_id=? AND relative_directory=? AND state='pending'",
                (run_id, root_id, relative_directory),
            )

        await super()._background_write(operation)

    async def get_unprovisioned_grouping_groups(
        self,
        run_id: str,
        root_id: str,
        relative_directory: str,
        *,
        limit: int,
    ) -> list[dict[str, Any]]:
        def operation(connection: sqlite3.Connection) -> list[dict[str, Any]]:
            rows = connection.execute(
                "SELECT * FROM library_scan_grouping_groups WHERE run_id=? "
                "AND root_id=? AND relative_directory=? AND local_album_id IS NULL "
                "ORDER BY grouping_token LIMIT ?",
                (run_id, root_id, relative_directory, limit),
            ).fetchall()
            return [dict(row) for row in rows]

        return await self._read(operation)

    async def provision_staged_grouping_groups(
        self,
        run_id: str,
        root_id: str,
        relative_directory: str,
        groups: list[dict[str, Any]],
        *,
        now: float,
    ) -> None:
        def operation(connection: sqlite3.Connection) -> int:
            changed = False
            for group in groups:
                album_id = str(group["local_album_id"])
                artist_id = str(group["local_artist_id"])
                existing = connection.execute(
                    "SELECT * FROM local_albums WHERE id=?", (album_id,)
                ).fetchone()
                if existing is None:
                    connection.execute(
                        "INSERT INTO local_albums (id,root_id,grouping_key,title,"
                        "title_folded,album_artist_name,album_artist_name_folded,"
                        "album_artist_id,grouping_source,created_at,updated_at) "
                        "VALUES (?,?,?,?,?,?,?,?,'automatic',?,?)",
                        (
                            album_id,
                            root_id,
                            group["grouping_key"],
                            group["title"],
                            _fold(group["title"]),
                            group["album_artist_name"],
                            _fold(group["album_artist_name"]),
                            artist_id,
                            now,
                            now,
                        ),
                    )
                    changed = True
                elif not bool(existing["grouping_locked"]) and (
                    existing["grouping_key"] != group["grouping_key"]
                    or existing["title"] != group["title"]
                    or existing["album_artist_name"] != group["album_artist_name"]
                    or existing["album_artist_id"] != artist_id
                    or existing["retired_into_album_id"] is not None
                ):
                    connection.execute(
                        "UPDATE local_albums SET grouping_key=?,title=?,title_folded=?,"
                        "album_artist_name=?,album_artist_name_folded=?,album_artist_id=?,"
                        "retired_into_album_id=NULL,updated_at=?,row_revision=row_revision+1 "
                        "WHERE id=?",
                        (
                            group["grouping_key"],
                            group["title"],
                            _fold(group["title"]),
                            group["album_artist_name"],
                            _fold(group["album_artist_name"]),
                            artist_id,
                            now,
                            album_id,
                        ),
                    )
                    changed = True
                if group["reason_code"] != "MANUAL_MEMBERSHIP_RESTORED":
                    credit = connection.execute(
                        "SELECT local_artist_id,credited_name FROM local_album_artists "
                        "WHERE local_album_id=? AND position=0",
                        (album_id,),
                    ).fetchone()
                    if (
                        credit is None
                        or credit["local_artist_id"] != artist_id
                        or (credit["credited_name"] != group["album_artist_name"])
                    ):
                        connection.execute(
                            "INSERT INTO local_album_artists "
                            "(local_album_id,position,local_artist_id,role,credited_name) "
                            "VALUES (?,0,?,'primary',?) ON CONFLICT(local_album_id,position) "
                            "DO UPDATE SET local_artist_id=excluded.local_artist_id,"
                            "credited_name=excluded.credited_name,"
                            "row_revision=local_album_artists.row_revision+1",
                            (album_id, artist_id, group["album_artist_name"]),
                        )
                        changed = True
                connection.execute(
                    "UPDATE library_scan_grouping_groups SET local_album_id=?,"
                    "local_artist_id=? WHERE run_id=? AND root_id=? "
                    "AND relative_directory=? AND grouping_token=?",
                    (
                        album_id,
                        artist_id,
                        run_id,
                        root_id,
                        relative_directory,
                        group["grouping_token"],
                    ),
                )
            if changed:
                return self._bump_catalog(connection)
            return int(
                connection.execute(
                    "SELECT value FROM library_catalog_revision WHERE singleton=1"
                ).fetchone()[0]
            )

        await self._write_scan(operation)

    async def finish_grouping_album_provisioning(
        self, run_id: str, root_id: str, relative_directory: str
    ) -> None:
        def operation(connection: sqlite3.Connection) -> None:
            connection.execute(
                "UPDATE library_scan_grouping_contexts SET staging_state='memberships',"
                "application_cursor=NULL,row_revision=row_revision+1 WHERE run_id=? "
                "AND root_id=? AND relative_directory=? AND state='pending'",
                (run_id, root_id, relative_directory),
            )

        await super()._background_write(operation)

    async def apply_staged_grouping_membership_page(
        self,
        run_id: str,
        root_id: str,
        relative_directory: str,
        *,
        limit: int,
    ) -> bool:
        def operation(connection: sqlite3.Connection) -> int:
            context = connection.execute(
                "SELECT application_cursor FROM library_scan_grouping_contexts "
                "WHERE run_id=? AND root_id=? AND relative_directory=? "
                "AND state='pending'",
                (run_id, root_id, relative_directory),
            ).fetchone()
            if context is None:
                raise StaleRevisionError("The grouping context is no longer pending.")
            cursor = str(context["application_cursor"] or "")
            rows = connection.execute(
                "SELECT evidence.local_track_id,groups.local_album_id FROM "
                "library_scan_grouping_evidence evidence JOIN "
                "library_scan_grouping_groups groups USING "
                "(run_id,root_id,relative_directory,grouping_token) "
                "WHERE evidence.run_id=? AND evidence.root_id=? "
                "AND evidence.relative_directory=? AND evidence.local_track_id>? "
                "ORDER BY evidence.local_track_id LIMIT ?",
                (run_id, root_id, relative_directory, cursor, limit),
            ).fetchall()
            if not rows:
                connection.execute(
                    "UPDATE library_scan_grouping_contexts SET "
                    "staging_state='retirement',application_cursor=NULL,"
                    "row_revision=row_revision+1 WHERE run_id=? AND root_id=? "
                    "AND relative_directory=?",
                    (run_id, root_id, relative_directory),
                )
                return 0
            changed = False
            for row in rows:
                result = connection.execute(
                    "UPDATE local_tracks SET local_album_id=?,"
                    "membership_source='automatic',row_revision=row_revision+1 "
                    "WHERE id=? AND membership_locked=0 AND "
                    "(local_album_id!=? OR membership_source!='automatic')",
                    (
                        row["local_album_id"],
                        row["local_track_id"],
                        row["local_album_id"],
                    ),
                )
                changed = changed or result.rowcount > 0
            connection.execute(
                "UPDATE library_scan_grouping_contexts SET application_cursor=?,"
                "row_revision=row_revision+1 WHERE run_id=? AND root_id=? "
                "AND relative_directory=?",
                (rows[-1]["local_track_id"], run_id, root_id, relative_directory),
            )
            if changed:
                self._bump_catalog(connection)
            return len(rows)

        return await self._write_scan(operation) == 0

    async def retire_staged_grouping_albums(
        self,
        run_id: str,
        root_id: str,
        relative_directory: str,
        *,
        now: float,
        limit: int,
    ) -> bool:
        def operation(connection: sqlite3.Connection) -> int:
            context = connection.execute(
                "SELECT application_cursor FROM library_scan_grouping_contexts "
                "WHERE run_id=? AND root_id=? AND relative_directory=? "
                "AND state='pending'",
                (run_id, root_id, relative_directory),
            ).fetchone()
            if context is None:
                raise StaleRevisionError("The grouping context is no longer pending.")
            cursor = str(context["application_cursor"] or "")
            albums = connection.execute(
                "SELECT DISTINCT old_album_id FROM library_scan_grouping_evidence "
                "WHERE run_id=? AND root_id=? AND relative_directory=? "
                "AND old_album_id>? ORDER BY old_album_id LIMIT ?",
                (run_id, root_id, relative_directory, cursor, limit),
            ).fetchall()
            if not albums:
                connection.execute(
                    "UPDATE library_scan_grouping_contexts SET staging_state='queue',"
                    "application_cursor=NULL,queue_cursor=NULL,"
                    "row_revision=row_revision+1 WHERE run_id=? AND root_id=? "
                    "AND relative_directory=?",
                    (run_id, root_id, relative_directory),
                )
                return 0
            changed = False
            for row in albums:
                old_album_id = str(row["old_album_id"])
                remaining = connection.execute(
                    "SELECT 1 FROM local_tracks WHERE local_album_id=? LIMIT 1",
                    (old_album_id,),
                ).fetchone()
                if remaining is not None:
                    continue
                successors = connection.execute(
                    "SELECT groups.local_album_id,edges.overlap_count FROM "
                    "library_scan_grouping_edges edges JOIN "
                    "library_scan_grouping_groups groups USING "
                    "(run_id,root_id,relative_directory,grouping_token) "
                    "WHERE edges.run_id=? AND edges.root_id=? "
                    "AND edges.relative_directory=? AND edges.old_album_id=? "
                    "ORDER BY edges.overlap_count DESC,groups.local_album_id",
                    (run_id, root_id, relative_directory, old_album_id),
                ).fetchall()
                successor: str | None = None
                if successors and (
                    len(successors) == 1
                    or successors[0]["overlap_count"] > successors[1]["overlap_count"]
                ):
                    successor = str(successors[0]["local_album_id"])
                connection.execute(
                    "UPDATE local_albums SET retired_into_album_id=?,updated_at=?,"
                    "row_revision=row_revision+1 WHERE id=?",
                    (successor, now, old_album_id),
                )
                if successor is not None:
                    connection.execute(
                        "INSERT OR IGNORE INTO local_album_aliases "
                        "(alias,local_album_id,kind,created_at) "
                        "VALUES (?,?,'merged_album',?)",
                        (old_album_id, successor, now),
                    )
                changed = True
            connection.execute(
                "UPDATE library_scan_grouping_contexts SET application_cursor=?,"
                "row_revision=row_revision+1 WHERE run_id=? AND root_id=? "
                "AND relative_directory=?",
                (albums[-1]["old_album_id"], run_id, root_id, relative_directory),
            )
            if changed:
                self._bump_catalog(connection)
            return len(albums)

        return await self._write_scan(operation) == 0

    async def get_staged_grouping_queue_page(
        self,
        run_id: str,
        root_id: str,
        relative_directory: str,
        *,
        limit: int,
    ) -> list[dict[str, Any]]:
        def operation(connection: sqlite3.Connection) -> list[dict[str, Any]]:
            context = connection.execute(
                "SELECT queue_cursor FROM library_scan_grouping_contexts WHERE "
                "run_id=? AND root_id=? AND relative_directory=? AND state='pending'",
                (run_id, root_id, relative_directory),
            ).fetchone()
            if context is None:
                raise StaleRevisionError("The grouping context is no longer pending.")
            rows = connection.execute(
                "SELECT groups.grouping_token,groups.local_album_id,"
                "groups.tag_revision_accumulator,groups.stat_revision_accumulator,"
                "groups.policy_revision_accumulator,groups.automatic_track_count,"
                "groups.local_metadata_track_count,groups.excluded_track_count,"
                "groups.embedded_identity_count FROM library_scan_grouping_groups groups "
                "WHERE groups.run_id=? "
                "AND groups.root_id=? AND groups.relative_directory=? "
                "AND groups.grouping_token>? ORDER BY groups.grouping_token LIMIT ?",
                (
                    run_id,
                    root_id,
                    relative_directory,
                    str(context["queue_cursor"] or ""),
                    limit,
                ),
            ).fetchall()
            result: list[dict[str, Any]] = []
            for row in rows:
                automatic = int(row["automatic_track_count"])
                local_metadata = int(row["local_metadata_track_count"])
                excluded = int(row["excluded_track_count"])
                if (automatic > 0 and local_metadata == 0 and excluded == 0) or (
                    local_metadata > 0
                    and automatic == 0
                    and excluded == 0
                    and int(row["embedded_identity_count"]) > 0
                ):
                    result.append(
                        {
                            "grouping_token": str(row["grouping_token"]),
                            "local_album_id": str(row["local_album_id"]),
                            "input_revision": ":".join(
                                str(row[column])
                                for column in (
                                    "tag_revision_accumulator",
                                    "stat_revision_accumulator",
                                    "policy_revision_accumulator",
                                )
                            ),
                            "trigger": (
                                "automatic" if automatic > 0 else "post_processing"
                            ),
                        }
                    )
                else:
                    result.append(
                        {
                            "grouping_token": str(row["grouping_token"]),
                            "local_album_id": None,
                        }
                    )
            return result

        return await self._read(operation)

    async def advance_staged_grouping_queue(
        self,
        run_id: str,
        root_id: str,
        relative_directory: str,
        grouping_token: str,
    ) -> None:
        def operation(connection: sqlite3.Connection) -> None:
            connection.execute(
                "UPDATE library_scan_grouping_contexts SET queue_cursor=?,"
                "row_revision=row_revision+1 WHERE run_id=? AND root_id=? "
                "AND relative_directory=? AND state='pending'",
                (grouping_token, run_id, root_id, relative_directory),
            )

        await super()._background_write(operation)

    async def apply_grouping_context(
        self,
        run_id: str,
        root_id: str,
        relative_directory: str,
        applications: list[GroupingApplication],
        *,
        now: float,
    ) -> tuple[list[str], int]:
        """Apply a full-directory proposal through bounded catalog transactions."""
        all_track_ids = [
            track_id
            for application in applications
            for track_id in application.group.track_ids
        ]
        if len(all_track_ids) != len(set(all_track_ids)):
            raise ValueError("A track cannot be assigned to two local albums.")
        target_ids = [application.local_album_id for application in applications]
        if len(target_ids) != len(set(target_ids)):
            raise ValueError("A local album cannot receive two grouping applications.")
        target_id_set = set(target_ids)
        track_to_target = {
            track_id: application.local_album_id
            for application in applications
            for track_id in application.group.track_ids
        }

        def snapshot(connection: sqlite3.Connection) -> dict[str, set[str]]:
            context = connection.execute(
                "SELECT state FROM library_scan_grouping_contexts WHERE run_id = ? "
                "AND root_id = ? AND relative_directory = ?",
                (run_id, root_id, relative_directory),
            ).fetchone()
            if context is None or context["state"] != "pending":
                raise StaleRevisionError("The grouping context is no longer pending.")
            membership: dict[str, set[str]] = {}
            for offset in range(0, len(all_track_ids), 500):
                track_ids = all_track_ids[offset : offset + 500]
                placeholders = ",".join("?" for _ in track_ids)
                rows = connection.execute(
                    "SELECT id, local_album_id FROM local_tracks "
                    f"WHERE id IN ({placeholders})",
                    tuple(track_ids),
                ).fetchall()
                for row in rows:
                    membership.setdefault(str(row["local_album_id"]), set()).add(
                        str(row["id"])
                    )
            return membership

        old_membership = await self._read(snapshot)
        retirement_plan: dict[str, str | None] = {}
        for old_album_id, old_tracks in old_membership.items():
            if old_album_id in target_id_set:
                continue
            counts: dict[str, int] = {}
            for track_id in old_tracks:
                if target_id := track_to_target.get(track_id):
                    counts[target_id] = counts.get(target_id, 0) + 1
            best = max(counts.values(), default=0)
            successors = sorted(
                target_id
                for target_id, score in counts.items()
                if score == best and score
            )
            retirement_plan[old_album_id] = (
                successors[0] if len(successors) == 1 else None
            )

        revision = await self.get_catalog_revision()
        for offset in range(0, len(applications), 500):
            batch = applications[offset : offset + 500]
            revision = await self._apply_grouping_batch(
                run_id,
                root_id,
                relative_directory,
                batch,
                target_id_set=target_id_set,
                retirement_plan=retirement_plan,
                now=now,
            )
        return target_ids, revision

    async def complete_grouping_context(
        self, run_id: str, root_id: str, relative_directory: str
    ) -> None:
        def operation(connection: sqlite3.Connection) -> None:
            updated = connection.execute(
                "UPDATE library_scan_grouping_contexts SET state = 'completed', "
                "staging_state = 'completed', "
                "row_revision = row_revision + 1 WHERE run_id = ? AND root_id = ? "
                "AND relative_directory = ? AND state = 'pending'",
                (run_id, root_id, relative_directory),
            ).rowcount
            if updated != 1:
                raise StaleRevisionError("The grouping context is no longer pending.")

        await super()._background_write(operation)

    async def _apply_grouping_batch(
        self,
        run_id: str,
        root_id: str,
        relative_directory: str,
        applications: list[GroupingApplication],
        *,
        target_id_set: set[str],
        retirement_plan: dict[str, str | None],
        now: float,
    ) -> int:
        def operation(connection: sqlite3.Connection) -> int:
            context = connection.execute(
                "SELECT state FROM library_scan_grouping_contexts WHERE run_id = ? "
                "AND root_id = ? AND relative_directory = ?",
                (run_id, root_id, relative_directory),
            ).fetchone()
            if context is None or context["state"] != "pending":
                raise StaleRevisionError("The grouping context is no longer pending.")
            track_ids = [
                track_id
                for application in applications
                for track_id in application.group.track_ids
            ]
            old_track_state: dict[str, tuple[str, str, bool]] = {}
            for offset in range(0, len(track_ids), 500):
                ids = track_ids[offset : offset + 500]
                placeholders = ",".join("?" for _ in ids)
                rows = connection.execute(
                    "SELECT id, local_album_id, membership_source, membership_locked "
                    f"FROM local_tracks WHERE id IN ({placeholders})",
                    tuple(ids),
                ).fetchall()
                old_track_state.update(
                    {
                        str(row["id"]): (
                            str(row["local_album_id"]),
                            str(row["membership_source"]),
                            bool(row["membership_locked"]),
                        )
                        for row in rows
                    }
                )
            album_ids = [application.local_album_id for application in applications]
            placeholders = ",".join("?" for _ in album_ids)
            rows = connection.execute(
                "SELECT a.*, aa.local_artist_id AS credit_artist_id, "
                "aa.credited_name AS credit_name FROM local_albums a "
                "LEFT JOIN local_album_artists aa ON aa.local_album_id = a.id "
                "AND aa.position = 0 "
                f"WHERE a.id IN ({placeholders})",
                tuple(album_ids),
            ).fetchall()
            existing_albums = {str(row["id"]): dict(row) for row in rows}
            new_albums: list[tuple[Any, ...]] = []
            updated_albums: list[tuple[Any, ...]] = []
            artist_credits: list[tuple[str, str, str]] = []
            track_memberships: list[tuple[str, str]] = []
            for application in applications:
                group = application.group
                album_id = application.local_album_id
                existing = existing_albums.get(album_id)
                if existing is None:
                    new_albums.append(
                        (
                            album_id,
                            root_id,
                            group.grouping_key,
                            group.title,
                            _fold(group.title),
                            group.album_artist_name,
                            _fold(group.album_artist_name),
                            application.local_artist_id,
                            now,
                            now,
                        )
                    )
                elif not bool(existing["grouping_locked"]) and (
                    existing["grouping_key"] != group.grouping_key
                    or existing["title"] != group.title
                    or existing["title_folded"] != _fold(group.title)
                    or existing["album_artist_name"] != group.album_artist_name
                    or existing["album_artist_name_folded"]
                    != _fold(group.album_artist_name)
                    or existing["album_artist_id"] != application.local_artist_id
                    or existing["retired_into_album_id"] is not None
                ):
                    updated_albums.append(
                        (
                            group.grouping_key,
                            group.title,
                            _fold(group.title),
                            group.album_artist_name,
                            _fold(group.album_artist_name),
                            application.local_artist_id,
                            now,
                            album_id,
                        )
                    )
                if group.reason_code != "MANUAL_MEMBERSHIP_RESTORED" and (
                    existing is None
                    or existing["credit_artist_id"] != application.local_artist_id
                    or existing["credit_name"] != group.album_artist_name
                ):
                    artist_credits.append(
                        (album_id, application.local_artist_id, group.album_artist_name)
                    )
                for track_id in group.track_ids:
                    state = old_track_state.get(track_id)
                    if state is None or (
                        not state[2]
                        and (state[0] != album_id or state[1] != "automatic")
                    ):
                        track_memberships.append((album_id, track_id))
            if new_albums:
                values = ",".join(
                    "(?,?,?,?,?,?,?,?,'automatic',?,?)" for _ in new_albums
                )
                connection.execute(
                    "INSERT INTO local_albums "
                    "(id,root_id,grouping_key,title,title_folded,album_artist_name,"
                    "album_artist_name_folded,album_artist_id,grouping_source,"
                    f"created_at,updated_at) VALUES {values}",
                    tuple(value for row in new_albums for value in row),
                )
            if updated_albums:
                values = ",".join("(?,?,?,?,?,?,?,?)" for _ in updated_albums)
                connection.execute(
                    "WITH batch(grouping_key,title,title_folded,album_artist_name,"
                    "album_artist_name_folded,album_artist_id,updated_at,id) AS "
                    f"(VALUES {values}) UPDATE local_albums AS target SET "
                    "grouping_key=batch.grouping_key,title=batch.title,"
                    "title_folded=batch.title_folded,album_artist_name=batch.album_artist_name,"
                    "album_artist_name_folded=batch.album_artist_name_folded,"
                    "album_artist_id=batch.album_artist_id,retired_into_album_id=NULL,"
                    "updated_at=batch.updated_at,row_revision=target.row_revision+1 "
                    "FROM batch WHERE target.id=batch.id",
                    tuple(value for row in updated_albums for value in row),
                )
            if artist_credits:
                values = ",".join("(?,0,?,'primary',?)" for _ in artist_credits)
                connection.execute(
                    "INSERT INTO local_album_artists "
                    "(local_album_id,position,local_artist_id,role,credited_name) "
                    f"VALUES {values} ON CONFLICT(local_album_id,position) DO UPDATE SET "
                    "local_artist_id=excluded.local_artist_id,"
                    "credited_name=excluded.credited_name,"
                    "row_revision=local_album_artists.row_revision+1",
                    tuple(value for row in artist_credits for value in row),
                )
            for offset in range(0, len(track_memberships), 500):
                rows = track_memberships[offset : offset + 500]
                values = ",".join("(?,?)" for _ in rows)
                connection.execute(
                    "WITH batch(local_album_id,id) AS "
                    f"(VALUES {values}) UPDATE local_tracks AS target SET "
                    "local_album_id=batch.local_album_id,membership_source='automatic',"
                    "row_revision=target.row_revision+1 FROM batch "
                    "WHERE target.id=batch.id AND target.membership_locked=0",
                    tuple(value for row in rows for value in row),
                )
            touched_old_ids = {state[0] for state in old_track_state.values()}
            for old_album_id in touched_old_ids - target_id_set:
                remaining = connection.execute(
                    "SELECT 1 FROM local_tracks WHERE local_album_id = ? LIMIT 1",
                    (old_album_id,),
                ).fetchone()
                if remaining is not None:
                    continue
                successor = retirement_plan.get(old_album_id)
                connection.execute(
                    "UPDATE local_albums SET retired_into_album_id=?,updated_at=?,"
                    "row_revision=row_revision+1 WHERE id=?",
                    (successor, now, old_album_id),
                )
                if successor is not None:
                    connection.execute(
                        "INSERT OR IGNORE INTO local_album_aliases "
                        "(alias,local_album_id,kind,created_at) "
                        "VALUES (?,?,'merged_album',?)",
                        (old_album_id, successor, now),
                    )
            return self._bump_catalog(connection)

        return await self._write_scan(operation)

    async def reconcile_scan_scope_batch(
        self,
        run_id: str,
        root_id: str,
        relative_path: str,
        *,
        now: float,
        limit: int,
    ) -> dict[str, int | bool]:
        """Reconcile one bounded batch only after a completed discovery fence."""

        def operation(connection: sqlite3.Connection) -> dict[str, int | bool]:
            scope = connection.execute(
                "SELECT discovery_state, discovery_generation, reconciliation_cursor FROM "
                "library_scan_run_scopes WHERE run_id = ? "
                "AND root_id = ? AND relative_path = ?",
                (run_id, root_id, relative_path),
            ).fetchone()
            if scope is None:
                raise ResourceNotFoundError("Scan scope not found.")
            if scope["discovery_state"] != "completed":
                return {
                    "missing": 0,
                    "excluded": 0,
                    "restored": 0,
                    "identification_enqueued": 0,
                    "reviews_resolved": 0,
                    "done": True,
                }
            path_clause = "1 = 1"
            path_params: tuple[Any, ...] = ()
            if relative_path != ".":
                path_clause = "track.relative_path LIKE ? ESCAPE '\\'"
                path_params = (_escape_like(relative_path.rstrip("/")) + "/%",)
            candidates = connection.execute(
                "SELECT track.id,track.local_album_id,track.relative_path,"
                "track.availability,track.manual_excluded,track.applied_policy,"
                "track.applied_policy_revision,inventory.effective_policy AS "
                "inventory_effective_policy,inventory.policy_revision AS "
                "inventory_policy_revision FROM local_tracks AS track LEFT JOIN "
                "library_scan_inventory AS inventory ON inventory.run_id=? "
                "AND inventory.root_id=track.root_id "
                "AND inventory.relative_path=track.relative_path "
                "AND inventory.scope_relative_path=? "
                "AND inventory.discovery_generation=? WHERE track.root_id=? "
                f"AND ({path_clause}) AND track.relative_path > COALESCE(?, '') "
                "ORDER BY track.relative_path LIMIT ?",
                (
                    run_id,
                    relative_path,
                    scope["discovery_generation"],
                    root_id,
                    *path_params,
                    scope["reconciliation_cursor"],
                    limit,
                ),
            ).fetchall()
            counts = {
                "missing": 0,
                "excluded": 0,
                "restored": 0,
                "identification_enqueued": 0,
                "reviews_resolved": 0,
            }
            queued_albums: set[str] = set()
            missing_track_ids: set[str] = set()
            missing_album_ids: set[str] = set()
            candidate_album_ids = sorted(
                {str(track["local_album_id"]) for track in candidates}
            )
            protected_albums: set[str] = set()
            active_dedupes: set[str] = set()
            if candidate_album_ids:
                placeholders = ",".join("?" for _ in candidate_album_ids)
                protected_albums = {
                    str(row["local_album_id"])
                    for row in connection.execute(
                        "SELECT local_album_id FROM local_album_external_identities "
                        f"WHERE local_album_id IN ({placeholders}) UNION SELECT "
                        "local_album_id FROM library_identification_reviews WHERE "
                        f"local_album_id IN ({placeholders}) AND state='keep_tagged'",
                        (*candidate_album_ids, *candidate_album_ids),
                    ).fetchall()
                }
                active_dedupes = {
                    str(row["dedupe_key"])
                    for row in connection.execute(
                        "SELECT dedupe_key FROM library_identification_jobs WHERE "
                        f"local_album_id IN ({placeholders}) "
                        "AND state IN ('queued','running','paused')",
                        candidate_album_ids,
                    ).fetchall()
                    if row["dedupe_key"] is not None
                }
            for track in candidates:
                inventory_policy = track["inventory_effective_policy"]
                inventory_revision = track["inventory_policy_revision"]
                if inventory_policy is None:
                    missing_track_ids.add(str(track["id"]))
                    missing_album_ids.add(str(track["local_album_id"]))
                    if track["availability"] != "missing":
                        connection.execute(
                            "UPDATE local_tracks SET availability = 'missing', missing_since = ?, "
                            "row_revision = row_revision + 1 WHERE id = ?",
                            (now, track["id"]),
                        )
                        counts["missing"] += 1
                elif inventory_policy == "excluded" and (
                    track["availability"] != "excluded"
                    or track["applied_policy"] != "excluded"
                    or track["applied_policy_revision"] != inventory_revision
                ):
                    connection.execute(
                        "UPDATE local_tracks SET availability = 'excluded', excluded_at = ?, "
                        "desired_policy_revision = ?, applied_policy_revision = ?, "
                        "applied_policy = 'excluded', "
                        "row_revision = row_revision + 1 WHERE id = ?",
                        (
                            now,
                            inventory_revision,
                            inventory_revision,
                            track["id"],
                        ),
                    )
                    counts["excluded"] += 1
                elif inventory_policy != "excluded" and (
                    track["applied_policy"] != inventory_policy
                    or track["applied_policy_revision"] != inventory_revision
                    or (
                        track["availability"] != "indexed"
                        and not bool(track["manual_excluded"])
                    )
                ):
                    restored = track["availability"] != "indexed" and not bool(
                        track["manual_excluded"]
                    )
                    connection.execute(
                        "UPDATE local_tracks SET availability = CASE WHEN manual_excluded = 1 "
                        "THEN 'excluded' ELSE 'indexed' END, missing_since = NULL, "
                        "excluded_at = CASE WHEN manual_excluded = 1 THEN excluded_at ELSE NULL END, "
                        "desired_policy_revision = ?, "
                        "applied_policy_revision = ?, applied_policy = ?, "
                        "row_revision = row_revision + 1 WHERE id = ?",
                        (
                            inventory_revision,
                            inventory_revision,
                            inventory_policy,
                            track["id"],
                        ),
                    )
                    counts["restored"] += int(restored)
                    album_id = str(track["local_album_id"])
                    if (
                        inventory_policy == "automatic"
                        and track["applied_policy"] != "automatic"
                        and not bool(track["manual_excluded"])
                        and album_id not in queued_albums
                    ):
                        if album_id not in protected_albums:
                            dedupe = f"automatic:{album_id}:policy:{inventory_revision}"
                            if dedupe not in active_dedupes:
                                sequence = self._increment_singleton(
                                    connection,
                                    "library_enqueue_sequence",
                                    "singleton",
                                    1,
                                )
                                connection.execute(
                                    "INSERT INTO library_identification_jobs "
                                    "(id, local_album_id, kind, state, priority, enqueue_sequence, "
                                    "input_revision, dedupe_key, created_at, updated_at) "
                                    "VALUES (?, ?, 'automatic', 'queued', 20, ?, ?, ?, ?, ?)",
                                    (
                                        str(uuid.uuid4()),
                                        album_id,
                                        sequence,
                                        str(inventory_revision),
                                        dedupe,
                                        now,
                                        now,
                                    ),
                                )
                                counts["identification_enqueued"] += 1
                                active_dedupes.add(dedupe)
                            queued_albums.add(album_id)
            for offset in range(0, len(missing_track_ids), 500):
                track_ids = sorted(missing_track_ids)[offset : offset + 500]
                placeholders = ",".join("?" for _ in track_ids)
                counts["reviews_resolved"] += connection.execute(
                    "UPDATE library_identification_reviews SET state = 'resolved', "
                    "reason_code = 'SUBJECT_MISSING', updated_at = ?, decided_at = ?, "
                    "row_revision = row_revision + 1 WHERE state = 'needs_review' "
                    f"AND local_track_id IN ({placeholders})",
                    (now, now, *track_ids),
                ).rowcount
            for offset in range(0, len(missing_album_ids), 500):
                album_ids = sorted(missing_album_ids)[offset : offset + 500]
                placeholders = ",".join("?" for _ in album_ids)
                counts["reviews_resolved"] += connection.execute(
                    "UPDATE library_identification_reviews SET state = 'resolved', "
                    "reason_code = 'SUBJECT_MISSING', updated_at = ?, decided_at = ?, "
                    "row_revision = row_revision + 1 WHERE state = 'needs_review' "
                    f"AND local_album_id IN ({placeholders}) "
                    "AND NOT EXISTS (SELECT 1 FROM local_tracks track "
                    "WHERE track.local_album_id = library_identification_reviews.local_album_id "
                    "AND track.availability = 'indexed')",
                    (now, now, *album_ids),
                ).rowcount
                stale_contributions = connection.execute(
                    "UPDATE library_contribution_drafts SET state = 'stale', "
                    "terminal_at = ?, updated_at = ?, seed_snapshot_json = NULL, "
                    "row_revision = row_revision + 1 WHERE state NOT IN "
                    "('linked','cancelled','stale') AND row_revision < ? "
                    f"AND local_album_id IN ({placeholders}) "
                    "AND NOT EXISTS (SELECT 1 FROM local_tracks track "
                    "WHERE track.local_album_id = library_contribution_drafts.local_album_id "
                    "AND track.availability = 'indexed') RETURNING id",
                    (now, now, MAX_REVISION, *album_ids),
                ).fetchall()
                contribution_ids = [str(item["id"]) for item in stale_contributions]
                if contribution_ids:
                    contribution_placeholders = ",".join("?" for _ in contribution_ids)
                    connection.execute(
                        "UPDATE library_contribution_verification_jobs "
                        "SET state = 'cancelled', terminal_at = ?, updated_at = ?, "
                        "lease_owner = NULL, lease_expires_at = NULL, heartbeat_at = NULL, "
                        "row_revision = row_revision + 1, event_revision = event_revision + 1 "
                        "WHERE state IN ('queued','running') AND row_revision < ? "
                        f"AND contribution_id IN ({contribution_placeholders})",
                        (now, now, MAX_REVISION, *contribution_ids),
                    )
            if any(
                counts[key]
                for key in ("missing", "excluded", "restored", "reviews_resolved")
            ):
                self._bump_catalog(connection)
            done = len(candidates) < limit
            cursor = (
                str(candidates[-1]["relative_path"])
                if candidates
                else scope["reconciliation_cursor"]
            )
            connection.execute(
                "UPDATE library_scan_run_scopes SET reconciliation_state = ?, "
                "reconciliation_cursor = ?, "
                "row_revision = row_revision + 1 WHERE run_id = ? AND root_id = ? "
                "AND relative_path = ?",
                (
                    "completed" if done else "running",
                    cursor,
                    run_id,
                    root_id,
                    relative_path,
                ),
            )
            scan_counter_values = (
                counts["missing"],
                counts["excluded"],
                counts["identification_enqueued"],
            )
            if any(scan_counter_values):
                updated = connection.execute(
                    "UPDATE library_scan_runs SET missing_count=missing_count+?,"
                    "excluded_count=excluded_count+?,identification_enqueued_count="
                    "identification_enqueued_count+?,updated_at=?,"
                    "row_revision=row_revision+1,event_revision=event_revision+1 "
                    "WHERE id=? AND row_revision<? AND event_revision<?",
                    (
                        *scan_counter_values,
                        now,
                        run_id,
                        MAX_REVISION,
                        MAX_REVISION,
                    ),
                )
                if updated.rowcount != 1:
                    raise RevisionOverflowError(
                        "A scan run revision reached its maximum value."
                    )
                self._bump_stream(connection, "scan")
            counts["done"] = done
            return counts

        return await self._write_scan(operation)

    async def apply_desired_policy(
        self,
        *,
        root_id: str,
        relative_prefix: str,
        policy_revision: str,
        policy: str,
        updated_at: float,
    ) -> dict[str, int]:
        """Record desired policy and synchronously suppress newly prohibited work."""

        def operation(connection: sqlite3.Connection) -> dict[str, int]:
            result = self._apply_desired_policy_sync(
                connection,
                root_id=root_id,
                relative_prefix=relative_prefix,
                policy_revision=policy_revision,
                policy=policy,
                updated_at=updated_at,
            )
            if result["changed"]:
                self._bump_catalog(connection)
            if result["cancelled"]:
                self._bump_stream(connection, "identification")
            return result

        return await self._write(operation)

    @staticmethod
    def _apply_desired_policy_sync(
        connection: sqlite3.Connection,
        *,
        root_id: str,
        relative_prefix: str,
        policy_revision: str,
        policy: str,
        updated_at: float,
    ) -> dict[str, int]:
        if relative_prefix in {"", "."}:
            path_clause = "1 = 1"
            path_params: tuple[Any, ...] = ()
        else:
            path_clause = "relative_path = ? OR relative_path LIKE ? ESCAPE '\\'"
            path_params = (
                relative_prefix,
                f"{_escape_like(relative_prefix)}/%",
            )
        changed = connection.execute(
            "UPDATE local_tracks SET desired_policy_revision = ?, "
            "row_revision = row_revision + 1 WHERE root_id = ? AND "
            f"({path_clause}) AND desired_policy_revision != ? AND row_revision < ?",
            (
                policy_revision,
                root_id,
                *path_params,
                policy_revision,
                MAX_REVISION,
            ),
        ).rowcount
        cancelled = 0
        if policy != "automatic":
            cancelled = connection.execute(
                "UPDATE library_identification_jobs SET state = 'cancelled', "
                "last_failure_code = 'POLICY_PROHIBITS_AUTOMATIC', terminal_at = ?, "
                "updated_at = ?, row_revision = row_revision + 1, "
                "event_revision = event_revision + 1 WHERE kind = 'automatic' "
                "AND state IN ('queued','paused') AND local_album_id IN ("
                "SELECT DISTINCT local_album_id FROM local_tracks WHERE root_id = ? "
                f"AND ({path_clause})) AND row_revision < ? AND event_revision < ?",
                (
                    updated_at,
                    updated_at,
                    root_id,
                    *path_params,
                    MAX_REVISION,
                    MAX_REVISION,
                ),
            ).rowcount
        return {"changed": changed, "cancelled": cancelled}

    async def prepare_policy_transition(
        self,
        *,
        previous_policy_revision: str,
        proposed_policy_revision: str,
        previous_settings_json: str,
        proposed_settings_json: str,
        scopes: list[ScanScope],
        prepared_at: float,
    ) -> None:
        def operation(connection: sqlite3.Connection) -> None:
            current = connection.execute(
                "SELECT proposed_policy_revision, state FROM library_policy_transitions "
                "WHERE singleton = 1"
            ).fetchone()
            if current is not None and current["state"] == "prepared":
                if str(current["proposed_policy_revision"]) == proposed_policy_revision:
                    return
                raise ConflictError(
                    "Another library policy change is still being applied."
                )
            connection.execute(
                "INSERT INTO library_policy_transitions "
                "(singleton, previous_policy_revision, proposed_policy_revision, "
                "previous_settings_json, proposed_settings_json, scopes_json, state, "
                "prepared_at, completed_at) VALUES (1,?,?,?,?,?,'prepared',?,NULL) "
                "ON CONFLICT(singleton) DO UPDATE SET "
                "previous_policy_revision = excluded.previous_policy_revision, "
                "proposed_policy_revision = excluded.proposed_policy_revision, "
                "previous_settings_json = excluded.previous_settings_json, "
                "proposed_settings_json = excluded.proposed_settings_json, "
                "scopes_json = excluded.scopes_json, state = 'prepared', "
                "prepared_at = excluded.prepared_at, completed_at = NULL",
                (
                    previous_policy_revision,
                    proposed_policy_revision,
                    previous_settings_json,
                    proposed_settings_json,
                    json.dumps(msgspec.to_builtins(scopes), sort_keys=True),
                    prepared_at,
                ),
            )

        await self._write(operation)

    async def get_policy_transition(self) -> dict[str, Any] | None:
        def operation(connection: sqlite3.Connection) -> dict[str, Any] | None:
            row = connection.execute(
                "SELECT * FROM library_policy_transitions WHERE singleton = 1"
            ).fetchone()
            if row is None:
                return None
            return {
                "previous_policy_revision": str(row["previous_policy_revision"]),
                "proposed_policy_revision": str(row["proposed_policy_revision"]),
                "previous_settings_json": str(row["previous_settings_json"]),
                "proposed_settings_json": str(row["proposed_settings_json"]),
                "scopes": msgspec.convert(
                    json.loads(str(row["scopes_json"])), type=list[ScanScope]
                ),
                "state": str(row["state"]),
                "prepared_at": float(row["prepared_at"]),
                "completed_at": (
                    float(row["completed_at"])
                    if row["completed_at"] is not None
                    else None
                ),
            }

        return await self._read(operation)

    async def abort_policy_transition(
        self, *, proposed_policy_revision: str, aborted_at: float
    ) -> None:
        def operation(connection: sqlite3.Connection) -> None:
            changed = connection.execute(
                "UPDATE library_policy_transitions SET state = 'aborted', completed_at = ? "
                "WHERE singleton = 1 AND state = 'prepared' "
                "AND proposed_policy_revision = ?",
                (aborted_at, proposed_policy_revision),
            ).rowcount
            if changed != 1:
                raise StaleRevisionError(
                    "The prepared library policy change is no longer current."
                )

        await self._write(operation)

    async def commit_policy_transition(
        self, *, proposed_policy_revision: str, updated_at: float
    ) -> dict[str, int]:
        def operation(connection: sqlite3.Connection) -> dict[str, int]:
            transition = connection.execute(
                "SELECT proposed_policy_revision, scopes_json, state "
                "FROM library_policy_transitions WHERE singleton = 1"
            ).fetchone()
            if transition is None or transition["state"] != "prepared":
                raise StaleRevisionError(
                    "No matching library policy change is prepared."
                )
            if str(transition["proposed_policy_revision"]) != proposed_policy_revision:
                raise StaleRevisionError(
                    "The prepared library policy change is no longer current."
                )
            scopes = msgspec.convert(
                json.loads(str(transition["scopes_json"])), type=list[ScanScope]
            )
            totals = self._apply_policy_boundary_sync(
                connection,
                scopes=scopes,
                policy_revision=proposed_policy_revision,
                updated_at=updated_at,
            )
            connection.execute(
                "UPDATE library_policy_transitions SET state = 'completed', completed_at = ? "
                "WHERE singleton = 1",
                (updated_at,),
            )
            return totals

        return await self._write(operation)

    def _apply_policy_boundary_sync(
        self,
        connection: sqlite3.Connection,
        *,
        scopes: list[ScanScope],
        policy_revision: str,
        updated_at: float,
    ) -> dict[str, int]:
        totals = {"changed": 0, "cancelled": 0}
        for scope in scopes:
            result = self._apply_desired_policy_sync(
                connection,
                root_id=scope.root_id,
                relative_prefix=scope.relative_path,
                policy_revision=policy_revision,
                policy=scope.effective_policy,
                updated_at=updated_at,
            )
            totals["changed"] += result["changed"]
            totals["cancelled"] += result["cancelled"]
        self._record_pending_policy_sync(
            connection,
            policy_revision=policy_revision,
            scopes=scopes,
            changed_track_count=totals["changed"],
            cancelled_work_count=totals["cancelled"],
            updated_at=updated_at,
        )
        if totals["changed"]:
            self._bump_catalog(connection)
        if totals["cancelled"]:
            self._bump_stream(connection, "identification")
        return totals

    async def save_policy_boundary(
        self,
        *,
        scopes: list[ScanScope],
        policy_revision: str,
        updated_at: float,
    ) -> dict[str, int]:
        """Apply desired-policy suppression and persist its frozen scopes atomically."""

        def operation(connection: sqlite3.Connection) -> dict[str, int]:
            return self._apply_policy_boundary_sync(
                connection,
                scopes=scopes,
                policy_revision=policy_revision,
                updated_at=updated_at,
            )

        return await self._write(operation)

    async def record_pending_policy(
        self,
        *,
        policy_revision: str,
        scopes: list[ScanScope],
        changed_track_count: int,
        cancelled_work_count: int,
        updated_at: float,
    ) -> dict[str, Any]:
        def operation(connection: sqlite3.Connection) -> dict[str, Any]:
            return self._record_pending_policy_sync(
                connection,
                policy_revision=policy_revision,
                scopes=scopes,
                changed_track_count=changed_track_count,
                cancelled_work_count=cancelled_work_count,
                updated_at=updated_at,
            )

        return await self._write(operation)

    @staticmethod
    def _record_pending_policy_sync(
        connection: sqlite3.Connection,
        *,
        policy_revision: str,
        scopes: list[ScanScope],
        changed_track_count: int,
        cancelled_work_count: int,
        updated_at: float,
    ) -> dict[str, Any]:
        scope_ids = sorted(
            {scope.scope_id for scope in scopes if scope.scope_id is not None}
        )
        encoded_scopes = json.dumps(msgspec.to_builtins(scopes), sort_keys=True)
        connection.execute(
            "INSERT INTO library_policy_state "
            "(singleton, desired_policy_revision, pending_scope_ids_json, "
            "pending_scopes_json, changed_track_count, cancelled_work_count, updated_at) "
            "VALUES (1, ?, ?, ?, ?, ?, ?) ON CONFLICT(singleton) DO UPDATE SET "
            "desired_policy_revision = excluded.desired_policy_revision, "
            "pending_scope_ids_json = excluded.pending_scope_ids_json, "
            "pending_scopes_json = excluded.pending_scopes_json, "
            "changed_track_count = excluded.changed_track_count, "
            "cancelled_work_count = excluded.cancelled_work_count, "
            "updated_at = excluded.updated_at",
            (
                policy_revision,
                json.dumps(scope_ids),
                encoded_scopes,
                changed_track_count,
                cancelled_work_count,
                updated_at,
            ),
        )
        return {
            "desired_policy_revision": policy_revision,
            "pending_scope_ids": scope_ids,
            "pending_scopes": scopes,
            "changed_track_count": changed_track_count,
            "cancelled_work_count": cancelled_work_count,
            "updated_at": updated_at,
        }

    async def get_pending_policy(self) -> dict[str, Any] | None:
        def operation(connection: sqlite3.Connection) -> dict[str, Any] | None:
            row = connection.execute(
                "SELECT * FROM library_policy_state WHERE singleton = 1"
            ).fetchone()
            if row is None:
                return None
            return {
                "desired_policy_revision": str(row["desired_policy_revision"]),
                "pending_scope_ids": json.loads(str(row["pending_scope_ids_json"])),
                "pending_scopes": msgspec.convert(
                    json.loads(str(row["pending_scopes_json"])),
                    type=list[ScanScope],
                ),
                "changed_track_count": int(row["changed_track_count"]),
                "cancelled_work_count": int(row["cancelled_work_count"]),
                "updated_at": float(row["updated_at"]),
            }

        return await self._read(operation)

    async def apply_membership_correction(
        self,
        *,
        kind: str,
        track_ids: list[str],
        expected_album_revisions: dict[str, int],
        target_album_id: str | None,
        new_album_id: str | None,
        title: str | None,
        album_artist_name: str | None,
        identity_choice: str,
        actor_user_id: str,
        idempotency_key: str,
        now: float,
    ) -> dict[str, Any]:
        def operation(connection: sqlite3.Connection) -> dict[str, Any]:
            existing = connection.execute(
                "SELECT * FROM library_catalog_actions WHERE idempotency_key = ?",
                (idempotency_key,),
            ).fetchone()
            if existing is not None:
                return json.loads(str(existing["after_json"]))
            if not track_ids:
                raise ValueError("A membership correction needs at least one track.")
            placeholders = ",".join("?" for _ in track_ids)
            tracks = connection.execute(
                f"SELECT * FROM local_tracks WHERE id IN ({placeholders}) ORDER BY id",
                track_ids,
            ).fetchall()
            if len(tracks) != len(set(track_ids)):
                raise ResourceNotFoundError(
                    "One or more selected local tracks were not found."
                )
            source_ids = sorted({str(track["local_album_id"]) for track in tracks})
            for album_id, expected in expected_album_revisions.items():
                album = connection.execute(
                    "SELECT row_revision FROM local_albums WHERE id = ?", (album_id,)
                ).fetchone()
                if album is None:
                    raise ResourceNotFoundError("A selected local album was not found.")
                if int(album["row_revision"]) != expected:
                    raise StaleRevisionError("Album membership changed after preview.")
            destination_id = target_album_id
            if kind == "split" and destination_id is None:
                destination_id = new_album_id or str(uuid.uuid4())
                source = connection.execute(
                    "SELECT * FROM local_albums WHERE id = ?", (source_ids[0],)
                ).fetchone()
                connection.execute(
                    "INSERT INTO local_albums "
                    "(id, root_id, grouping_key, title, title_folded, album_artist_name, "
                    "album_artist_name_folded, album_artist_id, year, primary_genre, is_compilation, "
                    "grouping_source, grouping_locked, created_at, updated_at) "
                    "VALUES (?,?,?,?,?,?,?,?,?,?,?,'manual',1,?,?)",
                    (
                        destination_id,
                        source["root_id"],
                        f"manual:{destination_id}",
                        title or source["title"],
                        _fold(title or source["title"]),
                        album_artist_name or source["album_artist_name"],
                        _fold(album_artist_name or source["album_artist_name"]),
                        source["album_artist_id"],
                        source["year"],
                        source["primary_genre"],
                        source["is_compilation"],
                        now,
                        now,
                    ),
                )
                credits = connection.execute(
                    "SELECT * FROM local_album_artists WHERE local_album_id = ? ORDER BY position",
                    (source_ids[0],),
                ).fetchall()
                connection.executemany(
                    "INSERT INTO local_album_artists "
                    "(local_album_id, position, local_artist_id, role, credited_name) "
                    "VALUES (?,?,?,?,?)",
                    [
                        (
                            destination_id,
                            row["position"],
                            row["local_artist_id"],
                            row["role"],
                            row["credited_name"],
                        )
                        for row in credits
                    ],
                )
            if kind in {"split", "move", "merge"}:
                if destination_id is None:
                    raise ValueError("A destination album is required.")
                destination = connection.execute(
                    "SELECT row_revision FROM local_albums WHERE id = ? AND retired_into_album_id IS NULL",
                    (destination_id,),
                ).fetchone()
                if destination is None:
                    raise ResourceNotFoundError("The destination album was not found.")
                connection.execute(
                    f"UPDATE local_tracks SET local_album_id = ?, membership_source = 'manual', "
                    f"membership_locked = 1, row_revision = row_revision + 1 WHERE id IN ({placeholders})",
                    (destination_id, *track_ids),
                )
                affected = sorted(set(source_ids) | {destination_id})
                affected_placeholders = ",".join("?" for _ in affected)
                identities = connection.execute(
                    f"SELECT * FROM local_album_external_identities WHERE local_album_id IN ({affected_placeholders})",
                    affected,
                ).fetchall()
                if identity_choice == "detach":
                    connection.execute(
                        f"DELETE FROM local_track_external_identities WHERE local_track_id IN "
                        f"(SELECT id FROM local_tracks WHERE local_album_id IN ({affected_placeholders}))",
                        affected,
                    )
                    connection.execute(
                        f"DELETE FROM local_album_external_identities WHERE local_album_id IN ({affected_placeholders})",
                        affected,
                    )
                else:
                    for identity in identities:
                        if (
                            identity["decision_source"] != "manual"
                            or identity["local_album_id"] != destination_id
                        ):
                            connection.execute(
                                "DELETE FROM local_album_external_identities WHERE local_album_id = ?",
                                (identity["local_album_id"],),
                            )
                    connection.execute(
                        "INSERT OR IGNORE INTO library_identification_reviews "
                        "(id, local_album_id, state, reason_code, input_revision, created_at, updated_at) "
                        "VALUES (?, ?, 'needs_review', 'MANUAL_GROUPING_CHANGED', ?, ?, ?)",
                        (
                            str(uuid.uuid4()),
                            destination_id,
                            f"manual-group:{idempotency_key}",
                            now,
                            now,
                        ),
                    )
                if kind == "merge":
                    for source_id in source_ids:
                        if source_id == destination_id:
                            continue
                        connection.execute(
                            "UPDATE local_albums SET retired_into_album_id = ?, grouping_locked = 1, "
                            "updated_at = ?, row_revision = row_revision + 1 WHERE id = ?",
                            (destination_id, now, source_id),
                        )
                        connection.execute(
                            "INSERT OR IGNORE INTO local_album_aliases "
                            "(alias, local_album_id, kind, created_at) VALUES (?, ?, 'merged_album', ?)",
                            (source_id, destination_id, now),
                        )
                for album_id in affected:
                    connection.execute(
                        "UPDATE local_albums SET grouping_source = 'manual', grouping_locked = 1, "
                        "updated_at = ?, row_revision = row_revision + 1 WHERE id = ?",
                        (now, album_id),
                    )
            elif kind == "reset":
                connection.execute(
                    f"UPDATE local_tracks SET membership_locked = 0, membership_source = 'automatic', "
                    f"row_revision = row_revision + 1 WHERE id IN ({placeholders})",
                    track_ids,
                )
                for album_id in source_ids:
                    connection.execute(
                        "UPDATE local_albums SET grouping_locked = 0, grouping_source = 'automatic', "
                        "updated_at = ?, row_revision = row_revision + 1 WHERE id = ?",
                        (now, album_id),
                    )
            else:
                raise ValueError(f"Unsupported membership correction: {kind}")
            after = {
                "kind": kind,
                "track_ids": track_ids,
                "source_album_ids": source_ids,
                "target_album_id": destination_id,
            }
            after["catalog_revision"] = self._bump_catalog(connection)
            connection.execute(
                "INSERT INTO library_catalog_actions "
                "(id, idempotency_key, actor_user_id, action_kind, local_album_id, "
                "before_json, after_json, reason_code, created_at) VALUES (?,?,?,?,?,?,?,?,?)",
                (
                    str(uuid.uuid4()),
                    idempotency_key,
                    actor_user_id,
                    kind,
                    destination_id or source_ids[0],
                    json.dumps({"source_album_ids": source_ids}),
                    json.dumps(after, sort_keys=True),
                    "MANUAL_GROUPING",
                    now,
                ),
            )
            return after

        return await self._write(operation)

    async def apply_grouping_reset(
        self,
        *,
        track_ids: list[str],
        expected_album_revisions: dict[str, int],
        applications: list[GroupingApplication],
        actor_user_id: str,
        idempotency_key: str,
        now: float,
    ) -> dict[str, Any]:
        """Unlock selected memberships and apply their stored-tag grouping atomically."""

        def operation(connection: sqlite3.Connection) -> dict[str, Any]:
            existing_action = connection.execute(
                "SELECT after_json FROM library_catalog_actions WHERE idempotency_key = ?",
                (idempotency_key,),
            ).fetchone()
            if existing_action is not None:
                return json.loads(str(existing_action["after_json"]))
            if not track_ids or not applications:
                raise ValueError(
                    "A grouping reset needs selected tracks and a preview."
                )
            for album_id, expected_revision in expected_album_revisions.items():
                album = connection.execute(
                    "SELECT row_revision FROM local_albums WHERE id = ? "
                    "AND retired_into_album_id IS NULL",
                    (album_id,),
                ).fetchone()
                if album is None:
                    raise ResourceNotFoundError("A reset album was not found.")
                if int(album["row_revision"]) != expected_revision:
                    raise StaleRevisionError("Album membership changed after preview.")
            all_track_ids = [
                track_id
                for application in applications
                for track_id in application.group.track_ids
            ]
            if len(all_track_ids) != len(set(all_track_ids)):
                raise ValueError("A reset preview assigned one track more than once.")
            all_placeholders = ",".join("?" for _ in all_track_ids)
            old_rows = connection.execute(
                "SELECT id, local_album_id FROM local_tracks "
                f"WHERE id IN ({all_placeholders})",
                all_track_ids,
            ).fetchall()
            if len(old_rows) != len(all_track_ids):
                raise StaleRevisionError("A reset track changed after preview.")
            selected_placeholders = ",".join("?" for _ in track_ids)
            selected_count = connection.execute(
                f"UPDATE local_tracks SET membership_locked = 0, "
                f"membership_source = 'automatic', row_revision = row_revision + 1 "
                f"WHERE id IN ({selected_placeholders})",
                track_ids,
            ).rowcount
            if selected_count != len(set(track_ids)):
                raise StaleRevisionError(
                    "A selected reset track is no longer available."
                )

            old_membership: dict[str, set[str]] = {}
            for row in old_rows:
                old_membership.setdefault(str(row["local_album_id"]), set()).add(
                    str(row["id"])
                )
            target_ids: list[str] = []
            for application in applications:
                group = application.group
                candidate_id = application.local_artist_id
                artist = connection.execute(
                    "SELECT id FROM local_artists WHERE id = ? "
                    "AND retired_into_artist_id IS NULL",
                    (candidate_id,),
                ).fetchone()
                if artist is None:
                    exact = connection.execute(
                        "SELECT id FROM local_artists WHERE normalized_name = ? "
                        "AND kind = 'group' AND retired_into_artist_id IS NULL "
                        "ORDER BY created_at, id",
                        (_normalize_exact(group.album_artist_name),),
                    ).fetchall()
                    if len(exact) == 1:
                        artist_id = str(exact[0]["id"])
                    else:
                        collisions = connection.execute(
                            "SELECT id FROM local_artists WHERE folded_name = ? "
                            "AND retired_into_artist_id IS NULL ORDER BY created_at, id",
                            (_fold(group.album_artist_name),),
                        ).fetchall()
                        connection.execute(
                            "INSERT INTO local_artists "
                            "(id, display_name, folded_name, normalized_name, kind, "
                            "created_at, updated_at) VALUES (?,?,?,?, 'group', ?, ?)",
                            (
                                candidate_id,
                                group.album_artist_name,
                                _fold(group.album_artist_name),
                                _normalize_exact(group.album_artist_name),
                                now,
                                now,
                            ),
                        )
                        for collision in collisions:
                            left, right = sorted((candidate_id, str(collision["id"])))
                            connection.execute(
                                "INSERT OR IGNORE INTO local_artist_merge_candidates "
                                "(id, left_artist_id, right_artist_id, reason_code, "
                                "created_at, updated_at) VALUES (?,?,?,?,?,?)",
                                (
                                    str(
                                        uuid.uuid5(
                                            uuid.NAMESPACE_URL,
                                            f"artist-collision:{left}:{right}",
                                        )
                                    ),
                                    left,
                                    right,
                                    "FOLDED_NAME_COLLISION",
                                    now,
                                    now,
                                ),
                            )
                        artist_id = candidate_id
                else:
                    artist_id = candidate_id
                album_id = application.local_album_id
                target_ids.append(album_id)
                album = connection.execute(
                    "SELECT id FROM local_albums WHERE id = ?", (album_id,)
                ).fetchone()
                root_id = connection.execute(
                    "SELECT root_id FROM local_tracks WHERE id = ?",
                    (group.track_ids[0],),
                ).fetchone()[0]
                if album is None:
                    connection.execute(
                        "INSERT INTO local_albums "
                        "(id, root_id, grouping_key, title, title_folded, album_artist_name, "
                        "album_artist_name_folded, album_artist_id, grouping_source, "
                        "created_at, updated_at) VALUES (?,?,?,?,?,?,?,?, 'automatic', ?, ?)",
                        (
                            album_id,
                            root_id,
                            group.grouping_key,
                            group.title,
                            _fold(group.title),
                            group.album_artist_name,
                            _fold(group.album_artist_name),
                            artist_id,
                            now,
                            now,
                        ),
                    )
                elif group.reason_code != "MANUAL_MEMBERSHIP_RESTORED":
                    connection.execute(
                        "UPDATE local_albums SET grouping_key = ?, title = ?, title_folded = ?, "
                        "album_artist_name = ?, album_artist_name_folded = ?, album_artist_id = ?, "
                        "grouping_source = 'automatic', grouping_locked = 0, "
                        "retired_into_album_id = NULL, updated_at = ?, "
                        "row_revision = row_revision + 1 WHERE id = ?",
                        (
                            group.grouping_key,
                            group.title,
                            _fold(group.title),
                            group.album_artist_name,
                            _fold(group.album_artist_name),
                            artist_id,
                            now,
                            album_id,
                        ),
                    )
                if group.reason_code != "MANUAL_MEMBERSHIP_RESTORED":
                    connection.execute(
                        "INSERT INTO local_album_artists "
                        "(local_album_id, position, local_artist_id, role, credited_name) "
                        "VALUES (?,0,?,'primary',?) ON CONFLICT(local_album_id, position) "
                        "DO UPDATE SET local_artist_id = excluded.local_artist_id, "
                        "credited_name = excluded.credited_name, row_revision = row_revision + 1",
                        (album_id, artist_id, group.album_artist_name),
                    )
                group_placeholders = ",".join("?" for _ in group.track_ids)
                connection.execute(
                    "UPDATE local_tracks SET local_album_id = ?, "
                    "membership_source = 'automatic', row_revision = row_revision + 1 "
                    f"WHERE id IN ({group_placeholders}) AND membership_locked = 0",
                    (album_id, *group.track_ids),
                )

            new_rows = connection.execute(
                "SELECT id, local_album_id FROM local_tracks "
                f"WHERE id IN ({all_placeholders})",
                all_track_ids,
            ).fetchall()
            new_membership: dict[str, set[str]] = {}
            for row in new_rows:
                new_membership.setdefault(str(row["local_album_id"]), set()).add(
                    str(row["id"])
                )
            affected = sorted(set(old_membership) | set(new_membership))
            changed = {
                album_id
                for album_id in affected
                if old_membership.get(album_id, set())
                != new_membership.get(album_id, set())
            }
            detached: set[str] = set()
            for album_id in changed:
                identity = connection.execute(
                    "SELECT * FROM local_album_external_identities "
                    "WHERE local_album_id = ?",
                    (album_id,),
                ).fetchone()
                if identity is None:
                    continue
                if identity["decision_source"] == "manual":
                    active_review = connection.execute(
                        "SELECT id FROM library_identification_reviews "
                        "WHERE local_album_id = ? AND state != 'resolved' "
                        "ORDER BY updated_at DESC LIMIT 1",
                        (album_id,),
                    ).fetchone()
                    if active_review is None:
                        connection.execute(
                            "INSERT INTO library_identification_reviews "
                            "(id, local_album_id, state, reason_code, input_revision, "
                            "created_at, updated_at) VALUES (?, ?, 'needs_review', "
                            "'MANUAL_IDENTITY_STALE_GROUPING', ?, ?, ?)",
                            (
                                str(uuid.uuid4()),
                                album_id,
                                f"grouping-reset:{idempotency_key}",
                                now,
                                now,
                            ),
                        )
                    else:
                        connection.execute(
                            "UPDATE library_identification_reviews SET "
                            "reason_code = 'MANUAL_IDENTITY_STALE_GROUPING', "
                            "updated_at = ?, row_revision = row_revision + 1 "
                            "WHERE id = ?",
                            (now, active_review["id"]),
                        )
                else:
                    detached.add(album_id)
                    connection.execute(
                        "DELETE FROM local_album_external_identities "
                        "WHERE local_album_id = ?",
                        (album_id,),
                    )
            if detached:
                detached_tracks = set()
                for album_id in detached:
                    detached_tracks.update(old_membership.get(album_id, set()))
                    detached_tracks.update(new_membership.get(album_id, set()))
                placeholders = ",".join("?" for _ in detached_tracks)
                connection.execute(
                    "DELETE FROM local_track_external_identities "
                    f"WHERE local_track_id IN ({placeholders})",
                    sorted(detached_tracks),
                )
            for old_album_id, old_tracks in old_membership.items():
                remaining = connection.execute(
                    "SELECT 1 FROM local_tracks WHERE local_album_id = ? LIMIT 1",
                    (old_album_id,),
                ).fetchone()
                if remaining is not None or old_album_id in target_ids:
                    continue
                overlaps = sorted(
                    (
                        len(old_tracks.intersection(track_ids)),
                        target_id,
                    )
                    for target_id, track_ids in new_membership.items()
                )
                best = overlaps[-1][0] if overlaps else 0
                successors = [
                    target for score, target in overlaps if score == best and score
                ]
                successor = successors[0] if len(successors) == 1 else None
                connection.execute(
                    "UPDATE local_albums SET retired_into_album_id = ?, updated_at = ?, "
                    "row_revision = row_revision + 1 WHERE id = ?",
                    (successor, now, old_album_id),
                )
                if successor is not None:
                    connection.execute(
                        "INSERT OR IGNORE INTO local_album_aliases "
                        "(alias, local_album_id, kind, created_at) "
                        "VALUES (?,?,'merged_album',?)",
                        (old_album_id, successor, now),
                    )
            after = {
                "kind": "reset",
                "track_ids": track_ids,
                "source_album_ids": sorted(old_membership),
                "target_album_id": None,
                "automatic_album_ids": sorted(set(target_ids)),
            }
            after["catalog_revision"] = self._bump_catalog(connection)
            connection.execute(
                "INSERT INTO library_catalog_actions "
                "(id, idempotency_key, actor_user_id, action_kind, local_album_id, "
                "before_json, after_json, reason_code, created_at) "
                "VALUES (?,?,?,?,?,?,?,?,?)",
                (
                    str(uuid.uuid4()),
                    idempotency_key,
                    actor_user_id,
                    "reset",
                    sorted(old_membership)[0],
                    json.dumps(
                        {
                            "membership": {
                                key: sorted(value)
                                for key, value in old_membership.items()
                            }
                        },
                        sort_keys=True,
                    ),
                    json.dumps(after, sort_keys=True),
                    "AUTOMATIC_GROUPING_RESET",
                    now,
                ),
            )
            return after

        return await self._write(operation)

    async def merge_local_artists(
        self,
        *,
        source_artist_ids: list[str],
        surviving_artist_id: str,
        expected_revisions: dict[str, int],
        provider_choice: str,
        actor_user_id: str,
        idempotency_key: str,
        now: float,
    ) -> dict[str, Any]:
        def operation(connection: sqlite3.Connection) -> dict[str, Any]:
            existing = connection.execute(
                "SELECT * FROM library_catalog_actions WHERE idempotency_key = ?",
                (idempotency_key,),
            ).fetchone()
            if existing is not None:
                return json.loads(str(existing["after_json"]))
            all_ids = sorted(set(source_artist_ids) | {surviving_artist_id})
            for artist_id in all_ids:
                row = connection.execute(
                    "SELECT row_revision FROM local_artists WHERE id = ? AND retired_into_artist_id IS NULL",
                    (artist_id,),
                ).fetchone()
                if row is None:
                    raise ResourceNotFoundError(
                        "A selected local artist was not found."
                    )
                if int(row["row_revision"]) != expected_revisions.get(artist_id):
                    raise StaleRevisionError("Artist references changed after preview.")
            retired = [
                artist_id for artist_id in all_ids if artist_id != surviving_artist_id
            ]
            for artist_id in retired:
                connection.execute(
                    "UPDATE local_album_artists SET local_artist_id = ?, row_revision = row_revision + 1 "
                    "WHERE local_artist_id = ?",
                    (surviving_artist_id, artist_id),
                )
                connection.execute(
                    "UPDATE local_track_artists SET local_artist_id = ?, row_revision = row_revision + 1 "
                    "WHERE local_artist_id = ?",
                    (surviving_artist_id, artist_id),
                )
                connection.execute(
                    "UPDATE local_albums SET album_artist_id = ?, updated_at = ?, "
                    "row_revision = row_revision + 1 WHERE album_artist_id = ?",
                    (surviving_artist_id, now, artist_id),
                )
                connection.execute(
                    "DELETE FROM local_artist_external_identities WHERE local_artist_id = ?",
                    (artist_id,),
                )
                connection.execute(
                    "UPDATE local_artists SET retired_into_artist_id = ?, updated_at = ?, "
                    "row_revision = row_revision + 1 WHERE id = ?",
                    (surviving_artist_id, now, artist_id),
                )
                connection.execute(
                    "INSERT OR IGNORE INTO local_artist_aliases "
                    "(alias, local_artist_id, kind, created_at) VALUES (?, ?, 'merged_artist', ?)",
                    (artist_id, surviving_artist_id, now),
                )
                connection.execute(
                    "UPDATE library_migration_provenance SET target_id = ? "
                    "WHERE target_kind = 'local_artist' AND target_id = ?",
                    (surviving_artist_id, artist_id),
                )
                connection.execute(
                    "INSERT OR IGNORE INTO library_user_favorites "
                    "(user_id, item_kind, item_id, created_at) "
                    "SELECT user_id, item_kind, ?, created_at FROM library_user_favorites "
                    "WHERE item_kind = 'artist' AND item_id = ?",
                    (surviving_artist_id, artist_id),
                )
                connection.execute(
                    "DELETE FROM library_user_favorites "
                    "WHERE item_kind = 'artist' AND item_id = ?",
                    (artist_id,),
                )
                connection.execute(
                    "UPDATE library_play_history SET local_artist_id = ? "
                    "WHERE local_artist_id = ?",
                    (surviving_artist_id, artist_id),
                )
                connection.execute(
                    "UPDATE library_playlist_tracks SET local_artist_id = ? "
                    "WHERE local_artist_id = ?",
                    (surviving_artist_id, artist_id),
                )
                connection.execute(
                    "UPDATE library_compat_id_map SET internal_id = ? "
                    "WHERE kind = 'artist' AND internal_id = ?",
                    (surviving_artist_id, artist_id),
                )
            if provider_choice == "detach":
                connection.execute(
                    "DELETE FROM local_artist_external_identities WHERE local_artist_id = ?",
                    (surviving_artist_id,),
                )
            after = {
                "kind": "merge_artist",
                "surviving_artist_id": surviving_artist_id,
                "retired_artist_ids": retired,
            }
            after["catalog_revision"] = self._bump_catalog(connection)
            connection.execute(
                "INSERT INTO library_catalog_actions "
                "(id, idempotency_key, actor_user_id, action_kind, local_artist_id, "
                "before_json, after_json, reason_code, created_at) VALUES (?,?,?,?,?,?,?,?,?)",
                (
                    str(uuid.uuid4()),
                    idempotency_key,
                    actor_user_id,
                    "merge_artist",
                    surviving_artist_id,
                    json.dumps({"artist_ids": all_ids}),
                    json.dumps(after, sort_keys=True),
                    "MANUAL_ARTIST_MERGE",
                    now,
                ),
            )
            return after

        return await self._write(operation)

    async def get_artist_merge_context(self, artist_ids: list[str]) -> dict[str, Any]:
        def operation(connection: sqlite3.Connection) -> dict[str, Any]:
            unique_ids = sorted(set(artist_ids))
            if not unique_ids:
                return {"artists": [], "identities": [], "reference_counts": {}}
            placeholders = ",".join("?" for _ in unique_ids)
            artists = connection.execute(
                "SELECT * FROM local_artists "
                f"WHERE id IN ({placeholders}) ORDER BY id",
                unique_ids,
            ).fetchall()
            identities = connection.execute(
                "SELECT * FROM local_artist_external_identities "
                f"WHERE local_artist_id IN ({placeholders}) "
                "ORDER BY local_artist_id, provider",
                unique_ids,
            ).fetchall()
            counts = {
                "album_credits": connection.execute(
                    "SELECT COUNT(*) FROM local_album_artists "
                    f"WHERE local_artist_id IN ({placeholders})",
                    unique_ids,
                ).fetchone()[0],
                "track_credits": connection.execute(
                    "SELECT COUNT(*) FROM local_track_artists "
                    f"WHERE local_artist_id IN ({placeholders})",
                    unique_ids,
                ).fetchone()[0],
                "primary_albums": connection.execute(
                    "SELECT COUNT(*) FROM local_albums "
                    f"WHERE album_artist_id IN ({placeholders})",
                    unique_ids,
                ).fetchone()[0],
                "migration_references": connection.execute(
                    "SELECT COUNT(*) FROM library_migration_provenance "
                    f"WHERE target_kind = 'local_artist' AND target_id IN ({placeholders})",
                    unique_ids,
                ).fetchone()[0],
                "favorites": connection.execute(
                    "SELECT COUNT(*) FROM library_user_favorites "
                    f"WHERE item_kind = 'artist' AND item_id IN ({placeholders})",
                    unique_ids,
                ).fetchone()[0],
                "playlist_snapshots": connection.execute(
                    "SELECT COUNT(*) FROM library_playlist_tracks "
                    f"WHERE local_artist_id IN ({placeholders})",
                    unique_ids,
                ).fetchone()[0],
                "history": connection.execute(
                    "SELECT COUNT(*) FROM library_play_history "
                    f"WHERE local_artist_id IN ({placeholders})",
                    unique_ids,
                ).fetchone()[0],
                "compatibility_ids": connection.execute(
                    "SELECT COUNT(*) FROM library_compat_id_map "
                    f"WHERE kind = 'artist' AND internal_id IN ({placeholders})",
                    unique_ids,
                ).fetchone()[0],
            }
            return {
                "artists": [dict(row) for row in artists],
                "identities": [dict(row) for row in identities],
                "reference_counts": {key: int(value) for key, value in counts.items()},
            }

        return await self._read(operation)

    async def list_identification_reviews(
        self,
        *,
        limit: int,
        cursor_updated_at: float | str | int | None = None,
        cursor_id: str | None = None,
        sort: str = "newest",
        state: str | None = None,
        reason_code: str | None = None,
        root_id: str | None = None,
        policy: str | None = None,
        search: str | None = None,
        metadata_incomplete: bool | None = None,
        candidate_available: bool | None = None,
        job_state: str | None = None,
        created_from: float | None = None,
        created_to: float | None = None,
        updated_from: float | None = None,
        updated_to: float | None = None,
    ) -> dict[str, Any]:
        def operation(connection: sqlite3.Connection) -> dict[str, Any]:
            sort_options = {
                "newest": ("r.updated_at", "DESC"),
                "oldest": ("r.updated_at", "ASC"),
                "album": (
                    "COALESCE(a.title_folded, t.album_title_folded, '')",
                    "ASC",
                ),
                "artist": (
                    "COALESCE(a.album_artist_name_folded, t.artist_name_folded, '')",
                    "ASC",
                ),
                "root": ("COALESCE(a.root_id, t.root_id, '')", "ASC"),
                "track_count": ("COALESCE(stats.track_count, 1)", "DESC"),
                "reason": ("r.reason_code", "ASC"),
            }
            if sort not in sort_options:
                raise ValueError(f"Unsupported review sort: {sort}")
            sort_expression, direction = sort_options[sort]
            clauses = ["1 = 1"]
            parameters: list[Any] = []
            count_clauses = ["1 = 1"]
            count_parameters: list[Any] = []
            if cursor_updated_at is not None and cursor_id is not None:
                operator = "<" if direction == "DESC" else ">"
                clauses.append(
                    f"({sort_expression} {operator} ? OR "
                    f"({sort_expression} = ? AND r.id {operator} ?))"
                )
                parameters.extend((cursor_updated_at, cursor_updated_at, cursor_id))
            if state is not None:
                clauses.append("r.state = ?")
                parameters.append(state)
                count_clauses.append("r.state = ?")
                count_parameters.append(state)
            if reason_code is not None:
                clauses.append("r.reason_code = ?")
                parameters.append(reason_code)
                count_clauses.append("r.reason_code = ?")
                count_parameters.append(reason_code)
            if root_id is not None:
                clauses.append("COALESCE(a.root_id, t.root_id) = ?")
                parameters.append(root_id)
                count_clauses.append("COALESCE(a.root_id, t.root_id) = ?")
                count_parameters.append(root_id)
            if policy is not None:
                clauses.append("COALESCE(t.applied_policy, at.applied_policy) = ?")
                parameters.append(policy)
                count_clauses.append(
                    "COALESCE(t.applied_policy, at.applied_policy) = ?"
                )
                count_parameters.append(policy)
            if metadata_incomplete is not None:
                clause = (
                    "COALESCE(stats.metadata_incomplete_count, t.metadata_incomplete, 0) "
                    + ("> 0" if metadata_incomplete else "= 0")
                )
                clauses.append(clause)
                count_clauses.append(clause)
            if candidate_available is not None:
                clause = "COALESCE(attempt.candidate_count, 0) " + (
                    "> 0" if candidate_available else "= 0"
                )
                clauses.append(clause)
                count_clauses.append(clause)
            if job_state is not None:
                clauses.append("job.state = ?")
                parameters.append(job_state)
                count_clauses.append("job.state = ?")
                count_parameters.append(job_state)
            if created_from is not None:
                clauses.append("r.created_at >= ?")
                parameters.append(created_from)
                count_clauses.append("r.created_at >= ?")
                count_parameters.append(created_from)
            if created_to is not None:
                clauses.append("r.created_at <= ?")
                parameters.append(created_to)
                count_clauses.append("r.created_at <= ?")
                count_parameters.append(created_to)
            if updated_from is not None:
                clauses.append("r.updated_at >= ?")
                parameters.append(updated_from)
                count_clauses.append("r.updated_at >= ?")
                count_parameters.append(updated_from)
            if updated_to is not None:
                clauses.append("r.updated_at <= ?")
                parameters.append(updated_to)
                count_clauses.append("r.updated_at <= ?")
                count_parameters.append(updated_to)
            if search:
                folded = f"%{_fold(search) or ''}%"
                search_clause = (
                    "(a.title_folded LIKE ? OR a.album_artist_name_folded LIKE ? "
                    "OR t.title_folded LIKE ? OR t.relative_path LIKE ?)"
                )
                clauses.append(search_clause)
                parameters.extend((folded, folded, folded, folded))
                count_clauses.append(search_clause)
                count_parameters.extend((folded, folded, folded, folded))
            where = " AND ".join(clauses)
            base = (
                " FROM library_identification_reviews r "
                "LEFT JOIN local_albums a ON a.id = r.local_album_id "
                "LEFT JOIN local_tracks t ON t.id = r.local_track_id "
                "LEFT JOIN local_tracks at ON at.id = (SELECT id FROM local_tracks "
                "WHERE local_album_id = a.id ORDER BY id LIMIT 1) "
                "LEFT JOIN (SELECT local_album_id, COUNT(*) track_count, "
                "SUM(metadata_incomplete) metadata_incomplete_count, MIN(relative_path) relative_path "
                "FROM local_tracks GROUP BY local_album_id) stats ON stats.local_album_id = a.id "
                "LEFT JOIN local_album_external_identities identity ON identity.local_album_id = a.id "
                "LEFT JOIN library_identification_attempts attempt ON attempt.id = r.attempt_id "
                "LEFT JOIN library_identification_jobs job ON job.id = (SELECT id FROM "
                "library_identification_jobs WHERE local_album_id = a.id "
                "AND state IN ('queued','running','paused') ORDER BY updated_at DESC LIMIT 1)"
            )
            rows = connection.execute(
                "SELECT r.*, a.title album_title, a.album_artist_name, a.year, "
                "COALESCE(a.root_id, t.root_id) root_id, "
                "COALESCE(stats.relative_path, t.relative_path, '') relative_path, "
                "COALESCE(stats.track_count, 1) track_count, "
                "COALESCE(stats.metadata_incomplete_count, t.metadata_incomplete, 0) metadata_incomplete_count, "
                "COALESCE(t.applied_policy, at.applied_policy, 'automatic') effective_policy, "
                "COALESCE(t.manual_excluded, at.manual_excluded, 0) manual_excluded, "
                "identity.release_group_mbid, identity.decision_source identity_source, "
                "COALESCE(attempt.candidate_count, 0) candidate_count, job.state active_job_state, "
                f"{sort_expression} cursor_value"
                + base
                + f" WHERE {where} ORDER BY {sort_expression} {direction}, r.id {direction} LIMIT ?",
                (*parameters, limit + 1),
            ).fetchall()
            total = connection.execute(
                "SELECT COUNT(*)" + base + f" WHERE {' AND '.join(count_clauses)}",
                count_parameters,
            ).fetchone()[0]
            state_counts = dict(
                connection.execute(
                    "SELECT state, COUNT(*) FROM library_identification_reviews "
                    "GROUP BY state"
                ).fetchall()
            )
            reason_counts = dict(
                connection.execute(
                    "SELECT reason_code, COUNT(*) FROM library_identification_reviews "
                    "GROUP BY reason_code"
                ).fetchall()
            )
            return {
                "rows": [dict(row) for row in rows[:limit]],
                "has_more": len(rows) > limit,
                "filtered_total": int(total),
                "counts_by_state": state_counts,
                "counts_by_reason": reason_counts,
            }

        return await self._read(operation)

    async def cleanup_bulk_review_preview_batch(
        self,
        *,
        now: float,
        batch_size: int = BULK_PREVIEW_CLEANUP_BATCH_SIZE,
    ) -> dict[str, int]:
        limit = min(max(batch_size, 1), BULK_PREVIEW_CLEANUP_BATCH_SIZE)

        def operation(connection: sqlite3.Connection) -> dict[str, int]:
            deleted_subjects = connection.execute(
                "DELETE FROM library_bulk_review_preview_subjects WHERE rowid IN ("
                "SELECT subject.rowid FROM library_bulk_review_preview_subjects subject "
                "JOIN library_bulk_review_previews preview "
                "ON preview.preview_token = subject.preview_token "
                "WHERE preview.expires_at < ? AND NOT EXISTS ("
                "SELECT 1 FROM library_bulk_review_snapshots snapshot "
                "WHERE snapshot.preview_token = preview.preview_token "
                "AND snapshot.staging_state = 'staging') "
                "ORDER BY preview.expires_at, subject.rowid LIMIT ?)",
                (now, limit),
            ).rowcount
            deleted_previews = connection.execute(
                "DELETE FROM library_bulk_review_previews WHERE preview_token IN ("
                "SELECT preview.preview_token FROM library_bulk_review_previews preview "
                "WHERE preview.expires_at < ? AND NOT EXISTS ("
                "SELECT 1 FROM library_bulk_review_preview_subjects subject "
                "WHERE subject.preview_token = preview.preview_token) AND NOT EXISTS ("
                "SELECT 1 FROM library_bulk_review_snapshots snapshot "
                "WHERE snapshot.preview_token = preview.preview_token "
                "AND snapshot.staging_state = 'staging') "
                "ORDER BY preview.expires_at, preview.preview_token LIMIT ?)",
                (now, limit),
            ).rowcount
            return {
                "deleted_subjects": int(deleted_subjects),
                "deleted_previews": int(deleted_previews),
            }

        return await super()._background_write(operation)

    async def create_bulk_review_preview(
        self,
        *,
        review_ids: list[str],
        normalized_filter: dict[str, str],
        preview_token: str,
        action: str,
        selection: dict[str, Any],
        catalog_revision: int | None,
        created_at: float,
        expires_at: float,
    ) -> None:
        await self.cleanup_bulk_review_preview_batch(now=created_at)

        def operation(connection: sqlite3.Connection) -> None:
            summary = {
                "eligible_count": 0,
                "reasons": {},
                "album_count": 0,
                "track_count": 0,
                "_roots": [],
                "_policies": [],
                "_common_candidate_keys": None,
                "_stale_revisions": 0,
            }
            connection.execute(
                "INSERT INTO library_bulk_review_previews "
                "(preview_token, action, selection_json, normalized_filter_json, "
                "catalog_revision, requires_local_metadata_confirmation, state, "
                "summary_json, subject_count, created_at, expires_at) "
                "VALUES (?,?,?,?,?,0,'staging',?,0,?,?)",
                (
                    preview_token,
                    action,
                    json.dumps(selection, sort_keys=True),
                    json.dumps(normalized_filter, sort_keys=True)
                    if normalized_filter
                    else None,
                    catalog_revision,
                    json.dumps(summary, sort_keys=True),
                    created_at,
                    expires_at,
                ),
            )

        await super()._background_write(operation)

    async def stage_bulk_review_preview_batch(
        self,
        preview_token: str,
        *,
        batch_size: int = BULK_PREVIEW_BATCH_SIZE,
    ) -> dict[str, Any]:
        limit = min(max(batch_size, 1), BULK_PREVIEW_BATCH_SIZE)

        def operation(connection: sqlite3.Connection) -> dict[str, Any]:
            preview = connection.execute(
                "SELECT * FROM library_bulk_review_previews WHERE preview_token = ?",
                (preview_token,),
            ).fetchone()
            if preview is None:
                raise StaleRevisionError("The bulk preview no longer exists.")
            if str(preview["state"]) == "ready":
                return {
                    "complete": True,
                    "staged_count": 0,
                    "summary": json.loads(str(preview["summary_json"])),
                }
            action = str(preview["action"])
            selection = json.loads(str(preview["selection_json"]))
            review_ids = [str(value) for value in selection.get("review_ids", [])]
            expected_revisions = {
                str(key): int(value)
                for key, value in selection.get("expected_revisions", {}).items()
            }
            normalized_filter = json.loads(
                str(preview["normalized_filter_json"] or "{}")
            )
            candidate_key = selection.get("candidate_key")
            clauses: list[str] = []
            parameters: list[Any] = []
            if review_ids:
                clauses.append("r.id IN (SELECT value FROM json_each(?))")
                parameters.append(json.dumps(review_ids))
            else:
                filter_clauses, filter_parameters = _review_filter_predicate(
                    normalized_filter
                )
                clauses.extend(filter_clauses)
                parameters.extend(filter_parameters)
            if preview["cursor_updated_at"] is not None:
                clauses.append("(r.updated_at < ? OR (r.updated_at = ? AND r.id < ?))")
                parameters.extend(
                    (
                        float(preview["cursor_updated_at"]),
                        float(preview["cursor_updated_at"]),
                        str(preview["cursor_review_id"]),
                    )
                )
            where = " AND ".join(clauses) if clauses else "0 = 1"
            raw_rows = connection.execute(
                "SELECT r.*, a.root_id, COALESCE(t.applied_policy, at.applied_policy, 'automatic') "
                "effective_policy, identity.release_group_mbid, "
                "COALESCE(attempt.candidate_count, 0) candidate_count "
                "FROM library_identification_reviews r "
                "LEFT JOIN local_albums a ON a.id = r.local_album_id "
                "LEFT JOIN local_tracks t ON t.id = r.local_track_id "
                "LEFT JOIN local_tracks at ON at.id = (SELECT id FROM local_tracks "
                "WHERE local_album_id = a.id ORDER BY id LIMIT 1) "
                "LEFT JOIN local_album_external_identities identity ON identity.local_album_id = a.id "
                "LEFT JOIN library_identification_attempts attempt ON attempt.id = r.attempt_id "
                "LEFT JOIN library_identification_jobs job ON job.id = (SELECT id FROM "
                "library_identification_jobs WHERE local_album_id = a.id "
                "AND state IN ('queued','running','paused') ORDER BY updated_at DESC LIMIT 1) "
                f"WHERE {where} ORDER BY r.updated_at DESC, r.id DESC LIMIT ?",
                (*parameters, limit + 1),
            ).fetchall()
            page = [dict(row) for row in raw_rows[:limit]]
            album_ids = sorted(
                {
                    str(row["local_album_id"])
                    for row in page
                    if row["local_album_id"] is not None
                }
            )
            track_counts: dict[str, int] = {}
            if album_ids:
                track_counts = {
                    str(row["local_album_id"]): int(row["track_count"])
                    for row in connection.execute(
                        "SELECT local_album_id, COUNT(*) track_count FROM local_tracks "
                        "WHERE local_album_id IN (SELECT value FROM json_each(?)) "
                        "GROUP BY local_album_id",
                        (json.dumps(album_ids),),
                    ).fetchall()
                }
            for row in page:
                row["track_count"] = track_counts.get(str(row["local_album_id"]), 1)
            attempt_ids = sorted(
                {
                    str(row["attempt_id"])
                    for row in page
                    if row["attempt_id"] is not None
                }
            )
            evidence_by_attempt: dict[str, list[tuple[str, CandidateEvidence]]] = {}
            if attempt_ids:
                evidence_rows = connection.execute(
                    "SELECT attempt_id, candidate_key, evidence_json "
                    "FROM library_identification_evidence "
                    "WHERE attempt_id IN (SELECT value FROM json_each(?)) "
                    "ORDER BY attempt_id, candidate_key",
                    (json.dumps(attempt_ids),),
                ).fetchall()
                for evidence_row in evidence_rows:
                    evidence_by_attempt.setdefault(
                        str(evidence_row["attempt_id"]), []
                    ).append(
                        (
                            str(evidence_row["candidate_key"]),
                            msgspec.json.decode(
                                bytes(evidence_row["evidence_json"]),
                                type=CandidateEvidence,
                            ),
                        )
                    )

            summary = json.loads(str(preview["summary_json"]))
            roots = set(summary.pop("_roots", []))
            policies = set(summary.pop("_policies", []))
            common_raw = summary.pop("_common_candidate_keys", None)
            common_candidates = set(common_raw) if common_raw is not None else None
            stale_revisions = int(summary.pop("_stale_revisions", 0))
            reasons = {
                str(key): int(value)
                for key, value in summary.get("reasons", {}).items()
            }
            requires_local_metadata = bool(
                preview["requires_local_metadata_confirmation"]
            )
            for row in page:
                evidence = evidence_by_attempt.get(str(row["attempt_id"]), [])
                safe_candidates = {
                    key
                    for key, item in evidence
                    if item.reason_code in AUTOMATIC_SAFE_EVIDENCE_REASONS
                }
                common_candidates = (
                    safe_candidates
                    if common_candidates is None
                    else common_candidates & safe_candidates
                )
                selected_evidence = next(
                    (item for key, item in evidence if key == candidate_key), None
                )
                reason = _bulk_preview_ineligibility(
                    action, row, candidate_key, selected_evidence
                )
                if reason is None:
                    summary["eligible_count"] = int(summary["eligible_count"]) + 1
                else:
                    reasons[reason] = reasons.get(reason, 0) + 1
                summary["album_count"] = int(summary["album_count"]) + int(
                    row["local_album_id"] is not None
                )
                summary["track_count"] = int(summary["track_count"]) + int(
                    row["track_count"]
                )
                roots.add(str(row["root_id"]))
                policy = str(row["effective_policy"])
                policies.add(policy)
                requires_local_metadata = requires_local_metadata or (
                    action == "retry" and policy == "local_metadata"
                )
                if review_ids and int(row["row_revision"]) != expected_revisions.get(
                    str(row["id"]), -1
                ):
                    stale_revisions += 1

            start_ordinal = int(preview["subject_count"])
            connection.executemany(
                "INSERT INTO library_bulk_review_preview_subjects "
                "(preview_token, ordinal, review_id, local_album_id, local_track_id, "
                "expected_subject_revision, expected_input_revision) "
                "VALUES (?,?,?,?,?,?,?)",
                [
                    (
                        preview_token,
                        start_ordinal + ordinal,
                        row["id"],
                        row["local_album_id"],
                        row["local_track_id"],
                        row["row_revision"],
                        row["input_revision"],
                    )
                    for ordinal, row in enumerate(page)
                ],
            )
            complete = len(raw_rows) <= limit
            subject_count = start_ordinal + len(page)
            summary["reasons"] = reasons
            summary["_roots"] = sorted(roots)
            summary["_policies"] = sorted(policies)
            summary["_common_candidate_keys"] = (
                sorted(common_candidates) if common_candidates is not None else None
            )
            summary["_stale_revisions"] = stale_revisions
            if complete:
                missing = (
                    max(0, len(set(review_ids)) - subject_count) if review_ids else 0
                )
                references = {"playlists": 0, "history": 0}
                if action == "exclude" and subject_count:
                    predicate = (
                        "EXISTS (SELECT 1 FROM library_bulk_review_preview_subjects subject "
                        "WHERE subject.preview_token = ? AND ("
                        "subject.local_album_id = target.local_album_id OR "
                        "subject.local_track_id = target.local_track_id))"
                    )
                    references = {
                        "playlists": int(
                            connection.execute(
                                "SELECT COUNT(*) FROM library_playlist_tracks target WHERE "
                                + predicate,
                                (preview_token,),
                            ).fetchone()[0]
                        ),
                        "history": int(
                            connection.execute(
                                "SELECT COUNT(*) FROM library_play_history target WHERE "
                                + predicate,
                                (preview_token,),
                            ).fetchone()[0]
                        ),
                    }
                final_summary = {
                    "eligible_count": int(summary["eligible_count"]),
                    "ineligible_count": sum(reasons.values()),
                    "stale_count": stale_revisions + missing,
                    "reasons": reasons,
                    "album_count": int(summary["album_count"]),
                    "track_count": int(summary["track_count"]),
                    "root_count": len(roots),
                    "crosses_policy_boundaries": len(policies) > 1,
                    "estimated_job_count": int(summary["eligible_count"])
                    if action == "retry"
                    else 0,
                    "playlist_reference_count": references["playlists"],
                    "history_reference_count": references["history"],
                    "requires_local_metadata_confirmation": requires_local_metadata,
                    "common_candidate_keys": sorted(common_candidates or set()),
                }
                connection.execute(
                    "UPDATE library_bulk_review_previews SET state = 'ready', "
                    "summary_json = ?, subject_count = ?, "
                    "requires_local_metadata_confirmation = ? "
                    "WHERE preview_token = ? AND state = 'staging'",
                    (
                        json.dumps(final_summary, sort_keys=True),
                        subject_count,
                        int(requires_local_metadata),
                        preview_token,
                    ),
                )
                return {
                    "complete": True,
                    "staged_count": len(page),
                    "summary": final_summary,
                }

            cursor = page[-1]
            connection.execute(
                "UPDATE library_bulk_review_previews SET summary_json = ?, "
                "subject_count = ?, requires_local_metadata_confirmation = ?, "
                "cursor_updated_at = ?, cursor_review_id = ? "
                "WHERE preview_token = ? AND state = 'staging'",
                (
                    json.dumps(summary, sort_keys=True),
                    subject_count,
                    int(requires_local_metadata),
                    float(cursor["updated_at"]),
                    str(cursor["id"]),
                    preview_token,
                ),
            )
            return {"complete": False, "staged_count": len(page), "summary": {}}

        return await super()._background_write(operation)

    async def get_identification_review_detail(
        self, review_id: str
    ) -> dict[str, Any] | None:
        def operation(connection: sqlite3.Connection) -> dict[str, Any] | None:
            review = connection.execute(
                "SELECT * FROM library_identification_reviews WHERE id = ?",
                (review_id,),
            ).fetchone()
            if review is None:
                return None
            album_id = review["local_album_id"]
            tracks = connection.execute(
                "SELECT t.*, i.recording_mbid, "
                "(SELECT ta.local_artist_id FROM local_track_artists ta "
                "WHERE ta.local_track_id = t.id ORDER BY ta.position LIMIT 1) AS local_artist_id "
                "FROM local_tracks t "
                "LEFT JOIN local_track_external_identities i ON i.local_track_id = t.id "
                "WHERE t.local_album_id = COALESCE(?, t.local_album_id) "
                "AND (? IS NULL OR t.id = ?) ORDER BY t.disc_number, t.track_number, t.id",
                (album_id, review["local_track_id"], review["local_track_id"]),
            ).fetchall()
            album = (
                connection.execute(
                    "SELECT * FROM local_albums WHERE id = ?", (album_id,)
                ).fetchone()
                if album_id is not None
                else None
            )
            identity = (
                connection.execute(
                    "SELECT * FROM local_album_external_identities WHERE local_album_id = ?",
                    (album_id,),
                ).fetchone()
                if album_id is not None
                else None
            )
            job = (
                connection.execute(
                    "SELECT * FROM library_identification_jobs WHERE local_album_id = ? "
                    "ORDER BY updated_at DESC LIMIT 1",
                    (album_id,),
                ).fetchone()
                if album_id is not None
                else None
            )
            attempts = connection.execute(
                "SELECT * FROM library_identification_attempts WHERE "
                "(local_album_id = ? AND ? IS NOT NULL) OR (local_track_id = ? AND ? IS NOT NULL) "
                "ORDER BY completed_at DESC LIMIT 25",
                (
                    album_id,
                    album_id,
                    review["local_track_id"],
                    review["local_track_id"],
                ),
            ).fetchall()
            actions = connection.execute(
                "SELECT * FROM library_catalog_actions WHERE "
                "(local_album_id = ? AND ? IS NOT NULL) OR (local_track_id = ? AND ? IS NOT NULL) "
                "ORDER BY created_at DESC LIMIT 25",
                (
                    album_id,
                    album_id,
                    review["local_track_id"],
                    review["local_track_id"],
                ),
            ).fetchall()
            evidence = connection.execute(
                "SELECT e.* FROM library_identification_evidence e "
                "WHERE e.attempt_id = ? ORDER BY e.candidate_key",
                (review["attempt_id"],),
            ).fetchall()
            return {
                "review": dict(review),
                "album": _row(album),
                "tracks": [dict(row) for row in tracks],
                "identity": _row(identity),
                "job": _row(job),
                "attempts": [dict(row) for row in attempts],
                "actions": [dict(row) for row in actions],
                "evidence": [dict(row) for row in evidence],
            }

        return await self._read(operation)

    async def apply_review_decision(
        self,
        review_id: str,
        *,
        action: str,
        actor_user_id: str,
        expected_review_revision: int,
        expected_catalog_revision: int,
        expected_identity_revision: int | None,
        action_id: str,
        idempotency_key: str | None,
        now: float,
    ) -> dict[str, Any]:
        def operation(connection: sqlite3.Connection) -> dict[str, Any]:
            existing_action = None
            if idempotency_key:
                existing_action = connection.execute(
                    "SELECT * FROM library_catalog_actions WHERE idempotency_key = ?",
                    (idempotency_key,),
                ).fetchone()
            if existing_action is not None:
                review = connection.execute(
                    "SELECT * FROM library_identification_reviews WHERE id = ?",
                    (review_id,),
                ).fetchone()
                if review is None:
                    raise ResourceNotFoundError("Review item not found.")
                return {
                    "review": dict(review),
                    "action_id": existing_action["id"],
                    "catalog_revision": connection.execute(
                        "SELECT value FROM library_catalog_revision WHERE singleton = 1"
                    ).fetchone()[0],
                    "remaining_exclusion_source": (
                        "directory_policy"
                        if action == "restore"
                        and connection.execute(
                            "SELECT 1 FROM local_tracks WHERE "
                            "((local_album_id = ? AND ? IS NOT NULL) OR (id = ? AND ? IS NOT NULL)) "
                            "AND applied_policy = 'excluded' LIMIT 1",
                            (
                                review["local_album_id"],
                                review["local_album_id"],
                                review["local_track_id"],
                                review["local_track_id"],
                            ),
                        ).fetchone()
                        is not None
                        else None
                    ),
                }
            current_catalog = connection.execute(
                "SELECT value FROM library_catalog_revision WHERE singleton = 1"
            ).fetchone()[0]
            if int(current_catalog) != expected_catalog_revision:
                raise StaleRevisionError(
                    "The catalog changed before this review action was applied."
                )
            review = connection.execute(
                "SELECT * FROM library_identification_reviews WHERE id = ?",
                (review_id,),
            ).fetchone()
            if review is None:
                raise ResourceNotFoundError("Review item not found.")
            if int(review["row_revision"]) != expected_review_revision:
                raise StaleRevisionError(
                    "The review item changed before this action was applied."
                )
            album_id = review["local_album_id"]
            track_id = review["local_track_id"]
            identity = None
            if album_id is not None:
                identity = connection.execute(
                    "SELECT * FROM local_album_external_identities WHERE local_album_id = ?",
                    (album_id,),
                ).fetchone()
            if action == "keep_tagged" and identity is not None:
                raise StaleRevisionError(
                    "Detach the external identity before keeping this album as tagged."
                )
            if action == "detach_keep_tagged":
                if identity is None:
                    raise StaleRevisionError(
                        "The external identity is no longer attached."
                    )
                if (
                    expected_identity_revision is None
                    or int(identity["row_revision"]) != expected_identity_revision
                ):
                    raise StaleRevisionError(
                        "The external identity changed before detachment."
                    )
                connection.execute(
                    "DELETE FROM local_track_external_identities WHERE local_track_id IN "
                    "(SELECT id FROM local_tracks WHERE local_album_id = ?)",
                    (album_id,),
                )
                connection.execute(
                    "DELETE FROM local_album_external_identities WHERE local_album_id = ?",
                    (album_id,),
                )
            new_state = {
                "keep_tagged": "keep_tagged",
                "detach_keep_tagged": "keep_tagged",
                "exclude": "excluded",
                "restore": "resolved",
            }.get(action, action)
            if action == "exclude":
                if album_id is not None:
                    connection.execute(
                        "UPDATE local_tracks SET manual_excluded = 1, availability = 'excluded', "
                        "excluded_at = ?, row_revision = row_revision + 1 WHERE local_album_id = ?",
                        (now, album_id),
                    )
                else:
                    connection.execute(
                        "UPDATE local_tracks SET manual_excluded = 1, availability = 'excluded', "
                        "excluded_at = ?, row_revision = row_revision + 1 WHERE id = ?",
                        (now, track_id),
                    )
            elif action == "restore":
                target_clause = (
                    "local_album_id = ?" if album_id is not None else "id = ?"
                )
                target_id = album_id or track_id
                connection.execute(
                    "UPDATE local_tracks SET manual_excluded = 0, "
                    "availability = CASE WHEN applied_policy = 'excluded' THEN 'excluded' ELSE 'indexed' END, "
                    "excluded_at = CASE WHEN applied_policy = 'excluded' THEN excluded_at ELSE NULL END, "
                    f"row_revision = row_revision + 1 WHERE {target_clause}",
                    (target_id,),
                )
                new_state = "resolved"
            connection.execute(
                "UPDATE library_identification_jobs SET state = 'cancelled', "
                "last_failure_code = 'ADMIN_DECISION', terminal_at = ?, updated_at = ?, "
                "row_revision = row_revision + 1, event_revision = event_revision + 1 "
                "WHERE state IN ('queued','paused') AND kind = 'automatic' AND "
                "((local_album_id = ? AND ? IS NOT NULL) OR (local_track_id = ? AND ? IS NOT NULL))",
                (now, now, album_id, album_id, track_id, track_id),
            )
            updated = connection.execute(
                "UPDATE library_identification_reviews SET state = ?, reason_code = ?, "
                "decided_by_user_id = ?, decided_at = ?, updated_at = ?, "
                "decision_revision = decision_revision + 1, row_revision = row_revision + 1 "
                "WHERE id = ? AND row_revision = ? AND row_revision < ? RETURNING *",
                (
                    new_state,
                    action.upper(),
                    actor_user_id,
                    now,
                    now,
                    review_id,
                    expected_review_revision,
                    MAX_REVISION,
                ),
            ).fetchone()
            if updated is None:
                raise StaleRevisionError(
                    "The review item changed before this action was applied."
                )
            connection.execute(
                "INSERT INTO library_catalog_actions "
                "(id, idempotency_key, actor_user_id, action_kind, local_album_id, "
                "local_track_id, before_json, after_json, reason_code, created_at) "
                "VALUES (?,?,?,?,?,?,?,?,?,?)",
                (
                    action_id,
                    idempotency_key,
                    actor_user_id,
                    action,
                    album_id,
                    track_id,
                    json.dumps(
                        {
                            "review_state": review["state"],
                            "identity": dict(identity) if identity else None,
                        },
                        sort_keys=True,
                    ),
                    json.dumps({"review_state": new_state}, sort_keys=True),
                    action.upper(),
                    now,
                ),
            )
            catalog_revision = self._bump_catalog(connection)
            self._bump_stream(connection, "identification")
            remaining_exclusion_source = None
            if action == "restore":
                remaining = connection.execute(
                    "SELECT 1 FROM local_tracks WHERE "
                    "((local_album_id = ? AND ? IS NOT NULL) OR (id = ? AND ? IS NOT NULL)) "
                    "AND applied_policy = 'excluded' LIMIT 1",
                    (album_id, album_id, track_id, track_id),
                ).fetchone()
                if remaining is not None:
                    remaining_exclusion_source = "directory_policy"
            return {
                "review": dict(updated),
                "action_id": action_id,
                "catalog_revision": catalog_revision,
                "remaining_exclusion_source": remaining_exclusion_source,
            }

        return await self._write(operation)

    async def accept_review_candidate(
        self,
        review_id: str,
        *,
        candidate_key: str,
        manual_override: bool,
        actor_user_id: str,
        expected_review_revision: int,
        expected_catalog_revision: int,
        expected_evidence_revision: str,
        action_id: str,
        idempotency_key: str | None,
        now: float,
    ) -> dict[str, Any]:
        def operation(connection: sqlite3.Connection) -> dict[str, Any]:
            if idempotency_key:
                existing = connection.execute(
                    "SELECT * FROM library_catalog_actions WHERE idempotency_key = ?",
                    (idempotency_key,),
                ).fetchone()
                if existing is not None:
                    review = connection.execute(
                        "SELECT * FROM library_identification_reviews WHERE id = ?",
                        (review_id,),
                    ).fetchone()
                    return {
                        "review": dict(review),
                        "action_id": existing["id"],
                        "catalog_revision": connection.execute(
                            "SELECT value FROM library_catalog_revision WHERE singleton = 1"
                        ).fetchone()[0],
                    }
            catalog_revision = connection.execute(
                "SELECT value FROM library_catalog_revision WHERE singleton = 1"
            ).fetchone()[0]
            if int(catalog_revision) != expected_catalog_revision:
                raise StaleRevisionError(
                    "The catalog changed before candidate acceptance."
                )
            review = connection.execute(
                "SELECT * FROM library_identification_reviews WHERE id = ?",
                (review_id,),
            ).fetchone()
            if review is None:
                raise ResourceNotFoundError("Review item not found.")
            if (
                int(review["row_revision"]) != expected_review_revision
                or review["local_album_id"] is None
            ):
                raise StaleRevisionError(
                    "The review changed before candidate acceptance."
                )
            evidence_row = connection.execute(
                "SELECT * FROM library_identification_evidence WHERE id = ? AND attempt_id = ? "
                "AND candidate_key = ?",
                (expected_evidence_revision, review["attempt_id"], candidate_key),
            ).fetchone()
            if evidence_row is None:
                raise StaleRevisionError(
                    "The candidate evidence changed before acceptance."
                )
            evidence = msgspec.json.decode(
                bytes(evidence_row["evidence_json"]), type=CandidateEvidence
            )
            if (
                evidence.reason_code not in AUTOMATIC_SAFE_EVIDENCE_REASONS
                and not manual_override
            ):
                raise StaleRevisionError(
                    "This candidate no longer passes every automatic safety gate."
                )
            connection.execute(
                "INSERT INTO local_album_external_identities "
                "(local_album_id, provider, release_group_mbid, release_mbid, decision_source, "
                "matcher_version, attempt_id, selected_by_user_id, selected_at) "
                "VALUES (?, 'musicbrainz', ?, ?, 'manual', ?, ?, ?, ?) "
                "ON CONFLICT(local_album_id, provider) DO UPDATE SET "
                "release_group_mbid = excluded.release_group_mbid, release_mbid = excluded.release_mbid, "
                "decision_source = 'manual', matcher_version = excluded.matcher_version, "
                "attempt_id = excluded.attempt_id, selected_by_user_id = excluded.selected_by_user_id, "
                "selected_at = excluded.selected_at, row_revision = row_revision + 1",
                (
                    review["local_album_id"],
                    evidence.release_group_mbid,
                    evidence.release_mbid,
                    evidence.matcher_version,
                    review["attempt_id"],
                    actor_user_id,
                    now,
                ),
            )
            connection.execute(
                "DELETE FROM local_track_external_identities WHERE local_track_id IN "
                "(SELECT id FROM local_tracks WHERE local_album_id = ?)",
                (review["local_album_id"],),
            )
            for track in evidence.track_evidence:
                if track.classification == "supported" and track.recording_mbid:
                    connection.execute(
                        "INSERT INTO local_track_external_identities "
                        "(local_track_id, provider, recording_mbid, release_mbid, decision_source, "
                        "attempt_id, selected_at) VALUES (?, 'musicbrainz', ?, ?, 'manual', ?, ?)",
                        (
                            track.local_track_id,
                            track.recording_mbid,
                            evidence.release_mbid,
                            review["attempt_id"],
                            now,
                        ),
                    )
            updated = connection.execute(
                "UPDATE library_identification_reviews SET state = 'resolved', reason_code = ?, "
                "decided_by_user_id = ?, decided_at = ?, updated_at = ?, "
                "decision_revision = decision_revision + 1, row_revision = row_revision + 1 "
                "WHERE id = ? AND row_revision = ? RETURNING *",
                (
                    "MANUAL_CANDIDATE_OVERRIDE"
                    if manual_override
                    else "SAFE_CANDIDATE_ACCEPTED",
                    actor_user_id,
                    now,
                    now,
                    review_id,
                    expected_review_revision,
                ),
            ).fetchone()
            connection.execute(
                "UPDATE library_identification_jobs SET state = 'cancelled', "
                "last_failure_code = 'ADMIN_DECISION', terminal_at = ?, updated_at = ?, "
                "row_revision = row_revision + 1, event_revision = event_revision + 1 "
                "WHERE local_album_id = ? AND kind = 'automatic' AND state IN ('queued','paused')",
                (now, now, review["local_album_id"]),
            )
            connection.execute(
                "INSERT INTO library_catalog_actions "
                "(id, idempotency_key, actor_user_id, action_kind, local_album_id, "
                "before_json, after_json, reason_code, created_at) VALUES (?,?,?,?,?,?,?,?,?)",
                (
                    action_id,
                    idempotency_key,
                    actor_user_id,
                    "accept_candidate",
                    review["local_album_id"],
                    json.dumps({"review_state": review["state"]}),
                    json.dumps(
                        {
                            "candidate_key": candidate_key,
                            "manual_override": manual_override,
                        }
                    ),
                    "MANUAL_CANDIDATE_OVERRIDE"
                    if manual_override
                    else "SAFE_CANDIDATE_ACCEPTED",
                    now,
                ),
            )
            new_catalog_revision = self._bump_catalog(connection)
            self._bump_stream(connection, "identification")
            return {
                "review": dict(updated),
                "action_id": action_id,
                "catalog_revision": new_catalog_revision,
            }

        return await self._write(operation)

    async def create_operation_with_work(
        self, job: OperationJob, work: list[OperationWorkItem]
    ) -> None:
        def operation(connection: sqlite3.Connection) -> None:
            connection.execute(
                "INSERT INTO library_operation_jobs "
                "(id, kind, state, requested_by_user_id, input_catalog_revision, "
                "expected_work_count, idempotency_key, created_at, updated_at, "
                "row_revision, event_revision) VALUES (?,?,?,?,?,?,?,?,?,?,?)",
                (
                    job.id,
                    job.kind,
                    job.state,
                    job.requested_by_user_id,
                    job.input_catalog_revision,
                    len(work),
                    job.idempotency_key,
                    job.created_at,
                    job.created_at,
                    job.row_revision,
                    job.event_revision,
                ),
            )
            connection.executemany(
                "INSERT INTO library_operation_work "
                "(job_id, ordinal, local_album_id, local_track_id, expected_subject_revision, "
                "expected_input_revision, action, idempotency_key, updated_at) "
                "VALUES (?,?,?,?,?,?,?,?,?)",
                [
                    (
                        job.id,
                        item.ordinal,
                        item.local_album_id,
                        item.local_track_id,
                        item.expected_subject_revision,
                        item.expected_input_revision,
                        item.action,
                        item.idempotency_key,
                        job.created_at,
                    )
                    for item in work
                ],
            )

        await self._write(operation)

    async def materialize_bulk_review_operation(
        self,
        job: OperationJob,
        *,
        action: str,
        review_ids: list[str],
        expected_revisions: dict[str, int],
        normalized_filter: dict[str, str],
        preview_token: str,
        preview_action: str,
        candidate_key: str | None,
        confirm_local_metadata: bool,
        created_at: float,
    ) -> dict[str, Any]:
        """Create the header; work is staged separately in restart-safe batches."""

        def operation(connection: sqlite3.Connection) -> dict[str, Any]:
            if job.idempotency_key:
                existing = connection.execute(
                    "SELECT * FROM library_operation_jobs WHERE idempotency_key = ?",
                    (job.idempotency_key,),
                ).fetchone()
                if existing is not None:
                    return dict(existing)
            expected_selection = {
                "review_ids": review_ids,
                "expected_revisions": expected_revisions,
                "normalized_filter": normalized_filter,
                "catalog_revision": job.input_catalog_revision,
                "candidate_key": candidate_key,
            }
            preview = connection.execute(
                "SELECT * FROM library_bulk_review_previews WHERE preview_token = ?",
                (preview_token,),
            ).fetchone()
            if (
                preview is None
                or float(preview["expires_at"]) < created_at
                or str(preview["action"]) != preview_action
                or str(preview["state"]) != "ready"
                or str(preview["selection_json"])
                != json.dumps(expected_selection, sort_keys=True)
            ):
                raise StaleRevisionError(
                    "The bulk preview changed or expired before Apply."
                )
            if (
                preview_action == "retry"
                and bool(preview["requires_local_metadata_confirmation"])
                and not confirm_local_metadata
            ):
                raise StaleRevisionError(
                    "Confirm the one-off lookup for Local metadata content."
                )
            subject_count = int(preview["subject_count"])
            if subject_count == 0:
                subject_count = int(
                    connection.execute(
                        "SELECT COUNT(*) FROM library_bulk_review_preview_subjects "
                        "WHERE preview_token = ?",
                        (preview_token,),
                    ).fetchone()[0]
                )
            connection.execute(
                "INSERT INTO library_operation_jobs "
                "(id, kind, state, requested_by_user_id, input_catalog_revision, "
                "expected_work_count, idempotency_key, created_at, updated_at) "
                "VALUES (?, 'bulk_review_apply', 'queued', ?, ?, ?, ?, ?, ?)",
                (
                    job.id,
                    job.requested_by_user_id,
                    job.input_catalog_revision,
                    subject_count,
                    job.idempotency_key,
                    created_at,
                    created_at,
                ),
            )
            connection.execute(
                "INSERT INTO library_bulk_review_snapshots "
                "(job_id, action, selection_json, normalized_filter_json, preview_token, "
                "staging_state, staging_cursor, created_at) "
                "VALUES (?,?,?,?,?,'staging',-1,?)",
                (
                    job.id,
                    action,
                    json.dumps(
                        review_ids
                        if review_ids
                        else {"normalized_filter": normalized_filter},
                        sort_keys=True,
                    ),
                    json.dumps(normalized_filter, sort_keys=True)
                    if normalized_filter
                    else None,
                    preview_token,
                    created_at,
                ),
            )
            self._bump_stream(connection, "operation")
            created = connection.execute(
                "SELECT * FROM library_operation_jobs WHERE id = ?", (job.id,)
            ).fetchone()
            return dict(created)

        return await self._write(operation)

    async def stage_bulk_review_operation_batch(
        self,
        job_id: str,
        worker_id: str,
        *,
        now: float,
        batch_size: int = BULK_PREVIEW_BATCH_SIZE,
    ) -> dict[str, Any]:
        limit = min(max(batch_size, 1), BULK_PREVIEW_BATCH_SIZE)

        def operation(connection: sqlite3.Connection) -> dict[str, Any]:
            job = connection.execute(
                "SELECT id FROM library_operation_jobs WHERE id = ? AND state = 'running' "
                "AND lease_owner = ?",
                (job_id, worker_id),
            ).fetchone()
            if job is None:
                raise StaleRevisionError(
                    "The operation lease changed while its work was staged."
                )
            snapshot = connection.execute(
                "SELECT * FROM library_bulk_review_snapshots WHERE job_id = ?",
                (job_id,),
            ).fetchone()
            if snapshot is None:
                raise ResourceNotFoundError("Bulk review snapshot not found.")
            if str(snapshot["staging_state"]) == "ready":
                return {"complete": True, "staged_count": 0}
            rows = connection.execute(
                "SELECT * FROM library_bulk_review_preview_subjects "
                "WHERE preview_token = ? AND ordinal > ? ORDER BY ordinal LIMIT ?",
                (snapshot["preview_token"], snapshot["staging_cursor"], limit + 1),
            ).fetchall()
            page = rows[:limit]
            connection.executemany(
                "INSERT OR IGNORE INTO library_operation_work "
                "(job_id, ordinal, local_album_id, local_track_id, expected_subject_revision, "
                "expected_input_revision, action, idempotency_key, updated_at) "
                "VALUES (?,?,?,?,?,?,?,?,?)",
                [
                    (
                        job_id,
                        row["ordinal"],
                        row["local_album_id"],
                        row["local_track_id"],
                        row["expected_subject_revision"],
                        row["expected_input_revision"],
                        snapshot["action"],
                        f"{job_id}:{row['review_id']}:{snapshot['action']}",
                        now,
                    )
                    for row in page
                ],
            )
            complete = len(rows) <= limit
            cursor = (
                int(page[-1]["ordinal"]) if page else int(snapshot["staging_cursor"])
            )
            connection.execute(
                "UPDATE library_bulk_review_snapshots SET staging_state = ?, "
                "staging_cursor = ? WHERE job_id = ?",
                ("ready" if complete else "staging", cursor, job_id),
            )
            return {"complete": complete, "staged_count": len(page)}

        return await super()._background_write(operation)

    async def create_reidentification_operation(
        self,
        job: OperationJob,
        *,
        local_album_id: str,
        expected_album_revision: int,
        expected_input_revision: str,
        one_off_local_metadata: bool,
        review_id: str | None = None,
        expected_review_revision: int | None = None,
    ) -> dict[str, Any]:
        def operation(connection: sqlite3.Connection) -> dict[str, Any]:
            if job.idempotency_key:
                existing = connection.execute(
                    "SELECT * FROM library_operation_jobs WHERE idempotency_key = ?",
                    (job.idempotency_key,),
                ).fetchone()
                if existing is not None:
                    return dict(existing)
            album = connection.execute(
                "SELECT row_revision FROM local_albums WHERE id = ? "
                "AND retired_into_album_id IS NULL",
                (local_album_id,),
            ).fetchone()
            if album is None:
                raise ResourceNotFoundError("Local album not found.")
            if int(album["row_revision"]) != expected_album_revision:
                raise StaleRevisionError(
                    "The album changed before re-identification started."
                )
            policies = {
                str(row[0])
                for row in connection.execute(
                    "SELECT DISTINCT applied_policy FROM local_tracks WHERE local_album_id = ?",
                    (local_album_id,),
                )
            }
            if "excluded" in policies:
                raise StaleRevisionError("Excluded albums cannot be re-identified.")
            if "local_metadata" in policies and not one_off_local_metadata:
                raise StaleRevisionError(
                    "Confirm the one-off lookup for Local metadata content."
                )
            if review_id is not None:
                review = connection.execute(
                    "SELECT * FROM library_identification_reviews WHERE id = ? "
                    "AND local_album_id = ? AND state != 'resolved'",
                    (review_id, local_album_id),
                ).fetchone()
                if (
                    review is None
                    or expected_review_revision is None
                    or int(review["row_revision"]) != expected_review_revision
                ):
                    raise StaleRevisionError(
                        "The review decision changed before Retry identification."
                    )
            connection.execute(
                "INSERT INTO library_operation_jobs "
                "(id, kind, state, requested_by_user_id, expected_work_count, idempotency_key, "
                "created_at, updated_at) VALUES (?, 'explicit_reidentification', 'queued', ?, 1, ?, ?, ?)",
                (
                    job.id,
                    job.requested_by_user_id,
                    job.idempotency_key,
                    job.created_at,
                    job.created_at,
                ),
            )
            if review_id is not None:
                connection.execute(
                    "UPDATE library_identification_reviews SET state = 'resolved', "
                    "reason_code = 'EXPLICIT_RETRY', decided_by_user_id = ?, decided_at = ?, "
                    "updated_at = ?, decision_revision = decision_revision + 1, "
                    "row_revision = row_revision + 1 WHERE id = ?",
                    (
                        job.requested_by_user_id,
                        job.created_at,
                        job.created_at,
                        review_id,
                    ),
                )
            connection.execute(
                "INSERT INTO library_reidentification_snapshots "
                "(job_id, local_album_id, expected_album_revision, expected_input_revision, "
                "one_off_local_metadata, created_at) VALUES (?,?,?,?,?,?)",
                (
                    job.id,
                    local_album_id,
                    expected_album_revision,
                    expected_input_revision,
                    int(one_off_local_metadata),
                    job.created_at,
                ),
            )
            connection.execute(
                "INSERT INTO library_operation_work "
                "(job_id, ordinal, local_album_id, expected_subject_revision, expected_input_revision, "
                "action, idempotency_key, updated_at) VALUES (?,0,?,?,?,?,?,?)",
                (
                    job.id,
                    local_album_id,
                    expected_album_revision,
                    expected_input_revision,
                    "reidentify",
                    f"{job.id}:{local_album_id}",
                    job.created_at,
                ),
            )
            self._bump_stream(connection, "operation")
            return dict(
                connection.execute(
                    "SELECT * FROM library_operation_jobs WHERE id = ?", (job.id,)
                ).fetchone()
            )

        return await self._write(operation)

    async def finish_reidentification_evaluation(
        self,
        job_id: str,
        ordinal: int,
        *,
        worker_id: str,
        expected_work_revision: int,
        expected_album_revision: int,
        attempt: IdentificationAttempt,
        evidence: list[IdentificationEvidenceRecord],
        now: float,
    ) -> dict[str, Any]:
        def operation(connection: sqlite3.Connection) -> dict[str, Any]:
            snapshot = connection.execute(
                "SELECT * FROM library_reidentification_snapshots WHERE job_id = ?",
                (job_id,),
            ).fetchone()
            if snapshot is None:
                raise ResourceNotFoundError("Re-identification snapshot not found.")
            album = connection.execute(
                "SELECT row_revision FROM local_albums WHERE id = ?",
                (snapshot["local_album_id"],),
            ).fetchone()
            if album is None or int(album["row_revision"]) != expected_album_revision:
                raise StaleRevisionError("The album changed during re-identification.")
            work = connection.execute(
                "SELECT row_revision FROM library_operation_work WHERE job_id = ? AND ordinal = ? "
                "AND state = 'running' AND row_revision = ?",
                (job_id, ordinal, expected_work_revision),
            ).fetchone()
            if work is None:
                raise StaleRevisionError("The re-identification checkpoint changed.")
            connection.execute(
                "INSERT INTO library_identification_attempts "
                "(id, local_album_id, local_track_id, trigger, requested_by_user_id, "
                "input_tag_revision, input_policy_revision, input_file_revision, matcher_version, "
                "state, terminal_reason_code, selected_candidate_key, candidate_count, "
                "degradation_flags_json, started_at, completed_at) VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)",
                (
                    attempt.id,
                    attempt.local_album_id,
                    attempt.local_track_id,
                    attempt.trigger,
                    attempt.requested_by_user_id,
                    attempt.input_tag_revision,
                    attempt.input_policy_revision,
                    attempt.input_file_revision,
                    attempt.matcher_version,
                    attempt.state,
                    attempt.terminal_reason_code,
                    attempt.selected_candidate_key,
                    attempt.candidate_count,
                    json.dumps(attempt.degradation_flags),
                    attempt.started_at,
                    attempt.completed_at,
                ),
            )
            self._insert_evidence(connection, evidence)
            result = {
                "attempt_id": attempt.id,
                "candidate_keys": [record.candidate_key for record in evidence],
                "outcome": attempt.state,
                "reason_code": attempt.terminal_reason_code,
            }
            provider_unavailable = (
                not evidence
                and attempt.terminal_reason_code == "PROVIDER_TEMPORARILY_UNAVAILABLE"
            )
            connection.execute(
                "UPDATE library_operation_work SET state = ?, result_json = ?, failure_code = ?, "
                "updated_at = ?, row_revision = row_revision + 1 WHERE job_id = ? AND ordinal = ?",
                (
                    "failed" if provider_unavailable else "succeeded",
                    json.dumps(result, sort_keys=True),
                    attempt.terminal_reason_code if provider_unavailable else None,
                    now,
                    job_id,
                    ordinal,
                ),
            )
            state = (
                "failed"
                if provider_unavailable
                else ("ready" if evidence else "succeeded")
            )
            terminal_code = (
                "CANDIDATES_READY" if evidence else attempt.terminal_reason_code
            )
            connection.execute(
                "UPDATE library_reidentification_snapshots SET result_json = ? WHERE job_id = ?",
                (json.dumps(result, sort_keys=True), job_id),
            )
            updated = connection.execute(
                "UPDATE library_operation_jobs SET state = ?, completed_count = 1, "
                "succeeded_count = ?, failed_count = ?, "
                "terminal_code = ?, lease_owner = NULL, lease_expires_at = NULL, heartbeat_at = NULL, "
                "updated_at = ?, terminal_at = CASE WHEN ? IN ('succeeded','failed') THEN ? ELSE NULL END, "
                "row_revision = row_revision + 1, event_revision = event_revision + 1 "
                "WHERE id = ? AND state = 'running' AND lease_owner = ? RETURNING *",
                (
                    state,
                    int(not provider_unavailable),
                    int(provider_unavailable),
                    terminal_code,
                    now,
                    state,
                    now,
                    job_id,
                    worker_id,
                ),
            ).fetchone()
            if updated is None:
                raise StaleRevisionError(
                    "The re-identification lease changed before completion."
                )
            self._bump_stream(connection, "operation")
            return dict(updated)

        return await self._write(operation)

    async def accept_reidentification_candidate(
        self,
        job_id: str,
        *,
        expected_job_revision: int,
        candidate_key: str,
        confirmation: bool,
        actor_user_id: str,
        now: float,
    ) -> dict[str, Any]:
        def operation(connection: sqlite3.Connection) -> dict[str, Any]:
            job = connection.execute(
                "SELECT * FROM library_operation_jobs WHERE id = ? AND kind = 'explicit_reidentification'",
                (job_id,),
            ).fetchone()
            snapshot = connection.execute(
                "SELECT * FROM library_reidentification_snapshots WHERE job_id = ?",
                (job_id,),
            ).fetchone()
            if job is None or snapshot is None:
                raise ResourceNotFoundError("Re-identification job not found.")
            if (
                job["state"] != "ready"
                or int(job["row_revision"]) != expected_job_revision
            ):
                raise StaleRevisionError(
                    "The re-identification candidates changed before selection."
                )
            album = connection.execute(
                "SELECT row_revision FROM local_albums WHERE id = ?",
                (snapshot["local_album_id"],),
            ).fetchone()
            if album is None or int(album["row_revision"]) != int(
                snapshot["expected_album_revision"]
            ):
                raise StaleRevisionError(
                    "The album changed after candidates were evaluated."
                )
            result = json.loads(str(snapshot["result_json"]))
            attempt_id = str(result["attempt_id"])
            evidence_row = connection.execute(
                "SELECT * FROM library_identification_evidence WHERE attempt_id = ? "
                "AND candidate_key = ?",
                (attempt_id, candidate_key),
            ).fetchone()
            if evidence_row is None:
                raise StaleRevisionError(
                    "The selected candidate is no longer available."
                )
            evidence = msgspec.json.decode(
                bytes(evidence_row["evidence_json"]), type=CandidateEvidence
            )
            if (
                evidence.reason_code not in AUTOMATIC_SAFE_EVIDENCE_REASONS
                and not confirmation
            ):
                raise ValidationError(
                    "Confirm the conflicting candidate evidence before applying it."
                )
            connection.execute(
                "INSERT INTO local_album_external_identities "
                "(local_album_id, provider, release_group_mbid, release_mbid, decision_source, "
                "matcher_version, attempt_id, selected_by_user_id, selected_at) "
                "VALUES (?, 'musicbrainz', ?, ?, 'manual', ?, ?, ?, ?) "
                "ON CONFLICT(local_album_id, provider) DO UPDATE SET "
                "release_group_mbid = excluded.release_group_mbid, release_mbid = excluded.release_mbid, "
                "decision_source = 'manual', matcher_version = excluded.matcher_version, "
                "attempt_id = excluded.attempt_id, selected_by_user_id = excluded.selected_by_user_id, "
                "selected_at = excluded.selected_at, row_revision = row_revision + 1",
                (
                    snapshot["local_album_id"],
                    evidence.release_group_mbid,
                    evidence.release_mbid,
                    evidence.matcher_version,
                    attempt_id,
                    actor_user_id,
                    now,
                ),
            )
            connection.execute(
                "DELETE FROM local_track_external_identities WHERE local_track_id IN "
                "(SELECT id FROM local_tracks WHERE local_album_id = ?)",
                (snapshot["local_album_id"],),
            )
            for track in evidence.track_evidence:
                if track.classification == "supported" and track.recording_mbid:
                    connection.execute(
                        "INSERT INTO local_track_external_identities "
                        "(local_track_id, provider, recording_mbid, release_mbid, decision_source, "
                        "attempt_id, selected_at) VALUES (?, 'musicbrainz', ?, ?, 'manual', ?, ?)",
                        (
                            track.local_track_id,
                            track.recording_mbid,
                            evidence.release_mbid,
                            attempt_id,
                            now,
                        ),
                    )
            connection.execute(
                "UPDATE library_identification_reviews SET state = 'resolved', "
                "reason_code = 'EXPLICIT_CANDIDATE_ACCEPTED', decided_by_user_id = ?, "
                "decided_at = ?, updated_at = ?, decision_revision = decision_revision + 1, "
                "row_revision = row_revision + 1 WHERE local_album_id = ? AND state != 'resolved'",
                (actor_user_id, now, now, snapshot["local_album_id"]),
            )
            result.update(
                {"selected_candidate_key": candidate_key, "outcome": "identified"}
            )
            connection.execute(
                "UPDATE library_reidentification_snapshots SET selected_candidate_key = ?, "
                "result_json = ? WHERE job_id = ?",
                (candidate_key, json.dumps(result, sort_keys=True), job_id),
            )
            updated = connection.execute(
                "UPDATE library_operation_jobs SET state = 'succeeded', terminal_code = 'IDENTIFIED', "
                "terminal_at = ?, updated_at = ?, row_revision = row_revision + 1, "
                "event_revision = event_revision + 1 WHERE id = ? AND state = 'ready' "
                "AND row_revision = ? RETURNING *",
                (now, now, job_id, expected_job_revision),
            ).fetchone()
            connection.execute(
                "INSERT INTO library_catalog_actions "
                "(id, actor_user_id, action_kind, local_album_id, operation_job_id, "
                "before_json, after_json, reason_code, created_at) VALUES (?,?,?,?,?,?,?,?,?)",
                (
                    str(uuid.uuid4()),
                    actor_user_id,
                    "explicit_reidentification",
                    snapshot["local_album_id"],
                    job_id,
                    json.dumps({}),
                    json.dumps(result, sort_keys=True),
                    "EXPLICIT_CANDIDATE_ACCEPTED",
                    now,
                ),
            )
            self._bump_catalog(connection)
            self._bump_stream(connection, "operation")
            return dict(updated)

        return await self._write(operation)

    async def request_operation_control(
        self,
        job_id: str,
        *,
        control: str,
        expected_row_revision: int,
        now: float,
    ) -> dict[str, Any]:
        def operation(connection: sqlite3.Connection) -> dict[str, Any]:
            row = connection.execute(
                "SELECT * FROM library_operation_jobs WHERE id = ?", (job_id,)
            ).fetchone()
            if row is None:
                raise ResourceNotFoundError("Library operation not found.")
            if int(row["row_revision"]) != expected_row_revision:
                raise StaleRevisionError(
                    "The operation changed before the control request."
                )
            if control == "resume":
                if row["state"] not in {"paused", "stopped", "failed"}:
                    return dict(row)
                if row["state"] == "failed" and row["terminal_code"] not in {
                    "PROVIDER_TEMPORARILY_UNAVAILABLE"
                }:
                    return dict(row)
                if row["state"] == "failed":
                    connection.execute(
                        "UPDATE library_operation_work SET state = 'pending', "
                        "failure_code = NULL, result_json = NULL, updated_at = ?, "
                        "row_revision = row_revision + 1 WHERE job_id = ? "
                        "AND state IN ('failed','running')",
                        (now, job_id),
                    )
                assignments = (
                    "state = 'queued', control_request = 'none', terminal_code = NULL, "
                    "terminal_at = NULL, completed_count = CASE WHEN state = 'failed' "
                    "THEN 0 ELSE completed_count END, failed_count = CASE WHEN state = 'failed' "
                    "THEN 0 ELSE failed_count END"
                )
            elif control in {"pause", "stop"}:
                if row["state"] in {"succeeded", "cancelled"}:
                    return dict(row)
                assignments = f"control_request = '{control}'"
            else:
                raise ValueError(f"Unsupported operation control: {control}")
            updated = connection.execute(
                f"UPDATE library_operation_jobs SET {assignments}, updated_at = ?, "
                "row_revision = row_revision + 1, event_revision = event_revision + 1 "
                "WHERE id = ? AND row_revision = ? AND row_revision < ? AND event_revision < ? "
                "RETURNING *",
                (now, job_id, expected_row_revision, MAX_REVISION, MAX_REVISION),
            ).fetchone()
            if updated is None:
                raise StaleRevisionError(
                    "The operation changed before the control request."
                )
            self._bump_stream(connection, "operation")
            return dict(updated)

        return await self._write(operation)

    async def checkpoint_operation_control(
        self, job_id: str, worker_id: str, *, now: float
    ) -> dict[str, Any] | None:
        def operation(connection: sqlite3.Connection) -> dict[str, Any] | None:
            row = connection.execute(
                "SELECT * FROM library_operation_jobs WHERE id = ? AND state = 'running' "
                "AND lease_owner = ?",
                (job_id, worker_id),
            ).fetchone()
            if row is None or row["control_request"] == "none":
                return _row(row)
            state = "paused" if row["control_request"] == "pause" else "stopped"
            updated = connection.execute(
                "UPDATE library_operation_jobs SET state = ?, control_request = 'none', "
                "terminal_code = ?, lease_owner = NULL, lease_expires_at = NULL, heartbeat_at = NULL, "
                "updated_at = ?, terminal_at = CASE WHEN ? = 'stopped' THEN ? ELSE terminal_at END, "
                "row_revision = row_revision + 1, event_revision = event_revision + 1 "
                "WHERE id = ? AND state = 'running' AND lease_owner = ? RETURNING *",
                (state, state.upper(), now, state, now, job_id, worker_id),
            ).fetchone()
            connection.execute(
                "UPDATE library_operation_work SET state = 'pending', row_revision = row_revision + 1, "
                "updated_at = ? WHERE job_id = ? AND state = 'running'",
                (now, job_id),
            )
            self._bump_stream(connection, "operation")
            return dict(updated)

        return await self._write(operation)

    async def finish_operation_job(
        self, job_id: str, worker_id: str, *, state: str, terminal_code: str, now: float
    ) -> dict[str, Any]:
        def operation(connection: sqlite3.Connection) -> dict[str, Any]:
            updated = connection.execute(
                "UPDATE library_operation_jobs SET state = ?, terminal_code = ?, terminal_at = ?, "
                "updated_at = ?, lease_owner = NULL, lease_expires_at = NULL, heartbeat_at = NULL, "
                "row_revision = row_revision + 1, event_revision = event_revision + 1 "
                "WHERE id = ? AND state = 'running' AND lease_owner = ? RETURNING *",
                (state, terminal_code, now, now, job_id, worker_id),
            ).fetchone()
            if updated is None:
                raise StaleRevisionError(
                    "The operation lease changed before completion."
                )
            self._bump_stream(connection, "operation")
            return dict(updated)

        return await self._write(operation)

    async def get_operation_snapshot(self, job_id: str) -> dict[str, Any] | None:
        def operation(connection: sqlite3.Connection) -> dict[str, Any] | None:
            job = connection.execute(
                "SELECT * FROM library_operation_jobs WHERE id = ?", (job_id,)
            ).fetchone()
            if job is None:
                return None
            table = {
                "bulk_review_apply": "library_bulk_review_snapshots",
                "repair": "library_repair_snapshots",
                "explicit_reidentification": "library_reidentification_snapshots",
            }[str(job["kind"])]
            snapshot = connection.execute(
                f"SELECT * FROM {table} WHERE job_id = ?", (job_id,)
            ).fetchone()
            return {"job": dict(job), "snapshot": _row(snapshot)}

        return await self._read(operation)

    async def get_operation_job(self, job_id: str) -> dict[str, Any] | None:
        def operation(connection: sqlite3.Connection) -> dict[str, Any] | None:
            return _row(
                connection.execute(
                    "SELECT * FROM library_operation_jobs WHERE id = ?", (job_id,)
                ).fetchone()
            )

        return await self._read(operation)

    async def list_operation_jobs(
        self,
        *,
        kind: str | None = None,
        limit: int = 50,
        before_created_at: float | None = None,
        before_id: str | None = None,
    ) -> list[dict[str, Any]]:
        def operation(connection: sqlite3.Connection) -> list[dict[str, Any]]:
            clauses = ["1 = 1"]
            parameters: list[Any] = []
            if kind is not None:
                clauses.append("kind = ?")
                parameters.append(kind)
            if before_created_at is not None and before_id is not None:
                clauses.append("(created_at < ? OR (created_at = ? AND id < ?))")
                parameters.extend((before_created_at, before_created_at, before_id))
            rows = connection.execute(
                "SELECT * FROM library_operation_jobs WHERE "
                + " AND ".join(clauses)
                + " ORDER BY created_at DESC, id DESC LIMIT ?",
                (*parameters, min(max(limit, 1), 51)),
            ).fetchall()
            return [dict(row) for row in rows]

        return await self._read(operation)

    async def get_operation_by_idempotency_key(
        self, idempotency_key: str
    ) -> dict[str, Any] | None:
        def operation(connection: sqlite3.Connection) -> dict[str, Any] | None:
            return _row(
                connection.execute(
                    "SELECT * FROM library_operation_jobs WHERE idempotency_key = ?",
                    (idempotency_key,),
                ).fetchone()
            )

        return await self._read(operation)

    async def list_operation_work_results(
        self, job_id: str, *, limit: int = 101
    ) -> list[dict[str, Any]]:
        def operation(connection: sqlite3.Connection) -> list[dict[str, Any]]:
            rows = connection.execute(
                "SELECT ordinal, local_album_id, local_track_id, action, state, "
                "failure_code, result_json FROM library_operation_work "
                "WHERE job_id = ? AND state IN ('succeeded','failed','skipped') "
                "ORDER BY ordinal LIMIT ?",
                (job_id, min(max(limit, 1), 101)),
            ).fetchall()
            return [dict(row) for row in rows]

        return await self._read(operation)

    async def claim_operation_job(
        self,
        worker_id: str,
        *,
        now: float,
        lease_seconds: float,
        kind: str | None = None,
    ) -> dict[str, Any] | None:
        def operation(connection: sqlite3.Connection) -> dict[str, Any] | None:
            kind_clause = "AND kind = ?" if kind is not None else ""
            parameters: tuple[Any, ...] = (kind,) if kind is not None else ()
            candidate = connection.execute(
                "SELECT id, row_revision FROM library_operation_jobs "
                f"WHERE state = 'queued' {kind_clause} ORDER BY created_at, id LIMIT 1",
                parameters,
            ).fetchone()
            if candidate is None:
                return None
            updated = connection.execute(
                "UPDATE library_operation_jobs SET state = 'running', started_at = COALESCE(started_at, ?), "
                "lease_owner = ?, lease_expires_at = ?, heartbeat_at = ?, updated_at = ?, "
                "row_revision = row_revision + 1, event_revision = event_revision + 1 "
                "WHERE id = ? AND state = 'queued' AND row_revision = ? AND row_revision < ? "
                "AND event_revision < ? RETURNING *",
                (
                    now,
                    worker_id,
                    now + lease_seconds,
                    now,
                    now,
                    candidate["id"],
                    candidate["row_revision"],
                    MAX_REVISION,
                    MAX_REVISION,
                ),
            ).fetchone()
            if updated is None:
                self._refuse_max_revision(
                    connection,
                    table="library_operation_jobs",
                    predicate="id = ?",
                    parameters=(candidate["id"],),
                    include_event_revision=True,
                )
                return None
            self._bump_stream(connection, "operation")
            return dict(updated)

        return await self._write(operation)

    async def heartbeat_operation_job(
        self, job_id: str, worker_id: str, *, now: float, lease_seconds: float
    ) -> bool:
        def operation(connection: sqlite3.Connection) -> bool:
            cursor = connection.execute(
                "UPDATE library_operation_jobs SET heartbeat_at = ?, lease_expires_at = ?, "
                "updated_at = ?, row_revision = row_revision + 1 WHERE id = ? "
                "AND state = 'running' AND lease_owner = ? AND row_revision < ?",
                (now, now + lease_seconds, now, job_id, worker_id, MAX_REVISION),
            )
            if cursor.rowcount == 1:
                return True
            self._refuse_max_revision(
                connection,
                table="library_operation_jobs",
                predicate="id = ? AND state = 'running' AND lease_owner = ?",
                parameters=(job_id, worker_id),
            )
            return False

        return await self._write(operation)

    async def defer_explicit_operation_for_scan(
        self, job_id: str, worker_id: str, *, now: float
    ) -> dict[str, Any]:
        def operation(connection: sqlite3.Connection) -> dict[str, Any]:
            connection.execute(
                "UPDATE library_operation_work SET state = 'pending', updated_at = ?, "
                "row_revision = row_revision + 1 WHERE job_id = ? AND state = 'running'",
                (now, job_id),
            )
            deferred = connection.execute(
                "UPDATE library_operation_jobs SET state = 'queued', lease_owner = NULL, "
                "lease_expires_at = NULL, heartbeat_at = NULL, updated_at = ?, "
                "row_revision = row_revision + 1, event_revision = event_revision + 1 "
                "WHERE id = ? AND kind = 'explicit_reidentification' AND state = 'running' "
                "AND lease_owner = ? RETURNING *",
                (now, job_id, worker_id),
            ).fetchone()
            if deferred is None:
                raise StaleRevisionError(
                    "The re-identification lease changed while it waited for the scan."
                )
            self._bump_stream(connection, "operation")
            return dict(deferred)

        return await self._write(operation)

    async def recover_expired_operation_leases(self, *, now: float) -> int:
        def operation(connection: sqlite3.Connection) -> int:
            overflow = connection.execute(
                "SELECT 1 FROM library_operation_jobs WHERE state = 'running' "
                "AND lease_expires_at < ? AND (row_revision >= ? OR event_revision >= ?) LIMIT 1",
                (now, MAX_REVISION, MAX_REVISION),
            ).fetchone()
            if overflow is not None:
                raise RevisionOverflowError(
                    "An operation job revision reached its maximum value."
                )
            cursor = connection.execute(
                "UPDATE library_operation_jobs SET state = 'queued', lease_owner = NULL, "
                "lease_expires_at = NULL, heartbeat_at = NULL, updated_at = ?, "
                "row_revision = row_revision + 1, event_revision = event_revision + 1 "
                "WHERE state = 'running' AND lease_expires_at < ? AND row_revision < ? "
                "AND event_revision < ?",
                (now, now, MAX_REVISION, MAX_REVISION),
            )
            if cursor.rowcount:
                self._bump_stream(connection, "operation")
            return cursor.rowcount

        return await self._write(operation)

    async def claim_operation_work(
        self, job_id: str, worker_id: str, *, now: float
    ) -> dict[str, Any] | None:
        def operation(connection: sqlite3.Connection) -> dict[str, Any] | None:
            job = connection.execute(
                "SELECT id FROM library_operation_jobs WHERE id = ? AND state = 'running' "
                "AND lease_owner = ?",
                (job_id, worker_id),
            ).fetchone()
            if job is None:
                return None
            candidate = connection.execute(
                "SELECT ordinal, row_revision FROM library_operation_work "
                "WHERE job_id = ? AND state = 'pending' ORDER BY ordinal LIMIT 1",
                (job_id,),
            ).fetchone()
            if candidate is None:
                return None
            updated = connection.execute(
                "UPDATE library_operation_work SET state = 'running', updated_at = ?, "
                "row_revision = row_revision + 1 WHERE job_id = ? AND ordinal = ? "
                "AND state = 'pending' AND row_revision = ? AND row_revision < ? RETURNING *",
                (
                    now,
                    job_id,
                    candidate["ordinal"],
                    candidate["row_revision"],
                    MAX_REVISION,
                ),
            ).fetchone()
            if updated is not None:
                return dict(updated)
            self._refuse_max_revision(
                connection,
                table="library_operation_work",
                predicate="job_id = ? AND ordinal = ?",
                parameters=(job_id, candidate["ordinal"]),
            )
            return None

        return await self._write(operation)

    async def complete_operation_work(
        self,
        job_id: str,
        ordinal: int,
        *,
        worker_id: str,
        expected_work_revision: int,
        state: str,
        result_json: str | None,
        failure_code: str | None,
        completed_at: float,
    ) -> tuple[int, int, int]:
        def operation(connection: sqlite3.Connection) -> tuple[int, int, int]:
            work_revision = connection.execute(
                "UPDATE library_operation_work SET state = ?, result_json = ?, failure_code = ?, "
                "updated_at = ?, row_revision = row_revision + 1 WHERE job_id = ? AND ordinal = ? "
                "AND state = 'running' AND row_revision = ? AND row_revision < ? "
                "RETURNING row_revision",
                (
                    state,
                    result_json,
                    failure_code,
                    completed_at,
                    job_id,
                    ordinal,
                    expected_work_revision,
                    MAX_REVISION,
                ),
            ).fetchone()
            if work_revision is None:
                self._refuse_max_revision(
                    connection,
                    table="library_operation_work",
                    predicate="job_id = ? AND ordinal = ?",
                    parameters=(job_id, ordinal),
                )
                raise StaleRevisionError(
                    "The operation subject changed before its result was recorded."
                )
            result_counter = {
                "succeeded": "succeeded_count",
                "failed": "failed_count",
                "skipped": "skipped_count",
            }.get(state)
            if result_counter is None:
                raise ValueError(f"Invalid terminal operation work state: {state}")
            job_revision = connection.execute(
                f"UPDATE library_operation_jobs SET completed_count = completed_count + 1, "
                f"{result_counter} = {result_counter} + 1, updated_at = ?, "
                "row_revision = row_revision + 1, event_revision = event_revision + 1 "
                "WHERE id = ? AND state = 'running' AND lease_owner = ? "
                "AND row_revision < ? AND event_revision < ? RETURNING row_revision",
                (completed_at, job_id, worker_id, MAX_REVISION, MAX_REVISION),
            ).fetchone()
            if job_revision is None:
                self._refuse_max_revision(
                    connection,
                    table="library_operation_jobs",
                    predicate="id = ?",
                    parameters=(job_id,),
                    include_event_revision=True,
                )
                raise StaleRevisionError(
                    "The operation job changed before its counters were recorded."
                )
            stream_revision = self._bump_stream(connection, "operation")
            return (
                int(work_revision["row_revision"]),
                int(job_revision["row_revision"]),
                stream_revision,
            )

        return await self._write(operation)

    async def apply_bulk_review_work(
        self,
        job_id: str,
        ordinal: int,
        *,
        worker_id: str,
        expected_work_revision: int,
        actor_user_id: str,
        now: float,
    ) -> dict[str, Any]:
        """Apply one materialized review subject and counters atomically."""

        def operation(connection: sqlite3.Connection) -> dict[str, Any]:
            work = connection.execute(
                "SELECT * FROM library_operation_work WHERE job_id = ? AND ordinal = ? "
                "AND state = 'running' AND row_revision = ?",
                (job_id, ordinal, expected_work_revision),
            ).fetchone()
            if work is None:
                raise StaleRevisionError(
                    "The bulk operation subject changed before Apply."
                )
            review = connection.execute(
                "SELECT * FROM library_identification_reviews WHERE "
                "((local_album_id = ? AND ? IS NOT NULL) OR (local_track_id = ? AND ? IS NOT NULL)) "
                "AND state != 'resolved' ORDER BY updated_at DESC LIMIT 1",
                (
                    work["local_album_id"],
                    work["local_album_id"],
                    work["local_track_id"],
                    work["local_track_id"],
                ),
            ).fetchone()
            terminal_state = "succeeded"
            failure_code = None
            result: dict[str, Any] = {}
            if (
                review is None
                or int(review["row_revision"]) != int(work["expected_subject_revision"])
                or str(review["input_revision"]) != str(work["expected_input_revision"])
            ):
                terminal_state = "skipped"
                failure_code = "STALE_SUBJECT"
            else:
                action = str(work["action"])
                album_id = work["local_album_id"]
                track_id = work["local_track_id"]
                if action == "keep_tagged":
                    identity = connection.execute(
                        "SELECT 1 FROM local_album_external_identities WHERE local_album_id = ?",
                        (album_id,),
                    ).fetchone()
                    if identity is not None:
                        terminal_state = "failed"
                        failure_code = "IDENTITY_REQUIRES_DETACH"
                    else:
                        connection.execute(
                            "UPDATE library_identification_reviews SET state = 'keep_tagged', "
                            "reason_code = 'BULK_KEEP_TAGGED', decided_by_user_id = ?, decided_at = ?, "
                            "updated_at = ?, decision_revision = decision_revision + 1, "
                            "row_revision = row_revision + 1 WHERE id = ?",
                            (actor_user_id, now, now, review["id"]),
                        )
                elif action == "exclude":
                    clause = "local_album_id = ?" if album_id is not None else "id = ?"
                    connection.execute(
                        "UPDATE local_tracks SET manual_excluded = 1, availability = 'excluded', "
                        f"excluded_at = ?, row_revision = row_revision + 1 WHERE {clause}",
                        (now, album_id or track_id),
                    )
                    connection.execute(
                        "UPDATE library_identification_reviews SET state = 'excluded', "
                        "reason_code = 'BULK_EXCLUDED', decided_by_user_id = ?, decided_at = ?, "
                        "updated_at = ?, decision_revision = decision_revision + 1, "
                        "row_revision = row_revision + 1 WHERE id = ?",
                        (actor_user_id, now, now, review["id"]),
                    )
                elif action == "retry":
                    if album_id is None:
                        terminal_state = "failed"
                        failure_code = "ALBUM_RETRY_REQUIRED"
                    else:
                        policies = {
                            str(row[0])
                            for row in connection.execute(
                                "SELECT DISTINCT applied_policy FROM local_tracks "
                                "WHERE local_album_id = ?",
                                (album_id,),
                            )
                        }
                        if "excluded" in policies:
                            terminal_state = "failed"
                            failure_code = "EXCLUDED"
                        else:
                            album = connection.execute(
                                "SELECT row_revision FROM local_albums WHERE id = ? "
                                "AND retired_into_album_id IS NULL",
                                (album_id,),
                            ).fetchone()
                            tracks = connection.execute(
                                "SELECT id, tag_revision, stat_revision, "
                                "applied_policy_revision, applied_policy FROM local_tracks "
                                "WHERE local_album_id = ? ORDER BY id",
                                (album_id,),
                            ).fetchall()
                            if album is None or not tracks:
                                terminal_state = "failed"
                                failure_code = "SUBJECT_NOT_AVAILABLE"
                            else:
                                retry_idempotency_key = (
                                    f"{work['idempotency_key']}:reidentification"
                                )
                                child = connection.execute(
                                    "SELECT id FROM library_operation_jobs "
                                    "WHERE idempotency_key = ?",
                                    (retry_idempotency_key,),
                                ).fetchone()
                                child_job_id = (
                                    str(child["id"])
                                    if child is not None
                                    else str(
                                        uuid.uuid5(
                                            uuid.NAMESPACE_URL,
                                            retry_idempotency_key,
                                        )
                                    )
                                )
                                input_revision = _album_input_revision(tracks)
                                if child is None:
                                    connection.execute(
                                        "INSERT INTO library_operation_jobs "
                                        "(id, kind, state, requested_by_user_id, "
                                        "expected_work_count, idempotency_key, created_at, updated_at) "
                                        "VALUES (?, 'explicit_reidentification', 'queued', ?, 1, ?, ?, ?)",
                                        (
                                            child_job_id,
                                            actor_user_id,
                                            retry_idempotency_key,
                                            now,
                                            now,
                                        ),
                                    )
                                    connection.execute(
                                        "INSERT INTO library_reidentification_snapshots "
                                        "(job_id, local_album_id, expected_album_revision, "
                                        "expected_input_revision, one_off_local_metadata, created_at) "
                                        "VALUES (?,?,?,?,?,?)",
                                        (
                                            child_job_id,
                                            album_id,
                                            album["row_revision"],
                                            input_revision,
                                            int("local_metadata" in policies),
                                            now,
                                        ),
                                    )
                                    connection.execute(
                                        "INSERT INTO library_operation_work "
                                        "(job_id, ordinal, local_album_id, "
                                        "expected_subject_revision, expected_input_revision, "
                                        "action, idempotency_key, updated_at) "
                                        "VALUES (?,0,?,?,?,?,?,?)",
                                        (
                                            child_job_id,
                                            album_id,
                                            album["row_revision"],
                                            input_revision,
                                            "reidentify",
                                            f"{child_job_id}:{album_id}",
                                            now,
                                        ),
                                    )
                                connection.execute(
                                    "UPDATE library_identification_reviews "
                                    "SET state = 'resolved', reason_code = 'EXPLICIT_RETRY', "
                                    "decided_by_user_id = ?, decided_at = ?, updated_at = ?, "
                                    "decision_revision = decision_revision + 1, "
                                    "row_revision = row_revision + 1 WHERE id = ?",
                                    (actor_user_id, now, now, review["id"]),
                                )
                                result["operation_job_id"] = child_job_id
                elif action.startswith("accept_candidate:"):
                    candidate_key = action.partition(":")[2]
                    evidence = connection.execute(
                        "SELECT e.* FROM library_identification_evidence e "
                        "WHERE e.attempt_id = ? AND e.candidate_key = ?",
                        (review["attempt_id"], candidate_key),
                    ).fetchone()
                    if evidence is None:
                        terminal_state = "failed"
                        failure_code = "CANDIDATE_NOT_FOUND"
                    else:
                        parsed = msgspec.json.decode(
                            bytes(evidence["evidence_json"]), type=CandidateEvidence
                        )
                        if parsed.reason_code not in AUTOMATIC_SAFE_EVIDENCE_REASONS:
                            terminal_state = "failed"
                            failure_code = "CANDIDATE_NOT_AUTOMATIC_SAFE"
                        else:
                            connection.execute(
                                "INSERT INTO local_album_external_identities "
                                "(local_album_id, provider, release_group_mbid, release_mbid, "
                                "decision_source, matcher_version, attempt_id, selected_by_user_id, selected_at) "
                                "VALUES (?, 'musicbrainz', ?, ?, 'manual', ?, ?, ?, ?) "
                                "ON CONFLICT(local_album_id, provider) DO UPDATE SET "
                                "release_group_mbid = excluded.release_group_mbid, "
                                "release_mbid = excluded.release_mbid, decision_source = 'manual', "
                                "attempt_id = excluded.attempt_id, selected_by_user_id = excluded.selected_by_user_id, "
                                "selected_at = excluded.selected_at, row_revision = row_revision + 1",
                                (
                                    album_id,
                                    parsed.release_group_mbid,
                                    parsed.release_mbid,
                                    parsed.matcher_version,
                                    review["attempt_id"],
                                    actor_user_id,
                                    now,
                                ),
                            )
                            for track in parsed.track_evidence:
                                if (
                                    track.classification == "supported"
                                    and track.recording_mbid
                                ):
                                    connection.execute(
                                        "INSERT INTO local_track_external_identities "
                                        "(local_track_id, provider, recording_mbid, release_mbid, "
                                        "decision_source, attempt_id, selected_at) VALUES (?, 'musicbrainz', ?, ?, 'manual', ?, ?) "
                                        "ON CONFLICT(local_track_id, provider) DO UPDATE SET recording_mbid = excluded.recording_mbid, "
                                        "release_mbid = excluded.release_mbid, decision_source = 'manual', "
                                        "attempt_id = excluded.attempt_id, selected_at = excluded.selected_at, "
                                        "row_revision = row_revision + 1",
                                        (
                                            track.local_track_id,
                                            track.recording_mbid,
                                            parsed.release_mbid,
                                            review["attempt_id"],
                                            now,
                                        ),
                                    )
                            connection.execute(
                                "UPDATE library_identification_reviews SET state = 'resolved', "
                                "reason_code = 'BULK_CANDIDATE_ACCEPTED', decided_by_user_id = ?, "
                                "decided_at = ?, updated_at = ?, decision_revision = decision_revision + 1, "
                                "row_revision = row_revision + 1 WHERE id = ?",
                                (actor_user_id, now, now, review["id"]),
                            )
                else:
                    terminal_state = "failed"
                    failure_code = "UNSUPPORTED_ACTION"
                if terminal_state == "succeeded":
                    connection.execute(
                        "UPDATE library_identification_jobs SET state = 'cancelled', "
                        "last_failure_code = 'ADMIN_DECISION', updated_at = ?, terminal_at = ?, "
                        "row_revision = row_revision + 1, event_revision = event_revision + 1 "
                        "WHERE kind = 'automatic' AND state IN ('queued','paused') AND "
                        "((local_album_id = ? AND ? IS NOT NULL) OR (local_track_id = ? AND ? IS NOT NULL))",
                        (now, now, album_id, album_id, track_id, track_id),
                    )
                    connection.execute(
                        "INSERT INTO library_catalog_actions "
                        "(id, idempotency_key, actor_user_id, action_kind, local_album_id, "
                        "local_track_id, operation_job_id, before_json, after_json, reason_code, created_at) "
                        "VALUES (?,?,?,?,?,?,?,?,?,?,?)",
                        (
                            str(uuid.uuid4()),
                            work["idempotency_key"],
                            actor_user_id,
                            action,
                            album_id,
                            track_id,
                            job_id,
                            json.dumps({"review_state": review["state"]}),
                            json.dumps({"review_state": action}),
                            "BULK_APPLY",
                            now,
                        ),
                    )
                    result["catalog_revision"] = self._bump_catalog(connection)
            encoded_result = json.dumps(result, sort_keys=True)
            connection.execute(
                "UPDATE library_operation_work SET state = ?, result_json = ?, failure_code = ?, "
                "updated_at = ?, row_revision = row_revision + 1 WHERE job_id = ? AND ordinal = ?",
                (terminal_state, encoded_result, failure_code, now, job_id, ordinal),
            )
            counter = {
                "succeeded": "succeeded_count",
                "failed": "failed_count",
                "skipped": "skipped_count",
            }[terminal_state]
            connection.execute(
                f"UPDATE library_operation_jobs SET completed_count = completed_count + 1, "
                f"{counter} = {counter} + 1, updated_at = ?, row_revision = row_revision + 1, "
                "event_revision = event_revision + 1 WHERE id = ? AND state = 'running' "
                "AND lease_owner = ?",
                (now, job_id, worker_id),
            )
            self._bump_stream(connection, "operation")
            return {"state": terminal_state, "failure_code": failure_code, **result}

        return await self._write(operation)

    async def add_repair_findings(
        self, job_id: str, findings: list[RepairFinding], *, updated_at: float
    ) -> None:
        def operation(connection: sqlite3.Connection) -> None:
            connection.executemany(
                "INSERT INTO library_identity_repair_findings "
                "(id, job_id, local_album_id, evidence_id, expected_album_revision, "
                "expected_identity_revision, finding_code, confidence, reason_code, "
                "apply_eligible, created_at, updated_at) VALUES (?,?,?,?,?,?,?,?,?,?,?,?)",
                [
                    (
                        finding.id,
                        job_id,
                        finding.local_album_id,
                        finding.evidence_id,
                        finding.expected_album_revision,
                        finding.expected_identity_revision,
                        finding.finding_code,
                        finding.confidence,
                        finding.reason_code,
                        int(finding.apply_eligible),
                        updated_at,
                        updated_at,
                    )
                    for finding in findings
                ],
            )

        await self._write(operation)

    async def create_repair_operation(
        self,
        job: OperationJob,
        *,
        scope: dict[str, Any],
        source_matcher_version: str | None,
        target_matcher_version: str,
    ) -> dict[str, Any]:
        def operation(connection: sqlite3.Connection) -> dict[str, Any]:
            if job.idempotency_key:
                existing = connection.execute(
                    "SELECT * FROM library_operation_jobs WHERE idempotency_key = ?",
                    (job.idempotency_key,),
                ).fetchone()
                if existing is not None:
                    return dict(existing)
            root_ids = [str(value) for value in scope.get("root_ids", [])]
            root_clause = ""
            parameters: list[Any] = []
            if root_ids:
                root_clause = f"AND a.root_id IN ({','.join('?' for _ in root_ids)})"
                parameters.extend(root_ids)
            identity_clause = ""
            if source_matcher_version is not None:
                identity_clause = "AND i.matcher_version = ?"
                parameters.append(source_matcher_version)
            elif bool(scope.get("legacy_only", True)):
                identity_clause = "AND i.decision_source = 'legacy_import'"
            rows = connection.execute(
                "SELECT a.id, a.row_revision, i.row_revision identity_revision, "
                "i.decision_source, i.attempt_id FROM local_albums a "
                "JOIN local_album_external_identities i ON i.local_album_id = a.id "
                f"WHERE a.retired_into_album_id IS NULL {root_clause} "
                f"{identity_clause} ORDER BY a.id",
                parameters,
            ).fetchall()
            connection.execute(
                "INSERT INTO library_operation_jobs "
                "(id, kind, state, requested_by_user_id, input_catalog_revision, expected_work_count, "
                "idempotency_key, created_at, updated_at) VALUES (?, 'repair', 'queued', ?, ?, ?, ?, ?, ?)",
                (
                    job.id,
                    job.requested_by_user_id,
                    job.input_catalog_revision,
                    len(rows),
                    job.idempotency_key,
                    job.created_at,
                    job.created_at,
                ),
            )
            connection.execute(
                "INSERT INTO library_repair_snapshots "
                "(job_id, scope_json, source_matcher_version, target_matcher_version, created_at) "
                "VALUES (?,?,?,?,?)",
                (
                    job.id,
                    json.dumps(scope, sort_keys=True),
                    source_matcher_version,
                    target_matcher_version,
                    job.created_at,
                ),
            )
            connection.executemany(
                "INSERT INTO library_operation_work "
                "(job_id, ordinal, local_album_id, expected_subject_revision, expected_input_revision, "
                "action, idempotency_key, updated_at) VALUES (?,?,?,?,?,?,?,?)",
                [
                    (
                        job.id,
                        ordinal,
                        row["id"],
                        row["row_revision"],
                        f"{row['identity_revision']}:{row['decision_source']}:{row['attempt_id'] or ''}",
                        "repair_audit",
                        f"{job.id}:{row['id']}:audit",
                        job.created_at,
                    )
                    for ordinal, row in enumerate(rows)
                ],
            )
            self._bump_stream(connection, "operation")
            return dict(
                connection.execute(
                    "SELECT * FROM library_operation_jobs WHERE id = ?", (job.id,)
                ).fetchone()
            )

        return await self._write(operation)

    async def estimate_repair_operation(self, root_ids: list[str]) -> dict[str, int]:
        def operation(connection: sqlite3.Connection) -> dict[str, int]:
            parameters: list[Any] = []
            root_clause = ""
            if root_ids:
                placeholders = ",".join("?" for _ in root_ids)
                root_clause = f"AND a.root_id IN ({placeholders})"
                parameters.extend(root_ids)
            identity_count = connection.execute(
                "SELECT COUNT(*) FROM local_albums a "
                "JOIN local_album_external_identities i ON i.local_album_id = a.id "
                "WHERE a.retired_into_album_id IS NULL "
                "AND i.decision_source = 'legacy_import' "
                f"{root_clause}",
                parameters,
            ).fetchone()[0]
            queued_repair_count = connection.execute(
                "SELECT COUNT(*) FROM library_operation_jobs "
                "WHERE kind = 'repair' AND state IN ('queued','running','pausing','paused','ready')"
            ).fetchone()[0]
            return {
                "identity_count": int(identity_count),
                "queued_repair_count": int(queued_repair_count),
            }

        return await self._read(operation)

    async def save_repair_finding_for_work(
        self,
        job_id: str,
        ordinal: int,
        *,
        worker_id: str,
        expected_work_revision: int,
        finding: RepairFinding,
        attempt: IdentificationAttempt | None = None,
        evidence: list[IdentificationEvidenceRecord] | None = None,
        now: float,
    ) -> None:
        def operation(connection: sqlite3.Connection) -> None:
            work = connection.execute(
                "SELECT * FROM library_operation_work WHERE job_id = ? AND ordinal = ? "
                "AND state = 'running' AND row_revision = ?",
                (job_id, ordinal, expected_work_revision),
            ).fetchone()
            if work is None:
                raise StaleRevisionError(
                    "The repair subject changed before its finding was saved."
                )
            if attempt is not None:
                connection.execute(
                    "INSERT INTO library_identification_attempts "
                    "(id, local_album_id, local_track_id, trigger, requested_by_user_id, "
                    "input_tag_revision, input_policy_revision, input_file_revision, matcher_version, "
                    "state, terminal_reason_code, selected_candidate_key, candidate_count, "
                    "degradation_flags_json, started_at, completed_at) "
                    "VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)",
                    (
                        attempt.id,
                        attempt.local_album_id,
                        attempt.local_track_id,
                        attempt.trigger,
                        attempt.requested_by_user_id,
                        attempt.input_tag_revision,
                        attempt.input_policy_revision,
                        attempt.input_file_revision,
                        attempt.matcher_version,
                        attempt.state,
                        attempt.terminal_reason_code,
                        attempt.selected_candidate_key,
                        attempt.candidate_count,
                        json.dumps(attempt.degradation_flags),
                        attempt.started_at,
                        attempt.completed_at,
                    ),
                )
                self._insert_evidence(connection, evidence or [])
            connection.execute(
                "INSERT INTO library_identity_repair_findings "
                "(id, job_id, local_album_id, evidence_id, expected_album_revision, "
                "expected_identity_revision, finding_code, confidence, reason_code, apply_eligible, "
                "created_at, updated_at) VALUES (?,?,?,?,?,?,?,?,?,?,?,?)",
                (
                    finding.id,
                    job_id,
                    finding.local_album_id,
                    finding.evidence_id,
                    finding.expected_album_revision,
                    finding.expected_identity_revision,
                    finding.finding_code,
                    finding.confidence,
                    finding.reason_code,
                    int(finding.apply_eligible),
                    now,
                    now,
                ),
            )
            connection.execute(
                "UPDATE library_operation_work SET state = 'succeeded', result_json = ?, "
                "updated_at = ?, row_revision = row_revision + 1 WHERE job_id = ? AND ordinal = ?",
                (json.dumps({"finding_id": finding.id}), now, job_id, ordinal),
            )
            connection.execute(
                "UPDATE library_operation_jobs SET completed_count = completed_count + 1, "
                "succeeded_count = succeeded_count + 1, updated_at = ?, "
                "row_revision = row_revision + 1, event_revision = event_revision + 1 "
                "WHERE id = ? AND state = 'running' AND lease_owner = ?",
                (now, job_id, worker_id),
            )
            self._bump_stream(connection, "operation")

        await self._write(operation)

    async def mark_repair_ready(
        self, job_id: str, worker_id: str, *, now: float
    ) -> dict[str, Any]:
        def operation(connection: sqlite3.Connection) -> dict[str, Any]:
            pending = connection.execute(
                "SELECT COUNT(*) FROM library_operation_work WHERE job_id = ? "
                "AND state IN ('pending','running')",
                (job_id,),
            ).fetchone()[0]
            if pending:
                raise StaleRevisionError(
                    "The repair audit still has unfinished subjects."
                )
            snapshot = connection.execute(
                "SELECT target_matcher_version FROM library_repair_snapshots WHERE job_id = ?",
                (job_id,),
            ).fetchone()
            findings = connection.execute(
                "SELECT finding_code, reason_code, COUNT(*) count "
                "FROM library_identity_repair_findings WHERE job_id = ? "
                "GROUP BY finding_code, reason_code",
                (job_id,),
            ).fetchall()
            roots = connection.execute(
                "SELECT a.root_id, COUNT(DISTINCT f.local_album_id) count "
                "FROM library_identity_repair_findings f "
                "JOIN local_albums a ON a.id = f.local_album_id "
                "WHERE f.job_id = ? GROUP BY a.root_id ORDER BY a.root_id",
                (job_id,),
            ).fetchall()
            input_tracks = int(
                connection.execute(
                    "SELECT COUNT(*) FROM local_tracks t WHERE t.local_album_id IN "
                    "(SELECT local_album_id FROM library_identity_repair_findings WHERE job_id = ?)",
                    (job_id,),
                ).fetchone()[0]
            )
            counts_by_finding: dict[str, int] = {}
            counts_by_reason: dict[str, int] = {}
            for finding in findings:
                counts_by_finding[str(finding["finding_code"])] = counts_by_finding.get(
                    str(finding["finding_code"]), 0
                ) + int(finding["count"])
                counts_by_reason[str(finding["reason_code"])] = counts_by_reason.get(
                    str(finding["reason_code"]), 0
                ) + int(finding["count"])
            total = sum(counts_by_finding.values())
            summary = {
                "total_identities": total,
                "remaining_identities": 0,
                "input_track_count": input_tracks,
                "playable_after_detach_track_count": input_tracks,
                "estimated_apply_changes": counts_by_finding.get("safe_detach", 0),
                "catalog_snapshot_revision": int(
                    connection.execute(
                        "SELECT input_catalog_revision FROM library_operation_jobs WHERE id = ?",
                        (job_id,),
                    ).fetchone()[0]
                ),
                "target_matcher_version": str(snapshot["target_matcher_version"]),
                "counts_by_finding": counts_by_finding,
                "counts_by_reason": counts_by_reason,
                "album_counts_by_root": {
                    str(root["root_id"]): int(root["count"]) for root in roots
                },
                "provider_deferred_count": counts_by_reason.get("PROVIDER_DEFERRED", 0),
                "failed_evidence_count": counts_by_reason.get(
                    "EVIDENCE_UNAVAILABLE", 0
                ),
            }
            connection.execute(
                "UPDATE library_repair_snapshots SET result_json = ? WHERE job_id = ?",
                (json.dumps(summary, sort_keys=True), job_id),
            )
            updated = connection.execute(
                "UPDATE library_operation_jobs SET state = 'ready', terminal_code = 'DRY_RUN_READY', "
                "lease_owner = NULL, lease_expires_at = NULL, heartbeat_at = NULL, updated_at = ?, "
                "row_revision = row_revision + 1, event_revision = event_revision + 1 "
                "WHERE id = ? AND state = 'running' AND lease_owner = ? RETURNING *",
                (now, job_id, worker_id),
            ).fetchone()
            if updated is None:
                raise StaleRevisionError(
                    "The repair job changed before it became ready."
                )
            self._bump_stream(connection, "operation")
            return dict(updated)

        return await self._write(operation)

    async def start_repair_apply(
        self, job_id: str, *, expected_row_revision: int, now: float
    ) -> dict[str, Any]:
        def operation(connection: sqlite3.Connection) -> dict[str, Any]:
            job = connection.execute(
                "SELECT * FROM library_operation_jobs WHERE id = ?", (job_id,)
            ).fetchone()
            if job is None:
                raise ResourceNotFoundError("Repair job not found.")
            snapshot = connection.execute(
                "SELECT phase FROM library_repair_snapshots WHERE job_id = ?",
                (job_id,),
            ).fetchone()
            if snapshot is not None and snapshot["phase"] == "apply":
                return dict(job)
            if (
                job["state"] != "ready"
                or int(job["row_revision"]) != expected_row_revision
            ):
                raise StaleRevisionError("The repair report changed before Apply.")
            findings = connection.execute(
                "SELECT * FROM library_identity_repair_findings WHERE job_id = ? "
                "AND apply_eligible = 1 AND state = 'open' ORDER BY id",
                (job_id,),
            ).fetchall()
            connection.execute(
                "DELETE FROM library_operation_work WHERE job_id = ?", (job_id,)
            )
            connection.executemany(
                "INSERT INTO library_operation_work "
                "(job_id, ordinal, local_album_id, expected_subject_revision, expected_input_revision, "
                "action, idempotency_key, updated_at) VALUES (?,?,?,?,?,?,?,?)",
                [
                    (
                        job_id,
                        ordinal,
                        finding["local_album_id"],
                        finding["expected_album_revision"],
                        f"{finding['expected_identity_revision'] or ''}:{finding['evidence_id'] or ''}",
                        "repair_apply",
                        f"{job_id}:{finding['id']}:apply",
                        now,
                    )
                    for ordinal, finding in enumerate(findings)
                ],
            )
            updated = connection.execute(
                "UPDATE library_operation_jobs SET state = 'queued', expected_work_count = ?, "
                "completed_count = 0, succeeded_count = 0, failed_count = 0, skipped_count = 0, "
                "terminal_code = NULL, terminal_at = NULL, updated_at = ?, row_revision = row_revision + 1, "
                "event_revision = event_revision + 1 WHERE id = ? RETURNING *",
                (len(findings), now, job_id),
            ).fetchone()
            connection.execute(
                "UPDATE library_repair_snapshots SET phase = 'apply' WHERE job_id = ?",
                (job_id,),
            )
            self._bump_stream(connection, "operation")
            return dict(updated)

        return await self._write(operation)

    async def apply_repair_work(
        self,
        job_id: str,
        ordinal: int,
        *,
        worker_id: str,
        expected_work_revision: int,
        actor_user_id: str,
        now: float,
    ) -> dict[str, Any]:
        def operation(connection: sqlite3.Connection) -> dict[str, Any]:
            work = connection.execute(
                "SELECT * FROM library_operation_work WHERE job_id = ? AND ordinal = ? "
                "AND state = 'running' AND row_revision = ?",
                (job_id, ordinal, expected_work_revision),
            ).fetchone()
            if work is None:
                raise StaleRevisionError("The repair Apply subject changed.")
            finding = connection.execute(
                "SELECT * FROM library_identity_repair_findings WHERE job_id = ? "
                "AND local_album_id = ? AND apply_eligible = 1 AND state = 'open'",
                (job_id, work["local_album_id"]),
            ).fetchone()
            album = connection.execute(
                "SELECT row_revision FROM local_albums WHERE id = ?",
                (work["local_album_id"],),
            ).fetchone()
            identity = connection.execute(
                "SELECT * FROM local_album_external_identities WHERE local_album_id = ?",
                (work["local_album_id"],),
            ).fetchone()
            state = "succeeded"
            failure_code = None
            if (
                finding is None
                or album is None
                or identity is None
                or int(album["row_revision"]) != int(work["expected_subject_revision"])
                or int(identity["row_revision"])
                != int(finding["expected_identity_revision"])
            ):
                state = "skipped"
                failure_code = "STALE_SUBJECT"
                if finding is not None:
                    connection.execute(
                        "UPDATE library_identity_repair_findings SET state = 'stale', "
                        "apply_result = 'STALE_SUBJECT', updated_at = ?, row_revision = row_revision + 1 "
                        "WHERE id = ?",
                        (now, finding["id"]),
                    )
            else:
                connection.execute(
                    "DELETE FROM local_track_external_identities WHERE local_track_id IN "
                    "(SELECT id FROM local_tracks WHERE local_album_id = ?)",
                    (work["local_album_id"],),
                )
                connection.execute(
                    "DELETE FROM local_album_external_identities WHERE local_album_id = ?",
                    (work["local_album_id"],),
                )
                review_id = str(uuid.uuid4())
                input_revision = f"repair:{job_id}:{finding['id']}"
                connection.execute(
                    "INSERT INTO library_identification_reviews "
                    "(id, local_album_id, state, reason_code, attempt_id, input_revision, "
                    "created_at, updated_at) VALUES (?, ?, 'needs_review', "
                    "'LEGACY_IDENTITY_FAILED_SAFETY_RULES', ?, ?, ?, ?)",
                    (
                        review_id,
                        work["local_album_id"],
                        identity["attempt_id"],
                        input_revision,
                        now,
                        now,
                    ),
                )
                action_id = str(uuid.uuid4())
                connection.execute(
                    "INSERT INTO library_catalog_actions "
                    "(id, actor_user_id, action_kind, local_album_id, operation_job_id, "
                    "before_json, after_json, reason_code, created_at) VALUES (?,?,?,?,?,?,?,?,?)",
                    (
                        action_id,
                        actor_user_id,
                        "repair_detach",
                        work["local_album_id"],
                        job_id,
                        json.dumps(
                            {"release_group_mbid": identity["release_group_mbid"]}
                        ),
                        json.dumps({"review_id": review_id}),
                        "LEGACY_IDENTITY_FAILED_SAFETY_RULES",
                        now,
                    ),
                )
                connection.execute(
                    "UPDATE library_identity_repair_findings SET state = 'applied', "
                    "apply_result = 'DETACHED', updated_at = ?, row_revision = row_revision + 1 "
                    "WHERE id = ?",
                    (now, finding["id"]),
                )
                self._bump_catalog(connection)
            connection.execute(
                "UPDATE library_operation_work SET state = ?, failure_code = ?, updated_at = ?, "
                "row_revision = row_revision + 1 WHERE job_id = ? AND ordinal = ?",
                (state, failure_code, now, job_id, ordinal),
            )
            counter = "succeeded_count" if state == "succeeded" else "skipped_count"
            connection.execute(
                f"UPDATE library_operation_jobs SET completed_count = completed_count + 1, "
                f"{counter} = {counter} + 1, updated_at = ?, row_revision = row_revision + 1, "
                "event_revision = event_revision + 1 WHERE id = ? AND state = 'running' AND lease_owner = ?",
                (now, job_id, worker_id),
            )
            self._bump_stream(connection, "operation")
            return {"state": state, "failure_code": failure_code}

        return await self._write(operation)

    async def list_repair_findings(
        self,
        job_id: str,
        *,
        limit: int,
        finding_codes: list[str] | None = None,
        cursor_updated_at: float | None = None,
        cursor_id: str | None = None,
    ) -> dict[str, Any]:
        def operation(connection: sqlite3.Connection) -> dict[str, Any]:
            params: list[Any] = [job_id]
            finding_filter = ""
            if finding_codes:
                placeholders = ",".join("?" for _ in finding_codes)
                finding_filter = f"AND f.finding_code IN ({placeholders})"
                params.extend(finding_codes)
            cursor = ""
            if cursor_updated_at is not None and cursor_id is not None:
                cursor = "AND (f.updated_at < ? OR (f.updated_at = ? AND f.id < ?))"
                params.extend((cursor_updated_at, cursor_updated_at, cursor_id))
            rows = connection.execute(
                "SELECT f.*, (SELECT r.id FROM library_identification_reviews r "
                "WHERE r.local_album_id = f.local_album_id ORDER BY r.updated_at DESC, r.id DESC "
                "LIMIT 1) review_id FROM library_identity_repair_findings f WHERE f.job_id = ? "
                f"{finding_filter} {cursor} ORDER BY f.updated_at DESC, f.id DESC LIMIT ?",
                (*params, limit + 1),
            ).fetchall()
            return {
                "rows": [dict(row) for row in rows[:limit]],
                "has_more": len(rows) > limit,
            }

        return await self._read(operation)

    async def save_migration_dry_run(
        self,
        migration_id: str,
        *,
        source_revision: str,
        root_revision: str,
        report_json: str,
        created_at: float,
    ) -> None:
        def operation(connection: sqlite3.Connection) -> None:
            existing = connection.execute(
                "SELECT source_revision, root_revision, state FROM library_migration_runs WHERE id = ?",
                (migration_id,),
            ).fetchone()
            if existing is not None and (
                existing["source_revision"] != source_revision
                or existing["root_revision"] != root_revision
            ):
                raise StaleRevisionError(
                    "The legacy migration input or library roots changed after this report was created."
                )
            connection.execute(
                "INSERT INTO library_migration_runs "
                "(id, source_revision, root_revision, state, report_json, started_at, updated_at) "
                "VALUES (?, ?, ?, 'dry_run', ?, ?, ?) "
                "ON CONFLICT(id) DO UPDATE SET report_json = excluded.report_json, "
                "updated_at = excluded.updated_at",
                (
                    migration_id,
                    source_revision,
                    root_revision,
                    report_json,
                    created_at,
                    created_at,
                ),
            )

        await self._write(operation)

    async def get_completed_migration_report(
        self,
        migration_id: str,
        *,
        source_revision: str,
        root_revision: str,
    ) -> str | None:
        def operation(connection: sqlite3.Connection) -> str | None:
            run = connection.execute(
                "SELECT source_revision, root_revision, state, report_json "
                "FROM library_migration_runs WHERE id = ?",
                (migration_id,),
            ).fetchone()
            if run is None:
                return None
            if (
                run["source_revision"] != source_revision
                or run["root_revision"] != root_revision
            ):
                raise StaleRevisionError(
                    "The legacy migration input or library roots changed after this report was created."
                )
            if run["state"] != "completed":
                return None
            marker = connection.execute(
                "SELECT source_revision FROM library_migration_markers "
                "WHERE marker = 'legacy_catalog_import_complete'"
            ).fetchone()
            if marker is None or marker["source_revision"] != source_revision:
                raise StaleRevisionError(
                    "The completed migration is missing its matching target marker."
                )
            return str(run["report_json"])

        return await self._read(operation)

    async def require_migration_input(
        self,
        migration_id: str,
        *,
        source_revision: str,
        root_revision: str,
    ) -> None:
        def operation(connection: sqlite3.Connection) -> None:
            run = connection.execute(
                "SELECT source_revision, root_revision FROM library_migration_runs WHERE id = ?",
                (migration_id,),
            ).fetchone()
            if run is None or (
                run["source_revision"] != source_revision
                or run["root_revision"] != root_revision
            ):
                raise StaleRevisionError(
                    "The migration report no longer matches the copied source and library roots."
                )

        await self._read(operation)

    async def apply_legacy_catalog_bundle(
        self,
        bundle: LegacyCatalogImportBundle,
        *,
        migration_run_id: str,
        source_revision: str,
        allow_existing_artists: bool = False,
        allow_existing_album: bool = False,
    ) -> bool:
        """Import one bounded catalog bundle and its provenance atomically."""

        def operation(connection: sqlite3.Connection) -> bool:
            run = connection.execute(
                "SELECT source_revision FROM library_migration_runs WHERE id = ?",
                (migration_run_id,),
            ).fetchone()
            if run is None or run["source_revision"] != source_revision:
                raise StaleRevisionError(
                    "The migration report no longer matches the copied source."
                )
            existing = []
            for provenance in bundle.provenance:
                row = connection.execute(
                    "SELECT target_kind, target_id, source_revision "
                    "FROM library_migration_provenance "
                    "WHERE source_kind = ? AND source_key = ?",
                    (provenance.source_kind, provenance.source_key),
                ).fetchone()
                if row is not None:
                    if (
                        row["target_kind"] != provenance.target_kind
                        or row["target_id"] != provenance.target_id
                        or row["source_revision"] != provenance.source_revision
                    ):
                        raise StaleRevisionError(
                            "A legacy catalog row changed after it was imported."
                        )
                    existing.append(row)
            if existing:
                if len(existing) != len(bundle.provenance):
                    raise StaleRevisionError(
                        "A legacy album import has incomplete provenance."
                    )
                return False

            album = bundle.membership.album
            existing_album = connection.execute(
                "SELECT 1 FROM local_albums WHERE id = ?", (album.id,)
            ).fetchone()
            if existing_album is not None and allow_existing_album:
                owned_album = connection.execute(
                    "SELECT 1 FROM library_migration_provenance p "
                    "JOIN local_tracks t ON t.id = p.target_id "
                    "WHERE p.migration_run_id = ? AND p.target_kind = 'local_track' "
                    "AND t.local_album_id = ? LIMIT 1",
                    (migration_run_id, album.id),
                ).fetchone()
                if owned_album is None:
                    raise StaleRevisionError(
                        "A legacy album collides with catalog data outside this migration."
                    )

            self._insert_catalog_membership(
                connection,
                bundle.membership,
                allow_existing_artists=allow_existing_artists,
                allow_existing_album=allow_existing_album,
            )
            if bundle.album_identity is not None:
                identity = bundle.album_identity
                connection.execute(
                    f"INSERT {'OR IGNORE ' if allow_existing_album else ''}INTO local_album_external_identities "
                    "(local_album_id, provider, release_group_mbid, release_mbid, "
                    "decision_source, matcher_version, attempt_id, selected_by_user_id, "
                    "selected_at, row_revision) VALUES (?, 'musicbrainz', ?, ?, ?, ?, ?, ?, ?, ?)",
                    (
                        identity.local_album_id,
                        identity.release_group_mbid,
                        identity.release_mbid,
                        identity.decision_source,
                        identity.matcher_version,
                        identity.attempt_id,
                        identity.selected_by_user_id,
                        identity.selected_at,
                        identity.row_revision,
                    ),
                )
            connection.executemany(
                f"INSERT {'OR IGNORE ' if allow_existing_artists else ''}INTO local_artist_external_identities "
                "(local_artist_id, provider, provider_artist_id, decision_source, attempt_id, "
                "selected_by_user_id, selected_at, row_revision) "
                "VALUES (?, 'musicbrainz', ?, ?, ?, ?, ?, ?)",
                [
                    (
                        identity.local_artist_id,
                        identity.provider_artist_id,
                        identity.decision_source,
                        identity.attempt_id,
                        identity.selected_by_user_id,
                        identity.selected_at,
                        identity.row_revision,
                    )
                    for identity in bundle.artist_identities
                ],
            )
            connection.executemany(
                "INSERT INTO local_track_external_identities "
                "(local_track_id, provider, recording_mbid, release_mbid, decision_source, "
                "attempt_id, selected_at, row_revision) "
                "VALUES (?, 'musicbrainz', ?, ?, ?, ?, ?, ?)",
                [
                    (
                        identity.local_track_id,
                        identity.recording_mbid,
                        identity.release_mbid,
                        identity.decision_source,
                        identity.attempt_id,
                        identity.selected_at,
                        identity.row_revision,
                    )
                    for identity in bundle.track_identities
                ],
            )
            connection.executemany(
                f"INSERT {'OR IGNORE ' if allow_existing_artists else ''}INTO local_artist_aliases "
                "(alias, local_artist_id, kind, created_at) VALUES (?,?,?,?)",
                [
                    (alias.alias, alias.local_artist_id, alias.kind, alias.created_at)
                    for alias in bundle.artist_aliases
                ],
            )
            connection.executemany(
                f"INSERT {'OR IGNORE ' if allow_existing_album else ''}INTO local_album_aliases "
                "(alias, local_album_id, kind, created_at) VALUES (?,?,?,?)",
                [
                    (alias.alias, alias.local_album_id, alias.kind, alias.created_at)
                    for alias in bundle.album_aliases
                ],
            )
            if bundle.artwork is not None:
                artwork = bundle.artwork
                connection.execute(
                    f"INSERT {'OR IGNORE ' if allow_existing_album else ''}INTO local_album_artwork "
                    "(local_album_id, cover_url, source, source_locator, version, updated_at, row_revision) "
                    "VALUES (?,?,?,?,?,?,?)",
                    (
                        artwork.local_album_id,
                        artwork.cover_url,
                        artwork.source,
                        artwork.source_locator,
                        artwork.version,
                        artwork.updated_at,
                        artwork.row_revision,
                    ),
                )
            connection.executemany(
                "INSERT INTO library_identification_reviews "
                "(id, local_album_id, local_track_id, state, reason_code, input_revision, "
                "created_at, updated_at, decided_at) VALUES (?,?,?,?,?,?,?,?,?)",
                [
                    (
                        review.id,
                        review.local_album_id,
                        review.local_track_id,
                        review.state,
                        review.reason_code,
                        review.input_revision,
                        review.created_at,
                        review.updated_at,
                        review.decided_at,
                    )
                    for review in bundle.reviews
                ],
            )
            if any(
                not self._migration_reference_matches(connection, provenance)
                for provenance in bundle.provenance
            ):
                raise StaleRevisionError(
                    "A migrated catalog row was not materialized as planned."
                )
            connection.executemany(
                "INSERT INTO library_migration_provenance "
                "(source_kind, source_key, target_kind, target_id, source_revision, "
                "imported_at, migration_run_id) VALUES (?,?,?,?,?,?,?)",
                [
                    (
                        provenance.source_kind,
                        provenance.source_key,
                        provenance.target_kind,
                        provenance.target_id,
                        provenance.source_revision,
                        provenance.imported_at,
                        migration_run_id,
                    )
                    for provenance in bundle.provenance
                ],
            )
            self._bump_catalog(connection)
            return True

        return await self._write(operation)

    async def apply_reference_provenance_batch(
        self,
        provenance_rows: list[MigrationProvenance],
        *,
        migration_run_id: str,
        source_revision: str,
        tombstones: list[MigrationTombstone] | None = None,
        copy_playlists: bool = True,
    ) -> int:
        def operation(connection: sqlite3.Connection) -> int:
            def row_value(row: sqlite3.Row, key: str) -> Any:
                return row[key] if key in row.keys() else None

            def stable_membership(
                target_kind: str, target_id: str
            ) -> tuple[str | None, str | None, str | None]:
                if target_kind == "local_track":
                    row = connection.execute(
                        "SELECT t.id AS track_id, t.local_album_id AS album_id, "
                        "COALESCE(ta.local_artist_id, a.album_artist_id) AS artist_id "
                        "FROM local_tracks t "
                        "JOIN local_albums a ON a.id = t.local_album_id "
                        "LEFT JOIN local_track_artists ta "
                        "ON ta.local_track_id = t.id AND ta.position = 0 "
                        "WHERE t.id = ?",
                        (target_id,),
                    ).fetchone()
                    if row is not None:
                        return row["track_id"], row["album_id"], row["artist_id"]
                elif target_kind == "local_album":
                    row = connection.execute(
                        "SELECT id, album_artist_id FROM local_albums WHERE id = ?",
                        (target_id,),
                    ).fetchone()
                    if row is not None:
                        return None, row["id"], row["album_artist_id"]
                elif target_kind == "local_artist":
                    return None, None, target_id
                return None, None, None

            def materialize_reference(provenance: MigrationProvenance) -> None:
                track_id, album_id, artist_id = stable_membership(
                    provenance.target_kind, provenance.target_id
                )
                if provenance.source_kind == "favorite":
                    user_id, item_kind, source_item_id = provenance.source_key.split(
                        ":", 2
                    )
                    connection.execute(
                        "INSERT OR IGNORE INTO library_user_favorites "
                        "(user_id, item_kind, item_id, created_at) "
                        "SELECT user_id, item_kind, ?, created_at FROM user_favorites "
                        "WHERE user_id = ? AND item_kind = ? AND item_id = ?",
                        (
                            provenance.target_id,
                            user_id,
                            item_kind,
                            source_item_id,
                        ),
                    )
                elif provenance.source_kind == "history":
                    source = connection.execute(
                        "SELECT * FROM play_history WHERE id = ?",
                        (provenance.source_key,),
                    ).fetchone()
                    if source is None:
                        return
                    connection.execute(
                        "INSERT OR IGNORE INTO library_play_history "
                        "(id, user_id, local_track_id, local_album_id, local_artist_id, "
                        "track_name, artist_name, album_name, recording_mbid, "
                        "release_group_mbid, duration_ms, source, played_at) "
                        "VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?)",
                        (
                            source["id"],
                            source["user_id"],
                            track_id,
                            album_id,
                            artist_id,
                            source["track_name"],
                            source["artist_name"],
                            row_value(source, "album_name"),
                            row_value(source, "recording_mbid"),
                            row_value(source, "release_group_mbid"),
                            row_value(source, "duration_ms"),
                            row_value(source, "source"),
                            source["played_at"],
                        ),
                    )
                elif provenance.source_kind == "playlist_track":
                    source = connection.execute(
                        "SELECT * FROM playlist_tracks WHERE id = ?",
                        (provenance.source_key,),
                    ).fetchone()
                    if source is None:
                        return
                    connection.execute(
                        "INSERT OR IGNORE INTO library_playlist_tracks "
                        "(id, playlist_id, position, track_name, artist_name, album_name, "
                        "album_id, artist_id, track_source_id, cover_url, source_type, "
                        "available_sources, format, track_number, disc_number, duration, "
                        "created_at, plex_rating_key, library_file_id, local_track_id, "
                        "local_album_id, local_artist_id, reference_tombstone_id) "
                        "VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)",
                        (
                            source["id"],
                            source["playlist_id"],
                            source["position"],
                            source["track_name"],
                            source["artist_name"],
                            source["album_name"],
                            row_value(source, "album_id"),
                            row_value(source, "artist_id"),
                            row_value(source, "track_source_id"),
                            row_value(source, "cover_url"),
                            source["source_type"],
                            row_value(source, "available_sources"),
                            row_value(source, "format"),
                            row_value(source, "track_number"),
                            row_value(source, "disc_number"),
                            row_value(source, "duration"),
                            source["created_at"],
                            row_value(source, "plex_rating_key"),
                            row_value(source, "library_file_id"),
                            track_id,
                            album_id,
                            artist_id,
                            provenance.target_id
                            if provenance.target_kind == "reference_tombstone"
                            else None,
                        ),
                    )
                elif provenance.source_kind == "album_release_pin":
                    connection.execute(
                        "INSERT OR IGNORE INTO library_album_release_pins "
                        "(local_album_id, release_group_mbid, release_mbid, "
                        "set_by_user_id, set_at) "
                        "SELECT ?, release_group_mbid, release_mbid, set_by_user_id, "
                        "set_at FROM album_release_pins WHERE release_group_mbid = ?",
                        (album_id, provenance.source_key),
                    )
                elif provenance.source_kind == "compat_bookmark" and track_id:
                    user_id, source_file_id = provenance.source_key.split(":", 1)
                    connection.execute(
                        "INSERT OR IGNORE INTO library_compat_bookmarks "
                        "(user_id, local_track_id, position_ms, comment, created_at, changed_at) "
                        "SELECT user_id, ?, position_ms, comment, created_at, changed_at "
                        "FROM compat_bookmarks WHERE user_id = ? AND file_id = ?",
                        (track_id, user_id, source_file_id),
                    )
                elif provenance.source_kind == "compat_play_queue":
                    connection.execute(
                        "INSERT OR IGNORE INTO library_compat_play_queues "
                        "(user_id, current_index, position_ms, updated_at, changed_by_client) "
                        "SELECT user_id, current_index, position_ms, updated_at, "
                        "changed_by_client FROM compat_play_queues WHERE user_id = ?",
                        (provenance.source_key,),
                    )
                elif provenance.source_kind == "compat_play_queue_item" and track_id:
                    user_id, item_index = provenance.source_key.rsplit(":", 1)
                    connection.execute(
                        "INSERT OR IGNORE INTO library_compat_play_queue_items "
                        "(user_id, item_index, local_track_id) VALUES (?, ?, ?)",
                        (user_id, int(item_index), track_id),
                    )
                elif provenance.source_kind == "jellyfin_id_map":
                    if provenance.target_kind == "reference_tombstone":
                        source = connection.execute(
                            "SELECT kind, internal_id FROM compat_id_map WHERE jf_id = ?",
                            (provenance.source_key,),
                        ).fetchone()
                        if source is None:
                            return
                        kind = str(source["kind"])
                        internal_id = str(source["internal_id"])
                    else:
                        kind = {
                            "local_artist": "artist",
                            "local_album": "album",
                            "local_track": "track",
                        }.get(provenance.target_kind, provenance.target_kind)
                        internal_id = provenance.target_id
                    connection.execute(
                        "INSERT OR IGNORE INTO library_compat_id_map "
                        "(jf_id, kind, internal_id) VALUES (?, ?, ?)",
                        (provenance.source_key, kind, internal_id),
                    )

            run = connection.execute(
                "SELECT source_revision FROM library_migration_runs WHERE id = ?",
                (migration_run_id,),
            ).fetchone()
            if run is None or run["source_revision"] != source_revision:
                raise StaleRevisionError(
                    "The migration report no longer matches the copied source."
                )
            if copy_playlists:
                playlist_columns = {
                    str(row["name"])
                    for row in connection.execute(
                        "PRAGMA table_info(playlists)"
                    ).fetchall()
                }
                connection.execute(
                    "INSERT OR IGNORE INTO library_playlists "
                    "(id, name, cover_image_path, created_at, updated_at, source_ref, "
                    "user_id, is_public) "
                    "SELECT id, name, "
                    f"{'cover_image_path' if 'cover_image_path' in playlist_columns else 'NULL'}, "
                    "created_at, updated_at, "
                    f"{'source_ref' if 'source_ref' in playlist_columns else 'NULL'}, "
                    f"{'user_id' if 'user_id' in playlist_columns else 'NULL'}, "
                    f"{'COALESCE(is_public, 0)' if 'is_public' in playlist_columns else '0'} "
                    "FROM playlists"
                )
            inserted = 0
            for tombstone in tombstones or []:
                connection.execute(
                    "INSERT INTO library_reference_tombstones "
                    "(id, source_kind, source_key, legacy_file_id, title, artist_name, "
                    "album_name, source_type, created_at) VALUES (?,?,?,?,?,?,?,?,?) "
                    "ON CONFLICT(source_kind, source_key) DO NOTHING",
                    (
                        tombstone.id,
                        tombstone.source_kind,
                        tombstone.source_key,
                        tombstone.legacy_file_id,
                        tombstone.title,
                        tombstone.artist_name,
                        tombstone.album_name,
                        tombstone.source_type,
                        tombstone.created_at,
                    ),
                )
            for provenance in provenance_rows:
                materialize_reference(provenance)
                if not self._migration_reference_matches(connection, provenance):
                    raise StaleRevisionError(
                        "A persisted legacy reference was not materialized as planned."
                    )
                existing = connection.execute(
                    "SELECT target_kind, target_id, source_revision "
                    "FROM library_migration_provenance "
                    "WHERE source_kind = ? AND source_key = ?",
                    (provenance.source_kind, provenance.source_key),
                ).fetchone()
                if existing is not None:
                    if (
                        existing["target_kind"] != provenance.target_kind
                        or existing["target_id"] != provenance.target_id
                        or existing["source_revision"] != provenance.source_revision
                    ):
                        raise StaleRevisionError(
                            "A persisted legacy reference changed after it was imported."
                        )
                    continue
                connection.execute(
                    "INSERT INTO library_migration_provenance "
                    "(source_kind, source_key, target_kind, target_id, source_revision, "
                    "imported_at, migration_run_id) VALUES (?,?,?,?,?,?,?)",
                    (
                        provenance.source_kind,
                        provenance.source_key,
                        provenance.target_kind,
                        provenance.target_id,
                        provenance.source_revision,
                        provenance.imported_at,
                        migration_run_id,
                    ),
                )
                inserted += 1
            return inserted

        return await self._write(operation)

    async def finish_migration(
        self,
        migration_id: str,
        *,
        source_revision: str,
        report_json: str,
        completed_at: float,
    ) -> int:
        def operation(connection: sqlite3.Connection) -> int:
            existing = connection.execute(
                "SELECT source_revision, state FROM library_migration_runs WHERE id = ?",
                (migration_id,),
            ).fetchone()
            if (
                existing is not None
                and existing["source_revision"] == source_revision
                and existing["state"] == "completed"
            ):
                marker = connection.execute(
                    "SELECT target_catalog_revision FROM library_migration_markers "
                    "WHERE marker = 'legacy_catalog_import_complete'"
                ).fetchone()
                if marker is None:
                    raise StaleRevisionError(
                        "The completed migration is missing its target marker."
                    )
                return int(marker["target_catalog_revision"])
            run = connection.execute(
                "UPDATE library_migration_runs SET state = 'completed', report_json = ?, "
                "updated_at = ?, completed_at = ?, row_revision = row_revision + 1 "
                "WHERE id = ? AND source_revision = ? AND state IN ('dry_run','applying') "
                "AND row_revision < ? RETURNING row_revision",
                (
                    report_json,
                    completed_at,
                    completed_at,
                    migration_id,
                    source_revision,
                    MAX_REVISION,
                ),
            ).fetchone()
            if run is None:
                self._refuse_max_revision(
                    connection,
                    table="library_migration_runs",
                    predicate="id = ?",
                    parameters=(migration_id,),
                )
                raise StaleRevisionError(
                    "The migration input changed before completion was recorded."
                )
            catalog_revision = self._bump_catalog(connection)
            connection.execute(
                "INSERT INTO library_migration_markers "
                "(marker, source_revision, target_catalog_revision, created_at) "
                "VALUES ('legacy_catalog_import_complete', ?, ?, ?) "
                "ON CONFLICT(marker) DO UPDATE SET source_revision = excluded.source_revision, "
                "target_catalog_revision = excluded.target_catalog_revision, "
                "created_at = excluded.created_at",
                (source_revision, catalog_revision, completed_at),
            )
            return catalog_revision

        return await self._write(operation)

    async def record_migration_provenance(
        self, provenance: MigrationProvenance, *, migration_run_id: str | None = None
    ) -> bool:
        def operation(connection: sqlite3.Connection) -> bool:
            existing = connection.execute(
                "SELECT target_kind, target_id, source_revision "
                "FROM library_migration_provenance WHERE source_kind = ? AND source_key = ?",
                (provenance.source_kind, provenance.source_key),
            ).fetchone()
            if existing is not None:
                if (
                    existing["target_kind"] == provenance.target_kind
                    and existing["target_id"] == provenance.target_id
                    and existing["source_revision"] == provenance.source_revision
                ):
                    return False
                raise StaleRevisionError(
                    "A legacy migration source changed after it was imported."
                )
            cursor = connection.execute(
                "INSERT INTO library_migration_provenance "
                "(source_kind, source_key, target_kind, target_id, source_revision, "
                "imported_at, migration_run_id) VALUES (?,?,?,?,?,?,?) "
                "ON CONFLICT(source_kind, source_key) DO NOTHING",
                (
                    provenance.source_kind,
                    provenance.source_key,
                    provenance.target_kind,
                    provenance.target_id,
                    provenance.source_revision,
                    provenance.imported_at,
                    migration_run_id,
                ),
            )
            return cursor.rowcount == 1

        return await self._write(operation)

    async def resolve_migrated_reference(
        self, source_kind: str, source_key: str
    ) -> tuple[str, str] | None:
        def operation(connection: sqlite3.Connection) -> tuple[str, str] | None:
            row = connection.execute(
                "SELECT target_kind, target_id FROM library_migration_provenance "
                "WHERE source_kind = ? AND source_key = ?",
                (source_kind, source_key),
            ).fetchone()
            return (
                (str(row["target_kind"]), str(row["target_id"]))
                if row is not None
                else None
            )

        return await self._read(operation)

    @staticmethod
    def _catalog_integrity_counts(
        connection: sqlite3.Connection,
    ) -> dict[str, int]:
        return {
            "foreign_key_violations": sum(
                1 for _row in connection.execute("PRAGMA foreign_key_check")
            ),
            "orphan_tracks": int(
                connection.execute(
                    "SELECT COUNT(*) FROM local_tracks t LEFT JOIN local_albums a "
                    "ON a.id = t.local_album_id WHERE a.id IS NULL"
                ).fetchone()[0]
            ),
            "duplicate_paths": int(
                connection.execute(
                    "SELECT COUNT(*) FROM (SELECT root_id, relative_path FROM local_tracks "
                    "GROUP BY root_id, relative_path HAVING COUNT(*) > 1)"
                ).fetchone()[0]
            ),
            "unresolved_provenance": int(
                connection.execute(
                    "SELECT COUNT(*) FROM library_migration_provenance "
                    "WHERE target_id IS NULL OR target_id = ''"
                ).fetchone()[0]
            ),
        }

    async def validate_catalog_integrity(self) -> dict[str, int]:
        return await self._read(self._catalog_integrity_counts)

    async def validate_migration_references(self) -> int:
        def operation(connection: sqlite3.Connection) -> int:
            return int(
                connection.execute(
                    "SELECT COUNT(*) FROM library_migration_provenance p WHERE NOT ("
                    "CASE p.target_kind "
                    "WHEN 'library_root' THEN p.source_kind = 'root' AND p.target_id != '' "
                    "WHEN 'library' THEN p.source_kind = 'jellyfin_id_map' AND p.target_id != '' "
                    "WHEN 'genre' THEN p.source_kind = 'jellyfin_id_map' AND p.target_id != '' "
                    "WHEN 'compat_play_queue' THEN EXISTS ("
                    "SELECT 1 FROM library_compat_play_queues q WHERE q.user_id = p.target_id) "
                    "WHEN 'local_track' THEN EXISTS ("
                    "SELECT 1 FROM local_tracks t WHERE t.id = p.target_id) "
                    "WHEN 'local_album' THEN EXISTS ("
                    "SELECT 1 FROM local_albums a WHERE a.id = p.target_id) "
                    "WHEN 'local_artist' THEN EXISTS ("
                    "SELECT 1 FROM local_artists a WHERE a.id = p.target_id) "
                    "WHEN 'playlist' THEN EXISTS ("
                    "SELECT 1 FROM library_playlists l WHERE l.id = p.target_id) "
                    "WHEN 'reference_tombstone' THEN EXISTS ("
                    "SELECT 1 FROM library_reference_tombstones t WHERE t.id = p.target_id) "
                    "ELSE 0 END "
                    "AND CASE p.source_kind "
                    "WHEN 'favorite' THEN EXISTS ("
                    "SELECT 1 FROM library_user_favorites f WHERE "
                    "f.user_id = SUBSTR(p.source_key, 1, INSTR(p.source_key, ':') - 1) "
                    "AND f.item_kind = SUBSTR("
                    "p.source_key, INSTR(p.source_key, ':') + 1, "
                    "INSTR(SUBSTR(p.source_key, INSTR(p.source_key, ':') + 1), ':') - 1) "
                    "AND f.item_id = p.target_id) "
                    "WHEN 'history' THEN EXISTS ("
                    "SELECT 1 FROM library_play_history h WHERE h.id = p.source_key AND ("
                    "(p.target_kind = 'local_track' AND h.local_track_id = p.target_id) OR "
                    "(p.target_kind = 'local_album' AND h.local_album_id = p.target_id) OR "
                    "(p.target_kind = 'local_artist' AND h.local_artist_id = p.target_id))) "
                    "WHEN 'playlist_track' THEN EXISTS ("
                    "SELECT 1 FROM library_playlist_tracks t WHERE t.id = p.source_key AND ("
                    "(p.target_kind = 'local_track' AND t.local_track_id = p.target_id) OR "
                    "(p.target_kind = 'local_album' AND t.local_album_id = p.target_id) OR "
                    "(p.target_kind = 'local_artist' AND t.local_artist_id = p.target_id) OR "
                    "(p.target_kind = 'reference_tombstone' "
                    "AND t.reference_tombstone_id = p.target_id))) "
                    "WHEN 'album_release_pin' THEN EXISTS ("
                    "SELECT 1 FROM library_album_release_pins r "
                    "WHERE r.local_album_id = p.target_id "
                    "AND r.release_group_mbid = p.source_key) "
                    "WHEN 'compat_bookmark' THEN EXISTS ("
                    "SELECT 1 FROM library_compat_bookmarks b WHERE "
                    "b.user_id = SUBSTR(p.source_key, 1, INSTR(p.source_key, ':') - 1) "
                    "AND b.local_track_id = p.target_id) "
                    "WHEN 'compat_play_queue' THEN p.source_key = p.target_id "
                    "WHEN 'compat_play_queue_item' THEN EXISTS ("
                    "SELECT 1 FROM library_compat_play_queue_items i "
                    "WHERE i.user_id = SUBSTR(p.source_key, 1, INSTR(p.source_key, ':') - 1) "
                    "AND i.item_index = CAST(SUBSTR("
                    "p.source_key, INSTR(p.source_key, ':') + 1) AS INTEGER) "
                    "AND p.source_key = i.user_id || ':' || i.item_index "
                    "AND i.local_track_id = p.target_id) "
                    "WHEN 'jellyfin_id_map' THEN EXISTS ("
                    "SELECT 1 FROM library_compat_id_map m WHERE m.jf_id = p.source_key "
                    "AND ((p.target_kind = 'reference_tombstone' AND EXISTS ("
                    "SELECT 1 FROM compat_id_map s WHERE s.jf_id = p.source_key "
                    "AND s.kind = m.kind AND s.internal_id = m.internal_id)) OR "
                    "(p.target_kind != 'reference_tombstone' "
                    "AND m.internal_id = p.target_id AND m.kind = CASE p.target_kind "
                    "WHEN 'local_artist' THEN 'artist' WHEN 'local_album' THEN 'album' "
                    "WHEN 'local_track' THEN 'track' ELSE p.target_kind END))) "
                    "WHEN 'review_row' THEN EXISTS ("
                    "SELECT 1 FROM library_identification_reviews r "
                    "WHERE r.local_track_id = p.target_id AND r.reason_code LIKE 'legacy_%') "
                    "WHEN 'manual_decision' THEN EXISTS ("
                    "SELECT 1 FROM library_identification_reviews r "
                    "WHERE r.local_track_id = p.target_id AND r.reason_code LIKE 'legacy_%') "
                    "WHEN 'native_album_alias' THEN EXISTS ("
                    "SELECT 1 FROM local_album_aliases a WHERE a.alias = p.source_key "
                    "AND a.local_album_id = p.target_id) "
                    "WHEN 'native_artist_alias' THEN EXISTS ("
                    "SELECT 1 FROM local_artist_aliases a WHERE a.alias = p.source_key "
                    "AND a.local_artist_id = p.target_id) "
                    "WHEN 'artwork_reference' THEN EXISTS ("
                    "SELECT 1 FROM local_album_artwork a WHERE a.local_album_id = p.target_id) "
                    "WHEN 'root' THEN 1 WHEN 'library_file' THEN 1 "
                    "WHEN 'subsonic_id' THEN 1 ELSE 0 END)"
                ).fetchone()[0]
            )

        return await self._read(operation)

    async def validate_migrated_catalog(self) -> dict[str, int]:
        invariants = await self.validate_catalog_integrity()
        unresolved_references = await self.validate_migration_references()
        return {**invariants, "unresolved_references": unresolved_references}

    async def get_target_startup_state(self) -> dict[str, Any]:
        def operation(connection: sqlite3.Connection) -> dict[str, Any]:
            marker = connection.execute(
                "SELECT source_revision, target_catalog_revision, created_at "
                "FROM library_migration_markers "
                "WHERE marker = 'legacy_catalog_import_complete'"
            ).fetchone()
            migration = None
            if marker is not None:
                migration = connection.execute(
                    "SELECT id, state, source_revision, completed_at "
                    "FROM library_migration_runs WHERE source_revision = ? "
                    "ORDER BY completed_at DESC, id DESC LIMIT 1",
                    (marker["source_revision"],),
                ).fetchone()
            catalog_revision = int(
                connection.execute(
                    "SELECT value FROM library_catalog_revision WHERE singleton = 1"
                ).fetchone()[0]
            )
            return {
                "marker": dict(marker) if marker is not None else None,
                "migration": dict(migration) if migration is not None else None,
                "catalog_revision": catalog_revision,
            }

        return await self._read(operation)

    async def get_migrated_root_ids(self) -> set[str]:
        def operation(connection: sqlite3.Connection) -> set[str]:
            return {
                str(row["target_id"])
                for row in connection.execute(
                    "SELECT target_id FROM library_migration_provenance "
                    "WHERE source_kind = 'root' AND target_kind = 'library_root'"
                ).fetchall()
            }

        return await self._read(operation)

    async def get_migration_provenance_counts(
        self, migration_run_id: str
    ) -> dict[str, int]:
        def operation(connection: sqlite3.Connection) -> dict[str, int]:
            return {
                str(row["source_kind"]): int(row["count"])
                for row in connection.execute(
                    "SELECT source_kind, COUNT(*) AS count "
                    "FROM library_migration_provenance WHERE migration_run_id = ? "
                    "GROUP BY source_kind",
                    (migration_run_id,),
                ).fetchall()
            }

        return await self._read(operation)

    async def get_bounded_migrated_artist_count(self) -> int:
        def operation(connection: sqlite3.Connection) -> int:
            return int(
                connection.execute(
                    "SELECT COUNT(*) FROM local_artists WHERE id NOT IN (?, ?)",
                    (VARIOUS_ARTISTS_ID, UNKNOWN_ARTIST_ID),
                ).fetchone()[0]
            )

        return await self._read(operation)

    async def get_bounded_migrated_catalog_counts(self) -> dict[str, int]:
        def operation(connection: sqlite3.Connection) -> dict[str, int]:
            identified_albums = int(
                connection.execute(
                    "SELECT COUNT(*) FROM local_albums a WHERE EXISTS ("
                    "SELECT 1 FROM local_album_external_identities e "
                    "WHERE e.local_album_id = a.id AND e.provider = 'musicbrainz')"
                ).fetchone()[0]
            )
            identified_tracks = int(
                connection.execute(
                    "SELECT COUNT(*) FROM local_tracks t WHERE EXISTS ("
                    "SELECT 1 FROM local_album_external_identities e "
                    "WHERE e.local_album_id = t.local_album_id "
                    "AND e.provider = 'musicbrainz')"
                ).fetchone()[0]
            )
            albums = int(
                connection.execute("SELECT COUNT(*) FROM local_albums").fetchone()[0]
            )
            tracks = int(
                connection.execute("SELECT COUNT(*) FROM local_tracks").fetchone()[0]
            )
            return {
                "identified_albums": identified_albums,
                "identified_tracks": identified_tracks,
                "local_only_albums": albums - identified_albums,
                "local_only_tracks": tracks - identified_tracks,
            }

        return await self._read(operation)

    async def get_policy_scope_counts(
        self, scopes: list[tuple[str, str]]
    ) -> dict[tuple[str, str], tuple[int, int]]:
        def operation(
            connection: sqlite3.Connection,
        ) -> dict[tuple[str, str], tuple[int, int]]:
            requested = [
                {
                    "root_id": root_id,
                    "relative_path": relative_path,
                    "prefix": relative_path.strip("/"),
                    "escaped_prefix": _escape_like(relative_path.strip("/")),
                }
                for root_id, relative_path in dict.fromkeys(scopes)
            ]
            if not requested:
                return {}
            rows = connection.execute(
                "WITH requested AS (SELECT "
                "json_extract(value, '$.root_id') root_id, "
                "json_extract(value, '$.relative_path') relative_path, "
                "json_extract(value, '$.prefix') prefix, "
                "json_extract(value, '$.escaped_prefix') escaped_prefix "
                "FROM json_each(?)) "
                "SELECT requested.root_id, requested.relative_path, "
                "COALESCE(SUM(track.availability = 'indexed'), 0) indexed_count, "
                "COALESCE(SUM(track.availability IN ('indexed','excluded')), 0) on_disk_count "
                "FROM requested LEFT JOIN local_tracks track ON track.root_id = requested.root_id "
                "AND (requested.prefix IN ('', '.') OR track.relative_path = requested.prefix "
                "OR track.relative_path LIKE requested.escaped_prefix || '/%' ESCAPE '\\') "
                "GROUP BY requested.root_id, requested.relative_path",
                (json.dumps(requested, sort_keys=True),),
            ).fetchall()
            return {
                (str(row["root_id"]), str(row["relative_path"])): (
                    int(row["indexed_count"]),
                    int(row["on_disk_count"]),
                )
                for row in rows
            }

        return await self._read(operation)

    async def get_policy_scope_total_counts(
        self, scopes: list[ScanScope]
    ) -> tuple[int, int]:
        def operation(connection: sqlite3.Connection) -> tuple[int, int]:
            normalized = sorted(
                {
                    (
                        scope.root_id,
                        scope.relative_path.strip("/")
                        if scope.relative_path.strip("/") not in {"", "."}
                        else ".",
                    )
                    for scope in scopes
                },
                key=lambda item: (item[0], len(item[1]), item[1]),
            )
            collapsed: list[tuple[str, str]] = []
            for root_id, prefix in normalized:
                if any(
                    parent_root == root_id
                    and (
                        parent_prefix == "."
                        or prefix == parent_prefix
                        or prefix.startswith(f"{parent_prefix}/")
                    )
                    for parent_root, parent_prefix in collapsed
                ):
                    continue
                collapsed.append((root_id, prefix))
            requested = [
                {
                    "root_id": root_id,
                    "prefix": prefix,
                    "escaped_prefix": _escape_like(prefix),
                }
                for root_id, prefix in collapsed
            ]
            if not requested:
                return (0, 0)
            row = connection.execute(
                "WITH requested AS (SELECT "
                "json_extract(value, '$.root_id') root_id, "
                "json_extract(value, '$.prefix') prefix, "
                "json_extract(value, '$.escaped_prefix') escaped_prefix "
                "FROM json_each(?)) "
                "SELECT COALESCE(SUM(track.availability = 'indexed'), 0) indexed_count, "
                "COALESCE(SUM(track.availability IN ('indexed','excluded')), 0) on_disk_count "
                "FROM local_tracks track WHERE EXISTS (SELECT 1 FROM requested "
                "WHERE requested.root_id = track.root_id AND (requested.prefix IN ('', '.') "
                "OR track.relative_path = requested.prefix "
                "OR track.relative_path LIKE requested.escaped_prefix || '/%' ESCAPE '\\'))",
                (json.dumps(requested, sort_keys=True),),
            ).fetchone()
            return (int(row["indexed_count"]), int(row["on_disk_count"]))

        return await self._read(operation)

    @staticmethod
    def _active_album_input(
        connection: sqlite3.Connection, album_id: str
    ) -> tuple[sqlite3.Row, list[sqlite3.Row], str] | None:
        album = connection.execute(
            "SELECT * FROM local_albums WHERE id = ? AND retired_into_album_id IS NULL",
            (album_id,),
        ).fetchone()
        if album is None:
            return None
        tracks = connection.execute(
            "SELECT * FROM local_tracks WHERE local_album_id = ? "
            "AND availability = 'indexed' ORDER BY disc_number, track_number, id",
            (album_id,),
        ).fetchall()
        if not tracks:
            return None
        return album, list(tracks), _album_input_revision(list(tracks))

    @classmethod
    def _contribution_row(
        cls, connection: sqlite3.Connection, contribution_id: str
    ) -> dict[str, Any] | None:
        row = connection.execute(
            "SELECT * FROM library_contribution_drafts WHERE id = ?",
            (contribution_id,),
        ).fetchone()
        if row is None:
            return None
        current = cls._active_album_input(connection, str(row["local_album_id"]))
        if current is None:
            result = dict(row)
            result["album_active"] = False
            result["current_album_row_revision"] = None
            result["current_input_revision"] = None
            return result
        album, _tracks, input_revision = current
        result = dict(row)
        result["album_active"] = True
        result["current_album_row_revision"] = int(album["row_revision"])
        result["current_input_revision"] = input_revision
        return result

    async def create_or_get_library_contribution(
        self,
        *,
        local_album_id: str,
        actor_user_id: str,
        album_row_revision: int,
        input_revision: str,
        local_snapshot_json: str,
        resolved_draft_json: str,
        source_selection_json: str,
        now: float,
    ) -> dict[str, Any]:
        def operation(connection: sqlite3.Connection) -> dict[str, Any]:
            current = self._active_album_input(connection, local_album_id)
            if current is None:
                raise ResourceNotFoundError("Library album not found.")
            album, _tracks, current_input_revision = current
            if (
                int(album["row_revision"]) != album_row_revision
                or current_input_revision != input_revision
            ):
                raise StaleRevisionError(
                    "The album changed before the contribution could be created."
                )
            existing = connection.execute(
                "SELECT id FROM library_contribution_drafts WHERE local_album_id = ? "
                "AND state NOT IN ('linked','cancelled','stale') "
                "ORDER BY created_at DESC LIMIT 1",
                (local_album_id,),
            ).fetchone()
            if existing is not None:
                result = self._contribution_row(connection, str(existing["id"]))
                if result is None:
                    raise ResourceNotFoundError("Library contribution not found.")
                return result
            contribution_id = str(uuid.uuid4())
            try:
                connection.execute(
                    "INSERT INTO library_contribution_drafts "
                    "(id, local_album_id, created_by_user_id, updated_by_user_id, state, "
                    "album_row_revision, input_revision, local_snapshot_json, "
                    "resolved_draft_json, source_selection_json, created_at, updated_at) "
                    "VALUES (?, ?, ?, ?, 'draft', ?, ?, ?, ?, ?, ?, ?)",
                    (
                        contribution_id,
                        local_album_id,
                        actor_user_id,
                        actor_user_id,
                        album_row_revision,
                        input_revision,
                        local_snapshot_json,
                        resolved_draft_json,
                        source_selection_json,
                        now,
                        now,
                    ),
                )
            except sqlite3.IntegrityError:
                existing = connection.execute(
                    "SELECT id FROM library_contribution_drafts WHERE local_album_id = ? "
                    "AND state NOT IN ('linked','cancelled','stale') LIMIT 1",
                    (local_album_id,),
                ).fetchone()
                if existing is None:
                    raise
                contribution_id = str(existing["id"])
            result = self._contribution_row(connection, contribution_id)
            if result is None:
                raise ResourceNotFoundError("Library contribution not found.")
            return result

        return await self._write(operation)

    async def get_library_contribution(
        self, contribution_id: str
    ) -> dict[str, Any] | None:
        def operation(connection: sqlite3.Connection) -> dict[str, Any] | None:
            return self._contribution_row(connection, contribution_id)

        return await self._read(operation)

    async def get_active_album_contribution(
        self, local_album_id: str
    ) -> dict[str, Any] | None:
        def operation(connection: sqlite3.Connection) -> dict[str, Any] | None:
            row = connection.execute(
                "SELECT id FROM library_contribution_drafts WHERE local_album_id = ? "
                "AND state NOT IN ('linked','cancelled','stale') "
                "ORDER BY created_at DESC LIMIT 1",
                (local_album_id,),
            ).fetchone()
            if row is None:
                return None
            contribution = self._contribution_row(connection, str(row["id"]))
            return (
                contribution
                if contribution is not None and contribution["album_active"]
                else None
            )

        return await self._read(operation)

    async def update_library_contribution_draft(
        self,
        *,
        contribution_id: str,
        expected_row_revision: int,
        actor_user_id: str,
        resolved_draft_json: str,
        state: str,
        now: float,
    ) -> dict[str, Any]:
        def operation(connection: sqlite3.Connection) -> dict[str, Any]:
            row = self._contribution_row(connection, contribution_id)
            if row is None or not row["album_active"]:
                raise ResourceNotFoundError("Library contribution not found.")
            if row["state"] not in {"draft", "ready", "needs_review"}:
                raise ConflictError("This contribution can no longer be edited.")
            if int(row["row_revision"]) != expected_row_revision:
                raise StaleRevisionError(
                    "The contribution changed before this edit was saved."
                )
            if row["input_revision"] != row["current_input_revision"] or int(
                row["album_row_revision"]
            ) != int(row["current_album_row_revision"]):
                connection.execute(
                    "UPDATE library_contribution_drafts SET state = 'stale', "
                    "terminal_at = ?, updated_at = ?, row_revision = row_revision + 1 "
                    "WHERE id = ? AND row_revision = ? AND row_revision < ?",
                    (
                        now,
                        now,
                        contribution_id,
                        expected_row_revision,
                        MAX_REVISION,
                    ),
                )
                raise StaleRevisionError(
                    "The local album changed. Rebuild the contribution before editing."
                )
            updated = connection.execute(
                "UPDATE library_contribution_drafts SET resolved_draft_json = ?, state = ?, "
                "updated_by_user_id = ?, duplicate_result_json = NULL, "
                "duplicate_checked_at = NULL, duplicate_input_revision = NULL, "
                "updated_at = ?, row_revision = row_revision + 1 "
                "WHERE id = ? AND row_revision = ? AND row_revision < ? RETURNING *",
                (
                    resolved_draft_json,
                    state,
                    actor_user_id,
                    now,
                    contribution_id,
                    expected_row_revision,
                    MAX_REVISION,
                ),
            ).fetchone()
            if updated is None:
                self._refuse_max_revision(
                    connection,
                    table="library_contribution_drafts",
                    predicate="id = ?",
                    parameters=(contribution_id,),
                )
                raise StaleRevisionError(
                    "The contribution changed before this edit was saved."
                )
            result = self._contribution_row(connection, contribution_id)
            if result is None:
                raise ResourceNotFoundError("Library contribution not found.")
            return result

        return await self._write(operation)

    async def rebuild_library_contribution(
        self,
        *,
        contribution_id: str,
        expected_row_revision: int,
        actor_user_id: str,
        album_row_revision: int,
        input_revision: str,
        local_snapshot_json: str,
        resolved_draft_json: str,
        source_selection_json: str,
        now: float,
    ) -> dict[str, Any]:
        def operation(connection: sqlite3.Connection) -> dict[str, Any]:
            existing = connection.execute(
                "SELECT * FROM library_contribution_drafts WHERE id = ?",
                (contribution_id,),
            ).fetchone()
            if existing is None:
                raise ResourceNotFoundError("Library contribution not found.")
            if int(existing["row_revision"]) != expected_row_revision:
                raise StaleRevisionError(
                    "The contribution changed before it could be rebuilt."
                )
            current = self._active_album_input(
                connection, str(existing["local_album_id"])
            )
            if current is None:
                raise ResourceNotFoundError("Library album not found.")
            album, _tracks, current_input_revision = current
            if (
                int(album["row_revision"]) != album_row_revision
                or current_input_revision != input_revision
            ):
                raise StaleRevisionError(
                    "The album changed before the contribution could be rebuilt."
                )
            connection.execute(
                "UPDATE library_contribution_drafts SET state = 'stale', terminal_at = ?, "
                "updated_at = ?, row_revision = row_revision + 1 "
                "WHERE id = ? AND row_revision = ?",
                (now, now, contribution_id, expected_row_revision),
            )
            connection.execute(
                "UPDATE library_contribution_callback_tokens SET consumed_at = ? "
                "WHERE contribution_id = ? AND consumed_at IS NULL",
                (now, contribution_id),
            )
            connection.execute(
                "UPDATE library_contribution_verification_jobs SET state = 'cancelled', "
                "terminal_at = ?, updated_at = ?, row_revision = row_revision + 1, "
                "event_revision = event_revision + 1 WHERE contribution_id = ? "
                "AND state IN ('queued','running')",
                (now, now, contribution_id),
            )
            new_id = str(uuid.uuid4())
            connection.execute(
                "INSERT INTO library_contribution_drafts "
                "(id, local_album_id, created_by_user_id, updated_by_user_id, state, "
                "album_row_revision, input_revision, local_snapshot_json, "
                "resolved_draft_json, source_selection_json, created_at, updated_at) "
                "VALUES (?, ?, ?, ?, 'draft', ?, ?, ?, ?, ?, ?, ?)",
                (
                    new_id,
                    existing["local_album_id"],
                    actor_user_id,
                    actor_user_id,
                    album_row_revision,
                    input_revision,
                    local_snapshot_json,
                    resolved_draft_json,
                    source_selection_json,
                    now,
                    now,
                ),
            )
            result = self._contribution_row(connection, new_id)
            if result is None:
                raise ResourceNotFoundError("Library contribution not found.")
            return result

        return await self._write(operation)

    async def cancel_library_contribution(
        self,
        *,
        contribution_id: str,
        expected_row_revision: int,
        actor_user_id: str,
        now: float,
    ) -> dict[str, Any]:
        def operation(connection: sqlite3.Connection) -> dict[str, Any]:
            row = self._contribution_row(connection, contribution_id)
            if row is None or not row["album_active"]:
                raise ResourceNotFoundError("Library contribution not found.")
            if row["state"] in {"linked", "cancelled", "stale"}:
                raise ConflictError("This contribution is already closed.")
            updated = connection.execute(
                "UPDATE library_contribution_drafts SET state = 'cancelled', "
                "updated_by_user_id = ?, terminal_at = ?, updated_at = ?, "
                "seed_snapshot_json = NULL, row_revision = row_revision + 1 "
                "WHERE id = ? AND row_revision = ? AND row_revision < ? RETURNING id",
                (
                    actor_user_id,
                    now,
                    now,
                    contribution_id,
                    expected_row_revision,
                    MAX_REVISION,
                ),
            ).fetchone()
            if updated is None:
                self._refuse_max_revision(
                    connection,
                    table="library_contribution_drafts",
                    predicate="id = ?",
                    parameters=(contribution_id,),
                )
                raise StaleRevisionError(
                    "The contribution changed before it could be cancelled."
                )
            connection.execute(
                "UPDATE library_contribution_callback_tokens SET consumed_at = ? "
                "WHERE contribution_id = ? AND consumed_at IS NULL",
                (now, contribution_id),
            )
            connection.execute(
                "UPDATE library_contribution_verification_jobs SET state = 'cancelled', "
                "terminal_at = ?, updated_at = ?, row_revision = row_revision + 1, "
                "event_revision = event_revision + 1 WHERE contribution_id = ? "
                "AND state IN ('queued','running')",
                (now, now, contribution_id),
            )
            result = dict(
                connection.execute(
                    "SELECT * FROM library_contribution_drafts WHERE id = ?",
                    (contribution_id,),
                ).fetchone()
            )
            result["current_album_row_revision"] = row["current_album_row_revision"]
            result["current_input_revision"] = row["current_input_revision"]
            result["album_active"] = row["album_active"]
            return result

        return await self._write(operation)

    async def mark_library_contribution_stale(
        self, *, contribution_id: str, expected_row_revision: int, now: float
    ) -> dict[str, Any]:
        def operation(connection: sqlite3.Connection) -> dict[str, Any]:
            row = connection.execute(
                "SELECT * FROM library_contribution_drafts WHERE id = ?",
                (contribution_id,),
            ).fetchone()
            if row is None:
                raise ResourceNotFoundError("Library contribution not found.")
            if str(row["state"]) not in {
                "linked",
                "cancelled",
                "stale",
            }:
                updated = connection.execute(
                    "UPDATE library_contribution_drafts SET state = 'stale', "
                    "terminal_at = ?, updated_at = ?, seed_snapshot_json = NULL, "
                    "row_revision = row_revision + 1 WHERE id = ? AND row_revision = ? "
                    "AND row_revision < ?",
                    (
                        now,
                        now,
                        contribution_id,
                        expected_row_revision,
                        MAX_REVISION,
                    ),
                )
                if updated.rowcount != 1:
                    if int(row["row_revision"]) >= MAX_REVISION:
                        raise RevisionOverflowError(
                            "The contribution revision cannot be increased."
                        )
                    raise StaleRevisionError(
                        "The contribution changed before it could be marked stale."
                    )
                connection.execute(
                    "UPDATE library_contribution_verification_jobs SET state = 'cancelled', "
                    "terminal_at = ?, updated_at = ?, row_revision = row_revision + 1, "
                    "event_revision = event_revision + 1 WHERE contribution_id = ? "
                    "AND state IN ('queued','running')",
                    (now, now, contribution_id),
                )
            result = dict(
                connection.execute(
                    "SELECT * FROM library_contribution_drafts WHERE id = ?",
                    (contribution_id,),
                ).fetchone()
            )
            current = self._active_album_input(
                connection, str(result["local_album_id"])
            )
            result["album_active"] = current is not None
            result["current_album_row_revision"] = (
                int(current[0]["row_revision"]) if current is not None else None
            )
            result["current_input_revision"] = (
                current[2] if current is not None else None
            )
            return result

        return await self._write(operation)

    async def upsert_local_entity_source_link(
        self,
        *,
        local_artist_id: str | None,
        local_album_id: str | None,
        local_track_id: str | None,
        provider: str,
        external_entity_type: str,
        external_id: str,
        canonical_url: str,
        decision_source: str,
        selected_by_user_id: str | None,
        verified_at: float,
        now: float,
    ) -> dict[str, Any]:
        subjects = [local_artist_id, local_album_id, local_track_id]
        if sum(subject is not None for subject in subjects) != 1:
            raise ValidationError("A source link requires exactly one local subject.")

        def operation(connection: sqlite3.Connection) -> dict[str, Any]:
            columns = (
                ("local_artist_id", local_artist_id)
                if local_artist_id is not None
                else (
                    ("local_album_id", local_album_id)
                    if local_album_id is not None
                    else ("local_track_id", local_track_id)
                )
            )
            subject_column, subject_id = columns
            existing = connection.execute(
                f"SELECT * FROM local_entity_source_links WHERE {subject_column} = ? "
                "AND provider = ? AND external_entity_type = ? AND external_id = ?",
                (subject_id, provider, external_entity_type, external_id),
            ).fetchone()
            if existing is not None:
                updated = connection.execute(
                    "UPDATE local_entity_source_links SET canonical_url = ?, "
                    "decision_source = ?, selected_by_user_id = ?, verified_at = ?, "
                    "updated_at = ?, row_revision = row_revision + 1 WHERE id = ? "
                    "AND row_revision < ? RETURNING *",
                    (
                        canonical_url,
                        decision_source,
                        selected_by_user_id,
                        verified_at,
                        now,
                        existing["id"],
                        MAX_REVISION,
                    ),
                ).fetchone()
                if updated is None:
                    raise RevisionOverflowError(
                        "The source-link revision cannot be increased."
                    )
                return dict(updated)
            link_id = str(uuid.uuid4())
            connection.execute(
                "INSERT INTO local_entity_source_links "
                "(id, local_artist_id, local_album_id, local_track_id, provider, "
                "external_entity_type, external_id, canonical_url, decision_source, "
                "selected_by_user_id, verified_at, created_at, updated_at) "
                "VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
                (
                    link_id,
                    local_artist_id,
                    local_album_id,
                    local_track_id,
                    provider,
                    external_entity_type,
                    external_id,
                    canonical_url,
                    decision_source,
                    selected_by_user_id,
                    verified_at,
                    now,
                    now,
                ),
            )
            return dict(
                connection.execute(
                    "SELECT * FROM local_entity_source_links WHERE id = ?", (link_id,)
                ).fetchone()
            )

        return await self._write(operation)

    async def remove_local_album_source_link(
        self,
        *,
        local_album_id: str,
        provider: str,
        external_entity_type: str,
        external_id: str,
    ) -> bool:
        def operation(connection: sqlite3.Connection) -> bool:
            result = connection.execute(
                "DELETE FROM local_entity_source_links WHERE local_album_id = ? "
                "AND provider = ? AND external_entity_type = ? AND external_id = ?",
                (local_album_id, provider, external_entity_type, external_id),
            )
            return result.rowcount > 0

        return await self._write(operation)

    async def select_discogs_source_for_contribution(
        self,
        *,
        contribution_id: str,
        expected_row_revision: int,
        actor_user_id: str,
        release_id: str,
        canonical_url: str,
        source_selection_json: str,
        provider_snapshot_expires_at: float,
        verified_at: float,
        now: float,
    ) -> dict[str, Any]:
        def operation(connection: sqlite3.Connection) -> dict[str, Any]:
            row = self._contribution_row(connection, contribution_id)
            if row is None or not row["album_active"]:
                raise ResourceNotFoundError("Library contribution not found.")
            if row["state"] not in {"draft", "ready", "needs_review"}:
                raise ConflictError("This contribution can no longer be edited.")
            if int(row["row_revision"]) != expected_row_revision:
                raise StaleRevisionError(
                    "The contribution changed before the source was selected."
                )
            if row["input_revision"] != row["current_input_revision"]:
                raise StaleRevisionError(
                    "The local album changed. Rebuild the contribution first."
                )
            album_id = str(row["local_album_id"])
            connection.execute(
                "DELETE FROM local_entity_source_links WHERE local_album_id = ? "
                "AND provider = 'discogs' AND external_entity_type = 'release' "
                "AND external_id <> ?",
                (album_id, release_id),
            )
            existing = connection.execute(
                "SELECT id, row_revision FROM local_entity_source_links "
                "WHERE local_album_id = ? AND provider = 'discogs' "
                "AND external_entity_type = 'release' AND external_id = ?",
                (album_id, release_id),
            ).fetchone()
            if existing is None:
                connection.execute(
                    "INSERT INTO local_entity_source_links "
                    "(id, local_album_id, provider, external_entity_type, external_id, "
                    "canonical_url, decision_source, selected_by_user_id, verified_at, "
                    "created_at, updated_at) VALUES (?, ?, 'discogs', 'release', ?, ?, "
                    "'curator_selected', ?, ?, ?, ?)",
                    (
                        str(uuid.uuid4()),
                        album_id,
                        release_id,
                        canonical_url,
                        actor_user_id,
                        verified_at,
                        now,
                        now,
                    ),
                )
            else:
                if int(existing["row_revision"]) >= MAX_REVISION:
                    raise RevisionOverflowError(
                        "The source-link revision cannot be increased."
                    )
                connection.execute(
                    "UPDATE local_entity_source_links SET canonical_url = ?, "
                    "selected_by_user_id = ?, verified_at = ?, updated_at = ?, "
                    "row_revision = row_revision + 1 WHERE id = ?",
                    (
                        canonical_url,
                        actor_user_id,
                        verified_at,
                        now,
                        existing["id"],
                    ),
                )
            updated = connection.execute(
                "UPDATE library_contribution_drafts SET source_selection_json = ?, "
                "provider_snapshot_expires_at = ?, updated_by_user_id = ?, "
                "duplicate_result_json = NULL, duplicate_checked_at = NULL, "
                "duplicate_input_revision = NULL, updated_at = ?, "
                "row_revision = row_revision + 1 WHERE id = ? AND row_revision = ? "
                "AND row_revision < ? RETURNING id",
                (
                    source_selection_json,
                    provider_snapshot_expires_at,
                    actor_user_id,
                    now,
                    contribution_id,
                    expected_row_revision,
                    MAX_REVISION,
                ),
            ).fetchone()
            if updated is None:
                self._refuse_max_revision(
                    connection,
                    table="library_contribution_drafts",
                    predicate="id = ?",
                    parameters=(contribution_id,),
                )
                raise StaleRevisionError(
                    "The contribution changed before the source was selected."
                )
            result = self._contribution_row(connection, contribution_id)
            if result is None:
                raise ResourceNotFoundError("Library contribution not found.")
            return result

        return await self._write(operation)

    async def remove_discogs_source_from_contribution(
        self,
        *,
        contribution_id: str,
        expected_row_revision: int,
        actor_user_id: str,
        source_selection_json: str,
        resolved_draft_json: str,
        state: str,
        now: float,
    ) -> dict[str, Any]:
        def operation(connection: sqlite3.Connection) -> dict[str, Any]:
            row = self._contribution_row(connection, contribution_id)
            if row is None or not row["album_active"]:
                raise ResourceNotFoundError("Library contribution not found.")
            if row["state"] not in {"draft", "ready", "needs_review"}:
                raise ConflictError("This contribution can no longer be edited.")
            if int(row["row_revision"]) != expected_row_revision:
                raise StaleRevisionError(
                    "The contribution changed before the source was removed."
                )
            connection.execute(
                "DELETE FROM local_entity_source_links WHERE local_album_id = ? "
                "AND provider = 'discogs' AND external_entity_type = 'release'",
                (row["local_album_id"],),
            )
            updated = connection.execute(
                "UPDATE library_contribution_drafts SET source_selection_json = ?, "
                "resolved_draft_json = ?, provider_snapshot_expires_at = NULL, "
                "state = ?, updated_by_user_id = ?, duplicate_result_json = NULL, "
                "duplicate_checked_at = NULL, duplicate_input_revision = NULL, "
                "updated_at = ?, row_revision = row_revision + 1 "
                "WHERE id = ? AND row_revision = ? AND row_revision < ? RETURNING id",
                (
                    source_selection_json,
                    resolved_draft_json,
                    state,
                    actor_user_id,
                    now,
                    contribution_id,
                    expected_row_revision,
                    MAX_REVISION,
                ),
            ).fetchone()
            if updated is None:
                self._refuse_max_revision(
                    connection,
                    table="library_contribution_drafts",
                    predicate="id = ?",
                    parameters=(contribution_id,),
                )
                raise StaleRevisionError(
                    "The contribution changed before the source was removed."
                )
            result = self._contribution_row(connection, contribution_id)
            if result is None:
                raise ResourceNotFoundError("Library contribution not found.")
            return result

        return await self._write(operation)

    async def issue_library_contribution_callback_token(
        self,
        *,
        token_hash: str,
        contribution_id: str,
        requested_by_user_id: str,
        expires_at: float,
        now: float,
    ) -> None:
        def operation(connection: sqlite3.Connection) -> None:
            contribution = self._contribution_row(connection, contribution_id)
            if contribution is None or not contribution["album_active"]:
                raise ResourceNotFoundError("Library contribution not found.")
            if contribution["state"] not in {"ready", "seeded"}:
                raise ConflictError("This contribution is not ready for MusicBrainz.")
            connection.execute(
                "INSERT INTO library_contribution_callback_tokens "
                "(token_hash, contribution_id, requested_by_user_id, expires_at, created_at) "
                "VALUES (?, ?, ?, ?, ?)",
                (
                    token_hash,
                    contribution_id,
                    requested_by_user_id,
                    expires_at,
                    now,
                ),
            )

        await self._write(operation)

    async def record_library_contribution_duplicate_result(
        self,
        *,
        contribution_id: str,
        expected_row_revision: int,
        actor_user_id: str,
        duplicate_result_json: str,
        duplicate_input_revision: str,
        state: str,
        now: float,
    ) -> dict[str, Any]:
        def operation(connection: sqlite3.Connection) -> dict[str, Any]:
            row = self._contribution_row(connection, contribution_id)
            if row is None or not row["album_active"]:
                raise ResourceNotFoundError("Library contribution not found.")
            if row["state"] not in {"ready", "needs_review"}:
                raise ConflictError("Complete the contribution draft first.")
            if int(row["row_revision"]) != expected_row_revision:
                raise StaleRevisionError(
                    "The contribution changed before duplicate results were saved."
                )
            if (
                row["input_revision"] != row["current_input_revision"]
                or duplicate_input_revision != row["input_revision"]
            ):
                raise StaleRevisionError(
                    "The local album changed before duplicate results were saved."
                )
            updated = connection.execute(
                "UPDATE library_contribution_drafts SET duplicate_result_json = ?, "
                "duplicate_checked_at = ?, duplicate_input_revision = ?, state = ?, "
                "updated_by_user_id = ?, updated_at = ?, row_revision = row_revision + 1 "
                "WHERE id = ? AND row_revision = ? AND row_revision < ? RETURNING id",
                (
                    duplicate_result_json,
                    now,
                    duplicate_input_revision,
                    state,
                    actor_user_id,
                    now,
                    contribution_id,
                    expected_row_revision,
                    MAX_REVISION,
                ),
            ).fetchone()
            if updated is None:
                self._refuse_max_revision(
                    connection,
                    table="library_contribution_drafts",
                    predicate="id = ?",
                    parameters=(contribution_id,),
                )
                raise StaleRevisionError(
                    "The contribution changed before duplicate results were saved."
                )
            result = self._contribution_row(connection, contribution_id)
            if result is None:
                raise ResourceNotFoundError("Library contribution not found.")
            return result

        return await self._write(operation)

    async def prepare_library_contribution_seed(
        self,
        *,
        contribution_id: str,
        expected_row_revision: int,
        actor_user_id: str,
        token_hash: str,
        token_expires_at: float,
        seed_snapshot_json: str,
        seed_hash: str,
        now: float,
    ) -> dict[str, Any]:
        def operation(connection: sqlite3.Connection) -> dict[str, Any]:
            row = self._contribution_row(connection, contribution_id)
            if row is None or not row["album_active"]:
                raise ResourceNotFoundError("Library contribution not found.")
            if (
                row["state"] not in {"ready", "seeded"}
                or row["duplicate_result_json"] is None
            ):
                raise ConflictError("Run the MusicBrainz duplicate check first.")
            if int(row["row_revision"]) != expected_row_revision:
                raise StaleRevisionError(
                    "The contribution changed before the editor could be opened."
                )
            if (
                row["input_revision"] != row["current_input_revision"]
                or row["duplicate_input_revision"] != row["input_revision"]
            ):
                raise StaleRevisionError(
                    "The local album changed before the editor could be opened."
                )
            duplicate = json.loads(str(row["duplicate_result_json"]))
            if any(
                bool(candidate.get("exact"))
                for candidate in duplicate.get("candidates", [])
            ):
                raise ConflictError(
                    "An exact MusicBrainz release already exists for this Discogs source."
                )
            connection.execute(
                "UPDATE library_contribution_callback_tokens SET consumed_at = ? "
                "WHERE contribution_id = ? AND consumed_at IS NULL",
                (now, contribution_id),
            )
            connection.execute(
                "INSERT INTO library_contribution_callback_tokens "
                "(token_hash, contribution_id, requested_by_user_id, expires_at, created_at) "
                "VALUES (?, ?, ?, ?, ?)",
                (token_hash, contribution_id, actor_user_id, token_expires_at, now),
            )
            updated = connection.execute(
                "UPDATE library_contribution_drafts SET state = 'seeded', "
                "seed_snapshot_json = ?, seed_hash = ?, seeded_at = ?, "
                "updated_by_user_id = ?, updated_at = ?, row_revision = row_revision + 1 "
                "WHERE id = ? AND row_revision = ? AND row_revision < ? RETURNING id",
                (
                    seed_snapshot_json,
                    seed_hash,
                    now,
                    actor_user_id,
                    now,
                    contribution_id,
                    expected_row_revision,
                    MAX_REVISION,
                ),
            ).fetchone()
            if updated is None:
                self._refuse_max_revision(
                    connection,
                    table="library_contribution_drafts",
                    predicate="id = ?",
                    parameters=(contribution_id,),
                )
                raise StaleRevisionError(
                    "The contribution changed before the editor could be opened."
                )
            result = self._contribution_row(connection, contribution_id)
            if result is None:
                raise ResourceNotFoundError("Library contribution not found.")
            return result

        return await self._write(operation)

    async def attach_existing_release_for_contribution(
        self,
        *,
        contribution_id: str,
        expected_row_revision: int,
        actor_user_id: str,
        release_mbid: str,
        release_group_mbid: str,
        artist_mbid: str | None,
        matcher_version: str,
        attempt: IdentificationAttempt,
        evidence: list[IdentificationEvidenceRecord],
        now: float,
    ) -> dict[str, Any]:
        def operation(connection: sqlite3.Connection) -> dict[str, Any]:
            row = self._contribution_row(connection, contribution_id)
            if row is None or not row["album_active"]:
                raise ResourceNotFoundError("Library contribution not found.")
            if row["state"] not in {"ready", "needs_review"}:
                raise ConflictError("This contribution cannot attach a release now.")
            if int(row["row_revision"]) != expected_row_revision:
                raise StaleRevisionError(
                    "The contribution changed before the release could be attached."
                )
            if (
                row["input_revision"] != row["current_input_revision"]
                or row["duplicate_input_revision"] != row["input_revision"]
            ):
                raise StaleRevisionError(
                    "The local album changed before the release could be attached."
                )
            duplicate = json.loads(str(row["duplicate_result_json"] or "{}"))
            if not any(
                candidate.get("release_mbid") == release_mbid
                for candidate in duplicate.get("candidates", [])
            ):
                raise ConflictError(
                    "The release is not in the current duplicate-check result."
                )
            selected = next(
                (
                    record.evidence
                    for record in evidence
                    if record.candidate_key == attempt.selected_candidate_key
                ),
                None,
            )
            if (
                selected is None
                or selected.release_mbid != release_mbid
                or selected.release_group_mbid != release_group_mbid
                or selected.artist_mbid != artist_mbid
                or str(attempt.local_album_id or "") != str(row["local_album_id"])
            ):
                raise ConflictError(
                    "The verified release evidence does not match this contribution."
                )
            connection.execute(
                "INSERT INTO library_identification_attempts "
                "(id, local_album_id, local_track_id, trigger, requested_by_user_id, "
                "input_tag_revision, input_policy_revision, input_file_revision, matcher_version, "
                "state, terminal_reason_code, selected_candidate_key, candidate_count, "
                "degradation_flags_json, started_at, completed_at) "
                "VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)",
                (
                    attempt.id,
                    attempt.local_album_id,
                    attempt.local_track_id,
                    attempt.trigger,
                    attempt.requested_by_user_id,
                    attempt.input_tag_revision,
                    attempt.input_policy_revision,
                    attempt.input_file_revision,
                    attempt.matcher_version,
                    attempt.state,
                    attempt.terminal_reason_code,
                    attempt.selected_candidate_key,
                    attempt.candidate_count,
                    json.dumps(attempt.degradation_flags, separators=(",", ":")),
                    attempt.started_at,
                    attempt.completed_at,
                ),
            )
            self._insert_evidence(connection, evidence)
            album_id = str(row["local_album_id"])
            existing_album_identity = connection.execute(
                "SELECT * FROM local_album_external_identities "
                "WHERE local_album_id = ? AND provider = 'musicbrainz'",
                (album_id,),
            ).fetchone()
            if existing_album_identity is not None and (
                existing_album_identity["release_mbid"] != release_mbid
                or existing_album_identity["release_group_mbid"] != release_group_mbid
            ):
                raise ConflictError(
                    "The local album already has a different MusicBrainz identity."
                )
            if existing_album_identity is None:
                album_revision = self._require_revision_update(
                    connection,
                    table="local_albums",
                    entity_id=album_id,
                    expected_revision=int(row["current_album_row_revision"]),
                    assignments="updated_at = ?",
                    parameters=(now,),
                )
                connection.execute(
                    "INSERT INTO local_album_external_identities "
                    "(local_album_id, provider, release_group_mbid, release_mbid, "
                    "decision_source, matcher_version, attempt_id, selected_by_user_id, selected_at) "
                    "VALUES (?, 'musicbrainz', ?, ?, 'manual', ?, ?, ?, ?)",
                    (
                        album_id,
                        release_group_mbid,
                        release_mbid,
                        matcher_version,
                        attempt.id,
                        actor_user_id,
                        now,
                    ),
                )
            else:
                album_revision = int(row["current_album_row_revision"])
            album = connection.execute(
                "SELECT album_artist_id FROM local_albums WHERE id = ?", (album_id,)
            ).fetchone()
            for track in selected.track_evidence:
                if track.classification != "supported" or not track.recording_mbid:
                    continue
                connection.execute(
                    "INSERT INTO local_track_external_identities "
                    "(local_track_id, provider, recording_mbid, release_mbid, "
                    "decision_source, attempt_id, selected_at) "
                    "VALUES (?, 'musicbrainz', ?, ?, 'manual', ?, ?) "
                    "ON CONFLICT(local_track_id, provider) DO UPDATE SET "
                    "recording_mbid = excluded.recording_mbid, "
                    "release_mbid = excluded.release_mbid, "
                    "decision_source = excluded.decision_source, "
                    "attempt_id = excluded.attempt_id, selected_at = excluded.selected_at, "
                    "row_revision = row_revision + 1",
                    (
                        track.local_track_id,
                        track.recording_mbid,
                        release_mbid,
                        attempt.id,
                        now,
                    ),
                )
            if artist_mbid and album is not None:
                local_artist_id = str(album["album_artist_id"])
                owner = connection.execute(
                    "SELECT local_artist_id FROM local_artist_external_identities "
                    "WHERE provider = 'musicbrainz' AND provider_artist_id = ?",
                    (artist_mbid,),
                ).fetchone()
                existing_artist = connection.execute(
                    "SELECT provider_artist_id FROM local_artist_external_identities "
                    "WHERE local_artist_id = ? AND provider = 'musicbrainz'",
                    (local_artist_id,),
                ).fetchone()
                if existing_artist is not None and (
                    existing_artist["provider_artist_id"] != artist_mbid
                ):
                    raise ConflictError(
                        "The local artist already has a different MusicBrainz identity."
                    )
                if (
                    owner is not None
                    and str(owner["local_artist_id"]) != local_artist_id
                ):
                    left, right = sorted(
                        (local_artist_id, str(owner["local_artist_id"]))
                    )
                    connection.execute(
                        "INSERT OR IGNORE INTO local_artist_merge_candidates "
                        "(id, left_artist_id, right_artist_id, reason_code, "
                        "created_at, updated_at) VALUES (?,?,?,?,?,?)",
                        (
                            f"provider:{left}:{right}",
                            left,
                            right,
                            "SHARED_PROVIDER_IDENTITY",
                            now,
                            now,
                        ),
                    )
                elif existing_artist is None:
                    artist_row = connection.execute(
                        "SELECT row_revision FROM local_artists WHERE id = ?",
                        (local_artist_id,),
                    ).fetchone()
                    if artist_row is None:
                        raise ResourceNotFoundError("Library artist not found.")
                    self._require_revision_update(
                        connection,
                        table="local_artists",
                        entity_id=local_artist_id,
                        expected_revision=int(artist_row["row_revision"]),
                        assignments="updated_at = ?",
                        parameters=(now,),
                    )
                    connection.execute(
                        "INSERT INTO local_artist_external_identities "
                        "(local_artist_id, provider, provider_artist_id, decision_source, "
                        "attempt_id, selected_by_user_id, selected_at) "
                        "VALUES (?, 'musicbrainz', ?, 'manual', ?, ?, ?)",
                        (
                            local_artist_id,
                            artist_mbid,
                            attempt.id,
                            actor_user_id,
                            now,
                        ),
                    )
            connection.execute(
                "UPDATE library_identification_reviews SET state = 'resolved', "
                "attempt_id = ?, updated_at = ?, row_revision = row_revision + 1 "
                "WHERE local_album_id = ? AND state = 'needs_review'",
                (attempt.id, now, album_id),
            )
            connection.execute(
                "UPDATE library_contribution_callback_tokens SET consumed_at = ? "
                "WHERE contribution_id = ? AND consumed_at IS NULL",
                (now, contribution_id),
            )
            updated = connection.execute(
                "UPDATE library_contribution_drafts SET state = 'linked', "
                "album_row_revision = ?, "
                "result_release_mbid = ?, result_source = 'manual', "
                "result_received_at = ?, terminal_at = ?, updated_by_user_id = ?, "
                "updated_at = ?, row_revision = row_revision + 1 WHERE id = ? "
                "AND row_revision = ? AND row_revision < ? RETURNING id",
                (
                    album_revision,
                    release_mbid,
                    now,
                    now,
                    actor_user_id,
                    now,
                    contribution_id,
                    expected_row_revision,
                    MAX_REVISION,
                ),
            ).fetchone()
            if updated is None:
                self._refuse_max_revision(
                    connection,
                    table="library_contribution_drafts",
                    predicate="id = ?",
                    parameters=(contribution_id,),
                )
                raise StaleRevisionError(
                    "The contribution changed before the release could be attached."
                )
            self._bump_catalog(connection)
            self._bump_stream(connection, "identification")
            result = self._contribution_row(connection, contribution_id)
            if result is None:
                raise ResourceNotFoundError("Library contribution not found.")
            result["current_album_row_revision"] = album_revision
            return result

        return await self._write(operation)

    async def consume_library_contribution_callback_token(
        self,
        *,
        token_hash: str,
        release_mbid: str,
        now: float,
    ) -> tuple[str, str | None]:
        def operation(connection: sqlite3.Connection) -> tuple[str, str | None]:
            token = connection.execute(
                "SELECT * FROM library_contribution_callback_tokens "
                "WHERE token_hash = ?",
                (token_hash,),
            ).fetchone()
            if (
                token is None
                or token["consumed_at"] is not None
                or float(token["expires_at"]) < now
            ):
                raise ResourceNotFoundError(
                    "Contribution callback is invalid or expired."
                )
            contribution_id = str(token["contribution_id"])
            contribution = self._contribution_row(connection, contribution_id)
            if contribution is None:
                raise ResourceNotFoundError("Library contribution not found.")
            if contribution["state"] not in {"seeded", "verifying", "stale"}:
                raise ConflictError("This contribution is not waiting for MusicBrainz.")
            current_mbid = contribution["result_release_mbid"]
            if current_mbid and str(current_mbid) != release_mbid:
                raise ConflictError(
                    "This contribution already has a different MusicBrainz result."
                )
            consumed = connection.execute(
                "UPDATE library_contribution_callback_tokens SET consumed_at = ? "
                "WHERE token_hash = ? AND consumed_at IS NULL AND expires_at >= ?",
                (now, token_hash, now),
            )
            if consumed.rowcount != 1:
                raise ResourceNotFoundError(
                    "Contribution callback is invalid or expired."
                )
            connection.execute(
                "UPDATE library_contribution_callback_tokens SET consumed_at = ? "
                "WHERE contribution_id = ? AND token_hash != ? AND consumed_at IS NULL",
                (now, contribution_id, token_hash),
            )
            next_state = (
                "verifying"
                if contribution["album_active"] and contribution["state"] != "stale"
                else "stale"
            )
            updated = connection.execute(
                "UPDATE library_contribution_drafts SET state = ?, "
                "result_release_mbid = ?, result_source = 'callback', "
                "result_received_at = ?, updated_at = ?, row_revision = row_revision + 1 "
                "WHERE id = ? AND row_revision < ?",
                (next_state, release_mbid, now, now, contribution_id, MAX_REVISION),
            )
            if updated.rowcount != 1:
                raise RevisionOverflowError(
                    "The contribution revision cannot be increased."
                )
            if next_state == "stale":
                return contribution_id, None
            active = connection.execute(
                "SELECT id FROM library_contribution_verification_jobs "
                "WHERE contribution_id = ? AND state IN ('queued','running') LIMIT 1",
                (contribution_id,),
            ).fetchone()
            if active is not None:
                return contribution_id, str(active["id"])
            job_id = str(uuid.uuid4())
            connection.execute(
                "INSERT INTO library_contribution_verification_jobs "
                "(id, contribution_id, state, not_before, requested_by_user_id, "
                "created_at, updated_at) VALUES (?, ?, 'queued', ?, ?, ?, ?)",
                (
                    job_id,
                    contribution_id,
                    now,
                    token["requested_by_user_id"],
                    now,
                    now,
                ),
            )
            return contribution_id, job_id

        return await self._write(operation)

    async def record_library_contribution_manual_result(
        self,
        *,
        contribution_id: str,
        release_mbid: str,
        expected_row_revision: int,
        actor_user_id: str,
        replace_existing_result: bool,
        now: float,
    ) -> dict[str, Any]:
        def operation(connection: sqlite3.Connection) -> dict[str, Any]:
            contribution = self._contribution_row(connection, contribution_id)
            if contribution is None:
                raise ResourceNotFoundError("Library contribution not found.")
            if contribution["state"] not in {
                "seeded",
                "verifying",
                "needs_review",
                "stale",
            }:
                raise ConflictError(
                    "This contribution is not waiting for a MusicBrainz result."
                )
            if int(contribution["row_revision"]) != expected_row_revision:
                raise StaleRevisionError(
                    "The contribution changed before the result was recorded."
                )
            current_mbid = contribution["result_release_mbid"]
            if current_mbid and str(current_mbid) != release_mbid:
                if not replace_existing_result or contribution["state"] not in {
                    "needs_review",
                    "stale",
                }:
                    raise ConflictError(
                        "Confirm replacement of the existing MusicBrainz result."
                    )
            connection.execute(
                "UPDATE library_contribution_callback_tokens SET consumed_at = ? "
                "WHERE contribution_id = ? AND consumed_at IS NULL",
                (now, contribution_id),
            )
            connection.execute(
                "UPDATE library_contribution_verification_jobs SET state = 'cancelled', "
                "terminal_at = ?, updated_at = ?, lease_owner = NULL, lease_expires_at = NULL, "
                "heartbeat_at = NULL, row_revision = row_revision + 1, "
                "event_revision = event_revision + 1 WHERE contribution_id = ? "
                "AND state IN ('queued','running') AND row_revision < ?",
                (now, now, contribution_id, MAX_REVISION),
            )
            next_state = (
                "verifying"
                if contribution["album_active"] and contribution["state"] != "stale"
                else "stale"
            )
            updated = connection.execute(
                "UPDATE library_contribution_drafts SET state = ?, "
                "result_release_mbid = ?, result_source = 'manual', result_received_at = ?, "
                "terminal_at = NULL, updated_by_user_id = ?, updated_at = ?, "
                "row_revision = row_revision + 1 WHERE id = ? AND row_revision = ? "
                "AND row_revision < ? RETURNING id",
                (
                    next_state,
                    release_mbid,
                    now,
                    actor_user_id,
                    now,
                    contribution_id,
                    expected_row_revision,
                    MAX_REVISION,
                ),
            ).fetchone()
            if updated is None:
                self._refuse_max_revision(
                    connection,
                    table="library_contribution_drafts",
                    predicate="id = ?",
                    parameters=(contribution_id,),
                )
                raise StaleRevisionError(
                    "The contribution changed before the result was recorded."
                )
            if next_state == "stale":
                result = self._contribution_row(connection, contribution_id)
                if result is None:
                    raise ResourceNotFoundError("Library contribution not found.")
                result["verification_job_id"] = None
                return result
            job_id = str(uuid.uuid4())
            connection.execute(
                "INSERT INTO library_contribution_verification_jobs "
                "(id, contribution_id, state, not_before, requested_by_user_id, "
                "created_at, updated_at) VALUES (?, ?, 'queued', ?, ?, ?, ?)",
                (job_id, contribution_id, now, actor_user_id, now, now),
            )
            result = self._contribution_row(connection, contribution_id)
            if result is None:
                raise ResourceNotFoundError("Library contribution not found.")
            result["verification_job_id"] = job_id
            return result

        return await self._write(operation)

    async def requeue_library_contribution_verification(
        self,
        *,
        contribution_id: str,
        expected_row_revision: int,
        actor_user_id: str,
        now: float,
    ) -> dict[str, Any]:
        def operation(connection: sqlite3.Connection) -> dict[str, Any]:
            contribution = self._contribution_row(connection, contribution_id)
            if contribution is None or not contribution["album_active"]:
                raise ResourceNotFoundError("Library contribution not found.")
            if contribution["state"] not in {"verifying", "needs_review"}:
                raise ConflictError("This contribution cannot be verified now.")
            if not contribution["result_release_mbid"]:
                raise ConflictError("No MusicBrainz result is ready to verify.")
            if int(contribution["row_revision"]) != expected_row_revision:
                raise StaleRevisionError(
                    "The contribution changed before verification was retried."
                )
            active = connection.execute(
                "SELECT id FROM library_contribution_verification_jobs "
                "WHERE contribution_id = ? AND state IN ('queued','running') LIMIT 1",
                (contribution_id,),
            ).fetchone()
            if active is None:
                job_id = str(uuid.uuid4())
                connection.execute(
                    "INSERT INTO library_contribution_verification_jobs "
                    "(id, contribution_id, state, not_before, requested_by_user_id, "
                    "created_at, updated_at) VALUES (?, ?, 'queued', ?, ?, ?, ?)",
                    (job_id, contribution_id, now, actor_user_id, now, now),
                )
            else:
                job_id = str(active["id"])
            updated = connection.execute(
                "UPDATE library_contribution_drafts SET state = 'verifying', "
                "terminal_at = NULL, updated_by_user_id = ?, updated_at = ?, "
                "row_revision = row_revision + 1 WHERE id = ? AND row_revision = ? "
                "AND row_revision < ? RETURNING id",
                (
                    actor_user_id,
                    now,
                    contribution_id,
                    expected_row_revision,
                    MAX_REVISION,
                ),
            ).fetchone()
            if updated is None:
                self._refuse_max_revision(
                    connection,
                    table="library_contribution_drafts",
                    predicate="id = ?",
                    parameters=(contribution_id,),
                )
                raise StaleRevisionError(
                    "The contribution changed before verification was retried."
                )
            result = self._contribution_row(connection, contribution_id)
            if result is None:
                raise ResourceNotFoundError("Library contribution not found.")
            result["verification_job_id"] = job_id
            return result

        return await self._write(operation)

    async def enqueue_library_contribution_verification(
        self,
        *,
        contribution_id: str,
        requested_by_user_id: str | None,
        not_before: float,
        now: float,
    ) -> str:
        def operation(connection: sqlite3.Connection) -> str:
            contribution = self._contribution_row(connection, contribution_id)
            if contribution is None or not contribution["album_active"]:
                raise ResourceNotFoundError("Library contribution not found.")
            if not contribution["result_release_mbid"]:
                raise ConflictError("No MusicBrainz result is ready to verify.")
            existing = connection.execute(
                "SELECT id FROM library_contribution_verification_jobs "
                "WHERE contribution_id = ? AND state IN ('queued','running') LIMIT 1",
                (contribution_id,),
            ).fetchone()
            if existing is not None:
                return str(existing["id"])
            job_id = str(uuid.uuid4())
            connection.execute(
                "INSERT INTO library_contribution_verification_jobs "
                "(id, contribution_id, state, not_before, requested_by_user_id, "
                "created_at, updated_at) VALUES (?, ?, 'queued', ?, ?, ?, ?)",
                (
                    job_id,
                    contribution_id,
                    not_before,
                    requested_by_user_id,
                    now,
                    now,
                ),
            )
            return job_id

        return await self._write(operation)

    async def claim_library_contribution_verification(
        self,
        *,
        worker_id: str,
        now: float,
        lease_seconds: float,
    ) -> dict[str, Any] | None:
        def operation(connection: sqlite3.Connection) -> dict[str, Any] | None:
            row = connection.execute(
                "SELECT * FROM library_contribution_verification_jobs "
                "WHERE state = 'queued' AND not_before <= ? "
                "ORDER BY not_before, created_at LIMIT 1",
                (now,),
            ).fetchone()
            if row is None:
                return None
            claimed = connection.execute(
                "UPDATE library_contribution_verification_jobs SET state = 'running', "
                "attempt_count = attempt_count + 1, lease_owner = ?, lease_expires_at = ?, "
                "heartbeat_at = ?, updated_at = ?, row_revision = row_revision + 1, "
                "event_revision = event_revision + 1 WHERE id = ? AND state = 'queued' "
                "AND row_revision = ? AND row_revision < ? RETURNING *",
                (
                    worker_id,
                    now + lease_seconds,
                    now,
                    now,
                    row["id"],
                    row["row_revision"],
                    MAX_REVISION,
                ),
            ).fetchone()
            return dict(claimed) if claimed is not None else None

        return await self._background_write(operation)

    async def heartbeat_library_contribution_verification(
        self,
        *,
        job_id: str,
        worker_id: str,
        expected_row_revision: int,
        now: float,
        lease_seconds: float,
    ) -> int:
        def operation(connection: sqlite3.Connection) -> int:
            row = connection.execute(
                "UPDATE library_contribution_verification_jobs SET heartbeat_at = ?, "
                "lease_expires_at = ?, updated_at = ?, row_revision = row_revision + 1, "
                "event_revision = event_revision + 1 WHERE id = ? AND state = 'running' "
                "AND lease_owner = ? AND row_revision = ? AND row_revision < ? "
                "RETURNING row_revision",
                (
                    now,
                    now + lease_seconds,
                    now,
                    job_id,
                    worker_id,
                    expected_row_revision,
                    MAX_REVISION,
                ),
            ).fetchone()
            if row is None:
                raise StaleRevisionError("The contribution verification lease changed.")
            return int(row["row_revision"])

        return await self._background_write(operation)

    async def retry_library_contribution_verification(
        self,
        *,
        job_id: str,
        worker_id: str,
        expected_row_revision: int,
        failure_code: str,
        not_before: float,
        now: float,
    ) -> int:
        def operation(connection: sqlite3.Connection) -> int:
            row = connection.execute(
                "UPDATE library_contribution_verification_jobs SET state = 'queued', "
                "last_failure_code = ?, not_before = ?, lease_owner = NULL, "
                "lease_expires_at = NULL, heartbeat_at = NULL, updated_at = ?, "
                "row_revision = row_revision + 1, event_revision = event_revision + 1 "
                "WHERE id = ? AND state = 'running' AND lease_owner = ? "
                "AND row_revision = ? AND row_revision < ? RETURNING row_revision",
                (
                    failure_code,
                    not_before,
                    now,
                    job_id,
                    worker_id,
                    expected_row_revision,
                    MAX_REVISION,
                ),
            ).fetchone()
            if row is None:
                raise StaleRevisionError(
                    "The contribution verification job changed before retry."
                )
            return int(row["row_revision"])

        return await self._background_write(operation)

    async def complete_library_contribution_verification_job(
        self,
        *,
        job_id: str,
        worker_id: str,
        expected_row_revision: int,
        state: str,
        failure_code: str | None,
        now: float,
    ) -> int:
        if state not in {"succeeded", "needs_review", "failed", "cancelled"}:
            raise ValidationError("Invalid contribution verification result state.")

        def operation(connection: sqlite3.Connection) -> int:
            row = connection.execute(
                "UPDATE library_contribution_verification_jobs SET state = ?, "
                "last_failure_code = ?, lease_owner = NULL, lease_expires_at = NULL, "
                "terminal_at = ?, updated_at = ?, row_revision = row_revision + 1, "
                "event_revision = event_revision + 1 WHERE id = ? AND state = 'running' "
                "AND lease_owner = ? AND row_revision = ? AND row_revision < ? "
                "RETURNING row_revision",
                (
                    state,
                    failure_code,
                    now,
                    now,
                    job_id,
                    worker_id,
                    expected_row_revision,
                    MAX_REVISION,
                ),
            ).fetchone()
            if row is None:
                raise StaleRevisionError(
                    "The contribution verification job changed before completion."
                )
            return int(row["row_revision"])

        return await self._background_write(operation)

    async def finish_library_contribution_verification(
        self,
        *,
        job_id: str,
        worker_id: str,
        expected_job_revision: int,
        expected_contribution_revision: int,
        expected_album_revision: int,
        attempt: IdentificationAttempt,
        evidence: list[IdentificationEvidenceRecord],
        outcome: str,
        failure_code: str | None,
        now: float,
    ) -> str:
        """Persist verification evidence and apply compatible identities atomically."""

        def operation(connection: sqlite3.Connection) -> str:
            job = connection.execute(
                "SELECT * FROM library_contribution_verification_jobs WHERE id = ? "
                "AND state = 'running' AND lease_owner = ? AND row_revision = ?",
                (job_id, worker_id, expected_job_revision),
            ).fetchone()
            if job is None:
                raise StaleRevisionError(
                    "The contribution verification job changed before completion."
                )
            contribution = self._contribution_row(
                connection, str(job["contribution_id"])
            )
            if contribution is None:
                raise ResourceNotFoundError("Library contribution not found.")
            if not contribution["album_active"] or (
                contribution["input_revision"] != contribution["current_input_revision"]
                or int(contribution["album_row_revision"])
                != int(contribution["current_album_row_revision"])
            ):
                connection.execute(
                    "UPDATE library_contribution_drafts SET state = 'stale', terminal_at = ?, "
                    "updated_at = ?, row_revision = row_revision + 1 WHERE id = ? "
                    "AND row_revision < ?",
                    (now, now, contribution["id"], MAX_REVISION),
                )
                connection.execute(
                    "UPDATE library_contribution_verification_jobs SET state = 'cancelled', "
                    "last_failure_code = 'LOCAL_INPUT_CHANGED', terminal_at = ?, updated_at = ?, "
                    "lease_owner = NULL, lease_expires_at = NULL, heartbeat_at = NULL, "
                    "row_revision = row_revision + 1, event_revision = event_revision + 1 "
                    "WHERE id = ? AND row_revision < ?",
                    (now, now, job_id, MAX_REVISION),
                )
                return "stale"
            if int(contribution["row_revision"]) != expected_contribution_revision:
                raise StaleRevisionError(
                    "The contribution changed before verification completed."
                )
            if contribution["state"] != "verifying":
                raise ConflictError("This contribution is not being verified.")
            if str(contribution["local_album_id"]) != str(attempt.local_album_id or ""):
                raise ConflictError("The verification subject does not match.")
            album = connection.execute(
                "SELECT row_revision, album_artist_id FROM local_albums WHERE id = ?",
                (attempt.local_album_id,),
            ).fetchone()
            if album is None:
                raise ResourceNotFoundError("Library album not found.")
            if int(album["row_revision"]) != expected_album_revision:
                raise StaleRevisionError(
                    "The album changed before verification completed."
                )

            selected = next(
                (
                    record.evidence
                    for record in evidence
                    if record.candidate_key == attempt.selected_candidate_key
                ),
                None,
            )
            final_outcome = outcome
            final_failure = failure_code
            existing_album_identity = connection.execute(
                "SELECT * FROM local_album_external_identities "
                "WHERE local_album_id = ? AND provider = 'musicbrainz'",
                (attempt.local_album_id,),
            ).fetchone()
            if outcome == "identified" and selected is not None:
                result_mbid = str(contribution["result_release_mbid"] or "")
                if selected.release_mbid != result_mbid:
                    final_outcome = "needs_review"
                    final_failure = "RETURNED_RELEASE_MISMATCH"
                elif existing_album_identity is not None and (
                    existing_album_identity["release_mbid"] != selected.release_mbid
                    or existing_album_identity["release_group_mbid"]
                    != selected.release_group_mbid
                ):
                    final_outcome = "needs_review"
                    final_failure = "EXISTING_IDENTITY_CONFLICT"
                elif selected.artist_mbid:
                    existing_artist = connection.execute(
                        "SELECT provider_artist_id FROM local_artist_external_identities "
                        "WHERE local_artist_id = ? AND provider = 'musicbrainz'",
                        (album["album_artist_id"],),
                    ).fetchone()
                    if existing_artist is not None and (
                        existing_artist["provider_artist_id"] != selected.artist_mbid
                    ):
                        final_outcome = "needs_review"
                        final_failure = "EXISTING_ARTIST_IDENTITY_CONFLICT"
            elif outcome == "identified":
                final_outcome = "needs_review"
                final_failure = "VERIFICATION_EVIDENCE_MISSING"

            selected_key = (
                attempt.selected_candidate_key
                if final_outcome == "identified"
                else None
            )
            connection.execute(
                "INSERT INTO library_identification_attempts "
                "(id, local_album_id, local_track_id, trigger, requested_by_user_id, "
                "input_tag_revision, input_policy_revision, input_file_revision, matcher_version, "
                "state, terminal_reason_code, selected_candidate_key, candidate_count, "
                "degradation_flags_json, started_at, completed_at) "
                "VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)",
                (
                    attempt.id,
                    attempt.local_album_id,
                    attempt.local_track_id,
                    attempt.trigger,
                    attempt.requested_by_user_id,
                    attempt.input_tag_revision,
                    attempt.input_policy_revision,
                    attempt.input_file_revision,
                    attempt.matcher_version,
                    "identified" if final_outcome == "identified" else "needs_review",
                    final_failure or attempt.terminal_reason_code,
                    selected_key,
                    attempt.candidate_count,
                    json.dumps(attempt.degradation_flags, separators=(",", ":")),
                    attempt.started_at,
                    attempt.completed_at,
                ),
            )
            self._insert_evidence(connection, evidence)

            if final_outcome == "identified" and selected is not None:
                album_revision = int(album["row_revision"])
                if existing_album_identity is None:
                    updated_album = connection.execute(
                        "UPDATE local_albums SET updated_at = ?, row_revision = row_revision + 1 "
                        "WHERE id = ? AND row_revision = ? AND row_revision < ? "
                        "RETURNING row_revision",
                        (
                            now,
                            attempt.local_album_id,
                            expected_album_revision,
                            MAX_REVISION,
                        ),
                    ).fetchone()
                    if updated_album is None:
                        raise StaleRevisionError(
                            "The album changed before verification completed."
                        )
                    album_revision = int(updated_album["row_revision"])
                    connection.execute(
                        "INSERT INTO local_album_external_identities "
                        "(local_album_id, provider, release_group_mbid, release_mbid, "
                        "decision_source, matcher_version, attempt_id, selected_by_user_id, "
                        "selected_at) VALUES (?, 'musicbrainz', ?, ?, 'manual', "
                        "?, ?, ?, ?)",
                        (
                            attempt.local_album_id,
                            selected.release_group_mbid,
                            selected.release_mbid,
                            attempt.matcher_version,
                            attempt.id,
                            attempt.requested_by_user_id,
                            now,
                        ),
                    )
                for track in selected.track_evidence:
                    if track.classification != "supported" or not track.recording_mbid:
                        continue
                    connection.execute(
                        "INSERT INTO local_track_external_identities "
                        "(local_track_id, provider, recording_mbid, release_mbid, "
                        "decision_source, attempt_id, selected_at) "
                        "VALUES (?, 'musicbrainz', ?, ?, 'manual', ?, ?) "
                        "ON CONFLICT(local_track_id, provider) DO UPDATE SET "
                        "recording_mbid = excluded.recording_mbid, "
                        "release_mbid = excluded.release_mbid, "
                        "decision_source = excluded.decision_source, "
                        "attempt_id = excluded.attempt_id, selected_at = excluded.selected_at, "
                        "row_revision = row_revision + 1",
                        (
                            track.local_track_id,
                            track.recording_mbid,
                            selected.release_mbid,
                            attempt.id,
                            now,
                        ),
                    )
                if selected.artist_mbid:
                    artist_id = str(album["album_artist_id"])
                    owner = connection.execute(
                        "SELECT local_artist_id FROM local_artist_external_identities "
                        "WHERE provider = 'musicbrainz' AND provider_artist_id = ?",
                        (selected.artist_mbid,),
                    ).fetchone()
                    if owner is None or str(owner["local_artist_id"]) == artist_id:
                        existing_artist = connection.execute(
                            "SELECT provider_artist_id FROM local_artist_external_identities "
                            "WHERE local_artist_id = ? AND provider = 'musicbrainz'",
                            (artist_id,),
                        ).fetchone()
                        if existing_artist is None:
                            artist = connection.execute(
                                "UPDATE local_artists SET updated_at = ?, "
                                "row_revision = row_revision + 1 WHERE id = ? "
                                "AND row_revision < ? RETURNING row_revision",
                                (now, artist_id, MAX_REVISION),
                            ).fetchone()
                            if artist is None:
                                raise ResourceNotFoundError("Library artist not found.")
                            connection.execute(
                                "INSERT INTO local_artist_external_identities "
                                "(local_artist_id, provider, provider_artist_id, "
                                "decision_source, attempt_id, selected_by_user_id, selected_at) "
                                "VALUES (?, 'musicbrainz', ?, 'manual', ?, ?, ?)",
                                (
                                    artist_id,
                                    selected.artist_mbid,
                                    attempt.id,
                                    attempt.requested_by_user_id,
                                    now,
                                ),
                            )
                    else:
                        left, right = sorted((artist_id, str(owner["local_artist_id"])))
                        connection.execute(
                            "INSERT OR IGNORE INTO local_artist_merge_candidates "
                            "(id, left_artist_id, right_artist_id, reason_code, "
                            "created_at, updated_at) VALUES (?,?,?,?,?,?)",
                            (
                                f"provider:{left}:{right}",
                                left,
                                right,
                                "SHARED_PROVIDER_IDENTITY",
                                now,
                                now,
                            ),
                        )
                connection.execute(
                    "UPDATE library_identification_reviews SET state = 'resolved', "
                    "attempt_id = ?, updated_at = ?, row_revision = row_revision + 1 "
                    "WHERE local_album_id = ? AND state = 'needs_review'",
                    (attempt.id, now, attempt.local_album_id),
                )
                contribution_state = "linked"
                job_state = "succeeded"
                terminal_at: float | None = now
                self._bump_catalog(connection)
            else:
                album_revision = int(album["row_revision"])
                active_review = connection.execute(
                    "SELECT id FROM library_identification_reviews "
                    "WHERE local_album_id = ? AND input_revision = ? "
                    "AND state != 'resolved'",
                    (attempt.local_album_id, contribution["input_revision"]),
                ).fetchone()
                if active_review is None:
                    connection.execute(
                        "INSERT INTO library_identification_reviews "
                        "(id, local_album_id, state, reason_code, attempt_id, "
                        "input_revision, created_at, updated_at) "
                        "VALUES (?, ?, 'needs_review', ?, ?, ?, ?, ?)",
                        (
                            str(uuid.uuid4()),
                            attempt.local_album_id,
                            final_failure,
                            attempt.id,
                            contribution["input_revision"],
                            now,
                            now,
                        ),
                    )
                else:
                    connection.execute(
                        "UPDATE library_identification_reviews SET reason_code = ?, "
                        "attempt_id = ?, updated_at = ?, row_revision = row_revision + 1 "
                        "WHERE id = ?",
                        (final_failure, attempt.id, now, active_review["id"]),
                    )
                contribution_state = "needs_review"
                job_state = "needs_review"
                terminal_at = None

            updated_contribution = connection.execute(
                "UPDATE library_contribution_drafts SET state = ?, album_row_revision = ?, "
                "seed_snapshot_json = NULL, terminal_at = ?, updated_at = ?, "
                "row_revision = row_revision + 1 WHERE id = ? AND row_revision = ? "
                "AND row_revision < ?",
                (
                    contribution_state,
                    album_revision,
                    terminal_at,
                    now,
                    contribution["id"],
                    expected_contribution_revision,
                    MAX_REVISION,
                ),
            )
            if updated_contribution.rowcount != 1:
                raise StaleRevisionError(
                    "The contribution changed before verification completed."
                )
            updated_job = connection.execute(
                "UPDATE library_contribution_verification_jobs SET state = ?, "
                "last_failure_code = ?, terminal_at = ?, updated_at = ?, "
                "lease_owner = NULL, lease_expires_at = NULL, heartbeat_at = NULL, "
                "row_revision = row_revision + 1, event_revision = event_revision + 1 "
                "WHERE id = ? AND state = 'running' AND lease_owner = ? "
                "AND row_revision = ? AND row_revision < ?",
                (
                    job_state,
                    final_failure,
                    now,
                    now,
                    job_id,
                    worker_id,
                    expected_job_revision,
                    MAX_REVISION,
                ),
            )
            if updated_job.rowcount != 1:
                raise StaleRevisionError(
                    "The contribution verification job changed before completion."
                )
            self._bump_stream(connection, "identification")
            return contribution_state

        return await self._write(operation)

    async def recover_library_contribution_verification_leases(
        self, *, now: float
    ) -> int:
        def operation(connection: sqlite3.Connection) -> int:
            result = connection.execute(
                "UPDATE library_contribution_verification_jobs SET state = 'queued', "
                "lease_owner = NULL, lease_expires_at = NULL, heartbeat_at = NULL, "
                "not_before = ?, updated_at = ?, row_revision = row_revision + 1, "
                "event_revision = event_revision + 1 WHERE state = 'running' "
                "AND lease_expires_at < ? AND row_revision < ?",
                (now, now, now, MAX_REVISION),
            )
            return result.rowcount

        return await self._background_write(operation)

    async def list_library_contributions_for_provider_purge(
        self, *, now: float, limit: int = 200
    ) -> list[dict[str, Any]]:
        bounded = max(1, min(limit, 1_000))

        def operation(connection: sqlite3.Connection) -> list[dict[str, Any]]:
            rows = connection.execute(
                "SELECT id FROM library_contribution_drafts "
                "WHERE provider_snapshot_expires_at IS NOT NULL "
                "AND (provider_snapshot_expires_at <= ? "
                "OR state IN ('linked','cancelled','stale')) "
                "ORDER BY provider_snapshot_expires_at, id LIMIT ?",
                (now, bounded),
            ).fetchall()
            result: list[dict[str, Any]] = []
            for item in rows:
                contribution = self._contribution_row(connection, str(item["id"]))
                if contribution is not None:
                    result.append(contribution)
            return result

        return await self._read(operation)

    async def purge_library_contribution_provider_data(
        self,
        *,
        contribution_id: str,
        expected_row_revision: int,
        resolved_draft_json: str,
        source_selection_json: str,
        now: float,
    ) -> bool:
        def operation(connection: sqlite3.Connection) -> bool:
            updated = connection.execute(
                "UPDATE library_contribution_drafts SET resolved_draft_json = ?, "
                "source_selection_json = ?, provider_snapshot_expires_at = NULL, "
                "duplicate_result_json = NULL, duplicate_checked_at = NULL, "
                "duplicate_input_revision = NULL, seed_snapshot_json = NULL, "
                "updated_at = ?, row_revision = row_revision + 1 "
                "WHERE id = ? AND row_revision = ? "
                "AND provider_snapshot_expires_at IS NOT NULL AND row_revision < ?",
                (
                    resolved_draft_json,
                    source_selection_json,
                    now,
                    contribution_id,
                    expected_row_revision,
                    MAX_REVISION,
                ),
            )
            return updated.rowcount == 1

        return await self._background_write(operation)

    async def clean_library_contribution_records(
        self, *, now: float, limit: int = 200
    ) -> dict[str, int]:
        bounded = max(1, min(limit, 1_000))

        def operation(connection: sqlite3.Connection) -> dict[str, int]:
            expired_tokens = connection.execute(
                "DELETE FROM library_contribution_callback_tokens WHERE token_hash IN ("
                "SELECT token_hash FROM library_contribution_callback_tokens WHERE "
                "(consumed_at IS NOT NULL AND consumed_at < ?) OR "
                "(consumed_at IS NULL AND expires_at < ?) LIMIT ?)",
                (now - 30 * 86_400, now - 7 * 86_400, bounded),
            ).rowcount
            seed_snapshots = connection.execute(
                "UPDATE library_contribution_drafts SET seed_snapshot_json = NULL, "
                "updated_at = ?, row_revision = row_revision + 1 WHERE id IN ("
                "SELECT d.id FROM library_contribution_drafts d WHERE "
                "d.seed_snapshot_json IS NOT NULL AND ("
                "d.state IN ('linked','cancelled','stale') OR (d.seeded_at IS NOT NULL "
                "AND NOT EXISTS (SELECT 1 FROM library_contribution_callback_tokens token "
                "WHERE token.contribution_id = d.id AND token.consumed_at IS NULL "
                "AND token.expires_at >= ?))) LIMIT ?) AND row_revision < ?",
                (now, now, bounded, MAX_REVISION),
            ).rowcount
            old_drafts = connection.execute(
                "DELETE FROM library_contribution_drafts WHERE id IN ("
                "SELECT id FROM library_contribution_drafts WHERE state IN ('cancelled','stale') "
                "AND result_release_mbid IS NULL AND terminal_at < ? LIMIT ?)",
                (now - 90 * 86_400, bounded),
            ).rowcount
            old_jobs = connection.execute(
                "DELETE FROM library_contribution_verification_jobs WHERE id IN ("
                "SELECT j.id FROM library_contribution_verification_jobs j "
                "JOIN library_contribution_drafts d ON d.id = j.contribution_id "
                "WHERE j.state IN ('succeeded','needs_review','failed','cancelled') "
                "AND j.terminal_at < ? AND d.state IN ('linked','cancelled','stale') LIMIT ?)",
                (now - 90 * 86_400, bounded),
            ).rowcount
            return {
                "callback_tokens": expired_tokens,
                "seed_snapshots": seed_snapshots,
                "drafts": old_drafts,
                "verification_jobs": old_jobs,
            }

        return await self._write(operation)

    async def explain_query_plan(
        self, sql: str, parameters: tuple[Any, ...] = ()
    ) -> list[str]:
        def operation(connection: sqlite3.Connection) -> list[str]:
            rows = connection.execute(
                f"EXPLAIN QUERY PLAN {sql}", parameters
            ).fetchall()
            return [str(item["detail"]) for item in rows]

        return await self._read(operation)

    async def table_names(self) -> set[str]:
        def operation(connection: sqlite3.Connection) -> set[str]:
            return {
                str(item["name"])
                for item in connection.execute(
                    "SELECT name FROM sqlite_master WHERE type = 'table'"
                ).fetchall()
            }

        return await self._read(operation)

    async def row_count(self, table: str) -> int:
        allowed = await self.table_names()
        if table not in allowed:
            raise ValueError(f"Unknown target table: {table}")

        def operation(connection: sqlite3.Connection) -> int:
            return int(
                connection.execute(f"SELECT COUNT(*) FROM {table}").fetchone()[0]
            )

        return await self._read(operation)
