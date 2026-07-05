"""Shared album-title discriminator for the release scorers.

``token_set_ratio`` (the obfuscation-tolerant identity metric both scorers lean on)
IGNORES extra tokens, so a request scores near-identical against every *other* album by
the same artist: "Led Zeppelin" (the self-titled debut) ~= "Led Zeppelin II", "Houses of
the Holy", "In Through the Out Door"... A search for a popular artist returns the whole
discography, and without this guard the picker burns one full download per wrong album
(import matches 0 tracks -> blocklist -> failover) and exhausts before reaching the album
actually requested.

``names_different_album`` rejects a candidate that names a DIFFERENT album: one carrying
album-name words the requested title didn't ask for ("II", "houses", "presence"...). It is
used by both the Usenet (``NewznabReleaseScorer``) and Soulseek (``AlbumPreflightScorer``)
paths so the rule can't drift between them.

Design (validated against real Newznab/Soulseek result sets):
- The album name lives BEFORE the format/source/year run: ``Artist-Album-LP-FLAC-1969-GROUP``.
  We isolate the album region by truncating at the first format/quality boundary token, so
  the trailing scene group (REETKEVER, GP...) and the year never read as album words.
- A leading ``[002/113] "`` Usenet part-counter is stripped first - its digits would
  otherwise trip the boundary at position 0.
- Rejection is on EXTRA words only, never missing ones, so an obfuscated release of a
  numbered album still passes (Q4). And the whole guard is gated on the artist being
  present in the candidate: a fully obfuscated title (no readable artist) is left alone, to
  be settled by the indexer-match base score + the import tag-match (Q4 obfuscation tolerance).
- Version descriptors (deluxe/remaster/edition...) are stripped from BOTH sides, so a
  deluxe/remastered edition of the requested album still matches.
- Roman series numerals (II..) are ordinary alpha words, so they discriminate naturally.
  Digit volumes ("Vol. 2" vs "Vol. 3") are NOT distinguished: a bare digit can't be told
  from a year / bit-depth / catalog number, and ``volNN+NN`` par2 names would false-reject -
  the same scene-filename-collision caution that keeps ``part N`` out of the markers.
"""

import re
import unicodedata

from rapidfuzz import fuzz
from unidecode import unidecode

# A leading Usenet part counter: ``[002/113] "`` (and the opening quote of the real name).
_PART_COUNTER_RE = re.compile(r'^\s*\[\d+\s*/\s*\d+\]\s*"?')
# Title separators -> spaces, so ``led_zeppelin``, ``In.Through``, ``(2014)`` all tokenise.
_SEP_RE = re.compile(r"[_.\-/()\[\]{}+,'\"]")
# A trailing/inline featuring credit: ``(feat. X)`` / ``ft. X`` / ``featuring X`` - not part of
# album identity, and a long credit tail drags the fuzzy ratio + can read as a foreign word.
_FEAT_RE = re.compile(r"\s[(\[]?\b(feat|ft|featuring)\b\.?.*$", re.IGNORECASE)
_CJK_RANGES = ((0x4E00, 0x9FFF), (0x3040, 0x309F), (0x30A0, 0x30FF), (0x3400, 0x4DBF))


def _has_cjk(text: str) -> bool:
    return any(low <= ord(c) <= high for c in text for low, high in _CJK_RANGES)


def fold(text: str) -> str:
    """NFC + casefold + accent-fold, so ``Mötley Crüe`` == ``Motley Crue`` and ``Sigur Rós``
    == ``Sigur Ros``. CJK is left intact (unidecode would romanise/mangle it). Used so the
    different-album guard and the Usenet identity score stop failing on accented artists."""
    text = unicodedata.normalize("NFC", text or "").lower()
    return text if _has_cjk(text) else unidecode(text)


def strip_featuring(text: str) -> str:
    return _FEAT_RE.sub("", text or "").strip()

