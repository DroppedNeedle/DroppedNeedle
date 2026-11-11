"""Which conflicting tracks an administrator may reassign to a chosen edition."""

from models.identification import (
    CandidateEvidence,
    TrackEvidence,
    track_reassignment_plan,
)


def _supported(index: int) -> TrackEvidence:
    return TrackEvidence(
        local_track_id=f"t{index}",
        classification="supported",
        candidate_track_title=f"Track {index}",
        candidate_disc_number=1,
        candidate_track_position=index,
        recording_mbid=f"rec-{index}",
        release_track_mbid=f"rt-{index}",
    )


def _conflicting(index: int, kinds: list[str] | None = None) -> TrackEvidence:
    return TrackEvidence(
        local_track_id=f"t{index}",
        classification="contradictory",
        evidence_kinds=kinds or ["release_track_mbid_conflict"],
        candidate_track_title=f"Track {index}",
        candidate_disc_number=1,
        candidate_track_position=index,
        recording_mbid=f"rec-{index}",
        release_track_mbid=f"rt-{index}",
    )


def _evidence(tracks: list[TrackEvidence], missing: list[str]) -> CandidateEvidence:
    return CandidateEvidence(
        release_group_mbid="rg",
        release_mbid="release",
        album_title_classification="supported",
        album_artist_classification="supported",
        track_evidence=tracks,
        unmatched_expected_tracks=missing,
        reason_code="CONFLICTING_TRACK_EVIDENCE",
    )


def test_identifier_only_conflicts_that_fill_the_unmatched_slots_are_reassignable() -> (
    None
):
    evidence = _evidence(
        [_supported(1), _supported(2), _conflicting(3)], ["1:3:Track 3"]
    )
    plan = track_reassignment_plan(evidence, 3)
    assert [item.local_track_id for item in plan] == ["t3"]


def test_both_embedded_id_conflict_kinds_are_allowed() -> None:
    evidence = _evidence(
        [
            _supported(1),
            _conflicting(2, ["release_track_mbid_conflict", "recording_mbid_conflict"]),
        ],
        ["1:2:Track 2"],
    )
    assert len(track_reassignment_plan(evidence, 2)) == 1


def test_a_real_conflict_blocks_reassignment() -> None:
    for kinds in (
        ["duration_conflict"],
        ["no_acceptable_candidate_track"],
        ["ambiguous_release_track_identity"],
    ):
        evidence = _evidence([_supported(1), _conflicting(2, kinds)], ["1:2:Track 2"])
        assert track_reassignment_plan(evidence, 2) == []


def test_a_contradicted_album_title_or_artist_blocks_reassignment() -> None:
    for field in ("album_title_classification", "album_artist_classification"):
        evidence = _evidence([_supported(1), _conflicting(2)], ["1:2:Track 2"])
        setattr(evidence, field, "contradictory")
        assert track_reassignment_plan(evidence, 2) == []


def test_an_unmatched_or_unknown_track_blocks_reassignment() -> None:
    unknown = TrackEvidence(local_track_id="t2", classification="unknown")
    evidence = _evidence([_supported(1), unknown], ["1:2:Track 2"])
    assert track_reassignment_plan(evidence, 2) == []


def test_a_genuinely_missing_track_is_never_hidden_by_a_reassignment() -> None:
    evidence = _evidence(
        [_supported(1), _conflicting(2)], ["1:2:Track 2", "1:3:Track 3"]
    )
    assert track_reassignment_plan(evidence, 2) == []


def test_two_tracks_cannot_claim_the_same_edition_track() -> None:
    first, second = _conflicting(2), _conflicting(2)
    second.local_track_id = "t2b"
    evidence = _evidence([_supported(1), first, second], ["1:2:Track 2"])
    assert track_reassignment_plan(evidence, 3) == []


def test_every_local_track_must_be_evaluated() -> None:
    evidence = _evidence([_supported(1), _conflicting(2)], ["1:2:Track 2"])
    assert track_reassignment_plan(evidence, 3) == []


def test_nothing_to_reassign_when_every_track_is_supported() -> None:
    evidence = _evidence([_supported(1), _supported(2)], [])
    assert track_reassignment_plan(evidence, 2) == []
