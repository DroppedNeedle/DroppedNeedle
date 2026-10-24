"""Import bundles and plan items keep artwork bytes in the blob store (#517)."""

import base64
import hashlib
import json
from pathlib import Path
import shutil
import sqlite3
from types import SimpleNamespace
from unittest.mock import AsyncMock
import uuid

import msgspec
import pytest

from api.v1.schemas.library_management_preview import LibraryManagementApplyRequest
from api.v1.schemas.library_management import (
    PICARD_ORGANIZER_PROFILE_ID,
    settings_revision,
)
from core.exceptions import (
    AutomaticManagementHoldError,
    StaleRevisionError,
    ValidationError,
)
from infrastructure.audio.metadata_engine import AudioMetadataEngine
from infrastructure.library_management_blob_store import LibraryManagementBlobStore
from models.audio_metadata import (
    DesiredAudioDocument,
    DesiredAudioField,
    EmbeddedArtworkDescriptor,
)
from models.library_management import (
    LibraryManagementImportArtifact,
    LibraryManagementImportBundle,
    LibraryManagementImportBundleRecord,
    LibraryManagementMetadataSnapshot,
    LibraryManagementPlanItem,
)
from models.library_management_planning import naming_policy_revision
from models.library_management_stored_artwork import (
    persisted_import_bundle,
    persisted_import_file,
)
from services.native.acquisition_cleanup_service import AcquisitionCleanupService
from services.native.audio_write_planning_service import AudioWritePlanningService
from services.native.library_filesystem_coordinator import LibraryFilesystemCoordinator
from services.native.edition_conversion_service import EditionConversionService
from services.native.library_management_preview_service import (
    LibraryManagementPreviewService,
)
from services.native.library_management_publisher import (
    _IMPORT_BUNDLE_NAMESPACE,
    LibraryManagementPublisher,
)
from services.native.library_policy_resolver import LibraryPolicyResolver
from services.native.target_import_library_service import TargetImportLibraryService
from tests.services.native.test_library_management_planner import _configured, _planner
from tests.services.native.test_library_management_publisher import (
    _activate_automatic_acquisitions,
    _import_file,
    _png,
)

COVER = _png(b"\x10\x80\xf0")
EXTERNAL = _png(b"\xf0\x10\x20")


def _descriptor(content: bytes) -> EmbeddedArtworkDescriptor:
    return EmbeddedArtworkDescriptor(
        image_type="front",
        mime_type="image/png",
        description="Front",
        width=4,
        height=4,
        byte_size=len(content),
        sha256=hashlib.sha256(content).hexdigest(),
        content=content,
        format_supported=True,
    )


