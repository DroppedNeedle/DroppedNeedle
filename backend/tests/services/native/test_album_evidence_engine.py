import json
from pathlib import Path

import pytest

from models.identification import (
    AlbumCandidate,
    CandidateTrack,
    ExistingAlbumMembership,
    GroupingTrack,
    ProposedLocalAlbum,
)
from services.native.album_evidence_engine import (
    CANDIDATE_MARGIN_FLOOR,
    LARGE_UNKNOWN_LIMIT,
    MATCHER_VERSION,
    ORDINARY_UNKNOWN_LIMIT,
    SUPPORTED_RATIO_FLOOR,
    AlbumEvidenceEngine,
)
from services.native.local_album_grouper import (
    LocalAlbumGrouper,
    assign_album_continuity,
)

FIXTURE = (
    Path(__file__).parents[2] / "fixtures" / "feedback_fixes" / "grouping_golden.json"
)


def _track(
    track_id: str,
    title: str,
    *,
    number: int = 1,
    disc: int = 1,
    duration: float | None = 180,
    recording: str | None = None,
    album: str = "Album",
    artist: str = "Artist",
    compilation: bool = False,
    readable: bool = True,
) -> GroupingTrack:
    return GroupingTrack(
        local_track_id=track_id,
        root_id="root",
        relative_path=f"Artist/Album/{number:02}.flac",
        title=title,
        artist_name=artist,
        album_title=album,
        album_artist_name=artist,
        track_number=number,
        disc_number=disc,
        duration_seconds=duration,
        recording_mbid=recording,
        is_compilation=compilation,
        tags_readable=readable,
    )


def _candidate(
    group: str,
    tracks: list[CandidateTrack],
    *,
    title: str = "Album",
    artist: str = "Artist",
    secondary: list[str] | None = None,
) -> AlbumCandidate:
    return AlbumCandidate(
        release_group_mbid=group,
        release_mbid=f"release-{group}",
        album_title=title,
        album_artist_name=artist,
        tracks=tracks,
        release_type="album",
        secondary_types=secondary or [],
    )


def _candidate_track(
    title: str,
    number: int,
    *,
    duration: float | None = 180,
    recording: str | None = None,
) -> CandidateTrack:
    return CandidateTrack(
        title=title,
        position=number,
        absolute_position=number,
        duration_seconds=duration,
        recording_mbid=recording,
    )


def test_committed_grouping_golden_corpus() -> None:
    cases = json.loads(FIXTURE.read_text())
    grouper = LocalAlbumGrouper()
    for case in cases:
        tracks = [
            GroupingTrack(
                local_track_id=item["id"],
                root_id="root",
                relative_path=item["path"],
                title=item.get("id", ""),
                artist_name=item.get("artist", item.get("album_artist", "")),
                album_title=item.get("album", ""),
                album_artist_name=item.get("album_artist", ""),
                track_number=item.get("number", 0),
                disc_number=item.get("disc", 1),
                is_compilation=item.get("compilation", False),
                tags_readable=item.get("readable", True),
            )
            for item in case["tracks"]
        ]
        actual = sorted(sorted(group.track_ids) for group in grouper.group(tracks))
        assert actual == sorted(case["groups"]), case["name"]


def test_manual_membership_is_restored_before_automatic_grouping() -> None:
    tracks = [
        _track("one", "One"),
        _track("two", "Two"),
    ]
    tracks[0].membership_locked = True
    tracks[0].current_album_id = "manual-a"
    tracks[1].membership_locked = True
    tracks[1].current_album_id = "manual-b"
    groups = LocalAlbumGrouper().group(tracks)
    assert sorted(group.track_ids for group in groups) == [["one"], ["two"]]
    assert {group.reason_code for group in groups} == {"MANUAL_MEMBERSHIP_RESTORED"}


