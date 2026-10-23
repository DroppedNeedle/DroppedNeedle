"""Pure token-matching core for acquisition scoring.

Stateless string helpers shared by the Soulseek album preflight scorer, the
per-track matcher, and plugin scoring (``PluginScoringHelper``). No engine
state, no stores, no models: every function takes plain ``str``/``float``
inputs so third-party plugins can score indexer/download candidates with the
exact same calibration as the native scorers.

Ownership: the low-level title discriminator (containment, artist evidence)
stays canonical in ``services.native.title_match``; this module owns the
normalisation + per-file confidence formula historically duplicated between
``album_preflight_scorer._file_confidence`` and ``track_matcher``.
"""

import re
import unicodedata

from rapidfuzz import fuzz
from unidecode import unidecode

from services.native.file_processor import _TAG_TITLE_WEAK
from services.native.title_match import title_containment_score

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


def has_cjk(text: str) -> bool:
    for char in text or "":
        codepoint = ord(char)
        for low, high in _CJK_RANGES:
            if low <= codepoint <= high:
                return True
    return False


def normalize_for_match(text: str) -> str:
    """NFC + lowercase + unidecode, but never mangle CJK."""
    text = unicodedata.normalize("NFC", text or "").lower()
    if has_cjk(text):
        return text
    return unidecode(text)


def artist_words(artist_name: str | None) -> frozenset[str]:
    """The matchable words of an artist name (same rule as the strict
    file-confidence path): normalized, split, single letters dropped."""
    return frozenset(
        t for t in normalize_for_match(artist_name or "").split() if len(t) >= 2
    )


def name_tokens(text: str | None) -> frozenset[str]:
    """Every alphanumeric token of a release name (artist or album), normalized
    and punctuation-split, single letters KEPT: ``"S I M"`` must still be
    recognisable as a filename prefix, and ``"Kaspa."`` must equal ``"Kaspa"``.
    Prefix recognition only - title judging keeps ``artist_words``."""
    return frozenset(
        t for t in re.split(r"[^a-z0-9]+", normalize_for_match(text or "")) if t
    )


def release_prefix_tokens(artist_name: str | None, album_title: str | None) -> frozenset[str]:
    """The words a peer may prepend to every file of a release: its artist's
    and its album's. Feed to ``tracklist_overlap(prefix=...)`` /
    ``strip_release_prefix``."""
    return name_tokens(artist_name) | name_tokens(album_title)


_PREFIX_SEGMENT_RE = re.compile(r"\s+[-\u2013\u2014]\s+")


def strip_release_prefix(stem: str, prefix: frozenset[str]) -> str:
    """Drop the leading `` - ``-separated segments of a filename stem that only
    repeat the release's own words, so ``Artist - Album - 02 - Title`` is judged
    as ``02 - Title``: the number becomes a positional claim and the album's
    words stop reading as a conflicting title (``Blue Bird`` is the album, not
    a song called Blue Bird). The common ``Artist - Album - NN - Title`` rip
    layout otherwise scores every file foreign and parks a correct, complete
    folder for review. The last segment always survives (a title track named
    only ``Blue Moon`` is still a title), and a segment carrying any other
    word is kept whole."""
    if not prefix or not stem:
        return stem
    parts = _PREFIX_SEGMENT_RE.split(stem)
    while len(parts) > 1:
        tokens = name_tokens(parts[0])
        if tokens and tokens <= prefix:
            parts.pop(0)
        else:
            break
    return " - ".join(parts)


def strip_edition_suffix(title: str) -> str:
    return _EDITION_SUFFIXES.sub("", title or "").strip()


def version_markers(text: str) -> frozenset[str]:
    return frozenset(marker.lower() for marker in _VERSION_MARKERS.findall(text or ""))


def artist_from_path(parent_directory: str, target_artist: str = "") -> str:
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


