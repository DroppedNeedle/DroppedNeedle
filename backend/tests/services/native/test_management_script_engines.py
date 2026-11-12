import unicodedata
from pathlib import Path

import msgspec
import pytest

from api.v1.schemas.library_management import (
    LibraryManagementSettings,
    NamingScriptSettings,
    PathCompatibilitySettings,
    PICARD_ORGANIZER_PROFILE_ID,
    TaggingScriptSettings,
    build_initial_library_management_settings,
    normalize_library_management_settings,
)
from core.exceptions import PathLimitExceededError, ScriptValidationError
from core.management_script_language import (
    EvaluationBudget,
    compile_expression,
    compile_tagging_program,
    evaluate_expression,
)
from infrastructure.audio.metadata_engine import AudioMetadataEngine
from models.audio_metadata import AudioMetadataDocument, AudioSemanticField
from models.library_management_planning import ManagementNamingContext
from models.library_management_scripts import CustomTagValue
from services.native.naming import NamingTemplateEngine
from services.native.tagging_scripts import TaggingScriptEngine

FIXTURES = Path(__file__).parents[2] / "fixtures" / "library"


def _document():
    return AudioMetadataEngine().read(FIXTURES / "management_full.flac")


def _tagging(source: str, *, name: str = "Test tags", script_id: str = "script-1"):
    return TaggingScriptSettings(id=script_id, name=name, source=source)


def _document_with_artists(*, artist: str, album_artist: str | None):
    document = _document()
    replacements = {
        "artist": (artist,),
        "album_artist": () if album_artist is None else (album_artist,),
    }
    metadata = msgspec.structs.replace(
        document.metadata,
        fields=tuple(
            msgspec.structs.replace(field, value=replacements[field.name])
            if field.name in replacements
            else field
            for field in document.metadata.fields
        ),
        artist_display=None,
        album_artist_display=None,
    )
    return msgspec.structs.replace(document, metadata=metadata)


def test_management_naming_supports_full_variables_and_safe_functions() -> None:
    source = (
        '{if(equals(compilation, true), "Various Artists", albumartist)}/'
        "{sort_name(album)} ({year})/"
        "{pad(disc, 2)}-{pad(track, 2)} "
        '{join(artists, " & ")} - {title} [{quality}].{lower(extension)}'
    )

    result = NamingTemplateEngine().format_management_path(
        source,
        _document(),
        PathCompatibilitySettings(extension_case="upper"),
        script_name="Detailed folders",
    )

    assert result.relative_path == (
        "Various Artists/Management Album (2024)/"
        "01-02 Alpha & Beta - Management Track [lossless].flac"
    )
    assert result.collision_key == result.relative_path.casefold()
    assert result.rendered_characters == len(result.relative_path)


@pytest.mark.parametrize(
    ("album_artist", "artist", "expected"),
    [
        ("Alpha", "Fallback", "A"),
        (" The National ", "Fallback", "N"),
        ("\t tHe\u2003National\u00a0", "Fallback", "N"),
        ("The\tNational", "Fallback", "N"),
        ("The  Doors", "Fallback", "D"),
        ("the xx", "Fallback", "X"),
        ("t0ni", "Fallback", "T"),
        ("2 Chainz", "Fallback", "#"),
        ("_m0lly", "Fallback", "#"),
        ("Öxxö Xööx", "Fallback", "Ö"),
        ("高橋洋子", "Fallback", "高"),
        ("The", "Fallback", "T"),
        ("\u0301Radiohead", "Fallback", "#"),
        ("ßeta", "Fallback", "S"),
        ("ﬃn", "Fallback", "F"),
        (None, "The National", "N"),
        ("", "Radiohead", "R"),
        ("  ", "Radiohead", "#"),
        (None, "", "#"),
    ],
)
def test_management_initial_uses_exact_effective_albumartist_bucket(
    album_artist, artist, expected
):
    result = NamingTemplateEngine().format_management_path(
        "{initial}",
        _document_with_artists(artist=artist, album_artist=album_artist),
        PathCompatibilitySettings(),
    )

    assert result.relative_path == expected
    assert len(result.relative_path) == 1


