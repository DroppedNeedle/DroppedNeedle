"""Detached manual profiles must survive the real preview/publisher revision fence."""

from pathlib import Path

import msgspec
import pytest

from api.v1.schemas.library_management import (
    LibraryManagementRootAssignment,
    LibraryManagementRootOverrides,
    profile_revision,
)
from core.exceptions import LibraryManagementPolicyChangedError, StaleRevisionError
from models.library_management import (
    LibraryManagementTagEditFieldIntent,
    LibraryManagementTagEditIntent,
)
from models.library_management_planning import (
    LibraryManagementSelection,
    pin_library_management_profile,
)
from services.native.library_filesystem_coordinator import LibraryFilesystemCoordinator
from services.native.library_management_profile_service import LibraryManagementProfileService
from services.native.library_management_publisher import LibraryManagementPublisher
from tests.services.native.test_library_management_planner import _configured, _planner


async def _preview(tmp_path: Path, *, mode="write_once", fields=("title",), overrides=False):
    root, source, prefs, store, _, policy_revision = _configured(tmp_path)
    if overrides:
        current = prefs.get_library_management_settings()
        settings = prefs.get_library_management_settings_raw()
        settings.root_assignments = [
            LibraryManagementRootAssignment(
                root_id="root-1",
                profile_id=settings.default_profile_id,
                overrides=LibraryManagementRootOverrides(
                    metadata_enabled=False,
                    genres_enabled=False,
                    rename_enabled=True,
                    move_enabled=True,
                ),
            )
        ]
        prefs.save_library_management_settings_if_current(
            settings, expected_settings_revision=current.settings_revision
        )
    stored_before = msgspec.json.encode(prefs.get_library_management_settings_raw())
    config_before = prefs._settings.config_file_path.read_bytes()
    settings, effective = LibraryManagementProfileService(prefs).prepare_tag_editor_profile(
        root_id="root-1", field_names=fields, reset_canonical=mode == "reset_canonical"
    )
    planner = _planner(tmp_path, store, prefs)
    handle = await planner.create_preview(
        selection=LibraryManagementSelection(kind="tracks", ids=("track-1",)),
        profile_id=effective.id,
        expected_settings_revision=prefs.get_library_management_settings().settings_revision,
        expected_policy_revision=policy_revision,
        actor_user_id="admin",
        idempotency_key="detached-tag-editor",
        effective_profile=effective,
        tag_edit_intent=LibraryManagementTagEditIntent(
            local_track_id="track-1",
            local_album_id="album-1",
            mode=mode,
            fields=[
                LibraryManagementTagEditFieldIntent(
                    field_name=name,
                    subject_kind="track" if name == "title" else "album",
                    value=None if mode == "reset_canonical" else "Edited",
                )
                for name in fields
            ],
        ),
    )
    snapshot = await store.get_library_management_job_snapshot(handle.job_id)
    assert snapshot is not None
    publisher = LibraryManagementPublisher(
        store,
        prefs,
        planner._audio,
        planner._write_planner,
        planner._blobs,
        LibraryFilesystemCoordinator(),
    )
    assert msgspec.json.encode(prefs.get_library_management_settings_raw()) == stored_before
    assert prefs._settings.config_file_path.read_bytes() == config_before
    assert msgspec.json.encode(settings) == stored_before
    return prefs, settings, effective, snapshot, publisher, root, source