def file_confidence(
    target_title: str,
    target_artist: str,
    target_duration: float | None,
    filename: str,
    parent_directory: str,
    file_duration: float | None,
    *,
    strict_title: bool = False,
) -> float:
    """Per-file confidence, pure-string form of the shared scorer formula.

    ``(0.55*title + 0.20*artist + 0.25*duration) * version_penalty`` when a target
    duration and a file duration are both available, else the duration term drops
    and weights redistribute to ``0.65*title + 0.35*artist``.

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
    file_title = re.split(r"[\\/]", filename or "")[-1]
    file_title = re.sub(r"\.\w+$", "", file_title)

    if strict_title and not (has_cjk(target_title or "") or has_cjk(file_title)):
        artist_words = frozenset(
            t for t in normalize_for_match(target_artist).split() if len(t) >= 2
        )
        title_score = title_containment_score(
            strip_edition_suffix(target_title or ""), file_title, ignore=artist_words
        )
    else:
        title_score = (
            fuzz.token_set_ratio(
                normalize_for_match(strip_edition_suffix(target_title or "")),
                normalize_for_match(strip_edition_suffix(file_title)),
            )
            / 100.0
        )

    file_artist = artist_from_path(parent_directory or "", target_artist or "")
    artist_score = (
        fuzz.token_set_ratio(
            normalize_for_match(target_artist or ""),
            normalize_for_match(file_artist),
        )
        / 100.0
    )

    # penalise when exactly one side carries a version marker
    version_penalty = (
        0.3 if version_markers(target_title or "") != version_markers(file_title) else 1.0
    )

    if target_duration and file_duration:
        diff = abs(file_duration - target_duration)
        duration_score = 1.0 if diff <= 15 else (0.5 if diff <= 25 else 0.0)
        base = 0.55 * title_score + 0.20 * artist_score + 0.25 * duration_score
    else:
        base = 0.65 * title_score + 0.35 * artist_score

    return base * version_penalty


def album_match(artist: str, album: str, candidate_title: str) -> float:
    """Score a plugin indexer/download candidate title against a wanted album.

    Parity form of the album preflight path: non-strict (``token_set``) title
    term, no duration, artist drawn from the candidate title itself via the same
    ``"Artist - Album"`` heuristic the folder scorer uses.
    """
    return file_confidence(
        album or "",
        artist or "",
        None,
        candidate_title or "",
        candidate_title or "",
        None,
        strict_title=False,
    )


# Codec/bitrate/container words: dating/quality annotations, never album identity.
# Medium/source words (cd, dvd, vinyl, web, remaster, deluxe, live...) are
# DELIBERATELY absent - they distinguish products ("Platinum Edition" vs the
# CD+DVD edition must not merge).
_FOLDER_NON_IDENTITY_TOKENS = frozenset(
    {
        "flac", "mp3", "m4a", "ogg", "oga", "opus", "wav", "aiff", "aif",
        "aac", "alac", "wma", "ape", "mp4", "tak", "tta", "wv",
        "320", "192", "256", "128", "96",
        "v0", "v2", "cbr", "vbr", "abr", "q0", "q2",
        "16bit", "24bit", "44100", "48000", "88200", "96000", "176400", "192000",
        "lossless", "lossy", "stereo", "mono", "hires",
    }
)
_FOLDER_BRACKET_RE = re.compile(r"\[[^\[\]]*\]|\([^()]*\)")
_FOLDER_YEAR_PREFIX_RE = re.compile(r"^\s*(?:19|20)\d{2}\s*[-._]\s*")


def normalize_folder_identity(
    parent_directory: str, *, artist_words: frozenset[str] = frozenset()
) -> str:
    """Canonical album identity of a Soulseek folder name for wrong-product
    learning: the same commercial product shared under different peers'
    naming (``2021. Flux``, ``Flux (2021)``, ``Flux [FLAC]``) normalizes to
    one key (``flux``), so proving one copy wrong teaches all of them.

    Strips dating/quality annotations only - strict ``YYYY<sep>`` prefixes,
    bracketed groups holding nothing but years/quality/digits, pure-digit
    tokens (bare years included), codec words, and the requesting artist's
    own words (flat ``Artist - Album`` shares merge with ``Album`` leaves) -
    then sorts the survivors. Edition/medium descriptors (platinum, deluxe,
    live, cd, dvd, remaster) SURVIVE: stripping those would merge distinct
    products. Accepted caveat: same-name products distinguished ONLY by a
    bare year (``NOW 2006`` vs ``NOW 2007``) share an identity - bounded by
    RG-scoping, the 7-day quarantine TTL, and manual re-request clearing.
    Empty when nothing identity-carrying remains (caller skips)."""
    text = normalize_for_match(parent_directory).replace("\\", "/")
    text = _FOLDER_YEAR_PREFIX_RE.sub("", text)
    for group in _FOLDER_BRACKET_RE.findall(text):
        inner = group[1:-1]
        words = [
            word
            for word in re.split(r"[^a-z0-9]+", inner.casefold())
            if word
        ]
        if words and all(
            word.isdigit() or word in _FOLDER_NON_IDENTITY_TOKENS for word in words
        ):
            text = text.replace(group, " ", 1)
    tokens = sorted(
        {
            word
            for word in re.split(r"[^a-z0-9]+", text.casefold())
            if word
            and not word.isdigit()
            and word not in _FOLDER_NON_IDENTITY_TOKENS
            and word not in artist_words
            and len(word) >= 2
        }
    )
    return " ".join(tokens)


def filename_stem(filename: str) -> str:
    """The last path segment with its extension stripped (Soulseek remote paths
    are backslash-delimited). Byte-identical to the stem rule the per-file
    failover filter has always used, so both judges read names the same way."""
    stem = (filename or "").replace("\\", "/").rsplit("/", 1)[-1]
    base, dot, _ext = stem.rpartition(".")
    return base if dot and base else stem


_TRACK_NUM_RE = re.compile(r"^\s*(\d{1,3})(?:\s*[\.\-_)]+\s*|\s+)")


def leading_track_number(filename: str) -> int | None:
    """A filename's leading track number (``04 Tom Jones - Delilah.mp3`` →
    ``4``), or ``None``. Deliberately looser than the import-time
    ``_filename_track_number``: bare-whitespace shapes (``01 Title.mp3``,
    ubiquitous on Soulseek) also parse here, because pre-download the
    filename is the ONLY position evidence (import falls back to tags).
    Four-digit years and digit-glued words (``10cc``) still never match."""
    base = re.split(r"[\\/]", filename or "")[-1]
    match = _TRACK_NUM_RE.match(base)
    return int(match.group(1)) if match else None


# Tokens that carry no title identity: numbering residue and the generic
# words obfuscated rips use (``Track 05``, ``audio.mp3``). A stem reduced to
# these (or nothing) has NO title signal - duration alone judges it.
_TITLE_FILLER_TOKENS = frozenset(
    {"track", "tracks", "audio", "unknown", "untitled", "song", "songs"}
)


def stem_title_signal(stem: str) -> str:
    """The title-carrying words of a filename stem: leading track number,
    digit-only tokens, and filler words removed. Empty means the name says
    nothing about which song this is (``01.flac``, ``Track 05.mp3``)."""
    text = _TRACK_NUM_RE.sub("", stem or "")
    words = []
    for token in re.split(r"[^a-z0-9]+", text.casefold()):
        if not token or token.isdigit():
            continue
        if token.rstrip("0123456789") in _TITLE_FILLER_TOKENS:
            continue
        words.append(token)
    return " ".join(words)


def durations_agree(
    file_duration: float | None, track_duration: float | None
) -> bool | None:
    """Tri-state length verdict under the shared ``max(15s, 10%)`` gate: True /
    False, or None when either side is unknown."""
    if not file_duration or not track_duration:
        return None
    return abs(file_duration - track_duration) <= max(15.0, 0.10 * track_duration)


def title_serves_track(
    track_title: str | None, stem: str, *, ignore: frozenset[str] = frozenset()
) -> bool | None:
    """Tri-state title verdict: containment-strong (``>= _TAG_TITLE_WEAK``),
    conflicting, or None when either side carries no title signal. ``ignore``
    excludes the artist's own words from the foreign-token penalty
    (``Poppy - Kitty.flac`` names Kitty, not a song called Poppy)."""
    if not (track_title and stem):
        return None
    return title_containment_score(track_title, stem, ignore=ignore) >= _TAG_TITLE_WEAK


def pair_serves_track(
    stem: str,
    file_duration: float | None,
    track_title: str | None,
    track_duration: float | None,
    *,
    rescue: bool = True,
    ignore: frozenset[str] = frozenset(),
) -> bool:
    """Whether one file plausibly IS one expected track — the shared tri-state
    core behind both the per-file failover filter and grab-time overlap.
    A hard duration miss always excludes, and no usable signal passes
    (fail-open). ``rescue`` (legacy filter behavior): a conflicting title is
    forgiven when the duration matches. Grab-time overlap passes
    ``rescue=False`` — a length coincidence must not rescue a conflicting
    title there (``So Mean`` at 177s is not ``Rot In LA`` at 175s), mirroring
    the import-time tag check, which holds on title conflict. ``ignore``
    excludes the artist's own words from the title penalty."""
    duration_ok = durations_agree(file_duration, track_duration)
    if duration_ok is False:
        return False
    title_ok = title_serves_track(track_title, stem, ignore=ignore)
    if title_ok is False:
        return rescue and duration_ok is True
    return True


