import asyncio
import hashlib
import json
import os
import shutil
import sqlite3
from pathlib import Path
from collections.abc import Callable
from unittest.mock import AsyncMock

import msgspec
import pytest

from api.v1.schemas.library_management import (
    NamingScriptSettings,
    PICARD_ORGANIZER_PROFILE_ID,
    settings_revision,
)
from api.v1.schemas.library_policies import LibraryRootSettings
from api.v1.schemas.library_management_preview import (
    LibraryManagementUndoPreviewRequest,
)
from core.exceptions import (
    AutomaticManagementHoldError,
    ConflictError,
    StaleRevisionError,
)
from infrastructure.audio.metadata_engine import (
    AudioMetadataEngine,
    legacy_audio_projection,
)
from infrastructure.library_management_blob_store import LibraryManagementBlobStore
from services.native.audio_write_planning_service import AudioWritePlanningService
from services.native.library_filesystem_coordinator import LibraryFilesystemCoordinator
from services.native.library_management_publisher import LibraryManagementPublisher
from services.native.library_management_undo_service import LibraryManagementUndoService
from services.native.library_policy_resolver import LibraryPolicyResolver
from services.native.target_import_library_service import TargetImportLibraryService
from models.audio import AudioTag
from models.audio_metadata import DesiredAudioDocument, DesiredAudioField
from models.library_management import (
    PATH_COLLISION_DIFFERENT,
    LibraryManagementImportArtifact,
    LibraryManagementImportBundle,
    LibraryManagementImportFile,
    LibraryManagementMetadataSnapshot,
)
from models.library_management_planning import LibraryManagementSelection
from repositories.musicbrainz_management_models import MbManagementRelease
from tests.services.native.test_library_management_planner import _configured, _planner
from tests.services.native.test_library_management_planner import (
    FIXTURES,
    _ArtworkRepository,
)


def _update_profile(preferences, update: Callable) -> None:
    current = preferences.get_library_management_settings()
    settings = preferences.get_library_management_settings_raw()
    profile = next(
        value for value in settings.profiles if value.id == PICARD_ORGANIZER_PROFILE_ID
    )
    update(settings, profile)
    preferences.save_library_management_settings_if_current(
        settings, expected_settings_revision=current.settings_revision
    )


def _import_file(
    audio: AudioMetadataEngine,
    source: Path,
    *,
    ordinal: int,
    relative_path: str,
) -> LibraryManagementImportFile:
    _existing_tag, info = legacy_audio_projection(audio.read(source))
    return LibraryManagementImportFile(
        ordinal=ordinal,
        input_path=str(source),
        destination_root_id="root-1",
        destination_relative_path=relative_path,
        tag=AudioTag(
            title=f"Track {ordinal + 1}",
            artist="Import Artist",
            album="Import Album",
            album_artist="Import Artist",
            track_number=ordinal + 1,
            year=2026,
            musicbrainz_release_group_id="import-rg",
            musicbrainz_release_id="import-release",
        ),
        info=info,
        release_group_mbid="import-rg",
        release_mbid="import-release",
        recording_mbid=None,
        confidence=0.9,
        source="download",
        source_path=source.name,
        download_task_id="task-1",
    )


def _same_path_configuration(_root, preferences, _store) -> None:
    def update(_settings, profile) -> None:
        profile.organization.rename_enabled = False
        profile.organization.move_enabled = False
        profile.organization.move_sidecars = False

    _update_profile(preferences, update)


def _keep_source_configuration(_root, preferences, _store) -> None:
    def update(_settings, profile) -> None:
        profile.organization.move_sidecars = False
        profile.organization.source_cleanup = "keep"

    _update_profile(preferences, update)


def _sidecar_configuration(root: Path, preferences, _store) -> None:
    (root / "disc.cue").write_text("FILE source.flac", encoding="utf-8")

    def update(_settings, profile) -> None:
        profile.organization.move_sidecars = True

    _update_profile(preferences, update)


def _external_artwork_configuration(_root, preferences, _store) -> None:
    def update(settings, profile) -> None:
        script = NamingScriptSettings(
            id="6e0e3245-8e5c-4202-acd8-41230c4ca09f",
            name="External artwork",
            source="{albumartist}/{album}/art-{artwork_type}.{artwork_extension}",
        )
        settings.naming_scripts.append(script)
        profile.artwork.embedded_enabled = False
        profile.artwork.external_enabled = True
        profile.artwork.providers = ["cover_art_archive_release"]
        profile.artwork.external_format = "png"
        profile.artwork.external_naming_script_id = script.id

    _update_profile(preferences, update)


def _add_second_album_track(root: Path, _preferences, store) -> None:
    source = root / "source2.flac"
    shutil.copy2(root / "source.flac", source)
    metadata = source.stat()
    with sqlite3.connect(store.db_path) as connection:
        connection.execute("PRAGMA foreign_keys=ON")
        connection.execute(
            "INSERT INTO local_tracks "
            "(id,local_album_id,root_id,file_path,relative_path,path_hash,"
            "file_size_bytes,file_mtime_ns,stat_revision,stat_revision_kind,tag_revision,"
            "title,title_folded,artist_name,artist_name_folded,album_title,"
            "album_title_folded,album_artist_name,album_artist_name_folded,disc_number,"
            "track_number,year,genre,genre_folded,file_format,ingest_source,imported_at,"
            "membership_source) VALUES "
            "('track-2','album-1','root-1',?,'source2.flac',?,?,?,?,"
            "'exact','tag-2','Second Track','second track','Alpha','alpha',"
            "'Management Album','management album','Alpha','alpha',1,2,2024,"
            "'Electronic','electronic','flac','scan',1,'automatic')",
            (
                str(source),
                hashlib.sha256(b"source2.flac").hexdigest(),
                metadata.st_size,
                metadata.st_mtime_ns,
                f"{metadata.st_size}:{metadata.st_mtime_ns}",
            ),
        )
        connection.execute(
            "INSERT INTO local_track_external_identities "
            "(local_track_id,provider,recording_mbid,release_mbid,release_track_mbid,"
            "medium_position,release_track_position,decision_source,selected_at) "
            "VALUES ('track-2','musicbrainz','55555555-5555-4555-8555-555555555555',"
            "'aff0622e-7bd3-4fb6-9ca3-0fa19dd2340b',"
            "'66666666-6666-4666-8666-666666666666',1,2,'manual',1)"
        )


