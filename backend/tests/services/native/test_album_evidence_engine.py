import json
from pathlib import Path

import pytest

from models.identification import (
    AlbumCandidate,
    CandidateTrack,
    ExistingAlbumMembership,
    GroupingTrack,
    ProposedLocalAlbum,
    TrackProvenance,
)
from services.native.album_evidence_engine import (
    CANDIDATE_MARGIN_FLOOR,
    LARGE_UNKNOWN_LIMIT,
    MATCHER_VERSION,
    ORDINARY_UNKNOWN_LIMIT,
    AlbumEvidenceEngine,
    _artist_subset_match,
    _otherwise_supported,
)
from services.native.local_album_grouper import (
    PROVISIONAL_PARSED_GROUP,
    LocalAlbumGrouper,
    _DISC_DIRECTORY,
    assign_album_continuity,
    grouping_directory,
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
    fingerprint_recording: str | None = None,
    release_track: str | None = None,
    release: str | None = None,
    release_group: str | None = None,
    album: str = "Album",
    artist: str = "Artist",
    compilation: bool = False,
    readable: bool = True,
    title_provenance: TrackProvenance = "tag",
    album_title_provenance: TrackProvenance = "tag",
    album_artist_provenance: TrackProvenance = "tag",
) -> GroupingTrack:
    return GroupingTrack(
        local_track_id=track_id,
        root_id="root",
        relative_path=f"Artist/Album/{number:02}.flac",
        title=title,
        artist_name=artist,
        album_title=album,
        album_artist_name=artist,
        title_provenance=title_provenance,
        album_title_provenance=album_title_provenance,
        album_artist_provenance=album_artist_provenance,
        track_number=number,
        disc_number=disc,
        duration_seconds=duration,
        recording_mbid=recording,
        fingerprint_recording_mbid=fingerprint_recording,
        release_track_mbid=release_track,
        release_mbid=release,
        release_group_mbid=release_group,
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
    release_track: str | None = None,
) -> CandidateTrack:
    return CandidateTrack(
        title=title,
        position=number,
        absolute_position=number,
        duration_seconds=duration,
        recording_mbid=recording,
        release_track_mbid=release_track,
    )


def test_release_track_match_cannot_hide_recording_conflict() -> None:
    evidence = AlbumEvidenceEngine().evaluate_candidate(
        [_track("one", "One", recording="recording-a", release_track="track-a")],
        _candidate(
            "group",
            [
                _candidate_track(
                    "One", 1, recording="recording-b", release_track="track-a"
                )
            ],
        ),
    )

    assert evidence.reason_code == "CONFLICTING_TRACK_EVIDENCE"
    assert evidence.track_evidence[0].classification == "contradictory"
    assert evidence.track_evidence[0].evidence_kinds == ["recording_mbid_conflict"]


def test_recording_match_cannot_hide_release_track_conflict() -> None:
    evidence = AlbumEvidenceEngine().evaluate_candidate(
        [_track("one", "One", recording="recording-a", release_track="track-a")],
        _candidate(
            "group",
            [
                _candidate_track(
                    "One", 1, recording="recording-a", release_track="track-b"
                )
            ],
        ),
    )

    assert evidence.reason_code == "CONFLICTING_TRACK_EVIDENCE"
    assert evidence.track_evidence[0].classification == "contradictory"
    assert evidence.track_evidence[0].evidence_kinds == ["release_track_mbid_conflict"]


def _golden_cases() -> list[dict]:
    return json.loads(FIXTURE.read_text())


def _golden_case(name: str) -> dict:
    for case in _golden_cases():
        if case["name"] == name:
            return case
    raise AssertionError(f"unknown golden case: {name}")


def _golden_tracks(case: dict) -> list[GroupingTrack]:
    tracks = []
    for item in case["tracks"]:
        # M-05: optional parsed_* fields simulate grouping_track_from_row
        # output for untagged-but-organized files (parsed display values +
        # parsed provenance; the pinned values mirror the 1.5 chain test).
        # Cases without them keep bare untagged rows exactly as before.
        if "parsed_album" in item or "parsed_artist" in item:
            tracks.append(
                GroupingTrack(
                    local_track_id=item["id"],
                    root_id="root",
                    relative_path=item["path"],
                    title=item.get("id", ""),
                    artist_name=item.get("parsed_artist", ""),
                    album_title=item.get("parsed_album", ""),
                    album_artist_name=item.get("parsed_artist", ""),
                    title_provenance="parsed",
                    album_title_provenance="parsed",
                    album_artist_provenance="parsed",
                    track_number=item.get("number", 0),
                    disc_number=item.get("disc", 1),
                    is_compilation=item.get("compilation", False),
                    tags_readable=item.get("readable", True),
                )
            )
            continue
        tracks.append(
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
        )
    return tracks


def _golden_grouped(name: str) -> list[ProposedLocalAlbum]:
    case = _golden_case(name)
    return LocalAlbumGrouper().group(_golden_tracks(case))


def _golden_groups(name: str) -> list[list[str]]:
    case = _golden_case(name)
    actual = sorted(sorted(group.track_ids) for group in _golden_grouped(name))
    assert actual == sorted(case["groups"]), name
    return actual


def test_committed_grouping_golden_corpus() -> None:
    cases = _golden_cases()
    grouper = LocalAlbumGrouper()
    for case in cases:
        if "xfail" in case:
            # Phase-0 failing-first goldens: pinned by the dedicated strict
            # xfail tests below (which carry the red); the fixing step flips
            # the mark there and drops this field to absorb the case here.
            continue
        tracks = _golden_tracks(case)
        actual = sorted(sorted(group.track_ids) for group in grouper.group(tracks))
        assert actual == sorted(case["groups"]), case["name"]


def test_golden_mixed_albumartist_merges_to_one_group() -> None:
    """M-02: one directory + one album fold with distinct non-empty artists
    and distinct track numbers is one group, not one group per artist."""
    _golden_groups("mixed_albumartist_single_group")


def test_golden_missing_albumartist_merges_to_one_group() -> None:
    """M-02: empty-vs-present artist must not partition the album fold."""
    _golden_groups("missing_albumartist_single_group")


def test_golden_same_title_different_artists_stays_split() -> None:
    """M-02 deliberate non-flip (Phase 1 step 1.7 recorded rationale): two
    distinct non-empty album artists claiming the SAME track number under
    one album fold in one folder are two albums sharing the folder, so the
    fold still splits - artist tolerance never overrides a track-number
    collision across distinct artists."""
    _golden_groups("same_title_different_album_artists")


def test_golden_artist_variance_unknown_numbers_merge_to_one_group() -> None:
    """M-02 follow-up: track number 0 is unknown, never a claim - two
    same-fold groups with distinct non-empty artists and all-unknown
    numbers merge; only known-number collisions split (pinned by
    ``test_golden_same_title_different_artists_stays_split``)."""
    groups = _golden_grouped("artist_variance_unknown_numbers_merge")
    assert sorted(sorted(group.track_ids) for group in groups) == [["a1", "a2"]]
    assert [group.reason_code for group in groups] == ["CONSISTENT_ALBUM_TAGS"]


def test_golden_disc_spelling_variants_fold_to_one_group() -> None:
    """M-03: Disc(1)/(CD1)/Volume N leaf dirs fold like CD1/Disc-02."""
    _golden_groups("disc_spelling_variants_fold")


def test_golden_top_level_cd1_folds_with_album_siblings() -> None:
    """M-03: a root-level disc folder groups with root siblings sharing
    album tags instead of standing alone on its directory name."""
    _golden_groups("top_level_cd1_folds_with_album_siblings")


def test_golden_parsed_sharing_untagged_dir_provisional_group() -> None:
    """M-05: an untagged-but-organized dir whose filenames share one parse
    is one provisional group, not per-track fallback.

    The golden rows carry parsed values + parsed provenance (simulating
    grouping_track_from_row output); the merge is anchored only on parsed
    evidence, so the reason is provisional rather than tagged. A
    genuinely-tagged anchor keeps CONSISTENT_ALBUM_TAGS (pinned by the
    corrupt_and_changed_neighbor golden, which stays green)."""
    groups = _golden_grouped("parsed_sharing_untagged_dir_provisional_group")
    assert sorted(sorted(group.track_ids) for group in groups) == [["u1", "u2"]]
    assert [group.reason_code for group in groups] == [PROVISIONAL_PARSED_GROUP]


def test_golden_untagged_number_collision_stays_split() -> None:
    """M-05 negative: strict numbered/no-collision for absent members -
    same-number untagged tracks never merge, pre- and post-fix."""
    _golden_groups("untagged_number_collision_stays_split")


def test_golden_different_albums_in_disc_dirs_stay_split() -> None:
    """M-03 negative: the disc fold requires shared album tags - different
    albums in disc dirs stay split, pre- and post-fix."""
    _golden_groups("different_albums_in_disc_dirs_stay_split")


@pytest.mark.parametrize(
    ("segment", "number"),
    [
        ("CD1", "1"),
        ("cd01", "1"),
        ("Disc-02", "2"),
        ("Disk 1", "1"),
        ("Disc(1)", "1"),
        ("(CD1)", "1"),
        ("(CD 2)", "2"),
        ("Volume 2", "2"),
        ("Volume2", "2"),
        ("Vol 1", "1"),
        ("Vol.1", "1"),
        ("Vol_02", "2"),
        ("vol-3", "3"),
        ("(Vol 1)", "1"),
    ],
)
def test_disc_directory_spellings_fold(segment: str, number: str) -> None:
    match = _DISC_DIRECTORY.match(segment)
    assert match is not None
    assert match.group(1) == number