def tracklist_overlap(
    files: list[tuple[str, float | None]],
    expected: list[tuple[int, str | None, float | None]],
    *,
    ignore: frozenset[str] = frozenset(),
    prefix: frozenset[str] = frozenset(),
) -> float | None:
    """0..1: how well a folder's NAMED files cover an expected tracklist —
    the grab-time simulation of the import-time positional check. ``files``
    is ``(filename, advertised duration)`` per audio file; ``expected`` is
    ``(track_number, title, duration)`` per pinned-edition track. None when
    there is no tracklist to judge against (caller keeps today's behavior).
    ``prefix`` (the release's artist + album tokens) lets a stem shed its
    ``Artist - Album - `` lead before judging - see ``strip_release_prefix``.

    Position-aware: a numbered file claims its position and is judged ONLY
    against it, so same-songs-in-shuffled-order scores low exactly as the
    positional import would hold it. Numberless files judge loose (any
    position, same tri-state rule). A file serving nothing is foreign; the
    score is position precision discounted by foreign content, so a legit
    deluxe (full overlap + bonus tracks) stays high while a wrong product
    (low overlap + many foreign files) collapses. Disc is ignored: a file
    serves when it matches ANY disc's same-numbered track.

    Fail-open throughout: a numbered claim with no title AND no duration
    signal counts as served (unknown, not wrong — mirrors the D18
    import rule), so obfuscated rips are never stranded by this term."""
    if not expected:
        return None
    if not files:
        return 0.0
    by_number: dict[int, list[tuple[str | None, float | None]]] = {}
    for number, title, duration in expected:
        by_number.setdefault(number, []).append((title, duration))
    claims = 0
    correct = 0
    foreign = 0
    for filename, file_duration in files:
        stem = strip_release_prefix(filename_stem(filename), prefix)
        # Titles judge on signal words only: numbering residue and filler
        # words ("01", "Track 05") are not a conflicting title - without this
        # every obfuscated rip would read as foreign.
        signal = stem_title_signal(stem)
        number = leading_track_number(stem)
        options = by_number.get(number, []) if number is not None else []
        if options:
            claims += 1
            if _claim_serves(stem, signal, file_duration, options, ignore):
                correct += 1
        elif not any(
            pair_serves_track(
                signal,
                file_duration,
                title,
                duration,
                rescue=False,
                ignore=ignore,
            )
            for _num, title, duration in expected
        ):
            foreign += 1
    total = len(files)
    foreign_ratio = foreign / total
    if claims:
        base = correct / claims
        return base * (1.0 - 0.5 * foreign_ratio)
    return 1.0 - foreign_ratio