@pytest.mark.asyncio
@pytest.mark.parametrize("overrides", [False, True])
@pytest.mark.parametrize(
    "mode,fields",
    [
        ("write_once", ("title",)),
        ("write_once", ("genre",)),
        ("reset_canonical", ("genre",)),
    ],
)
async def test_detached_tag_profile_survives_real_snapshot_and_publisher_guards(
    tmp_path: Path, mode, fields, overrides
):
    prefs, settings, effective, snapshot, publisher, root, source = await _preview(
        tmp_path, mode=mode, fields=fields, overrides=overrides
    )
    before = source.read_bytes()
    # This is the same immutable-config validator used by publication, plus its
    # real policy fence. No publisher condition or revision is replaced in tests.
    pinned, roots = publisher.recovery_configuration(snapshot)
    assert roots["root-1"] == root
    assert effective.revision == snapshot.profile_revision == profile_revision(effective)
    assert pinned.profile.revision == snapshot.profile_revision
    assert pinned.profile.metadata.enabled is True
    assert [value.field for value in pinned.profile.metadata.fields] == list(fields)
    assert not pinned.profile.organization.rename_enabled
    assert not pinned.profile.organization.move_enabled
    assert not pinned.profile.artwork.embedded_enabled
    assert not pinned.profile.artwork.external_enabled
    if fields == ("genre",):
        assert pinned.profile.genres.enabled is True
        if mode == "write_once":
            assert pinned.profile.genres.sources == ["existing_local"]
            assert pinned.profile.genres.canonicalize is False
        else:
            assert pinned.profile.genres.sources == ["musicbrainz"]
    else:
        assert pinned.profile.genres.enabled is False
    private_pin = pin_library_management_profile(settings, effective)
    assert private_pin.profile is not effective
    assert all(private_pin.naming_script is not value for value in settings.naming_scripts)
    private_pin_before = msgspec.json.encode(private_pin)
    pin_before = msgspec.json.encode(pinned)
    stored_before = msgspec.json.encode(prefs.get_library_management_settings_raw())
    effective.metadata.fields.clear()
    settings.naming_scripts[0].source = "private-copy mutation"
    assert msgspec.json.encode(private_pin) == private_pin_before
    assert msgspec.json.encode(pinned) == pin_before
    assert msgspec.json.encode(prefs.get_library_management_settings_raw()) == stored_before
    assert source.read_bytes() == before


@pytest.mark.asyncio
async def test_real_management_settings_change_still_rejects_tag_profile(tmp_path: Path):
    prefs, _, _, snapshot, publisher, _, _ = await _preview(tmp_path)
    current = prefs.get_library_management_settings()
    changed = prefs.get_library_management_settings_raw()
    changed.undo_retention_days += 1
    prefs.save_library_management_settings_if_current(
        changed, expected_settings_revision=current.settings_revision
    )
    with pytest.raises(StaleRevisionError, match="settings changed"):
        publisher.recovery_configuration(snapshot)


@pytest.mark.asyncio
async def test_real_naming_change_still_rejects_the_private_pin(tmp_path: Path):
    prefs, _, effective, snapshot, publisher, _, _ = await _preview(tmp_path)
    current = prefs.get_library_management_settings()
    changed = prefs.get_library_management_settings_raw()
    script = next(
        value
        for value in changed.naming_scripts
        if value.id == effective.organization.naming_script_id
    )
    script.source += " revised"
    saved = prefs.save_library_management_settings_if_current(
        changed, expected_settings_revision=current.settings_revision
    )
    with pytest.raises(StaleRevisionError, match="settings changed"):
        publisher.recovery_configuration(snapshot)
    # Isolate the independent naming fence after proving the outer settings
    # fence: only this test copy acknowledges the newer settings revision.
    isolated = msgspec.structs.replace(snapshot, settings_revision=saved.settings_revision)
    with pytest.raises(StaleRevisionError, match="management profile changed"):
        publisher.recovery_configuration(isolated)


@pytest.mark.asyncio
async def test_real_library_policy_change_still_rejects_tag_profile(tmp_path: Path):
    prefs, _, _, snapshot, publisher, _, _ = await _preview(tmp_path)
    policy = prefs.get_typed_library_settings_raw()
    policy.library_roots[0].policy = "local_metadata"
    prefs.save_typed_library_settings(policy)
    with pytest.raises(LibraryManagementPolicyChangedError, match="policy changed"):
        publisher.recovery_configuration(snapshot)


@pytest.mark.asyncio
async def test_changed_snapshot_profile_revision_is_still_rejected(tmp_path: Path):
    _, _, _, snapshot, publisher, _, _ = await _preview(tmp_path)
    changed = msgspec.structs.replace(snapshot, profile_revision="unreviewed")
    with pytest.raises(StaleRevisionError, match="management profile changed"):
        publisher.recovery_configuration(changed)
