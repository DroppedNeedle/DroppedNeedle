from pathlib import Path

from models.audio import AudioInfo, AudioTag
from models.download_manifest import ExpectedTrack
from services.native.file_processor import _slskd_expected_track

EDITION_RECORDING = "11111111-1111-4111-8111-111111111111"
OTHER_RECORDING = "22222222-2222-4222-8222-222222222222"


def _info(duration: float) -> AudioInfo:
    return AudioInfo(
        duration_seconds=duration,
        bitrate=900,
        sample_rate=44100,
        channels=2,
        file_format="flac",
        file_size_bytes=1000,
    )


def _expected() -> list[ExpectedTrack]:
    return [
        ExpectedTrack(
            track_number=3,
            disc_number=1,
            duration_seconds=200.0,
            recording_mbid=EDITION_RECORDING,
            title="Third Song",
            release_track_mbid="33333333-3333-4333-8333-333333333333",
        )
    ]


def _map(title: str, duration: float, recording: str | None = OTHER_RECORDING):
    tag = AudioTag(
        title=title,
        artist="Artist",
        album="Album",
        track_number=3,
        musicbrainz_recording_id=recording,
    )
    return _slskd_expected_track(
        Path("/downloads/Album/03 - Third Song.flac"),
        tag,
        _info(duration),
        _expected(),
        set(),
    )


def test_a_file_from_another_edition_stays_authoritative_when_only_ids_differ() -> None:
    track, authoritative, conflict = _map("Third Song", 201.5)

    assert conflict is None
    assert authoritative is True
    assert track is not None
    assert track.recording_mbid == EDITION_RECORDING


def test_a_different_recording_with_another_title_is_still_a_conflict() -> None:
    _, authoritative, conflict = _map("Third Song (Live)", 200.0)

    assert authoritative is False
    assert conflict == "recording MBID conflicts with the exact edition"


def test_a_different_recording_with_a_different_length_is_still_a_conflict() -> None:
    _, authoritative, conflict = _map("Third Song", 215.0)

    assert authoritative is False
    assert conflict == "recording MBID conflicts with the exact edition"


def test_a_matching_recording_is_unchanged() -> None:
    _, authoritative, conflict = _map("Third Song", 200.0, recording=EDITION_RECORDING)

    assert conflict is None
    assert authoritative is True
