"""Two-phase album preflight scorer.

Group candidate files by ``(username, parent_directory)``, score folder
coherence, then rank by identity (coherence + per-file confidence) alone; peer
availability (free slot / queue depth / speed) is a banded tiebreaker that can never buy
acceptance, and ``tier='auto'`` additionally requires the requested artist to be
named somewhere in the folder's remote paths (D2/D3, 2026-07-05 incident).

Below-threshold groups are kept (``tier='rejected'``, top ~50 by score) so the
Review tab's "Show all results anyway" needs no re-search. Quarantined
``(username, filename)`` sources are dropped before scoring. Non-audio sidecars
(cover art, cue, log, m3u) a folder search returns are excluded before judging - a
quality gate then drops folders whose audio is outside ``quality_min``..``quality_max``
or, when ``flac_mp3_only``, contains a non-FLAC/MP3 track. Acceptance tier and identity
band precede quality, so a weak hi-res folder cannot hide a safe standard-lossless match.
CJK strings skip ``unidecode``; off-version matches (remix/live/acoustic vs
original) are penalised x0.3.
"""

import logging
import re
from collections import Counter, defaultdict

import msgspec
from rapidfuzz import fuzz

from infrastructure.persistence.download_store import DownloadStore
from models.acquisition_quality import (
    AcquisitionQualitySnapshot,
    AudioQualityEvidence,
    CodecFamily,
    EvidenceCertainty,
    EvidenceProvenance,
)
from models.download import ScoredCandidate, TargetAlbum
from models.download_identity import (
    canonical_soulseek_identity,
    soulseek_folder_identity,
    soulseek_identity,
)
from models.download_manifest import ExpectedTrack
from models.acquisition_quality import (
    AudioQualityEvidence,
    CodecFamily,
    EvidenceCertainty,
    EvidenceProvenance,
    QualityDecision,
)
from services.native.acquisition import quality as acq_quality
from repositories.protocols.download_client import DownloadSearchResult
from services.native.acquisition import pipeline
from services.native.acquisition.context import build_context
from services.native.acquisition.decision import (
    Accept,
    Candidate,
    Reject,
    RejectCode,
    SpecPolicy,
)
from services.native.acquisition.scoring_core import (
    artist_from_path as _artist_from_path,  # noqa: F401 - re-exported for tests/callers
    artist_words as _artist_words,
    release_prefix_tokens as _release_prefix_tokens,
    file_confidence as _core_file_confidence,
    normalize_folder_identity as _normalize_folder_identity,
    normalize_for_match as _normalize_for_match,
    strip_edition_suffix as _strip_edition_suffix,
    tracklist_overlap as _core_tracklist_overlap,
)
from services.native.acquisition.specs.quarantine import quarantine
from services.native.title_match import (
    artist_evidence,
    names_different_album,
    title_containment_score,
)
from services.native.quality_tiers import (
    DEFAULT_QUALITY_MAX,
    DEFAULT_QUALITY_MIN,
    candidate_tier,
    folder_hires_key,
    is_audio,
    is_flac_or_mp3,
    tier_for,
    tier_rank,
)

_JUNK_KEYWORDS = ("various", "unknown album", "untitled", "misc")
_LEADING_RELEASE_YEAR = re.compile(r"^\s*(?:\[\d{4}\]|\(\d{4}\)|\d{4}\s*[-–]\s*)\s*")

logger = logging.getLogger(__name__)

_ACCEPTANCE_RANK = {"rejected": 0, "manual": 1, "auto": 2}


def _ext_from_filename(filename: str) -> str:
    base = re.split(r"[\\/]", filename)[-1]
    stem, dot, ext = base.rpartition(".")
    return ext.lower() if dot and stem else ""


def _effective_extension(file: DownloadSearchResult) -> str:
    """slskd's ``extension`` can be empty (C6a); fall back to the filename."""
    return (
        file.extension.lower() if file.extension else _ext_from_filename(file.filename)
    )


