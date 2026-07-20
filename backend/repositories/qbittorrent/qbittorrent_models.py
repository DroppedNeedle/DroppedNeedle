"""msgspec structs for qBittorrent's Web API v2 (the subset we read)."""

import msgspec


class QbtTorrentInfo(msgspec.Struct, kw_only=True):
    """One row from ``GET /api/v2/torrents/info``. ``content_path`` is the absolute
    path (in qBittorrent's namespace) of the torrent's root file/folder; ``save_path``
    is the category/save dir - the remap-prefix onto DroppedNeedle's mount."""

    hash: str = ""
    name: str = ""
    state: str = ""
    progress: float = 0.0
    size: int = 0
    downloaded: int = 0
    dlspeed: int = 0
    num_seeds: int = 0
    category: str = ""
    tags: str = ""
    content_path: str = ""
    save_path: str = ""


class QbtTorrentFile(msgspec.Struct, kw_only=True):
    """One row from ``GET /api/v2/torrents/files`` - ``name`` is the path relative
    to the save dir."""

    name: str = ""
    size: int = 0
    progress: float = 0.0
