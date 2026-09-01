import asyncio
import json
from pathlib import Path
from unittest.mock import AsyncMock, MagicMock

import httpx
import pytest
from api.v1.schemas.settings import (
    BrainzMashActiveBinding,
    BRAINZMASH_ENDPOINT,
    BRAINZMASH_DISCLOSURE_VERSION,
    MusicBrainzBindingRequest,
    MusicBrainzConnectionSettings,
    is_brainzmash_active_binding_valid,
    MusicBrainzSettingsUpdate,
)
from core.config import Settings
from core.exceptions import ConfigurationError, ValidationError
import repositories.musicbrainz_base as mb_base
from repositories.musicbrainz_repository import MusicBrainzRepository
from services.preferences_service import PreferencesService
from services.settings_service import SettingsService


def _preferences(tmp_path: Path) -> PreferencesService:
    return PreferencesService(
        Settings(
            root_app_dir=tmp_path,
            config_file_path=tmp_path / "config.json",
        )
    )


def _binding(settings: MusicBrainzConnectionSettings) -> MusicBrainzBindingRequest:
    assert settings.pending_brainzmash is not None
    pending = settings.pending_brainzmash
    return MusicBrainzBindingRequest(
        access_revision=pending.access_revision,
        source_id=pending.source_id,
        generation=pending.generation,
        disclosure_version=pending.disclosure_version,
    )


def _source_state(settings: MusicBrainzConnectionSettings) -> tuple[str, str, str, int]:
    return (
        settings.source_mode,
        settings.api_url,
        settings.source_id,
        settings.generation,
    )


def test_missing_musicbrainz_config_defaults_to_active_brainzmash(tmp_path: Path):
    prefs = _preferences(tmp_path)
    settings = prefs.get_musicbrainz_connection()

    assert settings.source_mode == "brainzmash"
    assert settings.selected_source_mode == "brainzmash"
    assert settings.api_url == BRAINZMASH_ENDPOINT.rstrip("/")
    assert settings.source_id
    assert settings.generation == 1
    assert settings.pending_brainzmash is None
    assert settings.active_brainzmash is None
    assert is_brainzmash_active_binding_valid(settings)

    persisted = json.loads((tmp_path / "config.json").read_text())
    assert persisted["musicbrainz_settings"]["source_mode"] == "brainzmash"


def test_explicit_official_source_migrates_to_active_brainzmash(tmp_path: Path):
    config_path = tmp_path / "config.json"
    config_path.write_text(
        json.dumps(
            {
                "musicbrainz_settings": {
                    "source_mode": "official",
                    "api_url": "https://musicbrainz.org/ws/2",
                    "rate_limit": 1.0,
                    "concurrent_searches": 6,
                    "source_id": "official-source",
                    "generation": 8,
                }
            }
        )
    )

    settings = _preferences(tmp_path).get_musicbrainz_connection()

    assert settings.source_mode == "brainzmash"
    assert settings.selected_source_mode == "brainzmash"
    assert settings.api_url == BRAINZMASH_ENDPOINT.rstrip("/")
    assert settings.source_id != "official-source"
    assert settings.generation == 1
    assert is_brainzmash_active_binding_valid(settings)
    persisted = json.loads(config_path.read_text())["musicbrainz_settings"]
    assert persisted["source_mode"] == "brainzmash"
    assert persisted["selected_source_mode"] == "brainzmash"


def test_existing_brainzmash_settings_are_canonicalized_without_disclosure_gate(
    tmp_path: Path,
):
    config_path = tmp_path / "config.json"
    config_path.write_text(
        json.dumps(
            {
                "musicbrainz_settings": {
                    "source_mode": "brainzmash",
                    "selected_source_mode": "official",
                    "api_url": "https://musicbrainz.org/ws/2",
                    "rate_limit": 2.0,
                    "concurrent_searches": 8,
                    "source_id": "brainzmash-source",
                    "generation": 4,
                    "source_quarantined": True,
                    "quarantine_reason": "legacy binding",
                }
            }
        )
    )

    settings = _preferences(tmp_path).get_musicbrainz_connection()

    assert settings.source_mode == "brainzmash"
    assert settings.selected_source_mode == "brainzmash"
    assert settings.api_url == BRAINZMASH_ENDPOINT.rstrip("/")
    assert settings.rate_limit == 10.0
    assert settings.concurrent_searches == 1
    assert settings.source_quarantined is False
    assert is_brainzmash_active_binding_valid(settings)
    persisted = json.loads(config_path.read_text())["musicbrainz_settings"]
    assert persisted["api_url"] == BRAINZMASH_ENDPOINT.rstrip("/")
    assert persisted["rate_limit"] == 10.0
    assert persisted["concurrent_searches"] == 1