def _has_artist_evidence(
    target: TargetAlbum, files: list[DownloadSearchResult]
) -> bool:
    return "various" in (target.artist_name or "").lower() or any(
        artist_evidence(target.artist_name, file.filename) for file in files
    )


def _album_identity_text(
    target: TargetAlbum, parent: str, files: list[DownloadSearchResult]
) -> str:
    # A leading release year is folder organisation, not album identity. Leaving it
    # first makes the title parser stop at the year before it can see distinguishing
    # words ("[2025] So Long, Avalon" previously slipped through for "Avalon").
    album_leaf = _LEADING_RELEASE_YEAR.sub("", parent) or parent
    return (
        f"{target.artist_name} - {album_leaf}"
        if _has_artist_evidence(target, files)
        else album_leaf
    )


def _availability_key(candidate: ScoredCandidate) -> tuple[int, int, int, int, int]:
    queue_lengths = [
        file.queue_length for file in candidate.files if file.queue_length is not None
    ]
    complete_queue = max(queue_lengths) if queue_lengths else 2**31 - 1
    return (
        int(
            bool(candidate.files)
            and all(file.has_free_slot for file in candidate.files)
        ),
        int(len(queue_lengths) == len(candidate.files)),
        -complete_queue,
        min((file.upload_speed for file in candidate.files), default=0),
        -sum(file.size for file in candidate.files),
    )


def _file_evidence(file: DownloadSearchResult) -> AudioQualityEvidence:
    """Per-file source-metadata projection with exact axes when fully present."""
    ext = _effective_extension(file)
    tier = tier_for(ext, file.bitrate, file.bit_depth)
    family = (
        CodecFamily.LOSSLESS
        if tier == "lossless"
        else CodecFamily.LOSSY
        if tier in {"low", "mp3_192", "mp3_256", "mp3_320"}
        else CodecFamily.UNKNOWN
    )
    exact = (
        family is CodecFamily.LOSSY and file.bitrate is not None and file.bitrate > 0
    ) or (
        family is CodecFamily.LOSSLESS
        and file.bit_depth is not None
        and file.bit_depth > 0
        and file.sample_rate is not None
        and file.sample_rate > 0
    )
    return AudioQualityEvidence(
        extension=ext,
        codec_family=family,
        bitrate_kbps=file.bitrate,
        bit_depth=file.bit_depth,
        sample_rate_hz=file.sample_rate,
        total_bytes=file.size,
        audio_file_count=1,
        mixed_format=False,
        mixed_quality=False,
        certainty=EvidenceCertainty.EXACT if exact else EvidenceCertainty.PARTIAL,
        provenance=EvidenceProvenance.SOURCE_METADATA,
    )


def _legacy_candidate_rank_key(
    candidate: ScoredCandidate,
    snapshot: AcquisitionQualitySnapshot | None = None,
):
    if snapshot is not None and acq_quality.is_recipe_snapshot(snapshot):
        decision = candidate.quality_decision
        if decision is None and candidate.files:
            decision = acq_quality.evaluate_worst(
                snapshot, [_file_evidence(file) for file in candidate.files]
            )
        step = (
            decision.preference_step
            if decision is not None and decision.preference_step is not None
            else len(snapshot.quality_recipe) + 1
        )
        evidence = decision.evidence if decision is not None else AudioQualityEvidence()
        refinement = acq_quality.recipe_refinement_key(snapshot, evidence)
        return (
            _ACCEPTANCE_RANK.get(candidate.tier, 0),
            -step,
            *(-value for value in refinement),
            *_availability_key(candidate),
            candidate.final_score,
        )
    return (
        _ACCEPTANCE_RANK.get(candidate.tier, 0),
        tier_rank(candidate_tier(candidate.files)),
        *folder_hires_key(candidate.files),
        *_availability_key(candidate),
        candidate.final_score,
    )


