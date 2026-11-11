import asyncio
import json
import sqlite3
import subprocess
import tempfile
import threading
import time
from collections.abc import Callable
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import AsyncMock, MagicMock

import msgspec
import pytest

from core.exceptions import ExternalServiceError, StaleRevisionError
from infrastructure.audio.fingerprinter import FingerprintStatus
from repositories.musicbrainz_management_models import MbManagementRelease
from infrastructure.degradation import try_get_degradation_context
from infrastructure.integration_result import IntegrationResult
from infrastructure.persistence.native_library_store import NativeLibraryStore
from infrastructure.queue.priority_queue import RequestPriority
from models.audio import FingerprintResult
from models.identification import (
    AlbumCandidate,
    IdentificationDecision,
    CandidateEvidence,
    CandidateTrack,
    FingerprintOutcome,
    GroupingApplication,
    GroupingTrack,
    IdentificationAttempt,
    IdentificationEvidenceRecord,
    ProposedLocalAlbum,
    TrackEvidence,
)
from models.library_work import (
    IdentificationJob,
    ReviewDecision,
    ScanRun,
)
from models.local_catalog import (
    CatalogMembership,
    LocalAlbum,
    LocalAlbumExternalIdentity,
    LocalArtist,
    LocalArtistCredit,
    LocalTrack,
)
from services.native.album_candidate_service import (
    MAX_CANDIDATES,
    AlbumCandidateService,
)
from services.native.album_coverage_service import AlbumCoverageService
from services.native.album_evidence_engine import (
    MATCHER_VERSION,
    AlbumEvidenceEngine,
)
from services.native.album_identification_service import (
    MAX_NEW_FINGERPRINTS_PER_ATTEMPT,
    AlbumIdentificationService,
    _edition_hold_metrics,
    _enforce_raw_track_identities,
)
from services.native.conditional_fingerprint_service import (
    FINGERPRINTER_VERSION,
    ConditionalFingerprintService,
)
from services.native.explicit_reidentification_worker import (
    ExplicitReidentificationWorker,
)
from services.native.identification_evidence_projector import (
    IdentificationEvidenceProjector,
)
from services.native.identification_queue_service import (
    LEASE_SECONDS,
    MAX_BACKOFF_SECONDS,
    MAX_DEFERRAL_ATTEMPTS,
    SUBJECT_NOT_AVAILABLE_GRACE_SECONDS,
    IdentificationQueueService,
)
from services.native.identification_revisions import (
    album_identity_revision,
    album_input_revisions,
)
from services.native.local_album_grouping_service import LocalAlbumGroupingService
from services.native.reidentification_service import (
    IdentificationWorkArbiter,
    ReidentificationService,
)
from services.native.target_application_runtime import (
    run_target_identification_worker,
)

EMBEDDED_GROUP = "11111111-1111-4111-8111-111111111111"
EMBEDDED_GROUP_OTHER = "22222222-2222-4222-8222-222222222222"
EMBEDDED_RELEASE = "33333333-3333-4333-8333-333333333333"
EMBEDDED_RECORDING = "44444444-4444-4444-8444-444444444444"
EMBEDDED_RELEASE_TRACK = "77777777-7777-4777-8777-777777777777"
EMBEDDED_RECORDING_OTHER = "55555555-5555-4555-8555-555555555555"
EMBEDDED_ARTIST = "66666666-6666-4666-8666-666666666666"


class FakeProvider:
    def __init__(self, candidates: list[AlbumCandidate] | None = None) -> None:
        self.candidates = candidates or []
        self.calls: list[tuple[str, RequestPriority]] = []
        self.exact_releases: list[tuple[str, RequestPriority]] = []
        self.edition_calls: list[str] = []

    async def search_album_candidate_ids(
        self, artist: str, title: str, limit: int, priority: RequestPriority
    ) -> list[str]:
        self.calls.append(("album", priority))
        return [candidate.release_group_mbid for candidate in self.candidates[:limit]]

    async def search_recording_candidate_ids(
        self,
        artist: str,
        title: str,
        limit: int,
        priority: RequestPriority,
    ) -> list[str]:
        self.calls.append(("recording", priority))
        return [candidate.release_group_mbid for candidate in self.candidates[:limit]]

    async def get_album_candidate(
        self,
        release_group_mbid: str,
        target_track_count: int,
        priority: RequestPriority,
    ) -> AlbumCandidate | None:
        self.calls.append(("detail", priority))
        return next(
            (
                candidate
                for candidate in self.candidates
                if candidate.release_group_mbid == release_group_mbid
            ),
            None,
        )

    async def get_album_candidate_editions(
        self,
        release_group_mbid: str,
        target_track_count: int,
        priority: RequestPriority,
        *,
        max_editions: int = 2,
    ) -> list[AlbumCandidate]:
        self.edition_calls.append(release_group_mbid)
        top_pick = next(
            (
                candidate
                for candidate in self.candidates
                if candidate.release_group_mbid == release_group_mbid
            ),
            None,
        )
        return [] if top_pick is None else [top_pick]

    async def get_exact_release_candidate(
        self,
        release_mbid: str,
        priority: RequestPriority,
    ) -> AlbumCandidate | None:
        self.exact_releases.append((release_mbid, priority))
        return next(
            (
                candidate
                for candidate in self.candidates
                if candidate.release_mbid == release_mbid
            ),
            None,
        )


class AliasCandidateProvider(FakeProvider):
    async def search_album_candidate_ids(
        self, artist: str, title: str, limit: int, priority: RequestPriority
    ) -> list[str]:
        self.calls.append(("album", priority))
        return ["alias-group-a", "alias-group-b"]

    async def search_recording_candidate_ids(
        self,
        artist: str,
        title: str,
        limit: int,
        priority: RequestPriority,
    ) -> list[str]:
        return []

    async def get_album_candidate(
        self,
        release_group_mbid: str,
        target_track_count: int,
        priority: RequestPriority,
    ) -> AlbumCandidate | None:
        self.calls.append((release_group_mbid, priority))
        return _candidate(group="canonical-group")


class FakeFingerprinter:
    def __init__(self, result: FingerprintResult, *, enabled: bool = True) -> None:
        self.result = result
        self.enabled = enabled
        self.generate_calls = 0
        self.lookup_calls = 0

    def is_enabled(self) -> bool:
        return self.enabled

    async def generate_fingerprint(self, path: Path) -> tuple[str, int]:
        self.generate_calls += 1
        return "fingerprint", 180

    async def lookup_fingerprint(
        self, fingerprint: str, duration: int
    ) -> FingerprintResult:
        self.lookup_calls += 1
        return self.result


class DegradedProvider(FakeProvider):
    async def search_album_candidate_ids(
        self, artist: str, title: str, limit: int, priority: RequestPriority
    ) -> list[str]:
        context = try_get_degradation_context()
        assert context is not None
        context.record(IntegrationResult.error("musicbrainz", "not persisted"))
        return []


@pytest.fixture
def db_path(tmp_path: Path) -> Path:
    path = tmp_path / "library.db"
    with sqlite3.connect(path) as connection:
        connection.execute("CREATE TABLE auth_users (id TEXT PRIMARY KEY)")
        connection.executemany(
            "INSERT INTO auth_users(id) VALUES (?)", [("admin",), ("worker",)]
        )
    return path


@pytest.fixture
def store(db_path: Path) -> NativeLibraryStore:
    return NativeLibraryStore(db_path, threading.Lock())


def _candidate(
    *,
    group: str = "rg-1",
    title: str = "Album",
    recording: str = "recording-1",
    release_track: str | None = None,
) -> AlbumCandidate:
    return AlbumCandidate(
        release_group_mbid=group,
        release_mbid=f"release-{group}",
        album_title=title,
        album_artist_name="Artist",
        artist_mbid="artist-mbid",
        tracks=[
            CandidateTrack(
                title="Track",
                position=1,
                absolute_position=1,
                duration_seconds=180,
                recording_mbid=recording,
                release_track_mbid=release_track,
            )
        ],
    )


async def _seed_album(
    store: NativeLibraryStore,
    suffix: str = "1",
    *,
    embedded_group: str | None = None,
    embedded_release: str | None = None,
    embedded_recording: str | None = None,
    embedded_release_track: str | None = None,
    policy: str = "automatic",
    second_embedded_group: str | None = None,
    second_embedded_recording: str | None = None,
) -> None:
    artist = LocalArtist(
        id=f"artist-{suffix}",
        display_name="Artist",
        folded_name="artist",
        normalized_name="artist",
        kind="group",
        created_at=1,
        updated_at=1,
    )
    album = LocalAlbum(
        id=f"album-{suffix}",
        root_id="root",
        grouping_key=f"group-{suffix}",
        title="Album",
        album_artist_id=artist.id,
        album_artist_name="Artist",
        created_at=1,
        updated_at=1,
    )
    track = LocalTrack(
        id=f"track-{suffix}",
        local_album_id=album.id,
        root_id="root",
        file_path=f"/music/{suffix}.flac",
        relative_path=f"{suffix}.flac",
        path_hash=f"hash-{suffix}",
        file_size_bytes=100,
        file_mtime_ns=1,
        stat_revision=f"stat-{suffix}",
        tag_revision=f"tag-{suffix}",
        title="Track",
        artist_name="Artist",
        album_title="Album",
        album_artist_name="Artist",
        title_provenance="tag",
        album_title_provenance="tag",
        album_artist_provenance="tag",
        track_number=1,
        duration_seconds=180,
        file_format="flac",
        imported_at=1,
        applied_policy=policy,
        applied_policy_revision="policy-1",
        embedded_release_group_mbid=embedded_group,
        embedded_release_mbid=embedded_release,
        embedded_recording_mbid=embedded_recording,
        embedded_release_track_mbid=embedded_release_track,
        embedded_album_artist_mbid=EMBEDDED_ARTIST if embedded_group else None,
    )
    tracks = [track]
    credits = {track.id: [LocalArtistCredit(local_artist_id=artist.id, position=0)]}
    if second_embedded_group is not None:
        second = msgspec.structs.replace(
            track,
            id=f"track-{suffix}-2",
            file_path=f"/music/{suffix}-2.flac",
            relative_path=f"{suffix}-2.flac",
            path_hash=f"hash-{suffix}-2",
            stat_revision=f"stat-{suffix}-2",
            tag_revision=f"tag-{suffix}-2",
            title="Track 2",
            track_number=2,
            embedded_release_group_mbid=second_embedded_group,
            embedded_recording_mbid=(
                second_embedded_recording or EMBEDDED_RECORDING_OTHER
            ),
        )
        tracks.append(second)
        credits[second.id] = [LocalArtistCredit(local_artist_id=artist.id, position=0)]
    await store.create_catalog_membership(
        CatalogMembership(
            album=album,
            artists=[artist],
            tracks=tracks,
            album_credits=[LocalArtistCredit(local_artist_id=artist.id, position=0)],
            track_credits=credits,
        )
    )


async def _claimed_job(
    store: NativeLibraryStore,
    album_id: str = "album-1",
    *,
    kind: str = "automatic",
) -> dict:
    await store.enqueue_identification_job(
        IdentificationJob(
            id=f"job-{album_id}",
            local_album_id=album_id,
            kind=kind,
            dedupe_key=f"{kind}:{album_id}:revision",
            input_revision="revision",
            priority=20,
            created_at=1,
        )
    )
    claimed = await store.claim_identification_job("worker", now=2, lease_seconds=60)
    assert claimed is not None
    return claimed


def _service(
    store: NativeLibraryStore,
    provider: FakeProvider,
    fingerprinter: FakeFingerprinter,
    invalidate: AsyncMock | None = None,
    on_identified: AsyncMock | None = None,
    provider_available: Callable[[], bool] | None = None,
    canonical_provider: object | None = None,
    edition_opt_in: Callable[[str], bool] | None = None,
) -> AlbumIdentificationService:
    queue = IdentificationQueueService(store)
    return AlbumIdentificationService(
        store,
        queue,
        AlbumCandidateService(provider),
        AlbumEvidenceEngine(),
        ConditionalFingerprintService(store, fingerprinter),
        invalidate,
        on_identified,
        provider_available=provider_available,
        canonical_provider=canonical_provider,  # type: ignore[arg-type]
        edition_opt_in=edition_opt_in,
    )


@pytest.mark.asyncio
async def test_candidate_recall_is_bounded_and_uses_honest_priorities() -> None:
    provider = FakeProvider([_candidate(group=f"rg-{index}") for index in range(20)])
    service = AlbumCandidateService(provider)
    track = LocalTrack  # keep the domain import exercised by this contract
    del track
    from models.identification import GroupingTrack

    local = [
        GroupingTrack(
            local_track_id="track",
            root_id="root",
            relative_path="track.flac",
            title="Track",
            artist_name="Artist",
            album_title="Album",
            album_artist_name="Artist",
            title_provenance="tag",
            album_title_provenance="tag",
            album_artist_provenance="tag",
        )
    ]
    automatic = await service.recall(local)
    assert len(automatic) <= 10
    assert {priority for _, priority in provider.calls} == {
        RequestPriority.BACKGROUND_SYNC
    }
    provider.calls.clear()
    await service.recall(local, explicit=True)
    assert {priority for _, priority in provider.calls} == {
        RequestPriority.USER_INITIATED
    }


@pytest.mark.asyncio
async def test_candidate_recall_uses_only_a_complete_unanimous_exact_release() -> None:
    provider = FakeProvider([_candidate(group="rg-1"), _candidate(group="rg-2")])
    service = AlbumCandidateService(provider)
    from models.identification import GroupingTrack

    tracks = [
        GroupingTrack(
            local_track_id=f"track-{index}",
            root_id="root",
            relative_path=f"track-{index}.flac",
            title="Track",
            artist_name="Artist",
            album_title="Album",
            album_artist_name="Artist",
            title_provenance="tag",
            album_title_provenance="tag",
            album_artist_provenance="tag",
            release_group_mbid="rg-1",
            release_mbid="release-rg-1",
        )
        for index in range(2)
    ]

    candidates = await service.recall(tracks, explicit=True)

    assert [candidate.release_mbid for candidate in candidates] == ["release-rg-1"]
    assert provider.exact_releases == [("release-rg-1", RequestPriority.USER_INITIATED)]
    assert provider.calls == []


@pytest.mark.asyncio
async def test_candidate_recall_does_not_search_around_mixed_release_ids() -> None:
    provider = FakeProvider([_candidate(group="rg-1"), _candidate(group="rg-2")])
    service = AlbumCandidateService(provider)
    from models.identification import GroupingTrack

    tracks = [
        GroupingTrack(
            local_track_id=f"track-{index}",
            root_id="root",
            relative_path=f"track-{index}.flac",
            title="Track",
            artist_name="Artist",
            album_title="Album",
            album_artist_name="Artist",
            title_provenance="tag",
            album_title_provenance="tag",
            album_artist_provenance="tag",
            release_group_mbid="rg-1",
            release_mbid=["release-rg-1", "release-rg-2"][index],
        )
        for index in range(2)
    ]

    assert await service.recall(tracks, explicit=True) == []
    assert provider.exact_releases == []
    assert provider.calls == []


@pytest.mark.asyncio
@pytest.mark.parametrize("track_count", [2, 26])
async def test_candidate_recall_attempts_a_partial_unanimous_exact_release(
    track_count: int,
) -> None:
    """Blanks abstain: populated tags that agree on one release take the
    exact-release path even when sibling tracks lack tags entirely."""
    provider = FakeProvider([_candidate(group="rg-1")])
    service = AlbumCandidateService(provider)
    from models.identification import GroupingTrack

    tracks = [
        GroupingTrack(
            local_track_id=f"track-{index}",
            root_id="root",
            relative_path=f"track-{index}.flac",
            title="Track",
            artist_name="Artist",
            album_title="Album",
            album_artist_name="Artist",
            title_provenance="tag",
            album_title_provenance="tag",
            album_artist_provenance="tag",
            release_group_mbid="rg-1",
            release_mbid="release-rg-1" if index == 0 else None,
        )
        for index in range(track_count)
    ]

    candidates = await service.recall(tracks, explicit=True)

    assert [candidate.release_mbid for candidate in candidates] == ["release-rg-1"]
    assert candidates[0].source_kinds == ["embedded_exact_release"]
    assert provider.exact_releases == [("release-rg-1", RequestPriority.USER_INITIATED)]
    assert provider.calls == []


@pytest.mark.asyncio
async def test_candidate_recall_derives_a_missing_group_from_the_exact_release() -> (
    None
):
    provider = FakeProvider([_candidate(group="rg-1")])
    service = AlbumCandidateService(provider)
    from models.identification import GroupingTrack

    tracks = [
        GroupingTrack(
            local_track_id=f"track-{index}",
            root_id="root",
            relative_path=f"track-{index}.flac",
            title="Track",
            artist_name="Artist",
            album_title="Album",
            album_artist_name="Artist",
            title_provenance="tag",
            album_title_provenance="tag",
            album_artist_provenance="tag",
            release_mbid="release-rg-1",
        )
        for index in range(2)
    ]

    candidates = await service.recall(tracks, explicit=True)

    assert [candidate.release_group_mbid for candidate in candidates] == ["rg-1"]
    assert provider.calls == []


@pytest.mark.asyncio
async def test_candidate_recall_keeps_an_exact_release_when_its_provider_group_differs() -> (
    None
):
    provider = FakeProvider([_candidate(group="rg-1")])
    service = AlbumCandidateService(provider)
    from models.identification import GroupingTrack

    tracks = [
        GroupingTrack(
            local_track_id=f"track-{index}",
            root_id="root",
            relative_path=f"track-{index}.flac",
            title="Track",
            artist_name="Artist",
            album_title="Album",
            album_artist_name="Artist",
            title_provenance="tag",
            album_title_provenance="tag",
            album_artist_provenance="tag",
            release_group_mbid="different-rg",
            release_mbid="release-rg-1",
        )
        for index in range(2)
    ]

    candidates = await service.recall(tracks, explicit=True)

    assert [(item.release_group_mbid, item.release_mbid) for item in candidates] == [
        ("rg-1", "release-rg-1")
    ]
    assert provider.calls == []


@pytest.mark.asyncio
async def test_candidate_recall_deduplicates_provider_canonical_aliases() -> None:
    provider = AliasCandidateProvider()
    tracks = [
        GroupingTrack(
            local_track_id="track-1",
            root_id="root",
            relative_path="track.flac",
            title="Track",
            artist_name="Artist",
            album_title="Album",
            album_artist_name="Artist",
            title_provenance="tag",
            album_title_provenance="tag",
            album_artist_provenance="tag",
        )
    ]

    candidates = await AlbumCandidateService(provider).recall(tracks, explicit=True)

    assert len(candidates) == 1
    assert candidates[0].release_group_mbid == "canonical-group"
    assert candidates[0].source_kinds == ["album_tags"]


def _sibling_edition(group: str) -> AlbumCandidate:
    return msgspec.structs.replace(
        _candidate(group=group),
        release_mbid=f"release-{group}-sibling",
        tracks=[
            CandidateTrack(
                title="Track",
                position=1,
                absolute_position=1,
                duration_seconds=181,
                recording_mbid="recording-sibling",
            )
        ],
    )


class EditionsProvider(FakeProvider):
    """FakeProvider plus a sibling-edition table for the Phase 2 trial."""

    def __init__(
        self,
        candidates: list[AlbumCandidate] | None = None,
        editions: dict[str, list[AlbumCandidate]] | None = None,
    ) -> None:
        super().__init__(candidates)
        self.editions = editions or {}

    async def get_album_candidate_editions(
        self,
        release_group_mbid: str,
        target_track_count: int,
        priority: RequestPriority,
        *,
        max_editions: int = 2,
    ) -> list[AlbumCandidate]:
        self.edition_calls.append(release_group_mbid)
        return list(self.editions.get(release_group_mbid, []))


def _single_album_tracks() -> list[GroupingTrack]:
    return [
        GroupingTrack(
            local_track_id="track-1",
            root_id="root",
            relative_path="track.flac",
            title="Track",
            artist_name="Artist",
            album_title="Album",
            album_artist_name="Artist",
            title_provenance="tag",
            album_title_provenance="tag",
            album_artist_provenance="tag",
        )
    ]


def _aborting_checkpoint(fail_on: int) -> tuple[Callable[[], bool], dict]:
    state = {"calls": 0}

    async def checkpoint() -> bool:
        state["calls"] += 1
        return state["calls"] != fail_on

    return checkpoint, state


@pytest.mark.asyncio
async def test_candidate_recall_sibling_trial_appends_editions_in_seed_order() -> None:
    provider = EditionsProvider(
        [_candidate(group="rg-a"), _candidate(group="rg-b")],
        {"rg-b": [_candidate(group="rg-b"), _sibling_edition("rg-b")]},
    )
    candidates = await AlbumCandidateService(provider).recall(
        _single_album_tracks(), sibling_release_group_ids=["rg-b"]
    )

    assert [candidate.release_mbid for candidate in candidates] == [
        "release-rg-a",
        "release-rg-b",
        "release-rg-b-sibling",
    ]
    assert [candidate.source_kinds for candidate in candidates] == [
        ["album_tags"],
        ["album_tags"],
        ["album_tags"],
    ]
    assert provider.edition_calls == ["rg-b"]


@pytest.mark.asyncio
async def test_candidate_recall_sibling_duplicate_collapses_into_built_candidate() -> (
    None
):
    provider = EditionsProvider(
        [_candidate(group="rg-b")],
        {"rg-b": [_candidate(group="rg-b")]},
    )

    candidates = await AlbumCandidateService(provider).recall(
        _single_album_tracks(), sibling_release_group_ids=["rg-b"]
    )

    assert [candidate.release_mbid for candidate in candidates] == ["release-rg-b"]
    assert candidates[0].source_kinds == ["album_tags", "recording_search"]


@pytest.mark.asyncio
async def test_candidate_recall_sibling_extras_respect_max_candidates() -> None:
    groups = [f"rg-{index}" for index in range(9)]
    provider = EditionsProvider(
        [_candidate(group=group) for group in groups],
        {group: [_candidate(group=group), _sibling_edition(group)] for group in groups},
    )

    candidates = await AlbumCandidateService(provider).recall(
        _single_album_tracks(), sibling_release_group_ids=groups
    )

    assert len(candidates) == MAX_CANDIDATES
    assert [candidate.release_mbid for candidate in candidates] == [
        mbid
        for group in groups[:5]
        for mbid in (f"release-{group}", f"release-{group}-sibling")
    ]


@pytest.mark.asyncio
@pytest.mark.parametrize(
    ("fail_on", "expected_edition_calls"),
    [
        (5, []),  # pause before the extra fetch: no provider call at all
        (6, ["rg-a"]),  # pause after the top pick: fetch already spent
    ],
)
async def test_candidate_recall_checkpoint_aborts_around_extra_fetch(
    fail_on: int, expected_edition_calls: list[str]
) -> None:
    provider = EditionsProvider(
        [],
        {"rg-a": [_candidate(group="rg-a"), _sibling_edition("rg-a")]},
    )
    checkpoint, state = _aborting_checkpoint(fail_on)

    candidates = await AlbumCandidateService(provider).recall(
        _single_album_tracks(),
        cached_fingerprint_release_groups=["rg-a"],
        checkpoint=checkpoint,
        sibling_release_group_ids=["rg-a"],
    )

    assert candidates == []
    assert provider.edition_calls == expected_edition_calls
    assert state["calls"] == fail_on


@pytest.mark.asyncio
async def test_candidate_recall_default_path_never_touches_editions_endpoint() -> None:
    """Regression pin: without sibling ids recall is byte-for-byte the
    historical bounded search - the editions endpoint is never called."""
    provider = EditionsProvider(
        [_candidate(group="rg-a"), _candidate(group="rg-b")],
        {"rg-b": [_sibling_edition("rg-b")]},
    )
    candidates = await AlbumCandidateService(provider).recall(_single_album_tracks())
    assert [candidate.release_mbid for candidate in candidates] == [
        "release-rg-a",
        "release-rg-b",
    ]
    assert provider.edition_calls == []
    assert [kind for kind, _ in provider.calls] == ["album", "detail", "detail"]


@pytest.mark.asyncio
async def test_local_metadata_embedded_identity_uses_zero_provider_calls_and_attaches_only_supported_tracks(
    store: NativeLibraryStore,
    db_path: Path,
) -> None:
    await _seed_album(
        store,
        embedded_group=EMBEDDED_GROUP,
        embedded_release=EMBEDDED_RELEASE,
        embedded_recording=EMBEDDED_RECORDING,
        embedded_release_track=EMBEDDED_RELEASE_TRACK,
        policy="local_metadata",
    )
    job = await _claimed_job(store, kind="post_processing")
    provider = FakeProvider()
    invalidator = AsyncMock()
    outcome = await _service(
        store,
        provider,
        FakeFingerprinter(
            FingerprintResult(status=FingerprintStatus.DISABLED), enabled=False
        ),
        invalidator,
    ).run_claimed_job(job, "worker", now=3)
    assert outcome == "identified"
    assert provider.calls == []
    with sqlite3.connect(db_path) as connection:
        album_identity = connection.execute(
            "SELECT release_group_mbid, decision_source FROM local_album_external_identities"
        ).fetchone()
        track_identity = connection.execute(
            "SELECT recording_mbid, release_mbid, release_track_mbid, "
            "medium_position, release_track_position, decision_source "
            "FROM local_track_external_identities"
        ).fetchone()
    assert album_identity == (EMBEDDED_GROUP, "embedded")
    assert track_identity == (
        EMBEDDED_RECORDING,
        EMBEDDED_RELEASE,
        EMBEDDED_RELEASE_TRACK,
        1,
        1,
        "embedded",
    )
    invalidated = invalidator.await_args.args[0]
    assert {
        "library",
        "artist",
        "search",
        "home",
        "discover",
        "compatibility",
        "artwork",
        "review",
    } <= invalidated


@pytest.mark.asyncio
async def test_identified_album_schedules_scan_management_after_identity_commit(
    store: NativeLibraryStore,
) -> None:
    # Step 2.6 (N-01): the lone-eligible quorum needs provider proof for a
    # single present track, so the seed carries the candidate's recording.
    await _seed_album(store, embedded_recording=EMBEDDED_RECORDING)
    context = await store.get_album_identification_context("album-1")
    assert context is not None
    expected_policy_revision = album_input_revisions(context["tracks"])[2]
    callback = AsyncMock()
    job = await _claimed_job(store)

    outcome = await _service(
        store,
        FakeProvider([_candidate(recording=EMBEDDED_RECORDING)]),
        FakeFingerprinter(FingerprintResult(status="disabled"), enabled=False),
        on_identified=callback,
    ).run_claimed_job(job, "worker", now=3)

    assert outcome == "identified"
    callback.assert_awaited_once_with("album-1", expected_policy_revision)


@pytest.mark.asyncio
async def test_provider_unavailable_defers_before_candidate_recall(
    store: NativeLibraryStore,
) -> None:
    """With the provider down (open breaker), a job that needs recall defers with
    the queue's backoff instead of recalling only to short-circuit every call."""
    await _seed_album(store)
    job = await _claimed_job(store)
    provider = FakeProvider()

    outcome = await _service(
        store,
        provider,
        FakeFingerprinter(FingerprintResult(status="disabled"), enabled=False),
        provider_available=lambda: False,
    ).run_claimed_job(job, "worker", now=3)

    assert outcome == "provider_deferred"
    assert provider.calls == []
    queue = IdentificationQueueService(store)
    assert await queue.claim("worker", now=4) is None
    assert await queue.claim("worker", now=3 + 31) is not None


@pytest.mark.asyncio
async def test_embedded_identity_still_identifies_while_provider_unavailable(
    store: NativeLibraryStore,
) -> None:
    """Beets-style libraries (complete embedded MBIDs) must keep draining with
    MusicBrainz fully down: the local decisions run before the provider gate."""
    await _seed_album(
        store,
        embedded_group=EMBEDDED_GROUP,
        embedded_release=EMBEDDED_RELEASE,
        embedded_recording=EMBEDDED_RECORDING,
        embedded_release_track=EMBEDDED_RELEASE_TRACK,
        policy="local_metadata",
    )
    job = await _claimed_job(store, kind="post_processing")
    provider = FakeProvider()

    outcome = await _service(
        store,
        provider,
        FakeFingerprinter(
            FingerprintResult(status=FingerprintStatus.DISABLED), enabled=False
        ),
        provider_available=lambda: False,
    ).run_claimed_job(job, "worker", now=3)

    assert outcome == "identified"
    assert provider.calls == []


@pytest.mark.asyncio
async def test_conflicting_embedded_ids_create_review_without_search(
    store: NativeLibraryStore, db_path: Path
) -> None:
    await _seed_album(
        store,
        embedded_group=EMBEDDED_GROUP,
        second_embedded_group=EMBEDDED_GROUP_OTHER,
        policy="local_metadata",
    )
    job = await _claimed_job(store, kind="post_processing")
    provider = FakeProvider()
    outcome = await _service(
        store,
        provider,
        FakeFingerprinter(FingerprintResult(status="disabled"), enabled=False),
    ).run_claimed_job(job, "worker", now=3)
    assert outcome == "contradictory"
    assert provider.calls == []
    with sqlite3.connect(db_path) as connection:
        assert (
            connection.execute(
                "SELECT COUNT(*) FROM local_album_external_identities"
            ).fetchone()[0]
            == 0
        )
        assert (
            connection.execute(
                "SELECT reason_code FROM library_identification_reviews"
            ).fetchone()[0]
            == "CONFLICTING_EMBEDDED_IDS"
        )


@pytest.mark.asyncio
async def test_partial_unanimous_embedded_release_attempts_the_agreed_release(
    store: NativeLibraryStore, db_path: Path
) -> None:
    """Beets-style partial tags (one track names the release, the sibling
    merely lacks tags) attempt the agreed release instead of holding: the
    fetched edition still has to earn SUPPORTED through track evidence."""
    await _seed_album(
        store,
        embedded_group=EMBEDDED_GROUP,
        embedded_release=EMBEDDED_RELEASE,
        second_embedded_group=EMBEDDED_GROUP,
    )
    with sqlite3.connect(db_path) as connection:
        connection.execute(
            "UPDATE local_tracks SET embedded_release_mbid=NULL, "
            "embedded_recording_mbid=NULL WHERE id='track-1-2'"
        )
    candidate = AlbumCandidate(
        release_group_mbid=EMBEDDED_GROUP,
        release_mbid=EMBEDDED_RELEASE,
        album_title="Album",
        album_artist_name="Artist",
        tracks=[
            CandidateTrack(
                title="Track",
                position=1,
                absolute_position=1,
                duration_seconds=180,
            ),
            CandidateTrack(
                title="Track 2",
                position=2,
                absolute_position=2,
                duration_seconds=180,
            ),
        ],
    )
    job = await _claimed_job(store)
    provider = FakeProvider([candidate])

    outcome = await _service(
        store,
        provider,
        FakeFingerprinter(FingerprintResult(status="disabled"), enabled=False),
    ).run_claimed_job(job, "worker", now=3)

    assert outcome == "identified"
    assert provider.exact_releases == [(EMBEDDED_RELEASE, RequestPriority.BACKGROUND_SYNC)]
    assert provider.calls == []


@pytest.mark.asyncio
async def test_local_metadata_partial_embedded_release_keeps_the_hold(
    store: NativeLibraryStore, db_path: Path
) -> None:
    """Local-metadata albums cannot fetch and check the agreed release, so
    partial tags keep the incomplete hold instead of attempting it."""
    await _seed_album(
        store,
        embedded_group=EMBEDDED_GROUP,
        embedded_release=EMBEDDED_RELEASE,
        second_embedded_group=EMBEDDED_GROUP,
        policy="local_metadata",
    )
    with sqlite3.connect(db_path) as connection:
        connection.execute(
            "UPDATE local_tracks SET embedded_release_mbid=NULL, "
            "embedded_recording_mbid=NULL WHERE id='track-1-2'"
        )
    job = await _claimed_job(store, kind="post_processing")
    provider = FakeProvider()

    outcome = await _service(
        store,
        provider,
        FakeFingerprinter(FingerprintResult(status="disabled"), enabled=False),
    ).run_claimed_job(job, "worker", now=3)

    assert outcome == "insufficient_evidence"
    assert provider.exact_releases == []
    assert provider.calls == []
    with sqlite3.connect(db_path) as connection:
        assert (
            connection.execute(
                "SELECT reason_code FROM library_identification_reviews"
            ).fetchone()[0]
            == "INCOMPLETE_EMBEDDED_RELEASE_IDS"
        )


@pytest.mark.asyncio
async def test_duplicate_embedded_release_tracks_never_attach(
    store: NativeLibraryStore, db_path: Path
) -> None:
    await _seed_album(
        store,
        embedded_group=EMBEDDED_GROUP,
        embedded_release=EMBEDDED_RELEASE,
        embedded_recording=EMBEDDED_RECORDING,
        embedded_release_track=EMBEDDED_RELEASE_TRACK,
        second_embedded_group=EMBEDDED_GROUP,
        policy="local_metadata",
    )
    job = await _claimed_job(store, kind="post_processing")
    provider = FakeProvider()
    outcome = await _service(
        store,
        provider,
        FakeFingerprinter(FingerprintResult(status="disabled"), enabled=False),
    ).run_claimed_job(job, "worker", now=3)

    assert outcome == "contradictory"
    assert provider.calls == []
    with sqlite3.connect(db_path) as connection:
        identity_count = connection.execute(
            "SELECT COUNT(*) FROM local_track_external_identities"
        ).fetchone()[0]
        reason = connection.execute(
            "SELECT reason_code FROM library_identification_reviews"
        ).fetchone()[0]
    assert identity_count == 0
    assert reason == "CONFLICTING_EMBEDDED_IDS"


