"""Is this the same song? Title and length rules shared by playlist linking and
studio-version substitution (#204).

A song title is compared after dropping qualifiers that do not change the
song ("- Remastered 1985", "(Mono)", "- Single Version"); live, demo, remix and
edit markers are kept, so those versions never match a studio take.
"""

from __future__ import annotations

import re
import unicodedata

from rapidfuzz import fuzz
from unidecode import unidecode

DURATION_TOLERANCE_SECONDS = 3
TITLE_MATCH_FLOOR = 90

_SAME_SONG_SUFFIX = re.compile(
    r"\s*(?:-\s*|[(\[])[^)\]-]*\b(?:remaster(?:ed)?|mono|stereo|single version|"
    r"album version|original version|\d{4} mix)\b[^)\]]*[)\]]?\s*$",
    re.IGNORECASE,
)
_NON_WORD = re.compile(r"[^\w]+")
_CJK_RANGES = ((0x4E00, 0x9FFF), (0x3040, 0x309F), (0x30A0, 0x30FF), (0x3400, 0x4DBF))


def _fold(text: str) -> str:
    """NFC + lower + accent-fold; CJK left intact (unidecode would mangle it)."""
    text = unicodedata.normalize("NFC", text or "").lower()
    if any(low <= ord(c) <= high for c in text for low, high in _CJK_RANGES):
        return text
    return unidecode(text)


def song_title_key(title: str) -> str:
    """Comparable form of a song title, dropping same-song qualifiers only."""
    stripped = title or ""
    while True:
        shorter = _SAME_SONG_SUFFIX.sub("", stripped)
        if shorter == stripped:
            break
        stripped = shorter
    return " ".join(_NON_WORD.sub(" ", _fold(stripped)).split())


def same_song_title(
    title: str,
    seconds: float | None,
    other_title: str,
    other_seconds: float | None,
) -> bool:
    """Same song when the titles agree and, if both lengths are known, the
    lengths are within :data:`DURATION_TOLERANCE_SECONDS`. With both lengths
    a near title (spelling) is enough; without them only an exact key is."""
    key, other = song_title_key(title), song_title_key(other_title)
    if not key or not other:
        return False
    if seconds and other_seconds:
        if abs(other_seconds - seconds) > DURATION_TOLERANCE_SECONDS:
            return False
        return fuzz.ratio(key, other) >= TITLE_MATCH_FLOOR
    return key == other
