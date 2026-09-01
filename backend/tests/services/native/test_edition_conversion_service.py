import hashlib
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import AsyncMock, MagicMock

import pytest

from api.v1.schemas.album import AlbumTracksInfo
from core.exceptions import ValidationError
from models.album import AlbumInfo, Track
from models.edition_management import EditionConversionJob, EditionConversionTarget
from services.native.edition_conversion_service import EditionConversionService


RELEASE_GROUP = "00000000-0000-4000-8000-000000000001"
RELEASE = "00000000-0000-4000-8000-000000000002"


def _mbid(value: int) -> str:
    return f"00000000-0000-4000-8000-{value:012d}"


def _track(index: int, *, recording: str) -> dict:
    return {
        "id": f"local-{index}",
        "availability": "indexed",
        "file_size_bytes": 1_000,
        "row_revision": 1,
        "stat_revision": f"stat-{index}",
        "tag_revision": f"tag-{index}",
        "applied_policy_revision": "policy-1",
        "applied_policy": "managed",
        "disc_number": 1,
        "track_number": index,
        "recording_mbid": recording,
        "embedded_recording_mbid": None,
        "fingerprint_recording_mbid": None,
        "release_track_mbid": None,
        "embedded_release_track_mbid": None,
        "identity_release_mbid": None,
        "embedded_release_mbid": None,
        "identity_row_revision": 1,
    }


def _service(tmp_path: Path, *, source_ready: bool = True):
    store = AsyncMock()
    store.get_active_edition_conversion.return_value = None
    store.create_edition_conversion.side_effect = lambda job, targets, local_files: (
        EditionConversionJob(
            **{
                field: getattr(job, field)
                for field in EditionConversionJob.__struct_fields__
                if field not in {"targets", "local_files", "artifacts"}
            },
            targets=targets,
            local_files=local_files,
        )
    )
    albums = AsyncMock()
    preferences = MagicMock()
    preferences.is_download_source_ready.return_value = source_ready
    preferences.is_builtin_download_ready.return_value = source_ready
    service = EditionConversionService(
        store=store,
        album_service=albums,
        preferences=preferences,
        acquisition=AsyncMock(),
        download_store=AsyncMock(),
        get_download_service=lambda: AsyncMock(),
        get_free_music_service=lambda: AsyncMock(),
        automatic_management=AsyncMock(),
        fingerprinter=AsyncMock(),
        held_dir=tmp_path / "held",
        import_library=AsyncMock(),
        clock=lambda: 100.0,
    )
    return service, store, albums


@pytest.mark.asyncio
async def test_greetings_preflight_keeps_twenty_and_never_uses_conflicting_positions(
    tmp_path: Path,
) -> None:
    service, store, albums = _service(tmp_path)
    target_recordings = [_mbid(100 + index) for index in range(1, 25)]
    local_tracks = [
        _track(
            index,
            recording=(
                target_recordings[index - 1] if index <= 20 else _mbid(900 + index)
            ),
        )
        for index in range(1, 25)
    ]
    store.get_album_identification_context.return_value = {
        "album": {"row_revision": 1},
        "identity": {
            "row_revision": 1,
            "release_group_mbid": RELEASE_GROUP,
            "release_mbid": None,
            "decision_source": "manual",
        },
        "tracks": local_tracks,
    }
    albums.get_album_info.return_value = AlbumInfo(
        title="Greetings",
        musicbrainz_id=RELEASE_GROUP,
        artist_name="Artist",
        artist_id=_mbid(50),
    )
    albums.get_exact_edition_tracks_info.return_value = AlbumTracksInfo(
        tracks=[
            Track(
                position=index,
                title=f"Track {index}",
                recording_id=target_recordings[index - 1],
                release_track_id=_mbid(200 + index),
            )
            for index in range(1, 25)
        ],
        total_tracks=24,
        selected_release_mbid=RELEASE,
    )

    response = await service.create_preflight(
        local_album_id="album-1",
        release_group_mbid=RELEASE_GROUP,
        release_mbid=RELEASE,
        actor_user_id="admin",
    )

    assert response.kept_count == 20
    assert response.acquire_count == 4
    assert response.recycle_count == 4
    assert response.preflight_token
    assert [value.action for value in response.local_files].count("keep") == 20
    assert [value.action for value in response.local_files].count(
        "recycle_conflict"
    ) == 4
    assert all(value.state == "pending" for value in response.targets[20:])
    service._acquisition.request_track.assert_not_awaited()
    service._import_library.publish_import_bundle.assert_not_awaited()


def test_repeated_recordings_are_retained_only_by_unique_release_track_position() -> (
    None
):
    recording = _mbid(301)
    targets = (
        EditionConversionTarget(
            job_id="job",
            ordinal=0,
            disc_number=1,
            track_number=1,
            release_track_mbid=_mbid(401),
            recording_mbid=recording,
            title="Part one",
            duration_seconds=1,
            state="pending",
        ),
        EditionConversionTarget(
            job_id="job",
            ordinal=1,
            disc_number=2,
            track_number=1,
            release_track_mbid=_mbid(402),
            recording_mbid=recording,
            title="Part two",
            duration_seconds=1,
            state="pending",
        ),
    )
    first = _track(1, recording=recording)
    second = _track(1, recording=recording)
    second.update(
        id="local-2", disc_number=2, stat_revision="stat-2", tag_revision="tag-2"
    )

    matched = EditionConversionService._match_local_files(
        job_id="job", tracks=[first, second], targets=targets, release_mbid=RELEASE
    )

    assert [(value.local_track_id, value.target_ordinal) for value in matched] == [
        ("local-1", 0),
        ("local-2", 1),
    ]
    assert all(value.evidence_kind == "recording_and_position" for value in matched)


