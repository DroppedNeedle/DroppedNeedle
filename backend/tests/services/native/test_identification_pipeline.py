import json
import sqlite3
import threading
from pathlib import Path
from unittest.mock import AsyncMock

import msgspec
import pytest

from infrastructure.audio.fingerprinter import FingerprintStatus
from infrastructure.degradation import try_get_degradation_context
from infrastructure.integration_result import IntegrationResult
from infrastructure.persistence.native_library_store import NativeLibraryStore
from infrastructure.queue.priority_queue import RequestPriority
from models.audio import FingerprintResult
from models.identification import (
    AlbumCandidate,
    CandidateEvidence,
    CandidateTrack,
    GroupingApplication,
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
from services.native.album_candidate_service import AlbumCandidateService
from services.native.album_coverage_service import AlbumCoverageService
from services.native.album_evidence_engine import AlbumEvidenceEngine
from services.native.album_identification_service import AlbumIdentificationService
from services.native.conditional_fingerprint_service import (
    ConditionalFingerprintService,
)
from services.native.identification_evidence_projector import (
    IdentificationEvidenceProjector,
)
from services.native.identification_queue_service import IdentificationQueueService
from services.native.local_album_grouping_service import LocalAlbumGroupingService
from services.native.reidentification_service import (
    IdentificationWorkArbiter,
    ReidentificationService,
)

EMBEDDED_GROUP = "11111111-1111-4111-8111-111111111111"
EMBEDDED_GROUP_OTHER = "22222222-2222-4222-8222-222222222222"
EMBEDDED_RELEASE = "33333333-3333-4333-8333-333333333333"
EMBEDDED_RECORDING = "44444444-4444-4444-8444-444444444444"
EMBEDDED_RECORDING_OTHER = "55555555-5555-4555-8555-555555555555"
EMBEDDED_ARTIST = "66666666-6666-4666-8666-666666666666"


class FakeProvider:
    def __init__(self, candidates: list[AlbumCandidate] | None = None) -> None:
        self.candidates = candidates or []
        self.calls: list[tuple[str, RequestPriority]] = []

    async def search_album_candidate_ids(
        self, query: str, limit: int, priority: RequestPriority
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
        self, query: str, limit: int, priority: RequestPriority
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
        track_number=1,
        duration_seconds=180,
        file_format="flac",
        imported_at=1,
        applied_policy=policy,
        applied_policy_revision="policy-1",
        embedded_release_group_mbid=embedded_group,
        embedded_release_mbid=embedded_release,
        embedded_recording_mbid=embedded_recording,
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
) -> AlbumIdentificationService:
    queue = IdentificationQueueService(store)
    return AlbumIdentificationService(
        store,
        queue,
        AlbumCandidateService(provider),
        AlbumEvidenceEngine(),
        ConditionalFingerprintService(store, fingerprinter),
        invalidate,
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
async def test_local_metadata_embedded_identity_uses_zero_provider_calls_and_attaches_only_supported_tracks(
    store: NativeLibraryStore,
    db_path: Path,
) -> None:
    await _seed_album(
        store,
        embedded_group=EMBEDDED_GROUP,
        embedded_release=EMBEDDED_RELEASE,
        embedded_recording=EMBEDDED_RECORDING,
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
            "SELECT recording_mbid, decision_source FROM local_track_external_identities"
        ).fetchone()
    assert album_identity == (EMBEDDED_GROUP, "embedded")
    assert track_identity == (EMBEDDED_RECORDING, "embedded")
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
    first = await service.fingerprint_if_needed(
        local_track_id="track-1",
        path=Path("/music/1.flac"),
        stat_revision="stat-1",
        needed=True,
        now=1,
    )
    second = await service.fingerprint_if_needed(
        local_track_id="track-1",
        path=Path("/music/1.flac"),
        stat_revision="stat-1",
        needed=True,
        now=2,
    )
    assert first is not None and first.state == expected_state
    assert second is not None and second.state == expected_state
    assert fake.generate_calls <= 1
    assert fake.lookup_calls <= 1
    changed = await service.fingerprint_if_needed(
        local_track_id="track-1",
        path=Path("/music/1.flac"),
        stat_revision="stat-2",
        needed=True,
        now=3,
    )
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
    first = await service.fingerprint_if_needed(
        local_track_id="track-1",
        path=Path("/music/1.flac"),
        stat_revision="stat-1",
        needed=True,
        now=1,
    )
    assert first is not None and first.state == "failed"
    assert first.fingerprint == "fingerprint"
    await service.fingerprint_if_needed(
        local_track_id="track-1",
        path=Path("/music/1.flac"),
        stat_revision="stat-1",
        needed=True,
        now=62,
    )
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
async def test_pause_at_candidate_and_fingerprint_checkpoints_releases_lease_without_attempt_increment(
    store: NativeLibraryStore, db_path: Path
) -> None:
    await _seed_album(store, "1")
    queue = IdentificationQueueService(store)

    class PausingProvider(FakeProvider):
        async def search_album_candidate_ids(
            self, query: str, limit: int, priority: RequestPriority
        ) -> list[str]:
            await queue.pause("admin", now=3)
            return await super().search_album_candidate_ids(query, limit, priority)

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
async def test_manual_and_legacy_identity_revalidation_never_silently_detaches(
    store: NativeLibraryStore, db_path: Path
) -> None:
    await _seed_album(store)
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
    provider = FakeProvider([_candidate(group="different-rg")])
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
async def test_identity_transaction_rolls_back_attempt_evidence_and_job_on_fk_failure(
    store: NativeLibraryStore, db_path: Path
) -> None:
    await _seed_album(store)
    job = await _claimed_job(store)
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
    await _service(
        store,
        FakeProvider(),
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
    await _service(
        store,
        FakeProvider(),
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
                    track.id: [
                        LocalArtistCredit(local_artist_id=artist.id, position=0)
                    ]
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
    await _seed_album(store, "1")
    await _seed_album(store, "2")
    provider = FakeProvider([_candidate()])
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
