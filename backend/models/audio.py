"""Audio domain models - the shapes the tagger reads and the scanner consumes.

`AudioTag` is the metadata surface (descriptive + MusicBrainz/Picard identifiers);
`AudioInfo` is the technical stream info; `FingerprintResult` is the outcome of a
Tier-3 AcoustID lookup. All are msgspec structs so they (de)serialise through the
house codec without dataclass/dict ad-hoc handling.
"""

from typing import Literal

import msgspec

from infrastructure.msgspec_fastapi import AppStruct


class AudioArtistCredit(AppStruct):
    """One format-native artist value; no joined scalar is heuristically split."""

    name: str
    credited_name: str | None = None
    sort_name: str | None = None
    musicbrainz_artist_id: str | None = None
    join_phrase: str = ""


class AudioTag(AppStruct):
    """Tag metadata read from (or written to) an audio file.

    The ``musicbrainz_*`` fields use the Picard tag names - see
    ``infrastructure/audio/tagger.py`` for the per-format mapping.
    """

    title: str
    artist: str
    album: str
    track_number: int
    album_artist: str | None = None
    disc_number: int = 1
    year: int | None = None
    genre: str | None = None
    musicbrainz_release_group_id: str | None = None
    musicbrainz_release_id: str | None = None
    musicbrainz_recording_id: str | None = None
    musicbrainz_release_track_id: str | None = None
    musicbrainz_artist_id: str | None = None
    musicbrainz_album_artist_id: str | None = None
    acoustid_id: str | None = None
    compilation: bool = False
    title_sort: str | None = None
    artist_sort: str | None = None
    album_sort: str | None = None
    album_artist_sort: str | None = None
    disc_subtitle: str | None = None
    original_release_date: str | None = None
    replaygain_track_gain: float | None = None
    replaygain_album_gain: float | None = None
    replaygain_track_peak: float | None = None
    replaygain_album_peak: float | None = None
    genres: list[str] = msgspec.field(default_factory=list)
    artists: list[AudioArtistCredit] = msgspec.field(default_factory=list)
    album_artists: list[AudioArtistCredit] = msgspec.field(default_factory=list)
    musicbrainz_artist_ids: list[str] = msgspec.field(default_factory=list)
    musicbrainz_album_artist_ids: list[str] = msgspec.field(default_factory=list)


class AudioInfo(AppStruct):
    """Technical properties of the audio stream / file on disk."""

    duration_seconds: float
    bitrate: int  # kbps
    sample_rate: int  # Hz
    channels: int
    file_format: str  # 'flac' | 'mp3' | 'ogg' | 'opus' | 'm4a'
    file_size_bytes: int
    bit_depth: int | None = None  # None for lossy formats


class FingerprintResult(AppStruct):
    """Outcome of a Tier-3 AcoustID fingerprint lookup.

    ``status`` drives the scanner's decision; the recording fields carry the best
    match only when ``status == 'pass'``. Failures are signalled via ``status``
    (+ ``error``) and never by raising, so fingerprinting fails open:

    - ``pass``     - confident recording match (AcoustID score >= 0.70).
    - ``skip``     - no result, or best score below the confidence floor.
    - ``fail``     - confident audio match but no recording MBID to act on.
    - ``disabled`` - no AcoustID API key configured; fpcalc never ran.
    - ``error``    - fpcalc missing/errored or the AcoustID call failed.
    """

    status: Literal["pass", "fail", "skip", "disabled", "error"]
    score: float | None = None
    recording_id: str | None = None
    title: str | None = None
    artist: str | None = None
    duration: int | None = None
    error: str | None = None
    # Release-group MBIDs the matched recording belongs to (from the AcoustID
    # ``meta=recordings releasegroups`` lookup). Populated only on ``pass``; the
    # download-verify release-group check (D15/B2) compares the requested
    # release group against this set.
    release_group_ids: list[str] = msgspec.field(default_factory=list)