def test_legacy_official_url_without_source_mode_defaults_to_brainzmash(
    tmp_path: Path,
):
    (tmp_path / "config.json").write_text(
        json.dumps(
            {
                "musicbrainz_settings": {
                    "api_url": "https://musicbrainz.org/ws/2",
                    "rate_limit": 1.0,
                    "concurrent_searches": 6,
                },
                "advanced_settings": {"musicbrainz_concurrent_searches": 4},
            }
        )
    )

    settings = _preferences(tmp_path).get_musicbrainz_connection()

    assert settings.source_mode == "brainzmash"
    assert settings.selected_source_mode == "brainzmash"
    assert settings.api_url == BRAINZMASH_ENDPOINT.rstrip("/")
    assert settings.pending_brainzmash is None


@pytest.mark.parametrize(
    ("source_mode", "api_url", "community_acknowledged"),
    [
        ("mirror", "https://mirror.example/ws/2/", False),
        ("community", "https://community.example/ws/2/", True),
    ],
)
def test_explicit_custom_source_is_preserved_byte_for_byte(
    tmp_path: Path,
    source_mode: str,
    api_url: str,
    community_acknowledged: bool,
):
    section = {
        "source_mode": source_mode,
        "selected_source_mode": source_mode,
        "api_url": api_url,
        "rate_limit": 2.5,
        "concurrent_searches": 4,
        "community_acknowledged": community_acknowledged,
        "source_id": f"{source_mode}-source",
        "generation": 7,
    }
    config_path = tmp_path / "config.json"
    config_path.write_text(json.dumps({"musicbrainz_settings": section}))

    settings = _preferences(tmp_path).get_musicbrainz_connection()

    assert settings.source_mode == source_mode
    assert settings.selected_source_mode == source_mode
    assert settings.api_url == api_url.rstrip("/")
    assert settings.source_id == section["source_id"]
    assert settings.generation == section["generation"]
    persisted = json.loads(config_path.read_text())["musicbrainz_settings"]
    assert persisted == section


def test_invalid_explicit_custom_source_migrates_to_brainzmash(
    tmp_path: Path,
):
    config_path = tmp_path / "config.json"
    config_path.write_text(
        json.dumps(
            {
                "musicbrainz_settings": {
                    "source_mode": "mirror",
                    "selected_source_mode": "mirror",
                    "api_url": "https://mirror.example/ws/2",
                    "rate_limit": 2.0,
                    "concurrent_searches": 0,
                    "source_id": "invalid-mirror",
                    "generation": 3,
                }
            }
        )
    )

    settings = _preferences(tmp_path).get_musicbrainz_connection()

    assert settings.source_mode == "brainzmash"
    assert settings.api_url == BRAINZMASH_ENDPOINT.rstrip("/")
    assert settings.selected_source_mode == "brainzmash"
    assert is_brainzmash_active_binding_valid(settings)
    persisted = json.loads(config_path.read_text())["musicbrainz_settings"]
    assert persisted["source_mode"] == "brainzmash"
    before = config_path.read_text()
    restarted = _preferences(tmp_path).get_musicbrainz_connection()
    assert restarted.source_mode == "brainzmash"
    assert is_brainzmash_active_binding_valid(restarted)
    assert config_path.read_text() == before


@pytest.mark.parametrize(
    "invalid_fields",
    [
        {"pending_brainzmash": {"generation": "not-an-int"}},
        {"selected_source_mode": "community"},
        {"source_id": ""},
        {"source_id": " mirror-source "},
        {"generation": 0},
    ],
    ids=[
        "malformed-pending",
        "selection-mismatch",
        "blank-id",
        "spaced-id",
        "zero-generation",
    ],
)
def test_incomplete_explicit_custom_settings_fall_back_idempotently(
    tmp_path: Path, invalid_fields: dict[str, object]
):
    config_path = tmp_path / "config.json"
    section: dict[str, object] = {
        "source_mode": "mirror",
        "selected_source_mode": "mirror",
        "api_url": "https://mirror.example/ws/2",
        "rate_limit": 2.0,
        "concurrent_searches": 4,
        "source_id": "mirror-source",
        "generation": 3,
    }
    section.update(invalid_fields)
    config_path.write_text(json.dumps({"musicbrainz_settings": section}))

    settings = _preferences(tmp_path).get_musicbrainz_connection()

    assert settings.source_mode == "brainzmash"
    assert settings.selected_source_mode == "brainzmash"
    assert is_brainzmash_active_binding_valid(settings)
    persisted = json.loads(config_path.read_text())["musicbrainz_settings"]
    assert persisted["source_mode"] == "brainzmash"
    before = config_path.read_text()
    restarted = _preferences(tmp_path).get_musicbrainz_connection()
    assert restarted.source_mode == "brainzmash"
    assert is_brainzmash_active_binding_valid(restarted)
    assert config_path.read_text() == before


