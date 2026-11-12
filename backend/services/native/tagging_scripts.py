"""Ordered, bounded metadata-only transformations for management previews."""

from __future__ import annotations

from collections.abc import Sequence

from api.v1.schemas.library_management import (
    MANAGED_FIELD_NAMES,
    TaggingScriptSettings,
)
from core.exceptions import ScriptValidationError
from core.management_script_language import (
    EvaluationBudget,
    Expression,
    ScriptValue,
    Statement,
    compile_tagging_program,
    evaluate_expression,
    walk_expressions,
    walk_statements,
)
from models.audio_metadata import (
    AudioMetadataDocument,
    AudioSemanticField,
    DesiredCustomTag,
)
from models.library_management_scripts import (
    CustomTagValue,
    TaggingScriptResult,
    TaggingTransformation,
)

MAX_CUSTOM_TAGS = 64
MAX_CUSTOM_TAG_NAME_LENGTH = 255
MAX_CUSTOM_TAG_VALUES = 100
MAX_CUSTOM_OUTPUT_CHARACTERS = 65_536

_ORDERED_FIELDS = {
    "artist",
    "album_artist",
    "artist_sort",
    "album_artist_sort",
    "release_type",
    "label",
    "catalog_number",
    "isrc",
    "musicbrainz_artist_id",
    "musicbrainz_album_artist_id",
    "musicbrainz_work_id",
    "composer",
    "lyricist",
    "conductor",
    "performer",
    "arranger",
    "remixer",
    "producer",
    "genre",
}
_INTEGER_FIELDS = {
    "track_number",
    "total_tracks",
    "disc_number",
    "total_discs",
    "movement_number",
    "movement_count",
}
_ALLOWED_FIELDS = frozenset({*MANAGED_FIELD_NAMES, "genre"})
_READ_ONLY_VARIABLES = frozenset(
    {
        "artist_display",
        "album_artist_display",
        "artists",
        "album_artists",
        "genres",
        "year",
        "primary_genre",
    }
)


def _custom_name(target: str) -> str | None:
    if not target.casefold().startswith("custom."):
        return None
    return target[7:].strip()


def _raise(
    message: str,
    script_name: str,
    line: int,
    column: int,
) -> None:
    raise ScriptValidationError(
        message,
        script_name=script_name,
        line=line,
        column=column,
    )


def _validate_target(statement: Statement, script_name: str) -> None:
    target = statement.target or ""
    if target in _ALLOWED_FIELDS:
        if statement.operation == "append" and target not in _ORDERED_FIELDS:
            _raise(
                f"Field {target} does not accept append.",
                script_name,
                statement.line,
                statement.column,
            )
        return
    custom = _custom_name(target)
    if (
        custom is None
        or not custom
        or "\x00" in custom
        or len(custom) > MAX_CUSTOM_TAG_NAME_LENGTH
    ):
        _raise(
            f"Unknown or invalid tagging target: {target}.",
            script_name,
            statement.line,
            statement.column,
        )


def validate_tagging_script(
    source: str,
    *,
    script_name: str = "tagging script",
) -> tuple[Statement, ...]:
    program = compile_tagging_program(source, script_name=script_name)
    for statement in walk_statements(program):
        if statement.operation != "if":
            _validate_target(statement, script_name)
        if statement.expression is None:
            continue
        for expression in walk_expressions(statement.expression):
            if expression.kind != "variable":
                continue
            variable = str(expression.value)
            if (
                variable not in _ALLOWED_FIELDS
                and variable not in _READ_ONLY_VARIABLES
                and _custom_name(variable) is None
            ):
                _raise(
                    f"Unknown tagging variable: {variable}.",
                    script_name,
                    expression.line,
                    expression.column,
                )
    return program


def _script_values(value: ScriptValue) -> tuple[str, ...]:
    if value is None:
        return ()
    if isinstance(value, tuple):
        return value
    if isinstance(value, bool):
        return ("true" if value else "false",)
    return (str(value),)


def _normalize_field_value(
    field: str,
    value: ScriptValue,
    *,
    script_name: str,
    statement: Statement,
) -> ScriptValue:
    if field in _ORDERED_FIELDS:
        return _script_values(value)
    if value is None:
        return None
    if field in _INTEGER_FIELDS:
        if isinstance(value, bool) or not isinstance(value, int) or value < 0:
            _raise(
                f"Field {field} requires a non-negative integer.",
                script_name,
                statement.line,
                statement.column,
            )
        return value
    if field == "compilation":
        if not isinstance(value, bool):
            _raise(
                "Field compilation requires true or false.",
                script_name,
                statement.line,
                statement.column,
            )
        return value
    if not isinstance(value, str):
        _raise(
            f"Field {field} requires text.",
            script_name,
            statement.line,
            statement.column,
        )
    return value


