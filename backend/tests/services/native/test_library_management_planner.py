import asyncio
import copy
import hashlib
import logging
from io import BytesIO
import json
from pathlib import Path
import shutil
import sqlite3
import threading
from types import SimpleNamespace
from unittest.mock import AsyncMock

import msgspec
from PIL import Image
import pytest

from api.v1.schemas.library_management import (
    NamingScriptSettings,
    PICARD_ORGANIZER_PROFILE_ID,
    picard_style_organizer_profile,
    settings_revision,
)
from core.config import Settings
from core.exceptions import (
    ExternalServiceError,
    PathLimitExceededError,
    ScriptValidationError,
    StaleRevisionError,
)
from infrastructure.audio.artwork_processor import ArtworkProcessor
from infrastructure.audio.metadata_engine import AudioMetadataEngine
from infrastructure.library_management_blob_store import LibraryManagementBlobStore
from infrastructure.persistence.download_store import DownloadStore
from infrastructure.persistence.native_library_store import NativeLibraryStore
from repositories.musicbrainz_management_models import MbManagementRelease
from models.library_management import (
    FILE_UNREADABLE,
    PATH_TOO_LONG,
    SCRIPT_VALIDATION_FAILED,
    LibraryManagementPlanItem,
)
from models.library_management_artwork import ArtworkCandidate, ArtworkProjection
from models.library_management_enrichment import (
    LyricsProjection,
    ReplayGainAnalysis,
    ReplayGainTrackResult,
)
from infrastructure.queue.priority_queue import RequestPriority
from models.audio_metadata import (
    AudioWritePolicy,
    DesiredAudioDocument,
    DesiredAudioField,
)
from services.native.artwork_projection_service import ArtworkProjectionService
from services.native.audio_write_planning_service import AudioWritePlanningService
from services.native.background_workload_gate import BackgroundWorkloadGate
from services.native.canonical_release_metadata_service import (
    CanonicalReleaseMetadataService,
)
from services.native.effective_metadata_projection_service import (
    EffectiveMetadataProjectionService,
)
from services.native.genre_normalizer import GenreNormalizer
from services.native.genre_projection_service import GenreProjectionService
from services.native.library_management_planner import LibraryManagementPlanner
import services.native.library_management_planner as planner_module
from services.native.library_management_preview_service import (
    LibraryManagementPreviewService,
)
from services.native.library_management_publisher import LibraryManagementPublisher
from services.native.library_management_worker import LibraryManagementWorker
from services.native.library_management_undo_service import LibraryManagementUndoService
from services.native.library_management_baseline_service import (
    LibraryManagementBaselineService,
)
from services.native.library_management_duplicate_service import (
    LibraryManagementDuplicateService,
)
from services.native.library_operation_service import LibraryOperationService
from services.native.library_operation_supervisor import LibraryOperationSupervisor
from services.native.library_policy_resolver import LibraryPolicyResolver
from services.native.naming import NamingTemplateEngine
from services.native.tagging_scripts import TaggingScriptEngine
from services.preferences_service import PreferencesService
from models.library_management_planning import LibraryManagementSelection

FIXTURES = Path(__file__).parents[2] / "fixtures"


def _preferences(tmp_path: Path, root: Path) -> PreferencesService:
    settings = Settings()
    settings.config_file_path = tmp_path / "config.json"
    settings.config_file_path.write_text(
        json.dumps(
            {
                "library_settings": {
                    "library_roots": [
                        {
                            "id": "root-1",
                            "path": str(root),
                            "label": "Music",
                            "policy": "automatic",
                            "rules": [],
                        }
                    ],
                    "staging_path": str(tmp_path / "staging"),
                }
            }
        ),
        encoding="utf-8",
    )
    return PreferencesService(settings)


def _seed_store(database: Path, audio_path: Path) -> NativeLibraryStore:
    with sqlite3.connect(database) as connection:
        connection.execute("CREATE TABLE auth_users (id TEXT PRIMARY KEY)")
        connection.execute("INSERT INTO auth_users(id) VALUES ('admin')")
    write_lock = threading.Lock()
    DownloadStore(database, write_lock)
    store = NativeLibraryStore(database, write_lock)
    metadata = audio_path.stat()
    with sqlite3.connect(database) as connection:
        connection.execute("PRAGMA foreign_keys=ON")
        connection.execute(
            "INSERT INTO local_artists "
            "(id, display_name, folded_name, normalized_name, kind, created_at, updated_at) "
            "VALUES ('artist-1', 'Alpha', 'alpha', 'alpha', 'group', 1, 1)"
        )
        connection.execute(
            "INSERT INTO local_albums "
            "(id, root_id, grouping_key, title, title_folded, album_artist_name, "
            "album_artist_name_folded, album_artist_id, grouping_source, created_at, "
            "updated_at) VALUES ('album-1', 'root-1', 'group-1', "
            "'Management Album', 'management album', 'Alpha', 'alpha', 'artist-1', "
            "'automatic', 1, 1)"
        )
        connection.execute(
            "INSERT INTO local_tracks "
            "(id, local_album_id, root_id, file_path, relative_path, path_hash, "
            "file_size_bytes, file_mtime_ns, stat_revision, stat_revision_kind, "
            "tag_revision, title, title_folded, artist_name, artist_name_folded, "
            "album_title, album_title_folded, album_artist_name, "
            "album_artist_name_folded, disc_number, track_number, year, genre, "
            "genre_folded, file_format, ingest_source, imported_at, membership_source) "
            "VALUES ('track-1', 'album-1', 'root-1', ?, 'source.flac', ?, ?, ?, ?, "
            "'exact', 'tag-1', 'Management Track', 'management track', 'Alpha', "
            "'alpha', 'Management Album', 'management album', 'Alpha', 'alpha', "
            "1, 2, 2024, 'Electronic', 'electronic', 'flac', 'scan', 1, 'automatic')",
            (
                str(audio_path),
                hashlib.sha256(b"source.flac").hexdigest(),
                metadata.st_size,
                metadata.st_mtime_ns,
                f"{metadata.st_size}:{metadata.st_mtime_ns}",
            ),
        )
        connection.execute(
            "INSERT INTO local_album_external_identities "
            "(local_album_id, provider, release_group_mbid, release_mbid, "
            "decision_source, selected_at) VALUES ('album-1', 'musicbrainz', "
            "'dcff25f1-702d-3b5e-b0da-d48172e6e62a', "
            "'aff0622e-7bd3-4fb6-9ca3-0fa19dd2340b', 'manual', 1)"
        )
        connection.execute(
            "INSERT INTO local_track_external_identities "
            "(local_track_id, provider, recording_mbid, release_mbid, "
            "release_track_mbid, medium_position, release_track_position, "
            "decision_source, selected_at) VALUES ('track-1', 'musicbrainz', "
            "'33333333-3333-4333-8333-333333333333', "
            "'aff0622e-7bd3-4fb6-9ca3-0fa19dd2340b', "
            "'22222222-2222-4222-8222-222222222222', 1, 1, 'manual', 1)"
        )
    return store


def _planner(
    tmp_path: Path,
    store: NativeLibraryStore,
    preferences: PreferencesService,
    workload_gate: BackgroundWorkloadGate | None = None,
    artwork_repository=None,
    lyrics=None,
    replaygain=None,
    canonical_release: MbManagementRelease | None = None,
) -> LibraryManagementPlanner:
    repository = AsyncMock()
    repository.get_canonical_release.return_value = (
        canonical_release
        if canonical_release is not None
        else msgspec.json.decode(
            (FIXTURES / "musicbrainz" / "management_release.json").read_bytes(),
            type=MbManagementRelease,
        )
    )
    artwork_repository = artwork_repository or AsyncMock()
    audio = AudioMetadataEngine()
    return LibraryManagementPlanner(
        store,
        preferences,
        CanonicalReleaseMetadataService(store, repository, clock=lambda: 100.0),
        EffectiveMetadataProjectionService(),
        GenreProjectionService(GenreNormalizer(), clock=lambda: 100.0),
        ArtworkProjectionService(artwork_repository, ArtworkProcessor()),
        audio,
        AudioWritePlanningService(audio),
        NamingTemplateEngine(),
        TaggingScriptEngine(),
        LibraryManagementBlobStore(tmp_path / "blobs", store),
        workload_gate,
        lyrics=lyrics,
        replaygain=replaygain,
        clock=lambda: 100.0,
    )


class _ArtworkRepository:
    def __init__(self) -> None:
        output = BytesIO()
        Image.new("RGB", (80, 60), (20, 40, 60)).save(output, format="PNG")
        self.content = output.getvalue()
        self.candidate = ArtworkCandidate(
            candidate_id="exact-front",
            source="cover_art_archive_release",
            locator="https://coverartarchive.org/release/front.png",
            image_types=("front",),
            approved=True,
            primary=True,
            source_is_exact_release=True,
        )

    async def list_management_artwork(
        self,
        *,
        entity_kind: str,
        mbid: str,
        download_size: str,
        priority: RequestPriority,
    ) -> tuple[ArtworkCandidate, ...]:
        del mbid, download_size, priority
        return (self.candidate,) if entity_kind == "release" else ()

    async def download_management_artwork(
        self,
        candidate: ArtworkCandidate,
        *,
        maximum_bytes: int,
        priority: RequestPriority,
    ) -> tuple[bytes, str | None]:
        del candidate, priority
        assert len(self.content) <= maximum_bytes
        return self.content, "image/png"


class _InitiallyFailingArtworkRepository(_ArtworkRepository):
    def __init__(self) -> None:
        super().__init__()
        self.download_calls = 0

    async def download_management_artwork(
        self,
        candidate: ArtworkCandidate,
        *,
        maximum_bytes: int,
        priority: RequestPriority,
    ) -> tuple[bytes, str | None]:
        self.download_calls += 1
        if self.download_calls == 1:
            raise ExternalServiceError("transient artwork failure")
        return await super().download_management_artwork(
            candidate,
            maximum_bytes=maximum_bytes,
            priority=priority,
        )


def _add_second_track(
    database: Path,
    source: Path,
    *,
    release_track_mbid: str,
    recording_mbid: str,
    medium_position: int = 1,
    release_track_position: int = 2,
) -> None:
    metadata = source.stat()
    with sqlite3.connect(database) as connection:
        connection.execute(
            "UPDATE local_tracks SET track_number = 1 WHERE id = 'track-1'"
        )
        connection.execute(
            "INSERT INTO local_tracks "
            "(id, local_album_id, root_id, file_path, relative_path, path_hash, "
            "file_size_bytes, file_mtime_ns, stat_revision, stat_revision_kind, "
            "tag_revision, title, title_folded, artist_name, artist_name_folded, "
            "album_title, album_title_folded, album_artist_name, "
            "album_artist_name_folded, disc_number, track_number, year, genre, "
            "genre_folded, file_format, ingest_source, imported_at, membership_source) "
            "VALUES ('track-2', 'album-1', 'root-1', ?, 'source-2.flac', ?, ?, ?, ?, "
            "'exact', 'tag-2', 'Management Track Two', 'management track two', "
            "'Alpha', 'alpha', 'Management Album', 'management album', 'Alpha', "
            "'alpha', ?, ?, 2024, 'Electronic', 'electronic', 'flac', 'scan', 1, "
            "'automatic')",
            (
                str(source),
                hashlib.sha256(b"source-2.flac").hexdigest(),
                metadata.st_size,
                metadata.st_mtime_ns,
                f"{metadata.st_size}:{metadata.st_mtime_ns}",
                medium_position,
                release_track_position,
            ),
        )
        connection.execute(
            "INSERT INTO local_track_external_identities "
            "(local_track_id, provider, recording_mbid, release_mbid, "
            "release_track_mbid, medium_position, release_track_position, "
            "decision_source, selected_at) VALUES ('track-2', 'musicbrainz', ?, "
            "'aff0622e-7bd3-4fb6-9ca3-0fa19dd2340b', ?, ?, ?, 'manual', 1)",
            (
                recording_mbid,
                release_track_mbid,
                medium_position,
                release_track_position,
            ),
        )


