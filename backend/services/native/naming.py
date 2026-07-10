"""NamingTemplateEngine — render a naming template into a filesystem path.

Substitutes ``{variable}`` (and ``{track:02d}`` style) tokens from an
``AudioTag`` into a relative ``Path``, then normalises each component: NFC
unicode form, invalid-filesystem-character replacement, and a 252-byte length
cap that never splits a multi-byte UTF-8 character.

Edition-suffix stripping is intentionally NOT here — it is a matcher concern
(``MusicBrainzMatcher``), applied before fuzzy matching, not to output paths.
"""

import re
import unicodedata
from pathlib import Path

from models.audio import AudioTag

# Leaves a 3-byte margin under the common 255-byte filename limit so a final
# multi-byte character truncation never overflows.
_MAX_COMPONENT_BYTES = 252

# Windows reserved device names (case-insensitive, with or without extension).
# Disarmed even on Linux so a library copied to an SMB/Windows host stays valid.
_RESERVED_NAMES = frozenset(
    {"con", "prn", "aux", "nul"}
    | {f"com{i}" for i in range(1, 10)}
    | {f"lpt{i}" for i in range(1, 10)}
)


class NamingTemplateEngine:
    DEFAULT = "{albumartist}/{album} ({year})/{disc:02d}{track:02d} {title}.{ext}"

    # Plain variables (format specs ignored) and the two that honour ``:fmt``.
    _VARIABLES = frozenset(
        {
            "artist",
            "album",
            "albumartist",
            "year",
            "title",
            "ext",
            "musicbrainz_id",
            "artist_mbid",
            "genre",
            "medium",
        }
    )
    _FORMATTED = frozenset({"track", "disc"})

    _TOKEN = re.compile(r"(\{[^}]+\})")
    # FS-reserved chars plus all C0 control characters and DEL.
    _INVALID_FS_CHARS = re.compile(r'[<>:"/\\|?*\x00-\x1f\x7f]')

    def format_path(self, template: str, tag: AudioTag, ext: str) -> Path:
        return self._normalize(self._render(template, tag, ext))

    def validate_template(self, template: str) -> list[str]:
        """Return human-readable errors for unknown variables / bad format specs."""
        errors: list[str] = []
        for segment in template.split("/"):
            if segment.strip() in ("..", "."):
                errors.append("Path segment '..' or '.' is not allowed")
        for part in self._TOKEN.split(template):
            if not (part.startswith("{") and part.endswith("}")):
                continue
            token = part[1:-1]
            var, _, fmt = token.partition(":")
            if var not in self._VARIABLES and var not in self._FORMATTED:
                errors.append(f"Unknown variable: {{{var}}}")
            elif fmt and var not in self._FORMATTED:
                errors.append(f"Variable {{{var}}} does not support a format spec")
        return errors

    def _render(self, template: str, tag: AudioTag, ext: str) -> Path:
        result: list[str] = []
        for part in self._TOKEN.split(template):
            if not part:
                continue
            if part.startswith("{") and part.endswith("}"):
                var, _, fmt = part[1:-1].partition(":")
                value = self._lookup(var, tag, ext)
                if fmt and var in self._FORMATTED:
                    try:
                        value = format(int(value), fmt)
                    except (ValueError, TypeError):
                        value = "00"
                # Sanitise substituted values so a value containing "/" (or other
                # invalid chars) can't inject path components — only the template's
                # literal separators define structure.
                result.append(self._INVALID_FS_CHARS.sub("_", value))
            else:
                result.append(part)
        rendered = "".join(result)
        # An empty variable leaves bracket litter from its template decoration -
        # "{album} ({year})" with no year renders "Album ()". Collapse empty
        # (), [] and {} groups (and their leading spaces) instead of shipping
        # them into folder names.
        rendered = re.sub(r"\s*(\(\s*\)|\[\s*\]|\{\s*\})", "", rendered)
        # Literals already carry the "/" separators; Path() splits them into
        # components, which _normalize then cleans individually.
        return Path(rendered)

    def _lookup(self, var: str, tag: AudioTag, ext: str) -> str:
        match var:
            case "artist":
                return tag.artist
            case "album":
                return tag.album
            case "albumartist":
                return tag.album_artist or tag.artist
            case "year":
                return str(tag.year) if tag.year else ""
            case "title":
                return tag.title
            case "ext":
                return ext
            case "track":
                return str(tag.track_number)
            case "disc":
                return str(tag.disc_number)
            case "genre":
                return tag.genre or ""
            case "medium":
                return ""  # populated by the caller if known (MB lookup, not template)
            case "musicbrainz_id":
                return tag.musicbrainz_release_group_id or ""
            case "artist_mbid":
                return tag.musicbrainz_artist_id or ""
            case _:
                return ""  # unknown variable renders empty (lenient; validate_template is strict)

    def _normalize(self, path: Path) -> Path:
        parts: list[str] = []
        for component in path.parts:
            # Drop the filesystem root so an absolute render (e.g. an empty
            # leading variable) can never escape library_root — output is always
            # relative.
            if path.anchor and component == path.anchor:
                continue
            parts.append(self._clean_component(component))
        return Path("/".join(parts)) if parts else Path("_")

    @classmethod
    def _clean_component(cls, component: str) -> str:
        """Normalise one path component to a safe, contained filename."""
        cleaned = cls._INVALID_FS_CHARS.sub("_", unicodedata.normalize("NFC", component))
        # Strip leading/trailing dots+spaces (Windows-illegal; '.'/'..' would
        # otherwise act as path operators and traverse on join).
        cleaned = cleaned.strip(" .")
        if not cleaned:
            cleaned = "_"
        # Disarm reserved Windows device names (with or without extension).
        if cleaned.split(".", 1)[0].lower() in _RESERVED_NAMES:
            cleaned = f"_{cleaned}"
        encoded = cleaned.encode("utf-8")
        if len(encoded) > _MAX_COMPONENT_BYTES:
            cleaned = encoded[:_MAX_COMPONENT_BYTES].decode("utf-8", errors="ignore")
            cleaned = cleaned.strip(" .") or "_"
        return cleaned