def test_direct_musicbrainz_save_persists_default_and_custom_sources(
    tmp_path: Path,
):
    prefs = _preferences(tmp_path)

    prefs.save_musicbrainz_connection(
        MusicBrainzConnectionSettings(
            source_mode="official",
            api_url="https://musicbrainz.org/ws/2",
            source_id="legacy-official",
            generation=9,
        )
    )
    default = prefs.get_musicbrainz_connection()
    persisted_default = json.loads((tmp_path / "config.json").read_text())[
        "musicbrainz_settings"
    ]
    assert default.source_mode == "official"
    assert default.api_url == "https://musicbrainz.org/ws/2"
    assert default.selected_source_mode == "official"
    assert persisted_default["source_mode"] == "official"
    assert persisted_default["api_url"] == "https://musicbrainz.org/ws/2"
    assert prefs.get_setting("official_source_selected") is True

    prefs.save_musicbrainz_connection(
        MusicBrainzConnectionSettings(source_mode="brainzmash")
    )
    generated = prefs.get_musicbrainz_connection()
    assert is_brainzmash_active_binding_valid(generated)
    assert generated.source_id
    assert generated.generation >= 1
    assert prefs.get_setting("official_source_selected") is None

    prefs.save_musicbrainz_connection(
        MusicBrainzConnectionSettings(
            source_mode="brainzmash",
            source_id="stable-brainzmash",
            generation=0,
        )
    )
    normalized = prefs.get_musicbrainz_connection()
    assert is_brainzmash_active_binding_valid(normalized)
    assert normalized.source_id == "stable-brainzmash"
    assert normalized.generation == 1
    normalized_identity = (normalized.source_id, normalized.generation)
    prefs.save_musicbrainz_connection(normalized)
    repeated = prefs.get_musicbrainz_connection()
    assert (repeated.source_id, repeated.generation) == normalized_identity

    prefs.save_musicbrainz_connection(
        MusicBrainzConnectionSettings(
            source_mode="community",
            selected_source_mode="community",
            api_url="https://community.example/ws/2",
            rate_limit=2.5,
            concurrent_searches=4,
            community_acknowledged=True,
            source_id="community-source",
            generation=3,
        )
    )
    custom = prefs.get_musicbrainz_connection()
    assert custom.source_mode == "community"
    assert custom.selected_source_mode == "community"
    assert custom.api_url == "https://community.example/ws/2"
    assert custom.rate_limit == 2.5
    assert custom.concurrent_searches == 4
    assert custom.source_id == "community-source"
    assert custom.generation == 3


def test_brainzmash_default_persists_across_restart_idempotently(tmp_path: Path):
    first = _preferences(tmp_path).get_musicbrainz_connection()
    config_path = tmp_path / "config.json"
    before = json.loads(config_path.read_text())["musicbrainz_settings"]

    second = _preferences(tmp_path).get_musicbrainz_connection()
    after = json.loads(config_path.read_text())["musicbrainz_settings"]

    assert second.source_mode == "brainzmash"
    assert second.source_id == first.source_id
    assert second.generation == first.generation
    assert is_brainzmash_active_binding_valid(second)
    assert after == before


def test_legacy_musicbrainz_config_migration_preserves_custom_mirror(tmp_path: Path):
    config_path = tmp_path / "config.json"
    config_path.write_text(
        json.dumps(
            {
                "musicbrainz_settings": {
                    "api_url": "https://legacy-mirror.example/ws/2/",
                    "rate_limit": 2.0,
                    "concurrent_searches": 4,
                }
            }
        )
    )

    settings = _preferences(tmp_path).get_musicbrainz_connection()

    assert settings.source_mode == "mirror"
    assert settings.selected_source_mode == "mirror"
    assert settings.api_url == "https://legacy-mirror.example/ws/2"
    assert settings.pending_brainzmash is None


def test_malformed_legacy_custom_url_falls_back_idempotently(
    tmp_path: Path,
):
    config_path = tmp_path / "config.json"
    config_path.write_text(
        json.dumps(
            {
                "musicbrainz_settings": {
                    "api_url": "https://legacy-mirror.example/ws/2",
                    "rate_limit": 2.0,
                    "concurrent_searches": 4,
                    "generation": "not-an-int",
                }
            }
        )
    )

    settings = _preferences(tmp_path).get_musicbrainz_connection()

    assert settings.source_mode == "brainzmash"
    assert settings.selected_source_mode == "brainzmash"
    assert settings.api_url == BRAINZMASH_ENDPOINT.rstrip("/")
    assert is_brainzmash_active_binding_valid(settings)
    persisted = json.loads(config_path.read_text())["musicbrainz_settings"]
    assert persisted["source_mode"] == "brainzmash"
    before = config_path.read_text()
    restarted = _preferences(tmp_path).get_musicbrainz_connection()
    assert restarted.source_mode == "brainzmash"
    assert is_brainzmash_active_binding_valid(restarted)
    assert config_path.read_text() == before


def test_stage_brainzmash_changes_effective_source_immediately(tmp_path: Path):
    prefs = _preferences(tmp_path)
    staged = prefs.stage_brainzmash()

    assert staged.source_mode == "brainzmash"
    assert staged.selected_source_mode == "brainzmash"
    assert staged.api_url == BRAINZMASH_ENDPOINT.rstrip("/")
    assert staged.pending_brainzmash is not None
    assert is_brainzmash_active_binding_valid(staged)

    current = prefs.get_musicbrainz_connection()
    assert current.source_mode == "brainzmash"
    assert current.pending_brainzmash is not None