def _add_unidentified_album(
    database: Path,
    source: Path,
    *,
    album_number: int,
) -> None:
    metadata = source.stat()
    album_id = f"album-{album_number}"
    track_id = f"track-{album_number}"
    relative_path = source.name
    with sqlite3.connect(database) as connection:
        connection.execute(
            "INSERT INTO local_albums "
            "(id, root_id, grouping_key, title, title_folded, album_artist_name, "
            "album_artist_name_folded, album_artist_id, grouping_source, created_at, "
            "updated_at) VALUES (?, 'root-1', ?, ?, ?, 'Alpha', 'alpha', "
            "'artist-1', 'automatic', 1, 1)",
            (
                album_id,
                f"group-{album_number}",
                f"Album {album_number}",
                f"album {album_number}",
            ),
        )
        connection.execute(
            "INSERT INTO local_tracks "
            "(id, local_album_id, root_id, file_path, relative_path, path_hash, "
            "file_size_bytes, file_mtime_ns, stat_revision, stat_revision_kind, "
            "tag_revision, title, title_folded, artist_name, artist_name_folded, "
            "album_title, album_title_folded, album_artist_name, "
            "album_artist_name_folded, disc_number, track_number, year, genre, "
            "genre_folded, file_format, ingest_source, imported_at, membership_source) "
            "VALUES (?, ?, 'root-1', ?, ?, ?, ?, ?, ?, 'exact', ?, ?, ?, "
            "'Alpha', 'alpha', ?, ?, 'Alpha', 'alpha', 1, 1, 2024, "
            "'Electronic', 'electronic', 'flac', 'scan', 1, 'automatic')",
            (
                track_id,
                album_id,
                str(source),
                relative_path,
                hashlib.sha256(relative_path.encode()).hexdigest(),
                metadata.st_size,
                metadata.st_mtime_ns,
                f"{metadata.st_size}:{metadata.st_mtime_ns}",
                f"tag-{album_number}",
                f"Track {album_number}",
                f"track {album_number}",
                f"Album {album_number}",
                f"album {album_number}",
            ),
        )


def _configured(tmp_path: Path, source_setup=None):  # noqa: ANN001
    root = tmp_path / "music"
    root.mkdir()
    source = root / "source.flac"
    shutil.copy2(FIXTURES / "library" / "management_full.flac", source)
    if source_setup is not None:
        source_setup(source)
    preferences = _preferences(tmp_path, root)
    current = preferences.get_library_management_settings()
    settings = preferences.get_library_management_settings_raw()
    profile = next(
        value for value in settings.profiles if value.id == PICARD_ORGANIZER_PROFILE_ID
    )
    profile.artwork.embedded_enabled = False
    profile.artwork.external_enabled = False
    profile.genres.sources = ["musicbrainz"]
    profile.organization.move_sidecars = False
    saved = preferences.save_library_management_settings_if_current(
        settings,
        expected_settings_revision=current.settings_revision,
    )
    store = _seed_store(tmp_path / "library.db", source)
    policy_revision = LibraryPolicyResolver(
        preferences.get_typed_library_settings_raw()
    ).policy_revision
    return root, source, preferences, store, saved.settings_revision, policy_revision


@pytest.mark.asyncio
async def test_preview_is_pinned_durable_and_never_mutates_library_files(
    tmp_path: Path,
) -> None:
    (
        root,
        source,
        preferences,
        store,
        settings_revision,
        policy_revision,
    ) = _configured(tmp_path)
    with sqlite3.connect(tmp_path / "library.db") as connection:
        connection.execute(
            "INSERT INTO local_album_artwork "
            "(local_album_id, cover_url, source, source_locator, version, "
            "updated_at, row_revision) VALUES "
            "('album-1', NULL, 'provider', 'release-group-1', 7, 1, 1)"
        )
    planner = _planner(tmp_path, store, preferences)
    before = source.read_bytes()
    handle = await planner.create_preview(
        selection=LibraryManagementSelection(kind="tracks", ids=("track-1",)),
        profile_id=PICARD_ORGANIZER_PROFILE_ID,
        expected_settings_revision=settings_revision,
        expected_policy_revision=policy_revision,
        actor_user_id="admin",
        idempotency_key="preview-1",
    )
    duplicate = await planner.create_preview(
        selection=LibraryManagementSelection(kind="tracks", ids=("track-1",)),
        profile_id=PICARD_ORGANIZER_PROFILE_ID,
        expected_settings_revision=settings_revision,
        expected_policy_revision=policy_revision,
        actor_user_id="admin",
        idempotency_key="preview-1",
    )
    claimed = await store.claim_operation_job(
        "worker-1", now=100, lease_seconds=60, kind="library_management"
    )

    assert claimed is not None
    snapshot = await planner.run_claimed_preview(claimed, "worker-1")
    plan = await store.list_library_management_plan_items(handle.job_id)
    operation = await store.get_operation_job(handle.job_id)

    assert duplicate.existing is True
    assert duplicate.job_id == handle.job_id
    assert duplicate.preview_token == handle.preview_token
    assert snapshot.phase == "ready"
    assert json.loads(snapshot.summary_json)["selected_item_count"] == 1
    assert operation is not None and operation["state"] == "ready"
    assert len(plan) == 1
    assert plan[0].eligibility in {"eligible", "warning"}
    assert plan[0].expected_file_fingerprint == hashlib.sha256(before).hexdigest()
    assert json.loads(plan[0].capability_json)["album_artwork_version"] == 7
    assert plan[0].destination_relative_path == (
        "Johann Sebastian Bach; Glenn Gould/"
        "Goldberg Variations, BWV 988 (1982)/01 - Aria.flac"
    )
    desired = json.loads(plan[0].desired_document_json)
    catalog = json.loads(str(plan[0].catalog_document_json))
    assert any(
        value["name"] == "musicbrainz_release_track_id"
        and value["value"] == "22222222-2222-4222-8222-222222222222"
        for value in desired["fields"]
    )
    assert (
        plan[0].catalog_document_hash
        == hashlib.sha256(str(plan[0].catalog_document_json).encode()).hexdigest()
    )
    catalog_fields = {value["name"]: value for value in catalog["fields"]}
    assert catalog_fields["title"] == {
        "name": "title",
        "action": "unchanged",
        "value": "Aria",
    }
    assert catalog_fields["album"]["value"] == "Goldberg Variations, BWV 988"
    assert catalog["artist_display"] == "Glenn Gould"
    assert catalog["album_artist_display"] == "Johann Sebastian Bach; Glenn Gould"
    assert source.read_bytes() == before


@pytest.mark.asyncio
async def test_partial_selection_uses_exact_release_multi_disc_policy(
    tmp_path: Path,
) -> None:
    (
        root,
        _source,
        preferences,
        store,
        settings_revision,
        policy_revision,
    ) = _configured(tmp_path)
    release = msgspec.json.decode(
        (FIXTURES / "musicbrainz" / "management_release.json").read_bytes(),
        type=MbManagementRelease,
    )
    release.release_group.disambiguation = "Deluxe Edition"
    second = copy.deepcopy(release.media[0])
    second.position = 2
    second.tracks[0].id = "99999999-9999-4999-8999-999999999999"
    second.tracks[0].recording.id = "88888888-8888-4888-8888-888888888888"
    release.media.append(second)
    planner = _planner(tmp_path, store, preferences, canonical_release=release)

    handle = await planner.create_preview(
        selection=LibraryManagementSelection(kind="tracks", ids=("track-1",)),
        profile_id=PICARD_ORGANIZER_PROFILE_ID,
        expected_settings_revision=settings_revision,
        expected_policy_revision=policy_revision,
        actor_user_id="admin",
        idempotency_key="multi-disc-partial",
    )
    claimed = await store.claim_operation_job(
        "worker-1", now=100, lease_seconds=60, kind="library_management"
    )
    assert claimed is not None

    await planner.run_claimed_preview(claimed, "worker-1")
    item = (await store.list_library_management_plan_items(handle.job_id))[0]

    assert item.destination_relative_path == (
        "Johann Sebastian Bach; Glenn Gould/"
        "Goldberg Variations, BWV 988 (1982) (Deluxe Edition)/CD 01/01 - Aria.flac"
    )
    assert sorted(path.relative_to(root).as_posix() for path in root.rglob("*")) == [
        "source.flac"
    ]


@pytest.mark.asyncio
async def test_two_disc_preview_resets_track_names_inside_each_medium(
    tmp_path: Path,
) -> None:
    root, _source, preferences, store, settings_revision, policy_revision = _configured(
        tmp_path
    )
    second_source = root / "source-2.flac"
    shutil.copy2(FIXTURES / "library" / "management_full.flac", second_source)
    release = msgspec.json.decode(
        (FIXTURES / "musicbrainz" / "management_release.json").read_bytes(),
        type=MbManagementRelease,
    )
    second = copy.deepcopy(release.media[0])
    second.position = 2
    second.tracks[0].id = "99999999-9999-4999-8999-999999999999"
    second.tracks[0].position = 1
    second.tracks[0].recording.id = "88888888-8888-4888-8888-888888888888"
    second.tracks[0].title = "Second Disc Opener"
    release.media.append(second)
    _add_second_track(
        tmp_path / "library.db",
        second_source,
        release_track_mbid=second.tracks[0].id,
        recording_mbid=second.tracks[0].recording.id,
        medium_position=2,
        release_track_position=1,
    )
    planner = _planner(tmp_path, store, preferences, canonical_release=release)
    handle = await planner.create_preview(
        selection=LibraryManagementSelection(kind="albums", ids=("album-1",)),
        profile_id=PICARD_ORGANIZER_PROFILE_ID,
        expected_settings_revision=settings_revision,
        expected_policy_revision=policy_revision,
        actor_user_id="admin",
        idempotency_key="two-disc-preview",
    )
    claimed = await store.claim_operation_job(
        "worker-1", now=100, lease_seconds=60, kind="library_management"
    )
    assert claimed is not None

    await planner.run_claimed_preview(claimed, "worker-1")
    items = await store.list_library_management_plan_items(handle.job_id)

    assert [item.destination_relative_path for item in items] == [
        "Johann Sebastian Bach; Glenn Gould/Goldberg Variations, BWV 988 (1982)/CD 01/01 - Aria.flac",
        "Johann Sebastian Bach; Glenn Gould/Goldberg Variations, BWV 988 (1982)/CD 02/01 - Second Disc Opener.flac",
    ]


@pytest.mark.asyncio
async def test_preview_refuses_a_catalog_change_while_counting_its_scope(
    tmp_path: Path,
) -> None:
    (
        _root,
        _source,
        preferences,
        store,
        settings_revision,
        policy_revision,
    ) = _configured(tmp_path)
    planner = _planner(tmp_path, store, preferences)
    store.get_catalog_revision = AsyncMock(side_effect=[0, 1])

    with pytest.raises(StaleRevisionError, match="catalog changed"):
        await planner.create_preview(
            selection=LibraryManagementSelection(kind="tracks", ids=("track-1",)),
            profile_id=PICARD_ORGANIZER_PROFILE_ID,
            expected_settings_revision=settings_revision,
            expected_policy_revision=policy_revision,
            actor_user_id="admin",
            idempotency_key="catalog-count-race",
        )

    assert await store.get_operation_by_idempotency_key("catalog-count-race") is None


