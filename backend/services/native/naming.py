"""NamingTemplateEngine - render a naming template into a filesystem path.

Substitutes ``{variable}`` (and ``{track:02d}`` style) tokens from an
``AudioTag`` into a relative ``Path``, then normalises each component: NFC
unicode form, invalid-filesystem-character replacement, and a 252-byte length
cap that never splits a multi-byte UTF-8 character.

Edition-suffix stripping is intentionally NOT here - it is a matcher concern
(``MusicBrainzMatcher``), applied before fuzzy matching, not to output paths.
"""

import re
import unicodedata
from pathlib import Path

from api.v1.schemas.library_management import (
    MANAGED_FIELD_NAMES,
    PathCompatibilitySettings,
)
from core.exceptions import PathLimitExceededError, ScriptValidationError
from core.management_script_language import (
    EvaluationBudget,
    ScriptValue,
    compile_naming_template,
    evaluate_expression,
    walk_expressions,
)
from models.audio import AudioTag
from models.audio_metadata import ReadAudioDocument
from models.library_management_scripts import NamingRenderResult
from models.library_management_planning import ManagementNamingContext

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
    _ALWAYS_INVALID_VALUE_CHARS = re.compile(r"[/\\\x00-\x1f\x7f]")
    _MANAGEMENT_VARIABLES = frozenset(
        {
            *MANAGED_FIELD_NAMES,
            "genre",
            "genres",
            "primary_genre",
            "artist_display",
            "artists",
            "artist_sorts",
            "album_artist_display",
            "album_artists",
            "album_artist_sorts",
            "albumartist",
            "year",
            "track",
            "disc",
            "ext",
            "extension",
            "medium",
            "album_disambiguation",
            "medium_format",
            "medium_number",
            "musicbrainz_id",
            "artist_mbid",
            "codec",
            "quality",
            "bitrate",
            "sample_rate",
            "bit_depth",
            "artwork_type",
            "artwork_comment",
            "artwork_extension",
            "artwork_format",
        }
    )

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

    def validate_management_script(
        self,
        source: str,
        *,
        script_name: str = "naming script",
    ) -> None:
        segments = compile_naming_template(source, script_name=script_name)
        if "\n" in source or "\r" in source:
            raise ScriptValidationError(
                "Naming scripts must be a single path template.",
                script_name=script_name,
                line=1,
                column=1,
            )
        if source.startswith("/"):
            raise ScriptValidationError(
                "Naming output must be relative.",
                script_name=script_name,
                line=1,
                column=1,
            )
        for segment in segments:
            if segment.legacy_variable is not None:
                variable = segment.legacy_variable
                if variable not in self._MANAGEMENT_VARIABLES:
                    raise ScriptValidationError(
                        f"Unknown naming variable: {variable}.",
                        script_name=script_name,
                        line=segment.line,
                        column=segment.column,
                    )
                if segment.format_spec and variable not in {
                    "track",
                    "disc",
                    "track_number",
                    "disc_number",
                    "total_tracks",
                    "total_discs",
                    "medium_number",
                }:
                    raise ScriptValidationError(
                        f"Variable {variable} does not support a numeric format.",
                        script_name=script_name,
                        line=segment.line,
                        column=segment.column,
                    )
            if segment.expression is not None:
                for expression in walk_expressions(segment.expression):
                    if (
                        expression.kind == "variable"
                        and expression.value not in self._MANAGEMENT_VARIABLES
                    ):
                        raise ScriptValidationError(
                            f"Unknown naming variable: {expression.value}.",
                            script_name=script_name,
                            line=expression.line,
                            column=expression.column,
                        )

    def format_management_path(
        self,
        source: str,
        document: ReadAudioDocument,
        compatibility: PathCompatibilitySettings,
        *,
        script_name: str = "naming script",
        root: Path | None = None,
        artwork_type: str = "",
        artwork_comment: str = "",
        artwork_extension: str = "",
        artwork_format: str = "",
        naming_context: ManagementNamingContext | None = None,
    ) -> NamingRenderResult:
        self.validate_management_script(source, script_name=script_name)
        variables = self._management_variables(
            document,
            compatibility,
            artwork_type=artwork_type,
            artwork_comment=artwork_comment,
            artwork_extension=artwork_extension,
            artwork_format=artwork_format,
            naming_context=naming_context,
        )
        budget = EvaluationBudget(script_name=script_name)
        rendered: list[str] = []
        for segment in compile_naming_template(source, script_name=script_name):
            if segment.literal is not None:
                rendered.append(segment.literal)
                continue
            if segment.legacy_variable is not None:
                value = variables.get(segment.legacy_variable)
                text = self._script_text(value)
                if segment.format_spec:
                    try:
                        text = format(int(text), segment.format_spec)
                    except (TypeError, ValueError) as error:
                        raise ScriptValidationError(
                            "Numeric naming format received a non-integer value.",
                            script_name=script_name,
                            line=segment.line,
                            column=segment.column,
                        ) from error
            else:
                value = evaluate_expression(segment.expression, variables, budget)
                text = self._script_text(value)
            rendered.append(
                self._sanitize_substituted_value(
                    text,
                    compatibility.separator_replacement,
                    compatibility.windows_compatible,
                )
            )
        raw_path = "".join(rendered)
        parts = raw_path.split("/")
        if (
            not raw_path
            or raw_path.startswith("/")
            or any(part in {"", ".", ".."} for part in parts)
        ):
            raise ScriptValidationError(
                "Naming script rendered an empty, absolute, or traversing path.",
                script_name=script_name,
                line=1,
                column=1,
            )
        cleaned = tuple(
            self._clean_management_component(part, compatibility) for part in parts
        )
        relative = "/".join(cleaned)
        encoded_length = len(relative.encode("utf-8"))
        if encoded_length > compatibility.maximum_path_length:
            raise PathLimitExceededError(
                "Rendered relative path exceeds the configured path limit.",
                script_name=script_name,
                line=1,
                column=1,
            )
        absolute_text = str(root / Path(relative)) if root is not None else relative
        if len(absolute_text.encode("utf-8")) > compatibility.maximum_path_length:
            raise PathLimitExceededError(
                "Rendered absolute path exceeds the configured path limit.",
                script_name=script_name,
                line=1,
                column=1,
            )
        if compatibility.windows_legacy_path_limit and len(absolute_text) > 259:
            raise PathLimitExceededError(
                "Rendered path exceeds the enabled Windows 259-character limit.",
                script_name=script_name,
                line=1,
                column=1,
            )
        collision_key = "/".join(
            unicodedata.normalize("NFC", part).casefold() for part in cleaned
        )
        return NamingRenderResult(
            relative_path=relative,
            collision_key=collision_key,
            rendered_characters=len(relative),
        )

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
                # invalid chars) can't inject path components - only the template's
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
            # leading variable) can never escape library_root - output is always
            # relative.
            if path.anchor and component == path.anchor:
                continue
            parts.append(self._clean_component(component))
        return Path("/".join(parts)) if parts else Path("_")

    @staticmethod
    def _script_text(value: ScriptValue) -> str:
        if value is None:
            return ""
        if isinstance(value, bool):
            return "true" if value else "false"
        if isinstance(value, tuple):
            return "; ".join(value)
        return str(value)

    @classmethod
    def _sanitize_substituted_value(
        cls,
        value: str,
        replacement: str,
        windows_compatible: bool,
    ) -> str:
        pattern = (
            cls._INVALID_FS_CHARS
            if windows_compatible
            else cls._ALWAYS_INVALID_VALUE_CHARS
        )
        return pattern.sub(replacement, value)

    @classmethod
    def _clean_management_component(
        cls,
        component: str,
        compatibility: PathCompatibilitySettings,
    ) -> str:
        replacement = compatibility.separator_replacement
        cleaned = unicodedata.normalize(compatibility.unicode_normalization, component)
        pattern = (
            cls._INVALID_FS_CHARS
            if compatibility.windows_compatible
            else cls._ALWAYS_INVALID_VALUE_CHARS
        )
        cleaned = pattern.sub(replacement, cleaned)
        if compatibility.replace_non_ascii:
            folded = (
                unicodedata.normalize("NFKD", cleaned)
                .encode("ascii", "ignore")
                .decode()
            )
            cleaned = folded or replacement
        if compatibility.replace_spaces_with_underscores:
            cleaned = cleaned.replace(" ", "_")
        if compatibility.windows_compatible:
            cleaned = cleaned.strip(" .")
            if cleaned and cleaned.split(".", 1)[0].casefold() in _RESERVED_NAMES:
                cleaned = f"{replacement}{cleaned}"
        if not cleaned:
            cleaned = replacement
        encoded = cleaned.encode("utf-8")
        maximum = compatibility.maximum_component_length
        if len(encoded) > maximum:
            cleaned = encoded[:maximum].decode("utf-8", "ignore")
            cleaned = (
                cleaned.strip(" .") if compatibility.windows_compatible else cleaned
            )
            if not cleaned:
                cleaned = replacement
            if (
                compatibility.windows_compatible
                and cleaned.split(".", 1)[0].casefold() in _RESERVED_NAMES
            ):
                cleaned = f"{replacement}{cleaned}"
                cleaned = cleaned.encode("utf-8")[:maximum].decode("utf-8", "ignore")
        return cleaned

    @classmethod
    def _management_variables(
        cls,
        document: ReadAudioDocument,
        compatibility: PathCompatibilitySettings,
        *,
        artwork_type: str,
        artwork_comment: str,
        artwork_extension: str,
        artwork_format: str,
        naming_context: ManagementNamingContext | None,
    ) -> dict[str, ScriptValue]:
        metadata = document.metadata
        variables: dict[str, ScriptValue] = {
            field.name: field.value for field in metadata.fields
        }
        artists = metadata.strings_for("artist")
        album_artists = metadata.strings_for("album_artist")
        artist_sorts = metadata.strings_for("artist_sort")
        album_artist_sorts = metadata.strings_for("album_artist_sort")
        genres = metadata.strings_for("genre")
        date = metadata.value_for("date")
        extension = document.probe.extension.removeprefix(".")
        if compatibility.extension_case == "lower":
            extension = extension.lower()
        elif compatibility.extension_case == "upper":
            extension = extension.upper()
        bitrate = max(0, round(document.technical.bitrate_bps / 1000))
        variables.update(
            artist=metadata.artist_display or (artists[0] if artists else ""),
            artists=artists,
            artist_display=metadata.artist_display or "",
            artist_sorts=artist_sorts,
            artist_sort=artist_sorts[0] if artist_sorts else "",
            album_artist=(
                metadata.album_artist_display
                or (album_artists[0] if album_artists else "")
            ),
            albumartist=(
                metadata.album_artist_display
                or (album_artists[0] if album_artists else "")
                or metadata.artist_display
                or (artists[0] if artists else "")
            ),
            album_artists=album_artists,
            album_artist_display=metadata.album_artist_display or "",
            album_artist_sorts=album_artist_sorts,
            album_artist_sort=(album_artist_sorts[0] if album_artist_sorts else ""),
            genre=genres[0] if genres else "",
            genres=genres,
            primary_genre=genres[0] if genres else "",
            year=(date[:4] if isinstance(date, str) else ""),
            track=metadata.value_for("track_number"),
            disc=metadata.value_for("disc_number"),
            ext=extension,
            extension=extension,
            medium=metadata.value_for("media"),
            musicbrainz_id=metadata.value_for("musicbrainz_release_group_id"),
            artist_mbid=(metadata.strings_for("musicbrainz_artist_id") or ("",))[0],
            codec=document.technical.codec or document.probe.detected_format,
            quality=(
                "lossless"
                if document.technical.bit_depth is not None
                else f"{bitrate}kbps"
            ),
            bitrate=bitrate,
            sample_rate=document.technical.sample_rate_hz,
            bit_depth=document.technical.bit_depth,
            artwork_type=artwork_type,
            artwork_comment=artwork_comment,
            artwork_extension=artwork_extension,
            artwork_format=artwork_format,
            album_disambiguation=(
                naming_context.album_disambiguation if naming_context else ""
            ),
            medium_format=(naming_context.medium_format if naming_context else ""),
            medium_number=(naming_context.medium_number if naming_context else 0),
        )
        return variables

    @classmethod
    def _clean_component(cls, component: str) -> str:
        """Normalise one path component to a safe, contained filename."""
        cleaned = cls._INVALID_FS_CHARS.sub(
            "_", unicodedata.normalize("NFC", component)
        )
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