def _nest_source(root: Path, _preferences, store) -> None:
    nested = root / "incoming"
    nested.mkdir()
    source = nested / "source.flac"
    (root / "source.flac").replace(source)
    with sqlite3.connect(store.db_path) as connection:
        connection.execute(
            "UPDATE local_tracks SET file_path=?,relative_path='incoming/source.flac',"
            "path_hash=? WHERE id='track-1'",
            (str(source), hashlib.sha256(b"incoming/source.flac").hexdigest()),
        )


def _add_second_canonical_track(planner) -> None:
    payload = json.loads(
        (FIXTURES / "musicbrainz" / "management_release.json").read_text(
            encoding="utf-8"
        )
    )
    second = json.loads(json.dumps(payload["media"][0]["tracks"][0]))
    second["id"] = "66666666-6666-4666-8666-666666666666"
    second["position"] = 2
    second["number"] = "A2"
    second["title"] = "Variation 1"
    second["recording"]["id"] = "55555555-5555-4555-8555-555555555555"
    second["recording"]["title"] = "Variation 1"
    payload["media"][0]["tracks"].append(second)
    planner._canonical._musicbrainz.get_canonical_release.return_value = (
        msgspec.json.decode(json.dumps(payload).encode(), type=MbManagementRelease)
    )


async def _ready_apply_operation(
    tmp_path: Path,
    *,
    configure: Callable | None = None,
    prepare_store: Callable | None = None,
    customize_planner: Callable | None = None,
    artwork_repository=None,
    target_root_id: str | None = None,
    selection: LibraryManagementSelection | None = None,
):
    root, source, preferences, store, _settings_revision, _policy_revision = (
        _configured(tmp_path)
    )
    if configure is not None:
        configure(root, preferences, store)
    if prepare_store is not None:
        prepare_store(root, preferences, store)
    settings_revision = preferences.get_library_management_settings().settings_revision
    policy_revision = LibraryPolicyResolver(
        preferences.get_typed_library_settings_raw()
    ).policy_revision
    planner = _planner(
        tmp_path,
        store,
        preferences,
        artwork_repository=artwork_repository,
    )
    if customize_planner is not None:
        customize_planner(planner)
    handle = await planner.create_preview(
        selection=selection
        or LibraryManagementSelection(kind="tracks", ids=("track-1",)),
        profile_id=PICARD_ORGANIZER_PROFILE_ID,
        expected_settings_revision=settings_revision,
        expected_policy_revision=policy_revision,
        actor_user_id="admin",
        idempotency_key="publisher-preview",
        target_root_id=target_root_id,
    )
    claimed = await store.claim_operation_job(
        "preview-worker", now=100, lease_seconds=60, kind="library_management"
    )
    assert claimed is not None
    await planner.run_claimed_preview(claimed, "preview-worker")
    with sqlite3.connect(tmp_path / "library.db") as connection:
        connection.execute(
            "UPDATE library_operation_jobs SET state='running',lease_owner='apply-worker',"
            "lease_expires_at=200,heartbeat_at=110,expected_work_count=1 WHERE id=?",
            (handle.job_id,),
        )
        connection.execute(
            "UPDATE library_management_job_snapshots SET mode='apply',phase='applying' "
            "WHERE job_id=?",
            (handle.job_id,),
        )
        connection.execute(
            "INSERT INTO library_operation_work "
            "(job_id,ordinal,local_album_id,expected_subject_revision,"
            "expected_input_revision,action,idempotency_key,state,updated_at) "
            "VALUES (?,0,'album-1',1,?,'library_management',?,'running',110)",
            (handle.job_id, settings_revision, f"{handle.job_id}:bundle:0"),
        )
    audio = AudioMetadataEngine()
    publisher = LibraryManagementPublisher(
        store,
        preferences,
        audio,
        AudioWritePlanningService(audio),
        LibraryManagementBlobStore(tmp_path / "blobs", store),
        LibraryFilesystemCoordinator(),
        clock=lambda: 110.0,
    )
    return root, source, store, audio, publisher, handle.job_id


def _import_publication_fixture(tmp_path: Path):
    root, catalog_source, preferences, store, _settings, policy_revision = _configured(
        tmp_path
    )
    audio = AudioMetadataEngine()
    filesystem = LibraryFilesystemCoordinator()
    publisher = LibraryManagementPublisher(
        store,
        preferences,
        audio,
        AudioWritePlanningService(audio),
        LibraryManagementBlobStore(tmp_path / "import-blobs", store),
        filesystem,
        clock=lambda: 110.0,
    )
    service = TargetImportLibraryService(
        store,
        lambda: LibraryPolicyResolver(preferences.get_typed_library_settings_raw()),
        AsyncMock(),
        filesystem_coordinator=filesystem,
        management_publisher=publisher,
    )
    return root, catalog_source, store, audio, publisher, service, policy_revision


@pytest.mark.asyncio
async def test_import_bundle_publishes_once_and_commits_catalog_atomically(
    tmp_path: Path,
) -> None:
    root, catalog_source, store, audio, _publisher, service, policy_revision = (
        _import_publication_fixture(tmp_path)
    )
    incoming = tmp_path / "incoming.flac"
    shutil.copy2(catalog_source, incoming)
    request = _import_file(
        audio,
        incoming,
        ordinal=0,
        relative_path="Import Artist/Import Album/01 Track.flac",
    )
    bundle = LibraryManagementImportBundle(
        idempotency_key="acquisition:task-1:minimal",
        origin="acquisition",
        policy_revision=policy_revision,
        files=(request,),
    )

    first = await service.publish_import_bundle(bundle)
    repeated = await service.publish_import_bundle(bundle)

    destination = root / request.destination_relative_path
    row = await store.get_target_track_by_path(str(destination))
    journals = await store.list_library_management_import_journals(first.bundle_id)
    barriers = await store.list_acquisition_import_bundles_for_download_task("task-1")
    assert destination.is_file()
    assert incoming.exists() is False
    assert row is not None and row["download_task_id"] == "task-1"
    assert first.paths == repeated.paths == (str(destination),)
    assert first.local_track_ids == repeated.local_track_ids
    assert repeated.repeated is True
    assert [value.state for value in journals] == ["completed"]
    assert [value.id for value in barriers] == [first.bundle_id]