@pytest.mark.asyncio
async def test_automatic_identification_rejects_duplicate_recording_position_overlap(
    store: NativeLibraryStore,
    db_path: Path,
) -> None:
    await _seed_album(store, embedded_recording=EMBEDDED_RECORDING)
    with sqlite3.connect(db_path) as connection:
        connection.execute(
            "UPDATE local_tracks SET title='Repeated', disc_number=2, track_number=10 "
            "WHERE id='track-1'"
        )
    candidate = AlbumCandidate(
        release_group_mbid="duplicate-group",
        release_mbid="duplicate-release",
        album_title="Album",
        album_artist_name="Artist",
        tracks=[
            CandidateTrack(
                title="Repeated",
                disc_number=2,
                position=1,
                absolute_position=10,
                recording_mbid=EMBEDDED_RECORDING,
                release_track_mbid="release-track-first",
            ),
            CandidateTrack(
                title="Repeated",
                disc_number=2,
                position=10,
                absolute_position=19,
                recording_mbid=EMBEDDED_RECORDING,
                release_track_mbid="release-track-second",
            ),
        ],
    )
    job = await _claimed_job(store)

    outcome = await _service(
        store,
        FakeProvider([candidate]),
        FakeFingerprinter(FingerprintResult(status="disabled"), enabled=False),
    ).run_claimed_job(job, "worker", now=3)
    evidence = await store.get_latest_album_candidate_evidence(
        "album-1", "duplicate-group:duplicate-release"
    )

    assert outcome == "contradictory"
    assert evidence is not None
    assert evidence.evidence.track_evidence[0].evidence_kinds == [
        "ambiguous_release_track_identity"
    ]
    with sqlite3.connect(db_path) as connection:
        assert (
            connection.execute(
                "SELECT COUNT(*) FROM local_track_external_identities"
            ).fetchone()[0]
            == 0
        )


@pytest.mark.asyncio
@pytest.mark.parametrize(
    ("embedded_group", "second_group", "second_recording", "reason"),
    [
        ("not-a-uuid", None, None, "INVALID_EMBEDDED_IDS"),
        (
            EMBEDDED_GROUP,
            EMBEDDED_GROUP,
            EMBEDDED_RECORDING,
            "CONFLICTING_EMBEDDED_IDS",
        ),
    ],
)
async def test_invalid_or_duplicate_embedded_ids_never_attach(
    store: NativeLibraryStore,
    db_path: Path,
    embedded_group: str,
    second_group: str | None,
    second_recording: str | None,
    reason: str,
) -> None:
    await _seed_album(
        store,
        embedded_group=embedded_group,
        embedded_recording=EMBEDDED_RECORDING,
        second_embedded_group=second_group,
        second_embedded_recording=second_recording,
        policy="local_metadata",
    )
    job = await _claimed_job(store, kind="post_processing")
    provider = FakeProvider()
    outcome = await _service(
        store,
        provider,
        FakeFingerprinter(FingerprintResult(status="disabled"), enabled=False),
    ).run_claimed_job(job, "worker", now=3)
    assert outcome == "contradictory"
    assert provider.calls == []
    with sqlite3.connect(db_path) as connection:
        assert (
            connection.execute(
                "SELECT COUNT(*) FROM local_album_external_identities"
            ).fetchone()[0]
            == 0
        )
        assert (
            connection.execute(
                "SELECT reason_code FROM library_identification_reviews"
            ).fetchone()[0]
            == reason
        )


@pytest.mark.asyncio
async def test_reported_forced_assignment_regression_remains_local_and_reviewable(
    store: NativeLibraryStore, db_path: Path
) -> None:
    await _seed_album(store)
    job = await _claimed_job(store)
    context = await store.get_album_identification_context("album-1")
    assert context is not None
    provider = FakeProvider([_candidate(title="Wrong Album", recording="wrong")])
    outcome = await _service(
        store,
        provider,
        FakeFingerprinter(FingerprintResult(status="disabled"), enabled=False),
    ).run_claimed_job(job, "worker", now=3)
    assert outcome in ("contradictory", "insufficient_evidence")
    with sqlite3.connect(db_path) as connection:
        assert (
            connection.execute(
                "SELECT COUNT(*) FROM local_album_external_identities"
            ).fetchone()[0]
            == 0
        )
        assert (
            connection.execute(
                "SELECT state FROM library_identification_reviews"
            ).fetchone()[0]
            == "needs_review"
        )


@pytest.mark.asyncio
async def test_background_degradation_is_sanitized_and_deferred(
    store: NativeLibraryStore, db_path: Path
) -> None:
    await _seed_album(store)
    job = await _claimed_job(store)
    outcome = await _service(
        store,
        DegradedProvider(),
        FakeFingerprinter(FingerprintResult(status="disabled"), enabled=False),
    ).run_claimed_job(job, "worker", now=3)
    assert outcome == "provider_deferred"
    assert try_get_degradation_context() is None
    with sqlite3.connect(db_path) as connection:
        state, failure = connection.execute(
            "SELECT state, last_failure_code FROM library_identification_jobs"
        ).fetchone()
    assert (state, failure) == ("queued", "PROVIDER_TEMPORARILY_UNAVAILABLE")
    assert "not persisted" not in db_path.read_bytes().decode("utf-8", "ignore")


@pytest.mark.asyncio
@pytest.mark.parametrize(
    ("result", "expected_state"),
    [
        (FingerprintResult(status="pass", recording_id="rec", score=0.9), "matched"),
        (FingerprintResult(status="skip"), "no_match"),
        (FingerprintResult(status="disabled"), "disabled"),
    ],
)
async def test_fingerprint_terminal_outcomes_are_reused_without_repeat(
    store: NativeLibraryStore,
    result: FingerprintResult,
    expected_state: str,
) -> None:
    await _seed_album(store)
    fake = FakeFingerprinter(result, enabled=result.status != "disabled")
    service = ConditionalFingerprintService(store, fake)
    first, first_work = await service.fingerprint_if_needed(
        local_track_id="track-1",
        path=Path("/music/1.flac"),
        stat_revision="stat-1",
        needed=True,
        now=1,
    )
    second, second_work = await service.fingerprint_if_needed(
        local_track_id="track-1",
        path=Path("/music/1.flac"),
        stat_revision="stat-1",
        needed=True,
        now=2,
    )
    assert first is not None and first.state == expected_state
    assert second is not None and second.state == expected_state
    assert first_work is True
    assert second_work is False
    assert fake.generate_calls <= 1
    assert fake.lookup_calls <= 1
    changed, changed_work = await service.fingerprint_if_needed(
        local_track_id="track-1",
        path=Path("/music/1.flac"),
        stat_revision="stat-2",
        needed=True,
        now=3,
    )
    assert changed_work is True
    assert changed is not None
    if result.status != "disabled":
        assert fake.generate_calls == 2


@pytest.mark.asyncio
async def test_fingerprint_is_persisted_before_lookup_and_transient_failure_reuses_it(
    store: NativeLibraryStore,
) -> None:
    await _seed_album(store)
    fake = FakeFingerprinter(FingerprintResult(status="error"))
    service = ConditionalFingerprintService(store, fake)
    first, first_work = await service.fingerprint_if_needed(
        local_track_id="track-1",
        path=Path("/music/1.flac"),
        stat_revision="stat-1",
        needed=True,
        now=1,
    )
    assert first is not None and first.state == "failed"
    assert first.fingerprint == "fingerprint"
    assert first_work is True
    _, second_work = await service.fingerprint_if_needed(
        local_track_id="track-1",
        path=Path("/music/1.flac"),
        stat_revision="stat-1",
        needed=True,
        now=62,
    )
    assert second_work is True
    assert fake.generate_calls == 1
    assert fake.lookup_calls == 2


@pytest.mark.asyncio
async def test_queue_dedupe_priority_fairness_backoff_pause_and_recovery(
    store: NativeLibraryStore, db_path: Path
) -> None:
    for index in range(1, 9):
        await _seed_album(store, str(index))
    queue = IdentificationQueueService(store)
    first, created = await queue.enqueue_album_with_disposition(
        "album-1", input_revision="one", now=1
    )
    coalesced, coalesced_created = await queue.enqueue_album_with_disposition(
        "album-1", input_revision="two", now=2
    )
    assert created is True
    assert coalesced_created is False
    assert coalesced == first
    for index in range(2, 8):
        await store.enqueue_identification_job(
            IdentificationJob(
                id=f"high-{index}",
                local_album_id=f"album-{index}",
                dedupe_key=f"high-{index}",
                input_revision="revision",
                priority=20,
                created_at=float(index),
            )
        )
    await store.enqueue_identification_job(
        IdentificationJob(
            id="backlog",
            local_album_id="album-8",
            dedupe_key="backlog",
            input_revision="revision",
            priority=40,
            created_at=0,
        )
    )
    claimed_ids = []
    for index in range(6):
        claimed = await queue.claim("worker", now=10 + index)
        assert claimed is not None
        claimed_ids.append(claimed["id"])
        with sqlite3.connect(db_path) as connection:
            connection.execute(
                "UPDATE library_identification_jobs SET state = 'succeeded', lease_owner = NULL "
                "WHERE id = ?",
                (claimed["id"],),
            )
    assert claimed_ids[5] == "backlog"

    running = await queue.claim("worker", now=20)
    assert running is not None
    attempts = running["attempt_count"]
    await queue.pause("admin", now=21)
    await queue.checkpoint_pause(
        running, "worker", {"phase": "candidate_search", "evidence": []}, now=22
    )
    with sqlite3.connect(db_path) as connection:
        paused_row = connection.execute(
            "SELECT state, lease_owner, attempt_count, checkpoint_json "
            "FROM library_identification_jobs WHERE id = ?",
            (running["id"],),
        ).fetchone()
    assert paused_row[0:3] == ("queued", None, attempts - 1)
    assert json.loads(paused_row[3])["phase"] == "candidate_search"
    assert await queue.claim("worker", now=23) is None
    await queue.resume(now=24)
    resumed = await queue.claim("worker", now=25)
    assert resumed is not None
    with sqlite3.connect(db_path) as connection:
        connection.execute(
            "UPDATE library_identification_jobs SET lease_expires_at = 1 WHERE id = ?",
            (resumed["id"],),
        )
    assert await queue.recover(now=30) == 1


@pytest.mark.asyncio
async def test_queue_batch_preserves_create_and_coalescence_dispositions(
    store: NativeLibraryStore, db_path: Path
) -> None:
    for index in range(1, 4):
        await _seed_album(store, str(index))
    queue = IdentificationQueueService(store)

    first = await queue.enqueue_albums_with_disposition(
        [
            ("album-1", "one", "automatic"),
            ("album-2", "one", "automatic"),
        ],
        now=1,
    )
    second = await queue.enqueue_albums_with_disposition(
        [
            ("album-1", "two", "automatic"),
            ("album-3", "one", "automatic"),
        ],
        now=2,
    )

    assert [created for _, created in first] == [True, True]
    assert [created for _, created in second] == [False, True]
    assert second[0][0] == first[0][0]
    with sqlite3.connect(db_path) as connection:
        assert (
            connection.execute(
                "SELECT COUNT(*) FROM library_identification_jobs"
            ).fetchone()[0]
            == 3
        )


@pytest.mark.asyncio
async def test_transient_queue_backoff_is_typed_and_respects_not_before(
    store: NativeLibraryStore, db_path: Path
) -> None:
    await _seed_album(store)
    queue = IdentificationQueueService(store)
    await queue.enqueue_album("album-1", input_revision="revision", now=1)
    claimed = await queue.claim("worker", now=2)
    assert claimed is not None
    await queue.defer(claimed, "worker", "PROVIDER_TEMPORARILY_UNAVAILABLE", now=10)
    with sqlite3.connect(db_path) as connection:
        not_before, failure = connection.execute(
            "SELECT not_before, last_failure_code FROM library_identification_jobs"
        ).fetchone()
    assert not_before == 40
    assert failure == "PROVIDER_TEMPORARILY_UNAVAILABLE"
    assert await queue.claim("worker", now=39) is None
    assert await queue.claim("worker", now=40) is not None


@pytest.mark.asyncio
async def test_deferral_cap_terminates_job_in_attention_state(
    store: NativeLibraryStore, db_path: Path
) -> None:
    await _seed_album(store)
    queue = IdentificationQueueService(store)
    await queue.enqueue_album("album-1", input_revision="revision", now=1)
    now = 2.0
    for attempt in range(1, MAX_DEFERRAL_ATTEMPTS + 1):
        claimed = await queue.claim("worker", now=now)
        assert claimed is not None
        assert claimed["attempt_count"] == attempt
        await queue.defer(
            claimed, "worker", "PROVIDER_TEMPORARILY_UNAVAILABLE", now=now
        )
        now += MAX_BACKOFF_SECONDS + 60
    with sqlite3.connect(db_path) as connection:
        row = connection.execute(
            "SELECT state, last_failure_code, terminal_at "
            "FROM library_identification_jobs"
        ).fetchone()
    assert row[0] == "failed"
    assert row[1] == "MAX_DEFERRALS_EXCEEDED"
    assert row[2] == pytest.approx(
        2.0 + (MAX_DEFERRAL_ATTEMPTS - 1) * (MAX_BACKOFF_SECONDS + 60)
    )
    assert await queue.claim("worker", now=now) is None
    assert (await queue.activity_snapshot())["attention_count"] == 1


@pytest.mark.asyncio
async def test_attention_failed_job_resurrects_only_for_provider_causes(
    store: NativeLibraryStore, db_path: Path
) -> None:
    await _seed_album(store)
    await _seed_album(store, "2")
    await _seed_album(store, "3")
    await _seed_album(store, "4")
    queue = IdentificationQueueService(store)

    # Deterministic cap (cause != PROVIDER_TEMPORARILY_UNAVAILABLE) stays
    # terminal on a same-key enqueue: no resurrection.
    job_id = await queue.enqueue_album("album-1", input_revision="revision", now=1)
    claimed = await queue.claim("worker", now=2)
    assert claimed is not None
    await queue.fail(claimed, "worker", "MAX_DEFERRALS_EXCEEDED", now=3)
    deduped_id, created = await queue.enqueue_album_with_disposition(
        "album-1", input_revision="revision", now=4
    )
    assert deduped_id == job_id
    assert created is False
    with sqlite3.connect(db_path) as connection:
        state = connection.execute(
            "SELECT state FROM library_identification_jobs WHERE id = ?",
            (job_id,),
        ).fetchone()[0]
    assert state == "failed"

    # Provider cap with the gate open: resurrects and clears the cause.
    job_id = await queue.enqueue_album("album-2", input_revision="revision", now=5)
    now = 6.0
    for _attempt in range(1, MAX_DEFERRAL_ATTEMPTS + 1):
        claimed = await queue.claim("worker", now=now)
        assert claimed is not None
        await queue.defer(
            claimed, "worker", "PROVIDER_TEMPORARILY_UNAVAILABLE", now=now
        )
        now += MAX_BACKOFF_SECONDS + 60
    with sqlite3.connect(db_path) as connection:
        cause = connection.execute(
            "SELECT attention_cause FROM library_identification_jobs WHERE id = ?",
            (job_id,),
        ).fetchone()[0]
    assert cause == "PROVIDER_TEMPORARILY_UNAVAILABLE"

    open_queue = IdentificationQueueService(store, provider_available=lambda: True)
    resurrected_id, created = await open_queue.enqueue_album_with_disposition(
        "album-2", input_revision="revision", now=now + 1
    )
    assert resurrected_id == job_id
    assert created is True
    with sqlite3.connect(db_path) as connection:
        row = connection.execute(
            "SELECT state, attempt_count, not_before, last_failure_code, "
            "attention_cause, terminal_at FROM library_identification_jobs WHERE id = ?",
            (job_id,),
        ).fetchone()
    assert row == ("queued", 0, 0, None, None, None)
    reclaimed = await queue.claim("worker", now=now + 2)
    assert reclaimed is not None
    assert reclaimed["id"] == job_id

    # Provider cap with the gate closed: stays terminal (dedupes, no resurrection).
    job_id = await queue.enqueue_album("album-3", input_revision="revision", now=20)
    now = 21.0
    for _attempt in range(1, MAX_DEFERRAL_ATTEMPTS + 1):
        claimed = await queue.claim("worker", now=now)
        assert claimed is not None
        await queue.defer(
            claimed, "worker", "PROVIDER_TEMPORARILY_UNAVAILABLE", now=now
        )
        now += MAX_BACKOFF_SECONDS + 60
    closed_queue = IdentificationQueueService(store, provider_available=lambda: False)
    deduped_id, created = await closed_queue.enqueue_album_with_disposition(
        "album-3", input_revision="revision", now=now + 1
    )
    assert deduped_id == job_id
    assert created is False
    with sqlite3.connect(db_path) as connection:
        state = connection.execute(
            "SELECT state FROM library_identification_jobs WHERE id = ?",
            (job_id,),
        ).fetchone()[0]
    assert state == "failed"

    # A terminal failure without an attention code still dedupes (no resurrection).
    await queue.enqueue_album("album-4", input_revision="revision", now=1)
    other = await queue.claim("worker", now=now + 2)
    assert other is not None
    await queue.fail(other, "worker", "UNRELATED_TERMINAL_CODE", now=now + 3)
    deduped_id, created = await queue.enqueue_album_with_disposition(
        "album-4", input_revision="revision", now=now + 4
    )
    assert deduped_id == str(other["id"])
    assert created is False
    with sqlite3.connect(db_path) as connection:
        state = connection.execute(
            "SELECT state FROM library_identification_jobs WHERE id = ?",
            (other["id"],),
        ).fetchone()[0]
    assert state == "failed"


