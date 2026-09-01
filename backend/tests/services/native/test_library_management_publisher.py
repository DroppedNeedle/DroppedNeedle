import asyncio
import errno
import hashlib
import json
import logging
import os
import shutil
import sqlite3
import unicodedata
import stat
from pathlib import Path, PurePosixPath
from collections.abc import AsyncIterator, Callable
from contextlib import asynccontextmanager
from unittest.mock import AsyncMock

import msgspec
import pytest

from api.v1.schemas.library_management import (
    LibraryManagementRootAssignment,
    LibraryManagementRootOverrides,
    NamingScriptSettings,
    PICARD_ORGANIZER_PROFILE_ID,
    profile_revision,
    settings_revision,
)
from api.v1.schemas.library_policies import LibraryRootSettings
from api.v1.schemas.library_management_preview import (
    LibraryManagementBaselineRestorePreviewRequest,
    LibraryManagementSelectionRequest,
    LibraryManagementUndoPreviewRequest,
)
from core.exceptions import (
    AudioWriteError,
    AutomaticManagementHoldError,
    ConflictError,
    LibraryManagementDestinationConflictError,
    LibraryManagementPolicyChangedError,
    StaleRevisionError,
    ValidationError,
)
from infrastructure.audio.metadata_engine import (
    AudioMetadataEngine,
    legacy_audio_projection,
)
from infrastructure.library_management_blob_store import LibraryManagementBlobStore
from services.native.audio_write_planning_service import AudioWritePlanningService
from services.native.library_filesystem_coordinator import LibraryFilesystemCoordinator
from services.native.library_management_baseline_service import (
    LibraryManagementBaselineService,
)
from services.native.library_management_publisher import (
    LibraryManagementPublisher,
    _catalog_tag_projection,
)
from services.native.library_management_profile_service import (
    LibraryManagementProfileService,
)
from services.native.library_management_undo_service import LibraryManagementUndoService
from services.native.library_policy_resolver import LibraryPolicyResolver
from services.native.identification_revisions import (
    album_identity_revision,
    album_input_revisions,
)
from services.native.target_import_library_service import TargetImportLibraryService
from models.audio import AudioInfo, AudioTag
from models.audio_metadata import (
    AudioWritePolicy,
    DesiredAudioDocument,
    DesiredAudioField,
    SemanticTagSnapshot,
)
from models.library_management import (
    BUNDLE_BLOCKED,
    MANAGEMENT_RECYCLE_ROOT_ID,
    PATH_COLLISION_DIFFERENT,
    POLICY_CHANGED,
    ROOT_UNAVAILABLE,
    LibraryManagementImportArtifact,
    LibraryManagementImportBundle,
    LibraryManagementImportFile,
    LibraryManagementJobSnapshot,
    LibraryManagementMetadataSnapshot,
)
from models.edition_management import (
    EditionConversionJob,
    EditionConversionLocalFile,
    EditionConversionTarget,
)
from models.library_management_planning import (
    LibraryManagementSelection,
    naming_policy_revision,
    pin_library_management_profile,
)
from models.library_work import OperationJob
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


def _activate_automatic_acquisitions(preferences, policy_revision: str) -> None:
    current = preferences.get_library_management_settings()
    settings = preferences.get_library_management_settings_raw()
    profile = next(
        value for value in settings.profiles if value.id == PICARD_ORGANIZER_PROFILE_ID
    )
    pinned = pin_library_management_profile(settings, profile)
    settings.root_assignments = [
        LibraryManagementRootAssignment(
            root_id="root-1",
            profile_id=profile.id,
            enabled=True,
            automatic_acquisitions=True,
            activation_profile_revision=profile_revision(profile),
            activation_policy_revision=policy_revision,
            activation_settings_revision=current.settings_revision,
            activation_naming_policy_revision=naming_policy_revision(pinned),
            activation_preview_token="confirmed",
            activation_preview_hash="confirmed-hash",
            activation_confirmed_at=100.0,
        )
    ]
    preferences.save_library_management_settings_if_current(
        settings, expected_settings_revision=current.settings_revision
    )


def test_restoration_catalog_projection_uses_pinned_values_for_empty_tags() -> None:
    projected = _catalog_tag_projection(
        AudioTag(title="", artist="", album="", track_number=0),
        DesiredAudioDocument(
            fields=(
                DesiredAudioField(name="title", action="set", value="Feel It Inside"),
                DesiredAudioField(name="artist", action="set", value=("Trapeze",)),
                DesiredAudioField(name="album", action="set", value="Hot Wire"),
                DesiredAudioField(
                    name="album_artist", action="set", value=("Trapeze",)
                ),
                DesiredAudioField(name="track_number", action="set", value=8),
                DesiredAudioField(name="disc_number", action="set", value=1),
                DesiredAudioField(name="date", action="set", value="1974"),
                DesiredAudioField(name="original_date", action="set", value="1974"),
                DesiredAudioField(name="artist_sort", action="set", value=("Trapeze",)),
                DesiredAudioField(
                    name="album_artist_sort", action="set", value=("Trapeze",)
                ),
                DesiredAudioField(
                    name="musicbrainz_artist_id",
                    action="set",
                    value=("artist-mbid",),
                ),
            ),
            artist_display="Trapeze",
            album_artist_display="Trapeze",
        ),
        "Trapeze/Trapeze - Hot Wire/08 Feel It Inside.mp3",
        native_fields=frozenset(),
    )

    assert projected.title == "Feel It Inside"
    assert projected.artist == projected.album_artist == "Trapeze"
    assert projected.album == "Hot Wire"
    assert projected.track_number == 8
    assert projected.disc_number == 1
    assert projected.year == 1974
    assert projected.original_release_date == "1974"
    assert projected.artist_sort == projected.album_artist_sort == "Trapeze"
    assert projected.musicbrainz_artist_id == "artist-mbid"
    assert projected.artists[0].musicbrainz_artist_id == "artist-mbid"


def test_restoration_catalog_projection_falls_back_to_path() -> None:
    projected = _catalog_tag_projection(
        AudioTag(title="", artist="", album="", track_number=0),
        DesiredAudioDocument(fields=()),
        "Artist/Album/08 Feel It Inside.mp3",
        native_fields=frozenset(),
    )

    assert (
        projected.title,
        projected.artist,
        projected.album,
        projected.album_artist,
    ) == (
        "08 Feel It Inside",
        "Unknown Artist",
        "08 Feel It Inside",
        "Unknown Artist",
    )


def test_restoration_catalog_projection_prefers_explicit_native_defaults() -> None:
    projected = _catalog_tag_projection(
        AudioTag(
            title="Track",
            artist="Track Artist",
            album="Album",
            track_number=1,
            disc_number=2,
            compilation=False,
        ),
        DesiredAudioDocument(
            fields=(
                DesiredAudioField(
                    name="album_artist", action="set", value=("Album Artist",)
                ),
                DesiredAudioField(name="disc_number", action="set", value=1),
                DesiredAudioField(name="compilation", action="set", value=True),
            ),
            artist_display="Track Artist",
            album_artist_display="Album Artist",
        ),
        "Album/02 Track.flac",
        native_fields=frozenset({"disc_number", "compilation"}),
    )

    assert projected.album_artist == "Album Artist"
    assert projected.disc_number == 2
    assert projected.compilation is False


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


def test_minimal_import_document_adds_only_reviewed_recording_identity() -> None:
    tag = AudioTag(
        title="Track",
        artist="Artist",
        album="Album",
        album_artist="Artist",
        track_number=1,
        musicbrainz_release_group_id="review-group",
        musicbrainz_release_id="review-release",
        musicbrainz_recording_id="review-recording",
    )

    default = LibraryManagementPublisher._minimal_import_document(tag)
    reviewed = LibraryManagementPublisher._minimal_import_document(
        tag, reviewed_recording_identity=True
    )
    default_names = {field.name for field in default.fields}
    reviewed_names = {field.name for field in reviewed.fields}

    assert "musicbrainz_recording_id" not in default_names
    assert reviewed_names - default_names == {"musicbrainz_recording_id"}
    assert all(field.name not in {"title", "artist"} for field in reviewed.fields)
    recording = next(
        field for field in reviewed.fields if field.name == "musicbrainz_recording_id"
    )
    assert recording.value == tag.musicbrainz_recording_id


def _reviewed_recording_bundle(
    tmp_path: Path,
    *,
    request_overrides: dict[str, object] | None = None,
    bundle_overrides: dict[str, object] | None = None,
) -> LibraryManagementImportBundle:
    source = tmp_path / "held.flac"
    source.write_bytes(b"held")
    request = LibraryManagementImportFile(
        ordinal=0,
        input_path=str(source),
        destination_root_id="root-1",
        destination_relative_path="reviewed.flac",
        tag=AudioTag(
            title="Track",
            artist="Artist",
            album="Album",
            album_artist="Artist",
            track_number=1,
            musicbrainz_release_group_id="review-group",
            musicbrainz_release_id="review-release",
            musicbrainz_recording_id="review-recording",
        ),
        info=AudioInfo(
            duration_seconds=1.0,
            bitrate=128,
            sample_rate=44100,
            channels=2,
            file_format="flac",
            file_size_bytes=4,
            bit_depth=16,
        ),
        release_group_mbid="review-group",
        release_mbid="review-release",
        recording_mbid="review-recording",
        confidence=1.0,
        source="download",
        source_path=str(source),
        download_task_id="task-1",
        reviewed_recording_identity=True,
    )
    request = msgspec.structs.replace(request, **(request_overrides or {}))
    bundle = LibraryManagementImportBundle(
        idempotency_key="acquisition:reviewed-recording",
        origin="acquisition",
        policy_revision="policy-1",
        files=(request,),
    )
    return msgspec.structs.replace(bundle, **(bundle_overrides or {}))


def test_reviewed_recording_identity_accepts_unmanaged_acquisition(
    tmp_path: Path,
) -> None:
    LibraryManagementPublisher._validate_import_bundle(
        _reviewed_recording_bundle(tmp_path)
    )


@pytest.mark.parametrize(
    ("request_overrides", "bundle_overrides"),
    [
        ({"authoritative_mapping": True}, {}),
        ({"release_track_mbid": "review-track"}, {}),
        ({"medium_position": 1}, {}),
        ({"release_track_position": 1}, {}),
        ({"desired_document": DesiredAudioDocument(fields=())}, {}),
        ({"metadata_snapshot_id": "snapshot"}, {}),
        ({"projection_hash": "a" * 64}, {}),
        ({"settings_revision": "settings"}, {}),
        ({"naming_policy_revision": "naming"}, {}),
        ({"undo_retention_days": 30}, {}),
        ({"baseline_relative_path": "incoming.flac"}, {}),
        ({"management_warnings": ("automatic warning",)}, {}),
        ({"conversion_recycle_only": True}, {}),
        ({"replacement_local_track_id": "track-1"}, {}),
        ({"recycle_bin_path": "/tmp/recycle"}, {}),
        ({"source": "drop"}, {}),
        ({}, {"origin": "drop_import"}),
    ],
    ids=(
        "authoritative",
        "release-track",
        "medium-position",
        "release-track-position",
        "desired-document",
        "metadata-snapshot",
        "projection-hash",
        "settings-revision",
        "naming-revision",
        "undo-retention",
        "baseline",
        "management-warning",
        "conversion-recycle",
        "replacement",
        "recycle",
        "source",
        "origin",
    ),
)
def test_reviewed_recording_identity_rejects_contradictory_requests(
    tmp_path: Path,
    request_overrides: dict[str, object],
    bundle_overrides: dict[str, object],
) -> None:
    with pytest.raises(ValidationError):
        LibraryManagementPublisher._validate_import_bundle(
            _reviewed_recording_bundle(
                tmp_path,
                request_overrides=request_overrides,
                bundle_overrides=bundle_overrides,
            )
        )


