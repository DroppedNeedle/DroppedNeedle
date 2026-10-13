"""Round-trip tests for AdvancedSettings ↔ AdvancedSettingsFrontend conversion."""

import msgspec
import pytest

from api.v1.schemas.advanced_settings import AdvancedSettings, AdvancedSettingsFrontend


class TestDirectRemoteImagesRoundTrip:
    def test_default_value_is_true(self) -> None:
        settings = AdvancedSettings()
        assert settings.direct_remote_images_enabled is True

    def test_frontend_default_is_true(self) -> None:
        frontend = AdvancedSettingsFrontend()
        assert frontend.direct_remote_images_enabled is True

    def test_roundtrip_preserves_true(self) -> None:
        backend = AdvancedSettings(direct_remote_images_enabled=True)
        frontend = AdvancedSettingsFrontend.from_backend(backend)
        assert frontend.direct_remote_images_enabled is True
        restored = frontend.to_backend()
        assert restored.direct_remote_images_enabled is True

    def test_roundtrip_preserves_false(self) -> None:
        backend = AdvancedSettings(direct_remote_images_enabled=False)
        frontend = AdvancedSettingsFrontend.from_backend(backend)
        assert frontend.direct_remote_images_enabled is False
        restored = frontend.to_backend()
        assert restored.direct_remote_images_enabled is False


class TestAudioDBNameSearchFallbackRoundTrip:
    def test_default_value_is_false(self) -> None:
        settings = AdvancedSettings()
        assert settings.audiodb_name_search_fallback is False

    def test_frontend_default_is_false(self) -> None:
        frontend = AdvancedSettingsFrontend()
        assert frontend.audiodb_name_search_fallback is False

    def test_roundtrip_preserves_true(self) -> None:
        backend = AdvancedSettings(audiodb_name_search_fallback=True)
        frontend = AdvancedSettingsFrontend.from_backend(backend)
        assert frontend.audiodb_name_search_fallback is True
        restored = frontend.to_backend()
        assert restored.audiodb_name_search_fallback is True

    def test_roundtrip_preserves_false(self) -> None:
        backend = AdvancedSettings(audiodb_name_search_fallback=False)
        frontend = AdvancedSettingsFrontend.from_backend(backend)
        assert frontend.audiodb_name_search_fallback is False
        restored = frontend.to_backend()
        assert restored.audiodb_name_search_fallback is False


class TestCacheTtlAudiodbFoundRoundTrip:
    def test_default_value(self) -> None:
        settings = AdvancedSettings()
        frontend = AdvancedSettingsFrontend.from_backend(settings)
        assert frontend.cache_ttl_audiodb_found == 168

    def test_roundtrip_preserves(self) -> None:
        backend = AdvancedSettings(cache_ttl_audiodb_found=604800)
        frontend = AdvancedSettingsFrontend.from_backend(backend)
        assert frontend.cache_ttl_audiodb_found == 168
        restored = frontend.to_backend()
        assert restored.cache_ttl_audiodb_found == 604800

    def test_custom_value_roundtrip(self) -> None:
        backend = AdvancedSettings(cache_ttl_audiodb_found=36000)
        frontend = AdvancedSettingsFrontend.from_backend(backend)
        assert frontend.cache_ttl_audiodb_found == 10
        restored = frontend.to_backend()
        assert restored.cache_ttl_audiodb_found == 36000


class TestCacheTtlAudiodbNotFoundRoundTrip:
    def test_default_value(self) -> None:
        settings = AdvancedSettings()
        frontend = AdvancedSettingsFrontend.from_backend(settings)
        assert frontend.cache_ttl_audiodb_not_found == 24

    def test_roundtrip_preserves(self) -> None:
        backend = AdvancedSettings(cache_ttl_audiodb_not_found=86400)
        frontend = AdvancedSettingsFrontend.from_backend(backend)
        assert frontend.cache_ttl_audiodb_not_found == 24
        restored = frontend.to_backend()
        assert restored.cache_ttl_audiodb_not_found == 86400

    def test_custom_value_roundtrip(self) -> None:
        backend = AdvancedSettings(cache_ttl_audiodb_not_found=7200)
        frontend = AdvancedSettingsFrontend.from_backend(backend)
        assert frontend.cache_ttl_audiodb_not_found == 2
        restored = frontend.to_backend()
        assert restored.cache_ttl_audiodb_not_found == 7200