@pytest.mark.asyncio
async def test_import_album_catalog_commit_adopts_every_track_in_one_revision(
    tmp_path: Path,
) -> None:
    root, catalog_source, store, audio, _publisher, service, policy_revision = (
        _import_publication_fixture(tmp_path)
    )
    sources = [tmp_path / "album-1.flac", tmp_path / "album-2.flac"]
    for source in sources:
        shutil.copy2(catalog_source, source)
    bundle = LibraryManagementImportBundle(
        idempotency_key="acquisition:album-atomic:minimal",
        origin="acquisition",
        policy_revision=policy_revision,
        files=tuple(
            _import_file(
                audio,
                source,
                ordinal=ordinal,
                relative_path=f"Import Artist/Import Album/0{ordinal + 1} Track.flac",
            )
            for ordinal, source in enumerate(sources)
        ),
    )
    before_revision = await store.get_catalog_revision()

    result = await service.publish_import_bundle(bundle)

    assert len(result.local_track_ids) == 2
    assert await store.get_catalog_revision() == before_revision + 1
    assert all(
        (root / value.destination_relative_path).is_file() for value in bundle.files
    )


@pytest.mark.asyncio
async def test_automatic_import_commits_identity_baseline_undo_and_history(
    tmp_path: Path,
) -> None:
    root, catalog_source, preferences, store, _settings, policy_revision = _configured(
        tmp_path
    )
    audio = AudioMetadataEngine()
    filesystem = LibraryFilesystemCoordinator()
    blobs = LibraryManagementBlobStore(tmp_path / "automatic-import-blobs", store)
    publisher = LibraryManagementPublisher(
        store,
        preferences,
        audio,
        AudioWritePlanningService(audio),
        blobs,
        filesystem,
        clock=lambda: 110.0,
    )
    identification_queue = AsyncMock()
    service = TargetImportLibraryService(
        store,
        lambda: LibraryPolicyResolver(preferences.get_typed_library_settings_raw()),
        identification_queue,
        filesystem_coordinator=filesystem,
        management_publisher=publisher,
    )
    incoming = tmp_path / "automatic-incoming.flac"
    shutil.copy2(catalog_source, incoming)
    original_snapshot = audio.snapshot(incoming)
    incoming_sidecar = tmp_path / "album.cue"
    incoming_sidecar.write_text("FILE original.flac WAVE", encoding="utf-8")
    sidecar_fingerprint = hashlib.sha256(incoming_sidecar.read_bytes()).hexdigest()
    artwork_content = b"generated-artwork"
    request = _import_file(
        audio,
        incoming,
        ordinal=0,
        relative_path="Managed Artist/Managed Album/01 Managed.flac",
    )
    management = preferences.get_library_management_settings_raw()
    profile = next(
        value
        for value in management.profiles
        if value.id == PICARD_ORGANIZER_PROFILE_ID
    )
    pinned = _planner(tmp_path, store, preferences).pin_profile(management, profile)
    metadata_snapshot = await store.put_management_metadata_snapshot(
        LibraryManagementMetadataSnapshot(
            id="automatic-metadata-snapshot",
            provider="musicbrainz",
            entity_kind="release",
            entity_id="import-release",
            input_hash="a" * 64,
            canonical_payload_json="{}",
            payload_sha256=hashlib.sha256(b"{}").hexdigest(),
            fetched_at=100.0,
        )
    )
    request = msgspec.structs.replace(
        request,
        authoritative_mapping=True,
        recording_mbid="recording-1",
        release_track_mbid="release-track-1",
        medium_position=1,
        release_track_position=1,
        baseline_relative_path="Incoming/01 Original.flac",
        desired_document=DesiredAudioDocument(
            fields=(DesiredAudioField(name="title", action="set", value="Managed"),)
        ),
        pinned_profile=pinned,
        metadata_snapshot_id=metadata_snapshot.id,
        projection_hash="c" * 64,
        settings_revision=settings_revision(management),
        undo_retention_days=management.undo_retention_days,
        management_warnings=("genre:listenbrainz",),
        artifacts=(
            LibraryManagementImportArtifact(
                kind="external_art",
                destination_root_id="root-1",
                destination_relative_path="Managed Artist/Managed Album/cover.jpg",
                content=artwork_content,
                source_fingerprint=hashlib.sha256(artwork_content).hexdigest(),
            ),
            LibraryManagementImportArtifact(
                kind="sidecar",
                destination_root_id="root-1",
                destination_relative_path="Managed Artist/Managed Album/album.cue",
                source_path=str(incoming_sidecar),
                source_fingerprint=sidecar_fingerprint,
            ),
        ),
    )
    bundle = LibraryManagementImportBundle(
        idempotency_key="acquisition:automatic-identity",
        origin="acquisition",
        policy_revision=policy_revision,
        files=(request,),
    )
    _update_profile(
        preferences,
        lambda _settings, value: setattr(
            value, "description", "Changed after automatic preparation"
        ),
    )

    result = await service.publish_import_bundle(bundle)

    track_id = result.local_track_ids[0]
    baseline = await store.get_management_baseline(track_id)
    state = await store.get_track_management_state(track_id)
    identity = await store.get_accepted_library_management_identity(
        (await store.get_target_track(track_id))["local_album_id"],
        local_track_ids=(track_id,),
    )
    operations = await store.list_library_management_operations(limit=10)
    operation_snapshot = await store.get_library_management_job_snapshot(
        str(operations[0]["id"])
    )
    assert (root / request.destination_relative_path).is_file()
    assert baseline is not None
    assert baseline.original_relative_path == "Incoming/01 Original.flac"
    ancillary = json.loads(baseline.ancillary_snapshot_json)
    assert {value["kind"] for value in ancillary} == {"external_art", "sidecar"}
    assert (
        root / "Managed Artist/Managed Album/cover.jpg"
    ).read_bytes() == artwork_content
    assert (root / "Managed Artist/Managed Album/album.cue").read_text() == (
        "FILE original.flac WAVE"
    )
    assert not incoming_sidecar.exists()
    assert state is not None and state.applied_projection_hash == "c" * 64
    assert identity is not None
    assert identity.release_mbid == "import-release"
    assert identity.tracks[0].release_track_mbid == "release-track-1"
    assert operations[0]["management_mode"] == "automatic_apply"
    assert operations[0]["management_origin"] == "acquisition"
    assert operation_snapshot is not None
    assert json.loads(operation_snapshot.warnings_json) == ["genre:listenbrainz"]
    identification_queue.enqueue_album.assert_not_awaited()

    source_job = await store.get_operation_job(str(operations[0]["id"]))
    assert source_job is not None
    undo = LibraryManagementUndoService(
        store,
        preferences,
        audio,
        blobs,
        filesystem,
        clock=lambda: 120.0,
    )
    undo_preview = await undo.create_preview(
        str(operations[0]["id"]),
        LibraryManagementUndoPreviewRequest(
            expected_operation_row_revision=int(source_job["row_revision"]),
            idempotency_key="undo-automatic-import-preview",
        ),
        "admin",
    )
    claimed_preview = await store.claim_operation_job(
        "undo-preview-worker",
        now=121.0,
        lease_seconds=60.0,
        kind="library_management",
    )
    assert claimed_preview is not None
    await undo.run_claimed_preview(claimed_preview, "undo-preview-worker")
    ready = await store.get_operation_job(undo_preview.job_id)
    assert ready is not None and ready["state"] == "ready"
    await store.begin_library_management_apply(
        undo_preview.job_id,
        preview_token_hash=hashlib.sha256(
            undo_preview.preview_token.encode()
        ).hexdigest(),
        expected_job_revision=int(ready["row_revision"]),
        idempotency_key="undo-automatic-import-apply",
        now=122.0,
    )
    claimed_apply = await store.claim_operation_job(
        "undo-apply-worker",
        now=123.0,
        lease_seconds=60.0,
        kind="library_management",
    )
    assert claimed_apply is not None
    work = await store.claim_operation_work(
        undo_preview.job_id, "undo-apply-worker", now=124.0
    )
    assert work is not None
    undo_publisher = LibraryManagementPublisher(
        store,
        preferences,
        audio,
        AudioWritePlanningService(audio),
        blobs,
        filesystem,
        clock=lambda: 125.0,
    )

    await undo_publisher.publish_bundle(
        undo_preview.job_id, int(work["ordinal"]), "undo-apply-worker"
    )

    restored_path = root / "Incoming/01 Original.flac"
    restored_track = await store.get_target_track(track_id)
    assert restored_track is not None
    assert restored_track["relative_path"] == "Incoming/01 Original.flac"
    assert restored_path.is_file()
    assert not (root / request.destination_relative_path).exists()
    assert audio.snapshot(restored_path).metadata == original_snapshot.metadata
    assert (root / "Incoming/album.cue").read_text() == "FILE original.flac WAVE"
    assert not (root / "Managed Artist/Managed Album/cover.jpg").exists()
    assert not (root / "Managed Artist/Managed Album/album.cue").exists()