@pytest.mark.asyncio
async def test_root_preview_honors_pause_between_album_bundles(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    (
        root,
        source,
        preferences,
        store,
        settings_revision,
        policy_revision,
    ) = _configured(tmp_path)
    second = root / "second.flac"
    shutil.copy2(source, second)
    second_stat = second.stat()
    with sqlite3.connect(tmp_path / "library.db") as connection:
        connection.execute(
            "INSERT INTO local_albums "
            "(id, root_id, grouping_key, title, title_folded, album_artist_name, "
            "album_artist_name_folded, album_artist_id, grouping_source, created_at, "
            "updated_at) VALUES ('album-2', 'root-1', 'group-2', "
            "'Second Album', 'second album', 'Alpha', 'alpha', 'artist-1', "
            "'automatic', 1, 1)"
        )
        connection.execute(
            "INSERT INTO local_tracks "
            "(id, local_album_id, root_id, file_path, relative_path, path_hash, "
            "file_size_bytes, file_mtime_ns, stat_revision, stat_revision_kind, "
            "tag_revision, title, title_folded, artist_name, artist_name_folded, "
            "album_title, album_title_folded, album_artist_name, "
            "album_artist_name_folded, disc_number, track_number, year, genre, "
            "genre_folded, file_format, ingest_source, imported_at, membership_source) "
            "VALUES ('track-2', 'album-2', 'root-1', ?, 'second.flac', ?, ?, ?, ?, "
            "'exact', 'tag-2', 'Second Track', 'second track', 'Alpha', 'alpha', "
            "'Second Album', 'second album', 'Alpha', 'alpha', 1, 1, 2024, "
            "'Electronic', 'electronic', 'flac', 'scan', 1, 'automatic')",
            (
                str(second),
                hashlib.sha256(b"second.flac").hexdigest(),
                second_stat.st_size,
                second_stat.st_mtime_ns,
                f"{second_stat.st_size}:{second_stat.st_mtime_ns}",
            ),
        )
    planner = _planner(tmp_path, store, preferences)
    handle = await planner.create_preview(
        selection=LibraryManagementSelection(kind="roots", ids=("root-1",)),
        profile_id=PICARD_ORGANIZER_PROFILE_ID,
        expected_settings_revision=settings_revision,
        expected_policy_revision=policy_revision,
        actor_user_id="admin",
        idempotency_key="pause-between-albums",
    )
    claimed = await store.claim_operation_job(
        "worker-1", now=100, lease_seconds=60, kind="library_management"
    )
    assert claimed is not None

    calls = 0

    async def plan_album(*_args, **_kwargs):
        nonlocal calls
        calls += 1
        operation = await store.get_operation_job(handle.job_id)
        assert operation is not None
        if calls == 1:
            await store.request_operation_control(
                handle.job_id,
                control="pause",
                expected_row_revision=int(operation["row_revision"]),
                idempotency_key="pause-after-first-album",
                now=100,
            )
        return [], []

    monkeypatch.setattr(planner, "_plan_album_page_with_lease", plan_album)

    snapshot = await planner.run_claimed_preview(claimed, "worker-1")
    operation = await store.get_operation_job(handle.job_id)

    assert calls == 1
    assert snapshot.phase == "planning"
    assert operation is not None
    assert operation["state"] == "paused"
    assert operation["control_request"] == "none"


@pytest.mark.asyncio
@pytest.mark.parametrize("control", ["pause", "stop"])
async def test_root_preview_honors_control_requested_during_final_album(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
    control: str,
) -> None:
    (
        _root,
        _source,
        preferences,
        store,
        settings_revision,
        policy_revision,
    ) = _configured(tmp_path)
    planner = _planner(tmp_path, store, preferences)
    handle = await planner.create_preview(
        selection=LibraryManagementSelection(kind="roots", ids=("root-1",)),
        profile_id=PICARD_ORGANIZER_PROFILE_ID,
        expected_settings_revision=settings_revision,
        expected_policy_revision=policy_revision,
        actor_user_id="admin",
        idempotency_key=f"{control}-during-final-album",
    )
    claimed = await store.claim_operation_job(
        "worker-1", now=100, lease_seconds=60, kind="library_management"
    )
    assert claimed is not None
    original_plan_album = planner._plan_album_page_with_lease  # noqa: SLF001

    async def request_control_after_planning(*args, **kwargs):  # noqa: ANN002, ANN003
        result = await original_plan_album(*args, **kwargs)
        operation = await store.get_operation_job(handle.job_id)
        assert operation is not None
        await store.request_operation_control(
            handle.job_id,
            control=control,
            expected_row_revision=int(operation["row_revision"]),
            idempotency_key=f"{control}-after-final-album",
            now=100,
        )
        return result

    monkeypatch.setattr(
        planner,
        "_plan_album_page_with_lease",
        request_control_after_planning,
    )

    controlled = await planner.run_claimed_preview(claimed, "worker-1")
    operation = await store.get_operation_job(handle.job_id)
    staged = await store.list_library_management_plan_items(handle.job_id)

    assert controlled.phase == "planning"
    assert operation is not None and operation["state"] == (
        "paused" if control == "pause" else "stopped"
    )
    assert operation["control_request"] == "none"
    assert len(staged) == 1
    assert json.loads(str(controlled.staging_cursor))["next_ordinal"] == 1


@pytest.mark.asyncio
async def test_root_preview_honors_control_arriving_during_seal(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    (
        _root,
        _source,
        preferences,
        store,
        settings_revision,
        policy_revision,
    ) = _configured(tmp_path)
    planner = _planner(tmp_path, store, preferences)
    handle = await planner.create_preview(
        selection=LibraryManagementSelection(kind="roots", ids=("root-1",)),
        profile_id=PICARD_ORGANIZER_PROFILE_ID,
        expected_settings_revision=settings_revision,
        expected_policy_revision=policy_revision,
        actor_user_id="admin",
        idempotency_key="pause-during-seal",
    )
    claimed = await store.claim_operation_job(
        "worker-1", now=100, lease_seconds=60, kind="library_management"
    )
    assert claimed is not None
    original_finalize = store.finalize_library_management_preview

    async def request_control_before_finalize(*args, **kwargs):  # noqa: ANN002, ANN003
        operation = await store.get_operation_job(handle.job_id)
        assert operation is not None
        await store.request_operation_control(
            handle.job_id,
            control="pause",
            expected_row_revision=int(operation["row_revision"]),
            idempotency_key="pause-at-seal-transaction",
            now=100,
        )
        return await original_finalize(*args, **kwargs)

    monkeypatch.setattr(
        store,
        "finalize_library_management_preview",
        request_control_before_finalize,
    )

    controlled = await planner.run_claimed_preview(claimed, "worker-1")
    operation = await store.get_operation_job(handle.job_id)

    assert controlled.phase == "planning"
    assert operation is not None and operation["state"] == "paused"
    assert operation["control_request"] == "none"
    assert len(await store.list_library_management_plan_items(handle.job_id)) == 1


@pytest.mark.asyncio
async def test_root_preview_persists_slow_planning_progress_between_page_boundaries(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    (
        root,
        source,
        preferences,
        store,
        settings_revision,
        policy_revision,
    ) = _configured(tmp_path)
    for album_number in (2, 3):
        album_source = root / f"album-{album_number}.flac"
        shutil.copy2(source, album_source)
        _add_unidentified_album(
            tmp_path / "library.db",
            album_source,
            album_number=album_number,
        )
    planner = _planner(tmp_path, store, preferences)
    # F-076 adds monotonic reads per inspection; keep a long 31s tail so
    # the fake clock never exhausts while still crossing the persist interval
    timestamps = iter((0.0, *(31.0 for _ in range(200))))
    planner._monotonic_clock = lambda: next(timestamps)  # noqa: SLF001
    handle = await planner.create_preview(
        selection=LibraryManagementSelection(kind="roots", ids=("root-1",)),
        profile_id=PICARD_ORGANIZER_PROFILE_ID,
        expected_settings_revision=settings_revision,
        expected_policy_revision=policy_revision,
        actor_user_id="admin",
        idempotency_key="persist-slow-progress",
    )
    claimed = await store.claim_operation_job(
        "worker-1", now=100, lease_seconds=60, kind="library_management"
    )
    assert claimed is not None
    original_append = store.append_library_management_plan_items
    batches: list[tuple[int, dict[str, object]]] = []

    async def record_append(
        job_id: str,
        items: list[LibraryManagementPlanItem],
        *,
        expected_snapshot_revision: int,
        staging_cursor: str | None = None,
    ) -> int:
        assert staging_cursor is not None
        batches.append((len(items), json.loads(staging_cursor)))
        return await original_append(
            job_id,
            items,
            expected_snapshot_revision=expected_snapshot_revision,
            staging_cursor=staging_cursor,
        )

    monkeypatch.setattr(store, "append_library_management_plan_items", record_append)

    snapshot = await planner.run_claimed_preview(claimed, "worker-1")

    assert snapshot.phase == "ready"
    assert [size for size, _cursor in batches] == [1, 2]
    assert batches[0][1]["album_id"] == "album-1"
    assert batches[0][1]["bundle_ordinal"] == 0
    assert batches[0][1]["next_ordinal"] == 1
    assert batches[0][1]["track_id"] == "track-1"
    assert batches[1][1]["album_id"] == "album-3"
    assert batches[1][1]["next_ordinal"] == 3
    assert len(await store.list_library_management_plan_items(handle.job_id)) == 3


@pytest.mark.asyncio
async def test_root_preview_yields_when_scan_starts_mid_page(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    (
        root,
        source,
        preferences,
        store,
        settings_revision,
        policy_revision,
    ) = _configured(tmp_path)
    second = root / "album-2.flac"
    shutil.copy2(source, second)
    _add_unidentified_album(
        tmp_path / "library.db",
        second,
        album_number=2,
    )
    gate = BackgroundWorkloadGate()
    planner = _planner(tmp_path, store, preferences, gate)
    handle = await planner.create_preview(
        selection=LibraryManagementSelection(kind="roots", ids=("root-1",)),
        profile_id=PICARD_ORGANIZER_PROFILE_ID,
        expected_settings_revision=settings_revision,
        expected_policy_revision=policy_revision,
        actor_user_id="admin",
        idempotency_key="scan-starts-mid-page",
    )
    claimed = await store.claim_operation_job(
        "worker-1", now=100, lease_seconds=60, kind="library_management"
    )
    assert claimed is not None
    original_plan_album = planner._plan_album_page_with_lease  # noqa: SLF001
    calls = 0

    async def activate_scan_after_first_album(*args, **kwargs):  # noqa: ANN002, ANN003
        nonlocal calls
        result = await original_plan_album(*args, **kwargs)
        calls += 1
        if calls == 1:
            gate.set_scan_active(True)
        return result

    monkeypatch.setattr(
        planner,
        "_plan_album_page_with_lease",
        activate_scan_after_first_album,
    )

    deferred = await planner.run_claimed_preview(claimed, "worker-1")
    operation = await store.get_operation_job(handle.job_id)
    staged = await store.list_library_management_plan_items(handle.job_id)

    assert calls == 1
    assert deferred.phase == "planning"
    assert operation is not None and operation["state"] == "queued"
    assert len(staged) == 1
    assert staged[0].local_album_id == "album-1"
    assert json.loads(str(deferred.staging_cursor))["next_ordinal"] == 1

    gate.set_scan_active(False)
    resumed = await store.claim_operation_job(
        "worker-2", now=101, lease_seconds=60, kind="library_management"
    )
    assert resumed is not None

    completed = await planner.run_claimed_preview(resumed, "worker-2")

    assert completed.phase == "ready"
    assert calls == 2
    assert len(await store.list_library_management_plan_items(handle.job_id)) == 2


@pytest.mark.asyncio
async def test_preview_restart_after_final_progress_checkpoint_does_not_duplicate(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    (
        _root,
        _source,
        preferences,
        store,
        settings_revision,
        policy_revision,
    ) = _configured(tmp_path)
    planner = _planner(tmp_path, store, preferences)
    handle = await planner.create_preview(
        selection=LibraryManagementSelection(kind="tracks", ids=("track-1",)),
        profile_id=PICARD_ORGANIZER_PROFILE_ID,
        expected_settings_revision=settings_revision,
        expected_policy_revision=policy_revision,
        actor_user_id="admin",
        idempotency_key="restart-after-final-checkpoint",
    )
    claimed = await store.claim_operation_job(
        "worker-1", now=100, lease_seconds=60, kind="library_management"
    )
    assert claimed is not None

    async def crash_before_seal(*_args, **_kwargs):
        raise RuntimeError("simulated process exit after durable progress")

    monkeypatch.setattr(planner, "_seal_preview", crash_before_seal)
    with pytest.raises(RuntimeError, match="simulated process exit"):
        await planner.run_claimed_preview(claimed, "worker-1")

    staged = await store.get_library_management_job_snapshot(handle.job_id)
    assert staged is not None
    assert staged.phase == "planning"
    assert json.loads(str(staged.staging_cursor))["next_ordinal"] == 1
    assert len(await store.list_library_management_plan_items(handle.job_id)) == 1

    resumed_planner = _planner(tmp_path, store, preferences)
    completed = await resumed_planner.run_claimed_preview(claimed, "worker-1")

    assert completed.phase == "ready"
    assert len(await store.list_library_management_plan_items(handle.job_id)) == 1


def test_scrubbed_raw_tag_evidence_includes_values_and_fingerprint() -> None:
    document = AudioMetadataEngine().read(FIXTURES / "library" / "management_full.flac")
    evidence = planner_module._scrubbed_raw_tag_evidence(  # noqa: SLF001
        document, ("custom_keep",)
    )

    marker = evidence[0]
    assert marker == {
        "key": "custom_keep",
        "value_kind": "text",
        "values": ["opaque local value"],
        "value_count": 1,
        "truncated": False,
        "sha256": hashlib.sha256(
            msgspec.json.encode(("opaque local value",))
        ).hexdigest(),
    }


@pytest.mark.asyncio
async def test_preview_preserves_embedded_artwork_without_replacement(
    tmp_path: Path,
) -> None:
    (
        _root,
        source,
        preferences,
        store,
        settings_revision,
        policy_revision,
    ) = _configured(tmp_path)
    settings = preferences.get_library_management_settings_raw()
    profile = next(
        value for value in settings.profiles if value.id == PICARD_ORGANIZER_PROFILE_ID
    )
    profile.artwork.embedded_enabled = True
    saved = preferences.save_library_management_settings_if_current(
        settings, expected_settings_revision=settings_revision
    )
    planner = _planner(tmp_path, store, preferences)
    planner._artwork.project = AsyncMock(
        return_value=ArtworkProjection(preserved_existing=True)
    )
    existing = planner._audio.read(source).artwork
    handle = await planner.create_preview(
        selection=LibraryManagementSelection(kind="tracks", ids=("track-1",)),
        profile_id=PICARD_ORGANIZER_PROFILE_ID,
        expected_settings_revision=saved.settings_revision,
        expected_policy_revision=policy_revision,
        actor_user_id="admin",
        idempotency_key="preserved-artwork-preview",
    )
    claimed = await store.claim_operation_job(
        "worker-1", now=100, lease_seconds=60, kind="library_management"
    )
    assert claimed is not None

    await planner.run_claimed_preview(claimed, "worker-1")

    item = (await store.list_library_management_plan_items(handle.job_id))[0]
    assert existing
    assert json.loads(item.diff_json)["artwork_changed"] is False


@pytest.mark.asyncio
async def test_preview_pins_exact_plain_and_synced_lyrics_without_mutating_source(
    tmp_path: Path,
) -> None:
    (
        _root,
        source,
        preferences,
        store,
        settings_revision,
        policy_revision,
    ) = _configured(tmp_path)
    settings = preferences.get_library_management_settings_raw()
    profile = next(
        value for value in settings.profiles if value.id == PICARD_ORGANIZER_PROFILE_ID
    )
    profile.enrichment.lyrics.enabled = True
    saved = preferences.save_library_management_settings_if_current(
        settings, expected_settings_revision=settings_revision
    )
    lyrics = AsyncMock()
    lyrics.project.return_value = LyricsProjection(
        status="available",
        plain_lyrics="Pinned lyrics",
        synced_lyrics="[00:01.000]Pinned lyrics",
        provider_id=42,
        provider_revision="lrclib-revision",
    )
    planner = _planner(tmp_path, store, preferences, lyrics=lyrics)
    before = source.read_bytes()

    handle = await planner.create_preview(
        selection=LibraryManagementSelection(kind="tracks", ids=("track-1",)),
        profile_id=PICARD_ORGANIZER_PROFILE_ID,
        expected_settings_revision=saved.settings_revision,
        expected_policy_revision=policy_revision,
        actor_user_id="admin",
        idempotency_key="lyrics-preview",
    )
    claimed = await store.claim_operation_job(
        "worker-1", now=100, lease_seconds=60, kind="library_management"
    )
    await planner.run_claimed_preview(claimed, "worker-1")
    item = (await store.list_library_management_plan_items(handle.job_id))[0]
    desired = json.loads(item.desired_document_json)
    diff = json.loads(item.diff_json)

    assert item.eligibility in {"eligible", "warning"}
    assert any(
        value["name"] == "lyrics_plain" and value["value"] == "Pinned lyrics"
        for value in desired["fields"]
    )
    assert any(
        value["name"] == "lyrics_synced"
        and value["value"] == "[00:01.000]Pinned lyrics"
        for value in desired["fields"]
    )
    assert diff["lyrics_projection"] == {
        "status": "available",
        "provider_id": 42,
        "provider_revision": "lrclib-revision",
        "reason": None,
        "plain_available": True,
        "synced_available": True,
        "plain_selected": True,
        "synced_selected": True,
        "synced_supported": True,
        "preserve_existing": False,
    }
    assert source.read_bytes() == before


@pytest.mark.parametrize("audio_format", ("m4a", "aac"))
@pytest.mark.asyncio
async def test_preview_falls_back_to_plain_lyrics_when_synced_is_unsupported(
    tmp_path: Path, audio_format: str
) -> None:
    (
        root,
        source,
        preferences,
        store,
        settings_revision,
        policy_revision,
    ) = _configured(tmp_path)
    replacement = root / f"source.{audio_format}"
    shutil.copy2(FIXTURES / "library" / f"management_full.{audio_format}", replacement)
    source.unlink()
    metadata = replacement.stat()
    with sqlite3.connect(tmp_path / "library.db") as connection:
        connection.execute(
            "UPDATE local_tracks SET file_path = ?, relative_path = ?, path_hash = ?, "
            "file_size_bytes = ?, file_mtime_ns = ?, stat_revision = ?, "
            "file_format = ? WHERE id = 'track-1'",
            (
                str(replacement),
                replacement.name,
                hashlib.sha256(replacement.name.encode()).hexdigest(),
                metadata.st_size,
                metadata.st_mtime_ns,
                f"{metadata.st_size}:{metadata.st_mtime_ns}",
                audio_format,
            ),
        )
    settings = preferences.get_library_management_settings_raw()
    profile = next(
        value for value in settings.profiles if value.id == PICARD_ORGANIZER_PROFILE_ID
    )
    profile.enrichment.lyrics.enabled = True
    saved = preferences.save_library_management_settings_if_current(
        settings, expected_settings_revision=settings_revision
    )
    lyrics = AsyncMock()
    lyrics.project.return_value = LyricsProjection(
        status="available",
        plain_lyrics="Pinned lyrics",
        synced_lyrics="[00:01.000]Pinned lyrics",
        provider_id=42,
        provider_revision="lrclib-revision",
    )
    planner = _planner(tmp_path, store, preferences, lyrics=lyrics)
    before = replacement.read_bytes()

    handle = await planner.create_preview(
        selection=LibraryManagementSelection(kind="tracks", ids=("track-1",)),
        profile_id=PICARD_ORGANIZER_PROFILE_ID,
        expected_settings_revision=saved.settings_revision,
        expected_policy_revision=policy_revision,
        actor_user_id="admin",
        idempotency_key=f"lyrics-{audio_format}-fallback-preview",
    )
    claimed = await store.claim_operation_job(
        "worker-1", now=100, lease_seconds=60, kind="library_management"
    )
    await planner.run_claimed_preview(claimed, "worker-1")
    item = (await store.list_library_management_plan_items(handle.job_id))[0]
    desired = json.loads(item.desired_document_json)
    diff = json.loads(item.diff_json)
    capability = json.loads(item.capability_json)

    assert any(
        value["name"] == "lyrics_plain" and value["value"] == "Pinned lyrics"
        for value in desired["fields"]
    )
    assert not any(value["name"] == "lyrics_synced" for value in desired["fields"])
    assert diff["lyrics_projection"]["synced_supported"] is False
    expected_blockers = (
        ["performer is not supported by the m4a adapter"]
        if audio_format == "m4a"
        else []
    )
    assert capability["blockers"] == expected_blockers
    if audio_format == "m4a":
        assert item.reason_code == "FIELD_UNSUPPORTED_BY_FORMAT"
        assert all(
            blocker != item.reason_code for blocker in capability["blockers"]
        )
    assert not any("lyrics_synced" in blocker for blocker in capability["blockers"])
    assert replacement.read_bytes() == before


@pytest.mark.asyncio
async def test_required_lyrics_failure_blocks_preview_item(tmp_path: Path) -> None:
    (
        _root,
        _source,
        preferences,
        store,
        settings_revision,
        policy_revision,
    ) = _configured(tmp_path)
    settings = preferences.get_library_management_settings_raw()
    profile = next(
        value for value in settings.profiles if value.id == PICARD_ORGANIZER_PROFILE_ID
    )
    profile.enrichment.lyrics.enabled = True
    profile.enrichment.lyrics.required = True
    saved = preferences.save_library_management_settings_if_current(
        settings, expected_settings_revision=settings_revision
    )
    lyrics = AsyncMock()
    lyrics.project.return_value = LyricsProjection(
        status="not_found", reason="No exact match."
    )
    planner = _planner(tmp_path, store, preferences, lyrics=lyrics)

    handle = await planner.create_preview(
        selection=LibraryManagementSelection(kind="tracks", ids=("track-1",)),
        profile_id=PICARD_ORGANIZER_PROFILE_ID,
        expected_settings_revision=saved.settings_revision,
        expected_policy_revision=policy_revision,
        actor_user_id="admin",
        idempotency_key="required-lyrics-preview",
    )
    claimed = await store.claim_operation_job(
        "worker-1", now=100, lease_seconds=60, kind="library_management"
    )
    await planner.run_claimed_preview(claimed, "worker-1")
    item = (await store.list_library_management_plan_items(handle.job_id))[0]

    assert item.eligibility == "blocked"
    assert item.reason_code == "METADATA_UNAVAILABLE"


@pytest.mark.asyncio
async def test_album_aware_replaygain_values_are_pinned_in_preview(
    tmp_path: Path,
) -> None:
    (
        _root,
        source,
        preferences,
        store,
        settings_revision,
        policy_revision,
    ) = _configured(tmp_path)
    settings = preferences.get_library_management_settings_raw()
    profile = next(
        value for value in settings.profiles if value.id == PICARD_ORGANIZER_PROFILE_ID
    )
    profile.enrichment.replaygain.enabled = True
    profile.enrichment.replaygain.mode = "replace"
    saved = preferences.save_library_management_settings_if_current(
        settings, expected_settings_revision=settings_revision
    )
    replaygain = AsyncMock()
    replaygain.analyze.return_value = ReplayGainAnalysis(
        status="available",
        analyzer_version="loudgain 0.6.8",
        tracks=(
            ReplayGainTrackResult(
                source_path=str(source),
                track_gain_db=3.75,
                track_peak=0.125093,
                album_gain_db=5.7,
                album_peak=0.125093,
            ),
        ),
    )
    planner = _planner(tmp_path, store, preferences, replaygain=replaygain)

    handle = await planner.create_preview(
        selection=LibraryManagementSelection(kind="tracks", ids=("track-1",)),
        profile_id=PICARD_ORGANIZER_PROFILE_ID,
        expected_settings_revision=saved.settings_revision,
        expected_policy_revision=policy_revision,
        actor_user_id="admin",
        idempotency_key="replaygain-preview",
    )
    claimed = await store.claim_operation_job(
        "worker-1", now=100, lease_seconds=60, kind="library_management"
    )
    await planner.run_claimed_preview(claimed, "worker-1")
    item = (await store.list_library_management_plan_items(handle.job_id))[0]
    desired = {
        value["name"]: value["value"]
        for value in json.loads(item.desired_document_json)["fields"]
    }
    diff = json.loads(item.diff_json)

    assert desired["replaygain_track_gain"] == 3.75
    assert desired["replaygain_track_peak"] == 0.125093
    assert desired["replaygain_album_gain"] == 5.7
    assert desired["replaygain_album_peak"] == 0.125093
    assert diff["replaygain_analysis"] == {
        "status": "available",
        "analyzer": "loudgain",
        "analyzer_version": "loudgain 0.6.8",
        "reason": None,
    }
    replaygain.analyze.assert_awaited_once_with((source,), album_aware=True)


@pytest.mark.asyncio
async def test_replaygain_fill_missing_preserves_valid_existing_values(
    tmp_path: Path,
) -> None:
    def seed_replaygain(source: Path) -> None:
        engine = AudioMetadataEngine()
        plan = engine.plan(
            engine.read(source),
            DesiredAudioDocument(
                fields=(
                    DesiredAudioField(
                        name="replaygain_track_gain", action="set", value=9.0
                    ),
                )
            ),
            AudioWritePolicy(),
        )
        engine.apply(source, plan)

    (
        _root,
        source,
        preferences,
        store,
        settings_revision,
        policy_revision,
    ) = _configured(tmp_path, source_setup=seed_replaygain)
    settings = preferences.get_library_management_settings_raw()
    profile = next(
        value for value in settings.profiles if value.id == PICARD_ORGANIZER_PROFILE_ID
    )
    profile.enrichment.replaygain.enabled = True
    profile.enrichment.replaygain.mode = "fill_missing"
    saved = preferences.save_library_management_settings_if_current(
        settings, expected_settings_revision=settings_revision
    )
    replaygain = AsyncMock()
    replaygain.analyze.return_value = ReplayGainAnalysis(
        status="available",
        tracks=(
            ReplayGainTrackResult(
                source_path=str(source),
                track_gain_db=1.25,
                track_peak=0.5,
                album_gain_db=2.5,
                album_peak=0.6,
            ),
        ),
    )
    planner = _planner(tmp_path, store, preferences, replaygain=replaygain)

    handle = await planner.create_preview(
        selection=LibraryManagementSelection(kind="tracks", ids=("track-1",)),
        profile_id=PICARD_ORGANIZER_PROFILE_ID,
        expected_settings_revision=saved.settings_revision,
        expected_policy_revision=policy_revision,
        actor_user_id="admin",
        idempotency_key="replaygain-fill-preview",
    )
    claimed = await store.claim_operation_job(
        "worker-1", now=100, lease_seconds=60, kind="library_management"
    )
    await planner.run_claimed_preview(claimed, "worker-1")
    desired = {
        value["name"]: value["value"]
        for value in json.loads(
            (await store.list_library_management_plan_items(handle.job_id))[
                0
            ].desired_document_json
        )["fields"]
    }

    assert "replaygain_track_gain" not in desired
    assert desired["replaygain_track_peak"] == 0.5
    assert desired["replaygain_album_gain"] == 2.5
    assert desired["replaygain_album_peak"] == 0.6


@pytest.mark.asyncio
async def test_replaygain_preserve_mode_never_runs_analyzer(tmp_path: Path) -> None:
    (
        _root,
        _source,
        preferences,
        store,
        settings_revision,
        policy_revision,
    ) = _configured(tmp_path)
    settings = preferences.get_library_management_settings_raw()
    profile = next(
        value for value in settings.profiles if value.id == PICARD_ORGANIZER_PROFILE_ID
    )
    profile.enrichment.replaygain.enabled = True
    profile.enrichment.replaygain.mode = "preserve"
    saved = preferences.save_library_management_settings_if_current(
        settings, expected_settings_revision=settings_revision
    )
    replaygain = AsyncMock()
    planner = _planner(tmp_path, store, preferences, replaygain=replaygain)

    handle = await planner.create_preview(
        selection=LibraryManagementSelection(kind="tracks", ids=("track-1",)),
        profile_id=PICARD_ORGANIZER_PROFILE_ID,
        expected_settings_revision=saved.settings_revision,
        expected_policy_revision=policy_revision,
        actor_user_id="admin",
        idempotency_key="replaygain-preserve-preview",
    )
    claimed = await store.claim_operation_job(
        "worker-1", now=100, lease_seconds=60, kind="library_management"
    )
    await planner.run_claimed_preview(claimed, "worker-1")
    desired = json.loads(
        (await store.list_library_management_plan_items(handle.job_id))[
            0
        ].desired_document_json
    )

    assert not any(
        value["name"].startswith("replaygain_") for value in desired["fields"]
    )
    replaygain.analyze.assert_not_called()


@pytest.mark.asyncio
async def test_required_replaygain_failure_blocks_entire_album(tmp_path: Path) -> None:
    (
        _root,
        _source,
        preferences,
        store,
        settings_revision,
        policy_revision,
    ) = _configured(tmp_path)
    settings = preferences.get_library_management_settings_raw()
    profile = next(
        value for value in settings.profiles if value.id == PICARD_ORGANIZER_PROFILE_ID
    )
    profile.enrichment.replaygain.enabled = True
    profile.enrichment.replaygain.mode = "replace"
    profile.enrichment.replaygain.required = True
    saved = preferences.save_library_management_settings_if_current(
        settings, expected_settings_revision=settings_revision
    )
    replaygain = AsyncMock()
    replaygain.analyze.return_value = ReplayGainAnalysis(
        status="deferred", reason="Analyzer unavailable."
    )
    planner = _planner(tmp_path, store, preferences, replaygain=replaygain)

    handle = await planner.create_preview(
        selection=LibraryManagementSelection(kind="tracks", ids=("track-1",)),
        profile_id=PICARD_ORGANIZER_PROFILE_ID,
        expected_settings_revision=saved.settings_revision,
        expected_policy_revision=policy_revision,
        actor_user_id="admin",
        idempotency_key="required-replaygain-preview",
    )
    claimed = await store.claim_operation_job(
        "worker-1", now=100, lease_seconds=60, kind="library_management"
    )
    await planner.run_claimed_preview(claimed, "worker-1")
    item = (await store.list_library_management_plan_items(handle.job_id))[0]

    assert item.eligibility == "blocked"
    assert item.reason_code == "METADATA_UNAVAILABLE"


@pytest.mark.asyncio
async def test_preview_rejects_changed_settings_before_reading_files(
    tmp_path: Path,
) -> None:
    (
        _root,
        source,
        preferences,
        store,
        settings_revision,
        policy_revision,
    ) = _configured(tmp_path)
    planner = _planner(tmp_path, store, preferences)
    handle = await planner.create_preview(
        selection=LibraryManagementSelection(kind="tracks", ids=("track-1",)),
        profile_id=PICARD_ORGANIZER_PROFILE_ID,
        expected_settings_revision=settings_revision,
        expected_policy_revision=policy_revision,
        actor_user_id="admin",
        idempotency_key=None,
    )
    current = preferences.get_library_management_settings()
    changed = preferences.get_library_management_settings_raw()
    changed.undo_retention_days += 1
    preferences.save_library_management_settings_if_current(
        changed, expected_settings_revision=current.settings_revision
    )
    claimed = await store.claim_operation_job(
        "worker-1", now=100, lease_seconds=60, kind="library_management"
    )

    with pytest.raises(StaleRevisionError, match="settings changed"):
        await planner.run_claimed_preview(claimed, "worker-1")

    assert await store.list_library_management_plan_items(handle.job_id) == []
    assert source.exists()


@pytest.mark.asyncio
async def test_preview_idempotency_key_rejects_a_different_request(
    tmp_path: Path,
) -> None:
    (
        _root,
        _source,
        preferences,
        store,
        settings_revision,
        policy_revision,
    ) = _configured(tmp_path)
    planner = _planner(tmp_path, store, preferences)
    await planner.create_preview(
        selection=LibraryManagementSelection(kind="tracks", ids=("track-1",)),
        profile_id=PICARD_ORGANIZER_PROFILE_ID,
        expected_settings_revision=settings_revision,
        expected_policy_revision=policy_revision,
        actor_user_id="admin",
        idempotency_key="same-key",
    )

    with pytest.raises(StaleRevisionError, match="different preview request"):
        await planner.create_preview(
            selection=LibraryManagementSelection(kind="albums", ids=("album-1",)),
            profile_id=PICARD_ORGANIZER_PROFILE_ID,
            expected_settings_revision=settings_revision,
            expected_policy_revision=policy_revision,
            actor_user_id="admin",
            idempotency_key="same-key",
        )


@pytest.mark.asyncio
async def test_preview_idempotency_binds_unsaved_activation_settings(
    tmp_path: Path,
) -> None:
    (
        _root,
        _source,
        preferences,
        store,
        current_revision,
        policy_revision,
    ) = _configured(tmp_path)
    planner = _planner(tmp_path, store, preferences)
    proposed = preferences.get_library_management_settings_raw()
    proposed.preview_retention_hours += 1
    profile = next(
        value for value in proposed.profiles if value.id == PICARD_ORGANIZER_PROFILE_ID
    )
    handle = await planner.create_preview(
        selection=LibraryManagementSelection(kind="roots", ids=("root-1",)),
        profile_id=profile.id,
        expected_settings_revision=current_revision,
        expected_policy_revision=policy_revision,
        actor_user_id="admin",
        idempotency_key="activation-preview",
        settings_snapshot=proposed,
        effective_profile=profile,
    )
    snapshot = await store.get_library_management_job_snapshot(handle.job_id)

    assert snapshot is not None
    assert snapshot.settings_revision == current_revision
    assert snapshot.proposed_settings_revision == settings_revision(proposed)

    changed_proposal = msgspec.convert(
        msgspec.to_builtins(proposed), type=type(proposed)
    )
    changed_proposal.undo_retention_days += 1
    with pytest.raises(StaleRevisionError, match="different preview request"):
        await planner.create_preview(
            selection=LibraryManagementSelection(kind="roots", ids=("root-1",)),
            profile_id=profile.id,
            expected_settings_revision=current_revision,
            expected_policy_revision=policy_revision,
            actor_user_id="admin",
            idempotency_key="activation-preview",
            settings_snapshot=changed_proposal,
            effective_profile=profile,
        )


@pytest.mark.asyncio
async def test_equivalent_activation_previews_reuse_the_active_durable_job(
    tmp_path: Path,
) -> None:
    (
        _root,
        _source,
        preferences,
        store,
        current_revision,
        policy_revision,
    ) = _configured(tmp_path)
    planner = _planner(tmp_path, store, preferences)
    proposed = preferences.get_library_management_settings_raw()
    proposed.preview_retention_hours += 1
    profile = next(
        value for value in proposed.profiles if value.id == PICARD_ORGANIZER_PROFILE_ID
    )
    first = await planner.create_preview(
        selection=LibraryManagementSelection(kind="roots", ids=("root-1",)),
        profile_id=profile.id,
        expected_settings_revision=current_revision,
        expected_policy_revision=policy_revision,
        actor_user_id="admin",
        idempotency_key="activation-first-click",
        settings_snapshot=proposed,
        effective_profile=profile,
    )
    claimed = await store.claim_operation_job(
        "worker-1", now=20, lease_seconds=60, kind="library_management"
    )

    resumed = await planner.create_preview(
        selection=LibraryManagementSelection(kind="roots", ids=("root-1",)),
        profile_id=profile.id,
        expected_settings_revision=current_revision,
        expected_policy_revision=policy_revision,
        actor_user_id="another-admin",
        idempotency_key="activation-after-reload",
        settings_snapshot=proposed,
        effective_profile=profile,
    )
    jobs = await store.list_operation_jobs(kind="library_management")

    assert claimed is not None and claimed["id"] == first.job_id
    assert resumed.existing is True
    assert resumed.job_id == first.job_id
    assert resumed.preview_token == first.preview_token
    assert [job["id"] for job in jobs] == [first.job_id]


@pytest.mark.asyncio
async def test_changed_activation_supersedes_running_preview_before_queueing_new_one(
    tmp_path: Path,
) -> None:
    (
        _root,
        _source,
        preferences,
        store,
        current_revision,
        policy_revision,
    ) = _configured(tmp_path)
    planner = _planner(tmp_path, store, preferences)
    proposed = preferences.get_library_management_settings_raw()
    profile = next(
        value for value in proposed.profiles if value.id == PICARD_ORGANIZER_PROFILE_ID
    )
    first = await planner.create_preview(
        selection=LibraryManagementSelection(kind="roots", ids=("root-1",)),
        profile_id=profile.id,
        expected_settings_revision=current_revision,
        expected_policy_revision=policy_revision,
        actor_user_id="admin",
        idempotency_key="activation-old-proposal",
        settings_snapshot=proposed,
        effective_profile=profile,
    )
    await store.claim_operation_job(
        "worker-1", now=20, lease_seconds=60, kind="library_management"
    )
    changed_proposal = msgspec.convert(
        msgspec.to_builtins(proposed), type=type(proposed)
    )
    changed_proposal.undo_retention_days += 1
    changed_profile = next(
        value
        for value in changed_proposal.profiles
        if value.id == PICARD_ORGANIZER_PROFILE_ID
    )

    replacement = await planner.create_preview(
        selection=LibraryManagementSelection(kind="roots", ids=("root-1",)),
        profile_id=changed_profile.id,
        expected_settings_revision=current_revision,
        expected_policy_revision=policy_revision,
        actor_user_id="admin",
        idempotency_key="activation-new-proposal",
        settings_snapshot=changed_proposal,
        effective_profile=changed_profile,
    )
    old_job = await store.get_operation_job(first.job_id)
    new_job = await store.get_operation_job(replacement.job_id)

    assert replacement.job_id != first.job_id
    assert old_job is not None
    assert old_job["state"] == "running"
    assert old_job["control_request"] == "stop"
    assert old_job["terminal_code"] == "SUPERSEDED_ACTIVATION_PREVIEW"
    assert new_job is not None and new_job["state"] == "queued"

    stopped = await store.checkpoint_operation_control(first.job_id, "worker-1", now=21)
    assert stopped is not None and stopped["state"] == "stopped"
    assert stopped["terminal_code"] == "SUPERSEDED_ACTIVATION_PREVIEW"


@pytest.mark.asyncio
async def test_management_preview_dispatches_through_supervisor_and_defers_for_scan(
    tmp_path: Path,
) -> None:
    (
        _root,
        _source,
        preferences,
        store,
        settings_revision,
        policy_revision,
    ) = _configured(tmp_path)
    gate = BackgroundWorkloadGate()
    planner = _planner(tmp_path, store, preferences, gate)
    worker = LibraryManagementWorker(
        store,
        planner,
        AsyncMock(spec=LibraryManagementPublisher),
        AsyncMock(spec=LibraryManagementUndoService),
        AsyncMock(spec=LibraryManagementBaselineService),
        AsyncMock(spec=LibraryManagementDuplicateService),
    )
    supervisor = LibraryOperationSupervisor(
        store,
        LibraryOperationService(store),
        AsyncMock(),
        AsyncMock(),
        gate,
        worker,
    )
    first = await planner.create_preview(
        selection=LibraryManagementSelection(kind="tracks", ids=("track-1",)),
        profile_id=PICARD_ORGANIZER_PROFILE_ID,
        expected_settings_revision=settings_revision,
        expected_policy_revision=policy_revision,
        actor_user_id="admin",
        idempotency_key="supervised-preview",
    )

    ready = await supervisor.run_once("worker-1", now=100)

    assert ready is not None and ready.id == first.job_id
    assert ready.state == "ready"

    second = await planner.create_preview(
        selection=LibraryManagementSelection(kind="tracks", ids=("track-1",)),
        profile_id=PICARD_ORGANIZER_PROFILE_ID,
        expected_settings_revision=settings_revision,
        expected_policy_revision=policy_revision,
        actor_user_id="admin",
        idempotency_key="scan-race-preview",
    )
    gate.set_scan_active(True)
    assert await supervisor.run_once("worker-2", now=101) is None
    gate.set_scan_active(False)
    claimed = await store.claim_operation_job(
        "worker-2", now=101, lease_seconds=60, kind="library_management"
    )
    assert claimed is not None
    gate.set_scan_active(True)

    deferred = await worker.run_claimed(claimed, "worker-2")

    assert deferred["id"] == second.job_id
    assert deferred["state"] == "queued"
    assert await store.list_library_management_plan_items(second.job_id) == []


@pytest.mark.asyncio
@pytest.mark.parametrize(
    ("scenario", "expected_reason"),
    [
        ("missing", "FILE_UNREADABLE"),
        ("unreadable", "FILE_UNREADABLE"),
        ("read_only", "ROOT_READ_ONLY"),
        ("out_of_root", "OUT_OF_ROOT"),
        ("symlink", "SYMLINK_UNSUPPORTED"),
    ],
)
async def test_preview_blocks_unsafe_sources(
    tmp_path: Path,
    scenario: str,
    expected_reason: str,
) -> None:
    case = tmp_path / scenario
    case.mkdir()
    (
        root,
        source,
        preferences,
        store,
        settings_revision,
        policy_revision,
    ) = _configured(case)
    if scenario == "missing":
        source.unlink()
    elif scenario == "unreadable":
        source.chmod(0o200)
    elif scenario == "read_only":
        root.chmod(0o500)
    elif scenario == "out_of_root":
        with sqlite3.connect(case / "library.db") as connection:
            connection.execute(
                "UPDATE local_tracks SET file_path = ? WHERE id = 'track-1'",
                (str(case / "outside.flac"),),
            )
    elif scenario == "symlink":
        target = case / "outside.flac"
        shutil.copy2(source, target)
        source.unlink()
        source.symlink_to(target)
    planner = _planner(case, store, preferences)
    try:
        handle = await planner.create_preview(
            selection=LibraryManagementSelection(kind="tracks", ids=("track-1",)),
            profile_id=PICARD_ORGANIZER_PROFILE_ID,
            expected_settings_revision=settings_revision,
            expected_policy_revision=policy_revision,
            actor_user_id="admin",
            idempotency_key=None,
        )
        claimed = await store.claim_operation_job(
            "worker-1", now=100, lease_seconds=60, kind="library_management"
        )
        assert claimed is not None
        await planner.run_claimed_preview(claimed, "worker-1")
        plan = await store.list_library_management_plan_items(handle.job_id)
    finally:
        root.chmod(0o700)
        if source.exists() and not source.is_symlink():
            source.chmod(0o600)

    assert len(plan) == 1
    assert plan[0].eligibility == "blocked"
    assert plan[0].reason_code == expected_reason
    capability = json.loads(plan[0].capability_json)
    assert capability == {
        "album_artwork_version": None,
        "audio_format": "flac",
        "catalog_album_artist_name": "Alpha",
        "catalog_album_title": "Management Album",
        "catalog_artist_name": "Alpha",
        "catalog_disc_number": 1,
        "catalog_track_number": 2,
        "catalog_track_title": "Management Track",
    }


@pytest.mark.asyncio
@pytest.mark.parametrize(
    ("error", "expected_reason"),
    [
        (
            PathLimitExceededError(
                "Rendered relative path exceeds the configured path limit.",
                script_name="Naming",
                line=1,
                column=1,
            ),
            PATH_TOO_LONG,
        ),
        (
            ScriptValidationError(
                "A custom tag exceeds its name or value-count bound.",
                script_name="tagging input",
                line=1,
                column=1,
            ),
            SCRIPT_VALIDATION_FAILED,
        ),
    ],
)
async def test_preview_distinguishes_path_limits_from_other_script_failures(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
    error: ScriptValidationError,
    expected_reason: str,
) -> None:
    (
        _root,
        _source,
        preferences,
        store,
        settings_revision,
        policy_revision,
    ) = _configured(tmp_path)
    planner = _planner(tmp_path, store, preferences)

    async def reject_track(**_kwargs) -> LibraryManagementPlanItem:
        raise error

    monkeypatch.setattr(planner, "_plan_track", reject_track)
    handle = await planner.create_preview(
        selection=LibraryManagementSelection(kind="tracks", ids=("track-1",)),
        profile_id=PICARD_ORGANIZER_PROFILE_ID,
        expected_settings_revision=settings_revision,
        expected_policy_revision=policy_revision,
        actor_user_id="admin",
        idempotency_key=None,
    )
    claimed = await store.claim_operation_job(
        "worker-1", now=100, lease_seconds=60, kind="library_management"
    )
    assert claimed is not None

    await planner.run_claimed_preview(claimed, "worker-1")
    item = (await store.list_library_management_plan_items(handle.job_id))[0]

    assert item.eligibility == "blocked"
    assert item.reason_code == expected_reason


@pytest.mark.asyncio
async def test_preview_times_out_one_stuck_source_keeps_its_lease_and_retries_later(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    (
        root,
        _source,
        preferences,
        store,
        settings_revision,
        policy_revision,
    ) = _configured(tmp_path)
    second_source = root / "source-2.flac"
    shutil.copy2(FIXTURES / "library" / "management_full.flac", second_source)
    _add_second_track(
        tmp_path / "library.db",
        second_source,
        release_track_mbid="44444444-4444-4444-8444-444444444444",
        recording_mbid="55555555-5555-4555-8555-555555555555",
    )
    planner = _planner(tmp_path, store, preferences)
    original_inspect = planner._inspect_source
    original_heartbeat = store.heartbeat_operation_job
    release_inspection = threading.Event()
    inspection_started = threading.Event()
    inspection_calls = 0

    def stuck_inspection(subject, roots):  # noqa: ANN001
        nonlocal inspection_calls
        inspection_calls += 1
        inspection_started.set()
        release_inspection.wait(timeout=2)
        return original_inspect(subject, roots)

    heartbeat = AsyncMock(side_effect=original_heartbeat)
    monkeypatch.setattr(planner, "_inspect_source", stuck_inspection)
    monkeypatch.setattr(store, "heartbeat_operation_job", heartbeat)
    monkeypatch.setattr(planner_module, "SOURCE_INSPECTION_TIMEOUT_SECONDS", 0.03)
    monkeypatch.setattr(planner_module, "PLANNING_LEASE_RENEWAL_SECONDS", 0.005)
    handle = await planner.create_preview(
        selection=LibraryManagementSelection(kind="albums", ids=("album-1",)),
        profile_id=PICARD_ORGANIZER_PROFILE_ID,
        expected_settings_revision=settings_revision,
        expected_policy_revision=policy_revision,
        actor_user_id="admin",
        idempotency_key="stuck-source-preview",
    )
    claimed = await store.claim_operation_job(
        "worker-1", now=100, lease_seconds=60, kind="library_management"
    )
    assert claimed is not None

    try:
        snapshot = await planner.run_claimed_preview(claimed, "worker-1")
    finally:
        release_inspection.set()
        await asyncio.sleep(0)

    plan = await store.list_library_management_plan_items(handle.job_id)
    operation = await store.get_operation_job(handle.job_id)
    assert inspection_started.is_set()
    assert heartbeat.await_count >= 2
    assert snapshot.phase == "ready"
    assert operation is not None and operation["state"] == "ready"
    assert len(plan) == 2
    assert all(item.eligibility == "blocked" for item in plan)
    assert all(item.reason_code == "FILE_UNREADABLE" for item in plan)

    repeated = await planner.create_preview(
        selection=LibraryManagementSelection(kind="albums", ids=("album-1",)),
        profile_id=PICARD_ORGANIZER_PROFILE_ID,
        expected_settings_revision=settings_revision,
        expected_policy_revision=policy_revision,
        actor_user_id="admin",
        idempotency_key="stuck-source-preview-repeat",
    )
    repeated_claim = await store.claim_operation_job(
        "worker-1", now=100, lease_seconds=60, kind="library_management"
    )
    assert repeated_claim is not None
    repeated_snapshot = await planner.run_claimed_preview(repeated_claim, "worker-1")
    repeated_plan = await store.list_library_management_plan_items(repeated.job_id)

    assert repeated_snapshot.phase == "ready"
    assert inspection_calls == 3
    assert len(repeated_plan) == 2
    assert all(item.reason_code != "FILE_UNREADABLE" for item in repeated_plan)


@pytest.mark.asyncio
async def test_preview_blocks_destination_collision_without_overwrite(
    tmp_path: Path,
) -> None:
    (
        root,
        _source,
        preferences,
        store,
        settings_revision,
        policy_revision,
    ) = _configured(tmp_path)
    destination = root / (
        "Johann Sebastian Bach; Glenn Gould/"
        "Goldberg Variations, BWV 988 (1982)/01 - Aria.flac"
    )
    destination.parent.mkdir(parents=True)
    destination.write_bytes(b"occupied")
    planner = _planner(tmp_path, store, preferences)
    handle = await planner.create_preview(
        selection=LibraryManagementSelection(kind="tracks", ids=("track-1",)),
        profile_id=PICARD_ORGANIZER_PROFILE_ID,
        expected_settings_revision=settings_revision,
        expected_policy_revision=policy_revision,
        actor_user_id="admin",
        idempotency_key=None,
    )
    claimed = await store.claim_operation_job(
        "worker-1", now=100, lease_seconds=60, kind="library_management"
    )
    assert claimed is not None

    await planner.run_claimed_preview(claimed, "worker-1")
    plan = await store.list_library_management_plan_items(handle.job_id)

    assert plan[0].eligibility == "blocked"
    assert plan[0].reason_code == "PATH_COLLISION_DIFFERENT"
    evidence = json.loads(plan[0].collision_json)
    assert evidence == [
        {
            "classification": "same_path_different_content",
            "existing_relative_path": (
                "Johann Sebastian Bach; Glenn Gould/"
                "Goldberg Variations, BWV 988 (1982)/01 - Aria.flac"
            ),
            "existing_root_id": "root-1",
        }
    ]
    assert destination.read_bytes() == b"occupied"


def test_destination_collision_allows_case_only_rename_of_source(
    tmp_path: Path,
) -> None:
    source = tmp_path / "Back To Me.flac"
    source.write_bytes(b"audio")
    destination = tmp_path / "Back to Me.flac"

    evidence, reason = LibraryManagementPlanner._destination_collisions(
        source,
        hashlib.sha256(b"audio").hexdigest(),
        destination,
        tmp_path,
    )

    assert evidence == []
    assert reason is None


@pytest.mark.asyncio
async def test_preview_marks_provider_deferral_as_warning(
    tmp_path: Path,
) -> None:
    (
        _root,
        _source,
        preferences,
        store,
        _settings_revision,
        policy_revision,
    ) = _configured(tmp_path)
    current = preferences.get_library_management_settings()
    settings = preferences.get_library_management_settings_raw()
    profile = next(
        value for value in settings.profiles if value.id == PICARD_ORGANIZER_PROFILE_ID
    )
    profile.genres.sources = ["musicbrainz", "listenbrainz"]
    saved = preferences.save_library_management_settings_if_current(
        settings, expected_settings_revision=current.settings_revision
    )
    planner = _planner(tmp_path, store, preferences)
    handle = await planner.create_preview(
        selection=LibraryManagementSelection(kind="tracks", ids=("track-1",)),
        profile_id=PICARD_ORGANIZER_PROFILE_ID,
        expected_settings_revision=saved.settings_revision,
        expected_policy_revision=policy_revision,
        actor_user_id="admin",
        idempotency_key=None,
    )
    claimed = await store.claim_operation_job(
        "worker-1", now=100, lease_seconds=60, kind="library_management"
    )
    assert claimed is not None

    await planner.run_claimed_preview(claimed, "worker-1")
    plan = await store.list_library_management_plan_items(handle.job_id)

    assert plan[0].eligibility == "warning"
    assert plan[0].reason_code == "OPTIONAL_ENRICHMENT_DEFERRED"

@pytest.mark.asyncio
async def test_preview_persists_per_source_deferred_detail_with_stable_codes(
    tmp_path: Path,
) -> None:
    (
        _root,
        _source,
        preferences,
        store,
        _settings_revision,
        policy_revision,
    ) = _configured(tmp_path)
    current = preferences.get_library_management_settings()
    settings = preferences.get_library_management_settings_raw()
    profile = next(
        value for value in settings.profiles if value.id == PICARD_ORGANIZER_PROFILE_ID
    )
    profile.genres.sources = ["musicbrainz", "listenbrainz"]
    profile.enrichment.lyrics.enabled = True
    profile.enrichment.replaygain.enabled = True
    profile.enrichment.replaygain.mode = "replace"
    profile.artwork.external_enabled = True
    saved = preferences.save_library_management_settings_if_current(
        settings, expected_settings_revision=current.settings_revision
    )
    lyrics = AsyncMock()
    lyrics.project.return_value = LyricsProjection(
        status="deferred", reason="The lyrics provider is not available."
    )
    artwork_repository = AsyncMock()
    artwork_repository.list_management_artwork.side_effect = ExternalServiceError(
        "artwork provider down"
    )
    planner = _planner(
        tmp_path,
        store,
        preferences,
        artwork_repository=artwork_repository,
        lyrics=lyrics,
    )
    handle = await planner.create_preview(
        selection=LibraryManagementSelection(kind="tracks", ids=("track-1",)),
        profile_id=PICARD_ORGANIZER_PROFILE_ID,
        expected_settings_revision=saved.settings_revision,
        expected_policy_revision=policy_revision,
        actor_user_id="admin",
        idempotency_key=None,
    )
    claimed = await store.claim_operation_job(
        "worker-1", now=100, lease_seconds=60, kind="library_management"
    )
    assert claimed is not None

    await planner.run_claimed_preview(claimed, "worker-1")
    plan = await store.list_library_management_plan_items(handle.job_id)

    assert plan[0].eligibility == "warning"
    assert plan[0].reason_code == "OPTIONAL_ENRICHMENT_DEFERRED"
    diff = json.loads(plan[0].diff_json)
    capability = json.loads(plan[0].capability_json)
    deferred = diff["deferred_sources"]
    assert "genre:listenbrainz" in deferred
    assert any(value.startswith("artwork:") for value in deferred)
    assert "lyrics:deferred" in deferred
    assert "replaygain:deferred" in deferred
    assert capability["blockers"] == []
    mapped = LibraryManagementPreviewService._item(plan[0])
    assert mapped.diff["deferred_sources"] == deferred
    assert mapped.reason_code == "OPTIONAL_ENRICHMENT_DEFERRED"
    snapshot = await store.get_library_management_job_snapshot(handle.job_id)
    assert snapshot is not None
    summary = json.loads(snapshot.summary_json)
    assert summary["reasons"] == {"OPTIONAL_ENRICHMENT_DEFERRED": 1}
    assert summary["deferred_sources"]["genre:listenbrainz"] == 1
    assert summary["deferred_sources"]["lyrics:deferred"] == 1
    assert summary["deferred_sources"]["replaygain:deferred"] == 1
    assert (
        sum(
            1
            for key in summary["deferred_sources"]
            if key.startswith("artwork:")
        )
        >= 1
    )


@pytest.mark.asyncio
async def test_preview_warning_without_deferral_keeps_a_stable_reason_code(
    tmp_path: Path,
) -> None:
    (
        root,
        _source,
        preferences,
        store,
        _settings_revision,
        policy_revision,
    ) = _configured(tmp_path)
    (root / "notes.cue").write_bytes(b"cue")
    current = preferences.get_library_management_settings()
    settings = preferences.get_library_management_settings_raw()
    profile = next(
        value for value in settings.profiles if value.id == PICARD_ORGANIZER_PROFILE_ID
    )
    profile.organization.move_sidecars = True
    saved = preferences.save_library_management_settings_if_current(
        settings, expected_settings_revision=current.settings_revision
    )
    planner = _planner(tmp_path, store, preferences)
    handle = await planner.create_preview(
        selection=LibraryManagementSelection(kind="tracks", ids=("track-1",)),
        profile_id=PICARD_ORGANIZER_PROFILE_ID,
        expected_settings_revision=saved.settings_revision,
        expected_policy_revision=policy_revision,
        actor_user_id="admin",
        idempotency_key=None,
    )
    claimed = await store.claim_operation_job(
        "worker-1", now=100, lease_seconds=60, kind="library_management"
    )
    assert claimed is not None

    await planner.run_claimed_preview(claimed, "worker-1")
    item = (await store.list_library_management_plan_items(handle.job_id))[0]
    capability = json.loads(item.capability_json)

    assert item.eligibility == "warning"
    assert item.reason_code == "FIELD_UNSUPPORTED_BY_FORMAT"
    assert "moved path-bearing sidecars are not rewritten" in capability["warnings"]


@pytest.mark.asyncio
async def test_preview_blocks_when_destination_has_insufficient_space(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    (
        _root,
        _source,
        preferences,
        store,
        settings_revision,
        policy_revision,
    ) = _configured(tmp_path)
    monkeypatch.setattr(
        "services.native.library_management_planner.shutil.disk_usage",
        lambda _path: SimpleNamespace(free=0),
    )
    planner = _planner(tmp_path, store, preferences)
    handle = await planner.create_preview(
        selection=LibraryManagementSelection(kind="tracks", ids=("track-1",)),
        profile_id=PICARD_ORGANIZER_PROFILE_ID,
        expected_settings_revision=settings_revision,
        expected_policy_revision=policy_revision,
        actor_user_id="admin",
        idempotency_key=None,
    )
    claimed = await store.claim_operation_job(
        "worker-1", now=100, lease_seconds=60, kind="library_management"
    )
    assert claimed is not None

    await planner.run_claimed_preview(claimed, "worker-1")
    plan = await store.list_library_management_plan_items(handle.job_id)

    assert plan[0].eligibility == "blocked"
    assert plan[0].reason_code == "INSUFFICIENT_SPACE"


@pytest.mark.asyncio
@pytest.mark.parametrize(
    ("identity_table", "expected_reason"),
    [
        ("local_album_external_identities", "IDENTITY_NOT_ACCEPTED"),
        ("local_track_external_identities", "TRACK_NOT_MAPPED"),
    ],
)
async def test_preview_requires_accepted_release_and_per_file_mapping(
    tmp_path: Path,
    identity_table: str,
    expected_reason: str,
) -> None:
    (
        _root,
        _source,
        preferences,
        store,
        settings_revision,
        policy_revision,
    ) = _configured(tmp_path)
    with sqlite3.connect(tmp_path / "library.db") as connection:
        connection.execute(f"DELETE FROM {identity_table}")
    planner = _planner(tmp_path, store, preferences)
    handle = await planner.create_preview(
        selection=LibraryManagementSelection(kind="tracks", ids=("track-1",)),
        profile_id=PICARD_ORGANIZER_PROFILE_ID,
        expected_settings_revision=settings_revision,
        expected_policy_revision=policy_revision,
        actor_user_id="admin",
        idempotency_key=None,
    )
    claimed = await store.claim_operation_job(
        "worker-1", now=100, lease_seconds=60, kind="library_management"
    )
    assert claimed is not None

    await planner.run_claimed_preview(claimed, "worker-1")
    plan = await store.list_library_management_plan_items(handle.job_id)

    assert plan[0].eligibility == "blocked"
    assert plan[0].reason_code == expected_reason


def test_sidecar_planning_is_album_relative_bounded_and_never_follows_symlinks(
    tmp_path: Path,
) -> None:
    source = tmp_path / "source"
    destination = tmp_path / "destination"
    nested = source / "nested"
    nested.mkdir(parents=True)
    destination.mkdir()
    (source / "disc.cue").write_text("FILE track.flac", encoding="utf-8")
    (nested / "nested.cue").write_text("FILE nested.flac", encoding="utf-8")
    (source / "notes.txt").write_text("keep", encoding="utf-8")
    profile = picard_style_organizer_profile()

    planned, reason = LibraryManagementPlanner._sidecars(
        source, destination, profile, True
    )

    assert reason is None
    assert len(planned) == 1
    assert planned[0]["source_relative_path"] == "disc.cue"
    assert planned[0]["destination_relative_path"] == "disc.cue"
    assert planned[0]["byte_size"] == len("FILE track.flac")
    assert planned[0]["sha256"] == hashlib.sha256(b"FILE track.flac").hexdigest()

    (source / "disc.cue").unlink()
    (source / "disc.cue").symlink_to(source / "notes.txt")
    _planned, reason = LibraryManagementPlanner._sidecars(
        source, destination, profile, True
    )
    assert reason == "SIDECAR_COLLISION"


@pytest.mark.asyncio
async def test_preview_materializes_custom_external_artwork_path_without_writing_it(
    tmp_path: Path,
) -> None:
    (
        root,
        _source,
        preferences,
        store,
        _settings_revision,
        policy_revision,
    ) = _configured(tmp_path)
    current = preferences.get_library_management_settings()
    settings = preferences.get_library_management_settings_raw()
    artwork_script = NamingScriptSettings(
        id="6e0e3245-8e5c-4202-acd8-41230c4ca09f",
        name="External artwork",
        source=("{albumartist}/{album}/art-{artwork_type}.{artwork_extension}"),
    )
    settings.naming_scripts.append(artwork_script)
    profile = next(
        value for value in settings.profiles if value.id == PICARD_ORGANIZER_PROFILE_ID
    )
    profile.artwork.embedded_enabled = False
    profile.artwork.external_enabled = True
    profile.artwork.providers = ["cover_art_archive_release"]
    profile.artwork.external_format = "png"
    profile.artwork.external_naming_script_id = artwork_script.id
    saved = preferences.save_library_management_settings_if_current(
        settings, expected_settings_revision=current.settings_revision
    )
    artwork_repository = _ArtworkRepository()
    planner = _planner(
        tmp_path,
        store,
        preferences,
        artwork_repository=artwork_repository,
    )
    before = sorted(path.relative_to(root).as_posix() for path in root.rglob("*"))
    handle = await planner.create_preview(
        selection=LibraryManagementSelection(kind="tracks", ids=("track-1",)),
        profile_id=PICARD_ORGANIZER_PROFILE_ID,
        expected_settings_revision=saved.settings_revision,
        expected_policy_revision=policy_revision,
        actor_user_id="admin",
        idempotency_key=None,
    )
    claimed = await store.claim_operation_job(
        "worker-1", now=100, lease_seconds=60, kind="library_management"
    )
    assert claimed is not None

    await planner.run_claimed_preview(claimed, "worker-1")
    plan = await store.list_library_management_plan_items(handle.job_id)
    choices = json.loads(plan[0].artwork_choices_json)

    assert len(choices) == 1
    assert choices[0]["output_kind"] == "external"
    assert choices[0]["destination_relative_path"] == (
        "Johann Sebastian Bach; Glenn Gould/"
        "Goldberg Variations, BWV 988/art-front.png"
    )
    assert (
        choices[0]["blob_sha256"]
        == hashlib.sha256(artwork_repository.content).hexdigest()
    )
    assert json.loads(plan[0].diff_json)["artwork_changed"] is True
    assert (
        sorted(path.relative_to(root).as_posix() for path in root.rglob("*")) == before
    )


@pytest.mark.asyncio
async def test_preview_reuses_later_artwork_success_across_the_release_bundle(
    tmp_path: Path,
) -> None:
    (
        root,
        _source,
        preferences,
        store,
        settings_revision,
        policy_revision,
    ) = _configured(tmp_path)
    second_source = root / "source-2.flac"
    shutil.copy2(FIXTURES / "library" / "management_full.flac", second_source)

    release = msgspec.json.decode(
        (FIXTURES / "musicbrainz" / "management_release.json").read_bytes(),
        type=MbManagementRelease,
    )
    first_track = release.media[0].tracks[0]
    second_recording = msgspec.structs.replace(
        first_track.recording,
        id="55555555-5555-4555-8555-555555555555",
        title="Variation One",
    )
    second_track = msgspec.structs.replace(
        first_track,
        id="44444444-4444-4444-8444-444444444444",
        title="Variation One",
        position=2,
        number="2",
        recording=second_recording,
    )
    release.media[0] = msgspec.structs.replace(
        release.media[0],
        track_count=2,
        tracks=[first_track, second_track],
    )
    _add_second_track(
        tmp_path / "library.db",
        second_source,
        release_track_mbid=second_track.id,
        recording_mbid=second_recording.id,
    )

    settings = preferences.get_library_management_settings_raw()
    profile = next(
        value for value in settings.profiles if value.id == PICARD_ORGANIZER_PROFILE_ID
    )
    profile.artwork.embedded_enabled = True
    profile.artwork.external_enabled = True
    profile.artwork.providers = ["cover_art_archive_release"]
    profile.artwork.never_replace_with_smaller = False
    profile.artwork.external_format = "png"
    saved = preferences.save_library_management_settings_if_current(
        settings, expected_settings_revision=settings_revision
    )
    artwork_repository = _InitiallyFailingArtworkRepository()
    planner = _planner(
        tmp_path,
        store,
        preferences,
        artwork_repository=artwork_repository,
        canonical_release=release,
    )
    handle = await planner.create_preview(
        selection=LibraryManagementSelection(kind="albums", ids=("album-1",)),
        profile_id=PICARD_ORGANIZER_PROFILE_ID,
        expected_settings_revision=saved.settings_revision,
        expected_policy_revision=policy_revision,
        actor_user_id="admin",
        idempotency_key=None,
    )
    claimed = await store.claim_operation_job(
        "worker-1", now=100, lease_seconds=60, kind="library_management"
    )
    assert claimed is not None

    await planner.run_claimed_preview(claimed, "worker-1")
    plan = await store.list_library_management_plan_items(handle.job_id)
    by_track = {item.local_track_id: item for item in plan}
    first_choices = json.loads(by_track["track-1"].artwork_choices_json)
    second_choices = json.loads(by_track["track-2"].artwork_choices_json)

    assert artwork_repository.download_calls == 2
    assert len(plan) == 2
    assert {value["output_kind"] for value in first_choices} == {
        "embedded",
        "external",
    }
    assert [value["output_kind"] for value in second_choices] == ["embedded"]
    assert {
        value["source_candidate_id"] for value in (*first_choices, *second_choices)
    } == {"exact-front"}
    assert all(item.reason_code != "OPTIONAL_ENRICHMENT_DEFERRED" for item in plan)


def _braced_canonical_release(album_title: str) -> MbManagementRelease:
    payload = json.loads(
        (FIXTURES / "musicbrainz" / "management_release.json").read_text(
            encoding="utf-8"
        )
    )
    payload["title"] = album_title
    return msgspec.json.decode(json.dumps(payload).encode(), type=MbManagementRelease)


def _external_artwork_settings(preferences) -> str:
    current = preferences.get_library_management_settings()
    settings = preferences.get_library_management_settings_raw()
    profile = next(
        value for value in settings.profiles if value.id == PICARD_ORGANIZER_PROFILE_ID
    )
    profile.artwork.embedded_enabled = False
    profile.artwork.external_enabled = True
    saved = preferences.save_library_management_settings_if_current(
        settings, expected_settings_revision=current.settings_revision
    )
    return saved.settings_revision


@pytest.mark.asyncio
@pytest.mark.parametrize(
    "album_title",
    ["Live {2020}", "Live {Deluxe}"],
)
async def test_preview_keeps_braced_album_directory_in_artwork_fallback(
    tmp_path: Path,
    album_title: str,
) -> None:
    """F-075/F-083: without an external naming script the composed fallback
    path is normalized literally - brace-bearing album directories are neither
    re-rendered as templates nor blocked with SCRIPT_VALIDATION_FAILED."""
    (
        root,
        _source,
        preferences,
        store,
        settings_revision,
        policy_revision,
    ) = _configured(tmp_path)
    saved_settings_revision = _external_artwork_settings(preferences)
    planner = _planner(
        tmp_path,
        store,
        preferences,
        artwork_repository=_ArtworkRepository(),
        canonical_release=_braced_canonical_release(album_title),
    )
    before = sorted(path.relative_to(root).as_posix() for path in root.rglob("*"))
    handle = await planner.create_preview(
        selection=LibraryManagementSelection(kind="tracks", ids=("track-1",)),
        profile_id=PICARD_ORGANIZER_PROFILE_ID,
        expected_settings_revision=saved_settings_revision,
        expected_policy_revision=policy_revision,
        actor_user_id="admin",
        idempotency_key=None,
    )
    claimed = await store.claim_operation_job(
        "worker-1", now=100, lease_seconds=60, kind="library_management"
    )
    assert claimed is not None
    await planner.run_claimed_preview(claimed, "worker-1")

    plan = await store.list_library_management_plan_items(handle.job_id)
    choices = json.loads(plan[0].artwork_choices_json)

    assert plan[0].eligibility == "eligible"
    assert plan[0].reason_code != SCRIPT_VALIDATION_FAILED
    assert len(choices) == 1
    assert choices[0]["output_kind"] == "external"
    destination = str(choices[0]["destination_relative_path"])
    # the standard template appends " (year)" after the album title
    assert destination.startswith(
        f"Johann Sebastian Bach; Glenn Gould/{album_title} (1982)/"
    )
    assert destination.endswith("cover.png")
    # planning never writes: the tree is byte-for-byte what it was before
    after = sorted(path.relative_to(root).as_posix() for path in root.rglob("*"))
    assert after == before


@pytest.mark.asyncio
async def test_preview_replay_after_operation_purge_reports_not_found(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """F-082: a purged operation row must surface not-found, not a
    different-preview-request conflict, when the snapshot row survives."""
    from core.exceptions import ResourceNotFoundError

    (
        root,
        _source,
        preferences,
        store,
        settings_revision,
        policy_revision,
    ) = _configured(tmp_path)
    planner = _planner(tmp_path, store, preferences)
    handle = await planner.create_preview(
        selection=LibraryManagementSelection(kind="tracks", ids=("track-1",)),
        profile_id=PICARD_ORGANIZER_PROFILE_ID,
        expected_settings_revision=settings_revision,
        expected_policy_revision=policy_revision,
        actor_user_id="admin",
        idempotency_key="purged-preview",
    )

    with sqlite3.connect(store.db_path) as connection:
        connection.execute(
            "DELETE FROM library_operation_jobs WHERE id=?", (handle.job_id,)
        )

    # the store's idempotency lookup lives in the operation row it just lost;
    # activation coalescing can still hand back the surviving snapshot's id.
    async def coalesced_purged_replay(*_args, **_kwargs):
        return handle.job_id, False

    monkeypatch.setattr(
        store, "create_library_management_job", coalesced_purged_replay
    )

    with pytest.raises(ResourceNotFoundError, match="no longer exists"):
        await planner.create_preview(
            selection=LibraryManagementSelection(kind="tracks", ids=("track-1",)),
            profile_id=PICARD_ORGANIZER_PROFILE_ID,
            expected_settings_revision=settings_revision,
            expected_policy_revision=policy_revision,
            actor_user_id="admin",
            idempotency_key="purged-preview",
        )


@pytest.mark.asyncio
async def test_sidecar_budget_charges_matched_candidates_not_walked_entries(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """F-085: unrelated files in the source directory must not consume the
    sidecar candidate budget (and the raw walk stays bounded separately)."""
    monkeypatch.setattr(planner_module, "MAX_SIDECAR_ENTRIES", 2)

    source_directory = tmp_path / "source-album"
    source_directory.mkdir(parents=True)
    destination_directory = tmp_path / "destination-album"
    destination_directory.mkdir()
    for index in range(10):
        (source_directory / f"unrelated-{index}.txt").write_bytes(b"noise")
    for index in range(2):
        (source_directory / f"{index:02d}-side.cue").write_text("FILE a.flac WAVE")

    matches, collision_reason = LibraryManagementPlanner._sidecars(
        source_directory,
        destination_directory,
        picard_style_organizer_profile(),
        bundle_first=True,
    )

    assert collision_reason is None
    assert len(matches) == 2


@pytest.mark.asyncio
async def test_sidecar_candidate_budget_still_blocks_at_the_cap(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """F-085: the candidate cap still fires once MATCHED sidecars exceed it."""
    monkeypatch.setattr(planner_module, "MAX_SIDECAR_ENTRIES", 2)

    source_directory = tmp_path / "source-album"
    source_directory.mkdir(parents=True)
    destination_directory = tmp_path / "destination-album"
    destination_directory.mkdir()
    for index in range(10):
        (source_directory / f"unrelated-{index}.txt").write_bytes(b"noise")
    for index in range(3):
        (source_directory / f"{index:02d}-side.cue").write_text("FILE a.flac WAVE")

    matches, collision_reason = LibraryManagementPlanner._sidecars(
        source_directory,
        destination_directory,
        picard_style_organizer_profile(),
        bundle_first=True,
    )

    assert collision_reason == "SIDECAR_COLLISION"
    assert len(matches) == 2


@pytest.mark.asyncio
async def test_sidecar_walk_guard_still_bounds_pathological_directories(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """F-085: the generous raw-walk guard (10x the candidate budget) still
    aborts unbounded directories instead of walking forever."""
    monkeypatch.setattr(planner_module, "MAX_SIDECAR_ENTRIES", 2)

    source_directory = tmp_path / "source-album"
    source_directory.mkdir(parents=True)
    destination_directory = tmp_path / "destination-album"
    destination_directory.mkdir()
    for index in range(40):
        (source_directory / f"unrelated-{index}.txt").write_bytes(b"noise")

    matches, collision_reason = LibraryManagementPlanner._sidecars(
        source_directory,
        destination_directory,
        picard_style_organizer_profile(),
        bundle_first=True,
    )

    assert collision_reason == "SIDECAR_COLLISION"
    assert matches == []


def _inspection_subject(track_id: str, relative_path: str):
    from models.library_management_planning import LibraryManagementSelectionSubject

    return LibraryManagementSelectionSubject(
        ordinal=0,
        bundle_ordinal=0,
        bundle_first=True,
        local_album_id="album-1",
        local_track_id=track_id,
        album_revision=1,
        track_revision=1,
        root_id="root-1",
        relative_path=relative_path,
        file_path=f"/music/{relative_path}",
        file_size_bytes=1,
        file_mtime_ns=1,
        stat_revision="1:1",
        tag_revision="tag-1",
        availability="indexed",
        applied_policy="automatic",
        file_format="flac",
        disc_number=1,
        track_number=1,
        track_title="Track",
        artist_name="Artist",
        album_title="Album",
        album_artist_name="Artist",
        year=2024,
        album_artwork_version=None,
    )


@pytest.mark.asyncio
async def test_inspection_saturation_fails_fast_without_charging_root_budget(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
    caplog: pytest.LogCaptureFixture,
) -> None:
    """F-076/F-084: a saturated shared pool returns FILE_UNREADABLE promptly
    and must NOT consume the per-root timeout budget (the root is not faulty)."""
    import concurrent.futures

    planner = _planner(tmp_path, AsyncMock(), AsyncMock())
    monkeypatch.setattr(planner_module, "_source_inspection_inflight", {})
    monkeypatch.setattr(planner_module, "_source_inspection_cooldown", {})
    single = concurrent.futures.ThreadPoolExecutor(max_workers=1)
    monkeypatch.setattr(planner_module, "_SOURCE_INSPECTION_EXECUTOR", single)
    monkeypatch.setattr(planner_module, "_INSPECTION_POOL_WORKERS", 1)

    blocked = threading.Event()
    started = threading.Event()

    def hang(_subject, _roots):
        started.set()
        blocked.wait(timeout=5)

    monkeypatch.setattr(planner, "_inspect_source", hang)
    timed_out_sources: set[tuple[str, str, int, int]] = set()
    budget_by_root: dict[str, int] = {}
    subject_a = _inspection_subject("track-a", "a.flac")
    subject_b = _inspection_subject("track-b", "b.flac")

    first = asyncio.create_task(
        planner._inspect_source_bounded(
            subject_a,
            {},
            timed_out_sources=timed_out_sources,
            timed_out_inspections_by_root=budget_by_root,
        )
    )
    for _ in range(200):
        if planner_module._source_inspection_inflight:
            break
        await asyncio.sleep(0.01)
    assert started.is_set()

    second = await planner._inspect_source_bounded(
        subject_b,
        {},
        timed_out_sources=timed_out_sources,
        timed_out_inspections_by_root=budget_by_root,
    )

    assert second.document is None and second.reason_code == FILE_UNREADABLE
    # saturation is not a root fault: the budget stays untouched
    assert budget_by_root.get("root-1", 0) == 0
    assert "saturated" in caplog.text
    with caplog.at_level(logging.WARNING):
        pass

    blocked.set()
    await asyncio.wait_for(first, timeout=5)
    single.shutdown(wait=False)


@pytest.mark.asyncio
async def test_inspection_cooldown_recover_after_abandoned_read_completes(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """F-076/F-084: once an abandoned read finally completes, its cooldown
    clears and the same source plans again without a process restart."""
    planner = _planner(tmp_path, AsyncMock(), AsyncMock())
    monkeypatch.setattr(planner_module, "_source_inspection_inflight", {})
    monkeypatch.setattr(planner_module, "_source_inspection_cooldown", {})
    monkeypatch.setattr(planner_module, "SOURCE_INSPECTION_TIMEOUT_SECONDS", 0.05)

    release = threading.Event()
    real_inspect = planner._inspect_source
    inspection_calls = 0

    def slow_then_healthy(subject, roots):
        nonlocal inspection_calls
        inspection_calls += 1
        release.wait(timeout=5)
        return real_inspect(subject, roots)

    monkeypatch.setattr(planner, "_inspect_source", slow_then_healthy)
    timed_out_sources: set[tuple[str, str, int, int]] = set()
    budget_by_root: dict[str, int] = {}
    subject = _inspection_subject("track-a", "a.flac")
    roots = {"root-1": SimpleNamespace(path=str(tmp_path))}

    timed_out = await planner._inspect_source_bounded(
        subject,
        roots,
        timed_out_sources=timed_out_sources,
        timed_out_inspections_by_root=budget_by_root,
    )
    assert timed_out.reason_code == FILE_UNREADABLE
    assert budget_by_root["root-1"] == 1

    # the abandoned read eventually completes -> cooldown cleared
    release.set()
    for _ in range(200):
        if (
            planner_module._source_inspection_cooldown.get(
                planner._source_inspection_key(subject)
            )
            is None
        ):
            break
        await asyncio.sleep(0.01)

    # a later preview job owns a fresh per-job timeout set; the cooldown from
    # the abandoned read must not leak into it once the read completed
    recovered = await planner._inspect_source_bounded(
        subject,
        roots,
        timed_out_sources=set(),
        timed_out_inspections_by_root=budget_by_root,
    )

    # the retry actually re-submitted to the pool: the synthetic subject's
    # recorded file_path lives outside the temp root, so the healthy lane's
    # outcome is OUT_OF_ROOT rather than another cooldown short-circuit
    assert recovered.reason_code == "OUT_OF_ROOT"
    assert inspection_calls == 2
    assert budget_by_root["root-1"] == 1


def test_pinned_profile_holds_private_deep_copies(tmp_path: Path) -> None:
    """F-078: mutating the live settings structs after pinning must never leak
    into an already-sealed PinnedLibraryManagementProfile."""
    from models.library_management_planning import pin_library_management_profile

    _root, _source, preferences, _store, _settings_revision, _policy_revision = (
        _configured(tmp_path)
    )
    settings = preferences.get_library_management_settings_raw()
    profile = next(
        value for value in settings.profiles if value.id == PICARD_ORGANIZER_PROFILE_ID
    )
    original_cleanup = profile.organization.source_cleanup
    standard_script = next(
        value
        for value in settings.naming_scripts
        if value.id == profile.organization.naming_script_id
    )
    original_standard_source = standard_script.source

    pinned = pin_library_management_profile(settings, profile)

    profile.name = "tampered-name"
    profile.organization.source_cleanup = "keep"
    standard_script.source = "{title}.flac"

    assert pinned.profile.name != "tampered-name"
    assert pinned.profile.organization.source_cleanup == original_cleanup
    assert pinned.naming_script.source == original_standard_source