class TestCacheTtlAudiodbLibraryRoundTrip:
    def test_default_value(self) -> None:
        settings = AdvancedSettings()
        frontend = AdvancedSettingsFrontend.from_backend(settings)
        assert frontend.cache_ttl_audiodb_library == 336

    def test_roundtrip_preserves(self) -> None:
        backend = AdvancedSettings(cache_ttl_audiodb_library=1209600)
        frontend = AdvancedSettingsFrontend.from_backend(backend)
        assert frontend.cache_ttl_audiodb_library == 336
        restored = frontend.to_backend()
        assert restored.cache_ttl_audiodb_library == 1209600


class TestCacheTtlRecentlyViewedBytesRoundTrip:
    def test_default_value(self) -> None:
        settings = AdvancedSettings()
        frontend = AdvancedSettingsFrontend.from_backend(settings)
        assert frontend.cache_ttl_recently_viewed_bytes == 48

    def test_roundtrip_preserves(self) -> None:
        backend = AdvancedSettings(cache_ttl_recently_viewed_bytes=172800)
        frontend = AdvancedSettingsFrontend.from_backend(backend)
        assert frontend.cache_ttl_recently_viewed_bytes == 48
        restored = frontend.to_backend()
        assert restored.cache_ttl_recently_viewed_bytes == 172800


class TestAudiodbValidationClamping:
    def test_cache_ttl_found_below_min(self) -> None:
        with pytest.raises(msgspec.ValidationError):
            AdvancedSettings(cache_ttl_audiodb_found=100)

    def test_cache_ttl_found_above_max(self) -> None:
        with pytest.raises(msgspec.ValidationError):
            AdvancedSettings(cache_ttl_audiodb_found=99999999)

    def test_cache_ttl_not_found_below_min(self) -> None:
        with pytest.raises(msgspec.ValidationError):
            AdvancedSettings(cache_ttl_audiodb_not_found=100)

    def test_cache_ttl_not_found_above_max(self) -> None:
        with pytest.raises(msgspec.ValidationError):
            AdvancedSettings(cache_ttl_audiodb_not_found=999999)

    def test_cache_ttl_library_below_min(self) -> None:
        with pytest.raises(msgspec.ValidationError):
            AdvancedSettings(cache_ttl_audiodb_library=100)

    def test_cache_ttl_library_above_max(self) -> None:
        with pytest.raises(msgspec.ValidationError):
            AdvancedSettings(cache_ttl_audiodb_library=99999999)

    def test_cache_ttl_recently_viewed_bytes_below_min(self) -> None:
        with pytest.raises(msgspec.ValidationError):
            AdvancedSettings(cache_ttl_recently_viewed_bytes=100)

    def test_cache_ttl_recently_viewed_bytes_above_max(self) -> None:
        with pytest.raises(msgspec.ValidationError):
            AdvancedSettings(cache_ttl_recently_viewed_bytes=999999)

    def test_api_key_empty_coerced(self) -> None:
        settings = AdvancedSettings(audiodb_api_key="")
        assert settings.audiodb_api_key == "123"

    def test_api_key_whitespace_coerced(self) -> None:
        settings = AdvancedSettings(audiodb_api_key="   ")
        assert settings.audiodb_api_key == "123"


class TestAudiodbEnabledRoundTrip:
    def test_default_is_true(self) -> None:
        settings = AdvancedSettings()
        frontend = AdvancedSettingsFrontend.from_backend(settings)
        assert frontend.audiodb_enabled is True

    def test_roundtrip_false(self) -> None:
        backend = AdvancedSettings(audiodb_enabled=False)
        frontend = AdvancedSettingsFrontend.from_backend(backend)
        assert frontend.audiodb_enabled is False
        restored = frontend.to_backend()
        assert restored.audiodb_enabled is False

    def test_roundtrip_true(self) -> None:
        backend = AdvancedSettings(audiodb_enabled=True)
        frontend = AdvancedSettingsFrontend.from_backend(backend)
        assert frontend.audiodb_enabled is True
        restored = frontend.to_backend()
        assert restored.audiodb_enabled is True


