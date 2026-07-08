"""Lidarr wire models (third-party shapes).

Verified against a live Lidarr **3.1.3.4968** (see ``lidarr_import_API_NOTES.md``) plus
Lidarr's committed ``openapi.json`` / ``ArtistResource.cs``. Tolerant defaults so absent
or unknown fields never break decode (CLAUDE.md third-party-shape rule). Decode failures
re-raise as ``LidarrImportError`` in the repository, never as raw msgspec errors.
"""

import msgspec


class LidarrArtistStatistics(msgspec.Struct, rename="camel"):
    # Informational only (whether Lidarr had files); not required for import.
    track_file_count: int = 0


class LidarrArtist(msgspec.Struct, rename="camel"):
    # foreign_artist_id is the MusicBrainz artist MBID and the join key to
    # user_followed_artists.artist_mbid. Lidarr's mbId field is never populated (verified
    # absent live) - we ignore it entirely.
    foreign_artist_id: str = ""
    artist_name: str = ""
    monitored: bool = False
    monitor_new_items: str = "none"  # "none" | "all"
    status: str = ""  # "continuing" | "ended" | ... (imported regardless, A3)
    statistics: LidarrArtistStatistics | None = None


class LidarrSystemStatus(msgspec.Struct, rename="camel"):
    version: str = ""