def _claim_serves(
    stem: str,
    signal: str,
    file_duration: float | None,
    options: list[tuple[str | None, float | None]],
    ignore: frozenset[str],
) -> bool:
    """Whether a numbered claim serves its expected position (any disc). A
    version-marker mismatch (live/remix/acoustic vs a clean expected title)
    vetoes: those are different recordings, so the folder routes to manual
    review instead of auto-import (deliberately stricter than the import-time
    tag check, whose subset-tolerant ratio passes suffixed titles).
    Zero-signal claims pass (fail-open)."""
    for title, duration in options:
        if not pair_serves_track(
            signal,
            file_duration,
            title,
            duration,
            rescue=False,
            ignore=ignore,
        ):
            continue
        if version_markers(stem) != version_markers(title or ""):
            continue
        return True
    return False


def track_match(artist: str, track: str, filename: str) -> float:
    """Score a plugin file path against a wanted track.

    Parity form of the track matcher path: strict (containment) title term, no
    duration, artist drawn from the filename's directory part so a bare
    ``"Artist - Track"`` name still carries its artist.
    """
    parts = [p for p in re.split(r"[\\/]", filename or "") if p]
    if len(parts) >= 2:
        parent = "/".join(parts[:-1])
    else:
        parent = filename or ""
    return file_confidence(
        track or "",
        artist or "",
        None,
        filename or "",
        parent,
        None,
        strict_title=True,
    )
