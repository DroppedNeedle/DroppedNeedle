"""Singleton-chain invalidation for acquisition settings saves.

A download-client settings save must rebuild every singleton that captured the
old client/policy at construction (scorer/matcher/file-processor/orchestrator/
service chain) - see the comments in ``routes/download_client.py``. The chains
were previously inlined in the two routes; they live here so the plugin settings
API (which proxies the same preference sections) reuses the exact same lists.
"""

from __future__ import annotations


def clear_soulseek_chain() -> None:
    """Bust the slskd singleton chain so new download-client settings take effect
    immediately. The orchestrator is built eagerly at startup (resume task), so
    omitting it leaves every download running against the old/empty URL."""
    from core.dependencies import (
        get_album_preflight_scorer,
        get_download_client_repository,
        get_download_orchestrator,
        get_download_service,
        get_file_processor,
        get_newznab_release_scorer,
        get_slskd_client,
        get_slskd_indexer,
        get_slskd_repository,
        get_status_service,
        get_track_matcher,
    )

    for provider in (
        get_slskd_client,
        get_slskd_repository,
        get_slskd_indexer,
        get_download_client_repository,
        get_album_preflight_scorer,
        get_track_matcher,
        get_newznab_release_scorer,
        # FileProcessor + StatusService capture the slskd repo (mount/URL) at construction,
        # so they must be cleared too - else the rebuilt orchestrator reuses a stale one.
        get_file_processor,
        get_status_service,
        get_download_orchestrator,
        get_download_service,
    ):
        provider.cache_clear()


def clear_usenet_chain() -> None:
    """Bust the SABnzbd/policy singleton chain. Both the SABnzbd connection and the
    shared policy feed the scorers, file processor, orchestrator and service - clear
    the whole chain so a save takes effect at once."""
    from core.dependencies import (
        get_album_preflight_scorer,
        get_download_orchestrator,
        get_download_service,
        get_file_processor,
        get_newznab_indexer,
        get_newznab_release_scorer,
        get_sabnzbd_client,
        get_sabnzbd_download_client,
        get_track_matcher,
    )

    for provider in (
        get_sabnzbd_client,
        get_sabnzbd_download_client,
        get_album_preflight_scorer,
        get_track_matcher,
        get_newznab_release_scorer,
        # the indexer derives its search-cache TTL from the policy's auto-retry interval
        get_newznab_indexer,
        get_file_processor,
        get_download_orchestrator,
        get_download_service,
    ):
        provider.cache_clear()