class TestSyncSettingsRoundTrip:
    """Round-trip tests for the sync robustness settings."""

    @pytest.mark.parametrize("field,backend_val,frontend_val", [
        ("sync_stall_timeout_minutes", 15, 15),
        ("sync_max_timeout_hours", 6, 6),
        ("audiodb_prewarm_concurrency", 6, 6),
        ("audiodb_prewarm_delay", 1.5, 1.5),
        ("artist_discovery_precache_concurrency", 5, 5),
    ])
    def test_roundtrip_preserves_value(self, field: str, backend_val, frontend_val) -> None:
        backend = AdvancedSettings(**{field: backend_val})
        frontend = AdvancedSettingsFrontend.from_backend(backend)
        assert getattr(frontend, field) == frontend_val
        restored = frontend.to_backend()
        assert getattr(restored, field) == backend_val

    @pytest.mark.parametrize("field,default_val", [
        ("sync_stall_timeout_minutes", 10),
        ("sync_max_timeout_hours", 8),
        ("audiodb_prewarm_concurrency", 4),
        ("audiodb_prewarm_delay", 0.3),
        ("artist_discovery_precache_concurrency", 5),
        ("artist_discovery_precache_delay", 0.2),
    ])
    def test_defaults_match(self, field: str, default_val) -> None:
        backend = AdvancedSettings()
        frontend = AdvancedSettingsFrontend()
        assert getattr(backend, field) == default_val
        assert getattr(frontend, field) == default_val


class TestSubsonicSearchPageLimit:
    def test_default_is_arpeggi_compatible(self) -> None:
        assert AdvancedSettings().subsonic_search_page_limit == 1000
        assert AdvancedSettingsFrontend().subsonic_search_page_limit == 1000

    def test_roundtrip_preserves_custom_limit(self) -> None:
        frontend = AdvancedSettingsFrontend.from_backend(
            AdvancedSettings(subsonic_search_page_limit=25_000)
        )
        assert frontend.subsonic_search_page_limit == 25_000
        assert frontend.to_backend().subsonic_search_page_limit == 25_000

    @pytest.mark.parametrize("value", [0, -1, 2_147_483_648])
    def test_rejects_invalid_backend_limit(self, value: int) -> None:
        with pytest.raises(msgspec.ValidationError):
            AdvancedSettings(subsonic_search_page_limit=value)

    @pytest.mark.parametrize("value", [0, -1, 2_147_483_648])
    def test_rejects_invalid_frontend_limit(self, value: int) -> None:
        with pytest.raises(msgspec.ValidationError):
            AdvancedSettingsFrontend(subsonic_search_page_limit=value)


class TestPreferLocalCoverArtRoundTrip:
    def test_default_value_is_true(self) -> None:
        settings = AdvancedSettings()
        assert settings.prefer_local_cover_art is True

    def test_frontend_default_is_true(self) -> None:
        frontend = AdvancedSettingsFrontend()
        assert frontend.prefer_local_cover_art is True

    def test_roundtrip_preserves_true(self) -> None:
        backend = AdvancedSettings(prefer_local_cover_art=True)
        frontend = AdvancedSettingsFrontend.from_backend(backend)
        assert frontend.prefer_local_cover_art is True
        restored = frontend.to_backend()
        assert restored.prefer_local_cover_art is True

    def test_roundtrip_preserves_false(self) -> None:
        backend = AdvancedSettings(prefer_local_cover_art=False)
        frontend = AdvancedSettingsFrontend.from_backend(backend)
        assert frontend.prefer_local_cover_art is False
        restored = frontend.to_backend()
        assert restored.prefer_local_cover_art is False
