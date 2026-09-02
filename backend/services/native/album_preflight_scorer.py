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
import unicodedata
from collections import Counter, defaultdict

import msgspec
from rapidfuzz import fuzz
from unidecode import unidecode

from infrastructure.persistence.download_store import DownloadStore
from models.acquisition_quality import (
    AcquisitionQualitySnapshot,
    AudioQualityEvidence,
    CodecFamily,
    EvidenceCertainty,
    EvidenceProvenance,
)
from models.download import ScoredCandidate, TargetAlbum
from models.download_identity import canonical_soulseek_identity, soulseek_identity
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

_EDITION_SUFFIXES = re.compile(
    r"\b(deluxe|remastered|remaster|edition|anniversary|special|expanded|"
    r"complete|bonus|acoustic|live|demo|radio edit|extended|instrumental)\b",
    re.IGNORECASE,
)
_VERSION_MARKERS = re.compile(
    r"\b(remix|live|acoustic|instrumental|demo|radio edit|karaoke|cover|commentary)\b",
    re.IGNORECASE,
)
_CJK_RANGES = (
    (0x4E00, 0x9FFF),  # CJK Unified Ideographs
    (0x3040, 0x309F),  # Hiragana
    (0x30A0, 0x30FF),  # Katakana
    (0x3400, 0x4DBF),  # CJK Extension A
)
_JUNK_KEYWORDS = ("various", "unknown album", "untitled", "misc")
_LEADING_RELEASE_YEAR = re.compile(r"^\s*(?:\[\d{4}\]|\(\d{4}\)|\d{4}\s*[-–]\s*)\s*")

logger = logging.getLogger(__name__)

_ACCEPTANCE_RANK = {"rejected": 0, "manual": 1, "auto": 2}


def _has_cjk(text: str) -> bool:
    for char in text:
        codepoint = ord(char)
        for low, high in _CJK_RANGES:
            if low <= codepoint <= high:
                return True
    return False


def _normalize_for_match(text: str) -> str:
    """NFC + lowercase + unidecode, but never mangle CJK."""
    text = unicodedata.normalize("NFC", text or "").lower()
    if _has_cjk(text):
        return text
    return unidecode(text)


def _strip_edition_suffix(title: str) -> str:
    return _EDITION_SUFFIXES.sub("", title or "").strip()


def _version_markers(text: str) -> frozenset[str]:
    return frozenset(marker.lower() for marker in _VERSION_MARKERS.findall(text or ""))


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


def _artist_from_path(parent_directory: str, target_artist: str = "") -> str:
    """Heuristic artist extraction: try "Artist - Album", then a "Artist/Album"
    layout (first path component), then the target artist, else ""."""
    if not parent_directory:
        return ""
    if " - " in parent_directory:
        return parent_directory.split(" - ", 1)[0].strip()
    parts = [p for p in re.split(r"[\\/]", parent_directory) if p]
    if len(parts) >= 2:
        return parts[0].strip()
    if target_artist:
        return target_artist
    return parts[0].strip() if parts else ""


def _file_confidence(
    target_title: str,
    target_artist: str,
    target_duration: float | None,
    file: DownloadSearchResult,
    *,
    strict_title: bool = False,
) -> float:
    """Per-file confidence (shared by the album scorer and the track matcher).

    ``(0.55*title + 0.20*artist + 0.25*duration) * version_penalty`` when a target
    duration is available, else the duration term drops and weights redistribute
    to ``0.65*title + 0.35*artist``.

    ``strict_title`` (the track matcher + 1-track album fallbacks, P3.4): the title
    term becomes CONTAINMENT-based - a filename must name the target and nothing
    else. ``token_set_ratio`` ignored extra tokens, so "the arrival" scored 0.78
    against "02. Arrival in Ashford" and 1.0 against "Arrival - The Waking Hour" -
    both real auto-tier candidates in the 2026-07-05 incident's search job. The
    artist's own words are excluded from the foreign-token penalty ("01 - Yan Qing -
    the arrival.flac" is not naming another work). Deliberately OFF for multi-track
    albums: their per-file names are TRACK titles, and comparing those to the ALBUM
    title is uniform noise under any metric - the replay corpus showed containment's
    lower noise floor demoting legitimate albums (Inferno, 0.801 -> 0.698), so the
    calibrated token_set noise stays. CJK titles always keep token_set (containment
    tokenisation needs word boundaries)."""
    file_title = re.split(r"[\\/]", file.filename)[-1]
    file_title = re.sub(r"\.\w+$", "", file_title)

    if strict_title and not (_has_cjk(target_title) or _has_cjk(file_title)):
        artist_words = frozenset(
            t for t in _normalize_for_match(target_artist).split() if len(t) >= 2
        )
        title_score = title_containment_score(
            _strip_edition_suffix(target_title), file_title, ignore=artist_words
        )
    else:
        title_score = (
            fuzz.token_set_ratio(
                _normalize_for_match(_strip_edition_suffix(target_title)),
                _normalize_for_match(_strip_edition_suffix(file_title)),
            )
            / 100.0
        )

    file_artist = _artist_from_path(file.parent_directory, target_artist)
    artist_score = (
        fuzz.token_set_ratio(
            _normalize_for_match(target_artist),
            _normalize_for_match(file_artist),
        )
        / 100.0
    )

    # penalise when exactly one side carries a version marker
    version_penalty = (
        0.3 if _version_markers(target_title) != _version_markers(file_title) else 1.0
    )

    if target_duration and file.duration:
        diff = abs(file.duration - target_duration)
        duration_score = 1.0 if diff <= 15 else (0.5 if diff <= 25 else 0.0)
        base = 0.55 * title_score + 0.20 * artist_score + 0.25 * duration_score
    else:
        base = 0.65 * title_score + 0.35 * artist_score

    return base * version_penalty


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

        scored: list[ScoredCandidate] = []
        drop_no_audio = drop_codec = 0
        pipeline_drops: Counter[RejectCode] = Counter()
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
            final = 0.625 * coherence + 0.375 * avg_confidence

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
        logger.info(
            "preflight.ranked",
            extra={
                "candidates_count": len(ranked),
                "top_score": ranked[0].final_score if ranked else 0.0,
                "auto_count": sum(1 for c in ranked if c.tier == "auto"),
                "manual_count": sum(1 for c in ranked if c.tier == "manual"),
                # why folders were dropped before scoring - a candidates_count of 0
                # with a non-zero results_count is explained entirely by these. The
                # inline gates (no_audio/codec) plus one key per shared-spec reject code.
                "groups_total": len(groups),
                "dropped_no_audio": drop_no_audio,
                "dropped_codec": drop_codec,
                "dropped_peer_exhausted": drop_peer_exhausted,
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