@pytest.mark.parametrize(
    "segment",
    ["CD01 bonus", "Volume 1 Remastered", "CD", "Volume", "Incoming", "(1)", "1"],
)
def test_disc_directory_rejects_non_full_segment(segment: str) -> None:
    """M-03 edge: the anchors require a whole-segment match, so suffixed
    segments stay per-folder (pinned here, documented at the regex)."""
    assert _DISC_DIRECTORY.match(segment) is None
    assert grouping_directory(f"Artist/Album/{segment}/01.flac") == (
        f"Artist/Album/{segment}"
    )


def test_album_title_named_volume_1_is_not_a_disc_folder() -> None:
    """M-03 edge: the fold is directory-driven - an album literally titled
    "Volume 1" sitting in a normal folder groups on its tags with no disc
    behavior."""
    tracks = [
        GroupingTrack(
            local_track_id=track_id,
            root_id="root",
            relative_path=f"Artist/Collection/{track_id}.flac",
            title=track_id,
            artist_name="Artist",
            album_title="Volume 1",
            album_artist_name="Artist",
            track_number=number,
        )
        for track_id, number in (("v1", 1), ("v2", 2))
    ]
    groups = LocalAlbumGrouper().group(tracks)
    assert sorted(sorted(group.track_ids) for group in groups) == [["v1", "v2"]]
    assert [group.reason_code for group in groups] == ["CONSISTENT_ALBUM_TAGS"]


def test_suffixed_disc_segment_stays_per_folder() -> None:
    """M-03 edge: CD01 bonus does not match the anchored pattern, so its
    tracks keep a per-folder group beside the sibling folder."""
    tracks = [
        GroupingTrack(
            local_track_id="b1",
            root_id="root",
            relative_path="Artist/Box/CD01 bonus/01.flac",
            title="b1",
            artist_name="Artist",
            album_title="Box",
            album_artist_name="Artist",
            track_number=1,
        ),
        GroupingTrack(
            local_track_id="s1",
            root_id="root",
            relative_path="Artist/Box/02.flac",
            title="s1",
            artist_name="Artist",
            album_title="Box",
            album_artist_name="Artist",
            track_number=2,
        ),
    ]
    groups = LocalAlbumGrouper().group(tracks)
    assert sorted(sorted(group.track_ids) for group in groups) == [["b1"], ["s1"]]


def test_golden_soundtrack_tagged_album_is_one_group() -> None:
    """F-03 green lock: grouping treats soundtrack album tags like any
    album tags (no distinct rule; owned by no implementation step)."""
    _golden_groups("soundtrack_tagged_single_group")


def test_golden_soundtrack_compilation_is_one_group() -> None:
    """F-03 green lock: a soundtrack VA compilation groups like any VA
    compilation (owned by no implementation step)."""
    _golden_groups("soundtrack_compilation_single_group")


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


