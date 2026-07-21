"""Deterministic, zero-network import from a coherent legacy database copy."""

from __future__ import annotations

import asyncio
import hashlib
import time
import unicodedata
from collections import Counter, defaultdict
from pathlib import Path
from typing import Protocol
from uuid import UUID, uuid5
from uuid import UUID as UUIDType

import msgspec

from core.exceptions import StaleRevisionError, ValidationError
from infrastructure.persistence.native_library_store import (
    VARIOUS_ARTISTS_ID,
    NativeLibraryStore,
)
from models.library_migration import (
    LegacyCatalogImportBundle,
    LegacyCatalogImportPlan,
    MigrationDryRunReport,
    MigrationReferenceCount,
    MigrationReview,
    MigrationTombstone,
)
from models.library_work import MigrationProvenance
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
from services.native.local_album_grouper import (
    grouping_directory,
    normalize_group_value,
)
from services.native.library_policy_resolver import LibraryPolicyResolver
from services.native.file_revision import exact_stat_revision

MIGRATION_NAMESPACE = UUIDType("3eb2364c-7086-4cb9-9360-6cf704259a40")
REFERENCE_KINDS = (
    "root",
    "library_file",
    "review_row",
    "favorite",
    "history",
    "playlist_track",
    "album_release_pin",
    "compat_bookmark",
    "compat_play_queue",
    "compat_play_queue_item",
    "manual_decision",
    "subsonic_id",
    "jellyfin_id_map",
    "native_album_alias",
    "native_artist_alias",
    "artwork_reference",
)
LOCAL_PLAYLIST_SOURCES = {"local", "droppedneedle-local", "howler"}
MAX_EMBEDDED_ART_READS_PER_REHEARSAL = 500


class EmbeddedCoverReaderProtocol(Protocol):
    def read_cover_art(self, path: Path) -> bytes | None: ...


def _stable_id(kind: str, value: str) -> str:
    return str(uuid5(MIGRATION_NAMESPACE, f"{kind}:{value}"))


def _fold(value: str | None) -> str:
    decomposed = unicodedata.normalize("NFKD", (value or "").strip())
    return "".join(
        character for character in decomposed if not unicodedata.combining(character)
    ).casefold()


def _hash(value: object) -> str:
    return hashlib.sha256(msgspec.json.encode(value)).hexdigest()


def _valid_mbid(value: object) -> bool:
    if not isinstance(value, str) or not value:
        return False
    try:
        UUID(value)
    except ValueError:
        return False
    return True


def _invalid_release_group_key(value: object) -> str:
    if not isinstance(value, str) or not value.strip() or _valid_mbid(value):
        return ""
    return value.casefold()


def _local_only_grouping_values(
    root_id: str,
    relative_path: str,
    album_title: object,
    album_artist_name: object,
    is_compilation: object,
    source_id: object,
) -> tuple[str, str, str, str]:
    directory = grouping_directory(relative_path)
    album_key = normalize_group_value(str(album_title or ""))
    artist_key = (
        ""
        if bool(is_compilation)
        else normalize_group_value(str(album_artist_name or ""))
    )
    group_key = _hash(
        [
            root_id,
            directory,
            album_key or f"untagged:{source_id}",
            artist_key if album_key else "",
        ]
    )
    return group_key, directory, album_key, artist_key


def _group_local_only_rows(
    rows: list[tuple[dict[str, object], str, str]],
) -> dict[str, list[dict[str, object]]]:
    groups: dict[str, list[dict[str, object]]] = defaultdict(list)
    tagged_by_directory: dict[tuple[str, str], set[str]] = defaultdict(set)
    untagged_by_directory: dict[
        tuple[str, str], list[tuple[str, dict[str, object]]]
    ] = defaultdict(list)

    for row, root_id, relative_path in rows:
        group_key, directory, album_key, _artist_key = _local_only_grouping_values(
            root_id,
            relative_path,
            row.get("album_title"),
            row.get("album_artist_name"),
            row.get("is_compilation"),
            row.get("id"),
        )
        directory_key = (root_id, directory)
        if album_key:
            groups[group_key].append(row)
            tagged_by_directory[directory_key].add(group_key)
        else:
            untagged_by_directory[directory_key].append((group_key, row))

    for directory_key, untagged in untagged_by_directory.items():
        candidates = tagged_by_directory.get(directory_key, set())
        target_key = next(iter(candidates)) if len(candidates) == 1 else None
        target_numbers = (
            {
                _as_int(row.get("track_number"))
                for row in groups[target_key]
                if _as_int(row.get("track_number")) > 0
            }
            if target_key is not None
            else set()
        )
        can_join = target_key is not None and all(
            _as_int(row.get("track_number")) > 0
            and _as_int(row.get("track_number")) not in target_numbers
            for _group_key, row in untagged
        )
        if can_join and target_key is not None:
            groups[target_key].extend(row for _group_key, row in untagged)
            continue
        for group_key, row in untagged:
            groups[group_key].append(row)
    return groups


def _as_int(value: object, default: int = 0) -> int:
    try:
        return int(value) if value is not None else default
    except (TypeError, ValueError):
        return default


def _as_float(value: object, default: float = 0.0) -> float:
    try:
        return float(value) if value is not None else default
    except (TypeError, ValueError):
        return default


