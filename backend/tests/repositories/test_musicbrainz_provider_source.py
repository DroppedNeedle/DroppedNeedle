from types import SimpleNamespace
from unittest.mock import MagicMock

import pytest

import core.dependencies.repo_providers as repo_providers
import repositories.musicbrainz_base as mb_base
from api.v1.schemas.settings import MusicBrainzConnectionSettings
from core.exceptions import ConfigurationError


@pytest.mark.parametrize(
    ("source_mode", "api_url", "source_id", "generation"),
    [
        ("official", "https://musicbrainz.org/ws/2", "official-id", 11),
        ("mirror", "https://mirror.example/ws/2", "mirror-id", 12),
        ("community", "https://community.example/ws/2", "community-id", 13),
        ("brainzmash", "https://api.brainzmash.cc/ws/2", "brainzmash-id", 14),
    ],
)
def test_production_musicbrainz_provider_applies_every_source_mode(
    monkeypatch, source_mode, api_url, source_id, generation
):
    before = mb_base.capture_mb_source_context()
    before_official_client = mb_base._http_client
    before_brainzmash_client = mb_base._brainzmash_http_client
    before_rate = mb_base.mb_rate_limiter.rate
    before_capacity = mb_base.mb_rate_limiter.capacity
    before_bypass = mb_base.mb_rate_limiter_bypassed()
    before_brainzmash_rate = mb_base.brainzmash_rate_limiter.rate
    before_brainzmash_capacity = mb_base.brainzmash_rate_limiter.capacity

    official_client = object()
    brainzmash_client = object()
    cache = object()
    canonical_store = object()
    preferences = MagicMock()
    preferences.get_musicbrainz_connection.return_value = MusicBrainzConnectionSettings(
        source_mode=source_mode,
        selected_source_mode=source_mode,
        api_url=api_url,
        source_id=source_id,
        generation=generation,
        community_acknowledged=source_mode == "community",
        rate_limit=25.0,
        concurrent_searches=24,
    )
    monkeypatch.setattr(
        repo_providers, "_get_configured_http_client", lambda: official_client
    )
    monkeypatch.setattr(
        repo_providers,
        "get_brainzmash_http_client",
        lambda *args, **kwargs: brainzmash_client,
    )
    monkeypatch.setattr(repo_providers, "get_settings", lambda: SimpleNamespace())
    monkeypatch.setattr(repo_providers, "get_cache", lambda: cache)
    monkeypatch.setattr(repo_providers, "get_preferences_service", lambda: preferences)
    monkeypatch.setattr(
        repo_providers, "get_mb_canonical_store", lambda: canonical_store
    )
    repo_providers.get_musicbrainz_repository.cache_clear()

    try:
        repository = repo_providers.get_musicbrainz_repository()

        assert repository._cache is cache
        assert repository.mb_canonical_store is canonical_store
        assert mb_base._http_client is official_client
        assert mb_base._brainzmash_http_client is brainzmash_client
        assert mb_base.get_mb_api_base() == api_url
        assert mb_base.get_mb_source_mode() == source_mode
        assert mb_base.get_mb_source_id() == source_id
        assert mb_base.get_mb_source_generation() == generation
        assert mb_base.brainzmash_rate_limiter.rate == 10.0
        assert mb_base.brainzmash_rate_limiter.capacity == 1

        if source_mode == "brainzmash":
            with pytest.raises(ConfigurationError, match="unavailable"):
                mb_base.get_mb_http_client()
            assert mb_base.get_mb_brainzmash_http_client() is brainzmash_client
        else:
            assert mb_base.get_mb_http_client() is official_client
    finally:
        repo_providers.get_musicbrainz_repository.cache_clear()
        mb_base._http_client = before_official_client
        mb_base._brainzmash_http_client = before_brainzmash_client
        mb_base.set_mb_api_base(
            before.source_url,
            source_mode=before.source_mode,
            source_id=before.source_id,
            generation=before.generation,
        )
        mb_base.set_mb_rate_limiter_bypass(before_bypass)
        mb_base.mb_rate_limiter.update_rate(before_rate)
        mb_base.mb_rate_limiter.update_capacity(before_capacity)
        mb_base.brainzmash_rate_limiter.update_rate(before_brainzmash_rate)
        mb_base.brainzmash_rate_limiter.update_capacity(before_brainzmash_capacity)