def test_unchanged_musicbrainz_source_preserves_identity_and_switch_allocates_new(
    tmp_path: Path,
):
    config_path = tmp_path / "config.json"
    config_path.write_text(
        json.dumps(
            {
                "musicbrainz_settings": {
                    "source_mode": "mirror",
                    "selected_source_mode": "mirror",
                    "api_url": "https://mirror.example/ws/2",
                    "rate_limit": 1.0,
                    "concurrent_searches": 4,
                    "source_id": "stable-source",
                    "generation": 7,
                }
            }
        )
    )
    prefs = _preferences(tmp_path)

    unchanged = prefs.save_musicbrainz_update(
        MusicBrainzSettingsUpdate(
            source_mode="mirror",
            api_url="https://mirror.example/ws/2/",
            rate_limit=2.0,
            concurrent_searches=8,
        )
    )
    assert unchanged.source_id == "stable-source"
    assert unchanged.generation == 7
    assert unchanged.rate_limit == 2.0
    assert unchanged.concurrent_searches == 8

    switched = prefs.save_musicbrainz_update(
        MusicBrainzSettingsUpdate(
            source_mode="official",
            api_url=None,
            rate_limit=1.0,
            concurrent_searches=6,
        )
    )
    assert switched.source_mode == "official"
    assert switched.selected_source_mode == "official"
    assert switched.api_url == "https://musicbrainz.org/ws/2"
    assert switched.rate_limit == 1.0
    assert switched.concurrent_searches == 6
    assert switched.source_id
    assert switched.source_id != "stable-source"
    assert switched.generation == 8
    assert prefs.get_setting("official_source_selected") is True

@pytest.mark.parametrize(
    ("source_mode", "api_url", "community_acknowledged"),
    [
        ("mirror", BRAINZMASH_ENDPOINT, False),
        ("community", "https://musicbrainz.org/ws/2", False),
    ],
)
def test_noncustom_endpoints_collapse_to_the_brainzmash_default(
    tmp_path: Path, source_mode: str, api_url: str, community_acknowledged: bool
):
    prefs = _preferences(tmp_path)

    result = prefs.save_musicbrainz_update(
        MusicBrainzSettingsUpdate(
            source_mode=source_mode,
            api_url=api_url,
            rate_limit=2.0,
            concurrent_searches=4,
            community_acknowledged=community_acknowledged,
        )
    )

    assert result.source_mode == "brainzmash"
    assert result.selected_source_mode == "brainzmash"
    assert result.api_url == BRAINZMASH_ENDPOINT.rstrip("/")
    assert result.rate_limit == 10.0
    assert result.concurrent_searches == 1


def test_deliberate_official_save_persists_and_survives_restart(tmp_path: Path):
    prefs = _preferences(tmp_path)

    revision_before = prefs.get_musicbrainz_settings_revision()
    saved = prefs.save_musicbrainz_update(
        MusicBrainzSettingsUpdate(
            source_mode="official",
            api_url=None,
            rate_limit=1.0,
            concurrent_searches=6,
        )
    )
    assert saved.source_mode == "official"
    assert saved.api_url == "https://musicbrainz.org/ws/2"
    assert saved.rate_limit == 1.0
    assert saved.selected_source_mode == "official"
    assert prefs.get_setting("official_source_selected") is True
    assert prefs.get_musicbrainz_settings_revision() == revision_before + 1

    persisted = json.loads((tmp_path / "config.json").read_text())
    assert persisted["musicbrainz_settings"]["source_mode"] == "official"
    assert persisted["_internal"]["official_source_selected"] is True

    restarted = _preferences(tmp_path).get_musicbrainz_connection()
    assert restarted.source_mode == "official"
    assert restarted.selected_source_mode == "official"
    assert restarted.api_url == "https://musicbrainz.org/ws/2"
    assert restarted.rate_limit == 1.0
    assert not is_brainzmash_active_binding_valid(restarted)