def test_management_initial_composes_nfd_artist_before_selecting_the_bucket():
    composed = "Éclair"
    decomposed = unicodedata.normalize("NFD", composed)
    engine = NamingTemplateEngine()
    compatibility = PathCompatibilitySettings()

    composed_result = engine.format_management_path(
        "{initial}",
        _document_with_artists(artist=composed, album_artist=None),
        compatibility,
    )
    decomposed_result = engine.format_management_path(
        "{initial}",
        _document_with_artists(artist=decomposed, album_artist=None),
        compatibility,
    )

    assert composed_result.relative_path == decomposed_result.relative_path == "É"
    assert composed_result.collision_key == decomposed_result.collision_key


@pytest.mark.parametrize(
    ("normalization", "expected"),
    [
        ("NFC", "Ｔ"),
        ("NFKC", "A"),
    ],
)
def test_management_initial_honors_normalization_before_article_removal(
    normalization, expected
):
    result = NamingTemplateEngine().format_management_path(
        "{initial}",
        _document_with_artists(
            artist="Fallback",
            album_artist="Ｔｈｅ\u3000Ａ",
        ),
        PathCompatibilitySettings(unicode_normalization=normalization),
    )

    assert result.relative_path == expected
    assert len(result.relative_path) == 1


@pytest.mark.parametrize("source", ("{initial}", "{upper(initial)}"))
def test_management_initial_is_valid_in_plain_and_expression_scripts(source):
    engine = NamingTemplateEngine()
    engine.validate_management_script(source, script_name="Initial script")

    result = engine.format_management_path(
        source,
        _document_with_artists(artist="Fallback", album_artist="The National"),
        PathCompatibilitySettings(),
        script_name="Initial script",
    )

    assert result.relative_path == "N"


def test_management_initial_preserves_traversal_blocking_and_collision_keys():
    engine = NamingTemplateEngine()
    compatibility = PathCompatibilitySettings()
    malicious = _document_with_artists(artist="../escape", album_artist=None)

    result = engine.format_management_path(
        "safe/{initial}/{albumartist}.{ext}",
        malicious,
        compatibility,
    )

    assert result.relative_path == "safe/#/_escape.flac"
    assert not result.as_path().is_absolute()
    assert ".." not in result.as_path().parts

    composed = engine.format_management_path(
        "{initial}/{albumartist}.{ext}",
        _document_with_artists(artist="Éclair", album_artist=None),
        compatibility,
    )
    decomposed = engine.format_management_path(
        "{initial}/{albumartist}.{ext}",
        _document_with_artists(
            artist=unicodedata.normalize("NFD", "Éclair"),
            album_artist=None,
        ),
        compatibility,
    )

    assert composed.relative_path == decomposed.relative_path == "É/Éclair.flac"
    assert composed.collision_key == decomposed.collision_key


def test_external_artwork_naming_has_explicit_artwork_context() -> None:
    result = NamingTemplateEngine().format_management_path(
        "{albumartist}/{album}/{artwork_type}-{artwork_comment}."
        "{artwork_extension}-{artwork_format}",
        _document(),
        PathCompatibilitySettings(),
        artwork_type="back",
        artwork_comment="Booklet scan",
        artwork_extension="png",
        artwork_format="png",
    )

    assert result.relative_path == ("Alpha/Management Album/back-Booklet scan.png-png")