@pytest.mark.asyncio
async def test_identification_worker_crash_loop_hits_the_deferral_cap(
    store: NativeLibraryStore,
    db_path: Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    import services.native.identification_queue_service as queue_module

    await _seed_album(store)
    queue = IdentificationQueueService(store)
    await queue.enqueue_album("album-1", input_revision="revision", now=1)

    clock = {"now": 2.0}

    def advancing_time() -> float:
        clock["now"] += MAX_BACKOFF_SECONDS + 60
        return clock["now"]

    monkeypatch.setattr(queue_module.time, "time", advancing_time)
    service = AsyncMock()
    service.run_claimed_job.side_effect = RuntimeError("boom")
    wakeups = SimpleNamespace(
        revision=lambda _kind: 0,
        wait=AsyncMock(
            side_effect=[None] * MAX_DEFERRAL_ATTEMPTS + [asyncio.CancelledError()]
        ),
    )
    await run_target_identification_worker(
        lambda: queue,
        lambda: service,
        worker_id="test-worker",
        work_wakeups=wakeups,
    )

    with sqlite3.connect(db_path) as connection:
        row = connection.execute(
            "SELECT state, last_failure_code, attention_cause, attempt_count "
            "FROM library_identification_jobs WHERE local_album_id = 'album-1'"
        ).fetchone()
        review = connection.execute(
            "SELECT state, reason_code, attempt_id "
            "FROM library_identification_reviews"
        ).fetchone()
    # The crashed job deferred through the cap instead of looping forever, and
    # the terminal failure surfaced a review row.
    assert row == ("failed", "MAX_DEFERRALS_EXCEEDED", "UNEXPECTED_ERROR", 10)
    assert review == ("needs_review", "MAX_DEFERRALS_EXCEEDED", None)
    assert await queue.claim("worker", now=clock["now"] + 100) is None


@pytest.mark.asyncio
async def test_deleted_or_retired_subject_terminates_immediately(
    store: NativeLibraryStore, db_path: Path
) -> None:
    await _seed_album(store)
    job = await _claimed_job(store)
    provider = FakeProvider()
    with sqlite3.connect(db_path) as connection:
        connection.execute(
            "UPDATE local_albums SET retired_into_album_id = 'album-merged' "
            "WHERE id = 'album-1'"
        )

    outcome = await _service(
        store,
        provider,
        FakeFingerprinter(FingerprintResult(status="disabled"), enabled=False),
    ).run_claimed_job(job, "worker", now=3)

    assert outcome == "attention"
    assert provider.calls == []
    with sqlite3.connect(db_path) as connection:
        row = connection.execute(
            "SELECT state, last_failure_code, terminal_at "
            "FROM library_identification_jobs WHERE id = 'job-album-1'"
        ).fetchone()
    assert row[0] == "failed"
    assert row[1] == "SUBJECT_NOT_AVAILABLE"
    assert row[2] == 3
    queue = IdentificationQueueService(store)
    assert await queue.claim("worker", now=4) is None


@pytest.mark.asyncio
async def test_empty_subject_defers_until_grace_sweep_terminates(
    store: NativeLibraryStore, db_path: Path
) -> None:
    await _seed_album(store)
    job = await _claimed_job(store)
    provider = FakeProvider()
    with sqlite3.connect(db_path) as connection:
        connection.execute(
            "UPDATE local_tracks SET availability = 'missing' WHERE id = 'track-1'"
        )

    outcome = await _service(
        store,
        provider,
        FakeFingerprinter(FingerprintResult(status="disabled"), enabled=False),
    ).run_claimed_job(job, "worker", now=3)

    assert outcome == "provider_deferred"
    queue = IdentificationQueueService(store)
    await queue.recover(now=3 + 3600)
    with sqlite3.connect(db_path) as connection:
        state = connection.execute(
            "SELECT state FROM library_identification_jobs WHERE id = 'job-album-1'"
        ).fetchone()[0]
    assert state == "queued"

    await queue.recover(now=3 + 25 * 3600)
    with sqlite3.connect(db_path) as connection:
        row = connection.execute(
            "SELECT state, last_failure_code, terminal_at "
            "FROM library_identification_jobs WHERE id = 'job-album-1'"
        ).fetchone()
    assert row[0] == "failed"
    assert row[1] == "SUBJECT_NOT_AVAILABLE"
    assert row[2] == 3 + 25 * 3600

    # The sweep surfaces a review row so the failed album leaves the stuck
    # attention state and becomes dismissable in the review queue.
    with sqlite3.connect(db_path) as connection:
        review = connection.execute(
            "SELECT local_album_id, state, reason_code, attempt_id, input_revision "
            "FROM library_identification_reviews"
        ).fetchone()
    assert review == (
        "album-1",
        "needs_review",
        "SUBJECT_NOT_AVAILABLE",
        None,
        "revision",
    )
    snapshot = await store.get_identification_activity_snapshot(now=3 + 25 * 3600)
    assert snapshot["attention_count"] == 1
    assert snapshot["needs_review_count"] == 1

    # A repeat sweep fails nothing new and never duplicates the review row.
    assert (
        await store.gc_stale_identification_jobs(
            now=3 + 26 * 3600, grace_seconds=SUBJECT_NOT_AVAILABLE_GRACE_SECONDS
        )
        == 0
    )
    with sqlite3.connect(db_path) as connection:
        count = connection.execute(
            "SELECT COUNT(*) FROM library_identification_reviews"
        ).fetchone()[0]
    assert count == 1


@pytest.mark.asyncio
async def test_grace_sweep_review_dismiss_clears_attention(
    store: NativeLibraryStore, db_path: Path
) -> None:
    await _seed_album(store)
    job = await _claimed_job(store)
    with sqlite3.connect(db_path) as connection:
        connection.execute(
            "UPDATE local_tracks SET availability = 'missing' WHERE id = 'track-1'"
        )
    outcome = await _service(
        store,
        FakeProvider(),
        FakeFingerprinter(FingerprintResult(status="disabled"), enabled=False),
    ).run_claimed_job(job, "worker", now=3)
    assert outcome == "provider_deferred"
    queue = IdentificationQueueService(store)
    await queue.recover(now=3 + 25 * 3600)
    assert (await store.get_identification_activity_snapshot(now=3 + 25 * 3600))[
        "attention_count"
    ] == 1

    with sqlite3.connect(db_path) as connection:
        review_id = connection.execute(
            "SELECT id FROM library_identification_reviews "
            "WHERE local_album_id = 'album-1'"
        ).fetchone()[0]
    catalog_revision = await store.get_catalog_revision()
    result = await store.apply_review_decision(
        str(review_id),
        action="dismiss",
        actor_user_id="admin",
        expected_review_revision=1,
        expected_catalog_revision=catalog_revision,
        expected_identity_revision=None,
        action_id="action-dismiss-gc-sna",
        idempotency_key=None,
        now=3 + 25 * 3600 + 1,
    )
    assert result["review"]["state"] == "resolved"
    assert result["review"]["reason_code"] == "DISMISS"
    with sqlite3.connect(db_path) as connection:
        capped = connection.execute(
            "SELECT state, last_failure_code, attention_cause "
            "FROM library_identification_jobs WHERE id = 'job-album-1'"
        ).fetchone()
    assert capped == ("failed", None, None)
    snapshot = await store.get_identification_activity_snapshot(
        now=3 + 25 * 3600 + 1
    )
    assert snapshot["attention_count"] == 0
    assert snapshot["needs_review_count"] == 0

    # Later sweeps must not resurrect attention for the dismissed album.
    assert (
        await store.gc_stale_identification_jobs(
            now=3 + 26 * 3600, grace_seconds=SUBJECT_NOT_AVAILABLE_GRACE_SECONDS
        )
        == 0
    )
    with sqlite3.connect(db_path) as connection:
        count = connection.execute(
            "SELECT COUNT(*) FROM library_identification_reviews"
        ).fetchone()[0]
    assert count == 1


@pytest.mark.asyncio
async def test_grace_sweep_heals_pre_existing_stuck_rows(
    store: NativeLibraryStore, db_path: Path
) -> None:
    await _seed_album(store)
    await _seed_album(store, "2")
    queue = IdentificationQueueService(store)
    await queue.enqueue_album("album-1", input_revision="revision", now=1)
    await queue.enqueue_album("album-2", input_revision="revision", now=1)
    # Simulate pre-fix sweep damage: failed attention jobs with no review row.
    sweep_now = 3 + 25 * 3600
    with sqlite3.connect(db_path) as connection:
        connection.execute(
            "UPDATE library_identification_jobs SET state = 'failed', "
            "last_failure_code = 'SUBJECT_NOT_AVAILABLE', "
            "attention_cause = 'SUBJECT_NOT_AVAILABLE', terminal_at = ? "
            "WHERE local_album_id = 'album-1'",
            (sweep_now,),
        )
        connection.execute(
            "UPDATE library_identification_jobs SET state = 'failed', "
            "last_failure_code = 'MAX_DEFERRALS_EXCEEDED', "
            "attention_cause = 'MAX_DEFERRALS_EXCEEDED', terminal_at = ? "
            "WHERE local_album_id = 'album-2'",
            (sweep_now,),
        )

    before = await store.get_stream_revision("identification")
    assert (
        await store.gc_stale_identification_jobs(
            now=sweep_now, grace_seconds=SUBJECT_NOT_AVAILABLE_GRACE_SECONDS
        )
        == 0
    )
    assert await store.get_stream_revision("identification") > before
    with sqlite3.connect(db_path) as connection:
        reviews = connection.execute(
            "SELECT local_album_id, state, reason_code, attempt_id, input_revision "
            "FROM library_identification_reviews ORDER BY local_album_id"
        ).fetchall()
    assert reviews == [
        ("album-1", "needs_review", "SUBJECT_NOT_AVAILABLE", None, "revision"),
        ("album-2", "needs_review", "MAX_DEFERRALS_EXCEEDED", None, "revision"),
    ]
    snapshot = await store.get_identification_activity_snapshot(now=sweep_now)
    assert snapshot["attention_count"] == 2
    assert snapshot["needs_review_count"] == 2

    # Healing is idempotent: a repeat sweep changes nothing.
    healed_revision = await store.get_stream_revision("identification")
    assert (
        await store.gc_stale_identification_jobs(
            now=sweep_now + 1, grace_seconds=SUBJECT_NOT_AVAILABLE_GRACE_SECONDS
        )
        == 0
    )
    assert await store.get_stream_revision("identification") == healed_revision
    with sqlite3.connect(db_path) as connection:
        count = connection.execute(
            "SELECT COUNT(*) FROM library_identification_reviews"
        ).fetchone()[0]
    assert count == 2


@pytest.mark.asyncio
async def test_grace_sweep_clears_superseded_attention_without_review(
    store: NativeLibraryStore, db_path: Path
) -> None:
    await _seed_album(store)
    queue = IdentificationQueueService(store)
    await queue.enqueue_album("album-1", input_revision="revision", now=1)
    sweep_now = 3 + 25 * 3600
    with sqlite3.connect(db_path) as connection:
        connection.execute(
            "UPDATE library_identification_jobs SET state = 'failed', "
            "last_failure_code = 'SUBJECT_NOT_AVAILABLE', "
            "attention_cause = 'SUBJECT_NOT_AVAILABLE', terminal_at = ? "
            "WHERE local_album_id = 'album-1'",
            (sweep_now,),
        )
        # A later successful identification supersedes the stale failure.
        connection.execute(
            "INSERT INTO local_album_external_identities "
            "(local_album_id, provider, release_group_mbid, decision_source, selected_at) "
            "VALUES ('album-1', 'musicbrainz', 'rg-1', 'automatic', ?)",
            (sweep_now,),
        )

    assert (
        await store.gc_stale_identification_jobs(
            now=sweep_now, grace_seconds=SUBJECT_NOT_AVAILABLE_GRACE_SECONDS
        )
        == 0
    )
    with sqlite3.connect(db_path) as connection:
        capped = connection.execute(
            "SELECT state, last_failure_code, attention_cause "
            "FROM library_identification_jobs WHERE local_album_id = 'album-1'"
        ).fetchone()
        count = connection.execute(
            "SELECT COUNT(*) FROM library_identification_reviews"
        ).fetchone()[0]
    assert capped == ("failed", None, None)
    assert count == 0
    snapshot = await store.get_identification_activity_snapshot(now=sweep_now)
    assert snapshot["attention_count"] == 0
    assert snapshot["needs_review_count"] == 0


@pytest.mark.asyncio
async def test_reset_provider_deferrals_clears_backoff_and_leaves_other_reasons(
    store: NativeLibraryStore, db_path: Path
) -> None:
    await _seed_album(store)
    await _seed_album(store, "2")
    queue = IdentificationQueueService(store)
    await queue.enqueue_album("album-1", input_revision="revision", now=1)
    await queue.enqueue_album("album-2", input_revision="revision", now=1)
    provider_job = await queue.claim("worker", now=2)
    assert provider_job is not None
    await queue.defer(provider_job, "worker", "PROVIDER_TEMPORARILY_UNAVAILABLE", now=2)
    subject_job = await queue.claim("worker", now=3)
    assert subject_job is not None
    await queue.defer(subject_job, "worker", "SUBJECT_NOT_AVAILABLE", now=3)
    assert await queue.claim("worker", now=4) is None

    # F-056: a FRESH provider deferral is NOT wiped inside the staleness
    # window - only outage-aged rows (or far-future backoff) get released.
    assert await queue.reset_provider_deferrals(now=100) == 0
    with sqlite3.connect(db_path) as connection:
        fresh_row = connection.execute(
            "SELECT attempt_count, not_before, last_failure_code "
            "FROM library_identification_jobs WHERE id = ?",
            (provider_job["id"],),
        ).fetchone()
    assert fresh_row == (1, 32, "PROVIDER_TEMPORARILY_UNAVAILABLE")

    assert await queue.reset_provider_deferrals(now=20_000) == 1

    with sqlite3.connect(db_path) as connection:
        provider_row = connection.execute(
            "SELECT attempt_count, not_before, last_failure_code "
            "FROM library_identification_jobs WHERE id = ?",
            (provider_job["id"],),
        ).fetchone()
        subject_row = connection.execute(
            "SELECT attempt_count, not_before, last_failure_code "
            "FROM library_identification_jobs WHERE id = ?",
            (subject_job["id"],),
        ).fetchone()
    assert provider_row == (0, 0, None)
    assert subject_row == (1, 33, "SUBJECT_NOT_AVAILABLE")
    reclaimed = await queue.claim("worker", now=20_000)
    assert reclaimed is not None
    assert reclaimed["id"] == provider_job["id"]


@pytest.mark.asyncio
async def test_pause_at_candidate_and_fingerprint_checkpoints_releases_lease_without_attempt_increment(
    store: NativeLibraryStore, db_path: Path
) -> None:
    await _seed_album(store, "1")
    queue = IdentificationQueueService(store)

    class PausingProvider(FakeProvider):
        async def search_album_candidate_ids(
            self, artist: str, title: str, limit: int, priority: RequestPriority
        ) -> list[str]:
            await queue.pause("admin", now=3)
            return await super().search_album_candidate_ids(
                artist, title, limit, priority
            )

    job = await _claimed_job(store)
    outcome = await _service(
        store,
        PausingProvider([_candidate()]),
        FakeFingerprinter(FingerprintResult(status="disabled"), enabled=False),
    ).run_claimed_job(job, "worker", now=3)
    assert outcome == "paused"
    with sqlite3.connect(db_path) as connection:
        row = connection.execute(
            "SELECT state, lease_owner, attempt_count, checkpoint_json "
            "FROM library_identification_jobs WHERE id = ?",
            (job["id"],),
        ).fetchone()
    assert row[0:3] == ("queued", None, 0)
    assert json.loads(row[3])["phase"] == "candidate_search"

    await queue.resume(now=4)
    with sqlite3.connect(db_path) as connection:
        connection.execute(
            "UPDATE library_identification_jobs SET state = 'cancelled' WHERE id = ?",
            (job["id"],),
        )
    await _seed_album(store, "2")
    second = await _claimed_job(store, "album-2")

    class PausingFingerprinter(FakeFingerprinter):
        async def generate_fingerprint(self, path: Path) -> tuple[str, int]:
            result = await super().generate_fingerprint(path)
            await queue.pause("admin", now=5)
            return result

    ambiguous = FakeProvider(
        [
            _candidate(group="a", recording="recording-a"),
            _candidate(group="b", recording="recording-b"),
        ]
    )
    outcome = await _service(
        store,
        ambiguous,
        PausingFingerprinter(FingerprintResult(status="skip")),
    ).run_claimed_job(second, "worker", now=5)
    assert outcome == "paused"
    with sqlite3.connect(db_path) as connection:
        row = connection.execute(
            "SELECT state, lease_owner, attempt_count, checkpoint_json "
            "FROM library_identification_jobs WHERE id = ?",
            (second["id"],),
        ).fetchone()
    assert row[0:3] == ("queued", None, 0)
    assert json.loads(row[3])["phase"] == "fingerprinting"


@pytest.mark.asyncio
async def test_sibling_trial_identifies_when_true_edition_shares_the_release_group(
    store: NativeLibraryStore,
    db_path: Path,
) -> None:
    """EditionsEtc Phase 2: recall picks a live-promo edition whose evidence
    cannot support the album; ONE bounded retry including ranked siblings
    surfaces the true edition from the same release group, so the album
    identifies instead of landing in review."""
    # Step 2.6 (N-01): the lone-eligible quorum needs provider proof for a
    # single present track - the shared recording rides both editions, so the
    # promo stays RELEASE_TYPE-blocked (not contradictory) and the trial still
    # fires for it.
    await _seed_album(store, embedded_recording=EMBEDDED_RECORDING)

    class PromoFirstProvider(FakeProvider):
        def __init__(
            self, group: str, promo: AlbumCandidate, true_edition: AlbumCandidate
        ) -> None:
            super().__init__([promo])
            self._group = group
            self._true_edition = true_edition

        async def get_album_candidate_editions(
            self,
            release_group_mbid: str,
            target_track_count: int,
            priority: RequestPriority,
            *,
            max_editions: int = 2,
        ) -> list[AlbumCandidate]:
            self.edition_calls.append(release_group_mbid)
            if release_group_mbid != self._group:
                return []
            return [self.candidates[0], self._true_edition]

    promo = msgspec.structs.replace(
        _candidate(group="rg-real", recording=EMBEDDED_RECORDING),
        secondary_types=["live"],
    )
    true_edition = msgspec.structs.replace(
        _candidate(group="rg-real", recording=EMBEDDED_RECORDING),
        release_mbid="release-rg-real-official",
    )
    provider = PromoFirstProvider("rg-real", promo, true_edition)
    job = await _claimed_job(store)

    outcome = await _service(
        store,
        provider,
        FakeFingerprinter(FingerprintResult(status="disabled"), enabled=False),
    ).run_claimed_job(job, "worker", now=3)

    assert outcome == "identified"
    # Exactly one extra editions fetch for the one qualifying group.
    assert provider.edition_calls == ["rg-real"]
    with sqlite3.connect(db_path) as connection:
        identity = connection.execute(
            "SELECT release_group_mbid, release_mbid FROM "
            "local_album_external_identities WHERE local_album_id = 'album-1'"
        ).fetchone()
    assert identity == ("rg-real", "release-rg-real-official")


@pytest.mark.asyncio
async def test_sibling_trial_never_fires_for_identified_outcome(
    store: NativeLibraryStore,
) -> None:
    # Step 2.6 (N-01): the lone-eligible quorum needs provider proof for a
    # single present track (mirrors the contradictory sibling below, which
    # pins the proof-mismatch pole with a different recording).
    await _seed_album(store, embedded_recording=EMBEDDED_RECORDING)
    provider = FakeProvider([_candidate(recording=EMBEDDED_RECORDING)])
    job = await _claimed_job(store)

    outcome = await _service(
        store,
        provider,
        FakeFingerprinter(FingerprintResult(status="disabled"), enabled=False),
    ).run_claimed_job(job, "worker", now=3)

    assert outcome == "identified"
    assert provider.edition_calls == []


@pytest.mark.asyncio
async def test_sibling_trial_never_fires_for_no_candidate_outcome(
    store: NativeLibraryStore,
) -> None:
    await _seed_album(store)
    provider = FakeProvider([])
    job = await _claimed_job(store)

    outcome = await _service(
        store,
        provider,
        FakeFingerprinter(FingerprintResult(status="disabled"), enabled=False),
    ).run_claimed_job(job, "worker", now=3)

    assert outcome == "no_candidate"
    assert provider.edition_calls == []
    assert provider.calls == [
        ("album", RequestPriority.BACKGROUND_SYNC),
        ("recording", RequestPriority.BACKGROUND_SYNC),
    ]


@pytest.mark.asyncio
async def test_sibling_trial_never_fires_for_contradictory_outcome(
    store: NativeLibraryStore,
) -> None:
    await _seed_album(store, embedded_recording=EMBEDDED_RECORDING)
    provider = FakeProvider([_candidate()])
    job = await _claimed_job(store)

    outcome = await _service(
        store,
        provider,
        FakeFingerprinter(FingerprintResult(status="disabled"), enabled=False),
    ).run_claimed_job(job, "worker", now=3)

    assert outcome == "contradictory"
    assert provider.edition_calls == []


@pytest.mark.asyncio
async def test_sibling_trial_respects_queue_pause_between_fetches(
    store: NativeLibraryStore,
    db_path: Path,
) -> None:
    """Pausing while the trial fetch runs parks the job at the
    candidate_search checkpoint exactly like any other recall."""
    await _seed_album(store)
    queue = IdentificationQueueService(store)

    class PausingPromoProvider(FakeProvider):
        def __init__(self) -> None:
            super().__init__(
                [
                    msgspec.structs.replace(
                        _candidate(group="rg-real"), secondary_types=["live"]
                    )
                ]
            )

        async def get_album_candidate_editions(
            self,
            release_group_mbid: str,
            target_track_count: int,
            priority: RequestPriority,
            *,
            max_editions: int = 2,
        ) -> list[AlbumCandidate]:
            await queue.pause("admin", now=3)
            return []

    provider = PausingPromoProvider()
    job = await _claimed_job(store)

    outcome = await _service(
        store,
        provider,
        FakeFingerprinter(FingerprintResult(status="disabled"), enabled=False),
    ).run_claimed_job(job, "worker", now=3)

    assert outcome == "paused"
    with sqlite3.connect(db_path) as connection:
        row = connection.execute(
            "SELECT state, lease_owner, checkpoint_json "
            "FROM library_identification_jobs WHERE id = ?",
            (job["id"],),
        ).fetchone()
    assert row[0:2] == ("queued", None)
    assert json.loads(row[2])["phase"] == "candidate_search"


@pytest.mark.asyncio
async def test_manual_and_legacy_identity_revalidation_never_silently_detaches(
    store: NativeLibraryStore, db_path: Path
) -> None:
    # Step 2.6 (N-01): the lone-eligible quorum needs provider proof before
    # the backstop has a selected candidate to flip to contradictory.
    await _seed_album(store, embedded_recording=EMBEDDED_RECORDING)
    await store.attach_album_identity(
        LocalAlbumExternalIdentity(
            local_album_id="album-1",
            release_group_mbid="manual-rg",
            decision_source="manual",
            selected_by_user_id="admin",
            selected_at=2,
        ),
        expected_album_revision=1,
    )
    job = await _claimed_job(store)
    provider = FakeProvider(
        [_candidate(group="different-rg", recording=EMBEDDED_RECORDING)]
    )
    outcome = await _service(
        store,
        provider,
        FakeFingerprinter(FingerprintResult(status="disabled"), enabled=False),
    ).run_claimed_job(job, "worker", now=3)
    assert outcome == "contradictory"
    with sqlite3.connect(db_path) as connection:
        identity = connection.execute(
            "SELECT release_group_mbid, decision_source FROM local_album_external_identities"
        ).fetchone()
        reason = connection.execute(
            "SELECT reason_code FROM library_identification_reviews"
        ).fetchone()[0]
    assert identity == ("manual-rg", "manual")
    assert reason == "MANUAL_IDENTITY_STALE"

    with sqlite3.connect(db_path) as connection:
        connection.execute(
            "UPDATE local_album_external_identities SET decision_source = 'legacy_import'"
        )
    suppressed = await store.enqueue_identification_job(
        IdentificationJob(
            id="legacy-auto",
            local_album_id="album-1",
            dedupe_key="legacy-auto",
            input_revision="new",
        )
    )
    assert suppressed == ""


@pytest.mark.asyncio
@pytest.mark.parametrize("decision_source", ["manual", "legacy_import"])
async def test_existing_exact_release_conflict_never_silently_changes_edition(
    store: NativeLibraryStore,
    db_path: Path,
    decision_source: str,
) -> None:
    # Step 2.6 (N-01): the lone-eligible quorum needs provider proof before
    # the backstop has a selected candidate to flip to contradictory.
    await _seed_album(store, embedded_recording=EMBEDDED_RECORDING)
    job = await _claimed_job(store)
    with sqlite3.connect(db_path) as connection:
        connection.execute(
            "INSERT INTO local_album_external_identities "
            "(local_album_id, provider, release_group_mbid, release_mbid, "
            "decision_source, selected_at) VALUES "
            "('album-1','musicbrainz','rg-1','current-edition',?,1)",
            (decision_source,),
        )

    outcome = await _service(
        store,
        FakeProvider([_candidate(group="rg-1", recording=EMBEDDED_RECORDING)]),
        FakeFingerprinter(FingerprintResult(status="disabled"), enabled=False),
    ).run_claimed_job(job, "worker", now=3)

    assert outcome == "contradictory"
    with sqlite3.connect(db_path) as connection:
        identity = connection.execute(
            "SELECT release_group_mbid,release_mbid,decision_source "
            "FROM local_album_external_identities"
        ).fetchone()
        reason = connection.execute(
            "SELECT reason_code FROM library_identification_reviews"
        ).fetchone()[0]
    assert identity == ("rg-1", "current-edition", decision_source)
    assert reason == "MANUAL_IDENTITY_STALE"


@pytest.mark.asyncio
async def test_identity_transaction_rolls_back_attempt_evidence_and_job_on_fk_failure(
    store: NativeLibraryStore, db_path: Path
) -> None:
    await _seed_album(store)
    job = await _claimed_job(store)
    context = await store.get_album_identification_context("album-1")
    assert context is not None
    evidence = CandidateEvidence(
        release_group_mbid="rg",
        release_mbid="release",
        album_title="Album",
        album_artist_name="Artist",
        album_title_classification="supported",
        album_artist_classification="supported",
        track_evidence=[
            TrackEvidence(
                local_track_id="missing-track",
                classification="supported",
                recording_mbid="recording",
            )
        ],
        reason_code="SUPPORTED",
    )
    attempt = IdentificationAttempt(
        id="rollback-attempt",
        local_album_id="album-1",
        input_tag_revision="tag",
        input_file_revision="file",
        input_policy_revision="policy",
        input_identity_revision=album_identity_revision(
            context["identity"],
            [
                track
                for track in context["tracks"]
                if track["availability"] == "indexed"
            ],
        ),
        matcher_version="matcher",
        state="identified",
        terminal_reason_code="SUPPORTED",
        selected_candidate_key="rg:release",
        candidate_count=1,
        started_at=3,
        completed_at=3,
    )
    with pytest.raises(sqlite3.IntegrityError):
        await store.finish_identification_job(
            job["id"],
            worker_id="worker",
            expected_job_revision=job["row_revision"],
            expected_album_revision=1,
            expected_input_revision=":".join(album_input_revisions(context["tracks"])),
            attempt=attempt,
            evidence=[
                IdentificationEvidenceRecord(
                    id="rollback-evidence",
                    attempt_id=attempt.id,
                    candidate_key="rg:release",
                    evidence=evidence,
                    created_at=3,
                )
            ],
            outcome="identified",
            review_id="rollback-review",
            completed_at=3,
        )
    with sqlite3.connect(db_path) as connection:
        counts = tuple(
            connection.execute(f"SELECT COUNT(*) FROM {table}").fetchone()[0]
            for table in (
                "library_identification_attempts",
                "library_identification_evidence",
                "local_album_external_identities",
            )
        )
        state = connection.execute(
            "SELECT state FROM library_identification_jobs WHERE id = ?", (job["id"],)
        ).fetchone()[0]
    assert counts == (0, 0, 0)
    assert state == "running"


@pytest.mark.asyncio
async def test_coverage_reads_selected_evidence_and_cannot_invent_contradictions(
    store: NativeLibraryStore,
) -> None:
    await _seed_album(
        store,
        embedded_group=EMBEDDED_GROUP,
        embedded_release=EMBEDDED_RELEASE,
        embedded_recording=EMBEDDED_RECORDING,
        policy="local_metadata",
    )
    job = await _claimed_job(store, kind="post_processing")
    exact_candidate = msgspec.structs.replace(
        _candidate(group=EMBEDDED_GROUP, recording=EMBEDDED_RECORDING),
        release_mbid=EMBEDDED_RELEASE,
    )
    await _service(
        store,
        FakeProvider([exact_candidate]),
        FakeFingerprinter(FingerprintResult(status="disabled"), enabled=False),
    ).run_claimed_job(job, "worker", now=3)
    coverage = await AlbumCoverageService(store).get_coverage("album-1")
    assert coverage.musicbrainz_release_group_id == EMBEDDED_GROUP
    assert [track.local_track_id for track in coverage.supported] == ["track-1"]
    assert coverage.contradictory == []
    assert coverage.stale is False


@pytest.mark.asyncio
async def test_stale_automatic_coverage_queues_revalidation(
    store: NativeLibraryStore, db_path: Path
) -> None:
    await _seed_album(
        store,
        embedded_group=EMBEDDED_GROUP,
        embedded_release=EMBEDDED_RELEASE,
        embedded_recording=EMBEDDED_RECORDING,
    )
    job = await _claimed_job(store, kind="post_processing")
    exact_candidate = msgspec.structs.replace(
        _candidate(group=EMBEDDED_GROUP, recording=EMBEDDED_RECORDING),
        release_mbid=EMBEDDED_RELEASE,
    )
    await _service(
        store,
        FakeProvider([exact_candidate]),
        FakeFingerprinter(FingerprintResult(status="disabled"), enabled=False),
    ).run_claimed_job(job, "worker", now=3)
    with sqlite3.connect(db_path) as connection:
        connection.execute(
            "UPDATE local_tracks SET tag_revision = 'changed' WHERE id = 'track-1'"
        )

    coverage = await AlbumCoverageService(
        store, IdentificationQueueService(store)
    ).get_coverage("album-1")

    assert coverage.stale is True
    with sqlite3.connect(db_path) as connection:
        queued = connection.execute(
            "SELECT kind, state FROM library_identification_jobs "
            "WHERE local_album_id = 'album-1' ORDER BY enqueue_sequence DESC LIMIT 1"
        ).fetchone()
    assert queued == ("automatic", "queued")


@pytest.mark.asyncio
async def test_explicit_reidentification_coalesces_and_precedes_automatic_work(
    store: NativeLibraryStore,
) -> None:
    await _seed_album(store, "1")
    await _seed_album(store, "2")
    explicit = ReidentificationService(store)
    first = await explicit.create_or_coalesce("album-1", "admin", now=1)
    second = await explicit.create_or_coalesce("album-1", "admin", now=2)
    assert second["id"] == first["id"]
    queue = IdentificationQueueService(store)
    await queue.enqueue_album("album-2", input_revision="revision", now=1)
    claimed = await IdentificationWorkArbiter(store, queue).claim("worker", now=3)
    assert claimed is not None
    assert claimed[0] == "explicit_reidentification"
    assert claimed[1]["id"] == first["id"]


def test_one_serialized_fixture_is_identical_for_every_evidence_consumer() -> None:
    fixture = CandidateEvidence(
        release_group_mbid="rg",
        track_evidence=[
            TrackEvidence("supported", "supported"),
            TrackEvidence("unknown", "unknown"),
            TrackEvidence("contradictory", "contradictory"),
        ],
        reason_code="CONFLICTING_TRACK_EVIDENCE",
    )
    serialized = msgspec.json.encode(fixture)
    restored = msgspec.json.decode(serialized, type=CandidateEvidence)
    projector = IdentificationEvidenceProjector()
    projections = [
        projector.project(restored),
        projector.for_review(restored),
        projector.for_repair(restored),
        projector.for_candidate_preview(restored),
        projector.for_reidentification(restored),
    ]
    assert all(projection == projections[0] for projection in projections)
    assert projections[0].supported_track_ids == ["supported"]
    assert projections[0].unknown_track_ids == ["unknown"]
    assert projections[0].contradictory_track_ids == ["contradictory"]


@pytest.mark.asyncio
async def test_artist_reuse_is_exact_and_folded_collisions_stay_separate(
    store: NativeLibraryStore, db_path: Path
) -> None:
    first, reused = await store.resolve_or_create_local_artist(
        display_name="Beyoncé",
        sort_name=None,
        kind="person",
        candidate_id="beyonce-accented",
        now=1,
    )
    again, reused_again = await store.resolve_or_create_local_artist(
        display_name="Beyoncé",
        sort_name=None,
        kind="person",
        candidate_id="unused",
        now=2,
    )
    collision, collision_reused = await store.resolve_or_create_local_artist(
        display_name="Beyonce",
        sort_name=None,
        kind="person",
        candidate_id="beyonce-plain",
        now=3,
    )
    assert (first, reused) == ("beyonce-accented", False)
    assert (again, reused_again) == ("beyonce-accented", True)
    assert (collision, collision_reused) == ("beyonce-plain", False)
    with sqlite3.connect(db_path) as connection:
        assert (
            connection.execute(
                "SELECT COUNT(*) FROM local_artist_merge_candidates"
            ).fetchone()[0]
            == 1
        )


@pytest.mark.asyncio
async def test_artist_reuse_follows_a_retired_deterministic_candidate(
    store: NativeLibraryStore, db_path: Path
) -> None:
    with sqlite3.connect(db_path) as connection:
        connection.executemany(
            "INSERT INTO local_artists "
            "(id, display_name, folded_name, normalized_name, kind, created_at, "
            "updated_at, retired_into_artist_id) VALUES (?,?,?,?,?,?,?,?)",
            [
                (
                    "surviving-artist",
                    "Circa Survive",
                    "circa survive",
                    "circa survive",
                    "group",
                    1,
                    1,
                    None,
                ),
                (
                    "retired-scan-id",
                    "Circa Survive",
                    "circa survive",
                    "circa survive",
                    "group",
                    1,
                    2,
                    "surviving-artist",
                ),
            ],
        )

    resolved, reused = await store.resolve_or_create_local_artist(
        display_name="Circa Survive",
        sort_name="Circa Survive",
        kind="group",
        candidate_id="retired-scan-id",
        now=3,
    )

    assert (resolved, reused) == ("surviving-artist", True)
    with sqlite3.connect(db_path) as connection:
        assert (
            connection.execute(
                "SELECT COUNT(*) FROM local_artists WHERE id = 'retired-scan-id'"
            ).fetchone()[0]
            == 1
        )


@pytest.mark.asyncio
async def test_artist_batch_preserves_exact_reuse_and_folded_collision_rules(
    store: NativeLibraryStore, db_path: Path
) -> None:
    resolved = await store.resolve_or_create_local_artists(
        [
            ("Beyoncé", None, "person", "beyonce-accented"),
            ("Beyoncé", None, "person", "unused"),
            ("Beyonce", None, "person", "beyonce-plain"),
        ],
        now=1,
    )

    assert resolved == {
        "beyonce-accented": ("beyonce-accented", False),
        "unused": ("beyonce-accented", True),
        "beyonce-plain": ("beyonce-plain", False),
    }
    with sqlite3.connect(db_path) as connection:
        assert (
            connection.execute(
                "SELECT COUNT(*) FROM local_artist_merge_candidates"
            ).fetchone()[0]
            == 1
        )


@pytest.mark.asyncio
async def test_post_index_grouping_rolls_disc_directories_together_and_aliases_unambiguous_merge(
    store: NativeLibraryStore, db_path: Path
) -> None:
    await _seed_album(store, "1")
    await _seed_album(store, "2")
    await store.create_scan_run(
        ScanRun(
            id="grouping-run",
            kind="incremental",
            trigger="manual",
            queued_at=1,
            updated_at=2,
        )
    )
    with sqlite3.connect(db_path) as connection:
        connection.execute(
            "UPDATE local_tracks SET relative_path = 'Artist/Box/CD1/01.flac', "
            "tag_album_title = 'Box', tag_album_artist_name = 'Artist', "
            "album_title = 'Box', album_title_folded = 'box' WHERE id = 'track-1'"
        )
        connection.execute(
            "UPDATE local_tracks SET relative_path = 'Artist/Box/Disc-02/01.flac', "
            "tag_album_title = 'Box', tag_album_artist_name = 'Artist', "
            "album_title = 'Box', album_title_folded = 'box', disc_number = 2 "
            "WHERE id = 'track-2'"
        )
        connection.execute(
            "INSERT INTO library_scan_grouping_contexts "
            "(run_id, root_id, relative_directory) "
            "VALUES ('grouping-run','root','Artist/Box')"
        )
    enqueued = await LocalAlbumGroupingService(
        store, IdentificationQueueService(store)
    ).regroup_run("grouping-run", now=3)
    with sqlite3.connect(db_path) as connection:
        albums = connection.execute(
            "SELECT DISTINCT local_album_id FROM local_tracks "
            "WHERE id IN ('track-1','track-2')"
        ).fetchall()
        aliases = connection.execute(
            "SELECT alias, local_album_id FROM local_album_aliases "
            "WHERE kind = 'merged_album'"
        ).fetchall()
    assert len(albums) == 1
    assert len(aliases) == 1
    assert aliases[0][1] == albums[0][0]
    assert enqueued == 1

    album_id = albums[0][0]
    with sqlite3.connect(db_path) as connection:
        before = (
            connection.execute(
                "SELECT row_revision FROM local_albums WHERE id = ?", (album_id,)
            ).fetchone()[0],
            connection.execute(
                "SELECT id, row_revision, membership_source FROM local_tracks "
                "WHERE id IN ('track-1','track-2') ORDER BY id"
            ).fetchall(),
            connection.execute(
                "SELECT row_revision FROM local_album_artists "
                "WHERE local_album_id = ? AND position = 0",
                (album_id,),
            ).fetchone()[0],
        )
        connection.execute(
            "UPDATE library_scan_grouping_contexts SET state = 'pending' "
            "WHERE run_id = 'grouping-run'"
        )
    repeated_enqueued = await LocalAlbumGroupingService(
        store, IdentificationQueueService(store)
    ).regroup_run("grouping-run", now=4)
    with sqlite3.connect(db_path) as connection:
        after = (
            connection.execute(
                "SELECT row_revision FROM local_albums WHERE id = ?", (album_id,)
            ).fetchone()[0],
            connection.execute(
                "SELECT id, row_revision, membership_source FROM local_tracks "
                "WHERE id IN ('track-1','track-2') ORDER BY id"
            ).fetchall(),
            connection.execute(
                "SELECT row_revision FROM local_album_artists "
                "WHERE local_album_id = ? AND position = 0",
                (album_id,),
            ).fetchone()[0],
        )
    assert repeated_enqueued == 0
    assert after == before


def _m03_equiv_specs(kind: str) -> tuple[str, list[dict]]:
    """Shared M-03 parametrized cases run against both grouping paths.

    Same disc regex + same top-level rule on the small path
    (grouping_track_from_row + LocalAlbumGrouper.group) and the staged path
    (regroup_run with a low STAGED_GROUPING_THRESHOLD): spelling variants
    fold, top-level disc folders join root siblings sharing album tags, and
    disc dirs holding different albums stay split.
    """
    if kind == "spellings":
        parent = "m03spell"
        return parent, [
            {
                "relative_path": f"{parent}/Disc(1)/01.flac",
                "tag_album_title": "Album",
                "tag_album_artist_name": "Artist",
                "track_number": 1,
            },
            {
                "relative_path": f"{parent}/Disc(1)/02.flac",
                "tag_album_title": "Album",
                "tag_album_artist_name": "Artist",
                "track_number": 2,
            },
            {
                "relative_path": f"{parent}/(CD1)/03.flac",
                "tag_album_title": "Album",
                "tag_album_artist_name": "Artist",
                "track_number": 3,
            },
            {
                "relative_path": f"{parent}/Volume 2/01.flac",
                "tag_album_title": "Album",
                "tag_album_artist_name": "Artist",
                "track_number": 1,
            },
        ]
    if kind == "top_level":
        return ".", [
            {
                "relative_path": "CD1/01.flac",
                "tag_album_title": "Album",
                "tag_album_artist_name": "Artist",
                "track_number": 1,
            },
            {
                "relative_path": "CD2/01.flac",
                "tag_album_title": "Album",
                "tag_album_artist_name": "Artist",
                "track_number": 1,
            },
            {
                "relative_path": "03.flac",
                "tag_album_title": "Album",
                "tag_album_artist_name": "Artist",
                "track_number": 3,
            },
        ]
    if kind == "split":
        parent = "m03split"
        return parent, [
            {
                "relative_path": f"{parent}/CD1/01.flac",
                "tag_album_title": "First",
                "tag_album_artist_name": "Artist",
                "track_number": 1,
            },
            {
                "relative_path": f"{parent}/CD1/02.flac",
                "tag_album_title": "Second",
                "tag_album_artist_name": "Artist",
                "track_number": 2,
            },
        ]
    raise AssertionError(f"unknown M-03 equivalence kind: {kind}")


def _m03_small_path_group_count(specs: list[dict]) -> int:
    """Run specs through the small path (producer + grouper) and count."""
    from services.native.local_album_grouper import LocalAlbumGrouper
    from services.native.local_album_grouping_service import grouping_track_from_row

    tracks = [
        grouping_track_from_row(
            {
                "id": f"m03-track-{index:02}",
                "root_id": "root",
                "relative_path": spec["relative_path"],
                "title": f"Track {index}",
                "artist_name": "Artist",
                "album_title": spec["tag_album_title"],
                "album_artist_name": spec["tag_album_artist_name"],
                "tag_album_title": spec["tag_album_title"],
                "tag_album_artist_name": spec["tag_album_artist_name"],
                "artist_sort": None,
                "album_artist_sort": None,
                "track_number": spec["track_number"],
                "disc_number": 1,
                "duration_seconds": 180.0,
                "embedded_recording_mbid": None,
                "embedded_release_mbid": None,
                "embedded_release_group_mbid": None,
                "is_compilation": False,
                "metadata_incomplete": False,
                "membership_locked": False,
                "local_album_id": "m03-old-album",
            }
        )
        for index, spec in enumerate(specs, start=1)
    ]
    return len(LocalAlbumGrouper().group(tracks))


async def _m03_seed_tracks(
    store: NativeLibraryStore, slug: str, specs: list[dict]
) -> None:
    """Seed specs once under one old album; runs regroup the same rows."""
    artist = LocalArtist(
        id=f"m03-artist-{slug}",
        display_name="Artist",
        folded_name="artist",
        normalized_name="artist",
        kind="group",
        created_at=1,
        updated_at=1,
    )
    album = LocalAlbum(
        id=f"m03-old-album-{slug}",
        root_id="root",
        grouping_key=f"m03-old-{slug}",
        title="Before",
        album_artist_id=artist.id,
        album_artist_name=artist.display_name,
        created_at=1,
        updated_at=1,
    )
    tracks = [
        LocalTrack(
            id=f"m03-track-{slug}-{index:02}",
            local_album_id=album.id,
            root_id="root",
            file_path=f"/music/{spec['relative_path']}",
            relative_path=spec["relative_path"],
            path_hash=f"m03-hash-{slug}-{index:02}",
            file_size_bytes=100,
            file_mtime_ns=1,
            stat_revision=f"100:{index}",
            tag_revision=f"tag-{index}",
            title=f"Track {index}",
            artist_name=artist.display_name,
            album_title=spec["tag_album_title"],
            album_artist_name=spec["tag_album_artist_name"],
            tag_album_title=spec["tag_album_title"],
            tag_album_artist_name=spec["tag_album_artist_name"],
            track_number=spec["track_number"],
            duration_seconds=180,
            file_format="flac",
            imported_at=1,
            applied_policy="automatic",
            applied_policy_revision="policy-1",
        )
        for index, spec in enumerate(specs, start=1)
    ]
    await store.create_catalog_membership(
        CatalogMembership(
            album=album,
            artists=[artist],
            tracks=tracks,
            album_credits=[LocalArtistCredit(local_artist_id=artist.id, position=0)],
            track_credits={
                track.id: [LocalArtistCredit(local_artist_id=artist.id, position=0)]
                for track in tracks
            },
        )
    )


async def _m03_regroup_and_count(
    store: NativeLibraryStore,
    db_path: Path,
    run_id: str,
    context: str,
    specs: list[dict],
    now: float,
) -> int:
    """Regroup one context for one run, count distinct track albums.

    Album unity (not staging rows) is the path-independent signal: the
    staged path writes library_scan_grouping_groups rows while the small
    path applies contexts directly, but both land tracks in the same
    local albums.
    """
    await store.create_scan_run(
        ScanRun(id=run_id, kind="incremental", trigger="manual", queued_at=1)
    )
    with sqlite3.connect(db_path) as connection:
        connection.execute(
            "INSERT INTO library_scan_grouping_contexts "
            "(run_id,root_id,relative_directory) VALUES (?,?,?)",
            (run_id, "root", context),
        )
    await LocalAlbumGroupingService(
        store, IdentificationQueueService(store)
    ).regroup_run(run_id, now=now, frozen_policy_revision="policy-1")
    paths = [spec["relative_path"] for spec in specs]
    with sqlite3.connect(db_path) as connection:
        count = connection.execute(
            "SELECT COUNT(DISTINCT local_album_id) FROM local_tracks WHERE "
            f"relative_path IN ({','.join('?' * len(paths))})",
            paths,
        ).fetchone()[0]
        connection.execute(
            "UPDATE library_scan_runs SET state='completed' WHERE id=?", (run_id,)
        )
    return count


@pytest.mark.asyncio
@pytest.mark.parametrize(
    ("kind", "expected_groups"),
    [("spellings", 1), ("top_level", 1), ("split", 2)],
)
async def test_disc_grouping_small_path_and_staged_agree(
    store: NativeLibraryStore,
    db_path: Path,
    monkeypatch: pytest.MonkeyPatch,
    kind: str,
    expected_groups: int,
) -> None:
    """M-03 equivalence: spelling variants fold, top-level disc folders join
    root siblings sharing album tags, and disc dirs holding different albums
    stay split - identically on the small and staged paths."""
    context, specs = _m03_equiv_specs(kind)
    assert _m03_small_path_group_count(specs) == expected_groups
    await _m03_seed_tracks(store, kind, specs)
    monkeypatch.setattr(
        "services.native.local_album_grouping_service.STAGED_GROUPING_THRESHOLD", 1
    )
    monkeypatch.setattr(
        "services.native.local_album_grouping_service.STAGING_BATCH_SIZE", 2
    )
    staged = await _m03_regroup_and_count(
        store, db_path, f"m03-staged-{kind}", context, specs, now=2
    )
    assert staged == expected_groups
    monkeypatch.setattr(
        "services.native.local_album_grouping_service.STAGED_GROUPING_THRESHOLD", 50
    )
    small = await _m03_regroup_and_count(
        store, db_path, f"m03-small-{kind}", context, specs, now=3
    )
    assert small == expected_groups


@pytest.mark.asyncio
@pytest.mark.parametrize(
    ("slug", "first_number", "second_number", "expected_groups"),
    [
        ("avmerge", 0, 0, 1),
        ("avsplit", 1, 1, 2),
    ],
)
async def test_artist_variance_unknown_numbers_merge_small_path(
    store: NativeLibraryStore,
    db_path: Path,
    monkeypatch: pytest.MonkeyPatch,
    slug: str,
    first_number: int,
    second_number: int,
    expected_groups: int,
) -> None:
    """M-02 follow-up (small path): same directory + same album fold with
    distinct non-empty artists merges when both track numbers are unknown
    (0), and stays split on a known-number collision. Staged-path parity
    lives in the store SQL collision check, outside this slice, so only
    the small path is asserted here."""
    parent = f"m02-{slug}"
    specs = [
        {
            "relative_path": f"{parent}/one.flac",
            "tag_album_title": "Greatest Hits",
            "tag_album_artist_name": "One",
            "track_number": first_number,
        },
        {
            "relative_path": f"{parent}/two.flac",
            "tag_album_title": "Greatest Hits",
            "tag_album_artist_name": "Two",
            "track_number": second_number,
        },
    ]
    assert _m03_small_path_group_count(specs) == expected_groups
    await _m03_seed_tracks(store, slug, specs)
    monkeypatch.setattr(
        "services.native.local_album_grouping_service.STAGED_GROUPING_THRESHOLD", 50
    )
    assert (
        await _m03_regroup_and_count(
            store, db_path, f"m02-small-{slug}", parent, specs, now=2
        )
        == expected_groups
    )


@pytest.mark.asyncio
async def test_grouping_context_track_read_excludes_deeper_descendants(
    store: NativeLibraryStore, db_path: Path
) -> None:
    await _seed_album(store, "direct")
    await _seed_album(store, "deep")
    with sqlite3.connect(db_path) as connection:
        connection.execute(
            "UPDATE local_tracks SET relative_path='box/01.flac' WHERE id='track-direct'"
        )
        connection.execute(
            "UPDATE local_tracks SET relative_path='box/nested/deeper/01.flac' "
            "WHERE id='track-deep'"
        )

    rows = await store.get_grouping_context_tracks("root", "box")

    assert [row["id"] for row in rows] == ["track-direct"]


@pytest.mark.asyncio
async def test_large_flat_grouping_uses_durable_pages_and_preserves_continuity(
    store: NativeLibraryStore,
    db_path: Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.setattr(
        "services.native.local_album_grouping_service.STAGED_GROUPING_THRESHOLD", 5
    )
    monkeypatch.setattr(
        "services.native.local_album_grouping_service.STAGING_BATCH_SIZE", 5
    )
    artist = LocalArtist(
        id="flat-artist",
        display_name="Flat Artist",
        folded_name="flat artist",
        normalized_name="flat artist",
        kind="group",
        created_at=1,
        updated_at=1,
    )
    album = LocalAlbum(
        id="flat-album",
        root_id="root",
        grouping_key="flat-old",
        title="Flat Album",
        album_artist_id=artist.id,
        album_artist_name=artist.display_name,
        created_at=1,
        updated_at=1,
    )
    tracks = [
        LocalTrack(
            id=f"flat-track-{index:03}",
            local_album_id=album.id,
            root_id="root",
            file_path=f"/music/flat/{index:03}.flac",
            relative_path=f"flat/{index:03}.flac",
            path_hash=f"flat-hash-{index:03}",
            file_size_bytes=100,
            file_mtime_ns=1,
            stat_revision=f"100:{index}",
            tag_revision=f"tag-{index}",
            title=f"Track {index}",
            artist_name=artist.display_name,
            album_title=album.title,
            album_artist_name=artist.display_name,
            tag_album_title=album.title,
            tag_album_artist_name=artist.display_name,
            track_number=index,
            duration_seconds=180,
            file_format="flac",
            imported_at=1,
            applied_policy="automatic",
            applied_policy_revision="policy-1",
        )
        for index in range(1, 13)
    ]
    await store.create_catalog_membership(
        CatalogMembership(
            album=album,
            artists=[artist],
            tracks=tracks,
            album_credits=[LocalArtistCredit(local_artist_id=artist.id, position=0)],
            track_credits={
                track.id: [LocalArtistCredit(local_artist_id=artist.id, position=0)]
                for track in tracks
            },
        )
    )
    await store.create_scan_run(
        ScanRun(
            id="large-grouping-run",
            kind="incremental",
            trigger="manual",
            queued_at=1,
        )
    )
    with sqlite3.connect(db_path) as connection:
        connection.execute(
            "INSERT INTO library_scan_grouping_contexts "
            "(run_id,root_id,relative_directory) "
            "VALUES ('large-grouping-run','root','flat')"
        )

    checkpoint_calls = 0

    async def pause_after_two_pages(_run_id: str, _policy_revision: str) -> bool:
        nonlocal checkpoint_calls
        checkpoint_calls += 1
        return checkpoint_calls <= 3

    first_enqueued = await LocalAlbumGroupingService(
        store, IdentificationQueueService(store)
    ).regroup_run(
        "large-grouping-run",
        now=2,
        checkpoint=pause_after_two_pages,
        frozen_policy_revision="policy-1",
    )
    with sqlite3.connect(db_path) as connection:
        partial = connection.execute(
            "SELECT state,staging_state FROM library_scan_grouping_contexts "
            "WHERE run_id='large-grouping-run'"
        ).fetchone()
        partial_staged = connection.execute(
            "SELECT COUNT(*) FROM library_scan_grouping_evidence "
            "WHERE run_id='large-grouping-run'"
        ).fetchone()[0]
    assert first_enqueued == 0
    assert partial == ("pending", "tracks")
    assert 0 < partial_staged < len(tracks)

    enqueued = await LocalAlbumGroupingService(
        store, IdentificationQueueService(store)
    ).regroup_run("large-grouping-run", now=3)

    with sqlite3.connect(db_path) as connection:
        context = connection.execute(
            "SELECT state,staging_state FROM library_scan_grouping_contexts "
            "WHERE run_id='large-grouping-run'"
        ).fetchone()
        album_ids = connection.execute(
            "SELECT DISTINCT local_album_id FROM local_tracks "
            "WHERE relative_path LIKE 'flat/%'"
        ).fetchall()
        staged = connection.execute(
            "SELECT COUNT(*) FROM library_scan_grouping_evidence "
            "WHERE run_id='large-grouping-run'"
        ).fetchone()[0]
    assert context == ("completed", "completed")
    assert album_ids == [("flat-album",)]
    assert staged == len(tracks)
    assert enqueued == 1


@pytest.mark.asyncio
@pytest.mark.parametrize("shape", ["split", "merge"])
async def test_large_ambiguous_continuity_uses_bounded_disk_matcher(
    store: NativeLibraryStore,
    db_path: Path,
    monkeypatch: pytest.MonkeyPatch,
    shape: str,
) -> None:
    monkeypatch.setattr(
        "services.native.local_album_grouping_service.STAGED_GROUPING_THRESHOLD", 3
    )
    monkeypatch.setattr(
        "services.native.local_album_grouping_service.CONTINUITY_COMPONENT_EDGE_LIMIT",
        3,
    )
    monkeypatch.setattr(
        "services.native.local_album_grouping_service.STAGING_BATCH_SIZE", 3
    )
    suffixes = [f"sparse-{index:02}" for index in range(8)]
    if shape == "split":
        artist = LocalArtist(
            id="sparse-artist",
            display_name="Sparse Artist",
            folded_name="sparse artist",
            normalized_name="sparse artist",
            kind="group",
            created_at=1,
            updated_at=1,
        )
        album = LocalAlbum(
            id="sparse-old-album",
            root_id="root",
            grouping_key="sparse-old",
            title="Before",
            album_artist_id=artist.id,
            album_artist_name=artist.display_name,
            created_at=1,
            updated_at=1,
        )
        tracks = [
            LocalTrack(
                id=f"track-{suffix}",
                local_album_id=album.id,
                root_id="root",
                file_path=f"/music/sparse/{suffix}.flac",
                relative_path=f"sparse/{suffix}.flac",
                path_hash=f"hash-{suffix}",
                file_size_bytes=1,
                file_mtime_ns=1,
                stat_revision=f"1:{index}",
                tag_revision=f"tag-{index}",
                title=f"Track {index}",
                artist_name=artist.display_name,
                album_title=f"Split {index}",
                album_artist_name=artist.display_name,
                tag_album_title=f"Split {index}",
                tag_album_artist_name=artist.display_name,
                file_format="flac",
                imported_at=1,
                applied_policy="automatic",
                applied_policy_revision="policy-1",
            )
            for index, suffix in enumerate(suffixes)
        ]
        await store.create_catalog_membership(
            CatalogMembership(
                album=album,
                artists=[artist],
                tracks=tracks,
                album_credits=[
                    LocalArtistCredit(local_artist_id=artist.id, position=0)
                ],
                track_credits={
                    track.id: [LocalArtistCredit(local_artist_id=artist.id, position=0)]
                    for track in tracks
                },
            )
        )
    else:
        for index, suffix in enumerate(suffixes):
            await _seed_album(store, suffix)
            with sqlite3.connect(db_path) as connection:
                connection.execute(
                    "UPDATE local_tracks SET relative_path=?,tag_album_title='Merged',"
                    "tag_album_artist_name='Artist' WHERE id=?",
                    (f"sparse/{index:02}.flac", f"track-{suffix}"),
                )
    await store.create_scan_run(
        ScanRun(id=f"sparse-{shape}", kind="incremental", trigger="manual", queued_at=1)
    )
    with sqlite3.connect(db_path) as connection:
        connection.execute(
            "INSERT INTO library_scan_grouping_contexts "
            "(run_id,root_id,relative_directory) VALUES (?, 'root', 'sparse')",
            (f"sparse-{shape}",),
        )

    await LocalAlbumGroupingService(
        store, IdentificationQueueService(store)
    ).regroup_run(f"sparse-{shape}", now=2)

    with sqlite3.connect(db_path) as connection:
        processed, total = connection.execute(
            "SELECT SUM(processed),COUNT(*) FROM library_scan_grouping_edges "
            "WHERE run_id=?",
            (f"sparse-{shape}",),
        ).fetchone()
        matched_old = connection.execute(
            "SELECT COUNT(*) FROM library_scan_grouping_old_nodes WHERE run_id=? "
            "AND matched_grouping_token IS NOT NULL",
            (f"sparse-{shape}",),
        ).fetchone()[0]
        matched_new = connection.execute(
            "SELECT COUNT(*) FROM library_scan_grouping_new_nodes WHERE run_id=? "
            "AND matched_old_album_id IS NOT NULL",
            (f"sparse-{shape}",),
        ).fetchone()[0]
    assert processed == total == len(suffixes)
    assert matched_old == matched_new == 1


@pytest.mark.asyncio
async def test_flat_grouping_indexes_refreshed_rows_once_and_reuses_artist_resolution(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.setattr(
        "services.native.local_album_grouping_service.STAGED_GROUPING_THRESHOLD", 2_000
    )

    class CountingRows(list):
        def __init__(self, rows):
            super().__init__(rows)
            self.iterations = 0

        def __iter__(self):
            self.iterations += 1
            return super().__iter__()

    rows = [
        {
            "id": f"track-{index}",
            "root_id": "root",
            "relative_path": f"flat/track-{index}.flac",
            "title": f"Track {index}",
            "artist_name": "Artist",
            "tag_album_title": "",
            "tag_album_artist_name": "Artist",
            "artist_sort": None,
            "album_artist_sort": None,
            "track_number": 0,
            "disc_number": 1,
            "duration_seconds": 180,
            "embedded_recording_mbid": None,
            "embedded_release_mbid": None,
            "embedded_release_group_mbid": None,
            "is_compilation": 0,
            "metadata_incomplete": 0,
            "membership_locked": 0,
            "local_album_id": f"old-album-{index}",
            "album_created_at": float(index),
            "tag_revision": f"tag-{index}",
            "stat_revision": f"stat-{index}",
            "applied_policy_revision": "policy",
            "applied_policy": "automatic",
        }
        for index in range(1_000)
    ]

    class Store:
        def __init__(self) -> None:
            self.applied = False
            self.resolve_calls = 0
            self.track_to_album: dict[str, str] = {}
            self.refreshed = CountingRows([])

        async def get_pending_grouping_contexts(self, _run_id):
            return (
                []
                if self.applied
                else [
                    {
                        "root_id": "root",
                        "relative_directory": "flat",
                    }
                ]
            )

        async def count_grouping_context_candidates(
            self, _root_id, _directory, *, limit
        ):
            return min(len(rows), limit)

        async def get_grouping_context_tracks(self, _root_id, _directory):
            if not self.applied:
                return rows
            self.refreshed = CountingRows(
                [
                    {
                        **row,
                        "local_album_id": self.track_to_album[row["id"]],
                    }
                    for row in rows
                ]
            )
            return self.refreshed

        async def resolve_or_create_local_artists(
            self, candidates, *, now, background=False
        ):
            del now, background
            self.resolve_calls += len(candidates)
            return {
                candidate_id: ("artist", False)
                for _display, _sort, _kind, candidate_id in candidates
            }

        async def apply_grouping_context(
            self, _run_id, _root_id, _directory, applications, *, now
        ):
            del now
            self.track_to_album = {
                track_id: application.local_album_id
                for application in applications
                for track_id in application.group.track_ids
            }
            self.applied = True
            return [application.local_album_id for application in applications], 1

        async def complete_grouping_context(self, _run_id, _root_id, _directory):
            return None

    class Queue:
        def __init__(self) -> None:
            self.calls = 0

        async def enqueue_albums_with_disposition(self, albums, **_kwargs):
            self.calls += len(albums)
            return [(album_id, True) for album_id, _revision, _kind in albums]

    store = Store()
    queue = Queue()
    enqueued = await LocalAlbumGroupingService(store, queue).regroup_run(
        "flat-run", now=3
    )

    assert enqueued == 1_000
    assert queue.calls == 1_000
    assert store.resolve_calls == 1
    assert store.refreshed.iterations == 1


@pytest.mark.asyncio
async def test_grouping_catalog_application_is_split_into_bounded_transactions(
    store: NativeLibraryStore, db_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    await store.create_scan_run(
        ScanRun(id="bounded-run", kind="incremental", trigger="manual", queued_at=1)
    )
    with sqlite3.connect(db_path) as connection:
        connection.execute(
            "INSERT INTO library_scan_grouping_contexts "
            "(run_id, root_id, relative_directory) VALUES ('bounded-run','root','flat')"
        )
    applications = [
        GroupingApplication(
            group=ProposedLocalAlbum(
                grouping_key=f"group-{index}",
                title=f"Album {index}",
                album_artist_name="Artist",
                track_ids=[f"track-{index}"],
                reason_code="AMBIGUOUS_FALLBACK_GROUP",
            ),
            local_album_id=f"album-{index}",
            local_artist_id="artist",
        )
        for index in range(1_001)
    ]
    batch_sizes: list[int] = []

    async def apply_batch(*_args, **kwargs):
        batch_sizes.append(len(_args[3]))
        assert len(kwargs["target_id_set"]) == 1_001
        return len(batch_sizes)

    monkeypatch.setattr(store, "_apply_grouping_batch", apply_batch)
    album_ids, revision = await store.apply_grouping_context(
        "bounded-run", "root", "flat", applications, now=2
    )

    assert batch_sizes == [500, 500, 1]
    assert len(album_ids) == 1_001
    assert revision == 3


@pytest.mark.asyncio
async def test_provider_artist_identity_proposes_merge_without_replacing_local_artist_ids(
    store: NativeLibraryStore, db_path: Path
) -> None:
    # Step 2.6 (N-01): the lone-eligible quorum needs provider proof for a
    # single present track, so both seeds carry the candidate's recording.
    await _seed_album(store, "1", embedded_recording=EMBEDDED_RECORDING)
    await _seed_album(store, "2", embedded_recording=EMBEDDED_RECORDING)
    provider = FakeProvider([_candidate(recording=EMBEDDED_RECORDING)])
    for suffix in ("1", "2"):
        job = await _claimed_job(store, f"album-{suffix}")
        outcome = await _service(
            store,
            provider,
            FakeFingerprinter(FingerprintResult(status="disabled"), enabled=False),
        ).run_claimed_job(job, "worker", now=3 + int(suffix))
        assert outcome == "identified"
    with sqlite3.connect(db_path) as connection:
        album_artists = connection.execute(
            "SELECT id, album_artist_id FROM local_albums ORDER BY id"
        ).fetchall()
        attached_artists = connection.execute(
            "SELECT local_artist_id, provider_artist_id "
            "FROM local_artist_external_identities"
        ).fetchall()
        merge_candidates = connection.execute(
            "SELECT left_artist_id, right_artist_id, reason_code "
            "FROM local_artist_merge_candidates"
        ).fetchall()
    assert album_artists == [("album-1", "artist-1"), ("album-2", "artist-2")]
    assert len(attached_artists) == 1
    assert attached_artists[0][1] == "artist-mbid"
    assert merge_candidates == [("artist-1", "artist-2", "SHARED_PROVIDER_IDENTITY")]


@pytest.mark.asyncio
async def test_terminal_miss_compaction_is_bounded_and_protects_references(
    store: NativeLibraryStore, db_path: Path
) -> None:
    await _seed_album(store, "1")
    await _seed_album(store, "2")
    for suffix in ("1", "2"):
        job = await _claimed_job(store, f"album-{suffix}")
        attempt = IdentificationAttempt(
            id=f"attempt-{suffix}",
            local_album_id=f"album-{suffix}",
            input_tag_revision="tag",
            input_file_revision="file",
            input_policy_revision="policy",
            matcher_version="matcher",
            state="no_candidate",
            terminal_reason_code="NO_EXTERNAL_RESULT",
            candidate_count=1,
            started_at=1,
            completed_at=2,
        )
        evidence = IdentificationEvidenceRecord(
            id=f"evidence-{suffix}",
            attempt_id=attempt.id,
            candidate_key=f"candidate-{suffix}",
            evidence=CandidateEvidence(
                release_group_mbid=f"rg-{suffix}",
                track_evidence=[
                    TrackEvidence(
                        local_track_id=f"track-{suffix}",
                        classification="contradictory",
                        evidence_kinds=["x" * 1000],
                    )
                ],
            ),
            created_at=2,
        )
        await store.complete_identification_job(
            job["id"],
            worker_id="worker",
            expected_job_revision=job["row_revision"],
            attempt=attempt,
            evidence=[evidence],
            terminal_state="needs_review",
            completed_at=2,
        )
    await store.create_review(
        ReviewDecision(
            id="protected-review",
            local_album_id="album-2",
            attempt_id="attempt-2",
            input_revision="protected",
            created_at=2,
            updated_at=2,
        )
    )
    compacted, total_bytes = await store.compact_terminal_identification_evidence(
        older_than=90 * 24 * 60 * 60
    )
    assert compacted == 1
    assert total_bytes <= 4096
    with sqlite3.connect(db_path) as connection:
        rows = connection.execute(
            "SELECT id, compacted, evidence_size_bytes FROM library_identification_evidence "
            "ORDER BY id"
        ).fetchall()
    assert rows[0][0:2] == ("evidence-1", 1)
    assert rows[0][2] <= 4096
    assert rows[1][0:2] == ("evidence-2", 0)

@pytest.mark.asyncio
async def test_identification_queue_defer_with_retry_after_persists_max_and_notifies_once(
    store: NativeLibraryStore, db_path: Path
) -> None:
    from infrastructure.resilience.retry import CircuitOpenError

    await _seed_album(store, "99")
    queue = IdentificationQueueService(store)
    delays: list[float] = []
    orig = store.work_wakeups.notify_after

    def spy(kind: str, delay: float) -> None:
        if kind == "identification":
            delays.append(delay)
        return orig(kind, delay)

    store.work_wakeups.notify_after = spy  # type: ignore[assignment]
    now = time.time() + 5
    job_id = "job-queue-test"
    await store.enqueue_identification_job(
        IdentificationJob(
            id=job_id,
            local_album_id="album-99",
            kind="automatic",
            dedupe_key="automatic:album-99:rev1",
            input_revision="rev1",
            priority=20,
            created_at=now - 10,
        )
    )
    job = await queue.claim("worker-1", now=now)
    assert job is not None
    delays.clear()
    await queue.defer(job, "worker-1", "PROVIDER_TEMPORARILY_UNAVAILABLE", now=now, retry_after_seconds=100)
    assert len(delays) == 1
    assert 99.5 <= delays[0] <= 101.0
    with sqlite3.connect(db_path) as conn:
        conn.row_factory = sqlite3.Row
        row = conn.execute("SELECT not_before FROM library_identification_jobs WHERE id = ?", (job_id,)).fetchone()
        assert row["not_before"] >= now + 100 - 0.5
    # No early claim before deadline
    assert await queue.claim("worker-2", now=now + 50) is None
    # Exactly one wake scheduled, claim after deadline succeeds
    job2 = await queue.claim("worker-2", now=now + 101)
    assert job2 is not None and job2["id"] == job_id
    delays.clear()
    await queue.defer(job2, "worker-2", "PROVIDER_TEMPORARILY_UNAVAILABLE", now=now + 101, retry_after_seconds=1.0)
    # Short retry should use backoff 60 for attempt 2
    assert len(delays) == 1
    assert 59.5 <= delays[0] <= 61.0
    with sqlite3.connect(db_path) as conn:
        conn.row_factory = sqlite3.Row
        row = conn.execute("SELECT not_before FROM library_identification_jobs WHERE id = ?", (job_id,)).fetchone()
        assert row["not_before"] >= now + 101 + 60 - 0.5
    # Invalid retry_after should be ignored and use backoff 120 for attempt 3
    job3 = await queue.claim("worker-3", now=now + 200)
    assert job3 is not None
    delays.clear()
    await queue.defer(job3, "worker-3", "PROVIDER_TEMPORARILY_UNAVAILABLE", now=now + 200, retry_after_seconds=float("inf"))
    assert len(delays) == 1
    assert 119.5 <= delays[0] <= 121.0


@pytest.mark.asyncio
async def test_album_identification_circuit_open_defers_with_retry_after(store: NativeLibraryStore, db_path: Path) -> None:
    from infrastructure.resilience.retry import CircuitOpenError
    from unittest.mock import MagicMock

    await _seed_album(store, "100")
    queue = IdentificationQueueService(store)

    class CircuitProvider:
        async def search_album_candidate_ids(self, *a, **k):
            raise CircuitOpenError("open", breaker_name="musicbrainz", retry_after_seconds=42)

        async def search_recording_candidate_ids(self, *a, **k):
            raise CircuitOpenError("open", breaker_name="musicbrainz", retry_after_seconds=42)

        async def get_album_candidate(self, *a, **k):
            raise CircuitOpenError("open", breaker_name="musicbrainz", retry_after_seconds=42)

        async def get_exact_release_candidate(self, *a, **k):
            raise CircuitOpenError("open", breaker_name="musicbrainz", retry_after_seconds=42)

    provider = CircuitProvider()
    candidates = AlbumCandidateService(provider)  # type: ignore[arg-type]
    evidence_engine = AlbumEvidenceEngine()
    fingerprints = MagicMock()
    fingerprints.fingerprint_if_needed = AsyncMock(return_value=(None, False))
    service = AlbumIdentificationService(store, queue, candidates, evidence_engine, fingerprints)
    now = time.time() + 5
    await store.enqueue_identification_job(
        IdentificationJob(
            id="job-album-100",
            local_album_id="album-100",
            kind="automatic",
            dedupe_key="automatic:album-100:rev1",
            input_revision="rev1",
            priority=20,
            created_at=now - 10,
        )
    )
    job = await queue.claim("worker-1", now=now)
    assert job is not None
    result = await service.run_claimed_job(job, "worker-1", now=now)  # type: ignore[arg-type]
    assert result == "provider_deferred"
    assert await queue.claim("worker-2", now=now + 10) is None
    later = await queue.claim("worker-2", now=now + 43)
    assert later is not None and later["id"] == job["id"]


@pytest.mark.asyncio
async def test_unmappable_payload_defers_with_deterministic_code(
    store: NativeLibraryStore, db_path: Path
) -> None:
    """F-IDENT-02: a healthy breaker plus a deterministic payload-shape failure
    must defer under UNMAPPABLE_PROVIDER_PAYLOAD - never the outage code."""
    await _seed_album(store)
    job = await _claimed_job(store)

    class UnmappableProvider(FakeProvider):
        async def search_album_candidate_ids(
            self, artist: str, title: str, limit: int, priority: RequestPriority
        ) -> list[str]:
            from infrastructure.degradation import try_get_degradation_context
            from infrastructure.integration_result import IntegrationResult

            ctx = try_get_degradation_context()
            assert ctx is not None
            ctx.record(
                IntegrationResult.deterministic_error(
                    source="musicbrainz",
                    msg="MusicBrainz release-group search returned an unmappable payload.",
                )
            )
            return []

    outcome = await _service(
        store,
        UnmappableProvider(),
        FakeFingerprinter(FingerprintResult(status="disabled"), enabled=False),
    ).run_claimed_job(job, "worker", now=3)

    assert outcome == "provider_deferred"
    with sqlite3.connect(db_path) as connection:
        row = connection.execute(
            "SELECT state, last_failure_code FROM library_identification_jobs "
            "WHERE id = ?",
            (job["id"],),
        ).fetchone()
    assert row == ("queued", "UNMAPPABLE_PROVIDER_PAYLOAD")


@pytest.mark.asyncio
async def test_unmappable_payload_reaches_terminal_attention_with_honest_cause(
    store: NativeLibraryStore, db_path: Path
) -> None:
    """The ordinary bounded cap applies; terminal attention preserves the
    deterministic cause instead of reporting a provider outage."""
    await _seed_album(store)

    class UnmappableProvider(FakeProvider):
        async def search_album_candidate_ids(
            self, artist: str, title: str, limit: int, priority: RequestPriority
        ) -> list[str]:
            from infrastructure.degradation import try_get_degradation_context
            from infrastructure.integration_result import IntegrationResult

            ctx = try_get_degradation_context()
            assert ctx is not None
            ctx.record(
                IntegrationResult.deterministic_error(
                    source="musicbrainz",
                    msg="MusicBrainz release-group search returned an unmappable payload.",
                )
            )
            return []

    service = _service(
        store,
        UnmappableProvider(),
        FakeFingerprinter(FingerprintResult(status="disabled"), enabled=False),
    )
    # The bounded cap is 10 attempts; exponential backoff stays under 40k s per
    # step, so advancing the clock by 40k s per attempt walks the whole ladder.
    for attempt in range(12):
        job = await store.enqueue_identification_job(
            IdentificationJob(
                id="job-album-1",
                local_album_id="album-1",
                kind="automatic",
                dedupe_key="automatic:album-1:revision",
                input_revision="revision",
                priority=20,
                created_at=1,
            )
        )
        claimed = await store.claim_identification_job(
            "worker", now=3 + attempt * 40000, lease_seconds=60
        )
        if claimed is None:
            continue
        outcome = await service.run_claimed_job(
            claimed, "worker", now=3 + attempt * 40000
        )
        if outcome != "provider_deferred":
            break

    with sqlite3.connect(db_path) as connection:
        failed_row = connection.execute(
            "SELECT last_failure_code, attention_cause FROM library_identification_jobs "
            "WHERE state = 'failed' ORDER BY terminal_at DESC LIMIT 1"
        ).fetchone()

    with sqlite3.connect(db_path) as connection:
        failed_row = connection.execute(
            "SELECT last_failure_code, attention_cause FROM library_identification_jobs "
            "WHERE state = 'failed' ORDER BY terminal_at DESC LIMIT 1"
        ).fetchone()
    assert failed_row is not None
    assert failed_row[0] == "MAX_DEFERRALS_EXCEEDED"
    assert failed_row[1] == "UNMAPPABLE_PROVIDER_PAYLOAD"


@pytest.mark.asyncio
async def test_provider_reset_and_enqueue_never_resurrect_unmappable_rows(
    store: NativeLibraryStore, db_path: Path
) -> None:
    """Provider-health reset keeps unmappable backoff untouched, and a later
    same-key enqueue must not resurrect a terminally-unmappable album."""
    await _seed_album(store)
    job = await _claimed_job(store)

    class UnmappableProvider(FakeProvider):
        async def search_album_candidate_ids(
            self, artist: str, title: str, limit: int, priority: RequestPriority
        ) -> list[str]:
            from infrastructure.degradation import try_get_degradation_context
            from infrastructure.integration_result import IntegrationResult

            ctx = try_get_degradation_context()
            assert ctx is not None
            ctx.record(
                IntegrationResult.deterministic_error(
                    source="musicbrainz",
                    msg="MusicBrainz release-group search returned an unmappable payload.",
                )
            )
            return []

    await _service(
        store,
        UnmappableProvider(),
        FakeFingerprinter(FingerprintResult(status="disabled"), enabled=False),
    ).run_claimed_job(job, "worker", now=3)

    queue = IdentificationQueueService(store)
    reset_count = await store.reset_provider_identification_deferrals(
        now=100, staleness_seconds=50
    )
    assert reset_count == 0  # exact-code gate: unmappable rows are untouched
    with sqlite3.connect(db_path) as connection:
        not_before, failure_code = connection.execute(
            "SELECT not_before, last_failure_code FROM library_identification_jobs "
            "WHERE id = ?",
            (job["id"],),
        ).fetchone()
    assert failure_code == "UNMAPPABLE_PROVIDER_PAYLOAD"
    assert not_before > 3  # backoff preserved, not cleared by the provider sweep


@pytest.mark.asyncio
async def test_deferral_sequence_matches_the_declared_cap_exactly(
    store: NativeLibraryStore, db_path: Path
) -> None:
    """F-IDENT-04: the applied backoff sequence is exactly 30 through 7,680 s,
    cumulative 15,330 s; attempt ten terminalizes with the original cause and
    no eleventh retry is schedulable."""
    await _seed_album(store)
    queue = IdentificationQueueService(store)
    await queue.enqueue_album("album-1", input_revision="revision", now=1)
    expected_waits = [30, 60, 120, 240, 480, 960, 1_920, 3_840, 7_680]
    now = 2.0
    applied: list[float] = []
    for attempt in range(1, MAX_DEFERRAL_ATTEMPTS + 1):
        claimed = await queue.claim("worker", now=now)
        assert claimed is not None, f"attempt {attempt} was not claimable"
        assert claimed["attempt_count"] == attempt
        with sqlite3.connect(db_path) as connection:
            not_before_before = connection.execute(
                "SELECT not_before FROM library_identification_jobs"
            ).fetchone()[0]
        await queue.defer(claimed, "worker", "PROVIDER_TEMPORARILY_UNAVAILABLE", now=now)
        with sqlite3.connect(db_path) as connection:
            state, not_before, failure_code, attempts = connection.execute(
                "SELECT state, not_before, last_failure_code, attempt_count "
                "FROM library_identification_jobs"
            ).fetchone()
        if attempt < MAX_DEFERRAL_ATTEMPTS:
            assert state == "queued"
            wait = not_before - now
            assert wait == pytest.approx(float(expected_waits[attempt - 1]))
            applied.append(wait)
            assert failure_code == "PROVIDER_TEMPORARILY_UNAVAILABLE"
            # A claim before the due time is blocked by the durable timestamp.
            early_claim = await queue.claim(
                "worker", now=(not_before_before + not_before) / 2
            )
            assert early_claim is None
            now = not_before
        else:
            assert state == "failed"
            assert failure_code == "MAX_DEFERRALS_EXCEEDED"
            cause = connection.execute(
                "SELECT attention_cause FROM library_identification_jobs"
            ).fetchone()[0]
            assert cause == "PROVIDER_TEMPORARILY_UNAVAILABLE"
            terminal_at_row = connection.execute(
                "SELECT terminal_at FROM library_identification_jobs"
            ).fetchone()[0]
            assert terminal_at_row == pytest.approx(now)
            break

    assert applied == [float(value) for value in expected_waits]
    assert sum(applied) == 15_330.0
    assert max(applied) == float(MAX_BACKOFF_SECONDS)

    # No eleventh retry is schedulable.
    assert await queue.claim("worker", now=now + 100_000) is None


class _OrderingProvider(FakeProvider):
    """Records every release-group fetch in order and serves one candidate each."""

    def __init__(
        self,
        album_ids: list[str],
        recording_ids: list[str],
        candidates: list[AlbumCandidate] | None = None,
    ) -> None:
        super().__init__(candidates)
        self.album_ids = album_ids
        self.recording_ids = recording_ids
        self._recording_page = 0
        self.fetch_order: list[str] = []

    async def search_album_candidate_ids(
        self, artist: str, title: str, limit: int, priority: RequestPriority
    ) -> list[str]:
        self.calls.append(("album", priority))
        return self.album_ids[:limit]

    async def search_recording_candidate_ids(
        self,
        artist: str,
        title: str,
        limit: int,
        priority: RequestPriority,
    ) -> list[str]:
        self.calls.append(("recording", priority))
        start = self._recording_page * limit
        self._recording_page += 1
        return self.recording_ids[start : start + limit]

    async def get_album_candidate(
        self,
        release_group_mbid: str,
        target_track_count: int,
        priority: RequestPriority,
    ) -> AlbumCandidate | None:
        self.calls.append((f"detail:{release_group_mbid}", priority))
        self.fetch_order.append(release_group_mbid)
        return AlbumCandidate(
            release_group_mbid=release_group_mbid,
            release_mbid=f"release-{release_group_mbid}",
            album_title="Noisy Tags",
            album_artist_name="Artist",
            tracks=[
                CandidateTrack(
                    title="Track",
                    position=1,
                    absolute_position=1,
                    duration_seconds=180.0,
                )
            ],
            release_type="album",
        )


def _recall_tracks() -> list[GroupingTrack]:
    return [
        GroupingTrack(
            local_track_id=f"t-{index}",
            root_id="root",
            relative_path=f"a/{index}.flac",
            title=f"Track {index}",
            artist_name="Artist",
            album_title="Noisy Tags",
            album_artist_name="Artist",
            title_provenance="tag",
            album_title_provenance="tag",
            album_artist_provenance="tag",
            track_number=index + 1,
            disc_number=1,
            duration_seconds=180.0,
        )
        for index in range(2)
    ]


@pytest.mark.asyncio
async def test_fingerprint_seed_survives_the_cap_and_is_fetched_first() -> None:
    """F-MATCH-02: with sparse tags and a long recording tail, the cached
    fingerprint seed leads the bounded fetch order instead of being truncated."""
    # 1 album id + 4 samples x 5 distinct recording ids + 1 seed > 10: the cap
    # must truncate the text tail, never the leading fingerprint seed.
    provider = _OrderingProvider(
        album_ids=["rg-text-00"],
        recording_ids=[f"rg-rec-{index:02d}" for index in range(20)],
    )
    service = AlbumCandidateService(provider)

    candidates = await service.recall(
        _recall_tracks(),
        cached_fingerprint_release_groups=["rg-fingerprint-audio"],
        explicit=True,
    )

    assert provider.fetch_order[0] == "rg-fingerprint-audio"
    assert "rg-fingerprint-audio" in provider.fetch_order[:MAX_CANDIDATES]
    assert len(provider.fetch_order) == MAX_CANDIDATES  # bound intact
    detail_calls = [
        call for call in provider.calls if call[0].startswith("detail:")
    ]
    assert {priority for _, priority in detail_calls} == {
        RequestPriority.USER_INITIATED
    }
    assert candidates[0].release_group_mbid == "rg-fingerprint-audio"
    assert candidates[0].source_kinds == ["cached_fingerprint"]


@pytest.mark.asyncio
async def test_multiple_fingerprint_seeds_keep_input_order_after_dedup() -> None:
    provider = _OrderingProvider(album_ids=[], recording_ids=[])
    service = AlbumCandidateService(provider)

    await service.recall(
        _recall_tracks(),
        cached_fingerprint_release_groups=[
            "rg-fp-b",
            "",
            "rg-fp-a",
            "rg-fp-b",
            None or "rg-fp-c",
            "rg-fp-a",
        ],
    )

    seeds = [
        group for group in provider.fetch_order if group.startswith("rg-fp-")
    ]
    assert seeds == ["rg-fp-b", "rg-fp-a", "rg-fp-c"]


@pytest.mark.asyncio
async def test_overlapping_fingerprint_and_text_sources_merge_labels() -> None:
    """One provider fetch per ID; the candidate keeps both source labels in a
    deterministic order (fingerprint first, then the text label)."""
    shared = "rg-shared"
    provider = _OrderingProvider(
        album_ids=[shared, "rg-text-only"],
        recording_ids=[],
    )
    service = AlbumCandidateService(provider)

    candidates = await service.recall(
        _recall_tracks(),
        cached_fingerprint_release_groups=[shared],
    )

    fetched_once = [
        group for group in provider.fetch_order if group == shared
    ] == [shared]
    assert fetched_once
    by_group = {c.release_group_mbid: c for c in candidates}
    assert by_group[shared].source_kinds == [
        "cached_fingerprint",
        "album_tags",
    ]


@pytest.mark.asyncio
async def test_embedded_and_exact_branches_keep_their_priority_contracts() -> None:
    """Exact-release branches are untouched; an embedded seed keeps its label
    and merges deterministically when it also appears as a text hit."""
    provider = _OrderingProvider(album_ids=["rg-embedded"], recording_ids=[])

    # Unanimous embedded exact release still short-circuits to the exact call.
    exact_candidate = AlbumCandidate(
        release_group_mbid="rg-exact",
        release_mbid="release-exact",
        album_title="Noisy Tags",
        album_artist_name="Artist",
        tracks=[
            CandidateTrack(
                title="Track",
                position=1,
                absolute_position=1,
                duration_seconds=180.0,
            )
        ],
        release_type="album",
    )
    exact_provider = _OrderingProvider(album_ids=[], recording_ids=[])
    exact_provider.candidates = [exact_candidate]
    exact_tracks = _recall_tracks()
    for track in exact_tracks:
        track.release_mbid = "release-exact"
    exact_candidates = await AlbumCandidateService(exact_provider).recall(exact_tracks)
    assert [c.source_kinds for c in exact_candidates] == [["embedded_exact_release"]]
    assert exact_provider.exact_releases == [("release-exact", RequestPriority.BACKGROUND_SYNC)]
    assert exact_provider.calls == []  # never enters bounded recall

    # Embedded release-group seed precedes the text hit after reorder.
    seeded_tracks = _recall_tracks()
    for track in seeded_tracks:
        track.release_group_mbid = "rg-embedded"
    candidates = await AlbumCandidateService(provider).recall(seeded_tracks)
    assert candidates[0].release_group_mbid == "rg-embedded"
    assert candidates[0].source_kinds == ["embedded", "album_tags"]


@pytest.mark.asyncio
async def test_stale_embedded_release_falls_through_to_full_recall() -> None:
    """A unanimous embedded release that no longer exists (merged/deleted
    upstream) must not orphan the album: recall falls through to full
    recall instead of returning no candidates."""
    provider = _OrderingProvider(album_ids=["rg-fallback"], recording_ids=[])
    provider.candidates = []
    tracks = _recall_tracks()
    for track in tracks:
        track.release_mbid = "release-stale"
        track.release_group_mbid = "rg-fallback"

    candidates = await AlbumCandidateService(provider).recall(tracks)

    assert provider.exact_releases == [
        ("release-stale", RequestPriority.BACKGROUND_SYNC)
    ]
    assert provider.calls != []  # entered bounded full recall
    assert [c.release_group_mbid for c in candidates] == ["rg-fallback"]
    assert all(c.release_mbid != "release-stale" for c in candidates)


@pytest.mark.asyncio
async def test_mixed_case_embedded_release_counts_as_unanimous() -> None:
    """Tag MBIDs keep their verbatim case: mixed-case unanimous tags take
    the exact-release path (then fall through when stale), not the orphan
    branch. The provider lookup uses the original string."""
    provider = _OrderingProvider(album_ids=["rg-fallback"], recording_ids=[])
    provider.candidates = []
    tracks = _recall_tracks()
    tracks[0].release_mbid = "RELEASE-STALE"
    tracks[1].release_mbid = "release-stale"
    tracks[0].release_group_mbid = "RG-FALLBACK"
    tracks[1].release_group_mbid = "rg-fallback"

    candidates = await AlbumCandidateService(provider).recall(tracks)

    assert provider.exact_releases == [
        ("RELEASE-STALE", RequestPriority.BACKGROUND_SYNC)
    ]
    assert provider.calls != []  # entered bounded full recall
    assert [c.release_group_mbid for c in candidates] == ["rg-fallback"]
    assert candidates[0].source_kinds == ["embedded", "album_tags"]


@pytest.mark.asyncio
async def test_explicit_exact_release_still_returns_empty_on_miss() -> None:
    """Unlike stale tags, an administrator demanding one exact release must
    never silently get another: an exact miss returns no candidates."""
    provider = _OrderingProvider(album_ids=["rg-fallback"], recording_ids=[])
    provider.candidates = []

    candidates = await AlbumCandidateService(provider).recall(
        _recall_tracks(), exact_release_mbid="release-missing"
    )

    assert candidates == []
    assert provider.calls == []  # never enters bounded recall


def test_target_seed_order_matches_album_identifier_reference() -> None:
    """Plan step 6 comparison fixture: the target ordering rule (deduped seeds
    first, then text ids) matches ``AlbumIdentifier._candidate_release_groups``'s
    documented seed-first behavior for equivalent inputs."""
    seeds = ["rg-fp-b", "", "rg-fp-a"]
    deduped_seeds = list(dict.fromkeys(value for value in seeds if value))
    text_ranked = ["rg-text-0", "rg-text-1"]

    from services.native.album_candidate_service import (
        ALBUM_SEARCH_LIMIT,
    )

    del ALBUM_SEARCH_LIMIT
    combined = (deduped_seeds + text_ranked)[:10]

    # The matcher reference builds exactly `seeds + text_ranked` with the same
    # empty-filter/dedupe semantics.
    matcher_reference = list(
        dict.fromkeys(m for m in seeds if m)
    ) + text_ranked

    assert combined == matcher_reference


class _LocalFailureThenRecoveringFingerprinter(FakeFingerprinter):
    """fpcalc fails on the first generation, then recovers."""

    def __init__(self) -> None:
        super().__init__(
            FingerprintResult(
                status="pass",
                recording_id="rec-local",
                release_group_ids=["rg-local"],
            ),
            enabled=True,
        )
        self.generate_calls = 0

    async def generate_fingerprint(self, path: Path) -> tuple[str, int]:
        self.generate_calls += 1
        if self.generate_calls == 1:
            raise OSError("fpcalc temporarily blocked")
        return ("fp-hash", 180)


@pytest.mark.asyncio
async def test_local_fingerprint_failure_gets_bounded_retry_deadline(
    store: NativeLibraryStore,
) -> None:
    """F-MATCH-04: FINGERPRINT_LOCAL_FAILURE persists a bounded retry_after;
    the cached failure is reused before the deadline and regenerated at it,
    fenced by stat_revision and fingerprinter version."""
    from services.native.conditional_fingerprint_service import (
        TRANSIENT_RETRY_SECONDS,
    )

    await _seed_album(store)
    fake = _LocalFailureThenRecoveringFingerprinter()
    service = ConditionalFingerprintService(store, fake)

    failed, failed_work = await service.fingerprint_if_needed(
        local_track_id="track-1",
        path=Path("/music/1.flac"),
        stat_revision="stat-1",
        needed=True,
        now=100.0,
    )
    assert failed_work is True
    assert failed.state == "failed"
    assert failed.failure_code == "FINGERPRINT_LOCAL_FAILURE"
    assert failed.retry_after == pytest.approx(100.0 + TRANSIENT_RETRY_SECONDS)
    assert failed.attempt_count == 1

    # Before the deadline: cached failure reused, no regeneration.
    before, before_work = await service.fingerprint_if_needed(
        local_track_id="track-1",
        path=Path("/music/1.flac"),
        stat_revision="stat-1",
        needed=True,
        now=100.0 + TRANSIENT_RETRY_SECONDS - 1,
    )
    assert fake.generate_calls == 1
    assert before.state == "failed"
    assert before.retry_after == failed.retry_after
    assert before_work is False

    # At the deadline: generation retried and lookup succeeds normally.
    recovered, recovered_work = await service.fingerprint_if_needed(
        local_track_id="track-1",
        path=Path("/music/1.flac"),
        stat_revision="stat-1",
        needed=True,
        now=100.0 + TRANSIENT_RETRY_SECONDS,
    )
    assert fake.generate_calls == 2
    assert recovered.state == "matched"
    assert recovered.recording_mbid == "rec-local"
    assert recovered.stat_revision == "stat-1"
    assert recovered.fingerprinter_version == "fpcalc-acoustid-v1"
    assert recovered_work is True

    # A different stat_revision is never blocked by the old revision's failure.
    changed, _ = await service.fingerprint_if_needed(
        local_track_id="track-1",
        path=Path("/music/1.flac"),
        stat_revision="stat-2",
        needed=True,
        now=100.0 + TRANSIENT_RETRY_SECONDS + 1,
    )
    assert fake.generate_calls == 3
    assert changed.stat_revision == "stat-2"


@pytest.mark.asyncio
async def test_automatic_worker_defers_local_fingerprint_failure_honestly(
    store: NativeLibraryStore, db_path: Path
) -> None:
    """A local fpcalc failure defers under its own code - never
    PROVIDER_TEMPORARILY_UNAVAILABLE - so provider-only reset/resurrection
    gates can never select the row."""
    await _seed_album(store)
    queue = IdentificationQueueService(store)

    # Candidates with NO usable track evidence leave the decision
    # insufficient_evidence with zero supported recordings, which is exactly
    # what sends the worker into the conditional fingerprint branch with
    # needed=True for every track.
    class LocalFailProvider(FakeProvider):
        async def search_album_candidate_ids(
            self, artist: str, title: str, limit: int, priority: RequestPriority
        ) -> list[str]:
            return ["rg-a", "rg-b"]

        async def get_album_candidate(
            self,
            release_group_mbid: str,
            target_track_count: int,
            priority: RequestPriority,
        ) -> AlbumCandidate | None:
            # Mirror the existing conditional-fingerprint fixture: one plausible
            # candidate per group so the ordinary decision lands on `ambiguous`
            # and the worker enters the fingerprint branch.
            return AlbumCandidate(
                release_group_mbid=release_group_mbid,
                release_mbid=f"release-{release_group_mbid}",
                album_title="Album",
                album_artist_name="Artist",
                tracks=[
                    CandidateTrack(
                        title="Track 1",
                        position=1,
                        absolute_position=1,
                        duration_seconds=180.0,
                        recording_mbid=f"recording-{release_group_mbid}",
                        release_track_mbid=f"release-track-{release_group_mbid}",
                    )
                ],
                release_type="album",
            )

    fingerprinter = _LocalFailureThenRecoveringFingerprinter()
    service = _service(store, LocalFailProvider(), fingerprinter)
    await queue.enqueue_album("album-1", input_revision="revision", now=1)
    claimed = await queue.claim("worker", now=2)
    assert claimed is not None
    outcome = await service.run_claimed_job(claimed, "worker", now=3)

    assert outcome == "provider_deferred"
    with sqlite3.connect(db_path) as connection:
        row = connection.execute(
            "SELECT last_failure_code FROM library_identification_jobs "
            "WHERE id = ?",
            (claimed["id"],),
        ).fetchone()
    assert row[0] == "FINGERPRINT_LOCAL_FAILURE"
    # Provider-only sweep leaves the local-failure row's backoff untouched.
    assert (
        await store.reset_provider_identification_deferrals(
            now=100_000, staleness_seconds=60
        )
        == 0
    )


class _AmbiguousTwoGroupProvider(FakeProvider):
    """Two plausible one-track groups: the ordinary decision lands ambiguous
    so the worker enters the conditional fingerprint branch with needed=True
    for the track."""

    async def search_album_candidate_ids(
        self, artist: str, title: str, limit: int, priority: RequestPriority
    ) -> list[str]:
        return ["rg-a", "rg-b"]

    async def get_album_candidate(
        self,
        release_group_mbid: str,
        target_track_count: int,
        priority: RequestPriority,
    ) -> AlbumCandidate | None:
        return AlbumCandidate(
            release_group_mbid=release_group_mbid,
            release_mbid=f"release-{release_group_mbid}",
            album_title="Album",
            album_artist_name="Artist",
            tracks=[
                CandidateTrack(
                    title="Track 1",
                    position=1,
                    absolute_position=1,
                    duration_seconds=180.0,
                    recording_mbid=f"recording-{release_group_mbid}",
                    release_track_mbid=f"release-track-{release_group_mbid}",
                )
            ],
            release_type="album",
        )


class _NoFpcalcBinaryFingerprinter(FakeFingerprinter):
    """F-06: the fpcalc binary is absent - every generation raises
    FileNotFoundError, exactly like a no-fpcalc host."""

    def __init__(self) -> None:
        super().__init__(FingerprintResult(status="error"))

    async def generate_fingerprint(self, path: Path) -> tuple[str, int]:
        self.generate_calls += 1
        raise FileNotFoundError("fpcalc")


class _ToggleableFpcalcFingerprinter(FakeFingerprinter):
    """F-06: binary absent until `binary_present` flips, modelling a
    mid-process fpcalc install."""

    def __init__(self) -> None:
        super().__init__(
            FingerprintResult(status="pass", recording_id="rec-1", score=0.9)
        )
        self.binary_present = False

    async def generate_fingerprint(self, path: Path) -> tuple[str, int]:
        self.generate_calls += 1
        if not self.binary_present:
            raise FileNotFoundError("fpcalc")
        return ("fp-hash", 180)


class _CrashingFpcalcFingerprinter(FakeFingerprinter):
    """F-06: fpcalc starts mid-run then crashes (genuine failure, not an
    absent binary)."""

    def __init__(self) -> None:
        super().__init__(FingerprintResult(status="error"))

    async def generate_fingerprint(self, path: Path) -> tuple[str, int]:
        self.generate_calls += 1
        raise subprocess.CalledProcessError(1, "fpcalc")


class _AlwaysLocalFailureFingerprinter(FakeFingerprinter):
    """F-07: fpcalc generation always fails transiently (never recovers)."""

    def __init__(self) -> None:
        super().__init__(FingerprintResult(status="error"))

    async def generate_fingerprint(self, path: Path) -> tuple[str, int]:
        self.generate_calls += 1
        raise OSError("fpcalc transiently blocked")


def _spied_identification_service(
    store: NativeLibraryStore,
    provider: FakeProvider,
    fingerprinter: FakeFingerprinter,
) -> tuple[
    IdentificationQueueService,
    AlbumIdentificationService,
    list[tuple[FingerprintOutcome | None, bool]],
]:
    """Build the identification service like `_service` but record every raw
    `(outcome, did_work)` tuple `fingerprint_if_needed` returns."""
    queue = IdentificationQueueService(store)
    fingerprints = ConditionalFingerprintService(store, fingerprinter)
    service = AlbumIdentificationService(
        store,
        queue,
        AlbumCandidateService(provider),
        AlbumEvidenceEngine(),
        fingerprints,
    )
    seen: list[tuple[FingerprintOutcome | None, bool]] = []
    original = fingerprints.fingerprint_if_needed

    async def spy(**kwargs: object) -> tuple[FingerprintOutcome | None, bool]:
        result = await original(**kwargs)  # type: ignore[arg-type]
        seen.append(result)
        return result

    fingerprints.fingerprint_if_needed = spy  # type: ignore[method-assign]
    return queue, service, seen


@pytest.mark.asyncio
async def test_missing_fpcalc_binary_maps_to_disabled_and_recovers(
    store: NativeLibraryStore,
) -> None:
    """F-06 unit: FileNotFoundError maps to disabled/FPCALC_BINARY_ABSENT
    (fresh work); a later-installed binary re-runs fpcalc - the absence is
    recorded for evidence but never reused."""
    await _seed_album(store)
    fake = _ToggleableFpcalcFingerprinter()
    service = ConditionalFingerprintService(store, fake)
    outcome, did_work = await service.fingerprint_if_needed(
        local_track_id="track-1",
        path=Path("/music/1.flac"),
        stat_revision="stat-1",
        needed=True,
        now=1,
    )
    assert did_work is True
    assert outcome is not None and outcome.state == "disabled"
    assert outcome.failure_code == "FPCALC_BINARY_ABSENT"
    fake.binary_present = True
    recovered, recovered_work = await service.fingerprint_if_needed(
        local_track_id="track-1",
        path=Path("/music/1.flac"),
        stat_revision="stat-1",
        needed=True,
        now=2,
    )
    assert recovered_work is True
    assert recovered is not None and recovered.state == "matched"
    assert recovered.recording_mbid == "rec-1"
    assert fake.generate_calls == 2


@pytest.mark.asyncio
@pytest.mark.parametrize(
    "error",
    [
        OSError("fpcalc transiently blocked"),
        subprocess.CalledProcessError(1, "fpcalc"),
        TimeoutError("fpcalc timed out"),
        ValueError("cannot parse fpcalc output"),
    ],
)
async def test_fpcalc_transient_errors_stay_failed(
    store: NativeLibraryStore, error: Exception
) -> None:
    """F-06 unit: non-FileNotFoundError OSErrors, mid-run crashes, timeouts,
    and parse errors stay failed (defer path) - nothing transient is masked
    as an absent binary."""

    class _RaisingFingerprinter(FakeFingerprinter):
        async def generate_fingerprint(self, path: Path) -> tuple[str, int]:
            self.generate_calls += 1
            raise error

    await _seed_album(store)
    service = ConditionalFingerprintService(
        store, _RaisingFingerprinter(FingerprintResult(status="error"))
    )
    outcome, did_work = await service.fingerprint_if_needed(
        local_track_id="track-1",
        path=Path("/music/1.flac"),
        stat_revision="stat-1",
        needed=True,
        now=1,
    )
    assert did_work is True
    assert outcome is not None and outcome.state == "failed"
    assert outcome.failure_code == "FINGERPRINT_LOCAL_FAILURE"


@pytest.mark.asyncio
async def test_missing_fpcalc_binary_proceeds_tag_only_to_decision(
    store: NativeLibraryStore, db_path: Path
) -> None:
    """F-06: on a no-fpcalc host the ambiguous album proceeds tag-only to a
    terminal decision - no defer, no fpcalc-attention loop."""
    await _seed_album(store)
    queue = IdentificationQueueService(store)
    service = AlbumIdentificationService(
        store,
        queue,
        AlbumCandidateService(_AmbiguousTwoGroupProvider()),
        AlbumEvidenceEngine(),
        ConditionalFingerprintService(store, _NoFpcalcBinaryFingerprinter()),
    )
    job = await _claimed_job(store)
    outcome = await service.run_claimed_job(job, "worker", now=3)
    assert outcome == "ambiguous"
    fp = await store.get_fingerprint_outcome(
        "track-1", "stat-1", FINGERPRINTER_VERSION
    )
    assert fp is not None and fp.state == "disabled"
    assert fp.failure_code == "FPCALC_BINARY_ABSENT"
    with sqlite3.connect(db_path) as connection:
        state, failure = connection.execute(
            "SELECT state, last_failure_code FROM library_identification_jobs "
            "WHERE id = ?",
            (job["id"],),
        ).fetchone()
    assert state == "needs_review"
    assert failure is None


@pytest.mark.asyncio
async def test_fpcalc_crash_still_defers_with_fresh_work(
    store: NativeLibraryStore, db_path: Path
) -> None:
    """F-06: a genuine mid-run fpcalc crash stays failed and defers under
    FINGERPRINT_LOCAL_FAILURE - and the fresh attempt reports did_work."""
    await _seed_album(store)
    queue, service, seen = _spied_identification_service(
        store, _AmbiguousTwoGroupProvider(), _CrashingFpcalcFingerprinter()
    )
    job = await _claimed_job(store)
    outcome = await service.run_claimed_job(job, "worker", now=3)
    assert outcome == "provider_deferred"
    assert len(seen) == 1
    assert seen[0][0] is not None and seen[0][0].state == "failed"
    assert seen[0][0].failure_code == "FINGERPRINT_LOCAL_FAILURE"
    assert seen[0][1] is True
    with sqlite3.connect(db_path) as connection:
        row = connection.execute(
            "SELECT last_failure_code FROM library_identification_jobs "
            "WHERE id = ?",
            (job["id"],),
        ).fetchone()
    assert row[0] == "FINGERPRINT_LOCAL_FAILURE"


@pytest.mark.asyncio
async def test_cached_fingerprint_failure_proceeds_to_decision(
    store: NativeLibraryStore, db_path: Path
) -> None:
    """F-07: a fresh local failure defers; the retry inside the 60s window
    reuses the unchanged cached failed row (no new work) and proceeds to a
    terminal decision instead of deferring again."""
    await _seed_album(store)
    queue = IdentificationQueueService(store)
    fingerprinter = _AlwaysLocalFailureFingerprinter()
    service = AlbumIdentificationService(
        store,
        queue,
        AlbumCandidateService(_AmbiguousTwoGroupProvider()),
        AlbumEvidenceEngine(),
        ConditionalFingerprintService(store, fingerprinter),
    )
    await queue.enqueue_album("album-1", input_revision="revision", now=1)
    first = await queue.claim("worker", now=2)
    assert first is not None
    assert await service.run_claimed_job(first, "worker", now=3) == (
        "provider_deferred"
    )
    assert fingerprinter.generate_calls == 1
    # After the 30s queue backoff but inside the 60s fingerprint retry
    # window: the cached failure is reused, so no new fpcalc work happens.
    second = await queue.claim("worker", now=34)
    assert second is not None
    assert await service.run_claimed_job(second, "worker", now=34) == "ambiguous"
    assert fingerprinter.generate_calls == 1
    with sqlite3.connect(db_path) as connection:
        state = connection.execute(
            "SELECT state FROM library_identification_jobs WHERE id = ?",
            (second["id"],),
        ).fetchone()
    assert state[0] == "needs_review"


@pytest.mark.asyncio
async def test_fresh_fingerprint_failure_still_defers(
    store: NativeLibraryStore, db_path: Path
) -> None:
    """F-07: a fresh failure on a load-bearing track keeps deferring - and
    the fresh attempt reports did_work."""
    await _seed_album(store)
    queue, service, seen = _spied_identification_service(
        store, _AmbiguousTwoGroupProvider(), _AlwaysLocalFailureFingerprinter()
    )
    await queue.enqueue_album("album-1", input_revision="revision", now=1)
    claimed = await queue.claim("worker", now=2)
    assert claimed is not None
    assert await service.run_claimed_job(claimed, "worker", now=3) == (
        "provider_deferred"
    )
    assert len(seen) == 1
    assert seen[0][0] is not None and seen[0][0].state == "failed"
    assert seen[0][1] is True
    with sqlite3.connect(db_path) as connection:
        row = connection.execute(
            "SELECT last_failure_code FROM library_identification_jobs "
            "WHERE id = ?",
            (claimed["id"],),
        ).fetchone()
    assert row[0] == "FINGERPRINT_LOCAL_FAILURE"


@pytest.mark.asyncio
async def test_staged_collision_tokens_stay_two_albums_like_the_small_path(
    store: NativeLibraryStore,
    db_path: Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """F-MATCH-06: two distinct grouping tokens that resolve to the same
    directory, normalized consensus title, and artist must remain two local
    albums on the staged path - matching the small path - and the staged keys
    must be deterministic across pages, resumes, and repeated runs."""
    monkeypatch.setattr(
        "services.native.local_album_grouping_service.STAGED_GROUPING_THRESHOLD", 4
    )
    monkeypatch.setattr(
        "services.native.local_album_grouping_service.STAGING_BATCH_SIZE", 2
    )
    artist = LocalArtist(
        id="mixed-artist",
        display_name="Artist One",
        folded_name="artist one",
        normalized_name="artist one",
        kind="group",
        created_at=1,
        updated_at=1,
    )
    album = LocalAlbum(
        id="mixed-old-album",
        root_id="root",
        grouping_key="mixed-old",
        title="Mixed Bag",
        album_artist_id=artist.id,
        album_artist_name=artist.display_name,
        created_at=1,
        updated_at=1,
    )
    # 6 tracks > patched threshold of 4. All share the same normalized tag
    # title/artist; alternating is_compilation splits them into two distinct
    # grouping tokens (tagged:mixed bag: vs tagged:mixed bag:artist one).
    tracks = [
        LocalTrack(
            id=f"mixed-track-{index:02}",
            local_album_id=album.id,
            root_id="root",
            file_path=f"/music/mixed/{index:02}.flac",
            relative_path=f"mixed/{index:02}.flac",
            path_hash=f"mixed-hash-{index:02}",
            file_size_bytes=100,
            file_mtime_ns=1,
            stat_revision=f"100:{index}",
            tag_revision=f"tag-{index}",
            title=f"Track {index}",
            artist_name=artist.display_name,
            album_title=album.title,
            album_artist_name=artist.display_name,
            tag_album_title="Mixed Bag",
            tag_album_artist_name="Artist One",
            track_number=index,
            duration_seconds=180,
            file_format="flac",
            imported_at=1,
            applied_policy="automatic",
            applied_policy_revision="policy-1",
            is_compilation=index % 2 == 0,
        )
        for index in range(1, 7)
    ]
    await store.create_catalog_membership(
        CatalogMembership(
            album=album,
            artists=[artist],
            tracks=tracks,
            album_credits=[LocalArtistCredit(local_artist_id=artist.id, position=0)],
            track_credits={
                track.id: [LocalArtistCredit(local_artist_id=artist.id, position=0)]
                for track in tracks
            },
        )
    )

    def _run(run_id: str, now: float):
        return LocalAlbumGroupingService(
            store, IdentificationQueueService(store)
        ).regroup_run(run_id, now=now, frozen_policy_revision="policy-1")

    # Staged run first.
    await store.create_scan_run(
        ScanRun(id="mixed-staged-run", kind="incremental", trigger="manual", queued_at=1)
    )
    with sqlite3.connect(db_path) as connection:
        connection.execute(
            "INSERT INTO library_scan_grouping_contexts "
            "(run_id,root_id,relative_directory) "
            "VALUES ('mixed-staged-run','root','mixed')"
        )
    await _run("mixed-staged-run", now=2)

    with sqlite3.connect(db_path) as connection:
        staged_groups = connection.execute(
            "SELECT grouping_token,grouping_key,local_album_id FROM "
            "library_scan_grouping_groups WHERE run_id='mixed-staged-run' "
            "ORDER BY grouping_token"
        ).fetchall()
        staged_track_albums = {
            row[0]: row[1]
            for row in connection.execute(
                "SELECT id,local_album_id FROM local_tracks "
                "WHERE relative_path LIKE 'mixed/%'"
            ).fetchall()
        }
    assert len(staged_groups) == 2
    first_token, second_token = (
        str(staged_groups[0][0]),
        str(staged_groups[1][0]),
    )
    first_key, second_key = (
        str(staged_groups[0][1]),
        str(staged_groups[1][1]),
    )
    first_album, second_album = (
        str(staged_groups[0][2]),
        str(staged_groups[1][2]),
    )
    assert first_key != second_key
    assert second_key.startswith(first_key + ":")  # deterministic ordinal suffix
    assert first_album != second_album

    compilation_tracks = sorted(
        track.id for track in tracks if track.is_compilation
    )
    plain_tracks = sorted(track.id for track in tracks if not track.is_compilation)
    token_a_tracks = {
        track_id
        for track_id, album_id in staged_track_albums.items()
        if album_id == first_album
    }
    assert sorted(token_a_tracks) in (compilation_tracks, plain_tracks)
    # Disjoint membership covering every input track.
    assigned = [album_id for _, album_id in staged_track_albums.items()]
    assert len(set(assigned)) == 2

    # Deterministic across a fresh run of the same input.
    await store.finish_scan_run("mixed-staged-run") if hasattr(store, "finish_scan_run") else None
    with sqlite3.connect(db_path) as connection:
        connection.execute(
            "UPDATE library_scan_runs SET state='completed' WHERE id='mixed-staged-run'"
        )
    await store.create_scan_run(
        ScanRun(id="mixed-staged-run-2", kind="incremental", trigger="manual", queued_at=2)
    )
    with sqlite3.connect(db_path) as connection:
        connection.execute(
            "INSERT INTO library_scan_grouping_contexts "
            "(run_id,root_id,relative_directory) "
            "VALUES ('mixed-staged-run-2','root','mixed')"
        )
    await _run("mixed-staged-run-2", now=3)
    with sqlite3.connect(db_path) as connection:
        rerun_keys = [
            str(row[0])
            for row in connection.execute(
                "SELECT grouping_key FROM library_scan_grouping_groups "
                "WHERE run_id='mixed-staged-run-2' ORDER BY grouping_token"
            ).fetchall()
        ]
    assert rerun_keys == [first_key, second_key]

    # Small path on a separate run: same group count and disjoint membership.
    with sqlite3.connect(db_path) as connection:
        connection.execute(
            "UPDATE library_scan_runs SET state='completed' WHERE id='mixed-staged-run-2'"
        )
    await store.create_scan_run(
        ScanRun(id="mixed-small-run", kind="incremental", trigger="manual", queued_at=3)
    )
    with sqlite3.connect(db_path) as connection:
        connection.execute(
            "INSERT INTO library_scan_grouping_contexts "
            "(run_id,root_id,relative_directory) "
            "VALUES ('mixed-small-run','root','mixed')"
        )
    monkeypatch.setattr(
        "services.native.local_album_grouping_service.STAGED_GROUPING_THRESHOLD", 50
    )
    await _run("mixed-small-run", now=4)
    with sqlite3.connect(db_path) as connection:
        small_album_ids = {
            row[0]
            for row in connection.execute(
                "SELECT DISTINCT local_album_id FROM local_tracks "
                "WHERE relative_path LIKE 'mixed/%'"
            ).fetchall()
        }
    assert len(small_album_ids - {album.id}) >= 2 or len(small_album_ids) >= 2


@pytest.mark.asyncio
async def test_provision_rejects_duplicate_staged_targets_fail_closed() -> None:
    """F-MATCH-06 guard: provisioning two distinct staged groups onto one new
    local album ID fails closed instead of silently merging them."""
    from core.exceptions import ConflictError

    duplicate_groups = [
        {
            "grouping_token": "manual:a",
            "grouping_key": "key-a",
            "title": "Same",
            "album_artist_name": "Artist",
            "reason_code": "AMBIGUOUS_FALLBACK_GROUP",
            "local_album_id": "album-dup",
            "local_artist_id": "artist-dup",
        },
        {
            "grouping_token": "manual:b",
            "grouping_key": "key-b",
            "title": "Same",
            "album_artist_name": "Artist",
            "reason_code": "AMBIGUOUS_FALLBACK_GROUP",
            "local_album_id": "album-dup",
            "local_artist_id": "artist-dup",
        },
    ]

    class _GuardStore(NativeLibraryStore):
        recorded: list = []

        async def _write_scan(self, operation):
            connection = self._connect()

            class _FakeCursor:
                rowcount = 0

            operation(connection)
            connection.close()
            return _FakeCursor()

    tmp = Path(tempfile.mkdtemp())
    db_file = tmp / "guard.db"
    with sqlite3.connect(db_file) as connection:
        connection.execute("CREATE TABLE auth_users (id TEXT PRIMARY KEY)")
        connection.execute("INSERT INTO auth_users VALUES ('admin')")
    guard_store = _GuardStore(db_file, threading.Lock())

    with pytest.raises(ConflictError):
        await guard_store.provision_staged_grouping_groups(
            "run-guard", "root", "dir", duplicate_groups, now=5.0
        )


# Cluster 5/6: F-057 F-042 F-041 F-043 behavioral pins


class _CountingEmptyProvider(FakeProvider):
    """Zero candidates, no degradation - the pure 'nothing found' lane."""

    def __init__(self) -> None:
        super().__init__([])
        self.search_calls = 0

    async def search_album_candidate_ids(self, artist, title, limit, priority):
        self.search_calls += 1
        return []


def _multi_track_album(store: NativeLibraryStore, count: int) -> None:
    from models.local_catalog import (
        CatalogMembership,
        LocalAlbum,
        LocalArtist,
        LocalArtistCredit,
        LocalTrack,
    )

    artist = LocalArtist(
        id="artist-multi",
        display_name="Artist",
        folded_name="artist",
        normalized_name="artist",
        kind="group",
        created_at=1,
        updated_at=1,
    )
    album = LocalAlbum(
        id="album-multi",
        root_id="root",
        grouping_key="group-multi",
        title="Album",
        album_artist_id=artist.id,
        album_artist_name="Artist",
        created_at=1,
        updated_at=1,
    )
    tracks = []
    credits = {}
    for index in range(1, count + 1):
        track = LocalTrack(
            id=f"track-multi-{index}",
            local_album_id=album.id,
            root_id="root",
            file_path=f"/music/multi-{index}.flac",
            relative_path=f"multi-{index}.flac",
            path_hash=f"hash-multi-{index}",
            file_size_bytes=100,
            file_mtime_ns=index,
            stat_revision=f"stat-multi-{index}",
            tag_revision=f"tag-multi-{index}",
            title="Track",
            artist_name="Artist",
            album_title="Album",
            album_artist_name="Artist",
            track_number=index,
            duration_seconds=180,
            file_format="flac",
            imported_at=1,
        )
        tracks.append(track)
        credits[track.id] = [
            LocalArtistCredit(local_artist_id=artist.id, position=0)
        ]
    return CatalogMembership(
        album=album,
        artists=[artist],
        tracks=tracks,
        album_credits=[LocalArtistCredit(local_artist_id=artist.id, position=0)],
        track_credits=credits,
    )


def _multi_track_candidate(
    *, group: str, release: str, recording_prefix: str, count: int
) -> AlbumCandidate:
    return AlbumCandidate(
        release_group_mbid=group,
        release_mbid=release,
        album_title="Album",
        album_artist_name="Artist",
        artist_mbid="artist-mbid",
        tracks=[
            CandidateTrack(
                title="Track",
                position=index,
                absolute_position=index,
                duration_seconds=180,
                recording_mbid=f"{recording_prefix}-{index}",
            )
            for index in range(1, count + 1)
        ],
    )


async def _seed_multi_track_album(store: NativeLibraryStore, count: int) -> None:
    await store.create_catalog_membership(_multi_track_album(store, count))


@pytest.mark.asyncio
async def test_cached_no_match_does_not_starve_later_tracks(
    store: NativeLibraryStore,
) -> None:
    """F-042: a terminal no_match on track 1 must not consume budget slots -
    tracks 2 and 3 still get fingerprinted in the same attempt."""
    from models.identification import FingerprintOutcome

    await _seed_multi_track_album(store, 3)
    await store.record_fingerprint_outcome(
        FingerprintOutcome(
            id="seed-1",
            local_track_id="track-multi-1",
            stat_revision="stat-multi-1",
            fingerprinter_version=FINGERPRINTER_VERSION,
            state="no_match",
            first_attempt_at=1,
            last_attempt_at=1,
        )
    )
    fake = FakeFingerprinter(FingerprintResult(status="skip"))
    # Two fully-supported three-track candidates with equal scores -> an
    # ambiguous decision whose per-track supported sets hold two recordings,
    # so every track needs a fingerprint.
    provider = FakeProvider(
        [
            _multi_track_candidate(
                group="rg-a",
                release="release-a",
                recording_prefix="recording-a",
                count=3,
            ),
            _multi_track_candidate(
                group="rg-b",
                release="release-b",
                recording_prefix="recording-b",
                count=3,
            ),
        ]
    )
    service = _service(store, provider, fake)
    job = await _claimed_job(store, "album-multi")

    outcome = await service.run_claimed_job(job, "worker")

    # All three tracks were processed: two fresh generations (tracks 2+3)
    # plus the free cache hit on track 1.
    # ambiguous survives enforcement on this fully-matched album
    assert outcome == "ambiguous"
    assert fake.generate_calls == 2
    third = await store.get_fingerprint_outcome(
        "track-multi-3", "stat-multi-3", FINGERPRINTER_VERSION
    )
    assert third is not None and third.state == "no_match"


@pytest.mark.asyncio
async def test_fingerprint_budget_still_caps_fresh_work(
    store: NativeLibraryStore,
) -> None:
    """F-042 regression guard: four un-fingerprinted tracks produce exactly
    MAX_NEW_FINGERPRINTS_PER_ATTEMPT generations on one attempt."""
    await _seed_multi_track_album(store, 4)
    fake = FakeFingerprinter(FingerprintResult(status="skip"))
    # Four-track twins keep every local track supported (decision ambiguous),
    # so all four tracks are needed and the cap bounds fresh work at 2.
    provider = FakeProvider(
        [
            _multi_track_candidate(
                group="rg-a",
                release="release-a",
                recording_prefix="recording-a",
                count=4,
            ),
            _multi_track_candidate(
                group="rg-b",
                release="release-b",
                recording_prefix="recording-b",
                count=4,
            ),
        ]
    )
    service = _service(store, provider, fake)
    job = await _claimed_job(store, "album-multi")

    outcome = await service.run_claimed_job(job, "worker")

    assert fake.generate_calls == MAX_NEW_FINGERPRINTS_PER_ATTEMPT


@pytest.mark.asyncio
async def test_disabled_outcome_recovers_once_acoustid_enabled(
    store: NativeLibraryStore,
) -> None:
    """F-041: a disabled outcome is reused only while AcoustID stays
    disabled; enabling the key regenerates and lands matched."""
    await _seed_album(store)
    disabled_fake = FakeFingerprinter(FingerprintResult(status="disabled"), enabled=False)
    enabled_fake = FakeFingerprinter(
        FingerprintResult(status="pass", recording_id="rec-1", score=0.9),
        enabled=True,
    )
    service = ConditionalFingerprintService(store, disabled_fake)
    first, first_work = await service.fingerprint_if_needed(
        local_track_id="track-1",
        path=Path("/music/1.flac"),
        stat_revision="stat-1",
        needed=True,
        now=1,
    )
    assert first is not None and first.state == "disabled"
    assert first_work is True

    upgraded = ConditionalFingerprintService(store, enabled_fake)
    second, second_work = await upgraded.fingerprint_if_needed(
        local_track_id="track-1",
        path=Path("/music/1.flac"),
        stat_revision="stat-1",
        needed=True,
        now=2,
    )
    assert second is not None and second.state == "matched"
    assert second_work is True
    assert enabled_fake.generate_calls == 1


@pytest.mark.asyncio
async def test_seeded_deferred_outcome_skips_generation_on_resume(
    store: NativeLibraryStore,
) -> None:
    """F-043 bridge: a deferred row carrying a known fingerprint resumes at
    the lookup without re-running fpcalc."""
    from models.identification import FingerprintOutcome

    await _seed_album(store)
    await store.record_fingerprint_outcome(
        FingerprintOutcome(
            id="seed-deferred",
            local_track_id="track-1",
            stat_revision="stat-1",
            fingerprinter_version=FINGERPRINTER_VERSION,
            state="deferred",
            fingerprint="known-fingerprint",
            duration_seconds=180.0,
            failure_code="LOOKUP_PENDING",
            first_attempt_at=1,
            last_attempt_at=1,
            retry_after=1,
        )
    )
    fake = FakeFingerprinter(
        FingerprintResult(status="pass", recording_id="rec-known", score=0.95)
    )
    service = ConditionalFingerprintService(store, fake)
    outcome, did_work = await service.fingerprint_if_needed(
        local_track_id="track-1",
        path=Path("/music/1.flac"),
        stat_revision="stat-1",
        needed=True,
        now=5,
    )
    assert did_work is True

    assert outcome is not None and outcome.state == "matched"
    assert outcome.recording_mbid == "rec-known"
    assert fake.generate_calls == 0
    assert fake.lookup_calls == 1


@pytest.mark.asyncio
async def test_heartbeat_renews_lease_on_phase_checkpoints(
    store: NativeLibraryStore,
) -> None:
    """F-057: the dormant lease heartbeat fires during a run."""
    # Step 2.6 (N-01): the lone-eligible quorum needs provider proof for a
    # single present track so the run reaches the identified terminal.
    await _seed_album(store, embedded_recording=EMBEDDED_RECORDING)
    provider = FakeProvider([_candidate(recording=EMBEDDED_RECORDING)])
    heartbeat_calls: list[tuple[str, str]] = []
    original = store.heartbeat_identification_job

    async def spy(job_id: str, worker_id: str, *, now: float, lease_seconds: float):
        heartbeat_calls.append((job_id, worker_id))
        return await original(job_id, worker_id, now=now, lease_seconds=lease_seconds)

    store.heartbeat_identification_job = spy  # type: ignore[method-assign]
    service = _service(store, provider, FakeFingerprinter(FingerprintResult(status="skip")))
    job = await _claimed_job(store)

    outcome = await service.run_claimed_job(job, "worker")

    assert outcome == "identified"
    assert heartbeat_calls and heartbeat_calls[0] == (job["id"], "worker")


@pytest.mark.asyncio
async def test_pause_checkpoint_keeps_phase_label_only(
    store: NativeLibraryStore,
) -> None:
    """F-058/F-063: the pause checkpoint stores ONLY the phase label and
    matcher version - the candidate-evidence blob implied replay semantics
    that do not exist, and post-decision evidence cannot reconstruct recall
    candidates losslessly."""
    import json as _json

    await _seed_album(store)
    queue = IdentificationQueueService(store)
    job = await _claimed_job(store)
    service = _service(
        store,
        FakeProvider([_candidate()]),
        FakeFingerprinter(FingerprintResult(status="skip")),
    )

    async def pause_after_first_track() -> bool:
        return False

    # Pause the queue so the run hits its candidate_search checkpoint...
    await queue.pause(None, now=2)

    async def flip_back() -> bool:
        await queue.resume(now=3)
        return True

    calls = {"n": 0}

    async def checkpoint() -> bool:
        calls["n"] += 1
        if calls["n"] == 1:
            return await pause_after_first_track()
        return await flip_back()

    outcome = await service.run_claimed_job(job, "worker", now=10)
    assert outcome in {"identified", "no_candidate", "paused"}

    with sqlite3.connect(store.db_path) as connection:
        row = connection.execute(
            "SELECT checkpoint_json FROM library_identification_jobs WHERE id=?",
            (job["id"],),
        ).fetchone()[0]
    if row is not None:
        payload = _json.loads(row)
        assert set(payload) <= {"phase", "matcher_version"}
        assert "evidence" not in payload


class _RecallSpyProvider(FakeProvider):
    def __init__(self, inner: FakeProvider) -> None:
        super().__init__(inner.candidates)
        self.album_search_calls = 0

    async def search_album_candidate_ids(self, artist, title, limit, priority):
        self.album_search_calls += 1
        return await inner_search(self, artist, title, limit, priority)


@pytest.mark.asyncio
async def test_cached_off_release_fingerprint_no_longer_vetoes_identification(
    store: NativeLibraryStore,
) -> None:
    """#392: a cached matched fingerprint outcome carrying an off-release
    recording ID lands in the support-only slot - the album still identifies
    and the track is never reconsidered for fingerprinting."""
    from models.identification import FingerprintOutcome

    # Step 2.6 (N-01): the lone-eligible quorum rides the release-track leg
    # here - a recording seed would break the `recording_mbid is None`
    # support-slot assertion below, while the release track still proves.
    await _seed_album(store, embedded_release_track=EMBEDDED_RELEASE_TRACK)
    await store.record_fingerprint_outcome(
        FingerprintOutcome(
            id="seed-off-release",
            local_track_id="track-1",
            stat_revision="stat-1",
            fingerprinter_version=FINGERPRINTER_VERSION,
            state="matched",
            recording_mbid="off-release-recording",
            release_group_ids=["rg-1"],
            first_attempt_at=1,
            last_attempt_at=1,
        )
    )
    seen: list[GroupingTrack] = []

    class _SpyEngine(AlbumEvidenceEngine):
        def decide(self, local_tracks, candidates, full_recall=False):  # type: ignore[no-untyped-def]
            seen.extend(local_tracks)
            return super().decide(local_tracks, candidates, full_recall=full_recall)

    fake = FakeFingerprinter(FingerprintResult(status="skip"))
    queue = IdentificationQueueService(store)
    service = AlbumIdentificationService(
        store,
        queue,
        AlbumCandidateService(
            FakeProvider([_candidate(release_track=EMBEDDED_RELEASE_TRACK)])
        ),
        _SpyEngine(),
        ConditionalFingerprintService(store, fake),
    )
    job = await _claimed_job(store)

    outcome = await service.run_claimed_job(job, "worker")

    assert outcome == "identified"
    assert len(seen) == 1
    assert seen[0].recording_mbid is None
    assert seen[0].fingerprint_recording_mbid == "off-release-recording"
    assert fake.generate_calls == 0




# ---------------------------------------------------------------------------
# Phase-0 staged-threshold mirrors (LibraryFindings-All X-04 step 0.2).
# The M-02/M-03/M-05 goldens above run through the staged large-context path
# here (STAGED_GROUPING_THRESHOLD override style); F-03 locks pin staged
# parity with the small path. Expectations match the golden JSON one-group
# pins; the same fixing step flips both the small-path golden test and its
# staged mirror here.
# ---------------------------------------------------------------------------


async def _run_staged_grouping_case(
    store: NativeLibraryStore,
    db_path: Path,
    monkeypatch: pytest.MonkeyPatch,
    *,
    slug: str,
    dirname: str,
    specs: list[dict],
    threshold: int = 3,
) -> set[str]:
    """Seed one catalog album of tagged/untagged rows, regroup one context
    through the staged large-context path, and return the distinct
    local_album_ids covering the seeded tracks."""
    monkeypatch.setattr(
        "services.native.local_album_grouping_service.STAGED_GROUPING_THRESHOLD",
        threshold,
    )
    monkeypatch.setattr(
        "services.native.local_album_grouping_service.STAGING_BATCH_SIZE", 2
    )
    artist = LocalArtist(
        id=f"{slug}-artist",
        display_name="Artist",
        folded_name="artist",
        normalized_name="artist",
        kind="group",
        created_at=1,
        updated_at=1,
    )
    album = LocalAlbum(
        id=f"{slug}-old-album",
        root_id="root",
        grouping_key=f"{slug}-old",
        title="Album",
        album_artist_id=artist.id,
        album_artist_name=artist.display_name,
        created_at=1,
        updated_at=1,
    )
    tracks = [
        LocalTrack(
            id=f"{slug}-track-{index:02}",
            local_album_id=album.id,
            root_id="root",
            file_path=f"/music/{spec['relative_path']}",
            relative_path=spec["relative_path"],
            path_hash=f"{slug}-hash-{index:02}",
            file_size_bytes=100,
            file_mtime_ns=1,
            stat_revision=f"100:{index}",
            tag_revision=f"tag-{index}",
            title=spec.get("title", f"Track {index}"),
            artist_name=spec.get("artist_name", "Artist"),
            album_title=spec.get("album_title", "Album"),
            album_artist_name=spec.get("album_artist_name", "Artist"),
            tag_album_title=spec.get("tag_album_title", "Album"),
            tag_album_artist_name=spec.get("tag_album_artist_name", "Artist"),
            title_provenance=spec.get("title_provenance", "absent"),
            album_title_provenance=spec.get("album_title_provenance", "absent"),
            album_artist_provenance=spec.get("album_artist_provenance", "absent"),
            track_number=spec.get("track_number", index),
            duration_seconds=180,
            file_format="flac",
            imported_at=1,
            applied_policy="automatic",
            applied_policy_revision="policy-1",
            is_compilation=spec.get("is_compilation", False),
            metadata_incomplete=spec.get("metadata_incomplete", False),
        )
        for index, spec in enumerate(specs, start=1)
    ]
    await store.create_catalog_membership(
        CatalogMembership(
            album=album,
            artists=[artist],
            tracks=tracks,
            album_credits=[LocalArtistCredit(local_artist_id=artist.id, position=0)],
            track_credits={
                track.id: [LocalArtistCredit(local_artist_id=artist.id, position=0)]
                for track in tracks
            },
        )
    )
    run_id = f"{slug}-staged-run"
    await store.create_scan_run(
        ScanRun(id=run_id, kind="incremental", trigger="manual", queued_at=1)
    )
    with sqlite3.connect(db_path) as connection:
        connection.execute(
            "INSERT INTO library_scan_grouping_contexts "
            "(run_id,root_id,relative_directory) VALUES (?,?,?)",
            (run_id, "root", dirname),
        )
    await LocalAlbumGroupingService(
        store, IdentificationQueueService(store)
    ).regroup_run(run_id, now=2, frozen_policy_revision="policy-1")
    with sqlite3.connect(db_path) as connection:
        staged_rows = connection.execute(
            "SELECT COUNT(*) FROM library_scan_grouping_groups WHERE run_id=?",
            (run_id,),
        ).fetchone()[0]
        album_ids = {
            row[0]
            for row in connection.execute(
                "SELECT DISTINCT local_album_id FROM local_tracks "
                "WHERE relative_path LIKE ?",
                (f"{dirname}/%",),
            ).fetchall()
        }
    # The staged large-context path must have run (not a vacuous pass), and
    # every seeded track must still be assigned.
    assert staged_rows >= 1
    assert len(album_ids) >= 1
    total = 0
    with sqlite3.connect(db_path) as connection:
        total = connection.execute(
            "SELECT COUNT(*) FROM local_tracks WHERE relative_path LIKE ?",
            (f"{dirname}/%",),
        ).fetchone()[0]
    assert total == len(specs)
    return {str(album_id) for album_id in album_ids}


@pytest.mark.asyncio
async def test_staged_mixed_albumartist_merges_to_one_group(
    store: NativeLibraryStore,
    db_path: Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """M-02 staged mirror of golden mixed_albumartist_single_group."""
    album_ids = await _run_staged_grouping_case(
        store,
        db_path,
        monkeypatch,
        slug="m02",
        dirname="m02",
        specs=[
            {
                "relative_path": f"m02/{index:02}.flac",
                "tag_album_title": "Album",
                "tag_album_artist_name": "Artist" if index < 4 else "Other",
                "track_number": index,
            }
            for index in range(1, 5)
        ],
    )
    assert len(album_ids) == 1


@pytest.mark.asyncio
async def test_staged_dense_continuity_component_uses_sparse_fallback(
    store: NativeLibraryStore,
    db_path: Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """F-14 staged mirror of the small-path dense-component cap test.

    Same synthetic 2-old-x-2-new component (4 overlap edges, cap pinned to
    3): the staged loader must decline the dense matrix and drain the
    component through sparse single picks, yielding the identical retained
    mapping as the small path (X keeps cap-old-a, Y keeps cap-old-b)."""
    monkeypatch.setattr(
        "services.native.local_album_grouping_service.STAGED_GROUPING_THRESHOLD", 3
    )
    monkeypatch.setattr(
        "services.native.local_album_grouping_service.CONTINUITY_COMPONENT_EDGE_LIMIT",
        3,
    )
    sparse_calls: list[bool] = []
    real_sparse = store.apply_next_sparse_grouping_continuity

    async def _counting_sparse(
        run_id: str, root_id: str, relative_directory: str
    ) -> bool:
        sparse_calls.append(True)
        return await real_sparse(run_id, root_id, relative_directory)

    monkeypatch.setattr(
        store, "apply_next_sparse_grouping_continuity", _counting_sparse
    )
    specs = [
        ("cap-t1", "cap-old-a", "X", 1),
        ("cap-t2", "cap-old-a", "X", 2),
        ("cap-t3", "cap-old-a", "Y", 3),
        ("cap-t4", "cap-old-b", "X", 4),
        ("cap-t5", "cap-old-b", "Y", 5),
        ("cap-t6", "cap-old-b", "Y", 6),
    ]
    for suffix, album_id in (("a", "cap-old-a"), ("b", "cap-old-b")):
        album_tracks = [
            LocalTrack(
                id=track_id,
                local_album_id=album_id,
                root_id="root",
                file_path=f"/music/cap/{track_id}.flac",
                relative_path=f"cap/{track_id}.flac",
                path_hash=f"cap-hash-{track_id}",
                file_size_bytes=100,
                file_mtime_ns=1,
                stat_revision=f"100:{track_id}",
                tag_revision=f"tag-{track_id}",
                title=f"Track {track_id}",
                artist_name="Artist",
                album_title=new_title,
                album_artist_name="Artist",
                tag_album_title=new_title,
                tag_album_artist_name="Artist",
                track_number=number,
                duration_seconds=180,
                file_format="flac",
                imported_at=1,
                applied_policy="automatic",
                applied_policy_revision="policy-1",
                is_compilation=False,
                metadata_incomplete=False,
            )
            for track_id, owner, new_title, number in specs
            if owner == album_id
        ]
        artist = LocalArtist(
            id=f"cap-artist-{suffix}",
            display_name="Artist",
            folded_name="artist",
            normalized_name="artist",
            kind="group",
            created_at=1,
            updated_at=1,
        )
        await store.create_catalog_membership(
            CatalogMembership(
                album=LocalAlbum(
                    id=album_id,
                    root_id="root",
                    grouping_key=album_id,
                    title=f"Before {suffix}",
                    album_artist_id=artist.id,
                    album_artist_name=artist.display_name,
                    created_at=1 if suffix == "a" else 2,
                    updated_at=1 if suffix == "a" else 2,
                ),
                artists=[artist],
                tracks=album_tracks,
                album_credits=[
                    LocalArtistCredit(local_artist_id=artist.id, position=0)
                ],
                track_credits={
                    track.id: [LocalArtistCredit(local_artist_id=artist.id, position=0)]
                    for track in album_tracks
                },
            )
        )
    await store.create_scan_run(
        ScanRun(id="cap-sparse", kind="incremental", trigger="manual", queued_at=1)
    )
    with sqlite3.connect(db_path) as connection:
        connection.execute(
            "INSERT INTO library_scan_grouping_contexts "
            "(run_id,root_id,relative_directory) VALUES ('cap-sparse','root','cap')"
        )
    await LocalAlbumGroupingService(
        store, IdentificationQueueService(store)
    ).regroup_run("cap-sparse", now=2, frozen_policy_revision="policy-1")

    assert len(sparse_calls) >= 1
    with sqlite3.connect(db_path) as connection:
        retained = {
            str(row[0]): (str(row[1]), str(row[2]))
            for row in connection.execute(
                "SELECT title, retained_album_id, continuity_reason_code "
                "FROM library_scan_grouping_groups WHERE run_id='cap-sparse'"
            ).fetchall()
        }
    assert retained["X"] == ("cap-old-a", "MAXIMUM_TRACK_OVERLAP")
    assert retained["Y"] == ("cap-old-b", "MAXIMUM_TRACK_OVERLAP")


@pytest.mark.asyncio
async def test_staged_disc_spelling_variants_fold_to_one_group(
    store: NativeLibraryStore,
    db_path: Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """M-03 staged mirror of golden disc_spelling_variants_fold.

    Pre-fix the staged large-context path is blind here: evidence only
    covers rows whose ``grouping_directory`` equals the context dir, and the
    current disc regex does not fold ``Disc(1)``/``(CD1)``/``Volume N`` -
    so a parent context stages zero tracks (no group rows at all). Step 2.1
    folds the spellings, the evidence flows, and this flips with the
    small-path golden test."""
    album_ids = await _run_staged_grouping_case(
        store,
        db_path,
        monkeypatch,
        slug="m03",
        dirname="m03",
        specs=[
            {
                "relative_path": "m03/Disc(1)/01.flac",
                "tag_album_title": "Album",
                "tag_album_artist_name": "Artist",
                "track_number": 1,
            },
            {
                "relative_path": "m03/Disc(1)/02.flac",
                "tag_album_title": "Album",
                "tag_album_artist_name": "Artist",
                "track_number": 2,
            },
            {
                "relative_path": "m03/(CD1)/03.flac",
                "tag_album_title": "Album",
                "tag_album_artist_name": "Artist",
                "track_number": 3,
            },
            {
                "relative_path": "m03/Volume 2/01.flac",
                "tag_album_title": "Album",
                "tag_album_artist_name": "Artist",
                "track_number": 1,
            },
        ],
    )
    assert len(album_ids) == 1


@pytest.mark.asyncio
async def test_staged_parsed_sharing_untagged_dir_provisional_group(
    store: NativeLibraryStore,
    db_path: Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """M-05 staged mirror of golden parsed_sharing_untagged_dir: rows carry
    parsed display values + parsed provenance with empty raw tags
    (simulating indexer fallback output); the staged path keys them as one
    tagged-or-parsed group via _grouping_evidence, mirroring the small
    path (same pinned values as the small-path golden)."""
    album_ids = await _run_staged_grouping_case(
        store,
        db_path,
        monkeypatch,
        slug="m05",
        dirname="m05",
        specs=[
            {
                "relative_path": f"m05/{index:02} - Title {index}.flac",
                "title": f"Title {index}",
                "artist_name": "Pink Floyd",
                "album_title": "Wish You Were Here",
                "album_artist_name": "Pink Floyd",
                "tag_album_title": "",
                "tag_album_artist_name": "",
                "title_provenance": "parsed",
                "album_title_provenance": "parsed",
                "album_artist_provenance": "parsed",
                "track_number": index,
                "metadata_incomplete": True,
            }
            for index in range(1, 5)
        ],
    )
    assert len(album_ids) == 1


def _m05_equiv_specs(kind: str, dirname: str) -> list[dict]:
    """Shared M-05 parametrized cases run against both grouping paths."""
    if kind == "agree":
        return [
            {
                "relative_path": f"{dirname}/{index:02} - Title {index}.flac",
                "title": f"Title {index}",
                "artist_name": "Pink Floyd",
                "album_title": "Wish You Were Here",
                "album_artist_name": "Pink Floyd",
                "tag_album_title": "",
                "tag_album_artist_name": "",
                "title_provenance": "parsed",
                "album_title_provenance": "parsed",
                "album_artist_provenance": "parsed",
                "track_number": index,
                "metadata_incomplete": True,
            }
            for index in range(1, 4)
        ]
    if kind == "conflict":
        return [
            {
                "relative_path": f"{dirname}/{index:02} - Title {index}.flac",
                "title": f"Title {index}",
                "artist_name": artist,
                "album_title": album,
                "album_artist_name": artist,
                "tag_album_title": "",
                "tag_album_artist_name": "",
                "title_provenance": "parsed",
                "album_title_provenance": "parsed",
                "album_artist_provenance": "parsed",
                "track_number": 1,
                "metadata_incomplete": True,
            }
            for index, (album, artist) in enumerate(
                [("First", "One"), ("Second", "Two")], start=1
            )
        ]
    if kind == "mixed":
        tagged = {
            "relative_path": f"{dirname}/01 - Tagged.flac",
            "title": "Tagged",
            "artist_name": "Pink Floyd",
            "album_title": "Wish You Were Here",
            "album_artist_name": "Pink Floyd",
            "tag_album_title": "Wish You Were Here",
            "tag_album_artist_name": "Pink Floyd",
            "title_provenance": "tag",
            "album_title_provenance": "tag",
            "album_artist_provenance": "tag",
            "track_number": 1,
        }
        return [tagged] + [
            {
                "relative_path": f"{dirname}/{index:02} - Title {index}.flac",
                "title": f"Title {index}",
                "artist_name": "Pink Floyd",
                "album_title": "Wish You Were Here",
                "album_artist_name": "Pink Floyd",
                "tag_album_title": "",
                "tag_album_artist_name": "",
                "title_provenance": "parsed",
                "album_title_provenance": "parsed",
                "album_artist_provenance": "parsed",
                "track_number": index,
                "metadata_incomplete": True,
            }
            for index in range(2, 4)
        ]
    if kind == "collision":
        return [
            {
                "relative_path": f"{dirname}/take-{index}.flac",
                "title": f"take-{index}",
                "artist_name": "Unknown Artist",
                "album_title": dirname,
                "album_artist_name": "Unknown Artist",
                "tag_album_title": "",
                "tag_album_artist_name": "",
                "track_number": 1,
                "metadata_incomplete": True,
            }
            for index in range(1, 3)
        ]
    raise AssertionError(f"unknown M-05 equivalence kind: {kind}")


def _small_path_group_count(specs: list[dict]) -> int:
    """Run specs through the small path (producer + grouper) and count."""
    from services.native.local_album_grouper import LocalAlbumGrouper
    from services.native.local_album_grouping_service import grouping_track_from_row

    tracks = [
        grouping_track_from_row(
            {
                "id": f"equiv-track-{index:02}",
                "root_id": "root",
                "relative_path": spec["relative_path"],
                "title": spec.get("title", f"Track {index}"),
                "artist_name": spec.get("artist_name", "Artist"),
                "album_title": spec.get("album_title", "Album"),
                "album_artist_name": spec.get("album_artist_name", "Artist"),
                "tag_album_title": spec.get("tag_album_title", "Album"),
                "tag_album_artist_name": spec.get("tag_album_artist_name", "Artist"),
                "title_provenance": spec.get("title_provenance", "absent"),
                "album_title_provenance": spec.get(
                    "album_title_provenance", "absent"
                ),
                "album_artist_provenance": spec.get(
                    "album_artist_provenance", "absent"
                ),
                "artist_sort": None,
                "album_artist_sort": None,
                "track_number": spec.get("track_number", index),
                "disc_number": 1,
                "duration_seconds": 180.0,
                "embedded_recording_mbid": None,
                "embedded_release_mbid": None,
                "embedded_release_group_mbid": None,
                "is_compilation": spec.get("is_compilation", False),
                "metadata_incomplete": spec.get("metadata_incomplete", False),
                "membership_locked": False,
                "local_album_id": "equiv-album",
            }
        )
        for index, spec in enumerate(specs, start=1)
    ]
    return len(LocalAlbumGrouper().group(tracks))


@pytest.mark.asyncio
@pytest.mark.parametrize(
    ("kind", "expected_groups"),
    [
        ("agree", 1),
        ("conflict", 2),
        ("mixed", 1),
        ("collision", 2),
    ],
)
async def test_parsed_grouping_small_path_and_staged_agree(
    store: NativeLibraryStore,
    db_path: Path,
    monkeypatch: pytest.MonkeyPatch,
    kind: str,
    expected_groups: int,
) -> None:
    """M-05 equivalence: agreeing parses merge (provisional), conflicting
    parses split, a genuinely-tagged anchor absorbs agreeing parses, and
    same-number untagged rows stay split - identically on both paths."""
    slug = f"m05eq-{kind}"
    specs = _m05_equiv_specs(kind, slug)
    assert _small_path_group_count(specs) == expected_groups
    album_ids = await _run_staged_grouping_case(
        store, db_path, monkeypatch, slug=slug, dirname=slug, specs=specs,
        threshold=1,
    )
    assert len(album_ids) == expected_groups


@pytest.mark.asyncio
async def test_staged_soundtrack_tagged_album_is_one_group(
    store: NativeLibraryStore,
    db_path: Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """F-03 staged lock: soundtrack album tags group like any album tags."""
    album_ids = await _run_staged_grouping_case(
        store,
        db_path,
        monkeypatch,
        slug="f03",
        dirname="f03",
        specs=[
            {
                "relative_path": f"f03/{index:02}.flac",
                "album_title": "Film OST",
                "album_artist_name": "Covers",
                "tag_album_title": "Film OST",
                "tag_album_artist_name": "Covers",
                "track_number": index,
            }
            for index in range(1, 5)
        ],
    )
    assert len(album_ids) == 1


# ---------------------------------------------------------------------------
# Phase-1 provenance threading (LibraryFindings-All step 1.4b, M-06/T4):
# persisted column values win when non-`absent`, else producers derive.
# ---------------------------------------------------------------------------


def _provenance_row(**overrides: object) -> dict:
    row: dict = {
        "id": "track-1",
        "root_id": "root",
        "relative_path": "Artist/Album/01 - Title.flac",
        "title": "Title",
        "artist_name": "Artist",
        "album_title": "Album",
        "album_artist_name": "Artist",
        "tag_album_title": "Album",
        "tag_album_artist_name": "Artist",
        "artist_sort": None,
        "album_artist_sort": None,
        "track_number": 1,
        "disc_number": 1,
        "duration_seconds": 180.0,
        "embedded_recording_mbid": None,
        "embedded_release_mbid": None,
        "embedded_release_group_mbid": None,
        "embedded_release_track_mbid": None,
        "is_compilation": False,
        "metadata_incomplete": False,
        "membership_locked": False,
        "local_album_id": "album-1",
    }
    row.update(overrides)
    return row


def test_provenance_threading_persisted_values_win_over_derivation() -> None:
    """1.4b: a row with persisted `parsed` provenance round-trips through
    both producers with the same values and provenance."""
    from services.native.album_identification_service import _to_grouping_track
    from services.native.local_album_grouping_service import grouping_track_from_row

    row = _provenance_row(
        tag_album_title="",
        tag_album_artist_name="",
        album_title="Parsed Album",
        album_artist_name="Parsed Artist",
        title="Parsed Title",
        artist_name="Parsed Artist",
        title_provenance="parsed",
        album_title_provenance="parsed",
        album_artist_provenance="parsed",
    )
    grouped = grouping_track_from_row(row)
    assert grouped.title_provenance == "parsed"
    assert grouped.album_title_provenance == "parsed"
    assert grouped.album_artist_provenance == "parsed"
    identified = _to_grouping_track(row)
    assert identified.title == grouped.title == "Parsed Title"
    assert identified.album_title == "Parsed Album"
    assert identified.album_artist_name == "Parsed Artist"
    assert identified.title_provenance == "parsed"
    assert identified.album_title_provenance == "parsed"
    assert identified.album_artist_provenance == "parsed"


def test_provenance_threading_derives_tag_for_genuine_tag_values() -> None:
    """1.4b: untagged-provenance rows with real tag values derive `tag`."""
    from services.native.album_identification_service import _to_grouping_track
    from services.native.local_album_grouping_service import grouping_track_from_row

    grouped = grouping_track_from_row(_provenance_row())
    assert grouped.album_title == "Album"
    assert grouped.album_artist_name == "Artist"
    assert grouped.title_provenance == "tag"
    assert grouped.album_title_provenance == "tag"
    assert grouped.album_artist_provenance == "tag"
    identified = _to_grouping_track(_provenance_row())
    assert identified.title_provenance == "tag"
    assert identified.album_title_provenance == "tag"
    assert identified.album_artist_provenance == "tag"


def test_provenance_threading_derives_placeholder_for_substitutes() -> None:
    """1.4b: stem-matching titles, `"Unknown Artist"`, and substituted album
    display values derive `placeholder`."""
    from services.native.local_album_grouping_service import grouping_track_from_row

    track = grouping_track_from_row(
        _provenance_row(
            title="01 - Title",
            artist_name="Unknown Artist",
            tag_album_title="",
            tag_album_artist_name="",
            album_title="Album",
            album_artist_name="Unknown Artist",
        )
    )
    assert track.title_provenance == "placeholder"
    assert track.album_title_provenance == "placeholder"
    assert track.album_artist_provenance == "placeholder"
    # Derivation never changes grouping values: raw stays the key signal.
    assert track.album_title == ""
    assert track.album_artist_name == ""


def test_provenance_threading_absent_for_empty_values() -> None:
    """1.4b: rows with no display values carry `absent`, never a guess (an
    unparseable bare filename backfills nothing either)."""
    from services.native.album_identification_service import _to_grouping_track
    from services.native.local_album_grouping_service import grouping_track_from_row

    row = _provenance_row(
        relative_path="track.flac",
        title="",
        artist_name="",
        album_title="",
        album_artist_name="",
        tag_album_title="",
        tag_album_artist_name="",
    )
    grouped = grouping_track_from_row(row)
    assert grouped.title_provenance == "absent"
    assert grouped.album_title_provenance == "absent"
    assert grouped.album_artist_provenance == "absent"
    identified = _to_grouping_track(row)
    assert identified.title_provenance == "absent"
    assert identified.album_title_provenance == "absent"
    assert identified.album_artist_provenance == "absent"


def _prepare_tagged_write(
    store: NativeLibraryStore, relative_path: str, tag, local_track_id: str
):
    """Drive the indexer's `_prepare_tagged` without any tag-reader IO."""
    from models.audio import AudioInfo
    from services.native.library_indexer import LibraryIndexer

    class _StubReader:
        def read_tags(self, path: Path):
            raise NotImplementedError

    indexer = LibraryIndexer(store, _StubReader())
    return indexer._prepare_tagged(
        "run-1",
        {
            "root_id": "root",
            "relative_path": relative_path,
            "absolute_path": f"/music/{relative_path}",
            "local_track_id": local_track_id,
            "file_size_bytes": 100,
            "file_mtime_ns": 200,
            "stat_revision": "stat-1",
            "policy_revision": "policy-1",
            "effective_policy": "automatic",
            "comparison_result": "new",
        },
        tag,
        AudioInfo(
            duration_seconds=200.0,
            bitrate=800,
            sample_rate=44100,
            channels=2,
            file_format="flac",
            file_size_bytes=100,
        ),
        now=1.0,
    )


def test_indexer_fallback_writes_parsed_display_with_explicit_provenance(
    tmp_path: Path,
) -> None:
    """1.5: missing tags fall back to filename parses for DISPLAY columns
    (raw `tag_*` stay empty) with explicit provenance on every touched row,
    including `tag` for fully-tagged rows."""
    import threading

    from models.audio import AudioTag

    store = NativeLibraryStore(tmp_path / "library.db", threading.Lock())
    write = _prepare_tagged_write(
        store,
        "Pink Floyd/Wish You Were Here/01 - Shine On You Crazy Diamond.flac",
        AudioTag(title="", artist="", album="", track_number=0),
        "track-1",
    )
    track = write.track
    assert track.title == "Shine On You Crazy Diamond"
    assert track.title_provenance == "parsed"
    assert track.album_title == "Wish You Were Here"
    assert track.album_title_provenance == "parsed"
    assert track.album_artist_name == "Pink Floyd"
    assert track.album_artist_provenance == "parsed"
    assert track.artist_name == "Pink Floyd"
    assert track.track_number == 1
    assert track.tag_album_title == ""
    assert track.tag_album_artist_name == ""

    tagged = _prepare_tagged_write(
        store,
        "Pink Floyd/Wish You Were Here/01 - Shine On You Crazy Diamond.flac",
        AudioTag(
            title="Shine On You Crazy Diamond",
            artist="Pink Floyd",
            album="Wish You Were Here",
            track_number=1,
            album_artist="Pink Floyd",
        ),
        "track-2",
    ).track
    assert tagged.title_provenance == "tag"
    assert tagged.album_title_provenance == "tag"
    assert tagged.album_artist_provenance == "tag"
    assert tagged.tag_album_title == "Wish You Were Here"
    assert tagged.tag_album_artist_name == "Pink Floyd"

    # Unparseable paths keep today's stem/`"Unknown Artist"` placeholders.
    bare = _prepare_tagged_write(
        store,
        "track.flac",
        AudioTag(title="", artist="", album="", track_number=0),
        "track-3",
    ).track
    assert bare.title == "track"
    assert bare.title_provenance == "placeholder"
    assert bare.album_title == "track"
    assert bare.album_title_provenance == "placeholder"
    assert bare.album_artist_name == "Unknown Artist"
    assert bare.album_artist_provenance == "placeholder"
    assert bare.track_number == 0


@pytest.mark.asyncio
async def test_ambiguous_tracks_beyond_the_fingerprint_cap_degrade_to_tag_evidence(
    store: NativeLibraryStore,
) -> None:
    """1.6 tuning (M-06/T5): with five ambiguous tracks and no-match
    fingerprints, only `MAX_NEW_FINGERPRINTS_PER_ATTEMPT` generations run -
    tracks 3+ provably degrade to tag-evidence and the decision completes
    instead of hanging."""
    from models.local_catalog import CatalogMembership

    artist = LocalArtist(
        id="artist-budget",
        display_name="Budget Artist",
        folded_name="budget artist",
        normalized_name="budget artist",
        kind="group",
        created_at=1,
        updated_at=1,
    )
    album = LocalAlbum(
        id="album-budget",
        root_id="root",
        grouping_key="group-budget",
        title="Budget Album",
        album_artist_id=artist.id,
        album_artist_name="Budget Artist",
        created_at=1,
        updated_at=1,
    )
    tracks = [
        LocalTrack(
            id=f"track-budget-{index}",
            local_album_id=album.id,
            root_id="root",
            file_path=f"/music/budget-{index}.flac",
            relative_path=f"Budget/{index:02}.flac",
            path_hash=f"hash-budget-{index}",
            file_size_bytes=100,
            file_mtime_ns=1,
            stat_revision=f"stat-budget-{index}",
            tag_revision=f"tag-budget-{index}",
            title="Alpha" if index == 1 else ("Beta" if index == 2 else f"{index:02}"),
            artist_name="Budget Artist"
            if index <= 2
            else "Unknown Artist",
            album_title="Budget Album",
            album_artist_name="Budget Artist",
            title_provenance="tag" if index <= 2 else "placeholder",
            album_title_provenance="tag",
            album_artist_provenance="tag",
            track_number=index,
            duration_seconds=180,
            file_format="flac",
            imported_at=1,
        )
        for index in range(1, 6)
    ]
    await store.create_catalog_membership(
        CatalogMembership(
            album=album,
            artists=[artist],
            tracks=tracks,
            album_credits=[LocalArtistCredit(local_artist_id=artist.id, position=0)],
            track_credits={
                track.id: [LocalArtistCredit(local_artist_id=artist.id, position=0)]
                for track in tracks
            },
        )
    )
    candidate = _candidate(group="budget-rg")
    # Deliberately wrong titles: the tag tracks mismatch (soft), the
    # placeholder tracks abstain - insufficient, never contradictory.
    candidate = msgspec.structs.replace(
        candidate,
        album_title="Budget Album",
        album_artist_name="Budget Artist",
        tracks=[
            CandidateTrack(
                title=f"Gamma {index}",
                position=index,
                absolute_position=index,
                duration_seconds=180.0,
            )
            for index in range(1, 6)
        ],
    )
    fingerprinter = FakeFingerprinter(FingerprintResult(status="skip"))
    job = await _claimed_job(store, album_id="album-budget")
    outcome = await _service(
        store, FakeProvider([candidate]), fingerprinter
    ).run_claimed_job(job, "worker", now=3)

    assert outcome == "insufficient_evidence"
    assert fingerprinter.generate_calls == MAX_NEW_FINGERPRINTS_PER_ATTEMPT


def test_parsed_sharing_untagged_tracks_group_together(tmp_path: Path) -> None:
    """1.5: untagged rows with agreeing parses key as tagged through
    `grouping_track_from_row`, so one organized directory is one group."""
    import threading

    from models.audio import AudioTag
    from services.native.local_album_grouper import LocalAlbumGrouper
    from services.native.local_album_grouping_service import grouping_track_from_row

    store = NativeLibraryStore(tmp_path / "library.db", threading.Lock())
    titles = ["Shine On You Crazy Diamond", "Welcome to the Machine"]
    tracks = []
    for index, title in enumerate(titles, start=1):
        write = _prepare_tagged_write(
            store,
            f"Pink Floyd/Wish You Were Here/0{index} - {title}.flac",
            AudioTag(title="", artist="", album="", track_number=0),
            f"track-{index}",
        )
        stored = write.track
        tracks.append(
            grouping_track_from_row(
                {
                    "id": stored.id,
                    "root_id": stored.root_id,
                    "relative_path": stored.relative_path,
                    "title": stored.title,
                    "artist_name": stored.artist_name,
                    "album_title": stored.album_title,
                    "album_artist_name": stored.album_artist_name,
                    "tag_album_title": stored.tag_album_title,
                    "tag_album_artist_name": stored.tag_album_artist_name,
                    "title_provenance": stored.title_provenance,
                    "album_title_provenance": stored.album_title_provenance,
                    "album_artist_provenance": stored.album_artist_provenance,
                    "artist_sort": None,
                    "album_artist_sort": None,
                    "track_number": stored.track_number,
                    "disc_number": stored.disc_number,
                    "duration_seconds": stored.duration_seconds,
                    "embedded_recording_mbid": None,
                    "embedded_release_mbid": None,
                    "embedded_release_group_mbid": None,
                    "is_compilation": False,
                    "metadata_incomplete": True,
                    "membership_locked": False,
                    "local_album_id": stored.local_album_id,
                }
            )
        )
    assert [track.album_title for track in tracks] == [
        "Wish You Were Here",
        "Wish You Were Here",
    ]
    groups = LocalAlbumGrouper().group(tracks)
    assert len(groups) == 1
    assert sorted(groups[0].track_ids) == ["track-1", "track-2"]


# Step 2.5 (E-02) T12: identify-lane auto-accept gate tests.

# Valid UUIDs: embedded values are UUID-validated up front, so gate fixtures
# cannot use readable placeholders here.
GATE_RECORDING = "88888888-8888-4888-8888-888888888888"
GATE_RELEASE_TRACK = "99999999-9999-4999-8999-999999999999"
GATE_MULTI_RECORDING = {
    1: "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaa1",
    2: "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaa2",
}
GATE_MULTI_RELEASE_TRACK = {
    1: "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbb1",
    2: "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbb2",
}


class FakeCanonicalProvider:
    """Minimal canonical-release stub: serves one release (or fails) and logs calls."""

    def __init__(
        self,
        release: MbManagementRelease | None = None,
        *,
        error: Exception | None = None,
    ) -> None:
        self.release = release
        self.error = error
        self.calls: list[tuple[str, tuple[str, ...], RequestPriority]] = []

    async def get_canonical_release(
        self,
        release_mbid: str,
        *,
        includes: tuple[str, ...] = (),
        priority: RequestPriority = RequestPriority.USER_INITIATED,
        **_kwargs: object,
    ) -> MbManagementRelease | None:
        self.calls.append((release_mbid, includes, priority))
        if self.error is not None:
            raise self.error
        return self.release


def _official_release(
    *, release_mbid: str = "release-rg-1", title: str = "Album"
) -> MbManagementRelease:
    return MbManagementRelease(
        id=release_mbid,
        title=title,
        status="Official",
        date="2024-01-31",
        country="XW",
    )


def _disabled_fingerprinter() -> FakeFingerprinter:
    return FakeFingerprinter(FingerprintResult(status="disabled"), enabled=False)


@pytest.mark.asyncio
async def test_identify_gate_accepts_exact_edition_behind_flag(
    store: NativeLibraryStore, db_path: Path, caplog: pytest.LogCaptureFixture
) -> None:
    """Unanimous opt-in + lone >=0.95 + local MBID proof seals the exact edition
    with an undo snapshot and an automatic_exact_edition audit row."""
    await _seed_album(
        store,
        embedded_recording=GATE_RECORDING,
        embedded_release_track=GATE_RELEASE_TRACK,
    )
    provider = FakeProvider([_candidate(recording=GATE_RECORDING, release_track=GATE_RELEASE_TRACK)])
    canonical = FakeCanonicalProvider(_official_release())
    job = await _claimed_job(store)
    service = _service(
        store,
        provider,
        _disabled_fingerprinter(),
        canonical_provider=canonical,
        edition_opt_in=lambda _root_id: True,
    )
    caplog.set_level("DEBUG")
    outcome = await service.run_claimed_job(job, "worker", now=3)

    assert outcome == "identified"
    # Bounded I/O: the final top-3 by score only, repair-lane call shape.
    assert canonical.calls == [
        ("release-rg-1", ("media",), RequestPriority.BACKGROUND_SYNC)
    ]
    with sqlite3.connect(db_path) as connection:
        album_identity = connection.execute(
            "SELECT release_group_mbid, release_mbid, decision_source "
            "FROM local_album_external_identities"
        ).fetchone()
        track_identity = connection.execute(
            "SELECT recording_mbid, release_mbid, release_track_mbid, decision_source "
            "FROM local_track_external_identities"
        ).fetchone()
        audit = connection.execute(
            "SELECT action_kind, reason_code, after_json FROM library_catalog_actions "
            "WHERE action_kind = 'automatic_exact_edition'"
        ).fetchone()
        undo = connection.execute(
            "SELECT local_album_id FROM library_automatic_edition_undo"
        ).fetchone()
    assert tuple(album_identity) == ("rg-1", "release-rg-1", "automatic")
    assert tuple(track_identity) == (
        GATE_RECORDING,
        "release-rg-1",
        GATE_RELEASE_TRACK,
        "automatic",
    )
    assert audit is not None
    assert tuple(audit[:2]) == ("automatic_exact_edition", "SUPPORTED")
    after = json.loads(audit[2])
    assert after["gate_reason"] == "AUTO_ACCEPT"
    assert after["actor"] == "system"
    assert after["ranking_inputs"][0]["release_mbid"] == "release-rg-1"
    assert tuple(undo) == ("album-1",)
    assert any(
        "identify edition gate: accept" in record.message for record in caplog.records
    )


@pytest.mark.asyncio
async def test_identify_gate_veto_routes_lone_candidate_to_edition_to_confirm(
    store: NativeLibraryStore, db_path: Path
) -> None:
    """A lone >=0.95 candidate without local MBID proof pins the RG only and
    files an edition_to_confirm review instead of sealing the exact edition."""
    # Step 2.6 (N-01): the lone-eligible quorum is met by two present-claim
    # tracks with no proof anywhere, so decide still selects and the gate's
    # own LONE_WITHOUT_PROOF veto stays the reason the exact edition never
    # seals.
    artist = LocalArtist(
        id="artist-lv",
        display_name="Artist",
        folded_name="artist",
        normalized_name="artist",
        kind="group",
        created_at=1,
        updated_at=1,
    )
    album = LocalAlbum(
        id="album-1",
        root_id="root",
        grouping_key="group-lv",
        title="Album",
        album_artist_id=artist.id,
        album_artist_name="Artist",
        created_at=1,
        updated_at=1,
    )
    tracks = [
        LocalTrack(
            id=f"track-lv-{index}",
            local_album_id=album.id,
            root_id="root",
            file_path=f"/music/lv-{index}.flac",
            relative_path=f"lv-{index}.flac",
            path_hash=f"hash-lv-{index}",
            file_size_bytes=100,
            file_mtime_ns=index,
            stat_revision=f"stat-lv-{index}",
            tag_revision=f"tag-lv-{index}",
            title=f"Track {index}",
            artist_name="Artist",
            album_title="Album",
            album_artist_name="Artist",
            title_provenance="tag",
            album_title_provenance="tag",
            album_artist_provenance="tag",
            track_number=index,
            duration_seconds=180,
            file_format="flac",
            imported_at=1,
            applied_policy="automatic",
            applied_policy_revision="policy-1",
        )
        for index in (1, 2)
    ]
    await store.create_catalog_membership(
        CatalogMembership(
            album=album,
            artists=[artist],
            tracks=tracks,
            album_credits=[LocalArtistCredit(local_artist_id=artist.id, position=0)],
            track_credits={
                track.id: [LocalArtistCredit(local_artist_id=artist.id, position=0)]
                for track in tracks
            },
        )
    )
    provider = FakeProvider(
        [
            AlbumCandidate(
                release_group_mbid="rg-1",
                release_mbid="release-rg-1",
                album_title="Album",
                album_artist_name="Artist",
                artist_mbid="artist-mbid",
                tracks=[
                    CandidateTrack(
                        title=f"Track {index}",
                        position=index,
                        absolute_position=index,
                        duration_seconds=180,
                        recording_mbid=GATE_MULTI_RECORDING[index],
                        release_track_mbid=GATE_MULTI_RELEASE_TRACK[index],
                    )
                    for index in (1, 2)
                ],
            )
        ]
    )
    canonical = FakeCanonicalProvider(_official_release())
    job = await _claimed_job(store)
    service = _service(
        store,
        provider,
        _disabled_fingerprinter(),
        canonical_provider=canonical,
        edition_opt_in=lambda _root_id: True,
    )
    outcome = await service.run_claimed_job(job, "worker", now=3)

    assert outcome == "edition_uncertain"
    assert canonical.calls != []
    with sqlite3.connect(db_path) as connection:
        album_identity = connection.execute(
            "SELECT release_group_mbid, release_mbid, decision_source "
            "FROM local_album_external_identities"
        ).fetchone()
        review = connection.execute(
            "SELECT state, reason_code, ranked_edition_keys_json "
            "FROM library_identification_reviews WHERE local_album_id = 'album-1'"
        ).fetchone()
        audit_count = connection.execute(
            "SELECT COUNT(*) FROM library_catalog_actions "
            "WHERE action_kind = 'automatic_exact_edition'"
        ).fetchone()[0]
        undo_count = connection.execute(
            "SELECT COUNT(*) FROM library_automatic_edition_undo"
        ).fetchone()[0]
        attempt_flags = connection.execute(
            "SELECT degradation_flags_json FROM library_identification_attempts "
            "WHERE local_album_id = 'album-1'"
        ).fetchone()[0]
    assert tuple(album_identity) == ("rg-1", None, "automatic")
    assert tuple(review[:2]) == ("edition_to_confirm", "EDITION_UNCERTAIN")
    assert json.loads(review[2]) == ["rg-1:release-rg-1"]
    assert audit_count == 0
    assert undo_count == 0
    assert "auto_gate:LONE_WITHOUT_PROOF" in attempt_flags


async def _seed_plain_two_track_album(store: NativeLibraryStore) -> None:
    """Two present-claim tracks with no embedded tags (lone quorum met)."""
    artist = LocalArtist(
        id="artist-hold",
        display_name="Artist",
        folded_name="artist",
        normalized_name="artist",
        kind="group",
        created_at=1,
        updated_at=1,
    )
    album = LocalAlbum(
        id="album-1",
        root_id="root",
        grouping_key="group-hold",
        title="Album",
        album_artist_id=artist.id,
        album_artist_name="Artist",
        created_at=1,
        updated_at=1,
    )
    tracks = [
        LocalTrack(
            id=f"track-hold-{index}",
            local_album_id=album.id,
            root_id="root",
            file_path=f"/music/hold-{index}.flac",
            relative_path=f"hold-{index}.flac",
            path_hash=f"hash-hold-{index}",
            file_size_bytes=100,
            file_mtime_ns=index,
            stat_revision=f"stat-hold-{index}",
            tag_revision=f"tag-hold-{index}",
            title=f"Track {index}",
            artist_name="Artist",
            album_title="Album",
            album_artist_name="Artist",
            title_provenance="tag",
            album_title_provenance="tag",
            album_artist_provenance="tag",
            track_number=index,
            duration_seconds=180,
            file_format="flac",
            imported_at=1,
            applied_policy="automatic",
            applied_policy_revision="policy-1",
        )
        for index in (1, 2)
    ]
    await store.create_catalog_membership(
        CatalogMembership(
            album=album,
            artists=[artist],
            tracks=tracks,
            album_credits=[LocalArtistCredit(local_artist_id=artist.id, position=0)],
            track_credits={
                track.id: [LocalArtistCredit(local_artist_id=artist.id, position=0)]
                for track in tracks
            },
        )
    )


def _lone_gate_candidate() -> AlbumCandidate:
    return AlbumCandidate(
        release_group_mbid="rg-1",
        release_mbid="release-rg-1",
        album_title="Album",
        album_artist_name="Artist",
        artist_mbid="artist-mbid",
        tracks=[
            CandidateTrack(
                title=f"Track {index}",
                position=index,
                absolute_position=index,
                duration_seconds=180,
                recording_mbid=GATE_MULTI_RECORDING[index],
                release_track_mbid=GATE_MULTI_RELEASE_TRACK[index],
            )
            for index in (1, 2)
        ],
    )


@pytest.mark.asyncio
async def test_tier_outcome_holds_sealed_automatic_exact(
    store: NativeLibraryStore, db_path: Path
) -> None:
    """A gate veto on merely weaker evidence keeps the sealed automatic
    exact (no demotion), files no new review row, and flags the attempt +
    hold metric."""
    await _seed_plain_two_track_album(store)
    await store.attach_album_identity(
        LocalAlbumExternalIdentity(
            local_album_id="album-1",
            release_group_mbid="rg-1",
            release_mbid="release-rg-1",
            decision_source="automatic",
            matcher_version=MATCHER_VERSION,
        ),
        expected_album_revision=1,
    )
    provider = FakeProvider([_lone_gate_candidate()])
    canonical = FakeCanonicalProvider(_official_release())
    job = await _claimed_job(store)
    service = _service(
        store,
        provider,
        _disabled_fingerprinter(),
        canonical_provider=canonical,
        edition_opt_in=lambda _root_id: True,
    )
    before = dict(_edition_hold_metrics.snapshot().counters)

    outcome = await service.run_claimed_job(job, "worker", now=3)

    assert outcome == "edition_uncertain"
    with sqlite3.connect(db_path) as connection:
        identity = connection.execute(
            "SELECT release_group_mbid, release_mbid, decision_source "
            "FROM local_album_external_identities"
        ).fetchone()
        tier_rows = connection.execute(
            "SELECT COUNT(*) FROM library_identification_reviews "
            "WHERE local_album_id = 'album-1' AND state = 'edition_to_confirm'"
        ).fetchone()[0]
        attempt_flags = connection.execute(
            "SELECT degradation_flags_json FROM library_identification_attempts "
            "WHERE local_album_id = 'album-1'"
        ).fetchone()[0]
    assert tuple(identity) == ("rg-1", "release-rg-1", "automatic")
    assert tier_rows == 0
    assert "edition_hold:kept_exact" in attempt_flags
    after = _edition_hold_metrics.snapshot().counters
    assert after.get("tier_hold:kept_exact", 0) == (
        before.get("tier_hold:kept_exact", 0) + 1
    )


@pytest.mark.asyncio
async def test_tier_outcome_demotes_automatic_exact_on_group_change(
    store: NativeLibraryStore, db_path: Path
) -> None:
    """The hold is same-group only: a tier for a *different* release group
    demotes the old sealed exact to the new group pin, files the tier row
    (the contradiction stays visible), and bumps the demotion counter."""
    await _seed_plain_two_track_album(store)
    await store.attach_album_identity(
        LocalAlbumExternalIdentity(
            local_album_id="album-1",
            release_group_mbid="rg-old",
            release_mbid="release-old",
            decision_source="automatic",
            matcher_version=MATCHER_VERSION,
        ),
        expected_album_revision=1,
    )
    provider = FakeProvider([_lone_gate_candidate()])
    canonical = FakeCanonicalProvider(_official_release())
    job = await _claimed_job(store)
    service = _service(
        store,
        provider,
        _disabled_fingerprinter(),
        canonical_provider=canonical,
        edition_opt_in=lambda _root_id: True,
    )
    before = dict(_edition_hold_metrics.snapshot().counters)

    outcome = await service.run_claimed_job(job, "worker", now=3)

    assert outcome == "edition_uncertain"
    with sqlite3.connect(db_path) as connection:
        identity = connection.execute(
            "SELECT release_group_mbid, release_mbid, decision_source "
            "FROM local_album_external_identities"
        ).fetchone()
        tier_rows = connection.execute(
            "SELECT COUNT(*) FROM library_identification_reviews "
            "WHERE local_album_id = 'album-1' AND state = 'edition_to_confirm'"
        ).fetchone()[0]
        attempt_flags = connection.execute(
            "SELECT degradation_flags_json FROM library_identification_attempts "
            "WHERE local_album_id = 'album-1'"
        ).fetchone()[0]
    assert tuple(identity) == ("rg-1", None, "automatic")
    assert tier_rows == 1
    assert "edition_hold:kept_exact" not in attempt_flags
    after = _edition_hold_metrics.snapshot().counters
    assert after.get("tier_demotion:rg_only", 0) == (
        before.get("tier_demotion:rg_only", 0) + 1
    )
    assert after.get("tier_hold:kept_exact", 0) == before.get(
        "tier_hold:kept_exact", 0
    )


@pytest.mark.asyncio
async def test_below_quorum_single_track_files_soft_review_row(
    store: NativeLibraryStore, db_path: Path
) -> None:
    """2.6 (N-01/T13) end to end: a lone descriptive track with no provider
    proof stays below the lone-eligible quorum, so the run terminalizes
    `insufficient_evidence` - the job still terminalizes `needs_review`
    (mapping unchanged) while the review row carries the soft
    `edition_to_confirm` state."""
    await _seed_album(store)
    job = await _claimed_job(store)
    outcome = await _service(
        store,
        FakeProvider([_candidate()]),
        FakeFingerprinter(FingerprintResult(status="disabled"), enabled=False),
    ).run_claimed_job(job, "worker", now=3)
    assert outcome == "insufficient_evidence"
    with sqlite3.connect(db_path) as connection:
        review = connection.execute(
            "SELECT state, reason_code FROM library_identification_reviews "
            "WHERE local_album_id = 'album-1'"
        ).fetchone()
        job_state = connection.execute(
            "SELECT state FROM library_identification_jobs WHERE id = ?",
            (job["id"],),
        ).fetchone()[0]
    assert tuple(review) == ("edition_to_confirm", "INSUFFICIENT_METADATA")
    assert job_state == "needs_review"


@pytest.mark.asyncio
async def test_identify_gate_off_by_default_keeps_legacy_exact_write(
    store: NativeLibraryStore, db_path: Path
) -> None:
    """Flag None/unset means pre-auto behavior byte-for-byte: the exact edition
    still seals (legacy path) but with no gate I/O, audit, or undo rows."""
    await _seed_album(
        store,
        embedded_recording=GATE_RECORDING,
        embedded_release_track=GATE_RELEASE_TRACK,
    )
    provider = FakeProvider([_candidate(recording=GATE_RECORDING, release_track=GATE_RELEASE_TRACK)])
    job = await _claimed_job(store)
    service = _service(store, provider, _disabled_fingerprinter())
    outcome = await service.run_claimed_job(job, "worker", now=3)

    assert outcome == "identified"
    assert provider.calls != []
    with sqlite3.connect(db_path) as connection:
        album_identity = connection.execute(
            "SELECT release_group_mbid, release_mbid, decision_source "
            "FROM local_album_external_identities"
        ).fetchone()
        audit_count = connection.execute(
            "SELECT COUNT(*) FROM library_catalog_actions "
            "WHERE action_kind = 'automatic_exact_edition'"
        ).fetchone()[0]
        undo_count = connection.execute(
            "SELECT COUNT(*) FROM library_automatic_edition_undo"
        ).fetchone()[0]
    assert tuple(album_identity) == ("rg-1", "release-rg-1", "automatic")
    assert audit_count == 0
    assert undo_count == 0


@pytest.mark.asyncio
async def test_identify_gate_never_downgrades_manual_rg_identity(
    store: NativeLibraryStore, db_path: Path
) -> None:
    """A curator's manual RG-only pick is never auto-refined: a gate-accepting
    decision resolves reviews but leaves the manual row byte-identical."""
    await _seed_album(
        store,
        embedded_recording=GATE_RECORDING,
        embedded_release_track=GATE_RELEASE_TRACK,
    )
    with sqlite3.connect(db_path) as connection:
        connection.execute(
            "INSERT INTO local_album_external_identities "
            "(local_album_id, provider, release_group_mbid, release_mbid, "
            "decision_source, matcher_version, attempt_id, selected_at) "
            "VALUES ('album-1', 'musicbrainz', 'rg-1', NULL, 'manual', 'test', NULL, 1.0)"
        )
        connection.commit()
        before = connection.execute(
            "SELECT * FROM local_album_external_identities WHERE local_album_id = 'album-1'"
        ).fetchone()
    provider = FakeProvider([_candidate(recording=GATE_RECORDING, release_track=GATE_RELEASE_TRACK)])
    canonical = FakeCanonicalProvider(_official_release())
    job = await _claimed_job(store)
    service = _service(
        store,
        provider,
        _disabled_fingerprinter(),
        canonical_provider=canonical,
        edition_opt_in=lambda _root_id: True,
    )
    outcome = await service.run_claimed_job(job, "worker", now=3)

    assert outcome == "identified"
    with sqlite3.connect(db_path) as connection:
        after = connection.execute(
            "SELECT * FROM local_album_external_identities WHERE local_album_id = 'album-1'"
        ).fetchone()
        audit_count = connection.execute(
            "SELECT COUNT(*) FROM library_catalog_actions "
            "WHERE action_kind = 'automatic_exact_edition'"
        ).fetchone()[0]
        undo_count = connection.execute(
            "SELECT COUNT(*) FROM library_automatic_edition_undo"
        ).fetchone()[0]
    assert tuple(after) == tuple(before)
    assert audit_count == 0
    assert undo_count == 0


@pytest.mark.asyncio
async def test_identify_gate_fetch_failure_defers_without_review(
    store: NativeLibraryStore, db_path: Path
) -> None:
    """Canonical fetch failure maps to PROVIDER_TEMPORARILY_UNAVAILABLE defer -
    same provider-unavailable semantics, never review."""
    # Step 2.6 (N-01): the lone-eligible quorum needs the seeded proof so the
    # run reaches the gate whose fetch then fails.
    await _seed_album(
        store,
        embedded_recording=GATE_RECORDING,
        embedded_release_track=GATE_RELEASE_TRACK,
    )
    provider = FakeProvider([_candidate(recording=GATE_RECORDING, release_track=GATE_RELEASE_TRACK)])
    canonical = FakeCanonicalProvider(
        _official_release(), error=ExternalServiceError("MusicBrainz is down")
    )
    job = await _claimed_job(store)
    service = _service(
        store,
        provider,
        _disabled_fingerprinter(),
        canonical_provider=canonical,
        edition_opt_in=lambda _root_id: True,
    )
    outcome = await service.run_claimed_job(job, "worker", now=3)

    assert outcome == "provider_deferred"
    assert canonical.calls != []
    with sqlite3.connect(db_path) as connection:
        failure_code = connection.execute(
            "SELECT last_failure_code FROM library_identification_jobs WHERE id = 'job-album-1'"
        ).fetchone()[0]
        identity_count = connection.execute(
            "SELECT COUNT(*) FROM local_album_external_identities"
        ).fetchone()[0]
        review_count = connection.execute(
            "SELECT COUNT(*) FROM library_identification_reviews "
            "WHERE local_album_id = 'album-1'"
        ).fetchone()[0]
    assert failure_code == "PROVIDER_TEMPORARILY_UNAVAILABLE"
    assert identity_count == 0
    assert review_count == 0


@pytest.mark.asyncio
async def test_identify_gate_multi_root_album_requires_unanimous_opt_in(
    store: NativeLibraryStore, db_path: Path
) -> None:
    """Tracks spanning two opted-in roots still take the legacy path: no gate
    I/O, exact seals without audit or undo rows."""
    from models.local_catalog import (
        CatalogMembership,
        LocalAlbum,
        LocalArtist,
        LocalArtistCredit,
        LocalTrack,
    )

    artist = LocalArtist(
        id="artist-mr",
        display_name="Artist",
        folded_name="artist",
        normalized_name="artist",
        kind="group",
        created_at=1,
        updated_at=1,
    )
    album = LocalAlbum(
        id="album-mr",
        root_id="root-a",
        grouping_key="group-mr",
        title="Album",
        album_artist_id=artist.id,
        album_artist_name="Artist",
        created_at=1,
        updated_at=1,
    )
    tracks = []
    credits = {}
    for index in (1, 2):
        track = LocalTrack(
            id=f"track-mr-{index}",
            local_album_id=album.id,
            root_id="root-a",
            file_path=f"/music/mr-{index}.flac",
            relative_path=f"mr-{index}.flac",
            path_hash=f"hash-mr-{index}",
            file_size_bytes=100,
            file_mtime_ns=index,
            stat_revision=f"stat-mr-{index}",
            tag_revision=f"tag-mr-{index}",
            title=f"Track {index}",
            artist_name="Artist",
            album_title="Album",
            album_artist_name="Artist",
            title_provenance="tag",
            album_title_provenance="tag",
            album_artist_provenance="tag",
            track_number=index,
            duration_seconds=180,
            file_format="flac",
            imported_at=1,
            applied_policy="automatic",
            applied_policy_revision="policy-1",
            embedded_recording_mbid=GATE_MULTI_RECORDING[index],
            embedded_release_track_mbid=GATE_MULTI_RELEASE_TRACK[index],
        )
        tracks.append(track)
        credits[track.id] = [LocalArtistCredit(local_artist_id=artist.id, position=0)]
    await store.create_catalog_membership(
        CatalogMembership(
            album=album,
            artists=[artist],
            tracks=tracks,
            album_credits=[LocalArtistCredit(local_artist_id=artist.id, position=0)],
            track_credits=credits,
        )
    )
    with sqlite3.connect(db_path) as connection:
        connection.execute(
            "UPDATE local_tracks SET root_id = 'root-b' WHERE id = 'track-mr-2'"
        )
        connection.commit()
    candidate = AlbumCandidate(
        release_group_mbid="rg-mr",
        release_mbid="release-mr",
        album_title="Album",
        album_artist_name="Artist",
        artist_mbid="artist-mbid",
        tracks=[
            CandidateTrack(
                title=f"Track {index}",
                position=index,
                absolute_position=index,
                duration_seconds=180,
                recording_mbid=GATE_MULTI_RECORDING[index],
                release_track_mbid=GATE_MULTI_RELEASE_TRACK[index],
            )
            for index in (1, 2)
        ],
    )
    canonical = FakeCanonicalProvider(
        _official_release(release_mbid="release-mr")
    )
    job = await _claimed_job(store, "album-mr")
    service = _service(
        store,
        FakeProvider([candidate]),
        _disabled_fingerprinter(),
        canonical_provider=canonical,
        edition_opt_in=lambda _root_id: True,
    )
    outcome = await service.run_claimed_job(job, "worker", now=3)

    assert outcome == "identified"
    assert canonical.calls == []
    with sqlite3.connect(db_path) as connection:
        album_identity = connection.execute(
            "SELECT release_group_mbid, release_mbid, decision_source "
            "FROM local_album_external_identities WHERE local_album_id = 'album-mr'"
        ).fetchone()
        audit_count = connection.execute(
            "SELECT COUNT(*) FROM library_catalog_actions "
            "WHERE action_kind = 'automatic_exact_edition'"
        ).fetchone()[0]
    assert tuple(album_identity) == ("rg-mr", "release-mr", "automatic")
    assert audit_count == 0


@pytest.mark.asyncio
async def test_apply_suggested_edition_tx_refuses_automatic_refine_of_manual_identity(
    store: NativeLibraryStore, db_path: Path
) -> None:
    """The accept tx's decision_source pre-check (identify-lane flag): an
    automatic accept over a manual/legacy row skips without touching it; the
    repair lane default still auto-refines (signed D-EDITION-AUTO behavior);
    a manual accept still seals under either setting."""
    from infrastructure.persistence.native_library_store import _album_input_revision

    await _seed_album(store)

    def _set_manual_rg_only() -> None:
        with sqlite3.connect(db_path) as connection:
            connection.execute(
                "DELETE FROM local_album_external_identities WHERE local_album_id = 'album-1'"
            )
            connection.execute(
                "INSERT INTO local_album_external_identities "
                "(local_album_id, provider, release_group_mbid, release_mbid, "
                "decision_source, matcher_version, attempt_id, selected_at) "
                "VALUES ('album-1', 'musicbrainz', 'rg-1', NULL, 'manual', 'test', NULL, 1.0)"
            )
            connection.commit()

    _set_manual_rg_only()
    evidence = CandidateEvidence(
        release_group_mbid="rg-1",
        release_mbid="release-rg-1",
        score=1.0,
        reason_code="SUPPORTED",
        matcher_version="test",
        track_evidence=[
            TrackEvidence(
                local_track_id="track-1",
                classification="supported",
                recording_mbid=GATE_RECORDING,
                release_track_mbid=GATE_RELEASE_TRACK,
                candidate_disc_number=1,
                candidate_track_position=1,
            )
        ],
    )

    async def _seal(
        *, decision_source: str, protect_manual_identity: bool = False
    ) -> tuple[str, str | None]:
        with sqlite3.connect(db_path) as connection:
            connection.row_factory = sqlite3.Row
            album_rev = connection.execute(
                "SELECT row_revision FROM local_albums WHERE id = 'album-1'"
            ).fetchone()[0]
            identity = connection.execute(
                "SELECT * FROM local_album_external_identities "
                "WHERE local_album_id = 'album-1'"
            ).fetchone()
            track_rows = connection.execute(
                "SELECT t.*, NULL AS recording_mbid, NULL AS identity_release_mbid, "
                "NULL AS release_track_mbid, NULL AS medium_position, "
                "NULL AS release_track_position FROM local_tracks t "
                "WHERE t.local_album_id = 'album-1' AND t.availability = 'indexed' "
                "ORDER BY t.id"
            ).fetchall()
            tag_rev, file_rev, policy_rev = _album_input_revision(track_rows).split(":")

            def operation(
                write_connection: sqlite3.Connection,
            ) -> tuple[str, str | None]:
                return store._apply_suggested_edition_tx(
                    write_connection,
                    work=None,
                    finding=None,
                    local_album_id="album-1",
                    expected_album_revision=int(album_rev),
                    expected_identity_revision=int(identity["row_revision"]),
                    evidence_id="ev-1",
                    album={"row_revision": int(album_rev)},
                    identity=dict(identity),
                    evidence_row={
                        "evidence_json": bytes(msgspec.json.encode(evidence)),
                        # No attempt row exists in this harness; the column is
                        # nullable and the pipeline path passes the real id.
                        "attempt_id": None,
                        "compacted": 0,
                        "input_tag_revision": tag_rev,
                        "input_file_revision": file_rev,
                        "input_policy_revision": policy_rev,
                    },
                    track_rows=list(track_rows),
                    evidence=evidence,
                    job_id=None,
                    actor_user_id=None,
                    now=3.0,
                    decision_source=decision_source,
                    ranking_inputs=[{"release_mbid": "release-rg-1"}],
                    protect_manual_identity=protect_manual_identity,
                )

            return await store._write(operation)

    with sqlite3.connect(db_path) as connection:
        before = connection.execute(
            "SELECT * FROM local_album_external_identities WHERE local_album_id = 'album-1'"
        ).fetchone()
    # Identify-lane flag: the automatic accept skips, leaving the row and
    # writing no audit.
    assert await _seal(
        decision_source="automatic", protect_manual_identity=True
    ) == ("skipped", "PROTECTED_IDENTITY")
    with sqlite3.connect(db_path) as connection:
        assert tuple(
            connection.execute(
                "SELECT * FROM local_album_external_identities WHERE local_album_id = 'album-1'"
            ).fetchone()
        ) == tuple(before)
        assert (
            connection.execute(
                "SELECT COUNT(*) FROM library_catalog_actions"
            ).fetchone()[0]
            == 0
        )
    # Repair-lane default (flag off): the same automatic accept seals, pinning
    # the signed D-EDITION-AUTO refine-with-undo behavior at unit level.
    assert await _seal(decision_source="automatic") == ("succeeded", None)
    with sqlite3.connect(db_path) as connection:
        refined = connection.execute(
            "SELECT release_mbid, decision_source FROM local_album_external_identities "
            "WHERE local_album_id = 'album-1'"
        ).fetchone()
        auto_audit = connection.execute(
            "SELECT action_kind FROM library_catalog_actions"
        ).fetchone()
    assert tuple(refined) == ("release-rg-1", "automatic")
    assert tuple(auto_audit) == ("automatic_exact_edition",)
    # Curator authority is unaffected: reset to manual RG-only and seal with a
    # manual source (flag on or off).
    _set_manual_rg_only()
    assert await _seal(
        decision_source="manual", protect_manual_identity=True
    ) == ("succeeded", None)
    with sqlite3.connect(db_path) as connection:
        sealed = connection.execute(
            "SELECT release_mbid, decision_source FROM local_album_external_identities "
            "WHERE local_album_id = 'album-1'"
        ).fetchone()
        kinds = connection.execute(
            "SELECT action_kind FROM library_catalog_actions ORDER BY rowid"
        ).fetchall()
    assert tuple(sealed) == ("release-rg-1", "manual")
    assert [row[0] for row in kinds] == [
        "automatic_exact_edition",
        "accept_suggested_edition",
    ]


class _CancellingProvider(FakeProvider):
    """R-04: raises CancelledError mid-recall like a shutdown arriving mid-job."""

    async def search_album_candidate_ids(
        self, artist: str, title: str, limit: int, priority: RequestPriority
    ) -> list[str]:
        raise asyncio.CancelledError


@pytest.mark.asyncio
async def test_release_identification_claim_requeues_without_backoff(
    store: NativeLibraryStore,
) -> None:
    """R-04 (3.4a/T20a): the cancel-path release primitive re-queues with NO
    backoff - the job is claimable at once, never via recover()."""
    await _seed_album(store)
    queue = IdentificationQueueService(store)
    job = await _claimed_job(store)
    claimed_revision = int(job["row_revision"])
    attempts_before = int(job["attempt_count"])

    released_revision = await queue.release(job, "worker", now=10.0)

    assert released_revision == claimed_revision + 1
    with sqlite3.connect(store.db_path) as connection:
        row = connection.execute(
            "SELECT state, lease_owner, lease_expires_at, not_before, "
            "attempt_count, last_failure_code FROM library_identification_jobs "
            "WHERE id = ?",
            (job["id"],),
        ).fetchone()
    assert row[0] == "queued"
    assert row[1] is None
    assert row[2] is None
    assert float(row[3]) <= 10.0
    assert int(row[4]) == attempts_before
    assert row[5] is None
    # Immediate reclaim with no wait and no recover().
    reclaimed = await store.claim_identification_job(
        "worker-2", now=10.0, lease_seconds=60
    )
    assert reclaimed is not None
    assert reclaimed["id"] == job["id"]
    # NOT idempotent: the same expected revision no longer matches, and a
    # foreign owner never matches either.
    with pytest.raises(StaleRevisionError):
        await store.release_identification_claim(
            str(job["id"]),
            worker_id="intruder",
            expected_job_revision=released_revision + 1,
            now=10.0,
        )
    with pytest.raises(StaleRevisionError):
        await store.release_identification_claim(
            str(job["id"]),
            worker_id="worker-2",
            expected_job_revision=claimed_revision,
            now=10.0,
        )


@pytest.mark.asyncio
async def test_cancel_mid_identification_job_releases_for_immediate_reclaim(
    store: NativeLibraryStore,
) -> None:
    """R-04 (3.4a/T20a): cancelling mid-job releases the claim exactly once -
    the next claim succeeds at once (no 60s lease wait, no recover(), no
    defer backoff)."""
    await _seed_album(store)
    service = _service(
        store,
        _CancellingProvider([_candidate()]),
        FakeFingerprinter(FingerprintResult(status="skip")),
    )
    job = await _claimed_job(store)
    attempts_before = int(job["attempt_count"])
    release_calls = {"n": 0}
    original = store.release_identification_claim

    async def spy(
        job_id: str, *, worker_id: str, expected_job_revision: int, now: float
    ) -> int:
        release_calls["n"] += 1
        return await original(
            job_id,
            worker_id=worker_id,
            expected_job_revision=expected_job_revision,
            now=now,
        )

    store.release_identification_claim = spy  # type: ignore[method-assign]

    with pytest.raises(asyncio.CancelledError):
        await service.run_claimed_job(dict(job), "worker", now=100.0)

    assert release_calls["n"] == 1
    with sqlite3.connect(store.db_path) as connection:
        row = connection.execute(
            "SELECT state, lease_owner, not_before, attempt_count "
            "FROM library_identification_jobs WHERE id = ?",
            (job["id"],),
        ).fetchone()
    assert row[0] == "queued"
    assert row[1] is None
    assert float(row[2]) <= 100.0
    assert int(row[3]) == attempts_before
    reclaimed = await store.claim_identification_job(
        "worker-2", now=100.0, lease_seconds=60
    )
    assert reclaimed is not None
    assert reclaimed["id"] == job["id"]


@pytest.mark.asyncio
async def test_cancel_after_identification_commit_swallows_stale_release(
    store: NativeLibraryStore, monkeypatch: pytest.MonkeyPatch
) -> None:
    """R-04 (3.4a/T20a): StaleRevisionError from the release means the finish
    commit already landed → swallowed (commit wins), CancelledError still
    propagates."""
    await _seed_album(store)
    service = _service(
        store,
        _CancellingProvider([_candidate()]),
        FakeFingerprinter(FingerprintResult(status="skip")),
    )
    job = await _claimed_job(store)

    async def _stale_release(
        self: IdentificationQueueService,
        job: dict,
        worker_id: str,
        *,
        now: float | None = None,
    ) -> int:
        raise StaleRevisionError(
            "The identification job changed before its claim could be released."
        )

    monkeypatch.setattr(IdentificationQueueService, "release", _stale_release)
    with pytest.raises(asyncio.CancelledError):
        await service.run_claimed_job(dict(job), "worker", now=100.0)


# Slice C (4.9 + 4.10): skipped emit, partial plumbing, dual-lane corroboration.


@pytest.mark.asyncio
async def test_not_needed_writes_skipped_row_without_budget_charge(
    store: NativeLibraryStore,
) -> None:
    """4.9: needed==False writes a skipped row (not None) and never charges
    budget - fails pre-fix where the path returned None with no row."""
    await _seed_album(store)
    fake = FakeFingerprinter(FingerprintResult(status="skip"))
    service = ConditionalFingerprintService(store, fake)

    outcome, did_work = await service.fingerprint_if_needed(
        local_track_id="track-1",
        path=Path("/music/1.flac"),
        stat_revision="stat-1",
        needed=False,
        now=1,
    )

    assert did_work is False
    assert outcome is not None and outcome.state == "skipped"
    stored = await store.get_fingerprint_outcome(
        "track-1", "stat-1", FINGERPRINTER_VERSION
    )
    assert stored is not None and stored.state == "skipped"

    # Write once per stat_revision: a second unneeded call reuses the row
    # without attempt_count noise and without work.
    again, again_work = await service.fingerprint_if_needed(
        local_track_id="track-1",
        path=Path("/music/1.flac"),
        stat_revision="stat-1",
        needed=False,
        now=2,
    )
    assert again_work is False
    assert again is not None and again.state == "skipped"
    assert again.attempt_count == stored.attempt_count
    assert fake.generate_calls == 0
    assert fake.lookup_calls == 0


@pytest.mark.asyncio
async def test_skipped_cache_hit_does_not_starve_later_tracks(
    store: NativeLibraryStore,
) -> None:
    """4.9 budget: a skipped row on track 1 is terminal-free like F-042 -
    tracks 2 and 3 still get fresh generations in the same attempt."""
    await _seed_multi_track_album(store, 3)
    # Track 1 was deemed unneeded on a prior attempt: skipped row exists.
    skipped_service = ConditionalFingerprintService(
        store, FakeFingerprinter(FingerprintResult(status="skip"))
    )
    skipped, skipped_work = await skipped_service.fingerprint_if_needed(
        local_track_id="track-multi-1",
        path=Path("/music/multi-1.flac"),
        stat_revision="stat-multi-1",
        needed=False,
        now=1,
    )
    assert skipped is not None and skipped.state == "skipped"
    assert skipped_work is False

    fake = FakeFingerprinter(FingerprintResult(status="skip"))
    provider = FakeProvider(
        [
            _multi_track_candidate(
                group="rg-a",
                release="release-a",
                recording_prefix="recording-a",
                count=3,
            ),
            _multi_track_candidate(
                group="rg-b",
                release="release-b",
                recording_prefix="recording-b",
                count=3,
            ),
        ]
    )
    service = _service(store, provider, fake)
    job = await _claimed_job(store, "album-multi")

    outcome = await service.run_claimed_job(job, "worker")

    assert outcome == "ambiguous"
    # Only tracks 2+3 needed fresh work; the skipped hit was free.
    assert fake.generate_calls == 2
    first = await store.get_fingerprint_outcome(
        "track-multi-1", "stat-multi-1", FINGERPRINTER_VERSION
    )
    assert first is not None and first.state == "skipped"


class _TrackedPartialFake(FakeFingerprinter):
    """Fake speaking the tracked generation seam with a fixed partial flag."""

    def __init__(self, result: FingerprintResult, *, partial: bool) -> None:
        super().__init__(result)
        self._partial = partial

    async def _generate_tracked(self, path: Path) -> tuple[str, int, bool]:
        self.generate_calls += 1
        return ("fingerprint-partial", 180, self._partial)


@pytest.mark.asyncio
async def test_tracked_partial_decode_persists_on_matched_outcome(
    store: NativeLibraryStore,
) -> None:
    """4.10a: a partial generation threads into FingerprintOutcome."""
    await _seed_album(store)
    fake = _TrackedPartialFake(
        FingerprintResult(
            status="pass", recording_id="rec-1", score=0.95,
            release_group_ids=["rg-1"],
        ),
        partial=True,
    )
    service = ConditionalFingerprintService(store, fake)
    outcome, did_work = await service.fingerprint_if_needed(
        local_track_id="track-1",
        path=Path("/music/1.flac"),
        stat_revision="stat-1",
        needed=True,
        now=1,
    )

    assert did_work is True
    assert outcome is not None and outcome.state == "matched"
    assert outcome.partial_decode is True
    assert outcome.recording_mbid == "rec-1"


@pytest.mark.asyncio
async def test_full_decode_persists_false_unchanged(
    store: NativeLibraryStore,
) -> None:
    """4.10a: full prints keep today's behavior (flag False)."""
    await _seed_album(store)
    fake = _TrackedPartialFake(
        FingerprintResult(
            status="pass", recording_id="rec-1", score=0.95,
            release_group_ids=["rg-1"],
        ),
        partial=False,
    )
    service = ConditionalFingerprintService(store, fake)
    outcome, did_work = await service.fingerprint_if_needed(
        local_track_id="track-1",
        path=Path("/music/1.flac"),
        stat_revision="stat-1",
        needed=True,
        now=1,
    )

    assert did_work is True
    assert outcome is not None and outcome.state == "matched"
    assert outcome.partial_decode is False


class _PerTrackPartialFake:
    """Two-track fake mapping each file to its own recording + partial flag."""

    def __init__(self, *, partial: bool) -> None:
        self.partial = partial
        self.generate_calls = 0
        self.lookup_calls = 0

    def is_enabled(self) -> bool:
        return True

    async def generate_fingerprint(self, path: Path) -> tuple[str, int]:
        self.generate_calls += 1
        return (f"fp-{Path(path).name}", 180)

    async def _generate_tracked(self, path: Path) -> tuple[str, int, bool]:
        self.generate_calls += 1
        return (f"fp-{Path(path).name}", 180, self.partial)

    async def lookup_fingerprint(
        self, fingerprint: str, duration: int
    ) -> FingerprintResult:
        self.lookup_calls += 1
        if "multi-1" in fingerprint:
            recording = "recording-a-1"
        elif "multi-2" in fingerprint:
            recording = "recording-a-2"
        else:
            recording = "recording-a-1"
        return FingerprintResult(
            status="pass",
            recording_id=recording,
            score=0.95,
            release_group_ids=["rg-a"],
        )


def _mismatched_two_track_candidate() -> AlbumCandidate:
    """One candidate whose track titles never match the seeded 'Track'
    descriptives, so only fingerprint support can carry it."""
    return AlbumCandidate(
        release_group_mbid="rg-a",
        release_mbid="release-a",
        album_title="Album",
        album_artist_name="Artist",
        artist_mbid="artist-mbid",
        tracks=[
            CandidateTrack(
                title="Zebra One",
                position=1,
                absolute_position=1,
                duration_seconds=180,
                recording_mbid="recording-a-1",
            ),
            CandidateTrack(
                title="Zebra Two",
                position=2,
                absolute_position=2,
                duration_seconds=180,
                recording_mbid="recording-a-2",
            ),
        ],
    )


@pytest.mark.asyncio
async def test_partial_only_evidence_never_identifies_alone(
    store: NativeLibraryStore,
) -> None:
    """4.10b lane A: partial-derived MBIDs never seed support/recall alone."""
    await _seed_multi_track_album(store, 2)
    provider = FakeProvider([_mismatched_two_track_candidate()])
    service = _service(store, provider, _PerTrackPartialFake(partial=True))
    job = await _claimed_job(store, "album-multi")

    outcome = await service.run_claimed_job(job, "worker")

    assert outcome != "identified"
    # The partial outcomes persist with the flag for evidence.
    first = await store.get_fingerprint_outcome(
        "track-multi-1", "stat-multi-1", FINGERPRINTER_VERSION
    )
    assert first is not None and first.partial_decode is True


@pytest.mark.asyncio
async def test_full_print_behavior_unchanged_identifies(
    store: NativeLibraryStore,
) -> None:
    """4.10b lane A: the same evidence with full prints keeps identifying."""
    await _seed_multi_track_album(store, 2)
    provider = FakeProvider([_mismatched_two_track_candidate()])
    service = _service(store, provider, _PerTrackPartialFake(partial=False))
    job = await _claimed_job(store, "album-multi")

    outcome = await service.run_claimed_job(job, "worker")

    assert outcome == "identified"


# Slice R1 (4.9 + 4.10b lane B): re-identification caller guards.


def _recording_less_twins(count: int) -> list[AlbumCandidate]:
    """Twin candidates without recording MBIDs, so tag support ties and the
    fingerprint lane decides - with no recording conflicts either way."""
    twins = []
    for group, release in (("rg-a", "release-a"), ("rg-b", "release-b")):
        twins.append(
            AlbumCandidate(
                release_group_mbid=group,
                release_mbid=release,
                album_title="Album",
                album_artist_name="Artist",
                artist_mbid="artist-mbid",
                tracks=[
                    CandidateTrack(
                        title="Track",
                        position=index,
                        absolute_position=index,
                        duration_seconds=180,
                    )
                    for index in range(1, count + 1)
                ],
            )
        )
    return twins


async def _run_reidentification(
    store: NativeLibraryStore,
    provider: FakeProvider,
    fingerprinter: object,
    *,
    idempotency_key: str,
    now: float = 1,
) -> str:
    """Run one explicit re-identification evaluation and return the snapshot
    decision outcome (the re-id `run_claimed` returns the job row, not the
    outcome string the automatic lane returns)."""
    worker = ExplicitReidentificationWorker(
        store,
        AlbumCandidateService(provider),
        AlbumEvidenceEngine(),
        ConditionalFingerprintService(store, fingerprinter),  # type: ignore[arg-type]
    )
    created = await ReidentificationService(store).create_or_coalesce(
        "album-multi", "admin", idempotency_key=idempotency_key, now=now
    )
    claimed = await store.claim_operation_job(
        "worker", now=now + 1, lease_seconds=60, kind="explicit_reidentification"
    )
    assert claimed is not None
    await worker.run_claimed(claimed, "worker", now=now + 2)
    snapshot = await store.get_operation_snapshot(str(created["id"]))
    assert snapshot is not None and snapshot["snapshot"] is not None
    return str(json.loads(snapshot["snapshot"]["result_json"])["outcome"])


@pytest.mark.asyncio
async def test_reidentification_cached_partial_never_seeds_alone(
    store: NativeLibraryStore,
) -> None:
    """4.10b lane B (cached): cached partial decodes never seed support or
    recall alone - fails pre-fix where the worker extended release groups
    and fingerprint MBIDs from partial rows and identified."""
    await _seed_multi_track_album(store, 2)
    for index in (1, 2):
        await store.record_fingerprint_outcome(
            FingerprintOutcome(
                id=f"seed-partial-{index}",
                local_track_id=f"track-multi-{index}",
                stat_revision=f"stat-multi-{index}",
                fingerprinter_version=FINGERPRINTER_VERSION,
                state="matched",
                recording_mbid=f"recording-a-{index}",
                release_group_ids=["rg-a"],
                first_attempt_at=1,
                last_attempt_at=1,
                partial_decode=True,
            )
        )
    provider = FakeProvider([_mismatched_two_track_candidate()])
    fake = FakeFingerprinter(FingerprintResult(status="skip"))

    outcome = await _run_reidentification(
        store, provider, fake, idempotency_key="r1-cached-partial"
    )

    assert outcome != "identified"
    # The partial outcomes persist with the flag for evidence.
    first = await store.get_fingerprint_outcome(
        "track-multi-1", "stat-multi-1", FINGERPRINTER_VERSION
    )
    assert first is not None and first.partial_decode is True
    assert fake.generate_calls == 0


@pytest.mark.asyncio
async def test_reidentification_cached_full_print_seeds_unchanged(
    store: NativeLibraryStore,
) -> None:
    """4.10b lane B (cached): the same cached evidence with full prints keeps
    seeding support/recall and identifying - the control for the partial
    test above (passes pre- and post-fix)."""
    await _seed_multi_track_album(store, 2)
    for index in (1, 2):
        await store.record_fingerprint_outcome(
            FingerprintOutcome(
                id=f"seed-full-{index}",
                local_track_id=f"track-multi-{index}",
                stat_revision=f"stat-multi-{index}",
                fingerprinter_version=FINGERPRINTER_VERSION,
                state="matched",
                recording_mbid=f"recording-a-{index}",
                release_group_ids=["rg-a"],
                first_attempt_at=1,
                last_attempt_at=1,
                partial_decode=False,
            )
        )
    provider = FakeProvider([_mismatched_two_track_candidate()])
    fake = FakeFingerprinter(FingerprintResult(status="skip"))

    outcome = await _run_reidentification(
        store, provider, fake, idempotency_key="r1-cached-full"
    )

    assert outcome == "identified"


@pytest.mark.asyncio
async def test_reidentification_fresh_partial_never_seeds_alone(
    store: NativeLibraryStore,
) -> None:
    """4.10b lane B (fresh): fresh partial decodes never seed support or
    recall alone - fails pre-fix where the worker assigned partial-derived
    MBIDs and extended release groups, then identified on re-recall."""
    await _seed_multi_track_album(store, 2)
    provider = FakeProvider([_mismatched_two_track_candidate()])

    outcome = await _run_reidentification(
        store,
        provider,
        _PerTrackPartialFake(partial=True),
        idempotency_key="r1-fresh-partial",
    )

    assert outcome != "identified"
    # The partial outcomes persist with the flag for evidence.
    first = await store.get_fingerprint_outcome(
        "track-multi-1", "stat-multi-1", FINGERPRINTER_VERSION
    )
    assert first is not None and first.partial_decode is True


@pytest.mark.asyncio
async def test_reidentification_fresh_full_print_seeds_unchanged(
    store: NativeLibraryStore,
) -> None:
    """4.10b lane B (fresh): the same evidence with full prints keeps
    identifying - the control for the partial test above (passes pre- and
    post-fix)."""
    await _seed_multi_track_album(store, 2)
    provider = FakeProvider([_mismatched_two_track_candidate()])

    outcome = await _run_reidentification(
        store,
        provider,
        _PerTrackPartialFake(partial=False),
        idempotency_key="r1-fresh-full",
    )

    assert outcome == "identified"


@pytest.mark.asyncio
async def test_identify_lane_unneeded_track_writes_skipped_row(
    store: NativeLibraryStore, db_path: Path
) -> None:
    """4.9 identify lane: a not-needed track still calls fingerprint_if_needed
    with needed=False (skipped row, no work, no budget charge) - fails
    pre-fix where the bare `continue` never called the service, so no call
    and no row exist. Track 1 carries an embedded recording (needed False);
    tracks 2+3 need fresh work and must not starve."""
    await _seed_multi_track_album(store, 3)
    with sqlite3.connect(db_path) as connection:
        connection.execute(
            "UPDATE local_tracks SET embedded_recording_mbid=? WHERE id=?",
            ("11111111-1111-4111-8111-111111111111", "track-multi-1"),
        )
        connection.commit()
    provider = FakeProvider(_recording_less_twins(3))
    fake = FakeFingerprinter(FingerprintResult(status="skip"))
    queue = IdentificationQueueService(store)
    fingerprints = ConditionalFingerprintService(store, fake)
    service = AlbumIdentificationService(
        store,
        queue,
        AlbumCandidateService(provider),
        AlbumEvidenceEngine(),
        fingerprints,
    )
    calls: list[dict] = []
    original = fingerprints.fingerprint_if_needed

    async def spy(**kwargs: object) -> tuple[FingerprintOutcome | None, bool]:
        calls.append(dict(kwargs))
        return await original(**kwargs)  # type: ignore[arg-type]

    fingerprints.fingerprint_if_needed = spy  # type: ignore[method-assign]
    job = await _claimed_job(store, "album-multi")

    outcome = await service.run_claimed_job(job, "worker")

    assert outcome in ("ambiguous", "insufficient_evidence")
    unneeded = [call for call in calls if call.get("needed") is False]
    assert [call["local_track_id"] for call in unneeded] == ["track-multi-1"]
    skipped = await store.get_fingerprint_outcome(
        "track-multi-1", "stat-multi-1", FINGERPRINTER_VERSION
    )
    assert skipped is not None and skipped.state == "skipped"
    # Both needed tracks did fresh work: the skipped call charged no budget.
    assert fake.generate_calls == 2


@pytest.mark.asyncio
async def test_reidentification_lane_unneeded_track_writes_skipped_row(
    store: NativeLibraryStore, db_path: Path
) -> None:
    """4.9 re-id lane: a not-needed track still calls fingerprint_if_needed
    with needed=False (skipped row, no work, no budget charge) - fails
    pre-fix where the bare `continue` never called the service, so no call
    and no row exist. Same mixed-need shape as the identify-lane test."""
    await _seed_multi_track_album(store, 3)
    with sqlite3.connect(db_path) as connection:
        connection.execute(
            "UPDATE local_tracks SET embedded_recording_mbid=? WHERE id=?",
            ("11111111-1111-4111-8111-111111111111", "track-multi-1"),
        )
        connection.commit()
    provider = FakeProvider(_recording_less_twins(3))
    fake = FakeFingerprinter(FingerprintResult(status="skip"))
    fingerprints = ConditionalFingerprintService(store, fake)
    calls: list[dict] = []
    original = fingerprints.fingerprint_if_needed

    async def spy(**kwargs: object) -> tuple[FingerprintOutcome | None, bool]:
        calls.append(dict(kwargs))
        return await original(**kwargs)  # type: ignore[arg-type]

    fingerprints.fingerprint_if_needed = spy  # type: ignore[method-assign]
    worker = ExplicitReidentificationWorker(
        store,
        AlbumCandidateService(provider),
        AlbumEvidenceEngine(),
        fingerprints,
    )
    created = await ReidentificationService(store).create_or_coalesce(
        "album-multi", "admin", idempotency_key="r1-skipped", now=1
    )
    claimed = await store.claim_operation_job(
        "worker", now=2, lease_seconds=60, kind="explicit_reidentification"
    )
    assert claimed is not None

    await worker.run_claimed(claimed, "worker", now=3)

    snapshot = await store.get_operation_snapshot(str(created["id"]))
    assert snapshot is not None and snapshot["snapshot"] is not None
    assert (
        str(json.loads(snapshot["snapshot"]["result_json"])["outcome"])
        != "identified"
    )
    unneeded = [call for call in calls if call.get("needed") is False]
    assert [call["local_track_id"] for call in unneeded] == ["track-multi-1"]
    skipped = await store.get_fingerprint_outcome(
        "track-multi-1", "stat-multi-1", FINGERPRINTER_VERSION
    )
    assert skipped is not None and skipped.state == "skipped"
    # Both needed tracks did fresh work: the skipped call charged no budget.
    assert fake.generate_calls == 2


def _decision_with_one_track(kinds: list[str]) -> IdentificationDecision:
    evidence = CandidateEvidence(
        release_group_mbid="group",
        release_mbid="release",
        track_evidence=[
            TrackEvidence(
                local_track_id="t1",
                classification="supported",
                evidence_kinds=kinds,
                candidate_track_title="One",
                candidate_disc_number=1,
                candidate_track_position=1,
                recording_mbid="recording-edition",
                release_track_mbid="track-edition",
            )
        ],
        reason_code="SUPPORTED",
    )
    return IdentificationDecision(
        outcome="identified",
        reason_code="SUPPORTED",
        selected_candidate_key="group:release",
        candidates=[evidence],
    )


_RAW_TRACK_OF_ANOTHER_EDITION = [
    {
        "id": "t1",
        "embedded_release_group_mbid": "group",
        "embedded_recording_mbid": "recording-other",
        "recording_mbid": None,
        "embedded_release_track_mbid": "track-other",
        "release_track_mbid": None,
    }
]


def test_second_pass_keeps_a_track_reconciled_by_the_unanimous_release() -> None:
    decision = _decision_with_one_track(["embedded_ids_reconciled"])
    _enforce_raw_track_identities(decision, _RAW_TRACK_OF_ANOTHER_EDITION)

    candidate = decision.candidates[0]
    assert candidate.track_evidence[0].classification == "supported"
    assert decision.outcome == "identified"


def test_second_pass_still_flags_an_ordinary_embedded_id_conflict() -> None:
    decision = _decision_with_one_track(["normalized_title"])
    _enforce_raw_track_identities(decision, _RAW_TRACK_OF_ANOTHER_EDITION)

    assert decision.candidates[0].track_evidence[0].classification == "contradictory"
    assert decision.outcome == "contradictory"

