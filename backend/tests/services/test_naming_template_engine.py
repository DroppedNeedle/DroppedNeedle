"""Tests for NamingTemplateEngine — variable substitution + path normalisation."""

import unicodedata
from pathlib import Path

import pytest

from models.audio import AudioTag
from services.native.naming import NamingTemplateEngine


def _tag(**overrides) -> AudioTag:
    base = dict(
        title="Airbag",
        artist="Radiohead",
        album="OK Computer",
        album_artist="Radiohead",
        track_number=1,
        disc_number=1,
        year=1997,
        genre="Alternative Rock",
        musicbrainz_release_group_id="rg-1",
        musicbrainz_artist_id="art-1",
    )
    base.update(overrides)
    return AudioTag(**base)


@pytest.fixture
def engine() -> NamingTemplateEngine:
    return NamingTemplateEngine()


def test_default_template_renders_expected_path(engine):
    result = engine.format_path(engine.DEFAULT, _tag(), "flac")
    assert result == Path("Radiohead/OK Computer (1997)/0101 Airbag.flac")


def test_albumartist_falls_back_to_artist(engine):
    result = engine.format_path("{albumartist}/x.{ext}", _tag(album_artist=None), "flac")
    assert result == Path("Radiohead/x.flac")


def test_track_and_disc_zero_padded(engine):
    result = engine.format_path("{disc:02d}{track:02d}", _tag(disc_number=3, track_number=7), "flac")
    assert result == Path("0307")


def test_track_without_format_spec_is_unpadded(engine):
    result = engine.format_path("{track}", _tag(track_number=7), "flac")
    assert result == Path("7")


def test_year_empty_when_none_leaves_no_bracket_litter(engine):
    # An empty variable must not leave its template decoration behind:
    # "{album} ({year})" with no year renders "OK Computer", not "OK Computer ()".
    result = engine.format_path("{album} ({year})", _tag(year=None), "flac")
    assert result == Path("OK Computer")


def test_default_template_without_year_has_no_empty_parens(engine):
    result = engine.format_path(engine.DEFAULT, _tag(year=None), "flac")
    assert result == Path("Radiohead/OK Computer/0101 Airbag.flac")


def test_empty_square_bracket_group_collapsed(engine):
    result = engine.format_path("{album} [{year}]/{title}.{ext}", _tag(year=None), "flac")
    assert result == Path("OK Computer/Airbag.flac")


def test_year_present_keeps_its_brackets(engine):
    # Only EMPTY groups are collapsed - a populated year renders as before.
    result = engine.format_path("{album} ({year})", _tag(year=1997), "flac")
    assert result == Path("OK Computer (1997)")


def test_unknown_variable_renders_empty(engine):
    result = engine.format_path("a{unknown}b", _tag(), "flac")
    assert result == Path("ab")


def test_musicbrainz_id_and_artist_mbid_variables(engine):
    result = engine.format_path("{musicbrainz_id}/{artist_mbid}", _tag(), "flac")
    assert result == Path("rg-1/art-1")


def test_genre_variable(engine):
    result = engine.format_path("{genre}", _tag(genre="Jazz"), "flac")
    assert result == Path("Jazz")


def test_medium_variable_is_empty(engine):
    result = engine.format_path("x{medium}y", _tag(), "flac")
    assert result == Path("xy")


def test_nfd_input_is_normalised_to_nfc(engine):
    # "ガ" decomposes (NFD) to カ + U+3099; the engine must recompose to NFC.
    decomposed = unicodedata.normalize("NFD", "ガ")
    assert len(decomposed) == 2  # guard: the input really is decomposed
    result = engine.format_path("{title}", _tag(title=decomposed), "flac")
    component = result.parts[-1]
    assert component == "ガ"
    assert len(component) == 1


def test_cjk_not_transliterated(engine):
    result = engine.format_path("{artist}", _tag(artist="ユキ"), "flac")
    assert result == Path("ユキ")  # no unidecode -> "Yuki" mangling


def test_invalid_fs_chars_replaced_with_underscore(engine):
    result = engine.format_path("{title}", _tag(title='a<b>c:d"e|f?g*h'), "flac")
    assert result == Path("a_b_c_d_e_f_g_h")


def test_slash_in_value_does_not_inject_path_component(engine):
    result = engine.format_path("{artist}/x.{ext}", _tag(artist="AC/DC"), "flac")
    assert result == Path("AC_DC/x.flac")


def test_long_component_truncated_to_252_bytes(engine):
    result = engine.format_path("{title}", _tag(title="A" * 300), "flac")
    component = result.parts[-1]
    assert len(component.encode("utf-8")) == 252


def test_multibyte_truncation_does_not_split_character(engine):
    # 200 three-byte chars = 600 bytes; truncating at 252 must land on a char
    # boundary (84 chars) — never a partial sequence / replacement char.
    result = engine.format_path("{title}", _tag(title="あ" * 200), "flac")
    component = result.parts[-1]
    assert len(component.encode("utf-8")) <= 252
    assert "�" not in component
    assert component == "あ" * 84


def test_validate_template_flags_unknown_variable(engine):
    errors = engine.validate_template("{albumartist}/{bogus}.{ext}")
    assert errors == ["Unknown variable: {bogus}"]


def test_validate_template_flags_format_spec_on_plain_variable(engine):
    errors = engine.validate_template("{title:02d}")
    assert errors == ["Variable {title} does not support a format spec"]


def test_validate_template_accepts_default(engine):
    assert engine.validate_template(engine.DEFAULT) == []


def test_dotdot_value_does_not_traverse(engine):
    result = engine.format_path("{albumartist}/{album}/{title}.{ext}", _tag(album_artist=".."), "flac")
    assert ".." not in result.parts
    assert not result.is_absolute()
    assert result.parts[0] == "_"


def test_dot_value_is_collapsed(engine):
    # A "." component is collapsed by pathlib itself, so it cannot traverse.
    result = engine.format_path("{albumartist}/x.{ext}", _tag(album_artist="."), "flac")
    assert result == Path("x.flac")
    assert not result.is_absolute()


def test_empty_leading_variable_stays_relative(engine):
    # album_artist=None and artist="" -> albumartist renders empty; the path must
    # not become absolute (which would discard library_root on join).
    result = engine.format_path("{albumartist}/{album}.{ext}", _tag(album_artist=None, artist=""), "flac")
    assert not result.is_absolute()
    assert result == Path("OK Computer.flac")


def test_leading_and_trailing_dots_and_spaces_stripped(engine):
    result = engine.format_path("{album}/x.{ext}", _tag(album=".hidden. "), "flac")
    assert result.parts[0] == "hidden"


def test_reserved_windows_names_disarmed(engine):
    result = engine.format_path("{albumartist}/{title}.{ext}", _tag(album_artist="CON", title="AUX"), "flac")
    assert result.parts == ("_CON", "_AUX.flac")


def test_control_characters_replaced(engine):
    result = engine.format_path("{title}.{ext}", _tag(title="a\x01b\tc"), "flac")
    assert result == Path("a_b_c.flac")


def test_validate_template_rejects_dotdot_segment(engine):
    errors = engine.validate_template("{albumartist}/../{title}.{ext}")
    assert any("not allowed" in e for e in errors)
