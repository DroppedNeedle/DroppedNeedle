"""Position mapping for multi-disc rips: a per-disc-numbered filename ("2-01 Title.flac")
or a "CD2/01 Title.flac" remote directory must claim the disc-2 position, not disc 1.

Regression: every file of the 2xCD release `eddc2843-…` that a peer numbered per disc was
mapped to a disc-1 position, failed the title check, and got quarantined as tag_mismatch.
"""

from pathlib import Path

from models.audio import AudioInfo, AudioTag
from models.download_manifest import ExpectedTrack
from services.native.file_processor import (
    _candidate_disc,
    _disc_from_path,
    _filename_disc_track,
    _FolderCandidate,
    _slskd_expected_track,
)

EXPECTED = [
    ExpectedTrack(track_number=1, disc_number=1, duration_seconds=398.0, title="Requiem for Dying Mothers, Part 1"),
    ExpectedTrack(track_number=2, disc_number=1, duration_seconds=457.0, title="Requiem for Dying Mothers, Part 2"),
    ExpectedTrack(track_number=1, disc_number=2, duration_seconds=409.0, title="Mullholland"),
    ExpectedTrack(track_number=2, disc_number=2, duration_seconds=606.0, title="The Lonely People (Are Getting Lonelier)"),
]


def _tag(title: str, track: int, disc: int = 1) -> AudioTag:
    return AudioTag(title=title, artist="Stars of the Lid", album="The Tired Sounds of Stars of the Lid",
                    track_number=track, disc_number=disc)


INFO = AudioInfo(duration_seconds=409.0, bitrate=900, sample_rate=44100, channels=2,
                 file_format="flac", file_size_bytes=20_000_000, bit_depth=16)


def test_filename_disc_track_reads_a_disc_track_pair():
    assert _filename_disc_track("2-01 Mullholland.flac") == (2, 1)
    assert _filename_disc_track("02-03. Stars of the Lid - Gasfarming.flac") == (2, 3)
    # a single number is NOT a disc ("485 - Paul Simon - Graceland (1986).flac")
    assert _filename_disc_track("485 - Paul Simon - Graceland (1986).flac") == (None, None)
    assert _filename_disc_track("01 - Requiem for Dying Mothers, Part 1.flac") == (None, None)


def test_disc_from_path_reads_cd_directories_in_remote_paths():
    assert _disc_from_path(r"music\Artist\Album\CD2\01 - Mullholland.flac") == 2
    assert _disc_from_path("music/Artist/Album/Disc 3/01 - X.flac") == 3
    assert _disc_from_path("music/Artist/Album/01 - X.flac") is None


def test_per_disc_numbered_filename_maps_to_the_disc_two_position():
    source = Path("/downloads/(2001) The Tired Sounds of Stars of the Lid/2-01 Mullholland.flac")
    track, _authoritative, conflict = _slskd_expected_track(
        source, _tag("Mullholland", track=1), INFO, EXPECTED, set())
    assert (track.disc_number, track.track_number) == (2, 1)
    assert conflict is None


def test_flattened_cd_directory_still_maps_via_the_remote_filename():
    source = Path("/downloads/(2001) The Tired Sounds of Stars of the Lid/01 - Mullholland.flac")
    track, _authoritative, conflict = _slskd_expected_track(
        source, _tag("Mullholland", track=1), INFO, EXPECTED, set(),
        remote_filename=r"music\Stars of the Lid\Album [2001]\CD2\01 - Mullholland.flac")
    assert (track.disc_number, track.track_number) == (2, 1)
    assert conflict is None


def test_explicit_tag_disc_still_wins():
    source = Path("/downloads/Album/1-01 Requiem for Dying Mothers, Pt. 1.flac")
    track, _authoritative, _conflict = _slskd_expected_track(
        source, _tag("Requiem for Dying Mothers, Part 1", track=1, disc=2), INFO, EXPECTED, set())
    assert (track.disc_number, track.track_number) == (2, 1)


def test_single_number_filenames_are_unchanged():
    source = Path("/downloads/Album/01 - Requiem for Dying Mothers, Part 1.flac")
    track, _authoritative, conflict = _slskd_expected_track(
        source, _tag("Requiem for Dying Mothers, Part 1", track=1), INFO, EXPECTED, set())
    assert (track.disc_number, track.track_number) == (1, 1)
    assert conflict is None


def test_folder_candidate_disc_honours_a_disc_track_filename():
    candidate = _FolderCandidate(
        path=Path("/library/Album/2-01 Mullholland.flac"),
        tag=_tag("Mullholland", track=1),
        info=INFO,
    )
    assert _candidate_disc(candidate) == 2