def rank_stored_candidates(
    target: TargetAlbum,
    candidates: list[ScoredCandidate],
    snapshot: AcquisitionQualitySnapshot | None = None,
) -> list[ScoredCandidate]:
    """Apply current safety and ranking rules to a read-only review projection.

    The persisted index is retained for the pick endpoint. Older parked reviews
    improve after an upgrade without rewriting jobs or starting downloads.
    """
    by_source: dict[str, list[ScoredCandidate]] = {}
    source_order: list[str] = []
    for original_index, candidate in enumerate(candidates):
        source = candidate.source or "soulseek"
        if source == "soulseek" and names_different_album(
            target.album_title,
            target.artist_name,
            _album_identity_text(target, candidate.parent_directory, candidate.files),
        ):
            continue
        if snapshot is not None and acq_quality.is_recipe_snapshot(snapshot):
            if source == "soulseek" and candidate.files:
                evidence = [_file_evidence(file) for file in candidate.files]
                decision = acq_quality.evaluate_worst(snapshot, evidence)
                if not decision.eligible and acq_quality.is_hard_quality_rejection(
                    decision
                ):
                    continue
                candidate = msgspec.structs.replace(
                    candidate,
                    tier=(
                        "manual"
                        if not decision.eligible and candidate.tier == "auto"
                        else candidate.tier
                    ),
                    quality_evidence=decision.evidence,
                    quality_decision=decision,
                )
        if source not in by_source:
            by_source[source] = []
            source_order.append(source)
        by_source[source].append(
            msgspec.structs.replace(candidate, candidate_index=original_index)
        )

    projected: list[ScoredCandidate] = []
    for source in source_order:
        projected.extend(
            sorted(
                by_source[source],
                key=lambda candidate: _legacy_candidate_rank_key(candidate, snapshot),
                reverse=True,
            )
        )
    return projected


def _file_confidence(
    target_title: str,
    target_artist: str,
    target_duration: float | None,
    file: DownloadSearchResult,
    *,
    strict_title: bool = False,
) -> float:
    """Per-file confidence (shared by the album scorer and the track matcher).

    Thin compat wrapper over ``scoring_core.file_confidence``; the formula lives
    there so plugins score with identical calibration. See its docstring for the
    weighting / strict_title / version-penalty contract.
    """
    return _core_file_confidence(
        target_title,
        target_artist,
        target_duration,
        file.filename,
        file.parent_directory,
        file.duration,
        strict_title=strict_title,
    )


