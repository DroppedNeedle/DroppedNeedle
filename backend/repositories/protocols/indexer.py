"""Pluggable indexer (search) contract - the search half of the split (D2).

Search and download are separate systems for Usenet, so the old fused
``DownloadClientProtocol`` is split: this module owns *search → releases*, and
``download_client.py`` owns *acquire → track → locate*. slskd satisfies the
search side via a thin ``SlskdIndexer`` adapter; Newznab satisfies it natively.

``IndexerResult`` is a tagged union so one protocol return type spans both
sources without forcing a ``Release`` supertype (D7): the orchestrator unwraps
by ``source`` and routes to the matching scorer, leaving the existing
``DownloadSearchResult`` and the existing scorers byte-for-byte unchanged.

Does NOT use ``from __future__ import annotations``: the conformance contract
test compares ``inspect.signature`` (including return annotations) across impls,
so annotations here and in every implementation must be real objects, not
strings, and identical.
"""

from typing import Protocol, runtime_checkable

from infrastructure.msgspec_fastapi import AppStruct
from models.common import ServiceStatus
from repositories.protocols.download_client import DownloadSearchResult


class UsenetRelease(AppStruct):
    """One NZB release from a Newznab indexer (release-granular, unlike the
    per-file ``DownloadSearchResult``).

    Shaped from the verified Lidarr/Prowlarr Newznab parsers (``02-…``): the NZB
    URL is the ``<enclosure>`` url (``<link>`` is ignored); ``size_bytes`` comes
    from the ``size`` attr (enclosure ``length`` fallback); ``grabs``/``files``
    are optional bonus signals (Prowlarr reads them, Lidarr ignores them) - never
    relied on. ``usenet_date`` is a unix timestamp (usenetdate attr → pubDate).
    Phase 1 reconciles this against a real indexer before finalising.
    """

    indexer_id: str
    indexer_name: str
    guid: str
    title: str
    nzb_url: str
    size_bytes: int = 0
    category_ids: list[int] = []
    grabs: int | None = None
    files: int | None = None
    usenet_date: float | None = None
    # Newznab "password" attr: 0/absent = none, non-zero = the NZB is password-protected
    # (SABnzbd can't auto-unpack it), so it's rejected before download (Lidarr/Prowlarr).
    password: int = 0


class TorrentRelease(AppStruct):
    """One torrent release from a Torznab-capable indexer (release-granular, like
    ``UsenetRelease``). Shaped from Prowlarr's ``/api/v1/search`` JSON: a release
    carries a ``download_url`` (a .torrent fetch, possibly proxied through Prowlarr)
    and/or a ``magnet_url``; at least one is required to enqueue. ``seeders`` is the
    health signal (0 seeders = dead, dropped at scoring). ``publish_date`` is a unix
    timestamp. Identity for dedup/quarantine is the same normalised title+size key
    Usenet uses (``models.download_identity.usenet_identity``)."""

    indexer_id: str
    indexer_name: str
    guid: str
    title: str
    download_url: str = ""
    magnet_url: str = ""
    info_hash: str = ""
    size_bytes: int = 0
    category_ids: list[int] = []
    seeders: int | None = None
    leechers: int | None = None
    grabs: int | None = None
    publish_date: float | None = None


class IndexerResult(AppStruct):
    """A single search result, tagged by ``source`` so all pipelines share one
    protocol return type. Exactly one of ``soulseek``/``usenet``/``torrent`` is set."""

    source: str
    soulseek: DownloadSearchResult | None = None
    usenet: UsenetRelease | None = None
    torrent: TorrentRelease | None = None


@runtime_checkable
class IndexerProtocol(Protocol):
    """Pluggable search contract. Implementations: ``SlskdIndexer`` (adapter over
    the slskd download client), ``NewznabIndexer`` (multi-indexer fan-out)."""

    @property
    def indexer_name(self) -> str: ...

    def is_configured(self) -> bool: ...

    async def health_check(self) -> ServiceStatus: ...

    async def search_album(
        self,
        artist_name: str,
        album_title: str,
        year: int | None = None,
        track_count: int | None = None,
        *,
        timeout: float = 30.0,
    ) -> list[IndexerResult]: ...

    async def search_track(
        self,
        artist_name: str,
        track_title: str,
        album_title: str | None = None,
        duration_seconds: int | None = None,
        *,
        timeout: float = 30.0,
    ) -> list[IndexerResult]: ...
