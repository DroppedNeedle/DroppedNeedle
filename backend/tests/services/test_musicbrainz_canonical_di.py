from unittest.mock import MagicMock

import pytest

from services.discover.facade import DiscoverService
from services.personal_mix_service import PersonalMixService


def test_discover_keeps_shared_canonical_store():
    canonical = object()
    common = MagicMock()
    discover = DiscoverService(
        listenbrainz_repo=common,
        jellyfin_repo=common,
        library_repo=common,
        musicbrainz_repo=common,
        preferences_service=common,
        mb_canonical_store=canonical,
    )

    assert discover._mbid_resolution._mb_canonical_store is canonical


def _patch_provider(monkeypatch, name, value):
    monkeypatch.setattr(
        "core.dependencies.service_providers." + name,
        lambda value=value: value,
    )


@pytest.mark.parametrize("builder", ["album", "discover"])
def test_provider_built_resolvers_receive_shared_canonical_store(monkeypatch, builder):
    import core.dependencies.service_providers as providers

    canonical = object()
    values = {
        "get_mb_canonical_store": canonical,
        "get_listenbrainz_repository": MagicMock(),
        "get_jellyfin_repository": MagicMock(),
        "get_musicbrainz_repository": MagicMock(),
        "get_preferences_service": MagicMock(),
        "get_cache": MagicMock(),
        "get_mbid_store": MagicMock(),
        "get_wikidata_repository": MagicMock(),
        "get_lastfm_repository": MagicMock(),
        "get_audiodb_image_service": MagicMock(),
        "get_artist_discovery_service": MagicMock(),
        "get_album_discovery_service": MagicMock(),
        "get_per_user_client_factory": MagicMock(),
        "get_user_listening_prefs_store": MagicMock(),
        "get_follow_service": MagicMock(),
        "get_preview_repository": MagicMock(),
        "get_discovery_snapshot_store": MagicMock(),
        "get_background_workload_gate": MagicMock(),
        "get_coverart_repository": MagicMock(),
    }
    for name, value in values.items():
        _patch_provider(monkeypatch, name, value)

    library_repo = MagicMock()
    library_db = MagicMock()
    if builder == "album":
        service = providers._build_album_discovery_service(library_repo, library_db)
        assert service._mbid._mb_canonical_store is canonical
        return

    service = providers._build_discover_service(
        library_repo,
        library_db,
        MagicMock(),
        artist_discovery=MagicMock(),
        album_discovery=MagicMock(),
        cover_repo=MagicMock(),
        genre_index=MagicMock(),
    )
    assert service._mbid_resolution._mb_canonical_store is canonical
    assert service._radio._mbid._mb_canonical_store is canonical