class AlbumPreflightScorer:
    """Quality moves EXCLUSIVELY through the per-call ``AcquisitionQualitySnapshot``
    (spec Snapshot rule). Non-quality spec gates ride ``spec_extras`` supplied
    fresh by the caller so a stale singleton can never hide them."""

    def __init__(self, download_store: DownloadStore):
        self._store = download_store

    async def rank(
        self,
        target: TargetAlbum,
        results: list[DownloadSearchResult],
        *,
        snapshot: AcquisitionQualitySnapshot,
        spec_extras: "SpecPolicy | None" = None,
        auto_accept_threshold: float = 0.70,
        manual_threshold: float = 0.50,
        held_tier: str | None = None,
        expected_tracks: list["ExpectedTrack"] | None = None,
        release_group_mbid: str | None = None,
    ) -> list[ScoredCandidate]:
        context = await build_context(self._store, held_tier=held_tier)
        # Canonical quality endpoints come from the SNAPSHOT; non-quality gates
        # (max size / terms / retention) arrive via spec_extras, fresh per call.
        policy = SpecPolicy(
            quality_min=(
                "low"
                if acq_quality.is_recipe_snapshot(snapshot)
                else snapshot.quality_preference_order[-1]
            ),
            quality_max=(
                "lossless"
                if acq_quality.is_recipe_snapshot(snapshot)
                else snapshot.quality_preference_order[0]
            ),
        )
        if spec_extras is not None:
            import msgspec as _ms

            policy = _ms.structs.replace(
                policy,
                max_size_mb=spec_extras.max_size_mb,
                ignored_terms=tuple(spec_extras.ignored_terms),
                required_terms=tuple(spec_extras.required_terms),
                usenet_retention_days=spec_extras.usenet_retention_days,
                usenet_min_age_minutes=getattr(
                    spec_extras, "usenet_min_age_minutes", 0
                ),
            )
        flac_mp3_only = snapshot.flac_mp3_only
        # Soulseek quarantine is file-granular (a peer may have just one bad file): apply
        # it as a pool pre-filter via the shared spec, so a quarantined file is dropped
        # before grouping while the folder's other files survive.
        filtered = [
            r
            for r in results
            if isinstance(
                quarantine(
                    Candidate(
                        source="soulseek",
                        identity=soulseek_identity(r.username, r.filename),
                    ),
                    target,
                    context,
                    policy,
                ),
                Accept,
            )
        ]

        groups: dict[tuple[str, str], list[DownloadSearchResult]] = defaultdict(list)
        for result in filtered:
            groups[(result.username, result.parent_directory)].append(result)

        # Peer-folder exhaustion (#255 defect 2): a clean import that still
        # under-delivers records a BARE-username quarantine row, which can never
        # match a per-file (username, filename) identity. Consult it here at
        # folder level - the grouping key already is (username, directory) - and
        # drop that peer's folders for this release group. A manual re-request
        # clears album-scoped rows by RG (covering bare rows too), restoring it.
        exhausted = {
            key
            for key in groups
            if ("soulseek", canonical_soulseek_identity(key[0]))
            in context.quarantine_set
        }
        drop_peer_exhausted = len(exhausted)
        for key in exhausted:
            del groups[key]

        # Wrong-product exclusions (Slice 3): folders whose normalized album
        # identity proved content-wrong for THIS release group drop before
        # scoring, however the peer named the folder ("2021. Flux",
        # "Flux (2021)" and "Flux" are one key). RG-scoped by construction
        # (the RG is in the key); without an RG the consult cannot run and
        # every folder survives (manual searches pass none today).
        drop_folder_excluded = 0
        if release_group_mbid:
            folder_artist_words = _artist_words(target.artist_name)
            folder_excluded = {
                key
                for key in groups
                if (
                    "soulseek",
                    soulseek_folder_identity(
                        release_group_mbid,
                        _normalize_folder_identity(
                            key[1], artist_words=folder_artist_words
                        ),
                    ),
                )
                in context.quarantine_set
            }
            drop_folder_excluded = len(folder_excluded)
            for key in folder_excluded:
                del groups[key]

        scored: list[ScoredCandidate] = []
        drop_no_audio = drop_codec = 0
        pipeline_drops: Counter[RejectCode] = Counter()
        # Overlap title judging ignores the artist's own words (same rule as
        # the strict file-confidence path): "Poppy - Kitty" names Kitty.
        overlap_ignore = _artist_words(target.artist_name)
        # ...and sheds an "Artist - Album - " filename lead before judging, so
        # "iamalex - Blue Bird - 02 - Belle" claims position 2 as "Belle"
        # instead of reading the album's words as a foreign title.
        overlap_prefix = _release_prefix_tokens(target.artist_name, target.album_title)
        for (username, parent), files in groups.items():
            # A folder search returns the album's sidecars (cover art, cue, log, m3u)
            # alongside the tracks; gate, score and enqueue on the AUDIO files only -
            # judging a folder by a non-audio file (no codec, no bitrate -> 'low')
            # rejected every well-ripped release, and enqueuing one fails the import.
            audio = [f for f in files if is_audio(f)]
            if not audio:
                drop_no_audio += 1
                continue
            # a folder is rated by its worst audio file (downloaded whole): drop on a
            # disallowed codec before the shared pipeline judges identity + quality range.
            if flac_mp3_only and not all(is_flac_or_mp3(f) for f in audio):
                drop_codec += 1
                continue
            # Positive artist evidence lets the wrong-album spec judge the album-level
            # leaf with trusted artist context. The leaf alone (for example
            # "So Long, Avalon") cannot pass that spec's same-artist guard, while feeding
            # an entire remote path would mistake share-root and track-title words for
            # album identity. This signal also remains the independent auto-accept gate.
            has_evidence = _has_artist_evidence(target, audio)
            album_identity_text = _album_identity_text(target, parent, audio)

            # Shared spec pipeline - the SAME rules as the Usenet path: blocklist (a folder
            # carries no single identity, so it's a no-op here - quarantine was applied
            # per-file above), wrong-edition + wrong-album (a live/boxset or a different
            # album by the same artist scores near-identical under token_set_ratio),
            # sample, ignored/required terms, quality-range, max-size, retention/min-age
            # (Usenet-only, no-op here), free-space. Off-by-default gates short-circuit.
            decision = pipeline.run(
                Candidate(
                    source="soulseek",
                    match_text=parent,
                    album_identity_text=album_identity_text,
                    tier=candidate_tier(audio),
                    size_bytes=sum(f.size for f in audio),
                ),
                target,
                context,
                policy,
            )
            if isinstance(decision, Reject):
                pipeline_drops[decision.code] += 1
                continue

            # For a 1-track release (a single that fell back here because identity
            # threading failed - the normal path scores via the TrackMatcher), a lone
            # spuriously-matched file must not read as a "complete" album: only files
            # whose names actually contain the title count toward count_ratio (P3.3).
            # Deliberately NOT applied to 2+-track releases: their per-file names are
            # TRACK titles, unjudgeable against the album title without a tracklist
            # (a legit B-side would disqualify) - the artist-evidence gate covers the
            # wrong-artist EP class instead.
            qualified_count = None
            if target.track_count == 1:
                artist_words = frozenset(
                    t
                    for t in _normalize_for_match(target.artist_name).split()
                    if len(t) >= 2
                )
                qualified_count = sum(
                    1
                    for f in audio
                    if title_containment_score(
                        _strip_edition_suffix(target.album_title),
                        re.sub(r"\.\w+$", "", re.split(r"[\\/]", f.filename)[-1]),
                        ignore=artist_words,
                    )
                    >= 0.60
                )

            coherence = self._coherence(
                target, audio, parent, qualified_count=qualified_count
            )
            confidences = [
                _file_confidence(
                    target.album_title,
                    target.artist_name,
                    target.duration_seconds,
                    f,
                    strict_title=target.track_count == 1,
                )
                for f in audio
            ]
            avg_confidence = sum(confidences) / len(confidences) if confidences else 0.0

            # Identity-only score (P3.2/D3): peer availability (speed, free slots) can
            # RANK candidates but never buy acceptance - 20% of the old formula was
            # availability, which is how the incident candidate crossed 0.70. The 5:3
            # coherence:confidence ratio is preserved and the scale stays 0..1, so the
            # persisted preflight_score_auto_accept keeps meaning what it always meant.
            base_final = 0.625 * coherence + 0.375 * avg_confidence

            # Grab-time tracklist overlap: when the pinned edition's tracklist is
            # known, a folder whose NAMED files don't cover it is the wrong
            # product wearing the right folder name (Flux vs Flux - Sessions) -
            # discount it multiplicatively (perfect overlap scores exactly as
            # before, so unknown/absent tracklists change nothing).
            overlap = (
                _core_tracklist_overlap(
                    [(f.filename, f.duration) for f in audio],
                    [
                        (t.track_number, t.title, t.duration_seconds)
                        for t in expected_tracks
                    ],
                    ignore=overlap_ignore,
                    prefix=overlap_prefix,
                )
                if expected_tracks
                else None
            )
            final = (
                base_final * (0.5 + 0.5 * overlap)
                if overlap is not None
                else base_final
            )

            if final >= auto_accept_threshold and has_evidence:
                tier = "auto"
            elif final >= manual_threshold and coherence >= manual_threshold:
                tier = "manual"
                if final >= auto_accept_threshold:
                    logger.info(
                        "preflight.evidence_capped",
                        extra={
                            "parent_directory": parent,
                            "final_score": round(final, 4),
                            "artist": target.artist_name,
                        },
                    )
            else:
                tier = "rejected"
            if (
                overlap is not None
                and tier == "rejected"
                and base_final >= manual_threshold
                and coherence >= manual_threshold
            ):
                # Overlap demotion floors at manual: the pick endpoint refuses
                # rejected-tier candidates, so a floor keeps the "pick a
                # release" escape hatch open (worst case parks for review,
                # never a dead end). The sub-threshold score stays honest.
                tier = "manual"

            if tier == "auto" and target.track_count and target.track_count > 1:
                count_ratio = len(audio) / target.track_count
                if count_ratio < 0.5:
                    if final >= manual_threshold and coherence >= manual_threshold:
                        tier = "manual"
                        logger.info(
                            "preflight.completeness_capped",
                            extra={
                                "parent_directory": parent,
                                "final_score": round(final, 4),
                                "count_ratio": round(count_ratio, 4),
                                "track_count": target.track_count,
                            },
                        )
                    else:
                        tier = "rejected"
            # Folder-worst evidence from per-file projections; attached so the
            # orchestrator's stored-snapshot recheck and the review UI reuse the
            # SAME evaluation instead of re-deriving from raw files.
            file_evidence = [_file_evidence(f) for f in audio]
            folder_decision = acq_quality.evaluate_worst(snapshot, file_evidence)
            if (
                acq_quality.is_recipe_snapshot(snapshot)
                and not folder_decision.eligible
                and acq_quality.is_hard_quality_rejection(folder_decision)
            ):
                pipeline_drops[RejectCode.QUALITY_REJECTED] += 1
                continue
            if (
                acq_quality.is_recipe_snapshot(snapshot)
                and not folder_decision.eligible
                and tier == "auto"
            ):
                tier = "manual"
            scored.append(
                ScoredCandidate(
                    username=username,
                    parent_directory=parent,
                    files=audio,
                    coherence=coherence,
                    file_confidence=avg_confidence,
                    final_score=final,
                    tier=tier,
                    track_overlap=overlap,
                    quality_evidence=folder_decision.evidence,
                    quality_decision=folder_decision,
                )
            )

        def _rank_key(candidate: ScoredCandidate):
            if acq_quality.is_recipe_snapshot(snapshot):
                decision = candidate.quality_decision
                step = (
                    decision.preference_step
                    if decision is not None and decision.preference_step is not None
                    else len(snapshot.quality_recipe) + 1
                )
                evidence = (
                    decision.evidence
                    if decision is not None
                    else AudioQualityEvidence()
                )
                refinement = acq_quality.recipe_refinement_key(snapshot, evidence)
                return (
                    _ACCEPTANCE_RANK.get(candidate.tier, 0),
                    -step,
                    *(-value for value in refinement),
                    *_availability_key(candidate),
                    candidate.final_score,
                )
            step = None
            dist = None
            certainty = EvidenceCertainty.PARTIAL
            if candidate.quality_decision is not None:
                step = candidate.quality_decision.preference_step
                certainty = candidate.quality_evidence.certainty
                if (
                    snapshot.lossy_target_kbps is not None
                    and candidate.quality_evidence.codec_family is CodecFamily.LOSSY
                ):
                    dist = acq_quality.lossy_target_distance(
                        snapshot, candidate.quality_evidence
                    )
            worst_step = len(snapshot.quality_preference_order) + 2
            return (
                # Identity band FIRST (legacy guarantee: a rejected/manual
                # hi-res folder must never hide a safe automatic result),
                _ACCEPTANCE_RANK.get(candidate.tier, 0),
                # Preference STEP, smaller = preferred; encoded NEGATIVE so
                # reverse=True ranks the smallest step first.
                -(step if step is not None else worst_step),
                -acq_quality.CERTAINTY_RANK[certainty],
                -(dist if dist is not None else -1),
                # Availability tuple and identity score are already
                # bigger-is-better for the descending comparison.
                *_availability_key(candidate),
                candidate.final_score,
            )

        scored.sort(key=_rank_key, reverse=True)
        ranked = scored[:50]
        # grab-time overlap diagnosis (keys present only when the rank knew
        # the tracklist) - the next wrong-product incident reads here.
        overlap_extras: dict = {}
        overlaps = [c.track_overlap for c in ranked if c.track_overlap is not None]
        if overlaps:
            overlap_extras = {
                "overlap_scored": len(overlaps),
                "overlap_min": round(min(overlaps), 4),
            }
            if ranked[0].track_overlap is not None:
                overlap_extras["overlap_top"] = round(ranked[0].track_overlap, 4)
        logger.info(
            "preflight.ranked",
            extra={
                "candidates_count": len(ranked),
                "top_score": ranked[0].final_score if ranked else 0.0,
                "auto_count": sum(1 for c in ranked if c.tier == "auto"),
                "manual_count": sum(1 for c in ranked if c.tier == "manual"),
                **overlap_extras,
                # why folders were dropped before scoring - a candidates_count of 0
                # with a non-zero results_count is explained entirely by these. The
                # inline gates (no_audio/codec) plus one key per shared-spec reject code.
                "groups_total": len(groups),
                "dropped_no_audio": drop_no_audio,
                "dropped_codec": drop_codec,
                "dropped_peer_exhausted": drop_peer_exhausted,
                "dropped_folder_excluded": drop_folder_excluded,
                **{f"dropped_{code.value}": n for code, n in pipeline_drops.items()},
            },
        )
        return ranked

    @staticmethod
    def _coherence(
        target: TargetAlbum,
        files: list[DownloadSearchResult],
        parent_directory: str,
        *,
        qualified_count: int | None = None,
    ) -> float:
        """``qualified_count`` (1-track releases only, P3.3): how many files actually
        NAME the requested title - a lone token-coincidence file must not present as a
        complete single (count_ratio was a 0.40-weight freebie in the incident)."""
        counted = len(files) if qualified_count is None else qualified_count
        if target.track_count and target.track_count > 0:
            count_ratio = min(1.0, counted / target.track_count)
        else:
            count_ratio = 0.5

        dir_hint = f"{target.artist_name} {target.album_title}"
        if target.year:
            dir_hint += f" {target.year}"
        dir_sim = (
            fuzz.token_set_ratio(
                _normalize_for_match(dir_hint),
                _normalize_for_match(parent_directory),
            )
            / 100.0
        )

        formats = {_effective_extension(f) for f in files if _effective_extension(f)}
        format_consistency = (
            1.0 if len(formats) == 1 else (0.5 if len(formats) <= 2 else 0.2)
        )

        bitrates = [f.bitrate for f in files if f.bitrate]
        if bitrates:
            mean = sum(bitrates) / len(bitrates)
            if mean > 0:
                variance = sum((b - mean) ** 2 for b in bitrates) / len(bitrates)
                bitrate_consistency = 1.0 if variance**0.5 < 50 else 0.5
            else:
                bitrate_consistency = 0.5
        else:
            bitrate_consistency = 0.5

        no_junk = (
            0.0 if any(k in parent_directory.lower() for k in _JUNK_KEYWORDS) else 1.0
        )

        return (
            0.40 * count_ratio
            + 0.20 * dir_sim
            + 0.15 * format_consistency
            + 0.15 * bitrate_consistency
            + 0.10 * no_junk
        )