def test_continuity_is_maximum_overlap_one_to_one_for_split_merge_and_ties() -> None:
    existing = [
        ExistingAlbumMembership("old-a", ["1", "2", "3"], created_at=1),
        ExistingAlbumMembership("old-b", ["4", "5"], created_at=2),
    ]
    proposed = [
        ProposedLocalAlbum("new-a", "A", "Artist", ["1", "2", "4"], "test"),
        ProposedLocalAlbum("new-b", "B", "Artist", ["3", "5"], "test"),
        ProposedLocalAlbum("new-c", "C", "Artist", ["6"], "test"),
    ]
    result = {
        item.grouping_key: item for item in assign_album_continuity(existing, proposed)
    }
    assert result["new-a"].retained_album_id == "old-a"
    assert result["new-b"].retained_album_id == "old-b"
    assert result["new-c"].retained_album_id is None

    tie = assign_album_continuity(
        [
            ExistingAlbumMembership("old-a", ["1", "2"], created_at=1),
            ExistingAlbumMembership("old-b", ["1", "2"], created_at=2),
        ],
        [
            ProposedLocalAlbum("a", "A", "Artist", ["1"], "test"),
            ProposedLocalAlbum("b", "B", "Artist", ["2"], "test"),
        ],
    )
    assert [(item.grouping_key, item.retained_album_id) for item in tie] == [
        ("a", "old-a"),
        ("b", "old-b"),
    ]
    assert all(item.continuity_reason_code == "CONTINUITY_TIE_BROKEN" for item in tie)


def test_continuity_handles_ten_thousand_disjoint_flat_groups_sparsely() -> None:
    existing = [
        ExistingAlbumMembership(
            local_album_id=f"old-{index}",
            track_ids=[f"track-{index}"],
            created_at=float(index),
        )
        for index in range(10_000)
    ]
    proposed = [
        ProposedLocalAlbum(
            grouping_key=f"group-{index:05d}",
            title=f"Album {index}",
            album_artist_name="Artist",
            track_ids=[f"track-{index}"],
            reason_code="AMBIGUOUS_FALLBACK_GROUP",
        )
        for index in range(10_000)
    ]

    result = assign_album_continuity(existing, proposed)

    assert len(result) == 10_000
    assert all(group.retained_album_id is not None for group in result)
    assert {group.retained_album_id for group in result} == {
        album.local_album_id for album in existing
    }


def test_renamed_path_with_zero_overlap_retains_no_album_id() -> None:
    [result] = assign_album_continuity(
        [ExistingAlbumMembership("old", ["old-track"], created_at=1)],
        [ProposedLocalAlbum("renamed", "Album", "Artist", ["new-track"], "test")],
    )
    assert result.retained_album_id is None


def test_zero_support_and_forced_bad_assignments_are_never_accepted() -> None:
    engine = AlbumEvidenceEngine()
    local = [_track("one", "Completely Different", duration=400)]
    candidate = _candidate("rg", [_candidate_track("Target", 1, duration=100)])
    decision = engine.decide(local, [candidate])
    assert decision.outcome in ("contradictory", "insufficient_evidence")
    assert decision.selected_candidate_key is None
    assert decision.candidates[0].track_evidence[0].classification == "contradictory"


def test_recording_id_conflict_blocks_even_perfect_fuzzy_metadata() -> None:
    decision = AlbumEvidenceEngine().decide(
        [_track("one", "Same", recording="local-recording")],
        [_candidate("rg", [_candidate_track("Same", 1, recording="other-recording")])],
    )
    assert decision.outcome == "contradictory"
    assert decision.reason_code == "CONFLICTING_TRACK_EVIDENCE"


@pytest.mark.parametrize(
    ("file_count", "unknown_count", "expected"),
    [
        (10, ORDINARY_UNKNOWN_LIMIT, "identified"),
        (10, ORDINARY_UNKNOWN_LIMIT + 1, "insufficient_evidence"),
        (21, LARGE_UNKNOWN_LIMIT, "identified"),
        (21, LARGE_UNKNOWN_LIMIT + 1, "insufficient_evidence"),
    ],
)
def test_unknown_extra_caps_are_exact(
    file_count: int, unknown_count: int, expected: str
) -> None:
    comparable_count = file_count - unknown_count
    local = [
        _track(str(index), f"Track {index}", number=index)
        for index in range(1, comparable_count + 1)
    ] + [
        _track(
            f"unknown-{index}",
            "",
            number=0,
            duration=None,
            album="",
            artist="",
            readable=False,
        )
        for index in range(unknown_count)
    ]
    candidate = _candidate(
        "rg",
        [
            _candidate_track(f"Track {index}", index)
            for index in range(1, comparable_count + 1)
        ],
    )
    assert AlbumEvidenceEngine().decide(local, [candidate]).outcome == expected


