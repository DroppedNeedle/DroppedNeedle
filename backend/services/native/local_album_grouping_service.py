"""Apply durable local grouping contexts after target tag indexing commits."""

from __future__ import annotations

import uuid
from collections.abc import Awaitable, Callable
from pathlib import PurePosixPath
from typing import Any

from infrastructure.persistence.native_library_store import NativeLibraryStore
from models.identification import (
    ExistingAlbumMembership,
    GroupingApplication,
    GroupingTrack,
)
from services.native.identification_queue_service import IdentificationQueueService
from services.native.identification_revisions import album_input_revisions
from services.native.local_album_grouper import (
    LocalAlbumGrouper,
    _hungarian_min,
    grouping_directory,
    normalize_group_value,
)

_GROUPING_NAMESPACE = uuid.UUID("296d6087-edf2-4861-8c72-7eae22654aef")
ARTIST_RESOLUTION_BATCH_SIZE = 256
QUEUE_BATCH_SIZE = 256
STAGING_BATCH_SIZE = 500
STAGED_GROUPING_THRESHOLD = 512
CONTINUITY_COMPONENT_EDGE_LIMIT = 512


def grouping_track_from_row(row: dict) -> GroupingTrack:
    return GroupingTrack(
        local_track_id=str(row["id"]),
        root_id=str(row["root_id"]),
        relative_path=str(row["relative_path"]),
        title=str(row["title"] or ""),
        artist_name=str(row["artist_name"] or ""),
        album_title=str(row["tag_album_title"] or ""),
        album_artist_name=str(row["tag_album_artist_name"] or ""),
        artist_sort_name=row["artist_sort"],
        album_artist_sort_name=row["album_artist_sort"],
        track_number=int(row["track_number"] or 0),
        disc_number=int(row["disc_number"] or 1),
        duration_seconds=row["duration_seconds"],
        recording_mbid=row["embedded_recording_mbid"],
        release_mbid=row["embedded_release_mbid"],
        release_group_mbid=row["embedded_release_group_mbid"],
        is_compilation=bool(row["is_compilation"]),
        tags_readable=not bool(row["metadata_incomplete"]),
        membership_locked=bool(row["membership_locked"]),
        current_album_id=str(row["local_album_id"]),
    )


def grouping_artist_candidate_id(display_name: str) -> str:
    return str(uuid.uuid5(_GROUPING_NAMESPACE, f"artist:{display_name}:group"))


def grouping_album_id(grouping_key: str) -> str:
    return str(uuid.uuid5(_GROUPING_NAMESPACE, grouping_key))