@pytest.mark.parametrize(
    ("request_recording", "tag_recording"),
    [
        ("", "review-recording"),
        ("other-recording", "review-recording"),
        ("review-recording", ""),
    ],
    ids=("missing-request", "mismatch", "missing-tag"),
)
def test_reviewed_recording_identity_requires_matching_recording_ids(
    tmp_path: Path,
    request_recording: str,
    tag_recording: str,
) -> None:
    bundle = _reviewed_recording_bundle(tmp_path)
    request = bundle.files[0]
    request = msgspec.structs.replace(
        request,
        recording_mbid=request_recording,
        tag=msgspec.structs.replace(
            request.tag, musicbrainz_recording_id=tag_recording
        ),
    )
    bundle = msgspec.structs.replace(bundle, files=(request,))

    with pytest.raises(ValidationError):
        LibraryManagementPublisher._validate_import_bundle(bundle)


def test_reviewed_recording_identity_rejects_release_track_in_target_tag(
    tmp_path: Path,
) -> None:
    bundle = _reviewed_recording_bundle(tmp_path)
    request = msgspec.structs.replace(
        bundle.files[0],
        tag=msgspec.structs.replace(
            bundle.files[0].tag,
            musicbrainz_release_track_id="tampered-release-track",
        ),
    )
    bundle = msgspec.structs.replace(bundle, files=(request,))

    with pytest.raises(ValidationError):
        LibraryManagementPublisher._validate_import_bundle(bundle)


def _same_path_configuration(_root, preferences, _store) -> None:
    def update(_settings, profile) -> None:
        profile.organization.rename_enabled = False
        profile.organization.move_enabled = False
        profile.organization.move_sidecars = False

    _update_profile(preferences, update)


def _tags_only_configuration(_root, preferences, _store) -> None:
    def update(_settings, profile) -> None:
        profile.organization.rename_enabled = False
        profile.organization.move_enabled = False
        profile.organization.move_sidecars = False
        profile.artwork.embedded_enabled = False
        profile.artwork.external_enabled = False

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