async def _scenario(tmp_path: Path):
    root, catalog_source, preferences, store, _settings, policy_revision = _configured(
        tmp_path
    )
    _activate_automatic_acquisitions(preferences, policy_revision)
    audio = AudioMetadataEngine()
    filesystem = LibraryFilesystemCoordinator()
    blobs = LibraryManagementBlobStore(tmp_path / "stored-artwork-blobs", store)
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
    incoming = tmp_path / "incoming.flac"
    shutil.copy2(catalog_source, incoming)
    management = preferences.get_library_management_settings_raw()
    profile = next(
        value
        for value in management.profiles
        if value.id == PICARD_ORGANIZER_PROFILE_ID
    )
    pinned = _planner(tmp_path, store, preferences).pin_profile(management, profile)
    snapshot = await store.put_management_metadata_snapshot(
        LibraryManagementMetadataSnapshot(
            id="stored-artwork-snapshot",
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
        _import_file(audio, incoming, ordinal=0, relative_path="Artist/Album/01.flac"),
        authoritative_mapping=True,
        recording_mbid="recording-1",
        release_track_mbid="release-track-1",
        medium_position=1,
        release_track_position=1,
        baseline_relative_path="Incoming/01 Original.flac",
        desired_document=DesiredAudioDocument(
            fields=(DesiredAudioField(name="title", action="set", value="Managed"),),
            artwork=(_descriptor(COVER),),
        ),
        pinned_profile=pinned,
        metadata_snapshot_id=snapshot.id,
        projection_hash="c" * 64,
        settings_revision=settings_revision(management),
        naming_policy_revision=naming_policy_revision(pinned),
        undo_retention_days=management.undo_retention_days,
        artifacts=(
            LibraryManagementImportArtifact(
                kind="external_art",
                destination_root_id="root-1",
                destination_relative_path="Artist/Album/cover.png",
                content=EXTERNAL,
                source_fingerprint=hashlib.sha256(EXTERNAL).hexdigest(),
            ),
        ),
    )
    bundle = LibraryManagementImportBundle(
        idempotency_key="acquisition:stored-artwork",
        origin="acquisition",
        policy_revision=policy_revision,
        files=(request,),
    )
    return SimpleNamespace(
        root=root,
        store=store,
        audio=audio,
        blobs=blobs,
        publisher=publisher,
        service=service,
        bundle=bundle,
        policy_revision=policy_revision,
        preferences=preferences,
    )


def _bundle_row(store, idempotency_key: str):
    with sqlite3.connect(store.db_path) as connection:
        connection.row_factory = sqlite3.Row
        return dict(
            connection.execute(
                "SELECT * FROM library_management_import_bundles "
                "WHERE idempotency_key=?",
                (idempotency_key,),
            ).fetchone()
        )


def _plan_item_row(store) -> dict:
    with sqlite3.connect(store.db_path) as connection:
        connection.row_factory = sqlite3.Row
        return dict(
            connection.execute("SELECT * FROM library_management_plan_items").fetchone()
        )


def _contains_artwork(text: str) -> bool:
    return any(
        value in text
        for payload in (COVER, EXTERNAL)
        for value in (base64.b64encode(payload).decode(), payload.hex())
    )


@pytest.mark.asyncio
async def test_persisted_form_is_deterministic_and_keeps_none_and_empty_artwork(
    tmp_path: Path,
) -> None:
    scenario = await _scenario(tmp_path)
    file = scenario.bundle.files[0]

    stored = persisted_import_file(file)

    assert persisted_import_file(stored) == stored
    assert stored.desired_document.artwork is None
    assert stored.desired_artwork[0].sha256 == hashlib.sha256(COVER).hexdigest()
    assert msgspec.json.encode(persisted_import_bundle(scenario.bundle)) == (
        msgspec.json.encode(persisted_import_bundle(scenario.bundle))
    )
    for artwork in (None, ()):
        plain = msgspec.structs.replace(
            file,
            desired_document=msgspec.structs.replace(
                file.desired_document, artwork=artwork
            ),
        )
        assert persisted_import_file(plain).desired_document.artwork == artwork
        assert persisted_import_file(plain).desired_artwork is None


@pytest.mark.asyncio
async def test_new_bundles_and_plan_items_hold_no_artwork_bytes(tmp_path: Path) -> None:
    scenario = await _scenario(tmp_path)

    result = await scenario.service.publish_import_bundle(scenario.bundle)

    bundle_row = _bundle_row(scenario.store, scenario.bundle.idempotency_key)
    assert not _contains_artwork(bundle_row["request_json"])
    assert (
        hashlib.sha256(bundle_row["request_json"].encode()).hexdigest()
        == bundle_row["request_hash"]
    )
    stored = msgspec.json.decode(
        bundle_row["request_json"].encode(), type=LibraryManagementImportBundle
    )
    assert stored.files[0].desired_document.artwork is None
    assert stored.files[0].desired_artwork[0].sha256 == hashlib.sha256(COVER).hexdigest()
    assert stored.files[0].artifacts[0].content is None
    assert stored.files[0].artifacts[0].content_blob_sha256 == (
        hashlib.sha256(EXTERNAL).hexdigest()
    )
    item = _plan_item_row(scenario.store)
    assert not _contains_artwork(item["desired_document_json"])
    assert not _contains_artwork(item["artwork_choices_json"])
    assert item["catalog_document_json"] is None
    assert item["catalog_document_hash"] is None
    choices = json.loads(item["artwork_choices_json"])
    embedded = [value for value in choices if value["output_kind"] == "embedded"]
    assert [value["blob_sha256"] for value in embedded] == [
        hashlib.sha256(COVER).hexdigest()
    ]
    assert await scenario.blobs.read_bytes(embedded[0]["blob_sha256"]) == COVER
    assert await scenario.blobs.read_bytes(hashlib.sha256(EXTERNAL).hexdigest()) == (
        EXTERNAL
    )
    with sqlite3.connect(scenario.store.db_path) as connection:
        references = {
            (row[0], row[1])
            for row in connection.execute(
                "SELECT blob_sha256,reference_id FROM library_management_blob_references "
                "WHERE reference_kind='artwork'"
            )
        }
    bundle_id = bundle_row["id"]
    assert (hashlib.sha256(COVER).hexdigest(), f"import-bundle:{bundle_id}") in references
    assert (
        hashlib.sha256(EXTERNAL).hexdigest(),
        f"import-bundle:{bundle_id}",
    ) in references
    assert (
        hashlib.sha256(COVER).hexdigest(),
        f"{item['job_id']}:{item['ordinal']}:0",
    ) in references

    published = scenario.root / "Artist/Album/01.flac"
    assert scenario.audio.read(published).artwork[0].content == COVER
    assert (scenario.root / "Artist/Album/cover.png").read_bytes() == EXTERNAL
    assert len(result.local_track_ids) == 1


@pytest.mark.asyncio
async def test_retry_recovery_and_cleanup_agree_with_the_stored_request(
    tmp_path: Path,
) -> None:
    scenario = await _scenario(tmp_path)
    await scenario.service.publish_import_bundle(scenario.bundle)
    first = _bundle_row(scenario.store, scenario.bundle.idempotency_key)

    # A separately built bundle with equal content hashes to the same request.
    rebuilt = msgspec.json.decode(
        msgspec.json.encode(scenario.bundle), type=LibraryManagementImportBundle
    )
    assert (
        await scenario.publisher.encode_import_bundle(rebuilt) == first["request_json"]
    )
    repeated = await scenario.service.publish_import_bundle(rebuilt)
    assert repeated.repeated is True
    assert _bundle_row(scenario.store, scenario.bundle.idempotency_key)[
        "request_hash"
    ] == first["request_hash"]

    record = await scenario.store.get_library_management_import_bundle(first["id"])
    assert record is not None
    assert await scenario.publisher.recover_import_bundle(record) == "skipped"
    still = await scenario.store.get_library_management_import_bundle(first["id"])
    assert still is not None and still.state == "completed"

    cleanup = AcquisitionCleanupService(
        scenario.store, scenario.store, lambda _source: None, lambda: tmp_path
    )
    fingerprints = await cleanup._publisher_source_fingerprints(
        SimpleNamespace(source="soulseek", publisher_bundle_ids=[first["id"]])
    )
    assert list(fingerprints.values()) == [
        (await scenario.store.list_library_management_import_journals(first["id"]))[
            0
        ].source_fingerprint
    ]


@pytest.mark.asyncio
async def test_a_different_artwork_payload_is_not_an_idempotent_retry(
    tmp_path: Path,
) -> None:
    scenario = await _scenario(tmp_path)
    await scenario.service.publish_import_bundle(scenario.bundle)
    other = _png(b"\x01\x02\x03")
    file = scenario.bundle.files[0]
    changed = msgspec.structs.replace(
        scenario.bundle,
        files=(
            msgspec.structs.replace(
                file,
                desired_document=msgspec.structs.replace(
                    file.desired_document, artwork=(_descriptor(other),)
                ),
            ),
        ),
    )

    with pytest.raises(AutomaticManagementHoldError):
        await scenario.service.publish_import_bundle(changed)


@pytest.mark.asyncio
async def test_a_stored_bundle_hydrates_from_blobs_and_fails_when_one_is_missing(
    tmp_path: Path,
) -> None:
    scenario = await _scenario(tmp_path)
    stored = msgspec.json.decode(
        (await scenario.publisher.encode_import_bundle(scenario.bundle)).encode(),
        type=LibraryManagementImportBundle,
    )

    hydrated = await scenario.publisher._hydrated_import_bundle(stored)

    assert hydrated.files[0].desired_document.artwork == (_descriptor(COVER),)
    assert hydrated.files[0].desired_artwork is None
    assert hydrated.files[0].artifacts[0].content == EXTERNAL
    assert hydrated.files[0].artifacts[0].content_blob_sha256 is None
    assert hydrated == scenario.bundle
    with sqlite3.connect(scenario.store.db_path) as connection:
        connection.execute(
            "DELETE FROM library_management_blob_references WHERE blob_sha256=?",
            (hashlib.sha256(COVER).hexdigest(),),
        )
        connection.execute(
            "DELETE FROM library_management_blobs WHERE sha256=?",
            (hashlib.sha256(COVER).hexdigest(),),
        )

    with pytest.raises(ValidationError, match="blob not found"):
        await scenario.publisher._hydrated_import_bundle(stored)


@pytest.mark.asyncio
async def test_a_bundle_saved_with_inline_artwork_still_retries_and_recovers(
    tmp_path: Path,
) -> None:
    scenario = await _scenario(tmp_path)
    legacy_json = msgspec.json.encode(scenario.bundle).decode()
    assert _contains_artwork(legacy_json)
    bundle_id = str(
        uuid.uuid5(_IMPORT_BUNDLE_NAMESPACE, scenario.bundle.idempotency_key)
    )
    saved, created = await scenario.store.ensure_library_management_import_bundle(
        LibraryManagementImportBundleRecord(
            id=bundle_id,
            idempotency_key=scenario.bundle.idempotency_key,
            origin="acquisition",
            policy_revision=scenario.policy_revision,
            request_json=legacy_json,
            request_hash=hashlib.sha256(legacy_json.encode()).hexdigest(),
            state="preparing",
            created_at=100.0,
            updated_at=100.0,
        )
    )
    assert created is True

    result = await scenario.service.publish_import_bundle(scenario.bundle)

    row = _bundle_row(scenario.store, scenario.bundle.idempotency_key)
    assert row["request_json"] == legacy_json
    assert row["request_hash"] == saved.request_hash
    assert row["state"] == "completed"
    record = await scenario.store.get_library_management_import_bundle(bundle_id)
    assert record is not None
    assert await scenario.publisher.recover_import_bundle(record) == "skipped"
    assert scenario.audio.read(scenario.root / "Artist/Album/01.flac").artwork[
        0
    ].content == COVER


@pytest.mark.asyncio
async def test_plan_item_artwork_hydrates_like_a_preview_item(tmp_path: Path) -> None:
    scenario = await _scenario(tmp_path)
    await scenario.service.publish_import_bundle(scenario.bundle)
    row = _plan_item_row(scenario.store)
    item = SimpleNamespace(
        desired_document_json=row["desired_document_json"],
        artwork_choices_json=row["artwork_choices_json"],
    )

    desired = await scenario.publisher._desired_document(item, ())

    assert [value.content for value in desired.artwork] == [COVER]
    assert desired.artwork[0].format_supported is True
    # The copy path (duplicate resolution) publishes the same stored columns.
    assert (
        await scenario.publisher._desired_document(item, (_descriptor(EXTERNAL),))
    ).artwork[0].content == COVER


@pytest.mark.asyncio
async def test_baseline_copy_uses_the_desired_document_when_catalog_is_null(
    tmp_path: Path,
) -> None:
    scenario = await _scenario(tmp_path)
    await scenario.service.publish_import_bundle(scenario.bundle)
    row = _plan_item_row(scenario.store)
    item = LibraryManagementPlanItem(
        **{
            name: row[name]
            for name in (
                "job_id",
                "ordinal",
                "bundle_ordinal",
                "expected_catalog_revision",
                "expected_policy_revision",
                "expected_profile_revision",
                "expected_root_id",
                "expected_relative_path",
                "expected_stat_revision",
                "expected_tag_revision",
                "expected_file_fingerprint",
                "source_path_identity",
                "desired_document_json",
                "desired_document_hash",
                "eligibility",
                "created_at",
            )
        }
    )

    catalog = LibraryManagementPublisher._catalog_document(item)

    assert catalog == msgspec.json.decode(
        row["desired_document_json"].encode(), type=DesiredAudioDocument
    )


@pytest.mark.asyncio
async def test_preview_detail_drops_inline_artwork_bytes_from_legacy_rows() -> None:
    desired = DesiredAudioDocument(fields=(), artwork=(_descriptor(COVER),))
    row = LibraryManagementPlanItem(
        job_id="job",
        ordinal=0,
        bundle_ordinal=0,
        expected_catalog_revision=1,
        expected_policy_revision="p",
        expected_profile_revision="r",
        expected_root_id="root-1",
        expected_relative_path="a.flac",
        expected_stat_revision="s",
        expected_tag_revision="t",
        expected_file_fingerprint="f",
        source_path_identity="i",
        desired_document_json=msgspec.json.encode(desired).decode(),
        desired_document_hash="0" * 64,
        eligibility="eligible",
        created_at=1.0,
    )

    response = LibraryManagementPreviewService._item(row)

    assert not _contains_artwork(json.dumps(response.desired_document))
    assert response.desired_document["artwork"][0]["sha256"] == (
        hashlib.sha256(COVER).hexdigest()
    )


@pytest.mark.asyncio
async def test_a_conversion_sealed_with_inline_artwork_is_sent_back_for_recheck(
    tmp_path: Path,
) -> None:
    scenario = await _scenario(tmp_path)
    legacy_json = msgspec.json.encode(scenario.bundle).decode()
    job = SimpleNamespace(
        id="conversion-1",
        row_revision=3,
        state="ready",
        final_bundle_json=legacy_json,
        final_bundle_hash=hashlib.sha256(legacy_json.encode()).hexdigest(),
        final_preview_token_hash=hashlib.sha256(b"token").hexdigest(),
    )
    store = AsyncMock()
    store.get_edition_conversion_for_preview.return_value = job
    import_library = AsyncMock()
    import_library.encode_import_bundle = scenario.publisher.encode_import_bundle
    service = EditionConversionService(
        store=store,
        album_service=AsyncMock(),
        preferences=AsyncMock(),
        acquisition=AsyncMock(),
        download_store=AsyncMock(),
        get_download_service=lambda: AsyncMock(),
        get_free_music_service=lambda: AsyncMock(),
        automatic_management=AsyncMock(),
        fingerprinter=AsyncMock(),
        held_dir=tmp_path / "held",
        import_library=import_library,
        clock=lambda: 100.0,
    )

    with pytest.raises(StaleRevisionError, match="predates this version"):
        await service.apply_preview(
            "preview-1",
            LibraryManagementApplyRequest(
                confirmation=True,
                idempotency_key="apply",
                preview_token="token",
                expected_operation_row_revision=1,
            ),
        )

    assert store.set_edition_conversion_state.await_args.kwargs["state"] == (
        "needs_recheck"
    )
    import_library.publish_import_bundle.assert_not_awaited()
