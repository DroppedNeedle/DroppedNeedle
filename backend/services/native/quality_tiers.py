"""Audio quality tiers for download preferences.

A single linear quality axis: lossless (FLAC/ALAC/...) at the top, then lossy
bitrate bands. The bands are CODEC-AGNOSTIC - MP3, AAC/M4A, OGG and Opus are all
classified by bitrate - because cross-codec bitrate isn't apples-to-apples but is
good enough at this granularity. The download-client quality range
(``quality_min``..``quality_max``) accepts a contiguous band; the scorer/matcher
then prefer the highest available tier ABSOLUTELY. ``flac_mp3_only`` (default on)
restricts acceptable formats to FLAC + MP3.

NOTE: ``TIER_KEYS`` is mirrored in ``DownloadClientConnectionSettings.__post_init__``
(api/v1/schemas/settings.py) for validation - keep them in sync.
"""

import re

from repositories.protocols.download_client import DownloadSearchResult

# Best -> worst. Lossy tiers are bitrate bands any lossy codec maps into.
TIER_KEYS: tuple[str, ...] = ("lossless", "mp3_320", "mp3_256", "mp3_192", "low")
DEFAULT_QUALITY_MIN = "mp3_320"
DEFAULT_QUALITY_MAX = "lossless"

# rank: higher = better (low=0 .. lossless=4)
_RANK = {key: rank for rank, key in enumerate(reversed(TIER_KEYS))}
_LOSSLESS_EXT = {"flac", "alac", "wav", "ape", "wv"}
_FLAC_MP3_EXT = {"flac", "mp3"}
# A Soulseek folder search returns every file in the matched folder, so cover art,
# .cue, .log, .m3u and .nfo arrive alongside the audio. These must never be quality-
# judged (they have no codec/bitrate) nor enqueued as if they were tracks. This is the
# set the library importer can actually import - a mirror of library_manager._AUDIO_SUFFIXES
# (sans dots). Keep it to importable formats only: gating in something we can't import
# (e.g. .ape/.wv) just enqueues a folder that then fails import - the bug this set prevents.
_AUDIO_EXT = {"flac", "mp3", "m4a", "m4b", "mp4", "ogg", "oga", "opus", "wav"}


def is_tier(key: str) -> bool:
    return key in _RANK


def tier_rank(key: str) -> int:
    return _RANK.get(key, 0)


def _ext_from_filename(filename: str) -> str:
    base = re.split(r"[\\/]", filename)[-1]
    stem, dot, ext = base.rpartition(".")
    return ext.lower() if dot and stem else ""


def effective_extension(file: DownloadSearchResult) -> str:
    """slskd's ``extension`` can be empty (C6a); fall back to the filename."""
    return file.extension.lower() if file.extension else _ext_from_filename(file.filename)


def file_tier(file: DownloadSearchResult) -> str:
    ext = effective_extension(file)
    if ext in _LOSSLESS_EXT:
        return "lossless"
    bitrate = file.bitrate or 0
    if bitrate >= 320:
        return "mp3_320"
    if bitrate >= 256:
        return "mp3_256"
    if bitrate >= 192:
        return "mp3_192"
    return "low"


def candidate_tier(files: list[DownloadSearchResult]) -> str:
    """A folder is only as good as its WORST file (the whole folder is downloaded)."""
    if not files:
        return "low"
    return min((file_tier(f) for f in files), key=tier_rank)


def is_flac_or_mp3(file: DownloadSearchResult) -> bool:
    return effective_extension(file) in _FLAC_MP3_EXT


def is_audio(file: DownloadSearchResult) -> bool:
    """True for audio files; False for the art/cue/log/m3u sidecars a Soulseek
    folder search returns alongside the tracks."""
    return effective_extension(file) in _AUDIO_EXT


def in_range(tier_key: str, quality_min: str, quality_max: str) -> bool:
    return tier_rank(quality_min) <= tier_rank(tier_key) <= tier_rank(quality_max)