def test_dense_continuity_component_falls_back_to_sparse_single_assignment(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """F-14 small path: a component past CONTINUITY_COMPONENT_EDGE_LIMIT
    uses greedy sparse single assignment, never a dense Hungarian matrix.

    Shared synthetic component with the staged mirror
    (``test_staged_dense_continuity_component_uses_sparse_fallback``): 2
    old x 2 new albums with 4 overlap edges; the cap is pinned to 3 so the
    component is over-cap in both paths and both yield this identical
    mapping."""
    monkeypatch.setattr(
        "services.native.local_album_grouper.CONTINUITY_COMPONENT_EDGE_LIMIT", 3
    )

    def _forbid_dense(cost):
        raise AssertionError("over-cap component must not build a dense matrix")

    monkeypatch.setattr(
        "services.native.local_album_grouper._hungarian_min", _forbid_dense
    )
    existing = [
        ExistingAlbumMembership("old-a", ["1", "2"], created_at=1),
        ExistingAlbumMembership("old-b", ["2", "3"], created_at=2),
    ]
    proposed = [
        ProposedLocalAlbum("new-x", "X", "Artist", ["1", "2"], "test"),
        ProposedLocalAlbum("new-y", "Y", "Artist", ["2", "3"], "test"),
    ]
    result = {
        item.grouping_key: item for item in assign_album_continuity(existing, proposed)
    }
    assert result["new-x"].retained_album_id == "old-a"
    assert result["new-y"].retained_album_id == "old-b"
    assert result["new-x"].continuity_reason_code == "MAXIMUM_TRACK_OVERLAP"
    assert result["new-y"].continuity_reason_code == "MAXIMUM_TRACK_OVERLAP"


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


def _present_limit_boundary_case(
    supported_count: int, unknown_count: int, placeholder_count: int
) -> tuple[list[GroupingTrack], AlbumCandidate]:
    """Present-claim tracks plus abstaining placeholder stems.

    Mirrors ``test_unknown_extra_caps_are_exact`` (tag-provenance empty
    tracks count as present unknowns) with ``placeholder``-provenance
    stems on top, so total files exceed ``ORDINARY_ALBUM_MAX_FILES``
    while present claims stay within it.
    """
    local = [
        _track(str(index), f"Track {index}", number=index)
        for index in range(1, supported_count + 1)
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
    ] + [
        _track(
            f"stem-{index}",
            f"{index:02} - bonus stem",
            number=supported_count + index,
            duration=200,
            title_provenance="placeholder",
            album_title_provenance="placeholder",
            album_artist_provenance="placeholder",
        )
        for index in range(placeholder_count)
    ]
    candidate = _candidate(
        "rg",
        [
            _candidate_track(f"Track {index}", index)
            for index in range(1, supported_count + 1)
        ],
    )
    return local, candidate


@pytest.mark.parametrize(
    ("unknown_count", "expected"),
    [
        (ORDINARY_UNKNOWN_LIMIT, "identified"),
        (ORDINARY_UNKNOWN_LIMIT + 1, "insufficient_evidence"),
    ],
)
def test_unknown_limit_keys_off_present_tracks_not_total_files(
    unknown_count: int, expected: str
) -> None:
    """Placeholder stems must not inflate the unknown allowance: 18
    supported + unknowns present with 4 abstaining stems (>20 total files,
    <=20 present) uses the ordinary limit, so 2 present unknowns exceed it
    while 1 still identifies."""
    local, candidate = _present_limit_boundary_case(18, unknown_count, 4)
    assert len(local) > 20
    decision = AlbumEvidenceEngine().decide(local, [candidate])
    assert decision.outcome == expected
    if expected == "insufficient_evidence":
        assert decision.reason_code == "UNKNOWN_EXTRAS_EXCEED_LIMIT"
        assert decision.candidates[0].reason_code == "UNKNOWN_EXTRAS_EXCEED_LIMIT"


def test_otherwise_supported_uses_present_count_for_unknown_limit() -> None:
    """The tier helper keys the same switch off present claims: 2 present
    unknowns with abstaining stems past 20 total files is not otherwise
    supported."""
    local, candidate = _present_limit_boundary_case(
        18, ORDINARY_UNKNOWN_LIMIT + 1, 4
    )
    evidence = AlbumEvidenceEngine().evaluate_candidate(local, candidate)
    assert evidence.reason_code == "UNKNOWN_EXTRAS_EXCEED_LIMIT"
    assert not _otherwise_supported(local, evidence)


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
    # D-238: the second copy is a soft descriptive miss at 1/2 support, so the
    # candidate is rejected as INSUFFICIENT_METADATA (not identified), not via
    # the provider-conflict veto. Double-claim protection is unchanged.
    assert decision.outcome == "insufficient_evidence"
    assert decision.reason_code == "INSUFFICIENT_METADATA"


def test_duplicate_recording_position_and_absolute_overlap_is_ambiguous() -> None:
    local = [_track("one", "Repeated", number=10, disc=2, recording="recording")]
    candidate = _candidate(
        "rg",
        [
            CandidateTrack(
                title="Repeated",
                position=1,
                disc_number=2,
                absolute_position=10,
                recording_mbid="recording",
                release_track_mbid="release-track-a",
            ),
            CandidateTrack(
                title="Repeated",
                position=10,
                disc_number=2,
                absolute_position=19,
                recording_mbid="recording",
                release_track_mbid="release-track-b",
            ),
        ],
    )

    decision = AlbumEvidenceEngine().decide(local, [candidate])

    assert decision.outcome == "contradictory"
    assert decision.candidates[0].track_evidence[0].evidence_kinds == [
        "ambiguous_release_track_identity"
    ]


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


def test_release_type_confirmation_and_genuine_compilation_acceptance() -> None:
    normal = [_track("one", "One")]
    unsafe = _candidate("live", [_candidate_track("One", 1)], secondary=["live"])
    assert (
        AlbumEvidenceEngine().decide(normal, [unsafe]).reason_code
        == "RELEASE_TYPE_REQUIRES_CONFIRMATION"
    )

    compilation = [_track("one", "One", compilation=True, artist="Various Artists")]
    genuine = _candidate(
        "compilation",
        [_candidate_track("One", 1)],
        artist="Various Artists",
        secondary=["compilation"],
    )
    # Step 2.6 (N-01): the genuine compilation still clears the release-type
    # gate at evidence level (SUPPORTED), but one present track with no
    # provider proof is below the lone-eligible quorum at decide level.
    genuine_decision = AlbumEvidenceEngine().decide(compilation, [genuine])
    assert genuine_decision.candidates[0].reason_code == "SUPPORTED"
    assert genuine_decision.outcome == "insufficient_evidence"


def test_compilation_flag_cannot_make_a_live_release_safe() -> None:
    tagged_compilation = [_track("one", "One", compilation=True)]
    live = _candidate("live", [_candidate_track("One", 1)], secondary=["live"])

    assert AlbumEvidenceEngine().decide(tagged_compilation, [live]).reason_code == (
        "RELEASE_TYPE_REQUIRES_CONFIRMATION"
    )


@pytest.mark.parametrize("secondary_type", ["compilation", "live"])
def test_complete_release_track_ids_prove_an_exact_special_release(
    secondary_type: str,
) -> None:
    local = [
        _track(
            "one",
            "One",
            recording="recording-1",
            release_track="release-track-1",
        ),
        _track(
            "two",
            "Two",
            number=2,
            recording="recording-2",
            release_track="release-track-2",
        ),
    ]
    candidate = _candidate(
        secondary_type,
        [
            _candidate_track(
                "One", 1, recording="recording-1", release_track="release-track-1"
            ),
            _candidate_track(
                "Two", 2, recording="recording-2", release_track="release-track-2"
            ),
        ],
        secondary=[secondary_type],
    )

    assert AlbumEvidenceEngine().decide(local, [candidate]).outcome == "identified"


def test_incomplete_release_track_ids_do_not_bypass_release_type_confirmation() -> None:
    local = [
        _track(
            "one",
            "One",
            recording="recording-1",
            release_track="release-track-1",
        ),
        _track("two", "Two", number=2, recording="recording-2"),
    ]
    candidate = _candidate(
        "compilation",
        [
            _candidate_track(
                "One", 1, recording="recording-1", release_track="release-track-1"
            ),
            _candidate_track(
                "Two", 2, recording="recording-2", release_track="release-track-2"
            ),
        ],
        secondary=["compilation"],
    )

    assert AlbumEvidenceEngine().decide(local, [candidate]).reason_code == (
        "RELEASE_TYPE_REQUIRES_CONFIRMATION"
    )


def test_unicode_punctuation_and_duration_grace_are_supported() -> None:
    local = [_track("one", "Beyoncé – Café!", duration=180)]
    candidate = _candidate("rg", [_candidate_track("Beyonce Cafe", 1, duration=190)])
    decision = AlbumEvidenceEngine().decide(local, [candidate])
    # Step 2.6 (N-01): the fold + grace still support the pair at evidence
    # level, but one present track with no provider proof is below the
    # lone-eligible quorum at decide level.
    assert decision.candidates[0].reason_code == "SUPPORTED"
    assert decision.outcome == "insufficient_evidence"
    assert decision.candidates[0].matcher_version == MATCHER_VERSION


def test_administrator_exact_release_can_map_title_variants_by_position_and_duration() -> (
    None
):
    engine = AlbumEvidenceEngine()
    local = [
        _track("one", "English title", number=1, duration=180),
        _track("two", "Title (Remastered)", number=2, duration=200),
    ]
    candidate = _candidate(
        "rg",
        [
            _candidate_track(
                "현지 제목",
                1,
                duration=182,
                recording="recording-1",
                release_track="release-track-1",
            ),
            _candidate_track(
                "Title",
                2,
                duration=200.1,
                recording="recording-2",
                release_track="release-track-2",
            ),
        ],
    )
    evidence = engine.evaluate_candidate(local, candidate)
    assert evidence.track_evidence[0].release_track_mbid is None

    assert engine.complete_administrator_exact_release_mapping(
        local, candidate, evidence
    )
    assert evidence.unmatched_expected_tracks == []
    assert [item.recording_mbid for item in evidence.track_evidence] == [
        "recording-1",
        "recording-2",
    ]
    assert [item.release_track_mbid for item in evidence.track_evidence] == [
        "release-track-1",
        "release-track-2",
    ]
    assert all(
        "administrator_exact_release_position_duration" in item.evidence_kinds
        for item in evidence.track_evidence
    )


def test_administrator_exact_release_can_map_flattened_multidisc_by_absolute_position() -> (
    None
):
    engine = AlbumEvidenceEngine()
    local = [
        _track("one", "Local one", number=1, duration=180),
        _track("two", "Local two", number=2, duration=200),
        _track("three", "Local three", number=3, duration=220),
    ]
    candidate = _candidate(
        "rg",
        [
            _candidate_track(
                "Provider one",
                1,
                duration=181,
                recording="recording-1",
                release_track="release-track-1",
            ),
            CandidateTrack(
                title="Provider two",
                position=1,
                disc_number=2,
                absolute_position=2,
                duration_seconds=199,
                recording_mbid="recording-2",
                release_track_mbid="release-track-2",
            ),
            CandidateTrack(
                title="Provider three",
                position=2,
                disc_number=2,
                absolute_position=3,
                duration_seconds=221,
                recording_mbid="recording-3",
                release_track_mbid="release-track-3",
            ),
        ],
    )
    evidence = engine.evaluate_candidate(local, candidate)

    assert engine.complete_administrator_exact_release_mapping(
        local, candidate, evidence
    )
    assert evidence.unmatched_expected_tracks == []
    assert [item.release_track_mbid for item in evidence.track_evidence] == [
        "release-track-1",
        "release-track-2",
        "release-track-3",
    ]
    assert all(
        "administrator_exact_release_absolute_position_duration" in item.evidence_kinds
        for item in evidence.track_evidence
    )


@pytest.mark.parametrize(
    "invalid_case",
    [
        "non_flat_local_discs",
        "duplicate_local_position",
        "missing_provider_absolute_position",
        "duplicate_provider_absolute_position",
    ],
)
def test_administrator_exact_release_flattened_multidisc_mapping_fails_closed(
    invalid_case: str,
) -> None:
    engine = AlbumEvidenceEngine()
    local = [
        _track("one", "Local one", number=1, duration=180),
        _track("two", "Local two", number=2, duration=200),
    ]
    candidate_tracks = [
        _candidate_track(
            "Provider one",
            1,
            duration=180,
            recording="recording-1",
            release_track="release-track-1",
        ),
        CandidateTrack(
            title="Provider two",
            position=1,
            disc_number=2,
            absolute_position=2,
            duration_seconds=200,
            recording_mbid="recording-2",
            release_track_mbid="release-track-2",
        ),
    ]
    if invalid_case == "non_flat_local_discs":
        local[1].disc_number = 2
    elif invalid_case == "duplicate_local_position":
        local[1].track_number = 1
    elif invalid_case == "missing_provider_absolute_position":
        candidate_tracks[1].absolute_position = 0
    else:
        candidate_tracks[1].absolute_position = 1
    candidate = _candidate("rg", candidate_tracks)
    evidence = engine.evaluate_candidate(local, candidate)

    assert not engine.complete_administrator_exact_release_mapping(
        local, candidate, evidence
    )


@pytest.mark.parametrize(
    "invalid_case",
    [
        "missing_duration",
        "duration_conflict",
        "duplicate_position",
        "count_mismatch",
        "missing_provider_identity",
    ],
)
def test_administrator_exact_release_position_mapping_fails_closed(
    invalid_case: str,
) -> None:
    engine = AlbumEvidenceEngine()
    local = [
        _track("one", "Local one", number=1, duration=180),
        _track("two", "Local two", number=2, duration=200),
    ]
    candidate_tracks = [
        _candidate_track(
            "Provider one",
            1,
            duration=180,
            recording="recording-1",
            release_track="release-track-1",
        ),
        _candidate_track(
            "Provider two",
            2,
            duration=200,
            recording="recording-2",
            release_track="release-track-2",
        ),
    ]
    if invalid_case == "missing_duration":
        candidate_tracks[0].duration_seconds = None
    elif invalid_case == "duration_conflict":
        candidate_tracks[0].duration_seconds = 191
    elif invalid_case == "duplicate_position":
        candidate_tracks[1].position = 1
    elif invalid_case == "count_mismatch":
        candidate_tracks.pop()
    else:
        candidate_tracks[0].release_track_mbid = None
    candidate = _candidate("rg", candidate_tracks)
    evidence = engine.evaluate_candidate(local, candidate)

    assert not engine.complete_administrator_exact_release_mapping(
        local, candidate, evidence
    )


def _suffixed_pair(
    suffix_title: str,
    *,
    local_title: str = "The Dark Side of the Moon",
) -> tuple[list[GroupingTrack], AlbumCandidate]:
    """Complete position+duration evidence for every track; only the release
    title carries an edition qualifier."""
    local = [
        _track(
            "one",
            "Speak to Me",
            number=1,
            duration=180,
            album=local_title,
        ),
        _track(
            "two",
            "Breathe",
            number=2,
            duration=200,
            album=local_title,
        ),
    ]
    candidate = _candidate(
        "rg-suffix",
        [
            _candidate_track("Speak to Me", 1, duration=182),
            _candidate_track("Breathe", 2, duration=201),
        ],
        title=suffix_title,
    )
    return local, candidate


@pytest.mark.parametrize(
    ("suffix_title",),
    [
        ("The Dark Side of the Moon (2011 - Remaster)",),
        ("The Dark Side of the Moon (Deluxe Edition)",),
        ("The Dark Side of the Moon [20th Anniversary Edition]",),
        ("Thé Därk Side of the Moon - Remastered",),
    ],
)
def test_edition_suffixes_do_not_contradict_complete_track_evidence(
    suffix_title: str,
) -> None:
    """F-MATCH-01: a provider-only edition qualifier must not fail the album
    title gate when supported track evidence is otherwise complete."""
    engine = AlbumEvidenceEngine()
    local, candidate = _suffixed_pair(suffix_title)

    evidence = engine.evaluate_candidate(local, candidate)

    assert evidence.album_title_classification == "supported"
    assert evidence.reason_code == "SUPPORTED"


def test_real_base_title_difference_remains_contradictory() -> None:
    """Negative control: stripping suffixes never rescues a different album."""
    engine = AlbumEvidenceEngine()
    local, candidate = _suffixed_pair(
        "A Completely Different Record (2011 - Remaster)"
    )

    evidence = engine.evaluate_candidate(local, candidate)

    assert evidence.album_title_classification == "contradictory"
    assert evidence.reason_code == "CONFLICTING_TRACK_EVIDENCE"


def test_artist_suffix_is_not_stripped_by_the_album_title_gate() -> None:
    """The shared helper applies to album titles only: an artist-name qualifier
    still fails the artist gate (no broad fuzzy normalization)."""
    from services.native.album_evidence_engine import (
        _album_metadata_class,
        _album_title_class,
    )

    assert _album_title_class(
        "The Wall", "The Wall (Special Edition)"
    ) == "supported"
    # Artist comparison keeps its raw gate - a qualified artist name that is
    # genuinely different stays contradictory.
    assert (
        _album_metadata_class("Pink Floyd", "Pink Floyd Tribute Band")
        == "contradictory"
    )


@pytest.mark.parametrize(
    ("local_title", "candidate_title"),
    [
        ("Abbey Road", "Abbey Road (Remastered)"),
        ("ABBEY ROAD", "abbey road (deluxe)"),
        ("Abbey Road", "Abbey Road - 20th Anniversary Edition"),
    ],
)
def test_suffix_normalization_is_symmetric_across_case_and_punctuation(
    local_title: str,
    candidate_title: str,
) -> None:
    """Both operands normalize identically regardless of which side carries
    the suffix, brackets, case, or punctuation variant."""
    from services.native.album_evidence_engine import _album_title_class

    assert (
        _album_title_class(local_title, candidate_title)
        == _album_title_class(candidate_title, local_title)
        == "supported"
    )


def _thriller_case() -> tuple[list[GroupingTrack], AlbumCandidate]:
    """D-238 repro: 9-track album, one guest-suffix title miss, rest exact."""
    local_titles = [
        "Wanna Be Startin Somethin",
        "Baby Be Mine",
        "The Girl Is Mine (with Paul Mccartney)",
        "Thriller",
        "Beat It",
        "Billie Jean",
        "Human Nature",
        "P.Y.T. (Pretty Young Thing)",
        "The Lady In My Life",
    ]
    candidate_titles = [
        "Wanna Be Startin Somethin",
        "Baby Be Mine",
        "The Girl Is Mine",
        "Thriller",
        "Beat It",
        "Billie Jean",
        "Human Nature",
        "P.Y.T. (Pretty Young Thing)",
        "The Lady In My Life",
    ]
    local = [
        _track(
            f"track-{index}",
            title,
            number=index + 1,
            duration=200,
            album="Thriller",
            artist="Michael Jackson",
        )
        for index, title in enumerate(local_titles)
    ]
    candidate = _candidate(
        "thriller-rg",
        [
            _candidate_track(title, index + 1, duration=200)
            for index, title in enumerate(candidate_titles)
        ],
        title="Thriller",
        artist="Michael Jackson",
    )
    return local, candidate


def test_single_soft_track_miss_does_not_veto_high_support_album() -> None:
    """D-238: 8/9 supported with one descriptive miss still identifies; the
    miss stays contradictory in track evidence (per-track exception: only
    supported tracks persist identities downstream)."""
    local, candidate = _thriller_case()
    decision = AlbumEvidenceEngine().decide(local, [candidate])
    assert decision.outcome == "identified"
    assert decision.reason_code == "SUPPORTED"
    evidence = decision.candidates[0]
    assert evidence.reason_code == "SUPPORTED"
    by_id = {item.local_track_id: item for item in evidence.track_evidence}
    assert by_id["track-2"].classification == "contradictory"
    assert by_id["track-2"].evidence_kinds == ["no_acceptable_candidate_track"]
    assert (
        sum(item.classification == "supported" for item in evidence.track_evidence)
        == 8
    )


def test_single_provider_conflict_still_vetoes_high_support_album() -> None:
    """Provider proof still vetoes: one recording-MBID conflict on an
    otherwise 8/9 album stays contradictory/CONFLICTING_TRACK_EVIDENCE."""
    local = [
        _track(
            f"track-{index}",
            title,
            number=index + 1,
            duration=200,
            album="Thriller",
            artist="Michael Jackson",
            recording="local-recording" if index == 2 else None,
        )
        for index, title in enumerate(
            [
                "Wanna Be Startin Somethin",
                "Baby Be Mine",
                "The Girl Is Mine",
                "Thriller",
                "Beat It",
                "Billie Jean",
                "Human Nature",
                "P.Y.T. (Pretty Young Thing)",
                "The Lady In My Life",
            ]
        )
    ]
    candidate = _candidate(
        "thriller-rg",
        [
            _candidate_track(
                title,
                index + 1,
                duration=200,
                recording="other-recording" if index == 2 else None,
            )
            for index, title in enumerate(
                [
                    "Wanna Be Startin Somethin",
                    "Baby Be Mine",
                    "The Girl Is Mine",
                    "Thriller",
                    "Beat It",
                    "Billie Jean",
                    "Human Nature",
                    "P.Y.T. (Pretty Young Thing)",
                    "The Lady In My Life",
                ]
            )
        ],
        title="Thriller",
        artist="Michael Jackson",
    )
    decision = AlbumEvidenceEngine().decide(local, [candidate])
    assert decision.outcome == "contradictory"
    assert decision.reason_code == "CONFLICTING_TRACK_EVIDENCE"


def test_fingerprint_recording_agreement_pairs_at_zero_cost_without_veto() -> None:
    """#392: an AcoustID recording ID that agrees with the candidate is
    support-only evidence - it pairs at cost 0.0 under its own kind."""
    from services.native.album_evidence_engine import _pair

    local = _track("one", "One", fingerprint_recording="recording-a")
    candidate_track = _candidate_track("One", 1, recording="recording-a")
    pair = _pair(local, candidate_track)
    assert pair.cost == 0.0
    assert not pair.hard_conflict
    assert pair.kinds == ["fingerprint_recording_mbid"]

    evidence = AlbumEvidenceEngine().evaluate_candidate(
        [local], _candidate("group", [candidate_track])
    )
    assert evidence.reason_code == "SUPPORTED"
    assert evidence.track_evidence[0].classification == "supported"
    assert evidence.track_evidence[0].evidence_kinds == [
        "fingerprint_recording_mbid"
    ]


def test_fingerprint_recording_disagreement_falls_back_to_descriptive_evidence() -> (
    None
):
    """#392: an off-release AcoustID recording ID never vetoes - exact title,
    position, and in-grace duration still support the track."""
    evidence = AlbumEvidenceEngine().evaluate_candidate(
        [_track("one", "One", fingerprint_recording="acoustid-other-entity")],
        _candidate(
            "group", [_candidate_track("One", 1, recording="release-recording")]
        ),
    )
    assert evidence.reason_code == "SUPPORTED"
    [item] = evidence.track_evidence
    assert item.classification == "supported"
    assert "recording_mbid_conflict" not in item.evidence_kinds
    assert "normalized_title" in item.evidence_kinds


def test_authoritative_recording_mismatch_still_vetoes_despite_fingerprint_agreement() -> (
    None
):
    """#392: provider proof keeps veto semantics - an authoritative recording
    mismatch vetoes even when the fingerprint ID agrees with the candidate."""
    decision = AlbumEvidenceEngine().decide(
        [
            _track(
                "one",
                "Same",
                recording="local-recording",
                fingerprint_recording="other-recording",
            )
        ],
        [_candidate("rg", [_candidate_track("Same", 1, recording="other-recording")])],
    )
    assert decision.outcome == "contradictory"
    assert decision.reason_code == "CONFLICTING_TRACK_EVIDENCE"


def test_disagreeing_fingerprints_never_degrade_support_across_runs() -> None:
    """#392 accumulation guard: persisting more off-release AcoustID outcomes
    across re-runs must not reduce the supported count."""
    engine = AlbumEvidenceEngine()
    candidate = _candidate(
        "group",
        [
            _candidate_track(
                f"Song {index}",
                index,
                duration=180.0 + index,
                recording=f"release-recording-{index}",
            )
            for index in range(1, 7)
        ],
        title="Compilation",
        artist="Various",
    )

    def local_with(off_release: set[int]) -> list[GroupingTrack]:
        tracks = [
            _track(
                f"local-{index}",
                f"Song {index}",
                number=index,
                duration=180.0 + index,
                album="Compilation",
                artist="Various",
                compilation=True,
            )
            for index in range(1, 7)
        ]
        for index in off_release:
            tracks[index - 1].fingerprint_recording_mbid = f"acoustid-other-{index}"
        return tracks

    for off_release in (set(), {5, 6}, {3, 4, 5, 6}):
        evidence = engine.evaluate_candidate(local_with(off_release), candidate)
        assert evidence.reason_code == "SUPPORTED"
        assert (
            sum(
                item.classification == "supported"
                for item in evidence.track_evidence
            )
            == 6
        )


def test_tribute_artist_mismatch_still_vetoes_despite_supported_titles() -> None:
    """Tribute noise stays rejected: identical track titles under a different
    album artist trip the hard artist gate, never the near-miss path."""
    local, _ = _thriller_case()
    candidate_titles = [
        "Wanna Be Startin Somethin",
        "Baby Be Mine",
        "The Girl Is Mine",
        "Thriller",
        "Beat It",
        "Billie Jean",
        "Human Nature",
        "P.Y.T. (Pretty Young Thing)",
        "The Lady In My Life",
    ]
    tribute = _candidate(
        "tribute-rg",
        [
            _candidate_track(title, index + 1, duration=200)
            for index, title in enumerate(candidate_titles)
        ],
        title="Thriller",
        artist="Tribute Band",
    )
    decision = AlbumEvidenceEngine().decide(local, [tribute])
    assert decision.outcome == "contradictory"
    assert decision.reason_code == "CONFLICTING_TRACK_EVIDENCE"


def _collab_case() -> tuple[list[GroupingTrack], AlbumCandidate]:
    """Single shape mirroring the live "Bad Omens; Poppy" vs "Bad Omens" row:
    every track carries matching release-track + recording MBIDs."""
    local = [
        _track(
            "collab-one",
            "Violence Against Nature",
            number=1,
            duration=200,
            recording="recording-a",
            release_track="track-a",
            album="Violence Against Nature",
            artist="Bad Omens; Poppy",
        )
    ]
    candidate = _candidate(
        "collab-rg",
        [
            _candidate_track(
                "Violence Against Nature",
                1,
                duration=200,
                recording="recording-a",
                release_track="track-a",
            )
        ],
        title="Violence Against Nature",
        artist="Bad Omens",
    )
    return local, candidate


def test_collab_artist_subset_with_mbid_proof_identifies() -> None:
    """Provider proof outranks a collaboration-credit difference: subset
    artist ("Bad Omens; Poppy" vs "Bad Omens") plus full track MBIDs
    identifies instead of vetoing."""
    local, candidate = _collab_case()
    decision = AlbumEvidenceEngine().decide(local, [candidate])
    assert decision.outcome == "identified"
    assert decision.reason_code == "SUPPORTED"
    assert decision.candidates[0].album_artist_classification == "supported"


def test_collab_artist_subset_without_proof_still_vetoes() -> None:
    """The subset escape needs MBID proof: descriptive-only support under a
    subset artist still vetoes."""
    local = [
        _track(
            "collab-one",
            "Violence Against Nature",
            number=1,
            duration=200,
            album="Violence Against Nature",
            artist="Bad Omens; Poppy",
        )
    ]
    candidate = _candidate(
        "collab-rg",
        [_candidate_track("Violence Against Nature", 1, duration=200)],
        title="Violence Against Nature",
        artist="Bad Omens",
    )
    decision = AlbumEvidenceEngine().decide(local, [candidate])
    assert decision.outcome == "contradictory"
    assert decision.reason_code == "CONFLICTING_TRACK_EVIDENCE"


def test_collab_artist_subset_partial_proof_still_vetoes() -> None:
    """One MBID-proven track is not enough: every present track must carry
    proof for the subset escape."""
    local = [
        _track(
            "proof-one",
            "Track One",
            number=1,
            duration=200,
            recording="recording-a",
            release_track="track-a",
            album="Album",
            artist="Alpha; Beta",
        ),
        _track(
            "proof-two",
            "Track Two",
            number=2,
            duration=200,
            album="Album",
            artist="Alpha; Beta",
        ),
    ]
    candidate = _candidate(
        "collab-rg",
        [
            _candidate_track(
                "Track One", 1, duration=200, recording="recording-a", release_track="track-a"
            ),
            _candidate_track("Track Two", 2, duration=200),
        ],
        title="Album",
        artist="Alpha",
    )
    decision = AlbumEvidenceEngine().decide(local, [candidate])
    assert decision.outcome == "contradictory"
    assert decision.reason_code == "CONFLICTING_TRACK_EVIDENCE"


def test_collab_artist_superset_candidate_with_proof_identifies() -> None:
    """Containment works in both directions: a provider-side collaboration
    ("Alpha × Beta") against a single local artist ("Beta") identifies."""
    local = [
        _track(
            "superset-one",
            "Track One",
            number=1,
            duration=200,
            recording="recording-a",
            release_track="track-a",
            album="Album",
            artist="Beta",
        )
    ]
    candidate = _candidate(
        "collab-rg",
        [
            _candidate_track(
                "Track One", 1, duration=200, recording="recording-a", release_track="track-a"
            )
        ],
        title="Album",
        artist="Alpha × Beta",
    )
    decision = AlbumEvidenceEngine().decide(local, [candidate])
    assert decision.outcome == "identified"
    assert decision.reason_code == "SUPPORTED"


def test_disjoint_artist_with_mbid_proof_still_vetoes() -> None:
    """MBID proof never excuses a genuinely different artist: disjoint
    credits veto even when every track matches."""
    local = [
        _track(
            "tribute-one",
            "Track One",
            number=1,
            duration=200,
            recording="recording-a",
            release_track="track-a",
            album="Album",
            artist="Tribute Band",
        )
    ]
    candidate = _candidate(
        "tribute-rg",
        [
            _candidate_track(
                "Track One", 1, duration=200, recording="recording-a", release_track="track-a"
            )
        ],
        title="Album",
        artist="Michael Jackson",
    )
    decision = AlbumEvidenceEngine().decide(local, [candidate])
    assert decision.outcome == "contradictory"
    assert decision.reason_code == "CONFLICTING_TRACK_EVIDENCE"


def test_collab_artist_subset_with_fingerprint_only_still_vetoes() -> None:
    """Fingerprint MBIDs are support-only, never authoritative proof: a
    subset artist agreed only via fingerprint still vetoes."""
    local = [
        _track(
            "fp-one",
            "Track One",
            number=1,
            duration=200,
            fingerprint_recording="recording-a",
            album="Album",
            artist="Alpha; Beta",
        )
    ]
    candidate = _candidate(
        "collab-rg",
        [_candidate_track("Track One", 1, duration=200, recording="recording-a")],
        title="Album",
        artist="Alpha",
    )
    decision = AlbumEvidenceEngine().decide(local, [candidate])
    assert decision.outcome == "contradictory"
    assert decision.reason_code == "CONFLICTING_TRACK_EVIDENCE"


@pytest.mark.parametrize("proof", ["recording", "release_track"])
def test_collab_artist_subset_with_single_kind_proof_identifies(proof: str) -> None:
    """Either MBID kind opens the escape: recording-only and
    release-track-only proof both identify under a subset artist."""
    mbids: dict[str, str | None] = {"recording": None, "release_track": None}
    mbids[proof] = f"{proof}-a"
    local = [
        _track(
            "single-kind-one",
            "Track One",
            number=1,
            duration=200,
            recording=mbids["recording"],
            release_track=mbids["release_track"],
            album="Album",
            artist="Alpha; Beta",
        )
    ]
    candidate = _candidate(
        "collab-rg",
        [
            _candidate_track(
                "Track One",
                1,
                duration=200,
                recording=mbids["recording"],
                release_track=mbids["release_track"],
            )
        ],
        title="Album",
        artist="Alpha",
    )
    decision = AlbumEvidenceEngine().decide(local, [candidate])
    assert decision.outcome == "identified"
    assert decision.reason_code == "SUPPORTED"


def test_collab_artist_subset_with_placeholder_track_identifies() -> None:
    """The proof quorum counts present claims only: an abstaining
    placeholder track neither proves nor blocks the escape."""
    local = [
        _track(
            "present-one",
            "Track One",
            number=1,
            duration=200,
            recording="recording-a",
            release_track="track-a",
            album="Album",
            artist="Alpha; Beta",
        ),
        _track(
            "present-two",
            "Track Two",
            number=2,
            duration=200,
            recording="recording-b",
            release_track="track-b",
            album="Album",
            artist="Alpha; Beta",
        ),
        _track(
            "placeholder-three",
            "",
            number=3,
            duration=None,
            album="Album",
            artist="Alpha; Beta",
            title_provenance="absent",
        ),
    ]
    candidate = _candidate(
        "collab-rg",
        [
            _candidate_track(
                "Track One", 1, duration=200, recording="recording-a", release_track="track-a"
            ),
            _candidate_track(
                "Track Two", 2, duration=200, recording="recording-b", release_track="track-b"
            ),
            _candidate_track("Track Three", 3, duration=200),
        ],
        title="Album",
        artist="Alpha",
    )
    decision = AlbumEvidenceEngine().decide(local, [candidate])
    assert decision.outcome == "identified"
    assert decision.reason_code == "SUPPORTED"


def test_collab_artist_subset_with_hard_conflict_track_still_vetoes() -> None:
    """A provider-proof conflict anywhere blocks the escape, even when the
    remaining tracks are fully proven."""
    local = [
        _track(
            "proof-one",
            "Track One",
            number=1,
            duration=200,
            recording="recording-a",
            release_track="track-a",
            album="Album",
            artist="Alpha; Beta",
        ),
        _track(
            "conflict-two",
            "Track Two",
            number=2,
            duration=200,
            recording="recording-local",
            album="Album",
            artist="Alpha; Beta",
        ),
    ]
    candidate = _candidate(
        "collab-rg",
        [
            _candidate_track(
                "Track One", 1, duration=200, recording="recording-a", release_track="track-a"
            ),
            _candidate_track(
                "Track Two", 2, duration=200, recording="recording-remote"
            ),
        ],
        title="Album",
        artist="Alpha",
    )
    decision = AlbumEvidenceEngine().decide(local, [candidate])
    assert decision.outcome == "contradictory"
    assert decision.reason_code == "CONFLICTING_TRACK_EVIDENCE"


def test_near_artist_without_proof_identifies() -> None:
    """The escape is consulted only after a contradictory base: an artist
    within the 0.20 distance needs no MBID proof at all."""
    local = [
        _track(
            f"near-{index}",
            f"Track {index}",
            number=index,
            duration=200,
            album="Album",
            artist="Bad Omens!",
        )
        for index in (1, 2)
    ]
    candidate = _candidate(
        "near-rg",
        [
            _candidate_track(f"Track {index}", index, duration=200)
            for index in (1, 2)
        ],
        title="Album",
        artist="Bad Omens",
    )
    decision = AlbumEvidenceEngine().decide(local, [candidate])
    assert decision.outcome == "identified"
    assert decision.reason_code == "SUPPORTED"


def test_collab_artist_subset_two_editions_pin_group_not_release() -> None:
    """The escape un-vetoes but the margin still polices editions: two
    same-group editions that both clear the subset rule land on the
    edition-uncertain tier (soft confirm), never a hard review nor a
    forced exact pick."""
    # Same recordings shared across both editions (the realistic shape);
    # local files carry recording MBIDs only, so both editions match by
    # identity and differ only in the artist-credit cost term.
    local = [
        _track(
            f"edition-{index}",
            f"Track {index}",
            number=index,
            duration=200,
            recording=f"recording-{index}",
            album="Album",
            artist="Bad Omens; Poppy",
        )
        for index in (1, 2)
    ]

    def _edition(release: str, artist: str) -> AlbumCandidate:
        return AlbumCandidate(
            release_group_mbid="edition-rg",
            release_mbid=release,
            album_title="Album",
            album_artist_name=artist,
            tracks=[
                CandidateTrack(
                    title=f"Track {index}",
                    position=index,
                    absolute_position=index,
                    disc_number=1,
                    duration_seconds=200,
                    recording_mbid=f"recording-{index}",
                    release_track_mbid=f"{release}-track-{index}",
                )
                for index in (1, 2)
            ],
            release_type="album",
            secondary_types=[],
        )

    decision = AlbumEvidenceEngine().decide(
        local,
        [_edition("release-collab", "Bad Omens & Poppy"), _edition("release-solo", "Bad Omens")],
    )
    assert decision.outcome == "edition_uncertain"
    assert decision.reason_code == "EDITION_UNCERTAIN"
    assert decision.release_group_mbid == "edition-rg"


@pytest.mark.parametrize(
    ("local", "candidate", "expected"),
    [
        ("Bad Omens; Poppy", "Bad Omens", True),
        ("Bad Omens", "Bad Omens; Poppy", True),
        ("BABYMETAL; Poppy", "BABYMETAL", True),
        ("Anthony Green; The High and Driving Band", "Anthony Green", True),
        ("Alpha × Beta", "Beta", True),
        ("Simon & Garfunkel", "Simon and Garfunkel", False),
        ("Lead feat. Guest", "Lead", True),
        ("Lead ft Guest", "Guest", True),
        ("Lead with Guest", "Lead", True),
        ("Lead vs Guest", "Guest", True),
        ("Poppy; Bad Omens", "Bad Omens; Poppy", True),
        ("Tribute Band", "Michael Jackson", False),
        ("Bill Withers", "Bill Wither", False),
        ("Witherspoon", "Spoon", False),
        ("Poppy", "Poppy Seeds", False),
        ("Alpha; Beta", "Beta; Gamma", False),
        ("A; B; C", "B", True),
        ("Lead featuring Guest", "Lead", True),
        ("Lead + Guest", "Lead", True),
        ("Lead / Guest", "Guest", True),
        ("Lead, Guest", "Lead", True),
        ("Lead FT. Guest", "Lead", True),
        ("Lead FEAT Guest", "Guest", True),
        ("Lead vs. Guest", "Lead", True),
        ("", "Bad Omens", False),
        ("Bad Omens", "", False),
    ],
)
def test_artist_subset_match_cases(local: str, candidate: str, expected: bool) -> None:
    assert _artist_subset_match(local, candidate) is expected


# ---------------------------------------------------------------------------
# Phase-0 shared RG fixture (LibraryFindings-All X-04 step 0.3, E-01/E-04
# pre-work): 10-track-US vs 12-track-XW, mirrored in
# tests/repositories/test_edition_policy.py (recall order) and
# tests/services/test_edition_selection.py (display divergence, D2 skip).
# The MBIDs are chosen so bare MBID order (XW first) disagrees with recall
# order (US first); the dates (precise US vs vague XW) agree with recall so
# the signed evidence_key order from step 2.4 converges on the same winner.
# ---------------------------------------------------------------------------

_SHARED_EDITION_RG = "rg-shared-edition-divergence"
_SHARED_EDITION_REL_US_10 = "rel-ffff-us-10"
_SHARED_EDITION_REL_XW_12 = "rel-0000-xw-12"
_SHARED_EDITION_TARGET_TRACKS = 10


def _shared_edition_releases() -> list[dict]:
    return [
        {
            "id": _SHARED_EDITION_REL_US_10,
            "status": "Official",
            "date": "2024-01-31",
            "country": "US",
            "media": [{"track-count": 10}],
        },
        {
            "id": _SHARED_EDITION_REL_XW_12,
            "status": "Official",
            "date": "2024",
            "country": "XW",
            "media": [{"track-count": 12}],
        },
    ]


def _shared_edition_local() -> list[GroupingTrack]:
    titles = [f"Track {index}" for index in range(1, 6)]
    return [
        _track(
            f"shared-{index}",
            title,
            number=index,
            duration=180,
            album="Album",
            artist="Artist",
        )
        for index, title in enumerate(titles, start=1)
    ]


def _shared_edition_candidates() -> list[AlbumCandidate]:
    titles = [f"Track {index}" for index in range(1, 6)]
    tracks = [
        _candidate_track(title, index, duration=180)
        for index, title in enumerate(titles, start=1)
    ]
    return [
        AlbumCandidate(
            release_group_mbid=_SHARED_EDITION_RG,
            release_mbid=_SHARED_EDITION_REL_US_10,
            album_title="Album",
            album_artist_name="Artist",
            tracks=list(tracks),
            release_type="album",
            release_date="2024-01-31",
        ),
        AlbumCandidate(
            release_group_mbid=_SHARED_EDITION_RG,
            release_mbid=_SHARED_EDITION_REL_XW_12,
            album_title="Album",
            album_artist_name="Artist",
            tracks=list(tracks),
            release_type="album",
            release_date="2024",
        ),
    ]


def test_shared_rg_repair_tail_orders_us_before_xw() -> None:
    """0.3 repair-key order lock: the repair lane ranks by the production
    ``evidence_key`` (score -> Official -> date -> XW -> mbid) - at tied
    scores and tied Official status that is the signed tail, so the
    precise US date beats the vague XW year: US-10 before XW-12."""
    from repositories.edition_policy import evidence_key

    releases = _shared_edition_releases()
    tail_order = sorted(
        releases,
        key=lambda release: evidence_key(
            0.9,
            release["status"],
            release["date"],
            release["country"],
            release["id"],
        ),
    )
    assert [release["id"] for release in tail_order] == [
        _SHARED_EDITION_REL_US_10,
        _SHARED_EDITION_REL_XW_12,
    ]


def test_shared_rg_decide_eligible_order_matches_recall() -> None:
    """0.3 recall-vs-decide agreement: identical evidence (equal scores) must
    rank the US-10 edition before XW-12, matching recall proximity order.
    Pre-fix ``decide()`` sorts eligible evidence by bare
    ``(-score, RG, release)`` so the smaller XW MBID wins; step 2.4 migrates
    the sort to the signed evidence key (score, Official, date, XW, MBID),
    where the precise US date wins the status-tied tail."""
    decision = AlbumEvidenceEngine().decide(
        _shared_edition_local(), _shared_edition_candidates()
    )
    # Premise locks: both editions are genuinely eligible with equal scores.
    assert {item.reason_code for item in decision.candidates} == {"SUPPORTED"}
    scores = {item.score for item in decision.candidates}
    assert len(scores) == 1
    assert decision.ranked_edition_keys == [
        f"{_SHARED_EDITION_RG}:{_SHARED_EDITION_REL_US_10}",
        f"{_SHARED_EDITION_RG}:{_SHARED_EDITION_REL_XW_12}",
    ]


# ---------------------------------------------------------------------------
# Phase-0 tagless-organized-dir chain (LibraryFindings-All X-04 step 0.4,
# F-01/M-06/N-01 pre-work): `flac_no_tags.flac`-style rows in an organized
# dir, through `_to_grouping_track` values + provenance (1.5), engine
# scoring (1.6), and the recall -> decide harness reused by both (the "Pink
# Floyd path" anchor for T5 - no separate Pink Floyd fixture exists).
# ---------------------------------------------------------------------------

_LIBRARY_FIXTURES = Path(__file__).parents[2] / "fixtures" / "library"
_TAGLESS_ORGANIZED_DIR = "Pink Floyd/Wish You Were Here"
_TAGLESS_PARSED = {
    "artist": "Pink Floyd",
    "album": "Wish You Were Here",
    "title": "Shine On You Crazy Diamond",
    "track_number": 1,
}


def _tagless_row(index: int = 1, title: str | None = None) -> dict:
    """A `flac_no_tags.flac`-style indexer row: empty tag columns in an
    organized directory. `title=None` selects the parsed track title."""
    return {
        "id": f"tagless-{index}",
        "root_id": "root",
        "relative_path": (
            f"{_TAGLESS_ORGANIZED_DIR}/0{index} - "
            f"{title or _TAGLESS_PARSED['title']}.flac"
        ),
        "title": "",
        "artist_name": "",
        "album_title": "",
        "album_artist_name": "",
        "artist_sort": None,
        "album_artist_sort": None,
        "track_number": 0,
        "disc_number": 1,
        "duration_seconds": 200.0,
        "embedded_recording_mbid": None,
        "recording_mbid": None,
        "embedded_release_mbid": None,
        "embedded_release_group_mbid": None,
        "embedded_release_track_mbid": None,
        "release_track_mbid": None,
        "is_compilation": False,
        "metadata_incomplete": True,
        "membership_locked": False,
        "local_album_id": "tagless-album",
    }


def _tagless_grouping_track(index: int = 1, title: str | None = None) -> GroupingTrack:
    from services.native.album_identification_service import _to_grouping_track

    return _to_grouping_track(_tagless_row(index, title))


def test_tagless_row_grouping_track_carries_parsed_values_and_provenance() -> None:
    """0.4(b-values): `_to_grouping_track` output for tagless rows carries
    the filename-parsed values with `parsed` provenance (M-01 wiring)."""
    from mutagen.flac import FLAC

    audio = FLAC(str(_LIBRARY_FIXTURES / "flac_no_tags.flac"))
    assert (audio.get("title") or [""])[0] == ""
    assert (audio.get("album") or [""])[0] == ""

    from services.native.album_identification_service import _to_grouping_track

    track = _to_grouping_track(_tagless_row())
    assert track.title == _TAGLESS_PARSED["title"]
    assert track.artist_name == _TAGLESS_PARSED["artist"]
    assert track.album_title == _TAGLESS_PARSED["album"]
    assert track.album_artist_name == _TAGLESS_PARSED["artist"]
    assert track.track_number == _TAGLESS_PARSED["track_number"]
    assert track.title_provenance == "parsed"
    assert track.album_title_provenance == "parsed"
    assert track.album_artist_provenance == "parsed"


def test_tagless_rows_score_unknown_neutral_never_contradictory() -> None:
    """0.4(b-scoring, corrected per W2): genuinely-`placeholder` rows abstain
    (`unknown`/neutral) instead of vetoing - contradiction requires two
    present, disagreeing claims. The mixed group below pairs one `parsed`
    track (which decides) with one `placeholder` track (which abstains even
    against a completely different candidate title)."""
    # Step 1.6 correction: 0.4(b-scoring) as authored expected unknown for
    # parsed rows, contradicting W2 parsed-as-weak-evidence + the Pink Floyd
    # Done; this test now pins placeholder-neutrality, parsed support is
    # pinned by the Pink Floyd + tie tests.
    local = [
        _track(
            "parsed-one",
            "Shine On You Crazy Diamond",
            number=1,
            duration=200,
            album="Wish You Were Here",
            artist="Pink Floyd",
            title_provenance="parsed",
            album_title_provenance="parsed",
            album_artist_provenance="parsed",
        ),
        _track(
            "placeholder-one",
            "02 - Shine On You Crazy Diamond",
            number=2,
            duration=200,
            album="Wish You Were Here",
            artist="Pink Floyd",
            title_provenance="placeholder",
            album_title_provenance="placeholder",
            album_artist_provenance="placeholder",
        ),
    ]
    candidate = _candidate(
        "wish-rg",
        [
            _candidate_track("Shine On You Crazy Diamond", 1, duration=200),
            _candidate_track("A Completely Different Title", 2, duration=200),
        ],
        title="Wish You Were Here",
        artist="Pink Floyd",
    )
    evidence = AlbumEvidenceEngine().evaluate_candidate(local, candidate)
    assert [item.classification for item in evidence.track_evidence] == [
        "supported",
        "unknown",
    ]
    assert evidence.track_evidence[1].evidence_kinds == ["incomparable"]
    assert evidence.reason_code == "SUPPORTED"
    assert evidence.reason_code != "CONFLICTING_TRACK_EVIDENCE"


class _PinkFloydProvider:
    """Recall provider for the tagless-organized-dir chain: answers album
    and recording searches only for non-empty artist+title, mirroring the
    production `search_recordings` emptiness guard (empty pre-1.5 rows
    recall nothing on either lane)."""

    def __init__(self, candidate: AlbumCandidate) -> None:
        self._candidate = candidate

    async def search_album_candidate_ids(
        self, artist: str, title: str, limit: int, priority
    ) -> list[str]:
        if not artist.strip() or not title.strip():
            return []
        return [self._candidate.release_group_mbid][:limit]

    async def search_recording_candidate_ids(
        self, artist: str, title: str, limit: int, priority
    ) -> list[str]:
        if not artist.strip() or not title.strip():
            return []
        return [self._candidate.release_group_mbid][:limit]

    async def get_album_candidate(
        self, release_group_mbid: str, target_track_count: int, priority
    ):
        if release_group_mbid != self._candidate.release_group_mbid:
            return None
        return self._candidate

    async def get_album_candidate_editions(
        self,
        release_group_mbid: str,
        target_track_count: int,
        priority,
        **kwargs,
    ):
        candidate = await self.get_album_candidate(
            release_group_mbid, target_track_count, priority
        )
        return [] if candidate is None else [candidate]

    async def get_exact_release_candidate(self, release_mbid: str, priority):
        return None


@pytest.mark.asyncio
async def test_pink_floyd_path_tagless_organized_dir_identifies() -> None:
    """0.4 Pink Floyd path (T5 anchor): the tagless-organized-dir -> recall
    -> decide chain harness reused by steps 1.5/1.6. Nine tagless rows in an
    organized Wish You Were Here dir must identify once parsed values (1.5)
    and neutral placeholder scoring (1.6) land; the 1.6 mark is the later
    flip (this test needs both)."""
    from services.native.album_candidate_service import AlbumCandidateService

    titles = [
        "Shine On You Crazy Diamond",
        "Welcome to the Machine",
        "Have a Cigar",
        "Wish You Were Here",
        "Shine On You Crazy Diamond (Reprise)",
        "Welcome to the Machine (Live)",
        "Have a Cigar (Live)",
        "Wish You Were Here (Live)",
        "Shine On You Crazy Diamond (Demo)",
    ]
    local = [
        _tagless_grouping_track(index, title)
        for index, title in enumerate(titles, start=1)
    ]
    candidate = _candidate(
        "wish-rg",
        [
            _candidate_track(title, index, duration=200)
            for index, title in enumerate(titles, start=1)
        ],
        title="Wish You Were Here",
        artist="Pink Floyd",
    )
    recalled = await AlbumCandidateService(_PinkFloydProvider(candidate)).recall(local)
    decision = AlbumEvidenceEngine().decide(local, recalled)
    assert decision.outcome == "identified"
    assert decision.selected_candidate_key == "wish-rg:release-wish-rg"


# ---------------------------------------------------------------------------
# Phase-1 placeholder/parsed scoring (LibraryFindings-All step 1.6, M-06/T5)
# ---------------------------------------------------------------------------

# Pinned by the 1.6 tuning procedure: start 0.5, kept because the Pink Floyd
# path identifies (exact parses still support) AND tag-vs-parsed ties favor
# tags (minimum-uncertainty floor). Change the engine constant and this test
# together, never one side alone.
_PINNED_PARSED_DAMPENING = 0.5


def test_parsed_dampening_factor_is_pinned_and_ties_favor_tags() -> None:
    """1.6: identical strings score higher from tags than from parses, while
    exact parses still support."""
    from services.native.album_evidence_engine import PARSED_DAMPENING

    assert PARSED_DAMPENING == _PINNED_PARSED_DAMPENING
    candidate = _candidate("group", [_candidate_track("Same Title", 1)])
    tag_evidence = AlbumEvidenceEngine().evaluate_candidate(
        [_track("tag-one", "Same Title")], candidate
    )
    parsed_evidence = AlbumEvidenceEngine().evaluate_candidate(
        [
            _track(
                "parsed-one",
                "Same Title",
                title_provenance="parsed",
                album_title_provenance="parsed",
                album_artist_provenance="parsed",
            )
        ],
        candidate,
    )
    assert tag_evidence.reason_code == "SUPPORTED"
    assert parsed_evidence.reason_code == "SUPPORTED"
    assert parsed_evidence.score < tag_evidence.score


def test_placeholder_tracks_abstain_while_tag_tracks_decide() -> None:
    """1.6: mixed groups - placeholder tracks are `unknown`/neutral and the
    tag tracks carry the quorum; placeholders never contradict."""
    local = [
        _track("tag-one", "Real Title", number=1),
        _track(
            "placeholder-one",
            "02 - Real Title",
            number=2,
            title_provenance="placeholder",
            album_title_provenance="placeholder",
            album_artist_provenance="placeholder",
        ),
    ]
    candidate = _candidate(
        "group",
        [
            _candidate_track("Real Title", 1),
            _candidate_track("Real Title", 2),
        ],
    )
    evidence = AlbumEvidenceEngine().evaluate_candidate(local, candidate)
    assert [item.classification for item in evidence.track_evidence] == [
        "supported",
        "unknown",
    ]
    assert evidence.reason_code == "SUPPORTED"


def test_all_placeholder_group_is_insufficient_never_contradictory() -> None:
    """1.6: an all-placeholder group abstains into `insufficient_evidence`,
    never `contradictory`."""
    local = [
        _track(
            f"placeholder-{index}",
            f"0{index} - Something",
            number=index,
            title_provenance="placeholder",
            album_title_provenance="placeholder",
            album_artist_provenance="placeholder",
        )
        for index in (1, 2)
    ]
    candidate = _candidate(
        "group",
        [
            _candidate_track("Something Else Entirely", 1, duration=200),
            _candidate_track("Another Thing Entirely", 2, duration=200),
        ],
    )
    evidence = AlbumEvidenceEngine().evaluate_candidate(local, candidate)
    assert [item.classification for item in evidence.track_evidence] == [
        "unknown",
        "unknown",
    ]
    assert evidence.reason_code == "INSUFFICIENT_METADATA"


# ---------------------------------------------------------------------------
# Step 2.6 descriptive quorum (LibraryFindings-All N-01/T13): the D-238
# thresholds hold, but computed over present-claim tracks only; a sole
# eligible candidate additionally needs the lone-eligible quorum (>=2
# present-claim supporting tracks or recording/release-track MBID proof).
# ---------------------------------------------------------------------------


def _thriller_with_placeholders() -> tuple[list[GroupingTrack], AlbumCandidate]:
    """D-238 8/9 case plus three abstaining placeholder tracks: the quorum
    must exclude the placeholders from the totals instead of counting them
    as misses."""
    local, candidate = _thriller_case()
    placeholders = [
        _track(
            f"placeholder-{index}",
            f"{index:02} - bonus stem",
            number=index,
            duration=200,
            album="Thriller",
            artist="Michael Jackson",
            title_provenance="placeholder",
            album_title_provenance="placeholder",
            album_artist_provenance="placeholder",
        )
        for index in (10, 11, 12)
    ]
    return [*local, *placeholders], candidate


def test_descriptive_quorum_excludes_placeholders_from_totals() -> None:
    """2.6: 8 present-supported + 1 present soft miss + 3 placeholders still
    identifies - the placeholders abstain instead of dragging the ratio to
    8/12."""
    local, candidate = _thriller_with_placeholders()
    decision = AlbumEvidenceEngine().decide(local, [candidate])
    assert decision.outcome == "identified"
    assert decision.reason_code == "SUPPORTED"
    evidence = decision.candidates[0]
    assert evidence.reason_code == "SUPPORTED"
    by_id = {item.local_track_id: item for item in evidence.track_evidence}
    assert by_id["track-2"].classification == "contradictory"
    assert by_id["track-2"].evidence_kinds == ["no_acceptable_candidate_track"]
    assert [
        by_id[f"placeholder-{index}"].classification for index in (10, 11, 12)
    ] == ["unknown", "unknown", "unknown"]


def test_lone_eligible_single_present_track_is_insufficient() -> None:
    """2.6 lone-eligible: one present-claim supporting track with no MBID
    proof no longer reaches the margin-1.0 default identify."""
    decision = AlbumEvidenceEngine().decide(
        [_track("one", "One")],
        [_candidate("group", [_candidate_track("One", 1)])],
    )
    assert decision.outcome == "insufficient_evidence"
    assert decision.reason_code == "INSUFFICIENT_METADATA"
    assert decision.selected_candidate_key is None
    assert decision.candidates[0].reason_code == "SUPPORTED"


def test_lone_eligible_placeholders_do_not_count_toward_two() -> None:
    """2.6 lone-eligible: one present-supported track plus abstaining
    placeholders is still below quorum - placeholders never contribute."""
    local = [
        _track("one", "One", number=1),
        _track(
            "placeholder-one",
            "02 - One",
            number=2,
            title_provenance="placeholder",
            album_title_provenance="placeholder",
            album_artist_provenance="placeholder",
        ),
    ]
    candidate = _candidate(
        "group",
        [_candidate_track("One", 1), _candidate_track("One", 2)],
    )
    evidence = AlbumEvidenceEngine().evaluate_candidate(local, candidate)
    assert evidence.reason_code == "SUPPORTED"
    decision = AlbumEvidenceEngine().decide(local, [candidate])
    assert decision.outcome == "insufficient_evidence"
    assert decision.reason_code == "INSUFFICIENT_METADATA"
    assert decision.selected_candidate_key is None


def test_lone_eligible_two_present_tracks_identify() -> None:
    """2.6 lone-eligible: two present-claim supporting tracks clear the
    quorum without any provider proof."""
    decision = AlbumEvidenceEngine().decide(
        [_track("one", "One", number=1), _track("two", "Two", number=2)],
        [
            _candidate(
                "group", [_candidate_track("One", 1), _candidate_track("Two", 2)]
            )
        ],
    )
    assert decision.outcome == "identified"
    assert decision.reason_code == "SUPPORTED"


def test_lone_eligible_mbid_proof_identifies_single_track() -> None:
    """2.6 lone-eligible: a single present-supported track with matching
    recording-MBID proof identifies - the proof is the second quorum leg."""
    decision = AlbumEvidenceEngine().decide(
        [_track("one", "One", recording="recording-a")],
        [_candidate("group", [_candidate_track("One", 1, recording="recording-a")])],
    )
    assert decision.outcome == "identified"
    assert decision.reason_code == "SUPPORTED"


def test_lone_quorum_opt_out_serves_human_verified_attachment() -> None:
    """2.6: curator-verified callers (contribution attachment, persisted as
    `manual`) opt out of the automatic lone quorum - the curator supplies
    sufficiency while contradiction detection still fires."""
    local = [_track("one", "One")]
    candidates = [_candidate("group", [_candidate_track("One", 1)])]
    assert (
        AlbumEvidenceEngine()
        .decide(local, candidates, require_lone_quorum=False)
        .outcome
        == "identified"
    )
    assert AlbumEvidenceEngine().decide(local, candidates).outcome == (
        "insufficient_evidence"
    )
    conflicting = [_track("one", "One", recording="recording-a")]
    vetoed = [
        _candidate("group", [_candidate_track("One", 1, recording="recording-b")])
    ]
    assert (
        AlbumEvidenceEngine()
        .decide(conflicting, vetoed, require_lone_quorum=False)
        .outcome
        == "contradictory"
    )


def _unanimous_release_album(
    *,
    third_release: str | None = "release-group",
    third_duration: float | None = 180,
    third_recording: str = "recording-other-edition",
    third_release_track: str = "track-other-edition",
    count: int = 3,
):
    """Files tagged from one edition, except one file whose track IDs come from another."""

    titles = ["One", "Two", "Three", "Four", "Five"][:count]
    local = []
    candidates = []
    for index, title in enumerate(titles, start=1):
        odd = index == count
        local.append(
            _track(
                f"t{index}",
                title,
                number=index,
                duration=third_duration if odd else 180,
                recording=third_recording if odd else f"recording-{index}",
                release_track=third_release_track if odd else f"track-{index}",
                release=(third_release if odd else "release-group"),
                release_group="group",
            )
        )
        candidates.append(
            _candidate_track(
                title, index, recording=f"recording-{index}", release_track=f"track-{index}"
            )
        )
    return local, _candidate("group", candidates)


def test_unanimous_embedded_release_reconciles_a_track_that_only_conflicts_on_ids() -> None:
    local, candidate = _unanimous_release_album()
    evidence = AlbumEvidenceEngine().evaluate_candidate(local, candidate)

    assert evidence.reason_code == "SUPPORTED"
    odd = next(item for item in evidence.track_evidence if item.local_track_id == "t3")
    assert odd.classification == "supported"
    assert odd.evidence_kinds == ["embedded_ids_reconciled"]
    assert odd.recording_mbid == "recording-3"
    assert odd.release_track_mbid == "track-3"
    assert evidence.unmatched_expected_tracks == []


def test_a_reconciled_album_is_identified_by_the_decision() -> None:
    local, candidate = _unanimous_release_album()
    decision = AlbumEvidenceEngine().decide(local, [candidate])

    assert decision.outcome == "identified"
    assert decision.selected_candidate_key == "group:release-group"


def test_no_reconciliation_when_a_file_does_not_name_the_release() -> None:
    local, candidate = _unanimous_release_album(third_release=None)
    evidence = AlbumEvidenceEngine().evaluate_candidate(local, candidate)

    assert evidence.reason_code == "CONFLICTING_TRACK_EVIDENCE"


def test_no_reconciliation_when_the_files_name_a_different_release() -> None:
    local, candidate = _unanimous_release_album(third_release="another-release")
    evidence = AlbumEvidenceEngine().evaluate_candidate(local, candidate)

    assert evidence.reason_code == "CONFLICTING_TRACK_EVIDENCE"


def test_no_reconciliation_when_the_conflict_is_a_duration_mismatch() -> None:
    local, candidate = _unanimous_release_album(third_duration=400)
    evidence = AlbumEvidenceEngine().evaluate_candidate(local, candidate)

    assert evidence.reason_code == "CONFLICTING_TRACK_EVIDENCE"


def test_no_reconciliation_when_the_conflicts_are_not_the_minority() -> None:
    # One supported track out of two: the album-level claim alone is not enough.
    local, candidate = _unanimous_release_album(count=2)
    evidence = AlbumEvidenceEngine().evaluate_candidate(local, candidate)

    assert evidence.reason_code == "CONFLICTING_TRACK_EVIDENCE"