def test_mutated_official_state_clears_brainzmash_binding_on_save_and_restart(
    tmp_path: Path,
):
    from infrastructure.serialization import to_jsonable
    from repositories.musicbrainz_base import OFFICIAL_MB_API_BASE
    from api.v1.schemas.settings import (
        _OFFICIAL_MB_CONCURRENT_SEARCHES,
        _OFFICIAL_MB_RATE_LIMIT,
    )

    prefs = _preferences(tmp_path)
    staged = prefs.stage_brainzmash()
    pending = staged.pending_brainzmash
    assert pending is not None
    binding = _binding(staged)
    prefs.accept_brainzmash_consent(binding, "admin-1")
    prefs.record_brainzmash_verification(binding)
    _, promoted = prefs.promote_brainzmash(binding)
    active = promoted.active_brainzmash
    assert active is not None

    mutated = MusicBrainzConnectionSettings(
        source_mode="official",
        api_url=OFFICIAL_MB_API_BASE,
        rate_limit=_OFFICIAL_MB_RATE_LIMIT,
        concurrent_searches=_OFFICIAL_MB_CONCURRENT_SEARCHES,
        selected_source_mode="official",
        source_id="official-source",
        generation=9,
    )
    mutated.api_url = BRAINZMASH_ENDPOINT
    mutated.rate_limit = 500.0
    mutated.concurrent_searches = 64
    mutated.selected_source_mode = "brainzmash"
    mutated.pending_brainzmash = pending
    mutated.active_brainzmash = active
    mutated.source_quarantined = True
    mutated.quarantine_reason = "stale"
    mutated.community_acknowledged = True
    mutated.clamped_to_official_limits = True

    prefs.save_musicbrainz_connection(mutated)
    saved = prefs.get_musicbrainz_connection()
    assert saved.source_mode == "official"
    assert saved.api_url == OFFICIAL_MB_API_BASE
    assert saved.rate_limit == _OFFICIAL_MB_RATE_LIMIT
    assert saved.concurrent_searches == _OFFICIAL_MB_CONCURRENT_SEARCHES
    assert saved.selected_source_mode == "official"
    assert saved.pending_brainzmash is None
    assert saved.active_brainzmash is None
    assert saved.source_quarantined is False
    assert saved.quarantine_reason == ""
    assert saved.community_acknowledged is False
    assert saved.clamped_to_official_limits is False

    config_path = tmp_path / "config.json"
    config = json.loads(config_path.read_text())
    section = config["musicbrainz_settings"]
    section.update(
        {
            "api_url": BRAINZMASH_ENDPOINT,
            "rate_limit": 500.0,
            "concurrent_searches": 64,
            "selected_source_mode": "brainzmash",
            "pending_brainzmash": to_jsonable(pending),
            "active_brainzmash": to_jsonable(active),
            "source_quarantined": True,
            "quarantine_reason": "stale",
            "community_acknowledged": True,
            "clamped_to_official_limits": True,
        }
    )
    config_path.write_text(json.dumps(config))

    restarted = _preferences(tmp_path)
    canonical = restarted.get_musicbrainz_connection()
    assert canonical.source_mode == "official"
    assert canonical.api_url == OFFICIAL_MB_API_BASE
    assert canonical.rate_limit == _OFFICIAL_MB_RATE_LIMIT
    assert canonical.concurrent_searches == _OFFICIAL_MB_CONCURRENT_SEARCHES
    assert canonical.selected_source_mode == "official"
    assert canonical.pending_brainzmash is None
    assert canonical.active_brainzmash is None
    assert canonical.source_quarantined is False
    assert canonical.quarantine_reason == ""
    assert canonical.community_acknowledged is False
    assert canonical.clamped_to_official_limits is False
    with pytest.raises(ValidationError, match="stale"):
        restarted.promote_brainzmash(binding)


def test_deliberate_official_marker_does_not_resurrect_missing_section(tmp_path: Path):
    prefs = _preferences(tmp_path)
    assert (
        prefs.save_musicbrainz_update(
            MusicBrainzSettingsUpdate(
                source_mode="official",
                api_url=None,
                rate_limit=1.0,
                concurrent_searches=6,
            )
        ).source_mode
        == "official"
    )

    config_path = tmp_path / "config.json"
    config = json.loads(config_path.read_text())
    config["musicbrainz_settings"] = "not-a-section"
    config_path.write_text(json.dumps(config))

    rebuilt = _preferences(tmp_path).get_musicbrainz_connection()
    assert rebuilt.source_mode == "brainzmash"
    assert rebuilt.selected_source_mode == "brainzmash"
    assert rebuilt.api_url == BRAINZMASH_ENDPOINT.rstrip("/")
    assert rebuilt.rate_limit == 10.0
    assert rebuilt.concurrent_searches == 1
    assert _preferences(tmp_path).get_setting("official_source_selected") is None


def test_migrated_official_still_converts_without_marker(tmp_path: Path):
    config_path = tmp_path / "config.json"
    config_path.write_text(
        json.dumps(
            {
                "musicbrainz_settings": {
                    "source_mode": "official",
                    "api_url": "https://musicbrainz.org/ws/2",
                    "rate_limit": 1.0,
                    "concurrent_searches": 6,
                }
            }
        )
    )

    migrated = _preferences(tmp_path).get_musicbrainz_connection()
    assert migrated.source_mode == "brainzmash"
    assert migrated.api_url == BRAINZMASH_ENDPOINT.rstrip("/")