# Tokens that begin the format/source/quality run: the album name ends before the first of
# these. Codecs, media, and online sources - none of which are album-name words. Bit-depth /
# sample-rate / year / catalog tokens carry a digit and are caught generically (see below).
_BOUNDARY = frozenset({
    "web", "webrip", "cdrip", "eac", "cd", "cds", "lp", "ep", "vinyl", "sacd", "dvd",
    "dvda", "bd", "bluray", "cassette", "shmcd", "tape", "reel", "hdtracks", "qobuz",
    "tidal", "deezer", "bandcamp",
    "flac", "alac", "ape", "wav", "wavpack", "wv", "mp3", "aac", "ogg", "opus", "dsd",
    "dsf", "m4a", "aiff", "mqa",
})
# Version / edition / packaging / content-rating descriptors that can appear BEFORE the format
# boundary (a deluxe/remastered/explicit edition IS the requested album) - stripped from both the
# request and the candidate so they can't read as a foreign album word. Critical for the
# import-time wrong-album guard, where rip ALBUM tags routinely carry "(Deluxe Version)" /
# "(Explicit)" / "(Disc 2)" that the MusicBrainz target title doesn't. Kept in sync with
# musicbrainz_matcher._EDITION_SUFFIXES; "single"/"complete" are deliberately ABSENT (a single or a
# complete-recordings boxset is a different PRODUCT, not an edition of the same album). Region/
# language tags (US/Japanese...) sit AFTER the format boundary, so truncation already excludes them.
_EDITION = frozenset({
    "remaster", "remastered", "deluxe", "edition", "anniversary", "expanded", "special",
    "limited", "collectors", "collector", "bonus", "reissue", "repack", "proper", "mono",
    "stereo", "original", "digipak", "version", "explicit", "clean", "extended", "standard",
    "promo", "disc",
})
# Sidecar / packaging extensions a per-file result or folder listing drags in.
_SIDECAR = frozenset({
    "log", "cue", "nfo", "sfv", "m3u", "m3u8", "jpg", "jpeg", "png", "txt", "pdf", "par",
    "par2", "rar", "zip", "sub", "covers",
})
_STOP = frozenset({
    "the", "a", "an", "of", "and", "in", "on", "to", "for", "with", "at", "by", "from",
    "as", "is", "or",
})


def _tokens(text: str) -> list[str]:
    cleaned = strip_featuring(fold(_PART_COUNTER_RE.sub("", text or "")))
    return [t for t in _SEP_RE.sub(" ", cleaned).split() if t]


def _content_words(tokens: list[str], artist: set[str]) -> set[str]:
    """Distinctive album-name words: purely-alphabetic tokens (>=2 chars) that aren't the
    artist, a stopword, a version descriptor, a format/media token, or a sidecar extension.
    Digit-bearing tokens (years, bit-depths, catalog numbers) and single characters are
    dropped - they aren't album identity."""
    out: set[str] = set()
    for t in tokens:
        if not t.isalpha() or len(t) < 2:
            continue
        if t in artist or t in _STOP or t in _EDITION or t in _BOUNDARY or t in _SIDECAR:
            continue
        out.add(t)
    return out


def _album_region(tokens: list[str]) -> list[str]:
    """The album-name portion: everything before the first format/quality boundary token (a
    codec/media token or any digit-bearing token - bit depth, sample rate, year, ``2CD``).
    A boundary at position 0 is ignored (no preceding album region to keep), so a stray
    leading number doesn't blank the title."""
    for i, t in enumerate(tokens):
        if i > 0 and (any(c.isdigit() for c in t) or t in _BOUNDARY):
            return tokens[:i]
    return tokens