@pytest.mark.asyncio
async def test_automatic_import_collision_becomes_actionable_management_hold(
    tmp_path: Path,
) -> None:
    _root, source, preferences, store, _settings, policy_revision = _configured(
        tmp_path
    )
    management = preferences.get_library_management_settings_raw()
    profile = next(
        value
        for value in management.profiles
        if value.id == PICARD_ORGANIZER_PROFILE_ID
    )
    request = _import_file(
        AudioMetadataEngine(),
        source,
        ordinal=0,
        relative_path="Managed/01 Track.flac",
    )
    request = msgspec.structs.replace(
        request,
        pinned_profile=_planner(tmp_path, store, preferences).pin_profile(
            management, profile
        ),
    )
    publisher = AsyncMock(spec=LibraryManagementPublisher)
    publisher.publish_import_bundle.side_effect = ConflictError("occupied destination")
    service = TargetImportLibraryService(
        store,
        lambda: LibraryPolicyResolver(preferences.get_typed_library_settings_raw()),
        AsyncMock(),
        management_publisher=publisher,
    )

    with pytest.raises(AutomaticManagementHoldError) as held:
        await service.publish_import_bundle(
            LibraryManagementImportBundle(
                idempotency_key="automatic-collision",
                origin="acquisition",
                policy_revision=policy_revision,
                files=(request,),
            )
        )

    assert held.value.reason_code == PATH_COLLISION_DIFFERENT


@pytest.mark.asyncio
async def test_import_bundle_prepares_every_file_before_any_publish(
    tmp_path: Path,
) -> None:
    root, catalog_source, store, audio, _publisher, service, policy_revision = (
        _import_publication_fixture(tmp_path)
    )
    first_source = tmp_path / "incoming-1.flac"
    second_source = tmp_path / "incoming-2.flac"
    shutil.copy2(catalog_source, first_source)
    shutil.copy2(catalog_source, second_source)
    first = _import_file(
        audio,
        first_source,
        ordinal=0,
        relative_path="Import Artist/Import Album/01 Track.flac",
    )
    second = _import_file(
        audio,
        second_source,
        ordinal=1,
        relative_path="Import Artist/Import Album/02 Track.flac",
    )
    occupied = root / second.destination_relative_path
    occupied.parent.mkdir(parents=True, exist_ok=True)
    occupied.write_bytes(b"third-party destination")
    bundle = LibraryManagementImportBundle(
        idempotency_key="acquisition:task-2:minimal",
        origin="acquisition",
        policy_revision=policy_revision,
        files=(first, second),
    )

    with pytest.raises(ConflictError, match="destination"):
        await service.publish_import_bundle(bundle)

    with sqlite3.connect(store.db_path) as connection:
        bundle_id = str(
            connection.execute(
                "SELECT id FROM library_management_import_bundles "
                "WHERE idempotency_key=?",
                (bundle.idempotency_key,),
            ).fetchone()[0]
        )
    record = await store.get_library_management_import_bundle(bundle_id)
    journals = await store.list_library_management_import_journals(bundle_id)
    assert not (root / first.destination_relative_path).exists()
    assert occupied.read_bytes() == b"third-party destination"
    assert first_source.is_file() and second_source.is_file()
    assert record is not None and record.state == "rolled_back"
    assert [value.state for value in journals] == ["rolled_back", "rolled_back"]