@pytest.mark.asyncio
async def test_start_blocks_acquisition_when_no_source_is_ready(tmp_path: Path) -> None:
    service, store, _albums = _service(tmp_path, source_ready=False)
    job = SimpleNamespace(
        id="job-1",
        acquire_count=1,
        preflight_token_hash=hashlib.sha256(b"secret").hexdigest(),
    )
    store.get_edition_conversion.return_value = job
    service._assert_current = AsyncMock()

    with pytest.raises(ValidationError, match="acquisition source"):
        await service.start(
            "job-1",
            preflight_token="secret",
            expected_row_revision=1,
            confirmation=True,
        )

    store.start_edition_conversion.assert_not_awaited()


@pytest.mark.parametrize(
    "fingerprint",
    [
        pytest.param(
            SimpleNamespace(status="no_match", recording_id=None),
            id="no-proof",
        ),
        pytest.param(
            SimpleNamespace(
                status="pass",
                recording_id="recording-other",
                recording_ids=["recording-other"],
            ),
            id="absent-expected-recording",
        ),
    ],
)
@pytest.mark.asyncio
async def test_final_preview_reverifies_a_retained_copy_with_acoustid(
    tmp_path: Path, fingerprint: SimpleNamespace
) -> None:
    service, store, _albums = _service(tmp_path)
    source = tmp_path / "retained.flac"
    source.write_bytes(b"retained audio")
    target = EditionConversionTarget(
        job_id="job-1",
        ordinal=0,
        disc_number=1,
        track_number=1,
        release_track_mbid=_mbid(401),
        recording_mbid=_mbid(301),
        title="Track",
        duration_seconds=1,
        state="kept",
        kept_local_track_id="local-1",
    )
    job = EditionConversionJob(
        id="job-1",
        local_album_id="album-1",
        target_release_group_mbid=RELEASE_GROUP,
        target_release_mbid=RELEASE,
        target_album_title="Album",
        target_artist_name="Artist",
        state="ready",
        expected_album_revision=1,
        expected_input_revision="input",
        expected_identity_revision="identity",
        preflight_token_hash="hash",
        download_source_ready=True,
        required_temporary_bytes=1,
        kept_count=1,
        acquire_count=0,
        recycle_count=0,
        staged_count=0,
        failed_count=0,
        final_preview_job_id=None,
        final_preview_token_hash=None,
        final_bundle_json=None,
        final_bundle_hash=None,
        requested_by_user_id="admin",
        error_code=None,
        created_at=1,
        updated_at=1,
        targets=(target,),
    )
    store.get_album_identification_context.return_value = {
        "album": {"row_revision": 1},
        "identity": None,
        "tracks": [
            {
                "id": "local-1",
                "availability": "indexed",
                "file_path": str(source),
            }
        ],
    }
    service._assert_current = AsyncMock()
    service._fingerprinter.fingerprint.return_value = fingerprint

    with pytest.raises(ValidationError, match="could not be verified"):
        await service._ensure_final_preview(job, preview_token="preview-token")

    service._fingerprinter.fingerprint.assert_awaited_once()
    store.stage_retained_edition_conversion_artifact.assert_not_awaited()
    assert list((tmp_path / "held").iterdir()) == []


@pytest.mark.asyncio
async def test_final_preview_accepts_expected_recording_after_selected_candidate(
    tmp_path: Path,
) -> None:
    service, store, _albums = _service(tmp_path)
    source = tmp_path / "retained.flac"
    source.write_bytes(b"retained audio")
    target = EditionConversionTarget(
        job_id="job-1",
        ordinal=0,
        disc_number=1,
        track_number=1,
        release_track_mbid=_mbid(401),
        recording_mbid=_mbid(301),
        title="Track",
        duration_seconds=1,
        state="kept",
        kept_local_track_id="local-1",
    )
    job = EditionConversionJob(
        id="job-1",
        local_album_id="album-1",
        target_release_group_mbid=RELEASE_GROUP,
        target_release_mbid=RELEASE,
        target_album_title="Album",
        target_artist_name="Artist",
        state="ready",
        expected_album_revision=1,
        expected_input_revision="input",
        expected_identity_revision="identity",
        preflight_token_hash="hash",
        download_source_ready=True,
        required_temporary_bytes=1,
        kept_count=1,
        acquire_count=0,
        recycle_count=0,
        staged_count=0,
        failed_count=0,
        final_preview_job_id=None,
        final_preview_token_hash=None,
        final_bundle_json=None,
        final_bundle_hash=None,
        requested_by_user_id="admin",
        error_code=None,
        created_at=1,
        updated_at=1,
        targets=(target,),
    )
    store.get_album_identification_context.return_value = {
        "album": {"row_revision": 1},
        "identity": None,
        "tracks": [
            {
                "id": "local-1",
                "availability": "indexed",
                "file_path": str(source),
            }
        ],
    }
    service._assert_current = AsyncMock()
    service._fingerprinter.fingerprint.return_value = SimpleNamespace(
        status="pass",
        recording_id="recording-other",
        recording_ids=["recording-other", target.recording_mbid],
    )

    class PreviewReachedReload(Exception):
        pass

    service._require = AsyncMock(side_effect=PreviewReachedReload)

    with pytest.raises(PreviewReachedReload):
        await service._ensure_final_preview(job, preview_token="preview-token")

    service._fingerprinter.fingerprint.assert_awaited_once()
    store.stage_retained_edition_conversion_artifact.assert_awaited_once()