def test_dynamic_disc_defaults_use_canonical_path_context() -> None:
    settings = build_initial_library_management_settings()
    profile = next(
        value for value in settings.profiles if value.id == PICARD_ORGANIZER_PROFILE_ID
    )
    scripts = {value.id: value for value in settings.naming_scripts}
    standard = scripts[profile.organization.naming_script_id]
    multi = scripts[profile.organization.multi_disc_naming_script_id]
    engine = NamingTemplateEngine()

    single = engine.format_management_path(
        standard.source,
        _document(),
        profile.organization.compatibility,
        naming_context=ManagementNamingContext(album_disambiguation="Deluxe/Edition"),
    )
    missing_format = engine.format_management_path(
        multi.source,
        _document(),
        profile.organization.compatibility,
        naming_context=ManagementNamingContext(
            album_disambiguation="Deluxe/Edition",
            medium_format="",
            medium_number=1,
            organization_audio_medium_count=2,
        ),
    )
    known_format = engine.format_management_path(
        multi.source,
        _document(),
        profile.organization.compatibility,
        naming_context=ManagementNamingContext(
            medium_format="CD/Audio", medium_number=3, organization_audio_medium_count=2
        ),
    )
    document = _document()
    without_year = msgspec.structs.replace(
        document,
        metadata=msgspec.structs.replace(
            document.metadata,
            fields=tuple(
                field for field in document.metadata.fields if field.name != "date"
            ),
        ),
    )
    missing_year_and_disambiguation = engine.format_management_path(
        standard.source,
        without_year,
        profile.organization.compatibility,
    )

    assert (
        single.relative_path
        == "Alpha/Management Album (2024) (Deluxe_Edition)/02 - Management Track.flac"
    )
    assert (
        missing_format.relative_path
        == "Alpha/Management Album (2024) (Deluxe_Edition)/Disc 01/02 - Management Track.flac"
    )
    assert (
        known_format.relative_path
        == "Alpha/Management Album (2024)/CD_Audio 03/02 - Management Track.flac"
    )
    assert (
        missing_year_and_disambiguation.relative_path
        == "Alpha/Management Album/02 - Management Track.flac"
    )


def test_picard_v4_single_disc_script_renders_avalon_release_year() -> None:
    settings = build_initial_library_management_settings()
    profile = next(
        value for value in settings.profiles if value.id == PICARD_ORGANIZER_PROFILE_ID
    )
    script = next(
        value
        for value in settings.naming_scripts
        if value.id == profile.organization.naming_script_id
    )
    document = _document()
    replacements = {
        "album": "Avalon",
        "title": "Drugdealer",
        "track_number": 3,
        "date": "2008",
    }
    metadata = msgspec.structs.replace(
        document.metadata,
        fields=tuple(
            msgspec.structs.replace(field, value=replacements[field.name])
            if field.name in replacements
            else field
            for field in document.metadata.fields
        ),
        album_artist_display="Anthony Green",
    )

    result = NamingTemplateEngine().format_management_path(
        script.source,
        msgspec.structs.replace(document, metadata=metadata),
        profile.organization.compatibility,
    )

    assert result.relative_path == "Anthony Green/Avalon (2008)/03 - Drugdealer.flac"


def test_management_only_path_variables_do_not_enter_legacy_naming_validation() -> None:
    engine = NamingTemplateEngine()

    assert engine.validate_template(
        "{album_disambiguation}/{medium_format}/{medium_number}"
    ) == [
        "Unknown variable: {album_disambiguation}",
        "Unknown variable: {medium_format}",
        "Unknown variable: {medium_number}",
    ]


@pytest.mark.parametrize(
    ("source", "expected"),
    (
        ("{albumartist}/{title}.{ext}", "Alpha/Management Track.flac"),
        (
            "{default(album_artist_display, artist_display)}/"
            '{conditional(contains(genres, "Ambient"), "Ambient", "Other")}/'
            "{slice(title, 0, 10)}.{ext}",
            "Alpha/Ambient/Management.flac",
        ),
        (
            '{ascii_fold("Beyoncé")}/{upper(codec)}/'
            "{first(musicbrainz_artist_id)}.{ext}",
            "Beyonce/FLAC/10000000-0000-4000-8000-000000000005.flac",
        ),
    ),
)
def test_naming_examples_are_deterministic(source: str, expected: str) -> None:
    engine = NamingTemplateEngine()
    compatibility = PathCompatibilitySettings()

    first = engine.format_management_path(source, _document(), compatibility)
    second = engine.format_management_path(source, _document(), compatibility)

    assert first.relative_path == expected
    assert second == first


def test_naming_compatibility_controls_and_collision_normalization() -> None:
    engine = NamingTemplateEngine()
    compatibility = PathCompatibilitySettings(
        separator_replacement="-",
        replace_spaces_with_underscores=True,
        replace_non_ascii=True,
        maximum_component_length=24,
        extension_case="upper",
    )
    result = engine.format_management_path(
        '{"CON"}/{concat("A/B ", "ガ", title)}.{ext}',
        _document(),
        compatibility,
    )

    # Truncation preserves the suffix and trims the stem instead of clipping
    # the extension.
    assert result.relative_path == "-CON/A-B_Management_Trac.FLAC"
    assert all(len(part.encode("utf-8")) <= 24 for part in result.as_path().parts)

    composed = engine.format_management_path(
        '{"CAFÉ"}.{ext}', _document(), PathCompatibilitySettings()
    )
    decomposed = engine.format_management_path(
        '{"cafe\\u0301"}.{ext}', _document(), PathCompatibilitySettings()
    )
    assert composed.collision_key == decomposed.collision_key