def _artist_present(tokens: list[str], artist: set[str]) -> bool:
    """A majority of the artist's words appear in the candidate. False for a fully
    obfuscated title (no readable artist) -> the guard then leaves it alone (Q4)."""
    if not artist:
        return False
    return sum(1 for a in artist if a in tokens) >= max(1, (len(artist) + 1) // 2)


def title_containment_score(
    expected_title: str, candidate: str, *, ignore: frozenset[str] = frozenset()
) -> float:
    """0..1: how well ``candidate`` names ``expected_title`` - and NOTHING ELSE.

    ``token_set_ratio`` ignores extra tokens, so "the arrival" scores 0.78 against
    "02. Arrival in Ashford" and a full 1.0 against "Arrival - The Waking Hour" -
    exactly how the 2026-07-05 wrong single fuzzy-matched. This score multiplies
    coverage of the expected title's distinctive words by a penalty per EXTRA
    distinctive word the candidate carries (a foreign word means a different work):

        "the arrival" vs "01 - the arrival"          -> 1.00 (extras: none)
        "the arrival" vs "the arrival (Deluxe)"      -> 1.00 (edition token, not extra)
        "the arrival" vs "02. Arrival in Ashford"    -> 0.50 (extra: ashford)
        "the arrival" vs "Arrival - The Waking Hour" -> 0.33 (extra: waking, hour)

    Digit tokens (track numbers, years) and edition/format/sidecar tokens are never
    "extra"; ``ignore`` lets callers exclude the artist's words when scoring full
    filenames ("01 - Yan Qing - the arrival.flac" must not read qing as foreign).
    Token matching is typo-tolerant (per-token Levenshtein >= 85, so "Arival" still
    counts as "arrival" - and a candidate's near-miss token is not read as foreign).
    Degenerate expected titles (single characters / all stopwords) fall back to
    ``token_set_ratio`` - there are no distinctive words to contain."""
    expected_tokens = _tokens(expected_title)
    core = {
        t for t in expected_tokens
        if len(t) >= 2 and (t.isalpha() or t.isdigit())
        and t not in _STOP and t not in _EDITION
    }
    if not core:
        return fuzz.token_set_ratio(fold(expected_title), fold(candidate)) / 100.0
    cand_tokens = set(_tokens(candidate.replace("\\", "/")))

    def matches_core(token: str) -> bool:
        return token in core or any(fuzz.ratio(token, c) >= 85 for c in core)

    covered = sum(
        1 for t in core
        if t in cand_tokens or any(fuzz.ratio(t, c) >= 85 for c in cand_tokens)
    )
    coverage = covered / len(core)
    extra = {
        t for t in cand_tokens
        if t.isalpha() and len(t) >= 2 and not matches_core(t) and t not in ignore
        and t not in _STOP and t not in _EDITION and t not in _BOUNDARY
        and t not in _SIDECAR
    }
    penalty = len(core) / (len(core) + len(extra))
    return coverage * penalty


def artist_evidence(artist_name: str, candidate_path: str) -> bool:
    """POSITIVE evidence that the requested artist is named somewhere in a candidate's
    full remote path (all folder levels + filename): a majority of the artist's
    distinctive words appear as path tokens.

    Used to gate ``tier='auto'`` (D2, 2026-07-05 wrong-single incident): auto-acceptance
    must be earned by identity evidence, so ABSENCE of evidence is False - an obfuscated
    path is *unknown*, not a match. Deliberately NOT built on the scorers'
    ``_artist_from_path``, whose target-artist fallback fabricates a perfect self-match
    for every folder without an ``"Artist - Album"`` shape (the common Soulseek layout).
    Scanning the full path (not just the parent folder) is what carries the
    ``Artist/Album/track`` share layouts, where the artist is a grandparent directory.

    Stopwords are dropped from the artist's words so "The Who" needs "who", not the
    ubiquitous "the" (fallback to all words when the name is entirely stopwords)."""
    words = {t for t in _tokens(artist_name) if t.isalpha() and len(t) >= 2 and t not in _STOP}
    if not words:
        words = {t for t in _tokens(artist_name) if t.isalpha() and len(t) >= 2}
    # Soulseek remote paths are backslash-delimited; ``_SEP_RE`` (calibrated for release
    # TITLES) doesn't split on backslash, so normalise to '/' before tokenising or every
    # directory level glues to its neighbour and the artist token is never seen.
    return _artist_present(_tokens(candidate_path.replace("\\", "/")), words)


def names_different_album(album_title: str, artist_name: str, candidate_title: str) -> bool:
    """True when ``candidate_title`` names a DIFFERENT album than the one requested - it
    carries distinctive album-name words ("II", "houses", "presence") the requested title
    didn't, marking another album by the same artist.

    Rejects on EXTRA words only (an obfuscated release of the requested album still passes),
    and only when the artist is recognisably present (a fully obfuscated title is left to the
    indexer-match base score + import tag-match). Version descriptors are stripped from both
    sides so a deluxe/remaster of the requested album matches."""
    artist = {t for t in _tokens(artist_name) if t.isalpha() and len(t) >= 2}
    candidate = _tokens(candidate_title)
    if not _artist_present(candidate, artist):
        return False
    wanted = _content_words(_tokens(album_title), artist)
    got = _content_words(_album_region(candidate), artist)
    return bool(got - wanted)
