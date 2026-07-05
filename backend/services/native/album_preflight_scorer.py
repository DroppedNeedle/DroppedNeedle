"""Two-phase album preflight scorer.

Group candidate files by ``(username, parent_directory)``, score folder
coherence, then rank by identity (coherence + per-file confidence) alone; peer
availability (speed / free slots) is a banded tiebreaker that can never buy
acceptance, and ``tier='auto'`` additionally requires the requested artist to be
named somewhere in the folder's remote paths (D2/D3, 2026-07-05 incident).

Below-threshold groups are kept (``tier='rejected'``, top ~50 by score) so the
Review tab's "Show all results anyway" needs no re-search. Quarantined
``(username, filename)`` sources are dropped before scoring. Non-audio sidecars
(cover art, cue, log, m3u) a folder search returns are excluded before judging - a
quality gate then drops folders whose audio is outside ``quality_min``..``quality_max``
or, when ``flac_mp3_only``, contains a non-FLAC/MP3 track; ranking prefers the highest
tier absolutely.
CJK strings skip ``unidecode``; off-version matches (remix/live/acoustic vs
original) are penalised x0.3.
"""

import logging
import re
import unicodedata
from collections import Counter, defaultdict

from rapidfuzz import fuzz
from unidecode import unidecode

from infrastructure.persistence.download_store import DownloadStore
from models.download import ScoredCandidate, TargetAlbum
from models.download_identity import soulseek_identity
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
from services.native.title_match import artist_evidence, title_containment_score
from services.native.quality_tiers import (
    DEFAULT_QUALITY_MAX,
    DEFAULT_QUALITY_MIN,
    candidate_tier,
    folder_hires_key,
    is_audio,
    is_flac_or_mp3,
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

logger = logging.getLogger(__name__)


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
    return file.extension.lower() if file.extension else _ext_from_filename(file.filename)


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
        if file_artist
        else 0.0
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
    def __init__(
        self,
        download_store: DownloadStore,
        *,
        quality_min: str = DEFAULT_QUALITY_MIN,
        quality_max: str = DEFAULT_QUALITY_MAX,
        flac_mp3_only: bool = True,
        policy: SpecPolicy | None = None,
    ):
        self._store = download_store
        self._flac_mp3_only = flac_mp3_only
        # The full spec policy: passed by the composition root (built from
        # DownloadPolicySettings) or derived from the quality kwargs in tests. The size/
        # term/age gates default off, so a quality-only construction is behaviour-unchanged.
        self._policy = policy or SpecPolicy(quality_min=quality_min, quality_max=quality_max)

    async def rank(
        self,
        target: TargetAlbum,
        results: list[DownloadSearchResult],
        *,
        auto_accept_threshold: float = 0.70,
        manual_threshold: float = 0.50,
        held_tier: str | None = None,
    ) -> list[ScoredCandidate]:
        context = await build_context(self._store, held_tier=held_tier)
        policy = self._policy
        # Soulseek quarantine is file-granular (a peer may have just one bad file): apply
        # it as a pool pre-filter via the shared spec, so a quarantined file is dropped
        # before grouping while the folder's other files survive.
        filtered = [
            r for r in results
            if isinstance(
                quarantine(
                    Candidate(source="soulseek",
                              identity=soulseek_identity(r.username, r.filename)),
                    target, context, policy,
                ),
                Accept,
            )
        ]

        groups: dict[tuple[str, str], list[DownloadSearchResult]] = defaultdict(list)
        for result in filtered:
            groups[(result.username, result.parent_directory)].append(result)

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
            if self._flac_mp3_only and not all(is_flac_or_mp3(f) for f in audio):
                drop_codec += 1
                continue
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
                    tier=candidate_tier(audio),
                    size_bytes=sum(f.size for f in audio),
                ),
                target, context, policy,
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
                    t for t in _normalize_for_match(target.artist_name).split() if len(t) >= 2
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

            coherence = self._coherence(target, audio, parent, qualified_count=qualified_count)
            confidences = [
                _file_confidence(
                    target.album_title, target.artist_name, target.duration_seconds, f,
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

            # Auto-acceptance must be EARNED by identity evidence (D2): a folder whose
            # full remote paths never name the requested artist caps at 'manual' - one
            # human click in Review instead of a silent wrong grab. Various-artists
            # requests skip the gate (the album artist legitimately differs per track).
            has_evidence = "various" in (target.artist_name or "").lower() or any(
                artist_evidence(target.artist_name, f.filename) for f in audio
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

            scored.append(
                ScoredCandidate(
                    username=username,
                    parent_directory=parent,
                    files=audio,
                    coherence=coherence,
                    file_confidence=avg_confidence,
                    final_score=final,
                    tier=tier,
                )
            )

        def _availability(c: ScoredCandidate) -> float:
            speed = min(1.0, max((f.upload_speed for f in c.files), default=0) / 1000.0)
            slot = 1.0 if any(f.has_free_slot for f in c.files) else 0.0
            return 0.5 * speed + 0.5 * slot

        # highest tier first (any decent FLAC beats any MP3), then hi-res before 16/44
        # within the tier (H1), then best match - with availability as a BANDED
        # tiebreaker only (D3): candidates within the same 0.05 identity band order by
        # peer speed/slots, but availability can never outrank a better identity match.
        scored.sort(
            key=lambda c: (
                tier_rank(candidate_tier(c.files)),
                *folder_hires_key(c.files),
                int(c.final_score / 0.05),
                _availability(c),
                c.final_score,
            ),
            reverse=True,
        )
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
        format_consistency = 1.0 if len(formats) == 1 else (0.5 if len(formats) <= 2 else 0.2)

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

        no_junk = 0.0 if any(k in parent_directory.lower() for k in _JUNK_KEYWORDS) else 1.0

        return (
            0.40 * count_ratio
            + 0.20 * dir_sim
            + 0.15 * format_consistency
            + 0.15 * bitrate_consistency
            + 0.10 * no_junk
        )