def test_partial_holding_can_match_without_fabricating_missing_tracks() -> None:
    local = [_track("one", "One", number=1), _track("two", "Two", number=2)]
    candidate = _candidate(
        "rg",
        [_candidate_track(str(number), number) for number in range(1, 11)],
    )
    candidate.tracks[0].title = "One"
    candidate.tracks[1].title = "Two"
    decision = AlbumEvidenceEngine().decide(local, [candidate])
    assert decision.outcome == "identified"
    assert len(decision.candidates[0].unmatched_expected_tracks) == 8


def test_duplicate_local_files_cannot_claim_one_candidate_track_twice() -> None:
    local = [_track("one", "Same"), _track("two", "Same")]
    decision = AlbumEvidenceEngine().decide(
        local, [_candidate("rg", [_candidate_track("Same", 1)])]
    )
    classes = [item.classification for item in decision.candidates[0].track_evidence]
    assert classes.count("supported") == 1
    assert classes.count("contradictory") == 1
    assert decision.outcome == "contradictory"


def test_equal_safe_candidates_are_ambiguous_at_the_signed_margin() -> None:
    local = [_track("one", "One")]
    candidates = [
        _candidate("a", [_candidate_track("One", 1)]),
        _candidate("b", [_candidate_track("One", 1)]),
    ]
    decision = AlbumEvidenceEngine().decide(local, candidates)
    assert decision.outcome == "ambiguous"
    assert (
        decision.candidates[0].score - decision.candidates[1].score
        < CANDIDATE_MARGIN_FLOOR
    )


def test_studio_protection_and_genuine_compilation_acceptance() -> None:
    normal = [_track("one", "One")]
    unsafe = _candidate("live", [_candidate_track("One", 1)], secondary=["live"])
    assert (
        AlbumEvidenceEngine().decide(normal, [unsafe]).reason_code
        == "UNSAFE_RELEASE_TYPE"
    )

    compilation = [_track("one", "One", compilation=True, artist="Various Artists")]
    genuine = _candidate(
        "compilation",
        [_candidate_track("One", 1)],
        artist="Various Artists",
        secondary=["compilation"],
    )
    assert AlbumEvidenceEngine().decide(compilation, [genuine]).outcome == "identified"


def _guest_suffix_album() -> tuple[list[GroupingTrack], AlbumCandidate]:
    """Nine tracks matching a release exactly, except one local title carries a guest
    credit the release does not use - the Thriller / "The Girl Is Mine (with Paul
    McCartney)" shape."""
    titles = [
        "Wanna Be Startin' Somethin'",
        "Baby Be Mine",
        "The Girl Is Mine",
        "Thriller",
        "Beat It",
        "Billie Jean",
        "Human Nature",
        "P.Y.T. (Pretty Young Thing)",
        "The Lady in My Life",
    ]
    local = [
        _track(
            f"local-{index}",
            title if index != 3 else "The Girl Is Mine (with Paul McCartney)",
            number=index,
        )
        for index, title in enumerate(titles, start=1)
    ]
    candidate = _candidate(
        "rg",
        [
            _candidate_track(title, index)
            for index, title in enumerate(titles, start=1)
        ],
    )
    return local, candidate


def test_one_contradicted_title_does_not_veto_an_otherwise_exact_release() -> None:
    local, candidate = _guest_suffix_album()
    decision = AlbumEvidenceEngine().decide(local, [candidate])
    evidence = decision.candidates[0]
    classes = [item.classification for item in evidence.track_evidence]

    assert classes.count("supported") == 8
    assert classes.count("contradictory") == 1
    assert evidence.album_title_classification == "supported"
    assert evidence.album_artist_classification == "supported"
    assert decision.outcome == "identified"
    assert decision.reason_code == "SUPPORTED"