def test_naming_values_cannot_inject_separators_or_traversal() -> None:
    document = _document()
    metadata = AudioMetadataDocument(
        fields=tuple(
            AudioSemanticField(
                name=field.name,
                value="../escape" if field.name == "title" else field.value,
            )
            for field in document.metadata.fields
        ),
        artist_display=document.metadata.artist_display,
        album_artist_display=document.metadata.album_artist_display,
    )
    replaced = document.__class__(
        probe=document.probe,
        metadata=metadata,
        artwork=document.artwork,
        technical=document.technical,
        raw_tags=document.raw_tags,
        native_tags=document.native_tags,
        file_attributes=document.file_attributes,
        warnings=document.warnings,
    )

    result = NamingTemplateEngine().format_management_path(
        "safe/{title}.{ext}", replaced, PathCompatibilitySettings()
    )

    assert result.relative_path == "safe/_escape.flac"
    assert not result.as_path().is_absolute()
    assert ".." not in result.as_path().parts
    with pytest.raises(ScriptValidationError, match="absolute|traversing"):
        NamingTemplateEngine().format_management_path(
            "../{title}.{ext}", replaced, PathCompatibilitySettings()
        )
    with pytest.raises(ScriptValidationError, match="relative"):
        NamingTemplateEngine().format_management_path(
            "/{title}.{ext}", replaced, PathCompatibilitySettings()
        )


def test_naming_component_total_and_windows_limits_block() -> None:
    engine = NamingTemplateEngine()
    result = engine.format_management_path(
        "{title}.{ext}",
        _document(),
        PathCompatibilitySettings(maximum_component_length=12),
    )
    assert len(result.relative_path.encode("utf-8")) == 12

    with pytest.raises(PathLimitExceededError, match="relative path"):
        engine.format_management_path(
            f'{"x" * 70}/{{title}}.{{ext}}',
            _document(),
            PathCompatibilitySettings(maximum_path_length=64),
        )
    with pytest.raises(PathLimitExceededError, match="absolute path"):
        engine.format_management_path(
            "{title}.{ext}",
            _document(),
            PathCompatibilitySettings(maximum_path_length=64),
            root=Path("/a/very/long/configured/library/root/for/testing"),
        )
    with pytest.raises(PathLimitExceededError, match="259-character"):
        engine.format_management_path(
            "{title}.{ext}",
            _document(),
            PathCompatibilitySettings(
                maximum_path_length=4096,
                windows_legacy_path_limit=True,
            ),
            root=Path("/" + "r" * 250),
        )


def test_naming_and_tagging_syntax_errors_include_script_location() -> None:
    with pytest.raises(ScriptValidationError) as naming_error:
        NamingTemplateEngine().validate_management_script(
            "{unknown(title)}", script_name="Broken naming"
        )
    assert naming_error.value.script_name == "Broken naming"
    assert naming_error.value.line == 1
    assert naming_error.value.column >= 1

    with pytest.raises(ScriptValidationError) as tagging_error:
        TaggingScriptEngine().validate(
            _tagging('if equals(title, "x"):\nset title = "y"', name="Broken tags")
        )
    assert tagging_error.value.script_name == "Broken tags"
    assert tagging_error.value.line == 1


def test_settings_save_validation_rejects_unknown_variables_and_host_capabilities() -> (
    None
):
    settings = build_initial_library_management_settings()
    settings.naming_scripts.append(
        NamingScriptSettings(
            id="55f447a4-3053-4a42-989e-669bf3d954e8",
            name="Unsafe naming",
            source='{environment("HOME")}/{title}.{ext}',
        )
    )
    with pytest.raises(ScriptValidationError, match="Unknown safe function"):
        normalize_library_management_settings(settings)

    settings = build_initial_library_management_settings()
    settings.tagging_scripts.append(
        TaggingScriptSettings(
            id="0a4d7612-bc01-49e0-81bb-6f735cbe9d65",
            name="Unsafe tags",
            source='set title = __import__("os")',
        )
    )
    with pytest.raises(ScriptValidationError, match="Unknown safe function"):
        normalize_library_management_settings(settings)