class LegacyCatalogImporter:
    """Prepare and apply an import without reading tags, fingerprinting, or networking."""

    def __init__(
        self,
        store: NativeLibraryStore,
        policy_resolver: LibraryPolicyResolver,
        cover_reader: EmbeddedCoverReaderProtocol,
        *,
        embedded_art_read_limit: int = MAX_EMBEDDED_ART_READS_PER_REHEARSAL,
    ) -> None:
        self._store = store
        self._resolver = policy_resolver
        self._cover_reader = cover_reader
        self._embedded_art_read_limit = max(0, embedded_art_read_limit)

    async def prepare(
        self, migration_id: str, *, now: float | None = None
    ) -> tuple[LegacyCatalogImportPlan, MigrationDryRunReport]:
        prepared_at = time.time() if now is None else now
        snapshot = await self._store.get_legacy_migration_snapshot()
        source_revision = _hash(snapshot)
        plan = self._build_plan(snapshot, source_revision, prepared_at)
        await self._discover_embedded_art(plan, prepared_at)
        report = self._report(migration_id, plan)
        await self._store.save_migration_dry_run(
            migration_id,
            source_revision=source_revision,
            root_revision=plan.root_revision,
            report_json=msgspec.json.encode(report).decode(),
            created_at=prepared_at,
        )
        return plan, report

    async def apply(
        self,
        migration_id: str,
        *,
        expected_source_revision: str,
        now: float | None = None,
    ) -> MigrationDryRunReport:
        applied_at = time.time() if now is None else now
        snapshot = await self._store.get_legacy_migration_snapshot()
        current_revision = _hash(snapshot)
        current_root_revision = self._resolver.policy_revision
        await self._store.require_migration_input(
            migration_id,
            source_revision=current_revision,
            root_revision=current_root_revision,
        )
        if current_revision != expected_source_revision:
            raise StaleRevisionError(
                "The copied legacy database changed after the dry run."
            )
        plan = self._build_plan(snapshot, current_revision, applied_at)
        await self._discover_embedded_art(plan, applied_at)
        if plan.blockers:
            raise ValidationError("The legacy import has unresolved active references.")
        for bundle in plan.bundles:
            await self._store.apply_legacy_catalog_bundle(
                bundle,
                migration_run_id=migration_id,
                source_revision=current_revision,
            )
        await self._store.apply_reference_provenance_batch(
            [],
            migration_run_id=migration_id,
            source_revision=current_revision,
        )
        for start in range(0, len(plan.tombstones), 500):
            await self._store.apply_reference_provenance_batch(
                [],
                migration_run_id=migration_id,
                source_revision=current_revision,
                tombstones=plan.tombstones[start : start + 500],
            )
        for start in range(0, len(plan.reference_provenance), 500):
            await self._store.apply_reference_provenance_batch(
                plan.reference_provenance[start : start + 500],
                migration_run_id=migration_id,
                source_revision=current_revision,
            )
        invariant_counts = await self._store.validate_migrated_catalog()
        if any(invariant_counts.values()):
            raise ValidationError("The imported catalog failed its target invariants.")
        expected_counts = Counter(
            provenance.source_kind
            for bundle in plan.bundles
            for provenance in bundle.provenance
        )
        expected_counts.update(
            provenance.source_kind for provenance in plan.reference_provenance
        )
        actual_counts = await self._store.get_migration_provenance_counts(migration_id)
        if any(
            actual_counts.get(kind, 0) != expected_counts.get(kind, 0)
            for kind in REFERENCE_KINDS
        ):
            raise ValidationError(
                "The imported catalog reference counts do not match the migration plan."
            )
        report = self._report(migration_id, plan, state="applied")
        await self._store.finish_migration(
            migration_id,
            source_revision=current_revision,
            report_json=msgspec.json.encode(report).decode(),
            completed_at=applied_at,
        )
        return report

    def _build_plan(
        self,
        snapshot: dict[str, list[dict[str, object]]],
        source_revision: str,
        prepared_at: float,
    ) -> LegacyCatalogImportPlan:
        plan = LegacyCatalogImportPlan(
            source_revision=source_revision,
            root_revision=self._resolver.policy_revision,
        )
        counts: dict[tuple[str, str | None], MigrationReferenceCount] = {
            (kind, None): MigrationReferenceCount(kind=kind) for kind in REFERENCE_KINDS
        }
        album_map: dict[str, str] = {}
        artist_map: dict[str, str] = {}
        track_map: dict[str, str] = {}
        recording_map: dict[str, list[str]] = defaultdict(list)
        path_map: dict[str, tuple[str, str]] = {}
        bundles_by_album: dict[str, LegacyCatalogImportBundle] = {}
        emitted_artists: set[str] = set()
        emitted_artist_aliases: set[str] = set()

        self._count_roots(plan, counts, prepared_at)
        active_files = [
            row for row in snapshot["library_files"] if row.get("deleted_at") is None
        ]
        by_release_group: dict[str, list[dict[str, object]]] = defaultdict(list)
        local_only_rows: list[tuple[dict[str, object], str, str]] = []
        for row in active_files:
            resolved = self._resolver.resolve(str(row.get("file_path") or ""))
            if resolved is None:
                plan.blockers.append(
                    f"Library file {row.get('id')} is outside every typed root."
                )
                self._increment(counts, "library_file", mapped=False)
                continue
            release_group = row.get("release_group_mbid")
            if not _valid_mbid(release_group):
                local_only_rows.append(
                    (row, resolved.root_id, resolved.relative_path)
                )
                continue
            by_release_group[str(release_group).casefold()].append(row)

        for release_group, rows in sorted(by_release_group.items()):
            bundle = self._identified_bundle(
                release_group,
                rows,
                snapshot,
                source_revision,
                prepared_at,
                plan,
                emitted_artists,
                emitted_artist_aliases,
                album_map,
                artist_map,
                track_map,
                recording_map,
                path_map,
                counts,
            )
            if bundle is not None:
                plan.bundles.append(bundle)
                bundles_by_album[bundle.membership.album.id] = bundle

        local_only_groups = _group_local_only_rows(local_only_rows)
        local_only_bundles: dict[str, LegacyCatalogImportBundle] = {}
        invalid_release_groups: dict[str, set[str]] = defaultdict(set)
        for group_key, rows in sorted(local_only_groups.items()):
            bundle = self._local_only_library_bundle(
                group_key,
                rows,
                snapshot,
                source_revision,
                prepared_at,
                plan,
                emitted_artists,
                emitted_artist_aliases,
                artist_map,
                track_map,
                recording_map,
                path_map,
                counts,
            )
            if bundle is not None:
                plan.bundles.append(bundle)
                bundles_by_album[bundle.membership.album.id] = bundle
                local_only_bundles[group_key] = bundle
                for row in rows:
                    invalid_key = _invalid_release_group_key(
                        row.get("release_group_mbid")
                    )
                    if invalid_key:
                        invalid_release_groups[invalid_key].add(group_key)

        for alias, group_keys in sorted(invalid_release_groups.items()):
            if len(group_keys) != 1:
                continue
            bundle = local_only_bundles[next(iter(group_keys))]
            bundle.album_aliases.append(
                LocalAlbumAlias(
                    alias=alias,
                    local_album_id=bundle.membership.album.id,
                    kind="compat_migration",
                    created_at=prepared_at,
                )
            )
            album_map[alias] = bundle.membership.album.id

        self._add_review_rows(
            snapshot,
            source_revision,
            prepared_at,
            plan,
            emitted_artists,
            emitted_artist_aliases,
            album_map,
            artist_map,
            track_map,
            path_map,
            bundles_by_album,
            counts,
        )
        self._map_references(
            snapshot,
            source_revision,
            prepared_at,
            plan,
            album_map,
            artist_map,
            track_map,
            recording_map,
            path_map,
            counts,
        )
        plan.reference_counts = sorted(
            counts.values(), key=lambda item: (item.kind, item.user_id or "")
        )
        plan.blockers = sorted(set(plan.blockers))
        plan.warnings = sorted(set(plan.warnings))
        return plan

    async def _discover_embedded_art(
        self, plan: LegacyCatalogImportPlan, prepared_at: float
    ) -> None:
        reads = 0
        for bundle in plan.bundles:
            if (
                bundle.album_identity is None
                or bundle.artwork is not None
                or not bundle.membership.tracks
                or reads >= self._embedded_art_read_limit
            ):
                continue
            track = bundle.membership.tracks[0]
            reads += 1
            art = await asyncio.to_thread(
                self._cover_reader.read_cover_art, Path(track.file_path)
            )
            if art:
                bundle.artwork = LocalArtworkAssociation(
                    local_album_id=bundle.membership.album.id,
                    cover_url=None,
                    source="embedded",
                    source_locator=track.id,
                    updated_at=prepared_at,
                )
        for bundle in plan.bundles:
            identity = bundle.album_identity
            if identity is None or bundle.artwork is not None:
                continue
            bundle.artwork = LocalArtworkAssociation(
                local_album_id=bundle.membership.album.id,
                cover_url=None,
                source="provider",
                source_locator=identity.release_group_mbid,
                updated_at=prepared_at,
            )
        plan.embedded_art_reads = reads

    def _count_roots(
        self,
        plan: LegacyCatalogImportPlan,
        counts: dict[tuple[str, str | None], MigrationReferenceCount],
        prepared_at: float,
    ) -> None:
        for root in self._resolver.settings.library_roots:
            self._increment(counts, "root", mapped=True)
            plan.reference_provenance.append(
                MigrationProvenance(
                    source_kind="root",
                    source_key=root.id,
                    target_kind="library_root",
                    target_id=root.id,
                    source_revision=_hash(msgspec.to_builtins(root)),
                    imported_at=prepared_at,
                )
            )

    def _artist(
        self,
        name: object,
        mbid: object,
        *,
        prepared_at: float,
        emitted_artists: set[str],
        emitted_aliases: set[str],
        artist_map: dict[str, str],
    ) -> tuple[
        str,
        list[LocalArtist],
        list[LocalArtistExternalIdentity],
        list[LocalArtistAlias],
    ]:
        display_name = str(name or "Unknown Artist")
        provider_id = str(mbid).casefold() if _valid_mbid(mbid) else None
        identity_key = (
            f"mbid:{provider_id}" if provider_id else f"name:{_fold(display_name)}"
        )
        artist_id = _stable_id("artist", identity_key)
        artists: list[LocalArtist] = []
        identities: list[LocalArtistExternalIdentity] = []
        aliases: list[LocalArtistAlias] = []
        if artist_id not in emitted_artists:
            emitted_artists.add(artist_id)
            artists.append(
                LocalArtist(
                    id=artist_id,
                    display_name=display_name,
                    folded_name=_fold(display_name),
                    kind="group",
                    created_at=prepared_at,
                    updated_at=prepared_at,
                )
            )
            if provider_id:
                identities.append(
                    LocalArtistExternalIdentity(
                        local_artist_id=artist_id,
                        provider_artist_id=provider_id,
                        decision_source="legacy_import",
                        selected_at=prepared_at,
                    )
                )
        if provider_id:
            artist_map[provider_id] = artist_id
            if provider_id not in emitted_aliases:
                emitted_aliases.add(provider_id)
                aliases.append(
                    LocalArtistAlias(
                        alias=provider_id,
                        local_artist_id=artist_id,
                        kind="legacy_artist",
                        created_at=prepared_at,
                    )
                )
        return artist_id, artists, identities, aliases

    def _identified_bundle(
        self,
        release_group: str,
        rows: list[dict[str, object]],
        snapshot: dict[str, list[dict[str, object]]],
        source_revision: str,
        prepared_at: float,
        plan: LegacyCatalogImportPlan,
        emitted_artists: set[str],
        emitted_artist_aliases: set[str],
        album_map: dict[str, str],
        artist_map: dict[str, str],
        track_map: dict[str, str],
        recording_map: dict[str, list[str]],
        path_map: dict[str, tuple[str, str]],
        counts: dict[tuple[str, str | None], MigrationReferenceCount],
        *,
        group_first: dict[str, object] | None = None,
        group_is_compilation: bool | None = None,
    ) -> LegacyCatalogImportBundle | None:
        rows.sort(
            key=lambda row: (
                _as_int(row.get("disc_number"), 1),
                _as_int(row.get("track_number")),
                str(row.get("id")),
            )
        )
        first = group_first or rows[0]
        album_id = _stable_id("album", release_group)
        album_map[release_group] = album_id
        is_compilation = (
            any(bool(row.get("is_compilation")) for row in rows)
            if group_is_compilation is None
            else group_is_compilation
        )
        artists: list[LocalArtist] = []
        artist_identities: list[LocalArtistExternalIdentity] = []
        artist_aliases: list[LocalArtistAlias] = []
        if is_compilation:
            album_artist_id = VARIOUS_ARTISTS_ID
            album_artist_name = "Various Artists"
        else:
            album_artist_id, created, identities, aliases = self._artist(
                first.get("album_artist_name") or first.get("artist_name"),
                first.get("album_artist_mbid") or first.get("artist_mbid"),
                prepared_at=prepared_at,
                emitted_artists=emitted_artists,
                emitted_aliases=emitted_artist_aliases,
                artist_map=artist_map,
            )
            artists.extend(created)
            artist_identities.extend(identities)
            artist_aliases.extend(aliases)
            album_artist_name = str(
                first.get("album_artist_name")
                or first.get("artist_name")
                or "Unknown Artist"
            )
        tracks: list[LocalTrack] = []
        track_credits: dict[str, list[LocalArtistCredit]] = {}
        track_identities: list[LocalTrackExternalIdentity] = []
        reviews: list[MigrationReview] = []
        provenance: list[MigrationProvenance] = []
        for row in rows:
            file_id = str(row.get("id"))
            resolved = self._resolver.resolve(str(row.get("file_path") or ""))
            if resolved is None:
                plan.blockers.append(
                    f"Library file {file_id} is outside every typed root."
                )
                self._increment(counts, "library_file", mapped=False)
                continue
            path_map[str(row.get("file_path"))] = (file_id, album_id)
            track_map[file_id] = file_id
            mtime_ns = int(_as_float(row.get("file_mtime")) * 1_000_000_000)
            stat_revision = exact_stat_revision(
                _as_int(row.get("file_size_bytes")), mtime_ns
            )
            tag_revision = _hash(
                [
                    row.get("track_title"),
                    row.get("artist_name"),
                    row.get("album_title"),
                    row.get("album_artist_name"),
                    row.get("recording_mbid"),
                    release_group,
                ]
            )
            tracks.append(
                LocalTrack(
                    id=file_id,
                    local_album_id=album_id,
                    root_id=resolved.root_id,
                    file_path=str(row.get("file_path")),
                    relative_path=resolved.relative_path,
                    path_hash=_hash([resolved.root_id, resolved.relative_path]),
                    file_size_bytes=_as_int(row.get("file_size_bytes")),
                    file_mtime_ns=mtime_ns,
                    stat_revision=stat_revision,
                    stat_revision_kind="legacy_float",
                    tag_revision=tag_revision,
                    tags_read_at=_as_float(row.get("tagged_at")) or None,
                    title=str(row.get("track_title") or "Unknown Track"),
                    artist_name=str(row.get("artist_name") or "Unknown Artist"),
                    album_title=str(row.get("album_title") or "Unknown Album"),
                    album_artist_name=album_artist_name,
                    disc_number=_as_int(row.get("disc_number"), 1),
                    track_number=_as_int(row.get("track_number")),
                    year=_as_int(row.get("year")) or None,
                    genre=str(row.get("genre")) if row.get("genre") else None,
                    title_sort=str(row.get("track_sort_name"))
                    if row.get("track_sort_name")
                    else None,
                    artist_sort=str(row.get("artist_sort_name"))
                    if row.get("artist_sort_name")
                    else None,
                    album_sort=str(row.get("album_sort_name"))
                    if row.get("album_sort_name")
                    else None,
                    album_artist_sort=str(row.get("album_artist_sort_name"))
                    if row.get("album_artist_sort_name")
                    else None,
                    disc_subtitle=str(row.get("disc_subtitle"))
                    if row.get("disc_subtitle")
                    else None,
                    is_compilation=is_compilation,
                    duration_seconds=_as_float(row.get("duration_seconds")) or None,
                    file_format=str(row.get("file_format") or "unknown"),
                    bit_rate=_as_int(row.get("bit_rate")) or None,
                    sample_rate=_as_int(row.get("sample_rate")) or None,
                    bit_depth=_as_int(row.get("bit_depth")) or None,
                    channels=_as_int(row.get("channels")) or None,
                    replaygain_track_gain=row.get("replaygain_track_gain"),
                    replaygain_album_gain=row.get("replaygain_album_gain"),
                    replaygain_track_peak=row.get("replaygain_track_peak"),
                    replaygain_album_peak=row.get("replaygain_album_peak"),
                    ingest_source=str(row.get("source") or "legacy_import"),
                    download_task_id=str(row.get("download_task_id"))
                    if row.get("download_task_id")
                    else None,
                    source_path=str(row.get("source_path"))
                    if row.get("source_path")
                    else None,
                    imported_at=_as_float(row.get("imported_at"), prepared_at),
                    membership_source="legacy_import",
                    membership_locked=True,
                    desired_policy_revision=self._resolver.policy_revision,
                    applied_policy_revision=self._resolver.policy_revision,
                    applied_policy=resolved.policy,
                )
            )
            track_artist_id, created, identities, aliases = self._artist(
                row.get("artist_name"),
                row.get("artist_mbid"),
                prepared_at=prepared_at,
                emitted_artists=emitted_artists,
                emitted_aliases=emitted_artist_aliases,
                artist_map=artist_map,
            )
            artists.extend(created)
            artist_identities.extend(identities)
            artist_aliases.extend(aliases)
            track_credits[file_id] = [
                LocalArtistCredit(
                    local_artist_id=track_artist_id,
                    position=0,
                    credited_name=str(row.get("artist_name") or "Unknown Artist"),
                )
            ]
            recording = row.get("recording_mbid")
            if recording:
                if not _valid_mbid(recording):
                    reviews.append(
                        MigrationReview(
                            id=_stable_id("library-file-review", file_id),
                            local_track_id=file_id,
                            state="needs_review",
                            reason_code="legacy_invalid_recording_id",
                            input_revision=_hash(row),
                            created_at=_as_float(row.get("imported_at"), prepared_at),
                            updated_at=prepared_at,
                        )
                    )
                else:
                    recording_key = str(recording).casefold()
                    recording_map[recording_key].append(file_id)
                    track_identities.append(
                        LocalTrackExternalIdentity(
                            local_track_id=file_id,
                            recording_mbid=recording_key,
                            release_mbid=str(row.get("release_mbid")).casefold()
                            if _valid_mbid(row.get("release_mbid"))
                            else None,
                            decision_source="legacy_import",
                            selected_at=prepared_at,
                        )
                    )
            provenance.append(
                MigrationProvenance(
                    source_kind="library_file",
                    source_key=file_id,
                    target_kind="local_track",
                    target_id=file_id,
                    source_revision=_hash(row),
                    imported_at=prepared_at,
                )
            )
            self._increment(counts, "library_file", mapped=True)
        if len(tracks) != len(rows):
            return None
        album = LocalAlbum(
            id=album_id,
            root_id=tracks[0].root_id,
            grouping_key=f"legacy:{release_group}",
            title=str(first.get("album_title") or "Unknown Album"),
            album_artist_id=album_artist_id,
            album_artist_name=album_artist_name,
            album_artist_sort_name=str(first.get("album_artist_sort_name"))
            if first.get("album_artist_sort_name")
            else None,
            year=_as_int(first.get("year")) or None,
            original_release_date=str(first.get("original_release_date"))
            if first.get("original_release_date")
            else None,
            primary_genre=str(first.get("genre")) if first.get("genre") else None,
            is_compilation=is_compilation,
            grouping_source="legacy_import",
            grouping_locked=True,
            created_at=min(track.imported_at for track in tracks),
            updated_at=prepared_at,
        )
        cover_url, cover_source = self._cover(snapshot, release_group)
        bundle = LegacyCatalogImportBundle(
            membership=CatalogMembership(
                album=album,
                artists=artists,
                tracks=tracks,
                album_credits=[
                    LocalArtistCredit(
                        local_artist_id=album_artist_id,
                        position=0,
                        credited_name=album_artist_name,
                    )
                ],
                track_credits=track_credits,
            ),
            album_identity=LocalAlbumExternalIdentity(
                local_album_id=album_id,
                release_group_mbid=release_group,
                release_mbid=self._mode_release(rows),
                decision_source="legacy_import",
                selected_at=prepared_at,
            ),
            track_identities=track_identities,
            artist_identities=artist_identities,
            artist_aliases=artist_aliases,
            album_aliases=[
                LocalAlbumAlias(
                    alias=release_group,
                    local_album_id=album_id,
                    kind="legacy_release_group",
                    created_at=prepared_at,
                )
            ],
            artwork=LocalArtworkAssociation(
                local_album_id=album_id,
                cover_url=cover_url,
                source=cover_source,
                source_locator=release_group,
                updated_at=prepared_at,
            )
            if cover_url
            else None,
            reviews=reviews,
            provenance=provenance,
        )
        return bundle

    def _local_only_library_bundle(
        self,
        group_key: str,
        rows: list[dict[str, object]],
        snapshot: dict[str, list[dict[str, object]]],
        source_revision: str,
        prepared_at: float,
        plan: LegacyCatalogImportPlan,
        emitted_artists: set[str],
        emitted_artist_aliases: set[str],
        artist_map: dict[str, str],
        track_map: dict[str, str],
        recording_map: dict[str, list[str]],
        path_map: dict[str, tuple[str, str]],
        counts: dict[tuple[str, str | None], MigrationReferenceCount],
        *,
        group_first: dict[str, object] | None = None,
        group_is_compilation: bool | None = None,
    ) -> LegacyCatalogImportBundle | None:
        synthetic_group = _stable_id("legacy-local-album", group_key)
        if group_first is None:
            tagged_rows = [
                row
                for row in rows
                if normalize_group_value(str(row.get("album_title") or ""))
            ]
            if tagged_rows:
                group_first = min(
                    tagged_rows,
                    key=lambda row: (
                        _as_int(row.get("disc_number"), 1),
                        _as_int(row.get("track_number")),
                        str(row.get("id")),
                    ),
                )
        bundle = self._identified_bundle(
            synthetic_group,
            rows,
            snapshot,
            source_revision,
            prepared_at,
            plan,
            emitted_artists,
            emitted_artist_aliases,
            {},
            artist_map,
            track_map,
            recording_map,
            path_map,
            counts,
            group_first=group_first,
            group_is_compilation=group_is_compilation,
        )
        if bundle is None:
            return None

        bundle.album_identity = None
        bundle.album_aliases = []
        bundle.artwork = None
        bundle.membership.album.grouping_key = f"legacy-local:{group_key}"
        effective_album_title = str(
            (group_first or {}).get("album_title") or ""
        ).strip()
        if not normalize_group_value(effective_album_title):
            directory = grouping_directory(
                bundle.membership.tracks[0].relative_path
            )
            effective_album_title = Path(directory).name or "Unknown Album"
        bundle.membership.album.title = effective_album_title
        source_by_id = {str(row.get("id")): row for row in rows}
        replacement_reviews: list[MigrationReview] = []
        local_track_ids = set(source_by_id)
        for review in bundle.reviews:
            if review.local_track_id not in local_track_ids:
                replacement_reviews.append(review)
        for track in bundle.membership.tracks:
            row = source_by_id[track.id]
            raw_track_title = str(row.get("track_title") or "")
            raw_album_title = str(row.get("album_title") or "")
            raw_album_artist = str(row.get("album_artist_name") or "")
            track.title = raw_track_title.strip() or Path(track.relative_path).stem
            track.album_title = effective_album_title
            track.tag_album_title = raw_album_title
            track.tag_album_artist_name = raw_album_artist
            track.metadata_incomplete = any(
                not str(row.get(field) or "").strip()
                for field in (
                    "track_title",
                    "artist_name",
                    "album_title",
                    "album_artist_name",
                )
            )
            track.tag_revision = _hash(
                [
                    row.get("track_title"),
                    row.get("artist_name"),
                    row.get("album_title"),
                    row.get("album_artist_name"),
                    row.get("recording_mbid"),
                    row.get("release_group_mbid"),
                ]
            )
            raw_release_group = row.get("release_group_mbid")
            replacement_reviews.append(
                MigrationReview(
                    id=_stable_id("library-file-review", track.id),
                    local_track_id=track.id,
                    state="needs_review",
                    reason_code=(
                        "legacy_invalid_release_group_id"
                        if raw_release_group
                        else "legacy_missing_release_group_id"
                    ),
                    input_revision=_hash(row),
                    created_at=_as_float(row.get("imported_at"), prepared_at),
                    updated_at=prepared_at,
                )
            )
        bundle.reviews = replacement_reviews
        return bundle

    def _add_review_rows(
        self,
        snapshot: dict[str, list[dict[str, object]]],
        source_revision: str,
        prepared_at: float,
        plan: LegacyCatalogImportPlan,
        emitted_artists: set[str],
        emitted_artist_aliases: set[str],
        album_map: dict[str, str],
        artist_map: dict[str, str],
        track_map: dict[str, str],
        path_map: dict[str, tuple[str, str]],
        bundles_by_album: dict[str, LegacyCatalogImportBundle],
        counts: dict[tuple[str, str | None], MigrationReferenceCount],
        *,
        group_first: dict[str, object] | None = None,
    ) -> None:
        grouped: dict[tuple[str, str, str], list[dict[str, object]]] = defaultdict(list)
        for row in snapshot["manual_review_queue"]:
            path = str(row.get("file_path") or "")
            resolution = row.get("resolution")
            linked = path_map.get(path)
            if linked is not None:
                track_id, album_id = linked
                state = (
                    "resolved"
                    if resolution in {"accepted", "manual_id"}
                    else "needs_review"
                )
                bundles_by_album[album_id].reviews.append(
                    MigrationReview(
                        id=_stable_id("review", str(row.get("id"))),
                        local_track_id=track_id,
                        state=state,
                        reason_code=f"legacy_{resolution or 'unresolved'}",
                        input_revision=_hash(row),
                        created_at=_as_float(row.get("created_at"), prepared_at),
                        updated_at=_as_float(row.get("resolved_at"), prepared_at),
                        decided_at=_as_float(row.get("resolved_at")) or None,
                    )
                )
                self._increment(counts, "review_row", mapped=True)
                continue
            resolved = self._resolver.resolve(path)
            if resolved is None:
                plan.blockers.append(
                    f"Review row {row.get('id')} is outside every typed root."
                )
                self._increment(counts, "review_row", mapped=False)
                continue
            parent = Path(resolved.relative_path).parent.as_posix()
            album_name = str(
                row.get("extracted_album") or Path(path).parent.name or "Unknown Album"
            )
            grouped[(resolved.root_id, parent, album_name)].append(row)

        for (root_id, parent, album_name), rows in sorted(grouped.items()):
            album_id = _stable_id(
                "review-album", f"{root_id}:{parent}:{_fold(album_name)}"
            )
            first = group_first or rows[0]
            artist_name = str(first.get("extracted_artist") or "Unknown Artist")
            artist_id, artists, identities, aliases = self._artist(
                artist_name,
                None,
                prepared_at=prepared_at,
                emitted_artists=emitted_artists,
                emitted_aliases=emitted_artist_aliases,
                artist_map=artist_map,
            )
            tracks: list[LocalTrack] = []
            reviews: list[MigrationReview] = []
            track_credits: dict[str, list[LocalArtistCredit]] = {}
            provenance: list[MigrationProvenance] = []
            for row in sorted(
                rows,
                key=lambda item: (
                    _as_int(item.get("disc_number"), 1),
                    _as_int(item.get("track_number")),
                    _as_int(item.get("id")),
                ),
            ):
                review_id = str(row.get("id"))
                path = str(row.get("file_path"))
                resolved = self._resolver.resolve(path)
                track_id = _stable_id("review-track", f"{review_id}:{path}")
                track_map[track_id] = track_id
                path_map[path] = (track_id, album_id)
                excluded = row.get("resolution") == "rejected"
                tracks.append(
                    LocalTrack(
                        id=track_id,
                        local_album_id=album_id,
                        root_id=root_id,
                        file_path=path,
                        relative_path=resolved.relative_path,
                        path_hash=_hash([root_id, resolved.relative_path]),
                        file_size_bytes=_as_int(row.get("file_size")),
                        file_mtime_ns=int(
                            _as_float(row.get("created_at"), prepared_at)
                            * 1_000_000_000
                        ),
                        stat_revision=exact_stat_revision(
                            _as_int(row.get("file_size")),
                            int(
                                _as_float(row.get("created_at"), prepared_at)
                                * 1_000_000_000
                            ),
                        ),
                        stat_revision_kind="legacy_review",
                        tag_revision=_hash(
                            [
                                row.get("extracted_title"),
                                row.get("extracted_artist"),
                                album_name,
                            ]
                        ),
                        tags_read_at=_as_float(row.get("created_at"), prepared_at),
                        title=str(row.get("extracted_title") or Path(path).stem),
                        artist_name=str(
                            row.get("extracted_artist") or "Unknown Artist"
                        ),
                        album_title=album_name,
                        album_artist_name=artist_name,
                        disc_number=_as_int(row.get("disc_number"), 1),
                        track_number=_as_int(row.get("track_number")),
                        year=_as_int(row.get("extracted_year")) or None,
                        duration_seconds=_as_float(row.get("duration")) or None,
                        file_format=str(
                            row.get("file_format")
                            or Path(path).suffix.lstrip(".")
                            or "unknown"
                        ),
                        availability="excluded" if excluded else "indexed",
                        manual_excluded=excluded,
                        excluded_at=_as_float(row.get("resolved_at"))
                        if excluded
                        else None,
                        ingest_source="legacy_review",
                        imported_at=_as_float(row.get("created_at"), prepared_at),
                        membership_source="legacy_import",
                        membership_locked=True,
                        desired_policy_revision=self._resolver.policy_revision,
                        applied_policy_revision=self._resolver.policy_revision,
                        applied_policy="excluded" if excluded else resolved.policy,
                    )
                )
                track_credits[track_id] = [
                    LocalArtistCredit(
                        local_artist_id=artist_id,
                        position=0,
                        credited_name=str(
                            row.get("extracted_artist") or "Unknown Artist"
                        ),
                    )
                ]
                reviews.append(
                    MigrationReview(
                        id=_stable_id("review", review_id),
                        local_track_id=track_id,
                        state="excluded" if excluded else "needs_review",
                        reason_code="legacy_rejected"
                        if excluded
                        else "legacy_unresolved",
                        input_revision=_hash(row),
                        created_at=_as_float(row.get("created_at"), prepared_at),
                        updated_at=_as_float(row.get("resolved_at"), prepared_at),
                        decided_at=_as_float(row.get("resolved_at")) or None,
                    )
                )
                provenance.append(
                    MigrationProvenance(
                        source_kind="review_row",
                        source_key=review_id,
                        target_kind="local_track",
                        target_id=track_id,
                        source_revision=_hash(row),
                        imported_at=prepared_at,
                    )
                )
                self._increment(counts, "review_row", mapped=True)
            bundle = LegacyCatalogImportBundle(
                membership=CatalogMembership(
                    album=LocalAlbum(
                        id=album_id,
                        root_id=root_id,
                        grouping_key=f"legacy-review:{root_id}:{parent}:{_fold(album_name)}",
                        title=album_name,
                        album_artist_id=artist_id,
                        album_artist_name=artist_name,
                        grouping_source="legacy_import",
                        grouping_locked=True,
                        created_at=min(track.imported_at for track in tracks),
                        updated_at=prepared_at,
                    ),
                    artists=artists,
                    tracks=tracks,
                    album_credits=[
                        LocalArtistCredit(
                            local_artist_id=artist_id,
                            position=0,
                            credited_name=artist_name,
                        )
                    ],
                    track_credits=track_credits,
                ),
                artist_identities=identities,
                artist_aliases=aliases,
                reviews=reviews,
                provenance=provenance,
            )
            plan.bundles.append(bundle)
            bundles_by_album[album_id] = bundle

    def _map_references(
        self,
        snapshot: dict[str, list[dict[str, object]]],
        source_revision: str,
        prepared_at: float,
        plan: LegacyCatalogImportPlan,
        album_map: dict[str, str],
        artist_map: dict[str, str],
        track_map: dict[str, str],
        recording_map: dict[str, list[str]],
        path_map: dict[str, tuple[str, str]],
        counts: dict[tuple[str, str | None], MigrationReferenceCount],
    ) -> None:
        del source_revision
        playlist_ids = {str(row.get("id")) for row in snapshot["playlists"]}
        history_text_map: dict[tuple[str, str, str], list[str]] = defaultdict(list)
        track_membership: dict[str, tuple[str, str]] = {}
        for bundle in plan.bundles:
            for track in bundle.membership.tracks:
                credits = bundle.membership.track_credits.get(track.id, [])
                artist_id = (
                    credits[0].local_artist_id
                    if credits
                    else bundle.membership.album.album_artist_id
                )
                track_membership[track.id] = (
                    track.local_album_id,
                    artist_id,
                )
                history_text_map[
                    (
                        _fold(track.title),
                        _fold(track.artist_name),
                        _fold(track.album_title),
                    )
                ].append(track.id)
        for row in snapshot["user_favorites"]:
            kind = str(row.get("item_kind"))
            item_id = str(row.get("item_id"))
            target = self._map_kind(
                kind, item_id, album_map, artist_map, track_map, playlist_ids
            )
            self._reference(
                plan,
                counts,
                "favorite",
                f"{row.get('user_id')}:{kind}:{item_id}",
                target,
                row,
                prepared_at,
                user_id=str(row.get("user_id")),
            )
        for row in snapshot["play_history"]:
            recording = str(row.get("recording_mbid") or "").casefold()
            candidates = recording_map.get(recording, [])
            target = ("local_track", candidates[0]) if len(candidates) == 1 else None
            if target is None and row.get("release_group_mbid"):
                album_id = album_map.get(str(row["release_group_mbid"]).casefold())
                target = ("local_album", album_id) if album_id else None
            if target is None:
                text_candidates = history_text_map.get(
                    (
                        _fold(str(row.get("track_name") or "")),
                        _fold(str(row.get("artist_name") or "")),
                        _fold(str(row.get("album_name") or "")),
                    ),
                    [],
                )
                if len(text_candidates) == 1:
                    target = ("local_track", text_candidates[0])
            self._reference(
                plan,
                counts,
                "history",
                str(row.get("id")),
                target,
                row,
                prepared_at,
                user_id=str(row.get("user_id")),
            )
        for row in snapshot["playlist_tracks"]:
            local_source = str(row.get("source_type")) in LOCAL_PLAYLIST_SOURCES
            file_id = row.get("library_file_id") or (
                row.get("track_source_id") if local_source else None
            )
            target_id = track_map.get(str(file_id)) if file_id else None
            valid_fields = True
            if row.get("album_id") and local_source:
                expected_album = album_map.get(str(row["album_id"]).casefold())
                valid_fields &= expected_album is None or (
                    target_id is not None
                    and track_membership.get(target_id, (None, None))[0]
                    == expected_album
                )
            if row.get("artist_id") and local_source:
                expected_artist = artist_map.get(str(row["artist_id"]).casefold())
                valid_fields &= expected_artist is None or (
                    target_id is not None
                    and track_membership.get(target_id, (None, None))[1]
                    == expected_artist
                )
            target = ("local_track", target_id) if target_id and valid_fields else None
            if target is None:
                tombstone_id = _stable_id("playlist-tombstone", str(row.get("id")))
                plan.tombstones.append(
                    MigrationTombstone(
                        id=tombstone_id,
                        source_kind="playlist_track",
                        source_key=str(row.get("id")),
                        legacy_file_id=str(file_id) if file_id else None,
                        title=str(row.get("track_name") or "Unavailable track"),
                        artist_name=str(row.get("artist_name"))
                        if row.get("artist_name")
                        else None,
                        album_name=str(row.get("album_name"))
                        if row.get("album_name")
                        else None,
                        source_type=str(row.get("source_type")),
                        created_at=prepared_at,
                    )
                )
                target = ("reference_tombstone", tombstone_id)
                self._increment(
                    counts,
                    "playlist_track",
                    mapped=True,
                    tombstoned=True,
                )
                self._add_provenance(
                    plan,
                    "playlist_track",
                    str(row.get("id")),
                    target,
                    row,
                    prepared_at,
                )
                continue
            self._reference(
                plan,
                counts,
                "playlist_track",
                str(row.get("id")),
                target,
                row,
                prepared_at,
                retained=target is not None,
            )
        for row in snapshot["album_release_pins"]:
            album_id = album_map.get(str(row.get("release_group_mbid")).casefold())
            self._reference(
                plan,
                counts,
                "album_release_pin",
                str(row.get("release_group_mbid")),
                ("local_album", album_id) if album_id else None,
                row,
                prepared_at,
            )
        for row in snapshot["compat_bookmarks"]:
            track_id = track_map.get(str(row.get("file_id")))
            self._reference(
                plan,
                counts,
                "compat_bookmark",
                f"{row.get('user_id')}:{row.get('file_id')}",
                ("local_track", track_id) if track_id else None,
                row,
                prepared_at,
                user_id=str(row.get("user_id")),
                retained=track_id is not None,
            )
        for row in snapshot["compat_play_queues"]:
            self._reference(
                plan,
                counts,
                "compat_play_queue",
                str(row.get("user_id")),
                ("compat_play_queue", str(row.get("user_id"))),
                row,
                prepared_at,
                user_id=str(row.get("user_id")),
                retained=True,
            )
        for row in snapshot["compat_play_queue_items"]:
            track_id = track_map.get(str(row.get("file_id")))
            self._reference(
                plan,
                counts,
                "compat_play_queue_item",
                f"{row.get('user_id')}:{row.get('item_index')}",
                ("local_track", track_id) if track_id else None,
                row,
                prepared_at,
                user_id=str(row.get("user_id")),
                retained=track_id is not None,
            )
        for row in snapshot["compat_id_map"]:
            kind = str(row.get("kind"))
            internal_id = str(row.get("internal_id"))
            target = self._map_kind(
                kind, internal_id, album_map, artist_map, track_map, playlist_ids
            )
            if target is None:
                tombstone_id = _stable_id("jellyfin-tombstone", str(row.get("jf_id")))
                plan.tombstones.append(
                    MigrationTombstone(
                        id=tombstone_id,
                        source_kind="jellyfin_id_map",
                        source_key=str(row.get("jf_id")),
                        legacy_file_id=internal_id
                        if kind.casefold() == "track"
                        else None,
                        title=f"Unavailable Jellyfin {kind.casefold()}",
                        source_type="jellyfin",
                        created_at=prepared_at,
                    )
                )
                target = ("reference_tombstone", tombstone_id)
                self._increment(
                    counts,
                    "jellyfin_id_map",
                    mapped=True,
                    retained=True,
                    tombstoned=True,
                )
                self._add_provenance(
                    plan,
                    "jellyfin_id_map",
                    str(row.get("jf_id")),
                    target,
                    row,
                    prepared_at,
                )
                continue
            self._reference(
                plan,
                counts,
                "jellyfin_id_map",
                str(row.get("jf_id")),
                target,
                row,
                prepared_at,
                retained=target is not None,
            )
        decisions = [
            row
            for row in snapshot["manual_review_queue"]
            if row.get("resolution") is not None
        ]
        for row in decisions:
            linked = path_map.get(str(row.get("file_path")))
            self._reference(
                plan,
                counts,
                "manual_decision",
                str(row.get("id")),
                ("local_track", linked[0]) if linked else None,
                row,
                prepared_at,
            )
        for release_group, album_id in album_map.items():
            source = {"kind": "album", "id": release_group}
            self._reference(
                plan,
                counts,
                "subsonic_id",
                f"album:{release_group}",
                ("local_album", album_id),
                source,
                prepared_at,
                retained=True,
            )
            self._reference(
                plan,
                counts,
                "native_album_alias",
                release_group,
                ("local_album", album_id),
                source,
                prepared_at,
                retained=True,
            )
        for legacy_artist, artist_id in artist_map.items():
            source = {"kind": "artist", "id": legacy_artist}
            self._reference(
                plan,
                counts,
                "subsonic_id",
                f"artist:{legacy_artist}",
                ("local_artist", artist_id),
                source,
                prepared_at,
                retained=True,
            )
            self._reference(
                plan,
                counts,
                "native_artist_alias",
                legacy_artist,
                ("local_artist", artist_id),
                source,
                prepared_at,
                retained=True,
            )
        for track_id in track_map:
            self._reference(
                plan,
                counts,
                "subsonic_id",
                f"track:{track_id}",
                ("local_track", track_id),
                {"kind": "track", "id": track_id},
                prepared_at,
                retained=True,
            )
        for bundle in plan.bundles:
            if bundle.artwork is not None:
                self._reference(
                    plan,
                    counts,
                    "artwork_reference",
                    bundle.membership.album.id,
                    ("local_album", bundle.membership.album.id),
                    {
                        "cover_url": bundle.artwork.cover_url,
                        "source": bundle.artwork.source,
                        "source_locator": bundle.artwork.source_locator,
                    },
                    prepared_at,
                    retained=True,
                )

    @staticmethod
    def _map_kind(
        kind: str,
        item_id: str,
        album_map: dict[str, str],
        artist_map: dict[str, str],
        track_map: dict[str, str],
        playlist_ids: set[str],
    ) -> tuple[str, str] | None:
        normalized = kind.casefold()
        if normalized in {"album", "music_album"}:
            target = album_map.get(item_id.casefold())
            return ("local_album", target) if target else None
        if normalized in {"artist", "music_artist"}:
            target = artist_map.get(item_id.casefold())
            return ("local_artist", target) if target else None
        if normalized in {"track", "song", "audio"}:
            target = track_map.get(item_id)
            return ("local_track", target) if target else None
        if normalized == "playlist":
            return ("playlist", item_id) if item_id in playlist_ids else None
        if normalized == "library" and item_id:
            return ("library", item_id)
        if normalized == "genre" and item_id:
            return ("genre", item_id)
        return None

    def _reference(
        self,
        plan: LegacyCatalogImportPlan,
        counts: dict[tuple[str, str | None], MigrationReferenceCount],
        kind: str,
        source_key: str,
        target: tuple[str, str] | None,
        source_row: object,
        prepared_at: float,
        *,
        user_id: str | None = None,
        retained: bool = False,
    ) -> None:
        self._increment(
            counts,
            kind,
            mapped=target is not None,
            user_id=user_id,
            retained=retained,
        )
        if target is None:
            plan.blockers.append(f"{kind} reference {source_key} cannot be resolved.")
            return
        self._add_provenance(plan, kind, source_key, target, source_row, prepared_at)

    @staticmethod
    def _add_provenance(
        plan: LegacyCatalogImportPlan,
        kind: str,
        source_key: str,
        target: tuple[str, str],
        source_row: object,
        prepared_at: float,
    ) -> None:
        plan.reference_provenance.append(
            MigrationProvenance(
                source_kind=kind,
                source_key=source_key,
                target_kind=target[0],
                target_id=target[1],
                source_revision=_hash(source_row),
                imported_at=prepared_at,
            )
        )

    @staticmethod
    def _increment(
        counts: dict[tuple[str, str | None], MigrationReferenceCount],
        kind: str,
        *,
        mapped: bool,
        user_id: str | None = None,
        retained: bool = False,
        tombstoned: bool = False,
    ) -> None:
        keys = [(kind, None)]
        if user_id is not None:
            keys.append((kind, user_id))
        for key in keys:
            count = counts.setdefault(
                key, MigrationReferenceCount(kind=kind, user_id=key[1])
            )
            count.source += 1
            if mapped:
                count.mapped += 1
            else:
                count.unresolved += 1
            if retained:
                count.retained += 1
            if tombstoned:
                count.tombstoned += 1

    @staticmethod
    def _mode_release(rows: list[dict[str, object]]) -> str | None:
        values = [
            str(row["release_mbid"]).casefold()
            for row in rows
            if _valid_mbid(row.get("release_mbid"))
        ]
        if not values:
            return None
        return max(sorted(set(values)), key=values.count)

    @staticmethod
    def _cover(
        snapshot: dict[str, list[dict[str, object]]], release_group: str
    ) -> tuple[str | None, str]:
        for row in snapshot["library_album_meta"]:
            if str(row.get("release_group_mbid")).casefold() == release_group:
                return (
                    str(row["cover_url"]) if row.get("cover_url") else None,
                    "provider",
                )
        for row in snapshot["library_albums"]:
            if str(row.get("mbid")).casefold() == release_group:
                return (
                    str(row["cover_url"]) if row.get("cover_url") else None,
                    "provider",
                )
        return None, "provider"

    @staticmethod
    def _report(
        migration_id: str,
        plan: LegacyCatalogImportPlan,
        *,
        state: str | None = None,
    ) -> MigrationDryRunReport:
        identified = [bundle for bundle in plan.bundles if bundle.album_identity]
        local_only = [bundle for bundle in plan.bundles if not bundle.album_identity]
        return MigrationDryRunReport(
            migration_id=migration_id,
            source_revision=plan.source_revision,
            root_revision=plan.root_revision,
            state=state or ("blocked" if plan.blockers else "ready"),
            identified_albums=len(identified),
            local_only_albums=len(local_only),
            identified_tracks=sum(
                len(bundle.membership.tracks) for bundle in identified
            ),
            local_only_tracks=sum(
                len(bundle.membership.tracks) for bundle in local_only
            ),
            artists=len(
                {
                    artist.id
                    for bundle in plan.bundles
                    for artist in bundle.membership.artists
                }
            ),
            reference_counts=plan.reference_counts,
            blockers=plan.blockers,
            warnings=plan.warnings,
            network_calls=plan.network_calls,
            tag_reads=plan.tag_reads,
            fingerprints=plan.fingerprints,
            embedded_art_reads=plan.embedded_art_reads,
        )