def test_brainzmash_activation_is_consent_and_verification_bound(tmp_path: Path):
    prefs = _preferences(tmp_path)
    staged = prefs.stage_brainzmash()

    assert staged.source_mode == "brainzmash"
    assert staged.selected_source_mode == "brainzmash"
    assert staged.api_url == BRAINZMASH_ENDPOINT.rstrip("/")
    assert staged.pending_brainzmash is not None
    binding = _binding(staged)

    consented = prefs.accept_brainzmash_consent(binding, "admin-1")
    assert consented.pending_brainzmash is not None
    assert consented.pending_brainzmash.consented is True
    verified = prefs.record_brainzmash_verification(binding)
    assert verified.pending_brainzmash is not None
    assert verified.pending_brainzmash.verified is True

    previous, promoted = prefs.promote_brainzmash(binding)

    assert previous.source_mode == "brainzmash"
    assert promoted.source_mode == "brainzmash"
    assert promoted.selected_source_mode == "brainzmash"
    assert promoted.api_url == BRAINZMASH_ENDPOINT.rstrip("/")
    assert promoted.source_id == binding.source_id
    assert promoted.generation == binding.generation
    assert promoted.rate_limit == 10.0
    assert promoted.concurrent_searches == 1
    assert promoted.pending_brainzmash is None
    assert promoted.active_brainzmash is not None
    assert promoted.active_brainzmash.endpoint == BRAINZMASH_ENDPOINT
    assert promoted.active_brainzmash.access_revision == binding.access_revision
    assert promoted.active_brainzmash.source_id == binding.source_id
    assert promoted.active_brainzmash.generation == binding.generation
    assert (
        promoted.active_brainzmash.disclosure_version == BRAINZMASH_DISCLOSURE_VERSION
    )
    assert promoted.active_brainzmash.consented is True
    assert promoted.active_brainzmash.verified is True
    persisted = prefs.get_musicbrainz_connection()
    assert persisted.source_mode == "brainzmash"
    assert persisted.api_url == BRAINZMASH_ENDPOINT.rstrip("/")
    assert persisted.pending_brainzmash is None


@pytest.mark.asyncio
async def test_valid_active_brainzmash_binding_survives_restart_and_routes_only_brainzmash(
    tmp_path: Path,
):
    prefs = _preferences(tmp_path)
    staged = prefs.stage_brainzmash()
    binding = _binding(staged)
    prefs.accept_brainzmash_consent(binding, "admin-1")
    prefs.record_brainzmash_verification(binding)
    prefs.promote_brainzmash(binding)

    restarted = _preferences(tmp_path)
    settings = restarted.get_musicbrainz_connection()
    assert settings.active_brainzmash is not None
    assert settings.active_brainzmash.access_revision == binding.access_revision
    assert settings.active_brainzmash.source_id == binding.source_id
    assert settings.active_brainzmash.generation == binding.generation
    assert (
        settings.active_brainzmash.disclosure_version == BRAINZMASH_DISCLOSURE_VERSION
    )
    assert settings.active_brainzmash.consented is True
    assert settings.active_brainzmash.verified is True

    official_client = AsyncMock()
    brainzmash_client = AsyncMock()
    brainzmash_client.get.return_value = httpx.Response(200, json={"artist": []})
    previous = mb_base.capture_mb_source_context()
    previous_runtime = mb_base.brainzmash_runtime_enabled()
    try:
        MusicBrainzRepository(
            official_client,
            object(),
            restarted,
            brainzmash_http_client=brainzmash_client,
        )
        assert await mb_base.mb_api_get("/artist") == {"artist": []}
        brainzmash_client.get.assert_awaited_once()
        official_client.get.assert_not_awaited()
        with pytest.raises(ConfigurationError, match="official"):
            mb_base.get_mb_http_client()
    finally:
        mb_base.set_mb_api_base(
            previous.source_url,
            source_mode=previous.source_mode,
            source_id=previous.source_id,
            generation=previous.generation,
            brainzmash_binding_valid=previous_runtime,
        )


@pytest.mark.parametrize(
    ("binding_field", "binding_value"),
    [
        ("endpoint", "https://api.brainzmash.cc/ws/3/"),
        ("disclosure_version", "brainzmash-old"),
    ],
    ids=["endpoint-drift", "disclosure-drift"],
)
def test_restart_keeps_brainzmash_active_when_optional_binding_drifts(
    tmp_path: Path, binding_field: str, binding_value: str
):
    prefs = _preferences(tmp_path)
    staged = prefs.stage_brainzmash()
    binding = _binding(staged)
    prefs.accept_brainzmash_consent(binding, "admin-1")
    prefs.record_brainzmash_verification(binding)
    prefs.promote_brainzmash(binding)

    config_path = tmp_path / "config.json"
    config = json.loads(config_path.read_text())
    config["musicbrainz_settings"]["active_brainzmash"][binding_field] = binding_value
    config_path.write_text(json.dumps(config))

    restarted = _preferences(tmp_path)
    settings = restarted.get_musicbrainz_connection()
    assert settings.source_mode == "brainzmash"
    assert settings.active_brainzmash is not None
    assert getattr(settings.active_brainzmash, binding_field) == binding_value
    assert is_brainzmash_active_binding_valid(settings)

    restarted_again = _preferences(tmp_path).get_musicbrainz_connection()
    assert restarted_again.source_id == settings.source_id
    assert restarted_again.generation == settings.generation
    assert restarted_again.source_mode == "brainzmash"


