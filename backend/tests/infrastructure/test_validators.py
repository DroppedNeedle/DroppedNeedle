"""Validator unit tests (BrainzMashEfficiency T7)."""

import pytest

from infrastructure.validators import is_valid_isrc, is_valid_mbid


@pytest.mark.parametrize(
    "value",
    [
        pytest.param("USRC17607839", id="upper"),
        pytest.param("usrc17607839", id="lower-normalizes-to-upper"),
        pytest.param("AB12CD34EF56", id="mixed-alnum"),
        pytest.param("  USRC17607839  ", id="surrounding-whitespace"),
    ],
)
def test_is_valid_isrc_accepts_canonical_shapes(value: str):
    assert is_valid_isrc(value) is True


@pytest.mark.parametrize(
    "value",
    [
        pytest.param("USRC1760783", id="too-short-11"),
        pytest.param("USRC176078390", id="too-long-13"),
        pytest.param("USRC1760783!", id="symbol"),
        pytest.param("US-RC1760783", id="hyphen"),
        pytest.param("4uLU6hMCjMI75M1A2tKUQ3", id="spotify-shaped-22-char"),
        pytest.param("", id="empty"),
        pytest.param(None, id="none"),
        pytest.param(123456789012, id="non-string"),
    ],
)
def test_is_valid_isrc_rejects_malformed_shapes(value):
    assert is_valid_isrc(value) is False


@pytest.mark.parametrize(
    "value",
    [
        pytest.param("f27d14f8-6c7e-4e2a-9b1c-3d4e5f6a7b8c", id="lowercase-valid"),
        pytest.param("F27D14F8-6C7E-4E2A-9B1C-3D4E5F6A7B8C", id="uppercase-valid"),
    ],
)
def test_is_valid_mbid_accepts_valid_uuid_shapes(value: str):
    assert is_valid_mbid(value) is True


@pytest.mark.parametrize(
    "value",
    [
        pytest.param("0", id="zero"),
        pytest.param("2047108-7", id="short-numeric"),
        pytest.param("unknown_x", id="unknown-placeholder"),
        pytest.param("4uLU6hMCjMI75M1A2tKUQ3", id="spotify-shaped-22-char"),
        pytest.param("", id="empty"),
        pytest.param(None, id="none"),
    ],
)
def test_is_valid_mbid_rejects_non_uuid_shapes(value):
    assert is_valid_mbid(value) is False