@pytest.mark.asyncio
async def test_import_preparation_failure_rolls_back_the_in_progress_journal(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    root, catalog_source, store, audio, publisher, _service, policy_revision = (
        _import_publication_fixture(tmp_path)
    )
    sources = [tmp_path / "prepare-1.flac", tmp_path / "prepare-2.flac"]
    for source in sources:
        shutil.copy2(catalog_source, source)
    bundle = LibraryManagementImportBundle(
        idempotency_key="acquisition:prepare-failure:minimal",
        origin="acquisition",
        policy_revision=policy_revision,
        files=tuple(
            _import_file(
                audio,
                source,
                ordinal=ordinal,
                relative_path=f"Import Artist/Import Album/0{ordinal + 1} Track.flac",
            )
            for ordinal, source in enumerate(sources)
        ),
    )
    original = publisher._stage_audio
    calls = 0

    def fail_second_stage(*args) -> None:
        nonlocal calls
        calls += 1
        if calls == 2:
            raise OSError("simulated staging failure")
        original(*args)

    monkeypatch.setattr(publisher, "_stage_audio", fail_second_stage)

    with pytest.raises(OSError, match="simulated staging failure"):
        await publisher.publish_import_bundle(bundle, AsyncMock())

    with sqlite3.connect(store.db_path) as connection:
        bundle_id = str(
            connection.execute(
                "SELECT id FROM library_management_import_bundles "
                "WHERE idempotency_key=?",
                (bundle.idempotency_key,),
            ).fetchone()[0]
        )
    journals = await store.list_library_management_import_journals(bundle_id)
    assert [value.state for value in journals] == ["rolled_back", "rolled_back"]
    assert all(source.is_file() for source in sources)
    assert list(root.rglob(".droppedneedle-management-*")) == []


@pytest.mark.asyncio
async def test_import_bundle_catalog_failure_restores_every_source(
    tmp_path: Path,
) -> None:
    root, catalog_source, store, audio, publisher, service, policy_revision = (
        _import_publication_fixture(tmp_path)
    )
    sources = [tmp_path / "incoming-1.flac", tmp_path / "incoming-2.flac"]
    for source in sources:
        shutil.copy2(catalog_source, source)
    bundle = LibraryManagementImportBundle(
        idempotency_key="acquisition:catalog-failure:minimal",
        origin="acquisition",
        policy_revision=policy_revision,
        files=tuple(
            _import_file(
                audio,
                source,
                ordinal=ordinal,
                relative_path=f"Import Artist/Import Album/0{ordinal + 1} Track.flac",
            )
            for ordinal, source in enumerate(sources)
        ),
    )
    commit = AsyncMock(side_effect=OSError("simulated sqlite failure"))

    with pytest.raises(OSError, match="simulated sqlite failure"):
        await publisher.publish_import_bundle(bundle, commit)

    with sqlite3.connect(store.db_path) as connection:
        bundle_id = str(
            connection.execute(
                "SELECT id FROM library_management_import_bundles WHERE idempotency_key=?",
                (bundle.idempotency_key,),
            ).fetchone()[0]
        )
    record = await store.get_library_management_import_bundle(bundle_id)
    journals = await store.list_library_management_import_journals(bundle_id)
    assert all(source.is_file() for source in sources)
    assert not any(
        (root / value.destination_relative_path).exists() for value in bundle.files
    )
    assert record is not None and record.state == "rolled_back"
    assert [value.state for value in journals] == ["rolled_back", "rolled_back"]

    retried = await service.publish_import_bundle(bundle)
    retried_journals = await store.list_library_management_import_journals(bundle_id)
    assert len(retried.local_track_ids) == 2
    assert all(
        (root / value.destination_relative_path).is_file() for value in bundle.files
    )
    assert [value.state for value in retried_journals] == ["completed", "completed"]


@pytest.mark.asyncio
async def test_failed_automatic_import_releases_provisional_snapshot_blobs(
    tmp_path: Path,
) -> None:
    root, catalog_source, preferences, store, _settings, policy_revision = _configured(
        tmp_path
    )
    audio = AudioMetadataEngine()
    publisher = LibraryManagementPublisher(
        store,
        preferences,
        audio,
        AudioWritePlanningService(audio),
        LibraryManagementBlobStore(tmp_path / "rollback-blobs", store),
        LibraryFilesystemCoordinator(),
        clock=lambda: 110.0,
    )
    incoming = tmp_path / "automatic-rollback.flac"
    shutil.copy2(catalog_source, incoming)
    artwork = b"temporary artwork"
    management = preferences.get_library_management_settings_raw()
    profile = next(
        value
        for value in management.profiles
        if value.id == PICARD_ORGANIZER_PROFILE_ID
    )
    request = msgspec.structs.replace(
        _import_file(
            audio,
            incoming,
            ordinal=0,
            relative_path="Managed/01 Rollback.flac",
        ),
        authoritative_mapping=True,
        recording_mbid="recording-1",
        release_track_mbid="release-track-1",
        medium_position=1,
        release_track_position=1,
        baseline_relative_path="Incoming/01 Original.flac",
        desired_document=DesiredAudioDocument(
            fields=(DesiredAudioField(name="title", action="set", value="Managed"),)
        ),
        pinned_profile=_planner(tmp_path, store, preferences).pin_profile(
            management, profile
        ),
        metadata_snapshot_id="automatic-rollback-snapshot",
        projection_hash="d" * 64,
        settings_revision=settings_revision(management),
        undo_retention_days=management.undo_retention_days,
        artifacts=(
            LibraryManagementImportArtifact(
                kind="external_art",
                destination_root_id="root-1",
                destination_relative_path="Managed/cover.jpg",
                content=artwork,
                source_fingerprint=hashlib.sha256(artwork).hexdigest(),
            ),
        ),
    )
    bundle = LibraryManagementImportBundle(
        idempotency_key="acquisition:automatic-rollback",
        origin="acquisition",
        policy_revision=policy_revision,
        files=(request,),
    )

    with pytest.raises(OSError, match="catalog failure"):
        await publisher.publish_import_bundle(
            bundle, AsyncMock(side_effect=OSError("catalog failure"))
        )

    with sqlite3.connect(store.db_path) as connection:
        references = int(
            connection.execute(
                "SELECT COUNT(*) FROM library_management_blob_references "
                "WHERE reference_kind='operation_snapshot' AND reference_id LIKE 'import:%'"
            ).fetchone()[0]
        )
        bundle_id = str(
            connection.execute(
                "SELECT id FROM library_management_import_bundles "
                "WHERE idempotency_key=?",
                (bundle.idempotency_key,),
            ).fetchone()[0]
        )
    journals = await store.list_library_management_import_journals(bundle_id)
    assert references == 0
    assert incoming.is_file()
    assert not (root / request.destination_relative_path).exists()
    assert not (root / "Managed/cover.jpg").exists()
    assert journals[0].baseline_blob_sha256 is None
    assert journals[0].baseline_ancillary_snapshot_json == "[]"
    assert not list(root.rglob(".droppedneedle-management-*"))


@pytest.mark.asyncio
async def test_import_bundle_same_path_upgrade_recycles_original_after_commit(
    tmp_path: Path,
) -> None:
    root, original, store, audio, _publisher, service, policy_revision = (
        _import_publication_fixture(tmp_path)
    )
    original_bytes = original.read_bytes()
    incoming = tmp_path / "incoming-upgrade.flac"
    shutil.copy2(original, incoming)
    recycle_bin = tmp_path / "recycle"
    request = msgspec.structs.replace(
        _import_file(
            audio,
            incoming,
            ordinal=0,
            relative_path="source.flac",
        ),
        replacement_local_track_id="track-1",
        replacement_root_id="root-1",
        replacement_relative_path="source.flac",
        recycle_bin_path=str(recycle_bin),
    )
    bundle = LibraryManagementImportBundle(
        idempotency_key="acquisition:same-path-upgrade:minimal",
        origin="acquisition",
        policy_revision=policy_revision,
        files=(request,),
    )

    result = await service.publish_import_bundle(bundle)

    recycled = list(recycle_bin.rglob("*.flac"))
    written = audio.read(original)
    assert result.local_track_ids == ("track-1",)
    assert incoming.exists() is False
    assert written.metadata.value_for("album") == "Import Album"
    assert len(recycled) == 1 and recycled[0].read_bytes() == original_bytes


@pytest.mark.asyncio
async def test_import_bundle_resumes_publish_after_process_stops_before_journal_update(
    tmp_path: Path,
) -> None:
    root, catalog_source, store, audio, publisher, service, policy_revision = (
        _import_publication_fixture(tmp_path)
    )
    incoming = tmp_path / "incoming-crash.flac"
    shutil.copy2(catalog_source, incoming)
    request = _import_file(
        audio,
        incoming,
        ordinal=0,
        relative_path="Import Artist/Import Album/01 Crash.flac",
    )
    bundle = LibraryManagementImportBundle(
        idempotency_key="acquisition:publish-boundary:minimal",
        origin="acquisition",
        policy_revision=policy_revision,
        files=(request,),
    )

    class SimulatedProcessStop(BaseException):
        pass

    publish_file = publisher._publish_import_file
    rollback = publisher._rollback_import_bundle

    async def stop_after_replace(value):
        await asyncio.to_thread(os.replace, value.temporary, value.destination)
        raise SimulatedProcessStop

    publisher._publish_import_file = stop_after_replace
    publisher._rollback_import_bundle = AsyncMock(side_effect=SimulatedProcessStop)
    with pytest.raises(SimulatedProcessStop):
        await service.publish_import_bundle(bundle)
    publisher._publish_import_file = publish_file
    publisher._rollback_import_bundle = rollback

    destination = root / request.destination_relative_path
    with sqlite3.connect(store.db_path) as connection:
        bundle_id = str(
            connection.execute(
                "SELECT id FROM library_management_import_bundles "
                "WHERE idempotency_key=?",
                (bundle.idempotency_key,),
            ).fetchone()[0]
        )
    before = await store.list_library_management_import_journals(bundle_id)
    resumed = await service.publish_import_bundle(bundle)

    assert destination.is_file()
    assert incoming.exists() is False
    assert [value.state for value in before] == ["validated"]
    assert resumed.paths == (str(destination),)


@pytest.mark.asyncio
async def test_published_import_retry_does_not_restage_artifact_temporaries(
    tmp_path: Path,
) -> None:
    root, catalog_source, store, audio, publisher, service, policy_revision = (
        _import_publication_fixture(tmp_path)
    )
    incoming = tmp_path / "incoming-artwork-crash.flac"
    shutil.copy2(catalog_source, incoming)
    artwork = b"published artwork"
    request = msgspec.structs.replace(
        _import_file(
            audio,
            incoming,
            ordinal=0,
            relative_path="Import Artist/Import Album/01 Artwork.flac",
        ),
        artifacts=(
            LibraryManagementImportArtifact(
                kind="external_art",
                destination_root_id="root-1",
                destination_relative_path="Import Artist/Import Album/cover.jpg",
                content=artwork,
                source_fingerprint=hashlib.sha256(artwork).hexdigest(),
            ),
        ),
    )
    bundle = LibraryManagementImportBundle(
        idempotency_key="acquisition:published-artifact-retry:minimal",
        origin="acquisition",
        policy_revision=policy_revision,
        files=(request,),
    )

    class SimulatedProcessStop(BaseException):
        pass

    commit = service._commit_published_import_bundle
    rollback = publisher._rollback_import_bundle
    service._commit_published_import_bundle = AsyncMock(
        side_effect=SimulatedProcessStop
    )
    publisher._rollback_import_bundle = AsyncMock(side_effect=SimulatedProcessStop)
    with pytest.raises(SimulatedProcessStop):
        await service.publish_import_bundle(bundle)
    service._commit_published_import_bundle = commit
    publisher._rollback_import_bundle = rollback

    result = await service.publish_import_bundle(bundle)

    assert result.paths == (str(root / request.destination_relative_path),)
    assert (root / "Import Artist/Import Album/cover.jpg").read_bytes() == artwork
    assert not list(root.rglob(".droppedneedle-management-*"))


@pytest.mark.asyncio
async def test_publisher_moves_validated_real_audio_and_is_idempotent(
    tmp_path: Path,
) -> None:
    root, source, store, audio, publisher, job_id = await _ready_apply_operation(
        tmp_path
    )

    result = await publisher.publish_bundle(job_id, 0, "apply-worker")
    repeated = await publisher.publish_bundle(job_id, 0, "apply-worker")

    destination = root / (
        "Johann Sebastian Bach; Glenn Gould/"
        "Goldberg Variations, BWV 988 (1982)/0101 Aria.flac"
    )
    document = audio.read(destination)
    row = await store.get_target_track("track-1")
    journals = await store.list_file_mutation_journals_for_bundle(job_id, 0)
    assert result.catalog_revision == repeated.catalog_revision == 1
    assert source.exists() is False
    assert destination.is_file()
    assert document.metadata.value_for("title") == "Aria"
    assert row is not None
    assert row["relative_path"] == destination.relative_to(root).as_posix()
    assert (
        journals[0].staged_fingerprint
        == hashlib.sha256(destination.read_bytes()).hexdigest()
    )
    assert [journal.state for journal in journals] == ["completed"]
    assert not list(root.rglob(".droppedneedle-management-*"))


@pytest.mark.asyncio
async def test_publisher_breaks_hardlinks_without_mutating_the_other_name(
    tmp_path: Path,
) -> None:
    root, source, _store, _audio, publisher, job_id = await _ready_apply_operation(
        tmp_path
    )
    sibling = root / "hardlinked-original.flac"
    os.link(source, sibling)
    original = sibling.read_bytes()
    original_inode = sibling.stat().st_ino

    await publisher.publish_bundle(job_id, 0, "apply-worker")

    destination = root / (
        "Johann Sebastian Bach; Glenn Gould/"
        "Goldberg Variations, BWV 988 (1982)/0101 Aria.flac"
    )
    assert sibling.read_bytes() == original
    assert sibling.stat().st_ino == original_inode
    assert destination.stat().st_ino != original_inode


@pytest.mark.asyncio
async def test_publisher_replaces_same_path_through_verified_backup(
    tmp_path: Path,
) -> None:
    root, source, store, audio, publisher, job_id = await _ready_apply_operation(
        tmp_path, configure=_same_path_configuration
    )
    original = source.read_bytes()

    await publisher.publish_bundle(job_id, 0, "apply-worker")

    journals = await store.list_file_mutation_journals_for_bundle(job_id, 0)
    assert source.is_file()
    assert source.read_bytes() != original
    assert audio.read(source).metadata.value_for("title") == "Aria"
    assert [journal.state for journal in journals] == ["completed"]
    assert not list(root.rglob(".droppedneedle-management-*"))


@pytest.mark.asyncio
async def test_publisher_moves_sidecar_and_publishes_external_artwork(
    tmp_path: Path,
) -> None:
    artwork = _ArtworkRepository()

    def configure(root, preferences, store) -> None:
        _sidecar_configuration(root, preferences, store)
        _external_artwork_configuration(root, preferences, store)

    root, source, store, _audio, publisher, job_id = await _ready_apply_operation(
        tmp_path,
        configure=configure,
        artwork_repository=artwork,
    )

    await publisher.publish_bundle(job_id, 0, "apply-worker")

    album_dir = root / (
        "Johann Sebastian Bach; Glenn Gould/Goldberg Variations, BWV 988 (1982)"
    )
    journals = await store.list_file_mutation_journals_for_bundle(job_id, 0)
    assert source.exists() is False
    assert (root / "disc.cue").exists() is False
    assert (album_dir / "disc.cue").read_text(encoding="utf-8") == "FILE source.flac"
    assert (
        album_dir.parent / "Goldberg Variations, BWV 988/art-front.png"
    ).read_bytes() == artwork.content
    assert {journal.subject_kind for journal in journals} == {
        "audio",
        "external_art",
        "sidecar",
    }
    assert all(journal.state == "completed" for journal in journals)


@pytest.mark.asyncio
async def test_publisher_supports_explicit_cross_root_move(tmp_path: Path) -> None:
    destination_root = tmp_path / "organized"
    destination_root.mkdir()

    def configure(_root, preferences, _store) -> None:
        settings = preferences.get_typed_library_settings_raw()
        settings.library_roots.append(
            LibraryRootSettings(
                id="root-2",
                path=str(destination_root),
                label="Organized",
                policy="automatic",
                rules=[],
            )
        )
        preferences.save_typed_library_settings(settings)

    _root, source, store, _audio, publisher, job_id = await _ready_apply_operation(
        tmp_path, configure=configure, target_root_id="root-2"
    )

    await publisher.publish_bundle(job_id, 0, "apply-worker")

    row = await store.get_target_track("track-1")
    assert source.exists() is False
    assert row is not None and row["root_id"] == "root-2"
    assert (destination_root / str(row["relative_path"])).is_file()


@pytest.mark.asyncio
async def test_publisher_honors_configured_keep_source_mode(tmp_path: Path) -> None:
    root, source, store, _audio, publisher, job_id = await _ready_apply_operation(
        tmp_path, configure=_keep_source_configuration
    )

    await publisher.publish_bundle(job_id, 0, "apply-worker")

    row = await store.get_target_track("track-1")
    journals = await store.list_file_mutation_journals_for_bundle(job_id, 0)
    assert source.is_file()
    assert row is not None and (root / str(row["relative_path"])).is_file()
    assert [journal.state for journal in journals] == ["completed"]


@pytest.mark.asyncio
async def test_publisher_removes_empty_source_directories_when_enabled(
    tmp_path: Path,
) -> None:
    root, _source, _store, _audio, publisher, job_id = await _ready_apply_operation(
        tmp_path, prepare_store=_nest_source
    )

    await publisher.publish_bundle(job_id, 0, "apply-worker")

    assert (root / "incoming").exists() is False


@pytest.mark.asyncio
async def test_publisher_prepares_and_commits_multi_file_album_as_one_bundle(
    tmp_path: Path,
) -> None:
    root, source, store, audio, publisher, job_id = await _ready_apply_operation(
        tmp_path,
        prepare_store=_add_second_album_track,
        customize_planner=_add_second_canonical_track,
        selection=LibraryManagementSelection(kind="albums", ids=("album-1",)),
    )

    await publisher.publish_bundle(job_id, 0, "apply-worker")

    first = await store.get_target_track("track-1")
    second = await store.get_target_track("track-2")
    journals = await store.list_file_mutation_journals_for_bundle(job_id, 0)
    assert source.exists() is False
    assert (root / "source2.flac").exists() is False
    assert first is not None and second is not None
    assert (
        audio.read(root / str(first["relative_path"])).metadata.value_for("title")
        == "Aria"
    )
    assert (
        audio.read(root / str(second["relative_path"])).metadata.value_for("title")
        == "Variation 1"
    )
    assert len(journals) == 2
    assert all(journal.state == "completed" for journal in journals)


@pytest.mark.asyncio
async def test_publisher_rolls_back_published_move_when_catalog_cas_fails(
    tmp_path: Path,
) -> None:
    root, source, store, _audio, publisher, job_id = await _ready_apply_operation(
        tmp_path
    )
    original = source.read_bytes()
    with sqlite3.connect(tmp_path / "library.db") as connection:
        connection.execute(
            "UPDATE library_catalog_revision SET value=1 WHERE singleton=1"
        )

    with pytest.raises(StaleRevisionError, match="catalog changed"):
        await publisher.publish_bundle(job_id, 0, "apply-worker")

    journals = await store.list_file_mutation_journals_for_bundle(job_id, 0)
    assert source.read_bytes() == original
    assert not list(root.rglob("0101 Aria.flac"))
    assert [journal.state for journal in journals] == ["rolled_back"]


@pytest.mark.asyncio
async def test_publisher_preserves_destination_changed_before_immediate_rollback(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    root, source, store, _audio, publisher, job_id = await _ready_apply_operation(
        tmp_path
    )
    item = (await store.list_library_management_plan_items(job_id))[0]
    destination = root / str(item.destination_relative_path)

    async def fail_commit(*_args, **_kwargs):
        destination.write_bytes(b"third-party replacement")
        raise StaleRevisionError("catalog changed")

    monkeypatch.setattr(store, "commit_library_management_bundle", fail_commit)

    with pytest.raises(StaleRevisionError, match="catalog changed"):
        await publisher.publish_bundle(job_id, 0, "apply-worker")

    journals = await store.list_file_mutation_journals_for_bundle(job_id, 0)
    assert source.is_file()
    assert destination.read_bytes() == b"third-party replacement"
    assert [journal.state for journal in journals] == ["needs_attention"]


@pytest.mark.asyncio
async def test_publisher_rejects_settings_changed_after_preview(tmp_path: Path) -> None:
    root, source, store, _audio, publisher, job_id = await _ready_apply_operation(
        tmp_path
    )
    current = publisher._preferences.get_library_management_settings()
    changed = publisher._preferences.get_library_management_settings_raw()
    changed.undo_retention_days += 1
    publisher._preferences.save_library_management_settings_if_current(
        changed, expected_settings_revision=current.settings_revision
    )

    with pytest.raises(StaleRevisionError, match="settings changed"):
        await publisher.publish_bundle(job_id, 0, "apply-worker")

    assert source.is_file()
    assert await store.list_file_mutation_journals_for_bundle(job_id, 0) == []
    assert not list(root.rglob(".droppedneedle-management-*"))


@pytest.mark.asyncio
async def test_publisher_rejects_identity_changed_after_preview(tmp_path: Path) -> None:
    root, source, store, _audio, publisher, job_id = await _ready_apply_operation(
        tmp_path
    )
    with sqlite3.connect(tmp_path / "library.db") as connection:
        connection.execute(
            "UPDATE local_track_external_identities SET row_revision=row_revision+1 "
            "WHERE local_track_id='track-1'"
        )

    with pytest.raises(StaleRevisionError, match="mapping changed"):
        await publisher.publish_bundle(job_id, 0, "apply-worker")

    assert source.is_file()
    assert not list(root.rglob(".droppedneedle-management-*"))


@pytest.mark.asyncio
async def test_publisher_refuses_destination_created_after_preview_and_records_it(
    tmp_path: Path,
) -> None:
    root, source, store, _audio, publisher, job_id = await _ready_apply_operation(
        tmp_path
    )
    item = (await store.list_library_management_plan_items(job_id))[0]
    destination = root / str(item.destination_relative_path)
    destination.parent.mkdir(parents=True, exist_ok=True)
    destination.write_bytes(b"third-party file")

    with pytest.raises(ConflictError, match="created after preview"):
        await publisher.publish_bundle(job_id, 0, "apply-worker")

    with sqlite3.connect(tmp_path / "library.db") as connection:
        collision = connection.execute(
            "SELECT classification FROM library_management_collision_evidence "
            "WHERE job_id=?",
            (job_id,),
        ).fetchone()
    assert source.is_file()
    assert destination.read_bytes() == b"third-party file"
    assert collision == ("destination_created_after_preview",)


@pytest.mark.asyncio
async def test_publisher_never_overwrites_late_external_artwork(
    tmp_path: Path,
) -> None:
    artwork = _ArtworkRepository()
    root, source, store, _audio, publisher, job_id = await _ready_apply_operation(
        tmp_path,
        configure=_external_artwork_configuration,
        artwork_repository=artwork,
    )
    item = (await store.list_library_management_plan_items(job_id))[0]
    choice = json.loads(item.artwork_choices_json)[0]
    destination = root / choice["destination_relative_path"]
    destination.parent.mkdir(parents=True, exist_ok=True)
    destination.write_bytes(b"late artwork")

    with pytest.raises(ConflictError, match="artwork destination"):
        await publisher.publish_bundle(job_id, 0, "apply-worker")

    assert source.is_file()
    assert destination.read_bytes() == b"late artwork"


@pytest.mark.asyncio
async def test_publisher_marks_cleanup_pending_without_rolling_back_catalog(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    root, source, store, _audio, publisher, job_id = await _ready_apply_operation(
        tmp_path
    )

    def fail_cleanup(_value) -> None:
        raise OSError("injected cleanup failure")

    monkeypatch.setattr(publisher, "_cleanup_committed_filesystem", fail_cleanup)
    result = await publisher.publish_bundle(job_id, 0, "apply-worker")

    row = await store.get_target_track("track-1")
    journals = await store.list_file_mutation_journals_for_bundle(job_id, 0)
    assert result.catalog_revision == 1
    assert source.is_file()
    assert row is not None
    assert (root / str(row["relative_path"])).is_file()
    assert [journal.state for journal in journals] == ["cleanup_pending"]


@pytest.mark.asyncio
async def test_publisher_defers_cancellation_until_critical_publish_is_durable(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    root, source, store, _audio, publisher, job_id = await _ready_apply_operation(
        tmp_path
    )
    published = asyncio.Event()
    release = asyncio.Event()
    original_publish = publisher._publish_one

    async def pause_after_publish(value) -> None:
        await original_publish(value)
        published.set()
        await release.wait()

    monkeypatch.setattr(publisher, "_publish_one", pause_after_publish)
    task = asyncio.create_task(publisher.publish_bundle(job_id, 0, "apply-worker"))
    await published.wait()
    task.cancel()
    release.set()

    with pytest.raises(asyncio.CancelledError):
        await task

    row = await store.get_target_track("track-1")
    journals = await store.list_file_mutation_journals_for_bundle(job_id, 0)
    assert source.exists() is False
    assert row is not None and (root / str(row["relative_path"])).is_file()
    assert [journal.state for journal in journals] == ["completed"]