def test_restart_keeps_optional_pending_disclosure_without_source_fallback(
    tmp_path: Path,
):
    prefs = _preferences(tmp_path)
    staged = prefs.stage_brainzmash()
    pending = staged.pending_brainzmash
    assert pending is not None
    config_path = tmp_path / "config.json"
    config = json.loads(config_path.read_text())
    config["musicbrainz_settings"]["pending_brainzmash"]["endpoint"] = (
        "https://api.brainzmash.cc/ws/3/"
    )
    config_path.write_text(json.dumps(config))

    restarted = _preferences(tmp_path).get_musicbrainz_connection()

    assert restarted.source_mode == "brainzmash"
    assert restarted.selected_source_mode == "brainzmash"
    assert restarted.pending_brainzmash is not None
    assert restarted.pending_brainzmash.source_id == pending.source_id
    assert restarted.pending_brainzmash.endpoint.endswith("/ws/3/")
    assert is_brainzmash_active_binding_valid(restarted)


def test_brainzmash_binding_is_compare_and_swap_protected(tmp_path: Path):
    prefs = _preferences(tmp_path)
    staged = prefs.stage_brainzmash()
    binding = _binding(staged)
    stale = MusicBrainzBindingRequest(
        access_revision=binding.access_revision,
        source_id=binding.source_id,
        generation=binding.generation,
        disclosure_version="old-disclosure",
    )

    with pytest.raises(ValidationError, match="stale|outdated"):
        prefs.accept_brainzmash_consent(stale, "admin-1")
    current = prefs.get_musicbrainz_connection()
    assert current.source_mode == "brainzmash"
    assert current.pending_brainzmash is not None
    assert current.pending_brainzmash.consented is False


@pytest.mark.asyncio
async def test_coordinated_brainzmash_stage_applies_pending_transition(tmp_path: Path):
    prefs = _preferences(tmp_path)
    service = SettingsService(prefs, cache=object())
    applied: list[MusicBrainzConnectionSettings] = []

    async def apply(settings: MusicBrainzConnectionSettings) -> None:
        applied.append(settings)

    service._apply_musicbrainz_settings = apply

    staged = await service.stage_brainzmash()

    assert staged.source_mode == "brainzmash"
    assert staged.pending_brainzmash is not None
    assert prefs.get_musicbrainz_connection() == staged
    assert applied == [staged]


@pytest.mark.asyncio
async def test_coordinated_musicbrainz_update_rolls_back_on_runtime_failure(
    tmp_path: Path,
):
    prefs = _preferences(tmp_path)
    old = prefs.save_musicbrainz_update(
        MusicBrainzSettingsUpdate(
            source_mode="mirror",
            api_url="https://old.example/ws/2",
        )
    )
    new_url = "https://new.example/ws/2"
    runtime = {"settings": old}
    applied: list[MusicBrainzConnectionSettings] = []
    service = SettingsService(prefs, cache=object())

    async def apply(settings: MusicBrainzConnectionSettings) -> None:
        applied.append(settings)
        runtime["settings"] = settings
        if settings.api_url == new_url:
            raise RuntimeError("runtime apply failed")

    service._apply_musicbrainz_settings = apply
    with pytest.raises(RuntimeError, match="runtime apply failed"):
        await service.save_musicbrainz_update(
            MusicBrainzSettingsUpdate(source_mode="mirror", api_url=new_url)
        )

    persisted = prefs.get_musicbrainz_connection()
    assert _source_state(persisted) == _source_state(old)
    assert _source_state(runtime["settings"]) == _source_state(old)
    assert [settings.api_url for settings in applied] == [new_url, old.api_url]


@pytest.mark.asyncio
async def test_coordinated_musicbrainz_update_rolls_back_on_cancellation(
    tmp_path: Path,
):
    prefs = _preferences(tmp_path)
    old = prefs.save_musicbrainz_update(
        MusicBrainzSettingsUpdate(
            source_mode="mirror",
            api_url="https://old.example/ws/2",
        )
    )
    new_url = "https://new.example/ws/2"
    runtime = {"settings": old}
    applied: list[MusicBrainzConnectionSettings] = []
    service = SettingsService(prefs, cache=object())

    async def apply(settings: MusicBrainzConnectionSettings) -> None:
        applied.append(settings)
        runtime["settings"] = settings
        if settings.api_url == new_url:
            raise asyncio.CancelledError()

    service._apply_musicbrainz_settings = apply
    with pytest.raises(asyncio.CancelledError):
        await service.save_musicbrainz_update(
            MusicBrainzSettingsUpdate(source_mode="mirror", api_url=new_url)
        )

    persisted = prefs.get_musicbrainz_connection()
    assert _source_state(persisted) == _source_state(old)
    assert _source_state(runtime["settings"]) == _source_state(old)
    assert [settings.api_url for settings in applied] == [new_url, old.api_url]


