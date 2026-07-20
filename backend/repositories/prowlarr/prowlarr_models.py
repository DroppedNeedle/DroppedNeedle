"""msgspec structs for Prowlarr's v1 JSON API (the subset we read).

Field names follow Prowlarr's camelCase wire format via ``rename``; unknown
fields are ignored (Prowlarr adds fields across releases). Verified against
Prowlarr 2.x ``/api/v1/search`` and ``/api/v1/indexer``.
"""

import msgspec


class ProwlarrSearchResult(msgspec.Struct, kw_only=True, rename="camel"):
    """One release row from ``GET /api/v1/search``. ``protocol`` discriminates
    ``usenet`` vs ``torrent``. ``download_url`` is Prowlarr's proxied grab link
    (works for both protocols); torrents may also carry ``magnet_url``."""

    guid: str = ""
    title: str = ""
    size: int = 0
    indexer_id: int = 0
    indexer: str = ""
    protocol: str = ""
    download_url: str = ""
    magnet_url: str = ""
    info_hash: str = ""
    categories: list["ProwlarrCategory"] = []
    seeders: int | None = None
    leechers: int | None = None
    grabs: int | None = None
    files: int | None = None
    publish_date: str = ""


class ProwlarrCategory(msgspec.Struct, kw_only=True, rename="camel"):
    id: int = 0
    name: str = ""


class ProwlarrIndexerInfo(msgspec.Struct, kw_only=True, rename="camel"):
    """One row from ``GET /api/v1/indexer`` (the Test-connection summary)."""

    id: int = 0
    name: str = ""
    enable: bool = True
    protocol: str = ""


class ProwlarrSystemStatus(msgspec.Struct, kw_only=True, rename="camel"):
    version: str = ""
    app_name: str = ""