def test_ordered_tagging_set_append_delete_condition_and_attribution() -> None:
    scripts = (
        _tagging(
            "set title = upper(title)\n"
            'append genre = ["Downtempo", "Ambient"]\n'
            'set custom.MOOD = default(first(genres), "Unknown")',
            name="First",
            script_id="first",
        ),
        _tagging(
            "if equals(compilation, true):\n"
            '  set album_artist = ["Various Artists"]\n'
            "else:\n"
            "  delete catalog_number\n"
            "end\n"
            "delete barcode",
            name="Second",
            script_id="second",
        ),
    )

    result = TaggingScriptEngine().apply(_document().metadata, scripts)

    assert result.metadata.value_for("title") == "MANAGEMENT TRACK"
    assert result.metadata.value_for("genre") == (
        "Electronic",
        "Ambient",
        "Downtempo",
    )
    assert result.metadata.value_for("album_artist") == ("Various Artists",)
    assert result.metadata.album_artist_display == "Various Artists"
    assert result.metadata.value_for("barcode") is None
    assert result.custom_tags == (CustomTagValue(name="MOOD", values=("Electronic",)),)
    assert [
        (item.script_name, item.operation, item.target)
        for item in result.transformations
    ] == [
        ("First", "set", "title"),
        ("First", "append", "genre"),
        ("First", "set", "custom.MOOD"),
        ("Second", "set", "album_artist"),
        ("Second", "delete", "barcode"),
    ]
    assert all(item.line > 0 and item.column > 0 for item in result.transformations)


def test_tagging_missing_values_custom_append_and_override_precedence() -> None:
    script = _tagging(
        'set catalog_number = default(catalog_number, "NO-CATALOG")\n'
        'append custom.NOTE = ["second", "first"]\n'
        'set title = "Blocked"'
    )
    result = TaggingScriptEngine().apply(
        AudioMetadataDocument(
            fields=(AudioSemanticField(name="title", value="Original"),)
        ),
        (script,),
        custom_tags=(CustomTagValue(name="NOTE", values=("first",)),),
        protected_fields=frozenset({"title"}),
    )

    assert result.metadata.value_for("catalog_number") == ("NO-CATALOG",)
    assert result.metadata.value_for("title") == "Original"
    assert result.custom_tags == (
        CustomTagValue(name="NOTE", values=("first", "second")),
    )
    assert result.transformations[-1].skipped_reason == "manual override has precedence"
    assert TaggingScriptEngine.transformed_values(result) == {
        "catalog_number": ("NO-CATALOG",)
    }
    assert TaggingScriptEngine.desired_custom_tags(
        (CustomTagValue(name="NOTE", values=("first",)),), result
    )[0].values == ("first", "second")


def test_tagging_custom_tag_count_and_duplicate_name_bounds() -> None:
    document = AudioMetadataDocument(fields=())
    too_many = tuple(
        CustomTagValue(name=f"TAG{index}", values=("value",)) for index in range(65)
    )
    with pytest.raises(ScriptValidationError, match="too many custom tags"):
        TaggingScriptEngine().apply(document, (), custom_tags=too_many)
    with pytest.raises(ScriptValidationError, match="unique"):
        TaggingScriptEngine().apply(
            document,
            (),
            custom_tags=(
                CustomTagValue(name="Mood", values=("one",)),
                CustomTagValue(name="MOOD", values=("two",)),
            ),
        )


def test_an_oversized_custom_tag_already_in_the_file_does_not_block_the_profile() -> None:
    document = AudioMetadataDocument(
        fields=(AudioSemanticField(name="title", value="Original"),)
    )
    oversized = CustomTagValue(name="NOTES", values=("x" * 9_000,))
    script = _tagging('set catalog_number = "CAT-1"')

    result = TaggingScriptEngine().apply(document, (script,), custom_tags=(oversized,))

    assert result.metadata.value_for("catalog_number") == ("CAT-1",)
    assert result.custom_tags == (oversized,)
    # Unchanged, so it is never planned for writing.
    assert TaggingScriptEngine.desired_custom_tags((oversized,), result) == ()