def test_a_contradicted_track_still_costs_the_candidate_score() -> None:
    """The tolerated mismatch must not score identically to a clean match, or the
    wrong edition can win the ordering in decide()."""
    local, candidate = _guest_suffix_album()
    engine = AlbumEvidenceEngine()
    tolerated = engine.evaluate_candidate(local, candidate)

    clean_local = list(local)
    clean_local[2] = _track("local-3", "The Girl Is Mine", number=3)
    clean = engine.evaluate_candidate(clean_local, candidate)

    assert clean.reason_code == "SUPPORTED"
    assert tolerated.reason_code == "SUPPORTED"
    assert clean.score > tolerated.score


@pytest.mark.parametrize(
    ("track_count", "contradicted", "expected"),
    [
        (10, 2, "identified"),  # 0.80, exactly at the floor
        (10, 3, "contradictory"),  # 0.70, below it
        (5, 1, "identified"),  # 0.80 on a short EP
        (2, 1, "contradictory"),  # a single mismatch out of two proves nothing
    ],
)
def test_supported_ratio_floor_bounds_what_a_mismatch_may_cost(
    track_count: int, contradicted: int, expected: str
) -> None:
    local = [
        _track(
            f"local-{index}",
            f"Track {index}" if index > contradicted else f"Unrelated {index}",
            number=index,
        )
        for index in range(1, track_count + 1)
    ]
    candidate = _candidate(
        "rg",
        [_candidate_track(f"Track {index}", index) for index in range(1, track_count + 1)],
    )
    decision = AlbumEvidenceEngine().decide(local, [candidate])
    supported = sum(
        item.classification == "supported"
        for item in decision.candidates[0].track_evidence
    )
    assert (supported / track_count >= SUPPORTED_RATIO_FLOOR) is (
        expected == "identified"
    )
    assert decision.outcome == expected


def test_a_conflicting_recording_mbid_remains_an_absolute_veto() -> None:
    """A different recording is a different recording, however well the rest lines
    up - the ratio floor must not launder that away."""
    local = [
        _track(f"local-{index}", f"Track {index}", number=index, recording=f"rec-{index}")
        for index in range(1, 11)
    ]
    candidate = _candidate(
        "rg",
        [
            _candidate_track(
                f"Track {index}",
                index,
                recording=f"rec-{index}" if index != 1 else "rec-elsewhere",
            )
            for index in range(1, 11)
        ],
    )
    decision = AlbumEvidenceEngine().decide(local, [candidate])
    blocked = decision.candidates[0].track_evidence[0]

    assert blocked.classification == "contradictory"
    assert "recording_mbid_conflict" in blocked.evidence_kinds
    assert decision.outcome == "contradictory"
    assert decision.reason_code == "CONFLICTING_TRACK_EVIDENCE"


def test_a_wrong_album_title_remains_an_absolute_veto() -> None:
    local = [
        _track(f"local-{index}", f"Track {index}", number=index, album="Greatest Hits")
        for index in range(1, 11)
    ]
    candidate = _candidate(
        "rg",
        [_candidate_track(f"Track {index}", index) for index in range(1, 11)],
        title="Something Else Entirely",
    )
    decision = AlbumEvidenceEngine().decide(local, [candidate])

    assert decision.candidates[0].album_title_classification == "contradictory"
    assert decision.outcome == "contradictory"
    assert decision.reason_code == "CONFLICTING_TRACK_EVIDENCE"


def test_unicode_punctuation_and_duration_grace_are_supported() -> None:
    local = [_track("one", "Beyoncé – Café!", duration=180)]
    candidate = _candidate("rg", [_candidate_track("Beyonce Cafe", 1, duration=190)])
    decision = AlbumEvidenceEngine().decide(local, [candidate])
    assert decision.outcome == "identified"
    assert decision.candidates[0].matcher_version == MATCHER_VERSION