class TaggingScriptEngine:
    def validate(self, script: TaggingScriptSettings) -> None:
        validate_tagging_script(script.source, script_name=script.name)

    def apply(
        self,
        document: AudioMetadataDocument,
        scripts: Sequence[TaggingScriptSettings],
        *,
        custom_tags: Sequence[CustomTagValue] = (),
        protected_fields: frozenset[str] = frozenset(),
    ) -> TaggingScriptResult:
        fields: dict[str, ScriptValue] = {
            field.name: field.value for field in document.fields
        }
        custom: dict[str, tuple[str, ...]] = {}
        seen_custom: set[str] = set()
        for value in custom_tags:
            folded = value.name.casefold()
            if folded in seen_custom:
                _raise("Custom tag names must be unique.", "tagging input", 1, 1)
            seen_custom.add(folded)
            custom[value.name] = value.values
        # Tags already in the file are validated only if a script changes them: an
        # unchanged tag is never written back, so its size cannot make the write unsafe.
        baseline = dict(custom)
        self._validate_custom_output(custom, "tagging input", 1, 1, baseline=baseline)
        transformations: list[TaggingTransformation] = []
        for script in scripts:
            program = validate_tagging_script(script.source, script_name=script.name)
            budget = EvaluationBudget(script_name=script.name)
            self._run_statements(
                program,
                script,
                fields,
                custom,
                protected_fields,
                transformations,
                budget,
                document,
            )
            self._validate_custom_output(
                custom, script.name, 1, 1, baseline=baseline
            )
        return TaggingScriptResult(
            metadata=AudioMetadataDocument(
                fields=tuple(
                    AudioSemanticField(name=name, value=value)
                    for name, value in fields.items()
                    if value is not None and value != ()
                ),
                artist_display=self._effective_display(
                    fields.get("artist"),
                    document.strings_for("artist"),
                    document.artist_display,
                ),
                album_artist_display=self._effective_display(
                    fields.get("album_artist"),
                    document.strings_for("album_artist"),
                    document.album_artist_display,
                ),
            ),
            custom_tags=tuple(
                CustomTagValue(name=name, values=values)
                for name, values in sorted(
                    custom.items(), key=lambda item: item[0].casefold()
                )
            ),
            transformations=tuple(transformations),
        )

    @staticmethod
    def transformed_values(result: TaggingScriptResult) -> dict[str, ScriptValue]:
        values: dict[str, ScriptValue] = {}
        for transformation in result.transformations:
            if (
                transformation.skipped_reason is None
                and _custom_name(transformation.target) is None
            ):
                values[transformation.target] = transformation.after
        return values

    @staticmethod
    def desired_custom_tags(
        before: Sequence[CustomTagValue],
        result: TaggingScriptResult,
    ) -> tuple[DesiredCustomTag, ...]:
        before_by_name = {value.name.casefold(): value for value in before}
        after_by_name = {value.name.casefold(): value for value in result.custom_tags}
        desired: list[DesiredCustomTag] = []
        for folded in sorted({*before_by_name, *after_by_name}):
            old = before_by_name.get(folded)
            new = after_by_name.get(folded)
            if old is not None and new is not None and old.values == new.values:
                continue
            if new is None:
                desired.append(DesiredCustomTag(name=old.name, action="delete"))
            else:
                desired.append(
                    DesiredCustomTag(
                        name=new.name,
                        action="set",
                        values=new.values,
                    )
                )
        return tuple(desired)

    def _run_statements(
        self,
        statements: tuple[Statement, ...],
        script: TaggingScriptSettings,
        fields: dict[str, ScriptValue],
        custom: dict[str, tuple[str, ...]],
        protected_fields: frozenset[str],
        transformations: list[TaggingTransformation],
        budget: EvaluationBudget,
        original: AudioMetadataDocument,
    ) -> None:
        for statement in statements:
            marker = statement.expression or Expression(
                kind="literal",
                value=None,
                line=statement.line,
                column=statement.column,
            )
            budget.consume(marker)
            variables = self._variables(fields, custom, original)
            if statement.operation == "if":
                condition = evaluate_expression(statement.expression, variables, budget)
                branch = statement.body if bool(condition) else statement.alternate
                self._run_statements(
                    branch,
                    script,
                    fields,
                    custom,
                    protected_fields,
                    transformations,
                    budget,
                    original,
                )
                continue
            target = statement.target or ""
            before = self._target_value(target, fields, custom)
            if target in protected_fields:
                transformations.append(
                    TaggingTransformation(
                        script_id=script.id,
                        script_name=script.name,
                        operation=statement.operation,
                        target=target,
                        before=before,
                        after=before,
                        line=statement.line,
                        column=statement.column,
                        skipped_reason="manual override has precedence",
                    )
                )
                continue
            custom_name = _custom_name(target)
            if statement.operation == "delete":
                after: ScriptValue = () if target in _ORDERED_FIELDS else None
            else:
                evaluated = evaluate_expression(statement.expression, variables, budget)
                if custom_name is not None:
                    values = _script_values(evaluated)
                    if statement.operation == "append":
                        values = (*_script_values(before), *values)
                    after = tuple(dict.fromkeys(values))
                elif statement.operation == "append":
                    values = (*_script_values(before), *_script_values(evaluated))
                    after = tuple(dict.fromkeys(values))
                else:
                    after = _normalize_field_value(
                        target,
                        evaluated,
                        script_name=script.name,
                        statement=statement,
                    )
            if isinstance(after, tuple) and len(after) > MAX_CUSTOM_TAG_VALUES:
                _raise(
                    "Tagging statement produced too many values.",
                    script.name,
                    statement.line,
                    statement.column,
                )
            self._set_target(target, after, fields, custom)
            transformations.append(
                TaggingTransformation(
                    script_id=script.id,
                    script_name=script.name,
                    operation=statement.operation,
                    target=target,
                    before=before,
                    after=after,
                    line=statement.line,
                    column=statement.column,
                )
            )

    @staticmethod
    def _variables(
        fields: dict[str, ScriptValue],
        custom: dict[str, tuple[str, ...]],
        original: AudioMetadataDocument,
    ) -> dict[str, ScriptValue]:
        genres = fields.get("genre")
        artist_display = TaggingScriptEngine._effective_display(
            fields.get("artist"),
            original.strings_for("artist"),
            original.artist_display,
        )
        album_artist_display = TaggingScriptEngine._effective_display(
            fields.get("album_artist"),
            original.strings_for("album_artist"),
            original.album_artist_display,
        )
        variables = dict(fields)
        variables.update(
            artist_display=artist_display,
            album_artist_display=album_artist_display,
            artists=_script_values(fields.get("artist")),
            album_artists=_script_values(fields.get("album_artist")),
            genres=_script_values(genres),
            year=(fields["date"][:4] if isinstance(fields.get("date"), str) else None),
            primary_genre=(genres[0] if isinstance(genres, tuple) and genres else None),
        )
        variables.update({f"custom.{name}": values for name, values in custom.items()})
        return variables

    @staticmethod
    def _effective_display(
        value: ScriptValue,
        original_values: tuple[str, ...],
        original_display: str | None,
    ) -> str | None:
        values = _script_values(value)
        if values == original_values:
            return original_display
        return "; ".join(values) or None

    @staticmethod
    def _target_value(
        target: str,
        fields: dict[str, ScriptValue],
        custom: dict[str, tuple[str, ...]],
    ) -> ScriptValue:
        custom_name = _custom_name(target)
        if custom_name is not None:
            existing = next(
                (name for name in custom if name.casefold() == custom_name.casefold()),
                None,
            )
            return custom.get(existing) if existing is not None else None
        return fields.get(target)

    @staticmethod
    def _set_target(
        target: str,
        value: ScriptValue,
        fields: dict[str, ScriptValue],
        custom: dict[str, tuple[str, ...]],
    ) -> None:
        custom_name = _custom_name(target)
        if custom_name is not None:
            existing = next(
                (name for name in custom if name.casefold() == custom_name.casefold()),
                None,
            )
            if value in (None, ()):
                if existing is not None:
                    custom.pop(existing, None)
            else:
                custom[existing or custom_name] = _script_values(value)
            return
        if value in (None, ()):
            fields.pop(target, None)
        else:
            fields[target] = value

    @staticmethod
    def _validate_custom_output(
        custom: dict[str, tuple[str, ...]],
        script_name: str,
        line: int,
        column: int,
        *,
        baseline: dict[str, tuple[str, ...]] | None = None,
    ) -> None:
        if len(custom) > MAX_CUSTOM_TAGS:
            _raise(
                "Tagging scripts produced too many custom tags.",
                script_name,
                line,
                column,
            )
        total = 0
        for name, values in custom.items():
            if baseline is not None and baseline.get(name) == values:
                continue
            if (
                not name
                or "\x00" in name
                or len(name) > MAX_CUSTOM_TAG_NAME_LENGTH
                or len(values) > MAX_CUSTOM_TAG_VALUES
            ):
                _raise(
                    "A custom tag exceeds its name or value-count bound.",
                    script_name,
                    line,
                    column,
                )
            total += sum(len(value) for value in values)
            if any(len(value) > 8_192 for value in values):
                _raise(
                    "A custom tag value is too long.",
                    script_name,
                    line,
                    column,
                )
        if total > MAX_CUSTOM_OUTPUT_CHARACTERS:
            _raise(
                "Custom tag output is too large.",
                script_name,
                line,
                column,
            )
