"""Shared album-coverage matcher (P4/P5, 2026-07-05 wrong-single incident).

Answers "which of the requested release's tracks does the library actually HOLD,
and which held files belong to none of them?" - the completeness gate
(``DownloadOrchestrator._coverage``) and the album page's honest-status
annotation (``AlbumService.annotate_album_coverage``) must agree byte-for-byte
on this, so the matcher lives here and nowhere else.

``tracks`` are MusicBrainz tracklist entries (``models.album.Track`` shape:
``position``/``disc_number``/``title``/``recording_id``/``length`` in ms);
``rows`` are ``library_files`` dicts. Each expected track consumes at most one
row, matched strongest-evidence-first so a wrong file squatting on a track's
POSITION can't shadow the right file sitting elsewhere:

1. recording identity (case-insensitive MBID equality),
2. the positional row, unless it positively disagrees (``row_covers_track``),
3. shifted-numbering rescue: containment-strong title + duration agreement.
"""

from services.native.file_processor import _TAG_TITLE_WEAK, row_covers_track
from services.native.title_match import title_containment_score


def match_rows_to_tracks(
    rows: list[dict], tracks: list  # noqa: ANN001 - Track shape, no import cycle on models
) -> tuple[int, list[dict], list[str]]:
    """``(covered_count, orphan_rows, matched_row_ids)``."""

    def _find_cover(remaining, track, expected_seconds):  # noqa: ANN001
        want_rec = (track.recording_id or "").strip().lower()
        for i, row in remaining:
            row_rec = (row.get("recording_mbid") or "").strip().lower()
            if want_rec and row_rec and row_rec == want_rec:
                return i
        for i, row in remaining:
            if (row.get("disc_number") or 1, row.get("track_number")) == (
                track.disc_number or 1,
                track.position,
            ) and row_covers_track(
                row,
                recording_mbid=track.recording_id,
                title=track.title,
                duration_seconds=expected_seconds,
            ):
                return i
        for i, row in remaining:
            row_title = (row.get("track_title") or "").strip()
            row_dur = row.get("duration_seconds")
            if not (track.title and row_title):
                continue
            if title_containment_score(track.title, row_title) < _TAG_TITLE_WEAK:
                continue
            if expected_seconds and not (
                row_dur and abs(row_dur - expected_seconds) <= max(15.0, 0.10 * expected_seconds)
            ):
                continue
            return i
        return None

    remaining = list(enumerate(rows))
    covered = 0
    matched_ids: list[str] = []
    for track in tracks:
        # MusicBrainz track lengths are MILLISECONDS; library rows store seconds.
        expected_seconds = (track.length / 1000.0) if track.length else None
        hit = _find_cover(remaining, track, expected_seconds)
        if hit is None:
            continue
        covered += 1
        row_id = rows[hit].get("id")
        if row_id:
            matched_ids.append(str(row_id))
        remaining = [(j, r) for j, r in remaining if j != hit]
    return covered, [r for _, r in remaining], matched_ids