class LocalAlbumGroupingService:
    def __init__(
        self,
        store: NativeLibraryStore,
        queue: IdentificationQueueService,
        grouper: LocalAlbumGrouper | None = None,
    ) -> None:
        self._store = store
        self._queue = queue
        self._grouper = grouper or LocalAlbumGrouper()

    async def regroup_run(
        self,
        run_id: str,
        *,
        now: float,
        checkpoint: Callable[[str, str], Awaitable[bool]] | None = None,
        frozen_policy_revision: str = "",
    ) -> int:
        enqueued = 0
        while contexts := await self._store.get_pending_grouping_contexts(run_id):
            for context in contexts:
                if checkpoint is not None and not await checkpoint(
                    run_id, frozen_policy_revision
                ):
                    return enqueued
                root_id = str(context["root_id"])
                relative_directory = str(context["relative_directory"])
                candidate_count = await self._store.count_grouping_context_candidates(
                    root_id,
                    relative_directory,
                    limit=STAGED_GROUPING_THRESHOLD + 1,
                )
                if candidate_count > STAGED_GROUPING_THRESHOLD:
                    enqueued += await self._regroup_large_context(
                        run_id,
                        root_id,
                        relative_directory,
                        now=now,
                        checkpoint=checkpoint,
                        frozen_policy_revision=frozen_policy_revision,
                    )
                    continue
                rows = await self._store.get_grouping_context_tracks(
                    root_id, relative_directory
                )
                rows = [
                    row
                    for row in rows
                    if grouping_directory(str(row["relative_path"]))
                    == context["relative_directory"]
                ]
                memberships: dict[str, ExistingAlbumMembership] = {}
                for row in rows:
                    album_id = str(row["local_album_id"])
                    membership = memberships.setdefault(
                        album_id,
                        ExistingAlbumMembership(
                            local_album_id=album_id,
                            track_ids=[],
                            created_at=float(row["album_created_at"]),
                        ),
                    )
                    membership.track_ids.append(str(row["id"]))
                groups = self._grouper.group(
                    [grouping_track_from_row(row) for row in rows],
                    existing=list(memberships.values()),
                )
                artist_names = list(
                    dict.fromkeys(group.album_artist_name for group in groups)
                )
                artist_ids: dict[str, str] = {}
                for offset in range(0, len(artist_names), ARTIST_RESOLUTION_BATCH_SIZE):
                    if checkpoint is not None and not await checkpoint(
                        run_id, frozen_policy_revision
                    ):
                        return enqueued
                    names = artist_names[offset : offset + ARTIST_RESOLUTION_BATCH_SIZE]
                    candidates = [
                        (name, None, "group", grouping_artist_candidate_id(name))
                        for name in names
                    ]
                    resolved = await self._store.resolve_or_create_local_artists(
                        candidates, now=now, background=True
                    )
                    artist_ids.update(
                        {
                            name: resolved[grouping_artist_candidate_id(name)][0]
                            for name in names
                        }
                    )
                applications: list[GroupingApplication] = []
                for group in groups:
                    artist_id = artist_ids[group.album_artist_name]
                    album_id = group.retained_album_id or grouping_album_id(
                        group.grouping_key
                    )
                    applications.append(
                        GroupingApplication(
                            group=group,
                            local_album_id=album_id,
                            local_artist_id=artist_id,
                        )
                    )
                album_ids, _ = await self._store.apply_grouping_context(
                    run_id,
                    str(context["root_id"]),
                    str(context["relative_directory"]),
                    applications,
                    now=now,
                )
                refreshed_rows = await self._store.get_grouping_context_tracks(
                    str(context["root_id"]), str(context["relative_directory"])
                )
                rows_by_album: dict[str, list[dict]] = {}
                for row in refreshed_rows:
                    rows_by_album.setdefault(str(row["local_album_id"]), []).append(row)
                queue_candidates: list[tuple[str, str, str]] = []
                for album_id in album_ids:
                    album_rows = rows_by_album.get(album_id, [])
                    if not album_rows:
                        continue
                    policy = {str(row["applied_policy"]) for row in album_rows}
                    embedded = any(
                        row["embedded_release_group_mbid"]
                        or row["embedded_release_mbid"]
                        for row in album_rows
                    )
                    if policy == {"automatic"} or (
                        policy == {"local_metadata"} and embedded
                    ):
                        revisions = album_input_revisions(album_rows)
                        queue_candidates.append(
                            (
                                album_id,
                                ":".join(revisions),
                                "automatic"
                                if policy == {"automatic"}
                                else "post_processing",
                            )
                        )
                for offset in range(0, len(queue_candidates), QUEUE_BATCH_SIZE):
                    if checkpoint is not None and not await checkpoint(
                        run_id, frozen_policy_revision
                    ):
                        return enqueued
                    batch = queue_candidates[offset : offset + QUEUE_BATCH_SIZE]
                    dispositions = await self._queue.enqueue_albums_with_disposition(
                        batch, now=now, scan_run_id=run_id, background=True
                    )
                    enqueued += sum(created for _, created in dispositions)
                await self._store.complete_grouping_context(
                    run_id,
                    str(context["root_id"]),
                    str(context["relative_directory"]),
                )
        return enqueued

    async def _regroup_large_context(
        self,
        run_id: str,
        root_id: str,
        relative_directory: str,
        *,
        now: float,
        checkpoint: Callable[[str, str], Awaitable[bool]] | None,
        frozen_policy_revision: str,
    ) -> int:
        enqueued = 0

        async def continue_grouping() -> bool:
            return checkpoint is None or await checkpoint(
                run_id, frozen_policy_revision
            )

        state = await self._store.get_grouping_staging_state(
            run_id, root_id, relative_directory
        )
        if state in {"pending", "tracks"}:
            while True:
                if not await continue_grouping():
                    return enqueued
                rows, cursor = await self._store.get_grouping_context_track_page(
                    run_id,
                    root_id,
                    relative_directory,
                    limit=STAGING_BATCH_SIZE,
                )
                evidence = [
                    self._grouping_evidence(row, relative_directory)
                    for row in rows
                    if grouping_directory(str(row["relative_path"]))
                    == relative_directory
                ]
                await self._store.stage_grouping_track_page(
                    run_id,
                    root_id,
                    relative_directory,
                    evidence,
                    next_cursor=cursor,
                    exhausted=not rows,
                )
                if not rows:
                    break
            state = "tokens"
        if state == "tokens":
            while True:
                if not await continue_grouping():
                    return enqueued
                if await self._store.prepare_staged_grouping_tokens(
                    run_id,
                    root_id,
                    relative_directory,
                    limit=STAGING_BATCH_SIZE,
                ):
                    break
            state = "groups"
        if state == "groups":
            while True:
                if not await continue_grouping():
                    return enqueued
                if await self._store.stage_grouping_summary_page(
                    run_id,
                    root_id,
                    relative_directory,
                    limit=STAGING_BATCH_SIZE,
                ):
                    break
            state = "continuity"
        if state == "continuity":
            while True:
                if not await continue_grouping():
                    return enqueued
                if not await self._store.assign_isolated_grouping_continuity(
                    run_id,
                    root_id,
                    relative_directory,
                    limit=STAGING_BATCH_SIZE,
                ):
                    break
            while True:
                if not await continue_grouping():
                    return enqueued
                while await self._store.discard_resolved_grouping_edges(
                    run_id,
                    root_id,
                    relative_directory,
                    limit=STAGING_BATCH_SIZE,
                ):
                    if not await continue_grouping():
                        return enqueued
                edge = await self._store.get_next_grouping_edge(
                    run_id, root_id, relative_directory
                )
                if edge is None:
                    break
                component = await self._load_continuity_component(
                    run_id, root_id, relative_directory, edge
                )
                if component is None:
                    await self._store.apply_next_sparse_grouping_continuity(
                        run_id, root_id, relative_directory
                    )
                    continue
                await self._store.apply_grouping_continuity_component(
                    run_id,
                    root_id,
                    relative_directory,
                    list(component.values()),
                    self._continuity_assignments(list(component.values())),
                )
            await self._store.finish_grouping_continuity(
                run_id, root_id, relative_directory
            )
            state = "albums"
        if state == "albums":
            while True:
                if not await continue_grouping():
                    return enqueued
                groups = await self._store.get_unprovisioned_grouping_groups(
                    run_id,
                    root_id,
                    relative_directory,
                    limit=ARTIST_RESOLUTION_BATCH_SIZE,
                )
                if not groups:
                    break
                candidates = [
                    (
                        str(group["album_artist_name"]),
                        None,
                        "group",
                        grouping_artist_candidate_id(
                            str(group["album_artist_name"])
                        ),
                    )
                    for group in groups
                ]
                resolved = await self._store.resolve_or_create_local_artists(
                    candidates, now=now, background=True
                )
                for group in groups:
                    candidate_id = grouping_artist_candidate_id(
                        str(group["album_artist_name"])
                    )
                    group["local_artist_id"] = resolved[candidate_id][0]
                    group["local_album_id"] = group["retained_album_id"] or (
                        grouping_album_id(str(group["grouping_key"]))
                    )
                await self._store.provision_staged_grouping_groups(
                    run_id,
                    root_id,
                    relative_directory,
                    groups,
                    now=now,
                )
            await self._store.finish_grouping_album_provisioning(
                run_id, root_id, relative_directory
            )
            state = "memberships"
        if state == "memberships":
            while True:
                if not await continue_grouping():
                    return enqueued
                if await self._store.apply_staged_grouping_membership_page(
                    run_id,
                    root_id,
                    relative_directory,
                    limit=STAGING_BATCH_SIZE,
                ):
                    break
            state = "retirement"
        if state == "retirement":
            while True:
                if not await continue_grouping():
                    return enqueued
                if await self._store.retire_staged_grouping_albums(
                    run_id,
                    root_id,
                    relative_directory,
                    now=now,
                    limit=STAGING_BATCH_SIZE,
                ):
                    break
            state = "queue"
        if state == "queue":
            while True:
                if not await continue_grouping():
                    return enqueued
                page = await self._store.get_staged_grouping_queue_page(
                    run_id,
                    root_id,
                    relative_directory,
                    limit=QUEUE_BATCH_SIZE,
                )
                if not page:
                    break
                candidates = [
                    (
                        str(row["local_album_id"]),
                        str(row["input_revision"]),
                        str(row["trigger"]),
                    )
                    for row in page
                    if row.get("local_album_id") is not None
                ]
                if candidates:
                    dispositions = await self._queue.enqueue_albums_with_disposition(
                        candidates,
                        now=now,
                        scan_run_id=run_id,
                        grouping_context=(root_id, relative_directory),
                        queue_cursor=str(page[-1]["grouping_token"]),
                        background=True,
                    )
                    enqueued += sum(created for _, created in dispositions)
                else:
                    await self._store.advance_staged_grouping_queue(
                        run_id,
                        root_id,
                        relative_directory,
                        str(page[-1]["grouping_token"]),
                    )
            await self._store.complete_grouping_context(
                run_id, root_id, relative_directory
            )
        return enqueued

    @staticmethod
    def _grouping_evidence(
        row: dict[str, Any], relative_directory: str
    ) -> dict[str, Any]:
        track_id = str(row["id"])
        album_title = str(row["tag_album_title"] or "")
        album_artist = str(row["tag_album_artist_name"] or "")
        if bool(row["membership_locked"]):
            preliminary_key = f"manual:{row['local_album_id']}"
            reason = "MANUAL_MEMBERSHIP_RESTORED"
        elif album_title.strip():
            artist_partition = "" if bool(row["is_compilation"]) else (
                normalize_group_value(album_artist)
            )
            preliminary_key = (
                f"tagged:{normalize_group_value(album_title)}:{artist_partition}"
            )
            parent = str(PurePosixPath(str(row["relative_path"])).parent)
            reason = (
                "COMPATIBLE_DISC_DIRECTORIES"
                if parent != relative_directory
                else "CONSISTENT_ALBUM_TAGS"
            )
        else:
            preliminary_key = "missing"
            reason = "MISSING_ALBUM_TAGS"
        return {
            "local_track_id": track_id,
            "preliminary_key": preliminary_key,
            "title": album_title,
            "title_normalized": normalize_group_value(album_title),
            "album_artist_name": album_artist,
            "album_artist_normalized": normalize_group_value(album_artist),
            "track_number": int(row["track_number"] or 0),
            "old_album_id": str(row["local_album_id"]),
            "album_created_at": float(row["album_created_at"]),
            "reason_code": reason,
        }

    async def _load_continuity_component(
        self,
        run_id: str,
        root_id: str,
        relative_directory: str,
        seed: dict[str, Any],
    ) -> dict[tuple[str, str], dict[str, Any]] | None:
        edges: dict[tuple[str, str], dict[str, Any]] = {}
        old_pending = {str(seed["old_album_id"])}
        token_pending: set[str] = set()
        old_seen: set[str] = set()
        token_seen: set[str] = set()
        while old_pending or token_pending:
            if old_pending:
                batch = sorted(old_pending)[:STAGING_BATCH_SIZE]
                old_pending.difference_update(batch)
                old_seen.update(batch)
                rows = await self._store.get_grouping_edges_for_old_albums(
                    run_id,
                    root_id,
                    relative_directory,
                    batch,
                    limit=CONTINUITY_COMPONENT_EDGE_LIMIT + 1,
                )
                if len(rows) > CONTINUITY_COMPONENT_EDGE_LIMIT:
                    return None
                for row in rows:
                    key = (str(row["old_album_id"]), str(row["grouping_token"]))
                    edges[key] = row
                    token = key[1]
                    if token not in token_seen:
                        token_pending.add(token)
                if len(edges) > CONTINUITY_COMPONENT_EDGE_LIMIT:
                    return None
            if token_pending:
                batch = sorted(token_pending)[:STAGING_BATCH_SIZE]
                token_pending.difference_update(batch)
                token_seen.update(batch)
                rows = await self._store.get_grouping_edges_for_tokens(
                    run_id,
                    root_id,
                    relative_directory,
                    batch,
                    limit=CONTINUITY_COMPONENT_EDGE_LIMIT + 1,
                )
                if len(rows) > CONTINUITY_COMPONENT_EDGE_LIMIT:
                    return None
                for row in rows:
                    key = (str(row["old_album_id"]), str(row["grouping_token"]))
                    edges[key] = row
                    album_id = key[0]
                    if album_id not in old_seen:
                        old_pending.add(album_id)
                if len(edges) > CONTINUITY_COMPONENT_EDGE_LIMIT:
                    return None
        return edges

    @staticmethod
    def _continuity_assignments(
        edges: list[dict[str, Any]],
    ) -> dict[str, tuple[str, str]]:
        old_created = {
            str(edge["old_album_id"]): float(edge["album_created_at"])
            for edge in edges
        }
        grouping_keys = {
            str(edge["grouping_token"]): str(edge["grouping_key"])
            for edge in edges
        }
        old_ids = sorted(old_created, key=lambda value: (old_created[value], value))
        tokens = sorted(grouping_keys, key=lambda value: grouping_keys[value])
        old_index = {value: index for index, value in enumerate(old_ids)}
        token_index = {value: index for index, value in enumerate(tokens)}
        size = max(len(old_ids), len(tokens))
        weights = [[0] * size for _ in range(size)]
        old_neighbors: dict[int, set[int]] = {}
        token_neighbors: dict[int, set[int]] = {}
        for edge in edges:
            row = old_index[str(edge["old_album_id"])]
            column = token_index[str(edge["grouping_token"])]
            weights[row][column] = int(edge["overlap_count"])
            old_neighbors.setdefault(row, set()).add(column)
            token_neighbors.setdefault(column, set()).add(row)
        maximum = max((int(edge["overlap_count"]) for edge in edges), default=0)
        assignment = _hungarian_min(
            [[maximum - value for value in row] for row in weights]
        )
        result: dict[str, tuple[str, str]] = {}
        for row_index, column_index in enumerate(assignment[: len(old_ids)]):
            if column_index >= len(tokens) or weights[row_index][column_index] == 0:
                continue
            value = weights[row_index][column_index]
            old_best = max(
                weights[row_index][candidate]
                for candidate in old_neighbors[row_index]
            )
            new_best = max(
                weights[candidate][column_index]
                for candidate in token_neighbors[column_index]
            )
            tied = (
                sum(
                    weights[row_index][candidate] == old_best
                    for candidate in old_neighbors[row_index]
                )
                > 1
                or sum(
                    weights[candidate][column_index]
                    == new_best
                    for candidate in token_neighbors[column_index]
                )
                > 1
            )
            result[tokens[column_index]] = (
                old_ids[row_index],
                "CONTINUITY_TIE_BROKEN" if tied else "MAXIMUM_TRACK_OVERLAP",
            )
        return result