def _case_only_source(root: Path, _preferences, store) -> None:
    relative = (
        "Johann Sebastian Bach; Glenn Gould/"
        "Goldberg Variations, BWV 988 (1982)/01 - ARIA.flac"
    )
    source = root / relative
    source.parent.mkdir(parents=True)
    (root / "source.flac").replace(source)
    with sqlite3.connect(store.db_path) as connection:
        connection.execute(
            "UPDATE local_tracks SET file_path=?,relative_path=?,path_hash=? "
            "WHERE id='track-1'",
            (str(source), relative, hashlib.sha256(relative.encode()).hexdigest()),
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


def test_m4a_staging_mutates_a_local_scratch_copy(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    _root, _catalog_source, _store, audio, publisher, _service, _policy_revision = (
        _import_publication_fixture(tmp_path)
    )
    source = tmp_path / "source.m4a"
    temporary = tmp_path / "destination" / "temporary.m4a"
    shutil.copy2(FIXTURES / "library" / "management_full.m4a", source)
    plan = audio.plan(
        audio.read(source),
        DesiredAudioDocument(
            fields=(
                DesiredAudioField(name="title", action="set", value="Changed title"),
            )
        ),
        AudioWritePolicy(),
    )
    applied_paths: list[Path] = []
    original_apply = audio.apply

    def record_apply(path: Path, write_plan):
        applied_paths.append(path)
        return original_apply(path, write_plan)

    monkeypatch.setattr(audio, "apply", record_apply)

    publisher._stage_audio(source, temporary, plan)

    assert len(applied_paths) == 1
    assert applied_paths[0] != temporary
    assert not applied_paths[0].exists()
    assert audio.read(temporary).metadata.value_for("title") == "Changed title"


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
    source_tag, _ = legacy_audio_projection(audio.read(catalog_source))
    destination_tag, _ = legacy_audio_projection(audio.read(destination))
    assert destination_tag.title == source_tag.title
    assert destination_tag.artist == source_tag.artist
    assert destination_tag.musicbrainz_recording_id == (
        source_tag.musicbrainz_recording_id
    )
    assert request.reviewed_recording_identity is False
    assert incoming.exists() is False
    assert row is not None and row["download_task_id"] == "task-1"
    assert first.paths == repeated.paths == (str(destination),)
    assert first.local_track_ids == repeated.local_track_ids
    assert repeated.repeated is True
    assert [value.state for value in journals] == ["completed"]
    assert [value.id for value in barriers] == [first.bundle_id]


@pytest.mark.asyncio
async def test_import_anyway_with_automatic_assignment_publishes_unmanaged(
    tmp_path: Path,
) -> None:
    from models.held_import import HeldImport
    from services.native.automatic_import_management_service import (
        AutomaticImportManagementService,
    )
    from tests.services.native.test_automatic_import_management_service import (
        _service as automatic_service,
    )
    from tests.services.test_file_processor import _TEMPLATE, _make_processor

    root, catalog_source, preferences, store, _settings, policy_revision = _configured(
        tmp_path
    )
    _activate_automatic_acquisitions(preferences, policy_revision)
    automatic, _planner = automatic_service(tmp_path, preferences, store)
    assert isinstance(automatic, AutomaticImportManagementService)

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
    target = TargetImportLibraryService(
        store,
        lambda: LibraryPolicyResolver(preferences.get_typed_library_settings_raw()),
        AsyncMock(),
        filesystem_coordinator=filesystem,
        management_publisher=publisher,
        automatic_management=automatic,
    )
    published_bundles = []

    async def publish(bundle):
        published_bundles.append(bundle)
        return await target.publish_import_bundle(bundle)

    fp, _manager, _client, _library, _downloads = _make_processor(
        tmp_path / "processor", publisher=publish
    )
    fp._library_paths = [root]
    fp._library_root_ids = ["root-1"]
    fp._policy_revision_getter = lambda: policy_revision

    held_dir = tmp_path / "held"
    held_dir.mkdir()
    held_file = held_dir / "held.flac"
    shutil.copy2(catalog_source, held_file)
    held = HeldImport(
        id=1,
        user_id="user-a",
        held_path=str(held_file),
        reason="fingerprint_mismatch",
        source="usenet",
        status="held",
        created_at=0.0,
        release_group_mbid="held-release-group",
        release_mbid="held-release",
        recording_mbid="held-recording",
        track_number=3,
        disc_number=1,
        track_title="You Shook Me",
        artist_name="Led Zeppelin",
        album_title="Led Zeppelin I",
        year=1969,
        naming_template=_TEMPLATE,
        artist_mbid="678d88b2-87b0-403b-b63d-5da7465aecc3",
    )

    destination = await fp.place_held_file(held)

    assert destination.is_file()
    assert not held_file.exists()
    assert len(published_bundles) == 1
    request = published_bundles[0].files[0]
    assert request.authoritative_mapping is False
    assert request.pinned_profile is None
    assert (
        request.release_track_mbid,
        request.medium_position,
        request.release_track_position,
    ) == (None, None, None)
    assert request.reviewed_recording_identity is True
    assert (
        request.desired_document,
        request.pinned_profile,
        request.metadata_snapshot_id,
        request.projection_hash,
        request.settings_revision,
        request.naming_policy_revision,
        request.undo_retention_days,
        request.baseline_relative_path,
        request.management_warnings,
        request.artifacts,
        request.conversion_recycle_only,
    ) == (None, None, None, None, None, None, None, None, (), (), False)
    assert (
        request.replacement_local_track_id,
        request.replacement_root_id,
        request.replacement_relative_path,
        request.recycle_bin_path,
    ) == (None, None, None, None)
    destination_tag, _ = legacy_audio_projection(audio.read(destination))
    assert destination_tag.musicbrainz_recording_id == held.recording_mbid
    source_tag, _ = legacy_audio_projection(audio.read(catalog_source))
    assert (
        destination_tag.musicbrainz_release_track_id
        == source_tag.musicbrainz_release_track_id
    )
    assert destination_tag.title == source_tag.title
    assert destination_tag.artist == source_tag.artist
    assert request.tag.musicbrainz_release_group_id == held.release_group_mbid
    assert request.tag.musicbrainz_release_id == held.release_mbid
    assert request.tag.musicbrainz_recording_id == held.recording_mbid
    assert request.recording_mbid == held.recording_mbid
    assert request.tag.musicbrainz_release_track_id is None
    with sqlite3.connect(store.db_path) as connection:
        embedded_recording = connection.execute(
            "SELECT embedded_recording_mbid FROM local_tracks WHERE file_path=?",
            (str(destination),),
        ).fetchone()
    assert embedded_recording == (held.recording_mbid,)


@pytest.mark.asyncio
async def test_import_anyway_upgrade_replaces_better_file_with_reviewed_identity(
    tmp_path: Path,
) -> None:
    from models.held_import import HeldImport
    from tests.services.test_file_processor import _TEMPLATE, _make_processor

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
    target = TargetImportLibraryService(
        store,
        lambda: LibraryPolicyResolver(preferences.get_typed_library_settings_raw()),
        AsyncMock(),
        filesystem_coordinator=filesystem,
        management_publisher=publisher,
    )
    published_bundles = []

    async def publish(bundle):
        published_bundles.append(bundle)
        return await target.publish_import_bundle(bundle)

    fp, _manager, _client, _library, _downloads = _make_processor(
        tmp_path / "processor", publisher=publish
    )
    fp._library_paths = [root]
    fp._library_root_ids = ["root-1"]
    fp._policy_revision_getter = lambda: policy_revision
    recycle_bin = tmp_path / "recycle"
    fp._recycle_bin = recycle_bin
    fp._library.get_file_at_position = AsyncMock(
        return_value={
            "id": "track-1",
            "file_path": str(catalog_source),
            "root_id": "root-1",
            "relative_path": "source.flac",
            "file_format": "mp3",
            "bit_rate": 128,
            "bit_depth": None,
        }
    )
    held_file = tmp_path / "held-upgrade.flac"
    shutil.copy2(catalog_source, held_file)
    previous_bytes = catalog_source.read_bytes()
    held = HeldImport(
        id=1,
        user_id="user-a",
        held_path=str(held_file),
        reason="fingerprint_mismatch",
        source="usenet",
        status="held",
        created_at=0.0,
        release_group_mbid="group-1",
        release_mbid="upgrade-release",
        recording_mbid="upgrade-recording",
        track_number=2,
        disc_number=1,
        track_title="Management Track",
        artist_name="Alpha",
        album_title="Management Album",
        year=2024,
        naming_template=_TEMPLATE,
        artist_mbid="artist-1",
        origin="upgrade",
    )

    destination = await fp.place_held_file(held)

    assert destination.is_file()
    assert not held_file.exists()
    assert not catalog_source.exists()
    recycled = list(recycle_bin.rglob("source.flac"))
    assert len(recycled) == 1
    assert recycled[0].read_bytes() == previous_bytes
    assert len(published_bundles) == 1
    request = published_bundles[0].files[0]
    assert request.reviewed_recording_identity is True
    assert (
        request.replacement_local_track_id,
        request.replacement_root_id,
        request.replacement_relative_path,
        request.recycle_bin_path,
    ) == ("track-1", "root-1", "source.flac", str(recycle_bin))
    destination_tag, _ = legacy_audio_projection(audio.read(destination))
    assert destination_tag.musicbrainz_recording_id == "upgrade-recording"
    row = await store.get_target_track_by_path(str(destination))
    assert row is not None
    assert row["file_path"] == str(destination)
    replaced = await store.get_target_track("track-1")
    assert replaced is not None
    assert replaced["availability"] == "indexed"
    assert replaced["file_path"] == str(destination)


@pytest.mark.asyncio
async def test_import_catalog_commit_atomically_settles_matching_management_hold(
    tmp_path: Path,
) -> None:
    root, catalog_source, store, audio, _publisher, service, policy_revision = (
        _import_publication_fixture(tmp_path)
    )
    incoming = tmp_path / "held-incoming.flac"
    shutil.copy2(catalog_source, incoming)
    request = msgspec.structs.replace(
        _import_file(
            audio,
            incoming,
            ordinal=0,
            relative_path="Import Artist/Import Album/01 Track.flac",
        ),
        source_path=str(incoming),
    )
    with sqlite3.connect(store.db_path) as connection:
        connection.execute(
            "INSERT INTO held_imports "
            "(user_id,held_path,reason,source,source_task_id,status,created_at) "
            "VALUES ('admin',?,'management:TRACK_NOT_MAPPED','soulseek',"
            "'task-1','held',1)",
            (str(incoming),),
        )
    bundle = LibraryManagementImportBundle(
        idempotency_key="acquisition:held-atomic:minimal",
        origin="acquisition",
        policy_revision=policy_revision,
        files=(request,),
    )

    result = await service.publish_import_bundle(bundle)

    with sqlite3.connect(store.db_path) as connection:
        status = connection.execute(
            "SELECT status FROM held_imports WHERE source_task_id='task-1'"
        ).fetchone()[0]
    assert status == "imported"
    assert (root / request.destination_relative_path).is_file()
    assert len(result.local_track_ids) == 1


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
    _activate_automatic_acquisitions(preferences, policy_revision)
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
        naming_policy_revision=naming_policy_revision(pinned),
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
    artwork_blob = await blobs.add_bytes(
        artwork_content,
        kind="image",
        created_at=109.0,
        media_metadata_json='{"height":534,"mime_type":"image/jpeg","width":599}',
    )
    sidecar_blob = await blobs.add_bytes(
        incoming_sidecar.read_bytes(),
        kind="sidecar_manifest",
        created_at=109.0,
        media_metadata_json='{"source":"existing-snapshot"}',
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
    before_snapshot = await store.get_management_operation_snapshot(
        str(operations[0]["id"]), 0, track_id
    )
    with sqlite3.connect(store.db_path) as connection:
        bundle_id = str(
            connection.execute(
                "SELECT id FROM library_management_import_bundles "
                "WHERE idempotency_key=?",
                (bundle.idempotency_key,),
            ).fetchone()[0]
        )
    journals = await store.list_library_management_import_journals(bundle_id)
    assert baseline is not None
    decoded_baseline = msgspec.json.decode(
        await blobs.read_bytes(baseline.semantic_snapshot_blob_sha256),
        type=SemanticTagSnapshot,
    )
    assert (root / request.destination_relative_path).is_file()
    assert baseline.image_snapshot_json == "[]"
    assert before_snapshot is not None
    assert before_snapshot.image_snapshot_json == "[]"
    assert [journal.baseline_image_snapshot_json for journal in journals] == ["[]"]
    assert decoded_baseline.artwork == original_snapshot.artwork
    assert baseline.original_relative_path == "Incoming/01 Original.flac"
    ancillary = json.loads(baseline.ancillary_snapshot_json)
    assert {value["kind"] for value in ancillary} == {"external_art", "sidecar"}
    assert (
        root / "Managed Artist/Managed Album/cover.jpg"
    ).read_bytes() == artwork_content
    assert (root / "Managed Artist/Managed Album/album.cue").read_text() == (
        "FILE original.flac WAVE"
    )
    assert (
        await store.get_management_blob(artwork_blob.sha256)
    ).media_metadata_json == artwork_blob.media_metadata_json
    assert (
        await store.get_management_blob(sidecar_blob.sha256)
    ).media_metadata_json == sidecar_blob.media_metadata_json
    assert not incoming_sidecar.exists()
    assert state is not None and state.applied_projection_hash == "c" * 64
    assert state.applied_naming_script_revision == naming_policy_revision(pinned)
    assert identity is not None
    assert identity.release_mbid == "import-release"
    assert identity.tracks[0].release_track_mbid == "release-track-1"
    assert operations[0]["management_mode"] == "automatic_apply"
    assert operations[0]["management_origin"] == "acquisition"
    assert operation_snapshot is not None
    assert operation_snapshot.naming_revision == naming_policy_revision(pinned)
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
async def test_edition_conversion_apply_and_undo_restore_a_different_track_set(
    tmp_path: Path,
) -> None:
    root, source, preferences, store, _settings_revision, policy_revision = _configured(
        tmp_path
    )
    _add_second_album_track(root, preferences, store)
    recycle = tmp_path / "managed-recycle"
    recycle.mkdir()
    current = preferences.get_library_management_settings()
    management = preferences.get_library_management_settings_raw()
    management.recycle_bin_path = str(recycle)
    saved = preferences.save_library_management_settings_if_current(
        management, expected_settings_revision=current.settings_revision
    )
    management = preferences.get_library_management_settings_raw()
    profile = next(
        value
        for value in management.profiles
        if value.id == PICARD_ORGANIZER_PROFILE_ID
    )
    pinned = pin_library_management_profile(management, profile)
    naming_revision = naming_policy_revision(pinned)
    audio = AudioMetadataEngine()
    filesystem = LibraryFilesystemCoordinator()
    blobs = LibraryManagementBlobStore(tmp_path / "conversion-blobs", store)
    publisher = LibraryManagementPublisher(
        store,
        preferences,
        audio,
        AudioWritePlanningService(audio),
        blobs,
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
    metadata_snapshot = await store.put_management_metadata_snapshot(
        LibraryManagementMetadataSnapshot(
            id="conversion-metadata-snapshot",
            provider="musicbrainz",
            entity_kind="release",
            entity_id="77777777-7777-4777-8777-777777777777",
            input_hash="a" * 64,
            canonical_payload_json="{}",
            payload_sha256=hashlib.sha256(b"{}").hexdigest(),
            fetched_at=100.0,
        )
    )
    target_release_group = "dcff25f1-702d-3b5e-b0da-d48172e6e62a"
    target_release = "77777777-7777-4777-8777-777777777777"
    target_recordings = (
        "33333333-3333-4333-8333-333333333333",
        "88888888-8888-4888-8888-888888888888",
    )
    target_release_tracks = (
        "77777777-7777-4777-8777-000000000001",
        "77777777-7777-4777-8777-000000000002",
    )

    def desired_document(position: int) -> DesiredAudioDocument:
        return DesiredAudioDocument(
            fields=(
                DesiredAudioField(
                    name="title", action="set", value=f"Exact Track {position}"
                ),
                DesiredAudioField(name="artist", action="set", value=("Alpha",)),
                DesiredAudioField(
                    name="album", action="set", value="Target Exact Edition"
                ),
                DesiredAudioField(name="album_artist", action="set", value=("Alpha",)),
                DesiredAudioField(name="disc_number", action="set", value=1),
                DesiredAudioField(name="track_number", action="set", value=position),
                DesiredAudioField(
                    name="musicbrainz_release_group_id",
                    action="set",
                    value=target_release_group,
                ),
                DesiredAudioField(
                    name="musicbrainz_release_id",
                    action="set",
                    value=target_release,
                ),
                DesiredAudioField(
                    name="musicbrainz_recording_id",
                    action="set",
                    value=target_recordings[position - 1],
                ),
                DesiredAudioField(
                    name="musicbrainz_release_track_id",
                    action="set",
                    value=target_release_tracks[position - 1],
                ),
            ),
            artist_display="Alpha",
            album_artist_display="Alpha",
        )

    held_retained = tmp_path / "retained-track.flac"
    held_acquired = tmp_path / "acquired-track.flac"
    shutil.copy2(source, held_retained)
    shutil.copy2(source, held_acquired)
    second_source = root / "source2.flac"
    target_requests: list[LibraryManagementImportFile] = []
    for ordinal, held in enumerate((held_retained, held_acquired)):
        position = ordinal + 1
        request = _import_file(
            audio,
            held,
            ordinal=ordinal,
            relative_path=f"Converted/0{position} Exact Track {position}.flac",
        )
        target_requests.append(
            msgspec.structs.replace(
                request,
                release_group_mbid=target_release_group,
                release_mbid=target_release,
                recording_mbid=target_recordings[ordinal],
                source="edition_conversion",
                source_path=str(held),
                download_task_id=None,
                replacement_local_track_id="track-1" if ordinal == 0 else None,
                replacement_root_id="root-1" if ordinal == 0 else None,
                replacement_relative_path="source.flac" if ordinal == 0 else None,
                recycle_bin_path=str(recycle) if ordinal == 0 else None,
                authoritative_mapping=True,
                release_track_mbid=target_release_tracks[ordinal],
                medium_position=1,
                release_track_position=position,
                baseline_relative_path=("source.flac" if ordinal == 0 else held.name),
                desired_document=desired_document(position),
                pinned_profile=pinned,
                metadata_snapshot_id=metadata_snapshot.id,
                projection_hash="c" * 64,
                settings_revision=saved.settings_revision,
                naming_policy_revision=naming_revision,
                undo_retention_days=management.undo_retention_days,
            )
        )
    second_tag, second_info = legacy_audio_projection(audio.read(second_source))
    recycle_request = msgspec.structs.replace(
        _import_file(
            audio,
            second_source,
            ordinal=2,
            relative_path="conversion-job/track-2-source2.flac",
        ),
        destination_root_id=MANAGEMENT_RECYCLE_ROOT_ID,
        tag=second_tag,
        info=second_info,
        release_group_mbid=None,
        release_mbid=None,
        recording_mbid=None,
        source="edition_conversion",
        source_path=str(second_source),
        download_task_id=None,
        replacement_local_track_id="track-2",
        replacement_root_id="root-1",
        replacement_relative_path="source2.flac",
        recycle_bin_path=str(recycle),
        baseline_relative_path="source2.flac",
        desired_document=DesiredAudioDocument(fields=()),
        pinned_profile=pinned,
        metadata_snapshot_id=metadata_snapshot.id,
        projection_hash="c" * 64,
        settings_revision=saved.settings_revision,
        naming_policy_revision=naming_revision,
        undo_retention_days=management.undo_retention_days,
        conversion_recycle_only=True,
    )
    conversion_job_id = "conversion-job"
    preview_job_id = "conversion-final-preview"
    bundle = LibraryManagementImportBundle(
        idempotency_key="edition-conversion:different-track-set",
        origin="edition_conversion",
        policy_revision=policy_revision,
        files=(*target_requests, recycle_request),
        conversion_job_id=conversion_job_id,
        conversion_expected_row_revision=1,
        conversion_local_album_id="album-1",
        conversion_preview_job_id=preview_job_id,
        conversion_recycle_bin_path=str(recycle),
    )
    bundle_json = msgspec.json.encode(bundle).decode()
    bundle_hash = hashlib.sha256(bundle_json.encode()).hexdigest()
    catalog_revision = await store.get_catalog_revision()
    preview_token = "sealed-conversion-preview"
    await store.create_library_management_job(
        OperationJob(
            id=preview_job_id,
            kind="library_management",
            state="ready",
            requested_by_user_id="admin",
            input_catalog_revision=catalog_revision,
            expected_work_count=1,
            idempotency_key="edition-conversion-preview:different-track-set",
            created_at=100.0,
        ),
        LibraryManagementJobSnapshot(
            job_id=preview_job_id,
            mode="preview",
            origin="manual",
            phase="ready",
            selection_json='{"kind":"albums","ids":["album-1"]}',
            profile_revision=pinned.profile.revision,
            settings_revision=saved.settings_revision,
            naming_revision=naming_revision,
            policy_revision=policy_revision,
            catalog_revision=catalog_revision,
            profile_snapshot_json=msgspec.json.encode(pinned).decode(),
            preview_token_hash=hashlib.sha256(preview_token.encode()).hexdigest(),
            preview_created_at=100.0,
            preview_expires_at=1_000.0,
            target_root_id="root-1",
            intent_json='{"edition_conversion_job_id":"conversion-job"}',
            created_at=100.0,
            updated_at=100.0,
        ),
        metadata_snapshot_ids=[metadata_snapshot.id],
    )
    context = await store.get_album_identification_context("album-1")
    assert context is not None
    tracks = context["tracks"]
    tracks_by_id = {str(value["id"]): value for value in tracks}
    await store.create_edition_conversion(
        EditionConversionJob(
            id=conversion_job_id,
            local_album_id="album-1",
            target_release_group_mbid=target_release_group,
            target_release_mbid=target_release,
            target_album_title="Target Exact Edition",
            target_artist_name="Alpha",
            state="ready",
            expected_album_revision=int(context["album"]["row_revision"]),
            expected_input_revision=":".join(album_input_revisions(tracks)),
            expected_identity_revision=album_identity_revision(
                context["identity"], tracks
            ),
            preflight_token_hash=hashlib.sha256(b"preflight").hexdigest(),
            download_source_ready=True,
            required_temporary_bytes=sum(
                path.stat().st_size for path in root.glob("*.flac")
            ),
            kept_count=1,
            acquire_count=1,
            recycle_count=1,
            staged_count=2,
            failed_count=0,
            final_preview_job_id=preview_job_id,
            final_preview_token_hash=hashlib.sha256(preview_token.encode()).hexdigest(),
            final_bundle_json=bundle_json,
            final_bundle_hash=bundle_hash,
            requested_by_user_id="admin",
            error_code=None,
            created_at=100.0,
            updated_at=100.0,
        ),
        (
            EditionConversionTarget(
                job_id=conversion_job_id,
                ordinal=0,
                disc_number=1,
                track_number=1,
                release_track_mbid=target_release_tracks[0],
                recording_mbid=target_recordings[0],
                title="Exact Track 1",
                duration_seconds=None,
                state="kept",
                kept_local_track_id="track-1",
            ),
            EditionConversionTarget(
                job_id=conversion_job_id,
                ordinal=1,
                disc_number=1,
                track_number=2,
                release_track_mbid=target_release_tracks[1],
                recording_mbid=target_recordings[1],
                title="Exact Track 2",
                duration_seconds=None,
                state="staged",
                staged_artifact_id="verified-held-artifact",
            ),
        ),
        (
            EditionConversionLocalFile(
                job_id=conversion_job_id,
                local_track_id="track-1",
                action="keep",
                target_ordinal=0,
                evidence_kind="recording",
                expected_track_revision=int(tracks_by_id["track-1"]["row_revision"]),
                expected_identity_revision=int(
                    tracks_by_id["track-1"]["identity_row_revision"]
                ),
                expected_stat_revision=str(tracks_by_id["track-1"]["stat_revision"]),
            ),
            EditionConversionLocalFile(
                job_id=conversion_job_id,
                local_track_id="track-2",
                action="recycle_extra",
                target_ordinal=None,
                evidence_kind="extra",
                expected_track_revision=int(tracks_by_id["track-2"]["row_revision"]),
                expected_identity_revision=int(
                    tracks_by_id["track-2"]["identity_row_revision"]
                ),
                expected_stat_revision=str(tracks_by_id["track-2"]["stat_revision"]),
            ),
        ),
    )
    operation = await store.get_operation_job(preview_job_id)
    assert operation is not None
    await store.begin_edition_conversion_apply(
        conversion_job_id,
        preview_job_id=preview_job_id,
        preview_token_hash=hashlib.sha256(preview_token.encode()).hexdigest(),
        expected_operation_row_revision=int(operation["row_revision"]),
        apply_idempotency_key="apply-different-track-set",
        now=105.0,
    )

    await service.publish_import_bundle(bundle)

    after_apply = await store.get_target_album_tracks(
        "album-1", include_unavailable=True
    )
    acquired_track_id = next(
        str(value["id"])
        for value in after_apply
        if str(value["id"]) not in {"track-1", "track-2"}
    )
    assert {
        str(value["id"]) for value in after_apply if value["availability"] == "indexed"
    } == {"track-1", acquired_track_id}
    recycled_extra = next(value for value in after_apply if value["id"] == "track-2")
    assert recycled_extra["root_id"] == MANAGEMENT_RECYCLE_ROOT_ID
    assert (recycle / "conversion-job/track-2-source2.flac").is_file()
    assert not source.exists()
    assert not second_source.exists()
    assert (root / "Converted/01 Exact Track 1.flac").is_file()
    assert (root / "Converted/02 Exact Track 2.flac").is_file()
    exact_identity = await store.get_accepted_library_management_identity(
        "album-1", local_track_ids=("track-1", acquired_track_id)
    )
    assert exact_identity is not None
    assert exact_identity.release_mbid == target_release
    assert {value.release_track_mbid for value in exact_identity.tracks} == set(
        target_release_tracks
    )
    source_snapshot = await store.get_library_management_job_snapshot(preview_job_id)
    assert source_snapshot is not None
    assert len(await store.list_management_operation_snapshots(preview_job_id)) == 3

    source_job = await store.get_operation_job(preview_job_id)
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
        preview_job_id,
        LibraryManagementUndoPreviewRequest(
            expected_operation_row_revision=int(source_job["row_revision"]),
            idempotency_key="undo-different-track-set-preview",
        ),
        "admin",
    )
    claimed_preview = await store.claim_operation_job(
        "conversion-undo-preview-worker",
        now=121.0,
        lease_seconds=60.0,
        kind="library_management",
    )
    assert claimed_preview is not None
    await undo.run_claimed_preview(claimed_preview, "conversion-undo-preview-worker")
    assert len(await store.list_library_management_plan_items(undo_preview.job_id)) == 3
    ready = await store.get_operation_job(undo_preview.job_id)
    assert ready is not None and ready["state"] == "ready"
    await store.begin_library_management_apply(
        undo_preview.job_id,
        preview_token_hash=hashlib.sha256(
            undo_preview.preview_token.encode()
        ).hexdigest(),
        expected_job_revision=int(ready["row_revision"]),
        idempotency_key="undo-different-track-set-apply",
        now=122.0,
    )
    claimed_apply = await store.claim_operation_job(
        "conversion-undo-apply-worker",
        now=123.0,
        lease_seconds=60.0,
        kind="library_management",
    )
    assert claimed_apply is not None
    work = await store.claim_operation_work(
        undo_preview.job_id, "conversion-undo-apply-worker", now=124.0
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
        undo_preview.job_id,
        int(work["ordinal"]),
        "conversion-undo-apply-worker",
    )

    after_undo = await store.get_target_album_tracks(
        "album-1", include_unavailable=True
    )
    assert {
        str(value["id"]) for value in after_undo if value["availability"] == "indexed"
    } == {"track-1", "track-2"}
    assert (
        next(value for value in after_undo if value["id"] == acquired_track_id)[
            "availability"
        ]
        == "missing"
    )
    assert source.is_file()
    assert second_source.is_file()
    assert not (root / "Converted/01 Exact Track 1.flac").exists()
    assert not (root / "Converted/02 Exact Track 2.flac").exists()
    restored_identity = await store.get_accepted_library_management_identity(
        "album-1", local_track_ids=("track-1", "track-2")
    )
    assert restored_identity is not None
    assert restored_identity.release_mbid == "aff0622e-7bd3-4fb6-9ca3-0fa19dd2340b"
    assert {value.release_track_mbid for value in restored_identity.tracks} == {
        "22222222-2222-4222-8222-222222222222",
        "66666666-6666-4666-8666-666666666666",
    }


def test_automatic_publication_rejects_a_tampered_pinned_profile(
    tmp_path: Path,
) -> None:
    _root, source, preferences, store, _settings, policy_revision = _configured(
        tmp_path
    )
    _activate_automatic_acquisitions(preferences, policy_revision)
    management = preferences.get_library_management_settings_raw()
    profile = next(
        value
        for value in management.profiles
        if value.id == PICARD_ORGANIZER_PROFILE_ID
    )
    pinned = _planner(tmp_path, store, preferences).pin_profile(management, profile)
    current_settings_revision = settings_revision(management)
    pinned.profile.description = "Tampered after preparation"
    audio = AudioMetadataEngine()
    request = msgspec.structs.replace(
        _import_file(
            audio,
            source,
            ordinal=0,
            relative_path="Managed/01 Track.flac",
        ),
        pinned_profile=pinned,
        settings_revision=current_settings_revision,
        naming_policy_revision=naming_policy_revision(pinned),
    )
    publisher = LibraryManagementPublisher(
        store,
        preferences,
        audio,
        AudioWritePlanningService(audio),
        LibraryManagementBlobStore(tmp_path / "tampered-blobs", store),
        LibraryFilesystemCoordinator(),
    )

    with pytest.raises(StaleRevisionError, match="activation is stale"):
        publisher._validate_automatic_import_configuration(
            LibraryManagementImportBundle(
                idempotency_key="acquisition:tampered-profile",
                origin="acquisition",
                policy_revision=policy_revision,
                files=(request,),
            )
        )


@pytest.mark.asyncio
async def test_automatic_managed_upgrade_preserves_baseline_and_undo_state(
    tmp_path: Path,
) -> None:
    root, original_path, preferences, store, _settings, policy_revision = _configured(
        tmp_path
    )
    _activate_automatic_acquisitions(preferences, policy_revision)
    audio = AudioMetadataEngine()
    filesystem = LibraryFilesystemCoordinator()
    blobs = LibraryManagementBlobStore(tmp_path / "managed-upgrade-blobs", store)
    publisher = LibraryManagementPublisher(
        store,
        preferences,
        audio,
        AudioWritePlanningService(audio),
        blobs,
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
    original_snapshot = audio.snapshot(original_path)
    management = preferences.get_library_management_settings_raw()
    profile = next(
        value
        for value in management.profiles
        if value.id == PICARD_ORGANIZER_PROFILE_ID
    )
    pinned = _planner(tmp_path, store, preferences).pin_profile(management, profile)
    metadata_snapshot = await store.put_management_metadata_snapshot(
        LibraryManagementMetadataSnapshot(
            id="managed-upgrade-metadata",
            provider="musicbrainz",
            entity_kind="release",
            entity_id="managed-upgrade-release",
            input_hash="a" * 64,
            canonical_payload_json="{}",
            payload_sha256=hashlib.sha256(b"{}").hexdigest(),
            fetched_at=100.0,
        )
    )
    recycle_bin = tmp_path / "upgrade-recycle"

    def automatic_request(
        incoming: Path,
        *,
        title: str,
        projection_hash: str,
        artifacts: tuple[LibraryManagementImportArtifact, ...] = (),
    ) -> LibraryManagementImportFile:
        return msgspec.structs.replace(
            _import_file(
                audio,
                incoming,
                ordinal=0,
                relative_path="source.flac",
            ),
            authoritative_mapping=True,
            recording_mbid="recording-1",
            release_track_mbid="release-track-1",
            medium_position=1,
            release_track_position=1,
            baseline_relative_path="source.flac",
            desired_document=DesiredAudioDocument(
                fields=(DesiredAudioField(name="title", action="set", value=title),)
            ),
            pinned_profile=pinned,
            metadata_snapshot_id=metadata_snapshot.id,
            projection_hash=projection_hash,
            settings_revision=settings_revision(management),
            naming_policy_revision=naming_policy_revision(pinned),
            undo_retention_days=management.undo_retention_days,
            replacement_local_track_id="track-1",
            replacement_root_id="root-1",
            replacement_relative_path="source.flac",
            recycle_bin_path=str(recycle_bin),
            artifacts=artifacts,
        )

    incoming_a = tmp_path / "managed-a.flac"
    shutil.copy2(original_path, incoming_a)
    result_a = await service.publish_import_bundle(
        LibraryManagementImportBundle(
            idempotency_key="acquisition:managed-upgrade:a",
            origin="acquisition",
            policy_revision=policy_revision,
            files=(
                automatic_request(
                    incoming_a,
                    title="Managed A",
                    projection_hash="a" * 64,
                ),
            ),
        )
    )
    assert result_a.local_track_ids == ("track-1",)
    baseline_a = await store.get_management_baseline("track-1")
    state_a = await store.get_track_management_state("track-1")
    managed_a_snapshot = audio.snapshot(original_path)
    assert baseline_a is not None and state_a is not None

    incoming_b = tmp_path / "managed-b.flac"
    shutil.copy2(original_path, incoming_b)
    incoming_sidecar = tmp_path / "upgrade.cue"
    incoming_sidecar.write_text("FILE managed-b.flac WAVE", encoding="utf-8")
    sidecar_hash = hashlib.sha256(incoming_sidecar.read_bytes()).hexdigest()
    result_b = await service.publish_import_bundle(
        LibraryManagementImportBundle(
            idempotency_key="acquisition:managed-upgrade:b",
            origin="acquisition",
            policy_revision=policy_revision,
            files=(
                automatic_request(
                    incoming_b,
                    title="Managed B",
                    projection_hash="b" * 64,
                    artifacts=(
                        LibraryManagementImportArtifact(
                            kind="sidecar",
                            destination_root_id="root-1",
                            destination_relative_path="upgrade.cue",
                            source_path=str(incoming_sidecar),
                            source_fingerprint=sidecar_hash,
                        ),
                    ),
                ),
            ),
        )
    )

    baseline_b = await store.get_management_baseline("track-1")
    state_b = await store.get_track_management_state("track-1")
    assert result_b.local_track_ids == ("track-1",)
    assert baseline_b == baseline_a
    assert state_b is not None and state_b.last_operation_job_id is not None
    assert state_b.applied_projection_hash == "b" * 64
    before_b = await store.get_management_operation_snapshot(
        state_b.last_operation_job_id, 0, "track-1"
    )
    assert before_b is not None
    before_b_bytes = await blobs.read_bytes(before_b.semantic_snapshot_blob_sha256)
    before_b_snapshot = msgspec.json.decode(
        before_b_bytes, type=type(managed_a_snapshot)
    )
    assert before_b_snapshot.metadata == managed_a_snapshot.metadata
    before_b_state = json.loads(before_b.before_management_state_json)
    assert before_b_state["last_operation_job_id"] == state_a.last_operation_job_id
    assert before_b_state["applied_projection_hash"] == "a" * 64
    assert json.loads(before_b.ancillary_snapshot_json)[0]["before_exists"] is False
    assert (root / "upgrade.cue").is_file()
    assert not incoming_sidecar.exists()

    source_b = await store.get_operation_job(state_b.last_operation_job_id)
    assert source_b is not None
    undo = LibraryManagementUndoService(
        store,
        preferences,
        audio,
        blobs,
        filesystem,
        clock=lambda: 120.0,
    )
    undo_preview = await undo.create_preview(
        state_b.last_operation_job_id,
        LibraryManagementUndoPreviewRequest(
            expected_operation_row_revision=int(source_b["row_revision"]),
            idempotency_key="undo-managed-upgrade-preview",
        ),
        "admin",
    )
    claimed_undo_preview = await store.claim_operation_job(
        "undo-managed-upgrade-preview-worker",
        now=121.0,
        lease_seconds=60.0,
        kind="library_management",
    )
    assert claimed_undo_preview is not None
    await undo.run_claimed_preview(
        claimed_undo_preview, "undo-managed-upgrade-preview-worker"
    )
    undo_ready = await store.get_operation_job(undo_preview.job_id)
    assert undo_ready is not None
    await store.begin_library_management_apply(
        undo_preview.job_id,
        preview_token_hash=hashlib.sha256(
            undo_preview.preview_token.encode()
        ).hexdigest(),
        expected_job_revision=int(undo_ready["row_revision"]),
        idempotency_key="undo-managed-upgrade-apply",
        now=122.0,
    )
    claimed_undo_apply = await store.claim_operation_job(
        "undo-managed-upgrade-apply-worker",
        now=123.0,
        lease_seconds=60.0,
        kind="library_management",
    )
    assert claimed_undo_apply is not None
    undo_work = await store.claim_operation_work(
        undo_preview.job_id, "undo-managed-upgrade-apply-worker", now=124.0
    )
    assert undo_work is not None
    await publisher.publish_bundle(
        undo_preview.job_id,
        int(undo_work["ordinal"]),
        "undo-managed-upgrade-apply-worker",
    )

    restored_a_state = await store.get_track_management_state("track-1")
    assert audio.snapshot(original_path).metadata == managed_a_snapshot.metadata
    assert restored_a_state is not None
    assert restored_a_state.applied_projection_hash == "a" * 64
    assert restored_a_state.last_operation_job_id == state_a.last_operation_job_id
    assert not (root / "upgrade.cue").exists()
    assert await store.get_management_baseline("track-1") == baseline_a

    baseline_service = LibraryManagementBaselineService(
        store,
        preferences,
        audio,
        blobs,
        filesystem,
        undo,
        clock=lambda: 130.0,
    )
    current_management = preferences.get_library_management_settings()
    restore_preview = await baseline_service.create_restore_preview(
        LibraryManagementBaselineRestorePreviewRequest(
            selection=LibraryManagementSelectionRequest(kind="tracks", ids=["track-1"]),
            expected_settings_revision=current_management.settings_revision,
            expected_policy_revision=policy_revision,
            idempotency_key="managed-upgrade-baseline-preview",
        ),
        "admin",
    )
    claimed_restore_preview = await store.claim_operation_job(
        "managed-upgrade-baseline-preview-worker",
        now=131.0,
        lease_seconds=60.0,
        kind="library_management",
    )
    assert claimed_restore_preview is not None
    await baseline_service.run_claimed_preview(
        claimed_restore_preview, "managed-upgrade-baseline-preview-worker"
    )
    restore_ready = await store.get_operation_job(restore_preview.job_id)
    assert restore_ready is not None
    await store.begin_library_management_apply(
        restore_preview.job_id,
        preview_token_hash=hashlib.sha256(
            restore_preview.preview_token.encode()
        ).hexdigest(),
        expected_job_revision=int(restore_ready["row_revision"]),
        idempotency_key="managed-upgrade-baseline-apply",
        now=132.0,
    )
    claimed_restore_apply = await store.claim_operation_job(
        "managed-upgrade-baseline-apply-worker",
        now=133.0,
        lease_seconds=60.0,
        kind="library_management",
    )
    assert claimed_restore_apply is not None
    restore_work = await store.claim_operation_work(
        restore_preview.job_id, "managed-upgrade-baseline-apply-worker", now=134.0
    )
    assert restore_work is not None
    await publisher.publish_bundle(
        restore_preview.job_id,
        int(restore_work["ordinal"]),
        "managed-upgrade-baseline-apply-worker",
    )

    final_state = await store.get_track_management_state("track-1")
    final_baseline = await store.get_management_baseline("track-1")
    assert audio.snapshot(original_path).metadata == original_snapshot.metadata
    assert final_state is not None and final_state.last_outcome == "restored"
    assert final_baseline is not None and final_baseline.restore_status == "restored"
    assert (
        msgspec.structs.replace(
            final_baseline,
            restore_status=baseline_a.restore_status,
            last_verified_at=baseline_a.last_verified_at,
            row_revision=baseline_a.row_revision,
        )
        == baseline_a
    )


_IO_WRITE_ERROR = AudioWriteError("staged write failed")
_IO_WRITE_ERROR.__cause__ = OSError("temporary NFS interruption")


@pytest.mark.parametrize(
    ("publisher_error", "expected_reason"),
    (
        (
            LibraryManagementDestinationConflictError("occupied destination"),
            PATH_COLLISION_DIFFERENT,
        ),
        (
            LibraryManagementPolicyChangedError("policy changed"),
            POLICY_CHANGED,
        ),
        (StaleRevisionError("journal revision changed"), BUNDLE_BLOCKED),
        (ConflictError("durable evidence disagrees"), BUNDLE_BLOCKED),
        (_IO_WRITE_ERROR, ROOT_UNAVAILABLE),
        (OSError("destination unavailable"), ROOT_UNAVAILABLE),
        (ValidationError("invalid pinned request"), BUNDLE_BLOCKED),
    ),
    ids=(
        "destination",
        "policy",
        "stale-evidence",
        "conflicting-evidence",
        "staged-io",
        "filesystem-io",
        "invalid-contract",
    ),
)
@pytest.mark.asyncio
async def test_automatic_import_conflicts_become_truthful_management_holds(
    tmp_path: Path,
    publisher_error: Exception,
    expected_reason: str,
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
    publisher.publish_import_bundle.side_effect = publisher_error
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

    assert held.value.reason_code == expected_reason
    if expected_reason == BUNDLE_BLOCKED:
        assert str(publisher_error) not in str(held.value)


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

    with pytest.raises(LibraryManagementDestinationConflictError, match="destination"):
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
async def test_import_recovery_skips_a_bundle_owned_by_the_active_publisher(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    root, catalog_source, store, audio, publisher, service, policy_revision = (
        _import_publication_fixture(tmp_path)
    )
    incoming = tmp_path / "active-incoming.flac"
    shutil.copy2(catalog_source, incoming)
    request = _import_file(
        audio,
        incoming,
        ordinal=0,
        relative_path="Import Artist/Import Album/01 Active.flac",
    )
    bundle = LibraryManagementImportBundle(
        idempotency_key="acquisition:active-recovery:minimal",
        origin="acquisition",
        policy_revision=policy_revision,
        files=(request,),
    )
    entered_preparation = asyncio.Event()
    release_preparation = asyncio.Event()
    original = publisher._prepare_import_file

    async def pause_preparation(*args):  # noqa: ANN002, ANN202 - method wrapper
        entered_preparation.set()
        await release_preparation.wait()
        return await original(*args)

    monkeypatch.setattr(publisher, "_prepare_import_file", pause_preparation)
    publication = asyncio.create_task(service.publish_import_bundle(bundle))
    await entered_preparation.wait()
    with sqlite3.connect(store.db_path) as connection:
        bundle_id = str(
            connection.execute(
                "SELECT id FROM library_management_import_bundles "
                "WHERE idempotency_key=?",
                (bundle.idempotency_key,),
            ).fetchone()[0]
        )
    record = await store.get_library_management_import_bundle(bundle_id)
    assert record is not None and record.state == "publishing"

    disposition = await publisher.recover_import_bundle(record)
    release_preparation.set()
    result = await publication

    assert disposition == "skipped"
    assert result.paths == (str(root / request.destination_relative_path),)
    completed = await store.get_library_management_import_bundle(bundle_id)
    assert completed is not None and completed.state == "completed"


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

    def fail_second_stage(*args, **kwargs) -> None:
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
async def test_legacy_standard_only_request_compensates_after_a_late_settings_change(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    root, catalog_source, preferences, store, _settings, policy_revision = _configured(
        tmp_path
    )
    _activate_automatic_acquisitions(preferences, policy_revision)
    current = preferences.get_library_management_settings()
    standard_only = preferences.get_library_management_settings_raw()
    assignment = standard_only.root_assignments[0]
    assignment.overrides = LibraryManagementRootOverrides(
        multi_disc_naming_mode="standard"
    )
    effective = LibraryManagementProfileService._effective_profile(
        standard_only, assignment
    )
    assignment.activation_profile_revision = profile_revision(effective)
    assignment.activation_naming_policy_revision = None
    preferences.save_library_management_settings_if_current(
        standard_only, expected_settings_revision=current.settings_revision
    )
    audio = AudioMetadataEngine()
    filesystem = LibraryFilesystemCoordinator()
    publisher = LibraryManagementPublisher(
        store,
        preferences,
        audio,
        AudioWritePlanningService(audio),
        LibraryManagementBlobStore(tmp_path / "rollback-blobs", store),
        filesystem,
        clock=lambda: 110.0,
    )
    sources = [
        tmp_path / "automatic-rollback-1.flac",
        tmp_path / "automatic-rollback-2.flac",
    ]
    for source in sources:
        shutil.copy2(catalog_source, source)
    artwork = b"temporary artwork"
    management = preferences.get_library_management_settings_raw()
    assignment = management.root_assignments[0]
    effective = LibraryManagementProfileService._effective_profile(
        management, assignment
    )
    pinned_profile = _planner(tmp_path, store, preferences).pin_profile(
        management, effective
    )
    assert pinned_profile.multi_disc_naming_script is None
    metadata_snapshot = await store.put_management_metadata_snapshot(
        LibraryManagementMetadataSnapshot(
            id="automatic-rollback-snapshot",
            provider="musicbrainz",
            entity_kind="release",
            entity_id="import-release",
            input_hash="d" * 64,
            canonical_payload_json="{}",
            payload_sha256=hashlib.sha256(b"{}").hexdigest(),
            fetched_at=100.0,
        )
    )
    requests = tuple(
        msgspec.structs.replace(
            _import_file(
                audio,
                source,
                ordinal=ordinal,
                relative_path=f"Managed/0{ordinal + 1} Rollback.flac",
            ),
            authoritative_mapping=True,
            recording_mbid=f"recording-{ordinal + 1}",
            release_track_mbid=f"release-track-{ordinal + 1}",
            medium_position=1,
            release_track_position=ordinal + 1,
            baseline_relative_path=f"Incoming/0{ordinal + 1} Original.flac",
            desired_document=DesiredAudioDocument(
                fields=(
                    DesiredAudioField(
                        name="title", action="set", value=f"Managed {ordinal + 1}"
                    ),
                )
            ),
            pinned_profile=pinned_profile,
            metadata_snapshot_id=metadata_snapshot.id,
            projection_hash="d" * 64,
            settings_revision=settings_revision(management),
            naming_policy_revision=None,
            undo_retention_days=management.undo_retention_days,
            artifacts=(
                LibraryManagementImportArtifact(
                    kind="external_art",
                    destination_root_id="root-1",
                    destination_relative_path="Managed/cover.jpg",
                    content=artwork,
                    source_fingerprint=hashlib.sha256(artwork).hexdigest(),
                ),
            )
            if ordinal == 0
            else (),
        )
        for ordinal, source in enumerate(sources)
    )
    bundle = LibraryManagementImportBundle(
        idempotency_key="acquisition:automatic-rollback",
        origin="acquisition",
        policy_revision=policy_revision,
        files=requests,
    )
    legacy_payload = msgspec.to_builtins(bundle)
    for request_payload in legacy_payload["files"]:
        request_payload.pop("naming_policy_revision")
    bundle = msgspec.convert(legacy_payload, type=LibraryManagementImportBundle)
    assert all(request.naming_policy_revision is None for request in bundle.files)

    original_project = publisher._published_import_file
    projection_started = asyncio.Event()
    resume_projection = asyncio.Event()

    async def paused_project(value):
        projection_started.set()
        await resume_projection.wait()
        return await original_project(value)

    monkeypatch.setattr(publisher, "_published_import_file", paused_project)
    catalog_commit = AsyncMock()
    publication = asyncio.create_task(
        publisher.publish_import_bundle(bundle, catalog_commit)
    )
    await projection_started.wait()
    current = preferences.get_library_management_settings()
    changed = preferences.get_library_management_settings_raw()
    changed.root_assignments[0].automatic_acquisitions = False
    preferences.save_library_management_settings_if_current(
        changed, expected_settings_revision=current.settings_revision
    )
    resume_projection.set()
    with pytest.raises(StaleRevisionError, match="settings changed"):
        await publication
    catalog_commit.assert_not_awaited()

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
    assert all(source.is_file() for source in sources)
    assert not any(
        (root / request.destination_relative_path).exists() for request in requests
    )
    assert not (root / "Managed/cover.jpg").exists()
    assert len(journals) == 2
    assert all(journal.baseline_blob_sha256 is None for journal in journals)
    assert all(journal.baseline_ancillary_snapshot_json == "[]" for journal in journals)
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

    async def stop_after_replace(value, _roots):
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
    original_snapshot = audio.snapshot(source)

    result = await publisher.publish_bundle(job_id, 0, "apply-worker")
    repeated = await publisher.publish_bundle(job_id, 0, "apply-worker")

    destination = root / (
        "Johann Sebastian Bach; Glenn Gould/"
        "Goldberg Variations, BWV 988 (1982)/01 - Aria.flac"
    )
    document = audio.read(destination)
    row = await store.get_target_track("track-1")
    journals = await store.list_file_mutation_journals_for_bundle(job_id, 0)
    baseline = await store.get_management_baseline("track-1")
    before_snapshot = await store.get_management_operation_snapshot(
        job_id, 0, "track-1"
    )
    assert baseline is not None
    decoded_baseline = msgspec.json.decode(
        await publisher._blobs.read_bytes(baseline.semantic_snapshot_blob_sha256),
        type=SemanticTagSnapshot,
    )
    assert result.catalog_revision == repeated.catalog_revision == 1
    assert source.exists() is False
    assert destination.is_file()
    assert document.metadata.value_for("title") == "Aria"
    assert baseline.image_snapshot_json == "[]"
    assert before_snapshot is not None
    assert before_snapshot.image_snapshot_json == "[]"
    assert decoded_baseline.artwork == original_snapshot.artwork
    assert row is not None
    assert row["relative_path"] == destination.relative_to(root).as_posix()
    assert (
        journals[0].staged_fingerprint
        == hashlib.sha256(destination.read_bytes()).hexdigest()
    )
    assert [journal.state for journal in journals] == ["completed"]
    assert not list(root.rglob(".droppedneedle-management-*"))


def _nfd_source_directory(root: Path, _preferences, store) -> None:
    """Move the source into an NFD-composed parent directory so staging reads
    FROM a decomposed-unicode path (the NFD-parent hazard of F-207)."""
    name = unicodedata.normalize("NFD", "ゴールド")
    assert not unicodedata.is_normalized("NFC", name)  # guard: truly decomposed
    nested = root / name
    nested.mkdir()
    source = root / "source.flac"
    source.replace(nested / "source.flac")
    relative = f"{name}/source.flac"
    with sqlite3.connect(store.db_path) as connection:
        connection.execute(
            "UPDATE local_tracks SET file_path=?,relative_path=?,path_hash=? "
            "WHERE id='track-1'",
            (
                str(nested / "source.flac"),
                relative,
                hashlib.sha256(relative.encode()).hexdigest(),
            ),
        )


def _unicode_release_configuration(planner) -> None:
    """Canonical release whose album/artist/track names mix NFD-decomposable
    katakana with invariant CJK; the naming engine must publish NFC."""
    payload = json.loads(
        (FIXTURES / "musicbrainz" / "management_release.json").read_bytes()
    )
    payload["title"] = unicodedata.normalize("NFD", "ゴールド") + "変奏曲"
    payload["artist-credit"] = [
        {
            "name": "作曲者",
            "joinphrase": "",
            "artist": {
                "id": "24e1b53c-3085-33e9-8f3c-52404792e9a8",
                "name": "作曲者",
                "sort-name": "作曲者",
            },
        }
    ]
    track = payload["media"][0]["tracks"][0]
    track["title"] = unicodedata.normalize("NFD", "ダリア")
    track["recording"]["title"] = track["title"]
    planner._canonical._musicbrainz.get_canonical_release.return_value = (
        msgspec.json.decode(json.dumps(payload).encode(), type=MbManagementRelease)
    )


@pytest.mark.asyncio
async def test_publisher_publishes_nfd_sources_to_nfc_cjk_destinations(
    tmp_path: Path,
) -> None:
    root, source, store, _audio, publisher, job_id = await _ready_apply_operation(
        tmp_path,
        configure=_nfd_source_directory,
        customize_planner=_unicode_release_configuration,
    )

    result = await publisher.publish_bundle(job_id, 0, "apply-worker")
    repeated = await publisher.publish_bundle(job_id, 0, "apply-worker")

    album = (
        unicodedata.normalize("NFC", unicodedata.normalize("NFD", "ゴールド"))
        + "変奏曲"
    )
    title = unicodedata.normalize("NFC", unicodedata.normalize("NFD", "ダリア"))
    destination = root / "作曲者" / f"{album} (1982)" / f"01 - {title}.flac"
    assert destination.is_file()
    # every published component is NFC, not the NFD the metadata arrived in
    for part in destination.relative_to(root).parts:
        assert unicodedata.is_normalized("NFC", part)
    assert source.exists() is False
    journals = await store.list_file_mutation_journals_for_bundle(job_id, 0)
    assert [journal.state for journal in journals] == ["completed"]
    assert (
        journals[0].staged_fingerprint
        == hashlib.sha256(destination.read_bytes()).hexdigest()
    )
    row = await store.get_target_track("track-1")
    assert row is not None
    assert row["relative_path"] == destination.relative_to(root).as_posix()
    assert result.catalog_revision == repeated.catalog_revision == 1


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
        "Goldberg Variations, BWV 988 (1982)/01 - Aria.flac"
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
async def test_manual_bundle_staging_failure_mid_album_rolls_back_every_journal(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """Manual-lane mirror of the import lane's preparation-failure drill: an
    OSError while staging the SECOND file of a two-file album must compensate
    the whole bundle - no published destination, both journals rolled back,
    catalog untouched."""
    root, source, store, _audio, publisher, job_id = await _ready_apply_operation(
        tmp_path,
        prepare_store=_add_second_album_track,
        customize_planner=_add_second_canonical_track,
        selection=LibraryManagementSelection(kind="albums", ids=("album-1",)),
    )
    before_first = await store.get_target_track("track-1")
    before_second = await store.get_target_track("track-2")
    original = publisher._stage_audio
    calls = 0

    def fail_second_stage(*args, **kwargs) -> None:
        nonlocal calls
        calls += 1
        if calls == 2:
            raise OSError("simulated staging failure")
        original(*args)

    monkeypatch.setattr(publisher, "_stage_audio", fail_second_stage)

    with pytest.raises(OSError, match="simulated staging failure"):
        await publisher.publish_bundle(job_id, 0, "apply-worker")

    journals = await store.list_file_mutation_journals_for_bundle(job_id, 0)
    assert [journal.state for journal in journals] == ["rolled_back", "rolled_back"]
    assert source.is_file()
    assert (root / "source2.flac").is_file()
    organized = root / "Johann Sebastian Bach; Glenn Gould"
    assert not organized.exists() or not any(organized.rglob("*.flac"))
    assert await store.get_target_track("track-1") == before_first
    assert await store.get_target_track("track-2") == before_second
    assert not list(root.rglob(".droppedneedle-management-*"))


@pytest.mark.asyncio
async def test_publisher_rejects_tampered_catalog_projection(
    tmp_path: Path,
) -> None:
    _root, source, store, _audio, publisher, job_id = await _ready_apply_operation(
        tmp_path
    )
    with sqlite3.connect(store.db_path) as connection:
        connection.execute(
            "UPDATE library_management_plan_items SET catalog_document_hash=? "
            "WHERE job_id=?",
            ("0" * 64, job_id),
        )

    with pytest.raises(ConflictError, match="Pinned catalog metadata changed"):
        await publisher.publish_bundle(job_id, 0, "apply-worker")

    assert source.is_file()


@pytest.mark.asyncio
async def test_tags_only_selected_siblings_publish_as_one_album_bundle(
    tmp_path: Path,
) -> None:
    _root, _source, store, audio, publisher, job_id = await _ready_apply_operation(
        tmp_path,
        configure=_tags_only_configuration,
        prepare_store=_add_second_album_track,
        customize_planner=_add_second_canonical_track,
        selection=LibraryManagementSelection(kind="tracks", ids=("track-1", "track-2")),
    )
    items = await store.list_library_management_plan_items(job_id)
    assert [item.bundle_ordinal for item in items] == [0, 0]

    await publisher.publish_bundle(job_id, 0, "apply-worker")

    first = await store.get_target_track("track-1")
    second = await store.get_target_track("track-2")
    assert first is not None and second is not None
    assert (
        audio.read(Path(str(first["file_path"]))).metadata.value_for("title") == "Aria"
    )
    assert (
        audio.read(Path(str(second["file_path"]))).metadata.value_for("title")
        == "Variation 1"
    )


@pytest.mark.asyncio
async def test_partial_tags_only_selection_rejects_duplicate_sibling_mapping(
    tmp_path: Path,
) -> None:
    def prepare(root, preferences, store) -> None:
        _add_second_album_track(root, preferences, store)
        with sqlite3.connect(store.db_path) as connection:
            connection.execute(
                "UPDATE local_track_external_identities SET release_track_mbid="
                "'22222222-2222-4222-8222-222222222222' WHERE local_track_id='track-2'"
            )

    _root, _source, store, _audio, _publisher, job_id = await _ready_apply_operation(
        tmp_path,
        configure=_tags_only_configuration,
        prepare_store=prepare,
        customize_planner=_add_second_canonical_track,
        selection=LibraryManagementSelection(kind="tracks", ids=("track-1",)),
    )

    items = await store.list_library_management_plan_items(job_id)
    assert len(items) == 1
    assert items[0].eligibility == "blocked"
    assert items[0].reason_code == "TRACK_NOT_MAPPED"


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
    assert not list(root.rglob("01 - Aria.flac"))
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
async def test_publisher_applies_case_only_rename_of_source(tmp_path: Path) -> None:
    root, _source, store, _audio, publisher, job_id = await _ready_apply_operation(
        tmp_path,
        prepare_store=_case_only_source,
    )
    source = root / (
        "Johann Sebastian Bach; Glenn Gould/"
        "Goldberg Variations, BWV 988 (1982)/01 - ARIA.flac"
    )
    destination = source.with_name("01 - Aria.flac")

    result = await publisher.publish_bundle(job_id, 0, "apply-worker")

    assert len(result.committed_journal_ids) == 1
    assert not source.exists()
    assert destination.is_file()
    journals = await store.list_file_mutation_journals_for_bundle(job_id, 0)
    assert [journal.state for journal in journals] == ["completed"]


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

    def fail_cleanup(_value, _roots) -> None:
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
async def test_publisher_defers_repeated_cancellation_until_publish_is_durable(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    root, source, store, _audio, publisher, job_id = await _ready_apply_operation(
        tmp_path
    )
    published = asyncio.Event()
    release = asyncio.Event()
    original_publish = publisher._publish_one

    async def pause_after_publish(value, roots) -> None:
        await original_publish(value, roots)
        published.set()
        await release.wait()

    monkeypatch.setattr(publisher, "_publish_one", pause_after_publish)
    task = asyncio.create_task(publisher.publish_bundle(job_id, 0, "apply-worker"))
    await published.wait()
    task.cancel()
    await asyncio.sleep(0)
    task.cancel()
    await asyncio.sleep(0)
    release.set()

    with pytest.raises(asyncio.CancelledError):
        await task

    row = await store.get_target_track("track-1")
    journals = await store.list_file_mutation_journals_for_bundle(job_id, 0)
    assert source.exists() is False
    assert row is not None and (root / str(row["relative_path"])).is_file()
    assert [journal.state for journal in journals] == ["completed"]


@pytest.mark.asyncio
async def test_import_publisher_defers_repeated_cancellation_through_catalog_commit(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    root, catalog_source, store, audio, _publisher, service, policy_revision = (
        _import_publication_fixture(tmp_path)
    )
    incoming = tmp_path / "cancelled-import.flac"
    shutil.copy2(catalog_source, incoming)
    request = _import_file(
        audio,
        incoming,
        ordinal=0,
        relative_path="Import Artist/Import Album/01 Cancelled.flac",
    )
    bundle = LibraryManagementImportBundle(
        idempotency_key="acquisition:repeated-cancellation:minimal",
        origin="acquisition",
        policy_revision=policy_revision,
        files=(request,),
    )
    commit_started = asyncio.Event()
    release_commit = asyncio.Event()
    original_commit = store.commit_library_management_import_bundle

    async def pause_commit(*args, **kwargs):
        commit_started.set()
        await release_commit.wait()
        return await original_commit(*args, **kwargs)

    monkeypatch.setattr(store, "commit_library_management_import_bundle", pause_commit)
    task = asyncio.create_task(service.publish_import_bundle(bundle))
    await commit_started.wait()
    task.cancel()
    await asyncio.sleep(0)
    task.cancel()
    await asyncio.sleep(0)
    release_commit.set()

    with pytest.raises(asyncio.CancelledError):
        await task

    destination = root / request.destination_relative_path
    row = await store.get_target_track_by_path(str(destination))
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
    assert incoming.exists() is False
    assert destination.is_file()
    assert row is not None
    assert record is not None and record.state == "completed"
    assert [journal.state for journal in journals] == ["completed"]


@pytest.mark.asyncio
async def test_concurrent_duplicate_import_publications_serialize_per_bundle(
    tmp_path: Path,
) -> None:
    """F-175: a second caller with the same idempotency key awaits the per-bundle
    lock and replays the state machine instead of interleaving staging under the
    winner's critical section."""
    root, catalog_source, store, audio, publisher, service, policy_revision = (
        _import_publication_fixture(tmp_path)
    )
    incoming = tmp_path / "duplicate-import.flac"
    shutil.copy2(catalog_source, incoming)
    request = _import_file(
        audio,
        incoming,
        ordinal=0,
        relative_path="Import Artist/Import Album/01 Duplicate.flac",
    )
    bundle = LibraryManagementImportBundle(
        idempotency_key="acquisition:duplicate-lane:minimal",
        origin="acquisition",
        policy_revision=policy_revision,
        files=(request,),
    )
    started = asyncio.Event()
    release = asyncio.Event()
    original = publisher._publish_import_file
    publish_calls: list[int] = []

    async def slow_first_publish(value, roots):
        publish_calls.append(value.journal.ordinal)
        if not release.is_set():
            started.set()
            await release.wait()
        return await original(value, roots)

    publisher._publish_import_file = slow_first_publish

    async def release_after_queue():
        await started.wait()
        # give the duplicate caller a chance to queue on the bundle lock
        await asyncio.sleep(0.05)
        release.set()

    results = await asyncio.gather(
        service.publish_import_bundle(bundle),
        service.publish_import_bundle(bundle),
        release_after_queue(),
    )
    publisher._publish_import_file = original

    first, second = results[0], results[1]
    assert sorted(publish_calls) == [0]
    assert first.local_track_ids == second.local_track_ids
    assert first.paths == second.paths
    assert second.repeated is True
    journals = await store.list_library_management_import_journals(first.bundle_id)
    assert [journal.state for journal in journals] == ["completed"]
    assert incoming.exists() is False
    assert (root / request.destination_relative_path).is_file()


def _cjk_canonical_track(planner) -> None:
    """Override the mocked canonical release so rendering yields CJK + accent."""
    payload = json.loads(
        (FIXTURES / "musicbrainz" / "management_release.json").read_text(
            encoding="utf-8"
        )
    )
    payload["title"] = "宇宙のアルバム Café"
    payload["artist-credit"][0]["artist"]["name"] = "宇宙の詩人"
    payload["media"][0]["tracks"][0]["title"] = "星の詩 Café"
    planner._canonical._musicbrainz.get_canonical_release.return_value = (
        msgspec.json.decode(json.dumps(payload).encode(), type=MbManagementRelease)
    )


def _cross_root_configuration(destination_root: Path):
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

    return configure


@pytest.mark.asyncio
async def test_publisher_publishes_cjk_album_with_destination_side_temps(
    tmp_path: Path,
) -> None:
    """F-152a/F-152c: CJK+accent names publish end to end and every journal
    stages its temporary under the destination root (EXDEV-safe by construction)."""
    destination_root = tmp_path / "organized"
    destination_root.mkdir()
    root, source, store, audio, publisher, job_id = await _ready_apply_operation(
        tmp_path,
        configure=_cross_root_configuration(destination_root),
        customize_planner=_cjk_canonical_track,
        target_root_id="root-2",
    )
    items = await store.get_library_management_bundle_plan_items(job_id, 0)
    planned_relative = str(items[0].destination_relative_path)
    assert "宇宙のアルバム" in planned_relative
    assert "星の詩" in planned_relative

    await publisher.publish_bundle(job_id, 0, "apply-worker")

    row = await store.get_target_track("track-1")
    assert row is not None and row["root_id"] == "root-2"
    assert str(row["relative_path"]) == planned_relative
    published = destination_root / planned_relative
    assert published.is_file()
    assert unicodedata.is_normalized(
        "NFC", str(published.relative_to(destination_root))
    )
    assert audio.read(published).metadata.value_for("title") == "星の詩 Café"
    journals = await store.list_file_mutation_journals_for_bundle(job_id, 0)
    assert journals
    assert all(journal.temporary_root_id == "root-2" for journal in journals)
    assert source.exists() is False


@pytest.mark.asyncio
async def test_publisher_fails_closed_on_nfd_normalized_sibling_directory(
    tmp_path: Path,
) -> None:
    root, source, store, _audio, publisher, job_id = await _ready_apply_operation(
        tmp_path, customize_planner=_cjk_canonical_track
    )
    items = await store.get_library_management_bundle_plan_items(job_id, 0)
    planned_relative = PurePosixPath(str(items[0].destination_relative_path))
    destination_parent = root / planned_relative.parent
    destination_parent.mkdir(parents=True)
    # The recheck scans the destination FILE's siblings for NFC-equivalent
    # names; a byte-level NFD twin of the file name must fail closed.
    nfd_name = unicodedata.normalize("NFD", planned_relative.name)
    assert nfd_name != planned_relative.name
    (destination_parent / nfd_name).write_bytes(b"nfd twin")

    with pytest.raises(Exception, match="normalized") as caught:
        await publisher.publish_bundle(job_id, 0, "apply-worker")

    del caught
    assert source.is_file()
    assert not (root / str(planned_relative)).exists()


@pytest.mark.asyncio
async def test_publish_logs_directory_fsync_failures_but_still_commits(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
    caplog: pytest.LogCaptureFixture,
) -> None:
    """F-146: a directory-fsync failure must be observable while availability
    is preserved - the commit still lands and the warning names the directory."""
    real_fsync = os.fsync
    music_root = os.path.normpath(str(tmp_path / "music"))

    def failing_directory_fsync(fd):
        if stat.S_ISDIR(os.fstat(fd).st_mode):
            try:
                directory = os.readlink(f"/proc/self/fd/{fd}")
            except OSError:
                directory = ""
            if os.path.normpath(directory).startswith(music_root):
                raise OSError(errno.EINVAL, "Invalid argument")
        return real_fsync(fd)

    monkeypatch.setattr(os, "fsync", failing_directory_fsync)
    root, source, store, _audio, publisher, job_id = await _ready_apply_operation(
        tmp_path
    )

    with caplog.at_level(
        logging.WARNING, logger="services.native.library_management_publisher"
    ):
        await publisher.publish_bundle(job_id, 0, "apply-worker")

    row = await store.get_target_track("track-1")
    journals = await store.list_file_mutation_journals_for_bundle(job_id, 0)
    assert row is not None
    assert (root / str(row["relative_path"])).is_file()
    assert [journal.state for journal in journals] == ["completed"]
    warnings = [
        record
        for record in caplog.records
        if "directory fsync failed" in record.getMessage()
    ]
    assert warnings, "expected a directory fsync failure warning"
    assert any(
        "EINVAL" in record.getMessage() or "(errno=22)" in record.getMessage()
        for record in warnings
    )


@pytest.mark.asyncio
async def test_cleanup_fsyncs_source_parent_after_unlink(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """F-147: the hot-path cleanup must persist the removed source's directory
    entry AFTER the unlink, matching the recovery protocol."""
    events: list[tuple[str, str]] = []
    real_unlink = os.unlink
    real_fsync = os.fsync

    def recording_unlink(name, *args, **kwargs):
        dir_fd = kwargs.get("dir_fd")
        if dir_fd is not None:
            target = os.path.join(
                os.readlink(f"/proc/self/fd/{dir_fd}"), os.fspath(name)
            )
        else:
            target = os.fspath(name)
        events.append(("unlink", os.path.normpath(target)))
        return real_unlink(name, *args, **kwargs)

    def recording_fsync(fd):
        try:
            path = os.readlink(f"/proc/self/fd/{fd}")
        except OSError:
            path = "<unknown>"
        events.append(("fsync", os.path.normpath(path)))
        return real_fsync(fd)

    monkeypatch.setattr(os, "unlink", recording_unlink)
    monkeypatch.setattr(os, "fsync", recording_fsync)
    root, source, store, _audio, publisher, job_id = await _ready_apply_operation(
        tmp_path
    )

    await publisher.publish_bundle(job_id, 0, "apply-worker")

    row = await store.get_target_track("track-1")
    assert row is not None and source.exists() is False
    unlink_indexes = [
        index
        for index, (kind, path) in enumerate(events)
        if kind == "unlink" and path == os.path.normpath(str(source))
    ]
    assert unlink_indexes, "expected the managed source unlink to be recorded"
    fsync_source_indexes = [
        index
        for index, (kind, path) in enumerate(events)
        if kind == "fsync" and path == os.path.normpath(str(source.parent))
    ]
    assert fsync_source_indexes, "expected the source parent directory fsync"
    assert max(fsync_source_indexes) > min(unlink_indexes), (
        "the source parent fsync must follow the source unlink"
    )


@pytest.mark.asyncio
async def test_publisher_replays_post_commit_hook_on_committed_replay(
    tmp_path: Path,
) -> None:
    """F-145 window 1: an all-terminal bundle replay re-runs the guarded
    post-commit hook so a lost enqueue is recovered on any later retry."""
    _root, _source, store, audio, publisher, job_id = await _ready_apply_operation(
        tmp_path
    )
    await publisher.publish_bundle(job_id, 0, "apply-worker")

    hook = AsyncMock()
    replay_publisher = LibraryManagementPublisher(
        store,
        publisher._preferences,
        audio,
        AudioWritePlanningService(audio),
        LibraryManagementBlobStore(tmp_path / "blobs", store),
        LibraryFilesystemCoordinator(),
        clock=lambda: 110.0,
        on_commit=hook,
    )

    result = await replay_publisher.publish_bundle(job_id, 0, "apply-worker")

    assert result.committed_journal_ids
    hook.assert_awaited_once()
    track_ids, album_ids = hook.await_args.args
    assert track_ids == {"track-1"}
    assert album_ids >= {"album-1"}


@pytest.mark.asyncio
async def test_post_commit_hook_failure_is_swallowed_with_exc_info(
    tmp_path: Path,
    caplog: pytest.LogCaptureFixture,
) -> None:
    """F-145: a throwing hook never fails a committed bundle; the warning keeps
    its traceback, and a later replay of the terminal bundle retries the hook."""
    hook = AsyncMock(side_effect=RuntimeError("refresh backend down"))
    (
        _root,
        _source,
        store,
        audio,
        publisher,
        job_id,
    ) = await _ready_apply_operation_with_hook(tmp_path, hook)

    with caplog.at_level(
        logging.WARNING, logger="services.native.library_management_publisher"
    ):
        await publisher.publish_bundle(job_id, 0, "apply-worker")
        await publisher.publish_bundle(job_id, 0, "apply-worker")

    assert hook.await_count == 2
    failures = [
        record
        for record in caplog.records
        if "post-commit invalidation failed" in record.getMessage()
    ]
    assert len(failures) == 2
    assert all(record.exc_info for record in failures)


async def _ready_apply_operation_with_hook(tmp_path: Path, hook):
    root, source, store, audio, preferences, job_id = await _bare_ready_operation(
        tmp_path
    )
    publisher = LibraryManagementPublisher(
        store,
        preferences,
        audio,
        AudioWritePlanningService(audio),
        LibraryManagementBlobStore(tmp_path / "blobs", store),
        LibraryFilesystemCoordinator(),
        clock=lambda: 110.0,
        on_commit=hook,
    )
    return root, source, store, audio, publisher, job_id


async def _bare_ready_operation(tmp_path: Path):
    root, source, preferences, store, _settings_revision, _policy_revision = (
        _configured(tmp_path)
    )
    audio = AudioMetadataEngine()
    settings_revision = preferences.get_library_management_settings().settings_revision
    policy_revision = LibraryPolicyResolver(
        preferences.get_typed_library_settings_raw()
    ).policy_revision
    planner = _planner(tmp_path, store, preferences)
    handle = await planner.create_preview(
        selection=LibraryManagementSelection(kind="tracks", ids=("track-1",)),
        profile_id=PICARD_ORGANIZER_PROFILE_ID,
        expected_settings_revision=settings_revision,
        expected_policy_revision=policy_revision,
        actor_user_id="admin",
        idempotency_key="post-commit-preview",
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
    return root, source, store, audio, preferences, handle.job_id


class _RecordingCoordinator(LibraryFilesystemCoordinator):
    """Coordinator wrapper that records lease acquisition/release ordering."""

    def __init__(self) -> None:
        super().__init__()
        self.events: list[tuple[str, tuple[str, ...]]] = []

    @asynccontextmanager
    async def write_many(self, root_ids):
        roots = tuple(sorted(set(root_ids)))
        self.events.append(("acquire", roots))
        try:
            async with super().write_many(root_ids):
                self.events.append(("held", roots))
                yield
        finally:
            self.events.append(("release", roots))


@pytest.mark.asyncio
async def test_outer_compensation_mutates_roots_only_under_the_writer_fence(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """F-106: when a failure bypasses the inner critical-section handler, the
    outer compensation must restore/unlink while a fresh write lease is held."""
    import services.native.library_management_publisher as publisher_module

    root, source, store, _audio, publisher, job_id = await _ready_apply_operation(
        tmp_path
    )
    recording = _RecordingCoordinator()
    publisher._filesystem = recording

    real_unlink = publisher_module.unlink_rooted

    def recording_unlink(roots, root_id, relative_path, **kwargs):
        recording.events.append(("unlink_rooted", (str(relative_path),)))
        return real_unlink(roots, root_id, relative_path, **kwargs)

    monkeypatch.setattr(publisher_module, "unlink_rooted", recording_unlink)

    # A destination created after preview fails the recheck inside the critical
    # section; its handler re-raises so the OUTER compensation path runs too.
    items = await store.get_library_management_bundle_plan_items(job_id, 0)
    planned_relative = str(items[0].destination_relative_path)
    planned = root / planned_relative
    planned.parent.mkdir(parents=True)
    planned.write_bytes(b"sneaky")

    with pytest.raises(Exception):
        await publisher.publish_bundle(job_id, 0, "apply-worker")

    acquires = [
        index for index, (kind, _roots) in enumerate(recording.events) if kind == "held"
    ]
    releases = [
        index
        for index, (kind, _roots) in enumerate(recording.events)
        if kind == "release"
    ]
    assert acquires and releases
    unlinks = [
        index
        for index, (kind, _paths) in enumerate(recording.events)
        if kind == "unlink_rooted"
    ]
    assert unlinks, "expected compensation unlink work to be recorded"
    for index in unlinks:
        assert any(a < index < r for a, r in zip(acquires, releases)), (
            "every compensation mutation must happen while a write lease is held"
        )
    # the fence covered every touched root of the bundle's journals
    first_acquire_roots = next(
        roots for kind, roots in recording.events if kind == "acquire"
    )
    assert "root-1" in first_acquire_roots


@pytest.mark.asyncio
async def test_publish_refuses_destination_created_in_replace_window(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """F-112: an out-of-model writer creating the destination inside the
    recheck-to-replace window must hit the NOREPLACE backstop instead of being
    silently overwritten."""
    root, source, store, _audio, publisher, job_id = await _ready_apply_operation(
        tmp_path
    )
    items = await store.get_library_management_bundle_plan_items(job_id, 0)
    planned = root / str(items[0].destination_relative_path)
    planned.parent.mkdir(parents=True)
    planned.write_bytes(b"external writer bytes")

    # silence the earlier recheck so the failure lands exactly on the
    # replace-time backstop rather than the recheck-time collision check
    async def no_recheck(_prepared, _roots):
        return None

    monkeypatch.setattr(publisher, "_recheck_prepared", no_recheck)

    with pytest.raises(
        LibraryManagementDestinationConflictError,
        match="created after preview",
    ):
        await publisher.publish_bundle(job_id, 0, "apply-worker")

    # the external file survived untouched and nothing half-published remains
    assert planned.read_bytes() == b"external writer bytes"
    assert source.is_file()
