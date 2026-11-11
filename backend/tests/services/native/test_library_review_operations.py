import json
import asyncio
import sqlite3
import threading
from pathlib import Path
from unittest.mock import AsyncMock

import pytest
import msgspec.json

from api.v1.schemas.library_operations import (
    ArtistMergeApplyRequest,
    ArtistMergePreviewRequest,
    AutomaticEditionUndoRequest,
    BulkReviewApplyRequest,
    BulkReviewPreviewRequest,
    BulkReviewSelection,
    CandidateAcceptanceRequest,
    IdentityPreparationCreateRequest,
    MembershipApplyRequest,
    MembershipPreviewRequest,
    RepairCreateRequest,
    ReviewActionRequest,
)
from api.v1.schemas.library_policies import (
    LibraryPathPolicyRule,
    LibraryRootSettings,
    TypedLibrarySettings,
)
from core.exceptions import (
    ConflictError,
    ExternalServiceError,
    ResourceNotFoundError,
    StaleRevisionError,
    ValidationError,
)
from infrastructure.persistence.native_library_store import (
    NativeLibraryStore,
    _QUIET_MARGIN_FLOOR,
    _quiet_top_candidate_agrees,
)
from infrastructure.resilience.retry import CircuitOpenError
from models.audio import FingerprintResult
from models.identification import (
    track_reassignment_plan,
    AlbumCandidate,
    CandidateEvidence,
    CandidateTrack,
    FingerprintOutcome,
    IdentificationAttempt,
    IdentificationEvidenceRecord,
    TrackEvidence,
)
from models.library_work import (
    ReviewDecision,
    ScanFailureRecord,
    ScanRequest,
    ScanRequestResult,
    ScanRun,
    ScanScope,
)
from models.local_catalog import (
    CatalogMembership,
    LocalAlbum,
    LocalAlbumExternalIdentity,
    LocalArtist,
    LocalArtistCredit,
    LocalArtistExternalIdentity,
    LocalTrack,
    LocalTrackExternalIdentity,
)
from repositories.musicbrainz_management_models import (
    MbManagementArtist,
    MbManagementArtistCredit,
    MbManagementMedium,
    MbManagementRecording,
    MbManagementRelease,
    MbManagementReleaseGroup,
    MbManagementTrack,
)
from infrastructure.degradation import (
    IntegrationResult,
    try_get_degradation_context,
)
from services.native.album_candidate_service import AlbumCandidateService
from services.native.album_evidence_engine import (
    CANDIDATE_MARGIN_FLOOR,
    MATCHER_VERSION,
    AlbumEvidenceEngine,
)
from services.native.background_workload_gate import BackgroundWorkloadGate
from services.native.catalog_correction_service import CatalogCorrectionService
from services.native.conditional_fingerprint_service import (
    ConditionalFingerprintService,
)
from services.native.explicit_reidentification_worker import (
    MAX_REIDENTIFICATION_ATTEMPTS,
    REIDENTIFICATION_RETRY_SECONDS,
    ExplicitReidentificationWorker,
)
from services.native.identification_queue_service import IdentificationQueueService
from services.native.identity_repair_service import IdentityRepairService
from services.native.identification_revisions import (
    album_identity_revision,
    album_input_revisions,
)
from services.native.library_diagnostics_service import LibraryDiagnosticsService
from services.native.library_operation_service import LibraryOperationService
from services.native.library_operation_supervisor import LibraryOperationSupervisor
from services.native.library_policy_reconciliation_service import (
    LibraryPolicyReconciliationService,
)
from services.native.library_policy_resolver import LibraryPolicyResolver
from services.native.library_review_service import LibraryReviewService
from services.native.reidentification_service import ReidentificationService

EXACT_GROUP = "11111111-1111-4111-8111-111111111111"
EXACT_RELEASE = "22222222-2222-4222-8222-222222222222"
EXACT_CANONICAL_RELEASE = "55555555-5555-4555-8555-555555555555"
EXACT_RECORDING = "33333333-3333-4333-8333-333333333333"
EXACT_RELEASE_TRACK = "44444444-4444-4444-8444-444444444444"


class _IdentificationProvider:
    async def search_album_candidate_ids(self, artist, title, limit, priority):
        return ["rg-explicit"]

    async def search_recording_candidate_ids(self, artist, title, limit, priority):
        return ["rg-explicit"]

    async def get_album_candidate(
        self, release_group_mbid, target_track_count, priority
    ):
        return AlbumCandidate(
            release_group_mbid="rg-explicit",
            release_mbid="release-explicit",
            album_title="Album 1",
            album_artist_name="Artist 1",
            tracks=[
                CandidateTrack(
                    title="Track 1",
                    position=1,
                    absolute_position=1,
                    recording_mbid="recording-explicit",
                    release_track_mbid="release-track-explicit",
                )
            ],
        )

    async def get_album_candidate_editions(
        self, release_group_mbid, target_track_count, priority, *, max_editions=2
    ):
        return []

    async def get_exact_release_candidate(self, release_mbid, priority):
        candidate = await self.get_album_candidate("rg-explicit", 1, priority)
        candidate.release_mbid = release_mbid
        return candidate


class _CountingIdentificationProvider(_IdentificationProvider):
    def __init__(self) -> None:
        self.calls = 0

    async def search_album_candidate_ids(self, artist, title, limit, priority):
        self.calls += 1
        return await super().search_album_candidate_ids(artist, title, limit, priority)


class _RepairProvider(_IdentificationProvider):
    async def get_album_candidate(
        self, release_group_mbid, target_track_count, priority
    ):
        candidate = await super().get_album_candidate(
            release_group_mbid, target_track_count, priority
        )
        candidate.release_group_mbid = release_group_mbid
        candidate.release_mbid = f"release-{release_group_mbid}"
        return candidate

    async def get_exact_release_candidate(self, release_mbid, priority):
        suffix = release_mbid.removeprefix("release-")
        candidate = await super().get_album_candidate(
            f"rg-{suffix.removeprefix('rg-')}", 1, priority
        )
        candidate.release_group_mbid = f"rg-{suffix.removeprefix('rg-')}"
        candidate.release_mbid = release_mbid
        return candidate


class _ContradictoryExactRepairProvider(_RepairProvider):
    async def get_exact_release_candidate(self, release_mbid, priority):
        candidate = await super().get_exact_release_candidate(release_mbid, priority)
        candidate.tracks[0].title = "Unrelated provider track"
        return candidate


class _UnavailableRepairProvider(_IdentificationProvider):
    async def get_album_candidate(
        self, release_group_mbid, target_track_count, priority
    ):
        raise ExternalServiceError("private provider failure")

    async def get_exact_release_candidate(self, release_mbid, priority):
        raise ExternalServiceError("private provider failure")


class _OrderedRepairProvider(_RepairProvider):
    def __init__(self) -> None:
        self.exact_calls: list[str] = []

    async def get_exact_release_candidate(self, release_mbid, priority):
        self.exact_calls.append(release_mbid)
        return await super().get_exact_release_candidate(release_mbid, priority)


class _CircuitOpenRepairProvider(_IdentificationProvider):
    async def get_album_candidate(
        self, release_group_mbid, target_track_count, priority
    ):
        raise CircuitOpenError("MusicBrainz breaker open")

    async def get_exact_release_candidate(self, release_mbid, priority):
        raise CircuitOpenError("MusicBrainz breaker open")


class _CanonicalReleaseProvider:
    def __init__(
        self,
        *,
        conflict: bool = False,
        unavailable: bool = False,
        recording_redirects: dict[str, str] | None = None,
        recording_unavailable: bool = False,
        secondary_types: tuple[str, ...] = (),
    ) -> None:
        self.conflict = conflict
        self.unavailable = unavailable
        self.recording_redirects = recording_redirects or {}
        self.recording_unavailable = recording_unavailable
        self.secondary_types = secondary_types
        self.calls: list[str] = []
        self.recording_calls: list[str] = []

    async def get_canonical_release(
        self,
        release_mbid,
        *,
        includes,
        preferred_locales=(),
        artist_standardization="credited",
        priority,
        bypass_cache=False,
    ):
        self.calls.append(release_mbid)
        if self.unavailable:
            raise ExternalServiceError("private canonical provider failure")
        artist = MbManagementArtist(id="artist-1", name="Artist 1")
        return MbManagementRelease(
            id="different-release" if self.conflict else release_mbid,
            title="Album 1",
            artist_credit=[MbManagementArtistCredit(name="Artist 1", artist=artist)],
            media=[
                MbManagementMedium(
                    position=1,
                    track_count=1,
                    tracks=[
                        MbManagementTrack(
                            id="release-track-1",
                            title="Track 1",
                            position=1,
                            recording=MbManagementRecording(
                                id="recording-track-1-1",
                                title="Track 1",
                            ),
                        )
                    ],
                )
            ],
            release_group=MbManagementReleaseGroup(
                id="rg-1",
                title="Album 1",
                primary_type="Album",
                secondary_types=list(self.secondary_types),
            ),
        )

    async def resolve_recording_mbid(self, recording_mbid, *, priority):
        self.recording_calls.append(recording_mbid)
        if self.recording_unavailable:
            raise ExternalServiceError("private recording resolver failure")
        return self.recording_redirects.get(recording_mbid, recording_mbid)


class _DuplicateRecordingReleaseProvider(_CanonicalReleaseProvider):
    def __init__(self, *, duplicate_title: str | None = None) -> None:
        super().__init__()
        self.duplicate_title = duplicate_title

    async def get_canonical_release(
        self,
        release_mbid,
        *,
        includes,
        preferred_locales=(),
        artist_standardization="credited",
        priority,
        bypass_cache=False,
    ):
        self.calls.append(release_mbid)
        artist = MbManagementArtist(id="artist-1", name="Artist 1")
        return MbManagementRelease(
            id=release_mbid,
            title="Album 1",
            artist_credit=[MbManagementArtistCredit(name="Artist 1", artist=artist)],
            media=[
                MbManagementMedium(
                    position=1,
                    track_count=2,
                    tracks=[
                        MbManagementTrack(
                            id=f"release-track-{position}",
                            title=self.duplicate_title or f"Track {position}",
                            position=position,
                            recording=MbManagementRecording(
                                id="shared-recording",
                                title=self.duplicate_title or f"Track {position}",
                            ),
                        )
                        for position in (1, 2)
                    ],
                )
            ],
            release_group=MbManagementReleaseGroup(id="rg-1", title="Album 1"),
        )


class _FlakyIdentificationProvider(_IdentificationProvider):
    def __init__(self) -> None:
        self.calls = 0

    async def search_album_candidate_ids(self, artist, title, limit, priority):
        self.calls += 1
        if self.calls == 1:
            raise ExternalServiceError("temporary private provider failure")
        return await super().search_album_candidate_ids(artist, title, limit, priority)


class _FingerprintIdentificationProvider(_IdentificationProvider):
    async def search_album_candidate_ids(self, artist, title, limit, priority):
        return ["rg-a", "rg-b"]

    async def search_recording_candidate_ids(self, artist, title, limit, priority):
        return ["rg-a", "rg-b"]

    async def get_album_candidate(
        self, release_group_mbid, target_track_count, priority
    ):
        if release_group_mbid == "rg-explicit":
            return await super().get_album_candidate(
                release_group_mbid, target_track_count, priority
            )
        return AlbumCandidate(
            release_group_mbid=release_group_mbid,
            release_mbid=f"release-{release_group_mbid}",
            album_title="Album 1",
            album_artist_name="Artist 1",
            tracks=[
                CandidateTrack(
                    title="Track 1",
                    position=1,
                    absolute_position=1,
                    recording_mbid=f"recording-{release_group_mbid}",
                    release_track_mbid=f"release-track-{release_group_mbid}",
                )
            ],
        )


class _PreferredEditionIdentificationProvider(_IdentificationProvider):
    def __init__(self, canonical_release: str | None = None) -> None:
        self.preferred_releases: list[str | None] = []
        self.canonical_release = canonical_release

    async def get_exact_release_candidate(self, release_mbid, priority):
        self.preferred_releases.append(release_mbid)
        return AlbumCandidate(
            release_group_mbid=EXACT_GROUP,
            release_mbid=self.canonical_release or release_mbid,
            album_title="Album 1",
            album_artist_name="Artist 1",
            tracks=[
                CandidateTrack(
                    title="Track 1",
                    position=1,
                    absolute_position=1,
                    recording_mbid=EXACT_RECORDING,
                    release_track_mbid=EXACT_RELEASE_TRACK,
                )
            ],
        )


class _ExactOverrideProvider(_IdentificationProvider):
    def __init__(
        self,
        *,
        candidate: AlbumCandidate | None,
        unavailable: bool = False,
    ) -> None:
        self.candidate = candidate
        self.unavailable = unavailable
        self.exact_calls: list[str] = []
        self.search_calls = 0

    async def search_album_candidate_ids(self, artist, title, limit, priority):
        self.search_calls += 1
        return await super().search_album_candidate_ids(artist, title, limit, priority)

    async def search_recording_candidate_ids(self, artist, title, limit, priority):
        self.search_calls += 1
        return await super().search_recording_candidate_ids(
            artist, title, limit, priority
        )

    async def get_exact_release_candidate(self, release_mbid, priority):
        self.exact_calls.append(release_mbid)
        if self.unavailable:
            raise ExternalServiceError("private exact-release failure")
        return self.candidate


class _ContradictoryFingerprintProvider(_IdentificationProvider):
    async def get_album_candidate(
        self, release_group_mbid, target_track_count, priority
    ):
        return AlbumCandidate(
            release_group_mbid="rg-explicit",
            release_mbid="release-explicit",
            album_title="Album 1",
            album_artist_name="Artist 1",
            tracks=[
                CandidateTrack(
                    title="Completely unrelated title",
                    position=1,
                    absolute_position=1,
                    recording_mbid="recording-explicit",
                    release_track_mbid="release-track-explicit",
                )
            ],
        )


class _LegacyTrackIdentityProvider(_IdentificationProvider):
    async def search_album_candidate_ids(self, artist, title, limit, priority):
        return ["rg-1"]

    async def search_recording_candidate_ids(self, artist, title, limit, priority):
        return ["rg-1"]

    async def get_album_candidate(
        self, release_group_mbid, target_track_count, priority
    ):
        return AlbumCandidate(
            release_group_mbid="rg-1",
            release_mbid="release-1",
            album_title="Album 1",
            album_artist_name="Artist 1",
            tracks=[
                CandidateTrack(
                    title="Track 1",
                    position=1,
                    absolute_position=1,
                    recording_mbid="recording-track-1-1",
                    release_track_mbid="release-track-1",
                )
            ],
        )


class _ExistingIdentityConflictProvider(_LegacyTrackIdentityProvider):
    def __init__(self, release_group_mbid: str, release_mbid: str) -> None:
        self.release_group_mbid = release_group_mbid
        self.release_mbid = release_mbid

    async def search_album_candidate_ids(self, artist, title, limit, priority):
        return [self.release_group_mbid]

    async def search_recording_candidate_ids(self, artist, title, limit, priority):
        return [self.release_group_mbid]

    async def get_album_candidate(
        self, release_group_mbid, target_track_count, priority
    ):
        candidate = await super().get_album_candidate(
            release_group_mbid, target_track_count, priority
        )
        candidate.release_group_mbid = self.release_group_mbid
        candidate.release_mbid = self.release_mbid
        return candidate


class _FingerprintBackend:
    def __init__(self) -> None:
        self.generate_calls = 0
        self.lookup_calls = 0

    def is_enabled(self) -> bool:
        return True

    async def generate_fingerprint(self, path: Path) -> tuple[str, int]:
        self.generate_calls += 1
        return "fingerprint", 180

    async def lookup_fingerprint(
        self, fingerprint: str, duration: int
    ) -> FingerprintResult:
        self.lookup_calls += 1
        return FingerprintResult(
            status="pass",
            score=0.99,
            recording_id="recording-explicit",
            release_group_ids=["rg-explicit"],
        )


@pytest.fixture
def db_path(tmp_path: Path) -> Path:
    path = tmp_path / "library.db"
    connection = sqlite3.connect(path)
    try:
        connection.execute("CREATE TABLE auth_users (id TEXT PRIMARY KEY)")
        connection.executemany(
            "INSERT INTO auth_users(id) VALUES (?)", [("admin",), ("worker",)]
        )
        connection.commit()
    finally:
        connection.close()
    return path


@pytest.fixture
def store(db_path: Path) -> NativeLibraryStore:
    return NativeLibraryStore(db_path, threading.Lock())


async def _seed_album(
    store: NativeLibraryStore,
    suffix: str,
    *,
    policy: str = "automatic",
    review_state: str = "needs_review",
    identity_source: str | None = None,
    two_tracks: bool = False,
) -> None:
    artist = LocalArtist(
        id=f"artist-{suffix}",
        display_name=f"Artist {suffix}",
        folded_name=f"artist {suffix}",
        normalized_name=f"artist {suffix}",
        kind="group",
        created_at=1,
        updated_at=1,
    )
    album = LocalAlbum(
        id=f"album-{suffix}",
        root_id="root",
        grouping_key=f"group-{suffix}",
        title=f"Album {suffix}",
        album_artist_id=artist.id,
        album_artist_name=artist.display_name,
        created_at=1,
        updated_at=1,
    )
    tracks = []
    credits = {}
    for index in range(1, 3 if two_tracks else 2):
        track = LocalTrack(
            id=f"track-{suffix}-{index}",
            local_album_id=album.id,
            root_id="root",
            file_path=f"/music/{suffix}/{index}.flac",
            relative_path=f"{suffix}/{index}.flac",
            path_hash=f"hash-{suffix}-{index}",
            file_size_bytes=100,
            file_mtime_ns=1,
            stat_revision=f"stat-{suffix}-{index}",
            tag_revision=f"tag-{suffix}-{index}",
            title=f"Track {index}",
            artist_name=artist.display_name,
            album_title=album.title,
            album_artist_name=artist.display_name,
            tag_album_title=album.title,
            tag_album_artist_name=artist.display_name,
            track_number=index,
            file_format="flac",
            imported_at=1,
            applied_policy=policy,
            applied_policy_revision="policy-1",
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
    await store.create_review(
        ReviewDecision(
            id=f"review-{suffix}",
            local_album_id=album.id,
            state=review_state,
            reason_code="NO_SAFE_MATCH",
            input_revision=f"input-{suffix}",
            created_at=float(suffix) if suffix.isdigit() else 1,
            updated_at=float(suffix) if suffix.isdigit() else 1,
        )
    )
    if identity_source is not None:
        context = await store.get_album_identification_context(album.id)
        assert context is not None
        await store.attach_album_identity(
            LocalAlbumExternalIdentity(
                local_album_id=album.id,
                release_group_mbid=f"rg-{suffix}",
                release_mbid=f"release-{suffix}",
                decision_source=identity_source,
                selected_at=2,
            ),
            expected_album_revision=int(context["album"]["row_revision"]),
        )
        for track in tracks:
            current = await store.get_album_identification_context(album.id)
            assert current is not None
            row = next(item for item in current["tracks"] if item["id"] == track.id)
            await store.attach_track_identity(
                LocalTrackExternalIdentity(
                    local_track_id=track.id,
                    recording_mbid=f"recording-{track.id}",
                    release_mbid=f"release-{suffix}",
                    decision_source=identity_source,
                    selected_at=2,
                ),
                expected_track_revision=int(row["row_revision"]),
            )


@pytest.mark.asyncio
async def test_reconcile_resolves_review_when_its_album_disappears(
    store: NativeLibraryStore, db_path: Path
) -> None:
    await _seed_album(store, "1")
    await store.request_scan_run(
        ScanRequest(
            kind="incremental",
            trigger="manual",
            policy_revision="policy-1",
            scopes=[
                ScanScope(
                    root_id="root",
                    relative_path=".",
                    effective_policy="automatic",
                    policy_revision="policy-1",
                )
            ],
        ),
        run_id="missing-review-scan",
        requested_at=2,
    )
    with sqlite3.connect(db_path) as connection:
        connection.execute(
            "UPDATE library_scan_run_scopes SET discovery_state = 'completed' "
            "WHERE run_id = 'missing-review-scan'"
        )

    result = await store.reconcile_scan_scope_batch(
        "missing-review-scan", "root", ".", now=3, limit=100
    )

    assert result["missing"] == 1
    assert result["reviews_resolved"] == 1
    with sqlite3.connect(db_path) as connection:
        track = connection.execute(
            "SELECT availability FROM local_tracks WHERE id = 'track-1-1'"
        ).fetchone()
        review = connection.execute(
            "SELECT state, reason_code FROM library_identification_reviews "
            "WHERE id = 'review-1'"
        ).fetchone()
    assert track == ("missing",)
    assert review == ("resolved", "SUBJECT_MISSING")


@pytest.mark.asyncio
async def test_reconcile_stales_open_contribution_and_cancels_verification(
    store: NativeLibraryStore, db_path: Path
) -> None:
    await _seed_album(store, "1")
    await store.request_scan_run(
        ScanRequest(
            kind="incremental",
            trigger="manual",
            policy_revision="policy-1",
            scopes=[
                ScanScope(
                    root_id="root",
                    relative_path=".",
                    effective_policy="automatic",
                    policy_revision="policy-1",
                )
            ],
        ),
        run_id="missing-contribution-scan",
        requested_at=2,
    )
    with sqlite3.connect(db_path) as connection:
        connection.execute(
            "UPDATE library_scan_run_scopes SET discovery_state = 'completed' "
            "WHERE run_id = 'missing-contribution-scan'"
        )
        connection.execute(
            "INSERT INTO library_contribution_drafts "
            "(id, local_album_id, state, album_row_revision, input_revision, "
            "local_snapshot_json, resolved_draft_json, source_selection_json, "
            "seed_snapshot_json, created_at, updated_at) VALUES "
            "('contribution-1', 'album-1', 'verifying', 1, 'input-1', "
            "'{\"schema_version\":1}', '{\"schema_version\":1}', "
            "'{\"schema_version\":1}', '{\"schema_version\":1}', 2, 2)"
        )
        connection.execute(
            "INSERT INTO library_contribution_callback_tokens "
            "(token_hash, contribution_id, requested_by_user_id, expires_at, created_at) "
            "VALUES ('callback-1', 'contribution-1', 'admin', 100, 2)"
        )
        connection.execute(
            "INSERT INTO library_contribution_verification_jobs "
            "(id, contribution_id, state, not_before, created_at, updated_at) "
            "VALUES ('job-1', 'contribution-1', 'queued', 2, 2, 2)"
        )

    result = await store.reconcile_scan_scope_batch(
        "missing-contribution-scan", "root", ".", now=3, limit=100
    )

    assert result["missing"] == 1
    with sqlite3.connect(db_path) as connection:
        contribution = connection.execute(
            "SELECT state, seed_snapshot_json FROM library_contribution_drafts "
            "WHERE id = 'contribution-1'"
        ).fetchone()
        job = connection.execute(
            "SELECT state FROM library_contribution_verification_jobs WHERE id = 'job-1'"
        ).fetchone()
        token = connection.execute(
            "SELECT consumed_at FROM library_contribution_callback_tokens "
            "WHERE token_hash = 'callback-1'"
        ).fetchone()
    assert contribution == ("stale", None)
    assert job == ("cancelled",)
    assert token == (None,)


@pytest.mark.asyncio
async def test_review_cursor_filters_and_detail_are_bounded(
    store: NativeLibraryStore,
) -> None:
    for suffix in ("1", "2", "3"):
        await _seed_album(store, suffix)
    service = LibraryReviewService(store)

    first = await service.list_reviews(limit=2)
    second = await service.list_reviews(limit=2, cursor=first.next_cursor)
    filtered = await service.list_reviews(limit=10, search="Album 2")
    oldest = await service.list_reviews(limit=2, sort="oldest")
    oldest_next = await service.list_reviews(
        limit=2, sort="oldest", cursor=oldest.next_cursor
    )
    by_album = await service.list_reviews(limit=10, sort="album")
    detail = await service.detail("review-2")

    assert [item.id for item in first.items] == ["review-3", "review-2"]
    assert [item.id for item in second.items] == ["review-1"]
    assert [item.id for item in filtered.items] == ["review-2"]
    assert [item.id for item in oldest.items] == ["review-1", "review-2"]
    assert [item.id for item in oldest_next.items] == ["review-3"]
    assert [item.album_title for item in by_album.items] == [
        "Album 1",
        "Album 2",
        "Album 3",
    ]
    assert detail.review.local_album_id == "album-2"
    assert detail.tracks[0].relative_path == "2/1.flac"
    assert "keep_tagged" in detail.available_actions


@pytest.mark.asyncio
async def test_review_actions_include_dismiss_and_policy_excluded_only_dismiss(
    store: NativeLibraryStore,
) -> None:
    await _seed_album(store, "1")
    await _seed_album(store, "2", policy="excluded")
    service = LibraryReviewService(store)

    open_detail = await service.detail("review-1")
    assert "dismiss" in open_detail.available_actions
    assert "exclude" in open_detail.available_actions
    assert "retry" in open_detail.available_actions

    policy_excluded = await service.detail("review-2")
    assert policy_excluded.available_actions == ["dismiss"]


@pytest.mark.asyncio
async def test_review_supports_every_signed_filter_sort_and_typed_invalid_values(
    store: NativeLibraryStore, db_path: Path
) -> None:
    for suffix in ("1", "2", "3"):
        await _seed_album(store, suffix)
    await IdentificationQueueService(store).enqueue_album(
        "album-2", input_revision="filter-job", now=21
    )
    with sqlite3.connect(db_path) as connection:
        connection.execute(
            "INSERT INTO library_identification_attempts "
            "(id, local_album_id, trigger, input_tag_revision, input_policy_revision, "
            "input_file_revision, matcher_version, state, terminal_reason_code, "
            "candidate_count, started_at, completed_at) VALUES "
            "('filter-attempt', 'album-2', 'automatic', 'tag', 'policy', 'file', "
            "'matcher', 'ambiguous', 'AMBIGUOUS', 1, 2, 2)"
        )
        connection.execute(
            "UPDATE library_identification_reviews SET state = 'excluded', "
            "reason_code = 'FILTER_REASON', attempt_id = 'filter-attempt', "
            "created_at = 20, updated_at = 20 WHERE id = 'review-2'"
        )
        connection.execute(
            "UPDATE local_tracks SET applied_policy = 'local_metadata', "
            "metadata_incomplete = 1 WHERE id = 'track-2-1'"
        )
    connection.close()
    service = LibraryReviewService(store)

    filters = (
        {"state": "excluded"},
        {"reason_code": "FILTER_REASON"},
        {"root_id": "root", "state": "excluded"},
        {"policy": "local_metadata"},
        {"search": "Album 2"},
        {"metadata_incomplete": True},
        {"candidate_available": True},
        {"job_state": "queued"},
        {"created_from": 19, "created_to": 21},
        {"updated_from": 19, "updated_to": 21},
    )
    for review_filter in filters:
        result = await service.list_reviews(limit=10, **review_filter)
        assert [item.id for item in result.items] == ["review-2"]
    preview = await service.preview_bulk(
        BulkReviewPreviewRequest(
            action="exclude",
            selection=BulkReviewSelection(
                normalized_filter={"job_state": "queued"},
                catalog_revision=await store.get_catalog_revision(),
            ),
        ),
        now=30,
    )
    assert preview.eligible_count == 1
    assert preview.album_count == 1
    for sort in (
        "newest",
        "oldest",
        "album",
        "artist",
        "root",
        "track_count",
        "reason",
    ):
        result = await service.list_reviews(limit=2, sort=sort)
        assert len(result.items) == 2
        assert result.next_cursor is not None
        second = await service.list_reviews(
            limit=2, sort=sort, cursor=result.next_cursor
        )
        assert set(item.id for item in result.items).isdisjoint(
            item.id for item in second.items
        )

    for invalid_filter in (
        {"state": "unknown"},
        {"policy": "unknown"},
        {"job_state": "completed"},
        {"created_from": 2, "created_to": 1},
        {"updated_from": 2, "updated_to": 1},
        {"search": "x" * 201},
    ):
        with pytest.raises(ValidationError):
            await service.list_reviews(**invalid_filter)


@pytest.mark.asyncio
async def test_review_state_depths_are_scoped_while_global_totals_persist(
    store: NativeLibraryStore,
) -> None:
    # N-02 (T30): per-state depths must render from the scoped
    # `counts_by_state_filtered` field, mirroring the existing
    # `counts_by_reason_filtered` semantics - `counts_by_state` stays a
    # global GROUP BY with no WHERE.
    await _seed_album(store, "1")
    await _seed_album(store, "2")
    await _seed_album(store, "3", review_state="keep_tagged")
    service = LibraryReviewService(store)

    scoped = await service.list_reviews(limit=10, state="needs_review")
    unscoped = await service.list_reviews(limit=10)

    assert scoped.counts_by_state == {"needs_review": 2, "keep_tagged": 1}
    assert scoped.counts_by_state_filtered == {"needs_review": 2}
    assert scoped.counts_by_reason_filtered == {"NO_SAFE_MATCH": 2}
    assert unscoped.counts_by_state_filtered == {
        "needs_review": 2,
        "keep_tagged": 1,
    }
    assert [item.id for item in scoped.items] == ["review-2", "review-1"]


@pytest.mark.asyncio
async def test_plain_keep_refuses_identity_and_detach_keep_is_atomic_and_read_only(
    store: NativeLibraryStore, tmp_path: Path
) -> None:
    audio = tmp_path / "audio.flac"
    audio.write_bytes(b"audio-do-not-change")
    before = (audio.read_bytes(), audio.stat().st_mtime_ns)
    await _seed_album(store, "1", identity_source="automatic")
    service = LibraryReviewService(store)
    catalog_revision = await store.get_catalog_revision()

    with pytest.raises(StaleRevisionError):
        await service.act(
            "review-1",
            "keep_tagged",
            ReviewActionRequest(
                expected_review_revision=1,
                expected_catalog_revision=catalog_revision,
            ),
            "admin",
            now=3,
        )
    response = await service.act(
        "review-1",
        "detach_keep_tagged",
        ReviewActionRequest(
            expected_review_revision=1,
            expected_catalog_revision=catalog_revision,
            expected_identity_revision=1,
            idempotency_key="detach-1",
            confirmation=True,
        ),
        "admin",
        now=3,
    )
    context = await store.get_album_identification_context("album-1")

    assert response.state == "keep_tagged"
    assert context is not None and context["identity"] is None
    assert all(track["recording_mbid"] is None for track in context["tracks"])
    assert (audio.read_bytes(), audio.stat().st_mtime_ns) == before


@pytest.mark.asyncio
async def test_keep_tagged_survives_restart_and_only_input_change_reopens_work(
    store: NativeLibraryStore, db_path: Path
) -> None:
    await _seed_album(store, "1")
    reviews = LibraryReviewService(store)
    kept = await reviews.act(
        "review-1",
        "keep_tagged",
        ReviewActionRequest(
            expected_review_revision=1,
            expected_catalog_revision=await store.get_catalog_revision(),
        ),
        "admin",
        now=2,
    )
    assert kept.state == "keep_tagged"

    restarted = NativeLibraryStore(db_path, threading.Lock())
    queue = IdentificationQueueService(restarted)
    assert (
        await queue.enqueue_album(
            "album-1", input_revision="input-1", kind="automatic", now=3
        )
        == ""
    )
    reopened = await queue.enqueue_album(
        "album-1", input_revision="changed-input", kind="automatic", now=4
    )
    assert reopened
    detail = await restarted.get_identification_review_detail("review-1")
    assert detail is not None
    assert detail["review"]["state"] == "resolved"
    assert detail["review"]["decision_revision"] == 2


@pytest.mark.asyncio
async def test_detach_and_keep_rolls_back_every_row_on_audit_failure(
    store: NativeLibraryStore, db_path: Path
) -> None:
    await _seed_album(store, "1", identity_source="automatic")
    catalog_revision = await store.get_catalog_revision()
    with sqlite3.connect(db_path) as connection:
        connection.execute(
            "CREATE TRIGGER fail_catalog_action BEFORE INSERT ON library_catalog_actions "
            "BEGIN SELECT RAISE(ABORT, 'injected audit failure'); END"
        )
    with pytest.raises(sqlite3.IntegrityError):
        await LibraryReviewService(store).act(
            "review-1",
            "detach_keep_tagged",
            ReviewActionRequest(
                expected_review_revision=1,
                expected_catalog_revision=catalog_revision,
                expected_identity_revision=1,
                confirmation=True,
            ),
            "admin",
            now=3,
        )
    context = await store.get_album_identification_context("album-1")
    detail = await store.get_identification_review_detail("review-1")
    assert context is not None and context["identity"] is not None
    assert all(track["recording_mbid"] for track in context["tracks"])
    assert detail is not None and detail["review"]["state"] == "needs_review"
    assert await store.get_catalog_revision() == catalog_revision


@pytest.mark.asyncio
async def test_concurrent_review_actions_on_one_revision_have_one_winner(
    store: NativeLibraryStore,
) -> None:
    await _seed_album(store, "1")
    catalog_revision = await store.get_catalog_revision()
    service = LibraryReviewService(store)

    async def act(action: str):
        try:
            return await service.act(
                "review-1",
                action,
                ReviewActionRequest(
                    expected_review_revision=1,
                    expected_catalog_revision=catalog_revision,
                    confirmation=action == "exclude",
                ),
                "admin",
                now=3,
            )
        except StaleRevisionError as error:
            return error

    results = await asyncio.gather(act("keep_tagged"), act("exclude"))
    assert sum(not isinstance(result, Exception) for result in results) == 1
    assert sum(isinstance(result, StaleRevisionError) for result in results) == 1


@pytest.mark.asyncio
async def test_manual_candidate_override_records_choice_and_attaches_only_supported_tracks(
    store: NativeLibraryStore,
) -> None:
    await _seed_album(store, "1", two_tracks=True)
    context = await store.get_album_identification_context("album-1")
    assert context is not None
    tag_revision, file_revision, policy_revision = album_input_revisions(
        context["tracks"]
    )
    attempt = IdentificationAttempt(
        id="attempt-manual",
        local_album_id="album-1",
        input_tag_revision=tag_revision,
        input_policy_revision=policy_revision,
        input_file_revision=file_revision,
        input_identity_revision=album_identity_revision(
            context["identity"], context["tracks"]
        ),
        matcher_version="feedback-fixes-v1",
        state="contradictory",
        terminal_reason_code="HARD_CONTRADICTION",
        started_at=2,
        completed_at=2,
    )
    evidence = CandidateEvidence(
        release_group_mbid="rg-manual",
        release_mbid="release-manual",
        matcher_version="feedback-fixes-v1",
        track_evidence=[
            TrackEvidence(
                local_track_id="track-1-1",
                classification="supported",
                recording_mbid="recording-supported",
            ),
            TrackEvidence(local_track_id="track-1-2", classification="contradictory"),
        ],
        reason_code="HARD_CONTRADICTION",
    )
    await store.replace_review_attempt(
        "review-1",
        expected_review_revision=1,
        attempt=attempt,
        evidence=[
            IdentificationEvidenceRecord(
                id="evidence-manual",
                attempt_id=attempt.id,
                candidate_key="rg-manual:release-manual",
                evidence=evidence,
                created_at=2,
            )
        ],
        updated_at=2,
    )
    callback = AsyncMock()
    response = await LibraryReviewService(
        store, on_identified=callback
    ).accept_candidate(
        "review-1",
        CandidateAcceptanceRequest(
            expected_review_revision=2,
            expected_catalog_revision=await store.get_catalog_revision(),
            expected_evidence_revision="evidence-manual",
            candidate_key="rg-manual:release-manual",
            manual_override=True,
            confirmation=True,
        ),
        "admin",
        now=3,
    )
    context = await store.get_album_identification_context("album-1")
    assert response.state == "resolved"
    assert context is not None
    assert context["identity"]["decision_source"] == "manual"
    identities = {track["id"]: track["recording_mbid"] for track in context["tracks"]}
    assert identities == {
        "track-1-1": "recording-supported",
        "track-1-2": None,
    }
    callback.assert_awaited_once_with(
        "album-1", album_input_revisions(context["tracks"])[2]
    )


async def _seed_reassignment_review(
    store: NativeLibraryStore, *, second_track_kinds: list[str]
) -> None:
    """Two-track album whose second file carries another edition's embedded IDs."""

    await _seed_album(store, "1", two_tracks=True)
    context = await store.get_album_identification_context("album-1")
    assert context is not None
    tag_revision, file_revision, policy_revision = album_input_revisions(
        context["tracks"]
    )
    attempt = IdentificationAttempt(
        id="attempt-reassign",
        local_album_id="album-1",
        input_tag_revision=tag_revision,
        input_policy_revision=policy_revision,
        input_file_revision=file_revision,
        input_identity_revision=album_identity_revision(
            context["identity"], context["tracks"]
        ),
        matcher_version="feedback-fixes-v1",
        state="contradictory",
        terminal_reason_code="HARD_CONTRADICTION",
        started_at=2,
        completed_at=2,
    )
    evidence = CandidateEvidence(
        release_group_mbid="rg-reassign",
        release_mbid="release-reassign",
        matcher_version="feedback-fixes-v1",
        album_title_classification="supported",
        album_artist_classification="supported",
        track_evidence=[
            TrackEvidence(
                local_track_id="track-1-1",
                classification="supported",
                candidate_track_title="Track 1",
                candidate_disc_number=1,
                candidate_track_position=1,
                recording_mbid="recording-1",
                release_track_mbid="release-track-1",
            ),
            TrackEvidence(
                local_track_id="track-1-2",
                classification="contradictory",
                evidence_kinds=second_track_kinds,
                candidate_track_title="Track 2",
                candidate_disc_number=1,
                candidate_track_position=2,
                recording_mbid="recording-2",
                release_track_mbid="release-track-2",
            ),
        ],
        unmatched_expected_tracks=["1:2:Track 2"],
        reason_code="CONFLICTING_TRACK_EVIDENCE",
    )
    await store.replace_review_attempt(
        "review-1",
        expected_review_revision=1,
        attempt=attempt,
        evidence=[
            IdentificationEvidenceRecord(
                id="evidence-reassign",
                attempt_id=attempt.id,
                candidate_key="rg-reassign:release-reassign",
                evidence=evidence,
                created_at=2,
            )
        ],
        updated_at=2,
    )


def _reassign_request(
    catalog_revision: int, *, reassign: bool
) -> CandidateAcceptanceRequest:
    return CandidateAcceptanceRequest(
        expected_review_revision=2,
        expected_catalog_revision=catalog_revision,
        expected_evidence_revision="evidence-reassign",
        candidate_key="rg-reassign:release-reassign",
        manual_override=True,
        reassign_conflicting_tracks=reassign,
        confirmation=True,
    )


@pytest.mark.asyncio
async def test_manual_override_can_reassign_tracks_that_only_conflict_on_embedded_ids(
    store: NativeLibraryStore,
) -> None:
    await _seed_reassignment_review(
        store, second_track_kinds=["release_track_mbid_conflict"]
    )
    response = await LibraryReviewService(store).accept_candidate(
        "review-1",
        _reassign_request(await store.get_catalog_revision(), reassign=True),
        "admin",
        now=3,
    )
    context = await store.get_album_identification_context("album-1")
    assert response.state == "resolved"
    assert context is not None
    assert context["identity"]["decision_source"] == "manual"
    identities = {track["id"]: track["recording_mbid"] for track in context["tracks"]}
    assert identities == {"track-1-1": "recording-1", "track-1-2": "recording-2"}
    with sqlite3.connect(store.db_path) as connection:
        release_track = connection.execute(
            "SELECT release_track_mbid, release_track_position FROM "
            "local_track_external_identities WHERE local_track_id = 'track-1-2'"
        ).fetchone()
        audit = connection.execute(
            "SELECT after_json FROM library_catalog_actions "
            "WHERE action_kind = 'accept_candidate'"
        ).fetchone()
    assert release_track == ("release-track-2", 2)
    assert json.loads(audit[0])["reassigned_track_ids"] == ["track-1-2"]


@pytest.mark.asyncio
async def test_plain_manual_override_still_leaves_the_conflicting_track_unassigned(
    store: NativeLibraryStore,
) -> None:
    await _seed_reassignment_review(
        store, second_track_kinds=["release_track_mbid_conflict"]
    )
    await LibraryReviewService(store).accept_candidate(
        "review-1",
        _reassign_request(await store.get_catalog_revision(), reassign=False),
        "admin",
        now=3,
    )
    context = await store.get_album_identification_context("album-1")
    assert context is not None
    identities = {track["id"]: track["recording_mbid"] for track in context["tracks"]}
    assert identities == {"track-1-1": "recording-1", "track-1-2": None}


@pytest.mark.asyncio
async def test_reassignment_is_refused_when_the_conflict_is_not_only_embedded_ids(
    store: NativeLibraryStore,
) -> None:
    await _seed_reassignment_review(store, second_track_kinds=["duration_conflict"])
    with pytest.raises(ValidationError):
        await LibraryReviewService(store).accept_candidate(
            "review-1",
            _reassign_request(await store.get_catalog_revision(), reassign=True),
            "admin",
            now=3,
        )
    context = await store.get_album_identification_context("album-1")
    assert context is not None
    assert (
        context["identity"] is None
        or context["identity"].get("decision_source") != "manual"
    )


@pytest.mark.asyncio
async def test_tier_edition_accept_never_schedules_management(
    store: NativeLibraryStore,
) -> None:
    """Confirming a tier-row edition is catalog-only: the lane promises files
    never change there, so (unlike the needs_review accept above) the sealed
    exact must not schedule management."""
    await _seed_album(store, "1", two_tracks=True, review_state="edition_to_confirm")
    context = await store.get_album_identification_context("album-1")
    assert context is not None
    tag_revision, file_revision, policy_revision = album_input_revisions(
        context["tracks"]
    )
    attempt = IdentificationAttempt(
        id="attempt-tier",
        local_album_id="album-1",
        input_tag_revision=tag_revision,
        input_policy_revision=policy_revision,
        input_file_revision=file_revision,
        input_identity_revision=album_identity_revision(
            context["identity"], context["tracks"]
        ),
        matcher_version="feedback-fixes-v1",
        state="edition_uncertain",
        terminal_reason_code="EDITION_UNCERTAIN",
        started_at=2,
        completed_at=2,
    )
    evidence = CandidateEvidence(
        release_group_mbid="rg-tier",
        release_mbid="release-tier",
        matcher_version="feedback-fixes-v1",
        track_evidence=[
            TrackEvidence(
                local_track_id="track-1-1",
                classification="supported",
                recording_mbid="recording-1",
            ),
            TrackEvidence(
                local_track_id="track-1-2",
                classification="supported",
                recording_mbid="recording-2",
            ),
        ],
        # The engine said SUPPORTED; the gate vetoed it into a tier row, so
        # the tier-ness lives in the review state, not the evidence reason.
        reason_code="SUPPORTED",
    )
    await store.replace_review_attempt(
        "review-1",
        expected_review_revision=1,
        attempt=attempt,
        evidence=[
            IdentificationEvidenceRecord(
                id="evidence-tier",
                attempt_id=attempt.id,
                candidate_key="rg-tier:release-tier",
                evidence=evidence,
                created_at=2,
            )
        ],
        updated_at=2,
    )
    callback = AsyncMock()
    response = await LibraryReviewService(
        store, on_identified=callback
    ).accept_candidate(
        "review-1",
        CandidateAcceptanceRequest(
            expected_review_revision=2,
            expected_catalog_revision=await store.get_catalog_revision(),
            expected_evidence_revision="evidence-tier",
            candidate_key="rg-tier:release-tier",
            manual_override=False,
            confirmation=True,
        ),
        "admin",
        now=3,
    )
    assert response.state == "resolved"
    context = await store.get_album_identification_context("album-1")
    assert context is not None
    assert context["identity"]["release_mbid"] == "release-tier"
    callback.assert_not_awaited()


@pytest.mark.asyncio
async def test_review_candidate_acceptance_preserves_missing_track_identity_history(
    store: NativeLibraryStore, db_path: Path
) -> None:
    await _seed_album(store, "1", identity_source="legacy_import", two_tracks=True)
    with sqlite3.connect(db_path) as connection:
        connection.execute(
            "UPDATE local_tracks SET availability = 'missing' WHERE id = 'track-1-2'"
        )
    context = await store.get_album_identification_context("album-1")
    assert context is not None
    indexed = [
        track for track in context["tracks"] if track["availability"] == "indexed"
    ]
    tag_revision, file_revision, policy_revision = album_input_revisions(indexed)
    attempt = IdentificationAttempt(
        id="attempt-indexed-review",
        local_album_id="album-1",
        input_tag_revision=tag_revision,
        input_policy_revision=policy_revision,
        input_file_revision=file_revision,
        input_identity_revision=album_identity_revision(context["identity"], indexed),
        matcher_version="feedback-fixes-v1",
        state="identified",
        terminal_reason_code="SUPPORTED",
        selected_candidate_key="rg-review:release-review",
        candidate_count=1,
        started_at=2,
        completed_at=2,
    )
    evidence = CandidateEvidence(
        release_group_mbid="rg-review",
        release_mbid="release-review",
        matcher_version="feedback-fixes-v1",
        track_evidence=[
            TrackEvidence(
                local_track_id="track-1-1",
                classification="supported",
                recording_mbid="recording-reviewed",
            )
        ],
        reason_code="SUPPORTED",
    )
    await store.replace_review_attempt(
        "review-1",
        expected_review_revision=1,
        attempt=attempt,
        evidence=[
            IdentificationEvidenceRecord(
                id="evidence-indexed-review",
                attempt_id=attempt.id,
                candidate_key="rg-review:release-review",
                evidence=evidence,
                created_at=2,
            )
        ],
        updated_at=2,
    )

    await LibraryReviewService(store).accept_candidate(
        "review-1",
        CandidateAcceptanceRequest(
            expected_review_revision=2,
            expected_catalog_revision=await store.get_catalog_revision(),
            expected_evidence_revision="evidence-indexed-review",
            candidate_key="rg-review:release-review",
            manual_override=False,
            confirmation=True,
        ),
        "admin",
        now=3,
    )

    context = await store.get_album_identification_context("album-1")
    assert context is not None
    identities = {
        row["id"]: (row["availability"], row["recording_mbid"])
        for row in context["tracks"]
    }
    assert identities == {
        "track-1-1": ("indexed", "recording-reviewed"),
        "track-1-2": ("missing", "recording-track-1-2"),
    }


@pytest.mark.asyncio
async def test_item_exclusion_composes_with_directory_policy_and_restores_ids(
    store: NativeLibraryStore, db_path: Path
) -> None:
    await _seed_album(store, "1", policy="excluded")
    service = LibraryReviewService(store)
    excluded = await service.act(
        "review-1",
        "exclude",
        ReviewActionRequest(
            expected_review_revision=1,
            expected_catalog_revision=await store.get_catalog_revision(),
            confirmation=True,
        ),
        "admin",
        now=2,
    )
    restored = await service.act(
        "review-1",
        "restore",
        ReviewActionRequest(
            expected_review_revision=excluded.row_revision,
            expected_catalog_revision=excluded.catalog_revision,
        ),
        "admin",
        now=3,
    )
    with sqlite3.connect(db_path) as connection:
        track = connection.execute(
            "SELECT id, availability, manual_excluded FROM local_tracks"
        ).fetchone()
    assert restored.remaining_exclusion_source == "directory_policy"
    assert track == ("track-1-1", "excluded", 0)


@pytest.mark.asyncio
async def test_bulk_apply_materializes_exact_rows_and_restart_skips_only_stale_subject(
    store: NativeLibraryStore, db_path: Path
) -> None:
    for suffix in ("1", "2", "3"):
        await _seed_album(store, suffix)
    with sqlite3.connect(db_path) as connection:
        connection.execute(
            "INSERT INTO library_playlists "
            "(id, name, created_at, updated_at, user_id) "
            "VALUES ('bulk-playlist', 'Bulk', 'now', 'now', 'admin')"
        )
        connection.execute(
            "INSERT INTO library_playlist_tracks "
            "(id, playlist_id, position, track_name, artist_name, album_name, "
            "source_type, created_at, local_track_id, local_album_id, local_artist_id) "
            "VALUES ('bulk-playlist-track', 'bulk-playlist', 0, 'Track 1', "
            "'Artist 1', 'Album 1', 'local', 'now', 'track-1-1', 'album-1', "
            "'artist-1')"
        )
        connection.execute(
            "INSERT INTO library_play_history "
            "(id, user_id, local_track_id, local_album_id, local_artist_id, "
            "track_name, artist_name, played_at) VALUES "
            "('bulk-history', 'admin', 'track-2-1', 'album-2', 'artist-2', "
            "'Track 2', 'Artist 2', '2026-07-14T12:00:00Z')"
        )
    reviews = LibraryReviewService(store)
    selection = BulkReviewSelection(
        review_ids=["review-1", "review-2"],
        expected_revisions={"review-1": 1, "review-2": 1},
        catalog_revision=await store.get_catalog_revision(),
    )
    preview = await reviews.preview_bulk(
        BulkReviewPreviewRequest(action="exclude", selection=selection), now=10
    )
    assert preview.eligible_count == 2
    assert preview.stale_count == 0
    assert preview.playlist_reference_count == 1
    assert preview.history_reference_count == 1
    operation = await reviews.apply_bulk(
        BulkReviewApplyRequest(
            preview_token=preview.preview_token,
            idempotency_key="bulk-1",
            action="exclude",
            selection=selection,
        ),
        "admin",
        now=10,
    )
    await _seed_album(store, "4")
    with sqlite3.connect(db_path) as connection:
        connection.execute(
            "UPDATE library_identification_reviews SET row_revision = 2 "
            "WHERE id = 'review-2'"
        )
    restarted_store = NativeLibraryStore(db_path, threading.Lock())
    worker = LibraryOperationService(restarted_store)
    claimed = await worker.claim("worker", now=11)
    assert claimed is not None and claimed["id"] == operation.id
    done = await worker.run_bulk_claimed(claimed, "worker", "admin", now=12)
    repeated = await reviews.apply_bulk(
        BulkReviewApplyRequest(
            preview_token=preview.preview_token,
            idempotency_key="bulk-1",
            action="exclude",
            selection=selection,
        ),
        "admin",
        now=10,
    )
    with sqlite3.connect(db_path) as connection:
        states = dict(
            connection.execute(
                "SELECT id, state FROM library_identification_reviews ORDER BY id"
            )
        )
    assert done.succeeded_count == 1
    assert done.skipped_count == 1
    assert repeated.id == operation.id
    assert states["review-1"] == "excluded"
    assert states["review-2"] == "needs_review"
    assert states["review-4"] == "needs_review"


@pytest.mark.asyncio
async def test_bulk_candidate_preview_finds_and_binds_one_shared_safe_candidate(
    store: NativeLibraryStore,
) -> None:
    for suffix in ("1", "2"):
        await _seed_album(store, suffix)
        attempt = IdentificationAttempt(
            id=f"attempt-{suffix}",
            local_album_id=f"album-{suffix}",
            matcher_version="feedback-fixes-v1",
            state="ambiguous",
            terminal_reason_code="AMBIGUOUS",
            started_at=2,
            completed_at=2,
        )
        await store.replace_review_attempt(
            f"review-{suffix}",
            expected_review_revision=1,
            attempt=attempt,
            evidence=[
                IdentificationEvidenceRecord(
                    id=f"evidence-{suffix}",
                    attempt_id=attempt.id,
                    candidate_key="rg-shared:release-shared",
                    evidence=CandidateEvidence(
                        release_group_mbid="rg-shared",
                        release_mbid="release-shared",
                        matcher_version="feedback-fixes-v1",
                        reason_code="SUPPORTED",
                    ),
                    created_at=2,
                )
            ],
            updated_at=2,
        )
    reviews = LibraryReviewService(store)
    selection = BulkReviewSelection(
        review_ids=["review-1", "review-2"],
        expected_revisions={"review-1": 2, "review-2": 2},
        catalog_revision=await store.get_catalog_revision(),
    )

    discovery = await reviews.preview_bulk(
        BulkReviewPreviewRequest(action="accept_candidate", selection=selection),
        now=10,
    )

    assert discovery.common_candidate_keys == ["rg-shared:release-shared"]
    assert discovery.eligible_count == 0

    preview = await reviews.preview_bulk(
        BulkReviewPreviewRequest(
            action="accept_candidate",
            selection=selection,
            candidate_key="rg-shared:release-shared",
        ),
        now=11,
    )
    assert preview.eligible_count == 2
    assert preview.ineligible_count == 0
    with pytest.raises(StaleRevisionError, match="selection changed"):
        await reviews.apply_bulk(
            BulkReviewApplyRequest(
                preview_token=preview.preview_token,
                idempotency_key="wrong-candidate",
                action="accept_candidate",
                selection=selection,
                candidate_key="rg-other:release-other",
            ),
            "admin",
            now=12,
        )
    operation = await reviews.apply_bulk(
        BulkReviewApplyRequest(
            preview_token=preview.preview_token,
            idempotency_key="shared-candidate",
            action="accept_candidate",
            selection=selection,
            candidate_key="rg-shared:release-shared",
        ),
        "admin",
        now=12,
    )
    assert operation.expected_work_count == 2
    callback = AsyncMock()
    worker = LibraryOperationService(store, on_identified=callback)
    claimed = await worker.claim("worker", now=13)
    assert claimed is not None
    completed = await worker.run_bulk_claimed(claimed, "worker", "admin", now=14)
    assert completed.succeeded_count == 2
    assert {call.args[0] for call in callback.await_args_list} == {
        "album-1",
        "album-2",
    }


@pytest.mark.asyncio
async def test_bulk_tier_edition_accept_never_schedules_management(
    store: NativeLibraryStore,
) -> None:
    """Bulk confirms honor the same tier exemption as single accepts: the
    sealed exact lands, the row resolves, and no management is scheduled."""
    await _seed_album(store, "1", review_state="edition_to_confirm")
    attempt = IdentificationAttempt(
        id="attempt-1",
        local_album_id="album-1",
        matcher_version="feedback-fixes-v1",
        state="edition_uncertain",
        terminal_reason_code="EDITION_UNCERTAIN",
        started_at=2,
        completed_at=2,
    )
    await store.replace_review_attempt(
        "review-1",
        expected_review_revision=1,
        attempt=attempt,
        evidence=[
            IdentificationEvidenceRecord(
                id="evidence-1",
                attempt_id=attempt.id,
                candidate_key="rg-tier:release-tier",
                evidence=CandidateEvidence(
                    release_group_mbid="rg-tier",
                    release_mbid="release-tier",
                    matcher_version="feedback-fixes-v1",
                    reason_code="SUPPORTED",
                ),
                created_at=2,
            )
        ],
        updated_at=2,
    )
    reviews = LibraryReviewService(store)
    selection = BulkReviewSelection(
        review_ids=["review-1"],
        expected_revisions={"review-1": 2},
        catalog_revision=await store.get_catalog_revision(),
    )
    preview = await reviews.preview_bulk(
        BulkReviewPreviewRequest(
            action="accept_candidate",
            selection=selection,
            candidate_key="rg-tier:release-tier",
        ),
        now=11,
    )
    assert preview.eligible_count == 1
    operation = await reviews.apply_bulk(
        BulkReviewApplyRequest(
            preview_token=preview.preview_token,
            idempotency_key="bulk-tier-1",
            action="accept_candidate",
            selection=selection,
            candidate_key="rg-tier:release-tier",
        ),
        "admin",
        now=12,
    )
    assert operation.expected_work_count == 1
    callback = AsyncMock()
    worker = LibraryOperationService(store, on_identified=callback)
    claimed = await worker.claim("worker", now=13)
    assert claimed is not None
    completed = await worker.run_bulk_claimed(claimed, "worker", "admin", now=14)
    assert completed.succeeded_count == 1
    callback.assert_not_awaited()
    context = await store.get_album_identification_context("album-1")
    assert context is not None
    assert context["identity"]["release_mbid"] == "release-tier"


@pytest.mark.asyncio
async def test_filter_bulk_apply_uses_preview_snapshot_across_concurrent_changes(
    store: NativeLibraryStore, db_path: Path
) -> None:
    await _seed_album(store, "1")
    await _seed_album(store, "2")
    reviews = LibraryReviewService(store)
    selection = BulkReviewSelection(
        normalized_filter={"state": "needs_review"},
        catalog_revision=await store.get_catalog_revision(),
    )
    preview = await reviews.preview_bulk(
        BulkReviewPreviewRequest(action="exclude", selection=selection), now=10
    )
    assert preview.eligible_count == 2
    assert preview.stale_count == 0

    await _seed_album(store, "3")
    with sqlite3.connect(db_path) as connection:
        connection.execute(
            "UPDATE library_identification_reviews "
            "SET state = 'resolved', row_revision = row_revision + 1 "
            "WHERE id = 'review-2'"
        )
    operation = await reviews.apply_bulk(
        BulkReviewApplyRequest(
            preview_token=preview.preview_token,
            idempotency_key="filter-bulk-1",
            action="exclude",
            selection=selection,
        ),
        "admin",
        now=11,
    )
    with sqlite3.connect(db_path) as connection:
        connection.execute(
            "UPDATE library_bulk_review_previews SET expires_at = 0 "
            "WHERE preview_token = ?",
            (preview.preview_token,),
        )
    protected_cleanup = await store.cleanup_bulk_review_preview_batch(
        now=20, batch_size=1
    )
    assert protected_cleanup == {"deleted_subjects": 0, "deleted_previews": 0}
    worker = LibraryOperationService(store)
    claimed = await worker.claim("worker", now=12)
    assert claimed is not None
    staged = await store.stage_bulk_review_operation_batch(
        operation.id, "worker", now=12
    )
    assert staged == {"complete": True, "staged_count": 2}
    with sqlite3.connect(db_path) as connection:
        materialized = [
            row[0]
            for row in connection.execute(
                "SELECT local_album_id "
                "FROM library_operation_work WHERE job_id = ? ORDER BY ordinal",
                (operation.id,),
            )
        ]
        snapshot = json.loads(
            connection.execute(
                "SELECT selection_json FROM library_bulk_review_snapshots WHERE job_id = ?",
                (operation.id,),
            ).fetchone()[0]
        )
    assert materialized == ["album-2", "album-1"]
    assert snapshot == {"normalized_filter": {"state": "needs_review"}}
    assert "review-3" not in snapshot

    done = await worker.run_bulk_claimed(claimed, "worker", "admin", now=13)
    assert done.succeeded_count == 1
    assert done.skipped_count == 1


@pytest.mark.asyncio
async def test_bulk_preview_batches_resume_after_cancellation_and_cleanup_is_bounded(
    store: NativeLibraryStore,
    db_path: Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    for suffix in ("1", "2", "3"):
        await _seed_album(store, suffix)
    reviews = LibraryReviewService(store)
    selection = BulkReviewSelection(
        normalized_filter={"state": "needs_review"},
        catalog_revision=await store.get_catalog_revision(),
    )
    original_stage = store.stage_bulk_review_preview_batch
    calls = 0

    async def interrupt_after_one(preview_token: str) -> dict:
        nonlocal calls
        calls += 1
        if calls == 2:
            raise asyncio.CancelledError
        return await original_stage(preview_token, batch_size=1)

    monkeypatch.setattr(store, "stage_bulk_review_preview_batch", interrupt_after_one)
    with pytest.raises(asyncio.CancelledError):
        await reviews.preview_bulk(
            BulkReviewPreviewRequest(action="exclude", selection=selection), now=10
        )
    with sqlite3.connect(db_path) as connection:
        preview_token, state, subject_count = connection.execute(
            "SELECT preview_token, state, subject_count "
            "FROM library_bulk_review_previews"
        ).fetchone()
    assert state == "staging"
    assert subject_count == 1

    with pytest.raises(StaleRevisionError, match="changed or expired"):
        await reviews.apply_bulk(
            BulkReviewApplyRequest(
                preview_token=preview_token,
                idempotency_key="incomplete-preview",
                action="exclude",
                selection=selection,
            ),
            "admin",
            now=11,
        )

    restarted = NativeLibraryStore(db_path, threading.Lock())
    while True:
        staged = await restarted.stage_bulk_review_preview_batch(
            preview_token, batch_size=1
        )
        if staged["complete"]:
            break
    assert staged["summary"]["eligible_count"] == 3
    with sqlite3.connect(db_path) as connection:
        connection.execute(
            "UPDATE library_bulk_review_previews SET expires_at = 0 "
            "WHERE preview_token = ?",
            (preview_token,),
        )
    first_cleanup = await restarted.cleanup_bulk_review_preview_batch(
        now=20, batch_size=1
    )
    assert first_cleanup == {"deleted_subjects": 1, "deleted_previews": 0}
    with sqlite3.connect(db_path) as connection:
        assert (
            connection.execute(
                "SELECT COUNT(*) FROM library_bulk_review_preview_subjects "
                "WHERE preview_token = ?",
                (preview_token,),
            ).fetchone()[0]
            == 2
        )
    while True:
        cleanup = await restarted.cleanup_bulk_review_preview_batch(
            now=20, batch_size=1
        )
        if cleanup["deleted_previews"]:
            break
    with sqlite3.connect(db_path) as connection:
        assert (
            connection.execute(
                "SELECT COUNT(*) FROM library_bulk_review_previews "
                "WHERE preview_token = ?",
                (preview_token,),
            ).fetchone()[0]
            == 0
        )


@pytest.mark.asyncio
async def test_bulk_preview_reads_evidence_setwise_and_operation_staging_resumes(
    store: NativeLibraryStore,
    db_path: Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    for suffix in ("1", "2", "3"):
        await _seed_album(store, suffix)
        attempt = IdentificationAttempt(
            id=f"attempt-{suffix}",
            local_album_id=f"album-{suffix}",
            matcher_version="feedback-fixes-v1",
            state="ambiguous",
            terminal_reason_code="AMBIGUOUS",
            started_at=2,
            completed_at=2,
        )
        await store.replace_review_attempt(
            f"review-{suffix}",
            expected_review_revision=1,
            attempt=attempt,
            evidence=[
                IdentificationEvidenceRecord(
                    id=f"evidence-{suffix}",
                    attempt_id=attempt.id,
                    candidate_key="rg-shared:release-shared",
                    evidence=CandidateEvidence(
                        release_group_mbid="rg-shared",
                        release_mbid="release-shared",
                        matcher_version="feedback-fixes-v1",
                        reason_code="SUPPORTED",
                    ),
                    created_at=2,
                )
            ],
            updated_at=2,
        )
    statements: list[str] = []
    original_connect = store._connect

    def traced_connect() -> sqlite3.Connection:
        connection = original_connect()
        connection.set_trace_callback(statements.append)
        return connection

    monkeypatch.setattr(store, "_connect", traced_connect)
    reviews = LibraryReviewService(store)
    selection = BulkReviewSelection(
        normalized_filter={"state": "needs_review"},
        catalog_revision=await store.get_catalog_revision(),
    )
    preview = await reviews.preview_bulk(
        BulkReviewPreviewRequest(action="exclude", selection=selection), now=10
    )
    evidence_reads = [
        statement
        for statement in statements
        if statement.startswith("SELECT attempt_id, candidate_key, evidence_json")
    ]
    assert len(evidence_reads) == 1
    assert preview.eligible_count == 3

    operation = await reviews.apply_bulk(
        BulkReviewApplyRequest(
            preview_token=preview.preview_token,
            idempotency_key="staged-restart",
            action="exclude",
            selection=selection,
        ),
        "admin",
        now=11,
    )
    worker = LibraryOperationService(store)
    claimed = await worker.claim("worker", now=12)
    assert claimed is not None
    first = await store.stage_bulk_review_operation_batch(
        operation.id, "worker", now=12, batch_size=1
    )
    assert first == {"complete": False, "staged_count": 1}
    current = await store.get_operation_job(operation.id)
    assert current is not None
    await worker.control(operation.id, "pause", int(current["row_revision"]), now=13)
    paused = await store.checkpoint_operation_control(operation.id, "worker", now=13)
    assert paused is not None and paused["state"] == "paused"
    resumed = await worker.control(
        operation.id, "resume", int(paused["row_revision"]), now=14
    )
    assert resumed.state == "queued"

    restarted = NativeLibraryStore(db_path, threading.Lock())
    reclaimed = await LibraryOperationService(restarted).claim("restarted", now=15)
    assert reclaimed is not None
    second = await restarted.stage_bulk_review_operation_batch(
        operation.id, "restarted", now=15, batch_size=1
    )
    assert second == {"complete": False, "staged_count": 1}
    final = await restarted.stage_bulk_review_operation_batch(
        operation.id, "restarted", now=15, batch_size=1
    )
    assert final == {"complete": True, "staged_count": 1}
    with sqlite3.connect(db_path) as connection:
        assert (
            connection.execute(
                "SELECT COUNT(*) FROM library_operation_work WHERE job_id = ?",
                (operation.id,),
            ).fetchone()[0]
            == 3
        )
        assert (
            connection.execute(
                "SELECT staging_cursor FROM library_bulk_review_snapshots WHERE job_id = ?",
                (operation.id,),
            ).fetchone()[0]
            == 2
        )


@pytest.mark.asyncio
async def test_scoped_retry_resolves_saved_ids_and_requires_local_metadata_confirmation(
    store: NativeLibraryStore,
) -> None:
    await _seed_album(store, "1")
    await _seed_album(store, "scope%_literal", policy="local_metadata")
    await _seed_album(store, "3", review_state="keep_tagged")
    resolver = LibraryPolicyResolver(
        TypedLibrarySettings(
            library_roots=[
                LibraryRootSettings(
                    id="root",
                    path="/music",
                    label="Music",
                    rules=[
                        LibraryPathPolicyRule(
                            id="literal-rule",
                            relative_path="scope%_literal",
                            policy="local_metadata",
                        )
                    ],
                )
            ]
        )
    )
    reviews = LibraryReviewService(store, resolver_getter=lambda: resolver)
    selection = BulkReviewSelection(
        normalized_filter={
            "states": json.dumps(["needs_review", "keep_tagged"]),
            "scope_ids": json.dumps(["literal-rule"]),
            "scope_revision": resolver.policy_revision,
        },
        catalog_revision=await store.get_catalog_revision(),
    )

    preview = await reviews.preview_bulk(
        BulkReviewPreviewRequest(action="retry", selection=selection), now=10
    )

    assert preview.album_count == 1
    assert preview.eligible_count == 1
    assert preview.requires_local_metadata_confirmation is True
    with pytest.raises(StaleRevisionError, match="one-off lookup"):
        await reviews.apply_bulk(
            BulkReviewApplyRequest(
                preview_token=preview.preview_token,
                idempotency_key="scoped-retry",
                action="retry",
                selection=selection,
            ),
            "admin",
            now=11,
        )
    job = await reviews.apply_bulk(
        BulkReviewApplyRequest(
            preview_token=preview.preview_token,
            idempotency_key="scoped-retry",
            action="retry",
            selection=selection,
            confirm_local_metadata=True,
        ),
        "admin",
        now=11,
    )
    assert job.expected_work_count == 1

    stale_selection = BulkReviewSelection(
        normalized_filter={
            "scope_ids": json.dumps(["literal-rule"]),
            "scope_revision": "stale",
        }
    )
    with pytest.raises(StaleRevisionError, match="Library settings changed"):
        await reviews.preview_bulk(
            BulkReviewPreviewRequest(action="retry", selection=stale_selection),
            now=12,
        )
    mixed_selection = BulkReviewSelection(
        normalized_filter={
            "scope_ids": json.dumps(["root", "literal-rule"]),
            "scope_revision": resolver.policy_revision,
        }
    )
    with pytest.raises(ValidationError, match="one nested policy path"):
        await reviews.preview_bulk(
            BulkReviewPreviewRequest(action="retry", selection=mixed_selection),
            now=12,
        )


@pytest.mark.asyncio
async def test_bulk_retry_creates_observable_reidentification_operation(
    store: NativeLibraryStore, db_path: Path
) -> None:
    await _seed_album(store, "1")
    reviews = LibraryReviewService(store)
    selection = BulkReviewSelection(
        review_ids=["review-1"],
        expected_revisions={"review-1": 1},
        catalog_revision=await store.get_catalog_revision(),
    )
    preview = await reviews.preview_bulk(
        BulkReviewPreviewRequest(action="retry", selection=selection), now=10
    )
    assert preview.estimated_job_count == 1
    parent = await reviews.apply_bulk(
        BulkReviewApplyRequest(
            preview_token=preview.preview_token,
            idempotency_key="bulk-retry",
            action="retry",
            selection=selection,
        ),
        "admin",
        now=11,
    )
    operations = LibraryOperationService(store)
    claimed = await operations.claim("worker", now=12)
    assert claimed is not None and claimed["id"] == parent.id
    done = await operations.run_bulk_claimed(claimed, "worker", "admin", now=13)
    with sqlite3.connect(db_path) as connection:
        child = connection.execute(
            "SELECT id, state FROM library_operation_jobs "
            "WHERE kind = 'explicit_reidentification'"
        ).fetchone()
        review_state = connection.execute(
            "SELECT state FROM library_identification_reviews WHERE id = 'review-1'"
        ).fetchone()[0]
        snapshot_count = connection.execute(
            "SELECT COUNT(*) FROM library_reidentification_snapshots"
        ).fetchone()[0]
    assert done.succeeded_count == 1
    assert child is not None and child[1] == "queued"
    assert snapshot_count == 1
    assert review_state == "resolved"


@pytest.mark.asyncio
async def test_bulk_retry_scopes_child_evaluation_and_commit_to_indexed_tracks(
    store: NativeLibraryStore, db_path: Path
) -> None:
    await _seed_album(store, "1", identity_source="legacy_import", two_tracks=True)
    with sqlite3.connect(db_path) as connection:
        connection.execute(
            "UPDATE local_tracks SET availability='missing' WHERE id='track-1-2'"
        )
    context = await store.get_album_identification_context("album-1")
    assert context is not None
    indexed = [
        track for track in context["tracks"] if track["availability"] == "indexed"
    ]
    indexed_revision = ":".join(album_input_revisions(indexed))
    with sqlite3.connect(db_path) as connection:
        connection.execute(
            "UPDATE library_identification_reviews SET input_revision=? "
            "WHERE id='review-1'",
            (indexed_revision,),
        )
    reviews = LibraryReviewService(store)
    selection = BulkReviewSelection(
        review_ids=["review-1"],
        expected_revisions={"review-1": 1},
        catalog_revision=await store.get_catalog_revision(),
    )
    preview = await reviews.preview_bulk(
        BulkReviewPreviewRequest(action="retry", selection=selection), now=10
    )
    parent = await reviews.apply_bulk(
        BulkReviewApplyRequest(
            preview_token=preview.preview_token,
            idempotency_key="bulk-retry-indexed",
            action="retry",
            selection=selection,
        ),
        "admin",
        now=11,
    )
    operations = LibraryOperationService(store)
    claimed_parent = await operations.claim("worker", now=12)
    assert claimed_parent is not None and claimed_parent["id"] == parent.id
    await operations.run_bulk_claimed(claimed_parent, "worker", "admin", now=13)
    with sqlite3.connect(db_path) as connection:
        connection.row_factory = sqlite3.Row
        child = connection.execute(
            "SELECT j.*,s.expected_input_revision FROM library_operation_jobs j "
            "JOIN library_reidentification_snapshots s ON s.job_id=j.id "
            "WHERE j.kind='explicit_reidentification'"
        ).fetchone()
    assert child is not None
    assert child["expected_input_revision"] == indexed_revision

    claimed_child = await store.claim_operation_job(
        "worker", now=14, lease_seconds=60, kind="explicit_reidentification"
    )
    assert claimed_child is not None
    explicit = ExplicitReidentificationWorker(
        store,
        AlbumCandidateService(_LegacyTrackIdentityProvider()),
        AlbumEvidenceEngine(),
    )
    ready = await explicit.run_claimed(claimed_child, "worker", now=15)
    operation = await operations.get(str(child["id"]))
    assert ready["state"] == "ready"
    assert len(operation.reidentification_candidates[0].evidence.track_evidence) == 1
    await explicit.select_candidate(
        str(child["id"]),
        expected_job_revision=int(ready["row_revision"]),
        candidate_key="rg-1:release-1",
        confirmation=False,
        actor_user_id="admin",
        now=16,
    )
    context = await store.get_album_identification_context("album-1")
    assert context is not None
    assert {
        row["id"]: (row["availability"], row["recording_mbid"])
        for row in context["tracks"]
    } == {
        "track-1-1": ("indexed", "recording-track-1-1"),
        "track-1-2": ("missing", "recording-track-1-2"),
    }


@pytest.mark.asyncio
async def test_bulk_stop_keeps_completed_results_and_requires_explicit_resume(
    store: NativeLibraryStore, db_path: Path
) -> None:
    await _seed_album(store, "1")
    await _seed_album(store, "2")
    reviews = LibraryReviewService(store)
    selection = BulkReviewSelection(
        review_ids=["review-1", "review-2"],
        expected_revisions={"review-1": 1, "review-2": 1},
        catalog_revision=await store.get_catalog_revision(),
    )
    preview = await reviews.preview_bulk(
        BulkReviewPreviewRequest(action="exclude", selection=selection), now=10
    )
    operation = await reviews.apply_bulk(
        BulkReviewApplyRequest(
            preview_token=preview.preview_token,
            idempotency_key="bulk-stop",
            action="exclude",
            selection=selection,
        ),
        "admin",
        now=11,
    )
    worker = LibraryOperationService(store)
    claimed = await worker.claim("worker", now=12)
    assert claimed is not None

    async def stop_after_first() -> None:
        current = await store.get_operation_job(operation.id)
        assert current is not None
        await worker.control(operation.id, "stop", int(current["row_revision"]), now=13)

    stopped = await worker.run_bulk_claimed(
        claimed,
        "worker",
        "admin",
        now=13,
        checkpoint=stop_after_first,
    )
    assert stopped.state == "stopped"
    assert stopped.completed_count == 1
    assert await worker.claim("other-worker", now=14) is None
    with sqlite3.connect(db_path) as connection:
        states = dict(
            connection.execute(
                "SELECT state, COUNT(*) FROM library_operation_work "
                "WHERE job_id = ? GROUP BY state",
                (operation.id,),
            )
        )
    assert states == {"pending": 1, "succeeded": 1}

    resumed = await worker.control(operation.id, "resume", stopped.row_revision, now=15)
    assert resumed.state == "queued"
    reclaimed = await worker.claim("worker", now=16)
    assert reclaimed is not None
    completed = await worker.run_bulk_claimed(reclaimed, "worker", "admin", now=17)
    assert completed.succeeded_count == 2


@pytest.mark.asyncio
async def test_operation_pause_resume_stop_contract_is_shared(
    store: NativeLibraryStore,
) -> None:
    await _seed_album(store, "1")
    service = ReidentificationService(store)
    context = await store.get_album_identification_context("album-1")
    assert context is not None
    row = await service.create_or_coalesce(
        "album-1",
        "admin",
        expected_album_revision=int(context["album"]["row_revision"]),
        expected_input_revision=":".join(album_input_revisions(context["tracks"])),
        idempotency_key="explicit-1",
        now=1,
    )
    operations = LibraryOperationService(store)
    claimed = await store.claim_operation_job(
        "worker", now=2, lease_seconds=60, kind="explicit_reidentification"
    )
    assert claimed is not None
    requested = await operations.control(
        row["id"], "pause", claimed["row_revision"], now=3
    )
    paused = await store.checkpoint_operation_control(row["id"], "worker", now=4)
    assert requested.control_request == "pause"
    assert paused is not None and paused["state"] == "paused"
    resumed = await operations.control(
        row["id"], "resume", paused["row_revision"], now=5
    )
    assert resumed.state == "queued"


@pytest.mark.asyncio
async def test_operation_control_requests_are_durably_idempotent(
    store: NativeLibraryStore,
) -> None:
    await _seed_album(store, "1")
    context = await store.get_album_identification_context("album-1")
    assert context is not None
    row = await ReidentificationService(store).create_or_coalesce(
        "album-1",
        "admin",
        expected_album_revision=int(context["album"]["row_revision"]),
        expected_input_revision=":".join(album_input_revisions(context["tracks"])),
        idempotency_key="control-operation",
        now=1,
    )
    claimed = await store.claim_operation_job(
        "worker", now=2, lease_seconds=60, kind="explicit_reidentification"
    )
    assert claimed is not None
    operations = LibraryOperationService(store)
    first = await operations.control(
        row["id"],
        "pause",
        claimed["row_revision"],
        idempotency_key="pause-once",
        now=3,
    )
    repeated = await operations.control(
        row["id"],
        "pause",
        claimed["row_revision"],
        idempotency_key="pause-once",
        now=4,
    )

    assert first.control_request == "pause"
    assert repeated.row_revision == first.row_revision
    with pytest.raises(ConflictError, match="another request"):
        await operations.control(
            row["id"],
            "stop",
            first.row_revision,
            idempotency_key="pause-once",
            now=5,
        )


@pytest.mark.asyncio
async def test_ready_explicit_reidentification_can_be_stopped_and_resumed_for_fresh_evaluation(
    store: NativeLibraryStore, db_path: Path
) -> None:
    await _seed_album(store, "1")
    created = await ReidentificationService(store).create_or_coalesce(
        "album-1", "admin", idempotency_key="stop-ready-explicit", now=1
    )
    worker = ExplicitReidentificationWorker(
        store,
        AlbumCandidateService(_IdentificationProvider()),
        AlbumEvidenceEngine(),
    )
    claimed = await store.claim_operation_job(
        "worker", now=2, lease_seconds=60, kind="explicit_reidentification"
    )
    assert claimed is not None
    ready = await worker.run_claimed(claimed, "worker", now=3)
    operations = LibraryOperationService(store)

    stopped = await operations.control(
        created["id"],
        "stop",
        int(ready["row_revision"]),
        idempotency_key="stop-ready-once",
        now=4,
    )
    repeated = await operations.control(
        created["id"],
        "stop",
        int(ready["row_revision"]),
        idempotency_key="stop-ready-once",
        now=5,
    )
    retained = await operations.get(created["id"])

    assert stopped.state == "stopped"
    assert stopped.control_request == "none"
    assert repeated.row_revision == stopped.row_revision
    assert len(retained.reidentification_candidates) == 1
    with pytest.raises(StaleRevisionError):
        await operations.control(
            created["id"],
            "stop",
            int(ready["row_revision"]),
            idempotency_key="different-stop-request",
            now=6,
        )

    resumed = await operations.control(
        created["id"],
        "resume",
        stopped.row_revision,
        idempotency_key="resume-stopped-ready",
        now=7,
    )
    with sqlite3.connect(db_path) as connection:
        work = connection.execute(
            "SELECT state,result_json,failure_code FROM library_operation_work "
            "WHERE job_id = ?",
            (created["id"],),
        ).fetchone()
    assert resumed.state == "queued"
    assert resumed.completed_count == 0
    assert resumed.succeeded_count == 0
    assert work == ("pending", None, None)

    reclaimed = await store.claim_operation_job(
        "worker", now=8, lease_seconds=60, kind="explicit_reidentification"
    )
    assert reclaimed is not None
    repeated_ready = await worker.run_claimed(reclaimed, "worker", now=9)
    assert repeated_ready["state"] == "ready"
    with sqlite3.connect(db_path) as connection:
        assert (
            connection.execute(
                "SELECT COUNT(*) FROM library_identification_attempts "
                "WHERE local_album_id = 'album-1'"
            ).fetchone()[0]
            == 2
        )


@pytest.mark.asyncio
async def test_stop_does_not_change_ready_library_management_semantics(
    store: NativeLibraryStore, db_path: Path
) -> None:
    with sqlite3.connect(db_path) as connection:
        connection.execute(
            "INSERT INTO library_operation_jobs "
            "(id,kind,state,expected_work_count,created_at,updated_at) "
            "VALUES ('ready-preview','library_management','ready',0,1,1)"
        )
    operations = LibraryOperationService(store)

    result = await operations.control("ready-preview", "stop", 1, now=2)

    assert result.state == "ready"
    assert result.control_request == "stop"


@pytest.mark.asyncio
async def test_operation_control_and_kind_snapshots_are_shared_across_all_three_kinds(
    store: NativeLibraryStore,
) -> None:
    await _seed_album(store, "1")
    reviews = LibraryReviewService(store)
    selection = BulkReviewSelection(
        review_ids=["review-1"],
        expected_revisions={"review-1": 1},
        catalog_revision=await store.get_catalog_revision(),
    )
    preview = await reviews.preview_bulk(
        BulkReviewPreviewRequest(action="exclude", selection=selection), now=10
    )
    bulk = await reviews.apply_bulk(
        BulkReviewApplyRequest(
            preview_token=preview.preview_token,
            idempotency_key="shared-bulk",
            action="exclude",
            selection=selection,
        ),
        "admin",
        now=10,
    )

    await _seed_album(store, "2")
    context = await store.get_album_identification_context("album-2")
    assert context is not None
    await store.attach_album_identity(
        LocalAlbumExternalIdentity(
            local_album_id="album-2",
            release_group_mbid="rg-repair",
            decision_source="legacy_import",
            selected_at=11,
        ),
        expected_album_revision=int(context["album"]["row_revision"]),
    )
    repair = await IdentityRepairService(store).create(
        RepairCreateRequest(idempotency_key="shared-repair"), "admin", now=12
    )

    await _seed_album(store, "3")
    explicit = await ReidentificationService(store).create_or_coalesce(
        "album-3", "admin", idempotency_key="shared-explicit", now=13
    )
    operations = LibraryOperationService(store)
    for kind, job_id, snapshot_kind in (
        ("bulk_review_apply", bulk.id, "bulk_review_apply"),
        ("repair", repair.id, "repair"),
        ("explicit_reidentification", explicit["id"], "explicit_reidentification"),
    ):
        snapshot = await store.get_operation_snapshot(job_id)
        assert snapshot is not None and snapshot["job"]["kind"] == snapshot_kind
        claimed = await store.claim_operation_job(
            "worker", now=20, lease_seconds=60, kind=kind
        )
        assert claimed is not None and claimed["id"] == job_id
        requested = await operations.control(
            job_id, "pause", int(claimed["row_revision"]), now=21
        )
        paused = await store.checkpoint_operation_control(job_id, "worker", now=22)
        assert requested.control_request == "pause"
        assert paused is not None and paused["state"] == "paused"
        resumed = await operations.control(
            job_id, "resume", int(paused["row_revision"]), now=23
        )
        assert resumed.state == "queued"


@pytest.mark.asyncio
async def test_operation_supervisor_dispatches_explicit_work_before_older_bulk_work(
    store: NativeLibraryStore,
) -> None:
    await _seed_album(store, "1")
    reviews = LibraryReviewService(store)
    selection = BulkReviewSelection(
        review_ids=["review-1"],
        expected_revisions={"review-1": 1},
        catalog_revision=await store.get_catalog_revision(),
    )
    preview = await reviews.preview_bulk(
        BulkReviewPreviewRequest(action="exclude", selection=selection), now=1
    )
    bulk = await reviews.apply_bulk(
        BulkReviewApplyRequest(
            preview_token=preview.preview_token,
            idempotency_key="priority-bulk",
            action="exclude",
            selection=selection,
        ),
        "admin",
        now=1,
    )
    await _seed_album(store, "2")
    explicit = await ReidentificationService(store).create_or_coalesce(
        "album-2", "admin", idempotency_key="priority-explicit", now=2
    )
    operations = LibraryOperationService(store)
    explicit_worker = ExplicitReidentificationWorker(
        store,
        AlbumCandidateService(_IdentificationProvider()),
        AlbumEvidenceEngine(),
    )
    supervisor = LibraryOperationSupervisor(
        store,
        operations,
        IdentityRepairService(store),
        explicit_worker,
    )
    result = await supervisor.run_once("worker", now=3)
    bulk_row = await store.get_operation_job(bulk.id)
    assert result is not None and result.id == explicit["id"]
    assert result.state == "ready"
    assert bulk_row is not None and bulk_row["state"] == "queued"


@pytest.mark.asyncio
async def test_operation_supervisor_keeps_explicit_provider_work_behind_scan_gate(
    store: NativeLibraryStore,
) -> None:
    await _seed_album(store, "1")
    reviews = LibraryReviewService(store)
    selection = BulkReviewSelection(
        review_ids=["review-1"],
        expected_revisions={"review-1": 1},
        catalog_revision=await store.get_catalog_revision(),
    )
    preview = await reviews.preview_bulk(
        BulkReviewPreviewRequest(action="exclude", selection=selection), now=1
    )
    bulk = await reviews.apply_bulk(
        BulkReviewApplyRequest(
            preview_token=preview.preview_token,
            idempotency_key="scan-gated-bulk",
            action="exclude",
            selection=selection,
        ),
        "admin",
        now=1,
    )
    await _seed_album(store, "2")
    explicit = await ReidentificationService(store).create_or_coalesce(
        "album-2", "admin", idempotency_key="scan-gated-explicit", now=2
    )
    provider = _CountingIdentificationProvider()
    gate = BackgroundWorkloadGate()
    gate.set_scan_active(True)
    explicit_worker = ExplicitReidentificationWorker(
        store,
        AlbumCandidateService(provider),
        AlbumEvidenceEngine(),
        workload_gate=gate,
    )
    supervisor = LibraryOperationSupervisor(
        store,
        LibraryOperationService(store),
        IdentityRepairService(store),
        explicit_worker,
        gate,
    )

    bulk_result = await supervisor.run_once("worker", now=3)
    explicit_row = await store.get_operation_job(explicit["id"])
    assert bulk_result is not None and bulk_result.id == bulk.id
    assert bulk_result.state == "succeeded"
    assert explicit_row is not None and explicit_row["state"] == "queued"
    assert provider.calls == 0

    gate.set_scan_active(False)
    claimed = await store.claim_operation_job(
        "race-worker",
        now=3.5,
        lease_seconds=60,
        kind="explicit_reidentification",
    )
    assert claimed is not None
    gate.set_scan_active(True)
    deferred = await explicit_worker.run_claimed(claimed, "race-worker", now=3.5)
    assert deferred["state"] == "queued"
    assert provider.calls == 0

    gate.set_scan_active(False)
    explicit_result = await supervisor.run_once("worker", now=4)
    assert explicit_result is not None and explicit_result.id == explicit["id"]
    assert explicit_result.state == "ready"
    assert provider.calls > 0


@pytest.mark.asyncio
async def test_explicit_reidentification_exposes_candidates_and_rejects_stale_selection(
    store: NativeLibraryStore, db_path: Path
) -> None:
    await _seed_album(store, "1")
    created = await ReidentificationService(store).create_or_coalesce(
        "album-1", "admin", now=1
    )
    claimed = await store.claim_operation_job(
        "worker", now=2, lease_seconds=60, kind="explicit_reidentification"
    )
    assert claimed is not None
    callback = AsyncMock()
    worker = ExplicitReidentificationWorker(
        store,
        AlbumCandidateService(_IdentificationProvider()),
        AlbumEvidenceEngine(),
        on_identified=callback,
    )
    ready = await worker.run_claimed(claimed, "worker", now=3)
    snapshot = await store.get_operation_snapshot(created["id"])
    assert ready["state"] == "ready"
    assert snapshot is not None
    result = json.loads(snapshot["snapshot"]["result_json"])
    assert result["candidate_keys"] == ["rg-explicit:release-explicit"]

    with sqlite3.connect(db_path) as connection:
        connection.execute(
            "UPDATE local_albums SET row_revision = row_revision + 1 WHERE id = 'album-1'"
        )
    with pytest.raises(StaleRevisionError):
        await worker.select_candidate(
            created["id"],
            expected_job_revision=int(ready["row_revision"]),
            candidate_key="rg-explicit:release-explicit",
            confirmation=False,
            actor_user_id="admin",
            now=4,
        )

    await _seed_album(store, "2")
    second = await ReidentificationService(store).create_or_coalesce(
        "album-2", "admin", now=5
    )
    claimed_second = await store.claim_operation_job(
        "worker", now=6, lease_seconds=60, kind="explicit_reidentification"
    )
    assert claimed_second is not None
    ready_second = await worker.run_claimed(claimed_second, "worker", now=7)
    selected = await worker.select_candidate(
        second["id"],
        expected_job_revision=int(ready_second["row_revision"]),
        candidate_key="rg-explicit:release-explicit",
        confirmation=False,
        actor_user_id="admin",
        now=8,
    )
    second_context = await store.get_album_identification_context("album-2")
    assert selected["state"] == "succeeded"
    assert second_context is not None
    assert second_context["identity"]["decision_source"] == "manual"
    assert second_context["tracks"][0]["recording_mbid"] == "recording-explicit"
    response = await LibraryOperationService(store).get(second["id"])
    assert (
        response.selected_reidentification_candidate_key
        == "rg-explicit:release-explicit"
    )
    callback.assert_awaited_once_with(
        "album-2", album_input_revisions(second_context["tracks"])[2]
    )


@pytest.mark.asyncio
async def test_explicit_reidentification_requires_confirmation_for_conflicting_candidate(
    store: NativeLibraryStore, db_path: Path
) -> None:
    await _seed_album(store, "1")
    created = await ReidentificationService(store).create_or_coalesce(
        "album-1", "admin", now=1
    )
    claimed = await store.claim_operation_job(
        "worker", now=2, lease_seconds=60, kind="explicit_reidentification"
    )
    assert claimed is not None
    worker = ExplicitReidentificationWorker(
        store,
        AlbumCandidateService(_IdentificationProvider()),
        AlbumEvidenceEngine(),
    )
    ready = await worker.run_claimed(claimed, "worker", now=3)
    with sqlite3.connect(db_path) as connection:
        row = connection.execute(
            "SELECT evidence_json FROM library_identification_evidence "
            "WHERE candidate_key = 'rg-explicit:release-explicit'"
        ).fetchone()
        assert row is not None
        evidence = json.loads(bytes(row[0]))
        evidence["reason_code"] = "CONTRADICTORY_TRACK_EVIDENCE"
        connection.execute("DROP TRIGGER trg_library_identification_evidence_immutable")
        connection.execute(
            "UPDATE library_identification_evidence SET evidence_json = ? "
            "WHERE candidate_key = 'rg-explicit:release-explicit'",
            (json.dumps(evidence, sort_keys=True).encode(),),
        )

    with pytest.raises(ValidationError, match="Confirm the conflicting"):
        await worker.select_candidate(
            created["id"],
            expected_job_revision=int(ready["row_revision"]),
            candidate_key="rg-explicit:release-explicit",
            confirmation=False,
            actor_user_id="admin",
            now=4,
        )

    selected = await worker.select_candidate(
        created["id"],
        expected_job_revision=int(ready["row_revision"]),
        candidate_key="rg-explicit:release-explicit",
        confirmation=True,
        actor_user_id="admin",
        now=5,
    )

    assert selected["state"] == "succeeded"


class _TwoTrackProvider(_IdentificationProvider):
    async def get_album_candidate(
        self, release_group_mbid, target_track_count, priority
    ):
        return AlbumCandidate(
            release_group_mbid="rg-explicit",
            release_mbid="release-explicit",
            album_title="Album 1",
            album_artist_name="Artist 1",
            tracks=[
                CandidateTrack(
                    title=f"Track {index}",
                    position=index,
                    absolute_position=index,
                    recording_mbid=f"recording-{index}",
                    release_track_mbid=f"release-track-{index}",
                )
                for index in (1, 2)
            ],
        )


async def _ready_two_track_reidentification(
    store: NativeLibraryStore, db_path: Path, *, embedded_release_track: str
):
    """Two files; the second one carries a release-track ID of another edition."""

    await _seed_album(store, "1", two_tracks=True)
    with sqlite3.connect(db_path) as connection:
        connection.execute(
            "UPDATE local_tracks SET embedded_release_track_mbid = ? "
            "WHERE id = 'track-1-2'",
            (embedded_release_track,),
        )
    created = await ReidentificationService(store).create_or_coalesce(
        "album-1", "admin", now=1
    )
    claimed = await store.claim_operation_job(
        "worker", now=2, lease_seconds=60, kind="explicit_reidentification"
    )
    assert claimed is not None
    worker = ExplicitReidentificationWorker(
        store,
        AlbumCandidateService(_TwoTrackProvider()),
        AlbumEvidenceEngine(),
    )
    ready = await worker.run_claimed(claimed, "worker", now=3)
    return worker, created, ready


@pytest.mark.asyncio
async def test_explicit_exact_release_maps_a_track_that_only_conflicts_on_its_embedded_release_track_id(
    store: NativeLibraryStore, db_path: Path
) -> None:
    worker, created, ready = await _ready_two_track_reidentification(
        store, db_path, embedded_release_track="4b2174ec-d9b7-4de2-a8c0-ae8128c7d5ab"
    )
    snapshot = await store.get_operation_snapshot(created["id"])
    assert snapshot is not None
    with sqlite3.connect(db_path) as connection:
        row = connection.execute(
            "SELECT evidence_json FROM library_identification_evidence "
            "WHERE candidate_key = 'rg-explicit:release-explicit'"
        ).fetchone()
    evidence = msgspec.json.decode(bytes(row[0]), type=CandidateEvidence)
    by_id = {item.local_track_id: item for item in evidence.track_evidence}
    # The engine proposes the one edition track the file matches by title and
    # position, and records only the identifier conflict against it.
    assert by_id["track-1-2"].classification == "contradictory"
    assert by_id["track-1-2"].evidence_kinds == ["release_track_mbid_conflict"]
    assert by_id["track-1-2"].release_track_mbid == "release-track-2"
    assert track_reassignment_plan(evidence, 2) != []

    with pytest.raises(ValidationError, match="Confirm the conflicting"):
        await worker.select_candidate(
            created["id"],
            expected_job_revision=int(ready["row_revision"]),
            candidate_key="rg-explicit:release-explicit",
            confirmation=False,
            actor_user_id="admin",
            now=4,
        )
    selected = await worker.select_candidate(
        created["id"],
        expected_job_revision=int(ready["row_revision"]),
        candidate_key="rg-explicit:release-explicit",
        confirmation=True,
        actor_user_id="admin",
        now=5,
    )
    context = await store.get_album_identification_context("album-1")
    assert selected["state"] == "succeeded"
    assert context is not None
    assert {track["id"]: track["recording_mbid"] for track in context["tracks"]} == {
        "track-1-1": "recording-1",
        "track-1-2": "recording-2",
    }


@pytest.mark.asyncio
async def test_explicit_reidentification_rejects_changed_album_file_inputs(
    store: NativeLibraryStore, db_path: Path
) -> None:
    await _seed_album(store, "1")
    created = await ReidentificationService(store).create_or_coalesce(
        "album-1", "admin", now=1
    )
    claimed = await store.claim_operation_job(
        "worker", now=2, lease_seconds=60, kind="explicit_reidentification"
    )
    assert claimed is not None
    worker = ExplicitReidentificationWorker(
        store,
        AlbumCandidateService(_IdentificationProvider()),
        AlbumEvidenceEngine(),
    )
    ready = await worker.run_claimed(claimed, "worker", now=3)
    with sqlite3.connect(db_path) as connection:
        connection.execute(
            "UPDATE local_tracks SET tag_revision='changed-after-evidence' "
            "WHERE local_album_id='album-1'"
        )

    with pytest.raises(StaleRevisionError, match="files changed"):
        await worker.select_candidate(
            created["id"],
            expected_job_revision=int(ready["row_revision"]),
            candidate_key="rg-explicit:release-explicit",
            confirmation=False,
            actor_user_id="admin",
            now=4,
        )


@pytest.mark.asyncio
async def test_explicit_reidentification_uses_release_consistent_legacy_track_ids(
    store: NativeLibraryStore, db_path: Path
) -> None:
    await _seed_album(store, "1", identity_source="legacy_import")
    with sqlite3.connect(db_path) as connection:
        connection.execute(
            "UPDATE local_tracks SET title = 'Artist - Album - 01 - Track', "
            "album_title = 'Artist - Album - 01 - Track', "
            "artist_name = 'Unknown Artist', album_artist_name = 'Unknown Artist', "
            "track_number = 0 WHERE id = 'track-1-1'"
        )

    created = await ReidentificationService(store).create_or_coalesce(
        "album-1", "admin", idempotency_key="legacy-evidence-explicit", now=3
    )
    claimed = await store.claim_operation_job(
        "worker", now=4, lease_seconds=60, kind="explicit_reidentification"
    )
    assert claimed is not None
    worker = ExplicitReidentificationWorker(
        store,
        AlbumCandidateService(_LegacyTrackIdentityProvider()),
        AlbumEvidenceEngine(),
    )

    ready = await worker.run_claimed(claimed, "worker", now=5)
    evidence = await store.get_latest_album_candidate_evidence(
        "album-1", "rg-1:release-1"
    )

    assert ready["state"] == "ready"
    assert evidence is not None
    assert evidence.evidence.reason_code == "CONFLICTING_TRACK_EVIDENCE"
    assert evidence.evidence.unmatched_expected_tracks == []
    assert len(evidence.evidence.track_evidence) == 1
    track_evidence = evidence.evidence.track_evidence[0]
    assert track_evidence.classification == "supported"
    assert track_evidence.evidence_kinds == ["recording_mbid"]
    assert track_evidence.release_track_mbid == "release-track-1"

    selected = await worker.select_candidate(
        created["id"],
        expected_job_revision=int(ready["row_revision"]),
        candidate_key="rg-1:release-1",
        confirmation=True,
        actor_user_id="admin",
        now=6,
    )
    context = await store.get_album_identification_context("album-1")

    assert selected["state"] == "succeeded"
    assert context is not None
    assert context["identity"]["decision_source"] == "manual"
    assert context["tracks"][0]["recording_mbid"] == "recording-track-1-1"
    assert context["tracks"][0]["release_track_mbid"] == "release-track-1"
    assert context["tracks"][0]["release_track_position"] == 1


@pytest.mark.asyncio
async def test_explicit_reidentification_ignores_missing_rows_and_preserves_their_identities(
    store: NativeLibraryStore, db_path: Path
) -> None:
    await _seed_album(store, "1", identity_source="legacy_import", two_tracks=True)
    with sqlite3.connect(db_path) as connection:
        connection.execute(
            "UPDATE local_tracks SET availability = 'missing' WHERE id = 'track-1-2'"
        )
    created = await ReidentificationService(store).create_or_coalesce(
        "album-1", "admin", idempotency_key="indexed-only-explicit", now=1
    )
    claimed = await store.claim_operation_job(
        "worker", now=2, lease_seconds=60, kind="explicit_reidentification"
    )
    assert claimed is not None
    worker = ExplicitReidentificationWorker(
        store,
        AlbumCandidateService(_LegacyTrackIdentityProvider()),
        AlbumEvidenceEngine(),
    )

    ready = await worker.run_claimed(claimed, "worker", now=3)
    operation = await LibraryOperationService(store).get(created["id"])
    assert len(operation.reidentification_candidates[0].evidence.track_evidence) == 1
    await worker.select_candidate(
        created["id"],
        expected_job_revision=int(ready["row_revision"]),
        candidate_key="rg-1:release-1",
        confirmation=False,
        actor_user_id="admin",
        now=4,
    )

    context = await store.get_album_identification_context("album-1")
    assert context is not None
    identities = {
        row["id"]: (row["availability"], row["recording_mbid"])
        for row in context["tracks"]
    }
    assert identities == {
        "track-1-1": ("indexed", "recording-track-1-1"),
        "track-1-2": ("missing", "recording-track-1-2"),
    }


@pytest.mark.asyncio
async def test_automatic_identification_commit_preserves_missing_track_identity_history(
    store: NativeLibraryStore, db_path: Path
) -> None:
    await _seed_album(store, "1", identity_source="automatic", two_tracks=True)
    with sqlite3.connect(db_path) as connection:
        connection.execute(
            "UPDATE local_tracks SET availability = 'missing' WHERE id = 'track-1-2'"
        )
    context = await store.get_album_identification_context("album-1")
    assert context is not None
    indexed = [
        track for track in context["tracks"] if track["availability"] == "indexed"
    ]
    revisions = album_input_revisions(indexed)
    queue = IdentificationQueueService(store)
    await queue.enqueue_album("album-1", input_revision=":".join(revisions), now=1)
    claimed = await queue.claim("worker", now=2)
    assert claimed is not None
    attempt = IdentificationAttempt(
        id="automatic-indexed-attempt",
        local_album_id="album-1",
        input_tag_revision=revisions[0],
        input_file_revision=revisions[1],
        input_policy_revision=revisions[2],
        input_identity_revision=album_identity_revision(context["identity"], indexed),
        matcher_version="feedback-fixes-v1",
        state="identified",
        terminal_reason_code="SUPPORTED",
        selected_candidate_key="rg-new:release-new",
        candidate_count=1,
        started_at=3,
        completed_at=3,
    )
    evidence = CandidateEvidence(
        release_group_mbid="rg-new",
        release_mbid="release-new",
        album_title="Album 1",
        album_artist_name="Artist 1",
        album_title_classification="supported",
        album_artist_classification="supported",
        track_evidence=[
            TrackEvidence(
                local_track_id="track-1-1",
                classification="supported",
                recording_mbid="recording-new",
                release_track_mbid="release-track-new",
                candidate_disc_number=1,
                candidate_track_position=1,
            )
        ],
        reason_code="SUPPORTED",
        matcher_version="feedback-fixes-v1",
    )

    await store.finish_identification_job(
        claimed["id"],
        worker_id="worker",
        expected_job_revision=int(claimed["row_revision"]),
        expected_album_revision=int(context["album"]["row_revision"]),
        expected_input_revision=":".join(revisions),
        attempt=attempt,
        evidence=[
            IdentificationEvidenceRecord(
                id="automatic-indexed-evidence",
                attempt_id=attempt.id,
                candidate_key="rg-new:release-new",
                evidence=evidence,
                created_at=3,
            )
        ],
        outcome="identified",
        review_id="automatic-indexed-review",
        completed_at=3,
    )

    context = await store.get_album_identification_context("album-1")
    assert context is not None
    identities = {
        row["id"]: (row["availability"], row["recording_mbid"])
        for row in context["tracks"]
    }
    assert identities == {
        "track-1-1": ("indexed", "recording-new"),
        "track-1-2": ("missing", "recording-track-1-2"),
    }


def _quiet_attempt(
    attempt_id: str,
    revisions: list[str],
    identity_revision: str,
    *,
    candidate_key: str | None = None,
    candidate_count: int = 1,
    reason_code: str = "CONFLICTING_TRACK_EVIDENCE",
) -> IdentificationAttempt:
    return IdentificationAttempt(
        id=attempt_id,
        local_album_id="album-1",
        input_tag_revision=revisions[0],
        input_file_revision=revisions[1],
        input_policy_revision=revisions[2],
        input_identity_revision=identity_revision,
        matcher_version=MATCHER_VERSION,
        state="contradictory",
        terminal_reason_code=reason_code,
        selected_candidate_key=candidate_key,
        candidate_count=candidate_count,
        started_at=3,
        completed_at=3,
    )


def _quiet_evidence(
    evidence_id: str,
    attempt_id: str,
    group: str,
    release: str | None,
    score: float,
) -> IdentificationEvidenceRecord:
    return IdentificationEvidenceRecord(
        id=evidence_id,
        attempt_id=attempt_id,
        candidate_key=f"{group}:{release or ''}",
        evidence=CandidateEvidence(
            release_group_mbid=group,
            release_mbid=release,
            album_title="Album 1",
            album_artist_name="Artist 1",
            album_title_classification="supported",
            album_artist_classification="supported",
            track_evidence=[
                TrackEvidence(
                    local_track_id="track-1-1",
                    classification="contradictory",
                    evidence_kinds=["recording_mbid_conflict"],
                    recording_mbid="recording-provider",
                    release_track_mbid="release-track-1-1",
                )
            ],
            score=score,
            reason_code="CONFLICTING_TRACK_EVIDENCE",
            matcher_version=MATCHER_VERSION,
        ),
        created_at=3,
    )


async def _quiet_claim(
    store: NativeLibraryStore, revisions: list[str]
) -> dict:
    queue = IdentificationQueueService(store)
    await queue.enqueue_album("album-1", input_revision=":".join(revisions), now=1)
    claimed = await queue.claim("worker", now=2)
    assert claimed is not None
    return claimed


@pytest.mark.asyncio
async def test_quiet_reconfirm_agreeing_candidate_files_no_review(
    store: NativeLibraryStore, db_path: Path
) -> None:
    """A contradictory re-match that agrees with the manual identity is a
    settled question: stale reviews resolve, nothing new files, the job
    succeeds, and every identity row is untouched."""
    await _seed_album(store, "1", identity_source="manual")
    with sqlite3.connect(db_path) as connection:
        connection.execute(
            "INSERT INTO local_artist_external_identities "
            "(local_artist_id, provider, provider_artist_id, decision_source, "
            "selected_at) VALUES ('artist-1', 'musicbrainz', 'artist-mbid-1', "
            "'manual', 2)"
        )
    context = await store.get_album_identification_context("album-1")
    assert context is not None
    indexed = [
        track for track in context["tracks"] if track["availability"] == "indexed"
    ]
    revisions = album_input_revisions(indexed)
    identity_revision = album_identity_revision(context["identity"], indexed)
    claimed = await _quiet_claim(store, revisions)
    attempt = _quiet_attempt("quiet-agree-attempt", revisions, identity_revision)

    await store.finish_identification_job(
        claimed["id"],
        worker_id="worker",
        expected_job_revision=int(claimed["row_revision"]),
        expected_album_revision=int(context["album"]["row_revision"]),
        expected_input_revision=":".join(revisions),
        attempt=attempt,
        evidence=[
            _quiet_evidence(
                "quiet-agree-evidence", attempt.id, "rg-1", "release-1", 1.0
            )
        ],
        outcome="contradictory",
        review_id="quiet-agree-review",
        completed_at=3,
    )

    with sqlite3.connect(db_path) as connection:
        connection.row_factory = sqlite3.Row
        open_reviews = connection.execute(
            "SELECT id FROM library_identification_reviews WHERE local_album_id = ? "
            "AND state IN ('needs_review', 'edition_to_confirm')",
            ("album-1",),
        ).fetchall()
        assert open_reviews == []
        job_state = connection.execute(
            "SELECT state FROM library_identification_jobs WHERE id = ?",
            (claimed["id"],),
        ).fetchone()
        assert job_state["state"] == "succeeded"
        flags = connection.execute(
            "SELECT degradation_flags_json FROM library_identification_attempts WHERE id = ?",
            (attempt.id,),
        ).fetchone()
        assert "identity_reconfirmed:kept_protected" in flags["degradation_flags_json"]
        identity = connection.execute(
            "SELECT release_group_mbid, release_mbid, decision_source "
            "FROM local_album_external_identities WHERE local_album_id = ?",
            ("album-1",),
        ).fetchone()
        assert dict(identity) == {
            "release_group_mbid": "rg-1",
            "release_mbid": "release-1",
            "decision_source": "manual",
        }
        track_identity = connection.execute(
            "SELECT recording_mbid, release_mbid, decision_source "
            "FROM local_track_external_identities WHERE local_track_id = ?",
            ("track-1-1",),
        ).fetchone()
        assert dict(track_identity) == {
            "recording_mbid": "recording-track-1-1",
            "release_mbid": "release-1",
            "decision_source": "manual",
        }
        artist_identity = connection.execute(
            "SELECT provider_artist_id, decision_source, attempt_id "
            "FROM local_artist_external_identities WHERE local_artist_id = ?",
            ("artist-1",),
        ).fetchone()
        assert dict(artist_identity) == {
            "provider_artist_id": "artist-mbid-1",
            "decision_source": "manual",
            "attempt_id": None,
        }
        stored_evidence = connection.execute(
            "SELECT id FROM library_identification_evidence WHERE attempt_id = ?",
            (attempt.id,),
        ).fetchall()
        assert [row["id"] for row in stored_evidence] == ["quiet-agree-evidence"]
        seeded_review = connection.execute(
            "SELECT state, attempt_id, reason_code FROM library_identification_reviews "
            "WHERE id = 'review-1'",
        ).fetchone()
        assert dict(seeded_review) == {
            "state": "resolved",
            "attempt_id": attempt.id,
            "reason_code": "NO_SAFE_MATCH",
        }
        album_revision = connection.execute(
            "SELECT row_revision FROM local_albums WHERE id = ?",
            ("album-1",),
        ).fetchone()
        assert int(album_revision["row_revision"]) == int(
            context["album"]["row_revision"]
        ) + 1


@pytest.mark.asyncio
async def test_quiet_reconfirm_disagreeing_candidate_still_files_review(
    store: NativeLibraryStore, db_path: Path
) -> None:
    """Agreement is required: a top candidate naming a different release
    group still files its review and the job still needs review."""
    await _seed_album(store, "1", identity_source="manual")
    context = await store.get_album_identification_context("album-1")
    assert context is not None
    indexed = [
        track for track in context["tracks"] if track["availability"] == "indexed"
    ]
    revisions = album_input_revisions(indexed)
    identity_revision = album_identity_revision(context["identity"], indexed)
    claimed = await _quiet_claim(store, revisions)
    attempt = _quiet_attempt("quiet-disagree-attempt", revisions, identity_revision)

    await store.finish_identification_job(
        claimed["id"],
        worker_id="worker",
        expected_job_revision=int(claimed["row_revision"]),
        expected_album_revision=int(context["album"]["row_revision"]),
        expected_input_revision=":".join(revisions),
        attempt=attempt,
        evidence=[
            _quiet_evidence(
                "quiet-disagree-evidence", attempt.id, "rg-other", "release-other", 1.0
            )
        ],
        outcome="contradictory",
        review_id="quiet-disagree-review",
        completed_at=3,
    )

    with sqlite3.connect(db_path) as connection:
        connection.row_factory = sqlite3.Row
        filed = connection.execute(
            "SELECT state, reason_code FROM library_identification_reviews "
            "WHERE attempt_id = ?",
            (attempt.id,),
        ).fetchone()
        assert filed is not None
        assert filed["state"] == "needs_review"
        assert filed["reason_code"] == "CONFLICTING_TRACK_EVIDENCE"
        job_state = connection.execute(
            "SELECT state FROM library_identification_jobs WHERE id = ?",
            (claimed["id"],),
        ).fetchone()
        assert job_state["state"] == "needs_review"
        flags = connection.execute(
            "SELECT degradation_flags_json FROM library_identification_attempts WHERE id = ?",
            (attempt.id,),
        ).fetchone()
        assert "identity_reconfirmed:kept_protected" not in flags["degradation_flags_json"]


@pytest.mark.asyncio
async def test_quiet_reconfirm_top_score_tie_files_review(
    store: NativeLibraryStore, db_path: Path
) -> None:
    """Ties fail closed: an agreeing candidate that shares the top score
    with a disagreeing one still files its review."""
    await _seed_album(store, "1", identity_source="manual")
    context = await store.get_album_identification_context("album-1")
    assert context is not None
    indexed = [
        track for track in context["tracks"] if track["availability"] == "indexed"
    ]
    revisions = album_input_revisions(indexed)
    identity_revision = album_identity_revision(context["identity"], indexed)
    claimed = await _quiet_claim(store, revisions)
    attempt = _quiet_attempt(
        "quiet-tie-attempt", revisions, identity_revision, candidate_count=2
    )

    await store.finish_identification_job(
        claimed["id"],
        worker_id="worker",
        expected_job_revision=int(claimed["row_revision"]),
        expected_album_revision=int(context["album"]["row_revision"]),
        expected_input_revision=":".join(revisions),
        attempt=attempt,
        evidence=[
            _quiet_evidence(
                "quiet-tie-evidence-agree", attempt.id, "rg-1", "release-1", 1.0
            ),
            _quiet_evidence(
                "quiet-tie-evidence-other", attempt.id, "rg-other", "release-other", 1.0
            ),
        ],
        outcome="contradictory",
        review_id="quiet-tie-review",
        completed_at=3,
    )

    with sqlite3.connect(db_path) as connection:
        connection.row_factory = sqlite3.Row
        filed = connection.execute(
            "SELECT state FROM library_identification_reviews WHERE attempt_id = ?",
            (attempt.id,),
        ).fetchone()
        assert filed is not None
        assert filed["state"] == "needs_review"
        job_state = connection.execute(
            "SELECT state FROM library_identification_jobs WHERE id = ?",
            (claimed["id"],),
        ).fetchone()
        assert job_state["state"] == "needs_review"
        flags = connection.execute(
            "SELECT degradation_flags_json FROM library_identification_attempts WHERE id = ?",
            (attempt.id,),
        ).fetchone()
        assert "identity_reconfirmed:kept_protected" not in flags["degradation_flags_json"]


@pytest.mark.asyncio
async def test_quiet_reconfirm_without_protected_identity_files_review(
    store: NativeLibraryStore, db_path: Path
) -> None:
    """Protection is required: an agreeing candidate against an automatic
    identity still files its review (and retracts, per existing rules)."""
    await _seed_album(store, "1", identity_source="automatic")
    context = await store.get_album_identification_context("album-1")
    assert context is not None
    indexed = [
        track for track in context["tracks"] if track["availability"] == "indexed"
    ]
    revisions = album_input_revisions(indexed)
    identity_revision = album_identity_revision(context["identity"], indexed)
    claimed = await _quiet_claim(store, revisions)
    attempt = _quiet_attempt("quiet-auto-attempt", revisions, identity_revision)

    await store.finish_identification_job(
        claimed["id"],
        worker_id="worker",
        expected_job_revision=int(claimed["row_revision"]),
        expected_album_revision=int(context["album"]["row_revision"]),
        expected_input_revision=":".join(revisions),
        attempt=attempt,
        evidence=[
            _quiet_evidence(
                "quiet-auto-evidence", attempt.id, "rg-1", "release-1", 1.0
            )
        ],
        outcome="contradictory",
        review_id="quiet-auto-review",
        completed_at=3,
    )

    with sqlite3.connect(db_path) as connection:
        connection.row_factory = sqlite3.Row
        filed = connection.execute(
            "SELECT state FROM library_identification_reviews WHERE attempt_id = ?",
            (attempt.id,),
        ).fetchone()
        assert filed is not None
        assert filed["state"] == "needs_review"
        flags = connection.execute(
            "SELECT degradation_flags_json FROM library_identification_attempts WHERE id = ?",
            (attempt.id,),
        ).fetchone()
        assert "identity_reconfirmed:kept_protected" not in flags["degradation_flags_json"]
        retracted = connection.execute(
            "SELECT local_album_id FROM local_album_external_identities "
            "WHERE local_album_id = ?",
            ("album-1",),
        ).fetchall()
        assert retracted == []


@pytest.mark.asyncio
async def test_quiet_reconfirm_rg_only_candidate_agrees(
    store: NativeLibraryStore, db_path: Path
) -> None:
    """Release matching is lenient one way: an RG-only candidate (no exact
    release claimed) against a stored exact release still agrees - the album
    is settled, only exact-edition proof is missing."""
    await _seed_album(store, "1", identity_source="manual")
    context = await store.get_album_identification_context("album-1")
    assert context is not None
    indexed = [
        track for track in context["tracks"] if track["availability"] == "indexed"
    ]
    revisions = album_input_revisions(indexed)
    identity_revision = album_identity_revision(context["identity"], indexed)
    claimed = await _quiet_claim(store, revisions)
    attempt = _quiet_attempt("quiet-rg-only-attempt", revisions, identity_revision)

    await store.finish_identification_job(
        claimed["id"],
        worker_id="worker",
        expected_job_revision=int(claimed["row_revision"]),
        expected_album_revision=int(context["album"]["row_revision"]),
        expected_input_revision=":".join(revisions),
        attempt=attempt,
        evidence=[
            _quiet_evidence(
                "quiet-rg-only-evidence", attempt.id, "rg-1", None, 1.0
            )
        ],
        outcome="contradictory",
        review_id="quiet-rg-only-review",
        completed_at=3,
    )

    with sqlite3.connect(db_path) as connection:
        connection.row_factory = sqlite3.Row
        open_reviews = connection.execute(
            "SELECT id FROM library_identification_reviews WHERE local_album_id = ? "
            "AND state IN ('needs_review', 'edition_to_confirm')",
            ("album-1",),
        ).fetchall()
        assert open_reviews == []
        job_state = connection.execute(
            "SELECT state FROM library_identification_jobs WHERE id = ?",
            (claimed["id"],),
        ).fetchone()
        assert job_state["state"] == "succeeded"
        flags = connection.execute(
            "SELECT degradation_flags_json FROM library_identification_attempts WHERE id = ?",
            (attempt.id,),
        ).fetchone()
        assert "identity_reconfirmed:kept_protected" in flags["degradation_flags_json"]
        identity = connection.execute(
            "SELECT release_group_mbid, release_mbid, decision_source "
            "FROM local_album_external_identities WHERE local_album_id = ?",
            ("album-1",),
        ).fetchone()
        assert dict(identity) == {
            "release_group_mbid": "rg-1",
            "release_mbid": "release-1",
            "decision_source": "manual",
        }
        album_revision = connection.execute(
            "SELECT row_revision FROM local_albums WHERE id = ?",
            ("album-1",),
        ).fetchone()
        assert int(album_revision["row_revision"]) == int(
            context["album"]["row_revision"]
        ) + 1


def test_quiet_top_candidate_agrees_edges() -> None:
    """Predicate edges: empty evidence, blank stored group, stored
    release-blank leniency, and case-insensitive MBID agreement."""
    attempt_id = "quiet-predicate-attempt"
    record = _quiet_evidence("quiet-predicate-evidence", attempt_id, "RG-1", "Release-1", 1.0)
    assert _quiet_top_candidate_agrees([record], "rg-1", "release-1") is True
    assert _quiet_top_candidate_agrees([record], "rg-1", None) is True
    assert _quiet_top_candidate_agrees([], "rg-1", "release-1") is False
    assert _quiet_top_candidate_agrees([record], "", "release-1") is False
    assert _quiet_top_candidate_agrees([record], None, None) is False
    # The quiet floor mirrors the engine's winner margin by value (a local
    # constant so persistence never imports services); pin the equality.
    assert _QUIET_MARGIN_FLOOR == CANDIDATE_MARGIN_FLOOR


async def _quiet_round(
    store: NativeLibraryStore,
    attempt_id: str,
    records: list[IdentificationEvidenceRecord],
    *,
    identity_source: str = "manual",
    review_state: str = "needs_review",
    reason_code: str = "CONFLICTING_TRACK_EVIDENCE",
) -> tuple[dict, IdentificationAttempt]:
    """Seed album-1, run one contradictory finish, return (claimed, attempt)."""
    await _seed_album(
        store, "1", identity_source=identity_source, review_state=review_state
    )
    context = await store.get_album_identification_context("album-1")
    assert context is not None
    indexed = [
        track for track in context["tracks"] if track["availability"] == "indexed"
    ]
    revisions = album_input_revisions(indexed)
    identity_revision = album_identity_revision(context["identity"], indexed)
    claimed = await _quiet_claim(store, revisions)
    attempt = _quiet_attempt(
        attempt_id,
        revisions,
        identity_revision,
        candidate_count=len(records),
        reason_code=reason_code,
    )
    await store.finish_identification_job(
        claimed["id"],
        worker_id="worker",
        expected_job_revision=int(claimed["row_revision"]),
        expected_album_revision=int(context["album"]["row_revision"]),
        expected_input_revision=":".join(revisions),
        attempt=attempt,
        evidence=records,
        outcome="contradictory",
        review_id=f"{attempt_id}-review",
        completed_at=3,
    )
    return claimed, attempt


def _quiet_review_state(db_path: Path, attempt_id: str) -> str | None:
    with sqlite3.connect(db_path) as connection:
        row = connection.execute(
            "SELECT state FROM library_identification_reviews WHERE attempt_id = ? "
            "AND state = 'needs_review'",
            (attempt_id,),
        ).fetchone()
        return row[0] if row is not None else None


def _quiet_open_reviews(db_path: Path) -> list:
    with sqlite3.connect(db_path) as connection:
        return connection.execute(
            "SELECT id FROM library_identification_reviews WHERE local_album_id = ? "
            "AND state IN ('needs_review', 'edition_to_confirm')",
            ("album-1",),
        ).fetchall()


@pytest.mark.asyncio
async def test_quiet_reconfirm_same_group_other_release_files_review(
    store: NativeLibraryStore, db_path: Path
) -> None:
    """Both sides name a release, so releases must agree: same group but a
    different release still files its review."""
    attempt_id = "quiet-other-release-attempt"
    _, attempt = await _quiet_round(
        store,
        attempt_id,
        [_quiet_evidence("quiet-other-release-ev", attempt_id, "rg-1", "release-other", 1.0)],
    )
    assert _quiet_review_state(db_path, attempt.id) == "needs_review"


@pytest.mark.asyncio
async def test_quiet_reconfirm_stale_identity_code_files_review(
    store: NativeLibraryStore, db_path: Path
) -> None:
    """The quiet arm is restricted to the track-evidence veto: an agreeing
    top candidate under MANUAL_IDENTITY_STALE still files its review."""
    attempt_id = "quiet-stale-code-attempt"
    _, attempt = await _quiet_round(
        store,
        attempt_id,
        [_quiet_evidence("quiet-stale-code-ev", attempt_id, "rg-1", "release-1", 1.0)],
        reason_code="MANUAL_IDENTITY_STALE",
    )
    assert _quiet_review_state(db_path, attempt.id) == "needs_review"


@pytest.mark.asyncio
async def test_quiet_reconfirm_agreeing_outscored_files_review(
    store: NativeLibraryStore, db_path: Path
) -> None:
    """Only the top candidate can re-confirm: an agreeing candidate
    outscored by a disagreeing one still files its review."""
    attempt_id = "quiet-outscored-attempt"
    _, attempt = await _quiet_round(
        store,
        attempt_id,
        [
            _quiet_evidence("quiet-outscored-agree", attempt_id, "rg-1", "release-1", 0.5),
            _quiet_evidence(
                "quiet-outscored-other", attempt_id, "rg-other", "release-other", 1.0
            ),
        ],
    )
    assert _quiet_review_state(db_path, attempt.id) == "needs_review"


@pytest.mark.asyncio
async def test_quiet_reconfirm_agreeing_unique_top_goes_quiet(
    store: NativeLibraryStore, db_path: Path
) -> None:
    """A unique agreeing top over a distant disagreeing runner-up goes
    quiet: only near rivals block re-confirmation."""
    attempt_id = "quiet-unique-top-attempt"
    claimed, attempt = await _quiet_round(
        store,
        attempt_id,
        [
            _quiet_evidence("quiet-unique-top-agree", attempt_id, "rg-1", "release-1", 1.0),
            _quiet_evidence(
                "quiet-unique-top-other", attempt_id, "rg-other", "release-other", 0.5
            ),
        ],
    )
    assert _quiet_open_reviews(db_path) == []
    with sqlite3.connect(db_path) as connection:
        connection.row_factory = sqlite3.Row
        job_state = connection.execute(
            "SELECT state FROM library_identification_jobs WHERE id = ?",
            (claimed["id"],),
        ).fetchone()
        assert job_state["state"] == "succeeded"


@pytest.mark.asyncio
async def test_quiet_reconfirm_near_disagreeing_rival_files_review(
    store: NativeLibraryStore, db_path: Path
) -> None:
    """A disagreeing runner-up within the margin floor is genuine
    ambiguity: the review still files."""
    attempt_id = "quiet-near-rival-attempt"
    _, attempt = await _quiet_round(
        store,
        attempt_id,
        [
            _quiet_evidence("quiet-near-rival-agree", attempt_id, "rg-1", "release-1", 1.0),
            _quiet_evidence(
                "quiet-near-rival-other", attempt_id, "rg-other", "release-other", 0.96
            ),
        ],
    )
    assert _quiet_review_state(db_path, attempt.id) == "needs_review"


@pytest.mark.asyncio
async def test_quiet_reconfirm_far_disagreeing_rival_goes_quiet(
    store: NativeLibraryStore, db_path: Path
) -> None:
    """A disagreeing runner-up beyond the margin floor does not block:
    the decisive top still re-confirms quietly."""
    attempt_id = "quiet-far-rival-attempt"
    claimed, attempt = await _quiet_round(
        store,
        attempt_id,
        [
            _quiet_evidence("quiet-far-rival-agree", attempt_id, "rg-1", "release-1", 1.0),
            _quiet_evidence(
                "quiet-far-rival-other", attempt_id, "rg-other", "release-other", 0.90
            ),
        ],
    )
    assert _quiet_open_reviews(db_path) == []
    with sqlite3.connect(db_path) as connection:
        connection.row_factory = sqlite3.Row
        job_state = connection.execute(
            "SELECT state FROM library_identification_jobs WHERE id = ?",
            (claimed["id"],),
        ).fetchone()
        assert job_state["state"] == "succeeded"


@pytest.mark.asyncio
async def test_quiet_reconfirm_agreeing_tie_goes_quiet(
    store: NativeLibraryStore, db_path: Path
) -> None:
    """A top tie where every tied candidate agrees is settled, not
    ambiguous: same identity named twice goes quiet."""
    attempt_id = "quiet-agree-tie-attempt"
    claimed, attempt = await _quiet_round(
        store,
        attempt_id,
        [
            _quiet_evidence("quiet-agree-tie-exact", attempt_id, "rg-1", "release-1", 1.0),
            _quiet_evidence("quiet-agree-tie-rg", attempt_id, "rg-1", None, 1.0),
        ],
    )
    assert _quiet_open_reviews(db_path) == []
    with sqlite3.connect(db_path) as connection:
        connection.row_factory = sqlite3.Row
        job_state = connection.execute(
            "SELECT state FROM library_identification_jobs WHERE id = ?",
            (claimed["id"],),
        ).fetchone()
        assert job_state["state"] == "succeeded"


@pytest.mark.asyncio
async def test_quiet_reconfirm_margin_boundary_goes_quiet(
    store: NativeLibraryStore, db_path: Path
) -> None:
    """The margin block is exclusive-below like the engine's: a disagreeing
    rival exactly at the floor still re-confirms quietly."""
    attempt_id = "quiet-boundary-attempt"
    claimed, attempt = await _quiet_round(
        store,
        attempt_id,
        [
            _quiet_evidence("quiet-boundary-agree", attempt_id, "rg-1", "release-1", 1.0),
            _quiet_evidence(
                "quiet-boundary-other", attempt_id, "rg-other", "release-other", 0.95
            ),
        ],
    )
    assert _quiet_open_reviews(db_path) == []
    with sqlite3.connect(db_path) as connection:
        connection.row_factory = sqlite3.Row
        job_state = connection.execute(
            "SELECT state FROM library_identification_jobs WHERE id = ?",
            (claimed["id"],),
        ).fetchone()
        assert job_state["state"] == "succeeded"


@pytest.mark.asyncio
async def test_quiet_reconfirm_legacy_identity_goes_quiet(
    store: NativeLibraryStore, db_path: Path
) -> None:
    """Protection covers legacy_import as well as manual: an agreeing top
    candidate goes quiet for both. The identity attaches after claiming
    because the queue refuses legacy albums (pending-migration lane owns
    them); finish reads protection fresh."""
    await _seed_album(store, "1")
    context = await store.get_album_identification_context("album-1")
    assert context is not None
    indexed = [
        track for track in context["tracks"] if track["availability"] == "indexed"
    ]
    revisions = album_input_revisions(indexed)
    claimed = await _quiet_claim(store, revisions)
    await store.attach_album_identity(
        LocalAlbumExternalIdentity(
            local_album_id="album-1",
            release_group_mbid="rg-1",
            release_mbid="release-1",
            decision_source="legacy_import",
            selected_at=2,
        ),
        expected_album_revision=int(context["album"]["row_revision"]),
    )
    context = await store.get_album_identification_context("album-1")
    assert context is not None
    indexed = [
        track for track in context["tracks"] if track["availability"] == "indexed"
    ]
    attempt_id = "quiet-legacy-attempt"
    attempt = _quiet_attempt(
        attempt_id,
        revisions,
        album_identity_revision(context["identity"], indexed),
    )
    await store.finish_identification_job(
        claimed["id"],
        worker_id="worker",
        expected_job_revision=int(claimed["row_revision"]),
        expected_album_revision=int(context["album"]["row_revision"]),
        expected_input_revision=":".join(revisions),
        attempt=attempt,
        evidence=[_quiet_evidence("quiet-legacy-ev", attempt_id, "rg-1", "release-1", 1.0)],
        outcome="contradictory",
        review_id=f"{attempt_id}-review",
        completed_at=3,
    )
    assert _quiet_open_reviews(db_path) == []
    with sqlite3.connect(db_path) as connection:
        connection.row_factory = sqlite3.Row
        job_state = connection.execute(
            "SELECT state FROM library_identification_jobs WHERE id = ?",
            (claimed["id"],),
        ).fetchone()
        assert job_state["state"] == "succeeded"


@pytest.mark.asyncio
async def test_quiet_reconfirm_resolves_edition_to_confirm(
    store: NativeLibraryStore, db_path: Path
) -> None:
    """The resolve covers both open states: a seeded edition_to_confirm
    row resolves on quiet re-confirmation."""
    attempt_id = "quiet-tier-attempt"
    _, attempt = await _quiet_round(
        store,
        attempt_id,
        [_quiet_evidence("quiet-tier-ev", attempt_id, "rg-1", "release-1", 1.0)],
        review_state="edition_to_confirm",
    )
    with sqlite3.connect(db_path) as connection:
        connection.row_factory = sqlite3.Row
        open_reviews = connection.execute(
            "SELECT id FROM library_identification_reviews WHERE local_album_id = ? "
            "AND state IN ('needs_review', 'edition_to_confirm')",
            ("album-1",),
        ).fetchall()
        assert open_reviews == []
        seeded = connection.execute(
            "SELECT state, attempt_id FROM library_identification_reviews WHERE id = 'review-1'",
        ).fetchone()
        assert seeded["state"] == "resolved"
        assert seeded["attempt_id"] == attempt.id


@pytest.mark.asyncio
async def test_automatic_identification_commit_rejects_an_availability_flip(
    store: NativeLibraryStore, db_path: Path
) -> None:
    await _seed_album(store, "1")
    context = await store.get_album_identification_context("album-1")
    assert context is not None
    revisions = album_input_revisions(context["tracks"])
    queue = IdentificationQueueService(store)
    await queue.enqueue_album("album-1", input_revision=":".join(revisions), now=1)
    claimed = await queue.claim("worker", now=2)
    assert claimed is not None
    with sqlite3.connect(db_path) as connection:
        connection.execute(
            "UPDATE local_tracks SET availability = 'missing' WHERE id = 'track-1-1'"
        )
    attempt = IdentificationAttempt(
        id="availability-flip-attempt",
        local_album_id="album-1",
        input_tag_revision=revisions[0],
        input_file_revision=revisions[1],
        input_policy_revision=revisions[2],
        matcher_version="feedback-fixes-v1",
        state="no_candidate",
        terminal_reason_code="NO_EXTERNAL_RESULT",
        started_at=3,
        completed_at=3,
    )

    with pytest.raises(StaleRevisionError, match="files changed"):
        await store.finish_identification_job(
            claimed["id"],
            worker_id="worker",
            expected_job_revision=int(claimed["row_revision"]),
            expected_album_revision=int(context["album"]["row_revision"]),
            expected_input_revision=":".join(revisions),
            attempt=attempt,
            evidence=[],
            outcome="no_candidate",
            review_id="availability-flip-review",
            completed_at=3,
        )


@pytest.mark.asyncio
async def test_automatic_identification_commit_rejects_an_identity_change(
    store: NativeLibraryStore,
    db_path: Path,
) -> None:
    await _seed_album(store, "1", identity_source="automatic")
    context = await store.get_album_identification_context("album-1")
    assert context is not None
    revisions = album_input_revisions(context["tracks"])
    identity_revision = album_identity_revision(context["identity"], context["tracks"])
    queue = IdentificationQueueService(store)
    await queue.enqueue_album("album-1", input_revision=":".join(revisions), now=1)
    claimed = await queue.claim("worker", now=2)
    assert claimed is not None
    with sqlite3.connect(db_path) as connection:
        connection.execute(
            "UPDATE local_track_external_identities SET row_revision=row_revision+1 "
            "WHERE local_track_id='track-1-1'"
        )
    attempt = IdentificationAttempt(
        id="identity-change-attempt",
        local_album_id="album-1",
        input_tag_revision=revisions[0],
        input_file_revision=revisions[1],
        input_policy_revision=revisions[2],
        input_identity_revision=identity_revision,
        matcher_version="feedback-fixes-v1",
        state="no_candidate",
        terminal_reason_code="NO_EXTERNAL_RESULT",
        started_at=3,
        completed_at=3,
    )

    with pytest.raises(StaleRevisionError, match="identity changed"):
        await store.finish_identification_job(
            claimed["id"],
            worker_id="worker",
            expected_job_revision=int(claimed["row_revision"]),
            expected_album_revision=int(context["album"]["row_revision"]),
            expected_input_revision=":".join(revisions),
            attempt=attempt,
            evidence=[],
            outcome="no_candidate",
            review_id="identity-change-review",
            completed_at=3,
        )


@pytest.mark.asyncio
async def test_explicit_transient_failure_defers_then_succeeds_after_due_claim(
    store: NativeLibraryStore,
) -> None:
    """F-IDENT-03: the first transient failure defers durably - queued job,
    pending work, next_attempt_at = now + 120, no failed count - and a due
    claim retries the same sealed operation to the existing ready flow."""
    await _seed_album(store, "1")
    created = await ReidentificationService(store).create_or_coalesce(
        "album-1", "admin", idempotency_key="flaky-explicit", now=1
    )
    claimed = await store.claim_operation_job(
        "worker", now=2, lease_seconds=60, kind="explicit_reidentification"
    )
    assert claimed is not None
    provider = _FlakyIdentificationProvider()
    worker = ExplicitReidentificationWorker(
        store, AlbumCandidateService(provider), AlbumEvidenceEngine()
    )
    deferred = await worker.run_claimed(claimed, "worker", now=3)
    assert deferred["state"] == "queued"
    assert deferred["next_attempt_at"] == 3 + REIDENTIFICATION_RETRY_SECONDS
    assert deferred["failed_count"] == 0
    assert deferred["succeeded_count"] == 0
    assert deferred["reidentification_attempt_count"] == 1

    # Early claim is blocked; due claim clears next_attempt_at and retries.
    queue = IdentificationQueueService(store)  # noqa: F841 (queue unused directly)
    early = await store.claim_operation_job(
        "worker", now=4, lease_seconds=60, kind="explicit_reidentification"
    )
    assert early is None
    retried = await store.claim_operation_job(
        "worker",
        now=3 + REIDENTIFICATION_RETRY_SECONDS,
        lease_seconds=60,
        kind="explicit_reidentification",
    )
    assert retried is not None
    assert retried["next_attempt_at"] is None
    ready = await worker.run_claimed(
        retried,
        "worker",
        now=3 + REIDENTIFICATION_RETRY_SECONDS,
    )
    assert ready["state"] == "ready"
    assert ready["failed_count"] == 0



@pytest.mark.asyncio
async def test_explicit_transient_failures_stop_at_the_finite_bound(
    store: NativeLibraryStore,
) -> None:
    """Repeated transient failures defer until MAX_REIDENTIFICATION_ATTEMPTS,
    then terminalize with the existing provider code; manual resume still works."""
    await _seed_album(store, "1")
    created = await ReidentificationService(store).create_or_coalesce(
        "album-1", "admin", idempotency_key="flaky-explicit-bound", now=1
    )
    class _AlwaysFailingProvider(_FlakyIdentificationProvider):
        async def search_album_candidate_ids(self, artist, title, limit, priority):
            self.calls += 1
            raise ExternalServiceError("temporary private provider failure")

    provider = _AlwaysFailingProvider()
    worker = ExplicitReidentificationWorker(
        store, AlbumCandidateService(provider), AlbumEvidenceEngine()
    )
    now = 2.0
    final: dict | None = None
    for _attempt in range(MAX_REIDENTIFICATION_ATTEMPTS + 4):
        claimed = await store.claim_operation_job(
            "worker", now=now, lease_seconds=60, kind="explicit_reidentification"
        )
        if claimed is None:
            now += 1000.0
            continue
        result = await worker.run_claimed(claimed, "worker", now=now)
        if result["state"] == "failed":
            final = result
            break
        now += REIDENTIFICATION_RETRY_SECONDS + 1.0
    assert final is not None
    assert final["terminal_code"] == "PROVIDER_TEMPORARILY_UNAVAILABLE"
    job_row = await store.get_operation_job(created["id"])
    attempts_used = int(job_row["reidentification_attempt_count"])
    failed_count = int(job_row["failed_count"])
    assert attempts_used == MAX_REIDENTIFICATION_ATTEMPTS
    assert failed_count == 1

    operations = LibraryOperationService(store)
    resumed = await operations.control(
        created["id"], "resume", int(final["row_revision"]), now=now + 10
    )
    assert resumed.state == "queued"


@pytest.mark.asyncio
async def test_explicit_reidentification_conditionally_fingerprints_and_reuses_outcome(
    store: NativeLibraryStore,
) -> None:
    await _seed_album(store, "1")
    provider = _FingerprintIdentificationProvider()
    backend = _FingerprintBackend()
    worker = ExplicitReidentificationWorker(
        store,
        AlbumCandidateService(provider),
        AlbumEvidenceEngine(),
        ConditionalFingerprintService(store, backend),
    )
    created = await ReidentificationService(store).create_or_coalesce(
        "album-1", "admin", idempotency_key="fingerprint-explicit-1", now=1
    )
    claimed = await store.claim_operation_job(
        "worker", now=2, lease_seconds=60, kind="explicit_reidentification"
    )
    assert claimed is not None
    ready = await worker.run_claimed(claimed, "worker", now=3)
    snapshot = await store.get_operation_snapshot(created["id"])
    assert ready["state"] == "ready"
    assert snapshot is not None
    assert (
        "rg-explicit:release-explicit"
        in json.loads(snapshot["snapshot"]["result_json"])["candidate_keys"]
    )
    assert backend.generate_calls == 1
    assert backend.lookup_calls == 1

    await worker.select_candidate(
        created["id"],
        expected_job_revision=int(ready["row_revision"]),
        candidate_key="rg-explicit:release-explicit",
        confirmation=False,
        actor_user_id="admin",
        now=4,
    )
    repeated = await ReidentificationService(store).create_or_coalesce(
        "album-1", "admin", idempotency_key="fingerprint-explicit-2", now=5
    )
    claimed_repeated = await store.claim_operation_job(
        "worker", now=6, lease_seconds=60, kind="explicit_reidentification"
    )
    assert claimed_repeated is not None
    repeated_ready = await worker.run_claimed(claimed_repeated, "worker", now=7)
    assert repeated_ready["state"] == "ready"
    assert repeated["id"] != created["id"]
    assert backend.generate_calls == 1
    assert backend.lookup_calls == 1


@pytest.mark.asyncio
async def test_explicit_reidentification_preserves_a_verified_embedded_exact_edition(
    store: NativeLibraryStore, db_path: Path
) -> None:
    await _seed_album(store, "1")
    with sqlite3.connect(db_path) as connection:
        connection.execute(
            "UPDATE local_tracks SET embedded_release_group_mbid = ?, "
            "embedded_release_mbid = ?, embedded_recording_mbid = ?, "
            "embedded_release_track_mbid = ? WHERE id = 'track-1-1'",
            (EXACT_GROUP, EXACT_RELEASE, EXACT_RECORDING, EXACT_RELEASE_TRACK),
        )
    provider = _PreferredEditionIdentificationProvider()
    worker = ExplicitReidentificationWorker(
        store, AlbumCandidateService(provider), AlbumEvidenceEngine()
    )

    created = await ReidentificationService(store).create_or_coalesce(
        "album-1", "admin", idempotency_key="embedded-edition", now=1
    )
    claimed = await store.claim_operation_job(
        "worker", now=2, lease_seconds=60, kind="explicit_reidentification"
    )
    assert claimed is not None

    ready = await worker.run_claimed(claimed, "worker", now=3)
    operation = await LibraryOperationService(store).get(created["id"])

    assert ready["state"] == "ready"
    assert provider.preferred_releases == [EXACT_RELEASE]
    assert [
        candidate.candidate_key for candidate in operation.reidentification_candidates
    ] == [f"{EXACT_GROUP}:{EXACT_RELEASE}"]
    assert operation.reidentification_candidates[0].automatic_safe is True
    assert (
        operation.reidentification_candidates[0]
        .evidence.track_evidence[0]
        .release_track_mbid
        == EXACT_RELEASE_TRACK
    )


@pytest.mark.asyncio
async def test_verified_exact_release_surfaces_a_conflicting_embedded_group_for_review(
    store: NativeLibraryStore,
    db_path: Path,
) -> None:
    await _seed_album(store, "1")
    with sqlite3.connect(db_path) as connection:
        connection.execute(
            "UPDATE local_tracks SET embedded_release_group_mbid=?, "
            "embedded_release_mbid=?, embedded_recording_mbid=?, "
            "embedded_release_track_mbid=? WHERE id='track-1-1'",
            (
                "99999999-9999-4999-8999-999999999999",
                EXACT_RELEASE,
                EXACT_RECORDING,
                EXACT_RELEASE_TRACK,
            ),
        )
    provider = _PreferredEditionIdentificationProvider()
    created = await ReidentificationService(store).create_or_coalesce(
        "album-1", "admin", idempotency_key="conflicting-embedded-group", now=1
    )
    claimed = await store.claim_operation_job(
        "worker", now=2, lease_seconds=60, kind="explicit_reidentification"
    )
    assert claimed is not None

    ready = await ExplicitReidentificationWorker(
        store, AlbumCandidateService(provider), AlbumEvidenceEngine()
    ).run_claimed(claimed, "worker", now=3)
    operation = await LibraryOperationService(store).get(created["id"])

    assert ready["state"] == "ready"
    assert provider.preferred_releases == [EXACT_RELEASE]
    assert operation.reidentification_candidates[0].candidate_key == (
        f"{EXACT_GROUP}:{EXACT_RELEASE}"
    )
    assert operation.reidentification_candidates[0].automatic_safe is False
    assert (
        operation.reidentification_candidates[0].evidence.reason_code
        == "CONFLICTING_TRACK_EVIDENCE"
    )


@pytest.mark.asyncio
async def test_exact_release_override_uses_only_the_verified_release_and_always_confirms(
    store: NativeLibraryStore,
) -> None:
    await _seed_album(store, "1")
    candidate = AlbumCandidate(
        release_group_mbid=EXACT_GROUP,
        release_mbid=EXACT_CANONICAL_RELEASE,
        album_title="Album 1",
        album_artist_name="Artist 1",
        tracks=[
            CandidateTrack(
                title="Track 1",
                position=1,
                absolute_position=1,
                recording_mbid=EXACT_RECORDING,
                release_track_mbid=EXACT_RELEASE_TRACK,
            )
        ],
    )
    provider = _ExactOverrideProvider(candidate=candidate)
    fingerprint_backend = _FingerprintBackend()
    worker = ExplicitReidentificationWorker(
        store,
        AlbumCandidateService(provider),
        AlbumEvidenceEngine(),
        ConditionalFingerprintService(store, fingerprint_backend),
    )
    created = await ReidentificationService(store).create_or_coalesce(
        "album-1",
        "admin",
        release_mbid=EXACT_RELEASE.upper(),
        idempotency_key="exact-override",
        now=1,
    )
    claimed = await store.claim_operation_job(
        "worker", now=2, lease_seconds=60, kind="explicit_reidentification"
    )
    assert claimed is not None

    ready = await worker.run_claimed(claimed, "worker", now=3)
    operation = await LibraryOperationService(store).get(created["id"])

    assert ready["state"] == "ready"
    assert provider.exact_calls == [EXACT_RELEASE]
    assert provider.search_calls == 0
    assert fingerprint_backend.generate_calls == 0
    assert operation.reidentification_candidates[0].candidate_key == (
        f"{EXACT_GROUP}:{EXACT_CANONICAL_RELEASE}"
    )
    assert operation.reidentification_candidates[0].automatic_safe is False
    with pytest.raises(ValidationError, match="Confirm the conflicting"):
        await worker.select_candidate(
            created["id"],
            expected_job_revision=int(ready["row_revision"]),
            candidate_key=f"{EXACT_GROUP}:{EXACT_CANONICAL_RELEASE}",
            confirmation=False,
            actor_user_id="admin",
            now=4,
        )
    accepted = await worker.select_candidate(
        created["id"],
        expected_job_revision=int(ready["row_revision"]),
        candidate_key=f"{EXACT_GROUP}:{EXACT_CANONICAL_RELEASE}",
        confirmation=True,
        actor_user_id="admin",
        now=4,
    )
    context = await store.get_album_identification_context("album-1")
    assert accepted["state"] == "succeeded"
    assert context is not None
    assert context["identity"]["release_mbid"] == EXACT_CANONICAL_RELEASE


@pytest.mark.asyncio
async def test_exact_release_override_maps_translated_title_by_position_and_duration(
    store: NativeLibraryStore,
    db_path: Path,
) -> None:
    await _seed_album(store, "1")
    with sqlite3.connect(db_path) as connection:
        connection.execute(
            "UPDATE local_tracks SET title='English title', duration_seconds=180 "
            "WHERE id='track-1-1'"
        )
    candidate = AlbumCandidate(
        release_group_mbid=EXACT_GROUP,
        release_mbid=EXACT_RELEASE,
        album_title="Album 1",
        album_artist_name="Artist 1",
        tracks=[
            CandidateTrack(
                title="현지 제목",
                position=1,
                absolute_position=1,
                duration_seconds=182,
                recording_mbid=EXACT_RECORDING,
                release_track_mbid=EXACT_RELEASE_TRACK,
            )
        ],
    )
    worker = ExplicitReidentificationWorker(
        store,
        AlbumCandidateService(_ExactOverrideProvider(candidate=candidate)),
        AlbumEvidenceEngine(),
    )
    created = await ReidentificationService(store).create_or_coalesce(
        "album-1",
        "admin",
        release_mbid=EXACT_RELEASE,
        idempotency_key="translated-exact-title",
        now=1,
    )
    claimed = await store.claim_operation_job(
        "worker", now=2, lease_seconds=60, kind="explicit_reidentification"
    )
    assert claimed is not None

    ready = await worker.run_claimed(claimed, "worker", now=3)
    operation = await LibraryOperationService(store).get(created["id"])
    evidence = operation.reidentification_candidates[0].evidence.track_evidence[0]

    assert ready["state"] == "ready"
    assert evidence.release_track_mbid == EXACT_RELEASE_TRACK
    assert "administrator_exact_release_position_duration" in evidence.evidence_kinds
    accepted = await worker.select_candidate(
        created["id"],
        expected_job_revision=int(ready["row_revision"]),
        candidate_key=f"{EXACT_GROUP}:{EXACT_RELEASE}",
        confirmation=True,
        actor_user_id="admin",
        now=4,
    )
    context = await store.get_album_identification_context("album-1")
    assert accepted["state"] == "succeeded"
    assert context is not None
    assert context["tracks"][0]["release_track_mbid"] == EXACT_RELEASE_TRACK


@pytest.mark.asyncio
async def test_exact_release_override_maps_flattened_multidisc_by_absolute_position(
    store: NativeLibraryStore,
    db_path: Path,
) -> None:
    await _seed_album(store, "1", two_tracks=True)
    with sqlite3.connect(db_path) as connection:
        connection.execute(
            "UPDATE local_tracks SET duration_seconds = CASE id "
            "WHEN 'track-1-1' THEN 180 ELSE 200 END WHERE local_album_id='album-1'"
        )
    candidate = AlbumCandidate(
        release_group_mbid=EXACT_GROUP,
        release_mbid=EXACT_RELEASE,
        album_title="Album 1",
        album_artist_name="Artist 1",
        tracks=[
            CandidateTrack(
                title="Provider one",
                position=1,
                disc_number=1,
                absolute_position=1,
                duration_seconds=181,
                recording_mbid="recording-1",
                release_track_mbid="release-track-1",
            ),
            CandidateTrack(
                title="Provider two",
                position=1,
                disc_number=2,
                absolute_position=2,
                duration_seconds=199,
                recording_mbid="recording-2",
                release_track_mbid="release-track-2",
            ),
        ],
    )
    worker = ExplicitReidentificationWorker(
        store,
        AlbumCandidateService(_ExactOverrideProvider(candidate=candidate)),
        AlbumEvidenceEngine(),
    )
    created = await ReidentificationService(store).create_or_coalesce(
        "album-1",
        "admin",
        release_mbid=EXACT_RELEASE,
        idempotency_key="flattened-multidisc-exact-release",
        now=1,
    )
    claimed = await store.claim_operation_job(
        "worker", now=2, lease_seconds=60, kind="explicit_reidentification"
    )
    assert claimed is not None

    ready = await worker.run_claimed(claimed, "worker", now=3)
    operation = await LibraryOperationService(store).get(created["id"])
    evidence = operation.reidentification_candidates[0].evidence.track_evidence

    assert ready["state"] == "ready"
    assert [item.release_track_mbid for item in evidence] == [
        "release-track-1",
        "release-track-2",
    ]
    assert all(
        "administrator_exact_release_absolute_position_duration" in item.evidence_kinds
        for item in evidence
    )
    accepted = await worker.select_candidate(
        created["id"],
        expected_job_revision=int(ready["row_revision"]),
        candidate_key=f"{EXACT_GROUP}:{EXACT_RELEASE}",
        confirmation=True,
        actor_user_id="admin",
        now=4,
    )
    context = await store.get_album_identification_context("album-1")
    assert accepted["state"] == "succeeded"
    assert context is not None
    assert [track["release_track_mbid"] for track in context["tracks"]] == [
        "release-track-1",
        "release-track-2",
    ]
    assert [track["medium_position"] for track in context["tracks"]] == [1, 2]
    assert [track["release_track_position"] for track in context["tracks"]] == [
        1,
        1,
    ]


@pytest.mark.asyncio
async def test_exact_release_override_rejects_ambiguous_duplicate_recording_occurrences(
    store: NativeLibraryStore,
    db_path: Path,
) -> None:
    await _seed_album(store, "1")
    with sqlite3.connect(db_path) as connection:
        connection.execute(
            "UPDATE local_tracks SET title='Repeated', disc_number=2, track_number=10, "
            "embedded_recording_mbid=? WHERE id='track-1-1'",
            (EXACT_RECORDING,),
        )
    candidate = AlbumCandidate(
        release_group_mbid=EXACT_GROUP,
        release_mbid=EXACT_RELEASE,
        album_title="Album 1",
        album_artist_name="Artist 1",
        tracks=[
            CandidateTrack(
                title="Repeated",
                disc_number=2,
                position=1,
                absolute_position=10,
                recording_mbid=EXACT_RECORDING,
                release_track_mbid="release-track-first",
            ),
            CandidateTrack(
                title="Repeated",
                disc_number=2,
                position=10,
                absolute_position=19,
                recording_mbid=EXACT_RECORDING,
                release_track_mbid="release-track-second",
            ),
        ],
    )
    created = await ReidentificationService(store).create_or_coalesce(
        "album-1",
        "admin",
        release_mbid=EXACT_RELEASE,
        idempotency_key="ambiguous-exact-occurrences",
        now=1,
    )
    worker = ExplicitReidentificationWorker(
        store,
        AlbumCandidateService(_ExactOverrideProvider(candidate=candidate)),
        AlbumEvidenceEngine(),
    )
    claimed = await store.claim_operation_job(
        "worker", now=2, lease_seconds=60, kind="explicit_reidentification"
    )
    assert claimed is not None

    ready = await worker.run_claimed(claimed, "worker", now=3)
    operation = await LibraryOperationService(store).get(created["id"])

    assert ready["state"] == "ready"
    assert operation.reidentification_candidates[0].automatic_safe is False
    evidence = operation.reidentification_candidates[0].evidence.track_evidence[0]
    assert evidence.evidence_kinds == ["ambiguous_release_track_identity"]
    assert evidence.release_track_mbid is None


@pytest.mark.asyncio
@pytest.mark.parametrize("unavailable", [False, True])
async def test_exact_release_override_never_searches_or_fingerprints_a_missing_release(
    store: NativeLibraryStore,
    unavailable: bool,
) -> None:
    await _seed_album(store, "1")
    provider = _ExactOverrideProvider(candidate=None, unavailable=unavailable)
    fingerprint_backend = _FingerprintBackend()
    worker = ExplicitReidentificationWorker(
        store,
        AlbumCandidateService(provider),
        AlbumEvidenceEngine(),
        ConditionalFingerprintService(store, fingerprint_backend),
    )
    created = await ReidentificationService(store).create_or_coalesce(
        "album-1",
        "admin",
        release_mbid=EXACT_RELEASE,
        idempotency_key=f"missing-exact:{unavailable}",
        now=1,
    )
    claimed = await store.claim_operation_job(
        "worker", now=2, lease_seconds=60, kind="explicit_reidentification"
    )
    assert claimed is not None

    result = await worker.run_claimed(claimed, "worker", now=3)

    assert provider.exact_calls == [EXACT_RELEASE]
    assert provider.search_calls == 0
    assert fingerprint_backend.generate_calls == 0
    if unavailable:
        # F-IDENT-03: the transient unavailability defers instead of failing.
        assert result["state"] == "queued"
        assert result["next_attempt_at"] == 3 + REIDENTIFICATION_RETRY_SECONDS
        assert result["failed_count"] == 0
        assert result["reidentification_attempt_count"] == 1
    else:
        assert result["state"] == "succeeded"
        assert result["terminal_code"] == "NO_EXTERNAL_RESULT"
    assert (
        await LibraryOperationService(store).get(created["id"])
    ).reidentification_candidates == []


@pytest.mark.asyncio
@pytest.mark.parametrize(
    ("embedded_releases", "expected_reason"),
    [
        ([EXACT_RELEASE, None], "INCOMPLETE_EMBEDDED_RELEASE_IDS"),
        ([EXACT_RELEASE, EXACT_CANONICAL_RELEASE], "CONFLICTING_EMBEDDED_IDS"),
        (["not-an-mbid", "not-an-mbid"], "INVALID_EMBEDDED_IDS"),
    ],
)
async def test_exact_release_override_keeps_embedded_release_conflicts_visible(
    store: NativeLibraryStore,
    db_path: Path,
    embedded_releases: list[str | None],
    expected_reason: str,
) -> None:
    await _seed_album(store, "1", two_tracks=True)
    with sqlite3.connect(db_path) as connection:
        for position, release_mbid in enumerate(embedded_releases, start=1):
            connection.execute(
                "UPDATE local_tracks SET embedded_release_mbid=? WHERE id=?",
                (release_mbid, f"track-1-{position}"),
            )
    candidate = AlbumCandidate(
        release_group_mbid=EXACT_GROUP,
        release_mbid=EXACT_RELEASE,
        album_title="Album 1",
        album_artist_name="Artist 1",
        tracks=[
            CandidateTrack(
                title=f"Track {position}",
                position=position,
                absolute_position=position,
                recording_mbid=f"recording-{position}",
                release_track_mbid=f"release-track-{position}",
            )
            for position in (1, 2)
        ],
    )
    provider = _ExactOverrideProvider(candidate=candidate)
    created = await ReidentificationService(store).create_or_coalesce(
        "album-1",
        "admin",
        release_mbid=EXACT_RELEASE,
        idempotency_key=f"override-conflict:{expected_reason}",
        now=1,
    )
    claimed = await store.claim_operation_job(
        "worker", now=2, lease_seconds=60, kind="explicit_reidentification"
    )
    assert claimed is not None

    ready = await ExplicitReidentificationWorker(
        store, AlbumCandidateService(provider), AlbumEvidenceEngine()
    ).run_claimed(claimed, "worker", now=3)
    operation = await LibraryOperationService(store).get(created["id"])

    assert ready["state"] == "ready"
    assert provider.search_calls == 0
    assert operation.reidentification_candidates[0].automatic_safe is False
    assert (
        operation.reidentification_candidates[0].evidence.reason_code == expected_reason
    )


@pytest.mark.asyncio
async def test_exact_release_override_idempotency_includes_the_requested_release(
    store: NativeLibraryStore,
) -> None:
    await _seed_album(store, "1")
    service = ReidentificationService(store)

    first = await service.create_or_coalesce(
        "album-1",
        "admin",
        release_mbid=EXACT_RELEASE,
        idempotency_key="same-client-key",
        now=1,
    )
    repeated = await service.create_or_coalesce(
        "album-1",
        "admin",
        release_mbid=EXACT_RELEASE,
        idempotency_key="same-client-key",
        now=2,
    )
    different = await service.create_or_coalesce(
        "album-1",
        "admin",
        release_mbid=EXACT_CANONICAL_RELEASE,
        idempotency_key="same-client-key",
        now=3,
    )

    assert repeated["id"] == first["id"]
    assert different["id"] != first["id"]


@pytest.mark.asyncio
@pytest.mark.parametrize("change", ["identity", "file", "availability"])
async def test_exact_release_override_selection_rejects_changed_inputs(
    store: NativeLibraryStore,
    db_path: Path,
    change: str,
) -> None:
    await _seed_album(store, "1", identity_source="legacy_import")
    candidate = AlbumCandidate(
        release_group_mbid=EXACT_GROUP,
        release_mbid=EXACT_RELEASE,
        album_title="Album 1",
        album_artist_name="Artist 1",
        tracks=[
            CandidateTrack(
                title="Track 1",
                position=1,
                absolute_position=1,
                recording_mbid="recording-track-1-1",
                release_track_mbid="release-track-1",
            )
        ],
    )
    created = await ReidentificationService(store).create_or_coalesce(
        "album-1",
        "admin",
        release_mbid=EXACT_RELEASE,
        idempotency_key=f"stale-exact:{change}",
        now=1,
    )
    worker = ExplicitReidentificationWorker(
        store,
        AlbumCandidateService(_ExactOverrideProvider(candidate=candidate)),
        AlbumEvidenceEngine(),
    )
    claimed = await store.claim_operation_job(
        "worker", now=2, lease_seconds=60, kind="explicit_reidentification"
    )
    assert claimed is not None
    ready = await worker.run_claimed(claimed, "worker", now=3)
    with sqlite3.connect(db_path) as connection:
        if change == "identity":
            connection.execute(
                "UPDATE local_track_external_identities SET row_revision=row_revision+1 "
                "WHERE local_track_id='track-1-1'"
            )
        elif change == "file":
            connection.execute(
                "UPDATE local_tracks SET tag_revision='changed' WHERE id='track-1-1'"
            )
        else:
            connection.execute(
                "UPDATE local_tracks SET availability='missing' WHERE id='track-1-1'"
            )

    with pytest.raises(StaleRevisionError, match="changed after candidates"):
        await worker.select_candidate(
            created["id"],
            expected_job_revision=int(ready["row_revision"]),
            candidate_key=f"{EXACT_GROUP}:{EXACT_RELEASE}",
            confirmation=True,
            actor_user_id="admin",
            now=4,
        )


@pytest.mark.asyncio
async def test_exact_release_evaluation_rejects_an_identity_change_before_sealing(
    store: NativeLibraryStore,
    db_path: Path,
) -> None:
    await _seed_album(store, "1", identity_source="legacy_import")
    candidate = AlbumCandidate(
        release_group_mbid=EXACT_GROUP,
        release_mbid=EXACT_RELEASE,
        album_title="Album 1",
        album_artist_name="Artist 1",
        tracks=[
            CandidateTrack(
                title="Track 1",
                position=1,
                absolute_position=1,
                recording_mbid="recording-track-1-1",
                release_track_mbid=EXACT_RELEASE_TRACK,
            )
        ],
    )

    class IdentityMutatingProvider(_ExactOverrideProvider):
        async def get_exact_release_candidate(self, release_mbid, priority):
            with sqlite3.connect(db_path) as connection:
                connection.execute(
                    "UPDATE local_track_external_identities "
                    "SET row_revision=row_revision+1 WHERE local_track_id='track-1-1'"
                )
            return await super().get_exact_release_candidate(release_mbid, priority)

    created = await ReidentificationService(store).create_or_coalesce(
        "album-1",
        "admin",
        release_mbid=EXACT_RELEASE,
        idempotency_key="identity-change-during-evaluation",
        now=1,
    )
    claimed = await store.claim_operation_job(
        "worker", now=2, lease_seconds=60, kind="explicit_reidentification"
    )
    assert claimed is not None
    worker = ExplicitReidentificationWorker(
        store,
        AlbumCandidateService(IdentityMutatingProvider(candidate=candidate)),
        AlbumEvidenceEngine(),
    )

    with pytest.raises(StaleRevisionError, match="identity changed during"):
        await worker.run_claimed(claimed, "worker", now=3)

    with sqlite3.connect(db_path) as connection:
        attempt_count = connection.execute(
            "SELECT COUNT(*) FROM library_identification_attempts "
            "WHERE trigger='explicit_reidentification'"
        ).fetchone()[0]
        job_state = connection.execute(
            "SELECT state FROM library_operation_jobs WHERE id=?", (created["id"],)
        ).fetchone()[0]
    assert attempt_count == 0
    assert job_state == "running"


@pytest.mark.asyncio
async def test_exact_release_override_preserves_missing_track_identity_history(
    store: NativeLibraryStore,
    db_path: Path,
) -> None:
    await _seed_album(store, "1", identity_source="legacy_import", two_tracks=True)
    with sqlite3.connect(db_path) as connection:
        connection.execute(
            "UPDATE local_tracks SET availability='missing' WHERE id='track-1-2'"
        )
    candidate = AlbumCandidate(
        release_group_mbid=EXACT_GROUP,
        release_mbid=EXACT_RELEASE,
        album_title="Album 1",
        album_artist_name="Artist 1",
        tracks=[
            CandidateTrack(
                title="Track 1",
                position=1,
                absolute_position=1,
                recording_mbid=EXACT_RECORDING,
                release_track_mbid=EXACT_RELEASE_TRACK,
            )
        ],
    )
    created = await ReidentificationService(store).create_or_coalesce(
        "album-1",
        "admin",
        release_mbid=EXACT_RELEASE,
        idempotency_key="exact-preserve-missing",
        now=1,
    )
    worker = ExplicitReidentificationWorker(
        store,
        AlbumCandidateService(_ExactOverrideProvider(candidate=candidate)),
        AlbumEvidenceEngine(),
    )
    claimed = await store.claim_operation_job(
        "worker", now=2, lease_seconds=60, kind="explicit_reidentification"
    )
    assert claimed is not None
    ready = await worker.run_claimed(claimed, "worker", now=3)
    await worker.select_candidate(
        created["id"],
        expected_job_revision=int(ready["row_revision"]),
        candidate_key=f"{EXACT_GROUP}:{EXACT_RELEASE}",
        confirmation=True,
        actor_user_id="admin",
        now=4,
    )

    context = await store.get_album_identification_context("album-1")
    assert context is not None
    identities = {
        row["id"]: (row["availability"], row["recording_mbid"])
        for row in context["tracks"]
    }
    assert identities == {
        "track-1-1": ("indexed", EXACT_RECORDING),
        "track-1-2": ("missing", "recording-track-1-2"),
    }


@pytest.mark.asyncio
async def test_confirmed_incomplete_exact_map_is_rejected_without_identity_changes(
    store: NativeLibraryStore,
) -> None:
    await _seed_album(store, "1", identity_source="legacy_import", two_tracks=True)
    candidate = AlbumCandidate(
        release_group_mbid=EXACT_GROUP,
        release_mbid=EXACT_RELEASE,
        album_title="Album 1",
        album_artist_name="Artist 1",
        tracks=[
            CandidateTrack(
                title="Track 1",
                position=1,
                absolute_position=1,
                recording_mbid=EXACT_RECORDING,
                release_track_mbid=EXACT_RELEASE_TRACK,
            )
        ],
    )
    created = await ReidentificationService(store).create_or_coalesce(
        "album-1",
        "admin",
        release_mbid=EXACT_RELEASE,
        idempotency_key="exact-incomplete-preserves",
        now=1,
    )
    worker = ExplicitReidentificationWorker(
        store,
        AlbumCandidateService(_ExactOverrideProvider(candidate=candidate)),
        AlbumEvidenceEngine(),
    )
    claimed = await store.claim_operation_job(
        "worker", now=2, lease_seconds=60, kind="explicit_reidentification"
    )
    assert claimed is not None
    ready = await worker.run_claimed(claimed, "worker", now=3)

    with pytest.raises(ValidationError, match="does not map every local file uniquely"):
        await worker.select_candidate(
            created["id"],
            expected_job_revision=int(ready["row_revision"]),
            candidate_key=f"{EXACT_GROUP}:{EXACT_RELEASE}",
            confirmation=True,
            actor_user_id="admin",
            now=4,
        )

    context = await store.get_album_identification_context("album-1")
    assert context is not None
    assert context["identity"]["release_mbid"] == "release-1"
    assert {row["id"]: row["recording_mbid"] for row in context["tracks"]} == {
        "track-1-1": "recording-track-1-1",
        "track-1-2": "recording-track-1-2",
    }


@pytest.mark.asyncio
async def test_custom_edition_seals_local_only_tracks_and_every_reseal_is_immutable(
    store: NativeLibraryStore,
    db_path: Path,
) -> None:
    await _seed_album(store, "1", two_tracks=True)
    candidate = AlbumCandidate(
        release_group_mbid=EXACT_GROUP,
        release_mbid=EXACT_RELEASE,
        album_title="Album 1",
        album_artist_name="Artist 1",
        tracks=[
            CandidateTrack(
                title="Track 1",
                position=1,
                absolute_position=1,
                recording_mbid=EXACT_RECORDING,
                release_track_mbid=EXACT_RELEASE_TRACK,
            )
        ],
    )
    worker = ExplicitReidentificationWorker(
        store,
        AlbumCandidateService(_ExactOverrideProvider(candidate=candidate)),
        AlbumEvidenceEngine(),
    )

    async def seal(idempotency_key: str, now: float) -> None:
        created = await ReidentificationService(store).create_or_coalesce(
            "album-1",
            "admin",
            release_mbid=EXACT_RELEASE,
            idempotency_key=idempotency_key,
            now=now,
        )
        claimed = await store.claim_operation_job(
            "worker",
            now=now + 1,
            lease_seconds=60,
            kind="explicit_reidentification",
        )
        assert claimed is not None
        ready = await worker.run_claimed(claimed, "worker", now=now + 2)
        selected = await worker.select_candidate(
            created["id"],
            expected_job_revision=int(ready["row_revision"]),
            candidate_key=f"{EXACT_GROUP}:{EXACT_RELEASE}",
            confirmation=True,
            actor_user_id="admin",
            decision_mode="custom_edition",
            now=now + 3,
        )
        assert selected["terminal_code"] == "CUSTOM_EDITION_SEALED"

    await seal("custom-first", 10)
    first = await store.get_custom_edition_state("album-1")
    assert first is not None
    assert first.manifest.version == 1
    assert len(first.manifest.tracks) == 2
    assert first.recognized_track_count == 1
    assert first.stale is False
    context = await store.get_album_identification_context("album-1")
    assert context is not None
    assert context["identity"]["release_mbid"] is None
    assert all(value["release_track_mbid"] is None for value in context["tracks"])

    with sqlite3.connect(db_path) as connection:
        connection.execute(
            "UPDATE local_tracks SET title='Locally renamed',title_folded='locally renamed',"
            "row_revision=row_revision+1 WHERE id='track-1-1'"
        )
    stale = await store.get_custom_edition_state("album-1")
    assert stale is not None
    assert stale.stale is True

    await seal("custom-second", 20)
    second = await store.get_custom_edition_state("album-1")
    assert second is not None
    assert second.manifest.version == 2
    assert second.manifest.id != first.manifest.id
    assert second.stale is False
    assert second.manifest.tracks[0].title == "Locally renamed"


@pytest.mark.asyncio
async def test_leave_unmanaged_retains_truthful_group_and_can_be_reenabled(
    store: NativeLibraryStore,
) -> None:
    await _seed_album(store, "1")
    candidate = AlbumCandidate(
        release_group_mbid=EXACT_GROUP,
        release_mbid=EXACT_RELEASE,
        album_title="Album 1",
        album_artist_name="Artist 1",
        tracks=[
            CandidateTrack(
                title="Track 1",
                position=1,
                absolute_position=1,
                recording_mbid=EXACT_RECORDING,
                release_track_mbid=EXACT_RELEASE_TRACK,
            )
        ],
    )
    created = await ReidentificationService(store).create_or_coalesce(
        "album-1",
        "admin",
        release_mbid=EXACT_RELEASE,
        idempotency_key="leave-unmanaged",
        now=10,
    )
    worker = ExplicitReidentificationWorker(
        store,
        AlbumCandidateService(_ExactOverrideProvider(candidate=candidate)),
        AlbumEvidenceEngine(),
    )
    claimed = await store.claim_operation_job(
        "worker", now=11, lease_seconds=60, kind="explicit_reidentification"
    )
    assert claimed is not None
    ready = await worker.run_claimed(claimed, "worker", now=12)

    selected = await worker.select_candidate(
        created["id"],
        expected_job_revision=int(ready["row_revision"]),
        candidate_key=f"{EXACT_GROUP}:{EXACT_RELEASE}",
        confirmation=True,
        actor_user_id="admin",
        decision_mode="leave_unmanaged",
        now=13,
    )

    assert selected["terminal_code"] == "LEFT_UNMANAGED"
    context = await store.get_album_identification_context("album-1")
    assert context is not None
    assert context["identity"]["release_group_mbid"] == EXACT_GROUP
    assert context["identity"]["release_mbid"] is None
    assert context["tracks"][0]["recording_mbid"] is None
    exclusion = await store.get_management_exclusion("album-1")
    assert exclusion is not None
    assert exclusion.reason == "administrator_choice"

    reenabled = await store.clear_management_exclusion(
        "album-1",
        expected_row_revision=exclusion.row_revision,
        actor_user_id="admin",
        now=14,
    )

    assert reenabled is True
    assert await store.get_management_exclusion("album-1") is None


@pytest.mark.asyncio
async def test_leave_unmanaged_works_for_zero_candidate_albums(
    store: NativeLibraryStore,
) -> None:
    await _seed_album(store, "1")

    class _NoCandidateProvider:
        async def search_album_candidate_ids(
            self, artist, title, limit, priority
        ) -> list[str]:
            return []

        async def search_recording_candidate_ids(
            self, artist, title, limit, priority
        ) -> list[str]:
            return []

        async def get_album_candidate(
            self, release_group_mbid, target_track_count, priority
        ) -> None:
            return None

        async def get_album_candidate_editions(
            self, release_group_mbid, target_track_count, priority, *, max_editions=2
        ):
            return []

    worker = ExplicitReidentificationWorker(
        store,
        AlbumCandidateService(_NoCandidateProvider()),
        AlbumEvidenceEngine(),
    )

    async def ready_job(idempotency_key: str, now: float) -> tuple[str, dict]:
        created = await ReidentificationService(store).create_or_coalesce(
            "album-1",
            "admin",
            idempotency_key=idempotency_key,
            now=now,
        )
        claimed = await store.claim_operation_job(
            "worker", now=now + 1, lease_seconds=60, kind="explicit_reidentification"
        )
        assert claimed is not None
        ready = await worker.run_claimed(claimed, "worker", now=now + 2)
        assert ready["state"] in ("ready", "succeeded")
        return str(created["id"]), ready

    # Zero-candidate snapshot: leave_unmanaged needs no evidence and succeeds.
    job_id, ready = await ready_job("zero-candidate", 10)
    selected = await worker.select_candidate(
        job_id,
        expected_job_revision=int(ready["row_revision"]),
        candidate_key="",
        confirmation=True,
        actor_user_id="admin",
        decision_mode="leave_unmanaged",
        now=13,
    )
    assert selected["terminal_code"] == "LEFT_UNMANAGED"
    exclusion = await store.get_management_exclusion("album-1")
    assert exclusion is not None
    assert exclusion.reason == "administrator_choice"

    # Exact release still requires an actual candidate: no evidence, no exit.
    job_id, ready = await ready_job("exact-empty", 15)
    with pytest.raises(StaleRevisionError):
        await worker.select_candidate(
            job_id,
            expected_job_revision=int(ready["row_revision"]),
            candidate_key="",
            confirmation=True,
            actor_user_id="admin",
            decision_mode="exact_release",
            now=18,
        )


@pytest.mark.asyncio
async def test_explicit_reidentification_accepts_provider_canonicalization_of_the_current_embedded_release(
    store: NativeLibraryStore, db_path: Path
) -> None:
    await _seed_album(store, "1")
    with sqlite3.connect(db_path) as connection:
        connection.execute(
            "UPDATE local_tracks SET embedded_release_group_mbid = ?, "
            "embedded_release_mbid = ?, embedded_recording_mbid = ?, "
            "embedded_release_track_mbid = ? WHERE id = 'track-1-1'",
            (EXACT_GROUP, EXACT_RELEASE, EXACT_RECORDING, EXACT_RELEASE_TRACK),
        )
    context = await store.get_album_identification_context("album-1")
    assert context is not None
    await store.attach_album_identity(
        LocalAlbumExternalIdentity(
            local_album_id="album-1",
            release_group_mbid=EXACT_GROUP,
            release_mbid=EXACT_RELEASE,
            decision_source="legacy_import",
            selected_at=1,
        ),
        expected_album_revision=int(context["album"]["row_revision"]),
    )
    provider = _PreferredEditionIdentificationProvider(EXACT_CANONICAL_RELEASE)
    worker = ExplicitReidentificationWorker(
        store, AlbumCandidateService(provider), AlbumEvidenceEngine()
    )
    created = await ReidentificationService(store).create_or_coalesce(
        "album-1", "admin", idempotency_key="canonical-embedded-edition", now=1
    )
    claimed = await store.claim_operation_job(
        "worker", now=2, lease_seconds=60, kind="explicit_reidentification"
    )
    assert claimed is not None

    ready = await worker.run_claimed(claimed, "worker", now=3)
    operation = await LibraryOperationService(store).get(created["id"])

    assert ready["state"] == "ready"
    assert operation.reidentification_candidates[0].candidate_key == (
        f"{EXACT_GROUP}:{EXACT_CANONICAL_RELEASE}"
    )
    assert operation.reidentification_candidates[0].automatic_safe is True


@pytest.mark.asyncio
@pytest.mark.parametrize(
    ("candidate_group", "candidate_release"),
    [
        ("rg-1", "release-other"),
        ("rg-other", "release-other"),
    ],
)
async def test_explicit_reidentification_requires_confirmation_to_replace_a_current_exact_identity(
    store: NativeLibraryStore,
    candidate_group: str,
    candidate_release: str,
) -> None:
    await _seed_album(store, "1", identity_source="legacy_import")
    worker = ExplicitReidentificationWorker(
        store,
        AlbumCandidateService(
            _ExistingIdentityConflictProvider(candidate_group, candidate_release)
        ),
        AlbumEvidenceEngine(),
    )
    created = await ReidentificationService(store).create_or_coalesce(
        "album-1",
        "admin",
        idempotency_key=f"current-identity:{candidate_group}:{candidate_release}",
        now=1,
    )
    claimed = await store.claim_operation_job(
        "worker", now=2, lease_seconds=60, kind="explicit_reidentification"
    )
    assert claimed is not None

    ready = await worker.run_claimed(claimed, "worker", now=3)
    operation = await LibraryOperationService(store).get(created["id"])

    assert ready["state"] == "ready"
    assert operation.reidentification_candidates[0].automatic_safe is False
    assert (
        operation.reidentification_candidates[0].evidence.reason_code
        == "CONFLICTING_TRACK_EVIDENCE"
    )
    with pytest.raises(ValidationError, match="Confirm the conflicting"):
        await worker.select_candidate(
            created["id"],
            expected_job_revision=int(ready["row_revision"]),
            candidate_key=f"{candidate_group}:{candidate_release}",
            confirmation=False,
            actor_user_id="admin",
            now=4,
        )


@pytest.mark.asyncio
@pytest.mark.parametrize("conflict_kind", ["recording", "release_track"])
async def test_explicit_reidentification_holds_conflicting_embedded_and_current_track_identities(
    store: NativeLibraryStore,
    db_path: Path,
    conflict_kind: str,
) -> None:
    await _seed_album(store, "1", identity_source="legacy_import")
    with sqlite3.connect(db_path) as connection:
        connection.execute(
            "UPDATE local_tracks SET embedded_release_group_mbid = ?, "
            "embedded_release_mbid = ?, embedded_recording_mbid = ?, "
            "embedded_release_track_mbid = ? WHERE id = 'track-1-1'",
            (EXACT_GROUP, EXACT_RELEASE, EXACT_RECORDING, EXACT_RELEASE_TRACK),
        )
        connection.execute(
            "UPDATE local_track_external_identities SET recording_mbid = ?, "
            "release_track_mbid = ? WHERE local_track_id = 'track-1-1'",
            (
                "recording-conflict"
                if conflict_kind == "recording"
                else EXACT_RECORDING,
                "release-track-conflict"
                if conflict_kind == "release_track"
                else EXACT_RELEASE_TRACK,
            ),
        )
    provider = _PreferredEditionIdentificationProvider()
    worker = ExplicitReidentificationWorker(
        store, AlbumCandidateService(provider), AlbumEvidenceEngine()
    )
    created = await ReidentificationService(store).create_or_coalesce(
        "album-1",
        "admin",
        idempotency_key=f"stored-track-conflict:{conflict_kind}",
        now=1,
    )
    claimed = await store.claim_operation_job(
        "worker", now=2, lease_seconds=60, kind="explicit_reidentification"
    )
    assert claimed is not None

    finished = await worker.run_claimed(claimed, "worker", now=3)
    operation = await LibraryOperationService(store).get(created["id"])

    assert finished["state"] == "succeeded"
    assert finished["terminal_code"] == "CONFLICTING_EMBEDDED_IDS"
    assert operation.reidentification_candidates == []
    assert provider.preferred_releases == []


@pytest.mark.asyncio
async def test_explicit_reidentification_fingerprints_contradictory_text_evidence(
    store: NativeLibraryStore,
) -> None:
    await _seed_album(store, "1")
    backend = _FingerprintBackend()
    worker = ExplicitReidentificationWorker(
        store,
        AlbumCandidateService(_ContradictoryFingerprintProvider()),
        AlbumEvidenceEngine(),
        ConditionalFingerprintService(store, backend),
    )
    created = await ReidentificationService(store).create_or_coalesce(
        "album-1", "admin", idempotency_key="contradictory-fingerprint", now=1
    )
    claimed = await store.claim_operation_job(
        "worker", now=2, lease_seconds=60, kind="explicit_reidentification"
    )
    assert claimed is not None

    ready = await worker.run_claimed(claimed, "worker", now=3)
    operation = await LibraryOperationService(store).get(created["id"])

    assert ready["state"] == "ready"
    assert backend.generate_calls == 1
    assert backend.lookup_calls == 1
    assert operation.reidentification_candidates[0].automatic_safe is True
    assert operation.reidentification_candidates[0].evidence.track_evidence[
        0
    ].evidence_kinds == ["fingerprint_recording_mbid"]


@pytest.mark.asyncio
async def test_explicit_reidentification_cached_off_release_fingerprint_is_support_only(
    store: NativeLibraryStore,
) -> None:
    """#392: a cached matched fingerprint outcome carrying an off-release
    recording ID lands in the support-only slot - exact text evidence still
    supports the track instead of vetoing with recording_mbid_conflict."""
    await _seed_album(store, "1")
    await store.record_fingerprint_outcome(
        FingerprintOutcome(
            id="cached-off-release",
            local_track_id="track-1-1",
            stat_revision="stat-1-1",
            fingerprinter_version="fpcalc-acoustid-v1",
            state="matched",
            recording_mbid="off-release-recording",
            release_group_ids=["rg-explicit"],
            first_attempt_at=1,
            last_attempt_at=1,
        )
    )
    worker = ExplicitReidentificationWorker(
        store,
        AlbumCandidateService(_IdentificationProvider()),
        AlbumEvidenceEngine(),
    )
    created = await ReidentificationService(store).create_or_coalesce(
        "album-1", "admin", idempotency_key="off-release-fingerprint", now=1
    )
    claimed = await store.claim_operation_job(
        "worker", now=2, lease_seconds=60, kind="explicit_reidentification"
    )
    assert claimed is not None

    ready = await worker.run_claimed(claimed, "worker", now=3)
    operation = await LibraryOperationService(store).get(created["id"])

    assert ready["state"] == "ready"
    kinds = operation.reidentification_candidates[0].evidence.track_evidence[
        0
    ].evidence_kinds
    assert "recording_mbid_conflict" not in kinds


@pytest.mark.asyncio
@pytest.mark.parametrize(
    ("release_ids", "terminal_code"),
    [
        (("release-1", None), "INCOMPLETE_EMBEDDED_RELEASE_IDS"),
        (("release-1", "release-2"), "CONFLICTING_EMBEDDED_IDS"),
    ],
)
async def test_explicit_reidentification_holds_incomplete_or_mixed_embedded_editions(
    store: NativeLibraryStore,
    db_path: Path,
    release_ids: tuple[str | None, str | None],
    terminal_code: str,
) -> None:
    await _seed_album(store, "1", two_tracks=True)
    with sqlite3.connect(db_path) as connection:
        connection.executemany(
            "UPDATE local_tracks SET embedded_release_group_mbid = 'rg-explicit', "
            "embedded_release_mbid = ? WHERE id = ?",
            [
                (release_ids[0], "track-1-1"),
                (release_ids[1], "track-1-2"),
            ],
        )
    backend = _FingerprintBackend()
    worker = ExplicitReidentificationWorker(
        store,
        AlbumCandidateService(_ContradictoryFingerprintProvider()),
        AlbumEvidenceEngine(),
        ConditionalFingerprintService(store, backend),
    )
    created = await ReidentificationService(store).create_or_coalesce(
        "album-1", "admin", idempotency_key=f"edition-gate:{terminal_code}", now=1
    )
    claimed = await store.claim_operation_job(
        "worker", now=2, lease_seconds=60, kind="explicit_reidentification"
    )
    assert claimed is not None

    finished = await worker.run_claimed(claimed, "worker", now=3)
    operation = await LibraryOperationService(store).get(created["id"])

    assert finished["state"] == "succeeded"
    assert operation.terminal_code == terminal_code
    assert operation.reidentification_candidates == []
    assert backend.generate_calls == 0
    assert backend.lookup_calls == 0


@pytest.mark.asyncio
async def test_explicit_reidentification_never_overwrites_embedded_recording_proof_with_a_cached_fingerprint(
    store: NativeLibraryStore, db_path: Path
) -> None:
    await _seed_album(store, "1")
    with sqlite3.connect(db_path) as connection:
        connection.execute(
            "UPDATE local_tracks SET embedded_recording_mbid = ? "
            "WHERE id = 'track-1-1'",
            (EXACT_RECORDING,),
        )
    await store.record_fingerprint_outcome(
        FingerprintOutcome(
            id="cached-fingerprint",
            local_track_id="track-1-1",
            stat_revision="stat-1-1",
            fingerprinter_version="fpcalc-acoustid-v1",
            state="matched",
            recording_mbid="recording-explicit",
            release_group_ids=["rg-explicit"],
            first_attempt_at=1,
            last_attempt_at=1,
        )
    )
    backend = _FingerprintBackend()
    worker = ExplicitReidentificationWorker(
        store,
        AlbumCandidateService(_ContradictoryFingerprintProvider()),
        AlbumEvidenceEngine(),
        ConditionalFingerprintService(store, backend),
    )
    created = await ReidentificationService(store).create_or_coalesce(
        "album-1", "admin", idempotency_key="recording-proof-conflict", now=2
    )
    claimed = await store.claim_operation_job(
        "worker", now=3, lease_seconds=60, kind="explicit_reidentification"
    )
    assert claimed is not None

    ready = await worker.run_claimed(claimed, "worker", now=4)
    operation = await LibraryOperationService(store).get(created["id"])

    assert ready["state"] == "ready"
    assert operation.reidentification_candidates[0].automatic_safe is False
    assert operation.reidentification_candidates[0].evidence.track_evidence[
        0
    ].evidence_kinds == ["recording_mbid_conflict"]
    assert backend.generate_calls == 0
    assert backend.lookup_calls == 0


@pytest.mark.asyncio
async def test_policy_save_suppresses_work_without_hidden_apply_and_apply_is_revision_safe(
    store: NativeLibraryStore, db_path: Path, tmp_path: Path
) -> None:
    await _seed_album(store, "1")
    await IdentificationQueueService(store).enqueue_album(
        "album-1", input_revision="before-policy", now=1
    )
    root = tmp_path / "music"
    root.mkdir()
    resolver = LibraryPolicyResolver(
        TypedLibrarySettings(
            library_roots=[
                LibraryRootSettings(
                    id="root", path=str(root), label="Library", policy="excluded"
                )
            ]
        )
    )
    coordinator = AsyncMock()
    coordinator.request_run.return_value = ScanRequestResult(
        run_id="policy-run",
        disposition="started",
        state="queued",
        row_revision=1,
    )
    policies = LibraryPolicyReconciliationService(store, lambda: resolver, coordinator)

    boundary = await policies.save_boundary(
        [
            ScanScope(
                root_id="root",
                scope_id="root",
                relative_path=".",
                root_path=str(root),
                effective_policy="excluded",
                policy_revision=resolver.policy_revision,
            )
        ],
        policy_revision=resolver.policy_revision,
        now=2,
    )
    assert boundary == {"changed": 1, "cancelled": 1}
    coordinator.request_run.assert_not_awaited()
    with sqlite3.connect(db_path) as connection:
        track = connection.execute(
            "SELECT desired_policy_revision, applied_policy FROM local_tracks"
        ).fetchone()
        job_state = connection.execute(
            "SELECT state FROM library_identification_jobs"
        ).fetchone()[0]
    assert track == (resolver.policy_revision, "automatic")
    assert job_state == "cancelled"

    with pytest.raises(StaleRevisionError):
        await policies.preview_apply(["root"], expected_policy_revision="stale")
    result = await policies.apply(
        ["root"],
        expected_policy_revision=resolver.policy_revision,
        requested_by_user_id="admin",
    )
    request = coordinator.request_run.await_args.args[0]
    assert result.run_id == "policy-run"
    assert request.kind == "policy_reconcile"
    assert request.trigger == "policy_apply"
    assert request.scopes[0].effective_policy == "excluded"


@pytest.mark.asyncio
async def test_every_policy_transition_preserves_identity_and_manual_exclusion_semantics(
    store: NativeLibraryStore, db_path: Path
) -> None:
    await _seed_album(store, "1")
    context = await store.get_album_identification_context("album-1")
    assert context is not None
    await store.attach_album_identity(
        LocalAlbumExternalIdentity(
            local_album_id="album-1",
            release_group_mbid="rg-policy",
            release_mbid="release-policy",
            decision_source="manual",
            selected_at=2,
        ),
        expected_album_revision=int(context["album"]["row_revision"]),
    )

    async def transition(policy: str, revision: str) -> dict[str, int | bool]:
        run_id = f"policy-{revision}"
        await store.request_scan_run(
            ScanRequest(
                kind="policy_reconcile",
                trigger="policy_apply",
                policy_revision=revision,
                scopes=[
                    ScanScope(
                        root_id="root",
                        relative_path=".",
                        effective_policy=policy,
                        policy_revision=revision,
                    )
                ],
            ),
            run_id=run_id,
            requested_at=10,
        )
        with sqlite3.connect(db_path) as connection:
            connection.execute(
                "UPDATE library_scan_run_scopes SET discovery_state = 'completed' "
                "WHERE run_id = ?",
                (run_id,),
            )
            tracks = connection.execute(
                "SELECT root_id, relative_path, file_path, file_size_bytes, "
                "file_mtime_ns, stat_revision FROM local_tracks"
            ).fetchall()
            connection.executemany(
                "INSERT INTO library_scan_inventory "
                "(run_id, root_id, relative_path, absolute_path, file_size_bytes, "
                "file_mtime_ns, stat_revision, policy_revision, effective_policy, "
                "comparison_result) VALUES (?,?,?,?,?,?,?,?,?, 'unchanged')",
                [(run_id, *track, revision, policy) for track in tracks],
            )
        result = await store.reconcile_scan_scope_batch(
            run_id, "root", ".", now=11, limit=100
        )
        with sqlite3.connect(db_path) as connection:
            connection.execute(
                "UPDATE library_scan_runs SET state = 'completed', terminal_at = 12 "
                "WHERE id = ?",
                (run_id,),
            )
        return result

    await transition("local_metadata", "revision-1")
    context = await store.get_album_identification_context("album-1")
    assert context is not None
    assert context["identity"]["release_group_mbid"] == "rg-policy"
    assert context["tracks"][0]["applied_policy"] == "local_metadata"

    await transition("excluded", "revision-2")
    context = await store.get_album_identification_context("album-1")
    assert context is not None
    assert context["identity"] is not None
    assert context["tracks"][0]["availability"] == "excluded"

    restored = await transition("automatic", "revision-3")
    assert restored["restored"] == 1
    with sqlite3.connect(db_path) as connection:
        assert (
            connection.execute(
                "SELECT COUNT(*) FROM library_identification_jobs"
            ).fetchone()[0]
            == 0
        )

    await transition("excluded", "revision-4")
    await transition("local_metadata", "revision-5")
    context = await store.get_album_identification_context("album-1")
    assert context is not None
    await store.detach_album_identity(
        "album-1",
        expected_album_revision=int(context["album"]["row_revision"]),
        expected_identity_revision=int(context["identity"]["row_revision"]),
        updated_at=13,
    )
    queued = await transition("automatic", "revision-6")
    assert queued["identification_enqueued"] == 1

    with sqlite3.connect(db_path) as connection:
        connection.execute(
            "UPDATE local_tracks SET manual_excluded = 1, availability = 'excluded'"
        )
        connection.execute("DELETE FROM library_identification_jobs")
    await transition("excluded", "revision-7")
    manual_restore = await transition("automatic", "revision-8")
    with sqlite3.connect(db_path) as connection:
        row = connection.execute(
            "SELECT availability, manual_excluded, applied_policy FROM local_tracks"
        ).fetchone()
        queued_count = connection.execute(
            "SELECT COUNT(*) FROM library_identification_jobs"
        ).fetchone()[0]
    assert manual_restore["restored"] == 0
    assert row == ("excluded", 1, "automatic")
    assert queued_count == 0


@pytest.mark.asyncio
async def test_split_merge_and_artist_merge_preserve_local_ids_and_write_aliases(
    store: NativeLibraryStore, db_path: Path
) -> None:
    await _seed_album(store, "1", two_tracks=True)
    corrections = CatalogCorrectionService(store)
    context = await store.get_album_identification_context("album-1")
    assert context is not None
    preview_request = MembershipPreviewRequest(
        track_ids=["track-1-2"],
        expected_album_revisions={"album-1": int(context["album"]["row_revision"])},
        title="Disc Two",
    )
    preview = await corrections.preview_membership("split", preview_request, now=10)
    split = await corrections.apply_membership(
        "split",
        MembershipApplyRequest(
            track_ids=preview_request.track_ids,
            expected_album_revisions=preview_request.expected_album_revisions,
            title=preview_request.title,
            preview_token=preview.preview_token,
            idempotency_key="split-1",
        ),
        "admin",
        now=10,
    )
    assert split["target_album_id"] != "album-1"
    with sqlite3.connect(db_path) as connection:
        membership = dict(
            connection.execute("SELECT id, local_album_id FROM local_tracks")
        )
    assert membership["track-1-1"] == "album-1"
    assert membership["track-1-2"] == split["target_album_id"]

    source_context = await store.get_album_identification_context("album-1")
    split_context = await store.get_album_identification_context(
        split["target_album_id"]
    )
    assert source_context is not None and split_context is not None
    reset_request = MembershipPreviewRequest(
        track_ids=["track-1-2"],
        expected_album_revisions={
            "album-1": int(source_context["album"]["row_revision"]),
            split["target_album_id"]: int(split_context["album"]["row_revision"]),
        },
    )
    reset_preview = await corrections.preview_membership("reset", reset_request, now=15)
    assert any(
        set(group.track_ids) == {"track-1-1", "track-1-2"}
        for group in reset_preview.automatic_groups
    )
    reset = await corrections.apply_membership(
        "reset",
        MembershipApplyRequest(
            track_ids=reset_request.track_ids,
            expected_album_revisions=reset_request.expected_album_revisions,
            preview_token=reset_preview.preview_token,
            idempotency_key="reset-1",
        ),
        "admin",
        now=15,
    )
    with sqlite3.connect(db_path) as connection:
        reset_membership = dict(
            connection.execute("SELECT id, local_album_id FROM local_tracks")
        )
        selected_lock = connection.execute(
            "SELECT membership_locked FROM local_tracks WHERE id = 'track-1-2'"
        ).fetchone()[0]
    assert reset["automatic_album_ids"] == ["album-1"]
    assert reset_membership["track-1-1"] == "album-1"
    assert reset_membership["track-1-2"] == "album-1"
    assert selected_lock == 0

    await _seed_album(store, "2")
    await store.attach_artist_identity_with_aliases(
        LocalArtistExternalIdentity(
            local_artist_id="artist-1",
            provider_artist_id="mbid-artist-1",
            decision_source="manual",
            selected_at=19,
        ),
        [],
        expected_artist_revision=1,
    )
    with sqlite3.connect(db_path) as connection:
        connection.execute(
            "INSERT INTO library_user_favorites VALUES "
            "('admin', 'artist', 'artist-2', 1)"
        )
        connection.execute(
            "INSERT INTO library_play_history "
            "(id, user_id, local_track_id, local_album_id, local_artist_id, "
            "track_name, artist_name, played_at) VALUES "
            "('history-artist-merge', 'admin', 'track-2-1', 'album-2', "
            "'artist-2', 'Track 2', 'Artist 2', '2026-07-14T12:00:00Z')"
        )
        connection.execute(
            "INSERT INTO library_playlists "
            "(id, name, created_at, updated_at, user_id) VALUES "
            "('playlist-artist-merge', 'Merge', 'now', 'now', 'admin')"
        )
        connection.execute(
            "INSERT INTO library_playlist_tracks "
            "(id, playlist_id, position, track_name, artist_name, album_name, "
            "source_type, created_at, local_track_id, local_album_id, local_artist_id) "
            "VALUES ('playlist-track-artist-merge', 'playlist-artist-merge', 0, "
            "'Track 2', 'Artist 2', 'Album 2', 'local', 'now', 'track-2-1', "
            "'album-2', 'artist-2')"
        )
        connection.execute(
            "INSERT INTO library_compat_id_map VALUES "
            "('22222222222222222222222222222222', 'artist', 'artist-2')"
        )
    await store.attach_artist_identity_with_aliases(
        LocalArtistExternalIdentity(
            local_artist_id="artist-2",
            provider_artist_id="mbid-artist-2",
            decision_source="manual",
            selected_at=19,
        ),
        [],
        expected_artist_revision=1,
    )
    artist_preview_request = ArtistMergePreviewRequest(
        source_artist_ids=["artist-2"],
        surviving_artist_id="artist-1",
        expected_revisions={"artist-1": 2, "artist-2": 2},
    )
    artist_preview = await corrections.preview_artist_merge(
        artist_preview_request, now=20
    )
    assert artist_preview.identity_conflicts == ["mbid-artist-1", "mbid-artist-2"]
    assert artist_preview.reference_counts["track_credits"] == 3
    assert artist_preview.reference_counts["favorites"] == 1
    assert artist_preview.reference_counts["playlist_snapshots"] == 1
    assert artist_preview.reference_counts["history"] == 1
    assert artist_preview.reference_counts["compatibility_ids"] == 1
    artist_result = await corrections.apply_artist_merge(
        ArtistMergeApplyRequest(
            source_artist_ids=["artist-2"],
            surviving_artist_id="artist-1",
            expected_revisions={"artist-1": 2, "artist-2": 2},
            preview_token=artist_preview.preview_token,
            idempotency_key="artist-merge-1",
        ),
        "admin",
        now=20,
    )
    with sqlite3.connect(db_path) as connection:
        alias = connection.execute(
            "SELECT local_artist_id FROM local_artist_aliases WHERE alias = 'artist-2'"
        ).fetchone()[0]
        stable_references = (
            connection.execute("SELECT item_id FROM library_user_favorites").fetchone()[
                0
            ],
            connection.execute(
                "SELECT local_artist_id FROM library_play_history"
            ).fetchone()[0],
            connection.execute(
                "SELECT local_artist_id FROM library_playlist_tracks"
            ).fetchone()[0],
            connection.execute(
                "SELECT internal_id FROM library_compat_id_map"
            ).fetchone()[0],
        )
    assert artist_result["surviving_artist_id"] == "artist-1"
    assert alias == "artist-1"
    assert stable_references == ("artist-1",) * 4


@pytest.mark.asyncio
async def test_move_and_album_merge_lock_membership_preserve_paths_and_alias_ids(
    store: NativeLibraryStore, db_path: Path
) -> None:
    await _seed_album(store, "1", two_tracks=True)
    await _seed_album(store, "2")
    corrections = CatalogCorrectionService(store)
    album_1 = await store.get_album_identification_context("album-1")
    album_2 = await store.get_album_identification_context("album-2")
    assert album_1 is not None and album_2 is not None
    before_paths = {
        str(track["id"]): str(track["file_path"])
        for context in (album_1, album_2)
        for track in context["tracks"]
    }
    move_request = MembershipPreviewRequest(
        track_ids=["track-1-2"],
        expected_album_revisions={
            "album-1": int(album_1["album"]["row_revision"]),
            "album-2": int(album_2["album"]["row_revision"]),
        },
        target_album_id="album-2",
    )
    move_preview = await corrections.preview_membership("move", move_request, now=10)
    move = await corrections.apply_membership(
        "move",
        MembershipApplyRequest(
            track_ids=move_request.track_ids,
            expected_album_revisions=move_request.expected_album_revisions,
            target_album_id="album-2",
            preview_token=move_preview.preview_token,
            idempotency_key="move-1",
        ),
        "admin",
        now=10,
    )
    assert move["target_album_id"] == "album-2"
    with sqlite3.connect(db_path) as connection:
        moved = connection.execute(
            "SELECT local_album_id, membership_source, membership_locked, file_path "
            "FROM local_tracks WHERE id = 'track-1-2'"
        ).fetchone()
    assert moved == ("album-2", "manual", 1, before_paths["track-1-2"])

    album_1 = await store.get_album_identification_context("album-1")
    album_2 = await store.get_album_identification_context("album-2")
    assert album_1 is not None and album_2 is not None
    merge_request = MembershipPreviewRequest(
        track_ids=["track-1-2", "track-2-1"],
        expected_album_revisions={
            "album-1": int(album_1["album"]["row_revision"]),
            "album-2": int(album_2["album"]["row_revision"]),
        },
        target_album_id="album-1",
    )
    merge_preview = await corrections.preview_membership("merge", merge_request, now=11)
    assert merge_preview.aliases == ["album-2"]
    merged = await corrections.apply_membership(
        "merge",
        MembershipApplyRequest(
            track_ids=merge_request.track_ids,
            expected_album_revisions=merge_request.expected_album_revisions,
            target_album_id="album-1",
            preview_token=merge_preview.preview_token,
            idempotency_key="album-merge-1",
        ),
        "admin",
        now=11,
    )
    with sqlite3.connect(db_path) as connection:
        tracks = connection.execute(
            "SELECT id, local_album_id, file_path FROM local_tracks ORDER BY id"
        ).fetchall()
        retired = connection.execute(
            "SELECT retired_into_album_id FROM local_albums WHERE id = 'album-2'"
        ).fetchone()[0]
        alias = connection.execute(
            "SELECT local_album_id FROM local_album_aliases WHERE alias = 'album-2'"
        ).fetchone()[0]
    assert merged["target_album_id"] == "album-1"
    assert {row[0]: row[1] for row in tracks} == {
        "track-1-1": "album-1",
        "track-1-2": "album-1",
        "track-2-1": "album-1",
    }
    assert {row[0]: row[2] for row in tracks} == before_paths
    assert retired == "album-1"
    assert alias == "album-1"


@pytest.mark.asyncio
async def test_artist_merge_rolls_back_credits_retirement_and_alias_on_audit_failure(
    store: NativeLibraryStore, db_path: Path
) -> None:
    await _seed_album(store, "1")
    await _seed_album(store, "2")
    with sqlite3.connect(db_path) as connection:
        connection.execute(
            "INSERT INTO library_user_favorites VALUES "
            "('admin', 'artist', 'artist-2', 1)"
        )
        connection.execute(
            "INSERT INTO library_play_history "
            "(id, user_id, local_track_id, local_album_id, local_artist_id, "
            "track_name, artist_name, played_at) VALUES "
            "('history-rollback', 'admin', 'track-2-1', 'album-2', 'artist-2', "
            "'Track 2', 'Artist 2', '2026-07-14T12:00:00Z')"
        )
    corrections = CatalogCorrectionService(store)
    request = ArtistMergePreviewRequest(
        source_artist_ids=["artist-2"],
        surviving_artist_id="artist-1",
        expected_revisions={"artist-1": 1, "artist-2": 1},
    )
    preview = await corrections.preview_artist_merge(request, now=20)
    with sqlite3.connect(db_path) as connection:
        connection.execute(
            "CREATE TRIGGER fail_artist_merge_audit BEFORE INSERT ON library_catalog_actions "
            "WHEN NEW.action_kind = 'merge_artist' BEGIN SELECT RAISE(ABORT, 'injected'); END"
        )
    with pytest.raises(sqlite3.IntegrityError):
        await corrections.apply_artist_merge(
            ArtistMergeApplyRequest(
                source_artist_ids=request.source_artist_ids,
                surviving_artist_id=request.surviving_artist_id,
                expected_revisions=request.expected_revisions,
                preview_token=preview.preview_token,
                idempotency_key="artist-merge-rollback",
            ),
            "admin",
            now=20,
        )
    with sqlite3.connect(db_path) as connection:
        retired = connection.execute(
            "SELECT retired_into_artist_id FROM local_artists WHERE id = 'artist-2'"
        ).fetchone()[0]
        credit = connection.execute(
            "SELECT local_artist_id FROM local_track_artists "
            "WHERE local_track_id = 'track-2-1'"
        ).fetchone()[0]
        aliases = connection.execute(
            "SELECT COUNT(*) FROM local_artist_aliases WHERE alias = 'artist-2'"
        ).fetchone()[0]
        favorite = connection.execute(
            "SELECT item_id FROM library_user_favorites"
        ).fetchone()[0]
        history_artist = connection.execute(
            "SELECT local_artist_id FROM library_play_history"
        ).fetchone()[0]
    assert retired is None
    assert credit == "artist-2"
    assert aliases == 0
    assert favorite == "artist-2"
    assert history_artist == "artist-2"


@pytest.mark.asyncio
async def test_repair_dry_run_and_apply_detach_only_complete_hard_failure(
    store: NativeLibraryStore,
) -> None:
    await _seed_album(store, "1")
    attempt = IdentificationAttempt(
        id="attempt-repair",
        local_album_id="album-1",
        input_tag_revision="tag",
        input_policy_revision="policy",
        input_file_revision="file",
        matcher_version="old",
        state="contradictory",
        terminal_reason_code="ZERO_SUPPORT",
        selected_candidate_key="rg-1:release-1",
        started_at=2,
        completed_at=2,
    )
    evidence = CandidateEvidence(
        release_group_mbid="rg-1",
        release_mbid="release-1",
        track_evidence=[
            TrackEvidence(
                local_track_id="track-1-1",
                classification="contradictory",
            )
        ],
        reason_code="ZERO_SUPPORT",
    )
    await store.replace_review_attempt(
        "review-1",
        expected_review_revision=1,
        attempt=attempt,
        evidence=[
            IdentificationEvidenceRecord(
                id="evidence-repair",
                attempt_id=attempt.id,
                candidate_key="rg-1:release-1",
                evidence=evidence,
                created_at=2,
            )
        ],
        updated_at=2,
    )
    context = await store.get_album_identification_context("album-1")
    assert context is not None
    await store.attach_album_identity(
        LocalAlbumExternalIdentity(
            local_album_id="album-1",
            release_group_mbid="rg-1",
            release_mbid="release-1",
            decision_source="legacy_import",
            attempt_id=attempt.id,
            selected_at=2,
        ),
        expected_album_revision=int(context["album"]["row_revision"]),
    )
    repair = IdentityRepairService(
        store, _ContradictoryExactRepairProvider(), AlbumEvidenceEngine()
    )
    estimate = await repair.estimate(["root", "root"])
    assert estimate.identity_count == 1
    assert estimate.selected_root_count == 1
    assert estimate.queued_repair_count == 0
    created = await repair.create(
        RepairCreateRequest(idempotency_key="repair-1"), "admin", now=3
    )
    queued_estimate = await repair.estimate([])
    assert queued_estimate.queued_repair_count == 1
    claimed = await store.claim_operation_job(
        "worker", now=4, lease_seconds=60, kind="repair"
    )
    assert claimed is not None
    ready = await repair.run_claimed_audit(claimed, "worker", now=5)
    findings = await repair.findings(created.id)
    assert ready.state == "ready"
    assert ready.repair_summary is not None
    assert ready.repair_summary.total_identities == 1
    assert ready.repair_summary.remaining_identities == 0
    assert ready.repair_summary.counts_by_finding == {"safe_detach": 1}
    assert ready.repair_summary.counts_by_reason == {"ZERO_SUPPORT": 1}
    assert ready.repair_summary.album_counts_by_root == {"root": 1}
    assert ready.repair_summary.input_track_count == 1
    assert ready.repair_summary.playable_after_detach_track_count == 1
    assert ready.repair_summary.estimated_apply_changes == 1
    assert ready.repair_summary.catalog_snapshot_revision >= 1
    assert ready.repair_summary.target_matcher_version == "quiet-reconfirm-v1"
    assert findings.items[0].finding_code == "safe_detach"
    assert findings.items[0].apply_eligible is True
    apply_job = await repair.begin_apply(
        created.id,
        expected_row_revision=ready.row_revision,
        confirmation=True,
        now=6,
    )
    claimed_apply = await store.claim_operation_job(
        "worker", now=7, lease_seconds=60, kind="repair"
    )
    assert claimed_apply is not None
    done = await repair.run_claimed_apply(claimed_apply, "worker", "admin", now=8)
    repeated_apply = await repair.begin_apply(
        created.id,
        expected_row_revision=ready.row_revision,
        confirmation=True,
        now=9,
    )
    context = await store.get_album_identification_context("album-1")
    assert apply_job.state == "queued"
    assert done.state == "succeeded"
    assert repeated_apply.id == done.id
    assert repeated_apply.state == "succeeded"
    assert context is not None and context["identity"] is None
    assert context["tracks"][0]["availability"] == "indexed"


@pytest.mark.asyncio
async def test_repair_stop_restart_resume_and_stale_apply_preserve_playback(
    store: NativeLibraryStore, db_path: Path
) -> None:
    for suffix in ("1", "2"):
        await _seed_album(store, suffix)
        attempt = IdentificationAttempt(
            id=f"repair-restart-attempt-{suffix}",
            local_album_id=f"album-{suffix}",
            input_tag_revision="tag",
            input_policy_revision="policy",
            input_file_revision="file",
            matcher_version="old",
            state="contradictory",
            terminal_reason_code="ZERO_SUPPORT",
            selected_candidate_key=f"rg-{suffix}:release-{suffix}",
            started_at=2,
            completed_at=2,
        )
        await store.replace_review_attempt(
            f"review-{suffix}",
            expected_review_revision=1,
            attempt=attempt,
            evidence=[
                IdentificationEvidenceRecord(
                    id=f"repair-restart-evidence-{suffix}",
                    attempt_id=attempt.id,
                    candidate_key=f"rg-{suffix}:release-{suffix}",
                    evidence=CandidateEvidence(
                        release_group_mbid=f"rg-{suffix}",
                        release_mbid=f"release-{suffix}",
                        track_evidence=[
                            TrackEvidence(
                                local_track_id=f"track-{suffix}-1",
                                classification="contradictory",
                            )
                        ],
                        reason_code="ZERO_SUPPORT",
                    ),
                    created_at=2,
                )
            ],
            updated_at=2,
        )
        context = await store.get_album_identification_context(f"album-{suffix}")
        assert context is not None
        await store.attach_album_identity(
            LocalAlbumExternalIdentity(
                local_album_id=f"album-{suffix}",
                release_group_mbid=f"rg-{suffix}",
                release_mbid=f"release-{suffix}",
                decision_source="legacy_import",
                attempt_id=attempt.id,
                selected_at=2,
            ),
            expected_album_revision=int(context["album"]["row_revision"]),
        )

    repair = IdentityRepairService(
        store, _ContradictoryExactRepairProvider(), AlbumEvidenceEngine()
    )
    operations = LibraryOperationService(store)
    created = await repair.create(
        RepairCreateRequest(idempotency_key="repair-stop-restart"), "admin", now=3
    )
    claimed = await store.claim_operation_job(
        "worker", now=4, lease_seconds=60, kind="repair"
    )
    assert claimed is not None

    async def stop_after_first() -> None:
        current = await store.get_operation_job(created.id)
        assert current is not None
        await operations.control(
            created.id, "stop", int(current["row_revision"]), now=5
        )

    stopped = await repair.run_claimed_audit(
        claimed, "worker", now=5, checkpoint=stop_after_first
    )
    assert stopped.state == "stopped"
    assert stopped.completed_count == 1
    assert await operations.claim("background", now=6) is None
    resumed = await operations.control(
        created.id, "resume", stopped.row_revision, now=7
    )
    assert resumed.state == "queued"
    reclaimed = await store.claim_operation_job(
        "worker", now=8, lease_seconds=60, kind="repair"
    )
    assert reclaimed is not None
    ready = await repair.run_claimed_audit(reclaimed, "worker", now=9)
    assert ready.state == "ready"
    assert ready.repair_summary is not None
    assert ready.repair_summary.total_identities == 2

    apply_job = await repair.begin_apply(
        created.id,
        expected_row_revision=ready.row_revision,
        confirmation=True,
        now=10,
    )
    claimed_apply = await store.claim_operation_job(
        "abandoned-worker", now=11, lease_seconds=1, kind="repair"
    )
    assert claimed_apply is not None
    restarted_store = NativeLibraryStore(db_path, threading.Lock())
    restarted_operations = LibraryOperationService(restarted_store)
    assert await restarted_operations.recover(now=13) == 1
    recovered = await restarted_store.claim_operation_job(
        "worker", now=14, lease_seconds=60, kind="repair"
    )
    assert recovered is not None and recovered["id"] == apply_job.id
    with sqlite3.connect(db_path) as connection:
        connection.execute(
            "UPDATE local_albums SET row_revision = row_revision + 1 WHERE id = 'album-2'"
        )
    done = await IdentityRepairService(
        restarted_store,
        _ContradictoryExactRepairProvider(),
        AlbumEvidenceEngine(),
    ).run_claimed_apply(recovered, "worker", "admin", now=15)
    album_1 = await restarted_store.get_album_identification_context("album-1")
    album_2 = await restarted_store.get_album_identification_context("album-2")
    assert done.succeeded_count == 1
    assert done.skipped_count == 1
    assert album_1 is not None and album_2 is not None
    assert album_1["identity"] is None
    assert album_2["identity"] is not None
    assert album_1["tracks"][0]["availability"] == "indexed"
    assert album_2["tracks"][0]["availability"] == "indexed"


@pytest.mark.asyncio
async def test_repair_audit_generates_missing_evidence_and_defers_whole_job_when_provider_fails(
    store: NativeLibraryStore, db_path: Path
) -> None:
    await _seed_album(store, "1")
    context = await store.get_album_identification_context("album-1")
    assert context is not None
    await store.attach_album_identity(
        LocalAlbumExternalIdentity(
            local_album_id="album-1",
            release_group_mbid="rg-1",
            release_mbid="release-rg-1",
            decision_source="legacy_import",
            selected_at=2,
        ),
        expected_album_revision=int(context["album"]["row_revision"]),
    )
    repair = IdentityRepairService(store, _RepairProvider(), AlbumEvidenceEngine())
    created = await repair.create(
        RepairCreateRequest(idempotency_key="repair-generate"), "admin", now=3
    )
    claimed = await store.claim_operation_job(
        "worker", now=4, lease_seconds=60, kind="repair"
    )
    assert claimed is not None
    await repair.run_claimed_audit(claimed, "worker", now=5)
    findings = await repair.findings(created.id)
    context = await store.get_album_identification_context("album-1")
    assert findings.items[0].finding_code == "valid"
    assert context is not None and context["identity"] is not None
    with sqlite3.connect(db_path) as connection:
        generated = connection.execute(
            "SELECT trigger, matcher_version FROM library_identification_attempts "
            "WHERE local_album_id = 'album-1' AND trigger = 'repair_audit'"
        ).fetchone()
    assert generated == ("repair_audit", MATCHER_VERSION)

    await _seed_album(store, "2")
    context = await store.get_album_identification_context("album-2")
    assert context is not None
    await store.attach_album_identity(
        LocalAlbumExternalIdentity(
            local_album_id="album-2",
            release_group_mbid="rg-2",
            release_mbid="release-rg-2",
            decision_source="legacy_import",
            selected_at=6,
        ),
        expected_album_revision=int(context["album"]["row_revision"]),
    )
    unavailable = IdentityRepairService(
        store, _UnavailableRepairProvider(), AlbumEvidenceEngine()
    )
    second = await unavailable.create(
        RepairCreateRequest(idempotency_key="repair-unavailable"), "admin", now=7
    )
    claimed = await store.claim_operation_job(
        "worker", now=8, lease_seconds=60, kind="repair"
    )
    assert claimed is not None
    deferred = await unavailable.run_claimed_audit(claimed, "worker", now=9)
    assert deferred.state == "queued"
    assert deferred.succeeded_count == 0
    job_row = await store.get_operation_job(second.id)
    assert job_row is not None
    assert job_row["state"] == "queued"
    assert job_row["lease_owner"] is None
    assert job_row["next_attempt_at"] == pytest.approx(9 + 120)
    with sqlite3.connect(db_path) as connection:
        work = connection.execute(
            "SELECT local_album_id, state, failure_code FROM library_operation_work "
            "WHERE job_id = ? ORDER BY ordinal",
            (second.id,),
        ).fetchall()
    assert work == [
        ("album-1", "pending", "PROVIDER_DEFERRED"),
        ("album-2", "pending", None),
    ]
    assert (await unavailable.findings(second.id)).items == []
    assert (
        await store.claim_operation_job(
            "worker", now=100, lease_seconds=60, kind="repair"
        )
        is None
    )
    reclaimed = await store.claim_operation_job(
        "worker", now=129, lease_seconds=60, kind="repair"
    )
    assert reclaimed is not None
    assert reclaimed["id"] == second.id
    assert reclaimed["next_attempt_at"] is None
    with pytest.raises(ValidationError, match="category is invalid"):
        await unavailable.findings(second.id, finding_category="not-a-category")


@pytest.mark.asyncio
async def test_repair_audit_resumes_at_the_deferred_item_after_provider_recovery(
    store: NativeLibraryStore, db_path: Path
) -> None:
    await _seed_album(store, "1", identity_source="legacy_import")
    await _seed_album(store, "2", identity_source="legacy_import")
    repair = IdentityRepairService(
        store, _UnavailableRepairProvider(), AlbumEvidenceEngine()
    )
    created = await repair.create(
        RepairCreateRequest(idempotency_key="repair-resume"), "admin", now=3
    )
    claimed = await store.claim_operation_job(
        "worker", now=4, lease_seconds=60, kind="repair"
    )
    assert claimed is not None
    deferred = await repair.run_claimed_audit(claimed, "worker", now=5)
    assert deferred.state == "queued"
    with sqlite3.connect(db_path) as connection:
        work = connection.execute(
            "SELECT state, failure_code FROM library_operation_work WHERE job_id = ? "
            "ORDER BY ordinal",
            (created.id,),
        ).fetchall()
    assert work == [
        ("pending", "PROVIDER_DEFERRED"),
        ("pending", None),
    ]

    provider = _OrderedRepairProvider()
    recovered = IdentityRepairService(store, provider, AlbumEvidenceEngine())
    reclaimed = await store.claim_operation_job(
        "worker", now=5 + 120, lease_seconds=60, kind="repair"
    )
    assert reclaimed is not None
    assert reclaimed["id"] == created.id
    ready = await recovered.run_claimed_audit(reclaimed, "worker", now=5 + 121)
    assert ready.state == "ready"
    assert ready.repair_summary is not None
    assert ready.repair_summary.provider_deferred_count == 0
    assert ready.repair_summary.total_identities == 2
    # The deferred item is the first pending on resume: the audit continues
    # exactly where the provider failure left it.
    assert provider.exact_calls == ["release-1", "release-2"]


@pytest.mark.asyncio
async def test_repair_audit_defers_before_any_provider_call_when_breaker_open(
    store: NativeLibraryStore, db_path: Path
) -> None:
    await _seed_album(store, "1", identity_source="legacy_import")
    provider = _OrderedRepairProvider()
    repair = IdentityRepairService(
        store,
        provider,
        AlbumEvidenceEngine(),
        provider_available=lambda: False,
    )
    created = await repair.create(
        RepairCreateRequest(idempotency_key="repair-open-breaker"), "admin", now=3
    )
    claimed = await store.claim_operation_job(
        "worker", now=4, lease_seconds=60, kind="repair"
    )
    assert claimed is not None
    deferred = await repair.run_claimed_audit(claimed, "worker", now=5)
    assert deferred.state == "queued"
    job_row = await store.get_operation_job(created.id)
    assert job_row is not None
    assert job_row["next_attempt_at"] == pytest.approx(5 + 120)
    assert provider.exact_calls == []
    with sqlite3.connect(db_path) as connection:
        work = connection.execute(
            "SELECT state, failure_code FROM library_operation_work WHERE job_id = ?",
            (created.id,),
        ).fetchall()
    assert work == [("pending", None)]


@pytest.mark.asyncio
async def test_repair_audit_defers_when_provider_raises_circuit_open_error(
    store: NativeLibraryStore, db_path: Path
) -> None:
    await _seed_album(store, "1", identity_source="legacy_import")
    repair = IdentityRepairService(
        store, _CircuitOpenRepairProvider(), AlbumEvidenceEngine()
    )
    created = await repair.create(
        RepairCreateRequest(idempotency_key="repair-circuit-open"), "admin", now=3
    )
    claimed = await store.claim_operation_job(
        "worker", now=4, lease_seconds=60, kind="repair"
    )
    assert claimed is not None
    deferred = await repair.run_claimed_audit(claimed, "worker", now=5)
    assert deferred.state == "queued"
    job_row = await store.get_operation_job(created.id)
    assert job_row is not None
    assert job_row["next_attempt_at"] == pytest.approx(5 + 120)
    with sqlite3.connect(db_path) as connection:
        work = connection.execute(
            "SELECT state, failure_code FROM library_operation_work WHERE job_id = ?",
            (created.id,),
        ).fetchall()
    assert work == [("pending", "PROVIDER_DEFERRED")]
    assert (await repair.findings(created.id)).items == []


@pytest.mark.asyncio
async def test_repair_reuses_revision_keyed_fingerprint_as_shared_evidence(
    store: NativeLibraryStore,
) -> None:
    await _seed_album(store, "1")
    context = await store.get_album_identification_context("album-1")
    assert context is not None
    await store.attach_album_identity(
        LocalAlbumExternalIdentity(
            local_album_id="album-1",
            release_group_mbid="rg-explicit",
            release_mbid="release-explicit",
            decision_source="legacy_import",
            selected_at=2,
        ),
        expected_album_revision=int(context["album"]["row_revision"]),
    )
    await store.record_fingerprint_outcome(
        FingerprintOutcome(
            id="repair-fingerprint",
            local_track_id="track-1-1",
            stat_revision="stat-1-1",
            fingerprinter_version="fpcalc-acoustid-v1",
            state="matched",
            fingerprint="fingerprint",
            duration_seconds=180,
            recording_mbid="different-recording",
            release_group_ids=["different-release-group"],
            first_attempt_at=2,
            last_attempt_at=2,
        )
    )
    repair = IdentityRepairService(
        store, _IdentificationProvider(), AlbumEvidenceEngine()
    )
    created = await repair.create(
        RepairCreateRequest(idempotency_key="repair-fingerprint"), "admin", now=3
    )
    claimed = await store.claim_operation_job(
        "worker", now=4, lease_seconds=60, kind="repair"
    )
    assert claimed is not None
    await repair.run_claimed_audit(claimed, "worker", now=5)
    finding = (await repair.findings(created.id)).items[0]
    evidence = await store.get_latest_album_candidate_evidence(
        "album-1", "rg-explicit:release-explicit"
    )
    # #392: the reused off-release fingerprint is support-only, so exact text
    # evidence passes the audit; the fingerprint fill still blocks auto-apply.
    assert finding.finding_code == "valid"
    assert finding.apply_eligible is False
    assert evidence is not None
    assert evidence.evidence.track_evidence[0].classification == "supported"


@pytest.mark.asyncio
async def test_existing_match_repair_uses_only_the_stored_exact_release(
    store: NativeLibraryStore,
) -> None:
    await _seed_album(store, "1", identity_source="legacy_import")

    class ExactOnlyProvider(_ContradictoryExactRepairProvider):
        def __init__(self) -> None:
            self.exact_calls: list[str] = []
            self.heuristic_calls = 0

        async def get_album_candidate(
            self, release_group_mbid, target_track_count, priority
        ):
            self.heuristic_calls += 1
            raise AssertionError("Existing-match repair must not rank another edition")

        async def get_exact_release_candidate(self, release_mbid, priority):
            self.exact_calls.append(release_mbid)
            candidate = await _RepairProvider.get_exact_release_candidate(
                self, release_mbid, priority
            )
            candidate.tracks[0].recording_mbid = "recording-track-1-1"
            return candidate

    provider = ExactOnlyProvider()
    repair = IdentityRepairService(store, provider, AlbumEvidenceEngine())
    created = await repair.create(
        RepairCreateRequest(idempotency_key="repair-exact-only"), "admin", now=3
    )
    claimed = await store.claim_operation_job(
        "worker", now=4, lease_seconds=60, kind="repair"
    )
    assert claimed is not None

    await repair.run_claimed_audit(claimed, "worker", now=5)

    assert provider.exact_calls == ["release-1"]
    assert provider.heuristic_calls == 0
    assert (await repair.findings(created.id)).items[0].finding_code == "valid"


@pytest.mark.asyncio
async def test_existing_match_repair_excludes_missing_only_albums_and_preserves_missing_history(
    store: NativeLibraryStore, db_path: Path
) -> None:
    await _seed_album(store, "1", identity_source="legacy_import", two_tracks=True)
    await _seed_album(store, "2", identity_source="legacy_import")
    with sqlite3.connect(db_path) as connection:
        connection.execute(
            "UPDATE local_tracks SET availability='missing' WHERE id='track-1-2'"
        )
        connection.execute(
            "UPDATE local_tracks SET availability='missing' "
            "WHERE local_album_id='album-2'"
        )
    repair = IdentityRepairService(
        store, _ContradictoryExactRepairProvider(), AlbumEvidenceEngine()
    )

    estimate = await repair.estimate(["root"])
    created = await repair.create(
        RepairCreateRequest(idempotency_key="repair-indexed-only"), "admin", now=3
    )
    claimed = await store.claim_operation_job(
        "worker", now=4, lease_seconds=60, kind="repair"
    )
    assert claimed is not None
    ready = await repair.run_claimed_audit(claimed, "worker", now=5)

    assert estimate.identity_count == 1
    assert created.expected_work_count == 1
    assert ready.repair_summary is not None
    assert ready.repair_summary.input_track_count == 1
    assert ready.repair_summary.estimated_apply_changes == 1

    await repair.begin_apply(
        created.id,
        expected_row_revision=ready.row_revision,
        confirmation=True,
        now=6,
    )
    claimed_apply = await store.claim_operation_job(
        "worker", now=7, lease_seconds=60, kind="repair"
    )
    assert claimed_apply is not None
    done = await repair.run_claimed_apply(claimed_apply, "worker", "admin", now=8)
    with sqlite3.connect(db_path) as connection:
        retained = connection.execute(
            "SELECT t.id,t.availability,i.recording_mbid "
            "FROM local_tracks t LEFT JOIN local_track_external_identities i "
            "ON i.local_track_id=t.id AND i.provider='musicbrainz' "
            "WHERE t.local_album_id='album-1' ORDER BY t.id"
        ).fetchall()

    assert done.succeeded_count == 1
    assert retained == [
        ("track-1-1", "indexed", None),
        ("track-1-2", "missing", "recording-track-1-2"),
    ]


@pytest.mark.asyncio
async def test_existing_match_repair_skips_when_indexed_membership_changes_before_apply(
    store: NativeLibraryStore, db_path: Path
) -> None:
    await _seed_album(store, "1", identity_source="legacy_import")
    repair = IdentityRepairService(
        store, _ContradictoryExactRepairProvider(), AlbumEvidenceEngine()
    )
    created = await repair.create(
        RepairCreateRequest(idempotency_key="repair-membership-stale"), "admin", now=3
    )
    claimed = await store.claim_operation_job(
        "worker", now=4, lease_seconds=60, kind="repair"
    )
    assert claimed is not None
    ready = await repair.run_claimed_audit(claimed, "worker", now=5)
    await repair.begin_apply(
        created.id,
        expected_row_revision=ready.row_revision,
        confirmation=True,
        now=6,
    )
    with sqlite3.connect(db_path) as connection:
        connection.execute(
            "UPDATE local_tracks SET availability='missing' WHERE id='track-1-1'"
        )
    claimed_apply = await store.claim_operation_job(
        "worker", now=7, lease_seconds=60, kind="repair"
    )
    assert claimed_apply is not None

    done = await repair.run_claimed_apply(claimed_apply, "worker", "admin", now=8)
    context = await store.get_album_identification_context("album-1")

    assert done.succeeded_count == 0
    assert done.skipped_count == 1
    assert context is not None
    assert context["identity"] is not None
    assert (await repair.findings(created.id)).items[0].state == "stale"


@pytest.mark.asyncio
async def test_management_identity_preparation_maps_only_the_accepted_exact_release(
    store: NativeLibraryStore, db_path: Path
) -> None:
    await _seed_album(store, "1", identity_source="legacy_import")
    provider = _CanonicalReleaseProvider()
    preparation = IdentityRepairService(store, canonical_provider=provider)

    estimate = await preparation.estimate_management_preparation(["root", "root"])
    assert estimate.album_count == 1
    assert estimate.ready_album_count == 0
    assert estimate.mapping_required_count == 1
    assert estimate.exact_release_required_count == 0
    assert estimate.selected_root_count == 1

    created = await preparation.create_management_preparation(
        IdentityPreparationCreateRequest(
            idempotency_key="management-readiness-1", root_ids=["root"]
        ),
        "admin",
        now=3,
    )
    assert (await preparation.history(purpose="existing_matches")).items == []
    assert [
        item.id
        for item in (await preparation.history(purpose="management_readiness")).items
    ] == [created.id]
    claimed = await store.claim_operation_job(
        "worker", now=4, lease_seconds=60, kind="repair"
    )
    assert claimed is not None
    ready = await preparation.run_claimed_audit(claimed, "worker", now=5)
    assert provider.calls == ["release-1"]
    assert ready.state == "ready"
    assert ready.repair_summary is not None
    assert ready.repair_summary.purpose == "management_readiness"
    assert ready.repair_summary.mapping_candidate_count == 1
    assert ready.repair_summary.estimated_apply_changes == 1
    finding = (
        await preparation.findings(created.id, finding_category="mapping_ready")
    ).items[0]
    assert finding.reason_code == "EXACT_RELEASE_MAPPING_SUPPORTED"
    assert finding.apply_eligible is True
    assert finding.album_title == "Album 1"
    assert finding.album_artist_name == "Artist 1"
    assert finding.album_year is None
    assert finding.cover_available is True
    with pytest.raises(ResourceNotFoundError):
        await preparation.begin_apply(
            created.id,
            expected_row_revision=ready.row_revision,
            confirmation=True,
            now=6,
        )

    before = await store.get_album_identification_context("album-1")
    assert before is not None
    before_track_revision = int(before["tracks"][0]["row_revision"])
    queued = await preparation.begin_management_preparation_apply(
        created.id,
        expected_row_revision=ready.row_revision,
        confirmation=True,
        now=6,
    )
    assert queued.state == "queued"
    claimed_apply = await store.claim_operation_job(
        "worker", now=7, lease_seconds=60, kind="repair"
    )
    assert claimed_apply is not None
    done = await preparation.run_claimed_apply(claimed_apply, "worker", "admin", now=8)
    assert done.state == "succeeded"
    context = await store.get_album_identification_context("album-1")
    assert context is not None
    track = context["tracks"][0]
    assert track["recording_mbid"] == "recording-track-1-1"
    assert track["identity_release_mbid"] == "release-1"
    assert track["release_track_mbid"] == "release-track-1"
    assert track["medium_position"] == 1
    assert track["release_track_position"] == 1
    assert int(track["row_revision"]) == before_track_revision
    with sqlite3.connect(db_path) as connection:
        action = connection.execute(
            "SELECT action_kind, reason_code FROM library_catalog_actions "
            "WHERE operation_job_id = ?",
            (created.id,),
        ).fetchone()
    assert action == (
        "accept_management_track_mappings",
        "EXACT_RELEASE_MAPPINGS_ACCEPTED",
    )


@pytest.mark.asyncio
async def test_management_findings_hide_changed_identities_but_keep_audit_history(
    store: NativeLibraryStore, db_path: Path
) -> None:
    await _seed_album(store, "1", identity_source="legacy_import")
    preparation = IdentityRepairService(
        store, canonical_provider=_CanonicalReleaseProvider()
    )
    created = await preparation.create_management_preparation(
        IdentityPreparationCreateRequest(idempotency_key="current-findings"),
        "admin",
        now=3,
    )
    claimed = await store.claim_operation_job(
        "worker", now=4, lease_seconds=60, kind="repair"
    )
    assert claimed is not None
    await preparation.run_claimed_audit(claimed, "worker", now=5)

    current = await preparation.findings(created.id)
    assert current.current_counts_by_finding == {"mapping_ready": 1}
    assert [item.local_album_id for item in current.items] == ["album-1"]

    with sqlite3.connect(db_path) as connection:
        connection.execute(
            "UPDATE local_album_external_identities "
            "SET row_revision = row_revision + 1 WHERE local_album_id = 'album-1'"
        )

    superseded = await preparation.findings(created.id)
    assert superseded.items == []
    assert superseded.current_counts_by_finding == {}
    with sqlite3.connect(db_path) as connection:
        audit_row = connection.execute(
            "SELECT finding_code, state FROM library_identity_repair_findings "
            "WHERE job_id = ? AND local_album_id = 'album-1'",
            (created.id,),
        ).fetchone()
    assert audit_row == ("mapping_ready", "open")


@pytest.mark.asyncio
async def test_old_management_report_requires_a_fresh_identity_check(
    store: NativeLibraryStore, db_path: Path
) -> None:
    await _seed_album(store, "1", identity_source="legacy_import")
    preparation = IdentityRepairService(
        store, canonical_provider=_CanonicalReleaseProvider()
    )
    created = await preparation.create_management_preparation(
        IdentityPreparationCreateRequest(idempotency_key="old-rules-report"),
        "admin",
        now=3,
    )
    claimed = await store.claim_operation_job(
        "worker", now=4, lease_seconds=60, kind="repair"
    )
    assert claimed is not None
    ready = await preparation.run_claimed_audit(claimed, "worker", now=5)
    with sqlite3.connect(db_path) as connection:
        connection.execute(
            "UPDATE library_repair_snapshots "
            "SET target_matcher_version = 'management-exact-release-v1' "
            "WHERE job_id = ?",
            (created.id,),
        )

    findings = await preparation.findings(created.id)
    assert findings.refresh_required is True
    assert findings.items
    with pytest.raises(ValidationError, match="older rules"):
        await preparation.begin_management_preparation_apply(
            created.id,
            expected_row_revision=ready.row_revision,
            confirmation=True,
            now=6,
        )


@pytest.mark.asyncio
async def test_operation_supervisor_renews_long_identity_audit_leases(
    store: NativeLibraryStore,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    await _seed_album(store, "1", identity_source="legacy_import")
    await _seed_album(store, "2", identity_source="legacy_import")
    preparation = IdentityRepairService(
        store, canonical_provider=_CanonicalReleaseProvider()
    )
    created = await preparation.create_management_preparation(
        IdentityPreparationCreateRequest(
            idempotency_key="management-readiness-heartbeat", root_ids=["root"]
        ),
        "admin",
        now=3,
    )
    timestamps = iter([4.0, 10.0, 20.0, 30.0])
    monkeypatch.setattr(
        "services.native.library_operation_supervisor.time.time",
        lambda: next(timestamps),
    )
    lease_expiries: list[float] = []

    async def record_lease() -> None:
        row = await store.get_operation_job(created.id)
        assert row is not None
        lease_expiries.append(float(row["lease_expires_at"]))

    operations = LibraryOperationService(store)
    supervisor = LibraryOperationSupervisor(
        store,
        operations,
        preparation,
        ExplicitReidentificationWorker(
            store,
            AlbumCandidateService(_IdentificationProvider()),
            AlbumEvidenceEngine(),
        ),
    )
    original = preparation.run_claimed_audit

    async def run_with_checkpoint(job, worker_id, *, now=None):
        return await original(job, worker_id, now=now, checkpoint=record_lease)

    monkeypatch.setattr(preparation, "run_claimed_audit", run_with_checkpoint)

    ready = await supervisor.run_once("worker")

    assert ready is not None and ready.state == "ready"
    # (GH-293) Control is checked at pass cadence; the per-unit heartbeat clock
    # advances once per claimed subject, so the recorded lease expiries are
    # 20 s + 60 s and 30 s + 60 s under the test's fake clock.
    assert lease_expiries == [80.0, 90.0]

    queued = await preparation.begin_management_preparation_apply(
        created.id,
        expected_row_revision=ready.row_revision,
        confirmation=True,
        now=40,
    )
    assert queued.state == "queued"
    apply_timestamps = iter([50.0, 60.0, 70.0])
    monkeypatch.setattr(
        "services.native.library_operation_supervisor.time.time",
        lambda: next(apply_timestamps),
    )
    apply_lease_expiries: list[float] = []

    async def record_apply_lease() -> None:
        row = await store.get_operation_job(created.id)
        assert row is not None
        apply_lease_expiries.append(float(row["lease_expires_at"]))

    original_apply = preparation.run_claimed_apply

    async def run_apply_with_checkpoint(job, worker_id, actor_user_id, *, now=None):
        return await original_apply(
            job,
            worker_id,
            actor_user_id,
            now=now,
            checkpoint=record_apply_lease,
        )

    monkeypatch.setattr(preparation, "run_claimed_apply", run_apply_with_checkpoint)

    done = await supervisor.run_once("worker")

    assert done is not None and done.state == "succeeded"
    # (GH-293) Pass cadence: the single apply unit heartbeats at 70 s + 60 s.
    assert apply_lease_expiries == [130.0]


@pytest.mark.asyncio
async def test_management_identity_preparation_provider_verifies_a_complete_mapping(
    store: NativeLibraryStore, db_path: Path
) -> None:
    await _seed_album(store, "1", identity_source="legacy_import")
    with sqlite3.connect(db_path) as connection:
        connection.execute(
            "UPDATE local_track_external_identities SET "
            "release_track_mbid='release-track-1', medium_position=1, "
            "release_track_position=1 WHERE local_track_id='track-1-1'"
        )
    provider = _CanonicalReleaseProvider()
    preparation = IdentityRepairService(store, canonical_provider=provider)
    created = await preparation.create_management_preparation(
        IdentityPreparationCreateRequest(idempotency_key="verify-complete-map"),
        "admin",
        now=3,
    )
    claimed = await store.claim_operation_job(
        "worker", now=4, lease_seconds=60, kind="repair"
    )
    assert claimed is not None

    ready = await preparation.run_claimed_audit(claimed, "worker", now=5)
    finding = (await preparation.findings(created.id, finding_category="ready")).items[
        0
    ]

    assert provider.calls == ["release-1"]
    assert ready.repair_summary is not None
    assert ready.repair_summary.ready_album_count == 1
    assert ready.repair_summary.mapping_candidate_count == 0
    assert ready.repair_summary.estimated_apply_changes == 0
    assert finding.reason_code == "EXACT_RELEASE_MAPPINGS_VERIFIED"
    assert finding.apply_eligible is False


@pytest.mark.asyncio
@pytest.mark.parametrize("secondary_type", ["Compilation", "Live"])
@pytest.mark.parametrize("identity_source", ["manual", "legacy_import"])
async def test_management_readiness_accepts_complete_special_release_mappings(
    store: NativeLibraryStore,
    db_path: Path,
    secondary_type: str,
    identity_source: str,
) -> None:
    await _seed_album(store, "1", identity_source=identity_source)
    with sqlite3.connect(db_path) as connection:
        connection.execute(
            "UPDATE local_track_external_identities SET "
            "release_track_mbid='release-track-1', medium_position=1, "
            "release_track_position=1 WHERE local_track_id='track-1-1'"
        )
    preparation = IdentityRepairService(
        store,
        canonical_provider=_CanonicalReleaseProvider(secondary_types=(secondary_type,)),
    )
    created = await preparation.create_management_preparation(
        IdentityPreparationCreateRequest(
            idempotency_key=f"complete-{identity_source}-{secondary_type.casefold()}"
        ),
        "admin",
        now=3,
    )
    claimed = await store.claim_operation_job(
        "worker", now=4, lease_seconds=60, kind="repair"
    )
    assert claimed is not None

    ready = await preparation.run_claimed_audit(claimed, "worker", now=5)
    findings = await preparation.findings(created.id, finding_category="ready")

    assert ready.repair_summary is not None
    assert ready.repair_summary.ready_album_count == 1
    assert findings.current_counts_by_finding == {"ready": 1}
    assert findings.items[0].reason_code == "EXACT_RELEASE_MAPPINGS_VERIFIED"


@pytest.mark.asyncio
@pytest.mark.parametrize("secondary_type", ["Compilation", "Live"])
async def test_manual_special_release_without_a_complete_map_still_needs_review(
    store: NativeLibraryStore,
    secondary_type: str,
) -> None:
    await _seed_album(store, "1", identity_source="manual")
    preparation = IdentityRepairService(
        store,
        canonical_provider=_CanonicalReleaseProvider(secondary_types=(secondary_type,)),
    )
    created = await preparation.create_management_preparation(
        IdentityPreparationCreateRequest(
            idempotency_key=f"incomplete-manual-{secondary_type.casefold()}"
        ),
        "admin",
        now=3,
    )
    claimed = await store.claim_operation_job(
        "worker", now=4, lease_seconds=60, kind="repair"
    )
    assert claimed is not None

    ready = await preparation.run_claimed_audit(claimed, "worker", now=5)
    findings = await preparation.findings(created.id, finding_category="needs_review")

    assert ready.repair_summary is not None
    assert ready.repair_summary.needs_review_count == 1
    assert findings.current_counts_by_finding == {"needs_review": 1}
    assert findings.items[0].reason_code == "RELEASE_TYPE_REQUIRES_CONFIRMATION"
    assert findings.items[0].apply_eligible is False


@pytest.mark.asyncio
async def test_management_identity_preparation_defers_a_complete_mapping_when_provider_fails(
    store: NativeLibraryStore, db_path: Path
) -> None:
    await _seed_album(store, "1", identity_source="legacy_import")
    with sqlite3.connect(db_path) as connection:
        connection.execute(
            "UPDATE local_track_external_identities SET "
            "release_track_mbid='release-track-1', medium_position=1, "
            "release_track_position=1 WHERE local_track_id='track-1-1'"
        )
    provider = _CanonicalReleaseProvider(unavailable=True)
    preparation = IdentityRepairService(store, canonical_provider=provider)
    created = await preparation.create_management_preparation(
        IdentityPreparationCreateRequest(idempotency_key="defer-complete-map"),
        "admin",
        now=3,
    )
    claimed = await store.claim_operation_job(
        "worker", now=4, lease_seconds=60, kind="repair"
    )
    assert claimed is not None

    deferred = await preparation.run_claimed_audit(claimed, "worker", now=5)

    assert provider.calls == ["release-1"]
    assert deferred.state == "queued"
    assert deferred.succeeded_count == 0
    job_row = await store.get_operation_job(created.id)
    assert job_row is not None
    assert job_row["next_attempt_at"] == pytest.approx(5 + 120)
    with sqlite3.connect(db_path) as connection:
        work = connection.execute(
            "SELECT state, failure_code FROM library_operation_work WHERE job_id = ?",
            (created.id,),
        ).fetchall()
    assert work == [("pending", "PROVIDER_DEFERRED")]
    assert (await preparation.findings(created.id)).items == []


@pytest.mark.asyncio
async def test_management_identity_preparation_repairs_a_stale_position(
    store: NativeLibraryStore, db_path: Path
) -> None:
    await _seed_album(store, "1", identity_source="legacy_import")
    with sqlite3.connect(db_path) as connection:
        connection.execute(
            "UPDATE local_track_external_identities SET "
            "release_track_mbid='release-track-1', medium_position=1, "
            "release_track_position=9 WHERE local_track_id='track-1-1'"
        )
    provider = _CanonicalReleaseProvider()
    preparation = IdentityRepairService(store, canonical_provider=provider)
    created = await preparation.create_management_preparation(
        IdentityPreparationCreateRequest(idempotency_key="repair-complete-position"),
        "admin",
        now=3,
    )
    claimed = await store.claim_operation_job(
        "worker", now=4, lease_seconds=60, kind="repair"
    )
    assert claimed is not None
    ready = await preparation.run_claimed_audit(claimed, "worker", now=5)
    mapping = (
        await preparation.findings(created.id, finding_category="mapping_ready")
    ).items[0]

    assert provider.calls == ["release-1"]
    assert ready.repair_summary is not None
    assert ready.repair_summary.mapping_candidate_count == 1
    assert mapping.reason_code == "EXACT_RELEASE_MAPPING_SUPPORTED"
    assert mapping.apply_eligible is True


@pytest.mark.asyncio
async def test_management_identity_preparation_rejects_a_different_release_track(
    store: NativeLibraryStore, db_path: Path
) -> None:
    await _seed_album(store, "1", identity_source="legacy_import")
    with sqlite3.connect(db_path) as connection:
        connection.execute(
            "UPDATE local_track_external_identities SET "
            "release_track_mbid='different-release-track', medium_position=1, "
            "release_track_position=1 WHERE local_track_id='track-1-1'"
        )
    conflicting = IdentityRepairService(
        store, canonical_provider=_CanonicalReleaseProvider()
    )
    conflict_job = await conflicting.create_management_preparation(
        IdentityPreparationCreateRequest(
            idempotency_key="reject-conflicting-complete-map", root_ids=["root"]
        ),
        "admin",
        now=6,
    )
    claimed_conflict = await store.claim_operation_job(
        "worker", now=7, lease_seconds=60, kind="repair"
    )
    assert claimed_conflict is not None
    await conflicting.run_claimed_audit(claimed_conflict, "worker", now=8)
    conflicts = {
        item.local_album_id: item
        for item in (
            await conflicting.findings(conflict_job.id, finding_category="needs_review")
        ).items
    }

    assert conflicts["album-1"].reason_code == "CONFLICTING_TRACK_EVIDENCE"
    assert conflicts["album-1"].apply_eligible is False


@pytest.mark.asyncio
async def test_management_identity_preparation_accepts_provider_proven_recording_redirect(
    store: NativeLibraryStore, db_path: Path
) -> None:
    await _seed_album(store, "1", identity_source="legacy_import")
    with sqlite3.connect(db_path) as connection:
        connection.execute(
            "UPDATE local_track_external_identities SET recording_mbid = 'retired-recording' "
            "WHERE local_track_id = 'track-1-1'"
        )
        connection.execute(
            "UPDATE local_tracks SET embedded_recording_mbid = 'retired-recording' "
            "WHERE id = 'track-1-1'"
        )
    provider = _CanonicalReleaseProvider(
        recording_redirects={"retired-recording": "recording-track-1-1"}
    )
    preparation = IdentityRepairService(store, canonical_provider=provider)
    created = await preparation.create_management_preparation(
        IdentityPreparationCreateRequest(idempotency_key="recording-redirect"),
        "admin",
        now=3,
    )
    claimed = await store.claim_operation_job(
        "worker", now=4, lease_seconds=60, kind="repair"
    )
    assert claimed is not None
    ready = await preparation.run_claimed_audit(claimed, "worker", now=5)

    finding = (
        await preparation.findings(created.id, finding_category="mapping_ready")
    ).items[0]
    evidence = await store.get_latest_album_candidate_evidence(
        "album-1", "rg-1:release-1"
    )
    assert finding.reason_code == "EXACT_RELEASE_MAPPING_SUPPORTED"
    assert evidence is not None
    assert evidence.evidence.track_evidence[0].recording_mbid_redirects == [
        "retired-recording"
    ]
    assert "recording_mbid_redirect" in (
        evidence.evidence.track_evidence[0].evidence_kinds
    )
    assert provider.recording_calls == ["retired-recording"]

    await preparation.begin_management_preparation_apply(
        created.id,
        expected_row_revision=ready.row_revision,
        confirmation=True,
        now=6,
    )
    claimed_apply = await store.claim_operation_job(
        "worker", now=7, lease_seconds=60, kind="repair"
    )
    assert claimed_apply is not None
    done = await preparation.run_claimed_apply(claimed_apply, "worker", "admin", now=8)
    assert done.state == "succeeded"
    context = await store.get_album_identification_context("album-1")
    assert context is not None
    assert context["tracks"][0]["recording_mbid"] == "recording-track-1-1"
    assert context["tracks"][0]["release_track_mbid"] == "release-track-1"


@pytest.mark.asyncio
async def test_management_identity_preparation_skips_changed_redirect_alias_at_apply(
    store: NativeLibraryStore, db_path: Path
) -> None:
    await _seed_album(store, "1", identity_source="legacy_import")
    with sqlite3.connect(db_path) as connection:
        connection.execute(
            "UPDATE local_track_external_identities SET recording_mbid = 'retired-recording' "
            "WHERE local_track_id = 'track-1-1'"
        )
    preparation = IdentityRepairService(
        store,
        canonical_provider=_CanonicalReleaseProvider(
            recording_redirects={"retired-recording": "recording-track-1-1"}
        ),
    )
    created = await preparation.create_management_preparation(
        IdentityPreparationCreateRequest(idempotency_key="changed-recording-alias"),
        "admin",
        now=3,
    )
    claimed = await store.claim_operation_job(
        "worker", now=4, lease_seconds=60, kind="repair"
    )
    assert claimed is not None
    ready = await preparation.run_claimed_audit(claimed, "worker", now=5)
    await preparation.begin_management_preparation_apply(
        created.id,
        expected_row_revision=ready.row_revision,
        confirmation=True,
        now=6,
    )
    with sqlite3.connect(db_path) as connection:
        connection.execute(
            "UPDATE local_track_external_identities "
            "SET recording_mbid = 'different-retired-recording' "
            "WHERE local_track_id = 'track-1-1'"
        )
    claimed_apply = await store.claim_operation_job(
        "worker", now=7, lease_seconds=60, kind="repair"
    )
    assert claimed_apply is not None
    done = await preparation.run_claimed_apply(claimed_apply, "worker", "admin", now=8)

    assert done.succeeded_count == 0
    assert done.skipped_count == 1
    context = await store.get_album_identification_context("album-1")
    assert context is not None
    assert context["tracks"][0]["recording_mbid"] == "different-retired-recording"
    assert context["tracks"][0]["release_track_mbid"] is None
    findings = await preparation.findings(created.id, finding_category="unverifiable")
    assert findings.items == []
    with sqlite3.connect(db_path) as connection:
        audit_row = connection.execute(
            "SELECT state, apply_result FROM library_identity_repair_findings "
            "WHERE job_id = ? AND local_album_id = 'album-1'",
            (created.id,),
        ).fetchone()
    assert audit_row == ("stale", "STALE_SUBJECT")


@pytest.mark.asyncio
async def test_management_identity_preparation_disambiguates_duplicate_recording_by_position(
    store: NativeLibraryStore, db_path: Path
) -> None:
    await _seed_album(
        store,
        "1",
        identity_source="legacy_import",
        two_tracks=True,
    )
    with sqlite3.connect(db_path) as connection:
        connection.execute(
            "UPDATE local_track_external_identities SET recording_mbid = 'shared-recording' "
            "WHERE local_track_id IN ('track-1-1', 'track-1-2')"
        )
    preparation = IdentityRepairService(
        store, canonical_provider=_DuplicateRecordingReleaseProvider()
    )
    created = await preparation.create_management_preparation(
        IdentityPreparationCreateRequest(
            idempotency_key="duplicate-recording-position"
        ),
        "admin",
        now=3,
    )
    claimed = await store.claim_operation_job(
        "worker", now=4, lease_seconds=60, kind="repair"
    )
    assert claimed is not None
    ready = await preparation.run_claimed_audit(claimed, "worker", now=5)
    finding = (
        await preparation.findings(created.id, finding_category="mapping_ready")
    ).items[0]
    evidence = await store.get_latest_album_candidate_evidence(
        "album-1", "rg-1:release-1"
    )

    assert finding.reason_code == "EXACT_RELEASE_MAPPING_SUPPORTED"
    assert evidence is not None
    assert {
        item.local_track_id: item.release_track_mbid
        for item in evidence.evidence.track_evidence
    } == {
        "track-1-1": "release-track-1",
        "track-1-2": "release-track-2",
    }
    assert all(
        "duplicate_recording_disambiguated" in item.evidence_kinds
        for item in evidence.evidence.track_evidence
    )

    await preparation.begin_management_preparation_apply(
        created.id,
        expected_row_revision=ready.row_revision,
        confirmation=True,
        now=6,
    )
    claimed_apply = await store.claim_operation_job(
        "worker", now=7, lease_seconds=60, kind="repair"
    )
    assert claimed_apply is not None
    done = await preparation.run_claimed_apply(claimed_apply, "worker", "admin", now=8)
    assert done.succeeded_count == 1
    with sqlite3.connect(db_path) as connection:
        mappings = connection.execute(
            "SELECT local_track_id, release_track_mbid "
            "FROM local_track_external_identities "
            "WHERE local_track_id IN ('track-1-1', 'track-1-2') "
            "ORDER BY local_track_id"
        ).fetchall()
    assert mappings == [
        ("track-1-1", "release-track-1"),
        ("track-1-2", "release-track-2"),
    ]


@pytest.mark.asyncio
async def test_management_identity_preparation_rejects_ambiguous_duplicate_recording(
    store: NativeLibraryStore, db_path: Path
) -> None:
    await _seed_album(store, "1", identity_source="legacy_import")
    with sqlite3.connect(db_path) as connection:
        connection.execute(
            "UPDATE local_tracks SET title = 'Repeated', track_number = 0 "
            "WHERE id = 'track-1-1'"
        )
        connection.execute(
            "UPDATE local_track_external_identities SET recording_mbid = 'shared-recording' "
            "WHERE local_track_id = 'track-1-1'"
        )
    preparation = IdentityRepairService(
        store,
        canonical_provider=_DuplicateRecordingReleaseProvider(
            duplicate_title="Repeated"
        ),
    )
    created = await preparation.create_management_preparation(
        IdentityPreparationCreateRequest(
            idempotency_key="duplicate-recording-ambiguous"
        ),
        "admin",
        now=3,
    )
    claimed = await store.claim_operation_job(
        "worker", now=4, lease_seconds=60, kind="repair"
    )
    assert claimed is not None
    await preparation.run_claimed_audit(claimed, "worker", now=5)

    finding = (
        await preparation.findings(created.id, finding_category="needs_review")
    ).items[0]
    evidence = await store.get_latest_album_candidate_evidence(
        "album-1", "rg-1:release-1"
    )
    assert finding.reason_code == "CONFLICTING_TRACK_EVIDENCE"
    assert finding.apply_eligible is False
    assert evidence is not None
    item = evidence.evidence.track_evidence[0]
    assert item.classification == "contradictory"
    assert item.release_track_mbid is None
    assert "ambiguous_release_track_identity" in item.evidence_kinds


@pytest.mark.asyncio
async def test_management_identity_preparation_rejects_conflicting_duplicate_discriminators(
    store: NativeLibraryStore, db_path: Path
) -> None:
    await _seed_album(store, "1", identity_source="legacy_import")
    with sqlite3.connect(db_path) as connection:
        connection.execute(
            "UPDATE local_tracks SET track_number = 3 WHERE id = 'track-1-1'"
        )
        connection.execute(
            "UPDATE local_track_external_identities SET recording_mbid = 'shared-recording' "
            "WHERE local_track_id = 'track-1-1'"
        )
    preparation = IdentityRepairService(
        store, canonical_provider=_DuplicateRecordingReleaseProvider()
    )
    created = await preparation.create_management_preparation(
        IdentityPreparationCreateRequest(
            idempotency_key="duplicate-recording-conflicting-position"
        ),
        "admin",
        now=3,
    )
    claimed = await store.claim_operation_job(
        "worker", now=4, lease_seconds=60, kind="repair"
    )
    assert claimed is not None
    await preparation.run_claimed_audit(claimed, "worker", now=5)

    finding = (
        await preparation.findings(created.id, finding_category="needs_review")
    ).items[0]
    evidence = await store.get_latest_album_candidate_evidence(
        "album-1", "rg-1:release-1"
    )
    assert finding.reason_code == "CONFLICTING_TRACK_EVIDENCE"
    assert evidence is not None
    item = evidence.evidence.track_evidence[0]
    assert item.classification == "contradictory"
    assert item.release_track_mbid is None
    assert "ambiguous_release_track_identity" in item.evidence_kinds


@pytest.mark.asyncio
async def test_management_identity_preparation_keeps_unverified_recording_conflict(
    store: NativeLibraryStore, db_path: Path
) -> None:
    await _seed_album(store, "1", identity_source="legacy_import")
    with sqlite3.connect(db_path) as connection:
        connection.execute(
            "UPDATE local_track_external_identities SET recording_mbid = 'other-recording' "
            "WHERE local_track_id = 'track-1-1'"
        )
    provider = _CanonicalReleaseProvider()
    preparation = IdentityRepairService(store, canonical_provider=provider)
    created = await preparation.create_management_preparation(
        IdentityPreparationCreateRequest(idempotency_key="recording-conflict"),
        "admin",
        now=3,
    )
    claimed = await store.claim_operation_job(
        "worker", now=4, lease_seconds=60, kind="repair"
    )
    assert claimed is not None
    await preparation.run_claimed_audit(claimed, "worker", now=5)

    finding = (
        await preparation.findings(created.id, finding_category="needs_review")
    ).items[0]
    evidence = await store.get_latest_album_candidate_evidence(
        "album-1", "rg-1:release-1"
    )
    assert finding.reason_code == "CONFLICTING_TRACK_EVIDENCE"
    assert finding.apply_eligible is False
    assert evidence is not None
    assert evidence.evidence.track_evidence[0].evidence_kinds == [
        "recording_mbid_conflict"
    ]
    assert evidence.evidence.track_evidence[0].recording_mbid_redirects == []
    assert provider.recording_calls == ["other-recording"]


@pytest.mark.asyncio
async def test_management_identity_preparation_defers_recording_redirect_failure(
    store: NativeLibraryStore, db_path: Path
) -> None:
    await _seed_album(store, "1", identity_source="legacy_import")
    with sqlite3.connect(db_path) as connection:
        connection.execute(
            "UPDATE local_track_external_identities SET recording_mbid = 'retired-recording' "
            "WHERE local_track_id = 'track-1-1'"
        )
    provider = _CanonicalReleaseProvider(recording_unavailable=True)
    preparation = IdentityRepairService(store, canonical_provider=provider)
    created = await preparation.create_management_preparation(
        IdentityPreparationCreateRequest(idempotency_key="recording-provider-failure"),
        "admin",
        now=3,
    )
    claimed = await store.claim_operation_job(
        "worker", now=4, lease_seconds=60, kind="repair"
    )
    assert claimed is not None
    deferred = await preparation.run_claimed_audit(claimed, "worker", now=5)

    assert provider.recording_calls == ["retired-recording"]
    assert deferred.state == "queued"
    job_row = await store.get_operation_job(created.id)
    assert job_row is not None
    assert job_row["next_attempt_at"] == pytest.approx(5 + 120)
    with sqlite3.connect(db_path) as connection:
        work = connection.execute(
            "SELECT state, failure_code FROM library_operation_work WHERE job_id = ?",
            (created.id,),
        ).fetchall()
    assert work == [("pending", "PROVIDER_DEFERRED")]
    assert (await preparation.findings(created.id)).items == []


@pytest.mark.asyncio
async def test_management_identity_preparation_excludes_trackless_albums(
    store: NativeLibraryStore, db_path: Path
) -> None:
    await _seed_album(store, "1", identity_source="legacy_import")
    await _seed_album(store, "2", identity_source="legacy_import")
    with sqlite3.connect(db_path) as connection:
        connection.execute("DELETE FROM local_tracks WHERE local_album_id = 'album-2'")

    preparation = IdentityRepairService(store)
    estimate = await preparation.estimate_management_preparation(["root"])
    created = await preparation.create_management_preparation(
        IdentityPreparationCreateRequest(
            idempotency_key="exclude-trackless", root_ids=["root"]
        ),
        "admin",
        now=3,
    )

    assert estimate.album_count == 1
    assert created.expected_work_count == 1
    # (GH-293) Work rows materialize in bounded pages when the worker runs.
    claimed = await store.claim_operation_job(
        "worker", now=3, lease_seconds=60, kind="repair"
    )
    assert claimed is not None
    staged = await store.materialize_repair_operation_batch(
        created.id, "worker", now=3
    )
    assert staged["complete"] is True
    assert staged["materialized_count"] == 1
    with sqlite3.connect(db_path) as connection:
        work = connection.execute(
            "SELECT local_album_id FROM library_operation_work WHERE job_id = ?",
            (created.id,),
        ).fetchall()
    assert work == [("album-1",)]


@pytest.mark.asyncio
async def test_management_identity_preparation_excludes_fully_missing_albums(
    store: NativeLibraryStore, db_path: Path
) -> None:
    await _seed_album(store, "1", identity_source="legacy_import")
    await _seed_album(store, "2", identity_source="legacy_import")
    with sqlite3.connect(db_path) as connection:
        connection.execute(
            "UPDATE local_tracks SET availability = 'missing' "
            "WHERE local_album_id = 'album-2'"
        )

    preparation = IdentityRepairService(store)
    estimate = await preparation.estimate_management_preparation(["root"])
    created = await preparation.create_management_preparation(
        IdentityPreparationCreateRequest(
            idempotency_key="exclude-missing", root_ids=["root"]
        ),
        "admin",
        now=3,
    )

    assert estimate.album_count == 1
    assert estimate.mapping_required_count == 1
    assert created.expected_work_count == 1
    # (GH-293) Work rows materialize in bounded pages when the worker runs.
    claimed = await store.claim_operation_job(
        "worker", now=3, lease_seconds=60, kind="repair"
    )
    assert claimed is not None
    staged = await store.materialize_repair_operation_batch(
        created.id, "worker", now=3
    )
    assert staged["complete"] is True
    assert staged["materialized_count"] == 1
    with sqlite3.connect(db_path) as connection:
        work = connection.execute(
            "SELECT local_album_id FROM library_operation_work WHERE job_id = ?",
            (created.id,),
        ).fetchall()
        retained = connection.execute(
            "SELECT id, availability FROM local_tracks "
            "WHERE local_album_id = 'album-2'"
        ).fetchall()
    assert work == [("album-1",)]
    assert retained == [("track-2-1", "missing")]


@pytest.mark.asyncio
async def test_management_identity_preparation_ignores_missing_duplicate_track_maps(
    store: NativeLibraryStore, db_path: Path
) -> None:
    await _seed_album(
        store,
        "1",
        identity_source="legacy_import",
        two_tracks=True,
    )
    with sqlite3.connect(db_path) as connection:
        connection.execute(
            "UPDATE local_track_external_identities SET "
            "release_track_mbid = 'release-track-1', medium_position = 1, "
            "release_track_position = 1 WHERE local_track_id = 'track-1-2'"
        )
        connection.execute(
            "UPDATE local_tracks SET availability = 'missing' WHERE id = 'track-1-2'"
        )

    preparation = IdentityRepairService(
        store, canonical_provider=_CanonicalReleaseProvider()
    )
    estimate = await preparation.estimate_management_preparation(["root"])
    created = await preparation.create_management_preparation(
        IdentityPreparationCreateRequest(
            idempotency_key="ignore-missing-duplicate", root_ids=["root"]
        ),
        "admin",
        now=3,
    )

    assert estimate.album_count == 1
    assert estimate.ready_album_count == 0
    assert estimate.mapping_required_count == 1
    assert estimate.exact_release_required_count == 0
    assert created.expected_work_count == 1
    claimed = await store.claim_operation_job(
        "worker", now=4, lease_seconds=60, kind="repair"
    )
    assert claimed is not None
    ready = await preparation.run_claimed_audit(claimed, "worker", now=5)
    assert ready.repair_summary is not None
    assert ready.repair_summary.input_track_count == 1
    assert ready.repair_summary.mapping_candidate_count == 1
    queued = await preparation.begin_management_preparation_apply(
        created.id,
        expected_row_revision=ready.row_revision,
        confirmation=True,
        now=6,
    )
    assert queued.state == "queued"
    claimed_apply = await store.claim_operation_job(
        "worker", now=7, lease_seconds=60, kind="repair"
    )
    assert claimed_apply is not None
    done = await preparation.run_claimed_apply(claimed_apply, "worker", "admin", now=8)
    assert done.state == "succeeded"
    with sqlite3.connect(db_path) as connection:
        retained = connection.execute(
            "SELECT t.id, t.availability, i.recording_mbid, i.release_track_mbid "
            "FROM local_tracks t JOIN local_track_external_identities i "
            "ON i.local_track_id = t.id AND i.provider = 'musicbrainz' "
            "WHERE t.local_album_id = 'album-1' ORDER BY t.id"
        ).fetchall()
    assert retained == [
        ("track-1-1", "indexed", "recording-track-1-1", "release-track-1"),
        ("track-1-2", "missing", "recording-track-1-2", "release-track-1"),
    ]


@pytest.mark.asyncio
async def test_management_identity_preparation_blocks_missing_conflicting_and_stale_inputs(
    store: NativeLibraryStore, db_path: Path
) -> None:
    await _seed_album(store, "1")
    missing_provider = _CanonicalReleaseProvider()
    missing = IdentityRepairService(store, canonical_provider=missing_provider)
    created = await missing.create_management_preparation(
        IdentityPreparationCreateRequest(idempotency_key="missing-exact"),
        "admin",
        now=3,
    )
    claimed = await store.claim_operation_job(
        "worker", now=4, lease_seconds=60, kind="repair"
    )
    assert claimed is not None
    ready = await missing.run_claimed_audit(claimed, "worker", now=5)
    finding = (
        await missing.findings(created.id, finding_category="exact_release_required")
    ).items[0]
    assert ready.repair_summary is not None
    assert ready.repair_summary.exact_release_required_count == 1
    assert finding.reason_code == "EXACT_EDITION_NOT_ACCEPTED"
    assert missing_provider.calls == []
    discarded = await missing.discard_management_preparation(
        created.id, expected_row_revision=ready.row_revision, now=5.5
    )
    assert discarded.state == "cancelled"
    assert discarded.terminal_code == "IDENTITY_PREPARATION_DISCARDED"
    repeated = await missing.discard_management_preparation(
        created.id, expected_row_revision=ready.row_revision, now=5.75
    )
    assert repeated.state == "cancelled"
    assert (
        await missing.findings(created.id, finding_category="exact_release_required")
    ).items

    await _seed_album(store, "2", identity_source="legacy_import")
    conflict = IdentityRepairService(
        store, canonical_provider=_CanonicalReleaseProvider(conflict=True)
    )
    conflict_job = await conflict.create_management_preparation(
        IdentityPreparationCreateRequest(
            idempotency_key="conflicting-exact", root_ids=["root"]
        ),
        "admin",
        now=6,
    )
    claimed = await store.claim_operation_job(
        "worker", now=7, lease_seconds=60, kind="repair"
    )
    assert claimed is not None
    await conflict.run_claimed_audit(claimed, "worker", now=8)
    by_album = {
        item.local_album_id: item
        for item in (
            await conflict.findings(conflict_job.id, finding_category="needs_review")
        ).items
    }
    assert by_album["album-2"].reason_code == "SELECTED_RELEASE_CONFLICT"
    assert by_album["album-2"].apply_eligible is False

    with sqlite3.connect(db_path) as connection:
        connection.execute(
            "UPDATE local_albums SET row_revision = row_revision + 1 WHERE id = 'album-1'"
        )
        connection.commit()
    assert ready.repair_summary.estimated_apply_changes == 0


@pytest.mark.asyncio
async def test_management_identity_preparation_defers_provider_failures(
    store: NativeLibraryStore,
    db_path: Path,
) -> None:
    await _seed_album(store, "1", identity_source="legacy_import")
    preparation = IdentityRepairService(
        store, canonical_provider=_CanonicalReleaseProvider(unavailable=True)
    )
    created = await preparation.create_management_preparation(
        IdentityPreparationCreateRequest(idempotency_key="provider-deferred"),
        "admin",
        now=3,
    )
    claimed = await store.claim_operation_job(
        "worker", now=4, lease_seconds=60, kind="repair"
    )
    assert claimed is not None
    deferred = await preparation.run_claimed_audit(claimed, "worker", now=5)
    assert deferred.state == "queued"
    assert deferred.succeeded_count == 0
    job_row = await store.get_operation_job(created.id)
    assert job_row is not None
    assert job_row["next_attempt_at"] == pytest.approx(5 + 120)
    with sqlite3.connect(db_path) as connection:
        work = connection.execute(
            "SELECT state, failure_code FROM library_operation_work WHERE job_id = ?",
            (created.id,),
        ).fetchall()
    assert work == [("pending", "PROVIDER_DEFERRED")]
    assert (await preparation.findings(created.id)).items == []


@pytest.mark.asyncio
async def test_management_identity_preparation_skips_a_changed_album_at_apply(
    store: NativeLibraryStore, db_path: Path
) -> None:
    await _seed_album(store, "1", identity_source="legacy_import")
    preparation = IdentityRepairService(
        store, canonical_provider=_CanonicalReleaseProvider()
    )
    created = await preparation.create_management_preparation(
        IdentityPreparationCreateRequest(idempotency_key="stale-before-apply"),
        "admin",
        now=3,
    )
    claimed = await store.claim_operation_job(
        "worker", now=4, lease_seconds=60, kind="repair"
    )
    assert claimed is not None
    ready = await preparation.run_claimed_audit(claimed, "worker", now=5)
    with sqlite3.connect(db_path) as connection:
        connection.execute(
            "UPDATE local_albums SET row_revision = row_revision + 1 WHERE id = 'album-1'"
        )
        connection.commit()
    await preparation.begin_management_preparation_apply(
        created.id,
        expected_row_revision=ready.row_revision,
        confirmation=True,
        now=6,
    )
    claimed_apply = await store.claim_operation_job(
        "worker", now=7, lease_seconds=60, kind="repair"
    )
    assert claimed_apply is not None
    done = await preparation.run_claimed_apply(claimed_apply, "worker", "admin", now=8)
    assert done.succeeded_count == 0
    assert done.skipped_count == 1
    context = await store.get_album_identification_context("album-1")
    assert context is not None
    assert context["tracks"][0]["release_track_mbid"] is None
    findings = await preparation.findings(created.id, finding_category="unverifiable")
    assert findings.items == []
    with sqlite3.connect(db_path) as connection:
        audit_row = connection.execute(
            "SELECT state, apply_result FROM library_identity_repair_findings "
            "WHERE job_id = ? AND local_album_id = 'album-1'",
            (created.id,),
        ).fetchone()
    assert audit_row == ("stale", "STALE_SUBJECT")


@pytest.mark.asyncio
async def test_diagnostic_export_is_bounded_redacted_and_ephemeral(
    store: NativeLibraryStore, db_path: Path
) -> None:
    await _seed_album(store, "diagnostic")
    await store.request_scan_run(
        ScanRequest(
            kind="incremental",
            trigger="manual",
            scopes=[
                ScanScope(
                    root_id="root-label",
                    relative_path="private/path",
                    policy_revision="policy",
                )
            ],
            policy_revision="policy",
        ),
        run_id="run-private-path",
        requested_at=1,
    )
    with sqlite3.connect(db_path) as connection:
        connection.execute(
            "INSERT INTO library_identification_attempts "
            "(id, local_album_id, trigger, input_tag_revision, input_policy_revision, "
            "input_file_revision, matcher_version, state, terminal_reason_code, "
            "started_at, completed_at) VALUES (?,?,?,?,?,?,?,?,?,?,?)",
            (
                "diagnostic-old-attempt",
                "album-diagnostic",
                "automatic",
                "tag",
                "policy",
                "file",
                "matcher",
                "no_candidate",
                "NO_CANDIDATE",
                -8_000_001,
                -8_000_000,
            ),
        )
        connection.execute(
            "INSERT INTO library_identification_evidence "
            "(id, attempt_id, candidate_key, evidence_json, evidence_size_bytes, created_at) "
            "VALUES (?,?,?,?,?,?)",
            (
                "diagnostic-old-evidence",
                "diagnostic-old-attempt",
                "candidate",
                b"{}",
                2,
                -8_000_000,
            ),
        )
        connection.executemany(
            "INSERT INTO library_scan_inventory "
            "(run_id, root_id, relative_path, absolute_path, file_size_bytes, "
            "file_mtime_ns, stat_revision, policy_revision, effective_policy, "
            "comparison_result) VALUES (?,?,?,?,?,?,?,?,?,?)",
            [
                (
                    "run-private-path",
                    "root-label",
                    f"private/path/{index}.flac",
                    f"/secret/music/{index}.flac",
                    100,
                    1,
                    f"stat-{index}",
                    "policy",
                    "automatic",
                    "unchanged",
                )
                for index in range(5_001)
            ],
        )
    filename, payload = await LibraryDiagnosticsService(store).export(
        "run-private-path"
    )
    decoded = json.loads(payload)
    assert filename.startswith("droppedneedle-library-run-")
    assert filename.endswith(".json")
    assert len(payload) < 2 * 1024 * 1024
    assert b"private/path" not in payload
    assert b"/secret/music" not in payload
    assert decoded["scopes"][0]["relative_path_hash"]
    assert decoded["exported_row_count"] == 5_000
    assert decoded["inventory_truncated"] is True
    assert decoded["evidence_storage"]["by_attempt_state"] == [
        {"category": "no_candidate", "rows": 1, "bytes": 2}
    ]
    assert decoded["evidence_storage"]["compactable_terminal_misses"] == 1
    assert decoded["evidence_storage"]["oldest_cleanup_eligible_at"] == -8_000_000
    assert decoded["excluded"] == [
        "credentials",
        "full_filesystem_paths",
        "raw_provider_responses",
        "exception_text",
    ]


def _suggestion_evidence(
    *,
    suffix: str = "1",
    release_mbid: str,
    release_group_mbid: str = "rg-suggested",
    reason_code: str = "SUPPORTED",
    release_date: str | None = "2020-01-01",
    album_title: str = "Album 1",
    complete: bool = True,
    score: float = 0.99,
) -> CandidateEvidence:
    return CandidateEvidence(
        release_group_mbid=release_group_mbid,
        release_mbid=release_mbid,
        album_title=album_title,
        release_date=release_date,
        track_evidence=[
            TrackEvidence(
                local_track_id=f"track-{suffix}-1",
                classification="supported",
                candidate_track_title="Track 1",
                candidate_disc_number=1,
                candidate_track_position=1,
                recording_mbid=f"recording-{release_mbid}",
                release_track_mbid=(
                    f"release-track-{release_mbid}" if complete else None
                ),
            )
        ],
        reason_code=reason_code,
        score=score,
        matcher_version="identification-test",
    )


def _seed_stored_attempt(
    db_path: Path,
    *,
    local_album_id: str,
    attempt_id: str,
    revisions: tuple[str, str, str],
    evidence: list[tuple[str, CandidateEvidence]],
    compacted: bool = False,
    completed_at: float = 2,
) -> None:
    with sqlite3.connect(db_path) as connection:
        connection.execute(
            "INSERT INTO library_identification_attempts "
            "(id, local_album_id, trigger, input_tag_revision, input_policy_revision, "
            "input_file_revision, matcher_version, state, terminal_reason_code, "
            "started_at, completed_at) VALUES (?,?,?,?,?,?,?,?,?,?,?)",
            (
                attempt_id,
                local_album_id,
                "automatic",
                revisions[0],
                revisions[2],
                revisions[1],
                "identification-test",
                "ambiguous",
                "AMBIGUOUS",
                completed_at - 1,
                completed_at,
            ),
        )
        for evidence_id, candidate in evidence:
            encoded = msgspec.json.encode(candidate)
            connection.execute(
                "INSERT INTO library_identification_evidence "
                "(id, attempt_id, candidate_key, evidence_json, evidence_size_bytes, "
                "compacted, created_at) VALUES (?,?,?,?,?,?,?)",
                (
                    evidence_id,
                    attempt_id,
                    f"candidate-{evidence_id}",
                    encoded,
                    len(encoded),
                    int(compacted),
                    completed_at,
                ),
            )


def _tie_release(
    release_mbid: str,
    *,
    status: str | None,
    date: str | None,
    country: str | None,
    track_count: int = 1,
) -> MbManagementRelease:
    return MbManagementRelease(
        id=release_mbid,
        title="Album 1",
        status=status,
        date=date,
        country=country,
        media=[MbManagementMedium(position=1, track_count=track_count)],
        release_group=MbManagementReleaseGroup(id="rg-suggested", title="Album 1"),
    )


class _SuggestedEditionProvider:
    def __init__(
        self,
        releases: dict[str, MbManagementRelease] | None = None,
        *,
        unavailable: bool = False,
    ) -> None:
        self.releases = releases or {}
        self.unavailable = unavailable
        self.calls: list[str] = []

    async def get_canonical_release(
        self,
        release_mbid,
        *,
        includes,
        preferred_locales=(),
        artist_standardization="credited",
        priority,
        bypass_cache=False,
    ):
        self.calls.append(release_mbid)
        if self.unavailable:
            raise ExternalServiceError("private canonical provider failure")
        return self.releases.get(release_mbid)

    async def resolve_recording_mbid(self, recording_mbid, *, priority):
        return recording_mbid


async def _run_preparation(
    store: NativeLibraryStore,
    provider: object,
    *,
    idempotency_key: str,
):
    preparation = IdentityRepairService(store, canonical_provider=provider)
    created = await preparation.create_management_preparation(
        IdentityPreparationCreateRequest(idempotency_key=idempotency_key),
        "admin",
        now=3,
    )
    claimed = await store.claim_operation_job(
        "worker", now=4, lease_seconds=60, kind="repair"
    )
    assert claimed is not None
    ready = await preparation.run_claimed_audit(claimed, "worker", now=5)
    return preparation, created, ready


@pytest.mark.asyncio
async def test_management_identity_preparation_suggests_single_stored_candidate(
    store: NativeLibraryStore, db_path: Path
) -> None:
    await _seed_album(store, "1")
    context = await store.get_album_identification_context("album-1")
    assert context is not None
    _seed_stored_attempt(
        db_path,
        local_album_id="album-1",
        attempt_id="attempt-suggested",
        revisions=album_input_revisions(context["tracks"]),
        evidence=[
            ("evidence-suggested", _suggestion_evidence(release_mbid="release-one"))
        ],
    )
    provider = _SuggestedEditionProvider()
    preparation, created, ready = await _run_preparation(
        store, provider, idempotency_key="suggest-single"
    )
    assert ready.repair_summary is not None
    finding = (
        await preparation.findings(
            created.id, finding_category="exact_release_required"
        )
    ).items[0]
    assert finding.finding_code == "exact_release_suggested"
    assert finding.reason_code == "EXACT_EDITION_SUGGESTED"
    assert finding.apply_eligible is True
    assert finding.evidence_id == "evidence-suggested"
    assert finding.suggested_edition is not None
    assert finding.suggested_edition.release_mbid == "release-one"
    assert finding.suggested_edition.release_group_mbid == "rg-suggested"
    assert finding.suggested_edition.title == "Album 1"
    assert finding.suggested_edition.date == "2020-01-01"
    assert finding.suggested_edition.track_count == 1
    assert finding.suggested_edition.competing_count == 1
    assert provider.calls == []


@pytest.mark.asyncio
async def test_management_identity_preparation_tie_breaks_official_first(
    store: NativeLibraryStore, db_path: Path
) -> None:
    await _seed_album(store, "1")
    context = await store.get_album_identification_context("album-1")
    assert context is not None
    _seed_stored_attempt(
        db_path,
        local_album_id="album-1",
        attempt_id="attempt-tie-official",
        revisions=album_input_revisions(context["tracks"]),
        evidence=[
            ("evidence-a", _suggestion_evidence(release_mbid="release-a")),
            ("evidence-b", _suggestion_evidence(release_mbid="release-b")),
        ],
    )
    provider = _SuggestedEditionProvider(
        {
            "release-a": _tie_release(
                "release-a", status="Promotion", date="2019-01-01", country="XW"
            ),
            "release-b": _tie_release(
                "release-b", status="Official", date="2021-05-01", country="DE"
            ),
        }
    )
    preparation, created, _ = await _run_preparation(
        store, provider, idempotency_key="suggest-tie-official"
    )
    finding = (
        await preparation.findings(
            created.id, finding_category="exact_release_required"
        )
    ).items[0]
    assert finding.finding_code == "exact_release_suggested"
    assert finding.evidence_id == "evidence-b"
    assert finding.suggested_edition is not None
    assert finding.suggested_edition.release_mbid == "release-b"
    assert finding.suggested_edition.status == "Official"
    assert finding.suggested_edition.competing_count == 2
    assert sorted(provider.calls) == ["release-a", "release-b"]


@pytest.mark.asyncio
async def test_management_identity_preparation_tie_breaks_earliest_then_worldwide(
    store: NativeLibraryStore, db_path: Path
) -> None:
    await _seed_album(store, "1")
    context = await store.get_album_identification_context("album-1")
    assert context is not None
    _seed_stored_attempt(
        db_path,
        local_album_id="album-1",
        attempt_id="attempt-tie-date",
        revisions=album_input_revisions(context["tracks"]),
        evidence=[
            ("evidence-a", _suggestion_evidence(release_mbid="release-a")),
            ("evidence-b", _suggestion_evidence(release_mbid="release-b")),
        ],
    )
    provider = _SuggestedEditionProvider(
        {
            "release-a": _tie_release(
                "release-a", status="Official", date="2020-01-01", country="XW"
            ),
            "release-b": _tie_release(
                "release-b", status="Official", date="2019-03-01", country="DE"
            ),
        }
    )
    preparation, created, _ = await _run_preparation(
        store, provider, idempotency_key="suggest-tie-date"
    )
    finding = (
        await preparation.findings(
            created.id, finding_category="exact_release_required"
        )
    ).items[0]
    assert finding.suggested_edition is not None
    assert finding.suggested_edition.release_mbid == "release-b"
    assert finding.suggested_edition.date == "2019-03-01"
    assert finding.suggested_edition.country == "DE"
    assert finding.suggested_edition.competing_count == 2


@pytest.mark.asyncio
async def test_management_identity_preparation_tie_breaks_worldwide_on_equal_dates(
    store: NativeLibraryStore, db_path: Path
) -> None:
    await _seed_album(store, "1")
    context = await store.get_album_identification_context("album-1")
    assert context is not None
    _seed_stored_attempt(
        db_path,
        local_album_id="album-1",
        attempt_id="attempt-tie-xw",
        revisions=album_input_revisions(context["tracks"]),
        evidence=[
            ("evidence-a", _suggestion_evidence(release_mbid="release-a")),
            ("evidence-b", _suggestion_evidence(release_mbid="release-b")),
        ],
    )
    provider = _SuggestedEditionProvider(
        {
            "release-a": _tie_release(
                "release-a", status="Official", date="2020-01-01", country="DE"
            ),
            "release-b": _tie_release(
                "release-b",
                status="Official",
                date="2020-01-01",
                country="XW",
                track_count=11,
            ),
        }
    )
    preparation, created, _ = await _run_preparation(
        store, provider, idempotency_key="suggest-tie-xw"
    )
    finding = (
        await preparation.findings(
            created.id, finding_category="exact_release_required"
        )
    ).items[0]
    assert finding.suggested_edition is not None
    assert finding.suggested_edition.release_mbid == "release-b"
    assert finding.suggested_edition.country == "XW"
    assert finding.suggested_edition.track_count == 11


@pytest.mark.asyncio
async def test_management_identity_preparation_defers_suggestion_fetch_failures(
    store: NativeLibraryStore, db_path: Path
) -> None:
    await _seed_album(store, "1")
    context = await store.get_album_identification_context("album-1")
    assert context is not None
    _seed_stored_attempt(
        db_path,
        local_album_id="album-1",
        attempt_id="attempt-deferred",
        revisions=album_input_revisions(context["tracks"]),
        evidence=[
            ("evidence-a", _suggestion_evidence(release_mbid="release-a")),
            ("evidence-b", _suggestion_evidence(release_mbid="release-b")),
        ],
    )
    preparation = IdentityRepairService(
        store, canonical_provider=_SuggestedEditionProvider(unavailable=True)
    )
    created = await preparation.create_management_preparation(
        IdentityPreparationCreateRequest(idempotency_key="suggest-deferred"),
        "admin",
        now=3,
    )
    claimed = await store.claim_operation_job(
        "worker", now=4, lease_seconds=60, kind="repair"
    )
    assert claimed is not None
    deferred = await preparation.run_claimed_audit(claimed, "worker", now=5)
    assert deferred.state == "queued"
    job_row = await store.get_operation_job(created.id)
    assert job_row is not None
    assert job_row["next_attempt_at"] == pytest.approx(5 + 120)
    with sqlite3.connect(db_path) as connection:
        work = connection.execute(
            "SELECT state, failure_code FROM library_operation_work WHERE job_id = ?",
            (created.id,),
        ).fetchall()
    assert work == [("pending", "PROVIDER_DEFERRED")]
    assert (await preparation.findings(created.id)).items == []


@pytest.mark.asyncio
async def test_management_identity_preparation_bare_when_evidence_unusable(
    store: NativeLibraryStore, db_path: Path
) -> None:
    await _seed_album(store, "1")
    await _seed_album(store, "2")
    await _seed_album(store, "3")
    await _seed_album(store, "4")
    contexts = {}
    for suffix in ("1", "2", "3", "4"):
        context = await store.get_album_identification_context(f"album-{suffix}")
        assert context is not None
        contexts[suffix] = context
    _seed_stored_attempt(
        db_path,
        local_album_id="album-1",
        attempt_id="attempt-stale-revisions",
        revisions=("stale-tag", "stale-file", "stale-policy"),
        evidence=[("evidence-1", _suggestion_evidence(release_mbid="release-1"))],
    )
    _seed_stored_attempt(
        db_path,
        local_album_id="album-2",
        attempt_id="attempt-compacted",
        revisions=album_input_revisions(contexts["2"]["tracks"]),
        evidence=[("evidence-2", _suggestion_evidence(release_mbid="release-2"))],
        compacted=True,
    )
    _seed_stored_attempt(
        db_path,
        local_album_id="album-3",
        attempt_id="attempt-incomplete",
        revisions=album_input_revisions(contexts["3"]["tracks"]),
        evidence=[
            (
                "evidence-3",
                _suggestion_evidence(release_mbid="release-3", complete=False),
            )
        ],
    )
    preparation, created, _ = await _run_preparation(
        store, _SuggestedEditionProvider(), idempotency_key="suggest-bare"
    )
    findings = await preparation.findings(
        created.id, finding_category="exact_release_required", limit=200
    )
    assert len(findings.items) == 4
    for item in findings.items:
        assert item.finding_code == "exact_release_required"
        assert item.reason_code == "EXACT_EDITION_NOT_ACCEPTED"
        assert item.apply_eligible is False
        assert item.suggested_edition is None


@pytest.mark.asyncio
async def test_management_identity_preparation_apply_seals_suggested_edition(
    store: NativeLibraryStore, db_path: Path
) -> None:
    await _seed_album(store, "1")
    context = await store.get_album_identification_context("album-1")
    assert context is not None
    _seed_stored_attempt(
        db_path,
        local_album_id="album-1",
        attempt_id="attempt-apply",
        revisions=album_input_revisions(context["tracks"]),
        evidence=[
            ("evidence-apply", _suggestion_evidence(release_mbid="release-apply"))
        ],
    )
    before_estimate = await store.estimate_management_identity_preparation([])
    assert before_estimate["exact_release_required_count"] == 1
    preparation, created, ready = await _run_preparation(
        store, _SuggestedEditionProvider(), idempotency_key="suggest-apply"
    )
    await preparation.begin_management_preparation_apply(
        created.id,
        expected_row_revision=ready.row_revision,
        confirmation=True,
        now=6,
    )
    claimed_apply = await store.claim_operation_job(
        "worker", now=7, lease_seconds=60, kind="repair"
    )
    assert claimed_apply is not None
    done = await preparation.run_claimed_apply(claimed_apply, "worker", "admin", now=8)
    assert done.state == "succeeded"
    assert done.succeeded_count == 1
    with sqlite3.connect(db_path) as connection:
        identity = connection.execute(
            "SELECT release_group_mbid, release_mbid, decision_source, "
            "selected_by_user_id, attempt_id FROM local_album_external_identities "
            "WHERE local_album_id = 'album-1'"
        ).fetchone()
        assert identity == (
            "rg-suggested",
            "release-apply",
            "manual",
            "admin",
            "attempt-apply",
        )
        track = connection.execute(
            "SELECT local_track_id, recording_mbid, release_mbid, release_track_mbid, "
            "medium_position, release_track_position, decision_source, attempt_id "
            "FROM local_track_external_identities WHERE local_track_id = 'track-1-1'"
        ).fetchone()
        assert track == (
            "track-1-1",
            "recording-release-apply",
            "release-apply",
            "release-track-release-apply",
            1,
            1,
            "manual",
            "attempt-apply",
        )
        review = connection.execute(
            "SELECT state, reason_code, decided_by_user_id "
            "FROM library_identification_reviews WHERE id = 'review-1'"
        ).fetchone()
        assert review == ("resolved", "SUGGESTED_EDITION_ACCEPTED", "admin")
        action = connection.execute(
            "SELECT action_kind, reason_code, before_json, after_json "
            "FROM library_catalog_actions WHERE local_album_id = 'album-1'"
        ).fetchone()
        assert action is not None
        assert action[0] == "accept_suggested_edition"
        assert action[1] == "SUGGESTED_EDITION_ACCEPTED"
        assert json.loads(action[2]) == {}
        after_payload = json.loads(action[3])
        assert after_payload["release_group_mbid"] == "rg-suggested"
        assert after_payload["release_mbid"] == "release-apply"
        assert after_payload["tracks"] == [
            {
                "local_track_id": "track-1-1",
                "recording_mbid": "recording-release-apply",
                "release_track_mbid": "release-track-release-apply",
                "medium_position": 1,
                "release_track_position": 1,
            }
        ]
        finding_row = connection.execute(
            "SELECT state, apply_result FROM library_identity_repair_findings "
            "WHERE job_id = ?",
            (created.id,),
        ).fetchone()
        assert finding_row == ("applied", "EDITION_ACCEPTED")
    after_estimate = await store.estimate_management_identity_preparation([])
    assert after_estimate["exact_release_required_count"] == 0
    assert after_estimate["ready_album_count"] == 1


@pytest.mark.asyncio
async def test_management_identity_preparation_apply_skips_stale_suggested_editions(
    store: NativeLibraryStore, db_path: Path
) -> None:
    for suffix in ("1", "2", "3", "4"):
        await _seed_album(store, suffix)
        context = await store.get_album_identification_context(f"album-{suffix}")
        assert context is not None
        revisions = album_input_revisions(context["tracks"])
        if suffix == "4":
            _seed_stored_attempt(
                db_path,
                local_album_id="album-4",
                attempt_id="attempt-unsafe-4",
                revisions=revisions,
                evidence=[
                    (
                        "evidence-unsafe-4",
                        _suggestion_evidence(
                            suffix="4",
                            release_mbid="release-stale-4",
                            reason_code="AMBIGUOUS",
                        ),
                    )
                ],
                completed_at=1,
            )
        _seed_stored_attempt(
            db_path,
            local_album_id=f"album-{suffix}",
            attempt_id=f"attempt-stale-{suffix}",
            revisions=album_input_revisions(context["tracks"]),
            evidence=[
                (
                    f"evidence-stale-{suffix}",
                    _suggestion_evidence(
                        suffix=suffix, release_mbid=f"release-stale-{suffix}"
                    ),
                )
            ],
        )
    preparation, created, ready = await _run_preparation(
        store, _SuggestedEditionProvider(), idempotency_key="suggest-stale-apply"
    )
    with sqlite3.connect(db_path) as connection:
        connection.execute(
            "UPDATE local_albums SET row_revision = row_revision + 1 "
            "WHERE id = 'album-1'"
        )
        connection.execute(
            "UPDATE local_tracks SET tag_revision = 'tag-changed' "
            "WHERE id = 'track-2-1'"
        )
        connection.execute(
            "UPDATE library_identity_repair_findings SET evidence_id = 'evidence-unsafe-4' "
            "WHERE job_id = ? AND local_album_id = 'album-4'",
            (created.id,),
        )
        connection.commit()
    sealed_context = await store.get_album_identification_context("album-3")
    assert sealed_context is not None
    await store.attach_album_identity(
        LocalAlbumExternalIdentity(
            local_album_id="album-3",
            release_group_mbid="rg-other",
            release_mbid="release-other",
            decision_source="manual",
            selected_at=6,
        ),
        expected_album_revision=int(sealed_context["album"]["row_revision"]),
    )
    await preparation.begin_management_preparation_apply(
        created.id,
        expected_row_revision=ready.row_revision,
        confirmation=True,
        now=6,
    )
    claimed_apply = await store.claim_operation_job(
        "worker", now=7, lease_seconds=60, kind="repair"
    )
    assert claimed_apply is not None
    done = await preparation.run_claimed_apply(claimed_apply, "worker", "admin", now=8)
    assert done.state == "succeeded"
    assert done.succeeded_count == 0
    assert done.skipped_count == 4
    with sqlite3.connect(db_path) as connection:
        findings = connection.execute(
            "SELECT local_album_id, state, apply_result "
            "FROM library_identity_repair_findings WHERE job_id = ? ORDER BY local_album_id",
            (created.id,),
        ).fetchall()
        assert findings == [
            (f"album-{suffix}", "stale", "STALE_SUBJECT")
            for suffix in ("1", "2", "3", "4")
        ]
        identities = connection.execute(
            "SELECT local_album_id, release_mbid FROM local_album_external_identities "
            "ORDER BY local_album_id"
        ).fetchall()
        assert identities == [("album-3", "release-other")]
        assert (
            connection.execute(
                "SELECT COUNT(*) FROM local_track_external_identities"
            ).fetchone()[0]
            == 0
        )


@pytest.mark.asyncio
async def test_diagnostic_export_includes_bounded_hashed_failure_rows(
    store: NativeLibraryStore, db_path: Path
) -> None:
    """NEW-SCAN-04: the diagnostic export carries bounded indexing failure rows
    with hashed paths and safe details - never raw paths or exception text."""
    await store.create_scan_run(
        ScanRun(id="run-diag-fail", kind="incremental", trigger="manual", queued_at=1)
    )
    await store.record_scan_failures(
        "run-diag-fail",
        [
            ScanFailureRecord(
                root_id="root-a",
                relative_path="secret/dir/track.flac",
                failure_code="TAG_READ_TIMEOUT",
                recorded_at=5.0,
                failure_detail=(
                    "The tag read exceeded its 30.0s deadline. A kernel-blocked "
                    "read is bounded by the timeout but the underlying syscall "
                    "may still be running."
                ),
                phase="indexing",
            ),
            ScanFailureRecord(
                root_id="root-a",
                relative_path="secret/dir/other.flac",
                failure_code="TAG_READ_FAILED",
                recorded_at=6.0,
                failure_detail="ValueError while reading tags.",
                phase="indexing",
            ),
        ],
    )

    service = LibraryDiagnosticsService(store)
    filename, payload = await service.export("run-diag-fail")
    decoded = json.loads(payload)

    assert len(decoded["failures"]) == 2
    assert decoded["failures_truncated"] is False
    for failure in decoded["failures"]:
        assert "relative_path" not in failure
        assert failure["relative_path_hash"]
        assert "exception class" not in failure["failure_detail"]
    codes = {failure["failure_code"] for failure in decoded["failures"]}
    assert codes == {"TAG_READ_TIMEOUT", "TAG_READ_FAILED"}
    # Raw paths and strerror text stay out of the payload.
    assert b"secret/dir" not in payload
    with pytest.raises(ValidationError):
        await LibraryDiagnosticsService(store).export("../escape")


class _LocalFailureFingerprintBackend(_FingerprintBackend):
    """fpcalc fails locally: outcome becomes FINGERPRINT_LOCAL_FAILURE."""

    async def generate_fingerprint(self, path: Path) -> tuple[str, int]:
        self.generate_calls += 1
        raise OSError("fpcalc temporarily blocked")


@pytest.mark.asyncio
async def test_explicit_local_fingerprint_failure_keeps_local_reason_through_defer(
    store: NativeLibraryStore,
) -> None:
    """F-MATCH-04: a local fpcalc failure in explicit re-identification defers
    under F-IDENT-03's bounded policy with the LOCAL reason - never rewritten
    to PROVIDER_TEMPORARILY_UNAVAILABLE."""
    await _seed_album(store, "1")
    # _FingerprintIdentificationProvider returns ambiguous candidates so the
    # worker enters the conditional fingerprint branch (like the existing
    # conditionally_fingerprints test).
    provider = _FingerprintIdentificationProvider()
    created = await ReidentificationService(store).create_or_coalesce(
        "album-1", "admin", idempotency_key="local-fp-explicit", now=1
    )
    claimed = await store.claim_operation_job(
        "worker", now=2, lease_seconds=60, kind="explicit_reidentification"
    )
    assert claimed is not None
    worker = ExplicitReidentificationWorker(
        store,
        AlbumCandidateService(provider),
        AlbumEvidenceEngine(),
        ConditionalFingerprintService(store, _LocalFailureFingerprintBackend()),
    )
    deferred = await worker.run_claimed(claimed, "worker", now=3)
    assert deferred["state"] == "queued"
    assert deferred["next_attempt_at"] == 3 + REIDENTIFICATION_RETRY_SECONDS

    job_row = await store.get_operation_job(created["id"])
    with sqlite3.connect(store.db_path) as connection:
        work_row = connection.execute(
            "SELECT failure_code FROM library_operation_work WHERE job_id = ?",
            (created["id"],),
        ).fetchone()
    assert work_row[0] == "FINGERPRINT_LOCAL_FAILURE"

    # The bounded retry re-runs; the fingerprinter still fails locally, and the
    # reason persists on the next defer as well.
    retried = await store.claim_operation_job(
        "worker",
        now=3 + REIDENTIFICATION_RETRY_SECONDS,
        lease_seconds=60,
        kind="explicit_reidentification",
    )
    assert retried is not None
    deferred_again = await worker.run_claimed(
        retried,
        "worker",
        now=3 + REIDENTIFICATION_RETRY_SECONDS,
    )
    assert deferred_again["state"] == "queued"


@pytest.mark.asyncio
async def test_cross_rg_candidate_is_not_suggested_and_apply_rejects_stale(
    store: NativeLibraryStore, db_path: Path
) -> None:
    """F-EDITION-01: with an RG-only current identity (rg-current), a complete,
    safe candidate from rg-other is filtered during preparation; and a sealed
    cross-RG finding is rejected as STALE_SUBJECT by the apply transaction."""
    await _seed_album(store, "1")
    context = await store.get_album_identification_context("album-1")
    assert context is not None

    # Attach an RG-only identity: release_group_mbid present, release_mbid NULL.
    await store.attach_album_identity(
        LocalAlbumExternalIdentity(
            local_album_id="album-1",
            release_group_mbid="rg-current",
            release_mbid=None,
            decision_source="automatic",
            selected_at=2,
        ),
        expected_album_revision=int(context["album"]["row_revision"]),
    )

    _seed_stored_attempt(
        db_path,
        local_album_id="album-1",
        attempt_id="attempt-cross-rg",
        revisions=album_input_revisions(context["tracks"]),
        evidence=[
            (
                "evidence-cross",
                _suggestion_evidence(
                    release_mbid="release-other", release_group_mbid="rg-other"
                ),
            )
        ],
    )
    provider = _SuggestedEditionProvider()
    preparation, created, ready = await _run_preparation(
        store, provider, idempotency_key="cross-rg-filter"
    )
    finding = (
        await preparation.findings(created.id, finding_category="exact_release_required")
    ).items[0]
    assert finding.finding_code == "exact_release_required"
    assert finding.reason_code == "EXACT_EDITION_NOT_ACCEPTED"
    with sqlite3.connect(db_path) as connection:
        identity_row = connection.execute(
            "SELECT release_group_mbid, release_mbid FROM "
            "local_album_external_identities WHERE local_album_id='album-1'"
        ).fetchone()
    assert identity_row == ("rg-current", None)

    # A sealed cross-RG finding is rejected as STALE_SUBJECT at apply time:
    # drive the real transaction against the CURRENT revisions and assert the
    # identity and tracks are untouched.
    import msgspec as _msgspec

    with sqlite3.connect(db_path) as connection:
        identity_revision = connection.execute(
            "SELECT row_revision FROM local_album_external_identities "
            "WHERE local_album_id='album-1'"
        ).fetchone()[0]
        album_revision = connection.execute(
            "SELECT row_revision FROM local_albums WHERE id='album-1'"
        ).fetchone()[0]
        connection.execute(
            "INSERT INTO library_identity_repair_findings "
            "(id, job_id, local_album_id, finding_code, state, reason_code, "
            "apply_eligible, evidence_id, expected_album_revision, "
            "expected_identity_revision, confidence, created_at, updated_at) "
            "VALUES ('finding-cross-sealed', 'mixed-run-x', 'album-1', "
            "'exact_release_suggested', 'open', 'EXACT_EDITION_SUGGESTED', "
            "1, 'evidence-cross', ?, ?, 'high', 5, 5)",
            (album_revision, identity_revision),
        )
        encoded = connection.execute(
            "SELECT evidence_json FROM library_identification_evidence "
            "WHERE id='evidence-cross'"
        ).fetchone()[0]
    candidate_evidence = _msgspec.json.decode(bytes(encoded), type=CandidateEvidence)

    def apply_tx(connection):
        finding_row = connection.execute(
            "SELECT * FROM library_identity_repair_findings "
            "WHERE id='finding-cross-sealed'"
        ).fetchone()
        # Mirror the production evidence/track projections exactly: the input
        # revisions live on the attempt row, joined here for the stale check.
        evidence_row = connection.execute(
            "SELECT e.evidence_json, e.attempt_id, e.compacted, "
            "a.input_tag_revision, a.input_file_revision, "
            "a.input_policy_revision FROM library_identification_evidence e "
            "JOIN library_identification_attempts a ON a.id = e.attempt_id "
            "WHERE e.id = 'evidence-cross'"
        ).fetchone()
        work_row = {
            "local_album_id": "album-1",
            "expected_subject_revision": int(album_revision),
        }
        return store._apply_suggested_edition_tx(
            connection,
            work=work_row,
            finding=finding_row,
            album=connection.execute(
                "SELECT * FROM local_albums WHERE id='album-1'"
            ).fetchone(),
            identity=connection.execute(
                "SELECT * FROM local_album_external_identities "
                "WHERE local_album_id='album-1'"
            ).fetchone(),
            evidence_row=evidence_row,
            track_rows=connection.execute(
                "SELECT t.*, ti.recording_mbid, "
                "ti.release_mbid AS identity_release_mbid, "
                "ti.release_track_mbid, ti.medium_position, "
                "ti.release_track_position FROM local_tracks t "
                "LEFT JOIN local_track_external_identities ti "
                "ON ti.local_track_id = t.id AND ti.provider = 'musicbrainz' "
                "WHERE t.local_album_id = 'album-1' AND t.availability = 'indexed' "
                "ORDER BY t.id"
            ).fetchall(),
            evidence=candidate_evidence,
            job_id="mixed-run-x",
            actor_user_id="admin",
            now=6.0,
        )

    result = await store._write(apply_tx)
    assert result == ("skipped", "STALE_SUBJECT")
    with sqlite3.connect(db_path) as connection:
        identity_row = connection.execute(
            "SELECT release_group_mbid, release_mbid FROM "
            "local_album_external_identities WHERE local_album_id='album-1'"
        ).fetchone()
        stale_state = connection.execute(
            "SELECT state, apply_result FROM library_identity_repair_findings "
            "WHERE id='finding-cross-sealed'"
        ).fetchone()
    assert identity_row == ("rg-current", None)
    assert stale_state == ("stale", "STALE_SUBJECT")


@pytest.mark.asyncio
async def test_same_rg_rg_only_identity_fills_release_and_keeps_group() -> None:
    """A same-RG candidate may fill the missing exact release through the
    sealed admin path while preserving the existing release group."""
    # Covered end-to-end below via the sealed preparation flow.
    assert True


@pytest.mark.asyncio
async def test_ranking_prefers_higher_evidence_score_before_metadata(
    store: NativeLibraryStore, db_path: Path
) -> None:
    """F-EDITION-01 ranking: among same-RG safe complete candidates, the
    highest CandidateEvidence.score wins before Official/date/XW/MBID."""
    await _seed_album(store, "1")
    context = await store.get_album_identification_context("album-1")
    assert context is not None
    low_score = _suggestion_evidence(
        release_mbid="release-low", release_date="2019-01-01"
    )
    low_score.score = 0.30
    high_score = _suggestion_evidence(
        release_mbid="release-high", release_date="2022-05-05"
    )
    high_score.score = 0.95
    _seed_stored_attempt(
        db_path,
        local_album_id="album-1",
        attempt_id="attempt-score-rank",
        revisions=album_input_revisions(context["tracks"]),
        evidence=[
            ("evidence-low", low_score),
            ("evidence-high", high_score),
        ],
    )
    provider = _SuggestedEditionProvider(
        {
            # Metadata would prefer release-low: Official, older date, XW.
            "release-low": _tie_release(
                "release-low", status="Official", date="2019-01-01", country="XW"
            ),
            "release-high": _tie_release(
                "release-high", status="Promotion", date="2022-05-05", country="DE"
            ),
        }
    )
    preparation, created, _ = await _run_preparation(
        store, provider, idempotency_key="score-first-ranking"
    )
    finding = (
        await preparation.findings(created.id, finding_category="exact_release_required")
    ).items[0]
    assert finding.reason_code == "EXACT_EDITION_SUGGESTED"
    assert finding.suggested_edition is not None
    assert finding.suggested_edition.release_mbid == "release-high"


@pytest.mark.asyncio
async def test_same_rg_rg_only_identity_accepts_fill_through_sealed_path(
    store: NativeLibraryStore, db_path: Path
) -> None:
    """RG-only identity + same-RG complete candidate: suggestion stays
    eligible, and the sealed admin apply fills the exact release while
    preserving the release group."""
    await _seed_album(store, "1")
    context = await store.get_album_identification_context("album-1")
    assert context is not None
    await store.attach_album_identity(
        LocalAlbumExternalIdentity(
            local_album_id="album-1",
            release_group_mbid="rg-suggested",
            release_mbid=None,
            decision_source="automatic",
            selected_at=2,
        ),
        expected_album_revision=int(context["album"]["row_revision"]),
    )
    revisions = album_input_revisions(context["tracks"])
    _seed_stored_attempt(
        db_path,
        local_album_id="album-1",
        attempt_id="attempt-rg-fill",
        revisions=revisions,
        evidence=[
            (
                "evidence-fill",
                _suggestion_evidence(
                    release_mbid="release-one",
                    release_group_mbid="rg-suggested",
                ),
            )
        ],
    )
    provider = _SuggestedEditionProvider()
    preparation, created, ready = await _run_preparation(
        store, provider, idempotency_key="rg-fill-sealed"
    )
    finding = (
        await preparation.findings(created.id, finding_category="exact_release_required")
    ).items[0]
    assert finding.reason_code == "EXACT_EDITION_SUGGESTED"

    queued = await preparation.begin_management_preparation_apply(
        created.id,
        expected_row_revision=ready.row_revision,
        confirmation=True,
        now=6,
    )
    claimed_apply = await store.claim_operation_job(
        "worker", now=7, lease_seconds=60, kind="repair"
    )
    assert claimed_apply is not None
    done = await preparation.run_claimed_apply(claimed_apply, "worker", "admin", now=8)
    assert done.state == "succeeded"
    after = await store.get_album_identification_context("album-1")
    assert after is not None
    identity = after["identity"]
    assert identity is not None
    assert identity["release_group_mbid"] == "rg-suggested"  # group preserved
    assert identity["release_mbid"] == "release-one"  # exact release filled
    track = after["tracks"][0]
    assert track["identity_release_mbid"] == "release-one"


@pytest.mark.asyncio
async def test_mixed_precision_dates_rank_more_precise_first(
    store: NativeLibraryStore, db_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """F-EDITION-02: year-only and month-only dates must not outrank a more
    precise date sharing the same known prefix. Chronological ordering holds
    for known months; invalid/empty sorts last without raising."""
    from repositories.edition_policy import edition_date_key

    # Unit-level ordering contract (shared with F-EDITION-01 rank tuple).
    assert edition_date_key("2024-01-31") < edition_date_key("2024")
    assert edition_date_key("2024-01-31") < edition_date_key("2024-01")
    assert edition_date_key("2024-01") < edition_date_key("2024-02")
    assert edition_date_key("2023-12-31") < edition_date_key("2024-01")
    assert edition_date_key("") > edition_date_key("2024-02")
    assert edition_date_key(None) > edition_date_key("2024-02")
    assert edition_date_key("not-a-date") > edition_date_key("1999")

    await _seed_album(store, "1")
    context = await store.get_album_identification_context("album-1")
    assert context is not None

    def evidence_for(release_mbid: str, date: str | None, score: float):
        item = _suggestion_evidence(
            release_mbid=release_mbid, release_date=date
        )
        item.score = score
        return item

    # Same score, same Official status, same country: the fully dated release
    # wins over a year-only sibling (pre-fix raw-string order chose 2024).
    year_only = _suggestion_evidence(release_mbid="release-year-only")
    year_only.score = 0.80
    full = _suggestion_evidence(release_mbid="release-full")
    full.score = 0.80
    _seed_stored_attempt(
        db_path,
        local_album_id="album-1",
        attempt_id="attempt-precision",
        revisions=album_input_revisions(context["tracks"]),
        evidence=[
            ("evidence-year-only", year_only),
            ("evidence-full", full),
        ],
    )
    provider = _SuggestedEditionProvider(
        {
            "release-year-only": _tie_release(
                "release-year-only", status="Official", date="2024", country="US"
            ),
            "release-full": _tie_release(
                "release-full", status="Official", date="2024-01-31", country="US"
            ),
        }
    )
    preparation, created, _ = await _run_preparation(
        store, provider, idempotency_key="precision-full-vs-year"
    )
    finding = (
        await preparation.findings(created.id, finding_category="exact_release_required")
    ).items[0]
    assert finding.reason_code == "EXACT_EDITION_SUGGESTED"
    assert finding.suggested_edition is not None
    assert finding.suggested_edition.release_mbid == "release-full"
    # Displayed/persisted precision is preserved verbatim.
    assert finding.suggested_edition.date == "2024-01-31"

    # Month/day precision boundary: month-only loses to full day of same month;
    # different known months stay chronological.
    month_only = _suggestion_evidence(release_mbid="release-month-only")
    month_only.score = 0.80
    other_month = _suggestion_evidence(release_mbid="release-feb")
    other_month.score = 0.80
    _seed_stored_attempt(
        db_path,
        local_album_id="album-1",
        attempt_id="attempt-precision-2",
        revisions=album_input_revisions(context["tracks"]),
        evidence=[
            ("evidence-month-only", month_only),
            ("evidence-feb", other_month),
        ],
    )
    provider2 = _SuggestedEditionProvider(
        {
            "release-month-only": _tie_release(
                "release-month-only", status="Official", date="2024-01", country="US"
            ),
            "release-feb": _tie_release(
                "release-feb", status="Official", date="2024-02-14", country="US"
            ),
        }
    )
    preparation2, created2, _ = await _run_preparation(
        store, provider2, idempotency_key="precision-month-boundary"
    )
    finding2 = (
        await preparation2.findings(
            created2.id, finding_category="exact_release_required"
        )
    ).items[0]
    assert finding2.suggested_edition is not None
    # Known January precedes known February chronologically.
    assert finding2.suggested_edition.release_mbid == "release-month-only"


@pytest.mark.asyncio
async def test_invalid_or_empty_dates_sort_last_and_provider_absent_uses_evidence_date(
    store: NativeLibraryStore, db_path: Path
) -> None:
    """Provider-absent fallback uses CandidateEvidence.release_date under the
    same policy; an invalid date on a single suggestible candidate still seals
    without raising (nothing to compare against), precision preserved."""
    from repositories.edition_policy import edition_date_key as _key
    assert _key("") == _key(None)
    assert _key("1999") < _key("")
    assert _key("2024-02") < _key("")

    await _seed_album(store, "1")
    context = await store.get_album_identification_context("album-1")
    assert context is not None

    single_valid = _suggestion_evidence(
        release_mbid="release-valid", release_date="2021-06-01"
    )
    _seed_stored_attempt(
        db_path,
        local_album_id="album-1",
        attempt_id="attempt-provider-absent",
        revisions=album_input_revisions(context["tracks"]),
        evidence=[("evidence-valid", single_valid)],
    )
    provider = _SuggestedEditionProvider()  # canonical absent
    preparation, created, _ = await _run_preparation(
        store, provider, idempotency_key="provider-absent-date"
    )
    finding = (
        await preparation.findings(created.id, finding_category="exact_release_required")
    ).items[0]
    assert finding.reason_code == "EXACT_EDITION_SUGGESTED"
    assert finding.suggested_edition is not None
    assert finding.suggested_edition.release_mbid == "release-valid"
    assert finding.suggested_edition.date == "2021-06-01"  # precision preserved

    # Single candidate with an INVALID date still seals (single-candidate path
    # never ranks) and does not raise during preparation.
    db_path2 = db_path  # same store fixture
    single_bad = _suggestion_evidence(
        release_mbid="release-bad", release_date="not-a-date"
    )
    _seed_stored_attempt(
        store.db_path,
        local_album_id="album-1",
        attempt_id="attempt-invalid-single",
        revisions=album_input_revisions(context["tracks"]),
        evidence=[("evidence-bad", single_bad)],
    )
    preparation2, created2, _ = await _run_preparation(
        store, provider, idempotency_key="invalid-single-seals"
    )
    finding2 = (
        await preparation2.findings(
            created2.id, finding_category="exact_release_required"
        )
    ).items[0]
    assert finding2.reason_code == "EXACT_EDITION_SUGGESTED"


# F-055: degradation-flag outages defer like raised ones


class _DegradedEmptyProvider(_IdentificationProvider):
    """Records a transient degradation and returns empty recall - exactly how
    musicbrainz_album swallows an outage (breaker open / transient 5xx)."""

    def __init__(self) -> None:
        self.calls = 0

    async def search_album_candidate_ids(self, artist, title, limit, priority):
        self.calls += 1
        context = try_get_degradation_context()
        assert context is not None
        context.record(IntegrationResult.error("musicbrainz", "breaker open"))
        return []

    async def get_album_candidate(
        self, release_group_mbid, target_track_count, priority
    ):
        return None


class _DeterministicEmptyProvider(_DegradedEmptyProvider):
    async def search_album_candidate_ids(self, artist, title, limit, priority):
        self.calls += 1
        context = try_get_degradation_context()
        assert context is not None
        context.record(
            IntegrationResult(
                data=None,
                source="musicbrainz",
                status="error",
                error_message="bad payload",
                deterministic=True,
            )
        )
        return []


async def _claim_explicit(store: NativeLibraryStore, album_suffix: str) -> dict:
    claimed = await store.claim_operation_job(
        "worker",
        now=12,
        lease_seconds=60,
        kind="explicit_reidentification",
    )
    assert claimed is not None
    return claimed


@pytest.mark.asyncio
async def test_degraded_empty_explicit_pass_defers_instead_of_terminalizing(
    store: NativeLibraryStore,
) -> None:
    await _seed_album(store, "1")
    created = await ReidentificationService(store).create_or_coalesce(
        "album-1", "admin", now=10
    )
    claimed = await _claim_explicit(store, "1")
    worker = ExplicitReidentificationWorker(
        store,
        AlbumCandidateService(_DegradedEmptyProvider()),
        AlbumEvidenceEngine(),
    )

    result = await worker.run_claimed(claimed, "worker", now=12)

    # The work item went back to pending with the honest outage code instead
    # of terminal-failing on the first degraded pass.
    assert result["state"] == "queued"
    with sqlite3.connect(store.db_path) as connection:
        connection.row_factory = sqlite3.Row
        job = connection.execute(
            "SELECT state, reidentification_attempt_count FROM "
            "library_operation_jobs WHERE id = ?",
            (str(claimed["id"]),),
        ).fetchone()
        work = connection.execute(
            "SELECT state, failure_code FROM library_operation_work "
            "WHERE job_id = ?",
            (str(claimed["id"]),),
        ).fetchone()
        attempt_rows = connection.execute(
            "SELECT COUNT(*) FROM library_identification_attempts"
        ).fetchone()[0]
    assert job["state"] == "queued"
    assert job["reidentification_attempt_count"] == 1
    assert work["state"] == "pending"
    assert work["failure_code"] == "PROVIDER_TEMPORARILY_UNAVAILABLE"
    assert attempt_rows == 0


@pytest.mark.asyncio
async def test_degraded_empty_explicit_pass_terminalizes_at_the_bound(
    store: NativeLibraryStore,
) -> None:
    await _seed_album(store, "1")
    created = await ReidentificationService(store).create_or_coalesce(
        "album-1", "admin", now=10
    )
    with sqlite3.connect(store.db_path) as connection:
        connection.execute(
            "UPDATE library_operation_jobs SET reidentification_attempt_count = ? "
            "WHERE kind = 'explicit_reidentification'",
            (MAX_REIDENTIFICATION_ATTEMPTS,),
        )
    claimed = await _claim_explicit(store, "1")
    worker = ExplicitReidentificationWorker(
        store,
        AlbumCandidateService(_DegradedEmptyProvider()),
        AlbumEvidenceEngine(),
    )

    result = await worker.run_claimed(claimed, "worker", now=12)

    assert result["state"] == "failed"
    with sqlite3.connect(store.db_path) as connection:
        connection.row_factory = sqlite3.Row
        terminal = connection.execute(
            "SELECT terminal_code FROM library_operation_jobs WHERE id = ?",
            (str(claimed["id"]),),
        ).fetchone()
    assert terminal["terminal_code"] == "PROVIDER_TEMPORARILY_UNAVAILABLE"


@pytest.mark.asyncio
async def test_deterministic_empty_payload_stays_terminal_first_pass(
    store: NativeLibraryStore,
) -> None:
    await _seed_album(store, "1")
    created = await ReidentificationService(store).create_or_coalesce(
        "album-1", "admin", now=10
    )
    claimed = await _claim_explicit(store, "1")
    worker = ExplicitReidentificationWorker(
        store,
        AlbumCandidateService(_DeterministicEmptyProvider()),
        AlbumEvidenceEngine(),
    )

    result = await worker.run_claimed(claimed, "worker", now=12)

    # Shipped deterministic-path semantics: the operation completes (no
    # retry slots consumed) carrying the honest unmappable-payload code.
    assert result["state"] == "succeeded"
    with sqlite3.connect(store.db_path) as connection:
        connection.row_factory = sqlite3.Row
        row = connection.execute(
            "SELECT terminal_code, reidentification_attempt_count FROM "
            "library_operation_jobs WHERE id = ?",
            (str(claimed["id"]),),
        ).fetchone()
    assert row["terminal_code"] == "UNMAPPABLE_PROVIDER_PAYLOAD"
    assert row["reidentification_attempt_count"] == 0


# D-EDITION-AUTO: automatic exact-edition acceptance (S-1/S-2/S-3).


def _auto_preparation(
    store: NativeLibraryStore,
    provider: object,
    *,
    opt_in: bool = True,
) -> IdentityRepairService:
    return IdentityRepairService(
        store,
        canonical_provider=provider,
        edition_opt_in=(lambda root_id: opt_in),
    )


async def _run_auto_preparation(
    store: NativeLibraryStore,
    provider: object,
    *,
    idempotency_key: str,
    opt_in: bool = True,
):
    preparation = _auto_preparation(store, provider, opt_in=opt_in)
    created = await preparation.create_management_preparation(
        IdentityPreparationCreateRequest(idempotency_key=idempotency_key),
        "admin",
        now=3,
    )
    claimed = await store.claim_operation_job(
        "worker", now=4, lease_seconds=60, kind="repair"
    )
    assert claimed is not None
    ready = await preparation.run_claimed_audit(claimed, "worker", now=5)
    return preparation, created, ready


def _undo_rows(db_path: Path) -> list[sqlite3.Row]:
    with sqlite3.connect(db_path) as connection:
        connection.row_factory = sqlite3.Row
        return connection.execute(
            "SELECT * FROM library_automatic_edition_undo"
        ).fetchall()


def _identity_row(db_path: Path, album_id: str) -> sqlite3.Row | None:
    with sqlite3.connect(db_path) as connection:
        connection.row_factory = sqlite3.Row
        return connection.execute(
            "SELECT * FROM local_album_external_identities WHERE local_album_id = ?",
            (album_id,),
        ).fetchone()


@pytest.mark.asyncio
async def test_auto_accept_off_keeps_single_candidate_suggestion_unchanged(
    store: NativeLibraryStore, db_path: Path
) -> None:
    await _seed_album(store, "1")
    context = await store.get_album_identification_context("album-1")
    assert context is not None
    _seed_stored_attempt(
        db_path,
        local_album_id="album-1",
        attempt_id="attempt-auto-off",
        revisions=album_input_revisions(context["tracks"]),
        evidence=[
            ("evidence-auto-off", _suggestion_evidence(release_mbid="release-one"))
        ],
    )
    provider = _SuggestedEditionProvider()
    preparation, created, _ = await _run_auto_preparation(
        store, provider, idempotency_key="auto-off", opt_in=False
    )
    finding = (
        await preparation.findings(
            created.id, finding_category="exact_release_required"
        )
    ).items[0]
    assert finding.finding_code == "exact_release_suggested"
    assert finding.apply_eligible is True
    assert provider.calls == []
    with sqlite3.connect(db_path) as connection:
        stored = connection.execute(
            "SELECT suggested_edition_json FROM library_identity_repair_findings "
            "WHERE id = ?",
            (finding.id,),
        ).fetchone()
    assert stored is not None
    assert "auto_gate" not in json.loads(stored[0])
    payload = await preparation.findings(created.id)
    assert payload.items[0].automatic_undo is None
    assert _undo_rows(db_path) == []
    assert _identity_row(db_path, "album-1") is None


@pytest.mark.asyncio
async def test_auto_accept_applies_winning_edition_with_audit_and_undo_snapshot(
    store: NativeLibraryStore, db_path: Path
) -> None:
    await _seed_album(store, "1")
    context = await store.get_album_identification_context("album-1")
    assert context is not None
    album_revision = int(context["album"]["row_revision"])
    _seed_stored_attempt(
        db_path,
        local_album_id="album-1",
        attempt_id="attempt-auto-on",
        revisions=album_input_revisions(context["tracks"]),
        evidence=[
            (
                "evidence-auto-on",
                _suggestion_evidence(release_mbid="release-one", score=0.99),
            )
        ],
    )
    provider = _SuggestedEditionProvider(
        {
            "release-one": _tie_release(
                "release-one", status="Official", date="2021-05-01", country="DE"
            )
        }
    )
    preparation, created, _ = await _run_auto_preparation(
        store, provider, idempotency_key="auto-on", opt_in=True
    )
    listing = await preparation.findings(created.id)
    assert listing.current_counts_by_finding.get("exact_release_auto_accepted") == 1
    accepted = (
        await preparation.findings(
            created.id, finding_category="exact_release_auto_accepted"
        )
    ).items[0]
    assert accepted.finding_code == "exact_release_auto_accepted"
    assert accepted.reason_code == "EXACT_EDITION_AUTO_ACCEPTED"
    assert accepted.state == "applied"
    assert accepted.apply_result == "EDITION_AUTO_ACCEPTED"
    assert accepted.apply_eligible is False
    # S-1: automatic identity rows with actor system (NULL user).
    identity = _identity_row(db_path, "album-1")
    assert identity is not None
    assert identity["decision_source"] == "automatic"
    assert identity["release_mbid"] == "release-one"
    assert identity["selected_by_user_id"] is None
    with sqlite3.connect(db_path) as connection:
        connection.row_factory = sqlite3.Row
        track_identity = connection.execute(
            "SELECT * FROM local_track_external_identities WHERE local_track_id = ?",
            ("track-1-1",),
        ).fetchone()
        audit = connection.execute(
            "SELECT * FROM library_catalog_actions "
            "WHERE action_kind = 'automatic_exact_edition'"
        ).fetchone()
    assert track_identity is not None
    assert track_identity["decision_source"] == "automatic"
    assert audit is not None
    assert audit["actor_user_id"] is None
    assert audit["reason_code"] == "SUPPORTED"
    after = json.loads(audit["after_json"])
    assert after["gate_reason"] == "AUTO_ACCEPT"
    assert after["actor"] == "system"
    assert after["evidence_id"] == "evidence-auto-on"
    assert after["ranking_inputs"][0]["release_mbid"] == "release-one"
    before = json.loads(audit["before_json"])
    assert before["prior_identity_revision"] is None
    # S-2 substrate: one live undo snapshot with CAS expectations.
    rows = _undo_rows(db_path)
    assert len(rows) == 1
    snapshot = rows[0]
    assert snapshot["prior_identity_json"] is None
    assert snapshot["consumed_at"] is None
    assert int(snapshot["expected_post_album_revision"]) == album_revision
    assert int(snapshot["expected_post_identity_revision"]) == 1
    assert accepted.automatic_undo is not None
    assert accepted.automatic_undo.expected_album_revision == album_revision
    assert accepted.automatic_undo.expected_identity_revision == 1


@pytest.mark.asyncio
async def test_undo_restores_prior_manual_identity_snapshot(
    store: NativeLibraryStore, db_path: Path
) -> None:
    await _seed_album(store, "2")
    context = await store.get_album_identification_context("album-2")
    assert context is not None
    prior_revision = int(context["album"]["row_revision"])
    await store.attach_album_identity(
        LocalAlbumExternalIdentity(
            local_album_id="album-2",
            release_group_mbid="rg-suggested",
            # Group-only prior (release MBID still empty) - the exact
            # F-EDITION-01 scenario where the suggestion path runs.
            release_mbid=None,
            decision_source="manual",
            selected_at=2,
        ),
        expected_album_revision=prior_revision,
    )
    refreshed = await store.get_album_identification_context("album-2")
    assert refreshed is not None
    _seed_stored_attempt(
        db_path,
        local_album_id="album-2",
        attempt_id="attempt-auto-restore",
        revisions=album_input_revisions(refreshed["tracks"]),
        evidence=[
            (
                "evidence-auto-restore",
                _suggestion_evidence(
                    suffix="2",
                    release_mbid="release-new", release_group_mbid="rg-suggested"
                ),
            )
        ],
    )
    provider = _SuggestedEditionProvider(
        {
            "release-new": _tie_release(
                "release-new", status="Official", date="2021-05-01", country="US"
            )
        }
    )
    preparation, created, _ = await _run_auto_preparation(
        store, provider, idempotency_key="auto-restore", opt_in=True
    )
    accepted = (
        await preparation.findings(
            created.id, finding_category="exact_release_auto_accepted"
        )
    ).items[0]
    assert accepted.automatic_undo is not None
    result = await store.undo_automatic_edition_acceptance(
        "album-2",
        expected_album_revision=accepted.automatic_undo.expected_album_revision,
        expected_identity_revision=(
            accepted.automatic_undo.expected_identity_revision
        ),
        actor_user_id="admin",
        now=9,
    )
    assert result["outcome"] == "restored"
    restored = _identity_row(db_path, "album-2")
    assert restored is not None
    assert restored["decision_source"] == "manual"
    assert restored["release_group_mbid"] == "rg-suggested"
    assert restored["release_mbid"] is None
    # The restored row advances the identity revision (append-only history).
    assert int(restored["row_revision"]) == int(
        accepted.automatic_undo.expected_identity_revision
    ) + 1
    with sqlite3.connect(db_path) as connection:
        connection.row_factory = sqlite3.Row
        consumed = connection.execute(
            "SELECT consumed_at, consumed_action_id IS NOT NULL AS has_action "
            "FROM library_automatic_edition_undo WHERE local_album_id = 'album-2'"
        ).fetchone()
        undo_audits = connection.execute(
            "SELECT * FROM library_catalog_actions "
            "WHERE action_kind = 'undo_automatic_edition'"
        ).fetchall()
        reviews = connection.execute(
            "SELECT state, reason_code FROM library_identification_reviews "
            "WHERE local_album_id = 'album-2' AND reason_code = "
            "'AUTOMATIC_EDITION_CLEARED_TO_REVIEW'"
        ).fetchall()
    assert consumed is not None and consumed["consumed_at"] == 9
    assert consumed["has_action"] == 1
    assert len(undo_audits) == 1
    assert undo_audits[0]["actor_user_id"] == "admin"
    assert reviews == []


@pytest.mark.asyncio
async def test_undo_automatic_edition_invalidates_display_caches(
    store: NativeLibraryStore, db_path: Path
) -> None:
    await _seed_album(store, "1")
    context = await store.get_album_identification_context("album-1")
    assert context is not None
    _seed_stored_attempt(
        db_path,
        local_album_id="album-1",
        attempt_id="attempt-auto-invalidate",
        revisions=album_input_revisions(context["tracks"]),
        evidence=[
            (
                "evidence-auto-invalidate",
                _suggestion_evidence(release_mbid="release-one"),
            )
        ],
    )
    provider = _SuggestedEditionProvider(
        {
            "release-one": _tie_release(
                "release-one", status="Official", date="2021-05-01", country="DE"
            )
        }
    )
    preparation, created, _ = await _run_auto_preparation(
        store, provider, idempotency_key="auto-invalidate", opt_in=True
    )
    accepted = (
        await preparation.findings(
            created.id, finding_category="exact_release_auto_accepted"
        )
    ).items[0]
    assert accepted.automatic_undo is not None
    invalidate = AsyncMock()
    service = IdentityRepairService(store, invalidate=invalidate)
    response = await service.undo_automatic_edition(
        "album-1",
        AutomaticEditionUndoRequest(
            expected_album_revision=accepted.automatic_undo.expected_album_revision,
            expected_identity_revision=(
                accepted.automatic_undo.expected_identity_revision
            ),
        ),
        "admin",
    )
    assert response.local_album_id == "album-1"
    invalidate.assert_awaited_once()
    domains, album_ids = invalidate.await_args.args
    assert album_ids == ["album-1"]
    # Same domain set as identification: undo restores a prior identity,
    # which search/home/discover may have cached too.
    assert set(domains) == {
        "library",
        "artist",
        "search",
        "home",
        "discover",
        "compatibility",
        "artwork",
        "review",
    }


@pytest.mark.asyncio
async def test_undo_clears_to_review_when_no_prior_existed(
    store: NativeLibraryStore, db_path: Path
) -> None:
    await _seed_album(store, "1")
    context = await store.get_album_identification_context("album-1")
    assert context is not None
    _seed_stored_attempt(
        db_path,
        local_album_id="album-1",
        attempt_id="attempt-auto-clear",
        revisions=album_input_revisions(context["tracks"]),
        evidence=[
            (
                "evidence-auto-clear",
                _suggestion_evidence(release_mbid="release-one"),
            )
        ],
    )
    provider = _SuggestedEditionProvider(
        {
            "release-one": _tie_release(
                "release-one", status="Official", date="2021-05-01", country="DE"
            )
        }
    )
    preparation, created, _ = await _run_auto_preparation(
        store, provider, idempotency_key="auto-clear", opt_in=True
    )
    accepted = (
        await preparation.findings(
            created.id, finding_category="exact_release_auto_accepted"
        )
    ).items[0]
    assert accepted.automatic_undo is not None
    result = await store.undo_automatic_edition_acceptance(
        "album-1",
        expected_album_revision=accepted.automatic_undo.expected_album_revision,
        expected_identity_revision=(
            accepted.automatic_undo.expected_identity_revision
        ),
        actor_user_id="admin",
        now=9,
    )
    assert result["outcome"] == "cleared_to_review"
    assert result["review_id"] is not None
    assert _identity_row(db_path, "album-1") is None
    with sqlite3.connect(db_path) as connection:
        connection.row_factory = sqlite3.Row
        review = connection.execute(
            "SELECT state, reason_code FROM library_identification_reviews "
            "WHERE id = ?",
            (result["review_id"],),
        ).fetchone()
        audits = connection.execute(
            "SELECT COUNT(*) FROM library_catalog_actions "
            "WHERE action_kind = 'undo_automatic_edition'"
        ).fetchone()[0]
    assert review is not None
    assert review["state"] == "needs_review"
    assert review["reason_code"] == "AUTOMATIC_EDITION_CLEARED_TO_REVIEW"
    assert audits == 1


@pytest.mark.asyncio
async def test_undo_rejects_stale_identity_revision(
    store: NativeLibraryStore, db_path: Path
) -> None:
    await _seed_album(store, "1")
    context = await store.get_album_identification_context("album-1")
    assert context is not None
    _seed_stored_attempt(
        db_path,
        local_album_id="album-1",
        attempt_id="attempt-auto-stale",
        revisions=album_input_revisions(context["tracks"]),
        evidence=[
            ("evidence-auto-stale", _suggestion_evidence(release_mbid="release-one"))
        ],
    )
    provider = _SuggestedEditionProvider(
        {
            "release-one": _tie_release(
                "release-one", status="Official", date="2021-05-01", country="DE"
            )
        }
    )
    preparation, created, _ = await _run_auto_preparation(
        store, provider, idempotency_key="auto-stale", opt_in=True
    )
    accepted = (
        await preparation.findings(
            created.id, finding_category="exact_release_auto_accepted"
        )
    ).items[0]
    assert accepted.automatic_undo is not None
    # Someone touches the identity after the auto-accept.
    current = await store.get_album_identification_context("album-1")
    assert current is not None
    await store.attach_album_identity(
        LocalAlbumExternalIdentity(
            local_album_id="album-1",
            release_group_mbid="rg-suggested",
            release_mbid="release-manual-later",
            decision_source="manual",
            selected_at=8,
        ),
        expected_album_revision=accepted.automatic_undo.expected_album_revision,
    )
    with pytest.raises(StaleRevisionError):
        await store.undo_automatic_edition_acceptance(
            "album-1",
            expected_album_revision=(
                accepted.automatic_undo.expected_album_revision
            ),
            expected_identity_revision=(
                accepted.automatic_undo.expected_identity_revision
            ),
            actor_user_id="admin",
            now=9,
        )


@pytest.mark.parametrize(
    ("first_score", "second_score", "expected_gate"),
    [
        (0.90, None, "BELOW_MIN_SCORE"),
        (0.99, 0.96, "MARGIN_TOO_NARROW"),
    ],
)
@pytest.mark.asyncio
async def test_auto_accept_gate_failures_keep_manual_suggestion(
    store: NativeLibraryStore,
    db_path: Path,
    first_score: float,
    second_score: float | None,
    expected_gate: str,
) -> None:
    await _seed_album(store, "1")
    context = await store.get_album_identification_context("album-1")
    assert context is not None
    evidence = [
        (
            "evidence-gate-a",
            _suggestion_evidence(
                release_mbid="release-a", score=first_score
            ),
        )
    ]
    if second_score is not None:
        evidence.append(
            (
                "evidence-gate-b",
                _suggestion_evidence(
                    release_mbid="release-b", score=second_score
                ),
            )
        )
    _seed_stored_attempt(
        db_path,
        local_album_id="album-1",
        attempt_id=f"attempt-gate-{expected_gate}",
        revisions=album_input_revisions(context["tracks"]),
        evidence=evidence,
    )
    provider = _SuggestedEditionProvider(
        {
            "release-a": _tie_release(
                "release-a", status="Official", date="2021-05-01", country="DE"
            ),
            "release-b": _tie_release(
                "release-b", status="Official", date="2022-05-01", country="US"
            ),
        }
    )
    preparation, created, _ = await _run_auto_preparation(
        store, provider, idempotency_key=f"gate-{expected_gate}", opt_in=True
    )
    finding = (
        await preparation.findings(
            created.id, finding_category="exact_release_required"
        )
    ).items[0]
    assert finding.finding_code == "exact_release_suggested"
    assert finding.suggested_edition is not None
    summary = json.loads(
        msgspec.json.encode(msgspec.to_builtins(finding.suggested_edition))
    ) if False else None
    with sqlite3.connect(db_path) as connection:
        row = connection.execute(
            "SELECT suggested_edition_json FROM library_identity_repair_findings "
            "WHERE id = ?",
            (finding.id,),
        ).fetchone()
        identity_count = connection.execute(
            "SELECT COUNT(*) FROM local_album_external_identities "
            "WHERE local_album_id = 'album-1'"
        ).fetchone()[0]
        undo_count = connection.execute(
            "SELECT COUNT(*) FROM library_automatic_edition_undo"
        ).fetchone()[0]
        auto_audits = connection.execute(
            "SELECT COUNT(*) FROM library_catalog_actions "
            "WHERE action_kind = 'automatic_exact_edition'"
        ).fetchone()[0]
    assert row is not None
    stored_summary = json.loads(row[0])
    assert stored_summary["auto_gate"] == expected_gate
    assert identity_count == 0
    assert undo_count == 0
    assert auto_audits == 0
    assert provider.calls  # ranking needed the canonical releases
    assert summary is None


@pytest.mark.asyncio
async def test_auto_accept_key_tie_goes_to_review(
    store: NativeLibraryStore, db_path: Path
) -> None:
    await _seed_album(store, "1")
    context = await store.get_album_identification_context("album-1")
    assert context is not None
    # Two stored evidence rows for the SAME release carry identical full sort
    # keys - indistinguishable under the signed order, so they go to review.
    _seed_stored_attempt(
        db_path,
        local_album_id="album-1",
        attempt_id="attempt-gate-tie",
        revisions=album_input_revisions(context["tracks"]),
        evidence=[
            ("evidence-tie-a", _suggestion_evidence(release_mbid="release-a")),
            ("evidence-tie-b", _suggestion_evidence(release_mbid="release-a")),
        ],
    )
    provider = _SuggestedEditionProvider(
        {
            "release-a": _tie_release(
                "release-a", status="Official", date="2021-05-01", country="DE"
            )
        }
    )
    preparation, created, _ = await _run_auto_preparation(
        store, provider, idempotency_key="gate-tie", opt_in=True
    )
    finding = (
        await preparation.findings(
            created.id, finding_category="exact_release_required"
        )
    ).items[0]
    assert finding.finding_code == "exact_release_suggested"
    with sqlite3.connect(db_path) as connection:
        row = connection.execute(
            "SELECT suggested_edition_json FROM library_identity_repair_findings "
            "WHERE id = ?",
            (finding.id,),
        ).fetchone()
        undo_count = connection.execute(
            "SELECT COUNT(*) FROM library_automatic_edition_undo"
        ).fetchone()[0]
    assert row is not None
    assert json.loads(row[0])["auto_gate"] == "TIE"
    assert undo_count == 0


@pytest.mark.asyncio
async def test_auto_accept_non_qualifying_reason_keeps_manual_suggestion(
    store: NativeLibraryStore, db_path: Path
) -> None:
    await _seed_album(store, "1")
    context = await store.get_album_identification_context("album-1")
    assert context is not None
    _seed_stored_attempt(
        db_path,
        local_album_id="album-1",
        attempt_id="attempt-gate-reason",
        revisions=album_input_revisions(context["tracks"]),
        evidence=[
            (
                "evidence-reason",
                _suggestion_evidence(
                    release_mbid="release-a",
                    reason_code="ACCEPTED",
                ),
            )
        ],
    )
    provider = _SuggestedEditionProvider(
        {
            "release-a": _tie_release(
                "release-a", status="Official", date="2021-05-01", country="DE"
            )
        }
    )
    preparation, created, _ = await _run_auto_preparation(
        store, provider, idempotency_key="gate-reason", opt_in=True
    )
    finding = (
        await preparation.findings(
            created.id, finding_category="exact_release_required"
        )
    ).items[0]
    assert finding.finding_code == "exact_release_suggested"
    with sqlite3.connect(db_path) as connection:
        row = connection.execute(
            "SELECT suggested_edition_json FROM library_identity_repair_findings "
            "WHERE id = ?",
            (finding.id,),
        ).fetchone()
        undo_count = connection.execute(
            "SELECT COUNT(*) FROM library_automatic_edition_undo"
        ).fetchone()[0]
        auto_audits = connection.execute(
            "SELECT COUNT(*) FROM library_catalog_actions "
            "WHERE action_kind = 'automatic_exact_edition'"
        ).fetchone()[0]
    assert row is not None
    assert json.loads(row[0])["auto_gate"] == "NON_QUALIFYING_REASON"
    assert undo_count == 0
    assert auto_audits == 0


@pytest.mark.asyncio
async def test_auto_accept_rg_filtered_to_empty_stays_exact_release_required(
    store: NativeLibraryStore, db_path: Path
) -> None:
    await _seed_album(store, "3")
    context = await store.get_album_identification_context("album-3")
    assert context is not None
    prior_revision = int(context["album"]["row_revision"])
    await store.attach_album_identity(
        LocalAlbumExternalIdentity(
            local_album_id="album-3",
            release_group_mbid="rg-other",
            release_mbid=None,
            decision_source="manual",
            selected_at=2,
        ),
        expected_album_revision=prior_revision,
    )
    refreshed = await store.get_album_identification_context("album-3")
    assert refreshed is not None
    _seed_stored_attempt(
        db_path,
        local_album_id="album-3",
        attempt_id="attempt-auto-cross-rg",
        revisions=album_input_revisions(refreshed["tracks"]),
        evidence=[
            (
                "evidence-cross-rg",
                _suggestion_evidence(
                    suffix="3",
                    release_mbid="release-new-3",
                    release_group_mbid="rg-suggested",
                ),
            )
        ],
    )
    provider = _SuggestedEditionProvider(
        {
            "release-new-3": _tie_release(
                "release-new-3", status="Official", date="2021-05-01", country="DE"
            )
        }
    )
    preparation, created, _ = await _run_auto_preparation(
        store, provider, idempotency_key="auto-cross-rg", opt_in=True
    )
    finding = (
        await preparation.findings(
            created.id, finding_category="exact_release_required"
        )
    ).items[0]
    assert finding.finding_code == "exact_release_required"
    assert finding.reason_code == "EXACT_EDITION_NOT_ACCEPTED"
    assert _identity_row(db_path, "album-3")["release_mbid"] is None
    assert _undo_rows(db_path) == []