@pytest.mark.asyncio
async def test_coordinated_musicbrainz_update_does_not_clobber_newer_cas_save(
    tmp_path: Path,
):
    prefs = _preferences(tmp_path)
    old = prefs.save_musicbrainz_update(
        MusicBrainzSettingsUpdate(
            source_mode="mirror",
            api_url="https://old.example/ws/2",
        )
    )
    new_url = "https://new.example/ws/2"
    newer_url = "https://newer.example/ws/2"
    runtime = {"settings": old}
    applied: list[MusicBrainzConnectionSettings] = []
    service = SettingsService(prefs, cache=object())

    async def apply(settings: MusicBrainzConnectionSettings) -> None:
        applied.append(settings)
        runtime["settings"] = settings
        if settings.api_url == new_url:
            newer = prefs.save_musicbrainz_update(
                MusicBrainzSettingsUpdate(source_mode="mirror", api_url=newer_url)
            )
            runtime["settings"] = newer
            raise RuntimeError("runtime apply failed")

    service._apply_musicbrainz_settings = apply
    with pytest.raises(RuntimeError, match="runtime apply failed"):
        await service.save_musicbrainz_update(
            MusicBrainzSettingsUpdate(source_mode="mirror", api_url=new_url)
        )

    persisted = prefs.get_musicbrainz_connection()
    assert persisted.api_url == newer_url
    assert _source_state(runtime["settings"]) == _source_state(persisted)
    assert [settings.api_url for settings in applied] == [new_url]


@pytest.mark.asyncio
async def test_runtime_brainzmash_commit_pins_ten_per_second_capacity():
    service = SettingsService.__new__(SettingsService)

    class _Cache:
        async def clear_prefix(self, _prefix: str) -> int:
            return 0

    service._cache = _Cache()
    service._disk_cache = None
    old_source = mb_base.capture_mb_source_context()
    old_rate = mb_base.mb_rate_limiter.rate
    old_capacity = mb_base.mb_rate_limiter.capacity
    old_bypass = mb_base.mb_rate_limiter_bypassed()
    try:
        settings = MusicBrainzConnectionSettings(
            source_mode="brainzmash",
            source_id="brainzmash-runtime",
            generation=old_source.generation + 1,
            active_brainzmash=BrainzMashActiveBinding(
                endpoint=BRAINZMASH_ENDPOINT,
                access_revision="runtime-access",
                source_id="brainzmash-runtime",
                generation=old_source.generation + 1,
                disclosure_version=BRAINZMASH_DISCLOSURE_VERSION,
                consented=True,
                verified=True,
            ),
        )
        await service.on_musicbrainz_settings_changed(settings)
        assert mb_base.get_mb_source_mode() == "brainzmash"
        assert mb_base.get_mb_api_base() == BRAINZMASH_ENDPOINT.rstrip("/")
        assert mb_base.mb_rate_limiter.rate == 1.0
        assert mb_base.mb_rate_limiter.capacity == 1
        assert mb_base.brainzmash_rate_limiter.rate == 10.0
        assert mb_base.brainzmash_rate_limiter.capacity == 1
    finally:
        mb_base.set_mb_rate_limiter_bypass(old_bypass)
        mb_base.mb_rate_limiter.update_rate(old_rate)
        mb_base.mb_rate_limiter.update_capacity(old_capacity)
        mb_base.set_mb_api_base(
            old_source.source_url,
            source_mode=old_source.source_mode,
            source_id=old_source.source_id,
            generation=old_source.generation,
        )


@pytest.mark.asyncio
async def test_runtime_commit_rejects_non_builtin_brainzmash_origin():
    service = SettingsService.__new__(SettingsService)

    class _Cache:
        async def clear_prefix(self, _prefix: str) -> int:
            return 0

    service._cache = _Cache()
    service._disk_cache = None

    malformed = MusicBrainzConnectionSettings(
        source_mode="brainzmash",
        api_url="https://musicbrainz.org/ws/2",
        source_id="brainz-source",
        generation=3,
    )
    # The schema canonicalizes direct BrainzMash settings. A mutated instance
    # must still be rejected by the runtime commit guard.
    malformed.api_url = "https://mirror.example/ws/2"

    with pytest.raises(ConfigurationError, match="BrainzMash endpoint"):
        await service.on_musicbrainz_settings_changed(malformed)


@pytest.mark.asyncio
async def test_legacy_verification_cannot_probe_while_brainzmash_active():
    service = SettingsService.__new__(SettingsService)
    service._preferences_service = MagicMock()
    service._preferences_service.get_musicbrainz_connection.return_value = (
        MusicBrainzConnectionSettings(source_mode="brainzmash")
    )
    service._preferences_service.get_musicbrainz_settings_revision.return_value = 3
    service._preferences_service.musicbrainz_settings_match.return_value = True
    settings = MusicBrainzConnectionSettings(source_mode="brainzmash")

    result = await service.verify_musicbrainz(settings)

    assert result.valid is False
    assert "consent-bound" in result.message
    assert BRAINZMASH_DISCLOSURE_VERSION