def test_a_script_cannot_write_an_oversized_custom_tag_even_over_an_existing_one() -> None:
    document = AudioMetadataDocument(fields=())
    existing = CustomTagValue(name="NOTES", values=("x" * 9_000,))

    with pytest.raises(ScriptValidationError, match="too long"):
        TaggingScriptEngine().apply(
            document,
            (_tagging('append custom.NOTES = "more"'),),
            custom_tags=(existing,),
        )


def test_script_depth_list_output_and_runtime_bounds() -> None:
    deeply_nested = "fallback(" * 21 + '"x"' + ")" * 21
    with pytest.raises(ScriptValidationError, match="nested too deeply"):
        compile_expression(deeply_nested, script_name="Deep")

    too_many_values = "[" + ",".join('"x"' for _ in range(101)) + "]"
    with pytest.raises(ScriptValidationError, match="too many values"):
        compile_expression(too_many_values, script_name="List")

    expression = compile_expression("title", script_name="Output")
    with pytest.raises(ScriptValidationError, match="too long"):
        evaluate_expression(
            expression,
            {"title": "x" * 8_193},
            EvaluationBudget(script_name="Output"),
        )
    exhausted = EvaluationBudget(script_name="Steps", steps=10_000)
    with pytest.raises(ScriptValidationError, match="step limit"):
        evaluate_expression(
            compile_expression('"x"', script_name="Steps"),
            {},
            exhausted,
        )

    program = "\n".join(f'set title = concat(title, "{index}")' for index in range(501))
    with pytest.raises(ScriptValidationError, match="too many statements"):
        compile_tagging_program(program, script_name="Steps")


def test_settings_round_trip_new_path_compatibility_controls() -> None:
    settings = build_initial_library_management_settings()
    compatibility = settings.profiles[0].organization.compatibility
    compatibility.extension_case = "lower"
    compatibility.windows_legacy_path_limit = True

    normalized = normalize_library_management_settings(settings)

    assert normalized.profiles[0].organization.compatibility.extension_case == "lower"
    assert (
        normalized.profiles[0].organization.compatibility.windows_legacy_path_limit
        is True
    )


def test_format_management_literal_path_never_parses_template_bodies() -> None:
    """F-075/F-083: the literal-path entry point applies only output-side
    normalization - brace-bearing directory names round-trip untouched and a
    body like {title} stays literal instead of resolving as a variable."""
    compatibility = PathCompatibilitySettings()
    result = NamingTemplateEngine().format_management_literal_path(
        "Live {2020}/Live {Deluxe}/{title} $special cover.jpg",
        compatibility,
        script_name="Default external artwork naming",
        root=Path("/music"),
    )

    assert result.relative_path == (
        "Live {2020}/Live {Deluxe}/{title} $special cover.jpg"
    )
    assert result.collision_key == (
        unicodedata.normalize("NFC", result.relative_path).casefold()
    )
    assert result.rendered_characters == len(result.relative_path)


def test_format_management_literal_path_normalizes_and_enforces_limits() -> None:
    """F-075: literal paths get the same NFC/cleaning/length pipeline as
    rendered templates, including CJK components and limit enforcement."""
    engine = NamingTemplateEngine()
    cjk_name = "宇宙のアルバム"
    result = engine.format_management_literal_path(
        f"{cjk_name}/cover.jpg",
        PathCompatibilitySettings(),
        script_name="Default external artwork naming",
        root=Path("/music"),
    )
    assert result.relative_path == f"{cjk_name}/cover.jpg"
    assert result.collision_key == f"{cjk_name.casefold()}/cover.jpg"

    with pytest.raises(ScriptValidationError, match="traversing"):
        engine.format_management_literal_path(
            "../escape.jpg",
            PathCompatibilitySettings(),
            script_name="Default external artwork naming",
        )
    with pytest.raises(PathLimitExceededError):
        engine.format_management_literal_path(
            "x" * 300 + ".jpg",
            PathCompatibilitySettings(maximum_path_length=180),
            script_name="Default external artwork naming",
        )
